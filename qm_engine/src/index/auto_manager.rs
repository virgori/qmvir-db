/*
 * Autonomous Index Manager – "The Brain"
 *
 * Observes query patterns, estimates costs, and automatically creates or
 * drops B+Tree indexes based on heuristic rules.
 *
 * Key behaviours:
 *   • Auto-Create when column frequency > threshold AND selectivity is high.
 *   • Auto-Drop when an index is unused for N seconds AND write overhead is high.
 *   • Shadow Indexing: validate new index speeds up queries by > 30 % before
 *     promoting it to the live set.
 *   • Background thread (low priority) for online index building – never
 *     blocks the main query path.
 */

use super::bplus_tree::{BPlusTree, IndexKey, RowId};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

const HIST_BUCKETS: usize = 16;

#[derive(Debug, Clone)]
pub struct NumericHistogram {
    pub min: f64,
    pub max: f64,
    pub total: u64,
    pub buckets: [u64; HIST_BUCKETS],
    pub samples: Vec<f64>,
}

impl Default for NumericHistogram {
    fn default() -> Self {
        Self {
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            total: 0,
            buckets: [0; HIST_BUCKETS],
            samples: Vec::new(),
        }
    }
}

impl NumericHistogram {
    pub fn record(&mut self, value: f64) {
        if value.is_nan() {
            return;
        }
        if self.total == 0 {
            self.min = value;
            self.max = value;
            self.total = 1;
            self.buckets[0] = 1;
            self.samples.push(value);
            return;
        }

        if value < self.min {
            self.min = value;
        }
        if value > self.max {
            self.max = value;
        }

        let idx = self.bucket_for(value);
        self.buckets[idx] = self.buckets[idx].saturating_add(1);
        self.total = self.total.saturating_add(1);
        if self.samples.len() < 4096 {
            self.samples.push(value);
        }
    }

    fn bucket_for(&self, value: f64) -> usize {
        if self.max <= self.min {
            return 0;
        }
        let ratio = ((value - self.min) / (self.max - self.min)).clamp(0.0, 1.0);
        let mut idx = (ratio * HIST_BUCKETS as f64) as usize;
        if idx >= HIST_BUCKETS {
            idx = HIST_BUCKETS - 1;
        }
        idx
    }

    pub fn estimate_selectivity_between(&self, lo: f64, hi: f64) -> Option<f64> {
        if self.total == 0 {
            return None;
        }
        let (lo_v, hi_v) = if lo <= hi { (lo, hi) } else { (hi, lo) };

        if !self.samples.is_empty() {
            let in_range = self
                .samples
                .iter()
                .filter(|v| **v >= lo_v && **v <= hi_v)
                .count();
            return Some((in_range as f64 / self.samples.len() as f64).clamp(0.0, 1.0));
        }

        if hi < self.min || lo > self.max {
            return Some(0.0);
        }
        if self.max <= self.min {
            return Some(1.0);
        }

        let lo_idx = self.bucket_for(lo_v);
        let hi_idx = self.bucket_for(hi_v);
        let mut in_range = 0u64;
        for i in lo_idx..=hi_idx {
            in_range = in_range.saturating_add(self.buckets[i]);
        }
        Some((in_range as f64 / self.total as f64).clamp(0.0, 1.0))
    }
}

// ── Configuration knobs ─────────────────────────────────────────────────

/// Minimum query hits on a column before we consider auto-creating an index.
/// Lowered from 50→10 so text/bool filters benefit from auto-indexing quickly.
const AUTO_CREATE_THRESHOLD: u64 = 10;

/// Selectivity ceiling – only auto-index if the column filters out at least
/// this fraction (0.0 = filters everything, 1.0 = filters nothing).
const MAX_SELECTIVITY_FOR_AUTO: f64 = 0.30;

/// If an index has not been used in this many seconds we consider dropping it.
const UNUSED_TTL_SECS: u64 = 86_400; // 24 hours default

/// An auto-built shadow index must improve query latency by at least this
/// factor before being promoted.
const SHADOW_SPEEDUP_THRESHOLD: f64 = 1.30; // 30 % faster

// ── Column statistics ───────────────────────────────────────────────────

/// Per-column access statistics tracked by the Query Observer.
#[derive(Debug, Clone)]
pub struct ColumnStats {
    /// How many queries referenced this column in WHERE / JOIN ON / ORDER BY.
    pub query_hits: u64,
    /// Number of writes (INSERT/UPDATE) to the table since last check.
    pub write_ops: u64,
    /// Estimated selectivity (distinct / total rows) – updated periodically.
    pub selectivity: f64,
    /// Total rows in the table at last estimate.
    pub table_rows: u64,
    /// Distinct values sampled.
    pub distinct_count: u64,
    /// Timestamp of last query hit.
    pub last_query_at: Instant,
    /// Timestamp of last write.
    pub last_write_at: Instant,
    /// Numeric histogram for runtime data distribution statistics.
    pub histogram: NumericHistogram,
}

impl Default for ColumnStats {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            query_hits: 0,
            write_ops: 0,
            selectivity: 1.0,
            table_rows: 0,
            distinct_count: 0,
            last_query_at: now,
            last_write_at: now,
            histogram: NumericHistogram::default(),
        }
    }
}

// ── Index metadata ──────────────────────────────────────────────────────

/// State of a managed index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    /// Being built in a background thread.
    Building,
    /// Shadow testing – queries are benchmarked with and without.
    Shadow,
    /// Fully active and used by the query planner.
    Active,
    /// Marked for removal.
    PendingDrop,
    /// Created manually by user (never auto-dropped).
    Manual,
}

/// Metadata for a single managed index.
#[derive(Debug, Clone)]
pub struct IndexMeta {
    pub name: String,
    pub table: String,
    pub columns: Vec<String>,
    pub state: IndexState,
    pub created_at: Instant,
    pub last_used_at: Instant,
    /// Number of times the query planner chose this index.
    pub use_count: u64,
    /// Whether the user created it explicitly (`CREATE INDEX`).
    pub manual: bool,
    /// Cumulative query latency (ns) when this shadow index was used.
    pub shadow_latency_ns: u64,
    /// Cumulative query latency (ns) for the same queries WITHOUT this index.
    pub baseline_latency_ns: u64,
    /// Number of latency samples recorded (for computing averages).
    pub latency_samples: u64,
    /// Estimated memory footprint in bytes (updated during build/populate).
    pub estimated_memory_bytes: u64,
    /// Number of queries that benefited from this index (shadow hit).
    pub benefiting_queries: u64,
    /// Total queries observed while this index was in shadow.
    pub total_queries: u64,
}

// ── Cost-Benefit Analysis ───────────────────────────────────────────────

/// Result of cost-benefit analysis for an index.
#[derive(Debug, Clone)]
pub struct IndexCostBenefit {
    /// Estimated memory cost in bytes.
    pub memory_bytes: u64,
    /// Average query speedup ratio (>1 means faster with index).
    pub speedup_ratio: f64,
    /// Number of queries that benefited.
    pub benefiting_queries: u64,
    /// Total queries observed.
    pub total_queries: u64,
    /// Composite score = speedup_ratio * hit_ratio / memory_MB.
    pub score: f64,
}

impl IndexCostBenefit {
    /// Compute cost-benefit from an IndexMeta's shadow statistics.
    pub fn compute(meta: &IndexMeta) -> Self {
        let memory_bytes = meta.estimated_memory_bytes;
        let memory_mb = (memory_bytes as f64 / 1_048_576.0).max(0.001);

        let speedup_ratio = if meta.latency_samples > 0 && meta.shadow_latency_ns > 0 {
            let avg_baseline = meta.baseline_latency_ns as f64 / meta.latency_samples as f64;
            let avg_shadow = meta.shadow_latency_ns as f64 / meta.latency_samples as f64;
            avg_baseline / avg_shadow
        } else {
            1.0
        };

        let total_q = meta.total_queries.max(1) as f64;
        let hit_ratio = meta.benefiting_queries as f64 / total_q;
        let score = speedup_ratio * hit_ratio / memory_mb;

        Self {
            memory_bytes,
            speedup_ratio,
            benefiting_queries: meta.benefiting_queries,
            total_queries: meta.total_queries,
            score,
        }
    }
}

// ── Decision record ─────────────────────────────────────────────────────

/// An action the autonomous manager decided to take.
#[derive(Debug, Clone)]
pub enum AutoDecision {
    CreateIndex {
        table: String,
        column: String,
        reason: String,
    },
    CreateCompositeIndex {
        table: String,
        columns: Vec<String>,
        reason: String,
    },
    DropIndex {
        name: String,
        reason: String,
    },
    PromoteShadow {
        name: String,
    },
    Noop,
}

// ── IndexManager ────────────────────────────────────────────────────────

/// Central index catalogue + autonomous controller.
pub struct IndexManager {
    /// Live B+Tree indexes keyed by name.
    pub indexes: RwLock<HashMap<String, Arc<BPlusTree>>>,
    /// Metadata for every known index.
    pub meta: RwLock<HashMap<String, IndexMeta>>,
    /// Per-(table, column) access statistics.
    pub stats: RwLock<HashMap<(String, String), ColumnStats>>,
    /// Shadow indexes being tested (before promotion).
    shadow: RwLock<HashMap<String, Arc<BPlusTree>>>,
    /// Whether the background builder is currently running.
    building: AtomicBool,
    /// Monotonic counter to derive unique hash suffixes.
    auto_counter: AtomicU64,
    /// Total memory budget for auto-managed indexes (bytes).
    memory_budget_bytes: u64,
}

#[derive(Clone)]
pub struct IndexManagerSnapshot {
    indexes: HashMap<String, Arc<BPlusTree>>,
    meta: HashMap<String, IndexMeta>,
    stats: HashMap<(String, String), ColumnStats>,
    shadow: HashMap<String, Arc<BPlusTree>>,
    building: bool,
    auto_counter: u64,
}

/// Default memory budget: 512 MB for auto-managed indexes.
const DEFAULT_MEMORY_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

/// Minimum idle time before GC considers an index for removal.
const GC_IDLE_SECS: u64 = 3600; // 1 hour
/// Negligible query threshold — indexes with fewer queries are GC candidates.
const GC_MIN_QUERIES: u64 = 10;

impl IndexManager {
    pub fn new() -> Self {
        Self {
            indexes: RwLock::new(HashMap::new()),
            meta: RwLock::new(HashMap::new()),
            stats: RwLock::new(HashMap::new()),
            shadow: RwLock::new(HashMap::new()),
            building: AtomicBool::new(false),
            auto_counter: AtomicU64::new(0),
            memory_budget_bytes: DEFAULT_MEMORY_BUDGET_BYTES,
        }
    }

    pub fn snapshot(&self) -> IndexManagerSnapshot {
        let indexes = self
            .indexes
            .read()
            .iter()
            .map(|(name, tree)| (name.clone(), Arc::new(tree.deep_clone())))
            .collect();
        let shadow = self
            .shadow
            .read()
            .iter()
            .map(|(name, tree)| (name.clone(), Arc::new(tree.deep_clone())))
            .collect();

        IndexManagerSnapshot {
            indexes,
            meta: self.meta.read().clone(),
            stats: self.stats.read().clone(),
            shadow,
            building: self.building.load(Ordering::Acquire),
            auto_counter: self.auto_counter.load(Ordering::Acquire),
        }
    }

    pub fn restore_snapshot(&self, snapshot: IndexManagerSnapshot) {
        *self.indexes.write() = snapshot.indexes;
        *self.meta.write() = snapshot.meta;
        *self.stats.write() = snapshot.stats;
        *self.shadow.write() = snapshot.shadow;
        self.building.store(snapshot.building, Ordering::Release);
        self.auto_counter
            .store(snapshot.auto_counter, Ordering::Release);
    }

    // ── Query Observer ──────────────────────────────────────────────────

    /// Record that column `col` of `table` was referenced in a query.
    pub fn record_query_hit(&self, table: &str, col: &str) {
        let key = (table.to_string(), col.to_string());
        let mut stats = self.stats.write();
        let entry = stats.entry(key).or_default();
        entry.query_hits += 1;
        entry.last_query_at = Instant::now();
    }

    /// Record a write (INSERT/UPDATE/DELETE) to `table`.
    pub fn record_write(&self, table: &str, col: &str) {
        let key = (table.to_string(), col.to_string());
        let mut stats = self.stats.write();
        let entry = stats.entry(key).or_default();
        entry.write_ops += 1;
        entry.last_write_at = Instant::now();
    }

    /// Record writes for several columns under one stats lock.
    pub fn record_writes<'a, I>(&self, table: &str, cols: I)
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut stats = self.stats.write();
        let now = Instant::now();
        for col in cols {
            let key = (table.to_string(), col.to_string());
            let entry = stats.entry(key).or_default();
            entry.write_ops += 1;
            entry.last_write_at = now;
        }
    }

    /// Record a numeric sample value into histogram.
    pub fn record_numeric_value(&self, table: &str, col: &str, value: f64) {
        let key = (table.to_string(), col.to_string());
        let mut stats = self.stats.write();
        let entry = stats.entry(key).or_default();
        entry.histogram.record(value);
    }

    /// Record numeric histogram samples under one stats lock.
    pub fn record_numeric_values<'a, I>(&self, table: &str, samples: I)
    where
        I: IntoIterator<Item = (&'a str, f64)>,
    {
        let mut stats = self.stats.write();
        for (col, value) in samples {
            let key = (table.to_string(), col.to_string());
            let entry = stats.entry(key).or_default();
            entry.histogram.record(value);
        }
    }

    /// Update selectivity estimate for a column.
    pub fn update_selectivity(&self, table: &str, col: &str, distinct: u64, total: u64) {
        let key = (table.to_string(), col.to_string());
        let mut stats = self.stats.write();
        let entry = stats.entry(key).or_default();
        entry.distinct_count = distinct;
        entry.table_rows = total;
        entry.selectivity = if total > 0 {
            distinct as f64 / total as f64
        } else {
            1.0
        };
    }

    /// Update observed predicate selectivity from an actual query result.
    ///
    /// This records "matched rows / table rows", which is the value the
    /// autonomous indexer expects when deciding whether a predicate is selective
    /// enough to deserve an index.
    pub fn update_observed_selectivity(&self, table: &str, col: &str, matched: u64, total: u64) {
        if total == 0 {
            return;
        }
        let key = (table.to_string(), col.to_string());
        let mut stats = self.stats.write();
        let entry = stats.entry(key).or_default();
        entry.table_rows = total;
        entry.selectivity = (matched as f64 / total as f64).clamp(0.0, 1.0);
    }

    /// Estimate BETWEEN selectivity from histogram and persist it into stats.
    pub fn update_selectivity_from_histogram_between(
        &self,
        table: &str,
        col: &str,
        lo: f64,
        hi: f64,
    ) -> Option<f64> {
        let key = (table.to_string(), col.to_string());
        let mut stats = self.stats.write();
        let entry = stats.entry(key).or_default();
        let est = entry.histogram.estimate_selectivity_between(lo, hi)?;
        entry.selectivity = est;
        Some(est)
    }

    /// Get histogram snapshot for diagnostics.
    pub fn histogram_snapshot(&self, table: &str, col: &str) -> Option<NumericHistogram> {
        self.stats
            .read()
            .get(&(table.to_string(), col.to_string()))
            .map(|s| s.histogram.clone())
    }

    /// Record that an index was consulted during query execution.
    pub fn record_index_use(&self, index_name: &str) {
        let mut meta = self.meta.write();
        if let Some(m) = meta.get_mut(index_name) {
            m.use_count += 1;
            m.last_used_at = Instant::now();
        }
    }

    /// Record latency samples for a shadow index vs baseline.
    /// Called by the query executor after running a query with and without the shadow index.
    pub fn record_shadow_latency(&self, index_name: &str, shadow_ns: u64, baseline_ns: u64) {
        let mut meta = self.meta.write();
        if let Some(m) = meta.get_mut(index_name) {
            m.shadow_latency_ns += shadow_ns;
            m.baseline_latency_ns += baseline_ns;
            m.latency_samples += 1;
        }
    }

    // ── Manual index operations ─────────────────────────────────────────

    /// Manually create an index (from `CREATE INDEX` SQL).
    pub fn create_manual_index(
        &self,
        name: &str,
        table: &str,
        columns: &[String],
    ) -> Arc<BPlusTree> {
        let tree = Arc::new(BPlusTree::new(
            name.to_string(),
            table.to_string(),
            columns.to_vec(),
        ));
        let now = Instant::now();
        let m = IndexMeta {
            name: name.to_string(),
            table: table.to_string(),
            columns: columns.to_vec(),
            state: IndexState::Manual,
            created_at: now,
            last_used_at: now,
            use_count: 0,
            manual: true,
            shadow_latency_ns: 0,
            baseline_latency_ns: 0,
            latency_samples: 0,
            estimated_memory_bytes: 0,
            benefiting_queries: 0,
            total_queries: 0,
        };
        self.indexes.write().insert(name.to_string(), tree.clone());
        self.meta.write().insert(name.to_string(), m);
        tree
    }

    /// Drop an index by name. Returns true if it existed.
    pub fn drop_index(&self, name: &str) -> bool {
        let existed = self.indexes.write().remove(name).is_some();
        self.meta.write().remove(name);
        self.shadow.write().remove(name);
        existed
    }

    // ── Lookup ──────────────────────────────────────────────────────────

    /// Find a live index for a given table+column combination.
    pub fn find_index(&self, table: &str, column: &str) -> Option<Arc<BPlusTree>> {
        let indexes = self.indexes.read();
        let meta = self.meta.read();
        for (name, tree) in indexes.iter() {
            if tree.table == table && tree.columns.contains(&column.to_string()) {
                if let Some(m) = meta.get(name) {
                    if m.state == IndexState::Active || m.state == IndexState::Manual {
                        return Some(tree.clone());
                    }
                }
            }
        }
        None
    }

    /// List all index names.
    pub fn list_indexes(&self) -> Vec<IndexMeta> {
        self.meta.read().values().cloned().collect()
    }

    // ── Autonomous decision cycle ───────────────────────────────────────

    /// Run one decision cycle: evaluate all stats and return actions.
    pub fn evaluate(&self) -> Vec<AutoDecision> {
        let mut decisions = Vec::new();

        // 1. Check for columns that deserve a new index.
        //    Phase 9: Also check memory budget before creating.
        {
            let stats = self.stats.read();
            let meta = self.meta.read();
            let remaining_budget = self.remaining_memory_bytes();
            for ((table, col), cs) in stats.iter() {
                if cs.query_hits >= AUTO_CREATE_THRESHOLD
                    && cs.selectivity <= MAX_SELECTIVITY_FOR_AUTO
                {
                    // Already have an index?
                    let already = meta.values().any(|m| {
                        m.table == *table
                            && m.columns.contains(col)
                            && m.state != IndexState::PendingDrop
                    });
                    if !already {
                        // Estimate memory: ~64 bytes per row × table_rows (conservative)
                        let est_memory = cs.table_rows * 64;
                        if est_memory > remaining_budget {
                            continue; // Skip — would exceed memory budget
                        }
                        decisions.push(AutoDecision::CreateIndex {
                            table: table.clone(),
                            column: col.clone(),
                            reason: format!(
                                "query_hits={}, selectivity={:.3}, hist_total={}, est_mem={}",
                                cs.query_hits, cs.selectivity, cs.histogram.total, est_memory
                            ),
                        });
                    }
                }
            }
        }

        // 2. Check for indexes that should be dropped.
        {
            let meta = self.meta.read();
            for (name, m) in meta.iter() {
                if m.manual {
                    continue; // never auto-drop user indexes
                }
                let idle = m.last_used_at.elapsed().as_secs();
                if idle >= UNUSED_TTL_SECS && m.state == IndexState::Active {
                    decisions.push(AutoDecision::DropIndex {
                        name: name.clone(),
                        reason: format!("unused for {} s", idle),
                    });
                }
            }
        }

        // 3. Promote shadow indexes that proved beneficial.
        //    FIX Bug 1.2: Actually compare measured latencies against
        //    SHADOW_SPEEDUP_THRESHOLD before promoting.
        //    Phase 9: Also apply cost-benefit analysis and memory budget check.
        {
            let shadow = self.shadow.read();
            let meta = self.meta.read();
            let remaining_budget = self.remaining_memory_bytes();
            for (name, _tree) in shadow.iter() {
                if let Some(m) = meta.get(name) {
                    let cb = IndexCostBenefit::compute(m);

                    // Gate 1: latency samples are preferred. During bootstrap,
                    // allow promotion from repeated beneficial shadow hits even
                    // when a full baseline replay was intentionally skipped.
                    if m.latency_samples >= 20 {
                        if cb.speedup_ratio < SHADOW_SPEEDUP_THRESHOLD {
                            if m.created_at.elapsed().as_secs() > UNUSED_TTL_SECS {
                                decisions.push(AutoDecision::DropIndex {
                                    name: name.clone(),
                                    reason: format!(
                                        "shadow speedup {:.2}x < threshold {:.2}x after {} samples",
                                        cb.speedup_ratio,
                                        SHADOW_SPEEDUP_THRESHOLD,
                                        m.latency_samples
                                    ),
                                });
                            }
                            continue;
                        }
                    } else if m.total_queries < 20 {
                        continue;
                    }

                    // Gate 2: hit ratio must be non-negligible (>10%).
                    let hit_ratio = m.benefiting_queries as f64 / m.total_queries.max(1) as f64;
                    if hit_ratio < 0.10 {
                        continue; // Too few queries benefit
                    }

                    // Gate 3: memory budget check
                    if m.estimated_memory_bytes > 0 && m.estimated_memory_bytes > remaining_budget {
                        decisions.push(AutoDecision::DropIndex {
                            name: name.clone(),
                            reason: format!(
                                "exceeds memory budget ({} B needed, {} B remaining)",
                                m.estimated_memory_bytes, remaining_budget
                            ),
                        });
                        continue;
                    }

                    decisions.push(AutoDecision::PromoteShadow { name: name.clone() });
                }
            }
        }

        // 4. Phase 9: Detect multi-column composite index opportunities.
        //    If two columns of the same table both have high query_hits and
        //    frequently co-occur in queries, suggest a composite index.
        {
            let stats = self.stats.read();
            let meta = self.meta.read();
            let mut table_cols: HashMap<String, Vec<(String, u64)>> = HashMap::new();
            for ((table, col), cs) in stats.iter() {
                if cs.query_hits >= AUTO_CREATE_THRESHOLD / 2 {
                    table_cols
                        .entry(table.clone())
                        .or_default()
                        .push((col.clone(), cs.query_hits));
                }
            }
            for (table, mut cols) in table_cols {
                if cols.len() < 2 {
                    continue;
                }
                // Sort by query_hits desc — most queried first (leading column)
                cols.sort_by(|a, b| b.1.cmp(&a.1));
                // Check if a composite covering these top-2 already exists
                let c0 = &cols[0].0;
                let c1 = &cols[1].0;
                let already = meta.values().any(|m| {
                    m.table == table
                        && m.columns.len() >= 2
                        && m.columns.contains(c0)
                        && m.columns.contains(c1)
                        && m.state != IndexState::PendingDrop
                });
                if !already {
                    let combined_hits = cols[0].1 + cols[1].1;
                    if combined_hits >= AUTO_CREATE_THRESHOLD * 2 {
                        decisions.push(AutoDecision::CreateCompositeIndex {
                            table: table.clone(),
                            columns: vec![c0.clone(), c1.clone()],
                            reason: format!(
                                "co-queried columns: {}({}), {}({})",
                                c0, cols[0].1, c1, cols[1].1
                            ),
                        });
                    }
                }
            }
        }

        if decisions.is_empty() {
            decisions.push(AutoDecision::Noop);
        }
        decisions
    }

    /// Apply a set of decisions. Called after `evaluate()`.
    pub fn apply_decisions(&self, decisions: &[AutoDecision]) {
        for d in decisions {
            match d {
                AutoDecision::CreateIndex { table, column, .. } => {
                    let idx_name = self.generate_auto_name(table, column);
                    self.begin_shadow_build(&idx_name, table, &[column.clone()]);
                }
                AutoDecision::CreateCompositeIndex { table, columns, .. } => {
                    let combined = columns.join("_");
                    let idx_name = self.generate_auto_name(table, &combined);
                    self.begin_shadow_build(&idx_name, table, columns);
                }
                AutoDecision::DropIndex { name, .. } => {
                    self.drop_index(name);
                }
                AutoDecision::PromoteShadow { name } => {
                    self.promote_shadow(name);
                }
                AutoDecision::Noop => {}
            }
        }
    }

    /// Generate a unique auto-index name: `idx_auto_<table>_<column>_<hash>`.
    pub fn generate_auto_name(&self, table: &str, column: &str) -> String {
        let seq = self.auto_counter.fetch_add(1, Ordering::Relaxed);
        format!("idx_auto_{}_{}_{:04x}", table, column, seq)
    }

    // ── Shadow build pipeline ───────────────────────────────────────────

    /// Start building an index in shadow mode.
    fn begin_shadow_build(&self, name: &str, table: &str, columns: &[String]) {
        let tree = Arc::new(BPlusTree::new(
            name.to_string(),
            table.to_string(),
            columns.to_vec(),
        ));
        let now = Instant::now();
        let m = IndexMeta {
            name: name.to_string(),
            table: table.to_string(),
            columns: columns.to_vec(),
            state: IndexState::Shadow,
            created_at: now,
            last_used_at: now,
            use_count: 0,
            manual: false,
            shadow_latency_ns: 0,
            baseline_latency_ns: 0,
            latency_samples: 0,
            estimated_memory_bytes: 0,
            benefiting_queries: 0,
            total_queries: 0,
        };
        self.shadow.write().insert(name.to_string(), tree);
        self.meta.write().insert(name.to_string(), m);
    }

    /// Populate a shadow index from existing table data.
    /// Called from a background thread with entries extracted from the table.
    pub fn populate_shadow(&self, name: &str, entries: Vec<(IndexKey, RowId)>) {
        self.building.store(true, Ordering::Release);
        if let Some(tree) = self.shadow.read().get(name) {
            tree.bulk_load(entries);
        }
        self.building.store(false, Ordering::Release);
    }

    /// Return empty shadow indexes that need table backfill.
    pub fn shadow_indexes_needing_population(&self) -> Vec<(String, String, Vec<String>)> {
        let shadow = self.shadow.read();
        let meta = self.meta.read();
        shadow
            .iter()
            .filter_map(|(name, tree)| {
                if tree.entry_count() > 0 {
                    return None;
                }
                let m = meta.get(name)?;
                if m.state != IndexState::Shadow {
                    return None;
                }
                Some((name.clone(), m.table.clone(), m.columns.clone()))
            })
            .collect()
    }

    /// Promote a shadow index to the active set.
    fn promote_shadow(&self, name: &str) {
        if let Some(tree) = self.shadow.write().remove(name) {
            self.indexes.write().insert(name.to_string(), tree);
            if let Some(m) = self.meta.write().get_mut(name) {
                m.state = IndexState::Active;
            }
        }
    }

    /// Return true if an index build is in progress.
    pub fn is_building(&self) -> bool {
        self.building.load(Ordering::Acquire)
    }

    /// Find a shadow index for planner-side trial execution.
    pub fn find_shadow_index(&self, table: &str, column: &str) -> Option<Arc<BPlusTree>> {
        let shadow = self.shadow.read();
        let meta = self.meta.read();
        for (name, tree) in shadow.iter() {
            let Some(m) = meta.get(name) else {
                continue;
            };
            if m.state == IndexState::Shadow
                && m.table == table
                && m.columns.contains(&column.to_string())
            {
                return Some(tree.clone());
            }
        }
        None
    }

    /// Return active/manual and shadow trees for DML maintenance.
    pub fn trees_for_table_including_shadow(&self, table: &str) -> Vec<Arc<BPlusTree>> {
        let mut out = Vec::new();
        {
            let indexes = self.indexes.read();
            out.extend(indexes.values().filter(|tree| tree.table == table).cloned());
        }
        {
            let shadow = self.shadow.read();
            out.extend(shadow.values().filter(|tree| tree.table == table).cloned());
        }
        out
    }

    // ── Garbage Collection & Memory ─────────────────────────────────────

    /// Total memory used by all auto-managed indexes.
    pub fn total_memory_bytes(&self) -> u64 {
        self.meta
            .read()
            .values()
            .filter(|m| !m.manual)
            .map(|m| m.estimated_memory_bytes)
            .sum()
    }

    /// Remaining memory budget for new auto-managed indexes.
    pub fn remaining_memory_bytes(&self) -> u64 {
        self.memory_budget_bytes
            .saturating_sub(self.total_memory_bytes())
    }

    /// Garbage-collect idle, negligible-use indexes.
    /// Drops auto-managed indexes that have been idle longer than `GC_IDLE_SECS`
    /// and used fewer than `GC_MIN_QUERIES` times.
    pub fn gc_unused_indexes(&self) -> Vec<String> {
        let to_remove: Vec<String> = {
            let meta = self.meta.read();
            meta.values()
                .filter(|m| {
                    !m.manual
                        && m.state == IndexState::Active
                        && m.last_used_at.elapsed().as_secs() > GC_IDLE_SECS
                        && m.use_count < GC_MIN_QUERIES
                })
                .map(|m| m.name.clone())
                .collect()
        };

        for name in &to_remove {
            self.drop_index(name);
        }

        to_remove
    }

    /// Update the estimated memory footprint of an index (after build/populate).
    pub fn update_memory_estimate(&self, name: &str, bytes: u64) {
        if let Some(m) = self.meta.write().get_mut(name) {
            m.estimated_memory_bytes = bytes;
        }
    }

    /// Record a shadow query observation (whether the index helped or not).
    pub fn record_shadow_observation(&self, name: &str, benefited: bool) {
        if let Some(m) = self.meta.write().get_mut(name) {
            m.total_queries += 1;
            if benefited {
                m.benefiting_queries += 1;
            }
        }
    }

    // ── Persistence helpers ─────────────────────────────────────────────

    /// Encode all active + manual indexes for WAL/checkpoint.
    pub fn encode_catalog(&self) -> Vec<u8> {
        let indexes = self.indexes.read();
        let meta = self.meta.read();
        let mut out = Vec::new();

        // Simple catalog format: [count:4][ [name_len:2][name][table_len:2][table]
        //   [col_count:2][ [col_len:2][col] ... ][state:1][tree_data] ]...
        let count = indexes.len() as u32;
        out.extend_from_slice(&count.to_le_bytes());

        for (name, tree) in indexes.iter() {
            // Name
            let nb = name.as_bytes();
            out.extend_from_slice(&(nb.len() as u16).to_le_bytes());
            out.extend_from_slice(nb);
            // Table
            let tb = tree.table.as_bytes();
            out.extend_from_slice(&(tb.len() as u16).to_le_bytes());
            out.extend_from_slice(tb);
            // Columns
            out.extend_from_slice(&(tree.columns.len() as u16).to_le_bytes());
            for c in &tree.columns {
                let cb = c.as_bytes();
                out.extend_from_slice(&(cb.len() as u16).to_le_bytes());
                out.extend_from_slice(cb);
            }
            // State
            let state = meta
                .get(name)
                .map(|m| m.state)
                .unwrap_or(IndexState::Active);
            out.push(match state {
                IndexState::Building => 0,
                IndexState::Shadow => 1,
                IndexState::Active => 2,
                IndexState::PendingDrop => 3,
                IndexState::Manual => 4,
            });
            // Tree data
            let tree_data = tree.encode_all();
            out.extend_from_slice(&(tree_data.len() as u32).to_le_bytes());
            out.extend_from_slice(&tree_data);
        }
        out
    }

    pub fn load_catalog(&self, data: &[u8]) -> Result<(), String> {
        fn read_u16(data: &[u8], offset: &mut usize) -> Result<u16, String> {
            if *offset + 2 > data.len() {
                return Err("index catalog truncated while reading u16".to_string());
            }
            let v = u16::from_le_bytes(
                data[*offset..*offset + 2]
                    .try_into()
                    .map_err(|_| "invalid u16 in index catalog")?,
            );
            *offset += 2;
            Ok(v)
        }

        fn read_u32(data: &[u8], offset: &mut usize) -> Result<u32, String> {
            if *offset + 4 > data.len() {
                return Err("index catalog truncated while reading u32".to_string());
            }
            let v = u32::from_le_bytes(
                data[*offset..*offset + 4]
                    .try_into()
                    .map_err(|_| "invalid u32 in index catalog")?,
            );
            *offset += 4;
            Ok(v)
        }

        fn read_string(data: &[u8], offset: &mut usize) -> Result<String, String> {
            let len = read_u16(data, offset)? as usize;
            if *offset + len > data.len() {
                return Err("index catalog truncated while reading string".to_string());
            }
            let s = String::from_utf8(data[*offset..*offset + len].to_vec())
                .map_err(|_| "index catalog contains invalid utf8".to_string())?;
            *offset += len;
            Ok(s)
        }

        if data.len() < 4 {
            return Err("index catalog truncated".to_string());
        }
        let mut offset = 0usize;
        let count = read_u32(data, &mut offset)? as usize;
        let mut indexes = HashMap::new();
        let mut meta = HashMap::new();
        let now = Instant::now();

        for _ in 0..count {
            let name = read_string(data, &mut offset)?;
            let table = read_string(data, &mut offset)?;
            let col_count = read_u16(data, &mut offset)? as usize;
            let mut columns = Vec::with_capacity(col_count);
            for _ in 0..col_count {
                columns.push(read_string(data, &mut offset)?);
            }
            if offset >= data.len() {
                return Err("index catalog truncated while reading state".to_string());
            }
            let state = match data[offset] {
                0 => IndexState::Building,
                1 => IndexState::Shadow,
                2 => IndexState::Active,
                3 => IndexState::PendingDrop,
                4 => IndexState::Manual,
                _ => return Err("index catalog contains invalid state".to_string()),
            };
            offset += 1;
            let tree_len = read_u32(data, &mut offset)? as usize;
            if offset + tree_len > data.len() {
                return Err("index catalog truncated while reading tree".to_string());
            }
            let tree = BPlusTree::decode_all(
                name.clone(),
                table.clone(),
                columns.clone(),
                &data[offset..offset + tree_len],
            )
            .ok_or_else(|| format!("index catalog tree '{}' is corrupt", name))?;
            offset += tree_len;

            let manual = state == IndexState::Manual;
            indexes.insert(name.clone(), Arc::new(tree));
            meta.insert(
                name.clone(),
                IndexMeta {
                    name,
                    table,
                    columns,
                    state,
                    created_at: now,
                    last_used_at: now,
                    use_count: 0,
                    manual,
                    shadow_latency_ns: 0,
                    baseline_latency_ns: 0,
                    latency_samples: 0,
                    estimated_memory_bytes: 0,
                    benefiting_queries: 0,
                    total_queries: 0,
                },
            );
        }

        *self.indexes.write() = indexes;
        *self.meta.write() = meta;
        self.shadow.write().clear();
        Ok(())
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_records_and_estimates_range() {
        let mut h = NumericHistogram::default();
        for i in 0..100 {
            h.record(i as f64);
        }
        let sel = h.estimate_selectivity_between(20.0, 39.0).unwrap_or(0.0);
        assert!(sel > 0.05);
        assert!(sel < 0.5);
    }

    #[test]
    fn manager_updates_selectivity_from_histogram() {
        let mgr = IndexManager::new();
        for i in 0..1000 {
            mgr.record_numeric_value("orders", "amount", i as f64);
        }
        let sel = mgr
            .update_selectivity_from_histogram_between("orders", "amount", 100.0, 200.0)
            .unwrap_or(0.0);
        assert!(sel > 0.02);
        assert!(sel < 0.3);
    }

    #[test]
    fn manual_index_lifecycle() {
        let mgr = IndexManager::new();
        let tree = mgr.create_manual_index("idx_users_age", "users", &["age".into()]);
        tree.insert(IndexKey::Integer(25), 1);
        tree.insert(IndexKey::Integer(30), 2);
        tree.insert(IndexKey::Integer(25), 3);

        let found = mgr.find_index("users", "age");
        assert!(found.is_some());
        let t = found.unwrap();
        assert_eq!(t.search(&IndexKey::Integer(25)).len(), 2);

        assert!(mgr.drop_index("idx_users_age"));
        assert!(mgr.find_index("users", "age").is_none());
    }

    #[test]
    fn auto_create_decision() {
        let mgr = IndexManager::new();

        // Simulate 60 query hits with good selectivity
        for _ in 0..60 {
            mgr.record_query_hit("orders", "account_id");
        }
        mgr.update_selectivity("orders", "account_id", 100, 10_000);

        let decisions = mgr.evaluate();
        let creates: Vec<_> = decisions
            .iter()
            .filter(|d| matches!(d, AutoDecision::CreateIndex { .. }))
            .collect();
        assert_eq!(creates.len(), 1);
        if let AutoDecision::CreateIndex { table, column, .. } = &creates[0] {
            assert_eq!(table, "orders");
            assert_eq!(column, "account_id");
        }
    }

    #[test]
    fn shadow_build_and_promote() {
        let mgr = IndexManager::new();
        let idx_name = mgr.generate_auto_name("orders", "account_id");
        mgr.begin_shadow_build(&idx_name, "orders", &["account_id".into()]);

        // Populate
        let entries: Vec<_> = (0..100).map(|i| (IndexKey::Integer(i % 10), i)).collect();
        mgr.populate_shadow(&idx_name, entries);

        // Not yet in live set
        assert!(mgr.find_index("orders", "account_id").is_none());

        // Promote
        mgr.promote_shadow(&idx_name);
        let live = mgr.find_index("orders", "account_id");
        assert!(live.is_some());
        assert_eq!(live.unwrap().entry_count(), 100);
    }

    #[test]
    fn never_auto_drop_manual() {
        let mgr = IndexManager::new();
        mgr.create_manual_index("idx_manual", "t1", &["c1".into()]);

        let decisions = mgr.evaluate();
        let drops: Vec<_> = decisions
            .iter()
            .filter(|d| matches!(d, AutoDecision::DropIndex { .. }))
            .collect();
        assert!(drops.is_empty());
    }

    #[test]
    fn auto_name_format() {
        let mgr = IndexManager::new();
        let n1 = mgr.generate_auto_name("bench_orders", "account_id");
        let n2 = mgr.generate_auto_name("bench_orders", "account_id");
        assert_ne!(n1, n2);
        assert!(n1.starts_with("idx_auto_bench_orders_account_id_"));
    }

    #[test]
    fn record_index_use_updates_meta() {
        let mgr = IndexManager::new();
        mgr.create_manual_index("idx1", "t", &["c".into()]);
        mgr.record_index_use("idx1");
        mgr.record_index_use("idx1");
        let meta = mgr.meta.read();
        assert_eq!(meta.get("idx1").unwrap().use_count, 2);
    }

    #[test]
    fn composite_index_detection() {
        let mgr = IndexManager::new();

        // Simulate heavy access on two columns of the same table
        for _ in 0..60 {
            mgr.record_query_hit("orders", "account_id");
            mgr.record_query_hit("orders", "product_id");
        }
        mgr.update_selectivity("orders", "account_id", 100, 10_000);
        mgr.update_selectivity("orders", "product_id", 50, 10_000);

        let decisions = mgr.evaluate();
        let composites: Vec<_> = decisions
            .iter()
            .filter(|d| matches!(d, AutoDecision::CreateCompositeIndex { .. }))
            .collect();
        assert_eq!(composites.len(), 1);
        if let AutoDecision::CreateCompositeIndex { table, columns, .. } = &composites[0] {
            assert_eq!(table, "orders");
            assert_eq!(columns.len(), 2);
            assert!(columns.contains(&"account_id".to_string()));
            assert!(columns.contains(&"product_id".to_string()));
        }
    }

    #[test]
    fn gc_removes_idle_low_use_indexes() {
        let mgr = IndexManager::new();
        // Create a shadow and promote it
        mgr.begin_shadow_build("idx_test_gc", "t", &["c".into()]);
        mgr.promote_shadow("idx_test_gc");
        // It's freshly promoted so GC shouldn't touch it
        let removed = mgr.gc_unused_indexes();
        assert!(removed.is_empty());
    }

    #[test]
    fn cost_benefit_analysis() {
        let now = Instant::now();
        let meta = IndexMeta {
            name: "idx_test".into(),
            table: "t".into(),
            columns: vec!["c".into()],
            state: IndexState::Shadow,
            created_at: now,
            last_used_at: now,
            use_count: 50,
            manual: false,
            shadow_latency_ns: 5_000,
            baseline_latency_ns: 10_000,
            latency_samples: 100,
            estimated_memory_bytes: 1_048_576,
            benefiting_queries: 80,
            total_queries: 100,
        };
        let cb = IndexCostBenefit::compute(&meta);
        // speedup = 10000/5000 = 2.0
        assert!((cb.speedup_ratio - 2.0).abs() < 0.01);
        // hit_ratio = 80/100 = 0.8
        // score = 2.0 * 0.8 / 1.0 = 1.6
        assert!(cb.score > 1.0);
    }
}
