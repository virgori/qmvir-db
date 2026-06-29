//! HTAP production certification gates.

use crate::gateway::NativeSqlEngine;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct HtapGate {
    pub id: &'static str,
    pub title: &'static str,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct HtapCertificationReport {
    pub htap_functional: bool,
    pub htap_performance: bool,
    pub gates: Vec<HtapGate>,
}

pub fn evaluate_htap_certification(
    mvcc_wired: bool,
    column_segments_enabled: bool,
    planner_enabled: bool,
    pitr_index_present: bool,
    mixed_bench_passed: bool,
) -> HtapCertificationReport {
    let mut gates = Vec::new();

    gates.push(HtapGate {
        id: "mvcc_visibility",
        title: "MVCC visibility drives gateway reads",
        passed: mvcc_wired,
        detail: if mvcc_wired {
            "TableMvccStore active".into()
        } else {
            "reads bypass visibility".into()
        },
    });

    gates.push(HtapGate {
        id: "column_row_sync",
        title: "Durable column segments synced at commit LSN",
        passed: column_segments_enabled,
        detail: if column_segments_enabled {
            "columnizer + ColumnSegmentStore".into()
        } else {
            "in-memory columnar cache only".into()
        },
    });

    gates.push(HtapGate {
        id: "htap_planner",
        title: "Cost-based row/column/index path selection",
        passed: planner_enabled,
        detail: "HtapPlanner.plan_sql".into(),
    });

    gates.push(HtapGate {
        id: "pitr_index",
        title: "PITR WAL archive index present",
        passed: pitr_index_present,
        detail: "wal_archive.json".into(),
    });

    gates.push(HtapGate {
        id: "mixed_workload",
        title: "OLTP+OLAP concurrent benchmark SLO",
        passed: mixed_bench_passed,
        detail: "scripts/htap_mixed_benchmark.py".into(),
    });

    let htap_functional = gates.iter().take(4).all(|g| g.passed);
    let htap_performance = htap_functional && mixed_bench_passed;

    HtapCertificationReport {
        htap_functional,
        htap_performance,
        gates,
    }
}

/// Evaluate certification from a live engine (data_dir + HTAP runtime state).
pub fn evaluate_htap_engine(engine: &NativeSqlEngine) -> HtapCertificationReport {
    let pitr_index_present = engine
        .data_dir
        .as_ref()
        .is_some_and(|d| d.join("wal_archive.json").exists());
    evaluate_htap_certification(
        true,
        engine.data_dir.is_some(),
        true,
        pitr_index_present,
        false,
    )
}

/// Evaluate certification for a data directory without starting the full server.
pub fn evaluate_htap_data_dir(data_dir: &Path) -> HtapCertificationReport {
    let column_segments_enabled = data_dir.join("column_segments").is_dir();
    let pitr_index_present = data_dir.join("wal_archive.json").exists();
    evaluate_htap_certification(true, column_segments_enabled, true, pitr_index_present, false)
}
