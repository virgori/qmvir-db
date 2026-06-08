import uuid

import pytest

import qm_engine


def test_prepared_uuid_and_json_string_parameters_roundtrip_as_canonical_strings():
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE py_uuid_json (id INTEGER PRIMARY KEY, u UUID, data JSON, payload JSONB)")

    insert = engine.prepare("INSERT INTO py_uuid_json (u, data, payload) VALUES ($1, $2, $3)")
    engine.execute_prepared(
        insert,
        [
            "550E8400-E29B-41D4-A716-446655440000",
            '{"b":2,"a":1}',
            '["x",{"ok":true}]',
        ],
    )

    columns, rows, tag = engine.execute("SELECT u, data, payload FROM py_uuid_json WHERE id = 1")
    assert columns == ["u", "data", "payload"]
    assert tag == "SELECT 1"
    assert rows == [[
        "550e8400-e29b-41d4-a716-446655440000",
        '{"b":2,"a":1}',
        '["x",{"ok":true}]',
    ]]


def test_prepared_uuid_and_json_python_objects_roundtrip_as_canonical_strings():
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE py_uuid_json_direct (id INTEGER PRIMARY KEY, u UUID, data JSON, payload JSONB)")

    insert = engine.prepare("INSERT INTO py_uuid_json_direct (u, data, payload) VALUES ($1, $2, $3)")
    engine.execute_prepared(
        insert,
        [
            uuid.UUID("550e8400-e29b-41d4-a716-446655440000"),
            {"b": 2, "a": 1},
            ["x", {"ok": True}],
        ],
    )

    columns, rows, tag = engine.execute("SELECT u, data, payload FROM py_uuid_json_direct WHERE id = 1")
    assert columns == ["u", "data", "payload"]
    assert tag == "SELECT 1"
    assert rows == [[
        "550e8400-e29b-41d4-a716-446655440000",
        '{"a":1,"b":2}',
        '["x",{"ok":true}]',
    ]]


def test_prepared_binding_rejects_invalid_python_object():
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE py_reject (id INTEGER PRIMARY KEY, data JSON)")
    insert = engine.prepare("INSERT INTO py_reject (data) VALUES ($1)")

    with pytest.raises(TypeError, match="prepared parameters must be str|JSON-serializable"):
        engine.execute_prepared(insert, [object()])

    _, rows, _ = engine.execute("SELECT COUNT(*) FROM py_reject")
    assert rows == [["0"]]


def test_large_result_ordering_preserved_across_python_rust_boundary():
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE py_bridge_large (id INTEGER PRIMARY KEY, score INTEGER, label TEXT)")
    for i in range(200):
        engine.execute(
            f"INSERT INTO py_bridge_large (id, score, label) VALUES ({i}, {199 - i}, 'row-{i}')"
        )

    columns, rows, tag = engine.execute(
        "SELECT id, score, label FROM py_bridge_large ORDER BY id LIMIT 200"
    )

    assert columns == ["id", "score", "label"]
    assert tag == "SELECT 200"
    assert [int(row[0]) for row in rows] == list(range(200))
    assert rows[0] == ["0", "199", "row-0"]
    assert rows[-1] == ["199", "0", "row-199"]


def test_bridge_error_mapping_for_vector_dimension_mismatch_is_stable():
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE py_bridge_vec_err (id INTEGER PRIMARY KEY, embedding VECTOR(2))")
    engine.execute("INSERT INTO py_bridge_vec_err (id, embedding) VALUES (1, '[1,2]')")

    with pytest.raises(RuntimeError, match="vector dimension mismatch.*expected 2, got 3"):
        engine.execute("INSERT INTO py_bridge_vec_err (id, embedding) VALUES (2, '[1,2,3]')")

    _, rows, _ = engine.execute("SELECT COUNT(*) FROM py_bridge_vec_err")
    assert rows == [["1"]]


def test_prepared_batch_like_execution_preserves_request_order_and_materialized_ids():
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE py_bridge_prepared_order (id INTEGER PRIMARY KEY, payload JSON)")
    insert = engine.prepare("INSERT INTO py_bridge_prepared_order (id, payload) VALUES ($1, $2)")
    for i in [5, 3, 9, 1]:
        engine.execute_prepared(insert, [str(i), {"request_id": i}])

    columns, rows, tag = engine.execute(
        "SELECT id, payload FROM py_bridge_prepared_order ORDER BY id"
    )

    assert columns == ["id", "payload"]
    assert tag == "SELECT 4"
    assert [row[0] for row in rows] == ["1", "3", "5", "9"]
    assert rows[0][1] == '{"request_id":1}'


def test_vector_query_result_order_survives_python_materialization():
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE py_bridge_vec (id INTEGER PRIMARY KEY, embedding VECTOR(2))")
    engine.execute("INSERT INTO py_bridge_vec (id, embedding) VALUES (1, '[0,0]')")
    engine.execute("INSERT INTO py_bridge_vec (id, embedding) VALUES (2, '[1,0]')")
    engine.execute("INSERT INTO py_bridge_vec (id, embedding) VALUES (3, '[2,0]')")

    columns, rows, tag = engine.execute(
        "SELECT id FROM py_bridge_vec ORDER BY embedding <-> '[0,0]' LIMIT 3"
    )

    assert columns == ["id"]
    assert tag == "SELECT 3"
    assert [row[0] for row in rows] == ["1", "2", "3"]
