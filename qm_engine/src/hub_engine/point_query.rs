use sqlparser::ast::{Expr, SetExpr, Statement, Value};

use crate::hub_engine::catalog::Catalog;
use crate::hub_engine::errors::ExecError;
use crate::hub_engine::types::QueryResult;
use crate::storage::StorageEngine;

pub async fn try_exec_point_query(
    stmt: &Statement,
    catalog: &Catalog,
    _storage: &StorageEngine,
) -> Result<Option<QueryResult>, ExecError> {
    let Statement::Query(query) = stmt else {
        return Ok(None);
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Ok(None);
    };

    if !select.from.is_empty() && select.from[0].joins.is_empty() {
        let table_name = select.from[0].relation.to_string();
        let _ = catalog.get_table_meta(&table_name).ok();

        if let Some(selection) = &select.selection {
            if let Some((_column, _literal)) = extract_eq_literal(selection) {
                // Placeholder for real storage point lookup.
                return Ok(Some(QueryResult::empty()));
            }
        }
    }

    Ok(None)
}

fn extract_eq_literal(expr: &Expr) -> Option<(String, String)> {
    match expr {
        Expr::BinaryOp { left, op, right } if op.to_string() == "=" => {
            let col = match left.as_ref() {
                Expr::Identifier(id) => id.value.clone(),
                Expr::CompoundIdentifier(parts) if !parts.is_empty() => {
                    parts.last().map(|p| p.value.clone())?
                }
                _ => return None,
            };
            let lit = match right.as_ref() {
                Expr::Value(Value::SingleQuotedString(s)) => s.clone(),
                Expr::Value(Value::Number(n, _)) => n.clone(),
                _ => return None,
            };
            Some((col, lit))
        }
        _ => None,
    }
}
