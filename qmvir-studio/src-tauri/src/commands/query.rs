use crate::state::{AppState, HistoryEntry};
use serde::Serialize;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const MAX_ROWS: usize = 10_000;

#[derive(Serialize)]
pub struct ColumnInfo {
    pub name: String,
    pub type_oid: i32,
    pub type_len: i16,
}

#[derive(Serialize)]
pub struct QueryResponse {
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
    pub duration_us: u64,
    pub command_tag: String,
    pub truncated: bool,
    pub query_id: String,
}

/// Auto-add LIMIT to SELECT queries that don't have one.
fn safe_sql(sql: &str) -> String {
    let upper = sql.trim().to_ascii_uppercase();
    if upper.starts_with("SELECT") && !upper.contains("LIMIT") {
        format!("{} LIMIT {}", sql.trim().trim_end_matches(';'), MAX_ROWS + 1)
    } else {
        sql.to_string()
    }
}

/// Execute a SQL query on the given connection (async, cancellable, auto-LIMIT).
#[tauri::command]
pub async fn execute_sql(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
    sql: String,
) -> Result<QueryResponse, String> {
    if sql.len() > 1_000_000 {
        return Err("Query too long (>1MB)".to_string());
    }

    let instance = state.get_instance(&connection_id)?;
    let conn_name = instance.name.clone();
    let query_id = state.next_query_id();
    let token = CancellationToken::new();
    state
        .running_queries
        .write()
        .insert(query_id.clone(), token.clone());

    let actual_sql = safe_sql(&sql);
    let qid = query_id.clone();

    // Run on blocking thread to avoid blocking Tauri main loop
    let result = tokio::select! {
        res = tokio::task::spawn_blocking(move || {
            let start = std::time::Instant::now();
            let exec_result = instance.engine.execute(&actual_sql);
            let duration_us = start.elapsed().as_micros() as u64;
            (exec_result, duration_us)
        }) => {
            res.map_err(|e| format!("Task error: {}", e))?
        }
        _ = token.cancelled() => {
            state.running_queries.write().remove(&qid);
            return Err("Query cancelled".to_string());
        }
    };

    state.running_queries.write().remove(&query_id);

    let (exec_result, duration_us) = result;
    let res = match exec_result {
        Ok(r) => r,
        Err(e) => {
            let ts = chrono::Utc::now().to_rfc3339();
            state.add_history(HistoryEntry {
                id: query_id.clone(),
                timestamp: ts,
                sql: sql.clone(),
                duration_us,
                row_count: 0,
                error: Some(e.clone()),
                connection_name: conn_name,
            });
            return Err(e);
        }
    };

    let columns: Vec<ColumnInfo> = res
        .columns
        .iter()
        .map(|(name, oid, len)| ColumnInfo {
            name: name.clone(),
            type_oid: *oid,
            type_len: *len,
        })
        .collect();

    let truncated = res.rows.len() > MAX_ROWS;
    let rows: Vec<Vec<serde_json::Value>> = res
        .rows
        .iter()
        .take(MAX_ROWS)
        .map(|row| {
            row.iter()
                .zip(res.columns.iter())
                .map(|(cell, (_name, oid, _len))| cell_to_json(cell, *oid))
                .collect()
        })
        .collect();

    let row_count = rows.len();
    let ts = chrono::Utc::now().to_rfc3339();
    state.add_history(HistoryEntry {
        id: query_id.clone(),
        timestamp: ts,
        sql: sql.clone(),
        duration_us,
        row_count,
        error: None,
        connection_name: conn_name,
    });

    Ok(QueryResponse {
        columns,
        rows,
        row_count,
        duration_us,
        command_tag: res.command_tag,
        truncated,
        query_id,
    })
}

/// Cancel a running query.
#[tauri::command]
pub async fn cancel_query(
    state: tauri::State<'_, Arc<AppState>>,
    query_id: String,
) -> Result<(), String> {
    if let Some(token) = state.running_queries.read().get(&query_id) {
        token.cancel();
        Ok(())
    } else {
        Err("Query not found or already finished".to_string())
    }
}

/// Get query history.
#[tauri::command]
pub async fn get_history(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<HistoryEntry>, String> {
    let h = state.history.read();
    Ok(h.iter().rev().take(100).cloned().collect())
}

/// Convert a wire-format cell to a serde_json::Value.
fn cell_to_json(cell: &Option<Vec<u8>>, type_oid: i32) -> serde_json::Value {
    match cell {
        None => serde_json::Value::Null,
        Some(bytes) => match type_oid {
            20 | 21 | 23 => {
                if bytes.len() == 8 {
                    let val = i64::from_be_bytes(bytes[..8].try_into().unwrap_or([0; 8]));
                    serde_json::Value::Number(val.into())
                } else {
                    serde_json::Value::String(String::from_utf8_lossy(bytes).into_owned())
                }
            }
            701 => {
                if bytes.len() == 8 {
                    let val = f64::from_be_bytes(bytes[..8].try_into().unwrap_or([0; 8]));
                    serde_json::json!(val)
                } else {
                    serde_json::Value::String(String::from_utf8_lossy(bytes).into_owned())
                }
            }
            _ => serde_json::Value::String(String::from_utf8_lossy(bytes).into_owned()),
        },
    }
}
