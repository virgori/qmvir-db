//! HTAP integration — MVCC visibility, column sync, planner.

use qm_engine::gateway::NativeSqlEngine;
use qm_engine::htap::planner::ScanPath;
use qm_engine::htap::{materialize_pitr_data_dir, wal_lines_to_replay};

fn setup() -> NativeSqlEngine {
    let e = NativeSqlEngine::new();
    e.execute("CREATE TABLE htap_t (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    e.execute("INSERT INTO htap_t (id, val) VALUES (1, 10)")
        .unwrap();
    e
}

#[test]
fn htap_mvcc_read_your_writes_then_rollback() {
    let e = setup();
    e.execute("BEGIN").unwrap();
    e.execute("INSERT INTO htap_t (id, val) VALUES (2, 20)")
        .unwrap();
    assert!(e.htap_visible_row("htap_t", 2).is_some());
    e.execute("ROLLBACK").unwrap();
    assert!(e.htap_visible_row("htap_t", 2).is_none());
}

#[test]
fn htap_commit_publishes_version() {
    let e = setup();
    e.execute("BEGIN").unwrap();
    e.execute("INSERT INTO htap_t (id, val) VALUES (3, 30)")
        .unwrap();
    e.execute("COMMIT").unwrap();
    assert!(e.htap_visible_row("htap_t", 3).is_some());
}

#[test]
fn htap_autocommit_uses_real_tx() {
    let e = setup();
    e.execute("INSERT INTO htap_t (id, val) VALUES (4, 40)")
        .unwrap();
    assert!(e.htap_visible_row("htap_t", 4).is_some());
    assert_eq!(e.mvcc_active_transaction_count(), 0);
}

#[test]
fn htap_autocommit_delete_hides_row() {
    let e = setup();
    e.execute("INSERT INTO htap_t (id, val) VALUES (5, 50)")
        .unwrap();
    assert!(e.htap_visible_row("htap_t", 5).is_some());
    e.execute("DELETE FROM htap_t WHERE id = 5").unwrap();
    assert!(e.htap_visible_row("htap_t", 5).is_none());
}

#[test]
fn htap_planner_picks_column_scan_for_analytics() {
    let planner = qm_engine::htap::HtapPlanner::new();
    let plan = planner.plan_sql(
        "SELECT SUM(val) FROM htap_t WHERE id BETWEEN 1 AND 1000",
        5000,
        true,
    );
    assert_eq!(plan.path, ScanPath::ColumnScan);
}

#[test]
fn htap_explain_uses_planner() {
    let e = setup();
    let r = e
        .execute("EXPLAIN SELECT SUM(val) FROM htap_t WHERE id BETWEEN 1 AND 100")
        .unwrap();
    let plan = String::from_utf8_lossy(r.rows[0][0].as_ref().unwrap());
    assert!(plan.contains("Column Scan") || plan.contains("Seq Scan"));
}

#[test]
fn htap_pitr_wal_lines() {
    let dir = std::env::temp_dir().join(format!(
        "qm_pitr_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let e = NativeSqlEngine::with_data_dir(dir.clone());
    e.execute("CREATE TABLE w (id INTEGER PRIMARY KEY)")
        .unwrap();
    e.execute("INSERT INTO w (id) VALUES (1)").unwrap();
    e.execute("INSERT INTO w (id) VALUES (2)").unwrap();
    let lines = wal_lines_to_replay(&dir, 100).expect("wal lines");
    assert!(lines.iter().any(|l| l.to_ascii_uppercase().contains("CREATE TABLE")));
    assert!(lines.iter().any(|l| l.contains("INSERT INTO")));
    let manifest = qm_engine::htap::restore_to_timestamp(&dir, i64::MAX / 2, None);
    assert!(manifest.is_ok());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn htap_pitr_materialize_truncates_wal() {
    let src = std::env::temp_dir().join(format!(
        "qm_pitr_src_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let dst = std::env::temp_dir().join(format!(
        "qm_pitr_dst_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let e = NativeSqlEngine::with_data_dir(src.clone());
    e.execute("CREATE TABLE p (id INTEGER PRIMARY KEY)")
        .unwrap();
    e.execute("INSERT INTO p (id) VALUES (1)").unwrap();
    e.execute("INSERT INTO p (id) VALUES (2)").unwrap();
    let manifest = materialize_pitr_data_dir(&src, &dst, 2).expect("materialize");
    assert_eq!(manifest.target_lsn, 2);
    let restored = NativeSqlEngine::with_data_dir(dst.clone());
    assert!(restored.htap_visible_row("p", 1).is_some());
    assert!(restored.htap_visible_row("p", 2).is_none());
    let _ = std::fs::remove_dir_all(src);
    let _ = std::fs::remove_dir_all(dst);
}

#[test]
fn htap_autocommit_update_visible() {
    let e = setup();
    e.execute("UPDATE htap_t SET val = 11 WHERE id = 1")
        .unwrap();
    let r = e
        .execute("SELECT val FROM htap_t WHERE id = 1")
        .unwrap();
    let val = String::from_utf8_lossy(r.rows[0][0].as_ref().unwrap());
    assert_eq!(val, "11");
}

#[test]
fn htap_rollback_restores_prior_version() {
    let e = setup();
    e.execute("BEGIN").unwrap();
    e.execute("UPDATE htap_t SET val = 99 WHERE id = 1")
        .unwrap();
    let r = e
        .execute("SELECT val FROM htap_t WHERE id = 1")
        .unwrap();
    let val = String::from_utf8_lossy(r.rows[0][0].as_ref().unwrap());
    assert_eq!(val, "99");
    e.execute("ROLLBACK").unwrap();
    let r = e
        .execute("SELECT val FROM htap_t WHERE id = 1")
        .unwrap();
    let val = String::from_utf8_lossy(r.rows[0][0].as_ref().unwrap());
    assert_eq!(val, "10");
}

#[test]
fn htap_visible_row_ids_respects_delete() {
    let e = setup();
    e.execute("INSERT INTO htap_t (id, val) VALUES (6, 60)")
        .unwrap();
    let before = e.htap_visible_row_ids("htap_t");
    assert!(before.contains(&6));
    e.execute("DELETE FROM htap_t WHERE id = 6").unwrap();
    let after = e.htap_visible_row_ids("htap_t");
    assert!(!after.contains(&6));
}

#[test]
fn htap_certify_functional_gate() {
    let report = qm_engine::htap::evaluate_htap_certification(true, true, true, true, false);
    assert!(report.htap_functional);
    assert!(!report.htap_performance);
}

#[test]
fn htap_isolation_battery_passes() {
    let e = setup();
    let report = qm_engine::htap::run_htap_isolation_battery(&e, "htap_t");
    assert!(report.passed, "{:?}", report.violations);
}

#[test]
fn htap_durable_column_sum_on_disk() {
    let dir = std::env::temp_dir().join(format!(
        "qm_cseg_e2e_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let e = NativeSqlEngine::with_data_dir(dir.clone());
    e.execute("CREATE TABLE m (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    for chunk in 0..41 {
        let mut vals = String::new();
        for i in 0..100 {
            let id = chunk * 100 + i + 1;
            vals.push_str(&format!("({id},{id}),"));
        }
        vals.pop();
        e.execute(&format!("INSERT INTO m (id, val) VALUES {vals}"))
            .unwrap();
    }
    assert!(e.htap.is_column_dirty("m") || e.htap.column_segments.has_any_for_table("m"));
    let r = e.execute("SELECT SUM(val) FROM m").unwrap();
    assert!(e.htap.column_segments.has_any_for_table("m"));
    let sum: i64 = String::from_utf8_lossy(r.rows[0][0].as_ref().unwrap())
        .parse()
        .unwrap();
    let expected: i64 = 4100 * 4101 / 2;
    assert_eq!(sum, expected);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn htap_durable_sum_between_on_id() {
    let dir = std::env::temp_dir().join(format!(
        "qm_cseg_bt_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let e = NativeSqlEngine::with_data_dir(dir.clone());
    e.execute("CREATE TABLE m (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    for chunk in 0..41 {
        let mut vals = String::new();
        for i in 0..100 {
            let id = chunk * 100 + i + 1;
            vals.push_str(&format!("({id},{id}),"));
        }
        vals.pop();
        e.execute(&format!("INSERT INTO m (id, val) VALUES {vals}"))
            .unwrap();
    }
    let r = e
        .execute("SELECT SUM(val) FROM m WHERE id BETWEEN 100 AND 199")
        .unwrap();
    let sum: i64 = String::from_utf8_lossy(r.rows[0][0].as_ref().unwrap())
        .parse()
        .unwrap();
    assert_eq!(sum, 14950);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn htap_vector_knn_top1_stable() {
    let e = NativeSqlEngine::new();
    e.execute("CREATE TABLE v (id INTEGER PRIMARY KEY, emb VECTOR(3))")
        .unwrap();
    e.execute("INSERT INTO v (id, emb) VALUES (1, '[1,0,0]')")
        .unwrap();
    e.execute("INSERT INTO v (id, emb) VALUES (2, '[0,1,0]')")
        .unwrap();
    e.execute("CREATE INDEX idx_v_hnsw ON v (emb) USING hnsw")
        .unwrap();
    let r = e
        .execute("SELECT id FROM v ORDER BY emb <-> '[0.9,0.1,0]' LIMIT 1")
        .unwrap();
    let id = String::from_utf8_lossy(r.rows[0][0].as_ref().unwrap());
    assert_eq!(id, "1", "nearest to [0.9,0.1,0] should be id=1 [1,0,0]");
    // Exact scan fallback on tiny table must match
    let r2 = e
        .execute("SELECT id FROM v ORDER BY emb <-> '[0.9,0.1,0]' LIMIT 2")
        .unwrap();
    assert_eq!(r2.rows.len(), 2);
}

#[test]
fn htap_isolation_history_checker_smoke() {
    // Jepsen-style: committed write visible after commit; aborted invisible.
    let e = setup();
    e.execute("BEGIN").unwrap();
    e.execute("INSERT INTO htap_t (id, val) VALUES (99, 990)")
        .unwrap();
    e.execute("ROLLBACK").unwrap();
    let visible: Vec<i64> = (1..=100)
        .filter(|id| e.htap_visible_row("htap_t", *id).is_some())
        .collect();
    assert!(!visible.contains(&99));
    assert!(visible.contains(&1));
}
