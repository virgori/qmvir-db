//! MVCC isolation history checker — Jepsen-style lite invariants for HTAP.

use crate::gateway::NativeSqlEngine;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationViolation {
    pub check: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub struct IsolationReport {
    pub passed: bool,
    pub violations: Vec<IsolationViolation>,
}

impl IsolationReport {
    pub fn ok() -> Self {
        Self {
            passed: true,
            violations: Vec::new(),
        }
    }

    pub fn fail(check: &'static str, detail: impl Into<String>) -> Self {
        Self {
            passed: false,
            violations: vec![IsolationViolation {
                check,
                detail: detail.into(),
            }],
        }
    }

    pub fn merge(mut self, other: Self) -> Self {
        self.passed &= other.passed;
        self.violations.extend(other.violations);
        self
    }
}

/// Read-your-writes: uncommitted insert visible in-txn, invisible after rollback.
pub fn check_read_your_writes(engine: &NativeSqlEngine, table: &str, row_id: i64) -> IsolationReport {
    let mut report = IsolationReport::ok();

    if let Err(e) = engine.execute("BEGIN") {
        return IsolationReport::fail("begin", e);
    }
    let insert = format!("INSERT INTO {table} (id, val) VALUES ({row_id}, 1)");
    if let Err(e) = engine.execute(&insert) {
        let _ = engine.execute("ROLLBACK");
        return IsolationReport::fail("insert_in_txn", e);
    }
    if engine.htap_visible_row(table, row_id).is_none() {
        report = report.merge(IsolationReport::fail(
            "read_your_writes",
            format!("row {row_id} not visible inside open transaction"),
        ));
    }
    if let Err(e) = engine.execute("ROLLBACK") {
        return report.merge(IsolationReport::fail("rollback", e));
    }
    if engine.htap_visible_row(table, row_id).is_some() {
        report = report.merge(IsolationReport::fail(
            "abort_invisible",
            format!("row {row_id} still visible after ROLLBACK"),
        ));
    }
    report
}

/// Committed writes persist; no active tx leaks after COMMIT.
pub fn check_commit_atomicity(engine: &NativeSqlEngine, table: &str, row_id: i64) -> IsolationReport {
    if let Err(e) = engine.execute("BEGIN") {
        return IsolationReport::fail("begin", e);
    }
    let insert = format!("INSERT INTO {table} (id, val) VALUES ({row_id}, 2)");
    if let Err(e) = engine.execute(&insert) {
        let _ = engine.execute("ROLLBACK");
        return IsolationReport::fail("insert_in_txn", e);
    }
    if let Err(e) = engine.execute("COMMIT") {
        return IsolationReport::fail("commit", e);
    }
    if engine.mvcc_active_transaction_count() != 0 {
        return IsolationReport::fail(
            "no_leaked_txn",
            format!(
                "active tx count {} after COMMIT",
                engine.mvcc_active_transaction_count()
            ),
        );
    }
    if engine.htap_visible_row(table, row_id).is_none() {
        return IsolationReport::fail(
            "commit_visible",
            format!("row {row_id} missing after COMMIT"),
        );
    }
    IsolationReport::ok()
}

/// Autocommit path uses real tx_mgr (implicit commit), not WAL-only fake commit.
pub fn check_autocommit_real_tx(engine: &NativeSqlEngine, table: &str, row_id: i64) -> IsolationReport {
    let before = engine.mvcc_active_transaction_count();
    let insert = format!("INSERT INTO {table} (id, val) VALUES ({row_id}, 3)");
    if let Err(e) = engine.execute(&insert) {
        return IsolationReport::fail("autocommit_insert", e);
    }
    let after = engine.mvcc_active_transaction_count();
    if after != before {
        return IsolationReport::fail(
            "autocommit_no_leak",
            format!("active tx {after}, expected {before}"),
        );
    }
    if engine.htap_visible_row(table, row_id).is_none() {
        return IsolationReport::fail(
            "autocommit_visible",
            format!("row {row_id} not visible after autocommit INSERT"),
        );
    }
    IsolationReport::ok()
}

/// Run the standard HTAP isolation battery against a prepared engine.
pub fn run_htap_isolation_battery(engine: &NativeSqlEngine, table: &str) -> IsolationReport {
    check_read_your_writes(engine, table, 9101)
        .merge(check_commit_atomicity(engine, table, 9102))
        .merge(check_autocommit_real_tx(engine, table, 9103))
}
