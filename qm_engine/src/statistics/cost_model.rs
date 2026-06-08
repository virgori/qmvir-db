/*
 * Cost Model — I/O + CPU Cost Estimation for Query Planner
 *
 * Provides cost estimates for every physical operator to enable
 * the query planner to choose optimal access paths.
 *
 * Cost units are abstract but calibrated so that:
 *   • 1.0 = one random page read (baseline)
 *   • Sequential page read = 0.25 (4× cheaper)
 *   • CPU comparison = 0.001
 *   • Hash computation = 0.002
 */

/// Table statistics used by the cost model.
#[derive(Clone, Debug)]
pub struct TableStats {
    pub row_count: u64,
    pub page_count: u64,
    pub avg_row_size: usize,
    pub distinct_values: Vec<(String, u64)>, // (column, ndistinct)
}

impl TableStats {
    pub fn new(row_count: u64, avg_row_size: usize) -> Self {
        let page_count = (row_count as usize * avg_row_size / 8192).max(1) as u64;
        Self {
            row_count,
            page_count,
            avg_row_size,
            distinct_values: Vec::new(),
        }
    }

    pub fn with_distinct(mut self, column: &str, ndistinct: u64) -> Self {
        self.distinct_values.push((column.to_string(), ndistinct));
        self
    }

    pub fn ndistinct(&self, column: &str) -> u64 {
        self.distinct_values
            .iter()
            .find(|(c, _)| c == column)
            .map(|(_, n)| *n)
            .unwrap_or(self.row_count)
    }
}

/// Cost breakdown for a physical plan.
#[derive(Clone, Debug)]
pub struct Cost {
    pub io_cost: f64,
    pub cpu_cost: f64,
    pub total: f64,
    pub estimated_rows: f64,
}

impl Cost {
    pub fn new(io: f64, cpu: f64, rows: f64) -> Self {
        Self {
            io_cost: io,
            cpu_cost: cpu,
            total: io + cpu,
            estimated_rows: rows,
        }
    }

    pub fn zero() -> Self {
        Self::new(0.0, 0.0, 0.0)
    }

    pub fn add(&self, other: &Cost) -> Cost {
        Cost::new(
            self.io_cost + other.io_cost,
            self.cpu_cost + other.cpu_cost,
            self.estimated_rows + other.estimated_rows,
        )
    }
}

/// Cost calibration constants.
#[derive(Clone, Debug)]
pub struct CostConstants {
    /// Random page read cost (baseline = 1.0)
    pub random_page_cost: f64,
    /// Sequential page read cost
    pub seq_page_cost: f64,
    /// CPU cost per tuple comparison
    pub cpu_tuple_cost: f64,
    /// CPU cost per index entry comparison
    pub cpu_index_cost: f64,
    /// CPU cost per hash computation
    pub cpu_hash_cost: f64,
    /// CPU cost per operator application
    pub cpu_operator_cost: f64,
}

impl Default for CostConstants {
    fn default() -> Self {
        Self {
            random_page_cost: 4.0,
            seq_page_cost: 1.0,
            cpu_tuple_cost: 0.01,
            cpu_index_cost: 0.005,
            cpu_hash_cost: 0.02,
            cpu_operator_cost: 0.0025,
        }
    }
}

/// Cost model for the query planner.
pub struct CostModel {
    constants: CostConstants,
}

impl CostModel {
    pub fn new() -> Self {
        Self {
            constants: CostConstants::default(),
        }
    }

    pub fn with_constants(constants: CostConstants) -> Self {
        Self { constants }
    }

    // ── Selectivity estimation ──────────────────────────────────────

    /// Equality predicate selectivity: 1/ndistinct.
    pub fn selectivity_eq(&self, stats: &TableStats, column: &str) -> f64 {
        let nd = stats.ndistinct(column) as f64;
        if nd <= 0.0 {
            return 1.0;
        }
        1.0 / nd
    }

    /// Range predicate selectivity (fraction of rows matching).
    pub fn selectivity_range(&self, _stats: &TableStats, fraction: f64) -> f64 {
        fraction.max(0.0).min(1.0)
    }

    /// NOT selectivity.
    pub fn selectivity_not(&self, sel: f64) -> f64 {
        1.0 - sel
    }

    /// AND selectivity (independent predicates).
    pub fn selectivity_and(&self, a: f64, b: f64) -> f64 {
        a * b
    }

    /// OR selectivity (independent predicates).
    pub fn selectivity_or(&self, a: f64, b: f64) -> f64 {
        a + b - a * b
    }

    // ── Scan costs ──────────────────────────────────────────────────

    /// Full sequential scan cost.
    pub fn seq_scan(&self, stats: &TableStats) -> Cost {
        let io = stats.page_count as f64 * self.constants.seq_page_cost;
        let cpu = stats.row_count as f64 * self.constants.cpu_tuple_cost;
        Cost::new(io, cpu, stats.row_count as f64)
    }

    /// Index scan cost (B+Tree point lookup).
    pub fn index_scan_eq(&self, stats: &TableStats, column: &str) -> Cost {
        let sel = self.selectivity_eq(stats, column);
        let rows = (stats.row_count as f64 * sel).max(1.0);
        let height = (stats.row_count as f64).log2().ceil().max(1.0); // tree height
        let leaf_pages = (rows / 100.0).max(1.0); // ~100 entries per leaf
        let io = (height + leaf_pages) * self.constants.random_page_cost;
        let cpu = height * self.constants.cpu_index_cost + rows * self.constants.cpu_tuple_cost;
        Cost::new(io, cpu, rows)
    }

    /// Index range scan cost.
    pub fn index_scan_range(&self, stats: &TableStats, selectivity: f64) -> Cost {
        let rows = (stats.row_count as f64 * selectivity).max(1.0);
        let height = (stats.row_count as f64).log2().ceil().max(1.0);
        let leaf_pages = (rows / 100.0).max(1.0);
        // Index traversal + leaf scan + random heap page reads
        let heap_pages = (rows / (8192.0 / stats.avg_row_size as f64)).max(1.0);
        let io = height * self.constants.random_page_cost
            + leaf_pages * self.constants.seq_page_cost
            + heap_pages * self.constants.random_page_cost;
        let cpu = height * self.constants.cpu_index_cost + rows * self.constants.cpu_tuple_cost;
        Cost::new(io, cpu, rows)
    }

    /// Bitmap index scan cost (Roaring bitmap AND/OR).
    pub fn bitmap_scan(&self, stats: &TableStats, selectivity: f64) -> Cost {
        let rows = (stats.row_count as f64 * selectivity).max(1.0);
        let pages_accessed = (rows / (8192.0 / stats.avg_row_size as f64)).max(1.0);
        // Bitmap scans have lower random I/O (sorted page access)
        let io = pages_accessed * self.constants.seq_page_cost * 1.5;
        let cpu = rows * self.constants.cpu_tuple_cost;
        Cost::new(io, cpu, rows)
    }

    // ── Join costs ──────────────────────────────────────────────────

    /// Nested loop join cost.
    pub fn nested_loop_join(&self, outer: &Cost, inner_stats: &TableStats) -> Cost {
        let io = outer.io_cost
            + outer.estimated_rows * inner_stats.page_count as f64 * self.constants.seq_page_cost;
        let cpu = outer.cpu_cost
            + outer.estimated_rows * inner_stats.row_count as f64 * self.constants.cpu_tuple_cost;
        let rows = outer.estimated_rows * inner_stats.row_count as f64 * 0.1; // assume 10% match
        Cost::new(io, cpu, rows)
    }

    /// Hash join cost.
    pub fn hash_join(&self, build: &TableStats, probe: &TableStats) -> Cost {
        let build_io = build.page_count as f64 * self.constants.seq_page_cost;
        let probe_io = probe.page_count as f64 * self.constants.seq_page_cost;
        let io = build_io + probe_io;

        let build_cpu = build.row_count as f64 * self.constants.cpu_hash_cost;
        let probe_cpu =
            probe.row_count as f64 * (self.constants.cpu_hash_cost + self.constants.cpu_tuple_cost);
        let cpu = build_cpu + probe_cpu;

        let rows = (build.row_count.min(probe.row_count)) as f64; // conservative estimate
        Cost::new(io, cpu, rows)
    }

    /// Sort-merge join cost.
    pub fn merge_join(&self, left: &TableStats, right: &TableStats) -> Cost {
        // Sort both sides, then merge
        let sort_left = self.sort_cost(left);
        let sort_right = self.sort_cost(right);
        let merge_io = (left.page_count + right.page_count) as f64 * self.constants.seq_page_cost;
        let merge_cpu = (left.row_count + right.row_count) as f64 * self.constants.cpu_tuple_cost;

        let io = sort_left.io_cost + sort_right.io_cost + merge_io;
        let cpu = sort_left.cpu_cost + sort_right.cpu_cost + merge_cpu;
        let rows = left.row_count.min(right.row_count) as f64;
        Cost::new(io, cpu, rows)
    }

    // ── Aggregation costs ───────────────────────────────────────────

    /// Hash aggregate cost.
    pub fn hash_aggregate(&self, input: &Cost, group_columns: usize) -> Cost {
        let io = input.io_cost;
        let cpu = input.cpu_cost
            + input.estimated_rows
                * (self.constants.cpu_hash_cost * group_columns as f64
                    + self.constants.cpu_operator_cost);
        let groups = (input.estimated_rows / 10.0).max(1.0); // rough estimate
        Cost::new(io, cpu, groups)
    }

    /// Sort aggregate cost.
    pub fn sort_aggregate(&self, input: &Cost) -> Cost {
        let sort_cpu = input.estimated_rows
            * (input.estimated_rows.log2().max(1.0))
            * self.constants.cpu_tuple_cost;
        let io = input.io_cost;
        let cpu =
            input.cpu_cost + sort_cpu + input.estimated_rows * self.constants.cpu_operator_cost;
        let groups = (input.estimated_rows / 10.0).max(1.0);
        Cost::new(io, cpu, groups)
    }

    // ── Sort cost ───────────────────────────────────────────────────

    /// External sort cost (n × log(n) comparisons + I/O for spilling).
    pub fn sort_cost(&self, stats: &TableStats) -> Cost {
        let n = stats.row_count as f64;
        let io = stats.page_count as f64 * self.constants.seq_page_cost * 2.0; // read + write
        let cpu = n * n.log2().max(1.0) * self.constants.cpu_tuple_cost;
        Cost::new(io, cpu, n)
    }

    // ── Decision helpers ────────────────────────────────────────────

    /// Choose between sequential scan and index scan.
    pub fn prefer_index_scan(&self, stats: &TableStats, selectivity: f64) -> bool {
        let seq = self.seq_scan(stats);
        let idx = self.index_scan_range(stats, selectivity);
        idx.total < seq.total
    }

    /// Choose optimal join strategy.
    pub fn best_join(&self, left: &TableStats, right: &TableStats) -> &'static str {
        let nl_left = self.seq_scan(left);
        let nl = self.nested_loop_join(&nl_left, right);
        let hj = self.hash_join(left, right);
        let mj = self.merge_join(left, right);

        if hj.total <= nl.total && hj.total <= mj.total {
            "hash_join"
        } else if mj.total <= nl.total {
            "merge_join"
        } else {
            "nested_loop"
        }
    }
}

impl Default for CostModel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_stats() -> TableStats {
        TableStats::new(100_000, 128)
            .with_distinct("id", 100_000)
            .with_distinct("status", 5)
            .with_distinct("category", 100)
    }

    #[test]
    fn test_seq_scan() {
        let cm = CostModel::new();
        let stats = test_stats();
        let cost = cm.seq_scan(&stats);
        assert!(cost.total > 0.0);
        assert_eq!(cost.estimated_rows, 100_000.0);
    }

    #[test]
    fn test_index_preferred_for_selective() {
        let cm = CostModel::new();
        let stats = test_stats();
        // Very selective (0.001) → should prefer index
        assert!(cm.prefer_index_scan(&stats, 0.001));
        // Non-selective (0.5) → should prefer seq scan
        assert!(!cm.prefer_index_scan(&stats, 0.5));
    }

    #[test]
    fn test_hash_join_preferred() {
        let cm = CostModel::new();
        let left = TableStats::new(100_000, 128);
        let right = TableStats::new(50_000, 128);
        assert_eq!(cm.best_join(&left, &right), "hash_join");
    }

    #[test]
    fn test_selectivity_eq() {
        let cm = CostModel::new();
        let stats = test_stats();
        let sel = cm.selectivity_eq(&stats, "status");
        assert!((sel - 0.2).abs() < 0.01); // 1/5 = 0.2
    }

    #[test]
    fn test_selectivity_and_or() {
        let cm = CostModel::new();
        let a = 0.3;
        let b = 0.2;
        assert!((cm.selectivity_and(a, b) - 0.06).abs() < 0.001);
        assert!((cm.selectivity_or(a, b) - 0.44).abs() < 0.001);
    }

    #[test]
    fn test_cost_ordering() {
        let cm = CostModel::new();
        let stats = test_stats();

        let seq = cm.seq_scan(&stats);
        let idx_001 = cm.index_scan_range(&stats, 0.001);
        let idx_50 = cm.index_scan_range(&stats, 0.5);

        // Very selective index scan should be cheaper than full scan
        assert!(
            idx_001.total < seq.total,
            "idx(0.001)={} >= seq={}",
            idx_001.total,
            seq.total
        );
        // Half-table index scan should be more expensive
        assert!(idx_50.total > idx_001.total);
    }
}
