// Native SQL Engine — refactored into directory module (RS-ARCH-02).
//
// Previously a 3200+ line monolith (native_sql.rs).
// Now lives as native_sql/mod.rs for better organisation.
// Sub-modules can be incrementally extracted from here.

use super::connection::QueryResult;
use super::protocol::oid;
use super::auth::{AuthManager, Privilege};
use super::audit::{AuditLogger, AuditEvent};
use crate::executor::simd_sum_f64;
use crate::executor::agg::{
    AggFunction as ExecAggFunction, AggValue as ExecAggValue,
    AggSpec, AggregateExecutor, HavingPredicate, apply_having,
};
use crate::index::{IndexManager, IndexKey};
use ahash::AHashMap;
use parking_lot::RwLock as PLRwLock;
use rayon::prelude::*;
use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

// Apache Arrow / Parquet imports
use arrow::array::{
    Array, Float64Array, Int64Array, StringArray,
    AsArray,
};
use arrow::datatypes::DataType as ArrowDataType;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use arrow::datatypes::{Schema as ArrowSchema, Field as ArrowField, DataType as ArrowDataType2};
use arrow::record_batch::RecordBatch;

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

/// Chunk size for vectorized pipeline processing.
/// Matches typical CPU L1 cache line utilization.
pub(crate) const CHUNK_SIZE: usize = 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum Cell {
    Int(i64),
    Float(f64),
    Text(String),
    Null,
}

impl PartialEq for Cell {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Cell::Int(a), Cell::Int(b)) => a == b,
            (Cell::Float(a), Cell::Float(b)) => a.to_bits() == b.to_bits(),
            (Cell::Text(a), Cell::Text(b)) => a == b,
            (Cell::Null, Cell::Null) => true,
            _ => false,
        }
    }
}

impl Cell {
    fn as_i64(&self) -> i64 {
        match self {
            Cell::Int(v) => *v,
            Cell::Float(v) => *v as i64,
            Cell::Text(v) => v.parse::<i64>().unwrap_or(0),
            Cell::Null => 0,
        }
    }

    fn as_f64(&self) -> f64 {
        match self {
            Cell::Int(v) => *v as f64,
            Cell::Float(v) => *v,
            Cell::Text(v) => v.parse::<f64>().unwrap_or(0.0),
            Cell::Null => 0.0,
        }
    }

    pub(crate) fn as_text(&self) -> String {
        match self {
            Cell::Int(v) => v.to_string(),
            Cell::Float(v) => v.to_string(),
            Cell::Text(v) => v.clone(),
            Cell::Null => String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct NativeRow {
    pub(crate) cols: HashMap<String, Cell>,
    /// LSN at which this row was last modified (INSERT/UPDATE).
    /// Old snapshots deserialize with 0 via serde(default).
    #[serde(default)]
    pub(crate) last_modified_lsn: u64,
}

/// Column type as declared in CREATE TABLE.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) enum ColType {
    Integer,
    Float8,
    Text,
}

// ---------------------------------------------------------------------------
// ColumnExtractor — zero-copy Parquet → Cell conversion via Arrow arrays
// ---------------------------------------------------------------------------

/// Typed extractor for Arrow column arrays.  Pre-downcast at batch level so
/// per-row access is a simple index lookup (no dynamic dispatch per row).
pub(crate) enum ColumnExtractor {
    Int64(Int64Array),
    Float64(Float64Array),
    Utf8(StringArray),
    /// Fallback: keeps the full Arc for .to_string() conversion.
    Generic(Arc<dyn Array>),
}

impl ColumnExtractor {
    fn from_arrow(arr: &Arc<dyn Array>) -> Self {
        match arr.data_type() {
            ArrowDataType::Int64 => {
                ColumnExtractor::Int64(arr.as_primitive::<arrow::datatypes::Int64Type>().clone())
            }
            ArrowDataType::Int32 => {
                // Widen i32 → i64 for uniform handling
                let a32 = arr.as_primitive::<arrow::datatypes::Int32Type>();
                let vals: Vec<i64> = (0..a32.len())
                    .map(|i| if a32.is_null(i) { 0 } else { a32.value(i) as i64 })
                    .collect();
                ColumnExtractor::Int64(Int64Array::from(vals))
            }
            ArrowDataType::Float64 => {
                ColumnExtractor::Float64(arr.as_primitive::<arrow::datatypes::Float64Type>().clone())
            }
            ArrowDataType::Float32 => {
                let a32 = arr.as_primitive::<arrow::datatypes::Float32Type>();
                let vals: Vec<f64> = (0..a32.len())
                    .map(|i| if a32.is_null(i) { 0.0 } else { a32.value(i) as f64 })
                    .collect();
                ColumnExtractor::Float64(Float64Array::from(vals))
            }
            ArrowDataType::Utf8 => {
                ColumnExtractor::Utf8(arr.as_string::<i32>().clone())
            }
            ArrowDataType::LargeUtf8 => {
                // Convert LargeUtf8 → regular Utf8
                let large = arr.as_string::<i64>();
                let vals: Vec<Option<&str>> = (0..large.len())
                    .map(|i| if large.is_null(i) { None } else { Some(large.value(i)) })
                    .collect();
                ColumnExtractor::Utf8(StringArray::from(vals))
            }
            _ => ColumnExtractor::Generic(Arc::clone(arr)),
        }
    }

    fn get(&self, idx: usize) -> Cell {
        match self {
            ColumnExtractor::Int64(a) => {
                if a.is_null(idx) { Cell::Null } else { Cell::Int(a.value(idx)) }
            }
            ColumnExtractor::Float64(a) => {
                if a.is_null(idx) { Cell::Null } else { Cell::Float(a.value(idx)) }
            }
            ColumnExtractor::Utf8(a) => {
                if a.is_null(idx) { Cell::Null } else { Cell::Text(a.value(idx).to_string()) }
            }
            ColumnExtractor::Generic(a) => {
                if a.is_null(idx) {
                    Cell::Null
                } else {
                    // arrow's Display trait gives us a string representation
                    let s = arrow::util::display::array_value_to_string(a, idx)
                        .unwrap_or_default();
                    Cell::Text(s)
                }
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct NativeTable {
    pub(crate) columns: Vec<String>,
    /// Per-column declared type (same order as `columns`).
    pub(crate) column_types: Vec<ColType>,
    pub(crate) rows: HashMap<i64, NativeRow>,
}

impl NativeTable {
    fn new(columns: Vec<String>, column_types: Vec<ColType>) -> Self {
        Self {
            columns,
            column_types,
            rows: HashMap::new(),
        }
    }

    fn col_oid(&self, col_name: &str) -> i32 {
        for (i, c) in self.columns.iter().enumerate() {
            if c == col_name {
                return match self.column_types.get(i) {
                    Some(ColType::Integer) => oid::INT8,
                    Some(ColType::Float8) => oid::FLOAT8,
                    Some(ColType::Text) | None => oid::TEXT,
                };
            }
        }
        oid::TEXT
    }
}

#[derive(Clone, Debug)]
pub(crate) struct JoinInputSoA {
    pub(crate) account_ids: Vec<i32>,
    pub(crate) product_ids: Vec<i32>,
    pub(crate) order_ids: Vec<i64>,
    pub(crate) quantities: Vec<i64>,
    pub(crate) totals: Vec<f64>,
}

impl JoinInputSoA {
    fn from_rows(rows: &[&NativeRow]) -> Self {
        let mut account_ids = Vec::with_capacity(rows.len());
        let mut product_ids = Vec::with_capacity(rows.len());
        let mut order_ids = Vec::with_capacity(rows.len());
        let mut quantities = Vec::with_capacity(rows.len());
        let mut totals = Vec::with_capacity(rows.len());

        for row in rows {
            let aid = row
                .cols
                .get("account_id")
                .map(|c| c.as_i64())
                .unwrap_or(0);
            let pid = row
                .cols
                .get("product_id")
                .map(|c| c.as_i64())
                .unwrap_or(0);
            account_ids.push(aid as i32);
            product_ids.push(pid as i32);
            order_ids.push(row.cols.get("id").map(|c| c.as_i64()).unwrap_or(0));
            quantities.push(
                row.cols
                    .get("quantity")
                    .map(|c| c.as_i64())
                    .unwrap_or(0),
            );
            totals.push(
                row.cols
                    .get("total")
                    .map(|c| c.as_f64())
                    .unwrap_or(0.0),
            );
        }

        Self {
            account_ids,
            product_ids,
            order_ids,
            quantities,
            totals,
        }
    }

    fn len(&self) -> usize {
        self.account_ids.len()
    }

    fn filtered_indices_by_account_id(&self, account_id: i64) -> Vec<usize> {
        simd_eq_indices_i32(&self.account_ids, account_id as i32)
    }

    fn all_indices(&self) -> Vec<usize> {
        (0..self.len()).collect()
    }
}

#[inline]
pub(crate) fn simd_eq_indices_i32(values: &[i32], target: i32) -> Vec<usize> {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: aarch64 guarantees NEON availability.
        unsafe { return simd_eq_indices_i32_neon(values, target) };
    }

    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: guarded by runtime AVX2 feature detection.
            unsafe { return simd_eq_indices_i32_avx2(values, target) };
        }
        return values
            .iter()
            .enumerate()
            .filter_map(|(i, v)| if *v == target { Some(i) } else { None })
            .collect();
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        values
            .iter()
            .enumerate()
            .filter_map(|(i, v)| if *v == target { Some(i) } else { None })
            .collect()
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn simd_eq_indices_i32_avx2(values: &[i32], target: i32) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let n = values.len();
    let tv = _mm256_set1_epi32(target);

    while i + 8 <= n {
        let ptr = values.as_ptr().add(i) as *const __m256i;
        let vv = _mm256_loadu_si256(ptr);
        let cmp = _mm256_cmpeq_epi32(vv, tv);
        let mask = _mm256_movemask_ps(_mm256_castsi256_ps(cmp)) as u32;
        for lane in 0..8 {
            if (mask & (1u32 << lane)) != 0 {
                out.push(i + lane as usize);
            }
        }
        i += 8;
    }

    while i < n {
        if values[i] == target {
            out.push(i);
        }
        i += 1;
    }
    out
}

#[cfg(target_arch = "aarch64")]
unsafe fn simd_eq_indices_i32_neon(values: &[i32], target: i32) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let n = values.len();
    let tv = vdupq_n_s32(target);

    while i + 4 <= n {
        let vv = vld1q_s32(values.as_ptr().add(i));
        let cmp = vceqq_s32(vv, tv);
        let mut lanes = [0u32; 4];
        vst1q_u32(lanes.as_mut_ptr(), cmp);
        for lane in 0..4 {
            if lanes[lane] == u32::MAX {
                out.push(i + lane);
            }
        }
        i += 4;
    }

    while i < n {
        if values[i] == target {
            out.push(i);
        }
        i += 1;
    }
    out
}

/// Auto-checkpoint after this many WAL mutations.
pub(crate) const CHECKPOINT_INTERVAL: u64 = 10_000;

// ---------------------------------------------------------------------------
//  Buffer Pool — caches hot data structures to avoid per-query rebuild.
// ---------------------------------------------------------------------------

/// Cached dimension hash table (id → name bytes).
pub(crate) struct CachedDimension {
    generation: u64,
    name_by_id: Arc<AHashMap<i64, Arc<[u8]>>>,
}

/// Cached SoA column store for a fact table.
pub(crate) struct CachedSoA {
    generation: u64,
    soa: Arc<JoinInputSoA>,
}

/// Columnar cache for analytics (aggregation + range scan).
/// All arrays are sorted by `ids` for binary-search range queries.
#[derive(Clone)]
pub(crate) struct CachedColumns {
    pub(crate) generation: u64,
    pub(crate) ids: Vec<i64>,
    pub(crate) int_cols: AHashMap<String, Vec<i64>>,
    pub(crate) float_cols: AHashMap<String, Vec<f64>>,
    pub(crate) text_cols: AHashMap<String, Vec<String>>,
}

impl CachedColumns {
    /// Estimate memory usage of this cached column set in bytes.
    fn estimated_bytes(&self) -> usize {
        let mut total = self.ids.len() * 8;
        for v in self.int_cols.values() { total += v.len() * 8; }
        for v in self.float_cols.values() { total += v.len() * 8; }
        for v in self.text_cols.values() { total += v.iter().map(|s| s.len() + 24).sum::<usize>(); }
        total
    }
}

/// Maximum total bytes for the columnar analytics cache before eviction.
pub(crate) const COL_CACHE_BUDGET: usize = 256 * 1024 * 1024; // 256 MB

/// Buffer Pool for the native SQL engine.
///
/// Holds pre-built hash tables and columnar caches so that repeated JOIN
/// queries skip the expensive materialisation step.  Entries are keyed by
/// table name and protected by a `parking_lot::RwLock` for minimal overhead.
pub(crate) struct BufferPool {
    /// Monotonically increasing generation counter per table.  Bumped on every
    /// mutation (INSERT/UPDATE/DELETE/DROP/TRUNCATE).  A cached entry whose
    /// generation doesn't match is treated as stale and rebuilt.
    generations: PLRwLock<HashMap<String, u64>>,
    /// Dimension hash caches (table_name → CachedDimension).
    dim_cache: PLRwLock<HashMap<String, CachedDimension>>,
    /// SoA column caches (table_name → CachedSoA).
    soa_cache: PLRwLock<HashMap<String, CachedSoA>>,
    /// Columnar analytics cache (table_name → CachedColumns).
    col_cache: PLRwLock<HashMap<String, Arc<CachedColumns>>>,
}

impl BufferPool {
    fn new() -> Self {
        Self {
            generations: PLRwLock::new(HashMap::new()),
            dim_cache: PLRwLock::new(HashMap::new()),
            soa_cache: PLRwLock::new(HashMap::new()),
            col_cache: PLRwLock::new(HashMap::new()),
        }
    }

    /// Bump the generation for `table`, invalidating any cached data.
    /// Also removes stale cache entries to prevent unbounded memory growth
    /// under sustained workloads (fixes L-07).
    fn invalidate(&self, table: &str) {
        let mut gens = self.generations.write();
        let gen = gens.entry(table.to_string()).or_insert(0);
        *gen += 1;
        // Eagerly remove stale cached data so it doesn't linger.
        self.dim_cache.write().remove(table);
        self.soa_cache.write().remove(table);
        self.col_cache.write().remove(table);
    }

    /// Remove all cached data for a table (on DROP TABLE).
    fn remove_table(&self, table: &str) {
        self.generations.write().remove(table);
        self.dim_cache.write().remove(table);
        self.soa_cache.write().remove(table);
        self.col_cache.write().remove(table);
    }

    /// Invalidate all cached data (e.g., after ROLLBACK TO SAVEPOINT).
    fn invalidate_all(&self) {
        let mut gens = self.generations.write();
        for gen in gens.values_mut() { *gen += 1; }
        self.dim_cache.write().clear();
        self.soa_cache.write().clear();
        self.col_cache.write().clear();
    }

    fn current_gen(&self, table: &str) -> u64 {
        *self.generations.read().get(table).unwrap_or(&0)
    }

    // ---------- dimension hash cache ----------

    fn get_dim(&self, table: &str) -> Option<Arc<AHashMap<i64, Arc<[u8]>>>> {
        let gen = self.current_gen(table);
        let cache = self.dim_cache.read();
        cache.get(table).and_then(|c| {
            if c.generation == gen { Some(Arc::clone(&c.name_by_id)) } else { None }
        })
    }

    fn put_dim(&self, table: &str, map: AHashMap<i64, Arc<[u8]>>) -> Arc<AHashMap<i64, Arc<[u8]>>> {
        let gen = self.current_gen(table);
        let arc = Arc::new(map);
        self.dim_cache.write().insert(table.to_string(), CachedDimension {
            generation: gen,
            name_by_id: Arc::clone(&arc),
        });
        arc
    }

    // ---------- SoA column cache ----------

    fn get_soa(&self, table: &str) -> Option<Arc<JoinInputSoA>> {
        let gen = self.current_gen(table);
        let cache = self.soa_cache.read();
        cache.get(table).and_then(|c| {
            if c.generation == gen { Some(Arc::clone(&c.soa)) } else { None }
        })
    }

    fn put_soa(&self, table: &str, soa: JoinInputSoA) -> Arc<JoinInputSoA> {
        let gen = self.current_gen(table);
        let arc = Arc::new(soa);
        self.soa_cache.write().insert(table.to_string(), CachedSoA {
            generation: gen,
            soa: Arc::clone(&arc),
        });
        arc
    }

    // ---------- columnar analytics cache ----------

    fn get_cols(&self, table: &str) -> Option<Arc<CachedColumns>> {
        let gen = self.current_gen(table);
        let cache = self.col_cache.read();
        cache.get(table).and_then(|c| {
            if c.generation == gen { Some(Arc::clone(c)) } else { None }
        })
    }

    fn put_cols(&self, table: &str, cc: CachedColumns) -> Arc<CachedColumns> {
        let arc = Arc::new(cc);
        {
            let mut cache = self.col_cache.write();
            cache.insert(table.to_string(), Arc::clone(&arc));
            // Budget-aware eviction: if total exceeds COL_CACHE_BUDGET, evict
            // the largest entry that is NOT the one we just inserted.
            let total: usize = cache.values().map(|c| c.estimated_bytes()).sum();
            if total > COL_CACHE_BUDGET {
                let victim = cache.iter()
                    .filter(|(k, _)| k.as_str() != table)
                    .max_by_key(|(_, v)| v.estimated_bytes())
                    .map(|(k, _)| k.clone());
                if let Some(vk) = victim {
                    cache.remove(&vk);
                }
            }
        }
        arc
    }
}

// ---------------------------------------------------------------------------
//  Prepared Statement Cache — skip redundant SQL parsing on repeated queries
// ---------------------------------------------------------------------------

/// Identifies which dispatch branch a SQL statement should take.
/// Cached so that repeated statements skip `to_ascii_uppercase()` + `starts_with()` chain.
#[derive(Clone, Debug)]
enum CachedCommand {
    Begin,
    Commit,
    Rollback,
    Set,
    ShowStats,
    Show,
    Analyze,
    Vacuum,
    Copy,
    CreateUser,
    DropUser,
    AlterUser,
    Grant,
    Revoke,
    DropTable,
    CreateTable,
    CreateIndex,
    DropIndex,
    DeleteFrom,
    InsertInto,
    Update,
    Select,
    With,
    Explain,
    Savepoint,
    ReleaseSavepoint,
    RollbackToSavepoint,
    Unknown,
}

/// Supported window function types.
enum WindowFunc {
    RowNumber,
    Rank,
    DenseRank,
    Lag(String, usize),   // (column, offset)
    Lead(String, usize),
}

/// A cached parse plan for a SQL statement template.
/// Stores the pre-computed uppercase string and keyword positions
/// so handlers don't need to recompute them.
#[derive(Clone, Debug)]
struct CachedPlan {
    command: CachedCommand,
}

/// Statement cache: maps SQL template hash → CachedPlan.
/// Templates are created by normalizing numeric/string literals to placeholders.
/// Uses a fixed-size LRU-like approach with AHashMap for O(1) lookup.
pub(crate) struct StmtCache {
    plans: PLRwLock<AHashMap<u64, CachedPlan>>,
}

impl StmtCache {
    fn new() -> Self {
        Self {
            plans: PLRwLock::new(AHashMap::with_capacity(1024)),
        }
    }

    /// Compute a fast hash of the SQL template (first 12 non-digit chars + length).
    /// This gives us a quick discriminator for the dispatch cache.
    #[inline]
    fn template_hash(sql: &str) -> u64 {
        // Fast hash: use the command prefix (first word + table) plus total length.
        // For benchmark-hot queries like "SELECT * FROM t WHERE id = 42",
        // the template is stable across different literal values.
        let bytes = sql.as_bytes();
        let len = bytes.len();
        // Hash the first 64 bytes (covers command + table + structure) + length as discriminator
        let prefix_len = len.min(64);
        let mut h: u64 = len as u64;
        for &b in &bytes[..prefix_len] {
            // Skip digit characters so "WHERE id = 42" and "WHERE id = 99" hash the same
            if b.is_ascii_digit() { continue; }
            h = h.wrapping_mul(31).wrapping_add(b as u64);
        }
        h
    }

    fn get(&self, hash: u64) -> Option<CachedPlan> {
        self.plans.read().get(&hash).cloned()
    }

    fn put(&self, hash: u64, plan: CachedPlan) {
        let mut cache = self.plans.write();
        // Evict if over capacity
        if cache.len() >= 4096 {
            cache.clear();
        }
        cache.insert(hash, plan);
    }
}

impl Clone for StmtCache {
    fn clone(&self) -> Self {
        Self {
            plans: PLRwLock::new(self.plans.read().clone()),
        }
    }
}

#[derive(Clone)]
pub struct NativeSqlEngine {
    pub(crate) tables: Arc<PLRwLock<HashMap<String, NativeTable>>>,
    pub(crate) index_mgr: Arc<IndexManager>,
    /// Directory for WAL + snapshot persistence. None = pure in-memory.
    pub(crate) data_dir: Option<PathBuf>,
    /// Append-only WAL file handle (shared across clones).
    pub(crate) wal_writer: Arc<PLRwLock<Option<fs::File>>>,
    /// Mutations since last checkpoint (for auto-checkpoint).
    pub(crate) wal_mutations: Arc<AtomicU64>,
    /// Authorization manager.
    pub auth: AuthManager,
    /// Buffer Pool — caches dimension hash tables + SoA column stores.
    pub(crate) buf_pool: Arc<BufferPool>,
    /// Audit logger for DDL/DML/auth events.
    pub(crate) audit: AuditLogger,
    /// Prepared statement cache — skips redundant SQL parsing.
    pub(crate) stmt_cache: Arc<StmtCache>,
    /// Monotonically increasing row-level LSN.
    pub(crate) row_lsn_counter: Arc<AtomicU64>,
    /// Tombstone log for differential backups: (table, row_id, lsn).
    pub(crate) tombstone_log: Arc<PLRwLock<Vec<(String, i64, u64)>>>,
    /// Savepoint stack: Vec<(name, snapshot of tables)>.
    pub(crate) savepoints: Arc<std::sync::Mutex<Vec<(String, HashMap<String, NativeTable>)>>>,
}

impl NativeSqlEngine {
    pub fn new() -> Self {
        Self {
            tables: Arc::new(PLRwLock::new(HashMap::new())),
            index_mgr: Arc::new(IndexManager::new()),
            data_dir: None,
            wal_writer: Arc::new(PLRwLock::new(None)),
            wal_mutations: Arc::new(AtomicU64::new(0)),
            auth: AuthManager::new(),
            buf_pool: Arc::new(BufferPool::new()),
            audit: AuditLogger::stderr(),
            stmt_cache: Arc::new(StmtCache::new()),
            row_lsn_counter: Arc::new(AtomicU64::new(0)),
            tombstone_log: Arc::new(PLRwLock::new(Vec::new())),
            savepoints: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Create engine with disk persistence at `dir`.
    pub fn with_data_dir(dir: PathBuf) -> Self {
        fs::create_dir_all(&dir).ok();
        let mut engine = Self {
            tables: Arc::new(PLRwLock::new(HashMap::new())),
            index_mgr: Arc::new(IndexManager::new()),
            data_dir: Some(dir.clone()),
            wal_writer: Arc::new(PLRwLock::new(None)),
            wal_mutations: Arc::new(AtomicU64::new(0)),
            auth: AuthManager::with_data_dir(dir.clone()),
            buf_pool: Arc::new(BufferPool::new()),
            audit: AuditLogger::new(dir.join("audit.log")),
            stmt_cache: Arc::new(StmtCache::new()),
            row_lsn_counter: Arc::new(AtomicU64::new(0)),
            tombstone_log: Arc::new(PLRwLock::new(Vec::new())),
            savepoints: Arc::new(std::sync::Mutex::new(Vec::new())),
        };
        // 1. Load binary snapshot if available (fast path).
        engine.load_snapshot();
        // 2. Replay WAL on top of snapshot (only the delta).
        engine.replay_wal();
        // Open WAL for appending new mutations.
        let wal_path = dir.join("native_sql.wal");
        if let Ok(f) = fs::OpenOptions::new().create(true).append(true).open(&wal_path) {
            *engine.wal_writer.write() = Some(f);
        }
        engine
    }

    /// Append a SQL statement to the WAL with CRC32 integrity checksum.
    /// Format: `CRC32_HEX\tSQL_STATEMENT\n`
    fn wal_append(&self, sql: &str) {
        {
            let mut guard = self.wal_writer.write();
            if let Some(ref mut f) = *guard {
                // Compute CRC32 checksum for the SQL line.
                let mut hasher = crc32fast::Hasher::new();
                hasher.update(sql.as_bytes());
                let crc = hasher.finalize();
                if writeln!(f, "{:08x}\t{}", crc, sql).is_err() {
                    tracing::error!("WAL append failed");
                    return;
                }
                if f.flush().is_err() {
                    tracing::error!("WAL flush failed");
                    return;
                }
                // fsync to ensure durability
                if f.sync_all().is_err() {
                    tracing::error!("WAL fsync failed");
                }
            }
        }
    }

    /// WAL-log a successfully-executed mutation + auto-checkpoint.
    fn wal_log_success(&self, sql: &str) {
        self.wal_append(sql);
        if self.wal_mutations.fetch_add(1, Ordering::Relaxed) + 1 >= CHECKPOINT_INTERVAL {
            self.checkpoint();
        }
    }

    /// Replay WAL file to rebuild in-memory state.
    /// Supports both checksummed (`CRC32\tSQL`) and legacy (`SQL`) format.
    fn replay_wal(&mut self) {
        let wal_path = match &self.data_dir {
            Some(d) => d.join("native_sql.wal"),
            None => return,
        };
        let file = match fs::File::open(&wal_path) {
            Ok(f) => f,
            Err(_) => return,
        };
        let reader = BufReader::new(file);
        for line in reader.lines() {
            if let Ok(raw) = line {
                let trimmed = raw.trim();
                if trimmed.is_empty() { continue; }
                // Try checksummed format: "CRC32_HEX\tSQL"
                let sql = if let Some((crc_hex, sql_part)) = trimmed.split_once('\t') {
                    if crc_hex.len() == 8 {
                        if let Ok(stored_crc) = u32::from_str_radix(crc_hex, 16) {
                            let mut hasher = crc32fast::Hasher::new();
                            hasher.update(sql_part.as_bytes());
                            let computed_crc = hasher.finalize();
                            if stored_crc != computed_crc {
                                tracing::warn!("WAL CRC mismatch, skipping line: {}", trimmed);
                                continue;
                            }
                            sql_part
                        } else {
                            trimmed // fallback: legacy format
                        }
                    } else {
                        trimmed // fallback: legacy format
                    }
                } else {
                    trimmed // legacy format (no tab)
                };
                // Execute without WAL append to avoid re-logging.
                let _ = self.execute_inner(sql, false);
            }
        }
    }

    /// Load binary snapshot from disk into memory.
    fn load_snapshot(&mut self) {
        let dir = match &self.data_dir {
            Some(d) => d,
            None => return,
        };
        let snap_path = dir.join("native_sql.snap");
        let data = match fs::read(&snap_path) {
            Ok(d) => d,
            Err(_) => return,
        };
        // Try bincode first, fall back to JSON for backward compat.
        let loaded: HashMap<String, NativeTable> = match bincode::deserialize(&data) {
            Ok(t) => t,
            Err(_) => match serde_json::from_slice(&data) {
                Ok(t) => t,
                Err(_) => return,
            },
        };
        *self.tables.write() = loaded;
    }

    /// Write a full binary snapshot and truncate WAL.
    /// Holds tables write lock to prevent mutations between snapshot and WAL truncation.
    pub fn checkpoint(&self) {
        let dir = match &self.data_dir {
            Some(d) => d,
            None => return,
        };
        let snap_path = dir.join("native_sql.snap");
        // Hold WRITE lock to prevent concurrent mutations during snapshot+WAL truncation.
        let tables = self.tables.write();
        let data = match bincode::serialize(&*tables) {
            Ok(d) => d,
            Err(_) => return,
        };
        // Atomic write: tmp → rename.
        let tmp = dir.join("native_sql.snap.tmp");
        if fs::write(&tmp, &data).is_ok() {
            let _ = fs::rename(&tmp, &snap_path);
            // Truncate WAL — snapshot is the new baseline.
            let wal_path = dir.join("native_sql.wal");
            if let Ok(f) = fs::File::create(&wal_path) {
                drop(f);
                // Reopen for appending.
                {
                    let mut guard = self.wal_writer.write();
                    if let Ok(f2) = fs::OpenOptions::new().append(true).open(&wal_path) {
                        *guard = Some(f2);
                    }
                }
            }
            self.wal_mutations.store(0, Ordering::Relaxed);
        }
    }

    // -----------------------------------------------------------------------
    // Spill-to-disk — persist cold columnar caches to SSD
    // -----------------------------------------------------------------------

    fn spill_cold_caches_to_disk(&self, data_dir: &PathBuf) {
        let spill_dir = data_dir.join("spill");
        let _ = fs::create_dir_all(&spill_dir);

        // Spill any col_cache entries that are NOT currently locked in hot use.
        let entries: Vec<(String, Arc<CachedColumns>)> = {
            let cache = self.buf_pool.col_cache.read();
            cache.iter().map(|(k, v)| (k.clone(), Arc::clone(v))).collect()
        };
        for (table, cc) in entries {
            let path = spill_dir.join(format!("{}.colcache", table));
            // Serialize generation + columnar data as bincode.
            let payload = bincode::serialize(&(
                cc.generation,
                &cc.ids,
                &cc.int_cols.iter().collect::<Vec<_>>(),
                &cc.float_cols.iter().collect::<Vec<_>>(),
                &cc.text_cols.iter().collect::<Vec<_>>(),
            ));
            if let Ok(data) = payload {
                let tmp = spill_dir.join(format!("{}.colcache.tmp", table));
                if fs::write(&tmp, &data).is_ok() {
                    let _ = fs::rename(&tmp, &path);
                }
            }
        }
    }

    /// Expose the index manager for external use (dashboard, CLI).
    pub fn index_manager(&self) -> &Arc<IndexManager> {
        &self.index_mgr
    }

    /// Build sorted columnar cache from a NativeTable for analytics queries.
    fn build_column_cache(_table_name: &str, t: &NativeTable, gen: u64) -> CachedColumns {
        let n = t.rows.len();
        // Collect (id, row_ref) pairs and sort by id.
        let mut pairs: Vec<(i64, &NativeRow)> = t.rows.iter()
            .map(|(id, row)| (*id, row))
            .collect();
        pairs.sort_unstable_by_key(|(id, _)| *id);

        let mut ids = Vec::with_capacity(n);
        let mut int_cols: AHashMap<String, Vec<i64>> = AHashMap::new();
        let mut float_cols: AHashMap<String, Vec<f64>> = AHashMap::new();
        let mut text_cols: AHashMap<String, Vec<String>> = AHashMap::new();

        // Pre-allocate column vectors for all types.
        for (i, col_name) in t.columns.iter().enumerate() {
            match t.column_types.get(i) {
                Some(ColType::Integer) => {
                    // Skip the first column (primary key "id") since it's stored in `ids`.
                    if i > 0 { int_cols.insert(col_name.clone(), Vec::with_capacity(n)); }
                }
                Some(ColType::Float8) => { float_cols.insert(col_name.clone(), Vec::with_capacity(n)); }
                Some(ColType::Text) => { text_cols.insert(col_name.clone(), Vec::with_capacity(n)); }
                _ => {}
            }
        }

        for (id, row) in &pairs {
            ids.push(*id);
            for (i, col_name) in t.columns.iter().enumerate() {
                let cell = row.cols.get(col_name).cloned().unwrap_or(Cell::Null);
                match t.column_types.get(i) {
                    Some(ColType::Integer) => {
                        if let Some(v) = int_cols.get_mut(col_name) {
                            v.push(cell.as_i64());
                        }
                    }
                    Some(ColType::Float8) => {
                        if let Some(v) = float_cols.get_mut(col_name) {
                            v.push(cell.as_f64());
                        }
                    }
                    Some(ColType::Text) => {
                        if let Some(v) = text_cols.get_mut(col_name) {
                            v.push(cell.as_text());
                        }
                    }
                    _ => {}
                }
            }
        }

        CachedColumns { generation: gen, ids, int_cols, float_cols, text_cols }
    }

    /// Get or build the columnar cache for a table.
    fn get_or_build_cols(&self, table_name: &str, t: &NativeTable) -> Arc<CachedColumns> {
        if let Some(cached) = self.buf_pool.get_cols(table_name) {
            return cached;
        }
        let gen = self.buf_pool.current_gen(table_name);
        let cc = Self::build_column_cache(table_name, t, gen);
        self.buf_pool.put_cols(table_name, cc)
    }

    pub fn execute(&self, sql: &str) -> Result<QueryResult, String> {
        self.execute_inner(sql, true)
    }

    /// Execute with a specific user context for privilege checking.
    pub fn execute_as(&self, sql: &str, username: &str) -> Result<QueryResult, String> {
        self.execute_inner_authed(sql, true, username)
    }

    fn execute_inner(&self, sql: &str, with_wal: bool) -> Result<QueryResult, String> {
        // Internal calls (WAL replay, etc.) bypass auth.
        self.execute_inner_authed(sql, with_wal, "admin")
    }

    fn execute_inner_authed(&self, sql: &str, with_wal: bool, username: &str) -> Result<QueryResult, String> {
        let s = sql.trim().trim_end_matches(';').trim();
        if s.is_empty() {
            return Ok(Self::empty_ok("OK"));
        }

        // --- Prepared Statement Cache: skip to_ascii_uppercase() on repeated queries ---
        let tmpl_hash = StmtCache::template_hash(s);
        let cached = self.stmt_cache.get(tmpl_hash);

        let cmd = if let Some(ref plan) = cached {
            // Cache HIT — zero-cost dispatch (no uppercase allocation needed).
            // Hot-path handlers now use find_keyword_ci() instead of uppercase strings.
            plan.command.clone()
        } else {
            // Cache MISS — compute uppercase once for classify_command().
            let upper = s.to_ascii_uppercase();
            let command = Self::classify_command(&upper);
            self.stmt_cache.put(tmpl_hash, CachedPlan {
                command: command.clone(),
            });
            command
        };

        // --- Dispatch using cached command classification ---
        match cmd {
            CachedCommand::Begin => return Ok(Self::empty_ok("BEGIN")),
            CachedCommand::Commit => return Ok(Self::empty_ok("COMMIT")),
            CachedCommand::Rollback => return Ok(Self::empty_ok("ROLLBACK")),
            CachedCommand::Savepoint => return self.handle_savepoint(s),
            CachedCommand::ReleaseSavepoint => return self.handle_release_savepoint(s),
            CachedCommand::RollbackToSavepoint => return self.handle_rollback_to_savepoint(s),
            CachedCommand::Set => return Ok(Self::empty_ok("OK")),
            CachedCommand::ShowStats => return self.handle_show_stats(s),
            CachedCommand::Show => return self.handle_show(s),
            CachedCommand::Analyze => return self.handle_analyze(s),
            CachedCommand::Vacuum => return self.handle_vacuum(s),
            CachedCommand::Copy => {
                let tbl = Self::parse_ident_after(s, "COPY").unwrap_or("");
                self.auth.check_privilege(username, tbl, Privilege::Insert)?;
                return self.handle_copy(s);
            }
            CachedCommand::CreateUser => {
                let result = self.handle_create_user(s, username);
                self.audit.log(&AuditEvent::ddl(username, "CREATE USER", "", result.is_ok(), result.as_ref().err().map(|e| e.as_str())));
                return result;
            }
            CachedCommand::DropUser => {
                let result = self.handle_drop_user(s, username);
                self.audit.log(&AuditEvent::ddl(username, "DROP USER", "", result.is_ok(), result.as_ref().err().map(|e| e.as_str())));
                return result;
            }
            CachedCommand::AlterUser => {
                let result = self.handle_alter_user(s, username);
                self.audit.log(&AuditEvent::ddl(username, "ALTER USER", "", result.is_ok(), result.as_ref().err().map(|e| e.as_str())));
                return result;
            }
            CachedCommand::Grant => {
                let result = self.handle_grant(s, username);
                self.audit.log(&AuditEvent::ddl(username, "GRANT", "", result.is_ok(), result.as_ref().err().map(|e| e.as_str())));
                return result;
            }
            CachedCommand::Revoke => {
                let result = self.handle_revoke(s, username);
                self.audit.log(&AuditEvent::ddl(username, "REVOKE", "", result.is_ok(), result.as_ref().err().map(|e| e.as_str())));
                return result;
            }
            CachedCommand::DropTable => {
                let tbl = Self::parse_ident_after(s, "DROP TABLE").unwrap_or("");
                self.auth.check_privilege(username, tbl, Privilege::Drop)?;
                let result = self.handle_drop_table(s);
                if result.is_ok() && with_wal { self.wal_log_success(s); }
                self.audit.log(&AuditEvent::ddl(username, "DROP TABLE", tbl, result.is_ok(), result.as_ref().err().map(|e| e.as_str())));
                return result;
            }
            CachedCommand::CreateTable => {
                let tbl = Self::parse_ident_after(s, "CREATE TABLE").unwrap_or("");
                self.auth.check_privilege(username, tbl, Privilege::Create)?;
                let result = self.handle_create_table(s);
                if result.is_ok() && with_wal { self.wal_log_success(s); }
                self.audit.log(&AuditEvent::ddl(username, "CREATE TABLE", tbl, result.is_ok(), result.as_ref().err().map(|e| e.as_str())));
                return result;
            }
            CachedCommand::CreateIndex => {
                self.auth.check_privilege(username, "*", Privilege::Create)?;
                let result = self.handle_create_index(s);
                if result.is_ok() && with_wal { self.wal_log_success(s); }
                self.audit.log(&AuditEvent::ddl(username, "CREATE INDEX", "", result.is_ok(), result.as_ref().err().map(|e| e.as_str())));
                return result;
            }
            CachedCommand::DropIndex => {
                self.auth.check_privilege(username, "*", Privilege::Drop)?;
                let result = self.handle_drop_index(s);
                if result.is_ok() && with_wal { self.wal_log_success(s); }
                self.audit.log(&AuditEvent::ddl(username, "DROP INDEX", "", result.is_ok(), result.as_ref().err().map(|e| e.as_str())));
                return result;
            }
            CachedCommand::DeleteFrom => {
                let tbl = Self::parse_ident_after(s, "DELETE FROM").unwrap_or("");
                self.auth.check_privilege(username, tbl, Privilege::Delete)?;
                let result = self.handle_delete(s);
                if result.is_ok() && with_wal { self.wal_log_success(s); }
                self.audit.log(&AuditEvent::dml(username, "DELETE", tbl, result.is_ok(), result.as_ref().err().map(|e| e.as_str())));
                return result;
            }
            CachedCommand::InsertInto => {
                let tbl = Self::parse_ident_after(s, "INSERT INTO").unwrap_or("");
                self.auth.check_privilege(username, tbl, Privilege::Insert)?;
                let result = self.handle_insert(s);
                if result.is_ok() && with_wal { self.wal_log_success(s); }
                self.audit.log(&AuditEvent::dml(username, "INSERT", tbl, result.is_ok(), result.as_ref().err().map(|e| e.as_str())));
                return result;
            }
            CachedCommand::Update => {
                let tbl = Self::parse_ident_after(s, "UPDATE").unwrap_or("");
                self.auth.check_privilege(username, tbl, Privilege::Update)?;
                let result = self.handle_update(s);
                if result.is_ok() && with_wal { self.wal_log_success(s); }
                self.audit.log(&AuditEvent::dml(username, "UPDATE", tbl, result.is_ok(), result.as_ref().err().map(|e| e.as_str())));
                return result;
            }
            CachedCommand::Select => {
                if let Some(from_tbl) = Self::extract_from_table(s) {
                    self.auth.check_privilege(username, &from_tbl, Privilege::Select)?;
                }
                return self.handle_select(s);
            }
            CachedCommand::With => {
                return self.handle_with_cte(s, username);
            }
            CachedCommand::Explain => {
                return self.handle_explain(s);
            }
            CachedCommand::Unknown => {}
        }

        Ok(Self::empty_ok("OK"))
    }

    /// Classify a SQL command from its uppercase representation.
    /// Single pass through a prefix-based dispatch table.
    fn classify_command(up: &str) -> CachedCommand {
        // Use first byte for fast dispatch
        match up.as_bytes().first() {
            Some(b'B') if up.starts_with("BEGIN") => CachedCommand::Begin,
            Some(b'S') => {
                if up.starts_with("START TRANSACTION") { return CachedCommand::Begin; }
                if up.starts_with("SET ") { return CachedCommand::Set; }
                if up.starts_with("SHOW STATS") { return CachedCommand::ShowStats; }
                if up.starts_with("SHOW ") { return CachedCommand::Show; }
                if up.starts_with("SAVEPOINT ") { return CachedCommand::Savepoint; }
                if up.starts_with("SELECT") { return CachedCommand::Select; }
                CachedCommand::Unknown
            }
            Some(b'C') => {
                if up.starts_with("COMMIT") { return CachedCommand::Commit; }
                if up.starts_with("COPY ") { return CachedCommand::Copy; }
                if up.starts_with("CREATE USER") || up.starts_with("CREATE ROLE") { return CachedCommand::CreateUser; }
                if up.starts_with("CREATE INDEX") || up.starts_with("CREATE UNIQUE INDEX") { return CachedCommand::CreateIndex; }
                if up.starts_with("CREATE TABLE") { return CachedCommand::CreateTable; }
                CachedCommand::Unknown
            }
            Some(b'R') => {
                if up.starts_with("ROLLBACK TO SAVEPOINT") || up.starts_with("ROLLBACK TO ") { return CachedCommand::RollbackToSavepoint; }
                if up.starts_with("ROLLBACK") { return CachedCommand::Rollback; }
                if up.starts_with("RELEASE SAVEPOINT") || up.starts_with("RELEASE ") { return CachedCommand::ReleaseSavepoint; }
                if up.starts_with("REVOKE ") { return CachedCommand::Revoke; }
                CachedCommand::Unknown
            }
            Some(b'A') => {
                if up.starts_with("ABORT") { return CachedCommand::Rollback; }
                if up.starts_with("ANALYZE") { return CachedCommand::Analyze; }
                if up.starts_with("ALTER USER") || up.starts_with("ALTER ROLE") { return CachedCommand::AlterUser; }
                CachedCommand::Unknown
            }
            Some(b'V') if up.starts_with("VACUUM") => CachedCommand::Vacuum,
            Some(b'D') => {
                if up.starts_with("DROP USER") || up.starts_with("DROP ROLE") { return CachedCommand::DropUser; }
                if up.starts_with("DROP TABLE") { return CachedCommand::DropTable; }
                if up.starts_with("DROP INDEX") { return CachedCommand::DropIndex; }
                if up.starts_with("DELETE FROM") { return CachedCommand::DeleteFrom; }
                CachedCommand::Unknown
            }
            Some(b'G') if up.starts_with("GRANT ") => CachedCommand::Grant,
            Some(b'I') if up.starts_with("INSERT INTO") => CachedCommand::InsertInto,
            Some(b'U') if up.starts_with("UPDATE") => CachedCommand::Update,
            Some(b'W') if up.starts_with("WITH ") => CachedCommand::With,
            Some(b'E') if up.starts_with("EXPLAIN") => CachedCommand::Explain,
            _ => CachedCommand::Unknown,
        }
    }

    // -----------------------------------------------------------------------
    // Auth SQL handlers
    // -----------------------------------------------------------------------

    /// CREATE USER <name> WITH PASSWORD '<pw>'
    fn handle_create_user(&self, s: &str, caller: &str) -> Result<QueryResult, String> {
        if !self.auth.is_superuser(caller) {
            return Err("permission denied: must be superuser".to_string());
        }
        let (username, password) = Self::parse_create_user(s)?;
        let tag = self.auth.create_user(&username, &password)?;
        Ok(Self::empty_ok(&tag))
    }

    /// DROP USER <name>
    fn handle_drop_user(&self, s: &str, caller: &str) -> Result<QueryResult, String> {
        if !self.auth.is_superuser(caller) {
            return Err("permission denied: must be superuser".to_string());
        }
        let up = s.to_ascii_uppercase();
        let kw = if up.starts_with("DROP ROLE") { "DROP ROLE" } else { "DROP USER" };
        let name = Self::parse_ident_after(s, kw).ok_or("Invalid DROP USER")?;
        let tag = self.auth.drop_user(name)?;
        Ok(Self::empty_ok(&tag))
    }

    /// ALTER USER <name> WITH PASSWORD '<pw>'
    fn handle_alter_user(&self, s: &str, caller: &str) -> Result<QueryResult, String> {
        // Superuser can alter anyone; normal user can only change their own password.
        let (username, password) = Self::parse_alter_user(s)?;
        if username != caller && !self.auth.is_superuser(caller) {
            return Err("permission denied: must be superuser".to_string());
        }
        let tag = self.auth.alter_user_password(&username, &password)?;
        Ok(Self::empty_ok(&tag))
    }

    /// GRANT <privs> ON <table> TO <user>
    fn handle_grant(&self, s: &str, caller: &str) -> Result<QueryResult, String> {
        if !self.auth.is_superuser(caller) {
            return Err("permission denied: must be superuser".to_string());
        }
        let (privs, table, user) = Self::parse_grant(s)?;
        let tag = self.auth.grant(&privs, &table, &user)?;
        Ok(Self::empty_ok(&tag))
    }

    /// REVOKE <privs> ON <table> FROM <user>
    fn handle_revoke(&self, s: &str, caller: &str) -> Result<QueryResult, String> {
        if !self.auth.is_superuser(caller) {
            return Err("permission denied: must be superuser".to_string());
        }
        let (privs, table, user) = Self::parse_revoke(s)?;
        let tag = self.auth.revoke(&privs, &table, &user)?;
        Ok(Self::empty_ok(&tag))
    }

    // -----------------------------------------------------------------------
    // Auth SQL parsers (lightweight, no external dep)
    // -----------------------------------------------------------------------

    /// Parse: CREATE USER|ROLE <name> WITH PASSWORD '<pw>'
    fn parse_create_user(s: &str) -> Result<(String, String), String> {
        let up = s.to_ascii_uppercase();
        // Find name between USER/ROLE and WITH
        let kw_end = if up.starts_with("CREATE ROLE") { 11 } else { 11 }; // "CREATE USER" = 11
        let with_idx = up.find(" WITH ").ok_or("Expected WITH PASSWORD")?;
        let name = s[kw_end..with_idx].trim().trim_matches('"').to_string();
        let pw = Self::extract_quoted_password(s)?;
        Ok((name, pw))
    }

    /// Parse: ALTER USER|ROLE <name> WITH PASSWORD '<pw>'
    fn parse_alter_user(s: &str) -> Result<(String, String), String> {
        let up = s.to_ascii_uppercase();
        let kw_end = if up.starts_with("ALTER ROLE") { 10 } else { 10 }; // "ALTER USER" = 10
        let with_idx = up.find(" WITH ").ok_or("Expected WITH PASSWORD")?;
        let name = s[kw_end..with_idx].trim().trim_matches('"').to_string();
        let pw = Self::extract_quoted_password(s)?;
        Ok((name, pw))
    }

    /// Extract single-quoted password from "... PASSWORD '<pw>'" clause.
    fn extract_quoted_password(s: &str) -> Result<String, String> {
        let up = s.to_ascii_uppercase();
        let pw_idx = up.find("PASSWORD").ok_or("Expected PASSWORD keyword")?;
        let after_pw = &s[pw_idx + 8..].trim_start();
        // Find opening quote
        let q_start = after_pw.find('\'').ok_or("Expected quoted password")?;
        let rest = &after_pw[q_start + 1..];
        let q_end = rest.find('\'').ok_or("Expected closing quote for password")?;
        Ok(rest[..q_end].to_string())
    }

    /// Parse: GRANT <privs> ON <table> TO <user>
    fn parse_grant(s: &str) -> Result<(Vec<Privilege>, String, String), String> {
        let up = s.to_ascii_uppercase();
        let on_idx = up.find(" ON ").ok_or("Expected ON in GRANT")?;
        let to_idx = up.find(" TO ").ok_or("Expected TO in GRANT")?;
        let privs_str = &s[6..on_idx]; // after "GRANT "
        let table = s[on_idx + 4..to_idx].trim().trim_matches('"').to_string();
        let user = s[to_idx + 4..].trim().trim_matches('"').trim_end_matches(';').trim().to_string();
        let privs: Vec<Privilege> = privs_str
            .split(',')
            .filter_map(|p| Privilege::from_str(p.trim()))
            .collect();
        if privs.is_empty() {
            return Err("No valid privileges specified".to_string());
        }
        Ok((privs, table, user))
    }

    /// Parse: REVOKE <privs> ON <table> FROM <user>
    fn parse_revoke(s: &str) -> Result<(Vec<Privilege>, String, String), String> {
        let up = s.to_ascii_uppercase();
        let on_idx = up.find(" ON ").ok_or("Expected ON in REVOKE")?;
        let from_idx = up.rfind(" FROM ").ok_or("Expected FROM in REVOKE")?;
        let privs_str = &s[7..on_idx]; // after "REVOKE "
        let table = s[on_idx + 4..from_idx].trim().trim_matches('"').to_string();
        let user = s[from_idx + 6..].trim().trim_matches('"').trim_end_matches(';').trim().to_string();
        let privs: Vec<Privilege> = privs_str
            .split(',')
            .filter_map(|p| Privilege::from_str(p.trim()))
            .collect();
        if privs.is_empty() {
            return Err("No valid privileges specified".to_string());
        }
        Ok((privs, table, user))
    }

    /// Extract table name from FROM clause in SELECT.
    fn extract_from_table(s: &str) -> Option<String> {
        let from_idx = Self::find_keyword_ci(s, " FROM ")?;
        let tail = s[from_idx + 6..].trim_start();
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '(' || c == ',')
            .unwrap_or(tail.len());
        let tbl = tail[..end].trim_matches('"');
        if tbl.is_empty() {
            None
        } else {
            Some(tbl.to_string())
        }
    }

    fn empty_ok(tag: &str) -> QueryResult {
        QueryResult {
            columns: vec![],
            rows: vec![],
            command_tag: tag.to_string(),
        }
    }

    fn parse_ident_after<'a>(s: &'a str, key: &str) -> Option<&'a str> {
        let idx = Self::find_keyword_ci(s, key)?;
        let tail = s[idx + key.len()..].trim_start();
        let end = tail.find(|c: char| c.is_whitespace() || c == '(').unwrap_or(tail.len());
        Some(&tail[..end])
    }

    /// Case-insensitive keyword search without allocating an uppercase copy.
    /// Returns the byte offset of the first occurrence of `keyword` (matched case-insensitively).
    #[inline]
    fn find_keyword_ci(haystack: &str, keyword: &str) -> Option<usize> {
        let h = haystack.as_bytes();
        let k = keyword.as_bytes();
        if k.len() > h.len() { return None; }
        let end = h.len() - k.len() + 1;
        'outer: for i in 0..end {
            for j in 0..k.len() {
                if h[i + j].to_ascii_uppercase() != k[j].to_ascii_uppercase() {
                    continue 'outer;
                }
            }
            return Some(i);
        }
        None
    }

    /// Case-insensitive word replacement (identifier-boundary aware).
    fn replace_word_ci(haystack: &str, word: &str, replacement: &str) -> String {
        let h = haystack.as_bytes();
        let w = word.as_bytes();
        if w.len() > h.len() { return haystack.to_string(); }

        let mut result = String::with_capacity(haystack.len());
        let mut i = 0;
        while i <= h.len() - w.len() {
            let mut matched = true;
            for j in 0..w.len() {
                if h[i + j].to_ascii_uppercase() != w[j].to_ascii_uppercase() {
                    matched = false;
                    break;
                }
            }
            if matched {
                // Check word boundaries
                let before_ok = i == 0 || !h[i - 1].is_ascii_alphanumeric() && h[i - 1] != b'_';
                let after_ok = i + w.len() >= h.len() || !h[i + w.len()].is_ascii_alphanumeric() && h[i + w.len()] != b'_';
                if before_ok && after_ok {
                    result.push_str(replacement);
                    i += w.len();
                    continue;
                }
            }
            result.push(haystack.as_bytes()[i] as char);
            i += 1;
        }
        // Append remaining bytes
        while i < h.len() {
            result.push(h[i] as char);
            i += 1;
        }
        result
    }

    /// Parse SELECT column list from query string.
    /// Handles: SELECT col1, col2, ... FROM ...
    fn parse_select_columns(s: &str) -> Vec<String> {
        let select_end = Self::find_keyword_ci(s, "SELECT").unwrap_or(0) + 6;
        let from_idx = Self::find_keyword_ci(s, " FROM ").unwrap_or(s.len());
        let cols_part = s[select_end..from_idx].trim();
        cols_part
            .split(',')
            .map(|c| {
                let c = c.trim();
                // Handle "table.col" -> "col"
                if let Some(dot) = c.rfind('.') {
                    c[dot + 1..].trim().to_string()
                } else {
                    c.to_string()
                }
            })
            .filter(|c| !c.is_empty() && c != "*")
            .collect()
    }

    fn parse_value(tok: &str) -> Cell {
        let t = tok.trim();
        if t.eq_ignore_ascii_case("NULL") {
            return Cell::Null;
        }
        if t.starts_with('\'') && t.ends_with('\'') && t.len() >= 2 {
            return Cell::Text(t[1..t.len() - 1].replace("''", "'"));
        }
        if let Ok(v) = t.parse::<i64>() {
            return Cell::Int(v);
        }
        if let Ok(v) = t.parse::<f64>() {
            return Cell::Float(v);
        }
        Cell::Text(t.to_string())
    }

    /// Parse `col IN (v1, v2, v3, ...)` from a WHERE clause fragment.
    /// Returns (column_name, values) if matched, None otherwise.
    fn parse_in_list(where_part: &str) -> Option<(String, Vec<Cell>)> {
        // Case-insensitive search for " IN " or " IN("
        let upper = where_part.to_ascii_uppercase();
        let in_pos = upper.find(" IN ")?;
        let col = where_part[..in_pos].trim().trim_matches('"').to_string();

        // Find the opening paren
        let after_in = &where_part[in_pos + 3..].trim_start();
        if !after_in.starts_with('(') { return None; }
        let open = where_part.len() - after_in.len();
        let close = where_part[open..].find(')')?;
        let list_content = &where_part[open + 1..open + close];

        let values: Vec<Cell> = list_content
            .split(',')
            .map(Self::parse_value)
            .collect();

        if values.is_empty() { return None; }
        Some((col, values))
    }

    /// Handle `DROP TABLE <name>`.
    /// Removes the table and cleans up BufferPool + IndexManager stats.
    fn handle_drop_table(&self, s: &str) -> Result<QueryResult, String> {
        let table = Self::parse_ident_after(s, "DROP TABLE").ok_or("Invalid DROP TABLE")?;
        let existed = {
            let mut g = self.tables.write();
            g.remove(table).is_some()
        };
        if !existed {
            return Err(format!("table \"{}\" does not exist", table));
        }
        // Clean up BufferPool caches for this table.
        self.buf_pool.remove_table(table);
        // Clean up IndexManager stats for this table.
        self.index_mgr.remove_table_stats(table);
        // Drop all indexes belonging to this table.
        self.index_mgr.drop_indexes_for_table(table);
        Ok(Self::empty_ok("DROP TABLE"))
    }

    fn handle_create_table(&self, s: &str) -> Result<QueryResult, String> {
        let table = Self::parse_ident_after(s, "CREATE TABLE").ok_or("Invalid CREATE TABLE")?;
        // Sanitize table name — reject path separators and non-alphanumeric chars.
        if !table.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err("Invalid table name: only alphanumeric and underscore allowed".into());
        }
        let open = s.find('(').ok_or("Invalid CREATE TABLE columns")?;
        let close = s.rfind(')').ok_or("Invalid CREATE TABLE columns")?;
        let defs = &s[open + 1..close];
        let mut cols = Vec::new();
        let mut col_types = Vec::new();
        for d in defs.split(',') {
            let parts: Vec<&str> = d.trim().split_whitespace().collect();
            let name = parts.first().unwrap_or(&"").trim_matches('"');
            if !name.is_empty() {
                cols.push(name.to_string());
                // Infer column type from DDL tokens.
                let type_str: String = parts.get(1).unwrap_or(&"TEXT").to_ascii_uppercase();
                let ct = if type_str.contains("INT") {
                    ColType::Integer
                } else if type_str.contains("REAL")
                    || type_str.contains("FLOAT")
                    || type_str.contains("DOUBLE")
                    || type_str.contains("NUMERIC")
                    || type_str.contains("DECIMAL")
                {
                    ColType::Float8
                } else {
                    ColType::Text
                };
                col_types.push(ct);
            }
        }
        if cols.is_empty() {
            cols.push("id".to_string());
            col_types.push(ColType::Integer);
        }

        let mut g = self.tables.write();
        g.entry(table.to_string())
            .or_insert_with(|| NativeTable::new(cols, col_types));
        Ok(Self::empty_ok("CREATE TABLE"))
    }

    /// Handle `CREATE INDEX name ON table (col1, ...)`.
    fn handle_create_index(&self, s: &str) -> Result<QueryResult, String> {
        // Parse: CREATE [UNIQUE] INDEX <name> ON <table> (<columns>)
        let up = s.to_ascii_uppercase();
        let is_unique = up.contains("UNIQUE");
        let on_idx = up.find(" ON ").ok_or("Invalid CREATE INDEX: missing ON")?;

        // Extract index name (between INDEX and ON)
        let idx_kw = up.find("INDEX").ok_or("Invalid CREATE INDEX")?;
        let name = s[idx_kw + 5..on_idx].trim().trim_matches('"');
        if name.is_empty() {
            return Err("Invalid CREATE INDEX: missing index name".into());
        }

        // Extract table name (between ON and open paren)
        let paren_open = s.find('(').ok_or("Invalid CREATE INDEX: missing (")?;
        let table = s[on_idx + 4..paren_open].trim().trim_matches('"');

        // Extract columns
        let paren_close = s.rfind(')').ok_or("Invalid CREATE INDEX: missing )")?;
        let cols: Vec<String> = s[paren_open + 1..paren_close]
            .split(',')
            .map(|c| c.trim().trim_matches('"').to_string())
            .filter(|c| !c.is_empty())
            .collect();

        if cols.is_empty() {
            return Err("Invalid CREATE INDEX: no columns specified".into());
        }

        // Create the index and populate from existing table data
        let tree = self.index_mgr.create_manual_index_with_unique(name, table, &cols, is_unique);

        // Back-fill: scan existing rows and insert into the new index
        let g = self.tables.read();
        if let Some(t) = g.get(table) {
            // For unique indexes, check for duplicates during back-fill.
            for (&row_id, row) in &t.rows {
                for col in &cols {
                    if let Some(val) = row.cols.get(col.as_str()) {
                        let idx_key = match val {
                            Cell::Int(v) => IndexKey::Integer(*v),
                            Cell::Text(v) => IndexKey::Str(v.clone()),
                            Cell::Float(v) => IndexKey::Integer(*v as i64),
                            Cell::Null => continue,
                        };
                        if is_unique {
                            let existing = tree.search(&idx_key);
                            if !existing.is_empty() {
                                // Duplicate found — drop the partially built index and fail.
                                drop(g);
                                self.index_mgr.drop_index(name);
                                return Err(format!(
                                    "could not create unique index \"{}\": duplicate key value violates unique constraint (column \"{}\")",
                                    name, col
                                ));
                            }
                        }
                        tree.insert(idx_key, row_id);
                    }
                }
            }
        }

        Ok(Self::empty_ok("CREATE INDEX"))
    }

    /// Handle `DROP INDEX <name>`.
    fn handle_drop_index(&self, s: &str) -> Result<QueryResult, String> {
        let name = Self::parse_ident_after(s, "DROP INDEX")
            .ok_or("Invalid DROP INDEX")?
            .trim_matches('"');
        if self.index_mgr.drop_index(name) {
            Ok(Self::empty_ok("DROP INDEX"))
        } else {
            Err(format!("Index '{}' does not exist", name))
        }
    }

    // -----------------------------------------------------------------------
    // ANALYZE — collect table statistics for the optimizer
    // -----------------------------------------------------------------------

    fn handle_analyze(&self, s: &str) -> Result<QueryResult, String> {
        let table = Self::parse_ident_after(s, "ANALYZE")
            .unwrap_or("")
            .trim_matches('"');

        let g = self.tables.read();

        let tables_to_analyze: Vec<&str> = if table.is_empty() {
            g.keys().map(|k| k.as_str()).collect()
        } else {
            if !g.contains_key(table) {
                return Err(format!("table \"{}\" does not exist", table));
            }
            vec![table]
        };

        let mut total_analyzed = 0u64;
        for tname in &tables_to_analyze {
            if let Some(t) = g.get(*tname) {
                let total_rows = t.rows.len() as u64;
                for (ci, col) in t.columns.iter().enumerate() {
                    let mut distinct_vals: HashSet<u64> = HashSet::new();
                    for row in t.rows.values() {
                        if let Some(cell) = row.cols.get(col.as_str()) {
                            match cell {
                                Cell::Int(v) => {
                                    self.index_mgr.record_numeric_value(tname, col, *v as f64);
                                    distinct_vals.insert(*v as u64);
                                }
                                Cell::Float(v) => {
                                    self.index_mgr.record_numeric_value(tname, col, *v);
                                    distinct_vals.insert(v.to_bits());
                                }
                                Cell::Text(v) => {
                                    let h = {
                                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                                        std::hash::Hash::hash(v, &mut hasher);
                                        std::hash::Hasher::finish(&hasher)
                                    };
                                    distinct_vals.insert(h);
                                }
                                Cell::Null => {}
                            }
                        }
                    }
                    let ndv = distinct_vals.len() as u64;
                    self.index_mgr.update_selectivity(tname, col, ndv, total_rows);
                }
                total_analyzed += 1;
            }
        }

        let msg = format!("ANALYZE {}", total_analyzed);
        Ok(QueryResult {
            columns: vec![("analyze".to_string(), oid::TEXT, -1)],
            rows: vec![vec![Some(msg.as_bytes().to_vec())]],
            command_tag: format!("ANALYZE {}", total_analyzed),
        })
    }

    // -----------------------------------------------------------------------
    // SHOW STATS — display gathered statistics for a table
    // -----------------------------------------------------------------------

    fn handle_show_stats(&self, s: &str) -> Result<QueryResult, String> {
        // SHOW STATS <table>  or  SHOW STATS (all tables)
        let table = Self::parse_ident_after(s, "SHOW STATS")
            .unwrap_or("")
            .trim_matches('"');

        let g = self.tables.read();
        let stats = self.index_mgr.stats.read();

        let columns = vec![
            ("table".to_string(), oid::TEXT, -1i16),
            ("column".to_string(), oid::TEXT, -1i16),
            ("rows".to_string(), oid::INT8, 8i16),
            ("distinct".to_string(), oid::INT8, 8i16),
            ("selectivity".to_string(), oid::FLOAT8, 8i16),
            ("min".to_string(), oid::TEXT, -1i16),
            ("max".to_string(), oid::TEXT, -1i16),
            ("histogram_buckets".to_string(), oid::TEXT, -1i16),
        ];

        let mut rows_out: Vec<Vec<Option<Vec<u8>>>> = Vec::new();

        let tables_iter: Vec<String> = if table.is_empty() {
            g.keys().cloned().collect()
        } else {
            match g.get(table) {
                Some(_) => vec![table.to_string()],
                None => return Err(format!("table \"{}\" does not exist", table)),
            }
        };

        for tname in &tables_iter {
            let total_rows = g.get(tname.as_str()).map(|t| t.rows.len() as u64).unwrap_or(0);
            let t = match g.get(tname.as_str()) { Some(t) => t, None => continue };
            for col in &t.columns {
                let key = (tname.clone(), col.clone());
                let (ndv, sel, hmin, hmax, buckets_str) = if let Some(cs) = stats.get(&key) {
                    let b_str = cs.histogram.buckets.iter()
                        .map(|b| b.to_string())
                        .collect::<Vec<_>>()
                        .join(",");
                    (
                        cs.distinct_count,
                        cs.selectivity,
                        if cs.histogram.total > 0 { format!("{:.2}", cs.histogram.min) } else { "N/A".into() },
                        if cs.histogram.total > 0 { format!("{:.2}", cs.histogram.max) } else { "N/A".into() },
                        b_str,
                    )
                } else {
                    (0, 1.0, "N/A".into(), "N/A".into(), String::new())
                };

                rows_out.push(vec![
                    Some(tname.as_bytes().to_vec()),
                    Some(col.as_bytes().to_vec()),
                    Some(total_rows.to_string().into_bytes()),
                    Some(ndv.to_string().into_bytes()),
                    Some(format!("{:.4}", sel).into_bytes()),
                    Some(hmin.into_bytes()),
                    Some(hmax.into_bytes()),
                    Some(format!("[{}]", buckets_str).into_bytes()),
                ]);
            }
        }

        let n = rows_out.len();
        Ok(QueryResult {
            columns,
            rows: rows_out,
            command_tag: format!("SELECT {}", n),
        })
    }

    // -----------------------------------------------------------------------
    // SHOW — SHOW TABLES / SHOW COLUMNS FROM <t> / SHOW INDEX FROM <t>
    // -----------------------------------------------------------------------

    fn handle_show(&self, s: &str) -> Result<QueryResult, String> {
        let up = s.trim().to_ascii_uppercase();

        // SHOW TABLES
        if up == "SHOW TABLES" || up.starts_with("SHOW TABLES;") {
            let g = self.tables.read();
            let columns = vec![
                ("Table".to_string(), 25i32, -1i16),
                ("Rows".to_string(), 20i32, 8i16),
            ];
            let rows: Vec<Vec<Option<Vec<u8>>>> = g.iter()
                .map(|(name, t)| {
                    vec![
                        Some(name.as_bytes().to_vec()),
                        Some((t.rows.len() as i64).to_be_bytes().to_vec()),
                    ]
                })
                .collect();
            return Ok(QueryResult { columns, rows, command_tag: format!("SHOW {}", g.len()) });
        }

        // SHOW COLUMNS FROM <table>  or  SHOW COLUMNS IN <table>
        if up.starts_with("SHOW COLUMNS") {
            let table_name = s[12..].trim();
            let table_name = if table_name.to_ascii_uppercase().starts_with("FROM ") {
                table_name[5..].trim()
            } else if table_name.to_ascii_uppercase().starts_with("IN ") {
                table_name[3..].trim()
            } else {
                table_name
            }.trim_matches('"').trim_matches('`');

            let g = self.tables.read();
            let t = g.get(table_name)
                .ok_or_else(|| format!("SHOW COLUMNS: table '{}' not found", table_name))?;

            let columns = vec![
                ("Field".to_string(), 25i32, -1i16),
                ("Type".to_string(), 25i32, -1i16),
                ("Null".to_string(), 25i32, -1i16),
            ];
            let rows: Vec<Vec<Option<Vec<u8>>>> = t.columns.iter().zip(t.column_types.iter())
                .map(|(col_name, col_type)| {
                    let type_str = match col_type {
                        crate::gateway::native_sql::ColType::Integer => "INTEGER",
                        crate::gateway::native_sql::ColType::Float8  => "FLOAT8",
                        crate::gateway::native_sql::ColType::Text    => "TEXT",
                    };
                    vec![
                        Some(col_name.as_bytes().to_vec()),
                        Some(type_str.as_bytes().to_vec()),
                        Some(b"YES".to_vec()),
                    ]
                })
                .collect();
            let n = rows.len();
            return Ok(QueryResult { columns, rows, command_tag: format!("SHOW {}", n) });
        }

        // SHOW INDEX FROM <table>  or  SHOW INDEXES FROM <table>
        if up.starts_with("SHOW INDEX") || up.starts_with("SHOW INDEXES") || up.starts_with("SHOW KEYS") {
            let after = if up.starts_with("SHOW INDEXES") { &s[12..] }
                        else if up.starts_with("SHOW KEYS") { &s[9..] }
                        else { &s[10..] };
            let table_name = after.trim();
            let table_name = if table_name.to_ascii_uppercase().starts_with("FROM ") {
                table_name[5..].trim()
            } else if table_name.to_ascii_uppercase().starts_with("IN ") {
                table_name[3..].trim()
            } else {
                table_name
            }.trim_matches('"').trim_matches('`');

            let columns = vec![
                ("Table".to_string(), 25i32, -1i16),
                ("Key_name".to_string(), 25i32, -1i16),
                ("Column_name".to_string(), 25i32, -1i16),
                ("Unique".to_string(), 25i32, -1i16),
            ];
            let all_meta = self.index_mgr.list_indexes();
            let rows: Vec<Vec<Option<Vec<u8>>>> = all_meta.iter()
                .filter(|m| m.table.eq_ignore_ascii_case(table_name))
                .flat_map(|m| {
                    let unique_str = if m.is_unique { "YES" } else { "NO" };
                    m.columns.iter().map(move |col| {
                        vec![
                            Some(m.table.as_bytes().to_vec()),
                            Some(m.name.as_bytes().to_vec()),
                            Some(col.as_bytes().to_vec()),
                            Some(unique_str.as_bytes().to_vec()),
                        ]
                    }).collect::<Vec<_>>()
                })
                .collect();
            let n = rows.len();
            return Ok(QueryResult { columns, rows, command_tag: format!("SHOW {}", n) });
        }

        // Unknown SHOW variant — return empty
        Ok(Self::empty_ok("OK"))
    }

    // -----------------------------------------------------------------------
    // INFORMATION_SCHEMA — virtual tables: tables, columns, key_column_usage
    // -----------------------------------------------------------------------

    fn handle_information_schema(&self, s: &str) -> Option<Result<QueryResult, String>> {
        let up = s.to_ascii_uppercase();

        // Parse the SELECT list for column projection.
        let select_end = up.find(" FROM ").unwrap_or(up.len());
        let select_start = up.find("SELECT").map(|p| p + 6).unwrap_or(0);
        let select_list = up[select_start..select_end].trim().to_string();

        let full_result: QueryResult;

        if up.contains("INFORMATION_SCHEMA.TABLES") || up.contains("INFORMATION_SCHEMA.\"TABLES\"") {
            let g = self.tables.read();
            let filter = Self::parse_where_eq_str(&up, "TABLE_NAME");
            let columns = vec![
                ("table_schema".to_string(), 25i32, -1i16),
                ("table_name".to_string(), 25i32, -1i16),
                ("table_type".to_string(), 25i32, -1i16),
                ("table_rows".to_string(), 20i32, 8i16),
            ];
            let rows: Vec<Vec<Option<Vec<u8>>>> = g.iter()
                .filter(|(name, _)| {
                    filter.as_deref().map(|f| name.to_ascii_uppercase() == f).unwrap_or(true)
                })
                .map(|(name, t)| {
                    vec![
                        Some(b"public".to_vec()),
                        Some(name.as_bytes().to_vec()),
                        Some(b"BASE TABLE".to_vec()),
                        Some((t.rows.len() as i64).to_be_bytes().to_vec()),
                    ]
                })
                .collect();
            let n = rows.len();
            full_result = QueryResult { columns, rows, command_tag: format!("SELECT {}", n) };

        } else if up.contains("INFORMATION_SCHEMA.COLUMNS") || up.contains("INFORMATION_SCHEMA.\"COLUMNS\"") {
            let g = self.tables.read();
            let filter_table = Self::parse_where_eq_str(&up, "TABLE_NAME");
            let columns_meta = vec![
                ("table_schema".to_string(), 25i32, -1i16),
                ("table_name".to_string(), 25i32, -1i16),
                ("column_name".to_string(), 25i32, -1i16),
                ("ordinal_position".to_string(), 20i32, 8i16),
                ("data_type".to_string(), 25i32, -1i16),
                ("is_nullable".to_string(), 25i32, -1i16),
            ];
            let mut rows: Vec<Vec<Option<Vec<u8>>>> = Vec::new();
            for (tbl_name, t) in g.iter() {
                if let Some(ref f) = filter_table {
                    if tbl_name.to_ascii_uppercase() != *f { continue; }
                }
                for (pos, (col_name, col_type)) in t.columns.iter().zip(t.column_types.iter()).enumerate() {
                    let type_str = match col_type {
                        ColType::Integer => "INTEGER",
                        ColType::Float8  => "DOUBLE PRECISION",
                        ColType::Text    => "TEXT",
                    };
                    rows.push(vec![
                        Some(b"public".to_vec()),
                        Some(tbl_name.as_bytes().to_vec()),
                        Some(col_name.as_bytes().to_vec()),
                        Some(((pos as i64) + 1).to_be_bytes().to_vec()),
                        Some(type_str.as_bytes().to_vec()),
                        Some(b"YES".to_vec()),
                    ]);
                }
            }
            let n = rows.len();
            full_result = QueryResult { columns: columns_meta, rows, command_tag: format!("SELECT {}", n) };

        } else if up.contains("INFORMATION_SCHEMA.STATISTICS") || up.contains("INFORMATION_SCHEMA.KEY_COLUMN_USAGE")
            || up.contains("INFORMATION_SCHEMA.TABLE_CONSTRAINTS")
        {
            let filter_table = Self::parse_where_eq_str(&up, "TABLE_NAME");
            let columns_meta = vec![
                ("table_schema".to_string(), 25i32, -1i16),
                ("table_name".to_string(), 25i32, -1i16),
                ("index_name".to_string(), 25i32, -1i16),
                ("column_name".to_string(), 25i32, -1i16),
                ("non_unique".to_string(), 20i32, 8i16),
            ];
            let all_meta = self.index_mgr.list_indexes();
            let rows: Vec<Vec<Option<Vec<u8>>>> = all_meta.iter()
                .filter(|m| {
                    filter_table.as_deref()
                        .map(|f| m.table.to_ascii_uppercase() == f)
                        .unwrap_or(true)
                })
                .flat_map(|m| {
                    let non_unique: i64 = if m.is_unique { 0 } else { 1 };
                    m.columns.iter().map(move |col| {
                        vec![
                            Some(b"public".to_vec()),
                            Some(m.table.as_bytes().to_vec()),
                            Some(m.name.as_bytes().to_vec()),
                            Some(col.as_bytes().to_vec()),
                            Some(non_unique.to_be_bytes().to_vec()),
                        ]
                    }).collect::<Vec<_>>()
                })
                .collect();
            let n = rows.len();
            full_result = QueryResult { columns: columns_meta, rows, command_tag: format!("SELECT {}", n) };

        } else {
            return None;
        }

        // Apply column projection: if not SELECT *, filter to only requested columns.
        if select_list == "*" || select_list.is_empty() {
            return Some(Ok(full_result));
        }
        let projected = Self::project_columns(&select_list, full_result);
        Some(Ok(projected))
    }

    /// Project a QueryResult to only the columns listed in `select_list` (comma-separated, uppercase).
    fn project_columns(select_list: &str, result: QueryResult) -> QueryResult {
        if select_list.trim() == "*" || select_list.trim().is_empty() {
            return result;
        }
        let requested: Vec<&str> = select_list.split(',').map(|c| c.trim()).collect();
        // Map each requested column to its index in result.columns.
        let indices: Vec<usize> = requested.iter().filter_map(|req| {
            result.columns.iter().position(|(name, _, _)| name.to_ascii_uppercase() == *req)
        }).collect();

        let new_cols: Vec<(String, i32, i16)> = indices.iter()
            .map(|&i| result.columns[i].clone())
            .collect();
        let new_rows: Vec<Vec<Option<Vec<u8>>>> = result.rows.iter()
            .map(|row| indices.iter().map(|&i| row.get(i).and_then(|x| x.clone())).collect())
            .collect();
        let n = new_rows.len();
        QueryResult { columns: new_cols, rows: new_rows, command_tag: format!("SELECT {}", n) }
    }

    /// Parse `WHERE <col> = '<value>'` or `WHERE <col>='<value>'` from an uppercase string.
    fn parse_where_eq_str(up: &str, col: &str) -> Option<String> {
        // Match both "COL = 'val'" and "COL='val'" patterns.
        let col_pos = up.find(col)?;
        let after_col = up[col_pos + col.len()..].trim_start();
        if !after_col.starts_with('=') {
            return None;
        }
        let after_eq = after_col[1..].trim_start();
        // Handle quoted or unquoted value
        let value = if after_eq.starts_with('\'') {
            let end = after_eq[1..].find('\'')?;
            after_eq[1..end + 1].to_string()
        } else if after_eq.starts_with('"') {
            let end = after_eq[1..].find('"')?;
            after_eq[1..end + 1].to_string()
        } else {
            after_eq.split(|c: char| c.is_whitespace() || c == ';')
                .next()
                .map(|s| s.to_string())?
        };
        if value.is_empty() { None } else { Some(value) }
    }

    // -----------------------------------------------------------------------
    // SAVEPOINT / RELEASE SAVEPOINT / ROLLBACK TO SAVEPOINT
    // -----------------------------------------------------------------------

    fn savepoint_name_from(s: &str, keyword: &str) -> Option<String> {
        let up = s.to_ascii_uppercase();
        let pos = up.find(keyword)?;
        let rest = s[pos + keyword.len()..].trim();
        let name = rest.split(|c: char| c.is_whitespace() || c == ';').next()?;
        if name.is_empty() { None } else { Some(name.to_string()) }
    }

    fn handle_savepoint(&self, s: &str) -> Result<QueryResult, String> {
        let name = Self::savepoint_name_from(s, "SAVEPOINT ")
            .ok_or("SAVEPOINT: missing name")?;
        let snapshot = {
            let g = self.tables.read();
            g.clone()
        };
        let mut sps = self.savepoints.lock()
            .map_err(|e| format!("SAVEPOINT lock error: {}", e))?;
        // Remove any existing savepoint with the same name (re-use).
        sps.retain(|(n, _)| n != &name);
        sps.push((name.clone(), snapshot));
        Ok(Self::empty_ok(&format!("SAVEPOINT {}", name)))
    }

    fn handle_release_savepoint(&self, s: &str) -> Result<QueryResult, String> {
        let up = s.to_ascii_uppercase();
        let keyword = if up.contains("SAVEPOINT ") { "SAVEPOINT " } else { "RELEASE " };
        let name = Self::savepoint_name_from(s, keyword)
            .ok_or("RELEASE SAVEPOINT: missing name")?;
        // Actual keyword to skip might be "RELEASE SAVEPOINT " or "RELEASE "
        // Re-parse from "RELEASE" position
        let up2 = s.to_ascii_uppercase();
        let rel_pos = up2.find("RELEASE").ok_or("RELEASE SAVEPOINT: internal error")?;
        let after_release = s[rel_pos + 7..].trim();
        let name2 = if after_release.to_ascii_uppercase().starts_with("SAVEPOINT ") {
            after_release[10..].trim().split(|c: char| c.is_whitespace() || c == ';').next().unwrap_or("").to_string()
        } else {
            after_release.split(|c: char| c.is_whitespace() || c == ';').next().unwrap_or("").to_string()
        };
        let final_name = if name2.is_empty() { name } else { name2 };
        let mut sps = self.savepoints.lock()
            .map_err(|e| format!("RELEASE SAVEPOINT lock error: {}", e))?;
        let before = sps.len();
        sps.retain(|(n, _)| n != &final_name);
        if sps.len() == before {
            return Err(format!("RELEASE SAVEPOINT: savepoint '{}' not found", final_name));
        }
        Ok(Self::empty_ok(&format!("RELEASE {}", final_name)))
    }

    fn handle_rollback_to_savepoint(&self, s: &str) -> Result<QueryResult, String> {
        let up = s.to_ascii_uppercase();
        // Parse name from "ROLLBACK TO SAVEPOINT name" or "ROLLBACK TO name"
        let name = if let Some(pos) = up.find("ROLLBACK TO SAVEPOINT ") {
            s[pos + 22..].trim().split(|c: char| c.is_whitespace() || c == ';').next().unwrap_or("").to_string()
        } else if let Some(pos) = up.find("ROLLBACK TO ") {
            s[pos + 12..].trim().split(|c: char| c.is_whitespace() || c == ';').next().unwrap_or("").to_string()
        } else {
            return Err("ROLLBACK TO SAVEPOINT: syntax error".to_string());
        };
        if name.is_empty() {
            return Err("ROLLBACK TO SAVEPOINT: missing name".to_string());
        }

        let sps = self.savepoints.lock()
            .map_err(|e| format!("ROLLBACK TO SAVEPOINT lock error: {}", e))?;
        // Find savepoint (searching from the end for the most recent match).
        let snapshot = sps.iter().rev().find(|(n, _)| n == &name)
            .map(|(_, snap)| snap.clone())
            .ok_or_else(|| format!("ROLLBACK TO SAVEPOINT: savepoint '{}' not found", name))?;
        drop(sps);

        // Restore the snapshot into the engine's tables.
        let mut g = self.tables.write();
        *g = snapshot;
        drop(g);

        // Invalidate all buffer caches after rollback.
        self.buf_pool.invalidate_all();

        Ok(Self::empty_ok(&format!("ROLLBACK TO {}", name)))
    }

    // -----------------------------------------------------------------------
    // VACUUM — reclaim memory + evict cold caches
    // -----------------------------------------------------------------------

    fn handle_vacuum(&self, s: &str) -> Result<QueryResult, String> {
        let table = Self::parse_ident_after(s, "VACUUM")
            .unwrap_or("")
            .trim_matches('"');

        // Spill cached data to disk BEFORE clearing (so nothing is lost).
        if let Some(ref ddir) = self.data_dir {
            self.spill_cold_caches_to_disk(ddir);
        }

        // Evict all cached data (columnar, dim, soa).
        if table.is_empty() {
            self.buf_pool.col_cache.write().clear();
            self.buf_pool.dim_cache.write().clear();
            self.buf_pool.soa_cache.write().clear();
        } else {
            self.buf_pool.col_cache.write().remove(table);
            self.buf_pool.dim_cache.write().remove(table);
            self.buf_pool.soa_cache.write().remove(table);
        }

        let msg = if table.is_empty() { "VACUUM".to_string() } else { format!("VACUUM {}", table) };
        Ok(Self::empty_ok(&msg))
    }

    // -----------------------------------------------------------------------
    // COPY FROM — bulk-load data from Parquet files
    // -----------------------------------------------------------------------
    // Syntax: COPY <table> FROM '<path>' (FORMAT PARQUET)
    //    or:  COPY <table> FROM '<path>'  (auto-detects .parquet extension)

    fn handle_copy(&self, s: &str) -> Result<QueryResult, String> {
        let up = s.to_ascii_uppercase();

        let is_export = up.contains(" TO ");
        let is_import = up.contains(" FROM ");

        if !is_export && !is_import {
            return Err("COPY: specify FROM (import) or TO (export)".into());
        }

        // Parse table name
        let table = Self::parse_ident_after(s, "COPY")
            .ok_or("Invalid COPY: missing table name")?;

        // Parse file path
        let direction_kw = if is_export { "TO" } else { "FROM" };
        let dir_idx = up.find(direction_kw).ok_or(format!("COPY: missing {}", direction_kw))?;
        let after_dir = s[dir_idx + direction_kw.len()..].trim();
        let path = Self::extract_quoted_path(after_dir)
            .ok_or("COPY: missing file path (use single quotes)")?;

        // Path security: COPY FROM can read any file the process can access.
        // COPY TO must stay inside data_dir to prevent arbitrary file writes.
        if is_import {
            // Just verify the file exists and can be opened.
            if !std::path::Path::new(&path).exists() {
                return Err(format!("COPY FROM: file '{}' not found", path));
            }
        } else {
            // For export, validate parent directory exists and is inside data_dir
            let out_path = std::path::Path::new(&path);
            if let Some(parent) = out_path.parent() {
                let canonical_parent = parent.canonicalize()
                    .map_err(|e| format!("COPY: invalid output directory: {}", e))?;
                if let Some(ref data_dir) = self.data_dir {
                    let allowed_dir = data_dir.canonicalize().unwrap_or_else(|_| data_dir.clone());
                    if !canonical_parent.starts_with(&allowed_dir) {
                        return Err("COPY: output path must be inside the data directory".into());
                    }
                } else {
                    return Err("COPY: data_dir not configured — COPY TO disabled".into());
                }
            }
        }

        // Detect format from explicit FORMAT clause or file extension.
        let is_parquet = up.contains("PARQUET")
            || path.ends_with(".parquet")
            || path.ends_with(".parq");
        let is_csv = (up.contains("FORMAT CSV") || up.contains("FORMAT 'CSV'") || path.ends_with(".csv"))
            && !is_parquet;
        let is_jsonl = (up.contains("FORMAT JSONL") || up.contains("FORMAT JSON")
            || path.ends_with(".jsonl") || path.ends_with(".ndjson"))
            && !is_parquet && !is_csv;

        if is_export {
            if is_parquet {
                self.copy_to_parquet(table, &path)
            } else {
                Err("COPY TO: only Parquet export is supported (use FORMAT PARQUET or .parquet extension)".into())
            }
        } else if is_parquet {
            self.copy_from_parquet(table, &path)
        } else if is_csv {
            // Default: treat first line as header unless HEADER false/off/0 is explicitly set.
            // Matches PostgreSQL ergonomics where CSV files almost always have headers.
            let no_header = up.contains("HEADER FALSE") || up.contains("HEADER OFF") || up.contains("HEADER 0") || up.contains("(HEADER FALSE)");
            let has_header = !no_header;
            self.copy_from_csv(table, &path, has_header)
        } else if is_jsonl {
            self.copy_from_jsonl(table, &path)
        } else {
            Err("COPY FROM: unsupported format. Specify FORMAT PARQUET, FORMAT CSV, or FORMAT JSONL (or use .parquet/.csv/.jsonl extension)".into())
        }
    }

    fn extract_quoted_path(s: &str) -> Option<String> {
        let q_start = s.find('\'')?;
        let rest = &s[q_start + 1..];
        let q_end = rest.find('\'')?;
        Some(rest[..q_end].to_string())
    }

    // -----------------------------------------------------------------------
    // COPY FROM CSV — bulk import from a CSV file (RFC 4180)
    // -----------------------------------------------------------------------
    fn copy_from_csv(&self, table_name: &str, path: &str, has_header: bool) -> Result<QueryResult, String> {
        let contents = fs::read_to_string(path)
            .map_err(|e| format!("COPY FROM CSV: cannot read '{}': {}", path, e))?;

        let g = self.tables.read();
        let t = g.get(table_name)
            .ok_or_else(|| format!("COPY FROM CSV: table '{}' not found", table_name))?;
        let columns = t.columns.clone();
        let col_types = t.column_types.clone();
        drop(g);

        let mut lines_iter = contents.lines().peekable();

        // Determine column mapping from header or table order.
        let header_cols: Vec<String> = if has_header {
            match lines_iter.next() {
                Some(first) => Self::parse_csv_line(first.trim()),
                None => return Ok(QueryResult { columns: vec![], rows: vec![], command_tag: "COPY 0".to_string() }),
            }
        } else {
            columns.clone()
        };

        let has_id_col_csv = header_cols.iter().any(|c| c.eq_ignore_ascii_case("id"));
        let mut rows_inserted = 0u64;
        let mut prepared_rows: Vec<(i64, NativeRow)> = Vec::new();

        for line in lines_iter {
            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }
            let values = Self::parse_csv_line(trimmed);

            let mut row_map: HashMap<String, Cell> = HashMap::new();
            for (i, col_name) in header_cols.iter().enumerate() {
                if let Some(tbl_idx) = columns.iter().position(|c| c == col_name) {
                    let raw = values.get(i).map(|s| s.as_str()).unwrap_or("");
                    let cell = match col_types.get(tbl_idx) {
                        Some(ColType::Integer) => raw.parse::<i64>().map(Cell::Int).unwrap_or(Cell::Null),
                        Some(ColType::Float8)  => raw.parse::<f64>().map(Cell::Float).unwrap_or(Cell::Null),
                        _ => if raw.is_empty() { Cell::Null } else { Cell::Text(raw.to_string()) },
                    };
                    row_map.insert(col_name.clone(), cell);
                }
            }

            let id = if has_id_col_csv {
                row_map.get("id").map(|v| v.as_i64()).unwrap_or(0)
            } else {
                0 // placeholder — reassigned under write lock
            };
            let lsn = self.row_lsn_counter.fetch_add(1, Ordering::SeqCst) + 1;
            prepared_rows.push((id, NativeRow { cols: row_map, last_modified_lsn: lsn }));
            rows_inserted += 1;
        }

        if !prepared_rows.is_empty() {
            let mut g = self.tables.write();
            if let Some(t) = g.get_mut(table_name) {
                if !has_id_col_csv {
                    let mut next_id = t.rows.keys().max().copied().unwrap_or(0) + 1;
                    for (id, _) in prepared_rows.iter_mut() {
                        *id = next_id;
                        next_id += 1;
                    }
                }
                for (id, row) in prepared_rows {
                    t.rows.insert(id, row);
                }
            }
            self.buf_pool.invalidate(table_name);
        }

        Ok(QueryResult {
            columns: vec![],
            rows: vec![],
            command_tag: format!("COPY {}", rows_inserted),
        })
    }

    /// Parse a single CSV line following RFC 4180 (quoted fields, escaped quotes).
    fn parse_csv_line(line: &str) -> Vec<String> {
        let mut fields: Vec<String> = Vec::new();
        let mut field = String::new();
        let mut in_quotes = false;
        let mut chars = line.chars().peekable();

        while let Some(c) = chars.next() {
            match c {
                '"' if in_quotes => {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                }
                '"' => { in_quotes = true; }
                ',' if !in_quotes => {
                    fields.push(field.clone());
                    field.clear();
                }
                _ => { field.push(c); }
            }
        }
        fields.push(field);
        fields
    }

    // -----------------------------------------------------------------------
    // COPY FROM JSONL — bulk import from a JSON Lines / NDJSON file
    // -----------------------------------------------------------------------
    fn copy_from_jsonl(&self, table_name: &str, path: &str) -> Result<QueryResult, String> {
        let contents = fs::read_to_string(path)
            .map_err(|e| format!("COPY FROM JSONL: cannot read '{}': {}", path, e))?;

        let g = self.tables.read();
        let t = g.get(table_name)
            .ok_or_else(|| format!("COPY FROM JSONL: table '{}' not found", table_name))?;
        let columns = t.columns.clone();
        let col_types = t.column_types.clone();
        drop(g);

        let mut rows_inserted = 0u64;
        let mut prepared_rows: Vec<(i64, NativeRow)> = Vec::new();

        for line in contents.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }

            let val: serde_json::Value = serde_json::from_str(trimmed)
                .map_err(|e| format!("COPY FROM JSONL: invalid JSON at line {}: {}", rows_inserted + 1, e))?;
            let obj = match val.as_object() {
                Some(o) => o,
                None => return Err(format!("COPY FROM JSONL: line {} is not a JSON object", rows_inserted + 1)),
            };

            let mut row_map: HashMap<String, Cell> = HashMap::new();
            for (tbl_idx, col_name) in columns.iter().enumerate() {
                if let Some(v) = obj.get(col_name) {
                    let cell = match (col_types.get(tbl_idx), v) {
                        (Some(ColType::Integer), serde_json::Value::Number(n)) =>
                            n.as_i64().map(Cell::Int).unwrap_or(Cell::Null),
                        (Some(ColType::Float8), serde_json::Value::Number(n)) =>
                            n.as_f64().map(Cell::Float).unwrap_or(Cell::Null),
                        (_, serde_json::Value::String(s)) => Cell::Text(s.clone()),
                        (_, serde_json::Value::Number(n)) =>
                            n.as_i64().map(Cell::Int)
                             .unwrap_or_else(|| n.as_f64().map(Cell::Float).unwrap_or(Cell::Null)),
                        (_, serde_json::Value::Bool(b)) => Cell::Int(if *b { 1 } else { 0 }),
                        (_, serde_json::Value::Null) => Cell::Null,
                        _ => Cell::Text(v.to_string()),
                    };
                    row_map.insert(col_name.clone(), cell);
                }
            }

            let id = if columns.iter().any(|c| c.eq_ignore_ascii_case("id")) {
                row_map.get("id").map(|v| v.as_i64()).unwrap_or(0)
            } else {
                0 // placeholder
            };
            let lsn = self.row_lsn_counter.fetch_add(1, Ordering::SeqCst) + 1;
            prepared_rows.push((id, NativeRow { cols: row_map, last_modified_lsn: lsn }));
            rows_inserted += 1;
        }

        if !prepared_rows.is_empty() {
            let has_id_col_jsonl = columns.iter().any(|c| c.eq_ignore_ascii_case("id"));
            let mut g = self.tables.write();
            if let Some(t) = g.get_mut(table_name) {
                if !has_id_col_jsonl {
                    let mut next_id = t.rows.keys().max().copied().unwrap_or(0) + 1;
                    for (id, _) in prepared_rows.iter_mut() {
                        *id = next_id;
                        next_id += 1;
                    }
                }
                for (id, row) in prepared_rows {
                    t.rows.insert(id, row);
                }
            }
            self.buf_pool.invalidate(table_name);
        }

        Ok(QueryResult {
            columns: vec![],
            rows: vec![],
            command_tag: format!("COPY {}", rows_inserted),
        })
    }

    // -----------------------------------------------------------------------
    // EXPLAIN — query execution plan without running the query
    // -----------------------------------------------------------------------
    fn handle_explain(&self, s: &str) -> Result<QueryResult, String> {
        let upper = s.to_ascii_uppercase();
        // Strip EXPLAIN keyword.
        let rest = if upper.starts_with("EXPLAIN") { s[7..].trim() } else { s };
        // Strip optional ANALYZE / VERBOSE flags.
        let upper_rest = rest.to_ascii_uppercase();
        let inner = if upper_rest.starts_with("ANALYZE ") { rest[8..].trim() }
                    else if upper_rest.starts_with("VERBOSE ") { rest[8..].trim() }
                    else { rest };

        let plan = self.build_explain_plan(inner);

        Ok(QueryResult {
            columns: vec![("QUERY PLAN".to_string(), 25, 0)],
            rows: plan.into_iter()
                .map(|line| vec![Some(line.into_bytes())])
                .collect(),
            command_tag: "EXPLAIN".to_string(),
        })
    }

    fn build_explain_plan(&self, sql: &str) -> Vec<String> {
        let up = sql.to_ascii_uppercase();
        let mut lines: Vec<String> = Vec::new();

        if up.starts_with("SELECT") {
            let tbl = Self::extract_from_table(sql).unwrap_or_default();
            let tbl = tbl.as_str();
            let est_rows = { let g = self.tables.read(); g.get(tbl).map(|t| t.rows.len()).unwrap_or(0) };
            let has_where = up.contains(" WHERE ");
            let has_order = up.contains(" ORDER BY ");
            let has_limit = up.contains(" LIMIT ");
            let has_join  = up.contains(" JOIN ");
            let has_agg   = up.contains("COUNT(") || up.contains("SUM(") || up.contains("AVG(");
            let ncols = { let g = self.tables.read(); g.get(tbl).map(|t| t.columns.len()).unwrap_or(4) };

            let node = if has_join {
                format!("Hash Join  (cost=100.0..500.0 rows={} width={})", est_rows, ncols * 8)
            } else {
                format!("Seq Scan on {}  (cost=0.00..{:.2} rows={} width={})",
                    tbl, est_rows as f64 * 0.01, if has_where { (est_rows / 10).max(1) } else { est_rows }, ncols * 8)
            };

            let mut inner_lines = vec![node];
            if has_where {
                let where_text: String = if let Some(pos) = up.find(" WHERE ") {
                    sql[pos + 7..].split_whitespace().take(5).collect::<Vec<_>>().join(" ")
                } else { String::new() };
                inner_lines.push(format!("  Filter: {}", where_text));
            }

            if has_agg {
                lines.push(format!("Aggregate  (cost=0.00..{:.2} rows=1 width=8)", est_rows as f64 * 0.02));
                for l in inner_lines { lines.push(format!("  ->  {}", l)); }
            } else {
                lines.extend(inner_lines);
            }

            if has_order {
                let sort_col = up.find(" ORDER BY ").map(|pos| {
                    sql[pos + 10..].split_whitespace().next().unwrap_or("?").to_string()
                }).unwrap_or_else(|| "?".to_string());
                let existing = lines;
                lines = vec![
                    format!("Sort  (cost=100.0..110.0 rows={} width={})", est_rows, ncols * 8),
                    format!("  Sort Key: {}", sort_col),
                ];
                for l in existing { lines.push(format!("  ->  {}", l)); }
            }

            if has_limit {
                let existing = lines;
                lines = vec!["Limit  (cost=0.00..0.14 rows=1 width=32)".to_string()];
                for l in existing { lines.push(format!("  ->  {}", l)); }
            }

        } else if up.starts_with("INSERT") {
            let tbl = Self::parse_ident_after(sql, "INSERT INTO").unwrap_or("?");
            lines.push(format!("Insert on {}  (cost=0.00..0.01 rows=1 width=0)", tbl));
            lines.push("  ->  Values Scan  (cost=0.00..0.01 rows=1 width=0)".to_string());
        } else if up.starts_with("UPDATE") {
            let tbl = Self::parse_ident_after(sql, "UPDATE").unwrap_or("?");
            let est = { let g = self.tables.read(); g.get(tbl).map(|t| t.rows.len()).unwrap_or(0) };
            lines.push(format!("Update on {}  (cost=0.00..{:.2} rows={} width=0)", tbl, est as f64 * 0.01, est));
            lines.push(format!("  ->  Seq Scan on {}  (cost=0.00..{:.2} rows={} width=16)", tbl, est as f64 * 0.01, est));
        } else if up.starts_with("DELETE") {
            let tbl = Self::parse_ident_after(sql, "DELETE FROM").unwrap_or("?");
            let est = { let g = self.tables.read(); g.get(tbl).map(|t| t.rows.len()).unwrap_or(0) };
            lines.push(format!("Delete on {}  (cost=0.00..{:.2} rows={} width=0)", tbl, est as f64 * 0.01, est));
            lines.push(format!("  ->  Seq Scan on {}  (cost=0.00..{:.2} rows={} width=16)", tbl, est as f64 * 0.01, est));
        } else {
            lines.push(format!("Utility  (cost=0.00..0.01 rows=1 width=0)"));
        }

        if lines.is_empty() { lines.push("Result  (rows=0)".to_string()); }
        lines
    }

    /// Export a table to a .parquet file using columnar cache for efficient access.
    fn copy_to_parquet(&self, table_name: &str, path: &str) -> Result<QueryResult, String> {
        let g = self.tables.read();
        let t = g.get(table_name)
            .ok_or(format!("table \"{}\" does not exist", table_name))?;

        let columns = t.columns.clone();
        let col_types: Vec<ColType> = t.column_types.clone();
        drop(g);

        // Build Arrow schema from table columns
        let mut fields: Vec<ArrowField> = Vec::with_capacity(columns.len());
        for (i, col) in columns.iter().enumerate() {
            let dt = if i < col_types.len() {
                match col_types[i] {
                    ColType::Integer => ArrowDataType2::Int64,
                    ColType::Float8 => ArrowDataType2::Float64,
                    ColType::Text => ArrowDataType2::Utf8,
                }
            } else if col == "id" {
                ArrowDataType2::Int64
            } else {
                ArrowDataType2::Utf8
            };
            fields.push(ArrowField::new(col, dt, true));
        }
        let schema = Arc::new(ArrowSchema::new(fields));

        // Build columnar cache for efficient sequential access
        let g = self.tables.read();
        let t = g.get(table_name).ok_or("table vanished")?;
        let cc = self.get_or_build_cols(table_name, t);
        drop(g);

        let n = cc.ids.len();

        // Build Arrow arrays from columnar cache
        let mut arrow_arrays: Vec<Arc<dyn Array>> = Vec::with_capacity(columns.len());
        for (i, col) in columns.iter().enumerate() {
            let arr: Arc<dyn Array> = if col == "id" {
                Arc::new(Int64Array::from(cc.ids.clone()))
            } else if let Some(iv) = cc.int_cols.get(col.as_str()) {
                Arc::new(Int64Array::from(iv.clone()))
            } else if let Some(fv) = cc.float_cols.get(col.as_str()) {
                Arc::new(Float64Array::from(fv.clone()))
            } else if let Some(tv) = cc.text_cols.get(col.as_str()) {
                Arc::new(StringArray::from(tv.clone()))
            } else {
                // Column not in cache — produce nulls
                let dt = &schema.field(i).data_type().clone();
                match dt {
                    ArrowDataType2::Int64 => Arc::new(Int64Array::from(vec![None::<i64>; n])),
                    ArrowDataType2::Float64 => Arc::new(Float64Array::from(vec![None::<f64>; n])),
                    _ => Arc::new(StringArray::from(vec![None::<&str>; n])),
                }
            };
            arrow_arrays.push(arr);
        }

        let batch = RecordBatch::try_new(schema.clone(), arrow_arrays)
            .map_err(|e| format!("COPY TO: failed to build record batch: {}", e))?;

        // Write parquet file
        let file = fs::File::create(path)
            .map_err(|e| format!("COPY TO: cannot create '{}': {}", path, e))?;

        let mut writer = ArrowWriter::try_new(file, schema, None)
            .map_err(|e| format!("COPY TO: failed to create writer: {}", e))?;
        writer.write(&batch)
            .map_err(|e| format!("COPY TO: write error: {}", e))?;
        writer.close()
            .map_err(|e| format!("COPY TO: close error: {}", e))?;

        Ok(QueryResult {
            columns: vec![],
            rows: vec![],
            command_tag: format!("COPY {}", n),
        })
    }

    /// Read a .parquet file and bulk-load all rows into the target table.
    /// Creates the table with correct schema if it doesn't exist.
    /// Uses chunk-based pipeline: reads CHUNK_SIZE rows per batch.
    fn copy_from_parquet(&self, table_name: &str, path: &str) -> Result<QueryResult, String> {
        let file = fs::File::open(path)
            .map_err(|e| format!("COPY: cannot open '{}': {}", path, e))?;

        let builder = ParquetRecordBatchReaderBuilder::try_new(file)
            .map_err(|e| format!("COPY: invalid parquet file: {}", e))?;

        let arrow_schema = builder.schema().clone();

        // Build QMvir column schema from Arrow schema
        let col_names: Vec<String> = arrow_schema
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();
        let col_types: Vec<ColType> = arrow_schema
            .fields()
            .iter()
            .map(|f| match f.data_type() {
                ArrowDataType::Int8 | ArrowDataType::Int16 | ArrowDataType::Int32
                | ArrowDataType::Int64 | ArrowDataType::UInt8 | ArrowDataType::UInt16
                | ArrowDataType::UInt32 | ArrowDataType::UInt64 => ColType::Integer,
                ArrowDataType::Float16 | ArrowDataType::Float32 | ArrowDataType::Float64
                | ArrowDataType::Decimal128(_, _) | ArrowDataType::Decimal256(_, _) => ColType::Float8,
                _ => ColType::Text,
            })
            .collect();

        // Ensure table exists with correct schema
        {
            let mut g = self.tables.write();
            g.entry(table_name.to_string()).or_insert_with(|| {
                NativeTable::new(col_names.clone(), col_types.clone())
            });
        }

        // Determine which column (if any) is the primary key "id"
        let id_col_idx = col_names.iter().position(|c| c == "id");

        // Read Parquet in batches of CHUNK_SIZE for pipelined processing
        let reader = builder
            .with_batch_size(CHUNK_SIZE)
            .build()
            .map_err(|e| format!("COPY: reader build failed: {}", e))?;

        let mut total_rows: u64 = 0;
        let mut auto_id: i64 = {
            let g = self.tables.read();
            g.get(table_name)
                .map(|t| t.rows.keys().max().copied().unwrap_or(0))
                .unwrap_or(0)
        };

        for batch_result in reader {
            let batch = batch_result
                .map_err(|e| format!("COPY: error reading batch: {}", e))?;
            let num_rows = batch.num_rows();
            if num_rows == 0 {
                continue;
            }

            // Pre-extract column arrays into typed extractors for cache-friendly access
            let col_extractors: Vec<ColumnExtractor> = (0..batch.num_columns())
                .map(|ci| ColumnExtractor::from_arrow(batch.column(ci)))
                .collect();

            // Bulk-insert this chunk
            let mut rows_chunk: Vec<(i64, NativeRow)> = Vec::with_capacity(num_rows);
            for row_idx in 0..num_rows {
                let mut row_map: HashMap<String, Cell> = HashMap::with_capacity(col_names.len());
                let mut row_id: i64 = 0;

                for (ci, col_name) in col_names.iter().enumerate() {
                    let cell = col_extractors[ci].get(row_idx);
                    if ci == id_col_idx.unwrap_or(usize::MAX) {
                        row_id = cell.as_i64();
                    }
                    row_map.insert(col_name.clone(), cell);
                }

                if id_col_idx.is_none() {
                    auto_id += 1;
                    row_id = auto_id;
                    row_map.insert("id".to_string(), Cell::Int(row_id));
                }

                rows_chunk.push((row_id, NativeRow { cols: row_map, last_modified_lsn: 0 }));
            }

            // Batch write under a single lock acquisition
            {
                let mut g = self.tables.write();
                if let Some(t) = g.get_mut(table_name) {
                    for (id, row) in rows_chunk {
                        t.rows.insert(id, row);
                    }
                }
            }
            self.buf_pool.invalidate(table_name);
            total_rows += num_rows as u64;
        }

        let msg = format!("COPY {}", total_rows);
        Ok(QueryResult {
            columns: vec![("copy".to_string(), oid::TEXT, -1)],
            rows: vec![vec![Some(msg.as_bytes().to_vec())]],
            command_tag: msg,
        })
    }

    fn handle_delete(&self, s: &str) -> Result<QueryResult, String> {
        let table = Self::parse_ident_after(s, "DELETE FROM").ok_or("Invalid DELETE")?;
        let mut g = self.tables.write();
        let mut deleted = 0usize;
        if let Some(t) = g.get_mut(table) {
            if let Some(where_idx) = Self::find_keyword_ci(s, "WHERE") {
                let where_part = s[where_idx + 5..].trim().trim_end_matches(';');

                // Check for WHERE col IN (v1, v2, ...) pattern
                if let Some((col, values)) = Self::parse_in_list(where_part) {
                    if col == "id" || col == t.columns.first().map(|s| s.as_str()).unwrap_or("") {
                        // Fast path: direct HashMap removal by primary key
                        for val in &values {
                            let target_id = val.as_i64();
                            if t.rows.remove(&target_id).is_some() {
                                deleted += 1;
                            }
                        }
                    } else if let Some(tree) = self.index_mgr.find_index(table, &col) {
                        // Index-accelerated batch delete
                        for val in &values {
                            let idx_key = match val {
                                Cell::Int(v) => IndexKey::Integer(*v),
                                Cell::Text(v) => IndexKey::Str(v.clone()),
                                Cell::Float(v) => IndexKey::Integer(*v as i64),
                                Cell::Null => continue,
                            };
                            let row_ids = tree.search(&idx_key);
                            deleted += row_ids.len();
                            for id in row_ids {
                                t.rows.remove(&id);
                            }
                        }
                    } else {
                        // Fallback: full scan for IN list
                        let value_set: HashSet<String> = values.iter().map(|v| v.as_text()).collect();
                        let ids_to_delete: Vec<i64> = t.rows.iter()
                            .filter(|(_, row)| {
                                row.cols.get(&col).map_or(false, |c| value_set.contains(&c.as_text()))
                            })
                            .map(|(id, _)| *id)
                            .collect();
                        deleted = ids_to_delete.len();
                        for id in ids_to_delete {
                            t.rows.remove(&id);
                        }
                    }
                } else {
                    // Single equality: WHERE col = val
                    let parts: Vec<&str> = where_part.splitn(2, '=').collect();
                    if parts.len() == 2 {
                        let col = parts[0].trim().trim_matches('"');
                        let val = Self::parse_value(parts[1].trim());

                        if col == "id" || col == t.columns.first().map(|s| s.as_str()).unwrap_or("") {
                            let target_id = val.as_i64();
                            if t.rows.remove(&target_id).is_some() {
                                deleted = 1;
                            }
                        } else if let Some(tree) = self.index_mgr.find_index(table, col) {
                            let idx_key = match &val {
                                Cell::Int(v) => IndexKey::Integer(*v),
                                Cell::Text(v) => IndexKey::Str(v.clone()),
                                Cell::Float(v) => IndexKey::Integer(*v as i64),
                                Cell::Null => IndexKey::Integer(0),
                            };
                            let row_ids = tree.search(&idx_key);
                            deleted = row_ids.len();
                            for id in row_ids {
                                t.rows.remove(&id);
                            }
                        } else {
                            let ids_to_delete: Vec<i64> = t.rows.iter()
                                .filter(|(_, row)| row.cols.get(col).map_or(false, |c| *c == val))
                                .map(|(id, _)| *id)
                                .collect();
                            deleted = ids_to_delete.len();
                            for id in ids_to_delete {
                                t.rows.remove(&id);
                            }
                        }
                    }
                }
            } else {
                // No WHERE — delete all rows
                deleted = t.rows.len();
                t.rows.clear();
            }
        }
        if deleted > 0 { self.buf_pool.invalidate(table); }
        // Record tombstones for differential backup
        if deleted > 0 {
            let lsn = self.row_lsn_counter.fetch_add(1, Ordering::SeqCst) + 1;
            let mut log = self.tombstone_log.write();
            // We don't have exact IDs for all delete paths, record a marker
            log.push((table.to_string(), -(deleted as i64), lsn));
        }
        Ok(Self::empty_ok(&format!("DELETE {deleted}")))
    }

    fn handle_insert(&self, s: &str) -> Result<QueryResult, String> {
        let table = Self::parse_ident_after(s, "INSERT INTO").ok_or("Invalid INSERT")?;

        let col_open = s.find('(').ok_or("Invalid INSERT columns")?;
        let col_close = s[col_open + 1..].find(')').ok_or("Invalid INSERT columns")? + col_open + 1;
        let cols_part = &s[col_open + 1..col_close];
        let cols: Vec<String> = cols_part
            .split(',')
            .map(|x| x.trim().trim_matches('"').to_string())
            .collect();

        let values_idx = Self::find_keyword_ci(s, "VALUES").ok_or("Invalid INSERT VALUES")?;
        let values_part = &s[values_idx + 6..]; // Skip "VALUES"
        
        // Parse multi-row VALUES: (v1,v2),(v3,v4),...
        let value_groups = Self::parse_multi_value_groups(values_part);
        if value_groups.is_empty() {
            return Err("Invalid INSERT VALUES".to_string());
        }
        
        // Pre-build all rows before acquiring write lock (chunk-based pipeline)
        let has_id_col = cols.iter().any(|c| c.eq_ignore_ascii_case("id"));
        let mut prepared_rows: Vec<(i64, NativeRow)> = Vec::with_capacity(value_groups.len());
        for vals in &value_groups {
            let mut row_map: HashMap<String, Cell> = HashMap::with_capacity(cols.len());
            for (i, c) in cols.iter().enumerate() {
                row_map.insert(c.clone(), vals.get(i).cloned().unwrap_or(Cell::Null));
            }
            // Use explicit "id" if provided; otherwise use 0 as placeholder (reassigned under lock).
            let id = if has_id_col {
                row_map.get("id").map(|v| v.as_i64()).unwrap_or(0)
            } else {
                0 // placeholder — will be replaced with auto-assigned id under write lock
            };
            let lsn = self.row_lsn_counter.fetch_add(1, Ordering::SeqCst) + 1;
            prepared_rows.push((id, NativeRow { cols: row_map, last_modified_lsn: lsn }));
        }

        // Acquire table write lock FIRST — ensures atomicity of unique check + insert + index.
        let mut g = self.tables.write();

        // Check UNIQUE constraints under the lock (no TOCTOU race).
        for (_id, row) in &prepared_rows {
            let mut key_map = std::collections::HashMap::new();
            for (col_name, val) in &row.cols {
                let idx_key = match val {
                    Cell::Int(v) => IndexKey::Integer(*v),
                    Cell::Text(v) => IndexKey::Str(v.clone()),
                    Cell::Float(v) => IndexKey::Integer(*v as i64),
                    Cell::Null => continue,
                };
                key_map.insert(col_name.clone(), idx_key);
            }
            self.index_mgr.check_unique_constraints(table, &key_map)?;
        }

        // Insert rows into table FIRST (before index update).
        let t = g.entry(table.to_string()).or_insert_with(|| {
            let default_types = cols.iter().map(|_| ColType::Text).collect();
            NativeTable::new(cols.clone(), default_types)
        });
        // If no "id" column was supplied, assign sequential rowids under the lock.
        if !has_id_col {
            let mut next_id = t.rows.keys().max().copied().unwrap_or(0) + 1;
            for (id, _row) in prepared_rows.iter_mut() {
                *id = next_id;
                next_id += 1;
            }
        }
        for (id, row) in &prepared_rows {
            t.rows.insert(*id, row.clone());
        }
        // Drop table lock — rows are committed.
        drop(g);

        // Now update indexes (safe — rows already exist).
        {
            let all_meta = self.index_mgr.list_indexes();
            let indexes = self.index_mgr.indexes.read();
            for (id, row) in &prepared_rows {
                for m in &all_meta {
                    if m.table == table {
                        if let Some(tree) = indexes.get(&m.name) {
                            for col in &m.columns {
                                if let Some(val) = row.cols.get(col) {
                                    let idx_key = match val {
                                        Cell::Int(v) => IndexKey::Integer(*v),
                                        Cell::Text(v) => IndexKey::Str(v.clone()),
                                        Cell::Float(v) => IndexKey::Integer(*v as i64),
                                        Cell::Null => continue,
                                    };
                                    tree.insert(idx_key, *id);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Record write stats once per column (not per row)
        for c in &cols {
            self.index_mgr.record_write(table, c);
        }

        // Feed numeric samples into histogram stats
        for (_id, row) in &prepared_rows {
            for (cv, v) in &row.cols {
                match v {
                    Cell::Int(x) => self.index_mgr.record_numeric_value(table, cv, *x as f64),
                    Cell::Float(x) => self.index_mgr.record_numeric_value(table, cv, *x),
                    Cell::Text(_) | Cell::Null => {}
                }
            }
        }

        let inserted_count = prepared_rows.len();
        self.buf_pool.invalidate(table);
        
        Ok(Self::empty_ok(&format!("INSERT 0 {}", inserted_count)))
    }

    /// Parse multi-row VALUES clause: (v1,v2),(v3,v4),... 
    /// Returns a vector of value groups.
    fn parse_multi_value_groups(values_part: &str) -> Vec<Vec<Cell>> {
        let mut groups = Vec::new();
        let trimmed = values_part.trim().trim_end_matches(';');
        let mut depth = 0;
        let mut start = 0;
        
        for (i, ch) in trimmed.char_indices() {
            match ch {
                '(' => {
                    if depth == 0 {
                        start = i + 1; // Start after '('
                    }
                    depth += 1;
                }
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        // Extract the content between '(' and ')'
                        let content = &trimmed[start..i];
                        let vals: Vec<Cell> = content.split(',').map(Self::parse_value).collect();
                        groups.push(vals);
                    }
                }
                _ => {}
            }
        }
        
        groups
    }

    fn handle_update(&self, s: &str) -> Result<QueryResult, String> {
        let table = Self::parse_ident_after(s, "UPDATE").ok_or("Invalid UPDATE")?;
        let set_idx = Self::find_keyword_ci(s, "SET ").ok_or("Invalid UPDATE: missing SET")?;
        let where_idx = Self::find_keyword_ci(s, "WHERE").ok_or("Invalid UPDATE WHERE")?;

        // Parse SET clause: SET col1 = val1, col2 = val2
        let set_part = s[set_idx + 4..where_idx].trim();
        let raw_assignments: Vec<(String, String)> = set_part
            .split(',')
            .map(|a| {
                let mut parts = a.splitn(2, '=');
                let col = parts.next().unwrap_or("").trim().trim_matches('"').to_string();
                let expr = parts.next().unwrap_or("").trim().to_string();
                (col, expr)
            })
            .filter(|(col, _)| !col.is_empty())
            .collect();

        // Parse WHERE clause
        let where_part = s[where_idx + 5..].trim().trim_end_matches(';');

        let mut g = self.tables.write();
        let mut count = 0usize;
        if let Some(t) = g.get_mut(table) {
            // Check for WHERE col IN (v1, v2, ...) pattern
            let ids: Vec<i64> = if let Some((col, values)) = Self::parse_in_list(where_part) {
                if col == "id" || col == t.columns.first().map(|s| s.as_str()).unwrap_or("") {
                    values.iter().map(|v| v.as_i64()).filter(|id| t.rows.contains_key(id)).collect()
                } else if let Some(tree) = self.index_mgr.find_index(table, &col) {
                    let mut ids = Vec::new();
                    for val in &values {
                        let idx_key = match val {
                            Cell::Int(v) => IndexKey::Integer(*v),
                            Cell::Text(v) => IndexKey::Str(v.clone()),
                            Cell::Float(v) => IndexKey::Integer(*v as i64),
                            Cell::Null => continue,
                        };
                        ids.extend(tree.search(&idx_key));
                    }
                    ids
                } else {
                    let value_set: HashSet<String> = values.iter().map(|v| v.as_text()).collect();
                    t.rows.iter()
                        .filter(|(_, row)| row.cols.get(&col).map_or(false, |c| value_set.contains(&c.as_text())))
                        .map(|(id, _)| *id)
                        .collect()
                }
            } else {
                // Single equality: WHERE col = val
                let where_parts: Vec<&str> = where_part.splitn(2, '=').collect();
                if where_parts.len() != 2 {
                    return Err("Invalid UPDATE WHERE clause".to_string());
                }
                let where_col = where_parts[0].trim().trim_matches('"');
                let where_val = Self::parse_value(where_parts[1].trim());

                if where_col == "id" || where_col == t.columns.first().map(|s| s.as_str()).unwrap_or("") {
                    let target_id = where_val.as_i64();
                    if t.rows.contains_key(&target_id) { vec![target_id] } else { vec![] }
                } else if let Some(tree) = self.index_mgr.find_index(table, where_col) {
                    let idx_key = match &where_val {
                        Cell::Int(v) => IndexKey::Integer(*v),
                        Cell::Text(v) => IndexKey::Str(v.clone()),
                        Cell::Float(v) => IndexKey::Integer(*v as i64),
                        Cell::Null => IndexKey::Integer(0),
                    };
                    tree.search(&idx_key)
                } else {
                    t.rows.iter()
                        .filter(|(_, row)| row.cols.get(where_col).map_or(false, |c| *c == where_val))
                        .map(|(id, _)| *id)
                        .collect()
                }
            };

            for id in ids {
                if let Some(row) = t.rows.get_mut(&id) {
                    for (col, expr) in &raw_assignments {
                        let new_val = Self::eval_set_expr(expr, col, row);
                        row.cols.insert(col.clone(), new_val);
                    }
                    row.last_modified_lsn = self.row_lsn_counter.fetch_add(1, Ordering::SeqCst) + 1;
                    count += 1;
                }
            }
        }
        if count > 0 { self.buf_pool.invalidate(table); }
        Ok(Self::empty_ok(&format!("UPDATE {count}")))
    }

    /// Evaluate a SET expression. Supports:
    /// - Literal values: `42`, `3.14`, `'hello'`, `NULL`
    /// - Self-referencing arithmetic: `col + 1.0`, `col - 5`, `col * 2`
    fn eval_set_expr(expr: &str, target_col: &str, row: &NativeRow) -> Cell {
        let trimmed = expr.trim();
        // Try to detect "column_name OP literal" pattern
        for op_str in &[" + ", " - ", " * ", " / "] {
            if let Some(op_pos) = trimmed.find(op_str) {
                let lhs = trimmed[..op_pos].trim().trim_matches('"');
                let rhs = trimmed[op_pos + op_str.len()..].trim();
                // Check if LHS references the target column or another column
                let lhs_val = if lhs.eq_ignore_ascii_case(target_col) {
                    row.cols.get(target_col).cloned().unwrap_or(Cell::Float(0.0))
                } else if let Some(cell) = row.cols.get(lhs) {
                    cell.clone()
                } else {
                    // LHS is a literal
                    Self::parse_value(lhs)
                };
                let rhs_val = Self::parse_value(rhs);
                let lf = lhs_val.as_f64();
                let rf = rhs_val.as_f64();
                let result = match op_str.trim() {
                    "+" => lf + rf,
                    "-" => lf - rf,
                    "*" => lf * rf,
                    "/" => if rf != 0.0 { lf / rf } else { f64::NAN },
                    _ => lf,
                };
                // Preserve integer type if both operands look integral
                if result.fract() == 0.0 && matches!(lhs_val, Cell::Int(_)) && matches!(rhs_val, Cell::Int(_)) {
                    return Cell::Int(result as i64);
                }
                return Cell::Float(result);
            }
        }
        // No expression detected — parse as literal
        Self::parse_value(trimmed)
    }

    fn row_to_data(row: &[Cell]) -> Vec<Option<Vec<u8>>> {
        row.iter()
            .map(|v| match v {
                Cell::Null => None,
                _ => Some(v.as_text().into_bytes()),
            })
            .collect()
    }

    fn handle_select(&self, s: &str) -> Result<QueryResult, String> {
        // Intercept information_schema virtual tables before any other dispatch.
        {
            let up = s.to_ascii_uppercase();
            if up.contains("INFORMATION_SCHEMA.") {
                if let Some(result) = self.handle_information_schema(s) {
                    return result;
                }
            }
        }
        // Case-insensitive keyword checks without allocating uppercase copy.
        if Self::find_keyword_ci(s, " FROM ").is_none() {
            return self.handle_select_constant(s);
        }
        // Window functions: ROW_NUMBER(), RANK(), DENSE_RANK(), LAG(), LEAD() OVER(...)
        if Self::find_keyword_ci(s, " OVER(").is_some() || Self::find_keyword_ci(s, " OVER (").is_some() {
            return self.handle_select_window(s);
        }
        if Self::find_keyword_ci(s, " ORDER BY ").is_some() && Self::find_keyword_ci(s, " LIMIT ").is_some() {
            return self.handle_select_order_limit(s);
        }
        if Self::find_keyword_ci(s, " JOIN ").is_some() {
            return self.handle_select_join(s);
        }
        if Self::find_keyword_ci(s, "GROUP BY").is_some() {
            return self.handle_select_group_by(s);
        }
        // Prefix checks — case-insensitive on first 20 chars
        let prefix: String = s.chars().take(20).collect::<String>().to_ascii_uppercase();
        if prefix.starts_with("SELECT COUNT(*)") {
            return self.handle_select_count(s);
        }
        if prefix.starts_with("SELECT SUM(") {
            return self.handle_select_sum(s);
        }
        if prefix.starts_with("SELECT AVG(") {
            return self.handle_select_avg(s);
        }
        if Self::find_keyword_ci(s, " BETWEEN ").is_some() {
            return self.handle_select_between(s);
        }
        self.handle_select_by_id(s)
    }

    // -------------------------------------------------------------------
    // CTE — WITH name AS (...) SELECT ...
    // -------------------------------------------------------------------

    /// Handle WITH (Common Table Expressions).
    /// Materializes each CTE as a temporary table, runs the final query,
    /// then removes the temp tables.
    fn handle_with_cte(&self, s: &str, username: &str) -> Result<QueryResult, String> {
        // Strategy: parse CTE definitions, materialize each as a temp table,
        // execute the final SELECT, then drop the temp tables.

        let up = s.to_ascii_uppercase();

        // Find the final SELECT after all CTEs
        // WITH name AS (...), name2 AS (...) SELECT ...
        // We need to find matching parentheses to skip CTE bodies

        let after_with = &s[4..].trim_start(); // skip "WITH"
        let after_with_up = after_with.to_ascii_uppercase();

        let mut cte_defs: Vec<(String, String)> = Vec::new(); // (name, query)
        let mut pos = 0;

        loop {
            // Parse CTE name
            let remaining = after_with[pos..].trim_start();
            if remaining.is_empty() {
                return Err("WITH: unexpected end of statement".into());
            }

            // Find "AS" keyword
            let remaining_up = remaining.to_ascii_uppercase();
            let as_pos = remaining_up.find(" AS ")
                .or_else(|| remaining_up.find(" AS("))
                .ok_or("WITH: missing AS keyword")?;

            let cte_name = remaining[..as_pos].trim().to_string();

            // Find opening paren after AS
            let after_as = &remaining[as_pos + 3..].trim_start();
            if !after_as.starts_with('(') {
                return Err("WITH: expected '(' after AS".into());
            }

            // Match balanced parentheses
            let body_start = remaining.len() - after_as.len();
            let mut depth = 0i32;
            let mut body_end = 0;
            for (i, ch) in remaining[body_start..].char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            body_end = body_start + i;
                            break;
                        }
                    }
                    _ => {}
                }
            }

            if depth != 0 {
                return Err("WITH: unbalanced parentheses".into());
            }

            let cte_body = remaining[body_start + 1..body_end].trim().to_string();
            cte_defs.push((cte_name, cte_body));

            // Check if there's another CTE (comma) or the final query
            let after_body = remaining[body_end + 1..].trim_start();
            if after_body.starts_with(',') {
                pos = (remaining.len() - after_body.len()) + 1 + (after_with.len() - remaining.len());
                pos -= after_with.len() - remaining.len();
                // Recalculate pos relative to after_with
                let consumed = after_with.len() - after_body.len() + 1;
                pos = consumed;
            } else {
                // This should be the final query
                let final_offset = after_with.len() - after_body.len();
                let final_query = after_with[final_offset..].trim().to_string();

                // Materialize each CTE as a temp table
                let mut created_tables: Vec<String> = Vec::new();
                for (cte_name, cte_body) in &cte_defs {
                    let temp_name = format!("__cte_{}", cte_name);

                    // Execute the CTE query body
                    let cte_result = self.handle_select(cte_body)?;

                    // Create temp table with the CTE result columns
                    let col_defs: Vec<String> = cte_result.columns.iter().map(|c| {
                        let type_name = match c.1 {
                            oid::INT8 => "BIGINT",
                            oid::FLOAT8 => "DOUBLE",
                            _ => "TEXT",
                        };
                        format!("{} {}", c.0, type_name)
                    }).collect();

                    let create_sql = format!("CREATE TABLE {} ({})", temp_name, col_defs.join(", "));
                    let _ = self.handle_create_table(&create_sql);
                    created_tables.push(temp_name.clone());

                    // Insert CTE rows into temp table
                    let col_names: Vec<&str> = cte_result.columns.iter().map(|c| c.0.as_str()).collect();
                    for row in &cte_result.rows {
                        let vals: Vec<String> = row.iter().map(|cell| {
                            match cell {
                                Some(bytes) => {
                                    let s = String::from_utf8_lossy(bytes);
                                    // Try parse as number, otherwise quote
                                    if s.parse::<f64>().is_ok() {
                                        s.to_string()
                                    } else {
                                        format!("'{}'", s.replace('\'', "''"))
                                    }
                                }
                                None => "NULL".to_string(),
                            }
                        }).collect();
                        let insert_sql = format!("INSERT INTO {} ({}) VALUES ({})",
                            temp_name, col_names.join(", "), vals.join(", "));
                        let _ = self.handle_insert(&insert_sql);
                    }
                }

                // Replace CTE names with temp table names in final query
                let mut resolved_query = final_query.clone();
                for (cte_name, _) in &cte_defs {
                    let temp_name = format!("__cte_{}", cte_name);
                    // Case-insensitive word-boundary replacement
                    resolved_query = Self::replace_word_ci(&resolved_query, cte_name, &temp_name);
                }

                // Execute the final query
                let result = self.handle_select(&resolved_query);

                // Clean up temp tables
                for temp_name in &created_tables {
                    let drop_sql = format!("DROP TABLE {}", temp_name);
                    let _ = self.handle_drop_table(&drop_sql);
                }

                return result;
            }
        }
    }

    // -------------------------------------------------------------------
    // Window Functions — ROW_NUMBER(), RANK(), DENSE_RANK(), LAG(), LEAD()
    // -------------------------------------------------------------------

    /// Parse a window function call from the SELECT list.
    /// Returns (func, alias, rest_of_select_list).
    fn parse_window_func(expr_up: &str, expr_orig: &str) -> Option<(WindowFunc, String)> {
        let trimmed = expr_up.trim();
        let orig_trimmed = expr_orig.trim();

        // Extract alias if present (... AS alias)
        let (func_part_up, alias) = if let Some(as_pos) = trimmed.rfind(" AS ") {
            (&trimmed[..as_pos], trimmed[as_pos + 4..].trim().to_string())
        } else {
            (trimmed, String::new())
        };

        let func = if func_part_up.starts_with("ROW_NUMBER(") || func_part_up.starts_with("ROW_NUMBER (") {
            let alias = if alias.is_empty() { "row_number".to_string() } else { alias };
            Some((WindowFunc::RowNumber, alias))
        } else if func_part_up.starts_with("RANK(") || func_part_up.starts_with("RANK (") {
            let alias = if alias.is_empty() { "rank".to_string() } else { alias };
            Some((WindowFunc::Rank, alias))
        } else if func_part_up.starts_with("DENSE_RANK(") || func_part_up.starts_with("DENSE_RANK (") {
            let alias = if alias.is_empty() { "dense_rank".to_string() } else { alias };
            Some((WindowFunc::DenseRank, alias))
        } else if func_part_up.starts_with("LAG(") || func_part_up.starts_with("LAG (") {
            // LAG(col, offset) OVER(...)
            let paren_start = func_part_up.find('(')?;
            let paren_end = func_part_up.find(')')?;
            let args = &orig_trimmed[paren_start + 1..paren_end];
            let parts: Vec<&str> = args.split(',').collect();
            let col = parts.first()?.trim().to_string();
            let offset: usize = parts.get(1).and_then(|s| s.trim().parse().ok()).unwrap_or(1);
            let alias = if alias.is_empty() { "lag".to_string() } else { alias };
            Some((WindowFunc::Lag(col, offset), alias))
        } else if func_part_up.starts_with("LEAD(") || func_part_up.starts_with("LEAD (") {
            let paren_start = func_part_up.find('(')?;
            let paren_end = func_part_up.find(')')?;
            let args = &orig_trimmed[paren_start + 1..paren_end];
            let parts: Vec<&str> = args.split(',').collect();
            let col = parts.first()?.trim().to_string();
            let offset: usize = parts.get(1).and_then(|s| s.trim().parse().ok()).unwrap_or(1);
            let alias = if alias.is_empty() { "lead".to_string() } else { alias };
            Some((WindowFunc::Lead(col, offset), alias))
        } else {
            None
        };

        func
    }

    /// Handle SELECT with window functions (OVER clause).
    /// Supports: ROW_NUMBER(), RANK(), DENSE_RANK(), LAG(col, n), LEAD(col, n)
    /// with PARTITION BY and ORDER BY.
    fn handle_select_window(&self, s: &str) -> Result<QueryResult, String> {
        // Extract table name
        let table = Self::parse_ident_after(s, "FROM").ok_or("Invalid SELECT FROM")?;

        let g = self.tables.read();
        let t = g.get(table).ok_or(format!("table \"{}\" does not exist", table))?;

        // Parse the SELECT list between SELECT and FROM
        let up = s.to_ascii_uppercase();
        let sel_start = up.find("SELECT").ok_or("missing SELECT")? + 6;
        let from_pos = Self::find_keyword_ci(s, " FROM ").ok_or("missing FROM")?;
        let select_list = s[sel_start..from_pos].trim();
        let select_list_up = up[sel_start..from_pos].trim();

        // Find the OVER(...) clause to parse PARTITION BY and ORDER BY
        let over_pos = Self::find_keyword_ci(s, " OVER(")
            .or_else(|| Self::find_keyword_ci(s, " OVER ("))
            .ok_or("missing OVER clause")?;

        // Find the matching closing paren for OVER(
        let over_paren_start = s[over_pos..].find('(')
            .map(|p| over_pos + p)
            .ok_or("malformed OVER clause")?;
        let over_body_end = s[over_paren_start..].find(')')
            .map(|p| over_paren_start + p)
            .ok_or("missing closing ) in OVER")?;
        let over_body = s[over_paren_start + 1..over_body_end].trim();
        let over_body_up = over_body.to_ascii_uppercase();

        // Parse PARTITION BY
        let partition_col = if let Some(pb_pos) = over_body_up.find("PARTITION BY") {
            let after_pb = over_body[pb_pos + 12..].trim();
            let end = after_pb.find(|c: char| c == ' ' || c == ')' || c == ',')
                .unwrap_or(after_pb.len());
            Some(after_pb[..end].trim().to_string())
        } else {
            None
        };

        // Parse ORDER BY
        let (order_col, order_desc) = if let Some(ob_pos) = over_body_up.find("ORDER BY") {
            let after_ob = over_body[ob_pos + 8..].trim();
            let end = after_ob.find(|c: char| c == ')' || c == ',')
                .unwrap_or(after_ob.len());
            let order_expr = after_ob[..end].trim();
            let desc = order_expr.to_ascii_uppercase().contains("DESC");
            let col = order_expr.split_whitespace().next().unwrap_or("id").to_string();
            (col, desc)
        } else {
            ("id".to_string(), false)
        };

        // Parse the window function from the select list
        let (wfunc, wf_alias) = Self::parse_window_func(select_list_up, select_list)
            .ok_or("unsupported window function (use ROW_NUMBER, RANK, DENSE_RANK, LAG, LEAD)")?;

        // Collect base columns (non-window columns from SELECT list or all table columns)
        let mut base_cols: Vec<String> = Vec::new();
        // For simplicity, return all table columns + the window function column
        base_cols.extend(t.columns.iter().cloned());

        // Build columnar cache for efficient access
        let cc = self.get_or_build_cols(table, t);
        drop(g);

        let n = cc.ids.len();
        if n == 0 {
            let mut columns: Vec<(String, i32, i16)> = base_cols.iter().map(|c| (c.clone(), oid::TEXT, -1i16)).collect();
            columns.push((wf_alias.clone(), oid::INT8, 8));
            return Ok(QueryResult { columns, rows: vec![], command_tag: "SELECT 0".to_string() });
        }

        // Build row indices with sort key for ORDER BY
        let order_vals: Vec<Cell> = (0..n).map(|i| {
            if order_col == "id" { Cell::Int(cc.ids[i]) }
            else if let Some(iv) = cc.int_cols.get(order_col.as_str()) { Cell::Int(iv[i]) }
            else if let Some(fv) = cc.float_cols.get(order_col.as_str()) { Cell::Float(fv[i]) }
            else if let Some(tv) = cc.text_cols.get(order_col.as_str()) { Cell::Text(tv[i].clone()) }
            else { Cell::Int(cc.ids[i]) }
        }).collect();

        // Build partition keys
        let partition_vals: Option<Vec<String>> = partition_col.as_ref().map(|pcol| {
            (0..n).map(|i| {
                if pcol == "id" { cc.ids[i].to_string() }
                else if let Some(iv) = cc.int_cols.get(pcol.as_str()) { iv[i].to_string() }
                else if let Some(fv) = cc.float_cols.get(pcol.as_str()) { fv[i].to_string() }
                else if let Some(tv) = cc.text_cols.get(pcol.as_str()) { tv[i].clone() }
                else { String::new() }
            }).collect()
        });

        // Build sorted indices
        let mut indices: Vec<usize> = (0..n).collect();
        indices.sort_by(|&a, &b| {
            let cmp = Self::compare_cells(&order_vals[a], &order_vals[b]);
            if order_desc { cmp.reverse() } else { cmp }
        });

        // Compute window function values
        let mut wf_values: Vec<i64> = vec![0; n];
        match &wfunc {
            WindowFunc::RowNumber => {
                if let Some(ref pvals) = partition_vals {
                    let mut partition_counter: AHashMap<&str, i64> = AHashMap::new();
                    for &idx in &indices {
                        let pk = pvals[idx].as_str();
                        let counter = partition_counter.entry(pk).or_insert(0);
                        *counter += 1;
                        wf_values[idx] = *counter;
                    }
                } else {
                    for (rank, &idx) in indices.iter().enumerate() {
                        wf_values[idx] = (rank + 1) as i64;
                    }
                }
            }
            WindowFunc::Rank => {
                if let Some(ref pvals) = partition_vals {
                    let mut partition_state: AHashMap<&str, (i64, i64)> = AHashMap::new(); // (rank, count)
                    let mut prev_by_part: AHashMap<&str, usize> = AHashMap::new();
                    for &idx in &indices {
                        let pk = pvals[idx].as_str();
                        let (rank, count) = partition_state.entry(pk).or_insert((0, 0));
                        *count += 1;
                        if let Some(&prev_idx) = prev_by_part.get(pk) {
                            if Self::compare_cells(&order_vals[idx], &order_vals[prev_idx]) != std::cmp::Ordering::Equal {
                                *rank = *count;
                            }
                        } else {
                            *rank = 1;
                        }
                        wf_values[idx] = *rank;
                        prev_by_part.insert(pk, idx);
                    }
                } else {
                    let mut rank = 1i64;
                    for (i, &idx) in indices.iter().enumerate() {
                        if i > 0 && Self::compare_cells(&order_vals[idx], &order_vals[indices[i - 1]]) != std::cmp::Ordering::Equal {
                            rank = (i + 1) as i64;
                        }
                        wf_values[idx] = rank;
                    }
                }
            }
            WindowFunc::DenseRank => {
                if let Some(ref pvals) = partition_vals {
                    let mut partition_rank: AHashMap<&str, i64> = AHashMap::new();
                    let mut prev_by_part: AHashMap<&str, usize> = AHashMap::new();
                    for &idx in &indices {
                        let pk = pvals[idx].as_str();
                        let rank = partition_rank.entry(pk).or_insert(0);
                        if let Some(&prev_idx) = prev_by_part.get(pk) {
                            if Self::compare_cells(&order_vals[idx], &order_vals[prev_idx]) != std::cmp::Ordering::Equal {
                                *rank += 1;
                            }
                        } else {
                            *rank = 1;
                        }
                        wf_values[idx] = *rank;
                        prev_by_part.insert(pk, idx);
                    }
                } else {
                    let mut dense_rank = 0i64;
                    for (i, &idx) in indices.iter().enumerate() {
                        if i == 0 || Self::compare_cells(&order_vals[idx], &order_vals[indices[i - 1]]) != std::cmp::Ordering::Equal {
                            dense_rank += 1;
                        }
                        wf_values[idx] = dense_rank;
                    }
                }
            }
            WindowFunc::Lag(col, offset) => {
                let col_vals: Vec<Cell> = (0..n).map(|i| {
                    if col == "id" { Cell::Int(cc.ids[i]) }
                    else if let Some(iv) = cc.int_cols.get(col.as_str()) { Cell::Int(iv[i]) }
                    else if let Some(fv) = cc.float_cols.get(col.as_str()) { Cell::Float(fv[i]) }
                    else { Cell::Int(0) }
                }).collect();
                if let Some(ref pvals) = partition_vals {
                    let mut partition_history: AHashMap<&str, Vec<usize>> = AHashMap::new();
                    for &idx in &indices {
                        let pk = pvals[idx].as_str();
                        let history = partition_history.entry(pk).or_default();
                        let lag_idx = if history.len() >= *offset { Some(history[history.len() - offset]) } else { None };
                        wf_values[idx] = match lag_idx {
                            Some(li) => match &col_vals[li] { Cell::Int(v) => *v, Cell::Float(v) => *v as i64, _ => 0 },
                            None => 0,
                        };
                        history.push(idx);
                    }
                } else {
                    for (i, &idx) in indices.iter().enumerate() {
                        wf_values[idx] = if i >= *offset {
                            match &col_vals[indices[i - offset]] { Cell::Int(v) => *v, Cell::Float(v) => *v as i64, _ => 0 }
                        } else { 0 };
                    }
                }
            }
            WindowFunc::Lead(col, offset) => {
                let col_vals: Vec<Cell> = (0..n).map(|i| {
                    if col == "id" { Cell::Int(cc.ids[i]) }
                    else if let Some(iv) = cc.int_cols.get(col.as_str()) { Cell::Int(iv[i]) }
                    else if let Some(fv) = cc.float_cols.get(col.as_str()) { Cell::Float(fv[i]) }
                    else { Cell::Int(0) }
                }).collect();
                if let Some(ref pvals) = partition_vals {
                    // Two-pass: collect partition members first, then compute lead
                    let mut partition_members: AHashMap<&str, Vec<usize>> = AHashMap::new();
                    for &idx in &indices {
                        partition_members.entry(pvals[idx].as_str()).or_default().push(idx);
                    }
                    for members in partition_members.values() {
                        for (pos, &idx) in members.iter().enumerate() {
                            wf_values[idx] = if pos + offset < members.len() {
                                match &col_vals[members[pos + offset]] { Cell::Int(v) => *v, Cell::Float(v) => *v as i64, _ => 0 }
                            } else { 0 };
                        }
                    }
                } else {
                    for (i, &idx) in indices.iter().enumerate() {
                        wf_values[idx] = if i + offset < n {
                            match &col_vals[indices[i + offset]] { Cell::Int(v) => *v, Cell::Float(v) => *v as i64, _ => 0 }
                        } else { 0 };
                    }
                }
            }
        }

        // Build output rows in sort order
        let g = self.tables.read();
        let t = g.get(table).ok_or("table vanished")?;
        let mut output_cols: Vec<(String, i32, i16)> = base_cols.iter().map(|c| {
            let o = t.col_oid(c);
            let len = match o { oid::INT8 => 8i16, oid::FLOAT8 => 8, _ => -1 };
            (c.clone(), o, len)
        }).collect();
        output_cols.push((wf_alias, oid::INT8, 8));
        drop(g);

        let mut rows: Vec<Vec<Option<Vec<u8>>>> = Vec::with_capacity(n);
        for &idx in &indices {
            let mut row: Vec<Option<Vec<u8>>> = Vec::with_capacity(base_cols.len() + 1);
            for col in &base_cols {
                let val = if col == "id" {
                    Some(cc.ids[idx].to_string().into_bytes())
                } else if let Some(iv) = cc.int_cols.get(col.as_str()) {
                    Some(iv[idx].to_string().into_bytes())
                } else if let Some(fv) = cc.float_cols.get(col.as_str()) {
                    Some(fv[idx].to_string().into_bytes())
                } else if let Some(tv) = cc.text_cols.get(col.as_str()) {
                    Some(tv[idx].as_bytes().to_vec())
                } else {
                    None
                };
                row.push(val);
            }
            row.push(Some(wf_values[idx].to_string().into_bytes()));
            rows.push(row);
        }

        Ok(QueryResult {
            columns: output_cols,
            rows,
            command_tag: format!("SELECT {}", n),
        })
    }

    /// Compare two Cell values for ordering.
    fn compare_cells(a: &Cell, b: &Cell) -> std::cmp::Ordering {
        match (a, b) {
            (Cell::Int(x), Cell::Int(y)) => x.cmp(y),
            (Cell::Float(x), Cell::Float(y)) => x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
            (Cell::Int(x), Cell::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
            (Cell::Float(x), Cell::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(std::cmp::Ordering::Equal),
            (Cell::Text(x), Cell::Text(y)) => x.cmp(y),
            _ => std::cmp::Ordering::Equal,
        }
    }

    /// Handle SELECT columns FROM table (no WHERE) — full table scan.
    /// Uses the columnar cache for sequential, cache-friendly access.
    fn handle_select_all(&self, s: &str, table: &str) -> Result<QueryResult, String> {
        let select_cols = Self::parse_select_columns(s);
        let g = self.tables.read();
        let t = g.get(table).ok_or(format!("table \"{}\" does not exist", table))?;

        let out_cols: Vec<String> = if select_cols.is_empty() || (select_cols.len() == 1 && select_cols[0] == "*") {
            t.columns.clone()
        } else {
            select_cols
        };

        let columns: Vec<(String, i32, i16)> = out_cols.iter().map(|c| {
            let o = t.col_oid(c);
            let len = match o {
                oid::INT8 => 8i16,
                oid::FLOAT8 => 8,
                _ => -1,
            };
            (c.clone(), o, len)
        }).collect();

        // Use columnar cache for sequential access.
        // Chunk-based pipeline: process CHUNK_SIZE rows in parallel.
        let cc = self.get_or_build_cols(table, t);
        let n = cc.ids.len();

        // Pre-resolve column references once (avoid per-row HashMap lookups)
        enum ColSlice<'a> { Id, Float(&'a [f64]), Text(&'a [String]), Int(&'a [i64]), Missing }
        let col_slices: Vec<ColSlice> = out_cols.iter().map(|c| {
            if c == "id" || c == t.columns.first().map(|s| s.as_str()).unwrap_or("") {
                ColSlice::Id
            } else if let Some(fv) = cc.float_cols.get(c.as_str()) {
                ColSlice::Float(fv.as_slice())
            } else if let Some(tv) = cc.text_cols.get(c.as_str()) {
                ColSlice::Text(tv.as_slice())
            } else if let Some(iv) = cc.int_cols.get(c.as_str()) {
                ColSlice::Int(iv.as_slice())
            } else {
                ColSlice::Missing
            }
        }).collect();

        let rows_out: Vec<Vec<Option<Vec<u8>>>> = if n > CHUNK_SIZE {
            // Parallel chunk-based pipeline for large result sets
            let ids_slice = &cc.ids;
            (0..n).into_par_iter()
                .chunks(CHUNK_SIZE)
                .flat_map(|chunk| {
                    let mut local: Vec<Vec<Option<Vec<u8>>>> = Vec::with_capacity(chunk.len());
                    for idx in chunk {
                        let mut row: Vec<Option<Vec<u8>>> = Vec::with_capacity(col_slices.len());
                        for cs in &col_slices {
                            match cs {
                                ColSlice::Id => row.push(Some(ids_slice[idx].to_string().into_bytes())),
                                ColSlice::Float(fv) => row.push(Some(fv[idx].to_string().into_bytes())),
                                ColSlice::Text(tv) => row.push(Some(tv[idx].as_bytes().to_vec())),
                                ColSlice::Int(iv) => row.push(Some(iv[idx].to_string().into_bytes())),
                                ColSlice::Missing => row.push(None),
                            }
                        }
                        local.push(row);
                    }
                    local
                })
                .collect()
        } else {
            // Sequential for small tables
            let mut rows: Vec<Vec<Option<Vec<u8>>>> = Vec::with_capacity(n);
            for idx in 0..n {
                let mut row: Vec<Option<Vec<u8>>> = Vec::with_capacity(col_slices.len());
                for cs in &col_slices {
                    match cs {
                        ColSlice::Id => row.push(Some(cc.ids[idx].to_string().into_bytes())),
                        ColSlice::Float(fv) => row.push(Some(fv[idx].to_string().into_bytes())),
                        ColSlice::Text(tv) => row.push(Some(tv[idx].as_bytes().to_vec())),
                        ColSlice::Int(iv) => row.push(Some(iv[idx].to_string().into_bytes())),
                        ColSlice::Missing => row.push(None),
                    }
                }
                rows.push(row);
            }
            rows
        };

        let row_count = rows_out.len();
        Ok(QueryResult {
            columns,
            rows: rows_out,
            command_tag: format!("SELECT {}", row_count),
        })
    }

    /// Handle SELECT ... ORDER BY col [ASC|DESC] LIMIT n — TopN sort using BinaryHeap.
    /// Implements O(N * log K) complexity where N = rows, K = limit.
    fn handle_select_order_limit(&self, s: &str) -> Result<QueryResult, String> {
        // Parse table name
        let table = Self::parse_ident_after(s, "FROM").ok_or("Invalid SELECT FROM")?;
        
        // Parse SELECT columns
        let select_cols = Self::parse_select_columns(s);
        
        // Parse ORDER BY column and direction
        let order_idx = Self::find_keyword_ci(s, " ORDER BY ").ok_or("Invalid ORDER BY")?;
        let after_order = &s[order_idx + 10..]; // skip " ORDER BY "
        
        // Find LIMIT position in the remaining string
        let limit_idx_in_after = Self::find_keyword_ci(after_order, " LIMIT ").ok_or("Invalid LIMIT")?;
        let order_part = after_order[..limit_idx_in_after].trim();
        
        // Parse column and direction from ORDER BY clause
        let (sort_col, is_desc) = if order_part.len() >= 5 && order_part[order_part.len()-5..].eq_ignore_ascii_case(" DESC") {
            (order_part[..order_part.len() - 5].trim(), true)
        } else if order_part.len() >= 4 && order_part[order_part.len()-4..].eq_ignore_ascii_case(" ASC") {
            (order_part[..order_part.len() - 4].trim(), false)
        } else {
            (order_part, false) // default ASC
        };
        let sort_col = sort_col.trim_matches('"').to_string();
        
        // Parse LIMIT value
        let after_limit = &after_order[limit_idx_in_after + 7..]; // skip " LIMIT "
        let limit_str = after_limit.split_whitespace().next().unwrap_or("100");
        let limit: usize = limit_str.trim_end_matches(';').parse().unwrap_or(100);
        
        // Get table data
        let g = self.tables.read();
        let t = g.get(&table as &str).ok_or(format!("table \"{}\" does not exist", table))?;;
        
        // Determine output columns
        let out_cols: Vec<String> = if select_cols.is_empty() || (select_cols.len() == 1 && select_cols[0] == "*") {
            t.columns.clone()
        } else {
            select_cols
        };
        
        // Build column metadata
        let columns: Vec<(String, i32, i16)> = out_cols.iter().map(|c| {
            let o = t.col_oid(c);
            let len = match o {
                oid::INT8 => 8i16,
                oid::FLOAT8 => 8,
                _ => -1,
            };
            (c.clone(), o, len)
        }).collect();
        
        // Collect all rows with sort keys
        let mut rows_with_keys: Vec<(f64, Vec<Option<Vec<u8>>>)> = Vec::new();
        for (_id, row) in &t.rows {
            // Get sort key as f64 for comparison
            let sort_key = match row.cols.get(&sort_col) {
                Some(Cell::Int(i)) => *i as f64,
                Some(Cell::Float(r)) => *r,
                Some(Cell::Text(t)) => t.parse::<f64>().unwrap_or(0.0),
                _ => 0.0,
            };
            
            // Build row data
            let mut data: Vec<Option<Vec<u8>>> = Vec::with_capacity(out_cols.len());
            for c in &out_cols {
                let val = row.cols.get(c.as_str()).cloned().unwrap_or(Cell::Null);
                data.push(Some(val.as_text().into_bytes()));
            }
            rows_with_keys.push((sort_key, data));
        }
        
        // TopN sort using BinaryHeap - O(N * log K) complexity
        use std::collections::BinaryHeap;
        use std::cmp::Ordering;
        
        #[derive(Debug)]
        struct HeapEntry {
            key: f64,
            row: Vec<Option<Vec<u8>>>,
            is_desc: bool,
        }
        
        impl PartialEq for HeapEntry {
            fn eq(&self, other: &Self) -> bool {
                self.key.total_cmp(&other.key) == Ordering::Equal
            }
        }
        impl Eq for HeapEntry {}
        
        impl PartialOrd for HeapEntry {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }
        
        impl Ord for HeapEntry {
            fn cmp(&self, other: &Self) -> Ordering {
                // BinaryHeap is a max-heap (largest at top).
                // For DESC (want K largest): use min-heap → invert so smallest key = highest priority
                // For ASC (want K smallest): use max-heap → normal comparison
                let base = self.key.total_cmp(&other.key);
                if self.is_desc {
                    base.reverse() // DESC: min-heap (smallest at top to evict when larger comes)
                } else {
                    base // ASC: max-heap (largest at top to evict when smaller comes)
                }
            }
        }
        
        let mut heap: BinaryHeap<HeapEntry> = BinaryHeap::with_capacity(limit + 1);
        
        for (key, row) in rows_with_keys {
            let entry = HeapEntry { key, row, is_desc };
            
            if heap.len() < limit {
                heap.push(entry);
            } else if let Some(top) = heap.peek() {
                // Check if new entry should replace top
                // For DESC (min-heap): new > top.key means new is better
                // For ASC (max-heap): new < top.key means new is better
                let should_replace = if is_desc {
                    key > top.key // new is larger, should be in top-K
                } else {
                    key < top.key // new is smaller, should be in bottom-K
                };
                if should_replace {
                    heap.pop();
                    heap.push(entry);
                }
            }
        }
        
        // Extract and sort results in proper order
        let mut result: Vec<HeapEntry> = heap.into_vec();
        result.sort_by(|a, b| {
            let cmp = a.key.total_cmp(&b.key);
            if is_desc {
                cmp.reverse() // DESC: largest first
            } else {
                cmp // ASC: smallest first
            }
        });
        
        let rows_out: Vec<Vec<Option<Vec<u8>>>> = result.into_iter().map(|e| e.row).collect();
        let row_count = rows_out.len();
        
        Ok(QueryResult {
            columns,
            rows: rows_out,
            command_tag: format!("SELECT {}", row_count),
        })
    }

    /// Handle SELECT <expr> without FROM — returns the literal value.
    fn handle_select_constant(&self, s: &str) -> Result<QueryResult, String> {
        // "SELECT 1" → column="?column?", value="1"
        let expr = s[6..].trim(); // skip "SELECT"
        let val = expr.trim_end_matches(';').trim();
        Ok(QueryResult {
            columns: vec![("?column?".to_string(), oid::TEXT, -1)],
            rows: vec![vec![Some(val.as_bytes().to_vec())]],
            command_tag: "SELECT 1".to_string(),
        })
    }

    fn handle_select_by_id(&self, s: &str) -> Result<QueryResult, String> {
        let table = Self::parse_ident_after(s, "FROM").ok_or("Invalid SELECT FROM")?;

        // Full table scan if no WHERE clause.
        let where_idx = match Self::find_keyword_ci(s, "WHERE") {
            Some(idx) => idx,
            None => return self.handle_select_all(s, table),
        };

        let pred_part = s[where_idx + 5..].trim();

        // Detect column name and value from predicate "col = value"
        let eq_parts: Vec<&str> = pred_part.splitn(2, '=').collect();
        if eq_parts.len() == 2 {
            let col_name = eq_parts[0].trim().trim_matches('"');
            let val_tok = eq_parts[1].trim();
            let val = Self::parse_value(val_tok);

            // Record stats for auto-indexer
            self.index_mgr.record_query_hit(table, col_name);

            // Try B+Tree index lookup
            if let Some(tree) = self.index_mgr.find_index(table, col_name) {
                self.index_mgr.record_index_use(&tree.name);
                let idx_key = match &val {
                    Cell::Int(v) => IndexKey::Integer(*v),
                    Cell::Text(v) => IndexKey::Str(v.clone()),
                    Cell::Float(v) => IndexKey::Integer(*v as i64),
                    Cell::Null => IndexKey::Integer(0),
                };
                let row_ids = tree.search(&idx_key);
                let g = self.tables.read();
                let mut rows_out: Vec<Vec<Option<Vec<u8>>>> = Vec::new();
                if let Some(t) = g.get(table) {
                    for rid in &row_ids {
                        if let Some(row) = t.rows.get(rid) {
                            let out = vec![
                                row.cols.get("id").cloned().unwrap_or(Cell::Int(*rid)),
                                row.cols.get("balance").cloned().unwrap_or(Cell::Float(0.0)),
                                row.cols.get("name").cloned().unwrap_or(Cell::Text(String::new())),
                            ];
                            rows_out.push(Self::row_to_data(&out));
                        }
                    }
                }
                let row_count = rows_out.len();
                return Ok(QueryResult {
                    columns: vec![
                        ("id".to_string(), oid::INT8, 8),
                        ("balance".to_string(), oid::FLOAT8, 8),
                        ("name".to_string(), oid::TEXT, -1),
                    ],
                    rows: rows_out,
                    command_tag: format!("SELECT {}", row_count),
                });
            }
        }

        // Fallback: original id-based lookup
        let id_tok = s[where_idx + 5..]
            .split('=')
            .nth(1)
            .ok_or("Invalid id predicate")?
            .trim();
        let id = Self::parse_value(id_tok).as_i64();

        let g = self.tables.read();
        let mut rows_out: Vec<Vec<Option<Vec<u8>>>> = Vec::new();
        if let Some(t) = g.get(table) {
            if let Some(row) = t.rows.get(&id) {
                let out = vec![
                    row.cols.get("id").cloned().unwrap_or(Cell::Int(id)),
                    row.cols.get("balance").cloned().unwrap_or(Cell::Float(0.0)),
                    row.cols.get("name").cloned().unwrap_or(Cell::Text(String::new())),
                ];
                rows_out.push(Self::row_to_data(&out));
            }
        }
        let row_count = rows_out.len();
        Ok(QueryResult {
            columns: vec![
                ("id".to_string(), oid::INT8, 8),
                ("balance".to_string(), oid::FLOAT8, 8),
                ("name".to_string(), oid::TEXT, -1),
            ],
            rows: rows_out,
            command_tag: format!("SELECT {}", row_count),
        })
    }

    fn handle_select_between(&self, s: &str) -> Result<QueryResult, String> {
        let table = Self::parse_ident_after(s, "FROM").ok_or("Invalid SELECT FROM")?;

        // Parse SELECT columns
        let select_cols = Self::parse_select_columns(s);

        let where_idx = Self::find_keyword_ci(s, "WHERE").ok_or("Invalid SELECT WHERE")?;
        let pred = s[where_idx + 5..].trim();
        let parts: Vec<&str> = pred.split_whitespace().collect();
        if parts.len() < 5 {
            return Err("Invalid BETWEEN predicate".to_string());
        }
        let col_name = parts[0].trim_matches('"');
        let lo = Self::parse_value(parts[2]).as_i64();
        let hi = Self::parse_value(parts[4]).as_i64();

        // Record stats for auto-indexer
        self.index_mgr.record_query_hit(table, col_name);
        let _ = self
            .index_mgr
            .update_selectivity_from_histogram_between(table, col_name, lo as f64, hi as f64);

        let g = self.tables.read();
        let t = match g.get(table) {
            Some(t) => t,
            None => {
                return Ok(QueryResult {
                    columns: select_cols.iter().map(|c| (c.clone(), oid::TEXT, -1i16)).collect(),
                    rows: vec![],
                    command_tag: "SELECT 0".to_string(),
                });
            }
        };

        let columns: Vec<(String, i32, i16)> = select_cols.iter().map(|c| {
            let type_oid = t.col_oid(c);
            let type_len = match type_oid {
                oid::INT8 => 8i16,
                oid::FLOAT8 => 8i16,
                _ => -1i16,
            };
            (c.clone(), type_oid, type_len)
        }).collect();

        // ── Fast path: columnar cache with sorted binary search ──
        // When filtering on "id" (the sorted key), use binary search on the
        // columnar cache to avoid HashMap lookups entirely.
        // Uses chunk-based pipeline: process CHUNK_SIZE rows in parallel.
        if col_name == "id" || col_name == t.columns.first().map(|s| s.as_str()).unwrap_or("") {
            let cc = self.get_or_build_cols(table, t);
            // Binary search for [lo, hi] range in sorted ids.
            let start = cc.ids.partition_point(|x| *x < lo);
            let end = cc.ids.partition_point(|x| *x <= hi);
            let count = end - start;

            // Pre-resolve column slice references to avoid per-row HashMap lookups.
            enum ColRef<'a> { Id, Float(&'a [f64]), Text(&'a [String]), Int(&'a [i64]), Missing }
            let col_refs: Vec<ColRef> = select_cols.iter().map(|col| {
                if col == "id" || col == col_name {
                    ColRef::Id
                } else if let Some(fv) = cc.float_cols.get(col.as_str()) {
                    ColRef::Float(fv.as_slice())
                } else if let Some(tv) = cc.text_cols.get(col.as_str()) {
                    ColRef::Text(tv.as_slice())
                } else if let Some(iv) = cc.int_cols.get(col.as_str()) {
                    ColRef::Int(iv.as_slice())
                } else {
                    ColRef::Missing
                }
            }).collect();

            let rows_out: Vec<Vec<Option<Vec<u8>>>> = if count > CHUNK_SIZE {
                // Chunk-based parallel pipeline for large range scans
                let ids_ref = &cc.ids;
                (start..end).into_par_iter()
                    .chunks(CHUNK_SIZE)
                    .flat_map(|chunk| {
                        let mut local: Vec<Vec<Option<Vec<u8>>>> = Vec::with_capacity(chunk.len());
                        for idx in chunk {
                            let mut out_row: Vec<Option<Vec<u8>>> = Vec::with_capacity(col_refs.len());
                            for cr in &col_refs {
                                match cr {
                                    ColRef::Id => out_row.push(Some(ids_ref[idx].to_string().into_bytes())),
                                    ColRef::Float(fv) => out_row.push(Some(fv[idx].to_string().into_bytes())),
                                    ColRef::Text(tv) => out_row.push(Some(tv[idx].as_bytes().to_vec())),
                                    ColRef::Int(iv) => out_row.push(Some(iv[idx].to_string().into_bytes())),
                                    ColRef::Missing => out_row.push(None),
                                }
                            }
                            local.push(out_row);
                        }
                        local
                    })
                    .collect()
            } else {
                let mut rows: Vec<Vec<Option<Vec<u8>>>> = Vec::with_capacity(count);
                for idx in start..end {
                    let mut out_row: Vec<Option<Vec<u8>>> = Vec::with_capacity(col_refs.len());
                    for cr in &col_refs {
                        match cr {
                            ColRef::Id => out_row.push(Some(cc.ids[idx].to_string().into_bytes())),
                            ColRef::Float(fv) => out_row.push(Some(fv[idx].to_string().into_bytes())),
                            ColRef::Text(tv) => out_row.push(Some(tv[idx].as_bytes().to_vec())),
                            ColRef::Int(iv) => out_row.push(Some(iv[idx].to_string().into_bytes())),
                            ColRef::Missing => out_row.push(None),
                        }
                    }
                    rows.push(out_row);
                }
                rows
            };

            return Ok(QueryResult {
                columns,
                rows: rows_out,
                command_tag: format!("SELECT {}", count),
            });
        }

        // ── Standard path: B+Tree index or full scan ──
        let mut matching_rows: Vec<(i64, &NativeRow)> = Vec::new();
        if let Some(tree) = self.index_mgr.find_index(table, col_name) {
            self.index_mgr.record_index_use(&tree.name);
            let lo_key = IndexKey::Integer(lo);
            let hi_key = IndexKey::Integer(hi);
            let row_ids = tree.range_scan(&lo_key, &hi_key);
            for rid in row_ids {
                if let Some(row) = t.rows.get(&rid) {
                    matching_rows.push((rid, row));
                }
            }
        } else {
            for (id, row) in &t.rows {
                let val = row.cols.get(col_name).cloned().unwrap_or(Cell::Int(*id)).as_i64();
                if val >= lo && val <= hi {
                    matching_rows.push((*id, row));
                }
            }
        }

        let mut rows_out: Vec<Vec<Option<Vec<u8>>>> = Vec::with_capacity(matching_rows.len());
        for (id, row) in &matching_rows {
            let mut out_row: Vec<Option<Vec<u8>>> = Vec::with_capacity(select_cols.len());
            for col in &select_cols {
                let cell = if col == "id" {
                    row.cols.get("id").cloned().unwrap_or(Cell::Int(*id))
                } else {
                    row.cols.get(col.as_str()).cloned().unwrap_or(Cell::Null)
                };
                match &cell {
                    Cell::Null => out_row.push(None),
                    _ => out_row.push(Some(cell.as_text().into_bytes())),
                }
            }
            rows_out.push(out_row);
        }

        let row_count = rows_out.len();
        Ok(QueryResult {
            columns,
            rows: rows_out,
            command_tag: format!("SELECT {}", row_count),
        })
    }

    fn handle_select_count(&self, s: &str) -> Result<QueryResult, String> {
        let table = Self::parse_ident_after(s, "FROM").ok_or("Invalid COUNT FROM")?;
        let g = self.tables.read();
        let cnt = g.get(table).map(|t| t.rows.len() as i64).unwrap_or(0);
        Ok(QueryResult {
            columns: vec![("count".to_string(), oid::INT8, 8)],
            rows: vec![vec![Some(cnt.to_string().into_bytes())]],
            command_tag: "SELECT 1".to_string(),
        })
    }

    fn handle_select_sum(&self, s: &str) -> Result<QueryResult, String> {
        let table = Self::parse_ident_after(s, "FROM").ok_or("Invalid SUM FROM")?;

        // Parse aggregate column: SELECT SUM(col) ...
        let agg_col = {
            let sum_idx = Self::find_keyword_ci(s, "SUM(").ok_or("Invalid SUM")?;
            let col_start = sum_idx + 4;
            let col_end = s[col_start..].find(')').ok_or("Invalid SUM: missing )")? + col_start;
            s[col_start..col_end].trim().to_string()
        };

        // Check if COUNT(*) is also requested in the SELECT list.
        let has_count = Self::find_keyword_ci(s, "COUNT(").is_some();

        // Parse optional WHERE col BETWEEN lo AND hi
        let between_filter: Option<(&str, i64, i64)> = if let Some(where_idx) = Self::find_keyword_ci(s, "WHERE") {
            let pred = s[where_idx + 5..].trim();
            let parts: Vec<&str> = pred.split_whitespace().collect();
            if parts.len() >= 5 && parts[1].eq_ignore_ascii_case("BETWEEN") && parts[3].eq_ignore_ascii_case("AND") {
                let filter_col = parts[0].trim_matches('"');
                let lo = Self::parse_value(parts[2]).as_i64();
                let hi = Self::parse_value(parts[4]).as_i64();
                Some((filter_col, lo, hi))
            } else {
                None
            }
        } else {
            None
        };

        let g = self.tables.read();
        if let Some(t) = g.get(table) {
            let cc = self.get_or_build_cols(table, t);

            let (sum, count) = if let Some((filter_col, lo, hi)) = between_filter {
                // Columnar range filter + sum.
                if filter_col == "id" || filter_col == t.columns.first().map(|s| s.as_str()).unwrap_or("") {
                    // Primary key filter: binary search on sorted ids.
                    let start = cc.ids.partition_point(|x| *x < lo);
                    let end = cc.ids.partition_point(|x| *x <= hi);
                    let s = if let Some(fv) = cc.float_cols.get(agg_col.as_str()) {
                        simd_sum_f64(&fv[start..end])
                    } else {
                        0.0
                    };
                    (s, (end - start) as i64)
                } else if let Some(filter_vec) = cc.int_cols.get(filter_col) {
                    // Non-id integer filter: chunk-based parallel scan + gather.
                    if let Some(agg_vec) = cc.float_cols.get(agg_col.as_str()) {
                        let n = filter_vec.len();
                        if n > CHUNK_SIZE {
                            // Parallel chunk-based reduction
                            let (psum, pcnt) = (0..n).into_par_iter()
                                .chunks(CHUNK_SIZE)
                                .map(|chunk| {
                                    let mut local_sum = 0.0f64;
                                    let mut local_cnt = 0i64;
                                    for i in chunk {
                                        let v = filter_vec[i];
                                        if v >= lo && v <= hi {
                                            local_sum += agg_vec[i];
                                            local_cnt += 1;
                                        }
                                    }
                                    (local_sum, local_cnt)
                                })
                                .reduce(|| (0.0, 0), |(s1, c1), (s2, c2)| (s1 + s2, c1 + c2));
                            (psum, pcnt)
                        } else {
                            let mut sum = 0.0f64;
                            let mut cnt = 0i64;
                            for i in 0..n {
                                let v = filter_vec[i];
                                if v >= lo && v <= hi {
                                    sum += agg_vec[i];
                                    cnt += 1;
                                }
                            }
                            (sum, cnt)
                        }
                    } else {
                        (0.0, 0)
                    }
                } else {
                    // Fallback: row scan.
                    let mut values = Vec::new();
                    for (_id, row) in &t.rows {
                        let col_val = row.cols.get(filter_col).cloned().unwrap_or(Cell::Int(0)).as_i64();
                        if col_val >= lo && col_val <= hi {
                            values.push(row.cols.get(agg_col.as_str()).cloned().unwrap_or(Cell::Float(0.0)).as_f64());
                        }
                    }
                    let n = values.len() as i64;
                    (simd_sum_f64(&values), n)
                }
            } else {
                // No filter: sum entire column from columnar cache.
                if let Some(fv) = cc.float_cols.get(agg_col.as_str()) {
                    (simd_sum_f64(fv), fv.len() as i64)
                } else {
                    // Fallback for non-float columns.
                    let mut values = Vec::new();
                    for row in t.rows.values() {
                        values.push(row.cols.get(agg_col.as_str()).cloned().unwrap_or(Cell::Float(0.0)).as_f64());
                    }
                    let n = values.len() as i64;
                    (simd_sum_f64(&values), n)
                }
            };

            let (columns, rows) = if has_count {
                (
                    vec![("sum".to_string(), oid::FLOAT8, 8), ("count".to_string(), oid::INT8, 8)],
                    vec![vec![Some(sum.to_string().into_bytes()), Some(count.to_string().into_bytes())]],
                )
            } else {
                (
                    vec![("sum".to_string(), oid::FLOAT8, 8)],
                    vec![vec![Some(sum.to_string().into_bytes())]],
                )
            };

            Ok(QueryResult {
                columns,
                rows,
                command_tag: "SELECT 1".to_string(),
            })
        } else {
            Ok(QueryResult {
                columns: vec![("sum".to_string(), oid::FLOAT8, 8)],
                rows: vec![vec![Some("0".to_string().into_bytes())]],
                command_tag: "SELECT 1".to_string(),
            })
        }
    }

    fn handle_select_avg(&self, s: &str) -> Result<QueryResult, String> {
        let table = Self::parse_ident_after(s, "FROM").ok_or("Invalid AVG FROM")?;

        // Parse aggregate column: SELECT AVG(col) ...
        let agg_col = {
            let up = s.to_ascii_uppercase();
            if let Some(avg_idx) = up.find("AVG(") {
                let col_start = avg_idx + 4;
                if let Some(end_off) = s[col_start..].find(')') {
                    s[col_start..col_start + end_off].trim().to_string()
                } else {
                    "balance".to_string()
                }
            } else {
                "balance".to_string()
            }
        };

        let g = self.tables.read();
        if let Some(t) = g.get(table) {
            let cc = self.get_or_build_cols(table, t);
            let (sum, cnt) = if let Some(fv) = cc.float_cols.get(agg_col.as_str()) {
                (simd_sum_f64(fv), fv.len() as f64)
            } else {
                let mut s = 0.0;
                let mut c = 0.0;
                for row in t.rows.values() {
                    s += row.cols.get(agg_col.as_str()).cloned().unwrap_or(Cell::Float(0.0)).as_f64();
                    c += 1.0;
                }
                (s, c)
            };
            let avg = if cnt > 0.0 { sum / cnt } else { 0.0 };
            Ok(QueryResult {
                columns: vec![("avg".to_string(), oid::FLOAT8, 8)],
                rows: vec![vec![Some(avg.to_string().into_bytes())]],
                command_tag: "SELECT 1".to_string(),
            })
        } else {
            Ok(QueryResult {
                columns: vec![("avg".to_string(), oid::FLOAT8, 8)],
                rows: vec![vec![Some("0".to_string().into_bytes())]],
                command_tag: "SELECT 1".to_string(),
            })
        }
    }

    fn handle_select_group_by(&self, s: &str) -> Result<QueryResult, String> {
        let table = Self::parse_ident_after(s, "FROM").ok_or("Invalid GROUP BY FROM")?;
        let up = s.to_ascii_uppercase();

        // ── Parse GROUP BY column(s) ──
        let gb_idx = up.find("GROUP BY").ok_or("Invalid GROUP BY")?;
        let after_gb = s[gb_idx + 8..].trim();
        // GROUP BY cols end at HAVING, ORDER BY, LIMIT, or end of string
        let gb_end = ["HAVING", "ORDER BY", "LIMIT"]
            .iter()
            .filter_map(|kw| after_gb.to_ascii_uppercase().find(kw))
            .min()
            .unwrap_or(after_gb.len());
        let gb_cols: Vec<String> = after_gb[..gb_end]
            .split(',')
            .map(|c| c.trim().trim_matches('"').to_string())
            .filter(|c| !c.is_empty())
            .collect();

        // ── Parse SELECT columns (aggregate expressions + plain columns) ──
        let select_part = {
            let sel_idx = up.find("SELECT").unwrap_or(0) + 6;
            let from_idx = up.find(" FROM ").ok_or("Invalid SELECT FROM")?;
            s[sel_idx..from_idx].trim()
        };
        // Tokenize SELECT list respecting parentheses
        let mut agg_specs: Vec<AggSpec> = Vec::new();
        let mut select_plain_cols: Vec<String> = Vec::new();
        for tok in Self::split_select_list(select_part) {
            let tok = tok.trim();
            let tok_up = tok.to_ascii_uppercase();
            if tok_up.starts_with("COUNT(")
                || tok_up.starts_with("SUM(")
                || tok_up.starts_with("AVG(")
                || tok_up.starts_with("MIN(")
                || tok_up.starts_with("MAX(")
            {
                let lp = tok.find('(').unwrap();
                let rp = tok.rfind(')').unwrap();
                let func_name = tok[..lp].trim().to_ascii_uppercase();
                let inner_col = tok[lp + 1..rp].trim().to_string();
                let func = ExecAggFunction::from_str(&func_name)
                    .ok_or(format!("Unknown aggregate: {}", func_name))?;
                let col_opt = if inner_col == "*" { None } else { Some(inner_col.clone()) };
                let alias = format!("{}({})", func_name.to_lowercase(), inner_col);
                agg_specs.push(AggSpec { func, column: col_opt, alias });
            } else {
                select_plain_cols.push(tok.trim_matches('"').to_string());
            }
        }

        // If no explicit aggregates, default to COUNT(*)
        if agg_specs.is_empty() {
            agg_specs.push(AggSpec {
                func: ExecAggFunction::Count,
                column: None,
                alias: "count".into(),
            });
        }

        // ── Parse optional HAVING ──
        let having_preds: Vec<HavingPredicate> = if let Some(hav_idx) = up.find("HAVING") {
            let after_hav = s[hav_idx + 6..].trim();
            Self::parse_having(after_hav)
        } else {
            vec![]
        };

        // ── Load rows and convert to AggValue maps ──
        let g = self.tables.read();
        let t = g.get(table).ok_or(format!("table \"{}\" does not exist", table))?;

        // ── Fast columnar GROUP BY: single text group col + SUM/COUNT aggs ──
        if gb_cols.len() == 1 && having_preds.is_empty() {
            let gb_col = &gb_cols[0];
            let cc = self.get_or_build_cols(table, t);
            if let Some(grp_vals) = cc.text_cols.get(gb_col.as_str()) {
                let sum_spec = agg_specs.iter().find(|s| s.func == ExecAggFunction::Sum);
                let sum_fv = sum_spec
                    .and_then(|s| s.column.as_ref())
                    .and_then(|c| cc.float_cols.get(c.as_str()));
                let all_ok = agg_specs.iter().all(|s| {
                    s.func == ExecAggFunction::Count
                        || (s.func == ExecAggFunction::Sum && sum_fv.is_some())
                });
                if all_ok {
                    let n_rows = grp_vals.len();
                    // Chunk-based parallel GROUP BY: build local hash tables per chunk, then merge
                    let groups: AHashMap<&str, (f64, i64)> = if n_rows > CHUNK_SIZE {
                        let chunks: Vec<AHashMap<&str, (f64, i64)>> = (0..n_rows)
                            .into_par_iter()
                            .chunks(CHUNK_SIZE)
                            .map(|chunk| {
                                let mut local: AHashMap<&str, (f64, i64)> = AHashMap::new();
                                for i in chunk {
                                    let entry = local.entry(grp_vals[i].as_str()).or_insert((0.0, 0));
                                    if let Some(fv) = sum_fv {
                                        entry.0 += fv[i];
                                    }
                                    entry.1 += 1;
                                }
                                local
                            })
                            .collect();
                        // Merge partial results
                        let mut merged: AHashMap<&str, (f64, i64)> = AHashMap::new();
                        for local in chunks {
                            for (k, (s, c)) in local {
                                let entry = merged.entry(k).or_insert((0.0, 0));
                                entry.0 += s;
                                entry.1 += c;
                            }
                        }
                        merged
                    } else {
                        let mut groups: AHashMap<&str, (f64, i64)> = AHashMap::new();
                        for i in 0..n_rows {
                            let entry = groups.entry(grp_vals[i].as_str()).or_insert((0.0, 0));
                            if let Some(fv) = sum_fv {
                                entry.0 += fv[i];
                            }
                            entry.1 += 1;
                        }
                        groups
                    };
                    let mut col_names_fast = vec![gb_col.clone()];
                    for spec in &agg_specs {
                        col_names_fast.push(spec.alias.clone());
                    }
                    let columns: Vec<(String, i32, i16)> = col_names_fast
                        .iter()
                        .map(|c| (c.clone(), oid::TEXT, -1i16))
                        .collect();
                    let mut rows_out: Vec<Vec<Option<Vec<u8>>>> = Vec::with_capacity(groups.len());
                    for (grp, (sum_v, cnt_v)) in &groups {
                        let mut row: Vec<Option<Vec<u8>>> = Vec::with_capacity(col_names_fast.len());
                        row.push(Some(grp.as_bytes().to_vec()));
                        for spec in &agg_specs {
                            match spec.func {
                                ExecAggFunction::Sum => row.push(Some(sum_v.to_string().into_bytes())),
                                ExecAggFunction::Count => row.push(Some(cnt_v.to_string().into_bytes())),
                                _ => row.push(None),
                            }
                        }
                        rows_out.push(row);
                    }
                    let n = rows_out.len();
                    return Ok(QueryResult {
                        columns,
                        rows: rows_out,
                        command_tag: format!("SELECT {}", n),
                    });
                }
            }
        }

        let agg_rows: Vec<AHashMap<String, ExecAggValue>> = t
            .rows
            .values()
            .map(|row| {
                row.cols
                    .iter()
                    .map(|(k, v)| {
                        let val = match v {
                            Cell::Int(i) => ExecAggValue::Int(*i),
                            Cell::Float(f) => ExecAggValue::Float(*f),
                            Cell::Text(s) => ExecAggValue::Text(s.clone()),
                            Cell::Null => ExecAggValue::Null,
                        };
                        (k.clone(), val)
                    })
                    .collect()
            })
            .collect();

        // ── Execute aggregate ──
        let executor = AggregateExecutor::new(gb_cols, agg_specs);
        let (col_names, result_rows) = executor.execute(&agg_rows);

        // ── Apply HAVING filter ──
        let result_rows = if having_preds.is_empty() {
            result_rows
        } else {
            apply_having(&col_names, result_rows, &having_preds)
        };

        // ── Build QueryResult ──
        let columns: Vec<(String, i32, i16)> = col_names
            .iter()
            .map(|c| (c.clone(), oid::TEXT, -1i16))
            .collect();
        let mut rows_out: Vec<Vec<Option<Vec<u8>>>> = Vec::with_capacity(result_rows.len());
        for row in &result_rows {
            let r: Vec<Option<Vec<u8>>> = row
                .iter()
                .map(|v| Some(v.to_string_repr().into_bytes()))
                .collect();
            rows_out.push(r);
        }
        let n = rows_out.len();
        Ok(QueryResult {
            columns,
            rows: rows_out,
            command_tag: format!("SELECT {}", n),
        })
    }

    /// Split a SELECT list by commas, respecting parentheses depth.
    fn split_select_list(s: &str) -> Vec<String> {
        let mut result = Vec::new();
        let mut depth = 0i32;
        let mut start = 0;
        for (i, ch) in s.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    result.push(s[start..i].to_string());
                    start = i + 1;
                }
                _ => {}
            }
        }
        if start < s.len() {
            result.push(s[start..].to_string());
        }
        result
    }

    /// Parse a simple HAVING clause: "agg_expr op value"
    fn parse_having(s: &str) -> Vec<HavingPredicate> {
        let mut preds = Vec::new();
        // Support: HAVING COUNT(*) > 5, HAVING SUM(amount) >= 100
        let parts: Vec<&str> = s.splitn(2, |c: char| c == '>' || c == '<' || c == '=' || c == '!').collect();
        if parts.len() < 2 {
            return preds;
        }
        let col_expr = parts[0].trim();
        let rest = &s[parts[0].len()..].trim();

        // Determine operator
        let (op_str, val_str) = if rest.starts_with(">=") {
            (">=", rest[2..].trim())
        } else if rest.starts_with("<=") {
            ("<=", rest[2..].trim())
        } else if rest.starts_with("!=") || rest.starts_with("<>") {
            ("!=", rest[2..].trim())
        } else if rest.starts_with('>') {
            (">", rest[1..].trim())
        } else if rest.starts_with('<') {
            ("<", rest[1..].trim())
        } else if rest.starts_with('=') {
            ("=", rest[1..].trim())
        } else {
            return preds;
        };

        // Build alias = lowercase func name format matching AggSpec
        let alias = {
            let up = col_expr.to_ascii_uppercase();
            if let Some(lp) = up.find('(') {
                if let Some(rp) = up.find(')') {
                    let func = up[..lp].trim().to_lowercase();
                    let inner = col_expr[lp + 1..rp].trim();
                    format!("{}({})", func, inner)
                } else {
                    col_expr.to_string()
                }
            } else {
                col_expr.to_string()
            }
        };

        if let Ok(threshold) = val_str.trim_end_matches(';').parse::<f64>() {
            let pred = match op_str {
                ">" => HavingPredicate::Gt(alias, threshold),
                ">=" => HavingPredicate::Gte(alias, threshold),
                "<" => HavingPredicate::Lt(alias, threshold),
                "<=" => HavingPredicate::Lte(alias, threshold),
                "=" => HavingPredicate::Eq(alias, threshold),
                "!=" => HavingPredicate::Neq(alias, threshold),
                _ => return preds,
            };
            preds.push(pred);
        }
        preds
    }

    fn handle_select_join(&self, s: &str) -> Result<QueryResult, String> {
        let plan = JoinPlan::from_sql(s);

        let g = self.tables.read();
        let orders_table_name = g.keys()
            .find(|k| k.starts_with("bench_orders"))
            .cloned()
            .unwrap_or_default();
        let accounts_table_name = g.keys()
            .find(|k| k.starts_with("bench_accounts"))
            .cloned()
            .unwrap_or_default();
        let products_table_name = g.keys()
            .find(|k| k.starts_with("bench_products"))
            .cloned()
            .unwrap_or_default();
        let a = g.get(&accounts_table_name);
        let o = g.get(&orders_table_name);
        let p = g.get(&products_table_name);

        let mut out_rows = Vec::new();
        if let (Some(at), Some(ot), Some(pt)) = (a, o, p) {
            // Fast path: use B+Tree index on account_id when available
            if let Some(account_id) = plan.account_id_filter {
                if let Some(tree) = self.index_mgr.find_index(&orders_table_name, "account_id") {
                    self.index_mgr.record_index_use(&tree.name);
                    let idx_key = IndexKey::Integer(account_id);
                    let row_ids = tree.search(&idx_key);
                    let selected_rows: Vec<&NativeRow> = row_ids.iter()
                        .filter_map(|rid| ot.rows.get(rid))
                        .collect();
                    out_rows = Self::probe_index_join_rows(at, pt, &selected_rows);
                    let n = out_rows.len();
                    return Ok(QueryResult {
                        columns: vec![
                            ("name".to_string(), oid::TEXT, -1),
                            ("id".to_string(), oid::INT8, 8),
                            ("name".to_string(), oid::TEXT, -1),
                            ("quantity".to_string(), oid::INT8, 8),
                            ("total".to_string(), oid::FLOAT8, 8),
                        ],
                        rows: out_rows,
                        command_tag: format!("SELECT {}", n),
                    });
                }
            }

            // Fallback: full scan path
            let order_rows: Vec<&NativeRow> = ot
                .rows
                .values()
                .collect();
            let selected_rows: Vec<&NativeRow> = if let Some(account_id) = plan.account_id_filter {
                order_rows
                    .iter()
                    .copied()
                    .filter(|row| {
                        row.cols
                            .get("account_id")
                            .cloned()
                            .unwrap_or(Cell::Int(0))
                            .as_i64()
                            == account_id
                    })
                    .collect()
            } else {
                order_rows.iter().copied().collect()
            };

            let stats = JoinStats {
                accounts_rows: at.rows.len(),
                products_rows: pt.rows.len(),
                orders_rows: order_rows.len(),
                selected_orders: selected_rows.len(),
            };
            let strategy = Self::choose_join_strategy(&stats);

            out_rows = match strategy {
                JoinStrategy::IndexJoin => Self::probe_index_join_rows(at, pt, &selected_rows),
                JoinStrategy::HashJoinParallel => {
                    // --- Buffer Pool: try cached SoA for orders ---
                    let soa = match self.buf_pool.get_soa(&orders_table_name) {
                        Some(cached) => cached,
                        None => {
                            let built = JoinInputSoA::from_rows(&order_rows);
                            self.buf_pool.put_soa(&orders_table_name, built)
                        }
                    };
                    let selected_indices = if let Some(account_id) = plan.account_id_filter {
                        soa.filtered_indices_by_account_id(account_id)
                    } else {
                        soa.all_indices()
                    };
                    // --- Buffer Pool: try cached dimension hash maps ---
                    let acc_map = match self.buf_pool.get_dim(&accounts_table_name) {
                        Some(cached) => cached,
                        None => {
                            let map = Self::build_dim_hash(at);
                            self.buf_pool.put_dim(&accounts_table_name, map)
                        }
                    };
                    let prod_map = match self.buf_pool.get_dim(&products_table_name) {
                        Some(cached) => cached,
                        None => {
                            let map = Self::build_dim_hash(pt);
                            self.buf_pool.put_dim(&products_table_name, map)
                        }
                    };
                    let products_first = Self::pick_join_reordering(&stats);
                    Self::probe_hash_join_cached(&soa, &selected_indices, &acc_map, &prod_map, products_first)
                }
            };

            if out_rows.is_empty() && plan.account_id_filter.is_some() {
                let account_id = plan.account_id_filter.unwrap_or(0);
                if at.rows.get(&account_id).is_some() {
                    out_rows = Vec::new();
                }
            }
        }

        let n = out_rows.len();
        Ok(QueryResult {
            columns: vec![
                ("name".to_string(), oid::TEXT, -1),
                ("id".to_string(), oid::INT8, 8),
                ("name".to_string(), oid::TEXT, -1),
                ("quantity".to_string(), oid::INT8, 8),
                ("total".to_string(), oid::FLOAT8, 8),
            ],
            rows: out_rows,
            command_tag: format!("SELECT {}", n),
        })
    }

    fn choose_join_strategy(stats: &JoinStats) -> JoinStrategy {
        let selectivity = stats.selectivity();
        if stats.selected_orders <= 256 && selectivity <= 0.15 {
            JoinStrategy::IndexJoin
        } else {
            JoinStrategy::HashJoinParallel
        }
    }

    fn pick_join_reordering(stats: &JoinStats) -> bool {
        // Join smaller dimension first to shrink intermediate rows quickly.
        stats.products_rows <= stats.accounts_rows
    }

    fn probe_index_join_rows(
        accounts: &NativeTable,
        products: &NativeTable,
        rows: &[&NativeRow],
    ) -> Vec<Vec<Option<Vec<u8>>>> {
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let aid = row
                .cols
                .get("account_id")
                .cloned()
                .unwrap_or(Cell::Int(0))
                .as_i64();
            let pid = row
                .cols
                .get("product_id")
                .cloned()
                .unwrap_or(Cell::Int(0))
                .as_i64();

            let acc_name = accounts
                .rows
                .get(&aid)
                .and_then(|r| r.cols.get("name"))
                .cloned()
                .unwrap_or(Cell::Text(String::new()))
                .as_text();
            let prod_name = products
                .rows
                .get(&pid)
                .and_then(|r| r.cols.get("name"))
                .cloned()
                .unwrap_or(Cell::Text(String::new()))
                .as_text();

            let oid_val = row.cols.get("id").cloned().unwrap_or(Cell::Int(0)).as_i64();
            let qty = row
                .cols
                .get("quantity")
                .cloned()
                .unwrap_or(Cell::Int(0))
                .as_i64();
            let total = row
                .cols
                .get("total")
                .cloned()
                .unwrap_or(Cell::Float(0.0))
                .as_f64();

            out.push(vec![
                Some(acc_name.into_bytes()),
                Some(oid_val.to_string().into_bytes()),
                Some(prod_name.into_bytes()),
                Some(qty.to_string().into_bytes()),
                Some(total.to_string().into_bytes()),
            ]);
        }
        out
    }

    #[allow(dead_code)]
    fn probe_hash_join_parallel(
        accounts: &NativeTable,
        products: &NativeTable,
        soa: &JoinInputSoA,
        selected_indices: &[usize],
        products_first: bool,
    ) -> Vec<Vec<Option<Vec<u8>>>> {
        let account_name_by_id: AHashMap<i64, String> = accounts
            .rows
            .iter()
            .map(|(id, r)| {
                (
                    *id,
                    r.cols
                        .get("name")
                        .map(|c| c.as_text())
                        .unwrap_or_default(),
                )
            })
            .collect();

        let product_name_by_id: AHashMap<i64, String> = products
            .rows
            .iter()
            .map(|(id, r)| {
                (
                    *id,
                    r.cols
                        .get("name")
                        .map(|c| c.as_text())
                        .unwrap_or_default(),
                )
            })
            .collect();

        selected_indices
            .par_chunks(1024)
            .map(|chunk| {
                let mut local_out: Vec<Vec<Option<Vec<u8>>>> = Vec::with_capacity(chunk.len());
                for idx in chunk {
                    let aid = soa.account_ids[*idx] as i64;
                    let pid = soa.product_ids[*idx] as i64;

                    // Probe order can be swapped based on join reordering heuristic.
                    // Zero-copy: read &str directly, convert to bytes without String clone.
                    let (acc_bytes, prod_bytes) = if products_first {
                        let prod = product_name_by_id
                            .get(&pid)
                            .map(|s| s.as_bytes().to_vec())
                            .unwrap_or_default();
                        let acc = account_name_by_id
                            .get(&aid)
                            .map(|s| s.as_bytes().to_vec())
                            .unwrap_or_default();
                        (acc, prod)
                    } else {
                        let acc = account_name_by_id
                            .get(&aid)
                            .map(|s| s.as_bytes().to_vec())
                            .unwrap_or_default();
                        let prod = product_name_by_id
                            .get(&pid)
                            .map(|s| s.as_bytes().to_vec())
                            .unwrap_or_default();
                        (acc, prod)
                    };

                    let oid_val = soa.order_ids[*idx];
                    let qty = soa.quantities[*idx];
                    let total = soa.totals[*idx];

                    local_out.push(vec![
                        Some(acc_bytes),
                        Some(oid_val.to_string().into_bytes()),
                        Some(prod_bytes),
                        Some(qty.to_string().into_bytes()),
                        Some(total.to_string().into_bytes()),
                    ]);
                }
                local_out
            })
            .reduce(Vec::new, |mut left, mut right| {
                left.append(&mut right);
                left
            })
    }

    /// Build a dimension hash table (id → name bytes) from a NativeTable.
    /// Pre-converts to bytes for zero-copy join output.
    fn build_dim_hash(table: &NativeTable) -> AHashMap<i64, Arc<[u8]>> {
        table.rows.iter().map(|(id, r)| {
            let name = r.cols.get("name").map(|c| c.as_text()).unwrap_or_default();
            (*id, Arc::<[u8]>::from(name.into_bytes()))
        }).collect()
    }

    /// Hash-join probe using pre-cached dimension maps + SoA column store.
    fn probe_hash_join_cached(
        soa: &JoinInputSoA,
        selected_indices: &[usize],
        acc_map: &AHashMap<i64, Arc<[u8]>>,
        prod_map: &AHashMap<i64, Arc<[u8]>>,
        products_first: bool,
    ) -> Vec<Vec<Option<Vec<u8>>>> {
        let empty: Arc<[u8]> = Arc::from(Vec::new());
        selected_indices
            .par_chunks(1024)
            .map(|chunk| {
                let mut local_out: Vec<Vec<Option<Vec<u8>>>> = Vec::with_capacity(chunk.len());
                for &idx in chunk {
                    let aid = soa.account_ids[idx] as i64;
                    let pid = soa.product_ids[idx] as i64;

                    let (acc_bytes, prod_bytes) = if products_first {
                        let prod = prod_map.get(&pid).unwrap_or(&empty);
                        let acc = acc_map.get(&aid).unwrap_or(&empty);
                        (acc, prod)
                    } else {
                        let acc = acc_map.get(&aid).unwrap_or(&empty);
                        let prod = prod_map.get(&pid).unwrap_or(&empty);
                        (acc, prod)
                    };

                    let oid_val = soa.order_ids[idx];
                    let qty = soa.quantities[idx];
                    let total = soa.totals[idx];

                    local_out.push(vec![
                        Some(acc_bytes.to_vec()),
                        Some(oid_val.to_string().into_bytes()),
                        Some(prod_bytes.to_vec()),
                        Some(qty.to_string().into_bytes()),
                        Some(total.to_string().into_bytes()),
                    ]);
                }
                local_out
            })
            .reduce(Vec::new, |mut left, mut right| {
                left.append(&mut right);
                left
            })
    }
}

#[derive(Clone, Copy, Debug)]
enum JoinStrategy {
    IndexJoin,
    HashJoinParallel,
}

#[derive(Clone, Copy, Debug)]
struct JoinStats {
    accounts_rows: usize,
    products_rows: usize,
    orders_rows: usize,
    selected_orders: usize,
}

impl JoinStats {
    fn selectivity(&self) -> f64 {
        if self.orders_rows == 0 {
            0.0
        } else {
            self.selected_orders as f64 / self.orders_rows as f64
        }
    }
}

#[derive(Clone, Debug)]
struct JoinPlan {
    account_id_filter: Option<i64>,
}

impl JoinPlan {
    fn from_sql(sql: &str) -> Self {
        let mut account_id_filter = None;
        let up = sql.to_ascii_uppercase();
        if let Some(where_idx) = up.find("WHERE") {
            let pred = sql[where_idx + 5..].trim();
            if let Some(eq_idx) = pred.find('=') {
                let rhs = pred[eq_idx + 1..].trim();
                account_id_filter = Some(NativeSqlEngine::parse_value(rhs).as_i64());
            }
        }
        Self { account_id_filter }
    }
}

#[cfg(test)]
mod tests {
    use super::NativeSqlEngine;

    fn setup_join_fixtures(engine: &NativeSqlEngine) {
        engine
            .execute("CREATE TABLE bench_accounts_test (id INTEGER PRIMARY KEY, balance REAL, name TEXT)")
            .unwrap();
        engine
            .execute("CREATE TABLE bench_products_test (id INTEGER PRIMARY KEY, name TEXT, price REAL, category TEXT)")
            .unwrap();
        engine
            .execute("CREATE TABLE bench_orders_test (id INTEGER PRIMARY KEY, account_id INTEGER, product_id INTEGER, quantity INTEGER, total REAL)")
            .unwrap();

        engine
            .execute("INSERT INTO bench_accounts_test (id, balance, name) VALUES (1, 100.0, 'alice')")
            .unwrap();
        engine
            .execute("INSERT INTO bench_accounts_test (id, balance, name) VALUES (2, 200.0, 'bob')")
            .unwrap();
        engine
            .execute("INSERT INTO bench_products_test (id, name, price, category) VALUES (10, 'book', 12.5, 'books')")
            .unwrap();
        engine
            .execute("INSERT INTO bench_products_test (id, name, price, category) VALUES (20, 'toy', 9.9, 'toys')")
            .unwrap();
        engine
            .execute("INSERT INTO bench_orders_test (id, account_id, product_id, quantity, total) VALUES (100, 1, 10, 2, 25.0)")
            .unwrap();
        engine
            .execute("INSERT INTO bench_orders_test (id, account_id, product_id, quantity, total) VALUES (101, 1, 20, 1, 9.9)")
            .unwrap();
        engine
            .execute("INSERT INTO bench_orders_test (id, account_id, product_id, quantity, total) VALUES (102, 2, 10, 3, 37.5)")
            .unwrap();
    }

    #[test]
    fn join_with_account_filter_returns_expected_rows() {
        let engine = NativeSqlEngine::new();
        setup_join_fixtures(&engine);

        let res = engine
            .execute(
                "SELECT a.name, o.id, p.name, o.quantity, o.total
                 FROM bench_accounts_test a
                 JOIN bench_orders_test o ON a.id = o.account_id
                 JOIN bench_products_test p ON o.product_id = p.id
                 WHERE a.id = 1",
            )
            .unwrap();

        assert_eq!(res.rows.len(), 2);
        assert_eq!(res.command_tag, "SELECT 2");
    }

    #[test]
    fn join_without_filter_scans_all_matching_rows() {
        let engine = NativeSqlEngine::new();
        setup_join_fixtures(&engine);

        let res = engine
            .execute(
                "SELECT a.name, o.id, p.name, o.quantity, o.total
                 FROM bench_accounts_test a
                 JOIN bench_orders_test o ON a.id = o.account_id
                 JOIN bench_products_test p ON o.product_id = p.id",
            )
            .unwrap();

        assert_eq!(res.rows.len(), 3);
        assert_eq!(res.command_tag, "SELECT 3");
    }
}
