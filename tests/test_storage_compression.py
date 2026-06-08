"""Tests for storage layer: RowStore, CompressionEngine, ColumnarStore.

Replaces old test_core.py BufferPool/compaction sections + test_phases789.py storage tests.
"""
from __future__ import annotations

import pytest

from storage.row_store.heap import RowStore
from storage.compression.engine import (
    CompressionCodec,
    CompressionEngine,
    CompressionTier,
    DeltaEncoder,
    DictionaryEncoder,
    RLEEncoder,
)
from analytics_platform.columnar_store.columnar import ColumnarStore


# ─────────────────────────────────────────────────────────────────────
# RowStore
# ─────────────────────────────────────────────────────────────────────

class TestRowStore:
    @pytest.fixture
    def store(self):
        return RowStore()

    def test_insert_and_get(self, store):
        page_id, slot_id = store.insert("users", "u1", 1, {"name": "alice", "age": 30})
        row = store.get("users", "u1")
        assert row is not None
        assert row["name"] == "alice"

    def test_get_missing(self, store):
        assert store.get("users", "missing") is None

    def test_delete(self, store):
        store.insert("users", "u1", 1, {"name": "alice"})
        assert store.delete("users", "u1", 1) is True
        assert store.get("users", "u1") is None

    def test_delete_missing(self, store):
        assert store.delete("users", "missing", 1) is False

    def test_page_count(self, store):
        for i in range(10):
            store.insert("users", f"u{i}", 1, {"name": f"user{i}"})
        assert store.page_count >= 1

    def test_total_tuples(self, store):
        assert store.total_tuples == 0
        store.insert("users", "u1", 1, {"name": "alice"})
        store.insert("users", "u2", 1, {"name": "bob"})
        assert store.total_tuples == 2

    def test_multiple_tables(self, store):
        store.insert("users", "u1", 1, {"name": "alice"})
        store.insert("orders", "o1", 1, {"total": 100})
        assert store.get("users", "u1")["name"] == "alice"
        assert store.get("orders", "o1")["total"] == 100

    def test_overwrite_same_pk(self, store):
        store.insert("users", "u1", 1, {"name": "alice"})
        store.insert("users", "u1", 2, {"name": "Alice"})
        row = store.get("users", "u1")
        assert row["name"] == "Alice"


# ─────────────────────────────────────────────────────────────────────
# CompressionEngine
# ─────────────────────────────────────────────────────────────────────

class TestCompressionEngine:
    @pytest.fixture
    def engine(self):
        return CompressionEngine()

    def test_compress_decompress_lz4(self, engine):
        data = b"hello world " * 100
        compressed = engine.compress(data, CompressionCodec.LZ4)
        decompressed = engine.decompress(compressed, CompressionCodec.LZ4)
        assert decompressed == data

    def test_compress_decompress_zstd(self, engine):
        data = b"test data for zstd compression " * 50
        compressed = engine.compress(data, CompressionCodec.ZSTD_FAST)
        decompressed = engine.decompress(compressed, CompressionCodec.ZSTD_FAST)
        assert decompressed == data

    def test_compression_reduces_size(self, engine):
        data = b"repeated pattern " * 1000
        compressed = engine.compress(data, CompressionCodec.ZSTD_FAST)
        assert len(compressed) < len(data)

    def test_compress_for_tier(self, engine):
        data = b"tier test " * 100
        compressed = engine.compress_for_tier(data, CompressionTier.HOT)
        assert len(compressed) > 0

    def test_estimate_ratio(self):
        ratio = CompressionEngine.estimate_ratio(CompressionCodec.LZ4)
        assert ratio > 0

    def test_small_data(self, engine):
        data = b"hi"
        compressed = engine.compress(data, CompressionCodec.LZ4)
        decompressed = engine.decompress(compressed, CompressionCodec.LZ4)
        assert decompressed == data

    def test_empty_data(self, engine):
        data = b""
        compressed = engine.compress(data, CompressionCodec.LZ4)
        decompressed = engine.decompress(compressed, CompressionCodec.LZ4)
        assert decompressed == data


class TestDeltaEncoder:
    def test_encode_decode(self):
        values = [100, 102, 105, 110, 115]
        encoded = DeltaEncoder.encode(values)
        decoded = DeltaEncoder.decode(encoded)
        assert decoded == values

    def test_single_value(self):
        values = [42]
        encoded = DeltaEncoder.encode(values)
        decoded = DeltaEncoder.decode(encoded)
        assert decoded == values

    def test_constant_values(self):
        values = [5, 5, 5, 5, 5]
        encoded = DeltaEncoder.encode(values)
        decoded = DeltaEncoder.decode(encoded)
        assert decoded == values


class TestDictionaryEncoder:
    def test_encode_decode(self):
        enc = DictionaryEncoder()
        values = ["apple", "banana", "apple", "cherry", "banana", "apple"]
        codes = enc.encode(values)
        decoded = enc.decode(codes)
        assert decoded == values

    def test_dict_size(self):
        enc = DictionaryEncoder()
        enc.encode(["a", "b", "c", "a", "b"])
        assert enc.dict_size == 3

    def test_empty(self):
        enc = DictionaryEncoder()
        codes = enc.encode([])
        decoded = enc.decode(codes)
        assert decoded == []


class TestRLEEncoder:
    def test_encode_decode(self):
        values = [1, 1, 1, 2, 2, 3, 3, 3, 3]
        runs = RLEEncoder.encode(values)
        decoded = RLEEncoder.decode(runs)
        assert decoded == values

    def test_no_runs(self):
        values = [1, 2, 3, 4, 5]
        runs = RLEEncoder.encode(values)
        decoded = RLEEncoder.decode(runs)
        assert decoded == values

    def test_single_long_run(self):
        values = [7] * 100
        runs = RLEEncoder.encode(values)
        assert len(runs) == 1
        decoded = RLEEncoder.decode(runs)
        assert decoded == values


# ─────────────────────────────────────────────────────────────────────
# ColumnarStore — extended (beyond test_columnar.py)
# ─────────────────────────────────────────────────────────────────────

class TestColumnarStoreExtended:
    @pytest.fixture
    def store(self):
        cs = ColumnarStore()
        cs.insert("sales", {"product": "laptop", "qty": 10, "price": 999.99, "region": "US"})
        cs.insert("sales", {"product": "phone", "qty": 50, "price": 699.99, "region": "EU"})
        cs.insert("sales", {"product": "laptop", "qty": 5, "price": 999.99, "region": "EU"})
        cs.insert("sales", {"product": "tablet", "qty": 20, "price": 499.99, "region": "US"})
        cs.insert("sales", {"product": "phone", "qty": 30, "price": 699.99, "region": "US"})
        return cs

    def test_scan_all(self, store):
        rows = store.scan("sales")
        assert len(rows) == 5

    def test_scan_column_pruning(self, store):
        rows = store.scan("sales", columns=["product", "qty"])
        assert len(rows) == 5
        assert "price" not in rows[0]

    def test_scan_with_filter(self, store):
        rows = store.scan("sales", where={"region": "US"})
        assert all(r["region"] == "US" for r in rows)

    def test_aggregate_sum(self, store):
        results = store.aggregate(
            "sales",
            group_by=["product"],
            metrics=[{"column": "qty", "op": "SUM"}],
        )
        assert len(results) >= 2

    def test_aggregate_count(self, store):
        results = store.aggregate(
            "sales",
            group_by=["region"],
            metrics=[{"column": "qty", "op": "COUNT"}],
        )
        assert len(results) == 2  # US and EU

    def test_aggregate_avg(self, store):
        results = store.aggregate(
            "sales",
            group_by=["region"],
            metrics=[{"column": "price", "op": "AVG"}],
        )
        assert len(results) >= 1

    def test_large_insert(self):
        cs = ColumnarStore()
        for i in range(500):
            cs.insert("big", {"id": i, "value": i * 10})
        rows = cs.scan("big")
        assert len(rows) == 500
