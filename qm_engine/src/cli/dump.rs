//! `qm dump` — streaming data export (SQL, CSV, JSON Lines, Parquet).

use crate::gateway::native_sql::{Cell, ColType, NativeSqlEngine};
use std::io::Write;
use std::path::Path;

/// Output format for dump.
#[derive(Clone, Copy, Debug)]
pub enum DumpFormat {
    Sql,
    Csv,
    Jsonl,
    Parquet,
}

impl DumpFormat {
    pub fn from_str(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "sql" => Ok(DumpFormat::Sql),
            "csv" => Ok(DumpFormat::Csv),
            "jsonl" | "json" => Ok(DumpFormat::Jsonl),
            "parquet" => Ok(DumpFormat::Parquet),
            _ => Err(format!(
                "unknown format '{s}': expected sql, csv, jsonl, parquet"
            )),
        }
    }
}

/// Dump a single table to the given writer.
/// Returns number of rows exported.
pub fn dump_table(
    engine: &NativeSqlEngine,
    table_name: &str,
    format: DumpFormat,
    writer: &mut dyn Write,
) -> Result<u64, String> {
    let tables = engine.tables.read().unwrap();
    let t = tables
        .get(table_name)
        .ok_or(format!("table '{table_name}' not found"))?;

    let columns = t.columns.clone();
    let col_types = t.column_types.clone();
    // Collect rows so we can drop the lock
    let rows: Vec<_> = t.rows.iter().map(|(id, row)| (*id, row.clone())).collect();
    drop(tables);

    match format {
        DumpFormat::Sql => dump_sql(&columns, &col_types, &rows, table_name, writer),
        DumpFormat::Csv => dump_csv(&columns, &rows, writer),
        DumpFormat::Jsonl => dump_jsonl(&columns, &rows, writer),
        DumpFormat::Parquet => {
            return Err("parquet format: use --output <file> (cannot stream to stdout)".into());
        }
    }
}

/// Dump a single table to a Parquet file (delegates to engine's COPY TO).
pub fn dump_table_parquet(
    engine: &NativeSqlEngine,
    table_name: &str,
    output: &Path,
) -> Result<u64, String> {
    let path_str = output.to_string_lossy();
    let sql = format!("COPY {table_name} TO '{path_str}' (FORMAT PARQUET)");
    let result = engine.execute(&sql)?;
    // Parse row count from command_tag like "COPY 1234"
    let count = result
        .command_tag
        .strip_prefix("COPY ")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    Ok(count)
}

/// Run the dump command from CLI arguments.
pub fn run_dump(
    engine: &NativeSqlEngine,
    table: Option<&str>,
    format_str: &str,
    output: Option<&Path>,
    stdout: bool,
) {
    let format = match DumpFormat::from_str(format_str) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    // Get list of tables to dump
    let table_names: Vec<String> = if let Some(t) = table {
        vec![t.to_string()]
    } else {
        let tables = engine.tables.read().unwrap();
        tables.keys().cloned().collect()
    };

    if table_names.is_empty() {
        println!("No tables to dump.");
        return;
    }

    // Parquet: must have output file
    if matches!(format, DumpFormat::Parquet) {
        let out = match output {
            Some(p) => p.to_path_buf(),
            None => {
                let name = table_names.first().unwrap();
                std::path::PathBuf::from(format!("{name}.parquet"))
            }
        };
        for t in &table_names {
            let target = if table_names.len() > 1 {
                out.parent()
                    .unwrap_or(Path::new("."))
                    .join(format!("{t}.parquet"))
            } else {
                out.clone()
            };
            match dump_table_parquet(engine, t, &target) {
                Ok(n) => println!("Exported {n} rows from '{t}' → {}", target.display()),
                Err(e) => eprintln!("error dumping '{t}': {e}"),
            }
        }
        return;
    }

    // Streaming formats: SQL, CSV, JSONL
    if stdout || output.is_none() {
        // Write to stdout
        let mut out = std::io::stdout().lock();
        for t in &table_names {
            match dump_table(engine, t, format, &mut out) {
                Ok(n) => eprintln!("-- {t}: {n} rows"),
                Err(e) => eprintln!("error dumping '{t}': {e}"),
            }
        }
    } else if let Some(path) = output {
        let file = match std::fs::File::create(path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("error: cannot create '{}': {e}", path.display());
                std::process::exit(1);
            }
        };
        let mut buf = std::io::BufWriter::new(file);
        for t in &table_names {
            match dump_table(engine, t, format, &mut buf) {
                Ok(n) => println!("Exported {n} rows from '{t}' → {}", path.display()),
                Err(e) => eprintln!("error dumping '{t}': {e}"),
            }
        }
    }
}

// ── Format implementations ──────────────────────────────────────────────

fn dump_sql(
    columns: &[String],
    _col_types: &[ColType],
    rows: &[(i64, crate::gateway::native_sql::NativeRow)],
    table_name: &str,
    w: &mut dyn Write,
) -> Result<u64, String> {
    let mut count = 0u64;
    for (id, row) in rows {
        let mut vals = Vec::with_capacity(columns.len());
        for col in columns {
            if col == "id" {
                vals.push(id.to_string());
                continue;
            }
            let cell = row.cols.get(col);
            let v = match cell {
                Some(crate::gateway::native_sql::Cell::Int(v)) => v.to_string(),
                Some(crate::gateway::native_sql::Cell::Float(v)) => v.to_string(),
                Some(crate::gateway::native_sql::Cell::Text(v)) => {
                    format!("'{}'", v.replace('\'', "''"))
                }
                Some(crate::gateway::native_sql::Cell::Bool(v)) => {
                    if *v {
                        "TRUE".to_string()
                    } else {
                        "FALSE".to_string()
                    }
                }
                Some(crate::gateway::native_sql::Cell::Timestamp(v)) => v.to_string(),
                Some(crate::gateway::native_sql::Cell::Json(v)) => {
                    format!("'{}'", v.replace('\'', "''"))
                }
                Some(other) => {
                    let t = other.as_text();
                    format!("'{}'", t.replace('\'', "''"))
                }
                None => "NULL".to_string(),
            };
            vals.push(v);
        }
        writeln!(
            w,
            "INSERT INTO {table_name} ({}) VALUES ({});",
            columns.join(", "),
            vals.join(", "),
        )
        .map_err(|e| format!("write error: {e}"))?;
        count += 1;
    }
    Ok(count)
}

fn dump_csv(
    columns: &[String],
    rows: &[(i64, crate::gateway::native_sql::NativeRow)],
    w: &mut dyn Write,
) -> Result<u64, String> {
    // Header
    writeln!(w, "{}", columns.join(",")).map_err(|e| format!("write error: {e}"))?;

    let mut count = 0u64;
    for (id, row) in rows {
        let vals: Vec<String> = columns
            .iter()
            .map(|col| {
                if col == "id" {
                    return id.to_string();
                }
                match row.cols.get(col) {
                    Some(crate::gateway::native_sql::Cell::Int(v)) => v.to_string(),
                    Some(crate::gateway::native_sql::Cell::Float(v)) => v.to_string(),
                    Some(crate::gateway::native_sql::Cell::Text(v)) => {
                        // CSV escaping: quote if contains comma, newline, or quote
                        if v.contains(',') || v.contains('\n') || v.contains('"') {
                            format!("\"{}\"", v.replace('"', "\"\""))
                        } else {
                            v.clone()
                        }
                    }
                    Some(crate::gateway::native_sql::Cell::Bool(v)) => {
                        if *v {
                            "true".to_string()
                        } else {
                            "false".to_string()
                        }
                    }
                    Some(crate::gateway::native_sql::Cell::Null) | None => String::new(),
                    Some(other) => {
                        let t = other.as_text();
                        if t.contains(',') || t.contains('\n') || t.contains('"') {
                            format!("\"{}\"", t.replace('"', "\"\""))
                        } else {
                            t
                        }
                    }
                }
            })
            .collect();
        writeln!(w, "{}", vals.join(",")).map_err(|e| format!("write error: {e}"))?;
        count += 1;
    }
    Ok(count)
}

fn dump_jsonl(
    columns: &[String],
    rows: &[(i64, crate::gateway::native_sql::NativeRow)],
    w: &mut dyn Write,
) -> Result<u64, String> {
    let mut count = 0u64;
    for (id, row) in rows {
        let mut map = serde_json::Map::new();
        for col in columns {
            if col == "id" {
                map.insert(col.clone(), serde_json::Value::Number((*id).into()));
                continue;
            }
            let val = match row.cols.get(col) {
                Some(&Cell::Int(v)) => serde_json::Value::Number(v.into()),
                Some(&Cell::Float(v)) => {
                    serde_json::json!(v)
                }
                Some(Cell::Text(v)) => serde_json::Value::String(v.clone()),
                Some(&Cell::Bool(v)) => serde_json::Value::Bool(v),
                Some(&Cell::Timestamp(v)) => serde_json::Value::Number(v.into()),
                Some(Cell::Json(v)) => {
                    serde_json::from_str(v).unwrap_or(serde_json::Value::String(v.clone()))
                }
                Some(Cell::Null) | None => serde_json::Value::Null,
                Some(other) => serde_json::Value::String(other.as_text()),
            };
            map.insert(col.clone(), val);
        }
        serde_json::to_writer(&mut *w, &serde_json::Value::Object(map))
            .map_err(|e| format!("json write error: {e}"))?;
        writeln!(w).map_err(|e| format!("write error: {e}"))?;
        count += 1;
    }
    Ok(count)
}
