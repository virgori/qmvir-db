"""QM Execution — JOIN Operators (Hash Join, Merge Join, Nested Loop).

Volcano-style iterator model: each operator has open/next/close.
Every next() returns one row at a time (pull-based pipeline).

Operators:
    - HashJoin: Build hash table on smaller side, probe with larger side.
      Best for equality joins when one side fits in memory.
    - MergeJoin: Requires both inputs pre-sorted on join key.
      Best for already-sorted inputs or when building index.
    - NestedLoopJoin: O(N*M) fallback. Supports arbitrary predicates.
      Used when no equality predicate or tiny inputs.
    - SemiJoin / AntiJoin: EXISTS / NOT EXISTS subquery optimization.

Each Row is a dict[str, Any] — column name → value.
"""

from __future__ import annotations

import json
import os
from collections import defaultdict
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Callable, Generator, Iterator

try:
    import qm_engine as _qm_engine
except Exception:
    _qm_engine = None


_RUST_JOIN_ENGINE = None


# ── Types ───────────────────────────────────────────────────────────

Row = dict[str, Any]
Predicate = Callable[[Row], bool]


class JoinType(IntEnum):
    INNER = 0
    LEFT = 1
    RIGHT = 2
    FULL = 3
    CROSS = 4
    SEMI = 5       # Return left row if ANY match on right
    ANTI = 6       # Return left row if NO match on right


# ── Base Operator ───────────────────────────────────────────────────

class Operator:
    """Abstract volcano-style operator."""

    def open(self) -> None:
        pass

    def next(self) -> Row | None:
        raise NotImplementedError

    def close(self) -> None:
        pass

    def __iter__(self) -> Iterator[Row]:
        self.open()
        try:
            while True:
                row = self.next()
                if row is None:
                    break
                yield row
        finally:
            self.close()


class ScanOperator(Operator):
    """Scan from a list of rows (table data)."""

    def __init__(self, rows: list[Row], alias: str = "") -> None:
        self._rows = rows
        self._alias = alias
        self._idx = 0

    def open(self) -> None:
        self._idx = 0

    def next(self) -> Row | None:
        if self._idx >= len(self._rows):
            return None
        row = self._rows[self._idx]
        self._idx += 1
        if self._alias:
            return {f"{self._alias}.{k}": v for k, v in row.items()}
        return dict(row)

    def close(self) -> None:
        self._idx = 0


class FilterOperator(Operator):
    """Filter rows by predicate."""

    def __init__(self, child: Operator, predicate: Predicate) -> None:
        self._child = child
        self._pred = predicate

    def open(self) -> None:
        self._child.open()

    def next(self) -> Row | None:
        while True:
            row = self._child.next()
            if row is None:
                return None
            if self._pred(row):
                return row

    def close(self) -> None:
        self._child.close()


class ProjectOperator(Operator):
    """Project (select) specific columns."""

    def __init__(self, child: Operator, columns: list[str]) -> None:
        self._child = child
        self._columns = columns

    def open(self) -> None:
        self._child.open()

    def next(self) -> Row | None:
        row = self._child.next()
        if row is None:
            return None
        return {c: row.get(c) for c in self._columns}

    def close(self) -> None:
        self._child.close()


class LimitOperator(Operator):
    """Limit output to N rows."""

    def __init__(self, child: Operator, limit: int, offset: int = 0) -> None:
        self._child = child
        self._limit = limit
        self._offset = offset
        self._count = 0
        self._skipped = 0

    def open(self) -> None:
        self._child.open()
        self._count = 0
        self._skipped = 0

    def next(self) -> Row | None:
        while self._skipped < self._offset:
            row = self._child.next()
            if row is None:
                return None
            self._skipped += 1
        if self._count >= self._limit:
            return None
        row = self._child.next()
        if row is None:
            return None
        self._count += 1
        return row

    def close(self) -> None:
        self._child.close()


# ── Hash Join ───────────────────────────────────────────────────────

class HashJoin(Operator):
    """Hash Join — build on right, probe with left.

    Supports INNER, LEFT, RIGHT, FULL, SEMI, ANTI.

    Algorithm:
        1. Build phase: scan right child, hash on join key → hash table
        2. Probe phase: scan left child, look up each key in hash table
        3. For LEFT/FULL: emit unmatched left rows with NULL right side
        4. For RIGHT/FULL: after probe, emit unmatched right rows
    """

    def __init__(
        self,
        left: Operator,
        right: Operator,
        left_key: str,
        right_key: str,
        join_type: JoinType = JoinType.INNER,
    ) -> None:
        self._left = left
        self._right = right
        self._left_key = left_key
        self._right_key = right_key
        self._join_type = join_type

        # Build-side state
        self._ht: dict[Any, list[tuple[int, Row]]] = {}
        self._right_matched: set[int] = set()
        self._all_right_rows: list[Row] = []
        self._right_null_template: dict[str, Any] = {}
        self._left_null_template: dict[str, Any] = {}

        # Probe-side state
        self._current_left: Row | None = None
        self._match_iter: Iterator[tuple[int, Row]] | None = None
        self._left_matched = False
        self._probe_done = False
        self._drain_idx = 0
        self._using_rust_fastpath = False
        self._rust_rows: list[Row] = []
        self._rust_idx = 0

    def open(self) -> None:
        self._using_rust_fastpath = False
        self._rust_rows.clear()
        self._rust_idx = 0

        # Fast path: delegate INNER hash join compute to Rust kernel when available.
        if self._join_type == JoinType.INNER and self._try_open_rust_fastpath():
            return

        self._right.open()
        self._left.open()
        self._ht.clear()
        self._right_matched.clear()
        self._all_right_rows.clear()
        self._right_null_template = {}
        self._left_null_template = {}
        self._probe_done = False
        self._drain_idx = 0

        # Build phase: hash the right side
        ridx = 0
        while True:
            rrow = self._right.next()
            if rrow is None:
                break
            key = rrow.get(self._right_key)
            self._ht.setdefault(key, []).append((ridx, rrow))
            self._all_right_rows.append(rrow)
            if not self._right_null_template:
                self._right_null_template = {k: None for k in rrow}
            ridx += 1
        self._right.close()

        self._current_left = None
        self._match_iter = None

    def next(self) -> Row | None:
        if self._using_rust_fastpath:
            if self._rust_idx >= len(self._rust_rows):
                return None
            row = self._rust_rows[self._rust_idx]
            self._rust_idx += 1
            return row

        while True:
            # If we have pending matches from hash table
            if self._match_iter is not None:
                try:
                    ridx, rrow = next(self._match_iter)
                    self._left_matched = True
                    self._right_matched.add(ridx)
                    if self._join_type == JoinType.SEMI:
                        self._match_iter = None  # One match is sufficient
                        return dict(self._current_left)
                    if self._join_type == JoinType.ANTI:
                        self._match_iter = None
                        self._left_matched = True
                        continue  # Skip — matched means NOT anti
                    return {**self._current_left, **rrow}
                except StopIteration:
                    self._match_iter = None
                    # Left row had no matches → emit for LEFT/FULL
                    if not self._left_matched:
                        if self._join_type == JoinType.ANTI:
                            return dict(self._current_left)
                        if self._join_type in (JoinType.LEFT, JoinType.FULL):
                            return {**self._current_left, **self._right_null_template}
                    continue

            if not self._probe_done:
                # Probe phase: get next left row
                lrow = self._left.next()
                if lrow is None:
                    self._probe_done = True
                    self._left.close()
                    if self._join_type in (JoinType.RIGHT, JoinType.FULL):
                        self._drain_idx = 0
                        continue
                    return None

                self._current_left = lrow
                if not self._left_null_template:
                    self._left_null_template = {k: None for k in lrow}
                self._left_matched = False
                lkey = lrow.get(self._left_key)

                if self._join_type == JoinType.CROSS:
                    self._match_iter = iter(enumerate(self._all_right_rows))
                else:
                    matches = self._ht.get(lkey, [])
                    self._match_iter = iter(matches)
                continue

            # Drain unmatched right rows (for RIGHT/FULL)
            if self._join_type in (JoinType.RIGHT, JoinType.FULL):
                while self._drain_idx < len(self._all_right_rows):
                    rrow = self._all_right_rows[self._drain_idx]
                    ridx = self._drain_idx
                    self._drain_idx += 1
                    if ridx not in self._right_matched:
                        null_left = self._left_null_template
                        return {**null_left, **rrow}
            return None

    def close(self) -> None:
        self._ht.clear()
        self._all_right_rows.clear()
        self._right_matched.clear()
        self._using_rust_fastpath = False
        self._rust_rows.clear()
        self._rust_idx = 0

    def _try_open_rust_fastpath(self) -> bool:
        if os.environ.get("QM_RUST_HASH_JOIN", "1") != "1":
            return False
        if _qm_engine is None or not hasattr(_qm_engine, "HubEngine"):
            return False

        left_rows = self._materialize(self._left)
        right_rows = self._materialize(self._right)
        if not left_rows or not right_rows:
            self._using_rust_fastpath = True
            self._rust_rows = []
            self._rust_idx = 0
            return True

        try:
            rust = _get_rust_join_engine()
            result_json = rust.execute_hash_join_bytes(
                json.dumps(left_rows).encode("utf-8"),
                json.dumps(right_rows).encode("utf-8"),
                self._left_key,
                self._right_key,
            )
            result = json.loads(result_json)
            cols = result.get("columns", [])
            rust_rows = result.get("rows", [])

            left_cols = set(left_rows[0].keys())
            out: list[Row] = []
            for vals in rust_rows:
                row = {cols[i]: vals[i] for i in range(min(len(cols), len(vals)))}
                # Keep Python merge semantics: right side overwrites duplicate keys.
                for k in list(row.keys()):
                    if k.startswith("probe_"):
                        base = k[6:]
                        if base in left_cols:
                            row[base] = row[k]
                        del row[k]
                out.append(row)

            self._using_rust_fastpath = True
            self._rust_rows = out
            self._rust_idx = 0
            return True
        except Exception:
            return False

    @staticmethod
    def _materialize(op: Operator) -> list[Row]:
        rows: list[Row] = []
        op.open()
        try:
            while True:
                row = op.next()
                if row is None:
                    break
                rows.append(dict(row))
        finally:
            op.close()
        return rows


def _get_rust_join_engine():
    global _RUST_JOIN_ENGINE
    if _RUST_JOIN_ENGINE is None:
        _RUST_JOIN_ENGINE = _qm_engine.HubEngine("/tmp/qm_data")
    return _RUST_JOIN_ENGINE


# ── Merge Join ──────────────────────────────────────────────────────

class MergeJoin(Operator):
    """Sort-Merge Join — inputs MUST be pre-sorted on join keys.

    Optimal for already-indexed/sorted data. O(N+M) in sorted case.
    Supports INNER, LEFT, RIGHT, FULL.
    """

    def __init__(
        self,
        left: Operator,
        right: Operator,
        left_key: str,
        right_key: str,
        join_type: JoinType = JoinType.INNER,
    ) -> None:
        self._left = left
        self._right = right
        self._left_key = left_key
        self._right_key = right_key
        self._join_type = join_type
        self._buffer: list[Row] = []

    def open(self) -> None:
        self._left.open()
        self._right.open()
        self._buffer.clear()
        self._left_row: Row | None = self._left.next()
        self._right_row: Row | None = self._right.next()
        self._right_group: list[Row] = []
        self._group_key: Any = None
        self._group_idx = 0

    def next(self) -> Row | None:
        while True:
            # Drain buffered group matches
            if self._group_idx < len(self._right_group):
                merged = {**self._left_row, **self._right_group[self._group_idx]}
                self._group_idx += 1
                if self._group_idx >= len(self._right_group):
                    self._left_row = self._left.next()
                return merged

            if self._left_row is None and self._right_row is None:
                return None

            if self._left_row is None:
                if self._join_type in (JoinType.RIGHT, JoinType.FULL):
                    row = self._right_row
                    self._right_row = self._right.next()
                    return row
                return None

            if self._right_row is None:
                if self._join_type in (JoinType.LEFT, JoinType.FULL):
                    row = self._left_row
                    self._left_row = self._left.next()
                    return row
                return None

            lk = self._left_row.get(self._left_key)
            rk = self._right_row.get(self._right_key)

            if lk == rk:
                # Collect all right rows with the same key
                self._right_group = [self._right_row]
                self._right_row = self._right.next()
                while self._right_row is not None and self._right_row.get(self._right_key) == lk:
                    self._right_group.append(self._right_row)
                    self._right_row = self._right.next()
                self._group_idx = 0
                continue

            if lk < rk:
                if self._join_type in (JoinType.LEFT, JoinType.FULL):
                    row = dict(self._left_row)
                    self._left_row = self._left.next()
                    return row
                self._left_row = self._left.next()
            else:
                if self._join_type in (JoinType.RIGHT, JoinType.FULL):
                    row = dict(self._right_row)
                    self._right_row = self._right.next()
                    return row
                self._right_row = self._right.next()

    def close(self) -> None:
        self._left.close()
        self._right.close()


# ── Nested Loop Join ────────────────────────────────────────────────

class NestedLoopJoin(Operator):
    """Nested Loop Join — universal fallback for any predicate.

    O(N*M) but supports arbitrary join conditions (not just equality).
    """

    def __init__(
        self,
        left: Operator,
        right: Operator,
        predicate: Predicate | None = None,
        join_type: JoinType = JoinType.INNER,
    ) -> None:
        self._left = left
        self._right = right
        self._pred = predicate or (lambda _: True)
        self._join_type = join_type
        self._right_rows: list[Row] = []

    def open(self) -> None:
        self._left.open()
        self._right.open()
        # Materialize right side
        self._right_rows.clear()
        while True:
            rrow = self._right.next()
            if rrow is None:
                break
            self._right_rows.append(rrow)
        self._right.close()

        self._left_row: Row | None = self._left.next()
        self._ridx = 0
        self._left_matched = False

    def next(self) -> Row | None:
        while self._left_row is not None:
            while self._ridx < len(self._right_rows):
                rrow = self._right_rows[self._ridx]
                self._ridx += 1
                combined = {**self._left_row, **rrow}
                if self._pred(combined):
                    self._left_matched = True
                    if self._join_type == JoinType.SEMI:
                        # Got a match — advance left, skip remaining right
                        self._ridx = len(self._right_rows)
                        return dict(self._left_row)
                    if self._join_type == JoinType.ANTI:
                        self._left_matched = True
                        continue
                    return combined

            # Finished right side for this left row
            if not self._left_matched:
                if self._join_type == JoinType.ANTI:
                    result = dict(self._left_row)
                    self._left_row = self._left.next()
                    self._ridx = 0
                    self._left_matched = False
                    return result
                if self._join_type in (JoinType.LEFT, JoinType.FULL):
                    result = dict(self._left_row)
                    self._left_row = self._left.next()
                    self._ridx = 0
                    self._left_matched = False
                    return result

            self._left_row = self._left.next()
            self._ridx = 0
            self._left_matched = False

        return None

    def close(self) -> None:
        self._left.close()
        self._right_rows.clear()


# ── Sort Operator ───────────────────────────────────────────────────

class SortOperator(Operator):
    """In-memory sort operator."""

    def __init__(self, child: Operator, sort_keys: list[tuple[str, bool]]) -> None:
        """sort_keys: [(column, ascending), ...]"""
        self._child = child
        self._sort_keys = sort_keys
        self._rows: list[Row] = []
        self._idx = 0

    def open(self) -> None:
        self._child.open()
        self._rows.clear()
        while True:
            row = self._child.next()
            if row is None:
                break
            self._rows.append(row)
        self._child.close()

        # Multi-key sort
        for key, asc in reversed(self._sort_keys):
            self._rows.sort(key=lambda r, k=key: (r.get(k) is None, r.get(k)), reverse=not asc)
        self._idx = 0

    def next(self) -> Row | None:
        if self._idx >= len(self._rows):
            return None
        row = self._rows[self._idx]
        self._idx += 1
        return row

    def close(self) -> None:
        self._rows.clear()
        self._idx = 0


# ── Hash Aggregate ──────────────────────────────────────────────────

class HashAggregateOperator(Operator):
    """Hash-based GROUP BY with aggregate functions."""

    def __init__(
        self,
        child: Operator,
        group_keys: list[str],
        aggregates: list[tuple[str, str, str]],  # (agg_func, input_col, output_alias)
    ) -> None:
        self._child = child
        self._group_keys = group_keys
        self._aggregates = aggregates
        self._groups: dict[tuple, list[Row]] = {}
        self._results: list[Row] = []
        self._idx = 0

    def open(self) -> None:
        self._child.open()
        self._groups.clear()
        self._results.clear()

        # Accumulate rows into groups
        while True:
            row = self._child.next()
            if row is None:
                break
            gkey = tuple(row.get(k) for k in self._group_keys)
            self._groups.setdefault(gkey, []).append(row)
        self._child.close()

        # Compute aggregates for each group
        for gkey, rows in self._groups.items():
            result: Row = {}
            for i, k in enumerate(self._group_keys):
                result[k] = gkey[i]
            for func, col, alias in self._aggregates:
                result[alias] = self._compute_agg(func, col, rows)
            self._results.append(result)
        self._idx = 0

    def next(self) -> Row | None:
        if self._idx >= len(self._results):
            return None
        row = self._results[self._idx]
        self._idx += 1
        return row

    def close(self) -> None:
        self._groups.clear()
        self._results.clear()

    @staticmethod
    def _compute_agg(func: str, col: str, rows: list[Row]) -> Any:
        func = func.upper()
        if func == "COUNT":
            if col == "*":
                return len(rows)
            return sum(1 for r in rows if r.get(col) is not None)
        vals = [r[col] for r in rows if r.get(col) is not None]
        if not vals:
            return None
        if func == "SUM":
            return sum(vals)
        if func == "AVG":
            return sum(vals) / len(vals)
        if func == "MIN":
            return min(vals)
        if func == "MAX":
            return max(vals)
        if func == "COUNT_DISTINCT":
            return len(set(vals))
        return None
