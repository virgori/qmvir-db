"""QM Vector Platform — Metadata filtering for vector search.

Supports:
  - Pre-filter (filter before ANN)
  - Post-filter (filter after ANN)
  - Equality, range, set membership, exists
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from typing import Any


class FilterOp(Enum):
    """Filter operators."""

    EQ = "eq"
    NE = "ne"
    GT = "gt"
    GTE = "gte"
    LT = "lt"
    LTE = "lte"
    IN = "in"
    NOT_IN = "not_in"
    EXISTS = "exists"
    CONTAINS = "contains"


@dataclass
class MetadataFilter:
    """A single metadata filter condition."""

    field: str
    op: FilterOp
    value: Any


class MetadataFilterEngine:
    """Evaluates metadata filters against vector entries."""

    def matches(self, metadata: dict[str, Any], filters: list[MetadataFilter]) -> bool:
        """Check if metadata satisfies all filters."""
        for f in filters:
            if not self._eval_filter(metadata, f):
                return False
        return True

    def build_filter_fn(self, filters: list[MetadataFilter | dict]):
        """Build a callable filter function for ANN search."""
        resolved = []
        for f in filters:
            if isinstance(f, dict):
                resolved.append(MetadataFilter(
                    field=f["field"],
                    op=f["op"] if isinstance(f["op"], FilterOp) else FilterOp(f["op"]),
                    value=f["value"],
                ))
            else:
                resolved.append(f)
        def fn(metadata: dict[str, Any]) -> bool:
            return self.matches(metadata, resolved)
        return fn

    def _eval_filter(self, metadata: dict[str, Any], f: MetadataFilter) -> bool:
        val = metadata.get(f.field)

        if f.op == FilterOp.EXISTS:
            return (f.field in metadata) == bool(f.value)

        if val is None:
            return False

        if f.op == FilterOp.EQ:
            return val == f.value
        elif f.op == FilterOp.NE:
            return val != f.value
        elif f.op == FilterOp.GT:
            return val > f.value
        elif f.op == FilterOp.GTE:
            return val >= f.value
        elif f.op == FilterOp.LT:
            return val < f.value
        elif f.op == FilterOp.LTE:
            return val <= f.value
        elif f.op == FilterOp.IN:
            return val in f.value
        elif f.op == FilterOp.NOT_IN:
            return val not in f.value
        elif f.op == FilterOp.CONTAINS:
            if isinstance(val, (list, set)):
                return f.value in val
            if isinstance(val, str):
                return f.value in val
            return False

        return False

    @staticmethod
    def parse_filters(raw: dict[str, Any]) -> list[MetadataFilter]:
        """Parse a dict of filters into MetadataFilter objects.

        Supports: {"field": value} for equality,
        or {"field_gte": value} for operators.
        """
        filters: list[MetadataFilter] = []
        for key, value in raw.items():
            for suffix, op in [
                ("_gte", FilterOp.GTE),
                ("_gt", FilterOp.GT),
                ("_lte", FilterOp.LTE),
                ("_lt", FilterOp.LT),
                ("_ne", FilterOp.NE),
                ("_in", FilterOp.IN),
                ("_nin", FilterOp.NOT_IN),
            ]:
                if key.endswith(suffix):
                    field = key[: -len(suffix)]
                    filters.append(MetadataFilter(field=field, op=op, value=value))
                    break
            else:
                filters.append(MetadataFilter(field=key, op=FilterOp.EQ, value=value))
        return filters
