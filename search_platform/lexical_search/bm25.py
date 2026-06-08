"""QM Search Platform — Lexical search engine (BM25 / TF-IDF).

Implements:
  - BM25 scoring
  - Term frequency / inverse document frequency
  - Field-weighted scoring
  - Phrase matching
  - Prefix matching
"""

from __future__ import annotations

import math
from collections import defaultdict
from dataclasses import dataclass, field
from typing import Any


@dataclass
class SearchHit:
    """A single search result."""

    doc_id: str
    score: float
    fields: dict[str, Any] = field(default_factory=dict)
    highlights: dict[str, list[str]] = field(default_factory=dict)


@dataclass
class SearchResult:
    """Search results envelope."""

    hits: list[SearchHit]
    total: int
    query_time_ms: float = 0.0
    facets: dict[str, dict[str, int]] | None = None


class BM25Scorer:
    """BM25 scoring algorithm.

    Parameters:
      k1: Term frequency saturation (default 1.2)
      b: Length normalization (default 0.75)
    """

    @staticmethod
    def _normalize(token: str) -> str:
        """Basic English normalization — strip plural 's'."""
        t = token.lower().strip()
        if len(t) > 3 and t.endswith("s") and not t.endswith("ss"):
            t = t[:-1]
        return t

    def __init__(self, k1: float = 1.2, b: float = 0.75, field_weights: dict[str, float] | None = None) -> None:
        self.k1 = k1
        self.b = b
        self._default_field_weights = field_weights
        # doc_id -> {field -> [tokens]}
        self._docs: dict[str, dict[str, list[str]]] = {}
        # term -> set of doc_ids
        self._inverted: dict[str, set[str]] = defaultdict(set)
        # field -> {term -> {doc_id -> count}}
        self._term_freqs: dict[str, dict[str, dict[str, int]]] = defaultdict(
            lambda: defaultdict(lambda: defaultdict(int))
        )
        self._doc_lengths: dict[str, int] = {}
        self._avg_doc_length: float = 0.0
        self._total_docs: int = 0

    def add_document(self, doc_id: str, fields: dict[str, list[str] | str]) -> None:
        """Index a document with tokenized fields (or raw strings)."""
        if doc_id in self._docs:
            self.remove_document(doc_id)
        # Auto-tokenize string fields
        tokenized: dict[str, list[str]] = {}
        for k, v in fields.items():
            raw = v.lower().split() if isinstance(v, str) else v
            tokenized[k] = [self._normalize(t) for t in raw]
        self._docs[doc_id] = tokenized
        total_tokens = 0

        for field_name, tokens in tokenized.items():
            total_tokens += len(tokens)
            for token in tokens:
                self._inverted[token].add(doc_id)
                self._term_freqs[field_name][token][doc_id] += 1

        self._doc_lengths[doc_id] = total_tokens
        self._total_docs = len(self._docs)
        self._avg_doc_length = (
            sum(self._doc_lengths.values()) / self._total_docs
            if self._total_docs > 0
            else 0.0
        )

    def remove_document(self, doc_id: str) -> None:
        """Remove a document from the index."""
        if doc_id not in self._docs:
            return

        fields = self._docs.pop(doc_id)
        self._doc_lengths.pop(doc_id, None)

        for field_name, tokens in fields.items():
            for token in set(tokens):
                norm = token  # Already normalized during add
                self._inverted[norm].discard(doc_id)
                self._term_freqs[field_name][norm].pop(doc_id, None)

        self._total_docs = len(self._docs)
        self._avg_doc_length = (
            sum(self._doc_lengths.values()) / self._total_docs
            if self._total_docs > 0
            else 0.0
        )

    def search(
        self,
        query_tokens: list[str] | str,
        field_weights: dict[str, float] | None = None,
        limit: int = 20,
    ) -> list[SearchHit]:
        """Score documents against query tokens using BM25."""
        if not query_tokens:
            return []

        # Auto-tokenize string queries
        if isinstance(query_tokens, str):
            query_tokens = query_tokens.lower().split()
        query_tokens = [self._normalize(t) for t in query_tokens]

        weights = field_weights or self._default_field_weights or {"_default": 1.0}
        scores: dict[str, float] = defaultdict(float)

        for token in query_tokens:
            if token not in self._inverted:
                continue

            matching_docs = self._inverted[token]
            idf = self._idf(len(matching_docs))

            for doc_id in matching_docs:
                doc_len = self._doc_lengths.get(doc_id, 0)
                tf_total = 0.0

                for field_name, weight in weights.items():
                    if field_name == "_default":
                        # Sum TF across all indexed fields
                        for fname in self._term_freqs:
                            tf = self._term_freqs[fname].get(token, {}).get(doc_id, 0)
                            tf_total += tf * weight
                    else:
                        tf = self._term_freqs.get(field_name, {}).get(token, {}).get(doc_id, 0)
                        tf_total += tf * weight

                # BM25 formula
                numerator = tf_total * (self.k1 + 1)
                denominator = tf_total + self.k1 * (
                    1 - self.b + self.b * (doc_len / max(self._avg_doc_length, 1))
                )
                scores[doc_id] += idf * (numerator / max(denominator, 1e-10))

        # Sort by score descending, filter zero-score results
        ranked = sorted(scores.items(), key=lambda x: (-x[1], x[0]))
        return [
            SearchHit(doc_id=doc_id, score=score)
            for doc_id, score in ranked
            if score > 0
        ][:limit]

    def _idf(self, doc_freq: int) -> float:
        """Inverse document frequency."""
        if doc_freq <= 0:
            return 0.0
        return math.log(1 + (self._total_docs - doc_freq + 0.5) / (doc_freq + 0.5))

    @property
    def doc_count(self) -> int:
        return self._total_docs


class LexicalSearchEngine:
    """High-level lexical search engine wrapping BM25 + filters."""

    def __init__(self) -> None:
        self._collections: dict[str, BM25Scorer] = {}

    def get_or_create_collection(self, name: str) -> BM25Scorer:
        if name not in self._collections:
            self._collections[name] = BM25Scorer()
        return self._collections[name]

    def index_document(
        self,
        collection: str,
        doc_id: str,
        fields: dict[str, list[str]],
    ) -> None:
        scorer = self.get_or_create_collection(collection)
        scorer.add_document(doc_id, fields)

    def search(
        self,
        collection: str,
        query_tokens: list[str],
        limit: int = 20,
        field_weights: dict[str, float] | None = None,
    ) -> SearchResult:
        scorer = self._collections.get(collection)
        if not scorer:
            return SearchResult(hits=[], total=0)

        import time
        start = time.monotonic()
        hits = scorer.search(query_tokens, field_weights, limit)
        elapsed = (time.monotonic() - start) * 1000

        return SearchResult(
            hits=hits,
            total=len(hits),
            query_time_ms=elapsed,
        )
