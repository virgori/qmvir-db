"""QM Search Platform — Hybrid fusion (lexical + vector).

Combines lexical (BM25) and vector (ANN) results using:
  - Reciprocal Rank Fusion (RRF)
  - Linear combination
  - Learned fusion weights
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from search_platform.ranker.relevance import RankCandidate, RelevanceRanker


@dataclass
class HybridSearchConfig:
    """Configuration for hybrid search fusion."""

    alpha: float = 0.5  # Weight for lexical vs vector
    fusion_method: str = "rrf"  # "rrf" or "linear"
    rrf_k: int = 60
    lexical_top_k: int = 100
    vector_top_k: int = 100
    final_top_k: int = 20


class HybridFusion:
    """Fuses lexical and vector search results."""

    def __init__(self, config: HybridSearchConfig | None = None) -> None:
        self.config = config or HybridSearchConfig()
        self._ranker = RelevanceRanker()

    def fuse(
        self,
        lexical_hits: list[RankCandidate],
        vector_hits: list[RankCandidate],
    ) -> list[RankCandidate]:
        """Fuse lexical and vector results according to config."""
        if self.config.fusion_method == "rrf":
            fused = self._ranker.reciprocal_rank_fusion(
                [lexical_hits, vector_hits],
                k=self.config.rrf_k,
            )
        elif self.config.fusion_method == "linear":
            fused = self._ranker.linear_combination(
                lexical_hits,
                vector_hits,
                alpha=self.config.alpha,
            )
        else:
            raise ValueError(f"Unknown fusion method: {self.config.fusion_method}")

        return fused[: self.config.final_top_k]
