"""Tests for search platform: BM25, InvertedIndex, LexicalSearchEngine.

Replaces old test_core.py InvertedIndex section + extends test_bm25.py coverage.
"""
from __future__ import annotations

import pytest

from search_platform.lexical_search.bm25 import BM25Scorer, LexicalSearchEngine
from search_platform.inverted_index.index import InvertedIndex


# ─────────────────────────────────────────────────────────────────────
# InvertedIndex
# ─────────────────────────────────────────────────────────────────────

class TestInvertedIndex:
    @pytest.fixture
    def idx(self):
        return InvertedIndex()

    def test_add_and_lookup(self, idx):
        idx.add_document("d1", {"title": ["hello", "world"]})
        posting = idx.lookup("hello")
        assert posting is not None
        assert "d1" in posting.get_doc_ids()

    def test_lookup_missing(self, idx):
        assert idx.lookup("nonexistent") is None

    def test_multiple_documents(self, idx):
        idx.add_document("d1", {"body": ["python", "database"]})
        idx.add_document("d2", {"body": ["rust", "database"]})
        idx.add_document("d3", {"body": ["python", "web"]})

        db_posting = idx.lookup("database")
        assert "d1" in db_posting.get_doc_ids()
        assert "d2" in db_posting.get_doc_ids()
        assert "d3" not in db_posting.get_doc_ids()

    def test_remove_document(self, idx):
        idx.add_document("d1", {"body": ["hello"]})
        idx.remove_document("d1")
        posting = idx.lookup("hello")
        if posting is not None:
            assert "d1" not in posting.get_doc_ids()

    def test_term_count(self, idx):
        assert idx.term_count == 0
        idx.add_document("d1", {"body": ["a", "b", "c"]})
        assert idx.term_count == 3

    def test_doc_count(self, idx):
        assert idx.doc_count == 0
        idx.add_document("d1", {"body": ["hello"]})
        idx.add_document("d2", {"body": ["world"]})
        assert idx.doc_count == 2

    def test_prefix_lookup(self, idx):
        idx.add_document("d1", {"body": ["database", "datastore", "cache"]})
        results = idx.prefix_lookup("data")
        terms = [p.term for p in results]
        assert "database" in terms
        assert "datastore" in terms
        assert "cache" not in terms

    def test_phrase_search(self, idx):
        idx.add_document("d1", {"body": ["the", "quick", "brown", "fox"]})
        idx.add_document("d2", {"body": ["the", "slow", "brown", "fox"]})
        results = idx.phrase_search(["brown", "fox"])
        assert "d1" in results
        assert "d2" in results

    def test_stored_fields(self, idx):
        idx.add_document("d1", {"body": ["hello"]}, stored_fields={"url": "http://example.com"})
        fields = idx.get_stored_fields("d1")
        assert fields is not None
        assert fields["url"] == "http://example.com"

    def test_posting_intersect(self, idx):
        idx.add_document("d1", {"body": ["python", "rust"]})
        idx.add_document("d2", {"body": ["python", "go"]})
        idx.add_document("d3", {"body": ["rust", "go"]})

        p_python = idx.lookup("python")
        p_rust = idx.lookup("rust")
        intersection = p_python.intersect(p_rust)
        assert "d1" in intersection
        assert len(intersection) == 1

    def test_posting_union(self, idx):
        idx.add_document("d1", {"body": ["python"]})
        idx.add_document("d2", {"body": ["rust"]})

        p_python = idx.lookup("python")
        p_rust = idx.lookup("rust")
        combined = p_python.union(p_rust)
        assert "d1" in combined
        assert "d2" in combined

    def test_multi_field(self, idx):
        idx.add_document("d1", {
            "title": ["machine", "learning"],
            "body": ["neural", "networks", "deep", "learning"],
        })
        posting = idx.lookup("learning")
        assert posting is not None
        assert "d1" in posting.get_doc_ids()


# ─────────────────────────────────────────────────────────────────────
# LexicalSearchEngine
# ─────────────────────────────────────────────────────────────────────

class TestLexicalSearchEngine:
    @pytest.fixture
    def engine(self):
        return LexicalSearchEngine()

    def test_create_collection(self, engine):
        scorer = engine.get_or_create_collection("articles")
        assert scorer is not None

    def test_index_and_search(self, engine):
        engine.index_document("articles", "a1", {"title": "intro to databases", "body": "sql and storage"})
        engine.index_document("articles", "a2", {"title": "vector search", "body": "embeddings ann"})
        engine.index_document("articles", "a3", {"title": "cache strategy", "body": "ttl invalidation"})

        result = engine.search("articles", ["databases", "sql"])
        assert len(result.hits) > 0
        # doc a1 should be most relevant
        assert result.hits[0].doc_id == "a1"

    def test_search_no_match(self, engine):
        engine.index_document("articles", "a1", {"title": "hello world"})
        result = engine.search("articles", ["nonexistent"])
        assert len(result.hits) == 0

    def test_search_limit(self, engine):
        for i in range(20):
            engine.index_document("docs", f"d{i}", {"body": f"keyword common word{i}"})
        result = engine.search("docs", ["keyword"], limit=5)
        assert len(result.hits) <= 5

    def test_multiple_collections(self, engine):
        engine.index_document("col1", "d1", {"body": "alpha"})
        engine.index_document("col2", "d2", {"body": "beta"})
        r1 = engine.search("col1", ["alpha"])
        r2 = engine.search("col2", ["beta"])
        assert len(r1.hits) > 0
        assert len(r2.hits) > 0

    def test_field_weights(self, engine):
        engine.index_document("articles", "a1", {"title": "database", "body": "general text"})
        engine.index_document("articles", "a2", {"body": "database is mentioned here in body"})
        result = engine.search("articles", ["database"], field_weights={"title": 2.0, "body": 1.0})
        assert len(result.hits) > 0


# ─────────────────────────────────────────────────────────────────────
# BM25Scorer — additional coverage (beyond test_bm25.py)
# ─────────────────────────────────────────────────────────────────────

class TestBM25Extended:
    @pytest.fixture
    def scorer(self):
        s = BM25Scorer()
        s.add_document("d1", {"title": ["intro", "database"], "body": ["sql", "storage", "engine"]})
        s.add_document("d2", {"title": ["vector", "search"], "body": ["embeddings", "ann", "hnsw"]})
        s.add_document("d3", {"title": ["cache", "strategy"], "body": ["ttl", "invalidation", "lru"]})
        return s

    def test_doc_count(self, scorer):
        assert scorer.doc_count == 3

    def test_relevance_ordering(self, scorer):
        hits = scorer.search(["database", "sql"])
        assert len(hits) > 0
        assert hits[0].doc_id == "d1"

    def test_remove_document(self, scorer):
        scorer.remove_document("d1")
        assert scorer.doc_count == 2
        hits = scorer.search(["database"])
        # d1 should not appear
        doc_ids = [h.doc_id for h in hits]
        assert "d1" not in doc_ids

    def test_custom_params(self):
        s = BM25Scorer(k1=2.0, b=0.5)
        s.add_document("d1", {"body": ["test", "query"]})
        hits = s.search(["test"])
        assert len(hits) == 1

    def test_single_token_search(self, scorer):
        hits = scorer.search(["hnsw"])
        assert len(hits) == 1
        assert hits[0].doc_id == "d2"
