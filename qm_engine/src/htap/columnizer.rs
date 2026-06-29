//! Columnizer — sync column segments from committed row state at LSN.

use crate::gateway::native_sql::{Cell, NativeTable};
use crate::htap::column_segment::ColumnSegmentStore;

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
            let mut row_ids: Vec<i64> = native.rows.keys().copied().collect();
            row_ids.sort_unstable();
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
            if f64s.is_empty() {
                continue;
            }
            store.append_f64_column(table, col_name, commit_lsn, commit_lsn, &f64s)?;
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
