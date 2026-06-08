use qm_engine::gateway::native_sql::NativeSqlEngine;
use std::path::Path;
use std::process::Command;

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

fn engine_at(path: &Path) -> NativeSqlEngine {
    NativeSqlEngine::with_data_dir(path.to_path_buf())
}

fn run_child(case_name: &str, data_dir: &Path, row_id: Option<i64>) {
    let exe = std::env::current_exe().expect("current test executable");
    let mut cmd = Command::new(exe);
    cmd.env("QM_CRASH_CHILD_CASE", case_name)
        .env("QM_CRASH_CHILD_DATA_DIR", data_dir)
        .arg("--exact")
        .arg("crash_child_entry")
        .arg("--nocapture");
    if let Some(row_id) = row_id {
        cmd.env("QM_CRASH_CHILD_ROW_ID", row_id.to_string());
    }
    let status = cmd.status().expect("spawn crash child");
    assert!(
        !status.success(),
        "crash child should terminate abruptly for case {case_name}"
    );
}

#[test]
fn crash_child_entry() {
    let Ok(case_name) = std::env::var("QM_CRASH_CHILD_CASE") else {
        return;
    };
    let data_dir = std::env::var("QM_CRASH_CHILD_DATA_DIR").expect("child data dir");
    let engine = NativeSqlEngine::with_data_dir(data_dir.into());

    match case_name.as_str() {
        "committed_tx" => {
            engine
                .execute("CREATE TABLE crash_kill (id INTEGER PRIMARY KEY, name TEXT)")
                .unwrap();
            engine.execute("BEGIN").unwrap();
            engine
                .execute("INSERT INTO crash_kill (id, name) VALUES (1, 'committed')")
                .unwrap();
            engine.execute("COMMIT").unwrap();
        }
        "uncommitted_tx" => {
            engine
                .execute("CREATE TABLE crash_kill (id INTEGER PRIMARY KEY, name TEXT)")
                .unwrap();
            engine.execute("BEGIN").unwrap();
            engine
                .execute("INSERT INTO crash_kill (id, name) VALUES (1, 'uncommitted')")
                .unwrap();
        }
        "many_before_commit" => {
            engine.set_wal_sync_policy("per_commit").unwrap();
            engine
                .execute("CREATE TABLE crash_kill (id INTEGER PRIMARY KEY, name TEXT)")
                .unwrap();
            engine.execute("BEGIN").unwrap();
            for id in 1..=25 {
                engine
                    .execute(&format!(
                        "INSERT INTO crash_kill (id, name) VALUES ({id}, 'pending-{id}')"
                    ))
                    .unwrap();
            }
        }
        "many_after_commit" => {
            engine.set_wal_sync_policy("per_commit").unwrap();
            engine
                .execute("CREATE TABLE crash_kill (id INTEGER PRIMARY KEY, name TEXT)")
                .unwrap();
            engine.execute("BEGIN").unwrap();
            for id in 1..=25 {
                engine
                    .execute(&format!(
                        "INSERT INTO crash_kill (id, name) VALUES ({id}, 'committed-{id}')"
                    ))
                    .unwrap();
            }
            engine.execute("COMMIT").unwrap();
            assert!(engine.wal_sync_count() > 0);
        }
        "checkpoint" => {
            engine
                .execute("CREATE TABLE crash_kill (id INTEGER PRIMARY KEY, name TEXT)")
                .unwrap();
            engine
                .execute("INSERT INTO crash_kill (id, name) VALUES (1, 'checkpointed')")
                .unwrap();
            engine.checkpoint();
        }
        "delete_before_checkpoint" => {
            engine
                .execute("CREATE TABLE crash_kill (id INTEGER PRIMARY KEY, name TEXT)")
                .unwrap();
            engine
                .execute("INSERT INTO crash_kill (id, name) VALUES (1, 'delete-me')")
                .unwrap();
            engine.checkpoint();
            engine
                .execute("DELETE FROM crash_kill WHERE id = 1")
                .unwrap();
        }
        "wal_replay_delta" => {
            engine
                .execute("CREATE TABLE crash_kill (id INTEGER PRIMARY KEY, name TEXT)")
                .unwrap();
            engine
                .execute("INSERT INTO crash_kill (id, name) VALUES (1, 'snapshot')")
                .unwrap();
            engine.checkpoint();
            engine
                .execute("INSERT INTO crash_kill (id, name) VALUES (2, 'wal-delta')")
                .unwrap();
        }
        "repeated_cycle" => {
            let row_id: i64 = std::env::var("QM_CRASH_CHILD_ROW_ID")
                .expect("row id")
                .parse()
                .expect("row id integer");
            engine
                .execute(&format!(
                    "INSERT INTO crash_kill (id, name) VALUES ({row_id}, 'cycle-{row_id}')"
                ))
                .unwrap();
        }
        "committed_generated_id" => {
            engine.set_wal_sync_policy("per_commit").unwrap();
            engine
                .execute("CREATE TABLE crash_auto (id INTEGER PRIMARY KEY, name TEXT)")
                .unwrap();
            engine.execute("BEGIN").unwrap();
            engine
                .execute("INSERT INTO crash_auto (name) VALUES ('committed')")
                .unwrap();
            engine.execute("COMMIT").unwrap();
            assert!(engine.wal_sync_count() > 0);
        }
        "failed_insert_before_wal_append" => {
            engine
                .execute("CREATE TABLE crash_failed (id INTEGER PRIMARY KEY, name TEXT UNIQUE)")
                .unwrap();
            engine
                .execute("INSERT INTO crash_failed (name) VALUES ('ok')")
                .unwrap();
            assert!(engine
                .execute("INSERT INTO crash_failed (name) VALUES ('ok')")
                .is_err());
        }
        "invalid_uuid" => {
            engine
                .execute("CREATE TABLE crash_uuid (id INTEGER PRIMARY KEY, u UUID)")
                .unwrap();
            assert!(engine
                .execute("INSERT INTO crash_uuid (u) VALUES ('not-a-uuid')")
                .is_err());
        }
        "invalid_json" => {
            engine
                .execute("CREATE TABLE crash_json (id INTEGER PRIMARY KEY, data JSON)")
                .unwrap();
            assert!(engine
                .execute(r#"INSERT INTO crash_json (data) VALUES ('{"bad":}')"#)
                .is_err());
        }
        "committed_uuid_json" => {
            engine.set_wal_sync_policy("per_commit").unwrap();
            engine
                .execute("CREATE TABLE crash_uuid_json (id INTEGER PRIMARY KEY, u UUID, data JSON)")
                .unwrap();
            engine.execute("BEGIN").unwrap();
            engine
                .execute(
                    r#"INSERT INTO crash_uuid_json (u, data) VALUES ('550E8400-E29B-41D4-A716-446655440000', '{"ok":true}')"#,
                )
                .unwrap();
            engine.execute("COMMIT").unwrap();
            assert!(engine.wal_sync_count() > 0);
        }
        "rolled_back_uuid_json" => {
            engine
                .execute("CREATE TABLE crash_uuid_json (id INTEGER PRIMARY KEY, u UUID, data JSON)")
                .unwrap();
            engine.execute("BEGIN").unwrap();
            engine
                .execute(
                    r#"INSERT INTO crash_uuid_json (u, data) VALUES ('550e8400-e29b-41d4-a716-446655440001', '{"rolled":true}')"#,
                )
                .unwrap();
            engine.execute("ROLLBACK").unwrap();
        }
        "committed_serial_identity" => {
            engine.set_wal_sync_policy("per_commit").unwrap();
            engine
                .execute("CREATE TABLE crash_serial (id SERIAL PRIMARY KEY, name TEXT)")
                .unwrap();
            engine.execute("BEGIN").unwrap();
            engine
                .execute("INSERT INTO crash_serial (name) VALUES ('committed')")
                .unwrap();
            engine.execute("COMMIT").unwrap();
            assert!(engine.wal_sync_count() > 0);
        }
        "committed_jsonb_containment_update" => {
            engine.set_wal_sync_policy("per_commit").unwrap();
            engine
                .execute("CREATE TABLE crash_jsonb_contains (id INTEGER PRIMARY KEY, data JSONB)")
                .unwrap();
            engine
                .execute(
                    r#"INSERT INTO crash_jsonb_contains (data) VALUES ('{"role":"admin","user":{"name":"alice"}}')"#,
                )
                .unwrap();
            engine.execute("BEGIN").unwrap();
            engine
                .execute(
                    r#"UPDATE crash_jsonb_contains SET data = '{"role":"user","user":{"name":"alice"}}' WHERE id = 1"#,
                )
                .unwrap();
            engine.execute("COMMIT").unwrap();
            assert!(engine.wal_sync_count() > 0);
        }
        "committed_indexed_update_delete" => {
            engine.set_wal_sync_policy("per_commit").unwrap();
            engine
                .execute("CREATE TABLE crash_ud_idx (id INTEGER PRIMARY KEY, status TEXT, score INTEGER, data JSONB)")
                .unwrap();
            engine
                .execute("CREATE INDEX idx_crash_ud_status ON crash_ud_idx(status)")
                .unwrap();
            engine
                .execute("CREATE INDEX idx_crash_ud_score ON crash_ud_idx(score)")
                .unwrap();
            engine
                .execute("CREATE INDEX idx_crash_ud_data ON crash_ud_idx(data)")
                .unwrap();
            engine
                .execute(r#"INSERT INTO crash_ud_idx (id, status, score, data) VALUES (1, 'old', 10, '{"role":"admin","user":{"name":"alice"}}')"#)
                .unwrap();
            engine
                .execute(r#"INSERT INTO crash_ud_idx (id, status, score, data) VALUES (2, 'delete', 20, '{"role":"user","user":{"name":"bob"}}')"#)
                .unwrap();
            engine.execute("BEGIN").unwrap();
            engine
                .execute(r#"UPDATE crash_ud_idx SET status = 'new', score = 30, data = '{"role":"reviewer","user":{"name":"alice"}}' WHERE id = 1"#)
                .unwrap();
            engine
                .execute("DELETE FROM crash_ud_idx WHERE id = 2")
                .unwrap();
            engine.execute("COMMIT").unwrap();
            assert!(engine.wal_sync_count() > 0);
        }
        other => panic!("unknown crash child case {other}"),
    }

    std::process::abort();
}

#[test]
fn committed_transaction_survives_process_abort() {
    let tmp = tempfile::TempDir::new().unwrap();
    run_child("committed_tx", tmp.path(), None);

    let engine = engine_at(tmp.path());
    assert_eq!(
        first_cell(&engine, "SELECT name FROM crash_kill WHERE id = 1"),
        "committed"
    );
}

#[test]
fn uncommitted_transaction_is_not_visible_after_process_abort() {
    let tmp = tempfile::TempDir::new().unwrap();
    run_child("uncommitted_tx", tmp.path(), None);

    let engine = engine_at(tmp.path());
    assert_eq!(first_cell(&engine, "SELECT COUNT(*) FROM crash_kill"), "0");
}

#[test]
fn crash_before_commit_marker_does_not_recover_transaction_body() {
    let tmp = tempfile::TempDir::new().unwrap();
    run_child("many_before_commit", tmp.path(), None);

    let engine = engine_at(tmp.path());
    assert_eq!(first_cell(&engine, "SELECT COUNT(*) FROM crash_kill"), "0");
}

#[test]
fn crash_after_commit_marker_recovers_transaction_body() {
    let tmp = tempfile::TempDir::new().unwrap();
    run_child("many_after_commit", tmp.path(), None);

    let engine = engine_at(tmp.path());
    assert_eq!(first_cell(&engine, "SELECT COUNT(*) FROM crash_kill"), "25");
    assert_eq!(
        first_cell(&engine, "SELECT name FROM crash_kill WHERE id = 25"),
        "committed-25"
    );
}

#[test]
fn checkpoint_survives_process_abort() {
    let tmp = tempfile::TempDir::new().unwrap();
    run_child("checkpoint", tmp.path(), None);

    let engine = engine_at(tmp.path());
    assert_eq!(
        first_cell(&engine, "SELECT name FROM crash_kill WHERE id = 1"),
        "checkpointed"
    );
}

#[test]
fn delete_before_checkpoint_survives_wal_replay_after_process_abort() {
    let tmp = tempfile::TempDir::new().unwrap();
    run_child("delete_before_checkpoint", tmp.path(), None);

    let engine = engine_at(tmp.path());
    assert_eq!(first_cell(&engine, "SELECT COUNT(*) FROM crash_kill"), "0");
}

#[test]
fn wal_delta_after_checkpoint_replays_after_process_abort() {
    let tmp = tempfile::TempDir::new().unwrap();
    run_child("wal_replay_delta", tmp.path(), None);

    let engine = engine_at(tmp.path());
    assert_eq!(first_cell(&engine, "SELECT COUNT(*) FROM crash_kill"), "2");
    assert_eq!(
        first_cell(&engine, "SELECT name FROM crash_kill WHERE id = 2"),
        "wal-delta"
    );
}

#[test]
fn repeated_open_write_abort_reopen_cycles_recover() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let engine = engine_at(tmp.path());
        engine
            .execute("CREATE TABLE crash_kill (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        engine.checkpoint();
    }

    for row_id in 1..=5 {
        run_child("repeated_cycle", tmp.path(), Some(row_id));
        let engine = engine_at(tmp.path());
        assert_eq!(
            first_cell(
                &engine,
                &format!("SELECT name FROM crash_kill WHERE id = {row_id}")
            ),
            format!("cycle-{row_id}")
        );
    }

    let engine = engine_at(tmp.path());
    assert_eq!(first_cell(&engine, "SELECT COUNT(*) FROM crash_kill"), "5");
}

#[test]
fn committed_generated_id_survives_abort_and_counter_continues() {
    let tmp = tempfile::TempDir::new().unwrap();
    run_child("committed_generated_id", tmp.path(), None);

    let engine = engine_at(tmp.path());
    assert_eq!(
        first_cell(
            &engine,
            "SELECT id FROM crash_auto WHERE name = 'committed'"
        ),
        "1"
    );
    engine
        .execute("INSERT INTO crash_auto (name) VALUES ('after_recovery')")
        .unwrap();
    assert_eq!(
        first_cell(
            &engine,
            "SELECT id FROM crash_auto WHERE name = 'after_recovery'"
        ),
        "2"
    );
}

#[test]
fn failed_insert_before_wal_append_is_not_replayed_after_abort() {
    let tmp = tempfile::TempDir::new().unwrap();
    run_child("failed_insert_before_wal_append", tmp.path(), None);

    let engine = engine_at(tmp.path());
    assert_eq!(
        first_cell(
            &engine,
            "SELECT COUNT(*) FROM crash_failed WHERE name = 'ok'"
        ),
        "1"
    );
}

#[test]
fn invalid_uuid_insert_does_not_appear_after_abort_reopen() {
    let tmp = tempfile::TempDir::new().unwrap();
    run_child("invalid_uuid", tmp.path(), None);

    let engine = engine_at(tmp.path());
    assert_eq!(first_cell(&engine, "SELECT COUNT(*) FROM crash_uuid"), "0");
}

#[test]
fn invalid_json_insert_does_not_appear_after_abort_reopen() {
    let tmp = tempfile::TempDir::new().unwrap();
    run_child("invalid_json", tmp.path(), None);

    let engine = engine_at(tmp.path());
    assert_eq!(first_cell(&engine, "SELECT COUNT(*) FROM crash_json"), "0");
}

#[test]
fn committed_uuid_and_json_survive_abort_recovery() {
    let tmp = tempfile::TempDir::new().unwrap();
    run_child("committed_uuid_json", tmp.path(), None);

    let engine = engine_at(tmp.path());
    assert_eq!(
        rows(&engine, "SELECT u, data FROM crash_uuid_json WHERE id = 1"),
        vec![vec![
            Some("550e8400-e29b-41d4-a716-446655440000".to_string()),
            Some(r#"{"ok":true}"#.to_string())
        ]]
    );
}

#[test]
fn rolled_back_uuid_and_json_do_not_reappear_after_abort_reopen() {
    let tmp = tempfile::TempDir::new().unwrap();
    run_child("rolled_back_uuid_json", tmp.path(), None);

    let engine = engine_at(tmp.path());
    assert_eq!(
        first_cell(&engine, "SELECT COUNT(*) FROM crash_uuid_json"),
        "0"
    );
}

#[test]
fn committed_serial_identity_survives_abort_and_sequence_continues() {
    let tmp = tempfile::TempDir::new().unwrap();
    run_child("committed_serial_identity", tmp.path(), None);

    let engine = engine_at(tmp.path());
    assert_eq!(
        first_cell(
            &engine,
            "SELECT id FROM crash_serial WHERE name = 'committed'"
        ),
        "1"
    );
    engine
        .execute("INSERT INTO crash_serial (name) VALUES ('after_recovery')")
        .unwrap();
    assert_eq!(
        first_cell(
            &engine,
            "SELECT id FROM crash_serial WHERE name = 'after_recovery'"
        ),
        "2"
    );
}

#[test]
fn committed_jsonb_containment_update_survives_abort_recovery() {
    let tmp = tempfile::TempDir::new().unwrap();
    run_child("committed_jsonb_containment_update", tmp.path(), None);

    let engine = engine_at(tmp.path());
    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT COUNT(*) FROM crash_jsonb_contains WHERE data @> '{"role":"admin"}'"#
        ),
        "0"
    );
    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT COUNT(*) FROM crash_jsonb_contains WHERE data @> '{"role":"user"}'"#
        ),
        "1"
    );
    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT COUNT(*) FROM crash_jsonb_contains WHERE data @> '{"user":{"name":"alice"}}'"#
        ),
        "1"
    );
}

#[test]
fn committed_indexed_update_delete_survives_abort_recovery() {
    let tmp = tempfile::TempDir::new().unwrap();
    run_child("committed_indexed_update_delete", tmp.path(), None);

    let engine = engine_at(tmp.path());
    assert_eq!(
        first_cell(&engine, "SELECT COUNT(*) FROM crash_ud_idx WHERE id = 2"),
        "0"
    );
    assert_eq!(
        first_cell(
            &engine,
            "SELECT COUNT(*) FROM crash_ud_idx WHERE status = 'old'"
        ),
        "0"
    );
    assert_eq!(
        rows(
            &engine,
            r#"SELECT id FROM crash_ud_idx WHERE status = 'new'"#
        ),
        vec![vec![Some("1".to_string())]]
    );
    assert_eq!(
        rows(&engine, r#"SELECT id FROM crash_ud_idx WHERE score = 30"#),
        vec![vec![Some("1".to_string())]]
    );
    assert_eq!(
        rows(
            &engine,
            r#"SELECT id FROM crash_ud_idx WHERE data @> '{"role":"reviewer"}'"#
        ),
        vec![vec![Some("1".to_string())]]
    );
    engine.validate_secondary_indexes().unwrap();
}
