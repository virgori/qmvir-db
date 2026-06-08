import gc
import struct

import pytest

import qm_engine


def _i64(view) -> list[int]:
    return [v[0] for v in struct.iter_unpack("<q", view)]


def _f64(view) -> list[float]:
    return [v[0] for v in struct.iter_unpack("<d", view)]


def _utf8(offsets_view, data_view) -> list[str]:
    offsets = _i64(offsets_view)
    data = bytes(data_view)
    return [
        data[offsets[i] : offsets[i + 1]].decode("utf-8")
        for i in range(len(offsets) - 1)
    ]


def _fixture_engine():
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE py_true_zc (id INTEGER PRIMARY KEY, score INTEGER, ratio REAL, label TEXT)")
    for i in range(5):
        engine.execute(
            f"INSERT INTO py_true_zc (id, score, ratio, label) VALUES ({i}, {10 + i}, {i + 0.25}, 'row-{i}')"
        )
    return engine


def test_old_execute_still_returns_row_materialization():
    engine = _fixture_engine()

    columns, rows, tag = engine.execute("SELECT id, score, ratio, label FROM py_true_zc ORDER BY id LIMIT 2")

    assert columns == ["id", "score", "ratio", "label"]
    assert rows == [["0", "10", "0.25", "row-0"], ["1", "11", "1.25", "row-1"]]
    assert tag == "SELECT 2"


def test_reduced_copy_execute_columnar_still_works():
    engine = _fixture_engine()

    result = engine.execute_columnar("SELECT id, score, ratio, label FROM py_true_zc ORDER BY id LIMIT 2")

    assert result["classification"] == "REDUCED_COPY"
    assert result["zero_copy"] is False
    assert result["row_count"] == 2
    assert "rows" not in result


def test_execute_columnar_zero_copy_api_exists_and_returns_memoryviews():
    engine = _fixture_engine()

    result = engine.execute_columnar_zero_copy(
        "SELECT id, score, ratio, label FROM py_true_zc ORDER BY id LIMIT 5"
    )

    assert result["classification"] == "ZERO_COPY"
    assert result["zero_copy_subtype"] == "ZERO_COPY_UTF8_OFFSETS_DATA"
    assert result["batch_kind"] == "COLUMNAR_BATCH"
    assert result["zero_copy"] is True
    assert result["row_count"] == 5
    assert "rows" not in result
    assert result["column_names"] == ["id", "score", "ratio", "label"]
    assert result["types"] == ["int64", "int64", "float64", "utf8"]
    assert set(result["buffers"]) == {"id", "score", "ratio", "label"}
    assert isinstance(result["buffers"]["id"], memoryview)
    assert isinstance(result["buffers"]["label"]["offsets"], memoryview)
    assert isinstance(result["buffers"]["label"]["data"], memoryview)
    for column in result["columns"]:
        assert column["zero_copy"] is True
        assert column["copy_reason"]
        assert column["buffer_owner"] == "rust_arc_pyclass"
        assert column["buffer_kind"] == "memoryview"


def test_numeric_and_float_memoryviews_decode_correctly():
    engine = _fixture_engine()

    result = engine.execute_columnar_zero_copy(
        "SELECT id, score, ratio FROM py_true_zc ORDER BY id LIMIT 5"
    )

    assert result["classification"] == "ZERO_COPY"
    assert result["zero_copy_subtype"] == "ZERO_COPY_NUMERIC_ONLY"
    assert _i64(result["columns"][0]["buffer"]) == [0, 1, 2, 3, 4]
    assert _i64(result["columns"][1]["buffer"]) == [10, 11, 12, 13, 14]
    assert _f64(result["columns"][2]["buffer"]) == [0.25, 1.25, 2.25, 3.25, 4.25]


def test_utf8_offsets_and_data_memoryviews_decode_correctly():
    engine = _fixture_engine()

    result = engine.execute_columnar_zero_copy("SELECT label FROM py_true_zc ORDER BY id LIMIT 3")
    column = result["columns"][0]

    assert column["physical_type"] == "utf8_offsets_data"
    assert isinstance(column["offsets"], memoryview)
    assert isinstance(column["data"], memoryview)
    assert _utf8(column["offsets"], column["data"]) == ["row-0", "row-1", "row-2"]


def test_memoryview_lifetime_survives_result_deletion():
    engine = _fixture_engine()
    result = engine.execute_columnar_zero_copy("SELECT id, label FROM py_true_zc ORDER BY id LIMIT 2")
    id_view = result["columns"][0]["buffer"]
    offsets_view = result["columns"][1]["offsets"]
    data_view = result["columns"][1]["data"]

    del result
    gc.collect()

    assert _i64(id_view) == [0, 1]
    assert _utf8(offsets_view, data_view) == ["row-0", "row-1"]


def test_result_object_deletion_is_safe_when_no_views_escape():
    engine = _fixture_engine()
    result = engine.execute_columnar_zero_copy("SELECT id FROM py_true_zc ORDER BY id LIMIT 1")

    del result
    gc.collect()


def test_unsupported_sql_falls_back_with_explicit_reason():
    engine = _fixture_engine()

    result = engine.execute_columnar_zero_copy("SELECT id FROM py_true_zc ORDER BY label LIMIT 2")

    assert result["classification"] == "REDUCED_COPY_FALLBACK"
    assert result["zero_copy"] is False
    assert "ORDER BY id" in result["fallback_reason"]
    assert result["columns"][0]["zero_copy"] is False
    assert result["columns"][0]["buffer_owner"] == "python_bytes"


def test_vector_query_ordering_falls_back_stably():
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE py_true_zc_vec (id INTEGER PRIMARY KEY, embedding VECTOR(2))")
    engine.execute("INSERT INTO py_true_zc_vec (id, embedding) VALUES (1, '[0,0]')")
    engine.execute("INSERT INTO py_true_zc_vec (id, embedding) VALUES (2, '[1,0]')")
    engine.execute("INSERT INTO py_true_zc_vec (id, embedding) VALUES (3, '[2,0]')")

    result = engine.execute_columnar_zero_copy(
        "SELECT id FROM py_true_zc_vec ORDER BY embedding <-> '[0,0]' LIMIT 3"
    )

    assert result["classification"] == "REDUCED_COPY_FALLBACK"
    assert result["zero_copy"] is False
    assert result["column_buffers"][0]["encoding"] == "int64_le"
    assert _i64(result["column_buffers"][0]["data"]) == [1, 2, 3]


def test_vector_dimension_mismatch_error_propagates():
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE py_true_zc_vec_err (id INTEGER PRIMARY KEY, embedding VECTOR(2))")
    engine.execute("INSERT INTO py_true_zc_vec_err (id, embedding) VALUES (1, '[1,2]')")

    with pytest.raises(RuntimeError, match="vector dimension mismatch.*expected 2, got 3"):
        engine.execute_columnar_zero_copy(
            "INSERT INTO py_true_zc_vec_err (id, embedding) VALUES (2, '[1,2,3]')"
        )


def test_empty_result_set_has_stable_metadata_and_buffers():
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE py_true_zc_empty (id INTEGER PRIMARY KEY, score INTEGER, label TEXT)")

    result = engine.execute_columnar_zero_copy("SELECT id, score, label FROM py_true_zc_empty")

    assert result["row_count"] == 0
    assert result["column_count"] == 3
    assert result["zero_copy"] is True
    assert _i64(result["columns"][0]["buffer"]) == []
    assert _i64(result["columns"][1]["buffer"]) == []
    assert _utf8(result["columns"][2]["offsets"], result["columns"][2]["data"]) == []


def test_bm25_compact_result_order_scores_limit_and_empty():
    engine = qm_engine.NativeSqlEngine()

    result = engine.search_bm25_compact(
        [(1, "alpha beta"), (2, "alpha alpha"), (3, "gamma")],
        "alpha",
        2,
    )

    assert result["compact_bridge"] == "bm25_compact"
    assert result["classification"] == "ZERO_COPY"
    assert result["zero_copy_subtype"] == "ZERO_COPY_NUMERIC_ONLY"
    assert result["zero_copy"] is True
    assert result["row_count"] == 2
    assert set(result["buffers"]) == {"doc_id", "score", "rank"}
    assert _i64(result["columns"][0]["buffer"]) == [2, 1]
    scores = _f64(result["columns"][1]["buffer"])
    assert scores[0] > scores[1] > 0
    assert _i64(result["columns"][2]["buffer"]) == [1, 2]

    empty = engine.search_bm25_compact([(1, "alpha")], "missing", 5)
    assert empty["row_count"] == 0
    assert _i64(empty["columns"][0]["buffer"]) == []


def test_hybrid_compact_result_order_scores_limit_and_empty():
    engine = qm_engine.NativeSqlEngine()

    result = engine.search_hybrid_compact(
        [(1, 10.0), (2, 1.0)],
        [(2, 10.0), (3, 5.0)],
        0.5,
        2,
    )

    assert result["compact_bridge"] == "hybrid_compact"
    assert result["classification"] == "ZERO_COPY"
    assert result["zero_copy_subtype"] == "ZERO_COPY_NUMERIC_ONLY"
    assert result["zero_copy"] is True
    assert result["row_count"] == 2
    assert set(result["buffers"]) == {"doc_id", "score", "rank"}
    assert _i64(result["columns"][0]["buffer"]) == [1, 2]
    assert _f64(result["columns"][1]["buffer"]) == [0.5, 0.5]
    assert _i64(result["columns"][2]["buffer"]) == [1, 2]

    empty = engine.search_hybrid_compact([], [], 0.5, 10)
    assert empty["row_count"] == 0
    assert _i64(empty["columns"][0]["buffer"]) == []
