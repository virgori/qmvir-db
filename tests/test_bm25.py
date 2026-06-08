"""Tests for search_platform.lexical_search.bm25 — BM25 scoring engine."""

from __future__ import annotations

import math

import pytest

from search_platform.lexical_search.bm25 import BM25Scorer, LexicalSearchEngine


@pytest.fixture
def scorer() -> BM25Scorer:
    s = BM25Scorer()
    s.add_document("d1", {"title": "introduction to databases", "body": "SQL and storage engines overview"})
    s.add_document("d2", {"title": "vector search guide", "body": "embeddings and approximate nearest neighbor"})
    s.add_document("d3", {"title": "cache invalidation strategies", "body": "TTL version aware caching patterns"})
    return s


class TestBM25Scorer:
    def test_search_returns_results(self, scorer: BM25Scorer) -> None:
        results = scorer.search("database", limit=5)
        assert len(results) > 0
        assert results[0].doc_id == "d1"

    def test_search_relevance_order(self, scorer: BM25Scorer) -> None:
        results = scorer.search("vector embeddings", limit=5)
        assert len(results) > 0
        assert results[0].doc_id == "d2"

    def test_search_no_match(self, scorer: BM25Scorer) -> None:
        results = scorer.search("xyznonexistent", limit=5)
        assert len(results) == 0

    def test_remove_document(self, scorer: BM25Scorer) -> None:
        scorer.remove_document("d1")
        results = scorer.search("database", limit=5)
        assert all(r.doc_id != "d1" for r in results)

    def test_field_weights(self) -> None:
        s = BM25Scorer(field_weights={"title": 3.0, "body": 1.0})
        s.add_document("d1", {"title": "database indexing", "body": "other stuff"})
        s.add_document("d2", {"title": "other stuff", "body": "database indexing"})
        results = s.search("database", limit=2)
        # Title match should rank higher
        assert results[0].doc_id == "d1"

    def test_formula_matches_hand_calculated_fixture(self) -> None:
        s = BM25Scorer(k1=1.2, b=0.75)
        s.add_document("d1", {"body": "alpha alpha beta"})
        s.add_document("d2", {"body": "alpha gamma"})
        s.add_document("d3", {"body": "delta epsilon"})

        avgdl = (3 + 2 + 2) / 3
        idf_alpha = math.log(1 + (3 - 2 + 0.5) / (2 + 0.5))

        def score(tf: float, doc_len: float) -> float:
            numerator = tf * (1.2 + 1)
            denominator = tf + 1.2 * (1 - 0.75 + 0.75 * (doc_len / avgdl))
            return idf_alpha * numerator / denominator

        results = s.search("alpha", limit=10)
        assert [row.doc_id for row in results] == ["d1", "d2"]
        assert results[0].score == pytest.approx(score(2, 3))
        assert results[1].score == pytest.approx(score(1, 2))

    def test_duplicate_document_replaces_old_terms_and_ties_sort_by_id(self) -> None:
        s = BM25Scorer()
        s.add_document("b", {"body": "same token"})
        s.add_document("a", {"body": "same token"})
        assert [row.doc_id for row in s.search("same", limit=10)] == ["a", "b"]

        s.add_document("a", {"body": "replacement only"})
        assert [row.doc_id for row in s.search("replacement", limit=10)] == ["a"]
        assert [row.doc_id for row in s.search("same", limit=10)] == ["b"]

    def test_unicode_token_policy_is_basic_whitespace_lowercase(self) -> None:
        s = BM25Scorer()
        s.add_document("zh", {"body": "数据库 搜索"})
        s.add_document("vi", {"body": "cơ sở dữ liệu"})
        assert s.search("数据库", limit=10)[0].doc_id == "zh"
        assert s.search("DỮ", limit=10)[0].doc_id == "vi"
