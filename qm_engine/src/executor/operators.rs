/*
 * Execution Operators - Query execution primitives
 */

use super::batch::{Bitmap, ColumnBatch, ColumnData, RecordBatch};
use super::vectorized::*;
use std::cmp::Ordering;

/// Scan operator - reads data from storage
pub struct ScanOperator {
    pub table_name: String,
    pub columns: Vec<String>,
    pub batch_size: usize,
}

impl ScanOperator {
    pub fn new(table: &str, columns: Vec<String>, batch_size: usize) -> Self {
        Self {
            table_name: table.to_string(),
            columns,
            batch_size,
        }
    }
}

/// Filter operator - applies predicates
pub struct FilterOperator {
    pub predicate: FilterPredicate,
}

#[derive(Clone)]
pub enum FilterPredicate {
    Eq { column: String, value: FilterValue },
    Ne { column: String, value: FilterValue },
    Lt { column: String, value: FilterValue },
    Le { column: String, value: FilterValue },
    Gt { column: String, value: FilterValue },
    Ge { column: String, value: FilterValue },
    IsNull { column: String },
    IsNotNull { column: String },
    And(Box<FilterPredicate>, Box<FilterPredicate>),
    Or(Box<FilterPredicate>, Box<FilterPredicate>),
    Not(Box<FilterPredicate>),
}

#[derive(Clone)]
pub enum FilterValue {
    Int64(i64),
    Float64(f64),
    String(String),
    Bool(bool),
}

impl FilterOperator {
    pub fn execute(&self, batch: &RecordBatch) -> Bitmap {
        self.evaluate_predicate(&self.predicate, batch)
    }

    fn evaluate_predicate(&self, pred: &FilterPredicate, batch: &RecordBatch) -> Bitmap {
        match pred {
            FilterPredicate::Gt { column, value } => {
                if let Some(col) = batch.column(column) {
                    match (&col.data, value) {
                        (ColumnData::Float32(data), FilterValue::Float64(v)) => {
                            filter_greater_than_f32(data, *v as f32)
                        }
                        (ColumnData::Int64(data), FilterValue::Int64(v)) => {
                            let mut bitmap = Bitmap::all_null(data.len());
                            for (i, val) in data.iter().enumerate() {
                                bitmap.set_valid(i, *val > *v);
                            }
                            bitmap
                        }
                        _ => Bitmap::all_valid(batch.num_rows()),
                    }
                } else {
                    Bitmap::all_valid(batch.num_rows())
                }
            }
            FilterPredicate::Eq { column, value } => {
                if let Some(col) = batch.column(column) {
                    let mut bitmap = Bitmap::all_null(col.len());
                    match (&col.data, value) {
                        (ColumnData::Int64(data), FilterValue::Int64(v)) => {
                            for (i, val) in data.iter().enumerate() {
                                bitmap.set_valid(i, *val == *v);
                            }
                        }
                        (ColumnData::String(data), FilterValue::String(v)) => {
                            for (i, val) in data.iter().enumerate() {
                                bitmap.set_valid(i, val == v);
                            }
                        }
                        _ => return Bitmap::all_valid(batch.num_rows()),
                    }
                    bitmap
                } else {
                    Bitmap::all_valid(batch.num_rows())
                }
            }
            FilterPredicate::And(left, right) => {
                let left_result = self.evaluate_predicate(left, batch);
                let right_result = self.evaluate_predicate(right, batch);
                Self::bitmap_and(&left_result, &right_result)
            }
            FilterPredicate::Or(left, right) => {
                let left_result = self.evaluate_predicate(left, batch);
                let right_result = self.evaluate_predicate(right, batch);
                Self::bitmap_or(&left_result, &right_result)
            }
            FilterPredicate::Not(inner) => {
                let inner_result = self.evaluate_predicate(inner, batch);
                Self::bitmap_not(&inner_result)
            }
            FilterPredicate::IsNull { column } => {
                if let Some(col) = batch.column(column) {
                    Self::bitmap_not(&col.validity)
                } else {
                    Bitmap::all_null(batch.num_rows())
                }
            }
            FilterPredicate::IsNotNull { column } => {
                if let Some(col) = batch.column(column) {
                    col.validity.clone()
                } else {
                    Bitmap::all_null(batch.num_rows())
                }
            }
            _ => Bitmap::all_valid(batch.num_rows()),
        }
    }

    fn bitmap_and(a: &Bitmap, b: &Bitmap) -> Bitmap {
        let mut result = Bitmap::all_null(a.len());
        for i in 0..a.len() {
            result.set_valid(i, a.is_valid(i) && b.is_valid(i));
        }
        result
    }

    fn bitmap_or(a: &Bitmap, b: &Bitmap) -> Bitmap {
        let mut result = Bitmap::all_null(a.len());
        for i in 0..a.len() {
            result.set_valid(i, a.is_valid(i) || b.is_valid(i));
        }
        result
    }

    fn bitmap_not(a: &Bitmap) -> Bitmap {
        let mut result = Bitmap::all_null(a.len());
        for i in 0..a.len() {
            result.set_valid(i, !a.is_valid(i));
        }
        result
    }
}

/// Project operator - selects columns
pub struct ProjectOperator {
    pub columns: Vec<String>,
}

impl ProjectOperator {
    pub fn execute(&self, batch: &RecordBatch) -> RecordBatch {
        let columns: Vec<ColumnBatch> = self
            .columns
            .iter()
            .filter_map(|name| batch.column(name).cloned())
            .collect();
        RecordBatch::new(columns)
    }
}

/// Sort operator
pub struct SortOperator {
    pub keys: Vec<(String, bool)>, // (column_name, ascending)
}

impl SortOperator {
    pub fn execute(&self, batch: &RecordBatch) -> RecordBatch {
        if batch.is_empty() || self.keys.is_empty() {
            return batch.clone();
        }

        let n = batch.num_rows();
        let mut indices: Vec<usize> = (0..n).collect();

        // Sort indices based on key columns
        indices.sort_by(|&a, &b| {
            for (col_name, asc) in &self.keys {
                if let Some(col) = batch.column(col_name) {
                    let ord = match &col.data {
                        ColumnData::Int64(data) => data[a].cmp(&data[b]),
                        ColumnData::Float64(data) => {
                            data[a].partial_cmp(&data[b]).unwrap_or(Ordering::Equal)
                        }
                        ColumnData::String(data) => data[a].cmp(&data[b]),
                        _ => Ordering::Equal,
                    };

                    let ord = if *asc { ord } else { ord.reverse() };
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
            }
            Ordering::Equal
        });

        // Reorder columns according to sorted indices
        let new_columns: Vec<ColumnBatch> = batch
            .columns
            .iter()
            .map(|col| {
                let new_data = match &col.data {
                    ColumnData::Int64(data) => {
                        ColumnData::Int64(indices.iter().map(|&i| data[i]).collect())
                    }
                    ColumnData::Float64(data) => {
                        ColumnData::Float64(indices.iter().map(|&i| data[i]).collect())
                    }
                    ColumnData::Float32(data) => {
                        ColumnData::Float32(indices.iter().map(|&i| data[i]).collect())
                    }
                    ColumnData::String(data) => {
                        ColumnData::String(indices.iter().map(|&i| data[i].clone()).collect())
                    }
                    _ => col.data.clone(),
                };
                ColumnBatch::new(col.name.clone(), new_data)
            })
            .collect();

        RecordBatch::new(new_columns)
    }
}

/// Limit operator
pub struct LimitOperator {
    pub limit: usize,
    pub offset: usize,
}

impl LimitOperator {
    pub fn execute(&self, batch: &RecordBatch) -> RecordBatch {
        let start = self.offset.min(batch.num_rows());
        let end = (self.offset + self.limit).min(batch.num_rows());

        if start >= end {
            return RecordBatch::new(vec![]);
        }

        batch.slice(start, end - start)
    }
}

/// Hash aggregate operator
pub struct HashAggregateOperator {
    pub group_by: Vec<String>,
    pub aggregates: Vec<(AggFunc, String, String)>, // (func, input_col, output_name)
}

#[derive(Clone, Copy)]
pub enum AggFunc {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

impl HashAggregateOperator {
    pub fn execute(&self, batch: &RecordBatch) -> RecordBatch {
        // Simple implementation without grouping for now
        let mut result_columns = Vec::new();

        for (func, input_col, output_name) in &self.aggregates {
            if let Some(col) = batch.column(input_col) {
                let result = match (&col.data, func) {
                    (ColumnData::Int64(data), AggFunc::Sum) => {
                        let sum = aggregate_sum_i64(data, &col.validity);
                        ColumnData::Int64(vec![sum])
                    }
                    (ColumnData::Int64(_data), AggFunc::Count) => {
                        let count = col.validity.count_valid() as i64;
                        ColumnData::Int64(vec![count])
                    }
                    (ColumnData::Float32(data), AggFunc::Sum) => {
                        let sum = aggregate_sum_f32(data, &col.validity);
                        ColumnData::Float32(vec![sum])
                    }
                    (ColumnData::Int64(data), AggFunc::Min) => {
                        let min = data
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| col.validity.is_valid(*i))
                            .map(|(_, v)| *v)
                            .min()
                            .unwrap_or(0);
                        ColumnData::Int64(vec![min])
                    }
                    (ColumnData::Int64(data), AggFunc::Max) => {
                        let max = data
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| col.validity.is_valid(*i))
                            .map(|(_, v)| *v)
                            .max()
                            .unwrap_or(0);
                        ColumnData::Int64(vec![max])
                    }
                    _ => continue,
                };

                result_columns.push(ColumnBatch::new(output_name.clone(), result));
            }
        }

        RecordBatch::new(result_columns)
    }
}

/// Vector search operator - optimized KNN search
pub struct VectorSearchOperator {
    pub column: String,
    pub query_vector: Vec<f32>,
    pub top_k: usize,
    pub metric: VectorMetric,
}

#[derive(Clone, Copy)]
pub enum VectorMetric {
    Cosine,
    L2,
    InnerProduct,
}

impl VectorSearchOperator {
    pub fn execute(&self, batch: &RecordBatch) -> Vec<(usize, f32)> {
        let col = match batch.column(&self.column) {
            Some(c) => c,
            None => return vec![],
        };

        let (dim, data) = match &col.data {
            ColumnData::Vector { dim, data } => (*dim, data.as_slice()),
            _ => return vec![],
        };

        // Compute distances
        let distances = match self.metric {
            VectorMetric::Cosine => batch_cosine_distances(&self.query_vector, data, dim),
            VectorMetric::L2 => batch_l2_distances(&self.query_vector, data, dim),
            VectorMetric::InnerProduct => {
                let n = data.len() / dim;
                (0..n)
                    .map(|i| {
                        let v = &data[i * dim..(i + 1) * dim];
                        -simd_dot_product(&self.query_vector, v) // Negate for min-heap
                    })
                    .collect()
            }
        };

        // Get top-k indices
        let mut indexed: Vec<(usize, f32)> = distances.into_iter().enumerate().collect();
        indexed.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
        indexed.truncate(self.top_k);

        indexed
    }
}
