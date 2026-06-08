//! `qm inspect` — table and engine introspection.

use crate::gateway::native_sql::NativeSqlEngine;

/// List all tables.
pub fn run_list_tables(engine: &NativeSqlEngine) {
    let tables = engine.tables.read().unwrap();
    if tables.is_empty() {
        println!("No tables found.");
        return;
    }
    println!("{:<30} {:>10}  Columns", "Table", "Rows");
    println!("{}", "─".repeat(70));
    for (name, table) in tables.iter() {
        let col_info: Vec<String> = table
            .columns
            .iter()
            .zip(table.column_types.iter())
            .map(|(c, t)| format!("{c} ({t:?})"))
            .collect();
        println!(
            "{:<30} {:>10}  {}",
            name,
            table.rows.len(),
            col_info.join(", ")
        );
    }
}

/// Show details for a specific table.
pub fn run_inspect_table(engine: &NativeSqlEngine, table_name: &str) {
    let tables = engine.tables.read().unwrap();
    let Some(table) = tables.get(table_name) else {
        eprintln!("error: table '{table_name}' not found");
        std::process::exit(1);
    };

    println!("Table: {table_name}");
    println!("  Rows: {}", table.rows.len());
    println!("  Columns:");
    for (i, (col, ctype)) in table
        .columns
        .iter()
        .zip(table.column_types.iter())
        .enumerate()
    {
        println!("    {i}: {col} ({ctype:?})");
    }

    // Sample first 5 rows
    println!();
    println!("  Sample rows (first 5):");
    let header: Vec<&str> = table.columns.iter().map(|s| s.as_str()).collect();
    println!("    {}", header.join(" | "));
    println!("    {}", "─".repeat(header.len() * 15));
    for (_, row) in table.rows.iter().take(5) {
        let vals: Vec<String> = table
            .columns
            .iter()
            .map(|col| match row.cols.get(col) {
                Some(cell) => cell.as_text(),
                None => "NULL".to_string(),
            })
            .collect();
        println!("    {}", vals.join(" | "));
    }
}
