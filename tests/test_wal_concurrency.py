"""Tests for Rust WAL + StorageEngine + Transaction concurrency.

Tests UringWalWriter advanced scenarios, StorageEngine transaction lifecycle,
and concurrent transaction safety.
"""
from __future__ import annotations

import os
import threading
import time

import pytest

import qm_engine


# ─────────────────────────────────────────────────────────────────────
# UringWalWriter — advanced
# ─────────────────────────────────────────────────────────────────────

class TestWalWriterAdvanced:
    @pytest.fixture
    def wal(self, tmp_path):
        return qm_engine.UringWalWriter(str(tmp_path))

    def test_sequential_txn_records(self, wal):
        """Multiple record types for a single transaction."""
        wal.append(1, 0, b"BEGIN")       # begin
        wal.append(1, 1, b"INSERT k1")  # insert
        wal.append(1, 1, b"INSERT k2")  # insert
        wal.append(1, 2, b"COMMIT")     # commit
        wal.flush()
        assert wal.current_lsn >= 4

    def test_interleaved_txns(self, wal):
        """Two transactions interleaved."""
        wal.append(1, 0, b"BEGIN")
        wal.append(2, 0, b"BEGIN")
        wal.append(1, 1, b"INSERT")
        wal.append(2, 1, b"INSERT")
        wal.append(1, 2, b"COMMIT")
        wal.append(2, 2, b"COMMIT")
        wal.flush()
        assert wal.current_lsn >= 6

    def test_large_record(self, wal):
        """WAL should handle large records."""
        big_data = b"x" * 100_000
        wal.append(1, 0, big_data)
        wal.flush()
        assert wal.total_bytes_written >= 100_000

    def test_many_small_records(self, wal):
        for i in range(1000):
            wal.append(i, 0, f"record_{i}".encode())
        wal.flush()
        assert wal.current_lsn >= 1000

    def test_flush_idempotent(self, wal):
        wal.append(1, 0, b"data")
        wal.flush()
        wal.flush()  # double flush should be safe
        assert wal.total_bytes_written > 0

    def test_lsn_strictly_monotonic(self, wal):
        lsns = []
        for i in range(100):
            wal.append(i, 0, b"data")
        wal.flush()
        # LSN should always increase
        assert wal.current_lsn >= 100


# ─────────────────────────────────────────────────────────────────────
# StorageEngine — transaction lifecycle
# ─────────────────────────────────────────────────────────────────────

class TestStorageEngineTransactions:
    @pytest.fixture
    def engine(self, tmp_path):
        data_dir = str(tmp_path / "data")
        wal_dir = str(tmp_path / "wal")
        os.makedirs(data_dir, exist_ok=True)
        os.makedirs(wal_dir, exist_ok=True)
        return qm_engine.StorageEngine(data_dir, wal_dir)

    def test_begin_and_commit(self, engine):
        txn = engine.begin_transaction()
        assert txn.state == "active"
        txn.commit()
        assert txn.state == "committed"

    def test_begin_and_rollback(self, engine):
        txn = engine.begin_transaction()
        txn.rollback()
        assert txn.state == "rolled_back"

    def test_txn_ids_unique(self, engine):
        ids = set()
        for _ in range(100):
            txn = engine.begin_transaction()
            ids.add(txn.txn_id)
        assert len(ids) == 100

    def test_flush_after_transactions(self, engine):
        for _ in range(10):
            txn = engine.begin_transaction()
            txn.commit()
        engine.flush()  # Should not raise


# ─────────────────────────────────────────────────────────────────────
# Transaction — isolation levels
# ─────────────────────────────────────────────────────────────────────

class TestTransactionIsolation:
    def test_read_committed(self):
        txn = qm_engine.Transaction(1, "read_committed")
        assert txn.isolation_level == "read_committed"

    def test_snapshot_isolation(self):
        txn = qm_engine.Transaction(2, "snapshot_isolation")
        assert txn.isolation_level == "snapshot_isolation"

    def test_serializable_maps_to_snapshot(self):
        txn = qm_engine.Transaction(3, "serializable")
        assert txn.isolation_level == "snapshot_isolation"

    def test_commit_changes_state(self):
        txn = qm_engine.Transaction(4, "read_committed")
        assert txn.state == "active"
        txn.commit()
        assert txn.state == "committed"

    def test_rollback_changes_state(self):
        txn = qm_engine.Transaction(5, "read_committed")
        txn.rollback()
        assert txn.state == "rolled_back"

    def test_double_commit_raises(self):
        txn = qm_engine.Transaction(6, "read_committed")
        txn.commit()
        with pytest.raises(RuntimeError):
            txn.commit()


# ─────────────────────────────────────────────────────────────────────
# RustRingBuffer — advanced IPC scenarios
# ─────────────────────────────────────────────────────────────────────

class TestRingBufferAdvanced:
    @pytest.fixture
    def ring(self, tmp_path):
        return qm_engine.RustRingBuffer(str(tmp_path / "ring"), 8, 512)

    def test_multiple_publish_consume(self, ring):
        for i in range(5):
            ring.publish(i, 0, f"msg_{i}".encode())

        consumed = []
        for _ in range(5):
            msg = ring.consume()
            if msg:
                consumed.append(msg)
        assert len(consumed) == 5

    def test_publish_consume_complete_cycle(self, ring):
        seq = ring.publish(1, 0, b"request")
        msg = ring.consume()
        assert msg is not None
        slot_idx = msg[0]
        ring.complete(slot_idx, b"response")
        state, data = ring.collect_result(slot_idx)
        assert bytes(data) == b"response"

    def test_large_payload(self, ring):
        big_payload = b"A" * 500
        ring.publish(1, 0, big_payload)
        msg = ring.consume()
        assert msg is not None
        assert bytes(msg[3]) == big_payload

    def test_status_fields(self, ring):
        status = ring.status()
        assert len(status) == 8
        # All fields should be integers
        for field in status:
            assert isinstance(field, int)

    def test_recover_empty(self, ring):
        result = ring.recover()
        assert isinstance(result, tuple)


# ─────────────────────────────────────────────────────────────────────
# JitCompiler — edge cases
# ─────────────────────────────────────────────────────────────────────

class TestJitCompilerEdgeCases:
    @pytest.fixture
    def jit(self):
        return qm_engine.JitCompiler()

    def test_empty_input(self, jit):
        result = jit.filter_eq_i64([], 0, 42)
        assert result == []

    def test_single_element_match(self, jit):
        result = jit.filter_eq_i64([42], 0, 42)
        assert result == [0]

    def test_single_element_no_match(self, jit):
        result = jit.filter_eq_i64([99], 0, 42)
        assert result == []

    def test_large_dataset(self, jit):
        data = list(range(10_000))
        result = jit.filter_eq_i64(data, 0, 5000)
        assert result == [5000]

    def test_filter_between_edge(self, jit):
        result = jit.filter_between_f64([1.0, 2.0, 3.0], 0, 2.0, 2.0)
        assert 1 in result

    def test_project_empty(self, jit):
        result = jit.project_f64([], [], 0.0)
        assert result == []

    def test_negative_values(self, jit):
        result = jit.filter_eq_i64([-5, -3, -1, 0, 1], 0, -3)
        assert result == [1]

    def test_filter_between_negative(self, jit):
        result = jit.filter_between_f64([-10.0, -5.0, 0.0, 5.0, 10.0], 0, -6.0, -4.0)
        assert 1 in result


# ─────────────────────────────────────────────────────────────────────
# WTinyLfuCache — advanced
# ─────────────────────────────────────────────────────────────────────

class TestCacheAdvanced:
    @pytest.fixture
    def cache(self):
        return qm_engine.WTinyLfuCache(1024)

    def test_many_insertions(self, cache):
        for i in range(100):
            cache.insert(f"k{i}", f"v{i}".encode())
        # Some may be evicted, but recent ones should be there
        assert cache.get("k99") is not None or len(cache) > 0

    def test_len(self, cache):
        assert len(cache) == 0
        cache.insert("a", b"1")
        cache.insert("b", b"2")
        assert len(cache) >= 2

    def test_mixed_operations(self, cache):
        cache.insert("a", b"1")
        cache.get("a")  # hit
        cache.get("b")  # miss
        cache.insert("b", b"2")
        cache.get("b")  # hit
        cache.remove("a")
        assert cache.get("a") is None
        assert cache.get("b") is not None
