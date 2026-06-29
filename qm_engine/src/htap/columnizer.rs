//! Columnizer — sync column segments from committed row state at LSN.

use crate::gateway::native_sql::{Cell, NativeTable};
use crate::htap::column_segment::ColumnSegmentStore;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Optional per-phase timers for `columnize_table` (wired from `NativeSqlProfileCounters`).
pub struct ColumnizePhaseProfile<'a> {
    pub collect_rows_ns: &'a AtomicU64,
    pub sort_rows_ns: &'a AtomicU64,
    pub extract_columns_ns: &'a AtomicU64,
    pub write_segments_ns: &'a AtomicU64,
}

impl<'a> ColumnizePhaseProfile<'a> {
    fn record(&self, counter: &'a AtomicU64, start: Instant) {
        counter.fetch_add(
            start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
    }
}

pub struct Columnizer;

impl Columnizer {
    pub fn new() -> Self {
        Self
    }

    pub fn columnize_table(
        &self,
        store: &ColumnSegmentStore,
        table: &str,
        native: &NativeTable,
        commit_lsn: u64,
        phase_profile: Option<&ColumnizePhaseProfile<'_>>,
    ) -> Result<usize, String> {
        let mut written = 0usize;
        for (col_idx, col_name) in native.columns.iter().enumerate() {
            let Some(ct) = native.column_types.get(col_idx) else {
                continue;
            };
            if !matches!(
                ct,
                crate::gateway::native_sql::ColType::Integer
                    | crate::gateway::native_sql::ColType::Float8
            ) {
                continue;
            }
            let collect_start = phase_profile.map(|_| Instant::now());
            let mut row_ids: Vec<i64> = native.rows.keys().copied().collect();
            if let (Some(profile), Some(start)) = (phase_profile, collect_start) {
                profile.record(profile.collect_rows_ns, start);
            }
            let sort_start = phase_profile.map(|_| Instant::now());
            row_ids.sort_unstable();
            if let (Some(profile), Some(start)) = (phase_profile, sort_start) {
                profile.record(profile.sort_rows_ns, start);
            }
            let extract_start = phase_profile.map(|_| Instant::now());
            let mut f64s = Vec::with_capacity(row_ids.len());
            for row_id in row_ids {
                let v = native
                    .rows
                    .get(&row_id)
                    .and_then(|row| row.cols.get(col_name.as_str()))
                    .map(|cell| match cell {
                        Cell::Int(i) => *i as f64,
                        Cell::Float(f) => *f,
                        _ => 0.0,
                    })
                    .unwrap_or(0.0);
                f64s.push(v);
            }
            if let (Some(profile), Some(start)) = (phase_profile, extract_start) {
                profile.record(profile.extract_columns_ns, start);
            }
            if f64s.is_empty() {
                continue;
            }
            let write_start = phase_profile.map(|_| Instant::now());
            store.append_f64_column(table, col_name, commit_lsn, commit_lsn, &f64s)?;
            if let (Some(profile), Some(start)) = (phase_profile, write_start) {
                profile.record(profile.write_segments_ns, start);
            }
            written += 1;
        }
        Ok(written)
    }
}

impl Default for Columnizer {
    fn default() -> Self {
        Self::new()
    }
}
