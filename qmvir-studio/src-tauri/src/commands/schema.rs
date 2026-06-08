use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize)]
pub struct TableOverview {
    pub name: String,
    pub row_count: i64,
}

#[derive(Serialize)]
pub struct ColumnDef {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub position: usize,
}

#[derive(Serialize)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
}

#[derive(Serialize)]
pub struct TableDetail {
    pub name: String,
    pub columns: Vec<ColumnDef>,
    pub row_count: i64,
    pub indexes: Vec<IndexInfo>,
}

/// List all tables (name + row count) via SHOW TABLES.
#[tauri::command]
pub async fn list_tables(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
) -> Result<Vec<TableOverview>, String> {
    let instance = {
        let conns = state.connections.read();
        conns
            .get(&connection_id)
            .cloned()
            .ok_or_else(|| format!("Connection '{}' not found", connection_id))?
    };

    let result = instance.engine.execute("SHOW TABLES")?;
    let mut tables = Vec::new();
    for row in &result.rows {
        let name = match row.first() {
            Some(Some(b)) => String::from_utf8_lossy(b).into_owned(),
            _ => continue,
        };
        let row_count = match row.get(1) {
            Some(Some(b)) if b.len() == 8 => {
                i64::from_be_bytes(b[..8].try_into().unwrap_or([0; 8]))
            }
            Some(Some(b)) => String::from_utf8_lossy(b).parse::<i64>().unwrap_or(0),
            _ => 0,
        };
        tables.push(TableOverview { name, row_count });
    }
    tables.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(tables)
}

/// Get detailed table info: columns, types, indexes.
#[tauri::command]
pub async fn table_detail(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
    table_name: String,
) -> Result<TableDetail, String> {
    // Reject names with whitespace/quotes to prevent injection via SHOW COLUMNS.
    if table_name.contains(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == ';') {
        return Err("Invalid table name".to_string());
    }

    let instance = {
        let conns = state.connections.read();
        conns
            .get(&connection_id)
            .cloned()
            .ok_or_else(|| format!("Connection '{}' not found", connection_id))?
    };

    // Columns
    let col_result = instance
        .engine
        .execute(&format!("SHOW COLUMNS FROM {}", table_name))?;
    let mut columns = Vec::new();
    for (i, row) in col_result.rows.iter().enumerate() {
        let field = match row.first() {
            Some(Some(b)) => String::from_utf8_lossy(b).into_owned(),
            _ => String::new(),
        };
        let type_name = match row.get(1) {
            Some(Some(b)) => String::from_utf8_lossy(b).into_owned(),
            _ => "TEXT".to_string(),
        };
        columns.push(ColumnDef {
            name: field,
            type_name,
            nullable: true,
            position: i,
        });
    }

    // Indexes
    let idx_result = instance
        .engine
        .execute(&format!("SHOW INDEX FROM {}", table_name));
    let mut indexes = Vec::new();
    if let Ok(idx_res) = idx_result {
        for row in &idx_res.rows {
            let idx_name = match row.get(1) {
                Some(Some(b)) => String::from_utf8_lossy(b).into_owned(),
                _ => String::new(),
            };
            let col_name = match row.get(2) {
                Some(Some(b)) => String::from_utf8_lossy(b).into_owned(),
                _ => String::new(),
            };
            let unique_str = match row.get(3) {
                Some(Some(b)) => String::from_utf8_lossy(b).into_owned(),
                _ => String::new(),
            };
            indexes.push(IndexInfo {
                name: idx_name,
                columns: vec![col_name],
                unique: unique_str == "YES",
            });
        }
    }

    // Row count via SHOW TABLES
    let tables = instance.engine.execute("SHOW TABLES")?;
    let mut row_count: i64 = 0;
    for r in &tables.rows {
        let tname = match r.first() {
            Some(Some(b)) => String::from_utf8_lossy(b),
            _ => continue,
        };
        if tname == table_name {
            row_count = match r.get(1) {
                Some(Some(b)) if b.len() == 8 => {
                    i64::from_be_bytes(b[..8].try_into().unwrap_or([0; 8]))
                }
                _ => 0,
            };
            break;
        }
    }

    Ok(TableDetail {
        name: table_name,
        columns,
        row_count,
        indexes,
    })
}

// ── DDL commands ──

#[derive(Deserialize)]
pub struct NewColumn {
    pub name: String,
    pub type_name: String,
}

fn validate_identifier(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 128 {
        return Err("Name must be 1-128 characters".to_string());
    }
    if name.contains(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == ';' || c == '-') {
        return Err(format!("Invalid identifier: {}", name));
    }
    Ok(())
}

/// Create a new table with the given columns.
#[tauri::command]
pub async fn create_table(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
    table_name: String,
    columns: Vec<NewColumn>,
) -> Result<String, String> {
    validate_identifier(&table_name)?;
    if columns.is_empty() {
        return Err("At least one column required".to_string());
    }
    for col in &columns {
        validate_identifier(&col.name)?;
        validate_identifier(&col.type_name)?;
    }

    let col_defs: Vec<String> = columns
        .iter()
        .map(|c| format!("{} {}", c.name, c.type_name))
        .collect();
    let sql = format!("CREATE TABLE {} ({})", table_name, col_defs.join(", "));

    let instance = state.get_instance(&connection_id)?;
    let result = instance.engine.execute(&sql)?;
    Ok(result.command_tag)
}

/// Drop a table.
#[tauri::command]
pub async fn drop_table(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
    table_name: String,
) -> Result<String, String> {
    validate_identifier(&table_name)?;
    let instance = state.get_instance(&connection_id)?;
    let result = instance.engine.execute(&format!("DROP TABLE {}", table_name))?;
    Ok(result.command_tag)
}

/// Create an index on a table.
#[tauri::command]
pub async fn create_index(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
    table_name: String,
    index_name: String,
    columns: Vec<String>,
    unique: bool,
) -> Result<String, String> {
    validate_identifier(&table_name)?;
    validate_identifier(&index_name)?;
    for c in &columns {
        validate_identifier(c)?;
    }
    if columns.is_empty() {
        return Err("At least one column required for index".to_string());
    }
    let uq = if unique { "UNIQUE " } else { "" };
    let sql = format!(
        "CREATE {}INDEX {} ON {} ({})",
        uq,
        index_name,
        table_name,
        columns.join(", ")
    );
    let instance = state.get_instance(&connection_id)?;
    let result = instance.engine.execute(&sql)?;
    Ok(result.command_tag)
}

/// Drop an index.
#[tauri::command]
pub async fn drop_index(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
    index_name: String,
) -> Result<String, String> {
    validate_identifier(&index_name)?;
    let instance = state.get_instance(&connection_id)?;
    let result = instance.engine.execute(&format!("DROP INDEX {}", index_name))?;
    Ok(result.command_tag)
}

/// Truncate a table (delete all rows, keep structure).
#[tauri::command]
pub async fn truncate_table(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
    table_name: String,
) -> Result<String, String> {
    validate_identifier(&table_name)?;
    let instance = state.get_instance(&connection_id)?;
    let result = instance.engine.execute(&format!("DELETE FROM {}", table_name))?;
    Ok(result.command_tag)
}

/// Add a column to a table.
#[tauri::command]
pub async fn add_column(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
    table_name: String,
    column_name: String,
    column_type: String,
) -> Result<String, String> {
    validate_identifier(&table_name)?;
    validate_identifier(&column_name)?;
    validate_identifier(&column_type)?;
    let instance = state.get_instance(&connection_id)?;
    let sql = format!("ALTER TABLE {} ADD COLUMN {} {}", table_name, column_name, column_type);
    let result = instance.engine.execute(&sql)?;
    Ok(result.command_tag)
}

/// Drop a column from a table.
#[tauri::command]
pub async fn drop_column(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
    table_name: String,
    column_name: String,
) -> Result<String, String> {
    validate_identifier(&table_name)?;
    validate_identifier(&column_name)?;
    let instance = state.get_instance(&connection_id)?;
    let sql = format!("ALTER TABLE {} DROP COLUMN {}", table_name, column_name);
    let result = instance.engine.execute(&sql)?;
    Ok(result.command_tag)
}

/// Rename a table.
#[tauri::command]
pub async fn rename_table(
    state: tauri::State<'_, Arc<AppState>>,
    connection_id: String,
    old_name: String,
    new_name: String,
) -> Result<String, String> {
    validate_identifier(&old_name)?;
    validate_identifier(&new_name)?;
    let instance = state.get_instance(&connection_id)?;
    let sql = format!("ALTER TABLE {} RENAME TO {}", old_name, new_name);
    let result = instance.engine.execute(&sql)?;
    Ok(result.command_tag)
}
