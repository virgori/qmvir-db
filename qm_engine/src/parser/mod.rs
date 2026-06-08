/*
 * SQL Parser Module - Fast SQL parsing and query dispatch
 *
 * Uses sqlparser-rs for SQL parsing with additional QMvir extensions.
 */

mod dispatcher;
mod query;

pub use dispatcher::*;
pub use query::*;

#[cfg(feature = "python")]
use pyo3::prelude::*;
use sqlparser::ast::{Expr, Statement};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ParseError {
    #[error("Syntax error: {0}")]
    Syntax(String),
    #[error("Unsupported query type: {0}")]
    Unsupported(String),
    #[error("Invalid expression: {0}")]
    InvalidExpr(String),
}

/// Query type classification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryType {
    Select,
    Insert,
    Update,
    Delete,
    CreateTable,
    CreateIndex,
    DropTable,
    DropIndex,
    Truncate,
    Begin,
    Commit,
    Rollback,
    Set,
    Show,
    Explain,
    VectorSearch, // QMvir extension
    Other,
}

/// Parsed query representation
#[derive(Debug, Clone)]
pub struct ParsedQuery {
    pub query_type: QueryType,
    pub tables: Vec<String>,
    pub columns: Vec<String>,
    pub where_clause: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    pub is_vector_query: bool,
    pub vector_column: Option<String>,
    pub vector_top_k: Option<usize>,
    pub raw_sql: String,
}

impl Default for ParsedQuery {
    fn default() -> Self {
        Self {
            query_type: QueryType::Other,
            tables: Vec::new(),
            columns: Vec::new(),
            where_clause: None,
            limit: None,
            offset: None,
            is_vector_query: false,
            vector_column: None,
            vector_top_k: None,
            raw_sql: String::new(),
        }
    }
}

/// Fast SQL parser
pub struct SqlParser {
    dialect: PostgreSqlDialect,
}

impl SqlParser {
    pub fn new() -> Self {
        Self {
            dialect: PostgreSqlDialect {},
        }
    }

    /// Parse SQL and return parsed query
    pub fn parse(&self, sql: &str) -> Result<ParsedQuery, ParseError> {
        let sql_upper = sql.trim().to_uppercase();

        // Quick classification for common queries
        let query_type = if sql_upper.starts_with("SELECT") {
            QueryType::Select
        } else if sql_upper.starts_with("INSERT") {
            QueryType::Insert
        } else if sql_upper.starts_with("UPDATE") {
            QueryType::Update
        } else if sql_upper.starts_with("DELETE") {
            QueryType::Delete
        } else if sql_upper.starts_with("CREATE TABLE") {
            QueryType::CreateTable
        } else if sql_upper.starts_with("CREATE INDEX") {
            QueryType::CreateIndex
        } else if sql_upper.starts_with("DROP TABLE") {
            QueryType::DropTable
        } else if sql_upper.starts_with("DROP INDEX") {
            QueryType::DropIndex
        } else if sql_upper.starts_with("TRUNCATE") {
            QueryType::Truncate
        } else if sql_upper.starts_with("BEGIN") || sql_upper.starts_with("START TRANSACTION") {
            QueryType::Begin
        } else if sql_upper.starts_with("COMMIT") {
            QueryType::Commit
        } else if sql_upper.starts_with("ROLLBACK") || sql_upper.starts_with("ABORT") {
            QueryType::Rollback
        } else if sql_upper.starts_with("SET") {
            QueryType::Set
        } else if sql_upper.starts_with("SHOW") {
            QueryType::Show
        } else if sql_upper.starts_with("EXPLAIN") {
            QueryType::Explain
        } else {
            QueryType::Other
        };

        // Check for vector search pattern
        let is_vector_query = sql_upper.contains("ORDER BY")
            && (sql_upper.contains("<->")
                || sql_upper.contains("<#>")
                || sql_upper.contains("COSINE_DISTANCE")
                || sql_upper.contains("L2_DISTANCE"));

        let mut parsed = ParsedQuery {
            query_type,
            is_vector_query,
            raw_sql: sql.to_string(),
            ..Default::default()
        };

        // Full parse for SELECTs to extract more info
        if matches!(
            query_type,
            QueryType::Select | QueryType::Insert | QueryType::Update | QueryType::Delete
        ) {
            if let Ok(statements) = Parser::parse_sql(&self.dialect, sql) {
                if let Some(stmt) = statements.into_iter().next() {
                    self.analyze_statement(&stmt, &mut parsed);
                }
            }
        }

        Ok(parsed)
    }

    /// Analyze a parsed statement
    fn analyze_statement(&self, stmt: &Statement, parsed: &mut ParsedQuery) {
        match stmt {
            Statement::Query(query) => {
                // Extract table names from FROM clause
                if let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref() {
                    for table in &select.from {
                        self.extract_table_name(&table.relation, &mut parsed.tables);
                    }

                    // Extract column names
                    for item in &select.projection {
                        if let sqlparser::ast::SelectItem::UnnamedExpr(expr) = item {
                            if let Expr::Identifier(ident) = expr {
                                parsed.columns.push(ident.value.clone());
                            }
                        }
                    }

                    // Check for vector operations in WHERE or ORDER BY
                    if let Some(ref selection) = select.selection {
                        parsed.where_clause = Some(format!("{}", selection));
                    }
                }

                // Extract LIMIT
                if let Some(ref limit) = query.limit {
                    if let Expr::Value(sqlparser::ast::Value::Number(n, _)) = limit {
                        parsed.limit = n.parse().ok();
                    }
                }

                // Extract OFFSET
                if let Some(ref offset) = query.offset {
                    if let Expr::Value(sqlparser::ast::Value::Number(n, _)) = &offset.value {
                        parsed.offset = n.parse().ok();
                    }
                }
            }
            Statement::Insert { table_name, .. } => {
                parsed.tables.push(table_name.to_string());
            }
            Statement::Update { table, .. } => {
                self.extract_table_name(&table.relation, &mut parsed.tables);
            }
            Statement::Delete { from, .. } => {
                for table_with_joins in from.iter() {
                    self.extract_table_name(&table_with_joins.relation, &mut parsed.tables);
                }
            }
            _ => {}
        }
    }

    /// Extract table name from TableFactor
    fn extract_table_name(&self, relation: &sqlparser::ast::TableFactor, tables: &mut Vec<String>) {
        if let sqlparser::ast::TableFactor::Table { name, .. } = relation {
            tables.push(name.to_string());
        }
    }

    /// Parse multiple statements
    pub fn parse_multi(&self, sql: &str) -> Result<Vec<ParsedQuery>, ParseError> {
        // Split by semicolon, handling quoted strings
        let mut queries = Vec::new();
        let mut current = String::new();
        let mut in_string = false;
        let mut string_char = ' ';

        for c in sql.chars() {
            if !in_string {
                if c == '\'' || c == '"' {
                    in_string = true;
                    string_char = c;
                } else if c == ';' {
                    let trimmed = current.trim();
                    if !trimmed.is_empty() {
                        queries.push(self.parse(trimmed)?);
                    }
                    current.clear();
                    continue;
                }
            } else if c == string_char {
                in_string = false;
            }
            current.push(c);
        }

        let trimmed = current.trim();
        if !trimmed.is_empty() {
            queries.push(self.parse(trimmed)?);
        }

        Ok(queries)
    }
}

impl Default for SqlParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Python-exposed SQL Parser
#[cfg(feature = "python")]
#[pyclass(name = "SqlParser")]
pub struct PySqlParser {
    inner: SqlParser,
}

#[cfg(feature = "python")]
#[pymethods]
impl PySqlParser {
    #[new]
    pub fn new() -> Self {
        Self {
            inner: SqlParser::new(),
        }
    }

    /// Parse SQL and return query info as dict
    pub fn parse(&self, sql: &str) -> PyResult<PyObject> {
        Python::with_gil(|py| match self.inner.parse(sql) {
            Ok(parsed) => {
                let dict = pyo3::types::PyDict::new_bound(py);
                dict.set_item("query_type", format!("{:?}", parsed.query_type))?;
                dict.set_item("tables", parsed.tables)?;
                dict.set_item("columns", parsed.columns)?;
                dict.set_item("is_vector_query", parsed.is_vector_query)?;
                dict.set_item("limit", parsed.limit)?;
                dict.set_item("offset", parsed.offset)?;
                Ok(dict.into())
            }
            Err(e) => Err(pyo3::exceptions::PyValueError::new_err(e.to_string())),
        })
    }

    /// Get query type quickly without full parse
    pub fn get_query_type(&self, sql: &str) -> String {
        let upper = sql.trim().to_uppercase();
        if upper.starts_with("SELECT") {
            "SELECT".to_string()
        } else if upper.starts_with("INSERT") {
            "INSERT".to_string()
        } else if upper.starts_with("UPDATE") {
            "UPDATE".to_string()
        } else if upper.starts_with("DELETE") {
            "DELETE".to_string()
        } else if upper.starts_with("CREATE") {
            "CREATE".to_string()
        } else if upper.starts_with("DROP") {
            "DROP".to_string()
        } else if upper.starts_with("BEGIN") || upper.starts_with("START") {
            "BEGIN".to_string()
        } else if upper.starts_with("COMMIT") {
            "COMMIT".to_string()
        } else if upper.starts_with("ROLLBACK") {
            "ROLLBACK".to_string()
        } else {
            "OTHER".to_string()
        }
    }
}
