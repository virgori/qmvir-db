/*
 * Query Dispatcher - Routes queries to appropriate handlers
 */

use super::{ParsedQuery, QueryType, SqlParser};

/// Query execution context
#[derive(Debug, Clone)]
pub struct QueryContext {
    pub user: String,
    pub database: String,
    pub transaction_id: Option<u64>,
    pub timeout_ms: u64,
}

impl Default for QueryContext {
    fn default() -> Self {
        Self {
            user: "default".to_string(),
            database: "default".to_string(),
            transaction_id: None,
            timeout_ms: 30000,
        }
    }
}

/// Dispatch target
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchTarget {
    /// Vector-optimized path
    VectorEngine,
    /// Standard SQL path  
    SqlEngine,
    /// Transaction management
    TransactionManager,
    /// DDL operations
    SchemaManager,
    /// System commands (SET, SHOW, etc.)
    SystemHandler,
}

/// Query dispatcher
pub struct QueryDispatcher {
    parser: SqlParser,
}

impl QueryDispatcher {
    pub fn new() -> Self {
        Self {
            parser: SqlParser::new(),
        }
    }

    /// Dispatch query to appropriate handler
    pub fn dispatch(
        &self,
        sql: &str,
        _ctx: &QueryContext,
    ) -> Result<(DispatchTarget, ParsedQuery), String> {
        let parsed = self.parser.parse(sql).map_err(|e| e.to_string())?;

        let target = self.determine_target(&parsed);

        Ok((target, parsed))
    }

    /// Determine which engine should handle this query
    fn determine_target(&self, parsed: &ParsedQuery) -> DispatchTarget {
        // Vector queries go to vector engine
        if parsed.is_vector_query {
            return DispatchTarget::VectorEngine;
        }

        match parsed.query_type {
            // Transaction commands
            QueryType::Begin | QueryType::Commit | QueryType::Rollback => {
                DispatchTarget::TransactionManager
            }

            // DDL commands
            QueryType::CreateTable
            | QueryType::CreateIndex
            | QueryType::DropTable
            | QueryType::DropIndex
            | QueryType::Truncate => DispatchTarget::SchemaManager,

            // System commands
            QueryType::Set | QueryType::Show | QueryType::Explain => DispatchTarget::SystemHandler,

            // DML goes to SQL engine
            _ => DispatchTarget::SqlEngine,
        }
    }

    /// Quick dispatch without full parsing (for high-frequency queries)
    pub fn quick_dispatch(&self, sql: &str) -> DispatchTarget {
        let upper = sql.trim().to_uppercase();

        // Check for vector operations
        if upper.contains("<->")
            || upper.contains("<#>")
            || upper.contains("COSINE_DISTANCE")
            || upper.contains("L2_DISTANCE")
        {
            return DispatchTarget::VectorEngine;
        }

        // Transaction commands
        if upper.starts_with("BEGIN")
            || upper.starts_with("START TRANSACTION")
            || upper.starts_with("COMMIT")
            || upper.starts_with("ROLLBACK")
            || upper.starts_with("ABORT")
        {
            return DispatchTarget::TransactionManager;
        }

        // DDL
        if upper.starts_with("CREATE")
            || upper.starts_with("DROP")
            || upper.starts_with("ALTER")
            || upper.starts_with("TRUNCATE")
        {
            return DispatchTarget::SchemaManager;
        }

        // System
        if upper.starts_with("SET") || upper.starts_with("SHOW") || upper.starts_with("EXPLAIN") {
            return DispatchTarget::SystemHandler;
        }

        DispatchTarget::SqlEngine
    }
}

impl Default for QueryDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dispatch_vector_query() {
        let dispatcher = QueryDispatcher::new();

        let sql = "SELECT * FROM vectors ORDER BY embedding <-> '[1,2,3]' LIMIT 10";
        let target = dispatcher.quick_dispatch(sql);
        assert_eq!(target, DispatchTarget::VectorEngine);
    }

    #[test]
    fn test_dispatch_transaction() {
        let dispatcher = QueryDispatcher::new();

        assert_eq!(
            dispatcher.quick_dispatch("BEGIN"),
            DispatchTarget::TransactionManager
        );
        assert_eq!(
            dispatcher.quick_dispatch("COMMIT"),
            DispatchTarget::TransactionManager
        );
        assert_eq!(
            dispatcher.quick_dispatch("ROLLBACK"),
            DispatchTarget::TransactionManager
        );
    }

    #[test]
    fn test_dispatch_ddl() {
        let dispatcher = QueryDispatcher::new();

        assert_eq!(
            dispatcher.quick_dispatch("CREATE TABLE t (id INT)"),
            DispatchTarget::SchemaManager
        );
        assert_eq!(
            dispatcher.quick_dispatch("DROP TABLE t"),
            DispatchTarget::SchemaManager
        );
    }
}
