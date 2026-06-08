//! `qm schema` — schema diff, export, and migration.

use crate::gateway::native_sql::NativeSqlEngine;
use std::path::Path;

/// Difference between two column sets.
pub struct TableDiff {
    pub table: String,
    pub added_columns: Vec<(String, String)>, // (name, type)
    pub removed_columns: Vec<String>,
    pub type_changes: Vec<(String, String, String)>, // (col, old_type, new_type)
}

/// Full schema diff result.
pub struct SchemaDiff {
    pub added_tables: Vec<String>,
    pub removed_tables: Vec<String>,
    pub modified_tables: Vec<TableDiff>,
    pub identical_tables: Vec<String>,
}

/// Diff two engines' schemas.
pub fn diff_schemas(a: &NativeSqlEngine, b: &NativeSqlEngine) -> SchemaDiff {
    let ta = a.tables.read().unwrap();
    let tb = b.tables.read().unwrap();

    let keys_a: std::collections::HashSet<&String> = ta.keys().collect();
    let keys_b: std::collections::HashSet<&String> = tb.keys().collect();

    let added: Vec<String> = keys_b.difference(&keys_a).map(|s| (*s).clone()).collect();
    let removed: Vec<String> = keys_a.difference(&keys_b).map(|s| (*s).clone()).collect();

    let mut modified = Vec::new();
    let mut identical = Vec::new();

    for name in keys_a.intersection(&keys_b) {
        let table_a = ta.get(*name).unwrap();
        let table_b = tb.get(*name).unwrap();

        let cols_a: std::collections::HashSet<&String> = table_a.columns.iter().collect();
        let cols_b: std::collections::HashSet<&String> = table_b.columns.iter().collect();

        let added_cols: Vec<(String, String)> = cols_b
            .difference(&cols_a)
            .map(|c| {
                let idx = table_b.columns.iter().position(|x| x == *c).unwrap();
                let ty = format!("{:?}", table_b.column_types[idx]);
                ((*c).clone(), ty)
            })
            .collect();

        let removed_cols: Vec<String> = cols_a.difference(&cols_b).map(|c| (*c).clone()).collect();

        let mut type_changes = Vec::new();
        for col in cols_a.intersection(&cols_b) {
            let idx_a = table_a.columns.iter().position(|x| x == *col).unwrap();
            let idx_b = table_b.columns.iter().position(|x| x == *col).unwrap();
            if table_a.column_types[idx_a] != table_b.column_types[idx_b] {
                type_changes.push((
                    (*col).clone(),
                    format!("{:?}", table_a.column_types[idx_a]),
                    format!("{:?}", table_b.column_types[idx_b]),
                ));
            }
        }

        if added_cols.is_empty() && removed_cols.is_empty() && type_changes.is_empty() {
            identical.push((*name).clone());
        } else {
            modified.push(TableDiff {
                table: (*name).clone(),
                added_columns: added_cols,
                removed_columns: removed_cols,
                type_changes,
            });
        }
    }

    SchemaDiff {
        added_tables: added,
        removed_tables: removed,
        modified_tables: modified,
        identical_tables: identical,
    }
}

/// Generate migration SQL from a diff.
pub fn generate_migration_sql(diff: &SchemaDiff, source_b: &NativeSqlEngine) -> String {
    let mut sql = String::new();

    // New tables
    for name in &diff.added_tables {
        let tables = source_b.tables.read().unwrap();
        if let Some(t) = tables.get(name) {
            let cols: Vec<String> = t
                .columns
                .iter()
                .zip(t.column_types.iter())
                .map(|(c, ty)| format!("{c} {}", coltype_sql(ty)))
                .collect();
            sql.push_str(&format!("CREATE TABLE {name} ({});\n", cols.join(", ")));
        }
    }

    // Dropped tables
    for name in &diff.removed_tables {
        sql.push_str(&format!("DROP TABLE {name};\n"));
    }

    // Modified tables
    for td in &diff.modified_tables {
        for (col, ty) in &td.added_columns {
            sql.push_str(&format!(
                "ALTER TABLE {} ADD COLUMN {col} {ty};\n",
                td.table
            ));
        }
        for col in &td.removed_columns {
            sql.push_str(&format!("ALTER TABLE {} DROP COLUMN {col};\n", td.table));
        }
        for (col, _old, new_ty) in &td.type_changes {
            sql.push_str(&format!(
                "ALTER TABLE {} ALTER COLUMN {col} TYPE {new_ty};\n",
                td.table
            ));
        }
    }

    sql
}

fn coltype_sql(ct: &crate::gateway::native_sql::ColType) -> &'static str {
    match ct {
        crate::gateway::native_sql::ColType::Integer => "INT",
        crate::gateway::native_sql::ColType::Float8 => "FLOAT",
        crate::gateway::native_sql::ColType::Text => "TEXT",
        crate::gateway::native_sql::ColType::Boolean => "BOOLEAN",
        crate::gateway::native_sql::ColType::Timestamp => "TIMESTAMP",
        crate::gateway::native_sql::ColType::Date => "DATE",
        crate::gateway::native_sql::ColType::Interval => "INTERVAL",
        crate::gateway::native_sql::ColType::Json => "JSON",
        crate::gateway::native_sql::ColType::Jsonb => "JSONB",
        crate::gateway::native_sql::ColType::Bytea => "BYTEA",
        crate::gateway::native_sql::ColType::Uuid => "UUID",
        crate::gateway::native_sql::ColType::Array => "TEXT[]",
        crate::gateway::native_sql::ColType::Numeric => "NUMERIC",
    }
}

/// Export DDL (CREATE TABLE statements) for all tables.
pub fn export_ddl(engine: &NativeSqlEngine) -> String {
    let tables = engine.tables.read().unwrap();
    let mut ddl = String::new();
    let mut names: Vec<&String> = tables.keys().collect();
    names.sort();
    for name in names {
        let t = tables.get(name).unwrap();
        let cols: Vec<String> = t
            .columns
            .iter()
            .zip(t.column_types.iter())
            .map(|(c, ty)| format!("{c} {}", coltype_sql(ty)))
            .collect();
        ddl.push_str(&format!("CREATE TABLE {name} ({});\n", cols.join(", ")));
    }
    ddl
}

// ── CLI entry points ──────────────────────────────────────────────────

/// `qm schema diff <dir_a> <dir_b>`
pub fn run_diff(dir_a: &Path, dir_b: &Path, output: Option<&Path>) {
    let engine_a = NativeSqlEngine::with_data_dir(dir_a.to_path_buf());
    let engine_b = NativeSqlEngine::with_data_dir(dir_b.to_path_buf());

    let diff = diff_schemas(&engine_a, &engine_b);

    // Print diff summary
    println!("--- {}", dir_a.display());
    println!("+++ {}", dir_b.display());
    println!();

    for name in &diff.added_tables {
        println!("+ CREATE TABLE {name}");
    }
    for name in &diff.removed_tables {
        println!("- DROP TABLE {name}");
    }
    for td in &diff.modified_tables {
        println!("Table \"{}\":", td.table);
        for (col, ty) in &td.added_columns {
            println!("  + ADD COLUMN {col} {ty}");
        }
        for col in &td.removed_columns {
            println!("  - DROP COLUMN {col}");
        }
        for (col, old, new) in &td.type_changes {
            println!("  ~ ALTER COLUMN {col}: {old} → {new}");
        }
    }
    for name in &diff.identical_tables {
        println!("Table \"{name}\": (identical)");
    }

    // Generate migration SQL if requested
    if let Some(out_path) = output {
        let sql = generate_migration_sql(&diff, &engine_b);
        if let Err(e) = std::fs::write(out_path, &sql) {
            eprintln!("error: cannot write '{}': {e}", out_path.display());
            std::process::exit(1);
        }
        let stmt_count = sql.lines().filter(|l| !l.is_empty()).count();
        println!(
            "\nGenerated: {} ({stmt_count} statements)",
            out_path.display()
        );
    }
}

/// `qm schema export --data-dir <dir>`
pub fn run_export(engine: &NativeSqlEngine) {
    let ddl = export_ddl(engine);
    print!("{ddl}");
}

/// `qm schema migrate <file> --data-dir <dir>`
pub fn run_migrate(engine: &NativeSqlEngine, file: &Path, dry_run: bool) {
    let content = match std::fs::read_to_string(file) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot read '{}': {e}", file.display());
            std::process::exit(1);
        }
    };

    // Split on semicolons and execute each statement
    for stmt in content.split(';') {
        let stmt = stmt.trim();
        if stmt.is_empty() {
            continue;
        }
        if dry_run {
            println!("  {stmt};  → OK (simulated)");
        } else {
            match engine.execute(&format!("{stmt};")) {
                Ok(result) => println!("  {stmt};  → {}", result.command_tag),
                Err(e) => eprintln!("  {stmt};  → ERROR: {e}"),
            }
        }
    }
}
