//! `qm stat` — engine statistics.

use crate::gateway::native_sql::NativeSqlEngine;

pub fn run_stat(engine: &NativeSqlEngine, json: bool) {
    let tables = engine.tables.to_native_map();
    let table_count = tables.len();
    let total_rows: usize = tables.values().map(|t| t.rows.len()).sum();

    if json {
        println!("{{");
        println!("  \"tables\": {table_count},");
        println!("  \"total_rows\": {total_rows}");
        println!("}}");
    } else {
        println!("QMvir v{} — Statistics", env!("CARGO_PKG_VERSION"));
        println!("─────────────────────────────");
        println!("  Tables:      {table_count}");
        println!("  Total rows:  {total_rows}");
    }
}
