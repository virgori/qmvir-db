"""QM Index — Statistics Collector for Query Planner.

Provides:
    - Cardinality estimation per column/index
    - Selectivity estimation
    - Histogram-based distribution stats
    - Most-common values (MCV)
    - Null fraction
    - Distinct count (via HyperLogLog)
    - Index size/depth metrics
    - Correlation estimation (physical vs logical order)
"""

from __future__ import annotations

import math
import time
from dataclasses import dataclass, field
from typing import Any


@dataclass(slots=True)
class ColumnStats:
    """Statistics for a single column."""
    column_name: str
    table_name: str
    row_count: int = 0
    distinct_count: int = 0
    null_count: int = 0
    min_value: Any = None
    max_value: Any = None
    avg_width: float = 0.0  # Average byte width
    correlation: float = 0.0  # Physical vs logical sort order [-1, 1]
    mcv_values: list[Any] = field(default_factory=list)  # Most common values
    mcv_frequencies: list[float] = field(default_factory=list)  # Their frequencies
    histogram_bounds: list[Any] = field(default_factory=list)  # Equi-depth histogram
    last_analyzed: float = 0.0

    @property
    def null_fraction(self) -> float:
        return self.null_count / max(self.row_count, 1)

    @property
    def distinct_fraction(self) -> float:
        return self.distinct_count / max(self.row_count, 1)

    def estimate_selectivity(self, op: str, value: Any) -> float:
        """Estimate selectivity for a predicate."""
        if self.row_count == 0:
            return 0.0

        if op == "eq":
            return self._selectivity_eq(value)
        elif op == "neq":
            return 1.0 - self._selectivity_eq(value)
        elif op in ("gt", "gte"):
            return self._selectivity_range(value, is_upper=False)
        elif op in ("lt", "lte"):
            return self._selectivity_range(value, is_upper=True)
        elif op == "between":
            if isinstance(value, (list, tuple)) and len(value) == 2:
                low_sel = self._selectivity_range(value[0], is_upper=False)
                high_sel = self._selectivity_range(value[1], is_upper=True)
                return max(0.0, low_sel + high_sel - 1.0)
        elif op == "is_null":
            return self.null_fraction
        elif op == "is_not_null":
            return 1.0 - self.null_fraction

        return 0.1  # Default guess

    def _selectivity_eq(self, value: Any) -> float:
        """Equality selectivity."""
        # Check MCV first
        if value in self.mcv_values:
            idx = self.mcv_values.index(value)
            return self.mcv_frequencies[idx]
        # Uniform assumption for non-MCV values
        if self.distinct_count == 0:
            return 0.0
        mcv_total = sum(self.mcv_frequencies)
        remaining_distinct = max(self.distinct_count - len(self.mcv_values), 1)
        return (1.0 - mcv_total) / remaining_distinct

    def _selectivity_range(self, value: Any, is_upper: bool) -> float:
        """Range selectivity using histogram or linear interpolation."""
        if self.min_value is None or self.max_value is None:
            return 0.5
        try:
            if self.min_value == self.max_value:
                return 1.0 if value == self.min_value else 0.0
            range_size = float(self.max_value) - float(self.min_value)
            if range_size == 0:
                return 0.5
            frac = (float(value) - float(self.min_value)) / range_size
            frac = max(0.0, min(1.0, frac))
            return frac if is_upper else (1.0 - frac)
        except (TypeError, ValueError):
            return 0.5


@dataclass(slots=True)
class IndexStats:
    """Statistics for an index."""
    index_name: str
    table_name: str
    index_type: str  # btree, bitmap, inverted, vector
    columns: list[str] = field(default_factory=list)
    row_count: int = 0
    distinct_count: int = 0
    height: int = 0  # B-tree height
    leaf_pages: int = 0
    total_pages: int = 0
    size_bytes: int = 0
    avg_leaf_density: float = 0.75
    last_analyzed: float = 0.0

    @property
    def estimated_io_cost(self) -> float:
        """Estimated I/O cost for a single point lookup (in page reads)."""
        return float(self.height) + 1.0


@dataclass(slots=True)
class TableStats:
    """Aggregate statistics for a table."""
    table_name: str
    row_count: int = 0
    page_count: int = 0
    row_width_avg: float = 100.0
    columns: dict[str, ColumnStats] = field(default_factory=dict)
    indexes: dict[str, IndexStats] = field(default_factory=dict)
    last_analyzed: float = 0.0

    @property
    def rows_per_page(self) -> float:
        return 8192 / max(self.row_width_avg, 1.0)


class StatsCollector:
    """Collects and manages statistics for the query planner.

    Usage:
        collector = StatsCollector()
        collector.analyze_column("users", "age", values)
        stats = collector.get_column_stats("users", "age")
        sel = stats.estimate_selectivity("gt", 25)
    """

    def __init__(self, n_mcv: int = 10, n_histogram_buckets: int = 100) -> None:
        self._n_mcv = n_mcv
        self._n_buckets = n_histogram_buckets
        self._table_stats: dict[str, TableStats] = {}

    def analyze_column(self, table: str, column: str, values: list[Any]) -> ColumnStats:
        """Analyze a column and compute statistics."""
        non_null = [v for v in values if v is not None]
        null_count = len(values) - len(non_null)

        # Distinct count
        distinct = set(non_null)
        distinct_count = len(distinct)

        # Min/max
        min_val = min(non_null) if non_null else None
        max_val = max(non_null) if non_null else None

        # MCV (Most Common Values)
        freq: dict[Any, int] = {}
        for v in non_null:
            freq[v] = freq.get(v, 0) + 1
        sorted_freq = sorted(freq.items(), key=lambda x: x[1], reverse=True)
        mcv_values = [v for v, _ in sorted_freq[:self._n_mcv]]
        mcv_frequencies = [c / max(len(values), 1) for _, c in sorted_freq[:self._n_mcv]]

        # Equi-depth histogram
        histogram_bounds: list[Any] = []
        if non_null:
            try:
                sorted_vals = sorted(non_null)
                step = max(len(sorted_vals) // self._n_buckets, 1)
                histogram_bounds = [sorted_vals[i] for i in range(0, len(sorted_vals), step)]
            except TypeError:
                pass

        # Average width (rough estimate)
        avg_width = 0.0
        if non_null:
            try:
                avg_width = sum(len(str(v)) for v in non_null[:1000]) / min(len(non_null), 1000)
            except Exception:
                avg_width = 8.0

        stats = ColumnStats(
            column_name=column, table_name=table,
            row_count=len(values), distinct_count=distinct_count,
            null_count=null_count, min_value=min_val, max_value=max_val,
            avg_width=avg_width,
            mcv_values=mcv_values, mcv_frequencies=mcv_frequencies,
            histogram_bounds=histogram_bounds,
            last_analyzed=time.time(),
        )

        # Store
        if table not in self._table_stats:
            self._table_stats[table] = TableStats(table_name=table)
        self._table_stats[table].columns[column] = stats
        self._table_stats[table].row_count = max(self._table_stats[table].row_count, len(values))
        self._table_stats[table].last_analyzed = time.time()

        return stats

    def register_index(self, stats: IndexStats) -> None:
        """Register index statistics."""
        table = stats.table_name
        if table not in self._table_stats:
            self._table_stats[table] = TableStats(table_name=table)
        self._table_stats[table].indexes[stats.index_name] = stats

    def get_table_stats(self, table: str) -> TableStats | None:
        return self._table_stats.get(table)

    def get_column_stats(self, table: str, column: str) -> ColumnStats | None:
        ts = self._table_stats.get(table)
        if ts:
            return ts.columns.get(column)
        return None

    def estimate_cardinality(self, table: str, predicates: list[dict[str, Any]]) -> int:
        """Estimate result cardinality for a set of predicates."""
        ts = self._table_stats.get(table)
        if not ts:
            return 1000  # Default guess

        selectivity = 1.0
        for pred in predicates:
            col = pred.get("column")
            op = pred.get("op", "eq")
            value = pred.get("value")
            cs = ts.columns.get(col) if col else None
            if cs:
                selectivity *= cs.estimate_selectivity(op, value)
            else:
                selectivity *= 0.1  # Default

        return max(1, int(ts.row_count * selectivity))

    def list_tables(self) -> list[str]:
        return list(self._table_stats.keys())
