use crate::hub_engine::errors::ExecError;
use crate::hub_engine::planner::PhysicalPlan;
use crate::hub_engine::types::QueryResult;
use crate::storage::StorageEngine;
use hashbrown::HashMap;
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::{BTreeSet, BinaryHeap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub type Row = HashMap<String, String>;

// ============================================================================
// TopN Sort Module - High Performance ORDER BY ... LIMIT N
// ============================================================================

/// Sort direction for ORDER BY
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

/// Sort key extracted from a row for efficient comparison
#[derive(Debug, Clone)]
pub enum SortKey {
    Int(i64),
    Float(f64),
    Text(String),
    Null,
}

impl SortKey {
    /// Parse a string value into a typed sort key
    #[inline]
    pub fn from_str(s: &str) -> Self {
        if s.is_empty() || s == "NULL" {
            return SortKey::Null;
        }
        // Try i64 first (more common for IDs, counts)
        if let Ok(i) = s.parse::<i64>() {
            return SortKey::Int(i);
        }
        // Try f64
        if let Ok(f) = s.parse::<f64>() {
            return SortKey::Float(f);
        }
        // Fall back to text
        SortKey::Text(s.to_string())
    }

    /// SIMD-friendly comparison for numeric types
    /// Returns Ordering with proper NaN and NULL handling
    #[inline]
    fn cmp_internal(&self, other: &Self) -> Ordering {
        match (self, other) {
            // NULL handling: NULLs sort last
            (SortKey::Null, SortKey::Null) => Ordering::Equal,
            (SortKey::Null, _) => Ordering::Greater,
            (_, SortKey::Null) => Ordering::Less,

            // Int comparison - direct, fast
            (SortKey::Int(a), SortKey::Int(b)) => a.cmp(b),

            // Float comparison with proper NaN handling
            // NaN sorts as greater than any number (like NULL)
            (SortKey::Float(a), SortKey::Float(b)) => {
                // Use total_cmp for deterministic NaN handling
                a.total_cmp(b)
            }

            // Cross-type numeric: promote to f64
            (SortKey::Int(a), SortKey::Float(b)) => (*a as f64).total_cmp(b),
            (SortKey::Float(a), SortKey::Int(b)) => a.total_cmp(&(*b as f64)),

            // Text comparison
            (SortKey::Text(a), SortKey::Text(b)) => a.cmp(b),

            // Type mismatch: numeric < text
            (SortKey::Int(_) | SortKey::Float(_), SortKey::Text(_)) => Ordering::Less,
            (SortKey::Text(_), SortKey::Int(_) | SortKey::Float(_)) => Ordering::Greater,
        }
    }
}

/// Entry in the TopN heap: (sort_key, row_index)
/// We use index-based sorting (zero-copy) to avoid cloning rows
struct HeapEntry {
    key: SortKey,
    index: usize,
    direction: SortDirection,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.key.cmp_internal(&other.key) == Ordering::Equal
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
        // For DESC, we want smallest values at the top of max-heap (to evict)
        // For ASC, we want largest values at the top of max-heap (to evict)
        // BinaryHeap is a max-heap, so we invert based on direction
        let base_cmp = self.key.cmp_internal(&other.key);
        match self.direction {
            SortDirection::Desc => base_cmp, // DESC: keep largest, evict smallest
            SortDirection::Asc => base_cmp.reverse(), // ASC: keep smallest, evict largest
        }
    }
}

/// TopN sorter using BinaryHeap with O(I * log N) complexity
/// where I = input rows, N = limit
pub struct TopNSorter {
    heap: BinaryHeap<HeapEntry>,
    limit: usize,
    direction: SortDirection,
    sort_column: String,
}

impl TopNSorter {
    pub fn new(sort_column: String, direction: SortDirection, limit: usize) -> Self {
        Self {
            heap: BinaryHeap::with_capacity(limit + 1),
            limit,
            direction,
            sort_column,
        }
    }

    /// Push a row into the sorter (zero-copy: just stores index)
    /// Returns true if the row might be in the final result
    #[inline]
    pub fn push(&mut self, row: &Row, index: usize) -> bool {
        let key = row
            .get(&self.sort_column)
            .map(|v| SortKey::from_str(v))
            .unwrap_or(SortKey::Null);

        let entry = HeapEntry {
            key,
            index,
            direction: self.direction,
        };

        if self.heap.len() < self.limit {
            self.heap.push(entry);
            true
        } else if let Some(top) = self.heap.peek() {
            // Compare with worst element in heap
            if entry.cmp(top) == Ordering::Less {
                // New entry is better than worst in heap
                self.heap.pop();
                self.heap.push(entry);
                true
            } else {
                false
            }
        } else {
            true
        }
    }

    /// Extract sorted indices (best to worst order)
    pub fn into_sorted_indices(self) -> Vec<usize> {
        let mut entries: Vec<_> = self.heap.into_vec();
        // Sort in correct order (reverse of heap order)
        entries.sort_by(|a, b| b.cmp(a));
        entries.into_iter().map(|e| e.index).collect()
    }
}

/// Execute ORDER BY ... LIMIT using TopN algorithm
/// O(I * log N) complexity instead of O(I * log I) for full sort
pub fn execute_topn_sort(
    rows: Vec<Row>,
    sort_column: &str,
    direction: SortDirection,
    limit: usize,
) -> Vec<Row> {
    if rows.is_empty() || limit == 0 {
        return Vec::new();
    }

    let effective_limit = limit.min(rows.len());
    let mut sorter = TopNSorter::new(sort_column.to_string(), direction, effective_limit);

    // Push all rows through the sorter
    for (idx, row) in rows.iter().enumerate() {
        sorter.push(row, idx);
    }

    // Get sorted indices and collect rows
    let indices = sorter.into_sorted_indices();
    indices.into_iter().map(|i| rows[i].clone()).collect()
}

/// Parallel TopN sort for large datasets
/// Splits data into chunks, sorts each chunk, then merges
pub fn execute_topn_sort_parallel(
    rows: Vec<Row>,
    sort_column: &str,
    direction: SortDirection,
    limit: usize,
) -> Vec<Row> {
    const PARALLEL_THRESHOLD: usize = 10_000;
    const CHUNK_SIZE: usize = 4096;

    if rows.len() < PARALLEL_THRESHOLD {
        return execute_topn_sort(rows, sort_column, direction, limit);
    }

    let effective_limit = limit.min(rows.len());
    let rows = Arc::new(rows);
    let sort_column = sort_column.to_string();

    // Parallel phase: each chunk produces top-N candidates
    let chunk_results: Vec<Vec<usize>> = rows
        .par_chunks(CHUNK_SIZE)
        .enumerate()
        .map(|(chunk_idx, _chunk)| {
            let base_idx = chunk_idx * CHUNK_SIZE;
            let chunk_end = (base_idx + CHUNK_SIZE).min(rows.len());

            let mut sorter = TopNSorter::new(sort_column.clone(), direction, effective_limit);

            for idx in base_idx..chunk_end {
                sorter.push(&rows[idx], idx);
            }

            sorter.into_sorted_indices()
        })
        .collect();

    // Merge phase: combine chunk results
    let mut final_sorter = TopNSorter::new(sort_column.clone(), direction, effective_limit);
    for chunk_indices in chunk_results {
        for idx in chunk_indices {
            final_sorter.push(&rows[idx], idx);
        }
    }

    let final_indices = final_sorter.into_sorted_indices();
    final_indices.into_iter().map(|i| rows[i].clone()).collect()
}

/// SIMD-optimized batch comparison for f64 arrays
/// Uses wide crate for vectorized operations when available
#[cfg(target_arch = "aarch64")]
pub fn simd_compare_f64_batch(a: &[f64], b: &[f64]) -> Vec<Ordering> {
    use wide::{f64x4, CmpGt, CmpLt};

    let len = a.len().min(b.len());
    let mut results = Vec::with_capacity(len);

    let chunks = len / 4;
    let remainder = len % 4;

    for i in 0..chunks {
        let base = i * 4;
        let va = f64x4::from([a[base], a[base + 1], a[base + 2], a[base + 3]]);
        let vb = f64x4::from([b[base], b[base + 1], b[base + 2], b[base + 3]]);

        // Compare using SIMD
        let lt_mask = va.cmp_lt(vb);
        let gt_mask = va.cmp_gt(vb);

        let lt_arr: [f64; 4] = lt_mask.into();
        let gt_arr: [f64; 4] = gt_mask.into();

        for j in 0..4 {
            if lt_arr[j] != 0.0 {
                results.push(Ordering::Less);
            } else if gt_arr[j] != 0.0 {
                results.push(Ordering::Greater);
            } else {
                results.push(Ordering::Equal);
            }
        }
    }

    // Handle remainder
    for i in (chunks * 4)..(chunks * 4 + remainder) {
        results.push(a[i].total_cmp(&b[i]));
    }

    results
}

#[cfg(not(target_arch = "aarch64"))]
pub fn simd_compare_f64_batch(a: &[f64], b: &[f64]) -> Vec<Ordering> {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| x.total_cmp(y))
        .collect()
}

pub async fn execute_physical_plan(
    plan: PhysicalPlan,
    storage: &StorageEngine,
) -> Result<QueryResult, ExecError> {
    match plan {
        PhysicalPlan::PointLookup { .. } => Ok(QueryResult::empty()),
        PhysicalPlan::HashJoin {
            build,
            probe,
            build_key,
            probe_key,
            ..
        } => {
            let build_rows = load_table_rows_from_disk(storage.data_dir(), &build)?;
            let probe_rows = load_table_rows_from_disk(storage.data_dir(), &probe)?;
            execute_parallel_hash_join(build_rows, probe_rows, &build_key, &probe_key)
        }
        PhysicalPlan::TopNSort {
            source,
            sort_column,
            direction,
            limit,
        } => {
            // Execute source plan first
            let source_result = Box::pin(execute_physical_plan(*source, storage)).await?;

            // Convert QueryResult rows back to Row format for sorting
            let rows: Vec<Row> = source_result
                .rows
                .into_iter()
                .map(|row_values| {
                    source_result
                        .columns
                        .iter()
                        .zip(row_values.into_iter())
                        .map(|(col, val)| (col.clone(), val))
                        .collect()
                })
                .collect();

            // Execute TopN sort with parallel optimization
            let sorted_rows = execute_topn_sort_parallel(rows, &sort_column, direction, limit);

            Ok(query_result_from_rows(sorted_rows))
        }
        PhysicalPlan::PassThroughSql { sql } => Err(ExecError::NotImplemented(format!(
            "pass-through SQL execution is not wired yet: {sql}"
        ))),
    }
}

pub(crate) fn load_table_rows_from_disk(
    base_data_dir: &Path,
    table: &str,
) -> Result<Vec<Row>, ExecError> {
    let rows_dir = resolve_rows_dir(base_data_dir)?;
    let mut files: Vec<PathBuf> = fs::read_dir(&rows_dir)
        .map_err(|e| ExecError::PolicyViolation(format!("read rows dir failed: {e}")))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| {
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                return false;
            };
            (name.starts_with(&format!("{table}_")) && name.ends_with(".qmr"))
                || (name.starts_with(&format!("{table}_batch_")) && name.ends_with(".qmb"))
        })
        .collect();

    files.sort();
    let mut out = Vec::new();
    for p in files {
        let bytes = fs::read(&p).map_err(|e| {
            ExecError::PolicyViolation(format!("read row file failed ({}): {e}", p.display()))
        })?;
        let payload = maybe_decompress(bytes);
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.ends_with(".qmr") {
            if let Some(row) = decode_qmr_row(&payload)? {
                out.push(row);
            }
        } else if name.ends_with(".qmb") {
            let mut batch = decode_qmb_rows(&payload)?;
            out.append(&mut batch);
        }
    }
    Ok(out)
}

fn resolve_rows_dir(base_data_dir: &Path) -> Result<PathBuf, ExecError> {
    let candidates = [
        base_data_dir.join("satellites").join("rows"),
        base_data_dir.join("rows"),
    ];
    for c in candidates {
        if c.exists() && c.is_dir() {
            return Ok(c);
        }
    }
    Err(ExecError::PolicyViolation(format!(
        "rows directory not found under {}",
        base_data_dir.display()
    )))
}

fn maybe_decompress(bytes: Vec<u8>) -> Vec<u8> {
    zstd::stream::decode_all(bytes.as_slice()).unwrap_or(bytes)
}

fn decode_qmr_row(payload: &[u8]) -> Result<Option<Row>, ExecError> {
    let v: Value = rmp_serde::from_slice(payload)
        .map_err(|e| ExecError::PolicyViolation(format!("decode .qmr msgpack failed: {e}")))?;
    if let Some(obj) = v.as_object() {
        let mut row = Row::new();
        for (k, v) in obj {
            row.insert(k.clone(), json_value_to_cell(v));
        }
        return Ok(Some(row));
    }
    Ok(None)
}

#[derive(Debug, Deserialize)]
struct BatchEntry(u64, Value);

fn decode_qmb_rows(payload: &[u8]) -> Result<Vec<Row>, ExecError> {
    let entries: Vec<BatchEntry> = rmp_serde::from_slice(payload)
        .map_err(|e| ExecError::PolicyViolation(format!("decode .qmb msgpack failed: {e}")))?;
    let mut out = Vec::with_capacity(entries.len());
    for BatchEntry(_id, val) in entries {
        if let Some(obj) = val.as_object() {
            let mut row = Row::new();
            for (k, v) in obj {
                row.insert(k.clone(), json_value_to_cell(v));
            }
            out.push(row);
        }
    }
    Ok(out)
}

pub fn execute_parallel_hash_join(
    build_rows: Vec<Row>,
    probe_rows: Vec<Row>,
    build_key: &str,
    probe_key: &str,
) -> Result<QueryResult, ExecError> {
    if build_key.is_empty() || probe_key.is_empty() {
        return Err(ExecError::PolicyViolation(
            "join key cannot be empty".to_string(),
        ));
    }

    let build_rows = Arc::new(build_rows);
    let probe_rows = Arc::new(probe_rows);

    let mut hash_table: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, row) in build_rows.iter().enumerate() {
        if let Some(key) = row.get(build_key) {
            hash_table.entry(key.clone()).or_default().push(idx);
        }
    }
    let hash_table = Arc::new(hash_table);

    let joined_rows = probe_rows
        .par_chunks(2048)
        .map(|chunk| {
            let mut out = Vec::new();
            for probe_row in chunk {
                if let Some(probe_val) = probe_row.get(probe_key) {
                    if let Some(build_matches) = hash_table.get(probe_val) {
                        for build_idx in build_matches {
                            out.push(merge_rows(&build_rows[*build_idx], probe_row));
                        }
                    }
                }
            }
            out
        })
        .reduce(Vec::new, |mut left, mut right| {
            left.append(&mut right);
            left
        });

    Ok(query_result_from_rows(joined_rows))
}

pub fn parse_rows_from_json_bytes(bytes: &[u8]) -> Result<Vec<Row>, ExecError> {
    let v: Value = serde_json::from_slice(bytes)
        .map_err(|e| ExecError::PolicyViolation(format!("invalid JSON rows payload: {e}")))?;
    let arr = v.as_array().ok_or_else(|| {
        ExecError::PolicyViolation("rows payload must be a JSON array".to_string())
    })?;

    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let obj = item.as_object().ok_or_else(|| {
            ExecError::PolicyViolation("each row must be a JSON object".to_string())
        })?;
        let mut row = Row::new();
        for (k, v) in obj {
            row.insert(k.clone(), json_value_to_cell(v));
        }
        out.push(row);
    }
    Ok(out)
}

fn json_value_to_cell(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => v.to_string(),
    }
}

fn merge_rows(build: &Row, probe: &Row) -> Row {
    let mut out = build.clone();
    for (k, v) in probe {
        if out.contains_key(k) {
            out.insert(format!("probe_{k}"), v.clone());
        } else {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

pub(crate) fn query_result_from_rows(rows: Vec<Row>) -> QueryResult {
    let mut columns_set = BTreeSet::new();
    for row in &rows {
        for c in row.keys() {
            columns_set.insert(c.clone());
        }
    }
    let columns: Vec<String> = columns_set.into_iter().collect();

    let mut data = Vec::with_capacity(rows.len());
    for row in rows {
        let mut projected = Vec::with_capacity(columns.len());
        for c in &columns {
            projected.push(row.get(c).cloned().unwrap_or_default());
        }
        data.push(projected);
    }

    QueryResult {
        columns,
        affected_rows: data.len() as u64,
        rows: data,
    }
}
