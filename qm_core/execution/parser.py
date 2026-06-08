"""QM Execution — Query DSL Parser.

Parses QM's query DSL into an Abstract Syntax Tree (AST).

Query format examples:
    {"action": "find", "table": "users", "where": {"age": {"$gt": 25}}, "limit": 10}
    {"action": "search", "table": "docs", "query": "machine learning", "top_k": 20}
    {"action": "aggregate", "table": "orders", "group_by": ["status"], "metrics": [{"count": "*"}]}
    {"action": "insert", "table": "users", "data": {"name": "Alice", "age": 30}}
    {"action": "vector_search", "table": "embeddings", "vector": [...], "top_k": 10}

Produces typed AST nodes for the planner.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any


# ── AST Node Types ──────────────────────────────────────────────────

class NodeType(Enum):
    # DML
    SCAN = "scan"
    INDEX_SCAN = "index_scan"
    FILTER = "filter"
    PROJECT = "project"
    SORT = "sort"
    LIMIT = "limit"
    OFFSET = "offset"
    # Aggregation
    GROUP_BY = "group_by"
    AGGREGATE = "aggregate"
    HAVING = "having"
    # Join
    NESTED_LOOP_JOIN = "nested_loop_join"
    HASH_JOIN = "hash_join"
    MERGE_JOIN = "merge_join"
    # Search
    LEXICAL_SEARCH = "lexical_search"
    VECTOR_SEARCH = "vector_search"
    HYBRID_SEARCH = "hybrid_search"
    # Mutation
    INSERT = "insert"
    UPDATE = "update"
    DELETE = "delete"
    # Special
    TOP_K = "top_k"
    MATERIALIZE = "materialize"
    BITMAP_FILTER = "bitmap_filter"


class CompareOp(Enum):
    EQ = "eq"
    NEQ = "neq"
    GT = "gt"
    GTE = "gte"
    LT = "lt"
    LTE = "lte"
    IN = "in"
    NOT_IN = "not_in"
    LIKE = "like"
    IS_NULL = "is_null"
    IS_NOT_NULL = "is_not_null"
    BETWEEN = "between"


class LogicOp(Enum):
    AND = "and"
    OR = "or"
    NOT = "not"


class AggFunc(Enum):
    COUNT = "count"
    SUM = "sum"
    AVG = "avg"
    MIN = "min"
    MAX = "max"
    COUNT_DISTINCT = "count_distinct"


# ── AST Nodes ───────────────────────────────────────────────────────

@dataclass
class Predicate:
    """A single comparison predicate."""
    column: str
    op: CompareOp
    value: Any
    negated: bool = False


@dataclass
class CompoundPredicate:
    """AND/OR/NOT of predicates."""
    logic: LogicOp
    children: list[Predicate | CompoundPredicate] = field(default_factory=list)


@dataclass
class AggregateExpr:
    """An aggregate expression (e.g. SUM(amount))."""
    func: AggFunc
    column: str
    alias: str = ""


@dataclass
class SortKey:
    """A sort specification."""
    column: str
    ascending: bool = True


@dataclass
class QueryAST:
    """The parsed query AST."""
    action: str  # find, search, aggregate, insert, update, delete, vector_search
    table: str
    # Selection
    predicates: list[Predicate | CompoundPredicate] = field(default_factory=list)
    # Projection
    columns: list[str] | None = None  # None = all columns
    # Search
    query_text: str = ""
    query_vector: list[float] | None = None
    search_fields: list[str] | None = None
    # Aggregation
    group_by: list[str] = field(default_factory=list)
    aggregates: list[AggregateExpr] = field(default_factory=list)
    having: list[Predicate] = field(default_factory=list)
    # Sort / Limit
    order_by: list[SortKey] = field(default_factory=list)
    limit: int = 0
    offset: int = 0
    top_k: int = 0  # For search/vector queries
    # Mutation data
    data: dict[str, Any] | None = None
    # Hints
    index_hints: list[str] = field(default_factory=list)
    use_cache: bool = True
    late_materialize: bool = False


# ── Operator mapping from DSL ───────────────────────────────────────
_OP_MAP: dict[str, CompareOp] = {
    "$eq": CompareOp.EQ, "eq": CompareOp.EQ, "=": CompareOp.EQ,
    "$ne": CompareOp.NEQ, "neq": CompareOp.NEQ, "!=": CompareOp.NEQ,
    "$gt": CompareOp.GT, "gt": CompareOp.GT, ">": CompareOp.GT,
    "$gte": CompareOp.GTE, "gte": CompareOp.GTE, ">=": CompareOp.GTE,
    "$lt": CompareOp.LT, "lt": CompareOp.LT, "<": CompareOp.LT,
    "$lte": CompareOp.LTE, "lte": CompareOp.LTE, "<=": CompareOp.LTE,
    "$in": CompareOp.IN, "in": CompareOp.IN,
    "$nin": CompareOp.NOT_IN, "not_in": CompareOp.NOT_IN,
    "$like": CompareOp.LIKE, "like": CompareOp.LIKE,
    "$between": CompareOp.BETWEEN, "between": CompareOp.BETWEEN,
}

_AGG_MAP: dict[str, AggFunc] = {
    "count": AggFunc.COUNT, "sum": AggFunc.SUM, "avg": AggFunc.AVG,
    "min": AggFunc.MIN, "max": AggFunc.MAX,
    "count_distinct": AggFunc.COUNT_DISTINCT,
}


class QueryParser:
    """Parses a DSL dict into a QueryAST."""

    def parse(self, request: dict[str, Any]) -> QueryAST:
        action = request.get("action", "find")
        table = request.get("table", request.get("entity", ""))

        ast = QueryAST(action=action, table=table)

        # Where clause
        where = request.get("where", request.get("filter", {}))
        if where:
            ast.predicates = self._parse_where(where)

        # Projection
        select = request.get("select", request.get("columns", request.get("fields")))
        if select:
            ast.columns = list(select)

        # Search
        ast.query_text = request.get("query", request.get("q", ""))
        ast.query_vector = request.get("vector")
        ast.search_fields = request.get("search_fields")

        # Aggregation
        group_by = request.get("group_by", [])
        ast.group_by = list(group_by) if group_by else []

        metrics = request.get("metrics", request.get("aggregates", []))
        for m in metrics:
            if isinstance(m, dict):
                for func_name, col in m.items():
                    func = _AGG_MAP.get(func_name, AggFunc.COUNT)
                    ast.aggregates.append(AggregateExpr(
                        func=func, column=col, alias=f"{func_name}_{col}"
                    ))

        # Sort
        order_by = request.get("order_by", request.get("sort", []))
        for item in order_by:
            if isinstance(item, str):
                asc = not item.startswith("-")
                col = item.lstrip("-+")
                ast.order_by.append(SortKey(column=col, ascending=asc))
            elif isinstance(item, dict):
                col = item.get("column", item.get("field", ""))
                asc = item.get("order", "asc") == "asc"
                ast.order_by.append(SortKey(column=col, ascending=asc))

        # Limit / offset
        ast.limit = int(request.get("limit", 0))
        ast.offset = int(request.get("offset", 0))
        ast.top_k = int(request.get("top_k", 0))

        # Mutation data
        ast.data = request.get("data")

        # Hints
        ast.index_hints = request.get("index_hints", [])
        ast.use_cache = request.get("use_cache", True)
        ast.late_materialize = request.get("late_materialize", False)

        return ast

    def _parse_where(self, where: dict[str, Any]) -> list[Predicate | CompoundPredicate]:
        """Parse where clause into predicates."""
        predicates: list[Predicate | CompoundPredicate] = []

        for key, value in where.items():
            if key == "$and" and isinstance(value, list):
                children: list[Predicate | CompoundPredicate] = []
                for sub in value:
                    children.extend(self._parse_where(sub))
                predicates.append(CompoundPredicate(logic=LogicOp.AND, children=children))
            elif key == "$or" and isinstance(value, list):
                children = []
                for sub in value:
                    children.extend(self._parse_where(sub))
                predicates.append(CompoundPredicate(logic=LogicOp.OR, children=children))
            elif key == "$not" and isinstance(value, dict):
                child_preds = self._parse_where(value)
                predicates.append(CompoundPredicate(logic=LogicOp.NOT, children=child_preds))
            elif isinstance(value, dict):
                for op_str, operand in value.items():
                    op = _OP_MAP.get(op_str, CompareOp.EQ)
                    predicates.append(Predicate(column=key, op=op, value=operand))
            else:
                predicates.append(Predicate(column=key, op=CompareOp.EQ, value=value))

        return predicates
