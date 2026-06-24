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
fn create_gin_index_supports_body_search_operator() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)")
        .unwrap();
    for i in 0..100 {
        engine
            .execute(&format!(
                "INSERT INTO docs (id, body) VALUES ({i}, 'alpha beta gamma chunk {i} needle')"
            ))
            .unwrap();
    }
    engine
        .execute("CREATE INDEX idx_docs_body ON docs (body) USING gin")
        .unwrap();

    let hits = rows(
        &engine,
        "SELECT id FROM docs WHERE body @@ 'needle alpha' LIMIT 5",
    );
    assert!(!hits.is_empty());
    assert!(hits.len() <= 5);
}

#[test]
fn inverted_index_updates_on_insert_and_delete() {
    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)")
        .unwrap();
    engine
        .execute("INSERT INTO docs (id, body) VALUES (1, 'hello world')")
        .unwrap();
    engine
        .execute("CREATE INDEX idx_docs_body ON docs (body) USING gin")
        .unwrap();

    engine
        .execute("INSERT INTO docs (id, body) VALUES (2, 'hello needle')")
        .unwrap();
    let after_insert = rows(
        &engine,
        "SELECT id FROM docs WHERE body @@ 'needle' LIMIT 10",
    );
    assert_eq!(after_insert, vec![vec![Some("2".to_string())]]);

    engine.execute("DELETE FROM docs WHERE id = 2").unwrap();
    let after_delete = rows(
        &engine,
        "SELECT id FROM docs WHERE body @@ 'needle' LIMIT 10",
    );
    assert!(after_delete.is_empty());
}
