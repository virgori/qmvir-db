//! `PredictEngine` — estimate backup size and duration.
//!
//! Samples table data to predict compressed backup size without
//! actually writing to disk. Useful for capacity planning.

use serde::Serialize;
use std::io;
use std::time::Instant;

use crate::backup::{compress, Compression, CHUNK_SIZE};
use crate::gateway::native_sql::NativeSqlEngine;

/// Prediction result.
#[derive(Debug, Serialize)]
pub struct PredictResult {
    pub estimated_size_bytes: u64,
    pub estimated_duration_ms: u64,
    pub row_count: u64,
    pub table_count: usize,
    pub compression: String,
}

pub struct PredictEngine<'a> {
    engine: &'a NativeSqlEngine,
}

impl<'a> PredictEngine<'a> {
    pub fn new(engine: &'a NativeSqlEngine) -> Self {
        Self { engine }
    }

    /// Estimate backup size and duration by sampling.
    ///
    /// Strategy: serialize + compress up to SAMPLE_ROWS rows per table,
    /// extrapolate the compression ratio to the full table.
    pub fn predict_backup(&self, compression: Compression) -> io::Result<PredictResult> {
        const SAMPLE_ROWS: usize = 256;
        let start = Instant::now();

        let tables = self.engine.tables.to_native_map();
        let table_count = tables.len();
        let mut total_rows = 0u64;
        let mut estimated_compressed = 0u64;

        // Fixed overhead: header(64) + footer(40) + manifest (~100 per table)
        let overhead = 64 + 40 + (table_count as u64 * 120);
        estimated_compressed += overhead;

        for (_name, table) in tables.iter() {
            let row_count = table.rows.len();
            total_rows += row_count as u64;

            if row_count == 0 {
                continue;
            }

            // Sample up to SAMPLE_ROWS rows.
            let sample_size = row_count.min(SAMPLE_ROWS);
            let sample: Vec<_> = table.rows.values().take(sample_size).collect();

            // Serialize the sample.
            let raw = bincode::serialize(&sample)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
            let compressed = compress(&raw, compression)?;

            // Compression ratio from sample.
            let ratio = if raw.is_empty() {
                1.0
            } else {
                compressed.len() as f64 / raw.len() as f64
            };

            // Estimate raw size for full table based on average row size.
            let avg_row_raw = raw.len() as f64 / sample_size as f64;
            let full_raw = avg_row_raw * row_count as f64;
            let full_compressed = full_raw * ratio;

            // Add per-chunk overhead (4 bytes chunk_count + 4 bytes per chunk length).
            let chunk_count = (row_count + CHUNK_SIZE - 1) / CHUNK_SIZE;
            let chunk_overhead = 4 + (chunk_count * 4);

            // Add table name overhead.
            let name_overhead = 2 + _name.len();

            estimated_compressed +=
                full_compressed as u64 + chunk_overhead as u64 + name_overhead as u64;
        }

        drop(tables);

        let elapsed = start.elapsed().as_millis() as u64;
        // Estimate full backup duration: roughly proportional to total_rows.
        // If sampling N rows took T ms, full backup with IO ≈ T * (total/N) * 1.5 (IO factor).
        let estimated_duration = if total_rows > 0 && elapsed > 0 {
            let sampled = tables_sampled_rows(total_rows, SAMPLE_ROWS as u64);
            (elapsed as f64 * (total_rows as f64 / sampled as f64) * 1.5) as u64
        } else {
            elapsed
        };

        Ok(PredictResult {
            estimated_size_bytes: estimated_compressed,
            estimated_duration_ms: estimated_duration,
            row_count: total_rows,
            table_count,
            compression: format!("{:?}", compression),
        })
    }
}

/// How many rows were actually sampled across all tables.
fn tables_sampled_rows(total_rows: u64, max_per_table: u64) -> u64 {
    // Conservative: at least 1 row sampled.
    total_rows.min(max_per_table).max(1)
}
