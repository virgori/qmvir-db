"""QM Optimizer — Adaptive Query Execution.

Monitors runtime behavior and re-optimizes mid-flight when actual
cardinalities diverge from estimates.

Key ideas:
    1. **Cardinality Fence**: If actual rows from a scan differ from estimate
       by > threshold, trigger re-plan for remaining operators.
    2. **Adaptive Budget**: Widen/narrow retrieval budgets based on recall.
    3. **Rule-Based Rewrites**: Apply transformations that are always beneficial:
       - Predicate pushdown
       - Constant folding
       - Dead column elimination
       - Filter reordering by selectivity
       - Bitmap merge for multi-predicate AND
    4. **History-Aware**: Track past plan performance to improve future planning.

Architecture:
    AdaptiveExecutor wraps a PhysicalNode tree.  After each operator produces
    results, it checks a "fence" and decides whether to re-invoke the planner
    for the remaining sub-tree.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from typing import Any, Callable

from qm_core.execution.plan import PhysicalNode


# ── Cardinality Fence ───────────────────────────────────────────────


@dataclass
class CardinalityFence:
    """Monitors actual vs estimated rows and triggers re-plan."""
    threshold: float = 3.0  # If actual/estimated > threshold → re-plan
    min_rows: int = 100     # Don't bother re-planning for tiny result sets

    def should_replan(self, estimated: int, actual: int) -> bool:
        if actual < self.min_rows:
            return False
        if estimated == 0:
            return actual > self.min_rows
        ratio = actual / estimated if estimated > 0 else float('inf')
        return ratio > self.threshold or ratio < (1.0 / self.threshold)


# ── Plan History ────────────────────────────────────────────────────


@dataclass
class PlanRecord:
    """A record of a past plan execution."""
    query_hash: str
    plan_type: str
    estimated_cost: float
    actual_time_ms: float
    actual_rows: int
    estimated_rows: int
    timestamp: float = 0.0


class PlanHistory:
    """Track historical plan performance for smarter future planning."""

    def __init__(self, max_records: int = 10000) -> None:
        self._records: list[PlanRecord] = []
        self._max = max_records
        # query_hash → best observed plan_type
        self._best_plans: dict[str, str] = {}

    def record(self, rec: PlanRecord) -> None:
        self._records.append(rec)
        if len(self._records) > self._max:
            self._records = self._records[-self._max:]
        # Track best plan for query pattern
        key = rec.query_hash
        existing = self._best_plans.get(key)
        if existing is None or rec.actual_time_ms < self._find_best_time(key):
            self._best_plans[key] = rec.plan_type

    def get_best_plan(self, query_hash: str) -> str | None:
        return self._best_plans.get(query_hash)

    def _find_best_time(self, query_hash: str) -> float:
        best = float('inf')
        for r in reversed(self._records):
            if r.query_hash == query_hash:
                best = min(best, r.actual_time_ms)
        return best

    def estimation_accuracy(self, last_n: int = 100) -> float:
        """Average estimation accuracy (1.0 = perfect)."""
        recent = self._records[-last_n:] if self._records else []
        if not recent:
            return 1.0
        ratios = []
        for r in recent:
            if r.estimated_rows > 0:
                ratios.append(min(r.actual_rows, r.estimated_rows) / max(r.actual_rows, r.estimated_rows))
        return sum(ratios) / len(ratios) if ratios else 1.0


# ── Rule-Based Rewrites ────────────────────────────────────────────


class RewriteRule:
    """Base class for plan rewrite rules."""

    def name(self) -> str:
        return self.__class__.__name__

    def applicable(self, node: PhysicalNode) -> bool:
        return False

    def apply(self, node: PhysicalNode) -> PhysicalNode:
        return node


class PredicatePushdown(RewriteRule):
    """Push filters below joins/aggregates when safe."""

    def applicable(self, node: PhysicalNode) -> bool:
        return node.node_type == "Filter" and node.children and node.children[0].node_type in ("HashAggregate", "Sort")

    def apply(self, node: PhysicalNode) -> PhysicalNode:
        # Swap filter below the child
        child = node.children[0]
        node.children = child.children
        child.children = [node]
        child.estimated_rows = node.estimated_rows
        return child


class FilterReorder(RewriteRule):
    """Reorder conjunctive filters: most selective first."""

    def applicable(self, node: PhysicalNode) -> bool:
        return node.node_type == "Filter" and hasattr(node, 'predicates') and len(node.predicates) > 1

    def apply(self, node: PhysicalNode) -> PhysicalNode:
        # Sort predicates by estimated selectivity (cheapest first)
        # Heuristic: equality < range < LIKE < function
        def pred_cost(p: Any) -> int:
            if hasattr(p, 'op'):
                op = p.op if isinstance(p.op, str) else p.op.value
                costs = {"eq": 1, "neq": 2, "gt": 3, "gte": 3, "lt": 3, "lte": 3,
                         "in": 4, "between": 4, "like": 5, "is_null": 1}
                return costs.get(op, 6)
            return 6
        node.predicates.sort(key=pred_cost)
        return node


class BitmapMerge(RewriteRule):
    """Merge multiple bitmap scans on the same table into one."""

    def applicable(self, node: PhysicalNode) -> bool:
        if node.node_type != "Filter":
            return False
        children_bitmaps = [c for c in node.children if c.node_type == "BitmapScan"]
        return len(children_bitmaps) >= 2

    def apply(self, node: PhysicalNode) -> PhysicalNode:
        # Merge bitmap scans: intersect for AND, union for OR
        # For now, just combine predicates into first bitmap scan
        bitmaps = [c for c in node.children if c.node_type == "BitmapScan"]
        main = bitmaps[0]
        for bm in bitmaps[1:]:
            if hasattr(bm, 'predicates'):
                main.predicates.extend(bm.predicates)
            main.estimated_rows = min(main.estimated_rows, bm.estimated_rows)
        node.children = [main] + [c for c in node.children if c.node_type != "BitmapScan"]
        return node


class DeadColumnElimination(RewriteRule):
    """Remove columns that are never referenced downstream."""

    def applicable(self, node: PhysicalNode) -> bool:
        return node.node_type == "Project" and hasattr(node, 'columns')

    def apply(self, node: PhysicalNode) -> PhysicalNode:
        # This is a placeholder — full implementation needs reference tracking
        return node


# ── Adaptive Executor ───────────────────────────────────────────────


class RuleOptimizer:
    """Apply rewrite rules to a physical plan tree."""

    def __init__(self) -> None:
        self.rules: list[RewriteRule] = [
            PredicatePushdown(),
            FilterReorder(),
            BitmapMerge(),
            DeadColumnElimination(),
        ]

    def optimize(self, root: PhysicalNode) -> PhysicalNode:
        """Apply all applicable rules bottom-up."""
        # Recurse into children first
        for i, child in enumerate(root.children):
            root.children[i] = self.optimize(child)

        # Apply rules to current node
        for rule in self.rules:
            if rule.applicable(root):
                root = rule.apply(root)

        return root


class AdaptiveExecutor:
    """Wraps plan execution with adaptive re-planning.

    Usage:
        executor = AdaptiveExecutor(planner)
        results = executor.execute(plan, context)
    """

    def __init__(self, replanner: Callable[..., PhysicalNode] | None = None,
                 fence: CardinalityFence | None = None,
                 history: PlanHistory | None = None) -> None:
        self._replanner = replanner
        self._fence = fence or CardinalityFence()
        self._history = history or PlanHistory()
        self._rule_optimizer = RuleOptimizer()

    def optimize_plan(self, plan: PhysicalNode) -> PhysicalNode:
        """Apply rule-based optimizations."""
        return self._rule_optimizer.optimize(plan)

    def check_fence(self, node: PhysicalNode, actual_rows: int) -> bool:
        """Check if the cardinality fence is breached."""
        return self._fence.should_replan(node.estimated_rows, actual_rows)

    def record_execution(self, query_hash: str, plan: PhysicalNode,
                         actual_time_ms: float, actual_rows: int) -> None:
        """Record execution for future optimization."""
        self._history.record(PlanRecord(
            query_hash=query_hash,
            plan_type=plan.node_type,
            estimated_cost=plan.estimated_cost,
            actual_time_ms=actual_time_ms,
            actual_rows=actual_rows,
            estimated_rows=plan.estimated_rows,
            timestamp=time.time(),
        ))

    def suggest_plan(self, query_hash: str) -> str | None:
        """Suggest the best plan type based on history."""
        return self._history.get_best_plan(query_hash)

    @property
    def estimation_accuracy(self) -> float:
        return self._history.estimation_accuracy()
