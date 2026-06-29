use qm_engine::gateway::native_sql::NativeSqlEngine;
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Instant;

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
fn generated_integer_ids_are_monotonic_and_do_not_reuse_deleted_or_rolled_back_ids() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE auto_ids (id INTEGER PRIMARY KEY, name TEXT UNIQUE)")
        .unwrap();

    engine
        .execute("INSERT INTO auto_ids (name) VALUES ('alice')")
        .unwrap();
    engine
        .execute("INSERT INTO auto_ids (name) VALUES ('bob')")
        .unwrap();
    assert_eq!(
        rows(&engine, "SELECT id, name FROM auto_ids ORDER BY id"),
        vec![
            vec![Some("1".to_string()), Some("alice".to_string())],
            vec![Some("2".to_string()), Some("bob".to_string())],
        ]
    );

    engine
        .execute("INSERT INTO auto_ids (id, name) VALUES (100, 'manual')")
        .unwrap();
    engine
        .execute("INSERT INTO auto_ids (name) VALUES ('after_manual')")
        .unwrap();
    assert_eq!(
        first_cell(
            &engine,
            "SELECT id FROM auto_ids WHERE name = 'after_manual'"
        ),
        "101"
    );

    engine.execute("DELETE FROM auto_ids WHERE id = 1").unwrap();
    engine
        .execute("INSERT INTO auto_ids (name) VALUES ('after_delete')")
        .unwrap();
    assert_eq!(
        first_cell(
            &engine,
            "SELECT id FROM auto_ids WHERE name = 'after_delete'"
        ),
        "102"
    );

    engine.execute("BEGIN").unwrap();
    engine
        .execute("INSERT INTO auto_ids (name) VALUES ('rolled_back')")
        .unwrap();
    engine.execute("ROLLBACK").unwrap();
    engine
        .execute("INSERT INTO auto_ids (name) VALUES ('after_rollback')")
        .unwrap();
    assert_eq!(
        first_cell(
            &engine,
            "SELECT id FROM auto_ids WHERE name = 'after_rollback'"
        ),
        "104"
    );

    let failed = engine.execute("INSERT INTO auto_ids (name) VALUES ('bob')");
    assert!(failed.is_err(), "duplicate UNIQUE insert must fail");
    engine
        .execute("INSERT INTO auto_ids (name) VALUES ('after_failed')")
        .unwrap();
    assert_eq!(
        first_cell(
            &engine,
            "SELECT id FROM auto_ids WHERE name = 'after_failed'"
        ),
        "105"
    );
}

#[test]
fn generated_integer_id_counter_survives_persistent_reopen() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let engine = engine_at(tmp.path());
        engine.set_wal_sync_policy("per_commit").unwrap();
        engine
            .execute("CREATE TABLE auto_persist (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        engine
            .execute("INSERT INTO auto_persist (name) VALUES ('a')")
            .unwrap();
        engine
            .execute("INSERT INTO auto_persist (name) VALUES ('b')")
            .unwrap();
    }
    {
        let engine = engine_at(tmp.path());
        engine.set_wal_sync_policy("per_commit").unwrap();
        engine
            .execute("INSERT INTO auto_persist (name) VALUES ('c')")
            .unwrap();
        assert_eq!(
            first_cell(&engine, "SELECT id FROM auto_persist WHERE name = 'c'"),
            "3"
        );
    }
}

#[test]
fn generated_integer_ids_do_not_collide_across_cloned_engine_handles() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE auto_concurrent (id INTEGER PRIMARY KEY, worker INTEGER)")
        .unwrap();

    let barrier = Arc::new(Barrier::new(4));
    let mut handles = Vec::new();
    for worker in 0..4 {
        let handle_engine = engine.clone();
        let handle_barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            handle_barrier.wait();
            for _ in 0..50 {
                handle_engine
                    .execute(&format!(
                        "INSERT INTO auto_concurrent (worker) VALUES ({worker})"
                    ))
                    .unwrap();
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }

    let ids = rows(&engine, "SELECT id FROM auto_concurrent ORDER BY id");
    assert_eq!(ids.len(), 200);
    let mut seen = HashSet::new();
    for row in ids {
        let id = row[0].clone().unwrap();
        assert!(seen.insert(id), "generated ID collision across handles");
    }
    assert_eq!(
        first_cell(&engine, "SELECT COUNT(*) FROM auto_concurrent"),
        "200"
    );
}

#[test]
fn serial_bigserial_and_identity_columns_use_persistent_sequence_metadata() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let engine = engine_at(tmp.path());
        engine.set_wal_sync_policy("per_commit").unwrap();
        engine
            .execute("CREATE TABLE serial_docs (id SERIAL PRIMARY KEY, name TEXT UNIQUE)")
            .unwrap();
        engine
            .execute("CREATE TABLE bigserial_docs (id BIGSERIAL PRIMARY KEY, name TEXT)")
            .unwrap();
        engine
            .execute(
                "CREATE TABLE by_default_docs (id INTEGER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, name TEXT)",
            )
            .unwrap();
        engine
            .execute(
                "CREATE TABLE always_docs (id INTEGER GENERATED ALWAYS AS IDENTITY PRIMARY KEY, name TEXT)",
            )
            .unwrap();

        engine
            .execute("INSERT INTO serial_docs (name) VALUES ('a')")
            .unwrap();
        engine
            .execute("INSERT INTO bigserial_docs (name) VALUES ('b')")
            .unwrap();
        engine
            .execute("INSERT INTO by_default_docs (id, name) VALUES (100, 'manual')")
            .unwrap();
        engine
            .execute("INSERT INTO by_default_docs (name) VALUES ('generated')")
            .unwrap();

        let explicit_always =
            engine.execute("INSERT INTO always_docs (id, name) VALUES (10, 'bad')");
        assert!(
            explicit_always.is_err(),
            "GENERATED ALWAYS must reject explicit values"
        );
        engine
            .execute("INSERT INTO always_docs (id, name) OVERRIDING SYSTEM VALUE VALUES (10, 'ok')")
            .unwrap();
        engine
            .execute("INSERT INTO always_docs (name) VALUES ('next')")
            .unwrap();

        assert_eq!(
            first_cell(&engine, "SELECT id FROM serial_docs WHERE name = 'a'"),
            "1"
        );
        assert_eq!(
            first_cell(&engine, "SELECT id FROM bigserial_docs WHERE name = 'b'"),
            "1"
        );
        assert_eq!(
            rows(&engine, "SELECT id FROM by_default_docs ORDER BY id"),
            vec![vec![Some("1".to_string())], vec![Some("100".to_string())]]
        );
        assert_eq!(
            rows(&engine, "SELECT id FROM always_docs ORDER BY id"),
            vec![vec![Some("1".to_string())], vec![Some("10".to_string())]]
        );

        let tables = engine.tables.to_native_map();
        let serial_seq = tables
            .get("serial_docs")
            .unwrap()
            .sequences
            .get("id")
            .unwrap();
        assert_eq!(serial_seq.sequence_name, "serial_docs_id_seq");
        assert_eq!(serial_seq.table_id, "serial_docs");
        assert_eq!(serial_seq.column_name, "id");
        assert_eq!(serial_seq.current_value, 1);
        assert_eq!(serial_seq.increment, 1);
        assert_eq!(serial_seq.min_value, 1);
        assert_eq!(serial_seq.max_value, i32::MAX as i64);
        assert!(matches!(
            serial_seq.identity_mode,
            qm_engine::gateway::native_sql::IdentityMode::Serial
        ));
    }
    {
        let engine = engine_at(tmp.path());
        engine.set_wal_sync_policy("per_commit").unwrap();
        engine
            .execute("INSERT INTO serial_docs (name) VALUES ('after_reopen')")
            .unwrap();
        assert_eq!(
            first_cell(
                &engine,
                "SELECT id FROM serial_docs WHERE name = 'after_reopen'"
            ),
            "2"
        );
    }
}

#[test]
fn serial_sequence_values_are_not_reused_after_rollback_or_failed_insert() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE serial_gaps (id SERIAL PRIMARY KEY, name TEXT UNIQUE)")
        .unwrap();

    engine.execute("BEGIN").unwrap();
    engine
        .execute("INSERT INTO serial_gaps (name) VALUES ('rolled')")
        .unwrap();
    engine.execute("ROLLBACK").unwrap();
    engine
        .execute("INSERT INTO serial_gaps (name) VALUES ('after_rollback')")
        .unwrap();
    assert_eq!(
        first_cell(
            &engine,
            "SELECT id FROM serial_gaps WHERE name = 'after_rollback'"
        ),
        "2"
    );

    assert!(engine
        .execute("INSERT INTO serial_gaps (name) VALUES ('after_rollback')")
        .is_err());
    engine
        .execute("INSERT INTO serial_gaps (name) VALUES ('after_failed')")
        .unwrap();
    assert_eq!(
        first_cell(
            &engine,
            "SELECT id FROM serial_gaps WHERE name = 'after_failed'"
        ),
        "4"
    );
}

#[test]
fn standalone_sequence_apis_and_overriding_user_value_are_rejected() {
    let engine = NativeSqlEngine::new();
    for sql in [
        "CREATE SEQUENCE s",
        "ALTER SEQUENCE s RESTART WITH 10",
        "SELECT nextval('s')",
        "SELECT currval('s')",
        "SELECT setval('s', 10)",
        "INSERT INTO missing OVERRIDING USER VALUE VALUES (1)",
    ] {
        let err = engine.execute(sql).unwrap_err();
        assert!(
            err.contains("sequence")
                || err.contains("OVERRIDING USER VALUE")
                || err.contains("does not exist"),
            "unexpected error for {sql}: {err}"
        );
    }
}

#[test]
fn uuid_values_are_validated_canonicalized_unique_and_persistent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let upper = "550E8400-E29B-41D4-A716-446655440000";
    let lower = "550e8400-e29b-41d4-a716-446655440000";
    {
        let engine = engine_at(tmp.path());
        engine.set_wal_sync_policy("per_commit").unwrap();
        engine
            .execute("CREATE TABLE uuid_docs (id UUID PRIMARY KEY, name TEXT)")
            .unwrap();
        engine
            .execute(&format!(
                "INSERT INTO uuid_docs (id, name) VALUES ('{}', 'doc')",
                upper
            ))
            .unwrap();
        assert_eq!(
            rows(
                &engine,
                &format!("SELECT id, name FROM uuid_docs WHERE id = '{}'", lower)
            ),
            vec![vec![Some(lower.to_string()), Some("doc".to_string())]]
        );

        let invalid =
            engine.execute("INSERT INTO uuid_docs (id, name) VALUES ('not-a-uuid', 'bad')");
        assert!(invalid.is_err(), "invalid UUID must be rejected");

        let duplicate = engine.execute(&format!(
            "INSERT INTO uuid_docs (id, name) VALUES ('{}', 'dup')",
            lower
        ));
        assert!(duplicate.is_err(), "duplicate UUID primary key must fail");
    }
    {
        let engine = engine_at(tmp.path());
        assert_eq!(
            rows(
                &engine,
                &format!("SELECT name FROM uuid_docs WHERE id = '{}'", lower)
            ),
            vec![vec![Some("doc".to_string())]]
        );
    }
}

#[test]
fn generated_uuid_function_returns_valid_unique_uuid_values() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE generated_uuid_docs (id UUID PRIMARY KEY)")
        .unwrap();
    engine
        .execute("INSERT INTO generated_uuid_docs (id) VALUES (gen_random_uuid())")
        .unwrap();
    engine
        .execute("INSERT INTO generated_uuid_docs (id) VALUES (uuid_generate_v4())")
        .unwrap();
    let values = rows(&engine, "SELECT id FROM generated_uuid_docs ORDER BY id");
    assert_eq!(values.len(), 2);
    let a = values[0][0].clone().unwrap();
    let b = values[1][0].clone().unwrap();
    assert_ne!(a, b, "generated UUIDs should be unique in this smoke test");
    assert_eq!(a.len(), 36);
    assert_eq!(b.len(), 36);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
    assert!(b.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
}

#[test]
fn uuid_primary_key_lookup_is_correct_but_uses_text_scan_or_generic_text_index() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE uuid_pk_probe (id UUID PRIMARY KEY, name TEXT)")
        .unwrap();
    for i in 0..512 {
        engine
            .execute(&format!(
                "INSERT INTO uuid_pk_probe (id, name) VALUES ('550e8400-e29b-41d4-a716-{i:012x}', 'row-{i}')"
            ))
            .unwrap();
    }
    let target = "550e8400-e29b-41d4-a716-0000000001ff";
    let scan_start = Instant::now();
    let scan_rows = rows(
        &engine,
        &format!("SELECT name FROM uuid_pk_probe WHERE id = '{target}'"),
    );
    let scan_elapsed = scan_start.elapsed();
    assert_eq!(scan_rows, vec![vec![Some("row-511".to_string())]]);

    engine
        .execute("CREATE INDEX idx_uuid_pk_probe_id ON uuid_pk_probe (id)")
        .unwrap();
    let indexed_start = Instant::now();
    let indexed_rows = rows(
        &engine,
        &format!("SELECT name FROM uuid_pk_probe WHERE id = '{target}'"),
    );
    let indexed_elapsed = indexed_start.elapsed();
    assert_eq!(indexed_rows, scan_rows);
    println!(
        "uuid_pk_lookup_text_scan_us={} uuid_pk_lookup_generic_text_index_us={}",
        scan_elapsed.as_micros(),
        indexed_elapsed.as_micros()
    );
}

#[test]
fn json_values_are_validated_canonicalized_queryable_and_persistent() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let engine = engine_at(tmp.path());
        engine.set_wal_sync_policy("per_commit").unwrap();
        engine
            .execute("CREATE TABLE json_docs (id INTEGER PRIMARY KEY, data JSON)")
            .unwrap();
        engine
            .execute(
                r#"INSERT INTO json_docs (data) VALUES ('{"name":"alice","age":18,"tags":["a","b"],"nested":{"ok":true}}')"#,
            )
            .unwrap();
        engine
            .execute(r#"INSERT INTO json_docs (data) VALUES ('["x","y"]')"#)
            .unwrap();
        engine
            .execute(r#"INSERT INTO json_docs (data) VALUES ('true')"#)
            .unwrap();
        engine
            .execute(r#"INSERT INTO json_docs (data) VALUES ('null')"#)
            .unwrap();

        let invalid = engine.execute(r#"INSERT INTO json_docs (data) VALUES ('{"name":}')"#);
        assert!(invalid.is_err(), "invalid JSON must be rejected");

        assert_eq!(
            first_cell(
                &engine,
                "SELECT id FROM json_docs WHERE JSON_EXTRACT_PATH_TEXT(data, 'name') = 'alice'"
            ),
            "1"
        );
        assert_eq!(
            first_cell(
                &engine,
                "SELECT id FROM json_docs WHERE JSON_ARRAY_LENGTH(data) = 2"
            ),
            "2"
        );
    }
    {
        let engine = engine_at(tmp.path());
        assert_eq!(
            first_cell(
                &engine,
                "SELECT id FROM json_docs WHERE JSON_EXTRACT_PATH_TEXT(data, 'name') = 'alice'"
            ),
            "1"
        );
        assert_eq!(
            first_cell(
                &engine,
                "SELECT COUNT(*) FROM json_docs WHERE data IS NOT NULL"
            ),
            "4"
        );
    }
}

#[test]
fn jsonb_alias_has_canonical_structural_storage_but_limited_operator_claims() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE jsonb_docs (id INTEGER PRIMARY KEY, data JSONB)")
        .unwrap();
    engine
        .execute(r#"INSERT INTO jsonb_docs (data) VALUES ('{"b":2,"a":1}')"#)
        .unwrap();
    engine
        .execute(r#"INSERT INTO jsonb_docs (data) VALUES ('{"a":1,"b":2}')"#)
        .unwrap();
    let values = rows(&engine, "SELECT data FROM jsonb_docs ORDER BY id");
    assert_eq!(values[0][0], values[1][0]);

    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT COUNT(*) FROM jsonb_docs WHERE data @> '{"a":1}'"#
        ),
        "2"
    );
}

#[test]
fn json_preserves_raw_text_while_jsonb_canonicalizes_structural_text() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE json_split (id INTEGER PRIMARY KEY, raw JSON, bin JSONB)")
        .unwrap();
    engine
        .execute(r#"INSERT INTO json_split (raw, bin) VALUES ('{"b":2, "a":1}', '{"b":2, "a":1}')"#)
        .unwrap();

    assert_eq!(
        rows(&engine, "SELECT raw, bin FROM json_split WHERE id = 1"),
        vec![vec![
            Some(r#"{"b":2, "a":1}"#.to_string()),
            Some(r#"{"a":1,"b":2}"#.to_string())
        ]]
    );
}

#[test]
fn jsonb_duplicate_key_and_numeric_canonical_policy_is_serde_json_text_policy() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE json_policy (id INTEGER PRIMARY KEY, data JSONB)")
        .unwrap();
    engine
        .execute(r#"INSERT INTO json_policy (data) VALUES ('{"a":1,"a":2}')"#)
        .unwrap();
    engine
        .execute(r#"INSERT INTO json_policy (data) VALUES ('{"n":1}')"#)
        .unwrap();
    engine
        .execute(r#"INSERT INTO json_policy (data) VALUES ('{"n":1.0}')"#)
        .unwrap();
    engine
        .execute(r#"INSERT INTO json_policy (data) VALUES ('{"b":2,"a":1}')"#)
        .unwrap();
    engine
        .execute(r#"INSERT INTO json_policy (data) VALUES ('{"a":1,"b":2}')"#)
        .unwrap();
    engine
        .execute(r#"INSERT INTO json_policy (data) VALUES ('[1,2]')"#)
        .unwrap();
    engine
        .execute(r#"INSERT INTO json_policy (data) VALUES ('[2,1]')"#)
        .unwrap();
    engine
        .execute(r#"INSERT INTO json_policy (data) VALUES ('{"outer":{"b":2,"a":1}}')"#)
        .unwrap();
    engine
        .execute(r#"INSERT INTO json_policy (data) VALUES ('{"outer":{"a":1,"b":2}}')"#)
        .unwrap();

    assert_eq!(
        first_cell(&engine, "SELECT data FROM json_policy WHERE id = 1"),
        r#"{"a":2}"#
    );
    assert_eq!(
        first_cell(&engine, "SELECT data FROM json_policy WHERE id = 2"),
        r#"{"n":1}"#
    );
    assert_eq!(
        first_cell(&engine, "SELECT data FROM json_policy WHERE id = 3"),
        r#"{"n":1.0}"#
    );
    assert_eq!(
        first_cell(&engine, "SELECT data FROM json_policy WHERE id = 4"),
        r#"{"a":1,"b":2}"#
    );
    assert_eq!(
        first_cell(
            &engine,
            "SELECT COUNT(*) FROM json_policy WHERE data = '{\"a\":1,\"b\":2}'"
        ),
        "2"
    );
    assert_eq!(
        first_cell(
            &engine,
            "SELECT COUNT(*) FROM json_policy WHERE data = '{\"b\":2,\"a\":1}'"
        ),
        "2"
    );
    assert_eq!(
        first_cell(
            &engine,
            "SELECT COUNT(*) FROM json_policy WHERE data = '{\"outer\":{\"b\":2,\"a\":1}}'"
        ),
        "2"
    );
    assert_eq!(
        first_cell(
            &engine,
            "SELECT COUNT(*) FROM json_policy WHERE data = '{\"n\":1}'"
        ),
        "1"
    );
    assert_eq!(
        first_cell(
            &engine,
            "SELECT COUNT(*) FROM json_policy WHERE data = '[1,2]'"
        ),
        "1"
    );
    assert_eq!(
        first_cell(
            &engine,
            "SELECT COUNT(*) FROM json_policy WHERE data = '[2,1]'"
        ),
        "1"
    );
}

#[test]
fn jsonb_containment_full_scan_object_subset_policy() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE jsonb_contains (id INTEGER PRIMARY KEY, data JSONB)")
        .unwrap();
    engine
        .execute(r#"INSERT INTO jsonb_contains (data) VALUES ('{"a":1,"b":2}')"#)
        .unwrap();
    engine
        .execute(r#"INSERT INTO jsonb_contains (data) VALUES ('{"a":{"b":2},"c":3}')"#)
        .unwrap();
    engine
        .execute(r#"INSERT INTO jsonb_contains (data) VALUES ('{"a":{"b":3}}')"#)
        .unwrap();
    engine
        .execute(r#"INSERT INTO jsonb_contains (data) VALUES ('{"b":2,"a":1}')"#)
        .unwrap();
    engine
        .execute(r#"INSERT INTO jsonb_contains (data) VALUES ('{"a":1,"a":2}')"#)
        .unwrap();
    engine
        .execute(r#"INSERT INTO jsonb_contains (data) VALUES ('{"n":null}')"#)
        .unwrap();
    engine
        .execute(r#"INSERT INTO jsonb_contains (data) VALUES ('{"other":null}')"#)
        .unwrap();

    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT COUNT(*) FROM jsonb_contains WHERE data @> '{"a":1}'"#
        ),
        "2"
    );
    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT COUNT(*) FROM jsonb_contains WHERE data @> '{"a":{"b":2}}'"#
        ),
        "1"
    );
    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT COUNT(*) FROM jsonb_contains WHERE data @> '{"a":{"b":3}}'"#
        ),
        "1"
    );
    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT COUNT(*) FROM jsonb_contains WHERE data @> '{"a":1,"b":2}'"#
        ),
        "2"
    );
    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT COUNT(*) FROM jsonb_contains WHERE data @> '{"a":2}'"#
        ),
        "1"
    );
    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT COUNT(*) FROM jsonb_contains WHERE data @> '{"n":null}'"#
        ),
        "1"
    );
    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT COUNT(*) FROM jsonb_contains WHERE data @> '{"missing":null}'"#
        ),
        "0"
    );
    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT COUNT(*) FROM jsonb_contains WHERE data @> '1'"#
        ),
        "0"
    );
    assert!(engine
        .execute(r#"SELECT COUNT(*) FROM jsonb_contains WHERE data @> '{"bad":}'"#)
        .unwrap_err()
        .contains("invalid JSONB RHS"));
    assert!(engine
        .execute(r#"SELECT COUNT(*) FROM jsonb_contains WHERE data @> '[1]'"#)
        .unwrap_err()
        .contains("array containment"));
}

#[test]
fn jsonb_extraction_operator_subset_matches_supported_policy() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE jsonb_ops (id INTEGER PRIMARY KEY, data JSONB)")
        .unwrap();
    engine
        .execute(
            r#"INSERT INTO jsonb_ops (data) VALUES ('{"name":"alice","nested":{"flag":true,"nullv":null},"arr":[{"x":7},"second"],"tags":["red","blue"]}')"#,
        )
        .unwrap();

    assert_eq!(
        first_cell(
            &engine,
            "SELECT data ->> 'name' FROM jsonb_ops WHERE id = 1"
        ),
        "alice"
    );
    assert_eq!(
        first_cell(
            &engine,
            "SELECT data -> 'nested' FROM jsonb_ops WHERE id = 1"
        ),
        r#"{"flag":true,"nullv":null}"#
    );
    assert_eq!(
        first_cell(
            &engine,
            "SELECT data #>> '{nested,flag}' FROM jsonb_ops WHERE id = 1"
        ),
        "true"
    );
    assert_eq!(
        first_cell(
            &engine,
            "SELECT data #> '{arr,0}' FROM jsonb_ops WHERE id = 1"
        ),
        r#"{"x":7}"#
    );
    assert_eq!(
        first_cell(
            &engine,
            "SELECT data ->> 'missing' FROM jsonb_ops WHERE id = 1"
        ),
        ""
    );
    assert_eq!(
        first_cell(
            &engine,
            "SELECT data #>> '{nested,nullv}' FROM jsonb_ops WHERE id = 1"
        ),
        ""
    );
    assert_eq!(
        first_cell(
            &engine,
            "SELECT id FROM jsonb_ops WHERE data ->> 'name' = 'alice'"
        ),
        "1"
    );
    assert_eq!(
        first_cell(&engine, "SELECT id FROM jsonb_ops WHERE data ? 'name'"),
        "1"
    );
    assert_eq!(
        first_cell(
            &engine,
            "SELECT id FROM jsonb_ops WHERE data -> 'arr' ->> 1 = 'second'"
        ),
        "1"
    );

    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT id FROM jsonb_ops WHERE data @> '{"name":"alice"}'"#
        ),
        "1"
    );
    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT id FROM jsonb_ops WHERE data @> '{"nested":{"flag":true}}'"#
        ),
        "1"
    );
    assert!(engine
        .execute("CREATE INDEX idx_jsonb_path ON jsonb_ops ((data ->> 'name'))")
        .unwrap_err()
        .contains("JSONB expression/path indexes"));
}

#[test]
fn json_null_and_missing_key_match_supported_extraction_policy() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE json_nulls (id INTEGER PRIMARY KEY, data JSON)")
        .unwrap();
    engine
        .execute(r#"INSERT INTO json_nulls (data) VALUES ('{"a":null}')"#)
        .unwrap();
    engine
        .execute(r#"INSERT INTO json_nulls (data) VALUES ('{"b":1}')"#)
        .unwrap();

    assert_eq!(
        first_cell(
            &engine,
            "SELECT COUNT(*) FROM json_nulls WHERE JSON_EXTRACT_PATH_TEXT(data, 'a') IS NULL"
        ),
        "2"
    );
    assert_eq!(
        first_cell(
            &engine,
            "SELECT COUNT(*) FROM json_nulls WHERE JSON_EXTRACT_PATH_TEXT(data, 'a') = 'null'"
        ),
        "0"
    );
}

#[test]
fn indexed_json_and_uuid_paths_match_full_scan_results_and_stay_current() {
    let engine = NativeSqlEngine::new();
    let uuid_a = "550e8400-e29b-41d4-a716-446655440000";
    let uuid_b = "550e8400-e29b-41d4-a716-446655440001";
    engine
        .execute("CREATE TABLE indexed_docs (id INTEGER PRIMARY KEY, uid UUID, data JSON)")
        .unwrap();
    engine
        .execute(&format!(
            "INSERT INTO indexed_docs (uid, data) VALUES ('{}', '{{\"role\":\"admin\"}}')",
            uuid_a
        ))
        .unwrap();
    engine
        .execute(&format!(
            "INSERT INTO indexed_docs (uid, data) VALUES ('{}', '{{\"role\":\"user\"}}')",
            uuid_b
        ))
        .unwrap();
    let scan_uuid = rows(
        &engine,
        &format!("SELECT id FROM indexed_docs WHERE uid = '{}'", uuid_a),
    );
    engine
        .execute("CREATE INDEX idx_uid ON indexed_docs (uid)")
        .unwrap();
    let indexed_uuid = rows(
        &engine,
        &format!("SELECT id FROM indexed_docs WHERE uid = '{}'", uuid_a),
    );
    assert_eq!(indexed_uuid, scan_uuid);

    let scan_json = rows(
        &engine,
        r#"SELECT id FROM indexed_docs WHERE JSON_EXTRACT_PATH_TEXT(data, 'role') = 'admin'"#,
    );
    engine
        .execute("CREATE INDEX idx_data ON indexed_docs (data)")
        .unwrap();
    let indexed_json = rows(
        &engine,
        r#"SELECT id FROM indexed_docs WHERE data = '{"role":"admin"}'"#,
    );
    assert_eq!(indexed_json, scan_json);

    engine
        .execute(r#"UPDATE indexed_docs SET data = '{"role":"auditor"}' WHERE id = 1"#)
        .unwrap();
    assert_eq!(
        rows(
            &engine,
            r#"SELECT id FROM indexed_docs WHERE JSON_EXTRACT_PATH_TEXT(data, 'role') = 'admin'"#
        ),
        Vec::<Vec<Option<String>>>::new()
    );
    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT COUNT(*) FROM indexed_docs WHERE data = '{"role":"admin"}'"#
        ),
        "0"
    );

    engine.execute("BEGIN").unwrap();
    engine
        .execute(r#"UPDATE indexed_docs SET data = '{"role":"rollback"}' WHERE id = 2"#)
        .unwrap();
    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT COUNT(*) FROM indexed_docs WHERE data = '{"role":"user"}'"#
        ),
        "0"
    );
    engine.execute("ROLLBACK").unwrap();
    assert_eq!(
        rows(
            &engine,
            r#"SELECT id FROM indexed_docs WHERE JSON_EXTRACT_PATH_TEXT(data, 'role') = 'user'"#
        ),
        vec![vec![Some("2".to_string())]]
    );
    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT COUNT(*) FROM indexed_docs WHERE data = '{"role":"user"}'"#
        ),
        "1"
    );

    engine
        .execute("DELETE FROM indexed_docs WHERE id = 2")
        .unwrap();
    assert_eq!(
        rows(
            &engine,
            r#"SELECT id FROM indexed_docs WHERE JSON_EXTRACT_PATH_TEXT(data, 'role') = 'user'"#
        ),
        Vec::<Vec<Option<String>>>::new()
    );
    assert_eq!(
        first_cell(
            &engine,
            r#"SELECT COUNT(*) FROM indexed_docs WHERE data = '{"role":"user"}'"#
        ),
        "0"
    );
}

#[test]
fn json_query_results_survive_reopen_after_indexed_update_delete() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let engine = engine_at(tmp.path());
        engine.set_wal_sync_policy("per_commit").unwrap();
        engine
            .execute("CREATE TABLE json_reopen_idx (id INTEGER PRIMARY KEY, data JSON)")
            .unwrap();
        engine
            .execute(r#"INSERT INTO json_reopen_idx (data) VALUES ('{"role":"keep"}')"#)
            .unwrap();
        engine
            .execute(r#"INSERT INTO json_reopen_idx (data) VALUES ('{"role":"delete"}')"#)
            .unwrap();
        engine
            .execute("CREATE INDEX idx_json_reopen_data ON json_reopen_idx (data)")
            .unwrap();
        engine
            .execute(r#"UPDATE json_reopen_idx SET data = '{"role":"updated"}' WHERE id = 1"#)
            .unwrap();
        engine
            .execute("DELETE FROM json_reopen_idx WHERE id = 2")
            .unwrap();
    }
    {
        let engine = engine_at(tmp.path());
        assert_eq!(
            rows(
                &engine,
                r#"SELECT id FROM json_reopen_idx WHERE JSON_EXTRACT_PATH_TEXT(data, 'role') = 'updated'"#
            ),
            vec![vec![Some("1".to_string())]]
        );
        assert_eq!(
            first_cell(
                &engine,
                r#"SELECT COUNT(*) FROM json_reopen_idx WHERE data = '{"role":"updated"}'"#
            ),
            "1"
        );
        assert_eq!(
            rows(
                &engine,
                r#"SELECT id FROM json_reopen_idx WHERE JSON_EXTRACT_PATH_TEXT(data, 'role') = 'delete'"#
            ),
            Vec::<Vec<Option<String>>>::new()
        );
    }
}

#[test]
fn jsonb_containment_survives_reopen_and_rollback_update() {
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let engine = engine_at(tmp.path());
        engine.set_wal_sync_policy("per_commit").unwrap();
        engine
            .execute("CREATE TABLE jsonb_reopen_contains (id INTEGER PRIMARY KEY, data JSONB)")
            .unwrap();
        engine
            .execute(
                r#"INSERT INTO jsonb_reopen_contains (data) VALUES ('{"role":"admin","user":{"name":"alice"}}')"#,
            )
            .unwrap();
        engine.execute("BEGIN").unwrap();
        engine
            .execute(
                r#"UPDATE jsonb_reopen_contains SET data = '{"role":"user","user":{"name":"bob"}}' WHERE id = 1"#,
            )
            .unwrap();
        assert_eq!(
            first_cell(
                &engine,
                r#"SELECT COUNT(*) FROM jsonb_reopen_contains WHERE data @> '{"role":"admin"}'"#
            ),
            "0"
        );
        engine.execute("ROLLBACK").unwrap();
        assert_eq!(
            first_cell(
                &engine,
                r#"SELECT COUNT(*) FROM jsonb_reopen_contains WHERE data @> '{"role":"admin"}'"#
            ),
            "1"
        );
        engine
            .execute(
                r#"UPDATE jsonb_reopen_contains SET data = '{"role":"user","user":{"name":"alice"}}' WHERE id = 1"#,
            )
            .unwrap();
    }
    {
        let engine = engine_at(tmp.path());
        assert_eq!(
            first_cell(
                &engine,
                r#"SELECT COUNT(*) FROM jsonb_reopen_contains WHERE data @> '{"role":"admin"}'"#
            ),
            "0"
        );
        assert_eq!(
            first_cell(
                &engine,
                r#"SELECT COUNT(*) FROM jsonb_reopen_contains WHERE data @> '{"role":"user"}'"#
            ),
            "1"
        );
        assert_eq!(
            first_cell(
                &engine,
                r#"SELECT COUNT(*) FROM jsonb_reopen_contains WHERE data @> '{"user":{"name":"alice"}}'"#
            ),
            "1"
        );
    }
}
