use qm_engine::gateway::native_sql::NativeSqlEngine;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

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

fn prepared_rows(
    engine: &NativeSqlEngine,
    plan_id: u64,
    params: Vec<String>,
) -> Vec<Vec<Option<String>>> {
    engine
        .execute_prepared(plan_id, params)
        .unwrap()
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

fn sorted_ids(engine: &NativeSqlEngine, sql: &str) -> Vec<String> {
    let mut ids: Vec<String> = rows(engine, sql)
        .into_iter()
        .map(|row| row[0].clone().unwrap())
        .collect();
    ids.sort();
    ids
}

fn sorted_prepared_ids(engine: &NativeSqlEngine, plan_id: u64, params: Vec<String>) -> Vec<String> {
    let mut ids: Vec<String> = prepared_rows(engine, plan_id, params)
        .into_iter()
        .map(|row| row[0].clone().unwrap())
        .collect();
    ids.sort();
    ids
}

fn execute_without_panic_is_err(engine: &NativeSqlEngine, sql: &str) -> bool {
    catch_unwind(AssertUnwindSafe(|| engine.execute(sql)))
        .unwrap_or_else(|_| panic!("SQL panicked instead of returning Result: {sql}"))
        .is_err()
}

fn assert_executes_without_panic(engine: &NativeSqlEngine, sql: &str) {
    let _ = catch_unwind(AssertUnwindSafe(|| engine.execute(sql)))
        .unwrap_or_else(|_| panic!("SQL panicked instead of returning Result: {sql}"));
}

fn create_query_table(engine: &NativeSqlEngine, table: &str, with_indexes: bool) {
    engine
        .execute(&format!(
            "CREATE TABLE {table} (id INTEGER PRIMARY KEY, tenant INTEGER, score INTEGER, status TEXT, uid UUID, raw JSON, data JSONB)"
        ))
        .unwrap();
    if with_indexes {
        engine
            .execute(&format!(
                "CREATE INDEX idx_{table}_tenant ON {table}(tenant)"
            ))
            .unwrap();
        engine
            .execute(&format!("CREATE INDEX idx_{table}_score ON {table}(score)"))
            .unwrap();
        engine
            .execute(&format!(
                "CREATE INDEX idx_{table}_status ON {table}(status)"
            ))
            .unwrap();
        engine
            .execute(&format!("CREATE INDEX idx_{table}_uid ON {table}(uid)"))
            .unwrap();
        engine
            .execute(&format!("CREATE INDEX idx_{table}_data ON {table}(data)"))
            .unwrap();
    }

    let rows = [
        (
            1,
            1,
            10,
            "active",
            "550e8400-e29b-41d4-a716-446655440001",
            r#"{"raw":1}"#,
            r#"{"role":"admin","user":{"name":"alice"},"tags":["red","blue"],"n":1}"#,
        ),
        (
            2,
            1,
            20,
            "active",
            "550e8400-e29b-41d4-a716-446655440002",
            r#"{"raw":2}"#,
            r#"{"role":"user","user":{"name":"bob"},"tags":["blue"],"n":1.0}"#,
        ),
        (
            3,
            2,
            30,
            "archived",
            "550e8400-e29b-41d4-a716-446655440003",
            r#"{"raw":3}"#,
            r#"{"role":"admin","user":{"name":"carol"},"flags":{"beta":true}}"#,
        ),
        (
            4,
            2,
            40,
            "active",
            "550e8400-e29b-41d4-a716-446655440004",
            r#"{"raw":4}"#,
            r#"{"user":{"name":"alice"},"missing_role":true}"#,
        ),
        (
            5,
            3,
            50,
            "deleted",
            "550e8400-e29b-41d4-a716-446655440005",
            r#"{"raw":5}"#,
            r#"{"role":null,"user":{"name":null}}"#,
        ),
    ];

    for (id, tenant, score, status, uid, raw, data) in rows {
        engine
            .execute(&format!(
                "INSERT INTO {table} (id, tenant, score, status, uid, raw, data) VALUES ({id}, {tenant}, {score}, '{status}', '{uid}', '{raw}', '{data}')"
            ))
            .unwrap();
    }
}

#[test]
fn query_shapes_match_full_scan_indexed_prepared_and_reopen_paths() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let engine = engine_at(tmp.path());
        engine.set_wal_sync_policy("per_commit").unwrap();
        create_query_table(&engine, "query_scan", false);
        create_query_table(&engine, "query_indexed", true);

        let unordered_queries = [
            (
                "SELECT id FROM {table} WHERE status = 'active'",
                vec!["1", "2", "4"],
            ),
            (
                "SELECT id FROM {table} WHERE score BETWEEN 20 AND 40",
                vec!["2", "3", "4"],
            ),
            (
                "SELECT id FROM {table} WHERE score >= 20 AND score <= 40",
                vec!["2", "3", "4"],
            ),
            (
                "SELECT id FROM {table} WHERE status = 'active' AND tenant = 1",
                vec!["1", "2"],
            ),
            (
                "SELECT id FROM {table} WHERE status = 'active' OR tenant = 2",
                vec!["1", "2", "3", "4"],
            ),
            (
                "SELECT id FROM {table} WHERE data ->> 'role' = 'admin'",
                vec!["1", "3"],
            ),
            (
                "SELECT id FROM {table} WHERE data #>> '{user,name}' = 'alice'",
                vec!["1", "4"],
            ),
            (
                "SELECT id FROM {table} WHERE data ? 'role'",
                vec!["1", "2", "3", "5"],
            ),
            (
                "SELECT id FROM {table} WHERE data @> '{\"role\":\"admin\"}'",
                vec!["1", "3"],
            ),
            (
                "SELECT id FROM {table} WHERE uid = '550e8400-e29b-41d4-a716-446655440003'",
                vec!["3"],
            ),
        ];

        for (template, expected) in unordered_queries {
            let scan_sql = template.replace("{table}", "query_scan");
            let indexed_sql = template.replace("{table}", "query_indexed");
            let mut expected_ids: Vec<String> = expected.into_iter().map(String::from).collect();
            expected_ids.sort();
            assert_eq!(sorted_ids(&engine, &scan_sql), expected_ids, "{scan_sql}");
            assert_eq!(
                sorted_ids(&engine, &indexed_sql),
                expected_ids,
                "{indexed_sql}"
            );
            assert_eq!(
                sorted_ids(&engine, &scan_sql),
                sorted_ids(&engine, &indexed_sql),
                "{template}"
            );
        }

        assert_eq!(
            rows(
                &engine,
                "SELECT id FROM query_indexed WHERE status = 'active' ORDER BY score DESC LIMIT 2"
            ),
            vec![vec![Some("4".to_string())], vec![Some("2".to_string())]]
        );
        assert_eq!(
            first_cell(
                &engine,
                "SELECT COUNT(*) FROM query_indexed WHERE status = 'active' OR tenant = 2"
            ),
            "4"
        );

        let prepared_status = engine
            .prepare("SELECT id FROM query_indexed WHERE status = $1")
            .unwrap();
        assert_eq!(
            sorted_prepared_ids(&engine, prepared_status, vec!["active".into()]),
            vec!["1", "2", "4"]
        );
        let prepared_uid = engine
            .prepare("SELECT id FROM query_indexed WHERE uid = $1")
            .unwrap();
        assert_eq!(
            sorted_prepared_ids(
                &engine,
                prepared_uid,
                vec!["550e8400-e29b-41d4-a716-446655440003".into()]
            ),
            vec!["3"]
        );

        engine.execute("BEGIN").unwrap();
        engine
            .execute("UPDATE query_indexed SET status = 'pending' WHERE id = 1")
            .unwrap();
        assert_eq!(
            sorted_ids(
                &engine,
                "SELECT id FROM query_indexed WHERE status = 'pending'"
            ),
            vec!["1"]
        );
        engine.execute("ROLLBACK").unwrap();
        assert!(sorted_ids(
            &engine,
            "SELECT id FROM query_indexed WHERE status = 'pending'"
        )
        .is_empty());
        assert_eq!(
            sorted_ids(
                &engine,
                "SELECT id FROM query_indexed WHERE status = 'active'"
            ),
            vec!["1", "2", "4"]
        );
        engine.validate_secondary_indexes().unwrap();
    }

    {
        let engine = engine_at(tmp.path());
        engine.set_wal_sync_policy("per_commit").unwrap();
        assert_eq!(
            sorted_ids(
                &engine,
                "SELECT id FROM query_indexed WHERE data @> '{\"role\":\"admin\"}'"
            ),
            vec!["1", "3"]
        );
        assert_eq!(
            rows(
                &engine,
                "SELECT id FROM query_indexed WHERE status = 'active' ORDER BY score DESC LIMIT 2"
            ),
            vec![vec![Some("4".to_string())], vec![Some("2".to_string())]]
        );
        engine.validate_secondary_indexes().unwrap();
    }
}

#[test]
fn update_delete_paths_preserve_indexes_rollback_and_reopen() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let engine = engine_at(tmp.path());
        engine.set_wal_sync_policy("per_commit").unwrap();
        create_query_table(&engine, "mut_audit", true);

        engine
            .execute("UPDATE mut_audit SET tenant = 9 WHERE id = 2")
            .unwrap();
        assert!(
            sorted_ids(&engine, "SELECT id FROM mut_audit WHERE tenant = 1")
                .into_iter()
                .all(|id| id != "2")
        );
        assert_eq!(
            sorted_ids(&engine, "SELECT id FROM mut_audit WHERE tenant = 9"),
            vec!["2"]
        );

        engine
            .execute(
                "UPDATE mut_audit SET uid = '550e8400-e29b-41d4-a716-446655449999' WHERE id = 3",
            )
            .unwrap();
        assert!(sorted_ids(
            &engine,
            "SELECT id FROM mut_audit WHERE uid = '550e8400-e29b-41d4-a716-446655440003'"
        )
        .is_empty());
        assert_eq!(
            sorted_ids(
                &engine,
                "SELECT id FROM mut_audit WHERE uid = '550e8400-e29b-41d4-a716-446655449999'"
            ),
            vec!["3"]
        );

        engine
            .execute(
                "UPDATE mut_audit SET data = '{\"role\":\"reviewer\",\"user\":{\"name\":\"alice\"}}' WHERE data @> '{\"role\":\"admin\"}'",
            )
            .unwrap();
        assert!(sorted_ids(
            &engine,
            "SELECT id FROM mut_audit WHERE data @> '{\"role\":\"admin\"}'"
        )
        .is_empty());
        assert_eq!(
            sorted_ids(
                &engine,
                "SELECT id FROM mut_audit WHERE data @> '{\"role\":\"reviewer\"}'"
            ),
            vec!["1", "3"]
        );

        engine
            .execute("UPDATE mut_audit SET status = 'range' WHERE score BETWEEN 20 AND 40")
            .unwrap();
        assert_eq!(
            sorted_ids(&engine, "SELECT id FROM mut_audit WHERE status = 'range'"),
            vec!["2", "3", "4"]
        );
        engine.validate_secondary_indexes().unwrap();

        engine.execute("BEGIN").unwrap();
        engine
            .execute("UPDATE mut_audit SET status = 'rolled' WHERE id = 1")
            .unwrap();
        assert_eq!(
            sorted_ids(&engine, "SELECT id FROM mut_audit WHERE status = 'rolled'"),
            vec!["1"]
        );
        engine
            .execute("DELETE FROM mut_audit WHERE data @> '{\"role\":\"reviewer\"}'")
            .unwrap();
        assert!(sorted_ids(&engine, "SELECT id FROM mut_audit WHERE status = 'rolled'").is_empty());
        assert!(sorted_ids(
            &engine,
            "SELECT id FROM mut_audit WHERE data @> '{\"role\":\"reviewer\"}'"
        )
        .is_empty());
        engine.execute("ROLLBACK").unwrap();
        assert!(sorted_ids(&engine, "SELECT id FROM mut_audit WHERE status = 'rolled'").is_empty());
        assert_eq!(
            sorted_ids(
                &engine,
                "SELECT id FROM mut_audit WHERE data @> '{\"role\":\"reviewer\"}'"
            ),
            vec!["1", "3"]
        );
        engine.validate_secondary_indexes().unwrap();

        engine
            .execute("DELETE FROM mut_audit WHERE tenant = 9")
            .unwrap();
        assert!(sorted_ids(&engine, "SELECT id FROM mut_audit WHERE id = 2").is_empty());
        engine
            .execute("DELETE FROM mut_audit WHERE data @> '{\"role\":\"reviewer\"}'")
            .unwrap();
        assert!(sorted_ids(
            &engine,
            "SELECT id FROM mut_audit WHERE data @> '{\"role\":\"reviewer\"}'"
        )
        .is_empty());
        engine.validate_secondary_indexes().unwrap();
    }

    {
        let engine = engine_at(tmp.path());
        engine.set_wal_sync_policy("per_commit").unwrap();
        assert!(sorted_ids(&engine, "SELECT id FROM mut_audit WHERE id = 2").is_empty());
        assert!(sorted_ids(
            &engine,
            "SELECT id FROM mut_audit WHERE data @> '{\"role\":\"reviewer\"}'"
        )
        .is_empty());
        assert_eq!(
            sorted_ids(&engine, "SELECT id FROM mut_audit WHERE status = 'range'"),
            vec!["4"]
        );
        engine.validate_secondary_indexes().unwrap();
    }
}

#[test]
fn malformed_native_sql_inputs_return_results_without_panicking() {
    let engine = NativeSqlEngine::new();
    engine
        .execute(
            "CREATE TABLE malformed_guard (id INTEGER PRIMARY KEY, name TEXT, uid UUID, data JSON)",
        )
        .unwrap();
    engine
        .execute("INSERT INTO malformed_guard (id, name, uid, data) VALUES (1, 'alice', '550e8400-e29b-41d4-a716-446655440001', '{\"ok\":true}')")
        .unwrap();

    let should_error = [
        "DELETE FROM malformed_guard WHERE",
        "ALTER TABLE malformed_guard ADD COLUMN",
        "ALTER TABLE malformed_guard DROP COLUMN",
        "ALTER TABLE malformed_guard RENAME COLUMN name",
        "DROP TABLE IF EXISTS",
        "INSERT INTO malformed_guard (id, uid) VALUES (2, 'not-a-uuid')",
        "INSERT INTO malformed_guard (id, data) VALUES (3, '{\"bad\":}')",
    ];

    for sql in should_error {
        assert!(
            execute_without_panic_is_err(&engine, sql),
            "malformed SQL should return Err: {sql}"
        );
    }

    let function_inputs = [
        "SELECT LENGTH(name) FROM malformed_guard",
        "SELECT GREATEST() FROM malformed_guard",
        "SELECT LEAST() FROM malformed_guard",
        "SELECT CASE WHEN id = 1 END FROM malformed_guard",
    ];

    for sql in function_inputs {
        assert_executes_without_panic(&engine, sql);
    }
}
