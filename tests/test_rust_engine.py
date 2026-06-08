"""Comprehensive tests for qm_engine Rust PyO3 API.

Tests all 12 exported Rust classes: SqlParser, WTinyLfuCache, JitCompiler,
VectorExecutor, StorageEngine, Transaction, HubEngine, IndexManager,
NativeDispatcher, RustRingBuffer, PostgresGateway, UringWalWriter.
"""
import json
import os
import tempfile

import pytest

import qm_engine


# ─────────────────────────────────────────────────────────────────────
# Fixtures
# ─────────────────────────────────────────────────────────────────────

@pytest.fixture
def tmp_dir(tmp_path):
    return str(tmp_path)


@pytest.fixture
def sql_parser():
    return qm_engine.SqlParser()


@pytest.fixture
def cache():
    return qm_engine.WTinyLfuCache(64)


@pytest.fixture
def jit():
    return qm_engine.JitCompiler()


@pytest.fixture
def vector_exec():
    return qm_engine.VectorExecutor()


@pytest.fixture
def storage_engine(tmp_path):
    data_dir = str(tmp_path / "data")
    wal_dir = str(tmp_path / "wal")
    os.makedirs(data_dir, exist_ok=True)
    os.makedirs(wal_dir, exist_ok=True)
    return qm_engine.StorageEngine(data_dir, wal_dir)


@pytest.fixture
def index_mgr():
    return qm_engine.IndexManager()


@pytest.fixture
def native_dispatcher(tmp_path):
    return qm_engine.NativeDispatcher(str(tmp_path))


@pytest.fixture
def ring_buffer(tmp_path):
    path = str(tmp_path / "ring")
    return qm_engine.RustRingBuffer(path, 4, 256)


@pytest.fixture
def wal_writer(tmp_path):
    return qm_engine.UringWalWriter(str(tmp_path))


@pytest.fixture
def hub_engine(tmp_path):
    return qm_engine.HubEngine(str(tmp_path))


# ─────────────────────────────────────────────────────────────────────
# SqlParser
# ─────────────────────────────────────────────────────────────────────

class TestSqlParser:
    def test_parse_select(self, sql_parser):
        result = sql_parser.parse("SELECT id, name FROM users WHERE id = 1")
        assert result["query_type"] == "Select"
        assert isinstance(result["tables"], list)
        assert isinstance(result["columns"], list)

    def test_parse_insert(self, sql_parser):
        result = sql_parser.parse("INSERT INTO users (id, name) VALUES (1, 'alice')")
        assert result["query_type"] == "Insert"

    def test_parse_update(self, sql_parser):
        result = sql_parser.parse("UPDATE users SET name = 'bob' WHERE id = 1")
        assert result["query_type"] == "Update"

    def test_parse_delete(self, sql_parser):
        result = sql_parser.parse("DELETE FROM users WHERE id = 1")
        assert result["query_type"] == "Delete"

    def test_parse_create_table(self, sql_parser):
        result = sql_parser.parse("CREATE TABLE t1 (id INT, name TEXT)")
        assert result["query_type"] == "CreateTable"

    def test_get_query_type(self, sql_parser):
        assert sql_parser.get_query_type("SELECT 1") == "SELECT"
        assert sql_parser.get_query_type("INSERT INTO t VALUES (1)") == "INSERT"
        assert sql_parser.get_query_type("DELETE FROM t") == "DELETE"

    def test_parse_vector_query(self, sql_parser):
        result = sql_parser.parse("SELECT 1")
        assert "is_vector_query" in result

    def test_parse_with_limit_offset(self, sql_parser):
        result = sql_parser.parse("SELECT * FROM t LIMIT 10 OFFSET 5")
        assert result["limit"] == 10
        assert result["offset"] == 5


# ─────────────────────────────────────────────────────────────────────
# WTinyLfuCache
# ─────────────────────────────────────────────────────────────────────

class TestWTinyLfuCache:
    def test_insert_and_get(self, cache):
        cache.insert("key1", b"value1")
        result = cache.get("key1")
        assert result is not None
        assert bytes(result) == b"value1"

    def test_get_missing(self, cache):
        assert cache.get("nonexistent") is None

    def test_remove(self, cache):
        cache.insert("k", b"v")
        cache.remove("k")
        assert cache.get("k") is None

    def test_hit_rate(self, cache):
        cache.insert("a", b"1")
        cache.get("a")  # hit
        cache.get("b")  # miss
        hr = cache.hit_rate()
        assert 0.0 <= hr <= 1.0

    def test_weight(self, cache):
        w = cache.weight()
        assert isinstance(w, int)

    def test_overwrite(self, cache):
        cache.insert("k", b"old")
        cache.insert("k", b"new")
        assert bytes(cache.get("k")) == b"new"


# ─────────────────────────────────────────────────────────────────────
# JitCompiler
# ─────────────────────────────────────────────────────────────────────

class TestJitCompiler:
    def test_filter_eq_i64(self, jit):
        data = [10, 20, 30, 20, 40]
        result = jit.filter_eq_i64(data, 0, 20)
        assert 1 in result
        assert 3 in result
        assert len(result) == 2

    def test_filter_eq_i64_no_match(self, jit):
        result = jit.filter_eq_i64([1, 2, 3], 0, 99)
        assert result == []

    def test_filter_between_f64(self, jit):
        data = [1.0, 2.0, 3.0, 4.0, 5.0]
        result = jit.filter_between_f64(data, 0, 2.0, 4.0)
        assert 1 in result  # 2.0
        assert 2 in result  # 3.0
        assert 3 in result  # 4.0

    def test_project_f64(self, jit):
        col_a = [1.0, 2.0, 3.0]
        col_b = [4.0, 5.0, 6.0]
        result = jit.project_f64(col_a, col_b, 0.0)
        assert len(result) == 3
        assert result[0] == pytest.approx(4.0)   # 1*4+0
        assert result[1] == pytest.approx(10.0)  # 2*5+0
        assert result[2] == pytest.approx(18.0)  # 3*6+0

    def test_project_f64_with_addend(self, jit):
        result = jit.project_f64([2.0], [3.0], 10.0)
        assert result[0] == pytest.approx(16.0)  # 2*3+10

    def test_cache_size(self, jit):
        assert jit.cache_size() == 0
        jit.filter_eq_i64([1, 2, 3], 0, 2)
        # cache_size may increase after JIT compilation
        assert isinstance(jit.cache_size(), int)


# ─────────────────────────────────────────────────────────────────────
# VectorExecutor
# ─────────────────────────────────────────────────────────────────────

class TestVectorExecutor:
    def test_batch_l2_distance(self, vector_exec):
        query = [1.0, 0.0, 0.0, 0.0]
        vectors = [
            [1.0, 0.0, 0.0, 0.0],  # dist=0
            [0.0, 1.0, 0.0, 0.0],  # dist=sqrt(2)
        ]
        result = vector_exec.batch_l2_distance(vectors, query)
        assert len(result) == 2
        assert result[0] == pytest.approx(0.0, abs=1e-5)
        assert result[1] > 0

    def test_batch_dot_product(self, vector_exec):
        query = [1.0, 2.0, 3.0]
        vectors = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
        ]
        result = vector_exec.batch_dot_product(vectors, query)
        assert len(result) == 2
        assert result[0] == pytest.approx(1.0)
        assert result[1] == pytest.approx(2.0)

    def test_search_top_k(self, vector_exec):
        query = [1.0, 0.0]
        vectors = [
            [1.0, 0.0],
            [0.0, 1.0],
            [0.5, 0.5],
        ]
        result = vector_exec.search(vectors, query, 2)
        assert len(result) == 2
        # First result should be the closest vector
        assert result[0][0] == 0  # index of [1,0]

    def test_parallel_batch_dot_product(self, vector_exec):
        query = [1.0, 1.0]
        vectors = [[2.0, 3.0], [4.0, 5.0]]
        result = vector_exec.parallel_batch_dot_product(vectors, query)
        assert len(result) == 2
        assert result[0] == pytest.approx(5.0)
        assert result[1] == pytest.approx(9.0)

    def test_get_batch_size(self, vector_exec):
        assert vector_exec.get_batch_size() == 1024

    def test_custom_batch_size(self):
        ve = qm_engine.VectorExecutor(batch_size=512)
        assert ve.get_batch_size() == 512


# ─────────────────────────────────────────────────────────────────────
# StorageEngine & Transaction
# ─────────────────────────────────────────────────────────────────────

class TestStorageEngine:
    def test_begin_transaction(self, storage_engine):
        txn = storage_engine.begin_transaction()
        assert txn is not None
        assert txn.state == "active"

    def test_flush(self, storage_engine):
        storage_engine.flush()

    def test_multiple_transactions(self, storage_engine):
        t1 = storage_engine.begin_transaction()
        t2 = storage_engine.begin_transaction()
        assert t1.txn_id != t2.txn_id


class TestTransaction:
    def test_create_and_properties(self):
        txn = qm_engine.Transaction(42, "read_committed")
        assert txn.txn_id == 42
        assert txn.isolation_level == "read_committed"
        assert txn.state == "active"

    def test_commit(self):
        txn = qm_engine.Transaction(1, "read_committed")
        txn.commit()
        assert txn.state == "committed"

    def test_serializable_isolation(self):
        txn = qm_engine.Transaction(2, "snapshot_isolation")
        assert txn.isolation_level == "snapshot_isolation"
        # Backward compat: "serializable" string still accepted
        txn2 = qm_engine.Transaction(3, "serializable")
        assert txn2.isolation_level == "snapshot_isolation"


# ─────────────────────────────────────────────────────────────────────
# IndexManager
# ─────────────────────────────────────────────────────────────────────

class TestIndexManager:
    def test_create_and_list(self, index_mgr):
        index_mgr.create_index("idx_users_id", "users", ["id"])
        indexes = index_mgr.list_indexes()
        assert len(indexes) == 1
        assert indexes[0][0] == "idx_users_id"
        assert indexes[0][1] == "users"

    def test_drop_index(self, index_mgr):
        index_mgr.create_index("idx_drop", "t", ["col"])
        index_mgr.drop_index("idx_drop")
        indexes = index_mgr.list_indexes()
        assert len(indexes) == 0

    def test_multi_column_index(self, index_mgr):
        index_mgr.create_index("idx_multi", "orders", ["customer_id", "order_date"])
        indexes = index_mgr.list_indexes()
        assert indexes[0][2] == ["customer_id", "order_date"]

    def test_list_empty(self, index_mgr):
        assert index_mgr.list_indexes() == []


# ─────────────────────────────────────────────────────────────────────
# NativeDispatcher
# ─────────────────────────────────────────────────────────────────────

class TestNativeDispatcher:
    def test_dispatch_ddl(self, native_dispatcher):
        result = native_dispatcher.dispatch_ddl(b"CREATE TABLE t (id INT)")
        assert isinstance(result, tuple)

    def test_dispatch_insert(self, native_dispatcher):
        native_dispatcher.dispatch_ddl(b"CREATE TABLE t (id INT)")
        result = native_dispatcher.dispatch_insert(b"INSERT INTO t VALUES (1)")
        assert isinstance(result, tuple)

    def test_dispatch_query(self, native_dispatcher):
        native_dispatcher.dispatch_ddl(b"CREATE TABLE t (id INT)")
        result = native_dispatcher.dispatch_query(b"SELECT * FROM t")
        assert isinstance(result, tuple)

    def test_current_lsn(self, native_dispatcher):
        lsn_before = native_dispatcher.current_lsn
        native_dispatcher.dispatch_ddl(b"CREATE TABLE test (id INT)")
        lsn_after = native_dispatcher.current_lsn
        assert lsn_after > lsn_before

    def test_ring_dir(self, native_dispatcher):
        assert isinstance(native_dispatcher.ring_dir, str)
        assert len(native_dispatcher.ring_dir) > 0

    def test_dispatch_update(self, native_dispatcher):
        result = native_dispatcher.dispatch_update(b"UPDATE t SET x=1")
        assert isinstance(result, tuple)

    def test_dispatch_delete(self, native_dispatcher):
        result = native_dispatcher.dispatch_delete(b"DELETE FROM t WHERE id=1")
        assert isinstance(result, tuple)


# ─────────────────────────────────────────────────────────────────────
# RustRingBuffer
# ─────────────────────────────────────────────────────────────────────

class TestRustRingBuffer:
    def test_publish_and_consume(self, ring_buffer):
        seq = ring_buffer.publish(1, 0, b"hello")
        assert isinstance(seq, int)
        msg = ring_buffer.consume()
        assert msg is not None
        slot_idx, lsn, cmd, payload = msg
        assert lsn == 1
        assert cmd == 0
        assert bytes(payload) == b"hello"

    def test_consume_empty(self, ring_buffer):
        assert ring_buffer.consume() is None

    def test_complete(self, ring_buffer):
        ring_buffer.publish(1, 0, b"data")
        msg = ring_buffer.consume()
        slot_idx = msg[0]
        ring_buffer.complete(slot_idx, b"result")
        state, data = ring_buffer.collect_result(slot_idx)
        assert bytes(data) == b"result"

    def test_fail(self, ring_buffer):
        ring_buffer.publish(2, 0, b"data")
        msg = ring_buffer.consume()
        slot_idx = msg[0]
        ring_buffer.fail(slot_idx, b"error_info")

    def test_status(self, ring_buffer):
        status = ring_buffer.status()
        assert isinstance(status, tuple)
        assert len(status) == 8

    def test_recover(self, ring_buffer):
        result = ring_buffer.recover()
        assert isinstance(result, tuple)
        assert len(result) == 4

    def test_attach(self, tmp_path):
        path = str(tmp_path / "ring_attach")
        original = qm_engine.RustRingBuffer(path, 4, 256)
        original.publish(1, 0, b"test")
        attached = qm_engine.RustRingBuffer.attach(path)
        msg = attached.consume()
        assert msg is not None


# ─────────────────────────────────────────────────────────────────────
# UringWalWriter
# ─────────────────────────────────────────────────────────────────────

class TestUringWalWriter:
    def test_append_and_flush(self, wal_writer):
        lsn = wal_writer.append(1, 0, b"record_data")
        assert isinstance(lsn, int)
        wal_writer.flush()

    def test_current_lsn_increments(self, wal_writer):
        lsn0 = wal_writer.current_lsn
        wal_writer.append(1, 0, b"data1")
        wal_writer.flush()
        lsn1 = wal_writer.current_lsn
        assert lsn1 > lsn0

    def test_total_bytes_written(self, wal_writer):
        wal_writer.append(1, 0, b"some_bytes")
        wal_writer.flush()
        assert wal_writer.total_bytes_written > 0

    def test_multiple_records(self, wal_writer):
        for i in range(10):
            wal_writer.append(i, 1, f"record_{i}".encode())
        wal_writer.flush()
        assert wal_writer.current_lsn >= 10


# ─────────────────────────────────────────────────────────────────────
# HubEngine
# ─────────────────────────────────────────────────────────────────────

class TestHubEngine:
    def test_execute_hash_join(self, hub_engine):
        build = json.dumps([{"id": 1, "name": "a"}, {"id": 2, "name": "b"}])
        probe = json.dumps([{"id": 1, "val": 100}, {"id": 3, "val": 300}])
        result = hub_engine.execute_hash_join_bytes(
            build.encode(), probe.encode(), "id", "id"
        )
        parsed = json.loads(result)
        assert isinstance(parsed, dict)
        assert "rows" in parsed
        assert "columns" in parsed
        assert parsed["affected_rows"] == 1

    def test_hash_join_no_match(self, hub_engine):
        build = json.dumps([{"k": 1}])
        probe = json.dumps([{"k": 2}])
        result = hub_engine.execute_hash_join_bytes(
            build.encode(), probe.encode(), "k", "k"
        )
        parsed = json.loads(result)
        assert isinstance(parsed, dict)
        assert parsed["affected_rows"] == 0
        assert parsed["rows"] == []

    def test_start(self, hub_engine):
        hub_engine.start()


# ─────────────────────────────────────────────────────────────────────
# PostgresGateway
# ─────────────────────────────────────────────────────────────────────

class TestPostgresGateway:
    def test_create_and_config(self):
        gw = qm_engine.PostgresGateway(port=15497)
        config = gw.get_config()
        assert isinstance(config, str)
        assert "15497" in config
        gw.stop()

    def test_not_running_initially(self):
        gw = qm_engine.PostgresGateway(port=15496)
        assert gw.is_running is False
        gw.stop()

    def test_connection_count_zero(self):
        gw = qm_engine.PostgresGateway(port=15495)
        assert gw.connection_count == 0
        gw.stop()
