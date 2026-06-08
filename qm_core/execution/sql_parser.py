"""QM Execution — Recursive-Descent SQL Parser.

Parses a practical subset of SQL into QueryAST objects that the
existing QM planner and engine can execute.

Supported statements:
    SELECT [DISTINCT] cols FROM table
        [JOIN table ON condition]
        [WHERE conditions]
        [GROUP BY cols [HAVING cond]]
        [ORDER BY cols [ASC|DESC]]
        [LIMIT n [OFFSET m]]
    INSERT INTO table (cols) VALUES (...)
    UPDATE table SET col=val [WHERE ...]
    DELETE FROM table [WHERE ...]
    CREATE TABLE table (col type [constraints], ...)
    WITH cte_name AS (SELECT ...) SELECT ...

Expressions: comparisons, AND/OR/NOT, BETWEEN, IN, LIKE, IS NULL,
             arithmetic (+,-,*,/), function calls, subqueries.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from enum import IntEnum, auto
from typing import Any

try:
    import qm_engine as _qm_engine
    _RUST_SQL_PARSER = _qm_engine.SqlParser()
except Exception:
    _qm_engine = None
    _RUST_SQL_PARSER = None


# ── Token types ─────────────────────────────────────────────────────

class TT(IntEnum):
    # Literals
    IDENT = auto()
    NUMBER = auto()
    STRING = auto()
    # Keywords
    SELECT = auto(); FROM = auto(); WHERE = auto()
    INSERT = auto(); INTO = auto(); VALUES = auto()
    UPDATE = auto(); SET = auto()
    DELETE = auto()
    CREATE = auto(); TABLE = auto()
    JOIN = auto(); LEFT = auto(); RIGHT = auto(); FULL = auto()
    INNER = auto(); OUTER = auto(); CROSS = auto(); ON = auto()
    AND = auto(); OR = auto(); NOT = auto()
    IN = auto(); BETWEEN = auto(); LIKE = auto()
    IS = auto(); NULL = auto(); TRUE = auto(); FALSE = auto()
    ORDER = auto(); BY = auto(); ASC = auto(); DESC = auto()
    GROUP = auto(); HAVING = auto()
    LIMIT = auto(); OFFSET = auto()
    AS = auto(); DISTINCT = auto(); ALL = auto()
    EXISTS = auto()
    WITH = auto()
    CASE = auto(); WHEN = auto(); THEN = auto(); ELSE = auto(); END = auto()
    OVER = auto(); PARTITION = auto(); ROWS = auto()
    WINDOW = auto(); UNBOUNDED = auto(); PRECEDING = auto()
    FOLLOWING = auto(); CURRENT = auto(); ROW = auto()
    PRIMARY = auto(); KEY = auto(); UNIQUE = auto()
    CHECK = auto(); REFERENCES = auto(); FOREIGN = auto()
    DEFAULT = auto(); CONSTRAINT = auto()
    # Operators
    EQ = auto(); NEQ = auto(); LT = auto(); GT = auto()
    LTE = auto(); GTE = auto()
    PLUS = auto(); MINUS = auto(); STAR = auto(); SLASH = auto()
    LPAREN = auto(); RPAREN = auto()
    COMMA = auto(); DOT = auto(); SEMICOLON = auto()
    # Special
    EOF = auto()


KEYWORDS: dict[str, TT] = {
    "SELECT": TT.SELECT, "FROM": TT.FROM, "WHERE": TT.WHERE,
    "INSERT": TT.INSERT, "INTO": TT.INTO, "VALUES": TT.VALUES,
    "UPDATE": TT.UPDATE, "SET": TT.SET,
    "DELETE": TT.DELETE,
    "CREATE": TT.CREATE, "TABLE": TT.TABLE,
    "JOIN": TT.JOIN, "LEFT": TT.LEFT, "RIGHT": TT.RIGHT,
    "FULL": TT.FULL, "INNER": TT.INNER, "OUTER": TT.OUTER,
    "CROSS": TT.CROSS, "ON": TT.ON,
    "AND": TT.AND, "OR": TT.OR, "NOT": TT.NOT,
    "IN": TT.IN, "BETWEEN": TT.BETWEEN, "LIKE": TT.LIKE,
    "IS": TT.IS, "NULL": TT.NULL, "TRUE": TT.TRUE, "FALSE": TT.FALSE,
    "ORDER": TT.ORDER, "BY": TT.BY, "ASC": TT.ASC, "DESC": TT.DESC,
    "GROUP": TT.GROUP, "HAVING": TT.HAVING,
    "LIMIT": TT.LIMIT, "OFFSET": TT.OFFSET,
    "AS": TT.AS, "DISTINCT": TT.DISTINCT, "ALL": TT.ALL,
    "EXISTS": TT.EXISTS,
    "WITH": TT.WITH,
    "CASE": TT.CASE, "WHEN": TT.WHEN, "THEN": TT.THEN,
    "ELSE": TT.ELSE, "END": TT.END,
    "OVER": TT.OVER, "PARTITION": TT.PARTITION, "ROWS": TT.ROWS,
    "WINDOW": TT.WINDOW, "UNBOUNDED": TT.UNBOUNDED,
    "PRECEDING": TT.PRECEDING, "FOLLOWING": TT.FOLLOWING,
    "CURRENT": TT.CURRENT, "ROW": TT.ROW,
    "PRIMARY": TT.PRIMARY, "KEY": TT.KEY, "UNIQUE": TT.UNIQUE,
    "CHECK": TT.CHECK, "REFERENCES": TT.REFERENCES,
    "FOREIGN": TT.FOREIGN, "DEFAULT": TT.DEFAULT,
    "CONSTRAINT": TT.CONSTRAINT,
}


@dataclass(slots=True)
class Token:
    type: TT
    value: str
    pos: int = 0


# ── Tokenizer ──────────────────────────────────────────────────────

class Lexer:
    """SQL tokenizer."""

    _PATTERNS = [
        (r"--[^\n]*",          None),        # Line comment
        (r"/\*.*?\*/",         None),        # Block comment
        (r"\s+",               None),        # Whitespace
        (r"'([^']*)'",        TT.STRING),    # String literal
        (r"\d+(?:\.\d+)?",    TT.NUMBER),    # Number
        (r"!=|<>",            TT.NEQ),
        (r"<=",               TT.LTE),
        (r">=",               TT.GTE),
        (r"=",                TT.EQ),
        (r"<",                TT.LT),
        (r">",                TT.GT),
        (r"\+",               TT.PLUS),
        (r"-",                TT.MINUS),
        (r"\*",               TT.STAR),
        (r"/",                TT.SLASH),
        (r"\(",               TT.LPAREN),
        (r"\)",               TT.RPAREN),
        (r",",                TT.COMMA),
        (r"\.",               TT.DOT),
        (r";",                TT.SEMICOLON),
        (r"[a-zA-Z_][a-zA-Z0-9_]*", TT.IDENT),
    ]

    def __init__(self, sql: str) -> None:
        self._sql = sql
        self._pos = 0
        self._compiled = [(re.compile(p), t) for p, t in self._PATTERNS]

    def tokenize(self) -> list[Token]:
        tokens: list[Token] = []
        while self._pos < len(self._sql):
            matched = False
            for regex, tt in self._compiled:
                m = regex.match(self._sql, self._pos)
                if m:
                    if tt is not None:
                        val = m.group(1) if m.lastindex else m.group(0)
                        if tt == TT.IDENT:
                            kw = KEYWORDS.get(val.upper())
                            if kw:
                                tokens.append(Token(kw, val.upper(), self._pos))
                            else:
                                tokens.append(Token(TT.IDENT, val, self._pos))
                        else:
                            tokens.append(Token(tt, val, self._pos))
                    self._pos = m.end()
                    matched = True
                    break
            if not matched:
                raise SQLSyntaxError(f"Unexpected character '{self._sql[self._pos]}' at position {self._pos}")
        tokens.append(Token(TT.EOF, "", self._pos))
        return tokens


# ── AST Nodes ──────────────────────────────────────────────────────

@dataclass
class ASTNode:
    """Base AST node."""
    pass


@dataclass
class Literal(ASTNode):
    value: Any

@dataclass
class ColumnRef(ASTNode):
    table: str | None
    column: str

@dataclass
class BinaryOp(ASTNode):
    op: str
    left: ASTNode
    right: ASTNode

@dataclass
class UnaryOp(ASTNode):
    op: str
    operand: ASTNode

@dataclass
class FunctionCall(ASTNode):
    name: str
    args: list[ASTNode]
    distinct: bool = False

@dataclass
class WindowExpr(ASTNode):
    func: FunctionCall
    partition_by: list[ASTNode] = field(default_factory=list)
    order_by: list[tuple[ASTNode, bool]] = field(default_factory=list)  # (expr, asc)

@dataclass
class InList(ASTNode):
    expr: ASTNode
    values: list[ASTNode]
    negate: bool = False

@dataclass
class BetweenExpr(ASTNode):
    expr: ASTNode
    low: ASTNode
    high: ASTNode
    negate: bool = False

@dataclass
class LikeExpr(ASTNode):
    expr: ASTNode
    pattern: ASTNode
    negate: bool = False

@dataclass
class IsNullExpr(ASTNode):
    expr: ASTNode
    negate: bool = False

@dataclass
class CaseExpr(ASTNode):
    operand: ASTNode | None
    when_clauses: list[tuple[ASTNode, ASTNode]]
    else_clause: ASTNode | None

@dataclass
class StarExpr(ASTNode):
    table: str | None = None

@dataclass
class AliasedExpr(ASTNode):
    expr: ASTNode
    alias: str | None = None


# ── Statement ASTs ─────────────────────────────────────────────────

@dataclass
class JoinClause:
    join_type: str  # "INNER", "LEFT", "RIGHT", "FULL", "CROSS"
    table: str
    alias: str | None
    condition: ASTNode | None

@dataclass
class CTEDef:
    name: str
    query: SelectStmt

@dataclass
class SelectStmt(ASTNode):
    columns: list[AliasedExpr]
    from_table: str | None = None
    from_alias: str | None = None
    joins: list[JoinClause] = field(default_factory=list)
    where: ASTNode | None = None
    group_by: list[ASTNode] = field(default_factory=list)
    having: ASTNode | None = None
    order_by: list[tuple[ASTNode, bool]] = field(default_factory=list)
    limit: int | None = None
    offset: int | None = None
    distinct: bool = False
    ctes: list[CTEDef] = field(default_factory=list)

@dataclass
class InsertStmt(ASTNode):
    table: str
    columns: list[str]
    values: list[list[ASTNode]]  # Multiple rows

@dataclass
class UpdateStmt(ASTNode):
    table: str
    assignments: list[tuple[str, ASTNode]]
    where: ASTNode | None = None

@dataclass
class DeleteStmt(ASTNode):
    table: str
    where: ASTNode | None = None

@dataclass
class ColumnDef:
    name: str
    data_type: str
    nullable: bool = True
    primary_key: bool = False
    unique: bool = False
    default: ASTNode | None = None
    check: ASTNode | None = None
    references: tuple[str, str] | None = None  # (table, column)

@dataclass
class CreateTableStmt(ASTNode):
    table: str
    columns: list[ColumnDef]
    constraints: list[Any] = field(default_factory=list)


class SQLSyntaxError(Exception):
    """Raised on SQL parse errors."""
    pass


# ── Parser ─────────────────────────────────────────────────────────

class SQLParser:
    """Recursive-descent SQL parser.

    Usage:
        parser = SQLParser("SELECT a, b FROM t WHERE a > 5")
        ast = parser.parse()
    """

    def __init__(self, sql: str) -> None:
        self._tokens = Lexer(sql).tokenize()
        self._pos = 0

    def parse(self) -> ASTNode:
        """Parse a single SQL statement."""
        if self._peek() == TT.WITH:
            return self._parse_with()
        result = self._parse_statement()
        if self._peek() == TT.SEMICOLON:
            self._advance()
        return result

    def parse_multi(self) -> list[ASTNode]:
        """Parse multiple statements separated by semicolons."""
        stmts: list[ASTNode] = []
        while self._peek() != TT.EOF:
            stmts.append(self.parse())
            while self._peek() == TT.SEMICOLON:
                self._advance()
        return stmts

    # ── Statement parsing ──────────────────────────────────────────

    def _parse_statement(self) -> ASTNode:
        tt = self._peek()
        if tt == TT.SELECT:
            return self._parse_select()
        if tt == TT.INSERT:
            return self._parse_insert()
        if tt == TT.UPDATE:
            return self._parse_update()
        if tt == TT.DELETE:
            return self._parse_delete()
        if tt == TT.CREATE:
            return self._parse_create()
        raise SQLSyntaxError(f"Expected statement, got {self._current().value!r}")

    def _parse_with(self) -> SelectStmt:
        """Parse WITH cte AS (...) SELECT ..."""
        self._expect(TT.WITH)
        ctes: list[CTEDef] = []
        while True:
            name = self._expect(TT.IDENT).value
            self._expect(TT.AS)
            self._expect(TT.LPAREN)
            query = self._parse_select()
            self._expect(TT.RPAREN)
            ctes.append(CTEDef(name=name, query=query))
            if self._peek() != TT.COMMA:
                break
            self._advance()
        stmt = self._parse_select()
        stmt.ctes = ctes
        return stmt

    def _parse_select(self) -> SelectStmt:
        self._expect(TT.SELECT)
        distinct = False
        if self._peek() == TT.DISTINCT:
            self._advance()
            distinct = True

        columns = self._parse_select_list()
        from_table = None
        from_alias = None
        joins: list[JoinClause] = []
        where = None
        group_by: list[ASTNode] = []
        having = None
        order_by: list[tuple[ASTNode, bool]] = []
        limit = None
        offset = None

        if self._peek() == TT.FROM:
            self._advance()
            from_table = self._expect(TT.IDENT).value
            if self._peek() == TT.AS:
                self._advance()
                from_alias = self._expect(TT.IDENT).value
            elif self._peek() == TT.IDENT and self._peek() not in (
                TT.WHERE, TT.JOIN, TT.LEFT, TT.RIGHT, TT.FULL, TT.INNER,
                TT.CROSS, TT.ORDER, TT.GROUP, TT.LIMIT, TT.ON,
            ):
                from_alias = self._expect(TT.IDENT).value

            # Parse JOINs
            while self._peek() in (TT.JOIN, TT.LEFT, TT.RIGHT, TT.FULL, TT.INNER, TT.CROSS):
                joins.append(self._parse_join())

        if self._peek() == TT.WHERE:
            self._advance()
            where = self._parse_expr()

        if self._peek() == TT.GROUP:
            self._advance()
            self._expect(TT.BY)
            group_by = [self._parse_expr()]
            while self._peek() == TT.COMMA:
                self._advance()
                group_by.append(self._parse_expr())
            if self._peek() == TT.HAVING:
                self._advance()
                having = self._parse_expr()

        if self._peek() == TT.ORDER:
            self._advance()
            self._expect(TT.BY)
            order_by = self._parse_order_list()

        if self._peek() == TT.LIMIT:
            self._advance()
            limit = int(self._expect(TT.NUMBER).value)
            if self._peek() == TT.OFFSET:
                self._advance()
                offset = int(self._expect(TT.NUMBER).value)

        return SelectStmt(
            columns=columns, from_table=from_table, from_alias=from_alias,
            joins=joins, where=where, group_by=group_by, having=having,
            order_by=order_by, limit=limit, offset=offset, distinct=distinct,
        )

    def _parse_select_list(self) -> list[AliasedExpr]:
        cols: list[AliasedExpr] = []
        cols.append(self._parse_aliased_expr())
        while self._peek() == TT.COMMA:
            self._advance()
            cols.append(self._parse_aliased_expr())
        return cols

    def _parse_aliased_expr(self) -> AliasedExpr:
        if self._peek() == TT.STAR:
            self._advance()
            return AliasedExpr(expr=StarExpr())
        expr = self._parse_expr()
        alias = None
        if self._peek() == TT.AS:
            self._advance()
            alias = self._expect(TT.IDENT).value
        elif self._peek() == TT.IDENT:
            # Implicit alias
            nxt = self._current()
            if nxt.type == TT.IDENT and nxt.value.upper() not in KEYWORDS:
                alias = self._advance().value
        return AliasedExpr(expr=expr, alias=alias)

    def _parse_join(self) -> JoinClause:
        jtype = "INNER"
        if self._peek() == TT.LEFT:
            self._advance(); jtype = "LEFT"
            if self._peek() == TT.OUTER:
                self._advance()
        elif self._peek() == TT.RIGHT:
            self._advance(); jtype = "RIGHT"
            if self._peek() == TT.OUTER:
                self._advance()
        elif self._peek() == TT.FULL:
            self._advance(); jtype = "FULL"
            if self._peek() == TT.OUTER:
                self._advance()
        elif self._peek() == TT.CROSS:
            self._advance(); jtype = "CROSS"
        elif self._peek() == TT.INNER:
            self._advance()

        self._expect(TT.JOIN)
        table = self._expect(TT.IDENT).value
        alias = None
        if self._peek() == TT.AS:
            self._advance()
            alias = self._expect(TT.IDENT).value
        elif self._peek() == TT.IDENT and self._current().value.upper() not in KEYWORDS:
            alias = self._advance().value

        condition = None
        if self._peek() == TT.ON:
            self._advance()
            condition = self._parse_expr()

        return JoinClause(join_type=jtype, table=table, alias=alias, condition=condition)

    def _parse_order_list(self) -> list[tuple[ASTNode, bool]]:
        items: list[tuple[ASTNode, bool]] = []
        expr = self._parse_expr()
        asc = True
        if self._peek() == TT.ASC:
            self._advance()
        elif self._peek() == TT.DESC:
            self._advance(); asc = False
        items.append((expr, asc))
        while self._peek() == TT.COMMA:
            self._advance()
            expr = self._parse_expr()
            asc = True
            if self._peek() == TT.ASC:
                self._advance()
            elif self._peek() == TT.DESC:
                self._advance(); asc = False
            items.append((expr, asc))
        return items

    def _parse_insert(self) -> InsertStmt:
        self._expect(TT.INSERT)
        self._expect(TT.INTO)
        table = self._expect(TT.IDENT).value
        columns: list[str] = []
        if self._peek() == TT.LPAREN:
            self._advance()
            columns.append(self._expect(TT.IDENT).value)
            while self._peek() == TT.COMMA:
                self._advance()
                columns.append(self._expect(TT.IDENT).value)
            self._expect(TT.RPAREN)
        self._expect(TT.VALUES)
        all_values: list[list[ASTNode]] = []
        all_values.append(self._parse_value_row())
        while self._peek() == TT.COMMA:
            self._advance()
            all_values.append(self._parse_value_row())
        return InsertStmt(table=table, columns=columns, values=all_values)

    def _parse_value_row(self) -> list[ASTNode]:
        self._expect(TT.LPAREN)
        vals: list[ASTNode] = [self._parse_expr()]
        while self._peek() == TT.COMMA:
            self._advance()
            vals.append(self._parse_expr())
        self._expect(TT.RPAREN)
        return vals

    def _parse_update(self) -> UpdateStmt:
        self._expect(TT.UPDATE)
        table = self._expect(TT.IDENT).value
        self._expect(TT.SET)
        assignments: list[tuple[str, ASTNode]] = []
        col = self._expect(TT.IDENT).value
        self._expect(TT.EQ)
        val = self._parse_expr()
        assignments.append((col, val))
        while self._peek() == TT.COMMA:
            self._advance()
            col = self._expect(TT.IDENT).value
            self._expect(TT.EQ)
            val = self._parse_expr()
            assignments.append((col, val))
        where = None
        if self._peek() == TT.WHERE:
            self._advance()
            where = self._parse_expr()
        return UpdateStmt(table=table, assignments=assignments, where=where)

    def _parse_delete(self) -> DeleteStmt:
        self._expect(TT.DELETE)
        self._expect(TT.FROM)
        table = self._expect(TT.IDENT).value
        where = None
        if self._peek() == TT.WHERE:
            self._advance()
            where = self._parse_expr()
        return DeleteStmt(table=table, where=where)

    def _parse_create(self) -> CreateTableStmt:
        self._expect(TT.CREATE)
        self._expect(TT.TABLE)
        table = self._expect(TT.IDENT).value
        self._expect(TT.LPAREN)
        columns: list[ColumnDef] = []
        columns.append(self._parse_column_def())
        while self._peek() == TT.COMMA:
            self._advance()
            if self._peek() in (TT.PRIMARY, TT.UNIQUE, TT.CHECK, TT.FOREIGN, TT.CONSTRAINT):
                break  # Table-level constraints — skip for now
            columns.append(self._parse_column_def())
        self._expect(TT.RPAREN)
        return CreateTableStmt(table=table, columns=columns)

    def _parse_column_def(self) -> ColumnDef:
        name = self._expect(TT.IDENT).value
        dtype = self._expect(TT.IDENT).value
        # Optional size: VARCHAR(255)
        if self._peek() == TT.LPAREN:
            self._advance()
            self._expect(TT.NUMBER)
            self._expect(TT.RPAREN)
            dtype += "(..)"

        nullable = True
        pk = False
        unique = False
        default = None

        while self._peek() in (TT.NOT, TT.PRIMARY, TT.UNIQUE, TT.DEFAULT):
            if self._peek() == TT.NOT:
                self._advance()
                self._expect(TT.NULL)
                nullable = False
            elif self._peek() == TT.PRIMARY:
                self._advance()
                self._expect(TT.KEY)
                pk = True
                nullable = False
            elif self._peek() == TT.UNIQUE:
                self._advance()
                unique = True
            elif self._peek() == TT.DEFAULT:
                self._advance()
                default = self._parse_primary()

        return ColumnDef(name=name, data_type=dtype, nullable=nullable,
                         primary_key=pk, unique=unique, default=default)

    # ── Expression parsing (precedence climbing) ───────────────────

    def _parse_expr(self) -> ASTNode:
        return self._parse_or()

    def _parse_or(self) -> ASTNode:
        left = self._parse_and()
        while self._peek() == TT.OR:
            self._advance()
            right = self._parse_and()
            left = BinaryOp("OR", left, right)
        return left

    def _parse_and(self) -> ASTNode:
        left = self._parse_not()
        while self._peek() == TT.AND:
            self._advance()
            right = self._parse_not()
            left = BinaryOp("AND", left, right)
        return left

    def _parse_not(self) -> ASTNode:
        if self._peek() == TT.NOT:
            self._advance()
            return UnaryOp("NOT", self._parse_not())
        return self._parse_comparison()

    def _parse_comparison(self) -> ASTNode:
        left = self._parse_addition()

        # IS [NOT] NULL
        if self._peek() == TT.IS:
            self._advance()
            negate = False
            if self._peek() == TT.NOT:
                self._advance()
                negate = True
            self._expect(TT.NULL)
            return IsNullExpr(expr=left, negate=negate)

        # [NOT] BETWEEN low AND high
        negate = False
        if self._peek() == TT.NOT:
            saved = self._pos
            self._advance()
            if self._peek() in (TT.BETWEEN, TT.IN, TT.LIKE):
                negate = True
            else:
                self._pos = saved
                return left

        if self._peek() == TT.BETWEEN:
            self._advance()
            low = self._parse_addition()
            self._expect(TT.AND)
            high = self._parse_addition()
            return BetweenExpr(expr=left, low=low, high=high, negate=negate)

        # [NOT] IN (values)
        if self._peek() == TT.IN:
            self._advance()
            self._expect(TT.LPAREN)
            vals: list[ASTNode] = [self._parse_expr()]
            while self._peek() == TT.COMMA:
                self._advance()
                vals.append(self._parse_expr())
            self._expect(TT.RPAREN)
            return InList(expr=left, values=vals, negate=negate)

        # [NOT] LIKE pattern
        if self._peek() == TT.LIKE:
            self._advance()
            pattern = self._parse_primary()
            return LikeExpr(expr=left, pattern=pattern, negate=negate)

        # Comparison operators
        op_map = {TT.EQ: "=", TT.NEQ: "!=", TT.LT: "<", TT.GT: ">",
                  TT.LTE: "<=", TT.GTE: ">="}
        if self._peek() in op_map:
            op = op_map[self._peek()]
            self._advance()
            right = self._parse_addition()
            return BinaryOp(op, left, right)

        return left

    def _parse_addition(self) -> ASTNode:
        left = self._parse_multiplication()
        while self._peek() in (TT.PLUS, TT.MINUS):
            op = "+" if self._peek() == TT.PLUS else "-"
            self._advance()
            right = self._parse_multiplication()
            left = BinaryOp(op, left, right)
        return left

    def _parse_multiplication(self) -> ASTNode:
        left = self._parse_unary()
        while self._peek() in (TT.STAR, TT.SLASH):
            op = "*" if self._peek() == TT.STAR else "/"
            self._advance()
            right = self._parse_unary()
            left = BinaryOp(op, left, right)
        return left

    def _parse_unary(self) -> ASTNode:
        if self._peek() == TT.MINUS:
            self._advance()
            return UnaryOp("-", self._parse_primary())
        return self._parse_primary()

    def _parse_primary(self) -> ASTNode:
        tok = self._current()

        # NULL literal
        if tok.type == TT.NULL:
            self._advance()
            return Literal(None)

        # Boolean literals
        if tok.type == TT.TRUE:
            self._advance()
            return Literal(True)
        if tok.type == TT.FALSE:
            self._advance()
            return Literal(False)

        # Number
        if tok.type == TT.NUMBER:
            self._advance()
            val = tok.value
            return Literal(int(val) if "." not in val else float(val))

        # String
        if tok.type == TT.STRING:
            self._advance()
            return Literal(tok.value)

        # Parenthesized expression
        if tok.type == TT.LPAREN:
            self._advance()
            expr = self._parse_expr()
            self._expect(TT.RPAREN)
            return expr

        # CASE expression
        if tok.type == TT.CASE:
            return self._parse_case()

        # * (star)
        if tok.type == TT.STAR:
            self._advance()
            return StarExpr()

        # Identifier — column ref, function call, or qualified name
        if tok.type == TT.IDENT:
            self._advance()
            name = tok.value

            # Function call: name(...)
            if self._peek() == TT.LPAREN:
                return self._parse_function_call(name)

            # Qualified reference: table.column
            if self._peek() == TT.DOT:
                self._advance()
                if self._peek() == TT.STAR:
                    self._advance()
                    return StarExpr(table=name)
                col = self._expect(TT.IDENT).value
                return ColumnRef(table=name, column=col)

            return ColumnRef(table=None, column=name)

        raise SQLSyntaxError(f"Unexpected token {tok.value!r} (type={tok.type.name}) at pos {tok.pos}")

    def _parse_function_call(self, name: str) -> ASTNode:
        self._expect(TT.LPAREN)
        distinct = False
        args: list[ASTNode] = []
        if self._peek() != TT.RPAREN:
            if self._peek() == TT.DISTINCT:
                self._advance()
                distinct = True
            if self._peek() == TT.STAR:
                self._advance()
                args.append(StarExpr())
            else:
                args.append(self._parse_expr())
                while self._peek() == TT.COMMA:
                    self._advance()
                    args.append(self._parse_expr())
        self._expect(TT.RPAREN)

        func = FunctionCall(name=name.upper(), args=args, distinct=distinct)

        # Window function: func(...) OVER (...)
        if self._peek() == TT.OVER:
            self._advance()
            self._expect(TT.LPAREN)
            partition_by: list[ASTNode] = []
            order_by: list[tuple[ASTNode, bool]] = []
            if self._peek() == TT.PARTITION:
                self._advance()
                self._expect(TT.BY)
                partition_by.append(self._parse_expr())
                while self._peek() == TT.COMMA:
                    self._advance()
                    partition_by.append(self._parse_expr())
            if self._peek() == TT.ORDER:
                self._advance()
                self._expect(TT.BY)
                order_by = self._parse_order_list()
            self._expect(TT.RPAREN)
            return WindowExpr(func=func, partition_by=partition_by, order_by=order_by)

        return func

    def _parse_case(self) -> CaseExpr:
        self._expect(TT.CASE)
        operand = None
        when_clauses: list[tuple[ASTNode, ASTNode]] = []
        else_clause = None

        # Simple CASE: CASE expr WHEN val THEN result ...
        if self._peek() != TT.WHEN:
            operand = self._parse_expr()

        while self._peek() == TT.WHEN:
            self._advance()
            cond = self._parse_expr()
            self._expect(TT.THEN)
            result = self._parse_expr()
            when_clauses.append((cond, result))

        if self._peek() == TT.ELSE:
            self._advance()
            else_clause = self._parse_expr()

        self._expect(TT.END)
        return CaseExpr(operand=operand, when_clauses=when_clauses, else_clause=else_clause)

    # ── Token helpers ──────────────────────────────────────────────

    def _current(self) -> Token:
        return self._tokens[self._pos]

    def _peek(self) -> TT:
        return self._tokens[self._pos].type

    def _advance(self) -> Token:
        tok = self._tokens[self._pos]
        if self._pos < len(self._tokens) - 1:
            self._pos += 1
        return tok

    def _expect(self, tt: TT) -> Token:
        tok = self._current()
        if tok.type != tt:
            raise SQLSyntaxError(
                f"Expected {tt.name}, got {tok.type.name} ({tok.value!r}) at pos {tok.pos}"
            )
        return self._advance()


# ── Convenience ────────────────────────────────────────────────────

def parse_sql(sql: str) -> ASTNode:
    """Parse a single SQL statement."""
    return SQLParser(sql).parse()


def parse_sql_multi(sql: str) -> list[ASTNode]:
    """Parse multiple SQL statements."""
    return SQLParser(sql).parse_multi()
