/*
 * Phase L — WAL catch-up buffer for gap heal after partition.
 */

use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};

use super::transport::WalEntry;

const MAX_BUFFER: usize = 4096;

static WAL_RING: LazyLock<Mutex<VecDeque<WalEntry>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));

pub fn record_shipped(entry: &WalEntry) {
    let mut ring = WAL_RING.lock().expect("wal ring lock");
    ring.push_back(entry.clone());
    while ring.len() > MAX_BUFFER {
        ring.pop_front();
    }
}

/// Entries with LSN strictly greater than `after_lsn`.
pub fn entries_after_lsn(after_lsn: u64) -> Vec<WalEntry> {
    let ring = WAL_RING.lock().expect("wal ring lock");
    ring.iter().filter(|e| e.lsn > after_lsn).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(lsn: u64) -> WalEntry {
        WalEntry {
            lsn,
            sql: format!("INSERT INTO t VALUES ({lsn})"),
            checksum: 0,
        }
    }

    #[test]
    fn ring_returns_gap_fill_range() {
        let base = 900_000u64;
        record_shipped(&entry(base + 1));
        record_shipped(&entry(base + 2));
        record_shipped(&entry(base + 3));
        let gap = entries_after_lsn(base + 1);
        assert_eq!(gap.len(), 2);
        assert_eq!(gap[0].lsn, base + 2);
    }
}
