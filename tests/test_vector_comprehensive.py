"""Tests for vector platform: EmbeddingStore, HNSWIndex, MetadataFilters, VectorQuantizer.

Replaces old test_core.py HNSW/PQ sections + extends test_vector.py coverage.
"""
from __future__ import annotations

import numpy as np
import pytest

from vector_platform.embedding_store.store import EmbeddingStore
from vector_platform.ann_index.hnsw import BruteForceIndex, DistanceMetric, HNSWIndex
from vector_platform.quantization.quantizer import VectorQuantizer
from vector_platform.metadata_filter.filters import FilterOp, MetadataFilter, MetadataFilterEngine


# ─────────────────────────────────────────────────────────────────────
# EmbeddingStore
# ─────────────────────────────────────────────────────────────────────

class TestEmbeddingStore:
    @pytest.fixture
    def store(self):
        return EmbeddingStore(dimension=4)

    def test_insert_and_get(self, store):
        vec = np.array([1.0, 0.0, 0.0, 0.0], dtype=np.float32)
        store.insert("v1", vec, metadata={"category": "test"})
        entry = store.get("v1")
        assert entry is not None
        np.testing.assert_array_almost_equal(entry.vector, vec)

    def test_get_missing(self, store):
        assert store.get("nonexistent") is None

    def test_delete(self, store):
        vec = np.array([1.0, 0.0, 0.0, 0.0], dtype=np.float32)
        store.insert("v1", vec)
        assert store.delete("v1") is True
        assert store.get("v1") is None

    def test_delete_nonexistent(self, store):
        assert store.delete("missing") is False

    def test_total_vectors(self, store):
        assert store.total_vectors == 0
        store.insert("v1", np.zeros(4, dtype=np.float32))
        store.insert("v2", np.ones(4, dtype=np.float32))
        assert store.total_vectors == 2

    def test_get_all_vectors(self, store):
        store.insert("v1", np.array([1, 0, 0, 0], dtype=np.float32))
        store.insert("v2", np.array([0, 1, 0, 0], dtype=np.float32))
        entries = store.get_all_vectors()
        assert len(entries) == 2

    def test_get_vectors_matrix(self, store):
        store.insert("v1", np.array([1, 0, 0, 0], dtype=np.float32))
        store.insert("v2", np.array([0, 1, 0, 0], dtype=np.float32))
        ids, matrix = store.get_vectors_matrix()
        assert len(ids) == 2
        assert matrix.shape == (2, 4)

    def test_namespaces(self, store):
        store.insert("v1", np.zeros(4, dtype=np.float32), namespace="ns1")
        store.insert("v2", np.ones(4, dtype=np.float32), namespace="ns2")
        ns1_entries = store.get_all_vectors(namespace="ns1")
        ns2_entries = store.get_all_vectors(namespace="ns2")
        assert len(ns1_entries) == 1
        assert len(ns2_entries) == 1

    def test_metadata_stored(self, store):
        vec = np.zeros(4, dtype=np.float32)
        store.insert("v1", vec, metadata={"color": "red", "count": 5})
        entry = store.get("v1")
        assert entry.metadata["color"] == "red"
        assert entry.metadata["count"] == 5


# ─────────────────────────────────────────────────────────────────────
# BruteForceIndex (ANN)
# ─────────────────────────────────────────────────────────────────────

class TestBruteForceIndex:
    @pytest.fixture
    def index(self):
        idx = BruteForceIndex(dimension=4, metric=DistanceMetric.COSINE)
        idx.add("v1", np.array([1, 0, 0, 0], dtype=np.float32))
        idx.add("v2", np.array([0, 1, 0, 0], dtype=np.float32))
        idx.add("v3", np.array([0.5, 0.5, 0, 0], dtype=np.float32))
        idx.add("v4", np.array([0, 0, 1, 0], dtype=np.float32))
        return idx

    def test_search_basic(self, index):
        query = np.array([1, 0, 0, 0], dtype=np.float32)
        result = index.search(query, top_k=2)
        assert len(result.results) == 2
        assert result.results[0].vector_id == "v1"

    def test_search_cosine_similarity(self, index):
        query = np.array([0.5, 0.5, 0, 0], dtype=np.float32)
        result = index.search(query, top_k=1)
        assert result.results[0].vector_id == "v3"

    def test_search_with_filter(self, index):
        index.add("v5", np.array([1, 0, 0, 0], dtype=np.float32), metadata={"type": "special"})
        query = np.array([1, 0, 0, 0], dtype=np.float32)
        result = index.search(query, top_k=10, filter_fn=lambda m: m.get("type") == "special")
        # Only v5 has metadata matching filter
        assert any(r.vector_id == "v5" for r in result.results)

    def test_remove(self, index):
        index.remove("v1")
        assert index.size == 3

    def test_size(self, index):
        assert index.size == 4

    def test_l2_metric(self):
        idx = BruteForceIndex(dimension=2, metric=DistanceMetric.L2)
        idx.add("a", np.array([0, 0], dtype=np.float32))
        idx.add("b", np.array([1, 0], dtype=np.float32))
        idx.add("c", np.array([10, 10], dtype=np.float32))
        result = idx.search(np.array([0, 0], dtype=np.float32), top_k=1)
        assert result.results[0].vector_id == "a"

    def test_exact_l2_ordering_matches_numpy_reference(self):
        idx = BruteForceIndex(dimension=2, metric="l2")
        vectors = {
            "a": np.array([0.0, 0.0], dtype=np.float32),
            "b": np.array([2.0, 0.0], dtype=np.float32),
            "c": np.array([0.0, 3.0], dtype=np.float32),
        }
        for vid, vec in vectors.items():
            idx.add(vid, vec)

        query = np.array([1.0, 0.0], dtype=np.float32)
        result = idx.search(query, top_k=3)
        expected = sorted(
            vectors,
            key=lambda vid: (float(np.linalg.norm(vectors[vid] - query)), vid),
        )
        assert [row.vector_id for row in result.results] == expected

    def test_equal_distance_tie_break_is_deterministic(self):
        idx = BruteForceIndex(dimension=2, metric=DistanceMetric.L2)
        idx.add("b", np.array([1.0, 0.0], dtype=np.float32))
        idx.add("a", np.array([-1.0, 0.0], dtype=np.float32))
        result = idx.search(np.array([0.0, 0.0], dtype=np.float32), top_k=2)
        assert [row.vector_id for row in result.results] == ["a", "b"]

    def test_top_k_edges(self):
        idx = BruteForceIndex(dimension=2, metric=DistanceMetric.L2)
        idx.add("a", np.array([0.0, 0.0], dtype=np.float32))
        assert idx.search(np.array([0.0, 0.0], dtype=np.float32), top_k=0).results == []
        assert len(idx.search(np.array([0.0, 0.0], dtype=np.float32), top_k=10).results) == 1
        with pytest.raises(ValueError):
            idx.search(np.array([0.0, 0.0], dtype=np.float32), top_k=-1)

    def test_dimension_and_finite_validation(self):
        idx = BruteForceIndex(dimension=2, metric=DistanceMetric.L2)
        with pytest.raises(ValueError):
            idx.add("bad", np.array([1.0], dtype=np.float32))
        with pytest.raises(ValueError):
            idx.add("nan", np.array([np.nan, 0.0], dtype=np.float32))
        idx.add("ok", np.array([1.0, 0.0], dtype=np.float32))
        with pytest.raises(ValueError):
            idx.search(np.array([1.0, 0.0, 0.0], dtype=np.float32))
        with pytest.raises(ValueError):
            BruteForceIndex(dimension=0)
        with pytest.raises(ValueError):
            BruteForceIndex(dimension=2, metric="not-a-metric")

    def test_duplicate_id_replaces_existing_vector_and_metadata(self):
        idx = BruteForceIndex(dimension=2, metric=DistanceMetric.L2)
        idx.add("same", np.array([10.0, 10.0], dtype=np.float32), metadata={"version": 1})
        idx.add("same", np.array([0.0, 0.0], dtype=np.float32), metadata={"version": 2})
        result = idx.search(np.array([0.0, 0.0], dtype=np.float32), top_k=1)
        assert idx.size == 1
        assert result.results[0].vector_id == "same"
        assert result.results[0].metadata["version"] == 2


# ─────────────────────────────────────────────────────────────────────
# HNSWIndex (if available)
# ─────────────────────────────────────────────────────────────────────

class TestHNSWIndex:
    @pytest.fixture
    def index(self):
        try:
            from vector_platform.ann_index.hnsw import HNSWIndex
        except ImportError:
            pytest.skip("HNSWIndex not available")
        idx = HNSWIndex(dimension=4, metric=DistanceMetric.COSINE, ef_construction=50, M=8)
        for i in range(20):
            vec = np.random.randn(4).astype(np.float32)
            vec = vec / np.linalg.norm(vec)
            idx.add(f"v{i}", vec)
        return idx

    def test_search_returns_results(self, index):
        query = np.random.randn(4).astype(np.float32)
        query = query / np.linalg.norm(query)
        result = index.search(query, top_k=5)
        assert len(result.results) == 5

    def test_size(self, index):
        assert index.size == 20

    def test_empty_and_top_k_zero(self):
        idx = HNSWIndex(dimension=4, metric=DistanceMetric.COSINE, ef_construction=50, M=8)
        query = np.ones(4, dtype=np.float32)
        assert idx.search(query, top_k=5).results == []
        assert idx.search(query, top_k=0).results == []

    def test_recall_against_exact_reference(self):
        rng = np.random.default_rng(1234)
        vectors = rng.normal(size=(120, 8)).astype(np.float32)
        vectors /= np.linalg.norm(vectors, axis=1, keepdims=True)
        query = rng.normal(size=8).astype(np.float32)
        query /= np.linalg.norm(query)

        exact = BruteForceIndex(dimension=8, metric=DistanceMetric.COSINE)
        ann = HNSWIndex(dimension=8, metric=DistanceMetric.COSINE, ef_construction=100, M=16)
        for idx, vec in enumerate(vectors):
            vector_id = f"v{idx}"
            exact.add(vector_id, vec)
            ann.add(vector_id, vec)

        expected = {row.vector_id for row in exact.search(query, top_k=10).results}
        actual = {row.vector_id for row in ann.search(query, top_k=10).results}
        recall = len(expected & actual) / len(expected)
        assert recall >= 0.8

    def test_hnsw_validation_matches_exact_index(self):
        idx = HNSWIndex(dimension=2, metric="l2")
        with pytest.raises(ValueError):
            idx.add("bad", np.array([1.0], dtype=np.float32))
        with pytest.raises(ValueError):
            idx.search(np.array([np.inf, 0.0], dtype=np.float32))
        with pytest.raises(ValueError):
            HNSWIndex(dimension=-1)

    def test_hnsw_duplicate_id_replace_remove_and_reinsert(self):
        idx = HNSWIndex(dimension=2, metric="l2", ef_construction=50, M=8)
        idx.add("same", np.array([100.0, 100.0], dtype=np.float32), metadata={"version": 1})
        idx.add("other", np.array([10.0, 0.0], dtype=np.float32))
        idx.add("same", np.array([0.0, 0.0], dtype=np.float32), metadata={"version": 2})

        result = idx.search(np.array([0.0, 0.0], dtype=np.float32), top_k=10)
        assert idx.size == 2
        assert [row.vector_id for row in result.results].count("same") == 1
        assert result.results[0].vector_id == "same"
        assert result.results[0].metadata["version"] == 2

        idx.remove("same")
        result = idx.search(np.array([0.0, 0.0], dtype=np.float32), top_k=10)
        assert idx.size == 1
        assert all(row.vector_id != "same" for row in result.results)

        idx.add("same", np.array([0.1, 0.0], dtype=np.float32), metadata={"version": 3})
        result = idx.search(np.array([0.0, 0.0], dtype=np.float32), top_k=10)
        assert idx.size == 2
        assert result.results[0].vector_id == "same"
        assert result.results[0].metadata["version"] == 3

    def test_hnsw_rebuild_first_medium_fixture_matches_exact_recall_boundary(self):
        rng = np.random.default_rng(20260608)
        vectors = rng.normal(size=(256, 16)).astype(np.float32)
        vectors /= np.maximum(np.linalg.norm(vectors, axis=1, keepdims=True), 1e-12)
        query = rng.normal(size=16).astype(np.float32)
        query /= max(float(np.linalg.norm(query)), 1e-12)

        exact = BruteForceIndex(dimension=16, metric=DistanceMetric.COSINE)
        hot = HNSWIndex(dimension=16, metric=DistanceMetric.COSINE, ef_construction=80, M=12)
        for i, vec in enumerate(vectors):
            exact.add(f"v{i}", vec)
            hot.add(f"v{i}", vec)

        expected = [row.vector_id for row in exact.search(query, top_k=10).results]
        hot_ids = [row.vector_id for row in hot.search(query, top_k=10).results]

        # Rebuild-first is a cold-ish fixture: it drops the in-memory object and
        # rebuilds immediately before querying; it does not claim OS cold cache.
        rebuilt = HNSWIndex(dimension=16, metric=DistanceMetric.COSINE, ef_construction=80, M=12)
        for i, vec in enumerate(vectors):
            rebuilt.add(f"v{i}", vec)
        rebuilt_ids = [row.vector_id for row in rebuilt.search(query, top_k=10).results]

        hot_recall = len(set(expected) & set(hot_ids)) / 10
        rebuilt_recall = len(set(expected) & set(rebuilt_ids)) / 10
        assert hot_recall >= 0.8
        assert rebuilt_recall >= 0.8
        assert len(hot_ids) == len(set(hot_ids))
        assert len(rebuilt_ids) == len(set(rebuilt_ids))


# ─────────────────────────────────────────────────────────────────────
# MetadataFilterEngine
# ─────────────────────────────────────────────────────────────────────

class TestMetadataFilterEngine:
    @pytest.fixture
    def engine(self):
        return MetadataFilterEngine()

    def test_eq_filter(self, engine):
        f = MetadataFilter(field="status", op=FilterOp.EQ, value="active")
        assert engine.matches({"status": "active"}, [f]) is True
        assert engine.matches({"status": "inactive"}, [f]) is False

    def test_ne_filter(self, engine):
        f = MetadataFilter(field="status", op=FilterOp.NE, value="deleted")
        assert engine.matches({"status": "active"}, [f]) is True
        assert engine.matches({"status": "deleted"}, [f]) is False

    def test_gt_filter(self, engine):
        f = MetadataFilter(field="age", op=FilterOp.GT, value=18)
        assert engine.matches({"age": 25}, [f]) is True
        assert engine.matches({"age": 15}, [f]) is False

    def test_lt_filter(self, engine):
        f = MetadataFilter(field="score", op=FilterOp.LT, value=5.0)
        assert engine.matches({"score": 3.5}, [f]) is True
        assert engine.matches({"score": 7.0}, [f]) is False

    def test_in_filter(self, engine):
        f = MetadataFilter(field="color", op=FilterOp.IN, value=["red", "blue"])
        assert engine.matches({"color": "red"}, [f]) is True
        assert engine.matches({"color": "green"}, [f]) is False

    def test_multiple_filters_and(self, engine):
        filters = [
            MetadataFilter(field="status", op=FilterOp.EQ, value="active"),
            MetadataFilter(field="age", op=FilterOp.GT, value=18),
        ]
        assert engine.matches({"status": "active", "age": 25}, filters) is True
        assert engine.matches({"status": "active", "age": 15}, filters) is False
        assert engine.matches({"status": "inactive", "age": 25}, filters) is False

    def test_build_filter_fn(self, engine):
        filters = [MetadataFilter(field="type", op=FilterOp.EQ, value="article")]
        fn = engine.build_filter_fn(filters)
        assert fn({"type": "article"}) is True
        assert fn({"type": "video"}) is False

    def test_parse_filters(self):
        raw = {"status": "active", "age": {"$gt": 18}}
        filters = MetadataFilterEngine.parse_filters(raw)
        assert len(filters) >= 1

    def test_missing_field(self, engine):
        f = MetadataFilter(field="missing_field", op=FilterOp.EQ, value="x")
        assert engine.matches({"other": "y"}, [f]) is False


# ─────────────────────────────────────────────────────────────────────
# VectorQuantizer — extended
# ─────────────────────────────────────────────────────────────────────

class TestVectorQuantizerExtended:
    def test_fp16_roundtrip_accuracy(self):
        original = np.random.randn(10, 128).astype(np.float32)
        fp16 = VectorQuantizer.to_fp16(original)
        recovered = VectorQuantizer.from_fp16(fp16)
        np.testing.assert_allclose(original, recovered, rtol=1e-3, atol=1e-3)

    def test_int8_roundtrip(self):
        original = np.random.randn(10, 64).astype(np.float32)
        quantized, params = VectorQuantizer.to_int8(original)
        recovered = VectorQuantizer.from_int8(quantized, params)
        # INT8 has lower precision
        np.testing.assert_allclose(original, recovered, rtol=0.1, atol=0.1)

    def test_memory_usage(self):
        usage_f32 = VectorQuantizer.memory_usage(1000, 128, "float32")
        usage_f16 = VectorQuantizer.memory_usage(1000, 128, "float16")
        assert usage_f32 > usage_f16

    def test_exact_memory_usage_and_aliases(self):
        assert VectorQuantizer.memory_usage(10, 8, "float32") == 320
        assert VectorQuantizer.memory_usage(10, 8, "fp32") == 320
        assert VectorQuantizer.memory_usage(10, 8, "float16") == 160
        assert VectorQuantizer.memory_usage(10, 8, "fp16") == 160
        assert VectorQuantizer.memory_usage(10, 8, "int8") == 80
        assert VectorQuantizer.memory_usage(10, 8, "uint8") == 80
        with pytest.raises(ValueError):
            VectorQuantizer.memory_usage(10, 8, "mystery")
        with pytest.raises(ValueError):
            VectorQuantizer.memory_usage(-1, 8, "float32")

    def test_compression_ratio(self):
        ratio = VectorQuantizer.compression_ratio("float32", "float16")
        assert ratio == pytest.approx(2.0)

    def test_compression_ratio_int8(self):
        assert VectorQuantizer.compression_ratio("float32", "int8") == pytest.approx(4.0)
        with pytest.raises(ValueError):
            VectorQuantizer.compression_ratio("float32", "unknown")

    def test_fp16_preserves_shape(self):
        original = np.random.randn(5, 32).astype(np.float32)
        fp16 = VectorQuantizer.to_fp16(original)
        assert fp16.shape == original.shape

    def test_quantization_rejects_empty_or_non_finite_inputs(self):
        with pytest.raises(ValueError):
            VectorQuantizer.to_int8(np.array([], dtype=np.float32))
        with pytest.raises(ValueError):
            VectorQuantizer.to_int8(np.array([np.nan], dtype=np.float32))
        with pytest.raises(ValueError):
            VectorQuantizer.to_fp16(np.array([np.inf], dtype=np.float32))
        with pytest.raises(ValueError):
            VectorQuantizer.from_int8(np.array([], dtype=np.uint8), {"scale": 1.0, "offset": 0.0})
        with pytest.raises(ValueError):
            VectorQuantizer.from_int8(np.array([1], dtype=np.uint8), {"scale": np.nan, "offset": 0.0})

    def test_int8_error_metrics_are_bounded(self):
        original = np.linspace(-1.0, 1.0, 256, dtype=np.float32).reshape(16, 16)
        quantized, params = VectorQuantizer.to_int8(original)
        recovered = VectorQuantizer.from_int8(quantized, params)
        error = np.abs(original - recovered)
        assert float(error.mean()) < 0.005
        assert float(error.max()) < 0.01

    def test_quantization_recall_loss_against_exact_reference(self):
        rng = np.random.default_rng(20260604)
        vectors = rng.normal(size=(100, 16)).astype(np.float32)
        query = rng.normal(size=16).astype(np.float32)

        quantized, params = VectorQuantizer.to_int8(vectors)
        recovered = VectorQuantizer.from_int8(quantized, params)

        exact_dist = np.linalg.norm(vectors - query, axis=1)
        recovered_dist = np.linalg.norm(recovered - query, axis=1)
        exact_top10 = set(np.argsort(exact_dist)[:10].tolist())
        recovered_top10 = set(np.argsort(recovered_dist)[:10].tolist())
        recall_at_10 = len(exact_top10 & recovered_top10) / 10
        error = np.abs(vectors - recovered)

        assert float(error.mean()) < 0.01
        assert float(error.max()) < 0.03
        assert recall_at_10 >= 0.9
