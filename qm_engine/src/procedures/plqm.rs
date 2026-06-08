/*
 * PL/QM — Lightweight procedural scripting language interpreter.
 *
 * Port of _py_legacy/qm_core/procedures/plqm.py
 *
 * Supports:
 *   DECLARE var [TYPE] [= expr]
 *   SET var = expr
 *   IF cond THEN … [ELIF … THEN …] [ELSE …] END IF
 *   WHILE cond DO … END WHILE
 *   FOR var IN expr DO … END FOR
 *   RETURN [expr]
 *   RAISE expr
 *   Built-ins: now(), coalesce(), len(), abs(), upper(), lower(),
 *              str(), int(), float(), range(), print()
 *   Registered callbacks (db_read, db_write, etc.)
 */

use std::collections::HashMap;
use std::sync::Arc;

// ── Value type ───────────────────────────────────────────────────────

/// Runtime value in PL/QM.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    List(Vec<Value>),
}

impl Value {
    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Float(f) => *f != 0.0,
            Value::Text(s) => !s.is_empty(),
            Value::List(v) => !v.is_empty(),
        }
    }

    pub fn as_text(&self) -> String {
        match self {
            Value::Null => "null".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Int(n) => n.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Text(s) => s.clone(),
            Value::List(v) => {
                let parts: Vec<String> = v.iter().map(|x| x.as_text()).collect();
                format!("[{}]", parts.join(", "))
            }
        }
    }

    pub fn as_i64(&self) -> Result<i64, PlqmError> {
        match self {
            Value::Int(n) => Ok(*n),
            Value::Float(f) => Ok(*f as i64),
            Value::Text(s) => s
                .parse::<i64>()
                .map_err(|_| PlqmError(format!("cannot convert '{}' to int", s))),
            Value::Bool(b) => Ok(if *b { 1 } else { 0 }),
            _ => Err(PlqmError("cannot convert to int".to_string())),
        }
    }

    pub fn as_f64(&self) -> Result<f64, PlqmError> {
        match self {
            Value::Float(f) => Ok(*f),
            Value::Int(n) => Ok(*n as f64),
            Value::Text(s) => s
                .parse::<f64>()
                .map_err(|_| PlqmError(format!("cannot convert '{}' to float", s))),
            Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            _ => Err(PlqmError("cannot convert to float".to_string())),
        }
    }
}

// ── Error type ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PlqmError(pub String);

impl std::fmt::Display for PlqmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PL/QM error: {}", self.0)
    }
}

impl std::error::Error for PlqmError {}

// ── Lexer ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Keywords
    Declare,
    Set,
    If,
    Then,
    Elif,
    Else,
    End,
    While,
    Do,
    For,
    In,
    Return,
    Raise,
    And,
    Or,
    Not,
    Is,
    Null,
    True,
    False,
    // Operators
    Eq,
    Neq,
    Lt,
    Gt,
    Lte,
    Gte,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    LParen,
    RParen,
    Comma,
    Semicolon,
    Dot,
    Assign,
    // Literals
    Number(i64),
    Float(f64),
    Str(String),
    Ident(String),
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub line: usize,
}

pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Self {
            src: src.as_bytes(),
            pos: 0,
            line: 1,
        }
    }

    pub fn tokenize(mut self) -> Result<Vec<Token>, PlqmError> {
        let mut tokens = Vec::new();
        while self.pos < self.src.len() {
            let c = self.src[self.pos];
            match c {
                b' ' | b'\t' | b'\r' => {
                    self.pos += 1;
                }
                b'\n' => {
                    self.line += 1;
                    self.pos += 1;
                }
                b'-' if self.peek(1) == Some(b'-') => {
                    while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                }
                b':' if self.peek(1) == Some(b'=') => {
                    tokens.push(Token {
                        kind: TokenKind::Assign,
                        line: self.line,
                    });
                    self.pos += 2;
                }
                b'<' if self.peek(1) == Some(b'=') => {
                    tokens.push(Token {
                        kind: TokenKind::Lte,
                        line: self.line,
                    });
                    self.pos += 2;
                }
                b'>' if self.peek(1) == Some(b'=') => {
                    tokens.push(Token {
                        kind: TokenKind::Gte,
                        line: self.line,
                    });
                    self.pos += 2;
                }
                b'!' if self.peek(1) == Some(b'=') => {
                    tokens.push(Token {
                        kind: TokenKind::Neq,
                        line: self.line,
                    });
                    self.pos += 2;
                }
                b'<' if self.peek(1) == Some(b'>') => {
                    tokens.push(Token {
                        kind: TokenKind::Neq,
                        line: self.line,
                    });
                    self.pos += 2;
                }
                b'=' => {
                    tokens.push(Token {
                        kind: TokenKind::Eq,
                        line: self.line,
                    });
                    self.pos += 1;
                }
                b'<' => {
                    tokens.push(Token {
                        kind: TokenKind::Lt,
                        line: self.line,
                    });
                    self.pos += 1;
                }
                b'>' => {
                    tokens.push(Token {
                        kind: TokenKind::Gt,
                        line: self.line,
                    });
                    self.pos += 1;
                }
                b'+' => {
                    tokens.push(Token {
                        kind: TokenKind::Plus,
                        line: self.line,
                    });
                    self.pos += 1;
                }
                b'-' => {
                    tokens.push(Token {
                        kind: TokenKind::Minus,
                        line: self.line,
                    });
                    self.pos += 1;
                }
                b'*' => {
                    tokens.push(Token {
                        kind: TokenKind::Star,
                        line: self.line,
                    });
                    self.pos += 1;
                }
                b'/' => {
                    tokens.push(Token {
                        kind: TokenKind::Slash,
                        line: self.line,
                    });
                    self.pos += 1;
                }
                b'%' => {
                    tokens.push(Token {
                        kind: TokenKind::Percent,
                        line: self.line,
                    });
                    self.pos += 1;
                }
                b'(' => {
                    tokens.push(Token {
                        kind: TokenKind::LParen,
                        line: self.line,
                    });
                    self.pos += 1;
                }
                b')' => {
                    tokens.push(Token {
                        kind: TokenKind::RParen,
                        line: self.line,
                    });
                    self.pos += 1;
                }
                b',' => {
                    tokens.push(Token {
                        kind: TokenKind::Comma,
                        line: self.line,
                    });
                    self.pos += 1;
                }
                b';' => {
                    tokens.push(Token {
                        kind: TokenKind::Semicolon,
                        line: self.line,
                    });
                    self.pos += 1;
                }
                b'.' => {
                    tokens.push(Token {
                        kind: TokenKind::Dot,
                        line: self.line,
                    });
                    self.pos += 1;
                }
                b'\'' | b'"' => {
                    let tok = self.read_string(c)?;
                    tokens.push(tok);
                }
                b'0'..=b'9' => {
                    let tok = self.read_number();
                    tokens.push(tok);
                }
                _ if c.is_ascii_alphabetic() || c == b'_' => {
                    let tok = self.read_ident();
                    tokens.push(tok);
                }
                _ => {
                    return Err(PlqmError(format!(
                        "unexpected character '{}' at line {}",
                        c as char, self.line
                    )));
                }
            }
        }
        tokens.push(Token {
            kind: TokenKind::Eof,
            line: self.line,
        });
        Ok(tokens)
    }

    fn peek(&self, offset: usize) -> Option<u8> {
        self.src.get(self.pos + offset).copied()
    }

    fn read_string(&mut self, quote: u8) -> Result<Token, PlqmError> {
        let line = self.line;
        self.pos += 1;
        let start = self.pos;
        while self.pos < self.src.len() && self.src[self.pos] != quote {
            if self.src[self.pos] == b'\\' {
                self.pos += 1;
            }
            self.pos += 1;
        }
        if self.pos >= self.src.len() {
            return Err(PlqmError(format!("unterminated string at line {}", line)));
        }
        let s = String::from_utf8_lossy(&self.src[start..self.pos]).to_string();
        self.pos += 1;
        Ok(Token {
            kind: TokenKind::Str(s),
            line,
        })
    }

    fn read_number(&mut self) -> Token {
        let line = self.line;
        let start = self.pos;
        let mut is_float = false;
        while self.pos < self.src.len()
            && (self.src[self.pos].is_ascii_digit() || self.src[self.pos] == b'.')
        {
            if self.src[self.pos] == b'.' {
                is_float = true;
            }
            self.pos += 1;
        }
        let s = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("0");
        if is_float {
            Token {
                kind: TokenKind::Float(s.parse().unwrap_or(0.0)),
                line,
            }
        } else {
            Token {
                kind: TokenKind::Number(s.parse().unwrap_or(0)),
                line,
            }
        }
    }

    fn read_ident(&mut self) -> Token {
        let line = self.line;
        let start = self.pos;
        while self.pos < self.src.len()
            && (self.src[self.pos].is_ascii_alphanumeric() || self.src[self.pos] == b'_')
        {
            self.pos += 1;
        }
        let word = std::str::from_utf8(&self.src[start..self.pos]).unwrap_or("");
        let kind = match word.to_ascii_lowercase().as_str() {
            "declare" => TokenKind::Declare,
            "set" => TokenKind::Set,
            "if" => TokenKind::If,
            "then" => TokenKind::Then,
            "elif" => TokenKind::Elif,
            "else" => TokenKind::Else,
            "end" => TokenKind::End,
            "while" => TokenKind::While,
            "do" => TokenKind::Do,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "return" => TokenKind::Return,
            "raise" => TokenKind::Raise,
            "and" => TokenKind::And,
            "or" => TokenKind::Or,
            "not" => TokenKind::Not,
            "is" => TokenKind::Is,
            "null" => TokenKind::Null,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            _ => TokenKind::Ident(word.to_string()),
        };
        Token { kind, line }
    }
}

// ── AST ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Value),
    Ident(String),
    Field(Box<Expr>, String),
    Call(String, Vec<Expr>),
    BinOp(String, Box<Expr>, Box<Expr>),
    Unary(String, Box<Expr>),
    List(Vec<Expr>),
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Declare {
        name: String,
        type_hint: Option<String>,
        init: Option<Expr>,
    },
    Set {
        name: String,
        expr: Expr,
    },
    If {
        cond: Expr,
        then_body: Vec<Stmt>,
        elif_branches: Vec<(Expr, Vec<Stmt>)>,
        else_body: Option<Vec<Stmt>>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    For {
        var: String,
        iter: Expr,
        body: Vec<Stmt>,
    },
    Return(Option<Expr>),
    Raise(Expr),
    Expr(Expr),
}

// ── Parser ───────────────────────────────────────────────────────────

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    pub fn parse(&mut self) -> Result<Vec<Stmt>, PlqmError> {
        let mut stmts = Vec::new();
        while !self.at_end() {
            self.skip_semis();
            if self.at_end() {
                break;
            }
            stmts.push(self.parse_stmt()?);
        }
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt, PlqmError> {
        self.skip_semis();
        match &self.cur().kind {
            TokenKind::Declare => self.parse_declare(),
            TokenKind::Set => self.parse_set(),
            TokenKind::If => self.parse_if(),
            TokenKind::While => self.parse_while(),
            TokenKind::For => self.parse_for(),
            TokenKind::Return => self.parse_return(),
            TokenKind::Raise => self.parse_raise(),
            _ => {
                let e = self.parse_expr()?;
                self.skip_semis();
                Ok(Stmt::Expr(e))
            }
        }
    }

    fn parse_declare(&mut self) -> Result<Stmt, PlqmError> {
        self.advance();
        let name = self.expect_ident()?;
        let type_hint = if matches!(self.cur().kind, TokenKind::Ident(_)) {
            Some(self.expect_ident()?)
        } else {
            None
        };
        let init = if matches!(self.cur().kind, TokenKind::Eq | TokenKind::Assign) {
            self.advance();
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.skip_semis();
        Ok(Stmt::Declare {
            name,
            type_hint,
            init,
        })
    }

    fn parse_set(&mut self) -> Result<Stmt, PlqmError> {
        self.advance();
        let name = self.expect_ident()?;
        if !matches!(self.cur().kind, TokenKind::Eq | TokenKind::Assign) {
            return Err(PlqmError(format!("expected = after SET {}", name)));
        }
        self.advance();
        let expr = self.parse_expr()?;
        self.skip_semis();
        Ok(Stmt::Set { name, expr })
    }

    fn parse_if(&mut self) -> Result<Stmt, PlqmError> {
        self.advance();
        let cond = self.parse_expr()?;
        self.expect_kw(TokenKind::Then)?;
        let then_body = self.parse_body(&[TokenKind::Elif, TokenKind::Else, TokenKind::End])?;
        let mut elif_branches = Vec::new();
        while matches!(self.cur().kind, TokenKind::Elif) {
            self.advance();
            let ec = self.parse_expr()?;
            self.expect_kw(TokenKind::Then)?;
            let eb = self.parse_body(&[TokenKind::Elif, TokenKind::Else, TokenKind::End])?;
            elif_branches.push((ec, eb));
        }
        let else_body = if matches!(self.cur().kind, TokenKind::Else) {
            self.advance();
            Some(self.parse_body(&[TokenKind::End])?)
        } else {
            None
        };
        self.expect_kw(TokenKind::End)?;
        if matches!(self.cur().kind, TokenKind::If) {
            self.advance();
        }
        self.skip_semis();
        Ok(Stmt::If {
            cond,
            then_body,
            elif_branches,
            else_body,
        })
    }

    fn parse_while(&mut self) -> Result<Stmt, PlqmError> {
        self.advance();
        let cond = self.parse_expr()?;
        self.expect_kw(TokenKind::Do)?;
        let body = self.parse_body(&[TokenKind::End])?;
        self.expect_kw(TokenKind::End)?;
        if matches!(self.cur().kind, TokenKind::While) {
            self.advance();
        }
        self.skip_semis();
        Ok(Stmt::While { cond, body })
    }

    fn parse_for(&mut self) -> Result<Stmt, PlqmError> {
        self.advance();
        let var = self.expect_ident()?;
        self.expect_kw(TokenKind::In)?;
        let iter = self.parse_expr()?;
        self.expect_kw(TokenKind::Do)?;
        let body = self.parse_body(&[TokenKind::End])?;
        self.expect_kw(TokenKind::End)?;
        if matches!(self.cur().kind, TokenKind::For) {
            self.advance();
        }
        self.skip_semis();
        Ok(Stmt::For { var, iter, body })
    }

    fn parse_return(&mut self) -> Result<Stmt, PlqmError> {
        self.advance();
        let expr = if matches!(self.cur().kind, TokenKind::Semicolon | TokenKind::Eof) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.skip_semis();
        Ok(Stmt::Return(expr))
    }

    fn parse_raise(&mut self) -> Result<Stmt, PlqmError> {
        self.advance();
        let msg = self.parse_expr()?;
        self.skip_semis();
        Ok(Stmt::Raise(msg))
    }

    fn parse_body(&mut self, terminators: &[TokenKind]) -> Result<Vec<Stmt>, PlqmError> {
        let mut stmts = Vec::new();
        while !self.at_end() {
            self.skip_semis();
            if self.at_end() {
                break;
            }
            if terminators
                .iter()
                .any(|t| std::mem::discriminant(t) == std::mem::discriminant(&self.cur().kind))
            {
                break;
            }
            stmts.push(self.parse_stmt()?);
        }
        Ok(stmts)
    }

    // ── Expressions (precedence climbing) ───────────────────────────

    fn parse_expr(&mut self) -> Result<Expr, PlqmError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, PlqmError> {
        let mut left = self.parse_and()?;
        while matches!(self.cur().kind, TokenKind::Or) {
            self.advance();
            let right = self.parse_and()?;
            left = Expr::BinOp("or".into(), Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, PlqmError> {
        let mut left = self.parse_not()?;
        while matches!(self.cur().kind, TokenKind::And) {
            self.advance();
            let right = self.parse_not()?;
            left = Expr::BinOp("and".into(), Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr, PlqmError> {
        if matches!(self.cur().kind, TokenKind::Not) {
            self.advance();
            return Ok(Expr::Unary(
                "not".into(),
                Box::new(self.parse_comparison()?),
            ));
        }
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<Expr, PlqmError> {
        let left = self.parse_addition()?;
        let op = match self.cur().kind {
            TokenKind::Eq => "=",
            TokenKind::Neq => "!=",
            TokenKind::Lt => "<",
            TokenKind::Gt => ">",
            TokenKind::Lte => "<=",
            TokenKind::Gte => ">=",
            TokenKind::Is => {
                self.advance();
                if matches!(self.cur().kind, TokenKind::Not) {
                    self.advance();
                    self.expect_kw(TokenKind::Null)?;
                    return Ok(Expr::Unary("is_not_null".into(), Box::new(left)));
                }
                self.expect_kw(TokenKind::Null)?;
                return Ok(Expr::Unary("is_null".into(), Box::new(left)));
            }
            _ => return Ok(left),
        };
        self.advance();
        let right = self.parse_addition()?;
        Ok(Expr::BinOp(op.into(), Box::new(left), Box::new(right)))
    }

    fn parse_addition(&mut self) -> Result<Expr, PlqmError> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.cur().kind {
                TokenKind::Plus => "+",
                TokenKind::Minus => "-",
                _ => break,
            };
            self.advance();
            let right = self.parse_mul()?;
            left = Expr::BinOp(op.into(), Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> Result<Expr, PlqmError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.cur().kind {
                TokenKind::Star => "*",
                TokenKind::Slash => "/",
                TokenKind::Percent => "%",
                _ => break,
            };
            self.advance();
            let right = self.parse_unary()?;
            left = Expr::BinOp(op.into(), Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, PlqmError> {
        if matches!(self.cur().kind, TokenKind::Minus) {
            self.advance();
            return Ok(Expr::Unary("-".into(), Box::new(self.parse_primary()?)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, PlqmError> {
        match self.cur().kind.clone() {
            TokenKind::Number(n) => {
                self.advance();
                Ok(Expr::Literal(Value::Int(n)))
            }
            TokenKind::Float(f) => {
                self.advance();
                Ok(Expr::Literal(Value::Float(f)))
            }
            TokenKind::Str(s) => {
                self.advance();
                Ok(Expr::Literal(Value::Text(s)))
            }
            TokenKind::True => {
                self.advance();
                Ok(Expr::Literal(Value::Bool(true)))
            }
            TokenKind::False => {
                self.advance();
                Ok(Expr::Literal(Value::Bool(false)))
            }
            TokenKind::Null => {
                self.advance();
                Ok(Expr::Literal(Value::Null))
            }
            TokenKind::LParen => {
                self.advance();
                let e = self.parse_expr()?;
                self.expect_kw(TokenKind::RParen)?;
                Ok(e)
            }
            TokenKind::Ident(name) => {
                self.advance();
                if matches!(self.cur().kind, TokenKind::LParen) {
                    self.advance();
                    let mut args = Vec::new();
                    if !matches!(self.cur().kind, TokenKind::RParen) {
                        args.push(self.parse_expr()?);
                        while matches!(self.cur().kind, TokenKind::Comma) {
                            self.advance();
                            args.push(self.parse_expr()?);
                        }
                    }
                    self.expect_kw(TokenKind::RParen)?;
                    return Ok(Expr::Call(name, args));
                }
                if matches!(self.cur().kind, TokenKind::Dot) {
                    self.advance();
                    let field = self.expect_ident()?;
                    return Ok(Expr::Field(Box::new(Expr::Ident(name)), field));
                }
                Ok(Expr::Ident(name))
            }
            _ => Err(PlqmError(format!(
                "unexpected token {:?} at line {}",
                self.cur().kind,
                self.cur().line
            ))),
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────

    fn cur(&self) -> &Token {
        self.tokens
            .get(self.pos)
            .unwrap_or(self.tokens.last().unwrap())
    }

    fn advance(&mut self) -> &Token {
        let t = self.tokens.get(self.pos).unwrap();
        self.pos += 1;
        t
    }

    fn at_end(&self) -> bool {
        self.pos >= self.tokens.len() || matches!(self.tokens[self.pos].kind, TokenKind::Eof)
    }

    fn skip_semis(&mut self) {
        while matches!(self.cur().kind, TokenKind::Semicolon) {
            self.advance();
        }
    }

    fn expect_kw(&mut self, expected: TokenKind) -> Result<(), PlqmError> {
        if std::mem::discriminant(&self.cur().kind) == std::mem::discriminant(&expected) {
            self.advance();
            Ok(())
        } else {
            Err(PlqmError(format!(
                "expected {:?}, got {:?} at line {}",
                expected,
                self.cur().kind,
                self.cur().line
            )))
        }
    }

    fn expect_ident(&mut self) -> Result<String, PlqmError> {
        if let TokenKind::Ident(name) = self.cur().kind.clone() {
            self.advance();
            Ok(name)
        } else {
            Err(PlqmError(format!(
                "expected identifier, got {:?} at line {}",
                self.cur().kind,
                self.cur().line
            )))
        }
    }
}

// ── Interpreter ──────────────────────────────────────────────────────

pub type BuiltinFn = Arc<dyn Fn(Vec<Value>) -> Result<Value, PlqmError> + Send + Sync>;

pub struct PlqmInterpreter {
    builtins: HashMap<String, BuiltinFn>,
    pub max_iterations: usize,
}

impl Default for PlqmInterpreter {
    fn default() -> Self {
        let mut interp = Self {
            builtins: HashMap::new(),
            max_iterations: 100_000,
        };
        interp.register_stdlib();
        interp
    }
}

impl PlqmInterpreter {
    pub fn new() -> Self {
        Self::default()
    }

    fn register_stdlib(&mut self) {
        self.register(
            "now",
            Arc::new(|_| {
                use std::time::{SystemTime, UNIX_EPOCH};
                Ok(Value::Float(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64(),
                ))
            }),
        );
        self.register(
            "coalesce",
            Arc::new(|args| {
                Ok(args
                    .into_iter()
                    .find(|v| !matches!(v, Value::Null))
                    .unwrap_or(Value::Null))
            }),
        );
        self.register(
            "len",
            Arc::new(|args| match args.first().unwrap_or(&Value::Null) {
                Value::Text(s) => Ok(Value::Int(s.len() as i64)),
                Value::List(v) => Ok(Value::Int(v.len() as i64)),
                Value::Null => Ok(Value::Int(0)),
                _ => Err(PlqmError("len() expects text or list".into())),
            }),
        );
        self.register(
            "abs",
            Arc::new(|args| match args.first().unwrap_or(&Value::Null) {
                Value::Int(n) => Ok(Value::Int(n.abs())),
                Value::Float(f) => Ok(Value::Float(f.abs())),
                _ => Err(PlqmError("abs() expects number".into())),
            }),
        );
        self.register(
            "upper",
            Arc::new(|args| {
                Ok(Value::Text(
                    args.first()
                        .unwrap_or(&Value::Null)
                        .as_text()
                        .to_ascii_uppercase(),
                ))
            }),
        );
        self.register(
            "lower",
            Arc::new(|args| {
                Ok(Value::Text(
                    args.first()
                        .unwrap_or(&Value::Null)
                        .as_text()
                        .to_ascii_lowercase(),
                ))
            }),
        );
        self.register(
            "str",
            Arc::new(|args| Ok(Value::Text(args.first().unwrap_or(&Value::Null).as_text()))),
        );
        self.register(
            "int",
            Arc::new(|args| {
                args.first()
                    .unwrap_or(&Value::Null)
                    .as_i64()
                    .map(Value::Int)
            }),
        );
        self.register(
            "float",
            Arc::new(|args| {
                args.first()
                    .unwrap_or(&Value::Null)
                    .as_f64()
                    .map(Value::Float)
            }),
        );
        self.register(
            "range",
            Arc::new(|args| {
                let (start, end) = match args.len() {
                    1 => (0i64, args[0].as_i64()?),
                    2 => (args[0].as_i64()?, args[1].as_i64()?),
                    _ => return Err(PlqmError("range() takes 1 or 2 args".into())),
                };
                Ok(Value::List((start..end).map(Value::Int).collect()))
            }),
        );
        self.register("print", Arc::new(|_| Ok(Value::Null)));
        self.register("list", Arc::new(|args| Ok(Value::List(args))));
    }

    /// Register or override a built-in/callback function.
    pub fn register(&mut self, name: &str, f: BuiltinFn) {
        self.builtins.insert(name.to_ascii_lowercase(), f);
    }

    /// Parse and execute PL/QM source. Returns the RETURN value or Null.
    pub fn execute(
        &self,
        source: &str,
        params: HashMap<String, Value>,
    ) -> Result<Value, PlqmError> {
        let tokens = Lexer::new(source).tokenize()?;
        let stmts = Parser::new(tokens).parse()?;
        self.exec_block(&stmts, &mut params.clone())
    }

    /// Execute pre-parsed AST.
    pub fn execute_ast(
        &self,
        stmts: &[Stmt],
        params: HashMap<String, Value>,
    ) -> Result<Value, PlqmError> {
        self.exec_block(stmts, &mut params.clone())
    }

    fn exec_block(
        &self,
        stmts: &[Stmt],
        scope: &mut HashMap<String, Value>,
    ) -> Result<Value, PlqmError> {
        for stmt in stmts {
            match self.exec_stmt(stmt, scope) {
                Err(e) if e.0.starts_with("__return__:") => {
                    // encode return signal in error message (simple approach)
                    return Err(e);
                }
                Err(e) => return Err(e),
                Ok(_) => {}
            }
        }
        Ok(Value::Null)
    }

    fn exec_stmt(
        &self,
        stmt: &Stmt,
        scope: &mut HashMap<String, Value>,
    ) -> Result<Value, PlqmError> {
        match stmt {
            Stmt::Declare { name, init, .. } => {
                let val = if let Some(e) = init {
                    self.eval(e, scope)?
                } else {
                    Value::Null
                };
                scope.insert(name.clone(), val);
            }
            Stmt::Set { name, expr } => {
                let val = self.eval(expr, scope)?;
                scope.insert(name.clone(), val);
            }
            Stmt::If {
                cond,
                then_body,
                elif_branches,
                else_body,
            } => {
                if self.eval(cond, scope)?.truthy() {
                    self.exec_stmts(then_body, scope)?;
                } else {
                    let mut matched = false;
                    for (ec, eb) in elif_branches {
                        if self.eval(ec, scope)?.truthy() {
                            self.exec_stmts(eb, scope)?;
                            matched = true;
                            break;
                        }
                    }
                    if !matched {
                        if let Some(eb) = else_body {
                            self.exec_stmts(eb, scope)?;
                        }
                    }
                }
            }
            Stmt::While { cond, body } => {
                let mut iters = 0usize;
                while self.eval(cond, scope)?.truthy() {
                    self.exec_stmts(body, scope)?;
                    iters += 1;
                    if iters > self.max_iterations {
                        return Err(PlqmError("maximum iterations exceeded in WHILE".into()));
                    }
                }
            }
            Stmt::For { var, iter, body } => {
                let iterable = self.eval(iter, scope)?;
                let items = match iterable {
                    Value::List(v) => v,
                    _ => return Err(PlqmError("FOR requires a list".into())),
                };
                let mut iters = 0usize;
                for item in items {
                    scope.insert(var.clone(), item);
                    self.exec_stmts(body, scope)?;
                    iters += 1;
                    if iters > self.max_iterations {
                        return Err(PlqmError("maximum iterations exceeded in FOR".into()));
                    }
                }
            }
            Stmt::Return(expr) => {
                let val = if let Some(e) = expr {
                    self.eval(e, scope)?
                } else {
                    Value::Null
                };
                // Use a tagged error as return signal
                return Err(PlqmError(format!(
                    "__return__:{}",
                    Self::encode_return(val)
                )));
            }
            Stmt::Raise(msg) => {
                let m = self.eval(msg, scope)?;
                return Err(PlqmError(m.as_text()));
            }
            Stmt::Expr(e) => {
                self.eval(e, scope)?;
            }
        }
        Ok(Value::Null)
    }

    fn exec_stmts(
        &self,
        stmts: &[Stmt],
        scope: &mut HashMap<String, Value>,
    ) -> Result<(), PlqmError> {
        for stmt in stmts {
            self.exec_stmt(stmt, scope)?;
        }
        Ok(())
    }

    fn eval(&self, expr: &Expr, scope: &HashMap<String, Value>) -> Result<Value, PlqmError> {
        match expr {
            Expr::Literal(v) => Ok(v.clone()),
            Expr::Ident(name) => scope
                .get(name)
                .cloned()
                .ok_or_else(|| PlqmError(format!("undefined variable: {}", name))),
            Expr::Field(obj, key) => match self.eval(obj, scope)? {
                Value::List(_) => Err(PlqmError("field access on list not supported".into())),
                _ => Err(PlqmError(format!("field access .{} on non-object", key))),
            },
            Expr::Call(name, args) => {
                let evaled: Vec<Value> = args
                    .iter()
                    .map(|a| self.eval(a, scope))
                    .collect::<Result<_, _>>()?;
                let fn_name = name.to_ascii_lowercase();
                if let Some(f) = self.builtins.get(&fn_name) {
                    f(evaled)
                } else {
                    Err(PlqmError(format!("undefined function: {}", name)))
                }
            }
            Expr::BinOp(op, left, right) => {
                let l = self.eval(left, scope)?;
                let r = self.eval(right, scope)?;
                self.binary_op(op, l, r)
            }
            Expr::Unary(op, operand) => {
                let v = self.eval(operand, scope)?;
                match op.as_str() {
                    "-" => match v {
                        Value::Int(n) => Ok(Value::Int(-n)),
                        Value::Float(f) => Ok(Value::Float(-f)),
                        _ => Err(PlqmError("unary minus on non-number".into())),
                    },
                    "not" => Ok(Value::Bool(!v.truthy())),
                    "is_null" => Ok(Value::Bool(matches!(v, Value::Null))),
                    "is_not_null" => Ok(Value::Bool(!matches!(v, Value::Null))),
                    _ => Err(PlqmError(format!("unknown unary op: {}", op))),
                }
            }
            Expr::List(items) => {
                let vals: Vec<Value> = items
                    .iter()
                    .map(|e| self.eval(e, scope))
                    .collect::<Result<_, _>>()?;
                Ok(Value::List(vals))
            }
        }
    }

    fn binary_op(&self, op: &str, l: Value, r: Value) -> Result<Value, PlqmError> {
        match op {
            "+" => match (&l, &r) {
                (Value::Text(a), _) => Ok(Value::Text(format!("{}{}", a, r.as_text()))),
                (_, Value::Text(b)) => Ok(Value::Text(format!("{}{}", l.as_text(), b))),
                (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
                (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 + b)),
                (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + *b as f64)),
                _ => Err(PlqmError(format!("unsupported + on {:?} and {:?}", l, r))),
            },
            "-" => Ok(Value::Float(l.as_f64()? - r.as_f64()?)),
            "*" => Ok(Value::Float(l.as_f64()? * r.as_f64()?)),
            "/" => {
                let d = r.as_f64()?;
                if d == 0.0 {
                    Err(PlqmError("division by zero".into()))
                } else {
                    Ok(Value::Float(l.as_f64()? / d))
                }
            }
            "%" => {
                let d = r.as_i64()?;
                if d == 0 {
                    Err(PlqmError("modulo by zero".into()))
                } else {
                    Ok(Value::Int(l.as_i64()? % d))
                }
            }
            "=" => Ok(Value::Bool(l == r)),
            "!=" => Ok(Value::Bool(l != r)),
            "<" => Ok(Value::Bool(l.as_f64()? < r.as_f64()?)),
            ">" => Ok(Value::Bool(l.as_f64()? > r.as_f64()?)),
            "<=" => Ok(Value::Bool(l.as_f64()? <= r.as_f64()?)),
            ">=" => Ok(Value::Bool(l.as_f64()? >= r.as_f64()?)),
            "and" => Ok(Value::Bool(l.truthy() && r.truthy())),
            "or" => Ok(Value::Bool(l.truthy() || r.truthy())),
            _ => Err(PlqmError(format!("unknown binary op: {}", op))),
        }
    }

    fn encode_return(v: Value) -> String {
        // Simple serialisation for return signal
        match v {
            Value::Null => "null".into(),
            Value::Bool(b) => {
                if b {
                    "true".into()
                } else {
                    "false".into()
                }
            }
            Value::Int(n) => n.to_string(),
            Value::Float(f) => {
                let s = f.to_string();
                if s.contains('.') || s.contains('e') {
                    s
                } else {
                    format!("{}.0", s)
                }
            }
            Value::Text(s) => format!("\"{}\"", s),
            Value::List(_) => "list".into(),
        }
    }

    /// Execute source, handling RETURN signals transparently.
    pub fn run(&self, source: &str, params: HashMap<String, Value>) -> Result<Value, PlqmError> {
        match self.execute(source, params) {
            Err(PlqmError(msg)) if msg.starts_with("__return__:") => {
                let payload = &msg["__return__:".len()..];
                Ok(Self::decode_return(payload))
            }
            other => other,
        }
    }

    fn decode_return(s: &str) -> Value {
        match s {
            "null" => Value::Null,
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ if s.starts_with('"') => Value::Text(s.trim_matches('"').to_string()),
            _ => s
                .parse::<i64>()
                .map(Value::Int)
                .unwrap_or_else(|_| s.parse::<f64>().map(Value::Float).unwrap_or(Value::Null)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> Value {
        PlqmInterpreter::new()
            .run(src, HashMap::new())
            .expect("run failed")
    }

    #[test]
    fn declare_and_return() {
        let v = run("DECLARE x INT = 42; RETURN x;");
        assert_eq!(v, Value::Int(42));
    }

    #[test]
    fn if_elif_else() {
        let v = run("DECLARE n = 5; IF n > 10 THEN RETURN 'big'; ELIF n > 3 THEN RETURN 'mid'; ELSE RETURN 'small'; END IF");
        assert_eq!(v, Value::Text("mid".into()));
    }

    #[test]
    fn while_loop_sum() {
        let v = run("DECLARE s = 0; DECLARE i = 1; WHILE i <= 5 DO SET s = s + i; SET i = i + 1; END WHILE; RETURN s;");
        // s = 1+2+3+4+5 = 15
        assert_eq!(v, Value::Int(15));
    }

    #[test]
    fn for_loop() {
        let v = run("DECLARE s = 0; FOR x IN range(5) DO SET s = s + x; END FOR; RETURN s;");
        // 0+1+2+3+4 = 10
        assert_eq!(v, Value::Int(10));
    }

    #[test]
    fn builtin_upper_lower() {
        let v = run("RETURN upper('hello');");
        assert_eq!(v, Value::Text("HELLO".into()));
    }

    #[test]
    fn raise_becomes_error() {
        let r = PlqmInterpreter::new().run("RAISE 'oops';", HashMap::new());
        assert!(r.is_err());
    }
}
