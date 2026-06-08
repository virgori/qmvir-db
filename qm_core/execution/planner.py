"""QM Execution — Cost-Based Query Planner.

Transforms logical plans into physical plans by:
    1. Enumerating access paths (seq scan, index scan, bitmap scan)
    2. Estimating cost using CPU / RAM / I/O model
    3. Choosing the cheapest plan
    4. Applying optimizations:
       - Predicate pushdown
       - Late materialization
       - Vectorized execution preference
       - Top-k aware sorting
       - Bitmap filter merging
       - Index intersection / union

Cost model parameters:
    - seq_page_cost: 1.0 (baseline for sequential I/O)
    - random_page_cost: 4.0 (random I/O is ~4x more expensive)
    - cpu_tuple_cost: 0.01 (per-row CPU processing)
    - cpu_index_cost: 0.005 (per-index-entry CPU)
    - cpu_operator_cost: 0.0025 (per-operator evaluation)
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from typing import Any

try:
    import qm_engine as _qm_engine
except Exception:
    _qm_engine = None

from qm_core.execution.parser import (
    QueryAST, Predicate, CompoundPredicate, CompareOp, AggregateExpr, SortKey,
)
from qm_core.execution.plan import (
    LogicalNode, LogicalScan, LogicalFilter, LogicalProject, LogicalSort,
    LogicalLimit, LogicalGroupBy, LogicalLexicalSearch, LogicalVectorSearch,
    LogicalHybridSearch,
    PhysicalNode, PhysicalSeqScan, PhysicalIndexScan, PhysicalBitmapScan,
    PhysicalFilter, PhysicalProject, PhysicalSort, PhysicalLimit,
    PhysicalHashAggregate, PhysicalBMWScan, PhysicalHNSWScan,
    PhysicalHybridFusion, PhysicalLateMaterialize,
)
from qm_core.index.stats import StatsCollector, TableStats, ColumnStats, IndexStats


@dataclass
class CostModel:
    """I/O + CPU cost model."""
    seq_page_cost: float = 1.0
    random_page_cost: float = 4.0
    cpu_tuple_cost: float = 0.01
    cpu_index_cost: float = 0.005
    cpu_operator_cost: float = 0.0025
    parallel_factor: float = 1.0  # >1 if parallel execution available
    page_size: int = 8192
    effective_cache_size: int = 256 * 1024 * 1024  # 256MB

    def seq_scan_cost(self, n_pages: int, n_rows: int) -> float:
        io = self.seq_page_cost * n_pages
        cpu = self.cpu_tuple_cost * n_rows
        return (io + cpu) / self.parallel_factor

    def index_scan_cost(self, n_pages: int, n_rows: int, index_height: int, selectivity: float) -> float:
        # Random I/O for index + sequential for matching rows
        index_io = self.random_page_cost * index_height
        data_pages = max(1, int(n_pages * selectivity))
        data_io = self.random_page_cost * data_pages
        cpu = self.cpu_index_cost * n_rows * selectivity + self.cpu_tuple_cost * n_rows * selectivity
        return index_io + data_io + cpu

    def bitmap_scan_cost(self, n_pages: int, n_rows: int, selectivity: float) -> float:
        # Bitmap is between seq and random I/O
        selected_pages = max(1, int(n_pages * selectivity))
        io = (self.seq_page_cost + self.random_page_cost) / 2.0 * selected_pages
        cpu = self.cpu_tuple_cost * n_rows * selectivity
        return io + cpu

    def sort_cost(self, n_rows: int) -> float:
        if n_rows <= 0:
            return 0.0
        return self.cpu_operator_cost * n_rows * math.log2(max(n_rows, 2))

    def hash_aggregate_cost(self, n_rows: int, n_groups: int) -> float:
        return self.cpu_tuple_cost * n_rows + self.cpu_operator_cost * n_groups

    def top_k_cost(self, n_rows: int, k: int) -> float:
        """Heap-select is O(n log k), much cheaper than full sort for small k."""
        if k <= 0 or n_rows <= 0:
            return 0.0
        return self.cpu_operator_cost * n_rows * math.log2(max(k, 2))

    def bmw_search_cost(self, n_docs: int, n_terms: int, top_k: int) -> float:
        """Block-Max WAND: sublinear in doc count."""
        # BMW typically processes ~10-30% of postings
        estimated_fraction = 0.2
        return self.cpu_operator_cost * n_docs * estimated_fraction * n_terms

    def hnsw_cost(self, n_vectors: int, ef_search: int) -> float:
        """HNSW: O(ef * log(n)) distance computations."""
        if n_vectors <= 0:
            return 0.0
        return self.cpu_operator_cost * ef_search * math.log2(max(n_vectors, 2))


class QueryPlanner:
    """Cost-based query planner.

    Transforms QueryAST → Physical Plan using statistics and cost model.

    Usage:
        planner = QueryPlanner(stats_collector)
        ast = parser.parse(request)
        plan = planner.plan(ast)
        print(plan.explain())
    """

    def __init__(self, stats: StatsCollector | None = None, cost_model: CostModel | None = None) -> None:
        self._stats = stats or StatsCollector()
        self._cost = cost_model or CostModel()

    def plan(self, ast: QueryAST) -> PhysicalNode:
        """Generate an optimized physical plan from a QueryAST."""
        if ast.action == "search":
            return self._plan_search(ast)
        elif ast.action == "vector_search":
            return self._plan_vector_search(ast)
        elif ast.action == "hybrid_search":
            return self._plan_hybrid_search(ast)
        elif ast.action == "aggregate":
            return self._plan_aggregate(ast)
        elif ast.action in ("find", "get"):
            return self._plan_find(ast)
        elif ast.action in ("insert", "update", "delete"):
            return self._plan_mutation(ast)
        else:
            return self._plan_find(ast)

    def explain(self, ast: QueryAST) -> str:
        """Generate EXPLAIN output."""
        plan = self.plan(ast)
        return plan.explain()

    # ── Plan generation ─────────────────────────────────────────────

    def _plan_find(self, ast: QueryAST) -> PhysicalNode:
        """Plan a FIND/GET query."""
        table_stats = self._stats.get_table_stats(ast.table)
        n_rows = table_stats.row_count if table_stats else 10000
        n_pages = table_stats.page_count if table_stats else max(n_rows // 80, 1)

        # Choose access path
        scan_node = self._choose_access_path(ast, table_stats, n_rows, n_pages)

        # Apply non-pushdown filters
        current: PhysicalNode = scan_node
        remaining_preds = self._get_remaining_predicates(ast, scan_node)
        if remaining_preds:
            filt = PhysicalFilter(predicates=remaining_preds, vectorized=(n_rows > 1000))
            filt.children = [current]
            filt.estimated_rows = max(1, int(current.estimated_rows * 0.5))
            filt.estimated_cost = current.estimated_cost + self._cost.cpu_tuple_cost * current.estimated_rows
            current = filt

        # Sort
        if ast.order_by:
            if ast.top_k > 0 or ast.limit > 0:
                k = ast.top_k or ast.limit
                sort_node = PhysicalSort(keys=ast.order_by, top_k=k)
                sort_node.estimated_cost = self._cost.top_k_cost(current.estimated_rows, k)
            else:
                sort_node = PhysicalSort(keys=ast.order_by)
                sort_node.estimated_cost = self._cost.sort_cost(current.estimated_rows)
            sort_node.children = [current]
            sort_node.estimated_rows = current.estimated_rows
            current = sort_node

        # Limit
        if ast.limit > 0:
            lim = PhysicalLimit(limit=ast.limit, offset=ast.offset)
            lim.children = [current]
            lim.estimated_rows = min(ast.limit, current.estimated_rows)
            lim.estimated_cost = current.estimated_cost
            current = lim

        # Late materialization point
        if ast.late_materialize and ast.columns:
            mat = PhysicalLateMaterialize(table=ast.table, columns=ast.columns)
            mat.children = [current]
            mat.estimated_rows = current.estimated_rows
            mat.estimated_cost = current.estimated_cost + self._cost.cpu_tuple_cost * current.estimated_rows
            current = mat
        elif ast.columns:
            proj = PhysicalProject(columns=ast.columns)
            proj.children = [current]
            proj.estimated_rows = current.estimated_rows
            proj.estimated_cost = current.estimated_cost
            current = proj

        return current

    def _plan_search(self, ast: QueryAST) -> PhysicalNode:
        """Plan a lexical search query using BMW."""
        table_stats = self._stats.get_table_stats(ast.table)
        n_docs = table_stats.row_count if table_stats else 10000
        terms = ast.query_text.lower().split()
        top_k = ast.top_k or ast.limit or 10

        bmw = PhysicalBMWScan(table=ast.table, query_terms=terms, top_k=top_k)
        bmw.estimated_rows = top_k
        bmw.estimated_cost = self._cost.bmw_search_cost(n_docs, len(terms), top_k)

        current: PhysicalNode = bmw

        # Apply metadata filters on search results
        if ast.predicates:
            filt = PhysicalFilter(predicates=ast.predicates, vectorized=True)
            filt.children = [current]
            filt.estimated_rows = max(1, int(top_k * 0.8))
            filt.estimated_cost = current.estimated_cost + self._cost.cpu_tuple_cost * top_k
            current = filt

        return current

    def _plan_vector_search(self, ast: QueryAST) -> PhysicalNode:
        """Plan a vector similarity search."""
        table_stats = self._stats.get_table_stats(ast.table)
        n_vecs = table_stats.row_count if table_stats else 10000
        top_k = ast.top_k or 10
        ef = max(top_k * 4, 50)

        hnsw = PhysicalHNSWScan(table=ast.table, vector=ast.query_vector, top_k=top_k, ef_search=ef)
        hnsw.estimated_rows = top_k
        hnsw.estimated_cost = self._cost.hnsw_cost(n_vecs, ef)

        current: PhysicalNode = hnsw

        if ast.predicates:
            filt = PhysicalFilter(predicates=ast.predicates)
            filt.children = [current]
            filt.estimated_rows = max(1, int(top_k * 0.8))
            current = filt

        return current

    def _plan_hybrid_search(self, ast: QueryAST) -> PhysicalNode:
        """Plan a hybrid lexical + vector search."""
        terms = ast.query_text.lower().split()
        top_k = ast.top_k or 10

        # Lexical arm
        bmw = PhysicalBMWScan(table=ast.table, query_terms=terms, top_k=top_k * 2)
        bmw.estimated_rows = top_k * 2
        bmw.estimated_cost = self._cost.bmw_search_cost(10000, len(terms), top_k * 2)

        # Vector arm
        ef = max(top_k * 4, 50)
        hnsw = PhysicalHNSWScan(table=ast.table, vector=ast.query_vector, top_k=top_k * 2, ef_search=ef)
        hnsw.estimated_rows = top_k * 2
        hnsw.estimated_cost = self._cost.hnsw_cost(10000, ef)

        # Fusion
        fusion = PhysicalHybridFusion(alpha=0.5, fusion_method="rrf", top_k=top_k)
        fusion.children = [bmw, hnsw]
        fusion.estimated_rows = top_k
        fusion.estimated_cost = bmw.estimated_cost + hnsw.estimated_cost + self._cost.cpu_tuple_cost * top_k * 4

        return fusion

    def _plan_aggregate(self, ast: QueryAST) -> PhysicalNode:
        """Plan an aggregation query."""
        # Scan base
        inner = self._plan_find(QueryAST(
            action="find", table=ast.table, predicates=ast.predicates,
        ))

        n_rows = inner.estimated_rows
        n_groups = min(n_rows, 1000)  # Estimate

        agg = PhysicalHashAggregate(
            group_columns=ast.group_by, aggregates=ast.aggregates,
            vectorized=(n_rows > 1000),
        )
        agg.children = [inner]
        agg.estimated_rows = n_groups
        agg.estimated_cost = inner.estimated_cost + self._cost.hash_aggregate_cost(n_rows, n_groups)

        current: PhysicalNode = agg

        if ast.order_by:
            sort_node = PhysicalSort(keys=ast.order_by)
            sort_node.children = [current]
            sort_node.estimated_rows = n_groups
            sort_node.estimated_cost = current.estimated_cost + self._cost.sort_cost(n_groups)
            current = sort_node

        if ast.limit:
            lim = PhysicalLimit(limit=ast.limit, offset=ast.offset)
            lim.children = [current]
            lim.estimated_rows = min(ast.limit, current.estimated_rows)
            current = lim

        return current

    def _plan_mutation(self, ast: QueryAST) -> PhysicalNode:
        """Plan INSERT/UPDATE/DELETE (simple for now)."""
        scan = PhysicalSeqScan(table=ast.table)
        scan.estimated_rows = 1
        scan.estimated_cost = self._cost.random_page_cost
        return scan

    # ── Access path selection ───────────────────────────────────────

    def _choose_access_path(self, ast: QueryAST, table_stats: TableStats | None,
                            n_rows: int, n_pages: int) -> PhysicalNode:
        """Choose between seq scan, index scan, bitmap scan based on cost."""
        candidates: list[tuple[float, PhysicalNode]] = []

        # Option 1: Sequential scan
        seq_cost = self._cost.seq_scan_cost(n_pages, n_rows)
        seq = PhysicalSeqScan(table=ast.table)
        seq.estimated_rows = n_rows
        seq.estimated_cost = seq_cost
        candidates.append((seq_cost, seq))

        if not table_stats or not ast.predicates:
            return candidates[0][1]

        # Check each predicate for index usage
        for pred in self._flatten_predicates(ast.predicates):
            if not isinstance(pred, Predicate):
                continue

            # Check for matching index
            for idx_name, idx_stats in table_stats.indexes.items():
                if pred.column in idx_stats.columns:
                    col_stats = table_stats.columns.get(pred.column)
                    selectivity = col_stats.estimate_selectivity(pred.op.value, pred.value) if col_stats else 0.1

                    # Option 2: Index scan
                    idx_cost = self._cost.index_scan_cost(
                        n_pages, n_rows, idx_stats.height, selectivity
                    )
                    idx_scan = PhysicalIndexScan(
                        table=ast.table, index_name=idx_name, predicates=[pred]
                    )
                    idx_scan.estimated_rows = max(1, int(n_rows * selectivity))
                    idx_scan.estimated_cost = idx_cost
                    candidates.append((idx_cost, idx_scan))

                    # Option 3: Bitmap scan (good for moderate selectivity)
                    if 0.01 < selectivity < 0.3:
                        bm_cost = self._cost.bitmap_scan_cost(n_pages, n_rows, selectivity)
                        bm_scan = PhysicalBitmapScan(
                            table=ast.table, index_name=idx_name, predicates=[pred]
                        )
                        bm_scan.estimated_rows = max(1, int(n_rows * selectivity))
                        bm_scan.estimated_cost = bm_cost
                        candidates.append((bm_cost, bm_scan))

        # Choose cheapest
        candidates.sort(key=lambda x: x[0])
        return candidates[0][1]

    def _get_remaining_predicates(self, ast: QueryAST, scan: PhysicalNode) -> list[Predicate | CompoundPredicate]:
        """Get predicates not handled by the index scan."""
        if isinstance(scan, (PhysicalIndexScan, PhysicalBitmapScan)):
            handled = {id(p) for p in scan.predicates}
            return [p for p in ast.predicates if id(p) not in handled]
        return list(ast.predicates)

    @staticmethod
    def _flatten_predicates(preds: list[Predicate | CompoundPredicate]) -> list[Predicate]:
        """Flatten compound predicates for index matching."""
        result: list[Predicate] = []
        for p in preds:
            if isinstance(p, Predicate):
                result.append(p)
            elif isinstance(p, CompoundPredicate):
                for child in p.children:
                    if isinstance(child, Predicate):
                        result.append(child)
        return result
