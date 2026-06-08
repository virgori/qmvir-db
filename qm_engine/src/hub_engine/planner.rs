use sqlparser::ast::{
    BinaryOperator, Expr, JoinConstraint, JoinOperator, OrderByExpr, SetExpr, Statement,
    TableFactor,
};

use crate::hub_engine::catalog::Catalog;
use crate::hub_engine::errors::PlanError;
use crate::hub_engine::executor::SortDirection;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinStrategy {
    Streaming,
}

#[derive(Debug, Clone)]
pub enum PhysicalPlan {
    PointLookup {
        table: String,
        key_column: String,
        key_literal: String,
    },
    HashJoin {
        build: String,
        probe: String,
        build_key: String,
        probe_key: String,
        strategy: JoinStrategy,
    },
    TopNSort {
        source: Box<PhysicalPlan>,
        sort_column: String,
        direction: SortDirection,
        limit: usize,
    },
    PassThroughSql {
        sql: String,
    },
}

pub fn plan_select(stmt: &Statement, catalog: &Catalog) -> Result<PhysicalPlan, PlanError> {
    let Statement::Query(query) = stmt else {
        return Err(PlanError::UnsupportedStatement);
    };

    let SetExpr::Select(select) = query.body.as_ref() else {
        return Err(PlanError::UnsupportedStatement);
    };

    let Some(first_from) = select.from.first() else {
        return Err(PlanError::UnsupportedStatement);
    };

    let (left_table, left_alias) =
        extract_table_and_alias(&first_from.relation).ok_or(PlanError::InvalidJoin)?;

    // Build the base plan (scan or join)
    let base_plan = if let Some(join) = first_from.joins.first() {
        match &join.join_operator {
            JoinOperator::Inner(constraint) => {
                let (right_table, right_alias) =
                    extract_table_and_alias(&join.relation).ok_or(PlanError::InvalidJoin)?;
                let (left_key, right_key) = extract_join_keys(
                    constraint,
                    &[left_table.as_str(), left_alias.as_deref().unwrap_or("")],
                    &[right_table.as_str(), right_alias.as_deref().unwrap_or("")],
                )
                .ok_or(PlanError::InvalidJoin)?;
                let left_rows = catalog
                    .get_table_meta(&left_table)
                    .map(|m| m.row_count)
                    .unwrap_or(100_000);
                let right_rows = catalog
                    .get_table_meta(&right_table)
                    .map(|m| m.row_count)
                    .unwrap_or(100_000);

                let (build_side, probe_side, build_key, probe_key) = if left_rows <= right_rows {
                    (left_table, right_table, left_key, right_key)
                } else {
                    (right_table, left_table, right_key, left_key)
                };

                PhysicalPlan::HashJoin {
                    build: build_side,
                    probe: probe_side,
                    build_key,
                    probe_key,
                    strategy: JoinStrategy::Streaming,
                }
            }
            JoinOperator::CrossJoin => PhysicalPlan::PassThroughSql {
                sql: stmt.to_string(),
            },
            _ => PhysicalPlan::PassThroughSql {
                sql: stmt.to_string(),
            },
        }
    } else {
        // No join - pass through for simple scan
        PhysicalPlan::PassThroughSql {
            sql: stmt.to_string(),
        }
    };

    // Check for ORDER BY ... LIMIT pattern
    if !query.order_by.is_empty() {
        if let Some(limit_expr) = &query.limit {
            // Extract limit value
            if let Some(limit) = extract_limit_value(limit_expr) {
                // Extract sort column and direction from first ORDER BY clause
                if let Some(first_order) = query.order_by.first() {
                    if let Some((sort_column, direction)) = extract_order_by_info(first_order) {
                        return Ok(PhysicalPlan::TopNSort {
                            source: Box::new(base_plan),
                            sort_column,
                            direction,
                            limit,
                        });
                    }
                }
            }
        }
    }

    Ok(base_plan)
}

/// Extract limit value from LIMIT expression
fn extract_limit_value(expr: &Expr) -> Option<usize> {
    match expr {
        Expr::Value(sqlparser::ast::Value::Number(n, _)) => n.parse().ok(),
        _ => None,
    }
}

/// Extract column name and direction from ORDER BY expression
fn extract_order_by_info(order_by: &OrderByExpr) -> Option<(String, SortDirection)> {
    let col_name = match &order_by.expr {
        Expr::Identifier(id) => id.value.clone(),
        Expr::CompoundIdentifier(parts) => parts.last()?.value.clone(),
        _ => return None,
    };

    let direction = if order_by.asc.unwrap_or(true) {
        SortDirection::Asc
    } else {
        SortDirection::Desc
    };

    Some((col_name, direction))
}

fn extract_table_and_alias(tf: &TableFactor) -> Option<(String, Option<String>)> {
    match tf {
        TableFactor::Table { name, alias, .. } => Some((
            name.to_string(),
            alias.as_ref().map(|a| a.name.value.clone()),
        )),
        _ => None,
    }
}

fn extract_join_keys(
    constraint: &JoinConstraint,
    left_qualifiers: &[&str],
    right_qualifiers: &[&str],
) -> Option<(String, String)> {
    let JoinConstraint::On(expr) = constraint else {
        return None;
    };
    let Expr::BinaryOp { left, op, right } = expr else {
        return None;
    };
    if !matches!(op, BinaryOperator::Eq) {
        return None;
    }

    let (lt, lc) = extract_qualified_col(left.as_ref())?;
    let (rt, rc) = extract_qualified_col(right.as_ref())?;

    let left_match = lt.is_empty() || left_qualifiers.iter().any(|q| !q.is_empty() && *q == lt);
    let right_match = rt.is_empty() || right_qualifiers.iter().any(|q| !q.is_empty() && *q == rt);
    if left_match && right_match {
        return Some((lc, rc));
    }
    let swapped_left_match =
        lt.is_empty() || right_qualifiers.iter().any(|q| !q.is_empty() && *q == lt);
    let swapped_right_match =
        rt.is_empty() || left_qualifiers.iter().any(|q| !q.is_empty() && *q == rt);
    if swapped_left_match && swapped_right_match {
        return Some((rc, lc));
    }
    None
}

fn extract_qualified_col(expr: &Expr) -> Option<(String, String)> {
    match expr {
        Expr::CompoundIdentifier(parts) if parts.len() == 2 => {
            Some((parts[0].value.clone(), parts[1].value.clone()))
        }
        Expr::Identifier(id) => Some(("".to_string(), id.value.clone())),
        _ => None,
    }
}
