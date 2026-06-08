use std::path::PathBuf;
use std::sync::Arc;

use hashbrown::HashMap;
use serde_json::{Map as JsonMap, Value as JsonValue};
use sqlparser::ast::{Expr, SelectItem, SetExpr, Statement, Value};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::index::IndexManager;
use crate::storage::{FsyncMode, StorageConfig, StorageEngine};

pub mod catalog;
pub mod errors;
pub mod executor;
pub mod ffi;
pub mod planner;
pub mod point_query;
pub mod types;
pub mod vector_gate;

#[cfg(test)]
mod join_ab_microbench;

pub use catalog::Catalog;
pub use errors::{ExecError, HubError, PlanError};
pub use executor::SortDirection;
pub use planner::{JoinStrategy, PhysicalPlan};
pub use types::{
    AtomicHubStatus, ExecutionMode, HubConfig, HubStatus, IndexInfo, QueryRequest, QueryResult,
    TableMeta,
};
use vector_gate::{EnforcementPolicy, VectorGate};

pub struct HubEngine {
    pub catalog: Arc<Catalog>,
    pub storage: Arc<StorageEngine>,
    pub index_manager: Arc<IndexManager>,
    pub config: HubConfig,
    pub status: AtomicHubStatus,
    vector_gate: VectorGate,
}

impl HubEngine {
    pub fn new(config: HubConfig) -> Self {
        let storage_cfg = StorageConfig {
            data_dir: PathBuf::from(&config.data_dir),
            wal_dir: PathBuf::from(format!("{}/wal", config.data_dir)),
            fsync_mode: FsyncMode::Periodic,
            ..Default::default()
        };

        let storage = StorageEngine::new(storage_cfg)
            .unwrap_or_else(|e| panic!("failed to initialize HubEngine storage: {e}"));

        Self {
            catalog: Arc::new(Catalog::new()),
            storage: Arc::new(storage),
            index_manager: Arc::new(IndexManager::new()),
            config,
            status: AtomicHubStatus::new(HubStatus::Init),
            vector_gate: VectorGate::new(EnforcementPolicy::Strict),
        }
    }

    pub async fn start(&self) -> Result<(), HubError> {
        self.status.store(HubStatus::Starting);
        self.status.store(HubStatus::Ready);
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), HubError> {
        self.status.store(HubStatus::Stopping);
        self.status.store(HubStatus::Stopped);
        Ok(())
    }

    pub async fn execute_request(&self, req: &QueryRequest) -> Result<QueryResult, HubError> {
        self.vector_gate.validate_request(req)?;
        self.execute_sql(&req.sql).await
    }

    pub async fn execute_sql(&self, sql: &str) -> Result<QueryResult, HubError> {
        let dialect = PostgreSqlDialect {};
        let ast = Parser::parse_sql(&dialect, sql).map_err(|e| HubError::Parse(e.to_string()))?;
        let stmt = ast
            .first()
            .ok_or_else(|| HubError::Parse("empty SQL".to_string()))?;

        if let Some(res) = self.try_execute_control_sql(stmt)? {
            return Ok(res);
        }

        if let Some(res) =
            point_query::try_exec_point_query(stmt, &self.catalog, &self.storage).await?
        {
            return Ok(res);
        }

        let plan = planner::plan_select(stmt, &self.catalog)?;
        executor::execute_physical_plan(plan, &self.storage)
            .await
            .map_err(HubError::from)
    }

    pub fn stats(&self) -> String {
        format!(
            "HubEngine(status={:?}, data_dir={}, max_connections={})",
            self.status.load(),
            self.config.data_dir,
            self.config.max_connections
        )
    }

    fn try_execute_control_sql(&self, stmt: &Statement) -> Result<Option<QueryResult>, HubError> {
        match stmt {
            Statement::CreateTable { name, columns, .. } => {
                let table = name.to_string();
                let column_names: Vec<String> =
                    columns.iter().map(|c| c.name.value.clone()).collect();
                let primary_key = column_names
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "id".to_string());
                self.catalog.register_table(TableMeta {
                    name: table,
                    columns: column_names,
                    row_count: 0,
                    primary_key,
                    indexes: Vec::<IndexInfo>::new(),
                    vector_dim: None,
                });
                self.ensure_rows_dir()?;
                Ok(Some(QueryResult::empty()))
            }
            Statement::Insert {
                table_name,
                columns,
                source,
                ..
            } => {
                let table = table_name.to_string();
                let meta = self
                    .catalog
                    .get_table_meta(&table)
                    .map_err(HubError::from)?;
                let Some(source) = source else {
                    return Err(HubError::Exec(ExecError::NotImplemented(
                        "INSERT without source is not supported".to_string(),
                    )));
                };
                let SetExpr::Values(values) = source.body.as_ref() else {
                    return Err(HubError::Exec(ExecError::NotImplemented(
                        "only INSERT ... VALUES is supported in HubEngine passthrough".to_string(),
                    )));
                };

                let insert_columns: Vec<String> = if columns.is_empty() {
                    meta.columns.clone()
                } else {
                    columns.iter().map(|c| c.value.clone()).collect()
                };
                if insert_columns.is_empty() {
                    return Err(HubError::Exec(ExecError::PolicyViolation(
                        "INSERT requires known target columns".to_string(),
                    )));
                }

                let rows_dir = self.ensure_rows_dir()?;
                let mut affected = 0u64;
                let mut next_row = meta.row_count;
                for row_exprs in &values.rows {
                    if row_exprs.len() != insert_columns.len() {
                        return Err(HubError::Exec(ExecError::PolicyViolation(format!(
                            "INSERT column count mismatch: got {}, expected {}",
                            row_exprs.len(),
                            insert_columns.len()
                        ))));
                    }
                    let mut row = JsonMap::new();
                    for (column, expr) in insert_columns.iter().zip(row_exprs.iter()) {
                        row.insert(column.clone(), expr_to_json_value(expr));
                    }
                    next_row += 1;
                    let path = rows_dir.join(format!("{table}_{next_row}.qmr"));
                    let bytes = rmp_serde::to_vec(&JsonValue::Object(row)).map_err(|e| {
                        HubError::Exec(ExecError::PolicyViolation(format!(
                            "encode HubEngine row failed: {e}"
                        )))
                    })?;
                    std::fs::write(&path, bytes).map_err(|e| {
                        HubError::Exec(ExecError::PolicyViolation(format!(
                            "write HubEngine row failed ({}): {e}",
                            path.display()
                        )))
                    })?;
                    affected += 1;
                }
                self.catalog
                    .update_row_count(&table, meta.row_count + affected);
                Ok(Some(QueryResult {
                    columns: Vec::new(),
                    rows: Vec::new(),
                    affected_rows: affected,
                }))
            }
            Statement::Query(query) => {
                let SetExpr::Select(select) = query.body.as_ref() else {
                    return Ok(None);
                };
                if select.from.len() != 1 || !select.from[0].joins.is_empty() {
                    return Ok(None);
                }
                if select.selection.is_some() || !query.order_by.is_empty() {
                    return Ok(None);
                }
                let table = select.from[0].relation.to_string();
                let meta = self
                    .catalog
                    .get_table_meta(&table)
                    .map_err(HubError::from)?;
                let mut rows =
                    executor::load_table_rows_from_disk(self.storage.data_dir(), &table)?;
                if let Some(limit) = query.limit.as_ref().and_then(extract_usize_literal) {
                    rows.truncate(limit);
                }
                let projected = project_hub_rows(rows, &select.projection, &meta.columns)?;
                Ok(Some(projected))
            }
            _ => Ok(None),
        }
    }

    fn ensure_rows_dir(&self) -> Result<PathBuf, HubError> {
        let rows_dir = self.storage.data_dir().join("rows");
        std::fs::create_dir_all(&rows_dir).map_err(|e| {
            HubError::Exec(ExecError::PolicyViolation(format!(
                "create HubEngine rows dir failed ({}): {e}",
                rows_dir.display()
            )))
        })?;
        Ok(rows_dir)
    }
}

fn extract_usize_literal(expr: &Expr) -> Option<usize> {
    match expr {
        Expr::Value(Value::Number(n, _)) => n.parse().ok(),
        _ => None,
    }
}

fn expr_to_json_value(expr: &Expr) -> JsonValue {
    match expr {
        Expr::Value(Value::Number(n, _)) => n
            .parse::<i64>()
            .map(JsonValue::from)
            .or_else(|_| n.parse::<f64>().map(JsonValue::from))
            .unwrap_or_else(|_| JsonValue::String(n.clone())),
        Expr::Value(Value::SingleQuotedString(s))
        | Expr::Value(Value::DoubleQuotedString(s))
        | Expr::Value(Value::EscapedStringLiteral(s)) => JsonValue::String(s.clone()),
        Expr::Value(Value::Boolean(b)) => JsonValue::Bool(*b),
        Expr::Value(Value::Null) => JsonValue::Null,
        _ => JsonValue::String(expr.to_string()),
    }
}

fn project_hub_rows(
    rows: Vec<executor::Row>,
    projection: &[SelectItem],
    table_columns: &[String],
) -> Result<QueryResult, HubError> {
    if projection
        .iter()
        .any(|item| matches!(item, SelectItem::Wildcard(_)))
    {
        let columns = if table_columns.is_empty() {
            let mut set = std::collections::BTreeSet::new();
            for row in &rows {
                for col in row.keys() {
                    set.insert(col.clone());
                }
            }
            set.into_iter().collect()
        } else {
            table_columns.to_vec()
        };
        return Ok(result_from_columns(rows, columns));
    }

    let mut columns = Vec::new();
    for item in projection {
        match item {
            SelectItem::UnnamedExpr(Expr::Identifier(id)) => columns.push(id.value.clone()),
            SelectItem::ExprWithAlias {
                expr: Expr::Identifier(_),
                alias,
            } => columns.push(alias.value.clone()),
            _ => {
                return Err(HubError::Exec(ExecError::NotImplemented(format!(
                    "unsupported HubEngine SELECT projection: {item}"
                ))));
            }
        }
    }
    Ok(result_from_columns(rows, columns))
}

fn result_from_columns(rows: Vec<HashMap<String, String>>, columns: Vec<String>) -> QueryResult {
    let mut data = Vec::with_capacity(rows.len());
    for row in rows {
        let mut projected = Vec::with_capacity(columns.len());
        for c in &columns {
            projected.push(row.get(c).cloned().unwrap_or_default());
        }
        data.push(projected);
    }
    QueryResult {
        columns,
        affected_rows: data.len() as u64,
        rows: data,
    }
}
