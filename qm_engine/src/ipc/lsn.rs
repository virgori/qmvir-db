//! Lock-free LSN (Log Sequence Number) Sequencer
//!
//! Provides monotonic, lock-free sequence numbers for WAL ordering and IPC.

use std::sync::atomic::{AtomicU64, Ordering};

/// Lock-free monotonic LSN sequencer.
///
/// Thread-safe: multiple writers can call `next()` concurrently without locks.
/// Uses `fetch_add` with `AcqRel` ordering for correctness.
pub struct LsnSequencer {
    /// Current LSN — monotonically increasing
    current: AtomicU64,
    /// Persisted LSN — the highest LSN confirmed durable on disk
    persisted: AtomicU64,
}

impl LsnSequencer {
    /// Create a new sequencer starting from `start_lsn`.
    pub fn new(start_lsn: u64) -> Self {
        Self {
            current: AtomicU64::new(start_lsn),
            persisted: AtomicU64::new(start_lsn),
        }
    }

    /// Allocate a single LSN — lock-free, monotonic.
    #[inline]
    pub fn next(&self) -> u64 {
        self.current.fetch_add(1, Ordering::AcqRel)
    }

    /// Allocate N contiguous LSNs for batch operations.
    /// Returns the range `start..start+count`.
    #[inline]
    pub fn next_batch(&self, count: u64) -> std::ops::Range<u64> {
        let start = self.current.fetch_add(count, Ordering::AcqRel);
        start..start + count
    }

    /// Current (latest allocated) LSN — may not yet be persisted.
    #[inline]
    pub fn current_lsn(&self) -> u64 {
        self.current.load(Ordering::Acquire)
    }

    /// Mark a LSN as persisted (called after WAL flush).
    /// Uses `fetch_max` to handle out-of-order completions safely.
    pub fn mark_persisted(&self, lsn: u64) {
        self.persisted.fetch_max(lsn, Ordering::Release);
    }

    /// The highest LSN that has been durably flushed.
    #[inline]
    pub fn persisted_lsn(&self) -> u64 {
        self.persisted.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sequential_lsn() {
        let seq = LsnSequencer::new(100);
        assert_eq!(seq.next(), 100);
        assert_eq!(seq.next(), 101);
        assert_eq!(seq.next(), 102);
        assert_eq!(seq.current_lsn(), 103);
    }

    #[test]
    fn test_batch_lsn() {
        let seq = LsnSequencer::new(0);
        let range = seq.next_batch(5);
        assert_eq!(range, 0..5);
        assert_eq!(seq.current_lsn(), 5);
        let range2 = seq.next_batch(3);
        assert_eq!(range2, 5..8);
    }

    #[test]
    fn test_persisted_tracking() {
        let seq = LsnSequencer::new(0);
        seq.next(); // 0
        seq.next(); // 1
        seq.next(); // 2
        assert_eq!(seq.persisted_lsn(), 0);
        seq.mark_persisted(1);
        assert_eq!(seq.persisted_lsn(), 1);
        // Out-of-order: mark 0 after 1 should keep 1
        seq.mark_persisted(0);
        assert_eq!(seq.persisted_lsn(), 1);
        seq.mark_persisted(2);
        assert_eq!(seq.persisted_lsn(), 2);
    }

    #[test]
    fn test_concurrent_lsn() {
        use std::sync::Arc;
        use std::thread;

        let seq = Arc::new(LsnSequencer::new(0));
        let mut handles = Vec::new();

        for _ in 0..8 {
            let seq = Arc::clone(&seq);
            handles.push(thread::spawn(move || {
                let mut lsns = Vec::new();
                for _ in 0..1000 {
                    lsns.push(seq.next());
                }
                lsns
            }));
        }

        let mut all_lsns: Vec<u64> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        all_lsns.sort();
        all_lsns.dedup();

        // All 8000 LSNs must be unique
        assert_eq!(all_lsns.len(), 8000);
        assert_eq!(seq.current_lsn(), 8000);
    }
}
