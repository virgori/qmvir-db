//! HTAP cost-based path selection — row vs column vs index.

use crate::cluster::WorkloadClass;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanPath {
    IndexPoint,
    RowScan,
    ColumnScan,
    VectorHnsw,
    FtsInverted,
}

#[derive(Clone, Debug)]
pub struct HtapPhysicalPlan {
    pub workload: WorkloadClass,
    pub path: ScanPath,
    pub estimated_rows: u64,
    pub use_spill: bool,
    pub reason: String,
}

pub struct HtapPlanner {
    pub column_scan_min_rows: u64,
    pub hnsw_min_rows: u64,
}

impl Default for HtapPlanner {
    fn default() -> Self {
        Self::new()
    }
}

impl HtapPlanner {
    pub fn new() -> Self {
        Self {
            column_scan_min_rows: 4096,
            hnsw_min_rows: 256,
        }
    }

    pub fn plan_sql(&self, sql: &str, table_rows: u64, has_column_seg: bool) -> HtapPhysicalPlan {
        let workload = crate::cluster::QmRouter::classify_sql(sql);
        let up = sql.to_ascii_uppercase();

        if up.contains(" <-> ") || up.contains(" <=> ") || up.contains(" <#> ") {
            let path = if table_rows >= self.hnsw_min_rows {
                ScanPath::VectorHnsw
            } else {
                ScanPath::RowScan
            };
            return HtapPhysicalPlan {
                workload,
                path,
                estimated_rows: table_rows,
                use_spill: false,
                reason: "vector ORDER BY distance".into(),
            };
        }

        if up.contains(" @@ ") || up.contains(" TO_TSVECTOR") {
            return HtapPhysicalPlan {
                workload: WorkloadClass::Search,
                path: ScanPath::FtsInverted,
                estimated_rows: table_rows,
                use_spill: false,
                reason: "full-text predicate".into(),
            };
        }

        if up.contains("WHERE ID =") || up.contains("WHERE \"ID\" =") {
            return HtapPhysicalPlan {
                workload,
                path: ScanPath::IndexPoint,
                estimated_rows: 1,
                use_spill: false,
                reason: "equality on id".into(),
            };
        }

        let analytic = up.contains("GROUP BY")
            || up.contains("SUM(")
            || up.contains("AVG(")
            || up.contains("COUNT(")
            || up.contains(" BETWEEN ");

        if analytic && has_column_seg && table_rows >= self.column_scan_min_rows {
            return HtapPhysicalPlan {
                workload: WorkloadClass::Analytics,
                path: ScanPath::ColumnScan,
                estimated_rows: table_rows,
                use_spill: table_rows > 1_000_000,
                reason: "analytic scan with durable column segment".into(),
            };
        }

        HtapPhysicalPlan {
            workload,
            path: ScanPath::RowScan,
            estimated_rows: table_rows,
            use_spill: table_rows > 2_000_000 && analytic,
            reason: "default row path".into(),
        }
    }
}
