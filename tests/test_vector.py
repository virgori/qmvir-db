"""Tests for vector_platform — embedding store and ANN index."""

from __future__ import annotations

import numpy as np
import pytest

from vector_platform.embedding_store.store import EmbeddingStore
from vector_platform.ann_index.hnsw import BruteForceIndex, DistanceMetric
from vector_platform.metadata_filter.filters import MetadataFilterEngine, FilterOp
from vector_platform.quantization.quantizer import QuantizationType, VectorQuantizer


class TestEmbeddingStore:
    def test_insert_and_get(self) -> None:
        store = EmbeddingStore(dimension=4)
        store.insert("v1", np.array([1.0, 0.0, 0.0, 0.0]), {"label": "a"})
        entry = store.get("v1")
        assert entry is not None
        assert entry.metadata["label"] == "a"
        np.testing.assert_array_equal(entry.vector, [1.0, 0.0, 0.0, 0.0])

    def test_delete(self) -> None:
        store = EmbeddingStore(dimension=4)
        store.insert("v1", np.array([1.0, 0.0, 0.0, 0.0]))
        store.delete("v1")
        assert store.get("v1") is None


class TestBruteForceIndex:
    def test_search_cosine(self) -> None:
        idx = BruteForceIndex(dimension=3, metric=DistanceMetric.COSINE)
        idx.add("a", np.array([1.0, 0.0, 0.0]))
        idx.add("b", np.array([0.0, 1.0, 0.0]))
        idx.add("c", np.array([0.9, 0.1, 0.0]))
        results = idx.search(np.array([1.0, 0.0, 0.0]), k=2)
        ids = [r.doc_id for r in results.results]
        assert "a" in ids
        assert "c" in ids

    def test_search_l2(self) -> None:
        idx = BruteForceIndex(dimension=2, metric=DistanceMetric.L2)
        idx.add("a", np.array([0.0, 0.0]))
        idx.add("b", np.array([10.0, 10.0]))
        results = idx.search(np.array([0.0, 0.0]), k=1)
        assert results.results[0].doc_id == "a"


class TestMetadataFilter:
    def test_eq_filter(self) -> None:
        engine = MetadataFilterEngine()
        fn = engine.build_filter_fn([{"field": "color", "op": FilterOp.EQ, "value": "red"}])
        assert fn({"color": "red"}) is True
        assert fn({"color": "blue"}) is False

    def test_in_filter(self) -> None:
        engine = MetadataFilterEngine()
        fn = engine.build_filter_fn([{"field": "size", "op": FilterOp.IN, "value": [1, 2, 3]}])
        assert fn({"size": 2}) is True
        assert fn({"size": 9}) is False


class TestVectorQuantizer:
    def test_fp16_roundtrip(self) -> None:
        q = VectorQuantizer()
        v = np.array([1.0, 2.0, 3.0], dtype=np.float32)
        encoded = q.to_fp16(v)
        decoded = q.from_fp16(encoded)
        np.testing.assert_allclose(decoded, v, atol=1e-3)

    def test_int8_roundtrip(self) -> None:
        q = VectorQuantizer()
        v = np.array([0.5, -0.5, 0.0], dtype=np.float32)
        encoded, params = q.to_int8(v)
        decoded = q.from_int8(encoded, params)
        np.testing.assert_allclose(decoded, v, atol=0.02)
