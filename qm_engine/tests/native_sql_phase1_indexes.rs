use qm_engine::NativeSqlEngine;

fn rows(engine: &NativeSqlEngine, sql: &str) -> Vec<Vec<Option<String>>> {
    let result = engine.execute(sql).expect(sql);
    result
        .rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|cell| cell.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
                .collect()
        })
        .collect()
}

#[test]
fn adaptive_json_path_index_accelerates_extract_filter() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE docs (id INTEGER PRIMARY KEY, data JSON)")
        .unwrap();
    for i in 0..200 {
        engine
            .execute(&format!(
                "INSERT INTO docs (id, data) VALUES ({i}, '{{\"name\":\"user{i}\"}}')"
            ))
            .unwrap();
    }

    let hits = rows(
        &engine,
        "SELECT id FROM docs WHERE JSON_EXTRACT_PATH_TEXT(data, 'name') = 'user42'",
    );
    assert_eq!(hits, vec![vec![Some("42".to_string())]]);
}

#[test]
fn explicit_json_path_index_survives_new_rows() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE docs (id INTEGER PRIMARY KEY, data JSON)")
        .unwrap();
    engine
        .execute("INSERT INTO docs (id, data) VALUES (1, '{\"name\":\"alpha\"}')")
        .unwrap();
    engine
        .execute("CREATE INDEX idx_name ON docs (data) USING json_path('name')")
        .unwrap();
    engine
        .execute("INSERT INTO docs (id, data) VALUES (2, '{\"name\":\"beta\"}')")
        .unwrap();

    let hits = rows(
        &engine,
        "SELECT id FROM docs WHERE JSON_EXTRACT_PATH_TEXT(data, 'name') = 'beta'",
    );
    assert_eq!(hits, vec![vec![Some("2".to_string())]]);
}

#[test]
fn trigram_index_accelerates_like_contains() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)")
        .unwrap();
    for i in 0..200 {
        engine
            .execute(&format!(
                "INSERT INTO docs (id, body) VALUES ({i}, 'alpha beta gamma chunk {i} needle')"
            ))
            .unwrap();
    }
    engine
        .execute("CREATE INDEX idx_body_trgm ON docs (body) USING gin_trgm")
        .unwrap();

    let hits = rows(
        &engine,
        "SELECT id FROM docs WHERE body LIKE '%needle%'",
    );
    assert!(!hits.is_empty());
}

#[test]
fn nested_json_path_index_supports_dot_paths() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE docs (id INTEGER PRIMARY KEY, data JSON)")
        .unwrap();
    engine
        .execute(
            "INSERT INTO docs (id, data) VALUES (1, '{\"user\":{\"name\":\"alice\"}}')",
        )
        .unwrap();
    engine
        .execute("CREATE INDEX idx_user_name ON docs (data) USING json_path('user.name')")
        .unwrap();

    let hits = rows(
        &engine,
        "SELECT id FROM docs WHERE JSON_EXTRACT_PATH_TEXT(data, 'user.name') = 'alice'",
    );
    assert_eq!(hits, vec![vec![Some("1".to_string())]]);
}

#[test]
fn search_indexes_survive_checkpoint_reload() {
    let dir = tempfile::tempdir().unwrap();
    let engine = NativeSqlEngine::with_data_dir(dir.path().to_path_buf());
    engine
        .execute("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, data JSON)")
        .unwrap();
    engine
        .execute("INSERT INTO docs (id, body, data) VALUES (1, 'needle alpha', '{\"name\":\"bob\"}')")
        .unwrap();
    engine
        .execute("CREATE INDEX idx_body ON docs (body) USING gin")
        .unwrap();
    engine
        .execute("CREATE INDEX idx_body_trgm ON docs (body) USING gin_trgm")
        .unwrap();
    engine
        .execute("CREATE INDEX idx_name ON docs (data) USING json_path('name')")
        .unwrap();
    engine.checkpoint();

    let engine2 = NativeSqlEngine::with_data_dir(dir.path().to_path_buf());
    let fts = rows(
        &engine2,
        "SELECT id FROM docs WHERE body @@ 'needle alpha' LIMIT 10",
    );
    assert_eq!(fts, vec![vec![Some("1".to_string())]]);
    let like = rows(&engine2, "SELECT id FROM docs WHERE body LIKE '%needle%'");
    assert_eq!(like, vec![vec![Some("1".to_string())]]);
}
