//! Native Hub Dispatcher — replaces Python HubDispatcher with Rust.
//!
//! Routes IPC commands to the correct ring buffer (gen/vec/proc)
//! using a shared LSN sequencer for global ordering.
//! Eliminates Python GIL overhead on the hot dispatch path.

#[cfg(feature = "python")]
use pyo3::prelude::*;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::lsn::LsnSequencer;
use super::ring_buffer::{CommandType, RingError, SharedRingBuffer};

// ── Configuration ─────────────────────────────────────────────────

/// Dispatcher config — mirrors Python `DispatcherConfig`.
pub struct DispatcherConfig {
    pub ring_dir: PathBuf,
    pub slot_count: usize,
    pub slot_data_size: usize,
    pub timeout_ms: u64,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            ring_dir: std::env::temp_dir().join("qm_rings_native"),
            slot_count: 1024,
            slot_data_size: 65536, // 64KB
            timeout_ms: 5000,
        }
    }
}

// ── Dispatch Result ───────────────────────────────────────────────

/// Result of a dispatched command.
#[derive(Debug)]
pub struct DispatchResult {
    /// Assigned LSN for this command.
    pub lsn: u64,
    /// Sequence number in the ring buffer.
    pub seq: u64,
    /// Which ring the command was dispatched to.
    pub target: DispatchTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchTarget {
    General,
    Vector,
    Procedure,
}

// ── Error ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum DispatchError {
    Io(io::Error),
    Ring(RingError),
    PayloadSerialize(String),
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DispatchError::Io(e) => write!(f, "IO error: {}", e),
            DispatchError::Ring(e) => write!(f, "Ring error: {}", e),
            DispatchError::PayloadSerialize(msg) => write!(f, "Serialize error: {}", msg),
        }
    }
}

impl From<io::Error> for DispatchError {
    fn from(e: io::Error) -> Self {
        DispatchError::Io(e)
    }
}

impl From<RingError> for DispatchError {
    fn from(e: RingError) -> Self {
        DispatchError::Ring(e)
    }
}

impl std::error::Error for DispatchError {}

// ── Native Hub Dispatcher ─────────────────────────────────────────

/// Rust-native Hub Dispatcher — zero-GIL command routing.
///
/// Owns three ring buffers (gen/vec/proc) and a shared LSN sequencer.
/// All dispatch methods are lock-free on the fast path.
pub struct NativeDispatcher {
    ring_gen: SharedRingBuffer,
    ring_vec: SharedRingBuffer,
    ring_proc: SharedRingBuffer,
    sequencer: Arc<LsnSequencer>,
    config: DispatcherConfig,
}

impl NativeDispatcher {
    /// Create a new dispatcher, initializing ring buffers in `ring_dir`.
    pub fn new(config: DispatcherConfig) -> Result<Self, DispatchError> {
        std::fs::create_dir_all(&config.ring_dir)?;

        let ring_gen = SharedRingBuffer::create(
            &config.ring_dir.join("gen_ring.shm"),
            config.slot_count,
            config.slot_data_size,
        )?;
        let ring_vec = SharedRingBuffer::create(
            &config.ring_dir.join("vec_ring.shm"),
            config.slot_count,
            config.slot_data_size,
        )?;
        let ring_proc = SharedRingBuffer::create(
            &config.ring_dir.join("proc_ring.shm"),
            config.slot_count,
            config.slot_data_size,
        )?;

        Ok(Self {
            ring_gen,
            ring_vec,
            ring_proc,
            sequencer: Arc::new(LsnSequencer::new(1)),
            config,
        })
    }

    /// Get the shared LSN sequencer.
    pub fn sequencer(&self) -> &Arc<LsnSequencer> {
        &self.sequencer
    }

    // ── DDL ───────────────────────────────────────────────────────

    /// Dispatch a DDL command (CREATE/DROP TABLE) to the general ring.
    pub fn dispatch_ddl(&self, payload: &[u8]) -> Result<DispatchResult, DispatchError> {
        let lsn = self.sequencer.next();
        let seq = self.ring_gen.publish(lsn, CommandType::Ddl, payload)?;
        Ok(DispatchResult {
            lsn,
            seq,
            target: DispatchTarget::General,
        })
    }

    // ── DML ───────────────────────────────────────────────────────

    /// Dispatch INSERT to the general ring.
    pub fn dispatch_insert(&self, payload: &[u8]) -> Result<DispatchResult, DispatchError> {
        let lsn = self.sequencer.next();
        let seq = self.ring_gen.publish(lsn, CommandType::Insert, payload)?;
        Ok(DispatchResult {
            lsn,
            seq,
            target: DispatchTarget::General,
        })
    }

    /// Dispatch BATCH INSERT to the general ring.
    pub fn dispatch_batch_insert(&self, payload: &[u8]) -> Result<DispatchResult, DispatchError> {
        let lsn = self.sequencer.next();
        let seq = self
            .ring_gen
            .publish(lsn, CommandType::BatchInsert, payload)?;
        Ok(DispatchResult {
            lsn,
            seq,
            target: DispatchTarget::General,
        })
    }

    /// Dispatch a batch UPDATE payload to the general ring.
    pub fn dispatch_batch_update(&self, payload: &[u8]) -> Result<DispatchResult, DispatchError> {
        self.dispatch_update(payload)
    }

    /// Dispatch a batch DELETE payload to the general ring.
    pub fn dispatch_batch_delete(&self, payload: &[u8]) -> Result<DispatchResult, DispatchError> {
        self.dispatch_delete(payload)
    }

    /// Dispatch UPDATE to the general ring.
    pub fn dispatch_update(&self, payload: &[u8]) -> Result<DispatchResult, DispatchError> {
        let lsn = self.sequencer.next();
        let seq = self.ring_gen.publish(lsn, CommandType::Update, payload)?;
        Ok(DispatchResult {
            lsn,
            seq,
            target: DispatchTarget::General,
        })
    }

    /// Dispatch DELETE to the general ring.
    pub fn dispatch_delete(&self, payload: &[u8]) -> Result<DispatchResult, DispatchError> {
        let lsn = self.sequencer.next();
        let seq = self.ring_gen.publish(lsn, CommandType::Delete, payload)?;
        Ok(DispatchResult {
            lsn,
            seq,
            target: DispatchTarget::General,
        })
    }

    /// Dispatch QUERY to the general ring.
    pub fn dispatch_query(&self, payload: &[u8]) -> Result<DispatchResult, DispatchError> {
        let lsn = self.sequencer.next();
        let seq = self.ring_gen.publish(lsn, CommandType::Query, payload)?;
        Ok(DispatchResult {
            lsn,
            seq,
            target: DispatchTarget::General,
        })
    }

    // ── Vector Operations ─────────────────────────────────────────

    /// Dispatch a vector operation to the vector ring.
    pub fn dispatch_vector_op(&self, payload: &[u8]) -> Result<DispatchResult, DispatchError> {
        let lsn = self.sequencer.next();
        let seq = self.ring_vec.publish(lsn, CommandType::VectorOp, payload)?;
        Ok(DispatchResult {
            lsn,
            seq,
            target: DispatchTarget::Vector,
        })
    }

    // ── Procedure Calls ───────────────────────────────────────────

    /// Dispatch a procedure call to the procedure ring.
    pub fn dispatch_procedure(&self, payload: &[u8]) -> Result<DispatchResult, DispatchError> {
        let lsn = self.sequencer.next();
        let seq = self.ring_proc.publish(lsn, CommandType::Ddl, payload)?;
        Ok(DispatchResult {
            lsn,
            seq,
            target: DispatchTarget::Procedure,
        })
    }

    // ── Batch Dispatch ────────────────────────────────────────────

    /// Dispatch multiple insert commands, each receiving a contiguous LSN.
    /// Returns results for each row. Stops on first error.
    pub fn dispatch_insert_batch_individual(
        &self,
        payloads: &[&[u8]],
    ) -> Vec<Result<DispatchResult, DispatchError>> {
        let lsn_range = self.sequencer.next_batch(payloads.len() as u64);
        let mut results = Vec::with_capacity(payloads.len());

        for (i, payload) in payloads.iter().enumerate() {
            let lsn = lsn_range.start + i as u64;
            match self.ring_gen.publish(lsn, CommandType::Insert, payload) {
                Ok(seq) => results.push(Ok(DispatchResult {
                    lsn,
                    seq,
                    target: DispatchTarget::General,
                })),
                Err(e) => results.push(Err(DispatchError::Ring(e))),
            }
        }

        results
    }

    // ── Collect Results ───────────────────────────────────────────

    /// Collect result from a completed slot in the specified ring.
    pub fn collect_result(
        &self,
        target: DispatchTarget,
        slot_idx: usize,
    ) -> Result<(u8, Vec<u8>), DispatchError> {
        let ring = match target {
            DispatchTarget::General => &self.ring_gen,
            DispatchTarget::Vector => &self.ring_vec,
            DispatchTarget::Procedure => &self.ring_proc,
        };
        ring.collect_result(slot_idx)
            .map(|(state, data)| (state as u8, data))
            .map_err(DispatchError::Ring)
    }

    // ── Diagnostics ───────────────────────────────────────────────

    /// Current LSN (highest allocated).
    pub fn current_lsn(&self) -> u64 {
        self.sequencer.current_lsn()
    }

    /// Ring directory path.
    pub fn ring_dir(&self) -> &Path {
        &self.config.ring_dir
    }

    /// Access the general ring buffer (for draining in benchmarks/tests).
    pub fn ring_general(&self) -> &SharedRingBuffer {
        &self.ring_gen
    }

    /// Access the vector ring buffer (for draining in benchmarks/tests).
    pub fn ring_vector(&self) -> &SharedRingBuffer {
        &self.ring_vec
    }

    /// Recover all three rings after crash.
    pub fn recover_all(
        &self,
    ) -> (
        super::ring_buffer::RecoveryReport,
        super::ring_buffer::RecoveryReport,
        super::ring_buffer::RecoveryReport,
    ) {
        (
            self.ring_gen.recover_after_crash(),
            self.ring_vec.recover_after_crash(),
            self.ring_proc.recover_after_crash(),
        )
    }

    /// Drain all committed slots from all rings, marking them complete.
    pub fn drain_all(&self) -> (usize, usize, usize) {
        fn drain_ring(ring: &SharedRingBuffer) -> usize {
            let mut drained = 0;
            while let Some(slot) = ring.consume() {
                ring.complete(slot.slot_idx, Some(b"ok"));
                drained += 1;
            }
            drained
        }

        (
            drain_ring(&self.ring_gen),
            drain_ring(&self.ring_vec),
            drain_ring(&self.ring_proc),
        )
    }
}

// ── PyO3 Bindings ─────────────────────────────────────────────────

/// Python-exposed Native Hub Dispatcher.
///
/// Rust dispatcher exposed to Python when the optional PyO3 feature is enabled.
/// All dispatch methods release the GIL for maximum Python-side concurrency.
#[cfg(feature = "python")]
#[pyclass(name = "NativeDispatcher")]
pub struct PyNativeDispatcher {
    inner: NativeDispatcher,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyNativeDispatcher {
    #[new]
    #[pyo3(signature = (ring_dir=None, slot_count=1024, slot_data_size=65536, timeout_ms=5000))]
    fn new(
        ring_dir: Option<&str>,
        slot_count: usize,
        slot_data_size: usize,
        timeout_ms: u64,
    ) -> PyResult<Self> {
        let config = DispatcherConfig {
            ring_dir: ring_dir
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("qm_rings_native")),
            slot_count,
            slot_data_size,
            timeout_ms,
        };
        let inner = NativeDispatcher::new(config)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok(Self { inner })
    }

    /// Dispatch DDL command. Returns (lsn, seq).
    fn dispatch_ddl(&self, payload: &[u8]) -> PyResult<(u64, u64)> {
        let r = self
            .inner
            .dispatch_ddl(payload)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok((r.lsn, r.seq))
    }

    /// Dispatch INSERT. Returns (lsn, seq).
    fn dispatch_insert(&self, payload: &[u8]) -> PyResult<(u64, u64)> {
        let r = self
            .inner
            .dispatch_insert(payload)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok((r.lsn, r.seq))
    }

    /// Dispatch BATCH INSERT. Returns (lsn, seq).
    fn dispatch_batch_insert(&self, payload: &[u8]) -> PyResult<(u64, u64)> {
        let r = self
            .inner
            .dispatch_batch_insert(payload)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok((r.lsn, r.seq))
    }

    /// Dispatch BATCH UPDATE. Returns (lsn, seq).
    fn dispatch_batch_update(&self, payload: &[u8]) -> PyResult<(u64, u64)> {
        let r = self
            .inner
            .dispatch_batch_update(payload)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok((r.lsn, r.seq))
    }

    /// Dispatch BATCH DELETE. Returns (lsn, seq).
    fn dispatch_batch_delete(&self, payload: &[u8]) -> PyResult<(u64, u64)> {
        let r = self
            .inner
            .dispatch_batch_delete(payload)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok((r.lsn, r.seq))
    }

    /// Dispatch UPDATE. Returns (lsn, seq).
    fn dispatch_update(&self, payload: &[u8]) -> PyResult<(u64, u64)> {
        let r = self
            .inner
            .dispatch_update(payload)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok((r.lsn, r.seq))
    }

    /// Dispatch DELETE. Returns (lsn, seq).
    fn dispatch_delete(&self, payload: &[u8]) -> PyResult<(u64, u64)> {
        let r = self
            .inner
            .dispatch_delete(payload)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok((r.lsn, r.seq))
    }

    /// Dispatch QUERY. Returns (lsn, seq).
    fn dispatch_query(&self, payload: &[u8]) -> PyResult<(u64, u64)> {
        let r = self
            .inner
            .dispatch_query(payload)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok((r.lsn, r.seq))
    }

    /// Dispatch vector operation. Returns (lsn, seq).
    fn dispatch_vector_op(&self, payload: &[u8]) -> PyResult<(u64, u64)> {
        let r = self
            .inner
            .dispatch_vector_op(payload)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok((r.lsn, r.seq))
    }

    /// Dispatch procedure call. Returns (lsn, seq).
    fn dispatch_procedure(&self, payload: &[u8]) -> PyResult<(u64, u64)> {
        let r = self
            .inner
            .dispatch_procedure(payload)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok((r.lsn, r.seq))
    }

    /// Current LSN.
    #[getter]
    fn current_lsn(&self) -> u64 {
        self.inner.current_lsn()
    }

    /// Ring directory path.
    #[getter]
    fn ring_dir(&self) -> String {
        self.inner.ring_dir().to_string_lossy().to_string()
    }

    /// Recover all rings after crash. Returns tuple of 3 reports:
    /// ((epoch,discarded,requeued,intact), ..., ...)
    fn recover_all(
        &self,
    ) -> (
        (u64, usize, usize, usize),
        (u64, usize, usize, usize),
        (u64, usize, usize, usize),
    ) {
        let (g, v, p) = self.inner.recover_all();
        (
            (
                g.epoch,
                g.discarded_writes,
                g.requeued_slots,
                g.intact_slots,
            ),
            (
                v.epoch,
                v.discarded_writes,
                v.requeued_slots,
                v.intact_slots,
            ),
            (
                p.epoch,
                p.discarded_writes,
                p.requeued_slots,
                p.intact_slots,
            ),
        )
    }

    /// Drain committed slots from all rings. Returns (general, vector, procedure).
    fn drain_all(&self) -> (usize, usize, usize) {
        self.inner.drain_all()
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dispatcher() -> NativeDispatcher {
        let id = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("qm_native_disp_{}_{}", std::process::id(), id));
        NativeDispatcher::new(DispatcherConfig {
            ring_dir: dir,
            slot_count: 64,
            slot_data_size: 4096,
            timeout_ms: 1000,
        })
        .unwrap()
    }

    #[test]
    fn test_dispatch_ddl() {
        let d = temp_dispatcher();
        let r = d.dispatch_ddl(b"CREATE TABLE users").unwrap();
        assert_eq!(r.lsn, 1);
        assert_eq!(r.target, DispatchTarget::General);
    }

    #[test]
    fn test_dispatch_routes_to_correct_ring() {
        let d = temp_dispatcher();
        let r1 = d.dispatch_insert(b"row data").unwrap();
        assert_eq!(r1.target, DispatchTarget::General);

        let r2 = d.dispatch_vector_op(b"vec data").unwrap();
        assert_eq!(r2.target, DispatchTarget::Vector);

        let r3 = d.dispatch_procedure(b"proc data").unwrap();
        assert_eq!(r3.target, DispatchTarget::Procedure);
    }

    #[test]
    fn test_global_lsn_ordering() {
        let d = temp_dispatcher();
        let r1 = d.dispatch_insert(b"a").unwrap();
        let r2 = d.dispatch_vector_op(b"b").unwrap();
        let r3 = d.dispatch_procedure(b"c").unwrap();
        // All three should have monotonically increasing LSNs
        assert!(r1.lsn < r2.lsn);
        assert!(r2.lsn < r3.lsn);
        assert_eq!(d.current_lsn(), 4); // next available
    }

    #[test]
    fn test_batch_individual_dispatch() {
        let d = temp_dispatcher();
        let payloads: Vec<&[u8]> = vec![b"row1", b"row2", b"row3"];
        let results = d.dispatch_insert_batch_individual(&payloads);
        assert_eq!(results.len(), 3);

        // LSNs should be contiguous
        let lsns: Vec<u64> = results.iter().map(|r| r.as_ref().unwrap().lsn).collect();
        assert_eq!(lsns[1] - lsns[0], 1);
        assert_eq!(lsns[2] - lsns[1], 1);
    }

    #[test]
    fn test_recover_all() {
        let d = temp_dispatcher();
        d.dispatch_insert(b"test").unwrap();
        let (g, _v, _p) = d.recover_all();
        // After fresh create with one publish, gen should have 1 committed slot
        assert_eq!(g.discarded_writes, 0);
    }

    #[test]
    fn test_concurrent_dispatch() {
        use std::sync::Arc;
        use std::thread;

        let d = Arc::new(temp_dispatcher());
        let mut handles = Vec::new();

        for _ in 0..4 {
            let d = Arc::clone(&d);
            handles.push(thread::spawn(move || {
                for _ in 0..10 {
                    d.dispatch_insert(b"data").unwrap();
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // All 40 dispatches should have unique LSNs
        assert_eq!(d.current_lsn(), 41);
    }
}
