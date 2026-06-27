/*
 * Phase K — write fencing via monotonic cluster epoch.
 *
 * Prevents stale primaries from accepting writes after failover promote.
 * Env: QM_CLUSTER_FENCING=1 (recommended with QM_CLUSTER_FAILOVER=1)
 */

use std::sync::atomic::{AtomicU64, Ordering};

static CLUSTER_EPOCH: AtomicU64 = AtomicU64::new(1);

pub fn current_epoch() -> u64 {
    CLUSTER_EPOCH.load(Ordering::Acquire)
}

    pub fn set_epoch(epoch: u64) {
        CLUSTER_EPOCH.store(epoch, Ordering::Release);
    }

    pub fn bump_epoch() -> u64 {
    CLUSTER_EPOCH.fetch_add(1, Ordering::AcqRel) + 1
}

/// Returns true when `incoming` is current (or fencing disabled path passes 0).
pub fn accept_epoch(incoming: u64, fencing_enabled: bool) -> bool {
    if !fencing_enabled {
        return true;
    }
    if incoming == 0 {
        // Legacy clients before epoch wire — accept only when epoch is 1.
        return current_epoch() <= 1;
    }
    incoming >= current_epoch()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_rejects_stale_epoch() {
        let _ = bump_epoch();
        let cur = current_epoch();
        assert!(accept_epoch(cur, true));
        assert!(!accept_epoch(cur - 1, true));
    }
}
