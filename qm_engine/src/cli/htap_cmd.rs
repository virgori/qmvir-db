//! `qm htap` — HTAP certification and isolation checks.

use crate::gateway::NativeSqlEngine;
use crate::htap::{evaluate_htap_engine, run_htap_isolation_battery};

pub fn run_certify(engine: &NativeSqlEngine, table: Option<&str>, isolation: bool) {
    let report = evaluate_htap_engine(engine);
    println!("HTAP certification:");
    println!(
        "  Functional:  {}",
        if report.htap_functional {
            "PASS"
        } else {
            "FAIL"
        }
    );
    println!(
        "  Performance: {}",
        if report.htap_performance {
            "PASS"
        } else {
            "pending (mixed benchmark)"
        }
    );
    for gate in &report.gates {
        let mark = if gate.passed { "✓" } else { "✗" };
        println!("  {mark} {} — {}", gate.id, gate.detail);
    }

    if isolation {
        let table = table.unwrap_or("htap_cert");
        if engine
            .execute(&format!(
                "CREATE TABLE IF NOT EXISTS {table} (id INTEGER PRIMARY KEY, val INTEGER)"
            ))
            .is_err()
        {
            eprintln!("error: could not prepare isolation table '{table}'");
            std::process::exit(1);
        }
        let iso = run_htap_isolation_battery(engine, table);
        println!();
        println!("Isolation battery:");
        if iso.passed {
            println!("  PASS ({} checks)", 3);
        } else {
            println!("  FAIL");
            for v in &iso.violations {
                println!("    {}: {}", v.check, v.detail);
            }
            std::process::exit(1);
        }
    }

    if !report.htap_functional {
        std::process::exit(1);
    }
}
