"""QM Execution — Vectorized Engine.

Processes data in batches (columns of 256–1024 rows) instead of row-at-a-time.
Uses numpy for tight inner loops when available (graceful fallback to lists).

Core abstractions:
    - ColumnBatch: a batch of typed column vectors
    - VecOp: vectorized operators (filter, project, aggregate, score, sort)
    - ExecutionContext: runtime state for plan execution

Anti-pattern avoidance:
    - NEVER copy row-by-row
    - ALWAYS operate on the full batch via numpy/memoryview
    - Fuse adjacent filter+project into single pass when possible
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from typing import Any, Callable, Sequence

try:
    import qm_engine as _qm_engine
    _RUST_VEC_EXECUTOR = _qm_engine.VectorExecutor()
except Exception:
    _qm_engine = None
    _RUST_VEC_EXECUTOR = None

try:
    import numpy as np  # type: ignore
    HAS_NUMPY = True
except ImportError:
    np = None  # type: ignore
    HAS_NUMPY = False


DEFAULT_BATCH_SIZE = 1024


@dataclass
class ColumnBatch:
    """A batch of column vectors — the fundamental data unit for vectorized execution.

    Each column is either a numpy array (fast path) or a Python list (fallback).
    A validity bitmask tracks NULLs.
    """
    columns: dict[str, Any]          # name → ndarray | list
    size: int = 0
    validity: dict[str, Any] = field(default_factory=dict)  # name → bool mask

    @classmethod
    def from_rows(cls, rows: list[dict[str, Any]], columns: list[str] | None = None) -> "ColumnBatch":
        """Create a ColumnBatch from row-oriented dicts."""
        if not rows:
            return cls(columns={}, size=0)
        cols = columns or list(rows[0].keys())
        n = len(rows)
        result: dict[str, Any] = {}
        validity: dict[str, Any] = {}
        for c in cols:
            values = [r.get(c) for r in rows]
            nulls = [v is None for v in values]
            if HAS_NUMPY:
                # Try numeric
                if all(isinstance(v, (int, float)) or v is None for v in values):
                    arr = np.array([v if v is not None else 0.0 for v in values], dtype=np.float64)
                    mask = np.array(nulls, dtype=np.bool_)
                else:
                    arr = np.array(values, dtype=object)
                    mask = np.array(nulls, dtype=np.bool_)
                result[c] = arr
                validity[c] = mask
            else:
                result[c] = values
                validity[c] = nulls
        return cls(columns=result, size=n, validity=validity)

    def to_rows(self) -> list[dict[str, Any]]:
        """Convert back to list of dicts."""
        rows = []
        cols = list(self.columns.keys())
        for i in range(self.size):
            row = {}
            for c in cols:
                val = self.columns[c][i]
                if HAS_NUMPY and hasattr(val, 'item'):
                    val = val.item()
                null_mask = self.validity.get(c)
                if null_mask is not None:
                    is_null = null_mask[i]
                    if HAS_NUMPY and hasattr(is_null, 'item'):
                        is_null = is_null.item()
                    if is_null:
                        val = None
                row[c] = val
            rows.append(row)
        return rows

    def select(self, mask: Any) -> "ColumnBatch":
        """Filter rows by boolean mask, returning a new batch."""
        new_cols: dict[str, Any] = {}
        new_val: dict[str, Any] = {}
        for c, col in self.columns.items():
            if HAS_NUMPY and isinstance(col, np.ndarray):
                new_cols[c] = col[mask]
                if c in self.validity:
                    new_val[c] = self.validity[c][mask]
            else:
                indices = [i for i, m in enumerate(mask) if m]
                new_cols[c] = [col[i] for i in indices]
                if c in self.validity:
                    new_val[c] = [self.validity[c][i] for i in indices]
        new_size = int(np.sum(mask)) if HAS_NUMPY and isinstance(mask, np.ndarray) else sum(mask)
        return ColumnBatch(columns=new_cols, size=new_size, validity=new_val)

    def project(self, cols: list[str]) -> "ColumnBatch":
        """Project to a subset of columns."""
        return ColumnBatch(
            columns={c: self.columns[c] for c in cols if c in self.columns},
            size=self.size,
            validity={c: self.validity[c] for c in cols if c in self.validity},
        )

    def append_column(self, name: str, data: Any, null_mask: Any = None) -> None:
        self.columns[name] = data
        if null_mask is not None:
            self.validity[name] = null_mask

    def slice(self, offset: int, length: int) -> "ColumnBatch":
        """Slice a batch."""
        end = min(offset + length, self.size)
        new_cols: dict[str, Any] = {}
        new_val: dict[str, Any] = {}
        for c, col in self.columns.items():
            new_cols[c] = col[offset:end]
            if c in self.validity:
                new_val[c] = self.validity[c][offset:end]
        return ColumnBatch(columns=new_cols, size=end - offset, validity=new_val)


# ── Vectorized Operators ────────────────────────────────────────────


class VecFilter:
    """Vectorized filter: apply predicate on column vector, returns boolean mask."""

    @staticmethod
    def compare(col: Any, op: str, value: Any) -> Any:
        """Compare column vector against scalar value, return bool mask."""
        if HAS_NUMPY and isinstance(col, np.ndarray):
            if op == "eq":
                return col == value
            elif op == "neq":
                return col != value
            elif op == "gt":
                return col > value
            elif op == "gte":
                return col >= value
            elif op == "lt":
                return col < value
            elif op == "lte":
                return col <= value
            elif op == "in":
                return np.isin(col, value)
            elif op == "not_in":
                return ~np.isin(col, value)
            elif op == "between":
                lo, hi = value
                return (col >= lo) & (col <= hi)
            elif op == "is_null":
                return np.full(len(col), False, dtype=np.bool_)  # handled via validity
            else:
                return np.ones(len(col), dtype=np.bool_)
        else:
            # Fallback: list-based
            n = len(col)
            ops_map = {
                "eq": lambda a, b: a == b,
                "neq": lambda a, b: a != b,
                "gt": lambda a, b: a > b,
                "gte": lambda a, b: a >= b,
                "lt": lambda a, b: a < b,
                "lte": lambda a, b: a <= b,
                "in": lambda a, b: a in b,
                "not_in": lambda a, b: a not in b,
            }
            fn = ops_map.get(op, lambda a, b: True)
            return [fn(col[i], value) for i in range(n)]

    @staticmethod
    def combine_and(masks: list[Any]) -> Any:
        """Combine masks with AND."""
        if not masks:
            return True
        result = masks[0]
        for m in masks[1:]:
            if HAS_NUMPY and isinstance(result, np.ndarray):
                result = result & m
            else:
                result = [a and b for a, b in zip(result, m)]
        return result

    @staticmethod
    def combine_or(masks: list[Any]) -> Any:
        """Combine masks with OR."""
        if not masks:
            return True
        result = masks[0]
        for m in masks[1:]:
            if HAS_NUMPY and isinstance(result, np.ndarray):
                result = result | m
            else:
                result = [a or b for a, b in zip(result, m)]
        return result


class VecAggregate:
    """Vectorized aggregation operators."""

    @staticmethod
    def count(col: Any, validity: Any = None) -> int:
        if validity is not None:
            if HAS_NUMPY and isinstance(validity, np.ndarray):
                return int(np.sum(~validity))
            return sum(1 for v in validity if not v)
        if HAS_NUMPY and isinstance(col, np.ndarray):
            return len(col)
        return len(col)

    @staticmethod
    def sum_agg(col: Any, validity: Any = None) -> float:
        if HAS_NUMPY and isinstance(col, np.ndarray):
            if validity is not None:
                return float(np.sum(col[~validity]))
            return float(np.sum(col))
        values = [col[i] for i in range(len(col)) if validity is None or not validity[i]]
        return sum(v for v in values if isinstance(v, (int, float)))

    @staticmethod
    def avg_agg(col: Any, validity: Any = None) -> float:
        if HAS_NUMPY and isinstance(col, np.ndarray):
            if validity is not None:
                valid = col[~validity]
                return float(np.mean(valid)) if len(valid) > 0 else 0.0
            return float(np.mean(col)) if len(col) > 0 else 0.0
        values = [col[i] for i in range(len(col)) if validity is None or not validity[i]]
        nums = [v for v in values if isinstance(v, (int, float))]
        return sum(nums) / len(nums) if nums else 0.0

    @staticmethod
    def min_agg(col: Any, validity: Any = None) -> Any:
        if HAS_NUMPY and isinstance(col, np.ndarray):
            if validity is not None:
                valid = col[~validity]
                return float(np.min(valid)) if len(valid) > 0 else None
            return float(np.min(col)) if len(col) > 0 else None
        values = [col[i] for i in range(len(col)) if validity is None or not validity[i]]
        return min(values) if values else None

    @staticmethod
    def max_agg(col: Any, validity: Any = None) -> Any:
        if HAS_NUMPY and isinstance(col, np.ndarray):
            if validity is not None:
                valid = col[~validity]
                return float(np.max(valid)) if len(valid) > 0 else None
            return float(np.max(col)) if len(col) > 0 else None
        values = [col[i] for i in range(len(col)) if validity is None or not validity[i]]
        return max(values) if values else None


class VecHashAggregate:
    """Vectorized hash aggregation.

    Groups rows by key columns and computes aggregates in batch.
    """

    def __init__(self, group_cols: list[str], agg_specs: list[tuple[str, str, str]]):
        """
        Args:
            group_cols: columns to group by
            agg_specs: list of (func, input_col, output_alias) — e.g. ("sum", "amount", "total")
        """
        self.group_cols = group_cols
        self.agg_specs = agg_specs

    def execute(self, batch: ColumnBatch) -> ColumnBatch:
        """Execute hash aggregation on a batch."""
        if batch.size == 0:
            return ColumnBatch(columns={}, size=0)

        # Build group keys
        groups: dict[tuple, list[int]] = {}
        for i in range(batch.size):
            key = tuple(
                batch.columns[c][i].item() if HAS_NUMPY and hasattr(batch.columns[c][i], 'item')
                else batch.columns[c][i]
                for c in self.group_cols
            )
            groups.setdefault(key, []).append(i)

        # Compute aggregates per group
        result_rows: list[dict[str, Any]] = []
        for key, indices in groups.items():
            row: dict[str, Any] = {}
            for col_name, k in zip(self.group_cols, key):
                row[col_name] = k

            for func, input_col, alias in self.agg_specs:
                if HAS_NUMPY and isinstance(batch.columns.get(input_col), np.ndarray):
                    vals = batch.columns[input_col][np.array(indices)]
                else:
                    col = batch.columns.get(input_col, [])
                    vals = [col[i] for i in indices]

                if func == "count":
                    row[alias] = len(indices)
                elif func == "sum":
                    row[alias] = float(np.sum(vals)) if HAS_NUMPY and isinstance(vals, np.ndarray) else sum(v for v in vals if isinstance(v, (int, float)))
                elif func == "avg":
                    row[alias] = float(np.mean(vals)) if HAS_NUMPY and isinstance(vals, np.ndarray) else (sum(v for v in vals if isinstance(v, (int, float))) / len(vals) if vals else 0.0)
                elif func == "min":
                    row[alias] = float(np.min(vals)) if HAS_NUMPY and isinstance(vals, np.ndarray) else min((v for v in vals if v is not None), default=None)
                elif func == "max":
                    row[alias] = float(np.max(vals)) if HAS_NUMPY and isinstance(vals, np.ndarray) else max((v for v in vals if v is not None), default=None)
                elif func == "count_distinct":
                    if HAS_NUMPY and isinstance(vals, np.ndarray):
                        row[alias] = len(np.unique(vals))
                    else:
                        row[alias] = len(set(vals))
                else:
                    row[alias] = None

            result_rows.append(row)

        return ColumnBatch.from_rows(result_rows)


class VecSort:
    """Vectorized sort with optional top-k heap selection."""

    @staticmethod
    def sort(batch: ColumnBatch, keys: list[tuple[str, bool]]) -> ColumnBatch:
        """Sort batch by keys. keys = [(col_name, ascending), ...]"""
        if batch.size <= 1:
            return batch

        if HAS_NUMPY:
            # Build sort key
            # numpy lexsort takes keys in reverse order
            sort_keys = []
            for col_name, ascending in reversed(keys):
                col = batch.columns.get(col_name)
                if col is None:
                    continue
                if not ascending:
                    if col.dtype.kind in ('f', 'i', 'u'):
                        col = -col
                sort_keys.append(col)
            if sort_keys:
                order = np.lexsort(sort_keys)
                new_cols = {c: batch.columns[c][order] for c in batch.columns}
                new_val = {c: batch.validity[c][order] for c in batch.validity}
                return ColumnBatch(columns=new_cols, size=batch.size, validity=new_val)

        # Fallback: Python sort
        indices = list(range(batch.size))
        for col_name, ascending in reversed(keys):
            col = batch.columns.get(col_name)
            if col is None:
                continue
            indices.sort(key=lambda i: col[i], reverse=not ascending)
        new_cols = {}
        new_val = {}
        for c, col in batch.columns.items():
            if HAS_NUMPY and isinstance(col, np.ndarray):
                new_cols[c] = col[np.array(indices)]
            else:
                new_cols[c] = [col[i] for i in indices]
            if c in batch.validity:
                new_val[c] = batch.validity[c][np.array(indices)] if HAS_NUMPY else [batch.validity[c][i] for i in indices]
        return ColumnBatch(columns=new_cols, size=batch.size, validity=new_val)

    @staticmethod
    def top_k(batch: ColumnBatch, key_col: str, k: int, ascending: bool = True) -> ColumnBatch:
        """Select top-k rows using partial sort (O(n + k log k))."""
        if batch.size <= k:
            return VecSort.sort(batch, [(key_col, ascending)])

        col = batch.columns.get(key_col)
        if col is None:
            return batch.slice(0, k)

        if HAS_NUMPY and isinstance(col, np.ndarray):
            if not ascending:
                col = -col
            # argpartition: O(n), then sort only the top k
            indices = np.argpartition(col, k)[:k]
            indices = indices[np.argsort(col[indices])]
            new_cols = {c: batch.columns[c][indices] for c in batch.columns}
            new_val = {c: batch.validity[c][indices] for c in batch.validity}
            return ColumnBatch(columns=new_cols, size=k, validity=new_val)
        else:
            sorted_batch = VecSort.sort(batch, [(key_col, ascending)])
            return sorted_batch.slice(0, k)


class VecScorer:
    """Vectorized BM25 scoring over a ColumnBatch."""

    def __init__(self, k1: float = 1.2, b: float = 0.75) -> None:
        self.k1 = k1
        self.b = b

    def score_batch(self, term_freqs: Any, doc_lens: Any, avg_dl: float,
                    n_docs: int, doc_freq: int) -> Any:
        """Score a batch of documents.

        Args:
            term_freqs: ndarray of tf values for the term
            doc_lens: ndarray of document lengths
            avg_dl: average document length
            n_docs: total documents
            doc_freq: document frequency of the term
        """
        idf = math.log((n_docs - doc_freq + 0.5) / (doc_freq + 0.5) + 1.0)

        if HAS_NUMPY and isinstance(term_freqs, np.ndarray):
            tf = term_freqs.astype(np.float64)
            dl = doc_lens.astype(np.float64)
            norm = 1.0 - self.b + self.b * (dl / avg_dl)
            numerator = tf * (self.k1 + 1.0)
            denominator = tf + self.k1 * norm
            return idf * (numerator / denominator)
        else:
            scores = []
            for i in range(len(term_freqs)):
                tf = term_freqs[i]
                dl = doc_lens[i]
                norm = 1.0 - self.b + self.b * (dl / avg_dl)
                s = idf * (tf * (self.k1 + 1.0)) / (tf + self.k1 * norm)
                scores.append(s)
            return scores


class VecDistance:
    """Vectorized distance computation for vector search."""

    @staticmethod
    def l2_batch(query: Any, vectors: Any) -> Any:
        """L2 distance from query to each vector in batch."""
        if HAS_NUMPY:
            q = np.asarray(query, dtype=np.float32)
            vecs = np.asarray(vectors, dtype=np.float32)
            diff = vecs - q
            return np.sum(diff * diff, axis=1)
        return [sum((a - b) ** 2 for a, b in zip(query, v)) for v in vectors]

    @staticmethod
    def cosine_batch(query: Any, vectors: Any) -> Any:
        """Cosine distance from query to each vector in batch."""
        if HAS_NUMPY:
            q = np.asarray(query, dtype=np.float32)
            vecs = np.asarray(vectors, dtype=np.float32)
            q_norm = np.linalg.norm(q)
            v_norms = np.linalg.norm(vecs, axis=1)
            dots = vecs @ q
            sims = dots / (q_norm * v_norms + 1e-10)
            return 1.0 - sims
        dists = []
        q_norm = math.sqrt(sum(x * x for x in query))
        for v in vectors:
            dot = sum(a * b for a, b in zip(query, v))
            v_norm = math.sqrt(sum(x * x for x in v))
            sim = dot / (q_norm * v_norm + 1e-10)
            dists.append(1.0 - sim)
        return dists

    @staticmethod
    def inner_product_batch(query: Any, vectors: Any) -> Any:
        """Inner product (negated for min-distance convention)."""
        if HAS_NUMPY:
            q = np.asarray(query, dtype=np.float32)
            vecs = np.asarray(vectors, dtype=np.float32)
            return -(vecs @ q)
        return [-sum(a * b for a, b in zip(query, v)) for v in vectors]


# ── Batch Execution Context ────────────────────────────────────────

@dataclass
class ExecutionContext:
    """Runtime context for vectorized plan execution."""
    batch_size: int = DEFAULT_BATCH_SIZE
    memory_budget_mb: float = 256.0
    enable_vectorized: bool = True
    stats: dict[str, Any] = field(default_factory=dict)

    def track(self, key: str, value: float) -> None:
        self.stats.setdefault(key, 0.0)
        self.stats[key] += value
