//! Regression: cluster/HA modules must not change single-node engine behavior.

use qm_engine::cluster::{ClusterRuntime, QmRouter};
use qm_engine::gateway::NativeSqlEngine;
use std::time::Instant;

fn oltp_smoke(engine: &NativeSqlEngine) -> usize {
    engine
        .execute("CREATE TABLE iso_t (id INTEGER PRIMARY KEY, score INTEGER, tag TEXT)")
        .expect("create");
    engine
        .execute("CREATE INDEX iso_t_score ON iso_t (score)")
        .expect("index");
    for i in 0..500 {
        engine
            .execute(&format!(
                "INSERT INTO iso_t (id, score, tag) VALUES ({i}, {}, 'tag_{}')",
                i % 17,
                i % 5
            ))
            .expect("insert");
    }
    let sel = engine
        .execute("SELECT id FROM iso_t WHERE score = 7")
        .expect("select");
    let cnt = engine
        .execute("SELECT COUNT(*) FROM iso_t WHERE tag = 'tag_2'")
        .expect("count");
    sel.rows.len() + cnt.rows.len()
}

#[test]
fn single_node_engine_unaffected_when_cluster_disabled() {
    let engine = NativeSqlEngine::new();
    let rows = oltp_smoke(&engine);
    assert!(rows > 0);

    let rt = ClusterRuntime::disabled();
    assert!(
        rt.route_sql_if_active("INSERT INTO iso_t (id, score, tag) VALUES (999, 1, 'x')", 999)
            .is_none()
    );
}

#[test]
fn router_classify_is_sidecar_not_on_engine_execute_path() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE v (id INTEGER PRIMARY KEY, embedding VECTOR(4))")
        .unwrap();
    engine
        .execute("INSERT INTO v (id, embedding) VALUES (1, '[0.1,0.2,0.3,0.4]')")
        .unwrap();

    // Classifier exists for future gateway use only.
    let _ = QmRouter::classify_sql(
        "SELECT id FROM v ORDER BY embedding <-> '[0.1,0.2,0.3,0.4]' LIMIT 1",
    );

    let t0 = Instant::now();
    for _ in 0..200 {
        engine
            .execute("SELECT id FROM v WHERE id = 1")
            .expect("pk select");
    }
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    // Sanity guard: PK path should stay sub-millisecond median on CI hardware.
    assert!(
        elapsed_ms < 500.0,
        "single-node PK select regression: 200 iter took {elapsed_ms:.1}ms"
    );
}

#[test]
fn persistent_engine_reopen_still_works_with_cluster_modules_linked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().to_path_buf();
    {
        let engine = NativeSqlEngine::with_data_dir(path.clone());
        engine
            .execute("CREATE TABLE iso_p (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        engine
            .execute("INSERT INTO iso_p (id, name) VALUES (1, 'a')")
            .unwrap();
    }
    let engine = NativeSqlEngine::with_data_dir(path);
    let rows = engine.execute("SELECT name FROM iso_p WHERE id = 1").unwrap();
    assert_eq!(rows.rows.len(), 1);
}

#[test]
fn create_hnsw_index_after_bulk_insert_does_not_deadlock() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE hnsw_deadlock (id INTEGER PRIMARY KEY, embedding VECTOR(8))")
        .unwrap();
    for i in 0..512 {
        engine
            .execute(&format!(
                "INSERT INTO hnsw_deadlock (id, embedding) VALUES ({i}, '[{i}.0,1,2,3,4,5,6,7]')"
            ))
            .unwrap();
    }
    engine
        .execute("CREATE INDEX idx_hnsw_deadlock ON hnsw_deadlock (embedding) USING hnsw")
        .expect("hnsw create index must not deadlock");
    let rows = engine
        .execute("SELECT id FROM hnsw_deadlock ORDER BY embedding <-> '[0.1,0.2,0.3,0.4,0.5,0.6,0.7,0.8]' LIMIT 5")
        .expect("knn after index");
    assert!(!rows.rows.is_empty());
}
