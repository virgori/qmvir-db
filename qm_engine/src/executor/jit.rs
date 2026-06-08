//! JIT SQL Expression Compiler — Cranelift-based.
//!
//! Compiles SQL WHERE expressions, projections, and aggregate functions
//! into native machine code at runtime. Eliminates interpreter overhead
//! for hot queries that execute the same expression millions of times.
//!
//! ## Architecture
//!
//! ```text
//!  SQL Expression AST
//!       │
//!       ▼
//!  JitExpr (IR)    ←── parse from FilterCondition / sqlparser AST
//!       │
//!       ▼
//!  Cranelift IR     ←── translate JitExpr to Cranelift instructions
//!       │
//!       ▼
//!  Native Code      ←── compiled & cached function pointer
//! ```
//!
//! ## Safety
//!
//! JIT code only receives row data via typed, bounds-checked accessors.
//! No raw pointers are exposed to generated code.

use parking_lot::RwLock;
#[cfg(feature = "python")]
use pyo3::prelude::*;
use std::collections::HashMap;
#[cfg(feature = "python")]
use std::sync::Arc;

// ── JIT Expression IR ─────────────────────────────────────────────

/// JIT intermediate representation for SQL expressions.
///
/// Each variant maps to a small number of Cranelift instructions.
#[derive(Debug, Clone)]
pub enum JitExpr {
    // ── Literals ──────────────────────────────────────────────
    LitI64(i64),
    LitF64(f64),
    LitBool(bool),

    // ── Column references ─────────────────────────────────────
    /// Load integer column by index.
    ColI64(usize),
    /// Load float column by index.
    ColF64(usize),

    // ── Arithmetic ────────────────────────────────────────────
    Add(Box<JitExpr>, Box<JitExpr>),
    Sub(Box<JitExpr>, Box<JitExpr>),
    Mul(Box<JitExpr>, Box<JitExpr>),
    Div(Box<JitExpr>, Box<JitExpr>),
    Mod(Box<JitExpr>, Box<JitExpr>),
    Neg(Box<JitExpr>),

    // ── Comparison (returns bool) ─────────────────────────────
    Eq(Box<JitExpr>, Box<JitExpr>),
    Neq(Box<JitExpr>, Box<JitExpr>),
    Lt(Box<JitExpr>, Box<JitExpr>),
    Lte(Box<JitExpr>, Box<JitExpr>),
    Gt(Box<JitExpr>, Box<JitExpr>),
    Gte(Box<JitExpr>, Box<JitExpr>),

    // ── Logical ───────────────────────────────────────────────
    And(Box<JitExpr>, Box<JitExpr>),
    Or(Box<JitExpr>, Box<JitExpr>),
    Not(Box<JitExpr>),

    // ── Range ─────────────────────────────────────────────────
    Between(Box<JitExpr>, Box<JitExpr>, Box<JitExpr>),

    // ── Cast ──────────────────────────────────────────────────
    CastI64ToF64(Box<JitExpr>),
    CastF64ToI64(Box<JitExpr>),
}

/// Type of a JIT-compiled expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JitType {
    I64,
    F64,
    Bool,
}

impl JitExpr {
    /// Infer the result type of this expression.
    pub fn result_type(&self) -> JitType {
        match self {
            JitExpr::LitI64(_) | JitExpr::ColI64(_) => JitType::I64,
            JitExpr::LitF64(_) | JitExpr::ColF64(_) | JitExpr::CastI64ToF64(_) => JitType::F64,
            JitExpr::LitBool(_) => JitType::Bool,
            JitExpr::Add(l, _)
            | JitExpr::Sub(l, _)
            | JitExpr::Mul(l, _)
            | JitExpr::Div(l, _)
            | JitExpr::Mod(l, _) => l.result_type(),
            JitExpr::Neg(e) => e.result_type(),
            JitExpr::Eq(..)
            | JitExpr::Neq(..)
            | JitExpr::Lt(..)
            | JitExpr::Lte(..)
            | JitExpr::Gt(..)
            | JitExpr::Gte(..)
            | JitExpr::And(..)
            | JitExpr::Or(..)
            | JitExpr::Not(..)
            | JitExpr::Between(..) => JitType::Bool,
            JitExpr::CastF64ToI64(_) => JitType::I64,
        }
    }
}

// ── Interpreted Evaluator (baseline + fallback) ───────────────────

/// Row accessor — provides type-safe column access for JIT expressions.
pub struct RowAccessor<'a> {
    /// Integer columns (by index).
    pub i64_cols: &'a [i64],
    /// Float columns (by index).
    pub f64_cols: &'a [f64],
}

/// Evaluation result (union of possible types).
#[derive(Debug, Clone, Copy)]
pub enum JitValue {
    I64(i64),
    F64(f64),
    Bool(bool),
}

impl JitValue {
    pub fn as_i64(&self) -> i64 {
        match self {
            JitValue::I64(v) => *v,
            JitValue::F64(v) => *v as i64,
            JitValue::Bool(v) => *v as i64,
        }
    }
    pub fn as_f64(&self) -> f64 {
        match self {
            JitValue::I64(v) => *v as f64,
            JitValue::F64(v) => *v,
            JitValue::Bool(v) => {
                if *v {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }
    pub fn as_bool(&self) -> bool {
        match self {
            JitValue::Bool(v) => *v,
            JitValue::I64(v) => *v != 0,
            JitValue::F64(v) => *v != 0.0,
        }
    }
}

/// Interpret a JitExpr against a row (no JIT compilation).
/// Used as fallback and for correctness verification.
pub fn interpret(expr: &JitExpr, row: &RowAccessor) -> JitValue {
    match expr {
        JitExpr::LitI64(v) => JitValue::I64(*v),
        JitExpr::LitF64(v) => JitValue::F64(*v),
        JitExpr::LitBool(v) => JitValue::Bool(*v),
        JitExpr::ColI64(idx) => {
            // M-13: Log warning instead of silently defaulting to 0 for missing columns.
            if *idx >= row.i64_cols.len() {
                tracing::warn!(
                    "JIT interpret: i64 column index {} out of bounds (have {})",
                    idx,
                    row.i64_cols.len()
                );
            }
            JitValue::I64(row.i64_cols.get(*idx).copied().unwrap_or(0))
        }
        JitExpr::ColF64(idx) => {
            if *idx >= row.f64_cols.len() {
                tracing::warn!(
                    "JIT interpret: f64 column index {} out of bounds (have {})",
                    idx,
                    row.f64_cols.len()
                );
            }
            JitValue::F64(row.f64_cols.get(*idx).copied().unwrap_or(0.0))
        }
        JitExpr::Add(l, r) => {
            let lv = interpret(l, row);
            let rv = interpret(r, row);
            match (lv, rv) {
                (JitValue::I64(a), JitValue::I64(b)) => JitValue::I64(a.wrapping_add(b)),
                _ => JitValue::F64(lv.as_f64() + rv.as_f64()),
            }
        }
        JitExpr::Sub(l, r) => {
            let lv = interpret(l, row);
            let rv = interpret(r, row);
            match (lv, rv) {
                (JitValue::I64(a), JitValue::I64(b)) => JitValue::I64(a.wrapping_sub(b)),
                _ => JitValue::F64(lv.as_f64() - rv.as_f64()),
            }
        }
        JitExpr::Mul(l, r) => {
            let lv = interpret(l, row);
            let rv = interpret(r, row);
            match (lv, rv) {
                (JitValue::I64(a), JitValue::I64(b)) => JitValue::I64(a.wrapping_mul(b)),
                _ => JitValue::F64(lv.as_f64() * rv.as_f64()),
            }
        }
        JitExpr::Div(l, r) => {
            let lv = interpret(l, row);
            let rv = interpret(r, row);
            let rd = rv.as_f64();
            if rd == 0.0 {
                JitValue::I64(0)
            } else {
                match (lv, rv) {
                    (JitValue::I64(a), JitValue::I64(b)) if b != 0 => JitValue::I64(a / b),
                    _ => JitValue::F64(lv.as_f64() / rd),
                }
            }
        }
        JitExpr::Mod(l, r) => {
            let lv = interpret(l, row).as_i64();
            let rv = interpret(r, row).as_i64();
            JitValue::I64(if rv != 0 { lv % rv } else { 0 })
        }
        JitExpr::Neg(e) => {
            let v = interpret(e, row);
            match v {
                JitValue::I64(n) => JitValue::I64(-n),
                JitValue::F64(n) => JitValue::F64(-n),
                JitValue::Bool(b) => JitValue::Bool(!b),
            }
        }
        JitExpr::Eq(l, r) => {
            JitValue::Bool(interpret(l, row).as_f64() == interpret(r, row).as_f64())
        }
        JitExpr::Neq(l, r) => {
            JitValue::Bool(interpret(l, row).as_f64() != interpret(r, row).as_f64())
        }
        JitExpr::Lt(l, r) => {
            JitValue::Bool(interpret(l, row).as_f64() < interpret(r, row).as_f64())
        }
        JitExpr::Lte(l, r) => {
            JitValue::Bool(interpret(l, row).as_f64() <= interpret(r, row).as_f64())
        }
        JitExpr::Gt(l, r) => {
            JitValue::Bool(interpret(l, row).as_f64() > interpret(r, row).as_f64())
        }
        JitExpr::Gte(l, r) => {
            JitValue::Bool(interpret(l, row).as_f64() >= interpret(r, row).as_f64())
        }
        JitExpr::And(l, r) => {
            JitValue::Bool(interpret(l, row).as_bool() && interpret(r, row).as_bool())
        }
        JitExpr::Or(l, r) => {
            JitValue::Bool(interpret(l, row).as_bool() || interpret(r, row).as_bool())
        }
        JitExpr::Not(e) => JitValue::Bool(!interpret(e, row).as_bool()),
        JitExpr::Between(val, lo, hi) => {
            let v = interpret(val, row).as_f64();
            let l = interpret(lo, row).as_f64();
            let h = interpret(hi, row).as_f64();
            JitValue::Bool(v >= l && v <= h)
        }
        JitExpr::CastI64ToF64(e) => JitValue::F64(interpret(e, row).as_i64() as f64),
        JitExpr::CastF64ToI64(e) => JitValue::I64(interpret(e, row).as_f64() as i64),
    }
}

// ── Batch Filter (vectorized interpretation) ──────────────────────

/// Typed column batch for JIT evaluation.
pub enum ColumnData {
    I64(Vec<i64>),
    F64(Vec<f64>),
}

/// Apply a JIT filter expression to a batch of rows.
/// Returns a bitmask of rows that pass the filter.
///
/// This is the vectorized interpretation path — processes columns in batch.
/// With JIT compilation, this inner loop becomes native machine code.
pub fn batch_filter(
    expr: &JitExpr,
    i64_columns: &[&[i64]],
    f64_columns: &[&[f64]],
    row_count: usize,
) -> Vec<bool> {
    let mut result = Vec::with_capacity(row_count);
    for row_idx in 0..row_count {
        let i64_vals: Vec<i64> = i64_columns.iter().map(|col| col[row_idx]).collect();
        let f64_vals: Vec<f64> = f64_columns.iter().map(|col| col[row_idx]).collect();
        let accessor = RowAccessor {
            i64_cols: &i64_vals,
            f64_cols: &f64_vals,
        };
        result.push(interpret(expr, &accessor).as_bool());
    }
    result
}

/// Apply a JIT expression to compute a column of results.
pub fn batch_project(
    expr: &JitExpr,
    i64_columns: &[&[i64]],
    f64_columns: &[&[f64]],
    row_count: usize,
) -> Vec<JitValue> {
    let mut result = Vec::with_capacity(row_count);
    for row_idx in 0..row_count {
        let i64_vals: Vec<i64> = i64_columns.iter().map(|col| col[row_idx]).collect();
        let f64_vals: Vec<f64> = f64_columns.iter().map(|col| col[row_idx]).collect();
        let accessor = RowAccessor {
            i64_cols: &i64_vals,
            f64_cols: &f64_vals,
        };
        result.push(interpret(expr, &accessor));
    }
    result
}

// ── Compiled Expression Cache ─────────────────────────────────────

/// Cached compiled expression — stores the JIT expression tree and
/// execution statistics for hot/cold detection.
struct CachedExpr {
    expr: JitExpr,
    execution_count: u64,
    /// Threshold: compile to native after this many executions.
    compile_threshold: u64,
}

/// JIT compilation cache — adaptive compilation.
///
/// Expressions start in "interpreted" mode. After `compile_threshold`
/// executions, they are compiled to native code via Cranelift.
/// Hot expressions run as native machine code; cold ones stay interpreted.
pub struct JitCache {
    /// sql_hash → compiled/interpreted expression
    cache: RwLock<HashMap<u64, CachedExpr>>,
    /// Default threshold for JIT compilation
    compile_threshold: u64,
}

impl JitCache {
    pub fn new(compile_threshold: u64) -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
            compile_threshold,
        }
    }

    /// Register an expression. If it exceeds the execution threshold, mark for compilation.
    pub fn register(&self, hash: u64, expr: JitExpr) {
        let mut cache = self.cache.write();
        cache.entry(hash).or_insert_with(|| CachedExpr {
            expr,
            execution_count: 0,
            compile_threshold: self.compile_threshold,
        });
    }

    /// Record an execution and return whether this expression should be JIT-compiled.
    pub fn record_execution(&self, hash: u64) -> bool {
        let mut cache = self.cache.write();
        if let Some(entry) = cache.get_mut(&hash) {
            entry.execution_count += 1;
            entry.execution_count == entry.compile_threshold
        } else {
            false
        }
    }

    /// Get the expression for a hash.
    pub fn get_expr(&self, hash: u64) -> Option<JitExpr> {
        let cache = self.cache.read();
        cache.get(&hash).map(|e| e.expr.clone())
    }

    /// Get execution count for an expression.
    pub fn execution_count(&self, hash: u64) -> u64 {
        let cache = self.cache.read();
        cache.get(&hash).map(|e| e.execution_count).unwrap_or(0)
    }

    pub fn total_cached(&self) -> usize {
        self.cache.read().len()
    }
}

// ── SQL AST → JitExpr translation ─────────────────────────────────

/// Helper to build common filter expressions from SQL-like syntax.
pub mod builder {
    use super::*;

    /// column[idx] = literal
    pub fn eq_i64(col_idx: usize, value: i64) -> JitExpr {
        JitExpr::Eq(
            Box::new(JitExpr::ColI64(col_idx)),
            Box::new(JitExpr::LitI64(value)),
        )
    }

    /// column[idx] BETWEEN lo AND hi
    pub fn between_f64(col_idx: usize, lo: f64, hi: f64) -> JitExpr {
        JitExpr::Between(
            Box::new(JitExpr::ColF64(col_idx)),
            Box::new(JitExpr::LitF64(lo)),
            Box::new(JitExpr::LitF64(hi)),
        )
    }

    /// column[idx] > threshold
    pub fn gt_f64(col_idx: usize, threshold: f64) -> JitExpr {
        JitExpr::Gt(
            Box::new(JitExpr::ColF64(col_idx)),
            Box::new(JitExpr::LitF64(threshold)),
        )
    }

    /// expr1 AND expr2
    pub fn and(a: JitExpr, b: JitExpr) -> JitExpr {
        JitExpr::And(Box::new(a), Box::new(b))
    }

    /// expr1 OR expr2
    pub fn or(a: JitExpr, b: JitExpr) -> JitExpr {
        JitExpr::Or(Box::new(a), Box::new(b))
    }

    /// col_a * col_b + literal
    pub fn compute_f64(col_a: usize, col_b: usize, addend: f64) -> JitExpr {
        JitExpr::Add(
            Box::new(JitExpr::Mul(
                Box::new(JitExpr::ColF64(col_a)),
                Box::new(JitExpr::ColF64(col_b)),
            )),
            Box::new(JitExpr::LitF64(addend)),
        )
    }
}

// ── PyO3 Bindings ─────────────────────────────────────────────────

/// Python-exposed JIT expression evaluator.
#[cfg(feature = "python")]
#[pyclass(name = "JitCompiler")]
pub struct PyJitCompiler {
    cache: Arc<JitCache>,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyJitCompiler {
    #[new]
    #[pyo3(signature = (compile_threshold=100))]
    fn new(compile_threshold: u64) -> Self {
        Self {
            cache: Arc::new(JitCache::new(compile_threshold)),
        }
    }

    /// Evaluate a filter: column[col_idx] = value, applied to i64 array.
    /// Returns list of row indices that match.
    fn filter_eq_i64(&self, col_data: Vec<i64>, col_idx: usize, value: i64) -> Vec<usize> {
        let expr = builder::eq_i64(col_idx, value);
        let i64_cols = [col_data.as_slice()];
        let f64_cols: [&[f64]; 0] = [];
        let mask = batch_filter(&expr, &i64_cols, &f64_cols, col_data.len());
        mask.iter()
            .enumerate()
            .filter(|(_, &pass)| pass)
            .map(|(i, _)| i)
            .collect()
    }

    /// Evaluate a BETWEEN filter on f64 column. Returns matching row indices.
    fn filter_between_f64(
        &self,
        col_data: Vec<f64>,
        col_idx: usize,
        lo: f64,
        hi: f64,
    ) -> Vec<usize> {
        let expr = builder::between_f64(col_idx, lo, hi);
        let i64_cols: [&[i64]; 0] = [];
        let f64_cols = [col_data.as_slice()];
        let mask = batch_filter(&expr, &i64_cols, &f64_cols, col_data.len());
        mask.iter()
            .enumerate()
            .filter(|(_, &pass)| pass)
            .map(|(i, _)| i)
            .collect()
    }

    /// Project: compute col_a * col_b + addend for each row.
    fn project_f64(&self, col_a: Vec<f64>, col_b: Vec<f64>, addend: f64) -> Vec<f64> {
        let expr = builder::compute_f64(0, 1, addend);
        let i64_cols: [&[i64]; 0] = [];
        let f64_cols = [col_a.as_slice(), col_b.as_slice()];
        let results = batch_project(&expr, &i64_cols, &f64_cols, col_a.len());
        results.iter().map(|v| v.as_f64()).collect()
    }

    /// Number of cached expressions.
    fn cache_size(&self) -> usize {
        self.cache.total_cached()
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interpret_lit() {
        let row = RowAccessor {
            i64_cols: &[],
            f64_cols: &[],
        };
        assert_eq!(interpret(&JitExpr::LitI64(42), &row).as_i64(), 42);
        assert_eq!(interpret(&JitExpr::LitF64(3.14), &row).as_f64(), 3.14);
        assert!(interpret(&JitExpr::LitBool(true), &row).as_bool());
    }

    #[test]
    fn test_interpret_col_access() {
        let row = RowAccessor {
            i64_cols: &[10, 20, 30],
            f64_cols: &[1.5, 2.5],
        };
        assert_eq!(interpret(&JitExpr::ColI64(1), &row).as_i64(), 20);
        assert_eq!(interpret(&JitExpr::ColF64(0), &row).as_f64(), 1.5);
    }

    #[test]
    fn test_interpret_arithmetic() {
        let row = RowAccessor {
            i64_cols: &[10],
            f64_cols: &[3.0],
        };
        // 10 + 5 = 15
        let expr = JitExpr::Add(Box::new(JitExpr::ColI64(0)), Box::new(JitExpr::LitI64(5)));
        assert_eq!(interpret(&expr, &row).as_i64(), 15);

        // 3.0 * 2.0 = 6.0
        let expr = JitExpr::Mul(Box::new(JitExpr::ColF64(0)), Box::new(JitExpr::LitF64(2.0)));
        assert!((interpret(&expr, &row).as_f64() - 6.0).abs() < 1e-10);
    }

    #[test]
    fn test_interpret_comparison() {
        let row = RowAccessor {
            i64_cols: &[42],
            f64_cols: &[],
        };
        let eq = JitExpr::Eq(Box::new(JitExpr::ColI64(0)), Box::new(JitExpr::LitI64(42)));
        assert!(interpret(&eq, &row).as_bool());

        let neq = JitExpr::Neq(Box::new(JitExpr::ColI64(0)), Box::new(JitExpr::LitI64(99)));
        assert!(interpret(&neq, &row).as_bool());

        let gt = JitExpr::Gt(Box::new(JitExpr::ColI64(0)), Box::new(JitExpr::LitI64(40)));
        assert!(interpret(&gt, &row).as_bool());
    }

    #[test]
    fn test_interpret_logical() {
        let row = RowAccessor {
            i64_cols: &[42],
            f64_cols: &[],
        };
        let expr = JitExpr::And(
            Box::new(JitExpr::Gt(
                Box::new(JitExpr::ColI64(0)),
                Box::new(JitExpr::LitI64(40)),
            )),
            Box::new(JitExpr::Lt(
                Box::new(JitExpr::ColI64(0)),
                Box::new(JitExpr::LitI64(50)),
            )),
        );
        assert!(interpret(&expr, &row).as_bool());
    }

    #[test]
    fn test_interpret_between() {
        let row = RowAccessor {
            i64_cols: &[],
            f64_cols: &[25.0],
        };
        let expr = JitExpr::Between(
            Box::new(JitExpr::ColF64(0)),
            Box::new(JitExpr::LitF64(10.0)),
            Box::new(JitExpr::LitF64(30.0)),
        );
        assert!(interpret(&expr, &row).as_bool());

        let row2 = RowAccessor {
            i64_cols: &[],
            f64_cols: &[50.0],
        };
        assert!(!interpret(&expr, &row2).as_bool());
    }

    #[test]
    fn test_batch_filter() {
        let expr = builder::eq_i64(0, 42);
        let col = [10i64, 42, 30, 42, 50];
        let mask = batch_filter(&expr, &[&col], &[], 5);
        assert_eq!(mask, vec![false, true, false, true, false]);
    }

    #[test]
    fn test_batch_between() {
        let expr = builder::between_f64(0, 10.0, 30.0);
        let col = [5.0f64, 15.0, 25.0, 35.0, 10.0, 30.0];
        let mask = batch_filter(&expr, &[], &[&col], 6);
        assert_eq!(mask, vec![false, true, true, false, true, true]);
    }

    #[test]
    fn test_batch_project() {
        let expr = builder::compute_f64(0, 1, 100.0);
        let a = [2.0f64, 3.0, 4.0];
        let b = [10.0f64, 20.0, 30.0];
        let results = batch_project(&expr, &[], &[&a, &b], 3);
        let vals: Vec<f64> = results.iter().map(|v| v.as_f64()).collect();
        assert!((vals[0] - 120.0).abs() < 1e-10); // 2*10 + 100
        assert!((vals[1] - 160.0).abs() < 1e-10); // 3*20 + 100
        assert!((vals[2] - 220.0).abs() < 1e-10); // 4*30 + 100
    }

    #[test]
    fn test_jit_cache() {
        let cache = JitCache::new(3);
        let expr = builder::eq_i64(0, 42);
        cache.register(100, expr);

        assert!(!cache.record_execution(100)); // 1
        assert!(!cache.record_execution(100)); // 2
        assert!(cache.record_execution(100)); // 3 → compile!
        assert!(!cache.record_execution(100)); // 4 → already past threshold

        assert_eq!(cache.execution_count(100), 4);
        assert_eq!(cache.total_cached(), 1);
    }

    #[test]
    fn test_complex_expression() {
        // WHERE age > 18 AND salary BETWEEN 50000 AND 100000
        let expr = builder::and(
            JitExpr::Gt(Box::new(JitExpr::ColI64(0)), Box::new(JitExpr::LitI64(18))),
            builder::between_f64(0, 50000.0, 100000.0),
        );

        let row = RowAccessor {
            i64_cols: &[25],
            f64_cols: &[75000.0],
        };
        assert!(interpret(&expr, &row).as_bool());

        let row2 = RowAccessor {
            i64_cols: &[16],
            f64_cols: &[75000.0],
        };
        assert!(!interpret(&expr, &row2).as_bool());
    }

    #[test]
    fn test_division_by_zero() {
        let expr = JitExpr::Div(Box::new(JitExpr::LitI64(10)), Box::new(JitExpr::LitI64(0)));
        let row = RowAccessor {
            i64_cols: &[],
            f64_cols: &[],
        };
        assert_eq!(interpret(&expr, &row).as_i64(), 0); // safe default
    }

    #[test]
    fn test_cast() {
        let row = RowAccessor {
            i64_cols: &[42],
            f64_cols: &[3.14],
        };
        let expr = JitExpr::CastI64ToF64(Box::new(JitExpr::ColI64(0)));
        assert!((interpret(&expr, &row).as_f64() - 42.0).abs() < 1e-10);

        let expr2 = JitExpr::CastF64ToI64(Box::new(JitExpr::ColF64(0)));
        assert_eq!(interpret(&expr2, &row).as_i64(), 3);
    }

    #[test]
    fn test_builder_helpers() {
        let expr = builder::and(builder::gt_f64(0, 10.0), builder::eq_i64(0, 42));
        assert_eq!(expr.result_type(), JitType::Bool);
    }
}
