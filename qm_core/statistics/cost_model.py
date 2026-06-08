"""QM Statistics — Cost Model.

Unified cost model for query planning.  Estimates I/O, CPU, and memory costs
for every physical operator the planner may consider.

Design:
    - All costs are expressed in *arbitrary cost units* (ACU).
    - 1 ACU ≈ 1 sequential page read.
    - Sequential I/O < random I/O < CPU per tuple.
    - The model is configurable: users can tune knobs via CostParams.

Main API:
    model = CostModelV2(params, catalog)
    cost = model.estimate(physical_node)
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from typing import Any


@dataclass(frozen=True)
class CostParams:
    """Tunable knobs for the cost model."""
    # I/O costs (in ACUs per page)
    seq_page_cost: float = 1.0
    random_page_cost: float = 4.0
    # CPU costs (in ACUs per operation)
    cpu_tuple_cost: float = 0.01
    cpu_index_cost: float = 0.005
    cpu_operator_cost: float = 0.0025
    cpu_hash_cost: float = 0.005       # Hashing a row for hash aggregate
    cpu_sort_cmp_cost: float = 0.003   # One comparison in sort
    cpu_distance_cost: float = 0.002   # One vector distance computation
    cpu_score_cost: float = 0.003      # One BM25 scoring operation
    # Memory
    work_mem_bytes: int = 64 * 1024 * 1024  # 64 MB sort/hash budget
    effective_cache_size: int = 256 * 1024 * 1024
    page_size: int = 8192
    # Parallel
    parallel_workers: int = 1


@dataclass
class CostEstimate:
    """Breakdown of a cost estimate."""
    io_cost: float = 0.0
    cpu_cost: float = 0.0
    memory_bytes: int = 0
    startup_cost: float = 0.0   # Cost before first row
    total_cost: float = 0.0     # Total cost
    estimated_rows: int = 0

    def __post_init__(self) -> None:
        if self.total_cost == 0.0:
            self.total_cost = self.startup_cost + self.io_cost + self.cpu_cost


@dataclass
class TableCatalog:
    """Metadata about tables used by the cost model."""
    tables: dict[str, TableMeta] = field(default_factory=dict)

    def register(self, name: str, meta: "TableMeta") -> None:
        self.tables[name] = meta

    def get(self, name: str) -> "TableMeta | None":
        return self.tables.get(name)


@dataclass
class TableMeta:
    """Physical metadata for one table."""
    row_count: int = 0
    page_count: int = 0
    avg_row_width: int = 100
    indexes: dict[str, "IndexMeta"] = field(default_factory=dict)

    @property
    def rows_per_page(self) -> int:
        return max(1, 8192 // self.avg_row_width)


@dataclass
class IndexMeta:
    """Physical metadata for one index."""
    height: int = 3
    leaf_pages: int = 0
    distinct_keys: int = 0
    columns: list[str] = field(default_factory=list)
    is_unique: bool = False


class CostModelV2:
    """Unified cost model for the QM query planner.

    Usage:
        catalog = TableCatalog()
        catalog.register("docs", TableMeta(row_count=100_000, page_count=1250))
        model = CostModelV2(catalog=catalog)
        est = model.seq_scan("docs")
    """

    def __init__(self, params: CostParams | None = None,
                 catalog: TableCatalog | None = None) -> None:
        self.p = params or CostParams()
        self.catalog = catalog or TableCatalog()

    def _meta(self, table: str) -> TableMeta:
        return self.catalog.get(table) or TableMeta(row_count=10000, page_count=125)

    # ── Scan operators ──────────────────────────────────────────────

    def seq_scan(self, table: str, selectivity: float = 1.0) -> CostEstimate:
        """Full sequential scan."""
        m = self._meta(table)
        io = self.p.seq_page_cost * m.page_count
        cpu = self.p.cpu_tuple_cost * m.row_count
        return CostEstimate(
            io_cost=io, cpu_cost=cpu,
            estimated_rows=max(1, int(m.row_count * selectivity)),
        )

    def index_scan(self, table: str, index: str, selectivity: float) -> CostEstimate:
        """B-tree index scan."""
        m = self._meta(table)
        idx = m.indexes.get(index, IndexMeta())
        # Descend tree + random I/O for matching data pages
        index_io = self.p.random_page_cost * idx.height
        data_pages = max(1, int(m.page_count * selectivity))
        # If selectivity is very low, pages are randomly scattered
        cache_hit_prob = min(1.0, self.p.effective_cache_size / (m.page_count * self.p.page_size + 1))
        effective_random_cost = self.p.random_page_cost * (1 - cache_hit_prob) + self.p.seq_page_cost * cache_hit_prob
        data_io = effective_random_cost * data_pages
        cpu = self.p.cpu_index_cost * max(1, int(m.row_count * selectivity))
        est_rows = max(1, int(m.row_count * selectivity))
        return CostEstimate(
            io_cost=index_io + data_io, cpu_cost=cpu,
            startup_cost=index_io,
            estimated_rows=est_rows,
        )

    def bitmap_scan(self, table: str, index: str, selectivity: float) -> CostEstimate:
        """Bitmap index scan — between seq and random I/O."""
        m = self._meta(table)
        selected_pages = max(1, int(m.page_count * selectivity))
        # Bitmap scan reads pages in order → closer to sequential
        blend = (self.p.seq_page_cost + self.p.random_page_cost) / 2.0
        io = blend * selected_pages
        cpu = self.p.cpu_tuple_cost * max(1, int(m.row_count * selectivity))
        # Bitmap build cost
        startup = self.p.cpu_index_cost * max(1, int(m.row_count * selectivity))
        return CostEstimate(
            io_cost=io, cpu_cost=cpu, startup_cost=startup,
            estimated_rows=max(1, int(m.row_count * selectivity)),
        )

    # ── Join operators ──────────────────────────────────────────────

    def nested_loop_join(self, outer_rows: int, inner_est: CostEstimate) -> CostEstimate:
        """Nested loop join cost."""
        io = inner_est.total_cost * outer_rows
        cpu = self.p.cpu_tuple_cost * outer_rows * inner_est.estimated_rows
        return CostEstimate(io_cost=io, cpu_cost=cpu, estimated_rows=outer_rows * inner_est.estimated_rows)

    def hash_join(self, build_rows: int, probe_rows: int) -> CostEstimate:
        """Hash join cost."""
        build_cpu = self.p.cpu_hash_cost * build_rows
        probe_cpu = self.p.cpu_hash_cost * probe_rows + self.p.cpu_tuple_cost * probe_rows
        # Memory for hash table
        mem = build_rows * 200  # Rough estimate: 200 bytes/row
        spill_io = 0.0
        if mem > self.p.work_mem_bytes:
            # Need disk spill
            spill_pages = mem // self.p.page_size
            spill_io = self.p.seq_page_cost * spill_pages * 2  # write + read
        return CostEstimate(
            io_cost=spill_io, cpu_cost=build_cpu + probe_cpu,
            memory_bytes=mem, startup_cost=build_cpu,
            estimated_rows=min(build_rows, probe_rows),
        )

    def merge_join(self, left_rows: int, right_rows: int,
                   left_sorted: bool = False, right_sorted: bool = False) -> CostEstimate:
        """Sort-merge join cost."""
        sort_left = 0.0 if left_sorted else self.sort_cost(left_rows)
        sort_right = 0.0 if right_sorted else self.sort_cost(right_rows)
        merge_cpu = self.p.cpu_tuple_cost * (left_rows + right_rows)
        return CostEstimate(
            cpu_cost=sort_left + sort_right + merge_cpu,
            startup_cost=sort_left + sort_right,
            estimated_rows=min(left_rows, right_rows),
        )

    # ── Aggregate / Sort ────────────────────────────────────────────

    def sort_cost(self, n_rows: int) -> float:
        """Cost of sorting n rows."""
        if n_rows <= 1:
            return 0.0
        comparisons = n_rows * math.log2(max(n_rows, 2))
        cpu = self.p.cpu_sort_cmp_cost * comparisons
        # Check if needs external sort
        mem = n_rows * 200
        if mem > self.p.work_mem_bytes:
            runs = mem // self.p.work_mem_bytes + 1
            pages = mem // self.p.page_size
            cpu += self.p.seq_page_cost * pages * 2 * math.ceil(math.log2(max(runs, 2)))
        return cpu

    def top_k_cost(self, n_rows: int, k: int) -> float:
        """Cost of top-k selection via heap."""
        if n_rows <= 0 or k <= 0:
            return 0.0
        return self.p.cpu_sort_cmp_cost * n_rows * math.log2(max(k, 2))

    def hash_aggregate(self, n_rows: int, n_groups: int) -> CostEstimate:
        """Hash aggregate cost."""
        cpu = self.p.cpu_hash_cost * n_rows + self.p.cpu_tuple_cost * n_groups
        mem = n_groups * 200
        return CostEstimate(cpu_cost=cpu, memory_bytes=mem, estimated_rows=n_groups)

    # ── Search operators ────────────────────────────────────────────

    def bmw_search(self, n_docs: int, n_terms: int, top_k: int) -> CostEstimate:
        """Block-Max WAND search cost."""
        # BMW skips ~70-80% of postings
        processed_fraction = 0.25
        cpu = self.p.cpu_score_cost * n_docs * processed_fraction * n_terms
        io = 0.0  # Postings are typically in memory
        return CostEstimate(io_cost=io, cpu_cost=cpu, estimated_rows=top_k)

    def wand_search(self, n_docs: int, n_terms: int, top_k: int) -> CostEstimate:
        """WAND search cost (worse than BMW)."""
        processed_fraction = 0.40
        cpu = self.p.cpu_score_cost * n_docs * processed_fraction * n_terms
        return CostEstimate(cpu_cost=cpu, estimated_rows=top_k)

    def hnsw_search(self, n_vectors: int, dim: int, ef_search: int) -> CostEstimate:
        """HNSW approximate nearest neighbor search."""
        if n_vectors <= 0:
            return CostEstimate()
        layers = max(1, int(math.log2(max(n_vectors, 2))))
        distance_comps = ef_search * layers
        cpu = self.p.cpu_distance_cost * distance_comps * dim / 128  # normalized by 128-d
        # Random memory access pattern
        io = self.p.random_page_cost * distance_comps / 100  # amortized
        return CostEstimate(io_cost=io, cpu_cost=cpu, estimated_rows=ef_search)

    def pq_search(self, n_vectors: int, n_subquantizers: int, top_k: int) -> CostEstimate:
        """Product Quantization search (very cheap per vector)."""
        cpu = self.p.cpu_operator_cost * n_vectors * n_subquantizers
        return CostEstimate(cpu_cost=cpu, estimated_rows=top_k)

    def hybrid_fusion(self, lex_cost: CostEstimate, vec_cost: CostEstimate,
                      top_k: int) -> CostEstimate:
        """Cost of hybrid search (lexical + vector + fusion)."""
        fusion_cpu = self.p.cpu_tuple_cost * top_k * 4
        return CostEstimate(
            io_cost=lex_cost.io_cost + vec_cost.io_cost,
            cpu_cost=lex_cost.cpu_cost + vec_cost.cpu_cost + fusion_cpu,
            estimated_rows=top_k,
        )

    # ── Utility ─────────────────────────────────────────────────────

    def filter_cost(self, n_rows: int, n_predicates: int = 1) -> float:
        return self.p.cpu_operator_cost * n_rows * n_predicates

    def project_cost(self, n_rows: int, n_cols: int) -> float:
        return self.p.cpu_tuple_cost * n_rows * 0.1 * n_cols

    def late_materialize_cost(self, n_rows: int) -> CostEstimate:
        """Late materialization: random reads for final top-k rows."""
        io = self.p.random_page_cost * n_rows  # Worst case: each row on different page
        cpu = self.p.cpu_tuple_cost * n_rows
        return CostEstimate(io_cost=io, cpu_cost=cpu, estimated_rows=n_rows)
