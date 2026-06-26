/*
 * Cross-shard distributed transactions via 2PC.
 *
 * Client sends a batch prefixed with `QM DISTRIBUTED`:
 *
 *   QM DISTRIBUTED
 *   INSERT INTO t (id) VALUES (1);
 *   INSERT INTO t (id) VALUES (2);
 *
 * Each statement is routed by shard key; coordinator runs prepare/commit.
 */

use std::net::SocketAddr;
use std::sync::Arc;

use super::config::ClusterNodeConfig;
use super::ddl_fanout;
use super::shard_key::extract_shard_key;
use super::runtime::ClusterRuntime;
use super::two_phase_commit::{TwoPhaseCoordinator, TwoPhaseParticipant};
use crate::gateway::native_sql::NativeSqlEngine;
use crate::gateway::QueryResult;

/// True when SQL is a `QM DISTRIBUTED` multi-statement batch.
pub fn is_distributed_batch(sql: &str) -> bool {
    parse_distributed_batch(sql).is_some()
}

/// Parse `QM DISTRIBUTED` batch into individual SQL statements.
pub fn parse_distributed_batch(sql: &str) -> Option<Vec<String>> {
    let trimmed = sql.trim();
    let upper = trimmed.to_ascii_uppercase();

    let body = if upper.starts_with("QM DISTRIBUTED") {
        let header_len = "QM DISTRIBUTED".len();
        let rest = &trimmed[header_len..];
        rest.trim_start_matches([';', ' ', '\n', '\r'])
    } else if upper.starts_with("/*QM:DISTRIBUTED*/") {
        trimmed.split_once("*/").map(|(_, tail)| tail.trim())?
    } else {
        return None;
    };

    let stmts: Vec<String> = body
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("{s};"))
        .collect();

    if stmts.is_empty() {
        None
    } else {
        Some(stmts)
    }
}

/// Assign stable node ids to distinct shard primaries (1-based index).
pub fn participant_map(runtime: &ClusterRuntime) -> Vec<(u32, SocketAddr)> {
    ddl_fanout::unique_primary_endpoints(runtime)
        .into_iter()
        .enumerate()
        .map(|(i, addr)| ((i as u32) + 1, addr))
        .collect()
}

fn node_id_for_addr(participants: &[(u32, SocketAddr)], addr: SocketAddr) -> Option<u32> {
    participants
        .iter()
        .find(|(_, a)| *a == addr)
        .map(|(id, _)| *id)
}

/// Execute a cross-shard batch atomically (2PC).
pub fn execute_distributed_batch(
    runtime: &ClusterRuntime,
    cfg: &ClusterNodeConfig,
    local_engine: &Arc<NativeSqlEngine>,
    local_addr: Option<SocketAddr>,
    sql: &str,
) -> Result<QueryResult, String> {
    if !cfg.two_pc_enabled {
        return Err("QM DISTRIBUTED requires QM_CLUSTER_2PC=1".into());
    }

    let statements = parse_distributed_batch(sql)
        .ok_or_else(|| "invalid QM DISTRIBUTED batch".to_string())?;
    let participants = participant_map(runtime);
    if participants.is_empty() {
        return Err("no cluster participants for 2PC".into());
    }

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
    for stmt in &statements {
        let shard_key = extract_shard_key(stmt);
        let plan = runtime
            .route_sql_if_active(stmt, shard_key)
            .ok_or_else(|| format!("no route for statement: {stmt}"))?;
        let addr = plan
            .primary
            .ok_or_else(|| format!("no primary for statement: {stmt}"))?;
        let node_id = node_id_for_addr(&participants, addr)
            .ok_or_else(|| format!("unknown primary {addr} for: {stmt}"))?;
        coordinator.add_op(txn_id, node_id, stmt.clone())?;
    }

    match coordinator.prepare(txn_id) {
        Ok(true) => {
            coordinator.commit(txn_id)?;
            Ok(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                command_tag: format!(
                    "QM DISTRIBUTED {} {}",
                    statements.len(),
                    txn_id
                ),
            })
        }
        Ok(false) => {
            let _ = coordinator.abort(txn_id);
            Err(format!("distributed transaction {txn_id} aborted at prepare"))
        }
        Err(e) => {
            let _ = coordinator.abort(txn_id);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::meta_cluster::MetaCluster;
    use crate::cluster::node_registry::{find_shard_key_for_shard, ShardEndpointRegistry};
    use crate::cluster::transport::TransportServer;
    use std::sync::Arc;
    use tokio::net::TcpListener;

    #[test]
    fn parse_distributed_batch_splits_statements() {
        let sql = "QM DISTRIBUTED\nINSERT INTO t (id) VALUES (1);\nINSERT INTO t (id) VALUES (2);";
        let stmts = parse_distributed_batch(sql).expect("parse");
        assert_eq!(stmts.len(), 2);
    }

    async fn spawn_transport(engine: Arc<NativeSqlEngine>) -> SocketAddr {
        use crate::cluster::transport::TransportServer;
        use crate::cluster::two_phase_commit::TwoPhaseParticipant;
        use crate::cluster::wal_apply::WalApplyTracker;
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let participant = Arc::new(TwoPhaseParticipant::new(Arc::clone(&engine)));
        let wal_tracker = Arc::new(WalApplyTracker::new());
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _ = ready_tx.send(());
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let eng = Arc::clone(&engine);
                let part = Arc::clone(&participant);
                let wal = Arc::clone(&wal_tracker);
                tokio::spawn(async move {
                    let _ =
                        TransportServer::serve_connection_shared(stream, eng, part, wal).await;
                });
            }
        });
        ready_rx.await.expect("ready");
        addr
    }

    #[test]
    fn cross_shard_2pc_commits_on_two_nodes() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("runtime");

        let node_a = Arc::new(NativeSqlEngine::new());
        let node_b = Arc::new(NativeSqlEngine::new());

        let addr_a = rt.block_on(spawn_transport(Arc::clone(&node_a)));
        let addr_b = rt.block_on(spawn_transport(Arc::clone(&node_b)));

        node_a
            .execute("CREATE TABLE dist2pc (id INTEGER PRIMARY KEY, n TEXT)")
            .expect("ddl a");
        node_b
            .execute("CREATE TABLE dist2pc (id INTEGER PRIMARY KEY, n TEXT)")
            .expect("ddl b");

        let mut registry = ShardEndpointRegistry::new();
        registry.insert(0, addr_a);
        registry.insert(1, addr_a);
        registry.insert(2, addr_b);
        registry.insert(3, addr_b);

        let cluster = MetaCluster::new(&[1]);
        cluster.bootstrap_default_groups_with_registry(1, 4, &registry);
        let runtime = ClusterRuntime::with_catalog(cluster.catalog_on(1).expect("cat"));
        runtime.enable();

        let key_a = find_shard_key_for_shard(4, 0);
        let key_b = find_shard_key_for_shard(4, 2);

        let batch = format!(
            "QM DISTRIBUTED\n\
             INSERT INTO dist2pc (id, n) VALUES ({key_a}, 'a');\n\
             INSERT INTO dist2pc (id, n) VALUES ({key_b}, 'b');"
        );

        let cfg = ClusterNodeConfig {
            enabled: true,
            local_addr: Some(addr_a),
            two_pc_enabled: true,
            ..ClusterNodeConfig::default()
        };

        execute_distributed_batch(&runtime, &cfg, &node_a, Some(addr_a), &batch)
            .expect("2pc commit");

        let on_a = node_a
            .execute(&format!("SELECT n FROM dist2pc WHERE id = {key_a}"))
            .expect("row a");
        assert_eq!(on_a.rows.len(), 1);

        let on_b = node_b
            .execute(&format!("SELECT n FROM dist2pc WHERE id = {key_b}"))
            .expect("row b");
        assert_eq!(on_b.rows.len(), 1);
    }
}
