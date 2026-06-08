"""QM Execution — Logical & Physical Plan Nodes.

Logical plan: what to compute (declarative)
Physical plan: how to compute (concrete operators)

The planner transforms logical → physical using cost model + statistics.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any

from qm_core.execution.parser import (
    Predicate, CompoundPredicate, AggregateExpr, SortKey, CompareOp, AggFunc,
)


# ═══════════════════════════════════════════════════════════════════
# Logical Plan Nodes
# ═══════════════════════════════════════════════════════════════════

class LogicalNodeType(Enum):
    TABLE_SCAN = "table_scan"
    FILTER = "filter"
    PROJECT = "project"
    SORT = "sort"
    LIMIT = "limit"
    GROUP_BY = "group_by"
    JOIN = "join"
    LEXICAL_SEARCH = "lexical_search"
    VECTOR_SEARCH = "vector_search"
    HYBRID_SEARCH = "hybrid_search"
    INSERT = "insert"
    UPDATE = "update"
    DELETE = "delete"
    TOP_K = "top_k"
    DISTINCT = "distinct"


@dataclass
class LogicalNode:
    """Base logical plan node."""
    node_type: LogicalNodeType
    children: list[LogicalNode] = field(default_factory=list)
    estimated_rows: int = 0
    estimated_cost: float = 0.0

    def explain(self, indent: int = 0) -> str:
        prefix = "  " * indent
        lines = [f"{prefix}{self.node_type.value} (rows≈{self.estimated_rows}, cost≈{self.estimated_cost:.1f})"]
        for child in self.children:
            lines.append(child.explain(indent + 1))
        return "\n".join(lines)


@dataclass
class LogicalScan(LogicalNode):
    """Full table scan."""
    table: str = ""

    def __post_init__(self):
        self.node_type = LogicalNodeType.TABLE_SCAN


@dataclass
class LogicalFilter(LogicalNode):
    """Filter rows by predicates."""
    predicates: list[Predicate | CompoundPredicate] = field(default_factory=list)

    def __post_init__(self):
        self.node_type = LogicalNodeType.FILTER


@dataclass
class LogicalProject(LogicalNode):
    """Select specific columns."""
    columns: list[str] = field(default_factory=list)

    def __post_init__(self):
        self.node_type = LogicalNodeType.PROJECT


@dataclass
class LogicalSort(LogicalNode):
    """Sort by columns."""
    keys: list[SortKey] = field(default_factory=list)

    def __post_init__(self):
        self.node_type = LogicalNodeType.SORT


@dataclass
class LogicalLimit(LogicalNode):
    """Limit + offset."""
    limit: int = 0
    offset: int = 0

    def __post_init__(self):
        self.node_type = LogicalNodeType.LIMIT


@dataclass
class LogicalGroupBy(LogicalNode):
    """Group-by + aggregation."""
    group_columns: list[str] = field(default_factory=list)
    aggregates: list[AggregateExpr] = field(default_factory=list)

    def __post_init__(self):
        self.node_type = LogicalNodeType.GROUP_BY


@dataclass
class LogicalJoin(LogicalNode):
    """Join two relations."""
    join_type: str = "inner"  # inner, left, right, full
    join_keys: list[tuple[str, str]] = field(default_factory=list)

    def __post_init__(self):
        self.node_type = LogicalNodeType.JOIN


@dataclass
class LogicalLexicalSearch(LogicalNode):
    """Full-text search."""
    table: str = ""
    query: str = ""
    fields: list[str] | None = None
    top_k: int = 10

    def __post_init__(self):
        self.node_type = LogicalNodeType.LEXICAL_SEARCH


@dataclass
class LogicalVectorSearch(LogicalNode):
    """Vector similarity search."""
    table: str = ""
    vector: list[float] | None = None
    top_k: int = 10
    metric: str = "cosine"

    def __post_init__(self):
        self.node_type = LogicalNodeType.VECTOR_SEARCH


@dataclass
class LogicalHybridSearch(LogicalNode):
    """Combined lexical + vector search."""
    table: str = ""
    query: str = ""
    vector: list[float] | None = None
    top_k: int = 10
    alpha: float = 0.5

    def __post_init__(self):
        self.node_type = LogicalNodeType.HYBRID_SEARCH


# ═══════════════════════════════════════════════════════════════════
# Physical Plan Nodes
# ═══════════════════════════════════════════════════════════════════

class PhysicalNodeType(Enum):
    SEQ_SCAN = "seq_scan"
    INDEX_SCAN = "index_scan"
    BITMAP_INDEX_SCAN = "bitmap_index_scan"
    BITMAP_HEAP_SCAN = "bitmap_heap_scan"
    FILTER = "filter"
    PROJECT = "project"
    SORT = "sort"
    TOP_K_SORT = "top_k_sort"
    LIMIT = "limit"
    HASH_AGGREGATE = "hash_aggregate"
    SORT_AGGREGATE = "sort_aggregate"
    VECTORIZED_AGGREGATE = "vectorized_aggregate"
    HASH_JOIN = "hash_join"
    MERGE_JOIN = "merge_join"
    NESTED_LOOP_JOIN = "nested_loop_join"
    # Search
    INVERTED_INDEX_SCAN = "inverted_index_scan"  # BM25/WAND
    BMW_SCAN = "bmw_scan"  # Block-Max WAND
    HNSW_SCAN = "hnsw_scan"
    PQ_SCAN = "pq_scan"  # Product quantization coarse
    HYBRID_FUSION = "hybrid_fusion"
    RERANK = "rerank"
    # Late materialization
    LATE_MATERIALIZE = "late_materialize"
    FETCH_ROWS = "fetch_rows"
    # Batch / vectorized
    VECTORIZED_FILTER = "vectorized_filter"
    VECTORIZED_PROJECT = "vectorized_project"


@dataclass
class PhysicalNode:
    """Base physical plan node."""
    node_type: PhysicalNodeType = PhysicalNodeType.SEQ_SCAN
    children: list[PhysicalNode] = field(default_factory=list)
    estimated_rows: int = 0
    estimated_cost: float = 0.0
    properties: dict[str, Any] = field(default_factory=dict)

    def explain(self, indent: int = 0) -> str:
        prefix = "  " * indent
        props_str = ", ".join(f"{k}={v}" for k, v in self.properties.items())
        lines = [
            f"{prefix}{self.node_type.value} (rows≈{self.estimated_rows}, cost≈{self.estimated_cost:.1f}"
            + (f", {props_str}" if props_str else "")
            + ")"
        ]
        for child in self.children:
            lines.append(child.explain(indent + 1))
        return "\n".join(lines)


@dataclass
class PhysicalSeqScan(PhysicalNode):
    table: str = ""
    def __post_init__(self):
        self.node_type = PhysicalNodeType.SEQ_SCAN
        self.properties["table"] = self.table


@dataclass
class PhysicalIndexScan(PhysicalNode):
    table: str = ""
    index_name: str = ""
    predicates: list[Predicate] = field(default_factory=list)
    scan_direction: str = "forward"
    def __post_init__(self):
        self.node_type = PhysicalNodeType.INDEX_SCAN
        self.properties["table"] = self.table
        self.properties["index"] = self.index_name


@dataclass
class PhysicalBitmapScan(PhysicalNode):
    table: str = ""
    index_name: str = ""
    predicates: list[Predicate] = field(default_factory=list)
    def __post_init__(self):
        self.node_type = PhysicalNodeType.BITMAP_INDEX_SCAN
        self.properties["table"] = self.table
        self.properties["index"] = self.index_name


@dataclass
class PhysicalFilter(PhysicalNode):
    predicates: list[Predicate | CompoundPredicate] = field(default_factory=list)
    vectorized: bool = False
    def __post_init__(self):
        self.node_type = PhysicalNodeType.VECTORIZED_FILTER if self.vectorized else PhysicalNodeType.FILTER


@dataclass
class PhysicalProject(PhysicalNode):
    columns: list[str] = field(default_factory=list)
    def __post_init__(self):
        self.node_type = PhysicalNodeType.PROJECT


@dataclass
class PhysicalSort(PhysicalNode):
    keys: list[SortKey] = field(default_factory=list)
    top_k: int = 0
    def __post_init__(self):
        self.node_type = PhysicalNodeType.TOP_K_SORT if self.top_k > 0 else PhysicalNodeType.SORT
        if self.top_k:
            self.properties["top_k"] = self.top_k


@dataclass
class PhysicalLimit(PhysicalNode):
    limit: int = 0
    offset: int = 0
    def __post_init__(self):
        self.node_type = PhysicalNodeType.LIMIT
        self.properties["limit"] = self.limit
        self.properties["offset"] = self.offset


@dataclass
class PhysicalHashAggregate(PhysicalNode):
    group_columns: list[str] = field(default_factory=list)
    aggregates: list[AggregateExpr] = field(default_factory=list)
    vectorized: bool = False
    def __post_init__(self):
        self.node_type = (PhysicalNodeType.VECTORIZED_AGGREGATE
                          if self.vectorized else PhysicalNodeType.HASH_AGGREGATE)


@dataclass
class PhysicalBMWScan(PhysicalNode):
    """Block-Max WAND search operator."""
    table: str = ""
    query_terms: list[str] = field(default_factory=list)
    top_k: int = 10
    def __post_init__(self):
        self.node_type = PhysicalNodeType.BMW_SCAN
        self.properties["top_k"] = self.top_k


@dataclass
class PhysicalHNSWScan(PhysicalNode):
    """HNSW vector search operator."""
    table: str = ""
    vector: list[float] | None = None
    top_k: int = 10
    ef_search: int = 50
    def __post_init__(self):
        self.node_type = PhysicalNodeType.HNSW_SCAN
        self.properties["top_k"] = self.top_k
        self.properties["ef_search"] = self.ef_search


@dataclass
class PhysicalHybridFusion(PhysicalNode):
    """Combines lexical + vector results."""
    alpha: float = 0.5
    fusion_method: str = "rrf"
    top_k: int = 10
    def __post_init__(self):
        self.node_type = PhysicalNodeType.HYBRID_FUSION
        self.properties["alpha"] = self.alpha
        self.properties["method"] = self.fusion_method


@dataclass
class PhysicalLateMaterialize(PhysicalNode):
    """Late materialization: fetch full rows only for final candidates."""
    table: str = ""
    columns: list[str] = field(default_factory=list)
    def __post_init__(self):
        self.node_type = PhysicalNodeType.LATE_MATERIALIZE
