"""QM Learned — Intelligent Assistants.

Lightweight ML/heuristic components that learn from query workload:

    1. **LearnedSelectivity**: Better cardinality estimates using query feedback.
       Trains a simple model (gradient-boosted histogram) on (predicate_features → actual_rows).
       Falls back to traditional estimation when confidence is low.

    2. **LearnedCachePolicy**: Predict which pages/segments will be accessed next.
       Uses exponential moving average of access patterns per segment.
       Decides pre-fetch and eviction priorities.

    3. **LearnedFusionWeights**: Tune hybrid search alpha dynamically.
       Tracks which arm (lexical vs vector) contributes more relevant results
       and adjusts weights per query "intent class".

    4. **QueryIntentClassifier**: Classify incoming queries as keyword/semantic/navigational/analytical.
       Uses simple feature extraction (query length, presence of operators, field names).

All models are Python-only with no external ML dependencies (pure numpy where possible).
They degrade gracefully to defaults when untrained.
"""

from __future__ import annotations

import math
import time
from collections import defaultdict
from dataclasses import dataclass, field
from typing import Any


# ── Learned Selectivity ─────────────────────────────────────────────


@dataclass
class SelectivitySample:
    """Training sample for selectivity estimation."""
    table: str
    column: str
    operator: str       # eq, gt, lt, between, in, like
    value_hash: int     # Hashed predicate value
    estimated: float    # What the planner predicted
    actual: float       # What actually happened


class LearnedSelectivity:
    """Learn better selectivity estimates from query feedback.

    Approach: maintain per-(table, column, operator) correction factors.
    When the planner estimates selectivity S but actual is A,
    update the correction factor toward A/S using exponential moving average.

    This is simple but effective — many real systems use similar approaches
    (e.g., PostgreSQL's extended statistics, Oracle's SQL Plan Directives).
    """

    def __init__(self, learning_rate: float = 0.1, min_samples: int = 5) -> None:
        self._lr = learning_rate
        self._min_samples = min_samples
        # (table, column, operator) → correction_factor
        self._corrections: dict[tuple[str, str, str], float] = {}
        self._sample_counts: dict[tuple[str, str, str], int] = defaultdict(int)
        self._confidence: dict[tuple[str, str, str], float] = defaultdict(float)

    def correct(self, table: str, column: str, operator: str, base_selectivity: float) -> float:
        """Apply learned correction to a base selectivity estimate."""
        key = (table, column, operator)
        if key not in self._corrections or self._sample_counts[key] < self._min_samples:
            return base_selectivity  # Not enough data, fall back
        factor = self._corrections[key]
        corrected = base_selectivity * factor
        return max(1e-6, min(1.0, corrected))

    def feedback(self, sample: SelectivitySample) -> None:
        """Provide feedback from actual query execution."""
        key = (sample.table, sample.column, sample.operator)
        self._sample_counts[key] += 1

        if sample.estimated <= 0:
            return

        actual_factor = sample.actual / sample.estimated
        current = self._corrections.get(key, 1.0)
        # EMA update
        updated = current * (1.0 - self._lr) + actual_factor * self._lr
        self._corrections[key] = updated

        # Update confidence (converges toward 1.0 with more samples)
        n = self._sample_counts[key]
        self._confidence[key] = 1.0 - 1.0 / (1.0 + n / 10.0)

    def get_confidence(self, table: str, column: str, operator: str) -> float:
        """How confident are we in the correction? 0 = not at all, 1 = very."""
        return self._confidence.get((table, column, operator), 0.0)

    def stats(self) -> dict[str, Any]:
        return {
            "tracked_patterns": len(self._corrections),
            "total_samples": sum(self._sample_counts.values()),
            "avg_confidence": (sum(self._confidence.values()) / len(self._confidence)
                               if self._confidence else 0.0),
        }


# ── Learned Cache Policy ────────────────────────────────────────────


@dataclass
class AccessRecord:
    """Record of a page/segment access."""
    segment_id: str
    page_no: int
    timestamp: float


class LearnedCachePolicy:
    """Predict which pages to pre-fetch and which to evict.

    Uses exponential moving average of access intervals per segment.
    Segments with shorter intervals → higher priority (keep in cache).
    Segments with long intervals → eviction candidates.
    Segments trending upward → pre-fetch candidates.
    """

    def __init__(self, decay: float = 0.9, prefetch_threshold: float = 0.7) -> None:
        self._decay = decay
        self._prefetch_threshold = prefetch_threshold
        # segment_id → EMA of inter-access time
        self._ema_interval: dict[str, float] = {}
        # segment_id → last access time
        self._last_access: dict[str, float] = {}
        # segment_id → access count
        self._access_count: dict[str, int] = defaultdict(int)
        # segment_id → trend (negative = accelerating, positive = decelerating)
        self._trend: dict[str, float] = {}

    def record_access(self, segment_id: str, timestamp: float | None = None) -> None:
        """Record an access event."""
        ts = timestamp or time.time()
        self._access_count[segment_id] += 1

        if segment_id in self._last_access:
            interval = ts - self._last_access[segment_id]
            old_ema = self._ema_interval.get(segment_id, interval)
            new_ema = self._decay * old_ema + (1.0 - self._decay) * interval
            self._trend[segment_id] = new_ema - old_ema  # Positive = slowing down
            self._ema_interval[segment_id] = new_ema
        else:
            self._ema_interval[segment_id] = 1.0  # Default

        self._last_access[segment_id] = ts

    def eviction_priority(self, segment_id: str) -> float:
        """Higher = more likely to be evicted. Range: [0, ∞)."""
        if segment_id not in self._ema_interval:
            return float('inf')  # Unknown → evict first
        ema = self._ema_interval[segment_id]
        trend = self._trend.get(segment_id, 0.0)
        # Long interval + decelerating trend → high eviction priority
        return ema + max(0, trend) * 2

    def should_prefetch(self, segment_id: str, current_time: float | None = None) -> bool:
        """Should we pre-fetch this segment?"""
        if segment_id not in self._ema_interval:
            return False
        ts = current_time or time.time()
        last = self._last_access.get(segment_id, 0)
        ema = self._ema_interval[segment_id]
        time_since = ts - last
        # If we're close to the expected next access and trend is accelerating
        ratio = time_since / (ema + 1e-10)
        return ratio >= self._prefetch_threshold and self._trend.get(segment_id, 0) <= 0

    def prefetch_candidates(self, current_time: float | None = None, limit: int = 10) -> list[str]:
        """Return segments that should be pre-fetched."""
        candidates = [
            sid for sid in self._ema_interval
            if self.should_prefetch(sid, current_time)
        ]
        # Sort by urgency (closest to next expected access)
        ts = current_time or time.time()
        candidates.sort(key=lambda sid: self.eviction_priority(sid))
        return candidates[:limit]


# ── Learned Fusion Weights ──────────────────────────────────────────


@dataclass
class FusionFeedback:
    """Feedback signal for fusion weight learning."""
    query_intent: str      # keyword, semantic, navigational, analytical
    lexical_ndcg: float    # NDCG of lexical-only results
    vector_ndcg: float     # NDCG of vector-only results
    alpha_used: float      # Alpha used for this query


class LearnedFusionWeights:
    """Learn optimal alpha for hybrid search per query intent.

    alpha = 0 → pure vector, alpha = 1 → pure lexical.

    Maintains per-intent alpha that converges toward the optimal blend.
    """

    def __init__(self, default_alpha: float = 0.5, learning_rate: float = 0.05) -> None:
        self._default_alpha = default_alpha
        self._lr = learning_rate
        # intent → learned alpha
        self._alphas: dict[str, float] = {}
        self._sample_counts: dict[str, int] = defaultdict(int)

    def get_alpha(self, intent: str) -> float:
        """Get the fusion alpha for a query intent."""
        return self._alphas.get(intent, self._default_alpha)

    def feedback(self, fb: FusionFeedback) -> None:
        """Update alpha based on feedback."""
        self._sample_counts[fb.query_intent] += 1
        current = self._alphas.get(fb.query_intent, self._default_alpha)

        # If lexical did better, increase alpha; if vector did better, decrease
        total = fb.lexical_ndcg + fb.vector_ndcg + 1e-10
        target = fb.lexical_ndcg / total
        updated = current * (1.0 - self._lr) + target * self._lr
        self._alphas[fb.query_intent] = max(0.0, min(1.0, updated))

    def all_weights(self) -> dict[str, float]:
        return dict(self._alphas)


# ── Query Intent Classifier ────────────────────────────────────────


class QueryIntentClassifier:
    """Classify query intent using simple features.

    Intent classes:
        - keyword: short, specific terms, looking for exact match
        - semantic: natural language question, conceptual search
        - navigational: looking for a specific known document
        - analytical: aggregation, counting, grouping queries
        - hybrid: mixed intent (default for ambiguous queries)
    """

    # Keywords that suggest analytical intent
    ANALYTICAL_WORDS = {"count", "sum", "average", "avg", "total", "group", "aggregate",
                        "how many", "statistics", "trend", "per", "breakdown"}
    # Keywords that suggest navigational intent
    NAV_PATTERNS = {"site:", "url:", "id:", "exact:", "title:"}

    def classify(self, query: str, has_aggregates: bool = False,
                 has_vector: bool = False) -> str:
        """Classify query intent."""
        q = query.lower().strip()
        tokens = q.split()

        # Analytical: has aggregates or analytical keywords
        if has_aggregates:
            return "analytical"
        if any(w in q for w in self.ANALYTICAL_WORDS):
            return "analytical"

        # Navigational: specific ID or URL lookup
        if any(q.startswith(p) for p in self.NAV_PATTERNS):
            return "navigational"
        if len(tokens) == 1 and (tokens[0].isdigit() or '-' in tokens[0]):
            return "navigational"

        # Semantic: long natural language, question words
        question_words = {"what", "how", "why", "when", "where", "which", "who", "explain", "describe"}
        if len(tokens) >= 5 or any(t in question_words for t in tokens[:2]):
            return "semantic"

        # Vector provided explicitly
        if has_vector:
            return "semantic"

        # Keyword: short, specific terms
        if len(tokens) <= 3:
            return "keyword"

        return "hybrid"

    def features(self, query: str) -> dict[str, Any]:
        """Extract features for logging/debugging."""
        tokens = query.lower().split()
        return {
            "n_tokens": len(tokens),
            "has_operator": any(":" in t for t in tokens),
            "has_question_word": any(t in {"what", "how", "why", "when", "where"} for t in tokens[:2]),
            "avg_token_length": sum(len(t) for t in tokens) / (len(tokens) or 1),
            "has_quotes": '"' in query or "'" in query,
        }
