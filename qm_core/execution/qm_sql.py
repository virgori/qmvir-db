"""QM-SQL — Hybrid AI-Native SQL dialect parsed with Lark (LALR).

Supports **two syntactic forms** for every QM-specific statement: a
verbose (self-documenting) form *and* a short-keyword form designed
for interactive REPL / Power-User workflows.

    ┌─────────────────────────────────┬──────────────────────────────┐
    │ Verbose form                    │ Short-keyword form           │
    ├─────────────────────────────────┼──────────────────────────────┤
    │ SEARCH VECTOR [...] IN t TOP k  │ LIKEV VEC [...] IN t TOP k   │
    │ CHECKPOINT [FULL|DELTA]         │ CPOINT [FULL|DELTA]          │
    │ LINK MEDIA '/path' TO t ROW <n> │ LINK '/path' TO t ROW <n>   │
    │ UNLINK MEDIA FROM t ROW_ID <n>  │ UNLINK FROM t ROW <n>       │
    │ SHOW SLABS                      │ SLABS                        │
    │ (n/a)                           │ MREF table row_id            │
    │ (n/a)                           │ SELECT DIST, … (virtual col) │
    └─────────────────────────────────┴──────────────────────────────┘

New short-keyword AST nodes:
    MrefStmt   — Retrieve a SlabHandle pointer for a given row.
    DistColumn — Virtual column representing similarity distance.

Standard SQL (SELECT/INSERT/UPDATE/DELETE/CREATE TABLE) is also
supported and transformed into the same AST nodes used by the
existing recursive-descent ``sql_parser.py``.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from lark import Lark, Transformer, v_args, Token as LarkToken

from qm_core.execution.sql_parser import (
    ASTNode, Literal, ColumnRef, BinaryOp, UnaryOp, FunctionCall,
    StarExpr, AliasedExpr, InList, BetweenExpr, LikeExpr, IsNullExpr,
    CaseExpr, WindowExpr,
    SelectStmt, InsertStmt, UpdateStmt, DeleteStmt, CreateTableStmt,
    JoinClause, CTEDef, SQLSyntaxError,
)


# ── QM-specific AST extensions ──────────────────────────────────────

@dataclass
class SearchVectorStmt(ASTNode):
    """SEARCH VECTOR [...] IN table TOP k [WHERE ...] [METRIC ...]"""
    vector: list[float]
    table: str
    top_k: int
    where: ASTNode | None = None
    metric: str = "cosine"


@dataclass
class SetCompressionStmt(ASTNode):
    """SET COMPRESSION 'codec_name' ON table"""
    codec: str
    table: str


@dataclass
class LinkMediaStmt(ASTNode):
    """LINK MEDIA '/path' TO table row_id <id>"""
    path: str
    table: str
    row_id: int


@dataclass
class UnlinkMediaStmt(ASTNode):
    """UNLINK MEDIA FROM table row_id <id>"""
    table: str
    row_id: int


@dataclass
class ShowSlabsStmt(ASTNode):
    """SHOW SLABS  /  SLABS"""
    pass


@dataclass
class CheckpointStmt(ASTNode):
    """CHECKPOINT [FULL|DELTA]  /  CPOINT [FULL|DELTA]"""
    mode: str = "full"


@dataclass
class MrefStmt(ASTNode):
    """MREF table row_id — retrieve SlabHandle pointer for a media row."""
    table: str
    row_id: int


@dataclass
class DistColumn(ASTNode):
    """Virtual column representing similarity distance in LIKEV results."""
    pass


# ── Lark Grammar ────────────────────────────────────────────────────

QM_SQL_GRAMMAR = r"""
    ?start: statement ";"?

    ?statement: search_vector_stmt
              | likev_stmt
              | set_compression_stmt
              | link_media_stmt
              | unlink_media_stmt
              | show_slabs_stmt
              | slabs_short_stmt
              | checkpoint_stmt
              | cpoint_stmt
              | mref_stmt
              | select_stmt
              | insert_stmt
              | update_stmt
              | delete_stmt
              | create_table_stmt

    // ── QM Extensions (verbose) ────────────────────────────────────

    search_vector_stmt: "SEARCH"i "VECTOR"i vector_literal "IN"i NAME "TOP"i INT (where_clause)? (metric_clause)?
    vector_literal: "[" number ("," number)* "]"
    metric_clause: "METRIC"i METRIC_NAME
    METRIC_NAME: "cosine"i | "l2"i | "ip"i | "dot"i

    set_compression_stmt: "SET"i "COMPRESSION"i STRING "ON"i NAME
    link_media_stmt: "LINK"i "MEDIA"i? STRING "TO"i NAME ("ROW_ID"i | "ROW"i) INT
    unlink_media_stmt: "UNLINK"i "MEDIA"i? "FROM"i NAME ("ROW_ID"i | "ROW"i) INT
    show_slabs_stmt: "SHOW"i "SLABS"i
    checkpoint_stmt: "CHECKPOINT"i CHECKPOINT_MODE?
    CHECKPOINT_MODE: "FULL"i | "DELTA"i

    // ── QM Extensions (short keywords) ─────────────────────────────

    likev_stmt: "LIKEV"i "VEC"i vector_literal "IN"i NAME "TOP"i INT (where_clause)? (metric_clause)?
    cpoint_stmt: "CPOINT"i CHECKPOINT_MODE?
    slabs_short_stmt: "SLABS"i
    mref_stmt: "MREF"i NAME INT

    // ── Standard SQL ───────────────────────────────────────────────

    select_stmt: with_clause? "SELECT"i distinct? select_list "FROM"i table_ref (join_clause)* (where_clause)? (group_clause)? (having_clause)? (order_clause)? (limit_clause)?
    with_clause: "WITH"i cte_def ("," cte_def)*
    cte_def: NAME "AS"i "(" select_stmt ")"
    distinct: "DISTINCT"i

    select_list: select_item ("," select_item)*
    ?select_item: star_expr
                | aliased_expr
    star_expr: (NAME ".")? "*"
    aliased_expr: expr ("AS"i NAME)?

    table_ref: NAME (NAME)?  // table [alias]
    join_clause: join_type "JOIN"i NAME (NAME)? "ON"i expr
    join_type: ("INNER"i | "LEFT"i "OUTER"i? | "RIGHT"i "OUTER"i? | "FULL"i "OUTER"i? | "CROSS"i)?

    where_clause: "WHERE"i expr
    group_clause: "GROUP"i "BY"i expr_list
    having_clause: "HAVING"i expr
    order_clause: "ORDER"i "BY"i order_item ("," order_item)*
    order_item: expr ORDER_DIR?
    ORDER_DIR: "ASC"i | "DESC"i
    limit_clause: "LIMIT"i INT ("OFFSET"i INT)?

    insert_stmt: "INSERT"i "INTO"i NAME "(" name_list ")" "VALUES"i "(" expr_list ")"
    update_stmt: "UPDATE"i NAME "SET"i set_list (where_clause)?
    set_list: set_item ("," set_item)*
    set_item: NAME "=" expr
    delete_stmt: "DELETE"i "FROM"i NAME (where_clause)?

    create_table_stmt: "CREATE"i "TABLE"i NAME "(" column_def_list ")"
    column_def_list: column_def ("," column_def)*
    column_def: NAME NAME+ // col_name type_name [constraints]

    // ── Expressions ────────────────────────────────────────────────

    expr_list: expr ("," expr)*
    name_list: NAME ("," NAME)*

    ?expr: or_expr
    ?or_expr: and_expr ("OR"i and_expr)*
    ?and_expr: not_expr ("AND"i not_expr)*
    ?not_expr: "NOT"i not_expr -> not_op
             | cmp_expr
    ?cmp_expr: add_expr "BETWEEN"i add_expr "AND"i add_expr -> between_expr
             | add_expr "NOT"i "BETWEEN"i add_expr "AND"i add_expr -> not_between_expr
             | add_expr "IN"i "(" expr_list ")" -> in_expr
             | add_expr "NOT"i "IN"i "(" expr_list ")" -> not_in_expr
             | add_expr "LIKE"i add_expr -> like_expr
             | add_expr "NOT"i "LIKE"i add_expr -> not_like_expr
             | add_expr "IS"i "NULL"i -> is_null_expr
             | add_expr "IS"i "NOT"i "NULL"i -> is_not_null_expr
             | add_expr CMP_OP add_expr -> cmp_op
             | add_expr
    CMP_OP: "=" | "!=" | "<>" | "<=" | ">=" | "<" | ">"

    ?add_expr: mul_expr (ADD_OP mul_expr)*
    ADD_OP: "+" | "-"
    ?mul_expr: unary_expr (MUL_OP unary_expr)*
    MUL_OP: "*" | "/"

    ?unary_expr: "-" atom -> neg
               | atom

    ?atom: func_call
         | dist_column
         | column_ref
         | literal
         | "(" expr ")"
         | case_expr

    func_call: NAME "(" (("DISTINCT"i)? expr_list | "*") ")"
    dist_column: "DIST"i
    column_ref: NAME "." NAME -> qual_column
              | NAME          -> simple_column
    case_expr: "CASE"i (expr)? when_clause+ ("ELSE"i expr)? "END"i
    when_clause: "WHEN"i expr "THEN"i expr

    ?literal: number
            | STRING     -> string_lit
            | "NULL"i    -> null_lit
            | "TRUE"i    -> true_lit
            | "FALSE"i   -> false_lit

    ?number: INT      -> int_lit
           | FLOAT    -> float_lit

    // ── Terminals ──────────────────────────────────────────────────
    NAME: /[a-zA-Z_][a-zA-Z0-9_]*/
    INT: /[0-9]+/
    FLOAT: /[0-9]+\.[0-9]*/
    STRING: "'" /[^']*/ "'"

    %ignore /[ \t\n\r]+/
    %ignore /--[^\n]*/
"""

# ── Transformer  (Lark tree → QM AST) ───────────────────────────────

@v_args(inline=True)
class _QMSQLTransformer(Transformer):
    """Transform Lark parse tree to QM AST nodes."""

    # ── Literals ────────────────────────────────────────────────────

    def int_lit(self, tok):
        return Literal(int(tok))

    def float_lit(self, tok):
        return Literal(float(tok))

    def string_lit(self, tok):
        s = str(tok)
        if s.startswith("'") and s.endswith("'"):
            s = s[1:-1]
        return Literal(s)

    def null_lit(self):
        return Literal(None)

    def true_lit(self):
        return Literal(True)

    def false_lit(self):
        return Literal(False)

    # ── Columns ─────────────────────────────────────────────────────

    def simple_column(self, name):
        return ColumnRef(table=None, column=str(name))

    def qual_column(self, table, col):
        return ColumnRef(table=str(table), column=str(col))

    def column_ref(self, *args):
        if len(args) == 2:
            return ColumnRef(table=str(args[0]), column=str(args[1]))
        return ColumnRef(table=None, column=str(args[0]))

    # ── Expressions ─────────────────────────────────────────────────

    def cmp_op(self, left, op, right):
        op_str = str(op).strip()
        if op_str == "<>":
            op_str = "!="
        return BinaryOp(op=op_str, left=left, right=right)

    def or_expr(self, *args):
        if len(args) == 1:
            return args[0]
        result = args[0]
        for a in args[1:]:
            result = BinaryOp(op="OR", left=result, right=a)
        return result

    def and_expr(self, *args):
        if len(args) == 1:
            return args[0]
        result = args[0]
        for a in args[1:]:
            result = BinaryOp(op="AND", left=result, right=a)
        return result

    def not_op(self, expr):
        return UnaryOp(op="NOT", operand=expr)

    def neg(self, expr):
        return UnaryOp(op="-", operand=expr)

    def add_expr(self, *args):
        if len(args) == 1:
            return args[0]
        result = args[0]
        i = 1
        while i < len(args):
            op = str(args[i])
            rhs = args[i + 1]
            result = BinaryOp(op=op, left=result, right=rhs)
            i += 2
        return result

    def mul_expr(self, *args):
        if len(args) == 1:
            return args[0]
        result = args[0]
        i = 1
        while i < len(args):
            op = str(args[i])
            rhs = args[i + 1]
            result = BinaryOp(op=op, left=result, right=rhs)
            i += 2
        return result

    def between_expr(self, expr, low, high):
        return BetweenExpr(expr=expr, low=low, high=high)

    def not_between_expr(self, expr, low, high):
        return BetweenExpr(expr=expr, low=low, high=high, negate=True)

    def in_expr(self, expr, values):
        return InList(expr=expr, values=values if isinstance(values, list) else [values])

    def not_in_expr(self, expr, values):
        return InList(expr=expr, values=values if isinstance(values, list) else [values], negate=True)

    def like_expr(self, expr, pattern):
        return LikeExpr(expr=expr, pattern=pattern)

    def not_like_expr(self, expr, pattern):
        return LikeExpr(expr=expr, pattern=pattern, negate=True)

    def is_null_expr(self, expr):
        return IsNullExpr(expr=expr)

    def is_not_null_expr(self, expr):
        return IsNullExpr(expr=expr, negate=True)

    def case_expr(self, *args):
        # Collect WHEN..THEN pairs and optional ELSE
        parts = list(args)
        when_clauses = []
        else_clause = None
        operand = None
        i = 0
        # first child could be operand (if CASE expr WHEN ...)
        while i < len(parts):
            if isinstance(parts[i], tuple) and len(parts[i]) == 2:
                when_clauses.append(parts[i])
                i += 1
            elif i == 0 and not isinstance(parts[i], tuple):
                operand = parts[i]
                i += 1
            else:
                else_clause = parts[i]
                i += 1
        return CaseExpr(operand=operand, when_clauses=when_clauses,
                        else_clause=else_clause)

    def when_clause(self, cond, result):
        return (cond, result)

    # ── Functions ───────────────────────────────────────────────────

    def func_call(self, *args):
        name = str(args[0]).upper()
        distinct = False
        func_args = []
        for a in args[1:]:
            if isinstance(a, LarkToken) and str(a).upper() == "DISTINCT":
                distinct = True
            elif isinstance(a, list):
                func_args.extend(a)
            elif isinstance(a, LarkToken) and str(a) == "*":
                func_args.append(StarExpr())
            else:
                func_args.append(a)
        return FunctionCall(name=name, args=func_args, distinct=distinct)

    # ── Aggregations ────────────────────────────────────────────────

    def expr_list(self, *args):
        return list(args)

    def name_list(self, *args):
        return [str(a) for a in args]

    # ── SELECT ──────────────────────────────────────────────────────

    def star_expr(self, *args):
        tbl = str(args[0]) if args else None
        return StarExpr(table=tbl)

    def aliased_expr(self, expr, *rest):
        alias = str(rest[0]) if rest else None
        return AliasedExpr(expr=expr, alias=alias)

    def select_list(self, *items):
        return list(items)

    def table_ref(self, name, *alias):
        return (str(name), str(alias[0]) if alias else None)

    def join_type(self, *types):
        parts = [str(t).upper() for t in types if t]
        return " ".join(parts) if parts else "INNER"

    def join_clause(self, jtype, name, *rest):
        alias = None
        condition = rest[-1]  # ON expr is always last
        if len(rest) > 1:
            alias = str(rest[0])
        return JoinClause(
            join_type=jtype if isinstance(jtype, str) else str(jtype),
            table=str(name),
            alias=alias,
            condition=condition,
        )

    def where_clause(self, expr):
        return ("where", expr)

    def group_clause(self, exprs):
        return ("group", exprs)

    def having_clause(self, expr):
        return ("having", expr)

    def order_item(self, expr, *direction):
        asc = True
        if direction and str(direction[0]).upper() == "DESC":
            asc = False
        return (expr, asc)

    def order_clause(self, *items):
        return ("order", list(items))

    def limit_clause(self, limit, *offset):
        off = int(offset[0]) if offset else 0
        return ("limit", int(limit), off)

    def distinct(self):
        return True

    def cte_def(self, name, stmt):
        return CTEDef(name=str(name), query=stmt)

    def with_clause(self, *ctes):
        return list(ctes)

    def select_stmt(self, *parts):
        ctes = None
        is_distinct = False
        columns = []
        from_table = None
        from_alias = None
        joins = []
        where = None
        group_by = None
        having = None
        order_by = None
        limit = None
        offset = None

        for p in parts:
            if isinstance(p, list) and p and isinstance(p[0], CTEDef):
                ctes = p
            elif p is True:
                is_distinct = True
            elif isinstance(p, list) and p and isinstance(p[0], (ASTNode, StarExpr, AliasedExpr)):
                if not columns:
                    columns = p
            elif isinstance(p, JoinClause):
                joins.append(p)
            # Check tagged clause tuples BEFORE generic string-tuple
            elif isinstance(p, tuple) and p and p[0] == "where":
                where = p[1]
            elif isinstance(p, tuple) and p and p[0] == "group":
                group_by = p[1]
            elif isinstance(p, tuple) and p and p[0] == "having":
                having = p[1]
            elif isinstance(p, tuple) and p and p[0] == "order":
                order_by = p[1]
            elif isinstance(p, tuple) and p and p[0] == "limit":
                limit = p[1]
                offset = p[2] if len(p) > 2 else 0
            # table_ref: (name, alias) — generic string tuple last
            elif isinstance(p, tuple) and len(p) == 2 and isinstance(p[0], str):
                if from_table is None:
                    from_table = p[0]
                    from_alias = p[1]

        return SelectStmt(
            columns=columns,
            from_table=from_table,
            from_alias=from_alias,
            joins=joins,
            where=where,
            group_by=group_by or [],
            having=having,
            order_by=order_by or [],
            limit=limit,
            offset=offset or 0,
            distinct=is_distinct,
            ctes=ctes or [],
        )

    # ── INSERT ──────────────────────────────────────────────────────

    def insert_stmt(self, table, cols, vals):
        # InsertStmt.values is list[list[ASTNode]] (multiple rows)
        row = vals if isinstance(vals, list) else [vals]
        return InsertStmt(
            table=str(table),
            columns=cols if isinstance(cols, list) else [cols],
            values=[row],
        )

    # ── UPDATE ──────────────────────────────────────────────────────

    def set_item(self, col, expr):
        return (str(col), expr)

    def set_list(self, *items):
        return list(items)

    def update_stmt(self, table, sets, *where):
        w = None
        for part in where:
            if isinstance(part, tuple) and part[0] == "where":
                w = part[1]
        return UpdateStmt(
            table=str(table),
            assignments=sets if isinstance(sets, list) else [sets],
            where=w,
        )

    # ── DELETE ──────────────────────────────────────────────────────

    def delete_stmt(self, table, *where):
        w = None
        for part in where:
            if isinstance(part, tuple) and part[0] == "where":
                w = part[1]
        return DeleteStmt(table=str(table), where=w)

    # ── CREATE TABLE ────────────────────────────────────────────────

    def column_def(self, *parts):
        name = str(parts[0])
        type_parts = [str(p) for p in parts[1:]]
        return (name, " ".join(type_parts))

    def column_def_list(self, *cols):
        return list(cols)

    def create_table_stmt(self, name, cols):
        from qm_core.execution.sql_parser import ColumnDef
        col_defs = []
        for col_name, col_type in cols:
            col_defs.append(ColumnDef(name=col_name, data_type=col_type))
        return CreateTableStmt(
            table=str(name),
            columns=col_defs,
        )

    # ── QM Extensions ───────────────────────────────────────────────

    def vector_literal(self, *numbers):
        return [n.value if isinstance(n, Literal) else float(n) for n in numbers]

    def metric_clause(self, name):
        return str(name).lower()

    def search_vector_stmt(self, *args):
        parts = list(args)
        vec = parts[0]
        table = str(parts[1])
        top_k = int(parts[2])
        where = None
        metric = "cosine"
        for p in parts[3:]:
            if isinstance(p, tuple) and p[0] == "where":
                where = p[1]
            elif isinstance(p, str):
                metric = p
        return SearchVectorStmt(
            vector=vec, table=table, top_k=top_k,
            where=where, metric=metric,
        )

    def set_compression_stmt(self, codec, table):
        c = str(codec)
        if c.startswith("'") and c.endswith("'"):
            c = c[1:-1]
        return SetCompressionStmt(codec=c, table=str(table))

    def link_media_stmt(self, path, table, row_id):
        p = str(path)
        if p.startswith("'") and p.endswith("'"):
            p = p[1:-1]
        return LinkMediaStmt(path=p, table=str(table), row_id=int(row_id))

    def unlink_media_stmt(self, table, row_id):
        return UnlinkMediaStmt(table=str(table), row_id=int(row_id))

    def show_slabs_stmt(self):
        return ShowSlabsStmt()

    def slabs_short_stmt(self):
        return ShowSlabsStmt()

    def checkpoint_stmt(self, *args):
        mode = "full"
        if args:
            mode = str(args[0]).lower()
        return CheckpointStmt(mode=mode)

    def cpoint_stmt(self, *args):
        mode = "full"
        if args:
            mode = str(args[0]).lower()
        return CheckpointStmt(mode=mode)

    def likev_stmt(self, *args):
        """LIKEV VEC [...] IN table TOP k — short form of search_vector_stmt."""
        return self.search_vector_stmt(*args)

    def mref_stmt(self, table, row_id):
        return MrefStmt(table=str(table), row_id=int(row_id))

    def dist_column(self):
        return DistColumn()


# ── Parser Class ─────────────────────────────────────────────────────

class QMSQLParser:
    """Parse QM-SQL Hybrid dialect into AST nodes.

    Supports both verbose and short-keyword syntax:
        parser = QMSQLParser()
        ast = parser.parse("SEARCH VECTOR [0.1, 0.5] IN photos TOP 10;")
        ast = parser.parse("LIKEV VEC [0.1, 0.5] IN photos TOP 10;")  # equivalent
        ast = parser.parse("CPOINT FULL;")  # short for CHECKPOINT FULL
        ast = parser.parse("SLABS;")        # short for SHOW SLABS
        ast = parser.parse("MREF photos 42;")  # get SlabHandle for row
    """

    def __init__(self) -> None:
        self._parser = Lark(
            QM_SQL_GRAMMAR,
            parser="lalr",
            propagate_positions=True,
        )
        self._transformer = _QMSQLTransformer()

    def parse(self, sql: str) -> ASTNode:
        """Parse a QM-SQL statement and return an AST node.

        Raises SQLSyntaxError on parse failure.
        """
        try:
            tree = self._parser.parse(sql)
            return self._transformer.transform(tree)
        except SQLSyntaxError:
            raise
        except Exception as e:
            raise SQLSyntaxError(f"QM-SQL parse error: {e}") from e
