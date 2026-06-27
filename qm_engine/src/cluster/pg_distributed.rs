/*
 * PG wire distributed transactions — BEGIN/COMMIT spanning shards via 2PC.
 *
 * Requires QM_CLUSTER_2PC=1 and QM_CLUSTER_PG_DISTRIBUTED=1.
 */

use parking_lot::Mutex;
use std::cell::Cell;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

use super::config::ClusterNodeConfig;
use super::cross_shard::participant_map;
use super::runtime::ClusterRuntime;
use super::shard_key::extract_shard_key;
use super::transport::NodeClient;
use super::two_phase_commit::{TwoPhaseCoordinator, TwoPhaseParticipant};
use crate::gateway::native_sql::NativeSqlEngine;
use crate::gateway::QueryResult;

static FWD_TXN: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static CONN_ID: Cell<u64> = const { Cell::new(0) };
}

pub fn set_connection_id(id: u64) {
    CONN_ID.with(|c| c.set(id));
}

pub fn clear_connection_id() {
    CONN_ID.with(|c| c.set(0));
}

fn connection_id() -> u64 {
    CONN_ID.with(|c| c.get())
}

#[derive(Default)]
struct PendingDistributedTxn {
    ops: HashMap<u32, Vec<String>>,
}

static SESSIONS: LazyLock<Mutex<HashMap<u64, PendingDistributedTxn>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn pg_distributed_enabled(cfg: &ClusterNodeConfig) -> bool {
    cfg.pg_distributed_enabled && cfg.two_pc_enabled
}

fn is_begin(sql: &str) -> bool {
    let up = sql.trim().to_ascii_uppercase();
    up == "BEGIN" || up.starts_with("BEGIN ") || up.starts_with("START TRANSACTION")
}

fn is_commit(sql: &str) -> bool {
    sql.trim().eq_ignore_ascii_case("COMMIT")
}

fn is_rollback(sql: &str) -> bool {
    sql.trim().eq_ignore_ascii_case("ROLLBACK")
}

fn node_id_for_addr(participants: &[(u32, SocketAddr)], addr: SocketAddr) -> Option<u32> {
    participants
        .iter()
        .find(|(_, a)| *a == addr)
        .map(|(id, _)| *id)
}

fn forward_remote(addr: SocketAddr, sql: &str) -> Result<(), String> {
    let txn_id = FWD_TXN.fetch_add(1, Ordering::Relaxed);
    NodeClient::new(0, addr)
        .forward_query_blocking(sql, txn_id, None)
        .map_err(|e| format!("forward to {addr}: {e}"))
        .and_then(|msg| {
            if msg.ok {
                Ok(())
            } else {
                Err(msg.error.unwrap_or_else(|| "remote forward failed".into()))
            }
        })
}

pub fn execute_pg_routed(
    runtime: &ClusterRuntime,
    cfg: &ClusterNodeConfig,
    local_engine: &Arc<NativeSqlEngine>,
    local_addr: Option<SocketAddr>,
    sql: &str,
    inner: impl FnOnce() -> Result<QueryResult, String>,
) -> Result<QueryResult, String> {
    if !runtime.is_active() || !pg_distributed_enabled(cfg) {
        return inner();
    }

    let conn = connection_id();
    if conn == 0 {
        return inner();
    }

    if is_begin(sql) {
        SESSIONS.lock().insert(conn, PendingDistributedTxn::default());
        return inner();
    }

    if is_rollback(sql) {
        SESSIONS.lock().remove(&conn);
        return inner();
    }

    if is_commit(sql) {
        return commit_distributed(runtime, cfg, local_engine, local_addr, conn);
    }

    if SESSIONS.lock().contains_key(&conn) {
        return stage_distributed_op(runtime, local_engine, local_addr, conn, sql);
    }

    inner()
}

fn stage_distributed_op(
    runtime: &ClusterRuntime,
    local_engine: &Arc<NativeSqlEngine>,
    local_addr: Option<SocketAddr>,
    conn: u64,
    sql: &str,
) -> Result<QueryResult, String> {
    let shard_key = extract_shard_key(sql);
    let plan = runtime
        .route_sql_if_active(sql, shard_key)
        .ok_or_else(|| format!("no route for: {sql}"))?;
    let addr = plan
        .primary
        .ok_or_else(|| format!("no primary for: {sql}"))?;
    let participants = participant_map(runtime);
    let node_id = node_id_for_addr(&participants, addr)
        .ok_or_else(|| format!("unknown primary {addr}"))?;

    let stmt = format!("{};", sql.trim().trim_end_matches(';'));
    {
        let mut sessions = SESSIONS.lock();
        let pending = sessions
            .get_mut(&conn)
            .ok_or_else(|| "no distributed txn for connection".to_string())?;
        pending.ops.entry(node_id).or_default().push(stmt);
    }

    if local_addr.is_some_and(|l| l == addr) {
        local_engine.execute(sql)?;
    } else {
        forward_remote(addr, sql)?;
    }

    Ok(QueryResult {
        columns: vec![],
        rows: vec![],
        command_tag: "STAGED".into(),
    })
}

fn commit_distributed(
    runtime: &ClusterRuntime,
    cfg: &ClusterNodeConfig,
    local_engine: &Arc<NativeSqlEngine>,
    local_addr: Option<SocketAddr>,
    conn: u64,
) -> Result<QueryResult, String> {
    let pending = SESSIONS.lock().remove(&conn).unwrap_or_default();
    if pending.ops.is_empty() {
        return Ok(QueryResult {
            columns: vec![],
            rows: vec![],
            command_tag: "COMMIT".into(),
        });
    }

    let participants = participant_map(runtime);
    let local_node_id = local_addr
        .and_then(|la| node_id_for_addr(&participants, la))
        .unwrap_or(cfg.node_id.max(1));

    let participant = Arc::new(TwoPhaseParticipant::new(Arc::clone(local_engine)));
    let coordinator = TwoPhaseCoordinator::with_local(
        participants.clone(),
        Arc::clone(&participant),
        local_node_id,
    );

    let txn_id = coordinator.begin();
    for (node_id, ops) in pending.ops {
        for sql in ops {
            coordinator.add_op(txn_id, node_id, sql)?;
        }
    }

    match coordinator.prepare(txn_id) {
        Ok(true) => {
            coordinator.commit(txn_id)?;
            Ok(QueryResult {
                columns: vec![],
                rows: vec![],
                command_tag: "COMMIT".into(),
            })
        }
        Ok(false) => {
            let _ = coordinator.abort(txn_id);
            Err(format!("distributed txn {txn_id} aborted at prepare"))
        }
        Err(e) => {
            let _ = coordinator.abort(txn_id);
            Err(e)
        }
    }
}
