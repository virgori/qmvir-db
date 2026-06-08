"""QM Execution — Multi-Stage Retrieval Pipeline.

Implements the core principle: **prune early, score late, fetch data last**.

Pipeline stages:
    1. Candidate Generation (cheap) — inverted index / bitmap / HNSW coarse
    2. Lightweight Scoring (moderate) — BM25 / PQ approximate distance
    3. Re-Ranking (expensive) — exact scoring / cross-encoder
    4. Late Materialization — fetch only top-k full documents
    5. Post-Processing — project, highlight, facet

Budget-aware execution:
    - Each stage has a configurable budget (max candidates, max time)
    - If Stage N produces fewer candidates than expected, skip Stage N+1
    - Adaptive budgets: widen/narrow based on recall estimates
"""

from __future__ import annotations

import time

try:
    import qm_engine as _qm_engine
except Exception:
    _qm_engine = None
from dataclasses import dataclass, field
from typing import Any, Callable, Protocol

from qm_core.execution.vectorized import ColumnBatch, VecSort, HAS_NUMPY

if HAS_NUMPY:
    import numpy as np


# ── Scored Candidate ────────────────────────────────────────────────


@dataclass(slots=True)
class ScoredCandidate:
    """A document that has survived a pipeline stage."""
    doc_id: int
    score: float
    metadata: dict[str, Any] = field(default_factory=dict)

    def __lt__(self, other: "ScoredCandidate") -> bool:
        return self.score < other.score


# ── Stage Protocol ──────────────────────────────────────────────────


class PipelineStage(Protocol):
    """A single stage in the retrieval pipeline."""

    @property
    def name(self) -> str:
        ...

    def execute(self, candidates: list[ScoredCandidate],
                context: PipelineContext) -> list[ScoredCandidate]:
        ...


# ── Pipeline Context ────────────────────────────────────────────────


@dataclass
class StageBudget:
    """Budget constraints for a pipeline stage."""
    max_candidates: int = 100_000
    max_time_ms: float = 50.0
    min_candidates: int = 10  # Early stop if below this after a stage


@dataclass
class PipelineContext:
    """Runtime context for the pipeline."""
    query: str = ""
    query_vector: list[float] | None = None
    table: str = ""
    top_k: int = 10

    # Per-stage budgets (stage_name → budget)
    budgets: dict[str, StageBudget] = field(default_factory=dict)

    # Telemetry
    stage_timings: dict[str, float] = field(default_factory=dict)
    stage_counts: dict[str, int] = field(default_factory=dict)

    # Adaptive: widen factor for under-performing stages
    widen_factor: float = 1.5
    adaptive: bool = True

    def get_budget(self, stage_name: str) -> StageBudget:
        return self.budgets.get(stage_name, StageBudget())


# ── Concrete Stages ─────────────────────────────────────────────────


class CandidateGenStage:
    """Stage 1: Generate initial candidate set.

    Uses inverted index / bitmap scan / HNSW to produce a broad candidate set.
    This is the cheapest stage — casts a wide net.
    """

    def __init__(self, generator: Callable[[PipelineContext], list[ScoredCandidate]]) -> None:
        self._generator = generator

    @property
    def name(self) -> str:
        return "candidate_gen"

    def execute(self, candidates: list[ScoredCandidate],
                context: PipelineContext) -> list[ScoredCandidate]:
        budget = context.get_budget(self.name)
        raw = self._generator(context)
        # Truncate to budget
        if len(raw) > budget.max_candidates:
            raw.sort(reverse=True)
            raw = raw[:budget.max_candidates]
        return raw


class LightweightScoringStage:
    """Stage 2: Apply cheap scoring (BM25 / PQ approximate distance).

    Re-scores candidates and prunes low-scoring ones.
    """

    def __init__(self, scorer: Callable[[list[ScoredCandidate], PipelineContext], list[ScoredCandidate]]) -> None:
        self._scorer = scorer

    @property
    def name(self) -> str:
        return "lightweight_score"

    def execute(self, candidates: list[ScoredCandidate],
                context: PipelineContext) -> list[ScoredCandidate]:
        if not candidates:
            return []
        budget = context.get_budget(self.name)
        scored = self._scorer(candidates, context)
        scored.sort(reverse=True)
        # Keep top budget.max_candidates
        limit = min(budget.max_candidates, len(scored))
        return scored[:limit]


class ReRankStage:
    """Stage 3: Expensive re-ranking (exact vector distance / cross-encoder).

    Operates on a much smaller set (typically 2-4x top_k).
    """

    def __init__(self, reranker: Callable[[list[ScoredCandidate], PipelineContext], list[ScoredCandidate]]) -> None:
        self._reranker = reranker

    @property
    def name(self) -> str:
        return "rerank"

    def execute(self, candidates: list[ScoredCandidate],
                context: PipelineContext) -> list[ScoredCandidate]:
        if not candidates:
            return []
        reranked = self._reranker(candidates, context)
        reranked.sort(reverse=True)
        return reranked[:context.top_k]


class LateMaterializeStage:
    """Stage 4: Fetch full documents only for the final top-k.

    This is the key optimization: we have been working with just doc_ids + scores
    until this point. Only NOW do we read the actual data.
    """

    def __init__(self, fetcher: Callable[[list[int], str], list[dict[str, Any]]]) -> None:
        """
        Args:
            fetcher: (doc_ids, table_name) → list of full documents
        """
        self._fetcher = fetcher

    @property
    def name(self) -> str:
        return "late_materialize"

    def execute(self, candidates: list[ScoredCandidate],
                context: PipelineContext) -> list[ScoredCandidate]:
        if not candidates:
            return []
        doc_ids = [c.doc_id for c in candidates]
        docs = self._fetcher(doc_ids, context.table)
        id_to_doc = {d.get("_id", d.get("id", i)): d for i, d in enumerate(docs)}

        for c in candidates:
            doc = id_to_doc.get(c.doc_id, {})
            c.metadata.update(doc)
            c.metadata["_score"] = c.score
        return candidates


class PostProcessStage:
    """Stage 5: Post-processing — projection, highlighting, faceting."""

    def __init__(self, projections: list[str] | None = None,
                 highlight_fields: list[str] | None = None) -> None:
        self.projections = projections
        self.highlight_fields = highlight_fields

    @property
    def name(self) -> str:
        return "post_process"

    def execute(self, candidates: list[ScoredCandidate],
                context: PipelineContext) -> list[ScoredCandidate]:
        for c in candidates:
            # Projection
            if self.projections:
                c.metadata = {k: v for k, v in c.metadata.items() if k in self.projections or k.startswith("_")}

            # Simple snippet highlighting
            if self.highlight_fields and context.query:
                terms = context.query.lower().split()
                for fld in self.highlight_fields:
                    text = c.metadata.get(fld, "")
                    if isinstance(text, str):
                        for t in terms:
                            text = text.replace(t, f"<b>{t}</b>")
                        c.metadata[f"_highlight_{fld}"] = text
        return candidates


# ── Fusion Stage (for Hybrid Search) ───────────────────────────────


class RRFFusionStage:
    """Reciprocal Rank Fusion: merge multiple ranked lists."""

    def __init__(self, k: int = 60) -> None:
        self.k = k

    @property
    def name(self) -> str:
        return "rrf_fusion"

    def execute_multi(self, ranked_lists: list[list[ScoredCandidate]],
                      context: PipelineContext) -> list[ScoredCandidate]:
        """Fuse multiple ranked lists using RRF."""
        scores: dict[int, float] = {}
        metadata_map: dict[int, dict] = {}

        for ranked in ranked_lists:
            for rank, c in enumerate(ranked):
                rrf_score = 1.0 / (self.k + rank + 1)
                scores[c.doc_id] = scores.get(c.doc_id, 0.0) + rrf_score
                if c.doc_id not in metadata_map:
                    metadata_map[c.doc_id] = c.metadata

        fused = [
            ScoredCandidate(doc_id=did, score=s, metadata=metadata_map.get(did, {}))
            for did, s in scores.items()
        ]
        fused.sort(reverse=True)
        return fused[:context.top_k]


class WeightedFusionStage:
    """Weighted linear combination of scores."""

    def __init__(self, weights: list[float] | None = None) -> None:
        self.weights = weights

    @property
    def name(self) -> str:
        return "weighted_fusion"

    def execute_multi(self, ranked_lists: list[list[ScoredCandidate]],
                      context: PipelineContext) -> list[ScoredCandidate]:
        weights = self.weights or [1.0 / len(ranked_lists)] * len(ranked_lists)

        # Normalize scores per list to [0, 1]
        normalized: list[dict[int, float]] = []
        for ranked in ranked_lists:
            if not ranked:
                normalized.append({})
                continue
            max_s = max(c.score for c in ranked) or 1.0
            min_s = min(c.score for c in ranked)
            span = max_s - min_s or 1.0
            normalized.append({c.doc_id: (c.score - min_s) / span for c in ranked})

        scores: dict[int, float] = {}
        metadata_map: dict[int, dict] = {}
        for i, norm_map in enumerate(normalized):
            w = weights[i] if i < len(weights) else 0.0
            for did, s in norm_map.items():
                scores[did] = scores.get(did, 0.0) + w * s
                if did not in metadata_map:
                    for c in ranked_lists[i]:
                        if c.doc_id == did:
                            metadata_map[did] = c.metadata
                            break

        fused = [
            ScoredCandidate(doc_id=did, score=s, metadata=metadata_map.get(did, {}))
            for did, s in scores.items()
        ]
        fused.sort(reverse=True)
        return fused[:context.top_k]


# ── The Pipeline ────────────────────────────────────────────────────


class RetrievalPipeline:
    """Multi-stage retrieval pipeline with budget-aware execution.

    Usage:
        pipeline = RetrievalPipeline()
        pipeline.add_stage(CandidateGenStage(my_gen_fn))
        pipeline.add_stage(LightweightScoringStage(my_scorer))
        pipeline.add_stage(ReRankStage(my_reranker))
        pipeline.add_stage(LateMaterializeStage(my_fetcher))
        pipeline.add_stage(PostProcessStage(projections=["title", "body"]))

        ctx = PipelineContext(query="database systems", top_k=10)
        results = pipeline.execute(ctx)
    """

    def __init__(self) -> None:
        self.stages: list[PipelineStage] = []

    def add_stage(self, stage: PipelineStage) -> "RetrievalPipeline":
        self.stages.append(stage)
        return self

    def execute(self, context: PipelineContext) -> list[ScoredCandidate]:
        """Execute all stages in sequence with budgets and telemetry."""
        candidates: list[ScoredCandidate] = []

        for stage in self.stages:
            budget = context.get_budget(stage.name)
            t0 = time.perf_counter()

            try:
                candidates = stage.execute(candidates, context)
            except Exception as e:
                # Stage failure: log and continue with what we have
                context.stage_timings[stage.name] = -1.0
                context.stage_counts[stage.name] = len(candidates)
                continue

            elapsed_ms = (time.perf_counter() - t0) * 1000.0
            context.stage_timings[stage.name] = elapsed_ms
            context.stage_counts[stage.name] = len(candidates)

            # FIX Bug 1.4: Enforce max_time_ms budget — abort pipeline
            # if a stage exceeds its time budget.
            if elapsed_ms > budget.max_time_ms:
                break

            # Early termination if too few candidates
            if len(candidates) < budget.min_candidates and stage.name != "post_process":
                break

            # Adaptive budget: if stage used <50% of budget, narrow next stage
            if context.adaptive and len(candidates) < budget.max_candidates * 0.5:
                # Widen the next stage to compensate
                for next_stage in self.stages:
                    if next_stage.name not in context.stage_timings:
                        next_budget = context.get_budget(next_stage.name)
                        next_budget.max_candidates = int(next_budget.max_candidates * context.widen_factor)
                        break

        return candidates

    def stats(self, context: PipelineContext) -> dict[str, Any]:
        """Return execution statistics."""
        total_ms = sum(t for t in context.stage_timings.values() if t > 0)
        return {
            "total_ms": total_ms,
            "stages": {
                name: {
                    "time_ms": context.stage_timings.get(name, 0.0),
                    "candidates": context.stage_counts.get(name, 0),
                }
                for name in [s.name for s in self.stages]
            },
        }


# ── Convenience builders ───────────────────────────────────────────


def build_search_pipeline(
    candidate_gen: Callable[[PipelineContext], list[ScoredCandidate]],
    scorer: Callable[[list[ScoredCandidate], PipelineContext], list[ScoredCandidate]] | None = None,
    reranker: Callable[[list[ScoredCandidate], PipelineContext], list[ScoredCandidate]] | None = None,
    fetcher: Callable[[list[int], str], list[dict[str, Any]]] | None = None,
    projections: list[str] | None = None,
    highlight_fields: list[str] | None = None,
) -> RetrievalPipeline:
    """Build a standard search pipeline."""
    pipeline = RetrievalPipeline()
    pipeline.add_stage(CandidateGenStage(candidate_gen))
    if scorer:
        pipeline.add_stage(LightweightScoringStage(scorer))
    if reranker:
        pipeline.add_stage(ReRankStage(reranker))
    if fetcher:
        pipeline.add_stage(LateMaterializeStage(fetcher))
    if projections or highlight_fields:
        pipeline.add_stage(PostProcessStage(projections, highlight_fields))
    return pipeline


def build_hybrid_pipeline(
    lexical_gen: Callable[[PipelineContext], list[ScoredCandidate]],
    vector_gen: Callable[[PipelineContext], list[ScoredCandidate]],
    fusion: str = "rrf",
    alpha: float = 0.5,
    fetcher: Callable[[list[int], str], list[dict[str, Any]]] | None = None,
) -> tuple[RetrievalPipeline, RetrievalPipeline, RRFFusionStage | WeightedFusionStage]:
    """Build a hybrid search pipeline with two arms and a fusion stage.

    Returns (lexical_pipeline, vector_pipeline, fusion_stage).
    The caller should:
        lex_results = lex_pipeline.execute(ctx)
        vec_results = vec_pipeline.execute(ctx)
        fused = fusion_stage.execute_multi([lex_results, vec_results], ctx)
    """
    lex_pipe = RetrievalPipeline()
    lex_pipe.add_stage(CandidateGenStage(lexical_gen))

    vec_pipe = RetrievalPipeline()
    vec_pipe.add_stage(CandidateGenStage(vector_gen))

    if fusion == "rrf":
        fusion_stage = RRFFusionStage()
    else:
        fusion_stage = WeightedFusionStage(weights=[alpha, 1.0 - alpha])

    return lex_pipe, vec_pipe, fusion_stage
