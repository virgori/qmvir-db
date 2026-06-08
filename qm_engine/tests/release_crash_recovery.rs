use qm_engine::gateway::native_sql::NativeSqlEngine;
use qm_engine::storage::snapshot::{DirtyTracker, PageProvider, SnapshotManager};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;

fn engine_at(dir: &Path) -> NativeSqlEngine {
    NativeSqlEngine::with_data_dir(dir.to_path_buf())
}

fn rows(engine: &NativeSqlEngine, sql: &str) -> Vec<Vec<Option<String>>> {
    engine
        .execute(sql)
        .expect(sql)
        .rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|cell| cell.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
                .collect()
        })
        .collect()
}

fn first_cell(engine: &NativeSqlEngine, sql: &str) -> String {
    rows(engine, sql)[0][0].clone().unwrap_or_default()
}

#[test]
fn native_sql_committed_transaction_survives_restart() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let engine = engine_at(tmp.path());
        engine
            .execute("CREATE TABLE release_crash (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        engine.execute("BEGIN").unwrap();
        engine
            .execute("INSERT INTO release_crash (id, name) VALUES (1, 'committed')")
            .unwrap();
        engine.execute("COMMIT").unwrap();
    }

    let engine = engine_at(tmp.path());
    assert_eq!(
        first_cell(&engine, "SELECT name FROM release_crash WHERE id = 1"),
        "committed"
    );
}

#[test]
fn native_sql_uncommitted_transaction_is_not_replayed_after_restart() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let engine = engine_at(tmp.path());
        engine
            .execute("CREATE TABLE release_crash (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        engine.execute("BEGIN").unwrap();
        engine
            .execute("INSERT INTO release_crash (id, name) VALUES (1, 'uncommitted')")
            .unwrap();
        // Simulated crash: drop the engine without COMMIT or ROLLBACK. The
        // transaction body must not have been written to WAL.
    }

    let engine = engine_at(tmp.path());
    assert_eq!(
        first_cell(&engine, "SELECT COUNT(*) FROM release_crash"),
        "0"
    );
}

#[test]
fn native_sql_rollback_state_survives_restart() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let engine = engine_at(tmp.path());
        engine
            .execute("CREATE TABLE release_crash (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        engine
            .execute("INSERT INTO release_crash (id, name) VALUES (1, 'before')")
            .unwrap();
        engine.execute("BEGIN").unwrap();
        engine
            .execute("UPDATE release_crash SET name = 'after' WHERE id = 1")
            .unwrap();
        engine.execute("ROLLBACK").unwrap();
    }

    let engine = engine_at(tmp.path());
    assert_eq!(
        first_cell(&engine, "SELECT name FROM release_crash WHERE id = 1"),
        "before"
    );
}

#[test]
fn native_sql_many_row_transaction_commit_survives_restart() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let engine = engine_at(tmp.path());
        engine.set_wal_sync_policy("per_commit").unwrap();
        engine
            .execute("CREATE TABLE tx_many (id INTEGER PRIMARY KEY, v INTEGER)")
            .unwrap();
        engine.execute("BEGIN").unwrap();
        for id in 1..=50 {
            engine
                .execute(&format!("INSERT INTO tx_many (id, v) VALUES ({id}, {id})"))
                .unwrap();
        }
        engine.execute("COMMIT").unwrap();
        assert!(engine.wal_sync_count() > 0);
    }

    let engine = engine_at(tmp.path());
    assert_eq!(first_cell(&engine, "SELECT COUNT(*) FROM tx_many"), "50");
    assert_eq!(
        first_cell(&engine, "SELECT v FROM tx_many WHERE id = 50"),
        "50"
    );
}

#[test]
fn native_sql_many_row_transaction_update_delete_survives_restart() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let engine = engine_at(tmp.path());
        engine.set_wal_sync_policy("per_commit").unwrap();
        engine
            .execute("CREATE TABLE tx_many (id INTEGER PRIMARY KEY, v INTEGER)")
            .unwrap();
        for id in 1..=50 {
            engine
                .execute(&format!("INSERT INTO tx_many (id, v) VALUES ({id}, {id})"))
                .unwrap();
        }
        engine.execute("BEGIN").unwrap();
        for id in 1..=25 {
            engine
                .execute(&format!(
                    "UPDATE tx_many SET v = {} WHERE id = {id}",
                    id + 100
                ))
                .unwrap();
        }
        for id in 26..=50 {
            engine
                .execute(&format!("DELETE FROM tx_many WHERE id = {id}"))
                .unwrap();
        }
        engine.execute("COMMIT").unwrap();
        assert!(engine.wal_sync_count() > 0);
    }

    let engine = engine_at(tmp.path());
    assert_eq!(first_cell(&engine, "SELECT COUNT(*) FROM tx_many"), "25");
    assert_eq!(
        first_cell(&engine, "SELECT v FROM tx_many WHERE id = 25"),
        "125"
    );
    assert_eq!(
        first_cell(&engine, "SELECT COUNT(*) FROM tx_many WHERE id = 50"),
        "0"
    );
}

#[test]
fn native_sql_many_row_transaction_rollback_absent_after_restart() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let engine = engine_at(tmp.path());
        engine.set_wal_sync_policy("per_commit").unwrap();
        engine
            .execute("CREATE TABLE tx_many (id INTEGER PRIMARY KEY, v INTEGER)")
            .unwrap();
        engine.execute("BEGIN").unwrap();
        for id in 1..=50 {
            engine
                .execute(&format!("INSERT INTO tx_many (id, v) VALUES ({id}, {id})"))
                .unwrap();
        }
        engine.execute("ROLLBACK").unwrap();
    }

    let engine = engine_at(tmp.path());
    assert_eq!(first_cell(&engine, "SELECT COUNT(*) FROM tx_many"), "0");
}

#[test]
fn native_sql_checkpoint_reload_does_not_resurrect_deleted_rows() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let engine = engine_at(tmp.path());
        engine
            .execute("CREATE TABLE release_crash (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        engine
            .execute("INSERT INTO release_crash (id, name) VALUES (1, 'keep')")
            .unwrap();
        engine
            .execute("INSERT INTO release_crash (id, name) VALUES (2, 'delete')")
            .unwrap();
        engine.checkpoint();
        engine
            .execute("DELETE FROM release_crash WHERE id = 2")
            .unwrap();
        engine.checkpoint();
    }

    let engine = engine_at(tmp.path());
    assert_eq!(
        first_cell(&engine, "SELECT COUNT(*) FROM release_crash"),
        "1"
    );
    assert_eq!(
        first_cell(&engine, "SELECT name FROM release_crash WHERE id = 1"),
        "keep"
    );
}

#[test]
fn native_sql_secondary_index_state_survives_recovery() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let engine = engine_at(tmp.path());
        engine
            .execute("CREATE TABLE release_crash (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        engine
            .execute("INSERT INTO release_crash (id, name) VALUES (1, 'alice')")
            .unwrap();
        engine
            .execute("INSERT INTO release_crash (id, name) VALUES (2, 'bob')")
            .unwrap();
        engine
            .execute("CREATE INDEX idx_release_crash_name ON release_crash (name)")
            .unwrap();
        engine.checkpoint();
    }

    let engine = engine_at(tmp.path());
    let index_names: HashSet<String> = engine
        .index_manager()
        .meta
        .read()
        .values()
        .map(|idx| idx.name.clone())
        .collect();
    assert!(index_names.contains("idx_release_crash_name"));
    assert_eq!(
        first_cell(&engine, "SELECT id FROM release_crash WHERE name = 'bob'"),
        "2"
    );
}

#[test]
fn native_sql_mvcc_visibility_metadata_is_reset_after_restart() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let engine = engine_at(tmp.path());
        engine
            .execute("CREATE TABLE release_crash (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        engine.execute("BEGIN").unwrap();
        assert_eq!(engine.mvcc_active_transaction_count(), 1);
    }

    let engine = engine_at(tmp.path());
    assert_eq!(engine.mvcc_active_transaction_count(), 0);
    assert_eq!(
        first_cell(&engine, "SELECT COUNT(*) FROM release_crash"),
        "0"
    );
}

struct MapPageProvider {
    pages: BTreeMap<u64, Vec<u8>>,
}

impl PageProvider for MapPageProvider {
    fn read_page(&self, page_id: u64) -> Option<Vec<u8>> {
        self.pages.get(&page_id).cloned()
    }
}

#[test]
fn snapshot_missing_dirty_page_fails_fast() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tracker = Arc::new(DirtyTracker::new());
    let manager = SnapshotManager::new(tmp.path(), Arc::clone(&tracker), 10).unwrap();
    tracker.mark_dirty(42);
    tracker.advance_lsn();

    let err = manager
        .take_snapshot(&MapPageProvider {
            pages: BTreeMap::new(),
        })
        .expect_err("missing dirty page must fail snapshot creation");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("dirty page 42 missing"));
}
