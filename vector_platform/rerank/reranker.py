"""QM Vector Platform — Reranking (cross-encoder or custom scorer).

Multi-stage retrieval:
  1. ANN top-200
  2. Metadata filtering
  3. Hybrid fusion with BM25
  4. Rerank top-50
  5. Return top-10
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Callable


@dataclass
class RerankCandidate:
    """A candidate for reranking."""

    doc_id: str
    initial_score: float
    rerank_score: float | None = None
    text: str = ""
    metadata: dict[str, Any] | None = None


class Reranker:
    """Reranks candidates using a scoring function."""

    def __init__(self, scorer: Callable[[str, str], float] | None = None) -> None:
        """Initialize with an optional scoring function (query, doc_text) -> score."""
        self._scorer = scorer

    def rerank(
        self,
        query: str,
        candidates: list[RerankCandidate],
        top_k: int = 10,
    ) -> list[RerankCandidate]:
        """Rerank candidates against query."""
        if self._scorer is None:
            # No reranker — return in original order
            return candidates[:top_k]

        for candidate in candidates:
            candidate.rerank_score = self._scorer(query, candidate.text)

        ranked = sorted(
            candidates,
            key=lambda c: c.rerank_score if c.rerank_score is not None else 0,
            reverse=True,
        )
        return ranked[:top_k]

    @staticmethod
    def simple_overlap_scorer(query: str, text: str) -> float:
        """Simple word-overlap scorer as baseline."""
        q_tokens = set(query.lower().split())
        t_tokens = set(text.lower().split())
        if not q_tokens:
            return 0.0
        overlap = q_tokens & t_tokens
        return len(overlap) / len(q_tokens)
