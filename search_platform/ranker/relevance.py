"""QM Search Platform — Relevance ranker and reranking."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


@dataclass
class RankCandidate:
    """A candidate document for ranking."""

    doc_id: str
    score: float
    source: str  # "bm25", "vector", "hybrid"
    fields: dict[str, Any] | None = None


class RelevanceRanker:
    """Combines and reranks results from multiple sources."""

    def reciprocal_rank_fusion(
        self,
        result_lists: list[list[RankCandidate]],
        k: int = 60,
    ) -> list[RankCandidate]:
        """Reciprocal Rank Fusion (RRF) — merges multiple ranked lists."""
        scores: dict[str, float] = {}
        candidate_map: dict[str, RankCandidate] = {}

        for result_list in result_lists:
            for rank, candidate in enumerate(result_list):
                rrf_score = 1.0 / (k + rank + 1)
                scores[candidate.doc_id] = scores.get(candidate.doc_id, 0) + rrf_score
                if candidate.doc_id not in candidate_map:
                    candidate_map[candidate.doc_id] = candidate

        # Sort by fused score
        ranked = sorted(scores.items(), key=lambda x: x[1], reverse=True)
        return [
            RankCandidate(
                doc_id=doc_id,
                score=score,
                source="rrf",
                fields=candidate_map[doc_id].fields,
            )
            for doc_id, score in ranked
        ]

    def linear_combination(
        self,
        lexical_results: list[RankCandidate],
        vector_results: list[RankCandidate],
        alpha: float = 0.5,
    ) -> list[RankCandidate]:
        """Linear combination: alpha * lexical + (1-alpha) * vector."""
        scores: dict[str, float] = {}
        candidate_map: dict[str, RankCandidate] = {}

        # Normalize lexical scores
        max_lex = max((r.score for r in lexical_results), default=1.0) or 1.0
        for r in lexical_results:
            norm_score = r.score / max_lex
            scores[r.doc_id] = alpha * norm_score
            candidate_map[r.doc_id] = r

        # Normalize vector scores
        max_vec = max((r.score for r in vector_results), default=1.0) or 1.0
        for r in vector_results:
            norm_score = r.score / max_vec
            scores[r.doc_id] = scores.get(r.doc_id, 0) + (1 - alpha) * norm_score
            if r.doc_id not in candidate_map:
                candidate_map[r.doc_id] = r

        ranked = sorted(scores.items(), key=lambda x: x[1], reverse=True)
        return [
            RankCandidate(
                doc_id=doc_id,
                score=score,
                source="hybrid",
                fields=candidate_map[doc_id].fields,
            )
            for doc_id, score in ranked
        ]
