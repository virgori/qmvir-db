"""QM PL/QM — Lightweight procedural scripting language.

Supports:
    - DECLARE variable [TYPE] [= expr]
    - SET variable = expr
    - IF condition THEN ... [ELIF ... THEN ...] [ELSE ...] END IF
    - WHILE condition DO ... END WHILE
    - FOR var IN list DO ... END FOR
    - RETURN expr
    - RAISE message
    - SQL passthrough: SELECT, INSERT, UPDATE, DELETE via engine binding
    - Built-in functions: NOW(), COALESCE(), LEN(), ABS(), UPPER(), LOWER()
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from enum import Enum, auto
from typing import Any, Callable


class PLQMError(Exception):
    """Error raised by PL/QM interpreter."""
    pass


class TokenType(Enum):
    # Keywords
    DECLARE = auto()
    SET = auto()
    IF = auto()
    THEN = auto()
    ELIF = auto()
    ELSE = auto()
    END = auto()
    WHILE = auto()
    DO = auto()
    FOR = auto()
    IN = auto()
    RETURN = auto()
    RAISE = auto()
    AND = auto()
    OR = auto()
    NOT = auto()
    IS = auto()
    NULL = auto()
    TRUE = auto()
    FALSE = auto()
    # Operators
    EQ = auto()       # =
    NEQ = auto()      # != or <>
    LT = auto()       # <
    GT = auto()       # >
    LTE = auto()      # <=
    GTE = auto()      # >=
    PLUS = auto()     # +
    MINUS = auto()    # -
    STAR = auto()     # *
    SLASH = auto()    # /
    MOD = auto()      # %
    LPAREN = auto()   # (
    RPAREN = auto()   # )
    COMMA = auto()    # ,
    SEMICOLON = auto()# ;
    DOT = auto()      # .
    ASSIGN = auto()   # :=
    # Literals
    NUMBER = auto()
    STRING = auto()
    IDENT = auto()
    # Special
    EOF = auto()


KEYWORDS = {
    "declare": TokenType.DECLARE,
    "set": TokenType.SET,
    "if": TokenType.IF,
    "then": TokenType.THEN,
    "elif": TokenType.ELIF,
    "else": TokenType.ELSE,
    "end": TokenType.END,
    "while": TokenType.WHILE,
    "do": TokenType.DO,
    "for": TokenType.FOR,
    "in": TokenType.IN,
    "return": TokenType.RETURN,
    "raise": TokenType.RAISE,
    "and": TokenType.AND,
    "or": TokenType.OR,
    "not": TokenType.NOT,
    "is": TokenType.IS,
    "null": TokenType.NULL,
    "true": TokenType.TRUE,
    "false": TokenType.FALSE,
}


@dataclass
class Token:
    type: TokenType
    value: Any
    line: int = 0


class Lexer:
    """Tokenize PL/QM source code."""

    def __init__(self, source: str) -> None:
        self._src = source
        self._pos = 0
        self._line = 1

    def tokenize(self) -> list[Token]:
        tokens: list[Token] = []
        while self._pos < len(self._src):
            c = self._src[self._pos]

            # Whitespace
            if c in " \t\r":
                self._pos += 1
                continue
            if c == "\n":
                self._line += 1
                self._pos += 1
                continue

            # Comments (-- line comment)
            if c == "-" and self._peek(1) == "-":
                while self._pos < len(self._src) and self._src[self._pos] != "\n":
                    self._pos += 1
                continue

            # Multi-char operators
            if c == ":" and self._peek(1) == "=":
                tokens.append(Token(TokenType.ASSIGN, ":=", self._line))
                self._pos += 2
                continue
            if c == "<" and self._peek(1) == "=":
                tokens.append(Token(TokenType.LTE, "<=", self._line))
                self._pos += 2
                continue
            if c == ">" and self._peek(1) == "=":
                tokens.append(Token(TokenType.GTE, ">=", self._line))
                self._pos += 2
                continue
            if c == "!" and self._peek(1) == "=":
                tokens.append(Token(TokenType.NEQ, "!=", self._line))
                self._pos += 2
                continue
            if c == "<" and self._peek(1) == ">":
                tokens.append(Token(TokenType.NEQ, "<>", self._line))
                self._pos += 2
                continue

            # Single-char operators
            op_map = {
                "=": TokenType.EQ, "<": TokenType.LT, ">": TokenType.GT,
                "+": TokenType.PLUS, "-": TokenType.MINUS, "*": TokenType.STAR,
                "/": TokenType.SLASH, "%": TokenType.MOD, "(": TokenType.LPAREN,
                ")": TokenType.RPAREN, ",": TokenType.COMMA, ";": TokenType.SEMICOLON,
                ".": TokenType.DOT,
            }
            if c in op_map:
                tokens.append(Token(op_map[c], c, self._line))
                self._pos += 1
                continue

            # Numbers
            if c.isdigit():
                start = self._pos
                while self._pos < len(self._src) and (self._src[self._pos].isdigit() or self._src[self._pos] == "."):
                    self._pos += 1
                val = self._src[start:self._pos]
                tokens.append(Token(TokenType.NUMBER, float(val) if "." in val else int(val), self._line))
                continue

            # Strings
            if c in ("'", '"'):
                tokens.append(self._read_string(c))
                continue

            # Identifiers / keywords
            if c.isalpha() or c == "_":
                start = self._pos
                while self._pos < len(self._src) and (self._src[self._pos].isalnum() or self._src[self._pos] == "_"):
                    self._pos += 1
                word = self._src[start:self._pos]
                tt = KEYWORDS.get(word.lower(), TokenType.IDENT)
                tokens.append(Token(tt, word, self._line))
                continue

            raise PLQMError(f"Unexpected character '{c}' at line {self._line}")

        tokens.append(Token(TokenType.EOF, None, self._line))
        return tokens

    def _peek(self, offset: int) -> str:
        idx = self._pos + offset
        return self._src[idx] if idx < len(self._src) else ""

    def _read_string(self, quote: str) -> Token:
        self._pos += 1  # skip opening quote
        start = self._pos
        while self._pos < len(self._src) and self._src[self._pos] != quote:
            if self._src[self._pos] == "\\":
                self._pos += 1  # skip escaped char
            self._pos += 1
        if self._pos >= len(self._src):
            raise PLQMError(f"Unterminated string at line {self._line}")
        val = self._src[start:self._pos]
        self._pos += 1  # skip closing quote
        return Token(TokenType.STRING, val, self._line)


# ── AST Nodes ───────────────────────────────────────────────────────

@dataclass
class ASTNode:
    pass

@dataclass
class DeclareNode(ASTNode):
    name: str
    type_hint: str | None = None
    init_expr: ASTNode | None = None

@dataclass
class SetNode(ASTNode):
    name: str
    expr: ASTNode | None = None

@dataclass
class IfNode(ASTNode):
    condition: ASTNode
    then_body: list[ASTNode]
    elif_branches: list[tuple[ASTNode, list[ASTNode]]] = field(default_factory=list)
    else_body: list[ASTNode] | None = None

@dataclass
class WhileNode(ASTNode):
    condition: ASTNode
    body: list[ASTNode]

@dataclass
class ForNode(ASTNode):
    var_name: str
    iterable: ASTNode
    body: list[ASTNode]

@dataclass
class ReturnNode(ASTNode):
    expr: ASTNode | None = None

@dataclass
class RaiseNode(ASTNode):
    message: ASTNode

@dataclass
class BinaryOpNode(ASTNode):
    op: str
    left: ASTNode
    right: ASTNode

@dataclass
class UnaryOpNode(ASTNode):
    op: str
    operand: ASTNode

@dataclass
class LiteralNode(ASTNode):
    value: Any

@dataclass
class IdentNode(ASTNode):
    name: str

@dataclass
class FuncCallNode(ASTNode):
    name: str
    args: list[ASTNode]

@dataclass
class ListNode(ASTNode):
    elements: list[ASTNode]

@dataclass
class IndexNode(ASTNode):
    obj: ASTNode
    key: str


# ── Parser ──────────────────────────────────────────────────────────

class Parser:
    """Parse PL/QM tokens into an AST."""

    def __init__(self, tokens: list[Token]) -> None:
        self._tokens = tokens
        self._pos = 0

    def parse(self) -> list[ASTNode]:
        stmts: list[ASTNode] = []
        while not self._at_end():
            stmt = self._parse_statement()
            if stmt is not None:
                stmts.append(stmt)
        return stmts

    def _parse_statement(self) -> ASTNode | None:
        self._skip_semicolons()
        if self._at_end():
            return None

        tok = self._current()

        if tok.type == TokenType.DECLARE:
            return self._parse_declare()
        if tok.type == TokenType.SET:
            return self._parse_set()
        if tok.type == TokenType.IF:
            return self._parse_if()
        if tok.type == TokenType.WHILE:
            return self._parse_while()
        if tok.type == TokenType.FOR:
            return self._parse_for()
        if tok.type == TokenType.RETURN:
            return self._parse_return()
        if tok.type == TokenType.RAISE:
            return self._parse_raise()

        # Expression statement (function calls, etc.)
        expr = self._parse_expr()
        self._skip_semicolons()
        return expr

    def _parse_declare(self) -> DeclareNode:
        self._advance()  # consume DECLARE
        name = self._expect(TokenType.IDENT).value
        type_hint = None
        init_expr = None

        if self._check(TokenType.IDENT):
            type_hint = self._advance().value

        if self._check(TokenType.EQ) or self._check(TokenType.ASSIGN):
            self._advance()
            init_expr = self._parse_expr()

        self._skip_semicolons()
        return DeclareNode(name=name, type_hint=type_hint, init_expr=init_expr)

    def _parse_set(self) -> SetNode:
        self._advance()  # consume SET
        name = self._expect(TokenType.IDENT).value
        self._expect_any(TokenType.EQ, TokenType.ASSIGN)
        expr = self._parse_expr()
        self._skip_semicolons()
        return SetNode(name=name, expr=expr)

    def _parse_if(self) -> IfNode:
        self._advance()  # consume IF
        condition = self._parse_expr()
        self._expect(TokenType.THEN)
        then_body = self._parse_body("elif", "else", "end")

        elif_branches: list[tuple[ASTNode, list[ASTNode]]] = []
        while self._check(TokenType.ELIF):
            self._advance()
            cond = self._parse_expr()
            self._expect(TokenType.THEN)
            body = self._parse_body("elif", "else", "end")
            elif_branches.append((cond, body))

        else_body = None
        if self._check(TokenType.ELSE):
            self._advance()
            else_body = self._parse_body("end")

        self._expect(TokenType.END)
        # Consume optional "IF" after "END"
        if self._check(TokenType.IF):
            self._advance()
        self._skip_semicolons()

        return IfNode(condition=condition, then_body=then_body,
                      elif_branches=elif_branches, else_body=else_body)

    def _parse_while(self) -> WhileNode:
        self._advance()  # consume WHILE
        condition = self._parse_expr()
        self._expect(TokenType.DO)
        body = self._parse_body("end")
        self._expect(TokenType.END)
        # Consume optional "WHILE" after "END"
        if self._check(TokenType.WHILE):
            self._advance()
        self._skip_semicolons()
        return WhileNode(condition=condition, body=body)

    def _parse_for(self) -> ForNode:
        self._advance()  # consume FOR
        var_name = self._expect(TokenType.IDENT).value
        self._expect(TokenType.IN)
        iterable = self._parse_expr()
        self._expect(TokenType.DO)
        body = self._parse_body("end")
        self._expect(TokenType.END)
        # Consume optional "FOR" after "END"
        if self._check(TokenType.FOR):
            self._advance()
        self._skip_semicolons()
        return ForNode(var_name=var_name, iterable=iterable, body=body)

    def _parse_return(self) -> ReturnNode:
        self._advance()  # consume RETURN
        expr = None
        if not self._check(TokenType.SEMICOLON) and not self._at_end():
            expr = self._parse_expr()
        self._skip_semicolons()
        return ReturnNode(expr=expr)

    def _parse_raise(self) -> RaiseNode:
        self._advance()  # consume RAISE
        msg = self._parse_expr()
        self._skip_semicolons()
        return RaiseNode(message=msg)

    def _parse_body(self, *terminators: str) -> list[ASTNode]:
        """Parse statements until a terminator keyword is seen."""
        stmts: list[ASTNode] = []
        term_types = {KEYWORDS.get(t.lower()) for t in terminators if KEYWORDS.get(t.lower())}
        while not self._at_end():
            if self._current().type in term_types:
                break
            stmt = self._parse_statement()
            if stmt is not None:
                stmts.append(stmt)
        return stmts

    # ── Expression parsing (precedence climbing) ────────────────────

    def _parse_expr(self) -> ASTNode:
        return self._parse_or()

    def _parse_or(self) -> ASTNode:
        left = self._parse_and()
        while self._check(TokenType.OR):
            self._advance()
            right = self._parse_and()
            left = BinaryOpNode(op="or", left=left, right=right)
        return left

    def _parse_and(self) -> ASTNode:
        left = self._parse_not()
        while self._check(TokenType.AND):
            self._advance()
            right = self._parse_not()
            left = BinaryOpNode(op="and", left=left, right=right)
        return left

    def _parse_not(self) -> ASTNode:
        if self._check(TokenType.NOT):
            self._advance()
            return UnaryOpNode(op="not", operand=self._parse_comparison())
        return self._parse_comparison()

    def _parse_comparison(self) -> ASTNode:
        left = self._parse_addition()
        ops = {TokenType.EQ: "=", TokenType.NEQ: "!=", TokenType.LT: "<",
               TokenType.GT: ">", TokenType.LTE: "<=", TokenType.GTE: ">="}
        if self._current().type in ops:
            op = ops[self._advance().type]
            right = self._parse_addition()
            return BinaryOpNode(op=op, left=left, right=right)
        # IS NULL / IS NOT NULL
        if self._check(TokenType.IS):
            self._advance()
            if self._check(TokenType.NOT):
                self._advance()
                self._expect(TokenType.NULL)
                return UnaryOpNode(op="is_not_null", operand=left)
            self._expect(TokenType.NULL)
            return UnaryOpNode(op="is_null", operand=left)
        return left

    def _parse_addition(self) -> ASTNode:
        left = self._parse_multiplication()
        while self._current().type in (TokenType.PLUS, TokenType.MINUS):
            op = "+" if self._advance().type == TokenType.PLUS else "-"
            right = self._parse_multiplication()
            left = BinaryOpNode(op=op, left=left, right=right)
        return left

    def _parse_multiplication(self) -> ASTNode:
        left = self._parse_unary()
        while self._current().type in (TokenType.STAR, TokenType.SLASH, TokenType.MOD):
            tok = self._advance()
            op = {TokenType.STAR: "*", TokenType.SLASH: "/", TokenType.MOD: "%"}[tok.type]
            right = self._parse_unary()
            left = BinaryOpNode(op=op, left=left, right=right)
        return left

    def _parse_unary(self) -> ASTNode:
        if self._check(TokenType.MINUS):
            self._advance()
            return UnaryOpNode(op="-", operand=self._parse_primary())
        return self._parse_primary()

    def _parse_primary(self) -> ASTNode:
        tok = self._current()

        if tok.type == TokenType.NUMBER:
            self._advance()
            return LiteralNode(value=tok.value)

        if tok.type == TokenType.STRING:
            self._advance()
            return LiteralNode(value=tok.value)

        if tok.type == TokenType.TRUE:
            self._advance()
            return LiteralNode(value=True)

        if tok.type == TokenType.FALSE:
            self._advance()
            return LiteralNode(value=False)

        if tok.type == TokenType.NULL:
            self._advance()
            return LiteralNode(value=None)

        if tok.type == TokenType.LPAREN:
            self._advance()
            expr = self._parse_expr()
            self._expect(TokenType.RPAREN)
            return expr

        if tok.type == TokenType.IDENT:
            name = self._advance().value
            # Function call
            if self._check(TokenType.LPAREN):
                self._advance()
                args: list[ASTNode] = []
                if not self._check(TokenType.RPAREN):
                    args.append(self._parse_expr())
                    while self._check(TokenType.COMMA):
                        self._advance()
                        args.append(self._parse_expr())
                self._expect(TokenType.RPAREN)
                return FuncCallNode(name=name, args=args)
            # Dot access (e.g., NEW.field)
            if self._check(TokenType.DOT):
                self._advance()
                attr = self._expect(TokenType.IDENT).value
                return IndexNode(obj=IdentNode(name=name), key=attr)
            return IdentNode(name=name)

        raise PLQMError(f"Unexpected token {tok.type.name} '{tok.value}' at line {tok.line}")

    # ── Token helpers ───────────────────────────────────────────────

    def _current(self) -> Token:
        return self._tokens[self._pos] if self._pos < len(self._tokens) else Token(TokenType.EOF, None)

    def _advance(self) -> Token:
        tok = self._current()
        self._pos += 1
        return tok

    def _check(self, tt: TokenType) -> bool:
        return self._current().type == tt

    def _expect(self, tt: TokenType) -> Token:
        tok = self._current()
        if tok.type != tt:
            raise PLQMError(f"Expected {tt.name}, got {tok.type.name} '{tok.value}' at line {tok.line}")
        return self._advance()

    def _expect_any(self, *types: TokenType) -> Token:
        tok = self._current()
        if tok.type not in types:
            raise PLQMError(f"Expected one of {[t.name for t in types]}, got {tok.type.name}")
        return self._advance()

    def _at_end(self) -> bool:
        return self._pos >= len(self._tokens) or self._tokens[self._pos].type == TokenType.EOF

    def _skip_semicolons(self) -> None:
        while self._check(TokenType.SEMICOLON):
            self._advance()


# ── Return signal ───────────────────────────────────────────────────

class _ReturnSignal(Exception):
    """Internal signal for RETURN statement."""
    def __init__(self, value: Any = None):
        self.value = value


# ── Interpreter ─────────────────────────────────────────────────────

class PLQMInterpreter:
    """Execute PL/QM AST with variable scoping and engine binding.

    Usage:
        interp = PLQMInterpreter()
        interp.register_builtin("db_read", lambda table, pk: engine.read(...))
        result = interp.execute(source, params={"table_name": "articles"})
    """

    MAX_ITERATIONS = 100_000  # Guard against infinite loops

    def __init__(self) -> None:
        self._builtins: dict[str, Callable] = {
            "now": lambda: time.time(),
            "coalesce": lambda *args: next((a for a in args if a is not None), None),
            "len": lambda x: len(x) if x is not None else 0,
            "abs": lambda x: abs(x),
            "upper": lambda x: str(x).upper(),
            "lower": lambda x: str(x).lower(),
            "str": lambda x: str(x),
            "int": lambda x: int(x),
            "float": lambda x: float(x),
            "list": lambda *args: list(args),
            "range": lambda *args: list(range(*[int(a) for a in args])),
            "print": lambda *args: None,  # No-op in production
        }

    def register_builtin(self, name: str, fn: Callable) -> None:
        """Register a built-in function callable from PL/QM."""
        self._builtins[name.lower()] = fn

    def execute(self, source: str, params: dict[str, Any] | None = None) -> Any:
        """Parse and execute PL/QM source. Returns the RETURN value or None."""
        tokens = Lexer(source).tokenize()
        ast = Parser(tokens).parse()
        scope: dict[str, Any] = dict(params or {})
        try:
            self._exec_block(ast, scope)
        except _ReturnSignal as ret:
            return ret.value
        return None

    def execute_ast(self, ast: list[ASTNode], params: dict[str, Any] | None = None) -> Any:
        """Execute pre-parsed AST."""
        scope: dict[str, Any] = dict(params or {})
        try:
            self._exec_block(ast, scope)
        except _ReturnSignal as ret:
            return ret.value
        return None

    # ── Statement execution ─────────────────────────────────────────

    def _exec_block(self, stmts: list[ASTNode], scope: dict[str, Any]) -> None:
        for stmt in stmts:
            self._exec_stmt(stmt, scope)

    def _exec_stmt(self, node: ASTNode, scope: dict[str, Any]) -> None:
        if isinstance(node, DeclareNode):
            val = self._eval(node.init_expr, scope) if node.init_expr else None
            scope[node.name] = val

        elif isinstance(node, SetNode):
            scope[node.name] = self._eval(node.expr, scope)

        elif isinstance(node, IfNode):
            if self._truthy(self._eval(node.condition, scope)):
                self._exec_block(node.then_body, scope)
            else:
                matched = False
                for cond, body in node.elif_branches:
                    if self._truthy(self._eval(cond, scope)):
                        self._exec_block(body, scope)
                        matched = True
                        break
                if not matched and node.else_body:
                    self._exec_block(node.else_body, scope)

        elif isinstance(node, WhileNode):
            iterations = 0
            while self._truthy(self._eval(node.condition, scope)):
                self._exec_block(node.body, scope)
                iterations += 1
                if iterations > self.MAX_ITERATIONS:
                    raise PLQMError("Maximum iterations exceeded in WHILE loop")

        elif isinstance(node, ForNode):
            iterable = self._eval(node.iterable, scope)
            if not hasattr(iterable, "__iter__"):
                raise PLQMError(f"FOR requires an iterable, got {type(iterable).__name__}")
            iterations = 0
            for item in iterable:
                scope[node.var_name] = item
                self._exec_block(node.body, scope)
                iterations += 1
                if iterations > self.MAX_ITERATIONS:
                    raise PLQMError("Maximum iterations exceeded in FOR loop")

        elif isinstance(node, ReturnNode):
            val = self._eval(node.expr, scope) if node.expr else None
            raise _ReturnSignal(val)

        elif isinstance(node, RaiseNode):
            msg = self._eval(node.message, scope)
            raise PLQMError(str(msg))

        else:
            # Expression statement (e.g., function calls)
            self._eval(node, scope)

    # ── Expression evaluation ───────────────────────────────────────

    def _eval(self, node: ASTNode, scope: dict[str, Any]) -> Any:
        if isinstance(node, LiteralNode):
            return node.value

        if isinstance(node, IdentNode):
            name = node.name
            if name in scope:
                return scope[name]
            raise PLQMError(f"Undefined variable: {name}")

        if isinstance(node, IndexNode):
            obj = self._eval(node.obj, scope)
            if isinstance(obj, dict):
                return obj.get(node.key)
            return getattr(obj, node.key, None)

        if isinstance(node, FuncCallNode):
            name_lower = node.name.lower()
            fn = self._builtins.get(name_lower)
            if fn is None:
                # Check scope for callable
                fn = scope.get(node.name)
                if fn is None or not callable(fn):
                    raise PLQMError(f"Undefined function: {node.name}")
            args = [self._eval(a, scope) for a in node.args]
            return fn(*args)

        if isinstance(node, BinaryOpNode):
            left = self._eval(node.left, scope)
            right = self._eval(node.right, scope)
            return self._binary_op(node.op, left, right)

        if isinstance(node, UnaryOpNode):
            operand = self._eval(node.operand, scope)
            if node.op == "-":
                return -operand
            if node.op == "not":
                return not self._truthy(operand)
            if node.op == "is_null":
                return operand is None
            if node.op == "is_not_null":
                return operand is not None
            raise PLQMError(f"Unknown unary op: {node.op}")

        if isinstance(node, ListNode):
            return [self._eval(e, scope) for e in node.elements]

        raise PLQMError(f"Cannot evaluate AST node type: {type(node).__name__}")

    def _binary_op(self, op: str, left: Any, right: Any) -> Any:
        if op == "+":
            if isinstance(left, str) or isinstance(right, str):
                return str(left) + str(right)
            return left + right
        if op == "-":
            return left - right
        if op == "*":
            return left * right
        if op == "/":
            if right == 0:
                raise PLQMError("Division by zero")
            return left / right
        if op == "%":
            return left % right
        if op == "=":
            return left == right
        if op == "!=":
            return left != right
        if op == "<":
            return left < right
        if op == ">":
            return left > right
        if op == "<=":
            return left <= right
        if op == ">=":
            return left >= right
        if op == "and":
            return self._truthy(left) and self._truthy(right)
        if op == "or":
            return self._truthy(left) or self._truthy(right)
        raise PLQMError(f"Unknown binary op: {op}")

    @staticmethod
    def _truthy(value: Any) -> bool:
        if value is None:
            return False
        if isinstance(value, bool):
            return value
        if isinstance(value, (int, float)):
            return value != 0
        if isinstance(value, str):
            return len(value) > 0
        return True
