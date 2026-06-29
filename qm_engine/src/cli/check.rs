//! `qm check` — integrity verification.

use crate::gateway::native_sql::NativeSqlEngine;

pub fn run_check(engine: &NativeSqlEngine, table: Option<&str>) {
    let tables = engine.tables.to_native_map();

    if let Some(name) = table {
        if let Some(t) = tables.get(name) {
            check_table(name, t);
        } else {
            eprintln!("error: table '{name}' not found");
            std::process::exit(1);
        }
    } else {
        println!("Checking all tables...");
        let mut ok = true;
        for (name, t) in tables.iter() {
            if !check_table(name, t) {
                ok = false;
            }
        }
        if ok {
            println!("✓ All tables OK");
        } else {
            println!("✗ Some tables have issues");
            std::process::exit(1);
        }
    }
}

fn check_table(name: &str, table: &crate::gateway::native_sql::NativeTable) -> bool {
    let col_count = table.columns.len();
    let type_count = table.column_types.len();
    let mut ok = true;

    if col_count != type_count {
        println!("  ✗ {name}: column/type count mismatch ({col_count} vs {type_count})");
        ok = false;
    }

    // Check each row has all declared columns
    for (row_id, row) in &table.rows {
        if row.cols.len() != col_count {
            println!(
                "  ✗ {name}: row {row_id} has {} cols, expected {col_count}",
                row.cols.len()
            );
            ok = false;
        }
    }

    if ok {
        println!(
            "  ✓ {name}: {} rows, {col_count} columns — OK",
            table.rows.len()
        );
    }
    ok
}
