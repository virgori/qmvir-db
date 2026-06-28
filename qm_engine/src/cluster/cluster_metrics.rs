/*
 * Phase J — cluster HA metrics (Prometheus text, lock-free atomics).
 */

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

static FAILOVER_TOTAL: AtomicU64 = AtomicU64::new(0);
static FAILOVER_SKIPPED: AtomicU64 = AtomicU64::new(0);
static FORWARD_ERRORS: AtomicU64 = AtomicU64::new(0);
static WAL_PRIMARY_LSN: AtomicU64 = AtomicU64::new(0);
static WAL_STANDBY_LSN: AtomicU64 = AtomicU64::new(0);
static FAILOVER_RTO_LAST_MS: AtomicU64 = AtomicU64::new(0);
static FAILOVER_RTO_MAX_MS: AtomicU64 = AtomicU64::new(0);
static FAILOVER_RTO_SUM_MS: AtomicU64 = AtomicU64::new(0);
static FAILOVER_RTO_COUNT: AtomicU64 = AtomicU64::new(0);
static PEER_BITS: LazyLock<Mutex<HashMap<String, AtomicU64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

const RTO_BUCKETS: [u64; 5] = [10, 50, 100, 1000, u64::MAX];
static RTO_BUCKET_COUNTS: LazyLock<Mutex<[u64; 5]>> = LazyLock::new(|| Mutex::new([0; 5]));

const LAG_SAMPLE_CAP: usize = 256;
static WAL_LAG_SAMPLES_MS: LazyLock<Mutex<Vec<u64>>> = LazyLock::new(|| Mutex::new(Vec::new()));

fn peer_key(addr: SocketAddr) -> String {
    addr.to_string()
}

pub fn inc_failover() {
    FAILOVER_TOTAL.fetch_add(1, Ordering::Relaxed);
}

pub fn inc_failover_skipped() {
    FAILOVER_SKIPPED.fetch_add(1, Ordering::Relaxed);
}

pub fn inc_forward_errors() {
    FORWARD_ERRORS.fetch_add(1, Ordering::Relaxed);
}

pub fn set_primary_wal_lsn(lsn: u64) {
    WAL_PRIMARY_LSN.store(lsn, Ordering::Relaxed);
    record_wal_lag_sample(wal_lag_lsn());
}

pub fn set_standby_wal_lsn(lsn: u64) {
    WAL_STANDBY_LSN.store(lsn, Ordering::Relaxed);
    record_wal_lag_sample(wal_lag_lsn());
}

pub fn wal_lag_lsn() -> u64 {
    WAL_PRIMARY_LSN
        .load(Ordering::Relaxed)
        .saturating_sub(WAL_STANDBY_LSN.load(Ordering::Relaxed))
}

pub fn primary_wal_lsn_metric() -> u64 {
    WAL_PRIMARY_LSN.load(Ordering::Relaxed)
}

pub fn standby_wal_lsn_metric() -> u64 {
    WAL_STANDBY_LSN.load(Ordering::Relaxed)
}

/// Record observed failover promotion latency (milliseconds).
pub fn record_failover_rto_ms(ms: u64) {
    FAILOVER_RTO_LAST_MS.store(ms, Ordering::Relaxed);
    FAILOVER_RTO_SUM_MS.fetch_add(ms, Ordering::Relaxed);
    FAILOVER_RTO_COUNT.fetch_add(1, Ordering::Relaxed);
    let mut max = FAILOVER_RTO_MAX_MS.load(Ordering::Relaxed);
    while ms > max {
        match FAILOVER_RTO_MAX_MS.compare_exchange_weak(max, ms, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(cur) => max = cur,
        }
    }
    if let Ok(mut buckets) = RTO_BUCKET_COUNTS.lock() {
        for (i, le) in RTO_BUCKETS.iter().enumerate() {
            if ms <= *le {
                buckets[i] += 1;
                break;
            }
        }
    }
}

pub fn failover_rto_last_ms() -> u64 {
    FAILOVER_RTO_LAST_MS.load(Ordering::Relaxed)
}

pub fn failover_rto_max_ms() -> u64 {
    FAILOVER_RTO_MAX_MS.load(Ordering::Relaxed)
}

/// Record WAL lag sample (LSN units treated as ms-scale for SLA dashboards).
pub fn record_wal_lag_sample(lag: u64) {
    if let Ok(mut samples) = WAL_LAG_SAMPLES_MS.lock() {
        samples.push(lag);
        if samples.len() > LAG_SAMPLE_CAP {
            let drain = samples.len() - LAG_SAMPLE_CAP;
            samples.drain(0..drain);
        }
    }
}

pub fn wal_lag_p99_ms() -> u64 {
    let Ok(mut samples) = WAL_LAG_SAMPLES_MS.lock() else {
        return 0;
    };
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    let idx = ((samples.len() as f64) * 0.99).ceil() as usize;
    samples[idx.saturating_sub(1)]
}

pub fn set_peer_up(addr: SocketAddr, up: bool) {
    let key = peer_key(addr);
    let mut map = PEER_BITS.lock().expect("peer metrics lock");
    let entry = map
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0));
    entry.store(if up { 1 } else { 0 }, Ordering::Relaxed);
}

pub fn render_prometheus() -> String {
    let mut out = String::with_capacity(1024);
    out.push_str("# HELP qmvir_cluster_failover_total Failover promotions applied\n");
    out.push_str("# TYPE qmvir_cluster_failover_total counter\n");
    out.push_str(&format!(
        "qmvir_cluster_failover_total {}\n",
        FAILOVER_TOTAL.load(Ordering::Relaxed)
    ));
    out.push_str("# HELP qmvir_cluster_failover_skipped_total Failover attempts without standby\n");
    out.push_str("# TYPE qmvir_cluster_failover_skipped_total counter\n");
    out.push_str(&format!(
        "qmvir_cluster_failover_skipped_total {}\n",
        FAILOVER_SKIPPED.load(Ordering::Relaxed)
    ));
    out.push_str("# HELP qmvir_cluster_forward_errors_total Remote forward failures\n");
    out.push_str("# TYPE qmvir_cluster_forward_errors_total counter\n");
    out.push_str(&format!(
        "qmvir_cluster_forward_errors_total {}\n",
        FORWARD_ERRORS.load(Ordering::Relaxed)
    ));
    out.push_str("# HELP qmvir_cluster_wal_primary_lsn Primary shipped WAL LSN high-water\n");
    out.push_str("# TYPE qmvir_cluster_wal_primary_lsn gauge\n");
    out.push_str(&format!(
        "qmvir_cluster_wal_primary_lsn {}\n",
        WAL_PRIMARY_LSN.load(Ordering::Relaxed)
    ));
    out.push_str("# HELP qmvir_cluster_wal_standby_lsn Standby applied WAL LSN high-water\n");
    out.push_str("# TYPE qmvir_cluster_wal_standby_lsn gauge\n");
    out.push_str(&format!(
        "qmvir_cluster_wal_standby_lsn {}\n",
        WAL_STANDBY_LSN.load(Ordering::Relaxed)
    ));
    out.push_str("# HELP qmvir_cluster_wal_lag_lsn WAL replication lag in LSN units\n");
    out.push_str("# TYPE qmvir_cluster_wal_lag_lsn gauge\n");
    out.push_str(&format!("qmvir_cluster_wal_lag_lsn {}\n", wal_lag_lsn()));
    out.push_str("# HELP qmvir_cluster_wal_lag_p99 Rolling p99 WAL lag sample\n");
    out.push_str("# TYPE qmvir_cluster_wal_lag_p99 gauge\n");
    out.push_str(&format!("qmvir_cluster_wal_lag_p99 {}\n", wal_lag_p99_ms()));

    let rto_count = FAILOVER_RTO_COUNT.load(Ordering::Relaxed);
    let rto_sum = FAILOVER_RTO_SUM_MS.load(Ordering::Relaxed);
    out.push_str("# HELP qmvir_cluster_failover_rto_ms Failover promotion latency ms\n");
    out.push_str("# TYPE qmvir_cluster_failover_rto_ms histogram\n");
    if let Ok(buckets) = RTO_BUCKET_COUNTS.lock() {
        let mut cumulative = 0u64;
        for (i, le) in RTO_BUCKETS.iter().enumerate() {
            cumulative += buckets[i];
            let le_label = if *le == u64::MAX {
                "+Inf".to_string()
            } else {
                le.to_string()
            };
            out.push_str(&format!(
                "qmvir_cluster_failover_rto_ms_bucket{{le=\"{le_label}\"}} {cumulative}\n"
            ));
        }
    }
    out.push_str(&format!("qmvir_cluster_failover_rto_ms_sum {rto_sum}\n"));
    out.push_str(&format!("qmvir_cluster_failover_rto_ms_count {rto_count}\n"));
    out.push_str("# HELP qmvir_cluster_failover_rto_last_ms Last observed failover RTO ms\n");
    out.push_str("# TYPE qmvir_cluster_failover_rto_last_ms gauge\n");
    out.push_str(&format!(
        "qmvir_cluster_failover_rto_last_ms {}\n",
        FAILOVER_RTO_LAST_MS.load(Ordering::Relaxed)
    ));
    out.push_str("# HELP qmvir_cluster_failover_rto_max_ms Max observed failover RTO ms\n");
    out.push_str("# TYPE qmvir_cluster_failover_rto_max_ms gauge\n");
    out.push_str(&format!(
        "qmvir_cluster_failover_rto_max_ms {}\n",
        FAILOVER_RTO_MAX_MS.load(Ordering::Relaxed)
    ));

    let map = PEER_BITS.lock().expect("peer metrics lock");
    out.push_str("# HELP qmvir_cluster_peer_up Peer reachability (1=up)\n");
    out.push_str("# TYPE qmvir_cluster_peer_up gauge\n");
    for (addr, bit) in map.iter() {
        out.push_str(&format!(
            "qmvir_cluster_peer_up{{addr=\"{addr}\"}} {}\n",
            bit.load(Ordering::Relaxed)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wal_lag_p99_from_samples() {
        for v in [1, 10, 100, 200, 500] {
            record_wal_lag_sample(v);
        }
        assert!(wal_lag_p99_ms() >= 200);
    }
}
