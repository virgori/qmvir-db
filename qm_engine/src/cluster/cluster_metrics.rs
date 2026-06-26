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
static PEER_BITS: LazyLock<Mutex<HashMap<String, AtomicU64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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
}

pub fn set_standby_wal_lsn(lsn: u64) {
    WAL_STANDBY_LSN.store(lsn, Ordering::Relaxed);
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

pub fn set_peer_up(addr: SocketAddr, up: bool) {
    let key = peer_key(addr);
    let mut map = PEER_BITS.lock().expect("peer metrics lock");
    let entry = map
        .entry(key)
        .or_insert_with(|| AtomicU64::new(0));
    entry.store(if up { 1 } else { 0 }, Ordering::Relaxed);
}

pub fn render_prometheus() -> String {
    let mut out = String::with_capacity(512);
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
