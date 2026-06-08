import gc
import struct

import pytest

import qm_engine


def _int64_values(data: bytes) -> list[int]:
    return [value[0] for value in struct.iter_unpack("<q", data)]


def _float64_values(data: bytes) -> list[float]:
    return [value[0] for value in struct.iter_unpack("<d", data)]


def _offset_values(data: bytes) -> list[int]:
    return [value[0] for value in struct.iter_unpack("<Q", data)]


def _utf8_values(buffer: dict) -> list[str]:
    offsets = _offset_values(buffer["offsets"])
    data = buffer["data"]
    return [
        data[offsets[i] : offsets[i + 1]].decode("utf-8")
        for i in range(len(offsets) - 1)
    ]


def test_execute_columnar_preserves_old_execute_contract():
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE py_col_contract (id INTEGER PRIMARY KEY, score INTEGER, label TEXT)")
    engine.execute("INSERT INTO py_col_contract (id, score, label) VALUES (1, 7, 'alpha')")

    columns, rows, tag = engine.execute("SELECT id, score, label FROM py_col_contract ORDER BY id")

    assert columns == ["id", "score", "label"]
    assert rows == [["1", "7", "alpha"]]
    assert tag == "SELECT 1"


def test_execute_columnar_returns_column_buffers_without_row_cells():
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE py_col_buffers (id INTEGER PRIMARY KEY, score INTEGER, ratio REAL, label TEXT)")
    for i in range(4):
        engine.execute(
            f"INSERT INTO py_col_buffers (id, score, ratio, label) VALUES ({i}, {10 + i}, {i + 0.5}, 'row-{i}')"
        )

    result = engine.execute_columnar("SELECT id, score, ratio, label FROM py_col_buffers ORDER BY id")

    assert result["classification"] == "REDUCED_COPY"
    assert result["batch_kind"] == "COLUMNAR_BATCH"
    assert result["zero_copy"] is False
    assert "rows" not in result
    assert result["row_count"] == 4
    assert [col["name"] for col in result["columns"]] == ["id", "score", "ratio", "label"]

    id_buffer, score_buffer, ratio_buffer, label_buffer = result["column_buffers"]
    assert id_buffer["encoding"] == "int64_le"
    assert score_buffer["encoding"] == "int64_le"
    assert ratio_buffer["encoding"] == "float64_le"
    assert label_buffer["encoding"] == "utf8_offsets_data"

    assert _int64_values(id_buffer["data"]) == [0, 1, 2, 3]
    assert _int64_values(score_buffer["data"]) == [10, 11, 12, 13]
    assert _float64_values(ratio_buffer["data"]) == [0.5, 1.5, 2.5, 3.5]
    assert _utf8_values(label_buffer) == ["row-0", "row-1", "row-2", "row-3"]


def test_execute_columnar_empty_text_uses_stable_offsets():
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE py_col_empty_text (id INTEGER PRIMARY KEY, label TEXT)")
    engine.execute("INSERT INTO py_col_empty_text (id, label) VALUES (1, 'one')")
    engine.execute("INSERT INTO py_col_empty_text (id) VALUES (2)")
    engine.execute("INSERT INTO py_col_empty_text (id, label) VALUES (3, 'three')")

    result = engine.execute_columnar("SELECT id, label FROM py_col_empty_text ORDER BY id")
    label_buffer = result["column_buffers"][1]

    assert label_buffer["null_count"] == 0
    assert label_buffer["validity"][0] == 0b00000111
    assert _utf8_values(label_buffer) == ["one", "", "three"]


def test_execute_columnar_buffers_have_python_owned_lifetime():
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE py_col_lifetime (id INTEGER PRIMARY KEY, label TEXT)")
    engine.execute("INSERT INTO py_col_lifetime (id, label) VALUES (1, 'alive')")

    result = engine.execute_columnar("SELECT id, label FROM py_col_lifetime ORDER BY id")
    id_data = result["column_buffers"][0]["data"]
    label_data = result["column_buffers"][1]["data"]
    del result
    gc.collect()

    assert _int64_values(id_data) == [1]
    assert label_data.decode("utf-8") == "alive"


def test_execute_columnar_vector_query_preserves_order():
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE py_col_vec (id INTEGER PRIMARY KEY, embedding VECTOR(2))")
    engine.execute("INSERT INTO py_col_vec (id, embedding) VALUES (1, '[0,0]')")
    engine.execute("INSERT INTO py_col_vec (id, embedding) VALUES (2, '[1,0]')")
    engine.execute("INSERT INTO py_col_vec (id, embedding) VALUES (3, '[2,0]')")

    result = engine.execute_columnar(
        "SELECT id FROM py_col_vec ORDER BY embedding <-> '[0,0]' LIMIT 3"
    )

    assert _int64_values(result["column_buffers"][0]["data"]) == [1, 2, 3]


def test_execute_columnar_error_mapping_for_vector_dimension_mismatch_is_stable():
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE py_col_vec_err (id INTEGER PRIMARY KEY, embedding VECTOR(2))")
    engine.execute("INSERT INTO py_col_vec_err (id, embedding) VALUES (1, '[1,2]')")

    with pytest.raises(RuntimeError, match="vector dimension mismatch.*expected 2, got 3"):
        engine.execute_columnar("INSERT INTO py_col_vec_err (id, embedding) VALUES (2, '[1,2,3]')")
