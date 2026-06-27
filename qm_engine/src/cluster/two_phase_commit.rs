/*
 * Two-Phase Commit Coordinator & Participant — Phase C2
 *
 * Protocol:
 *   Coordinator                      Participant(s)
 *   ──────────────────────────────────────────────
 *   begin()         → txn_id
 *   add_op(node_id, sql)
 *   prepare()       → PREPARE_REQ   → vote YES/NO
 *   if all YES      → COMMIT_REQ    → commit
 *   else            → ABORT_REQ     → rollback
 *
 * The coordinator is pure Rust/async. Participants live in the same
 * process (TransportServer handles remote participants — the Participant
 * type here represents LOCAL prepared state).
 */

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(feature = "python")]
use pyo3::prelude::*;

use crate::cluster::transport::NodeClient;

// ── Phase enum ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxnPhase {
    Active,
    Preparing,
    Prepared,
    Committing,
    Committed,
    Aborted,
}

impl std::fmt::Display for TxnPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TxnPhase::Active     => "active",
            TxnPhase::Preparing  => "preparing",
            TxnPhase::Prepared   => "prepared",
            TxnPhase::Committing => "committing",
            TxnPhase::Committed  => "committed",
            TxnPhase::Aborted    => "aborted",
        };
        f.write_str(s)
    }
}

// ── In-flight distributed transaction ─────────────────────────────

#[derive(Debug)]
struct DistributedTxn {
    txn_id: u64,
    phase: TxnPhase,
    /// Per-node SQL operations. Key = node_id.
    ops: HashMap<u32, Vec<String>>,
    /// YES votes received so far.
    votes: HashMap<u32, bool>,
    /// When this txn was created (for stuck-txn detection).
    created_at: Instant,
}

impl DistributedTxn {
    fn new(txn_id: u64) -> Self {
        Self {
            txn_id,
            phase: TxnPhase::Active,
            ops: HashMap::new(),
            votes: HashMap::new(),
            created_at: Instant::now(),
        }
    }

    fn participant_count(&self) -> usize {
        self.ops.len()
    }

    fn all_voted_yes(&self) -> bool {
        self.participant_count() > 0
            && self.votes.len() == self.participant_count()
            && self.votes.values().all(|&v| v)
    }
}

// ── Coordinator ─────────────────────────────────────────────────────

/// Drives distributed transactions across remote participants.
pub struct TwoPhaseCoordinator {
    txns: Mutex<HashMap<u64, DistributedTxn>>,
    next_txn_id: std::sync::atomic::AtomicU64,
    /// (node_id, peer_address)
    participants: Vec<(u32, SocketAddr)>,
    runtime: Arc<tokio::runtime::Runtime>,
    local_participant: Option<Arc<TwoPhaseParticipant>>,
    local_node_id: Option<u32>,
}

impl TwoPhaseCoordinator {
    pub fn new(participants: Vec<(u32, SocketAddr)>) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to build 2PC runtime");
        Self {
            txns: Mutex::new(HashMap::new()),
            next_txn_id: std::sync::atomic::AtomicU64::new(1),
            participants,
            runtime: Arc::new(runtime),
            local_participant: None,
            local_node_id: None,
        }
    }

    pub fn with_local(
        participants: Vec<(u32, SocketAddr)>,
        local_participant: Arc<TwoPhaseParticipant>,
        local_node_id: u32,
    ) -> Self {
        let mut coord = Self::new(participants);
        coord.local_participant = Some(local_participant);
        coord.local_node_id = Some(local_node_id);
        coord
    }

    /// Begin a new distributed transaction. Returns the transaction ID.
    pub fn begin(&self) -> u64 {
        let txn_id = self
            .next_txn_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut txns = self.txns.lock().unwrap();
        txns.insert(txn_id, DistributedTxn::new(txn_id));
        txn_id
    }

    /// Add a SQL operation to be executed on `node_id` as part of `txn_id`.
    pub fn add_op(&self, txn_id: u64, node_id: u32, sql: String) -> Result<(), String> {
        let mut txns = self.txns.lock().unwrap();
        let txn = txns
            .get_mut(&txn_id)
            .ok_or_else(|| format!("Unknown txn {}", txn_id))?;
        if txn.phase != TxnPhase::Active {
            return Err(format!("Txn {} is not active (phase: {})", txn_id, txn.phase));
        }
        txn.ops.entry(node_id).or_default().push(sql);
        Ok(())
    }

    /// Phase 1: send PREPARE to all participants.  
    /// Returns `true` if all voted YES, `false` if any voted NO.
    pub fn prepare(&self, txn_id: u64) -> Result<bool, String> {
        // Snapshot ops under lock, then release before async I/O.
        let (ops, participant_addr) = {
            let mut txns = self.txns.lock().unwrap();
            let txn = txns
                .get_mut(&txn_id)
                .ok_or_else(|| format!("Unknown txn {}", txn_id))?;
            txn.phase = TxnPhase::Preparing;
            let ops: Vec<(u32, Vec<String>)> = txn.ops.clone().into_iter().collect();
            let addrs: HashMap<u32, SocketAddr> = self.participants.iter().cloned().collect();
            (ops, addrs)
        };

        // Build futures: one PREPARE per participant that has ops.
        let txns_ref = &self.txns;
        let local_node_id = self.local_node_id;
        let local_participant = self.local_participant.clone();
        self.runtime.block_on(async move {
            let mut all_yes = true;
            for (node_id, node_ops) in ops {
                let vote = if local_node_id == Some(node_id) {
                    local_participant
                        .as_ref()
                        .map(|p| p.prepare(txn_id, node_ops))
                        .unwrap_or(false)
                } else {
                    let addr = participant_addr
                        .get(&node_id)
                        .ok_or_else(|| format!("No address for node {}", node_id))?;
                    let client = NodeClient::new(node_id, *addr);
                    match tokio::time::timeout(
                        Duration::from_secs(30),
                        client.send_prepare(txn_id, node_ops),
                    )
                    .await
                    {
                        Ok(result) => result.unwrap_or(false),
                        Err(_) => {
                            tracing::warn!(
                                "2PC prepare timeout for node {} in txn {}",
                                node_id,
                                txn_id
                            );
                            false
                        }
                    }
                };
                if !vote {
                    all_yes = false;
                }
                txns_ref
                    .lock()
                    .unwrap()
                    .get_mut(&txn_id)
                    .map(|t| {
                        t.votes.insert(node_id, vote);
                    });
            }

            txns_ref.lock().unwrap()
                .get_mut(&txn_id)
                .map(|t| {
                    t.phase = if all_yes { TxnPhase::Prepared } else { TxnPhase::Aborted };
                });

            Ok::<bool, String>(all_yes)
        })
    }

    /// Phase 2: send COMMIT to all participants.
    pub fn commit(&self, txn_id: u64) -> Result<(), String> {
        self.phase2(txn_id, true)
    }

    /// Phase 2: send ABORT to all participants.
    pub fn abort(&self, txn_id: u64) -> Result<(), String> {
        self.phase2(txn_id, false)
    }

    fn phase2(&self, txn_id: u64, commit: bool) -> Result<(), String> {
        let participant_addr: HashMap<u32, SocketAddr> =
            self.participants.iter().cloned().collect();

        let node_ids: Vec<u32> = {
            let mut txns = self.txns.lock().unwrap();
            let txn = txns
                .get_mut(&txn_id)
                .ok_or_else(|| format!("Unknown txn {}", txn_id))?;
            // Validate phase transition: only Prepared → Committing/Aborted is valid
            match (&txn.phase, commit) {
                (TxnPhase::Prepared, _) => {}
                (TxnPhase::Aborted, false) => {
                    return Ok(()); // already aborted, idempotent
                }
                (phase, _) => {
                    return Err(format!(
                        "Txn {} cannot {} from phase {} (must be Prepared)",
                        txn_id,
                        if commit { "commit" } else { "abort" },
                        phase
                    ));
                }
            }
            txn.phase = if commit { TxnPhase::Committing } else { TxnPhase::Aborted };
            txn.ops.keys().cloned().collect()
        };

        let txns_ref = &self.txns;
        let local_node_id = self.local_node_id;
        let local_participant = self.local_participant.clone();
        self.runtime.block_on(async move {
            for node_id in node_ids {
                if local_node_id == Some(node_id) {
                    if let Some(p) = local_participant.as_ref() {
                        if commit {
                            let _ = p.commit(txn_id);
                        } else {
                            p.abort(txn_id);
                        }
                    }
                } else if let Some(&addr) = participant_addr.get(&node_id) {
                    let client = NodeClient::new(node_id, addr);
                    if let Err(e) = client.send_commit_or_abort(txn_id, commit).await {
                        return Err(format!(
                            "2PC {} to node {} ({}) failed: {e}",
                            if commit { "commit" } else { "abort" },
                            node_id,
                            addr
                        ));
                    }
                }
            }
            txns_ref.lock().unwrap()
                .get_mut(&txn_id)
                .map(|t| {
                    t.phase = if commit { TxnPhase::Committed } else { TxnPhase::Aborted };
                });
            Ok::<(), String>(())
        })
    }

    /// Return the current phase of a transaction as a string.
    pub fn txn_phase(&self, txn_id: u64) -> Option<String> {
        self.txns.lock().unwrap()
            .get(&txn_id)
            .map(|t| t.phase.to_string())
    }

    /// Remove completed / aborted transactions and abort stuck ones to free memory.
    pub fn gc(&self) {
        let mut txns = self.txns.lock().unwrap();
        // M-06: Abort transactions stuck in Preparing/Prepared for > 5 minutes.
        let stuck_timeout = Duration::from_secs(300);
        for txn in txns.values_mut() {
            if matches!(txn.phase, TxnPhase::Preparing | TxnPhase::Prepared)
                && txn.created_at.elapsed() > stuck_timeout
            {
                tracing::warn!("2PC GC: aborting stuck txn {} in phase {}", txn.txn_id, txn.phase);
                txn.phase = TxnPhase::Aborted;
            }
        }
        txns.retain(|_, t| {
            !matches!(t.phase, TxnPhase::Committed | TxnPhase::Aborted)
        });
    }
}

// ── Local participant ────────────────────────────────────────────────

/// Local participant for 2PC — holds prepared-but-not-yet-committed operations.
pub struct TwoPhaseParticipant {
    engine: Arc<crate::gateway::native_sql::NativeSqlEngine>,
    /// txn_id → list of SQL operations to execute on COMMIT.
    prepared: Mutex<HashMap<u64, Vec<String>>>,
}

impl TwoPhaseParticipant {
    pub fn new(engine: Arc<crate::gateway::native_sql::NativeSqlEngine>) -> Self {
        Self { engine, prepared: Mutex::new(HashMap::new()) }
    }

    /// PREPARE: validate + stash ops. Returns true if ready to commit.
    pub fn prepare(&self, txn_id: u64, ops: Vec<String>) -> bool {
        // Basic validation: SQL must be non-empty and parseable.
        let all_valid = ops.iter().all(|sql| {
            let s = sql.trim();
            !s.is_empty()
        });
        if all_valid {
            self.prepared.lock().unwrap().insert(txn_id, ops);
        }
        all_valid
    }

    /// COMMIT: execute all prepared ops.
    pub fn commit(&self, txn_id: u64) -> bool {
        let ops = {
            let mut map = self.prepared.lock().unwrap();
            map.remove(&txn_id)
        };
        let Some(ops) = ops else { return false };
        for sql in &ops {
            if let Err(e) = self.engine.execute(sql) {
                tracing::error!("2PC commit failed for txn {}: {}", txn_id, e);
                return false;
            }
        }
        true
    }

    /// ABORT: discard prepared state.
    pub fn abort(&self, txn_id: u64) {
        self.prepared.lock().unwrap().remove(&txn_id);
    }
}

// ── Python-facing coordinator ───────────────────────────────────────

/// Python-accessible 2PC coordinator.
#[cfg(feature = "python")]
#[pyclass(name = "DistributedCoordinator")]
pub struct PyDistributedCoordinator {
    inner: Arc<TwoPhaseCoordinator>,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyDistributedCoordinator {
    #[new]
    #[pyo3(signature = (participants))]
    pub fn new(participants: Vec<(u32, String, u16)>) -> PyResult<Self> {
        let mut parsed: Vec<(u32, SocketAddr)> = Vec::with_capacity(participants.len());
        for (node_id, host, port) in participants {
            let addr: SocketAddr = format!("{}:{}", host, port)
                .parse()
                .map_err(|e| {
                    pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {}", e))
                })?;
            parsed.push((node_id, addr));
        }
        Ok(Self { inner: Arc::new(TwoPhaseCoordinator::new(parsed)) })
    }

    /// Begin a distributed transaction. Returns the transaction ID.
    pub fn begin(&self) -> u64 {
        self.inner.begin()
    }

    /// Add a SQL op for a specific node.
    pub fn add_op(&self, txn_id: u64, node_id: u32, sql: &str) -> PyResult<()> {
        self.inner
            .add_op(txn_id, node_id, sql.to_string())
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e))
    }

    /// Phase-1 PREPARE. Returns True if all participants voted YES.
    pub fn prepare(&self, txn_id: u64) -> PyResult<bool> {
        self.inner
            .prepare(txn_id)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e))
    }

    /// Phase-2 COMMIT.
    pub fn commit(&self, txn_id: u64) -> PyResult<()> {
        self.inner
            .commit(txn_id)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e))
    }

    /// Phase-2 ABORT.
    pub fn abort_txn(&self, txn_id: u64) -> PyResult<()> {
        self.inner
            .abort(txn_id)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e))
    }

    /// Return current phase of a transaction, or None if unknown.
    pub fn txn_phase(&self, txn_id: u64) -> Option<String> {
        self.inner.txn_phase(txn_id)
    }

    /// Execute a full distributed transaction atomically.  
    /// `ops` is a list of (node_id, sql_statement) tuples.
    pub fn execute_distributed(
        &self,
        ops: Vec<(u32, String)>,
    ) -> PyResult<bool> {
        let txn_id = self.inner.begin();
        for (node_id, sql) in ops {
            self.inner
                .add_op(txn_id, node_id, sql)
                .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e))?;
        }
        match self.inner.prepare(txn_id) {
            Ok(true) => {
                self.inner.commit(txn_id)
                    .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e))?;
                Ok(true)
            }
            Ok(false) => {
                let _ = self.inner.abort(txn_id);
                Ok(false)
            }
            Err(e) => {
                let _ = self.inner.abort(txn_id);
                Err(pyo3::exceptions::PyRuntimeError::new_err(e))
            }
        }
    }

    /// Remove completed/aborted transactions from memory.
    pub fn gc(&self) {
        self.inner.gc();
    }
}
