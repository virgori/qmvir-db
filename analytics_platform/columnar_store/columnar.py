"""QM Analytics Platform — Columnar Store.

Column-oriented storage engine optimized for:
  - Dictionary encoding
  - RLE (Run-Length Encoding)
  - Bit-packing
  - Delta encoding
  - Zone maps (min/max per column chunk)
  - Predicate pushdown
  - Column pruning
"""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class EncodingType(Enum):
    """Column encoding types."""

    PLAIN = "plain"
    DICTIONARY = "dictionary"
    RLE = "rle"
    DELTA = "delta"
    BIT_PACKED = "bit_packed"


@dataclass
class ZoneMap:
    """Min/max statistics for a column chunk (for predicate pushdown)."""

    col_name: str
    min_val: Any = None
    max_val: Any = None
    null_count: int = 0
    row_count: int = 0

    def can_skip(self, predicate_op: str, value: Any) -> bool:
        """Check if this zone can be skipped based on a predicate."""
        if self.min_val is None or self.max_val is None:
            return False
        try:
            if predicate_op == "eq" and (value < self.min_val or value > self.max_val):
                return True
            if predicate_op == "gt" and value >= self.max_val:
                return True
            if predicate_op == "lt" and value <= self.min_val:
                return True
            if predicate_op == "gte" and value > self.max_val:
                return True
            if predicate_op == "lte" and value < self.min_val:
                return True
        except TypeError:
            return False
        return False


@dataclass
class ColumnChunk:
    """A chunk of column data with encoding and statistics."""

    col_name: str
    values: list[Any] = field(default_factory=list)
    encoding: EncodingType = EncodingType.PLAIN
    zone_map: ZoneMap | None = None
    dictionary: dict[Any, int] | None = None

    def append(self, value: Any) -> None:
        self.values.append(value)

    def build_zone_map(self) -> ZoneMap:
        non_null = [v for v in self.values if v is not None]
        self.zone_map = ZoneMap(
            col_name=self.col_name,
            min_val=min(non_null) if non_null else None,
            max_val=max(non_null) if non_null else None,
            null_count=len(self.values) - len(non_null),
            row_count=len(self.values),
        )
        return self.zone_map

    def apply_dictionary_encoding(self) -> None:
        """Apply dictionary encoding to this chunk."""
        unique_vals = list(set(v for v in self.values if v is not None))
        if len(unique_vals) < len(self.values) * 0.5:  # Worth encoding
            self.dictionary = {v: i for i, v in enumerate(unique_vals)}
            self.encoding = EncodingType.DICTIONARY


@dataclass
class ColumnarPartition:
    """A partition of columnar data (e.g., by time range)."""

    partition_id: str
    columns: dict[str, ColumnChunk] = field(default_factory=dict)
    row_count: int = 0

    def insert_row(self, row: dict[str, Any]) -> None:
        for col_name, value in row.items():
            if col_name not in self.columns:
                self.columns[col_name] = ColumnChunk(col_name=col_name)
            self.columns[col_name].append(value)
        self.row_count += 1

    def build_zone_maps(self) -> None:
        for chunk in self.columns.values():
            chunk.build_zone_map()

    def get_column(self, col_name: str) -> list[Any]:
        chunk = self.columns.get(col_name)
        return chunk.values if chunk else []


class ColumnarStore:
    """Columnar storage engine for analytics workloads."""

    def __init__(self, partition_size: int = 100_000) -> None:
        self._partitions: dict[str, dict[str, ColumnarPartition]] = {}
        self._partition_size = partition_size
        self._partition_counters: dict[str, int] = defaultdict(int)

    def insert(self, dataset: str, row: dict[str, Any]) -> None:
        """Insert a row into a dataset."""
        if dataset not in self._partitions:
            self._partitions[dataset] = {}

        # Get active partition
        active = self._get_active_partition(dataset)
        active.insert_row(row)

        if active.row_count >= self._partition_size:
            active.build_zone_maps()

    def scan(
        self,
        dataset: str,
        columns: list[str] | None = None,
        where: dict[str, Any] | None = None,
        predicates: dict[str, Any] | None = None,
    ) -> list[dict[str, Any]]:
        """Scan a dataset with optional column pruning and predicate pushdown."""
        where = where or predicates
        partitions = self._partitions.get(dataset, {})
        results: list[dict[str, Any]] = []

        for part in partitions.values():
            # Check zone maps for predicate pushdown
            if where and self._can_skip_partition(part, where):
                continue

            # Get row count
            row_count = part.row_count
            target_cols = columns or list(part.columns.keys())

            for i in range(row_count):
                row: dict[str, Any] = {}
                skip = False

                for col in target_cols:
                    vals = part.get_column(col)
                    row[col] = vals[i] if i < len(vals) else None

                # Apply filters
                if where:
                    for key, val in where.items():
                        row_val = row.get(key)
                        if isinstance(val, dict):
                            # Operator filters
                            for op, operand in val.items():
                                if op == "gte" and (row_val is None or row_val < operand):
                                    skip = True
                                elif op == "lt" and (row_val is None or row_val >= operand):
                                    skip = True
                                elif op == "gt" and (row_val is None or row_val <= operand):
                                    skip = True
                                elif op == "lte" and (row_val is None or row_val > operand):
                                    skip = True
                        else:
                            if row_val != val:
                                skip = True

                    if skip:
                        continue

                results.append(row)

        return results

    def aggregate(
        self,
        dataset: str,
        group_by: list[str],
        metrics: list[dict[str, str]],
        where: dict[str, Any] | None = None,
    ) -> list[dict[str, Any]]:
        """Run aggregation query with GROUP BY."""
        rows = self.scan(dataset, where=where)

        # Group
        groups: dict[tuple, list[dict[str, Any]]] = defaultdict(list)
        for row in rows:
            key = tuple(row.get(g) for g in group_by)
            groups[key].append(row)

        # Aggregate
        results: list[dict[str, Any]] = []
        for key, group_rows in groups.items():
            result: dict[str, Any] = {}
            for i, g in enumerate(group_by):
                result[g] = key[i]

            for metric in metrics:
                for agg_fn, col in metric.items():
                    if agg_fn == "count":
                        result[f"count_{col}"] = len(group_rows)
                        if col == "*":
                            result["count"] = len(group_rows)
                    elif agg_fn == "sum":
                        result[f"sum_{col}"] = sum(
                            r.get(col, 0) for r in group_rows if r.get(col) is not None
                        )
                    elif agg_fn == "avg":
                        vals = [r.get(col, 0) for r in group_rows if r.get(col) is not None]
                        result[f"avg_{col}"] = sum(vals) / len(vals) if vals else 0
                    elif agg_fn == "min":
                        vals = [r.get(col) for r in group_rows if r.get(col) is not None]
                        result[f"min_{col}"] = min(vals) if vals else None
                    elif agg_fn == "max":
                        vals = [r.get(col) for r in group_rows if r.get(col) is not None]
                        result[f"max_{col}"] = max(vals) if vals else None

            results.append(result)

        return results

    def _get_active_partition(self, dataset: str) -> ColumnarPartition:
        parts = self._partitions[dataset]
        active_id = f"{dataset}_p{self._partition_counters[dataset]}"

        if active_id not in parts or parts[active_id].row_count >= self._partition_size:
            self._partition_counters[dataset] += 1
            active_id = f"{dataset}_p{self._partition_counters[dataset]}"
            parts[active_id] = ColumnarPartition(partition_id=active_id)

        return parts[active_id]

    def _can_skip_partition(self, partition: ColumnarPartition, where: dict[str, Any]) -> bool:
        """Check zone maps to see if partition can be skipped."""
        for key, val in where.items():
            chunk = partition.columns.get(key)
            if chunk and chunk.zone_map:
                if isinstance(val, dict):
                    for op, operand in val.items():
                        if chunk.zone_map.can_skip(op, operand):
                            return True
                else:
                    if chunk.zone_map.can_skip("eq", val):
                        return True
        return False
