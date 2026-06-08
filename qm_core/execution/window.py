"""QM Execution — Window Functions & CTE (Common Table Expressions).

Window functions:
    ROW_NUMBER, RANK, DENSE_RANK, NTILE
    SUM, AVG, COUNT, MIN, MAX over window frames
    LAG, LEAD, FIRST_VALUE, LAST_VALUE

CTE:
    WITH name AS (subquery) SELECT ...
    Materializes CTE once, referenced by name in main query.

Integrates with the volcano-style join.py operators.
"""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass, field
from typing import Any, Callable, Iterator

try:
    import qm_engine as _qm_engine
except Exception:
    _qm_engine = None

from qm_core.execution.join import Operator, Row, ScanOperator


# ── Window Frame Spec ───────────────────────────────────────────────

@dataclass
class FrameBound:
    """Window frame boundary."""
    UNBOUNDED_PRECEDING = "UNBOUNDED PRECEDING"
    CURRENT_ROW = "CURRENT ROW"
    UNBOUNDED_FOLLOWING = "UNBOUNDED FOLLOWING"

    bound_type: str = "UNBOUNDED PRECEDING"
    offset: int = 0  # For N PRECEDING / N FOLLOWING


@dataclass
class WindowSpec:
    """Window function specification."""
    partition_by: list[str] = field(default_factory=list)
    order_by: list[tuple[str, bool]] = field(default_factory=list)  # (col, asc)
    frame_start: FrameBound = field(default_factory=lambda: FrameBound("UNBOUNDED PRECEDING"))
    frame_end: FrameBound = field(default_factory=lambda: FrameBound("CURRENT ROW"))


# ── Window Operator ─────────────────────────────────────────────────

class WindowOperator(Operator):
    """Compute window functions over a child operator's output.

    Steps:
        1. Materialize all input rows
        2. Partition by partition_by columns
        3. Sort each partition by order_by
        4. For each row, compute window function over its frame
        5. Emit rows with new computed columns

    Usage:
        wop = WindowOperator(
            child=scan,
            functions=[
                ("ROW_NUMBER", None, "rn", WindowSpec(order_by=[("salary", False)])),
                ("SUM", "salary", "running_sum", WindowSpec(order_by=[("id", True)])),
            ]
        )
    """

    def __init__(
        self,
        child: Operator,
        functions: list[tuple[str, str | None, str, WindowSpec]],
        # (func_name, input_col, output_alias, window_spec)
    ) -> None:
        self._child = child
        self._functions = functions
        self._results: list[Row] = []
        self._idx = 0

    def open(self) -> None:
        self._child.open()
        rows: list[Row] = []
        while True:
            row = self._child.next()
            if row is None:
                break
            rows.append(row)
        self._child.close()

        # For each window function, compute values
        for func_name, input_col, alias, spec in self._functions:
            partitions = self._partition(rows, spec.partition_by)
            for part_rows in partitions.values():
                sorted_rows = self._sort_partition(part_rows, spec.order_by)
                values = self._compute_window(func_name, input_col, sorted_rows, spec)
                for i, row in enumerate(sorted_rows):
                    row[alias] = values[i]

        self._results = rows
        self._idx = 0

    def next(self) -> Row | None:
        if self._idx >= len(self._results):
            return None
        row = self._results[self._idx]
        self._idx += 1
        return row

    def close(self) -> None:
        self._results.clear()
        self._idx = 0

    @staticmethod
    def _partition(rows: list[Row], partition_by: list[str]) -> dict[tuple, list[Row]]:
        """Group rows by partition key."""
        if not partition_by:
            return {(): rows}
        groups: dict[tuple, list[Row]] = defaultdict(list)
        for row in rows:
            key = tuple(row.get(c) for c in partition_by)
            groups[key].append(row)
        return dict(groups)

    @staticmethod
    def _sort_partition(rows: list[Row], order_by: list[tuple[str, bool]]) -> list[Row]:
        """Sort rows within a partition."""
        if not order_by:
            return rows
        for col, asc in reversed(order_by):
            rows.sort(key=lambda r, c=col: (r.get(c) is None, r.get(c)), reverse=not asc)
        return rows

    def _compute_window(
        self, func: str, col: str | None, rows: list[Row], spec: WindowSpec
    ) -> list[Any]:
        """Compute window function for each row in a sorted partition."""
        func = func.upper()
        n = len(rows)
        results: list[Any] = []

        for i in range(n):
            frame_start, frame_end = self._resolve_frame(i, n, spec)
            frame_rows = rows[frame_start:frame_end + 1]

            if func == "ROW_NUMBER":
                results.append(i + 1)
            elif func == "RANK":
                results.append(self._rank(rows, i, spec.order_by))
            elif func == "DENSE_RANK":
                results.append(self._dense_rank(rows, i, spec.order_by))
            elif func == "NTILE":
                num_buckets = int(col) if col and col.isdigit() else 4
                results.append((i * num_buckets) // n + 1)
            elif func == "LAG":
                offset = 1
                if i - offset >= 0:
                    results.append(rows[i - offset].get(col))
                else:
                    results.append(None)
            elif func == "LEAD":
                offset = 1
                if i + offset < n:
                    results.append(rows[i + offset].get(col))
                else:
                    results.append(None)
            elif func == "FIRST_VALUE":
                results.append(frame_rows[0].get(col) if frame_rows else None)
            elif func == "LAST_VALUE":
                results.append(frame_rows[-1].get(col) if frame_rows else None)
            else:
                # Aggregate window functions
                vals = [r.get(col) for r in frame_rows if r.get(col) is not None]
                results.append(self._agg(func, vals))

        return results

    @staticmethod
    def _resolve_frame(idx: int, n: int, spec: WindowSpec) -> tuple[int, int]:
        """Resolve frame bounds to row indices."""
        # Start
        if spec.frame_start.bound_type == "UNBOUNDED PRECEDING":
            start = 0
        elif spec.frame_start.bound_type == "CURRENT ROW":
            start = idx
        else:
            start = max(0, idx - spec.frame_start.offset)

        # End
        if spec.frame_end.bound_type == "UNBOUNDED FOLLOWING":
            end = n - 1
        elif spec.frame_end.bound_type == "CURRENT ROW":
            end = idx
        else:
            end = min(n - 1, idx + spec.frame_end.offset)

        return start, end

    @staticmethod
    def _rank(rows: list[Row], idx: int, order_by: list[tuple[str, bool]]) -> int:
        """Standard RANK: same values get same rank, with gaps."""
        if not order_by:
            return 1
        current_vals = tuple(rows[idx].get(c) for c, _ in order_by)
        rank = 1
        for i in range(idx):
            prev_vals = tuple(rows[i].get(c) for c, _ in order_by)
            if prev_vals != current_vals:
                rank = i + 1
        if idx > 0:
            prev_vals = tuple(rows[idx - 1].get(c) for c, _ in order_by)
            if prev_vals == current_vals:
                # Find first occurrence of this value
                for i in range(idx - 1, -1, -1):
                    v = tuple(rows[i].get(c) for c, _ in order_by)
                    if v != current_vals:
                        return i + 2
                return 1
        return rank

    @staticmethod
    def _dense_rank(rows: list[Row], idx: int, order_by: list[tuple[str, bool]]) -> int:
        """DENSE_RANK: same values get same rank, NO gaps."""
        if not order_by:
            return 1
        seen: list[tuple] = []
        for i in range(idx + 1):
            vals = tuple(rows[i].get(c) for c, _ in order_by)
            if vals not in seen:
                seen.append(vals)
        return len(seen)

    @staticmethod
    def _agg(func: str, vals: list[Any]) -> Any:
        if not vals:
            return None
        func = func.upper()
        if func == "SUM":
            return sum(vals)
        if func == "AVG":
            return sum(vals) / len(vals)
        if func == "COUNT":
            return len(vals)
        if func == "MIN":
            return min(vals)
        if func == "MAX":
            return max(vals)
        return None


# ── CTE Operator ────────────────────────────────────────────────────

class CTEOperator(Operator):
    """Common Table Expression — materializes a subquery once.

    Usage:
        cte = CTEOperator(
            cte_defs={"top_employees": subquery_operator},
            main_query=main_operator_that_references_top_employees,
        )
    """

    def __init__(
        self,
        cte_defs: dict[str, Operator],
        main_query: Callable[[dict[str, list[Row]]], Operator],
    ) -> None:
        self._cte_defs = cte_defs
        self._main_factory = main_query
        self._main: Operator | None = None

    def open(self) -> None:
        # Materialize each CTE
        materialized: dict[str, list[Row]] = {}
        for name, op in self._cte_defs.items():
            rows: list[Row] = []
            op.open()
            while True:
                row = op.next()
                if row is None:
                    break
                rows.append(row)
            op.close()
            materialized[name] = rows

        # Create main query operator with materialized CTEs
        self._main = self._main_factory(materialized)
        self._main.open()

    def next(self) -> Row | None:
        if self._main is None:
            return None
        return self._main.next()

    def close(self) -> None:
        if self._main:
            self._main.close()


# ── Distinct Operator ───────────────────────────────────────────────

class DistinctOperator(Operator):
    """Remove duplicate rows."""

    def __init__(self, child: Operator) -> None:
        self._child = child
        self._seen: set[tuple] = set()

    def open(self) -> None:
        self._child.open()
        self._seen.clear()

    def next(self) -> Row | None:
        while True:
            row = self._child.next()
            if row is None:
                return None
            key = tuple(sorted(row.items()))
            if key not in self._seen:
                self._seen.add(key)
                return row

    def close(self) -> None:
        self._child.close()
        self._seen.clear()


# ── Union / Intersect / Except ──────────────────────────────────────

class UnionOperator(Operator):
    """UNION ALL — concatenate two inputs."""

    def __init__(self, left: Operator, right: Operator, distinct: bool = False) -> None:
        self._left = left
        self._right = right
        self._distinct = distinct
        self._on_right = False
        self._seen: set[tuple] = set()

    def open(self) -> None:
        self._left.open()
        self._on_right = False
        self._seen.clear()

    def next(self) -> Row | None:
        while True:
            if not self._on_right:
                row = self._left.next()
                if row is None:
                    self._left.close()
                    self._right.open()
                    self._on_right = True
                    continue
            else:
                row = self._right.next()
                if row is None:
                    return None
            if self._distinct:
                key = tuple(sorted(row.items()))
                if key in self._seen:
                    continue
                self._seen.add(key)
            return row

    def close(self) -> None:
        if not self._on_right:
            self._left.close()
        self._right.close()
        self._seen.clear()
