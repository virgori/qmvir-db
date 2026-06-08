/*
 * Aggregate Executor — Phase 7a
 *
 * Implements SQL aggregate functions (COUNT, SUM, AVG, MIN, MAX, COUNT DISTINCT)
 * with GROUP BY support.  Uses a two-phase approach:
 *
 *   1. Accumulate  — scan rows and update per-group accumulators
 *   2. Finalize    — produce final aggregate values for each group
 *
 * Designed for integration with NativeSqlEngine's row model.
 */

use ahash::AHashMap;
use std::fmt;

// ── Aggregate function types ────────────────────────────────────────────

/// Supported SQL aggregate functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFunction {
    Count,
    CountDistinct,
    Sum,
    Avg,
    Min,
    Max,
}

impl fmt::Display for AggFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AggFunction::Count => write!(f, "COUNT"),
            AggFunction::CountDistinct => write!(f, "COUNT(DISTINCT)"),
            AggFunction::Sum => write!(f, "SUM"),
            AggFunction::Avg => write!(f, "AVG"),
            AggFunction::Min => write!(f, "MIN"),
            AggFunction::Max => write!(f, "MAX"),
        }
    }
}

impl AggFunction {
    /// Parse from SQL string token.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "COUNT" => Some(AggFunction::Count),
            "SUM" => Some(AggFunction::Sum),
            "AVG" => Some(AggFunction::Avg),
            "MIN" => Some(AggFunction::Min),
            "MAX" => Some(AggFunction::Max),
            _ => None,
        }
    }
}

// ── Aggregate value ─────────────────────────────────────────────────────

/// A value that flows through the aggregate pipeline.
#[derive(Debug, Clone, PartialEq)]
pub enum AggValue {
    Int(i64),
    Float(f64),
    Text(String),
    Null,
}

impl AggValue {
    pub fn as_f64(&self) -> f64 {
        match self {
            AggValue::Int(v) => *v as f64,
            AggValue::Float(v) => *v,
            AggValue::Text(v) => v.parse::<f64>().unwrap_or(0.0),
            AggValue::Null => 0.0,
        }
    }

    pub fn as_i64(&self) -> i64 {
        match self {
            AggValue::Int(v) => *v,
            AggValue::Float(v) => *v as i64,
            AggValue::Text(v) => v.parse::<i64>().unwrap_or(0),
            AggValue::Null => 0,
        }
    }

    pub fn to_string_repr(&self) -> String {
        match self {
            AggValue::Int(v) => v.to_string(),
            AggValue::Float(v) => v.to_string(),
            AggValue::Text(v) => v.clone(),
            AggValue::Null => String::new(),
        }
    }
}

// ── Accumulator (per aggregate per group) ───────────────────────────────

/// Internal state for one aggregate accumulator.
#[derive(Debug, Clone)]
struct Accumulator {
    func: AggFunction,
    count: u64,
    sum: f64,
    min: f64,
    max: f64,
    /// For COUNT(DISTINCT): track unique values.
    distinct_set: Option<AHashMap<u64, ()>>,
}

impl Accumulator {
    fn new(func: AggFunction) -> Self {
        Self {
            func,
            count: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            distinct_set: if func == AggFunction::CountDistinct {
                Some(AHashMap::new())
            } else {
                None
            },
        }
    }

    fn accumulate(&mut self, value: &AggValue) {
        if matches!(value, AggValue::Null) {
            return; // SQL semantics: NULLs are ignored by aggregates (except COUNT(*))
        }

        self.count += 1;
        let fval = value.as_f64();

        match self.func {
            AggFunction::Count => { /* count already incremented */ }
            AggFunction::CountDistinct => {
                if let Some(ref mut set) = self.distinct_set {
                    set.insert(value.as_i64() as u64, ());
                }
            }
            AggFunction::Sum | AggFunction::Avg => {
                self.sum += fval;
            }
            AggFunction::Min => {
                if fval < self.min {
                    self.min = fval;
                }
            }
            AggFunction::Max => {
                if fval > self.max {
                    self.max = fval;
                }
            }
        }
    }

    fn finalize(&self) -> AggValue {
        match self.func {
            AggFunction::Count => AggValue::Int(self.count as i64),
            AggFunction::CountDistinct => {
                let n = self.distinct_set.as_ref().map(|s| s.len()).unwrap_or(0);
                AggValue::Int(n as i64)
            }
            AggFunction::Sum => AggValue::Float(self.sum),
            AggFunction::Avg => {
                if self.count == 0 {
                    AggValue::Null
                } else {
                    AggValue::Float(self.sum / self.count as f64)
                }
            }
            AggFunction::Min => {
                if self.count == 0 {
                    AggValue::Null
                } else {
                    AggValue::Float(self.min)
                }
            }
            AggFunction::Max => {
                if self.count == 0 {
                    AggValue::Null
                } else {
                    AggValue::Float(self.max)
                }
            }
        }
    }
}

// ── Aggregate specification ─────────────────────────────────────────────

/// Describes one aggregate in the SELECT list.
#[derive(Debug, Clone)]
pub struct AggSpec {
    pub func: AggFunction,
    /// Column name to aggregate (None for COUNT(*)).
    pub column: Option<String>,
    /// Output alias (e.g. "total_sales").
    pub alias: String,
}

// ── Aggregate Executor ──────────────────────────────────────────────────

/// Executes GROUP BY + aggregate queries.
///
/// Input: a row iterator where each row is a `AHashMap<String, AggValue>`.
/// Output: grouped/aggregated result rows.
pub struct AggregateExecutor {
    /// GROUP BY column names (empty = single-group / global aggregate).
    group_by_cols: Vec<String>,
    /// Aggregate specifications.
    aggs: Vec<AggSpec>,
}

impl AggregateExecutor {
    pub fn new(group_by_cols: Vec<String>, aggs: Vec<AggSpec>) -> Self {
        Self {
            group_by_cols,
            aggs,
        }
    }

    /// Execute the aggregate over a set of rows.
    ///
    /// Each row is represented as a map from column name → value.
    /// Returns: `(column_names, result_rows)`.
    pub fn execute(
        &self,
        rows: &[AHashMap<String, AggValue>],
    ) -> (Vec<String>, Vec<Vec<AggValue>>) {
        // Group key → vector of accumulators (one per AggSpec)
        let mut groups: AHashMap<Vec<String>, Vec<Accumulator>> = AHashMap::new();
        // Track insertion order for deterministic output
        let mut group_order: Vec<Vec<String>> = Vec::new();

        for row in rows {
            // Build group key
            let key: Vec<String> = self
                .group_by_cols
                .iter()
                .map(|col| row.get(col).map(|v| v.to_string_repr()).unwrap_or_default())
                .collect();

            let accs = groups.entry(key.clone()).or_insert_with(|| {
                group_order.push(key.clone());
                self.aggs.iter().map(|a| Accumulator::new(a.func)).collect()
            });

            // Feed values to accumulators
            for (i, spec) in self.aggs.iter().enumerate() {
                let val = match &spec.column {
                    Some(col) => row.get(col).cloned().unwrap_or(AggValue::Null),
                    None => AggValue::Int(1), // COUNT(*)
                };
                accs[i].accumulate(&val);
            }
        }

        // Build output columns
        let mut col_names: Vec<String> = self.group_by_cols.clone();
        for spec in &self.aggs {
            col_names.push(spec.alias.clone());
        }

        // Build output rows (in insertion order)
        let mut result_rows: Vec<Vec<AggValue>> = Vec::with_capacity(group_order.len());
        for key in &group_order {
            let accs = &groups[key];
            let mut row: Vec<AggValue> = key.iter().map(|k| AggValue::Text(k.clone())).collect();
            for acc in accs {
                row.push(acc.finalize());
            }
            result_rows.push(row);
        }

        (col_names, result_rows)
    }
}

// ── HAVING filter ───────────────────────────────────────────────────────

/// Simple HAVING predicate on an aggregate column.
#[derive(Debug, Clone)]
pub enum HavingPredicate {
    Gt(String, f64),
    Gte(String, f64),
    Lt(String, f64),
    Lte(String, f64),
    Eq(String, f64),
    Neq(String, f64),
}

impl HavingPredicate {
    /// Evaluate predicate against a result row.
    pub fn matches(&self, col_names: &[String], row: &[AggValue]) -> bool {
        let (col, op): (&str, Box<dyn Fn(f64) -> bool>) = match self {
            HavingPredicate::Gt(c, v) => (c, Box::new(move |x| x > *v)),
            HavingPredicate::Gte(c, v) => (c, Box::new(move |x| x >= *v)),
            HavingPredicate::Lt(c, v) => (c, Box::new(move |x| x < *v)),
            HavingPredicate::Lte(c, v) => (c, Box::new(move |x| x <= *v)),
            HavingPredicate::Eq(c, v) => (c, Box::new(move |x| (x - *v).abs() < f64::EPSILON)),
            HavingPredicate::Neq(c, v) => (c, Box::new(move |x| (x - *v).abs() >= f64::EPSILON)),
        };
        // Find column index
        if let Some(idx) = col_names.iter().position(|n| n == col) {
            op(row[idx].as_f64())
        } else {
            false
        }
    }
}

/// Filter aggregate results by HAVING predicate(s).
pub fn apply_having(
    col_names: &[String],
    rows: Vec<Vec<AggValue>>,
    predicates: &[HavingPredicate],
) -> Vec<Vec<AggValue>> {
    rows.into_iter()
        .filter(|row| predicates.iter().all(|p| p.matches(col_names, row)))
        .collect()
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(pairs: &[(&str, AggValue)]) -> AHashMap<String, AggValue> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn test_global_count() {
        let exec = AggregateExecutor::new(
            vec![],
            vec![AggSpec {
                func: AggFunction::Count,
                column: None,
                alias: "cnt".into(),
            }],
        );
        let rows = vec![
            make_row(&[("x", AggValue::Int(1))]),
            make_row(&[("x", AggValue::Int(2))]),
            make_row(&[("x", AggValue::Int(3))]),
        ];
        let (cols, result) = exec.execute(&rows);
        assert_eq!(cols, vec!["cnt"]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0][0], AggValue::Int(3));
    }

    #[test]
    fn test_group_by_sum() {
        let exec = AggregateExecutor::new(
            vec!["category".into()],
            vec![AggSpec {
                func: AggFunction::Sum,
                column: Some("amount".into()),
                alias: "total".into(),
            }],
        );
        let rows = vec![
            make_row(&[
                ("category", AggValue::Text("A".into())),
                ("amount", AggValue::Float(10.0)),
            ]),
            make_row(&[
                ("category", AggValue::Text("B".into())),
                ("amount", AggValue::Float(20.0)),
            ]),
            make_row(&[
                ("category", AggValue::Text("A".into())),
                ("amount", AggValue::Float(30.0)),
            ]),
        ];
        let (cols, result) = exec.execute(&rows);
        assert_eq!(cols, vec!["category", "total"]);
        assert_eq!(result.len(), 2);
        // Group A: sum=40, Group B: sum=20
        let a_row = result
            .iter()
            .find(|r| r[0] == AggValue::Text("A".into()))
            .unwrap();
        assert_eq!(a_row[1], AggValue::Float(40.0));
        let b_row = result
            .iter()
            .find(|r| r[0] == AggValue::Text("B".into()))
            .unwrap();
        assert_eq!(b_row[1], AggValue::Float(20.0));
    }

    #[test]
    fn test_avg_with_nulls() {
        let exec = AggregateExecutor::new(
            vec![],
            vec![AggSpec {
                func: AggFunction::Avg,
                column: Some("val".into()),
                alias: "avg_val".into(),
            }],
        );
        let rows = vec![
            make_row(&[("val", AggValue::Float(10.0))]),
            make_row(&[("val", AggValue::Null)]),
            make_row(&[("val", AggValue::Float(20.0))]),
        ];
        let (_, result) = exec.execute(&rows);
        // AVG should ignore nulls: (10 + 20) / 2 = 15
        assert_eq!(result[0][0], AggValue::Float(15.0));
    }

    #[test]
    fn test_min_max() {
        let exec = AggregateExecutor::new(
            vec![],
            vec![
                AggSpec {
                    func: AggFunction::Min,
                    column: Some("v".into()),
                    alias: "mn".into(),
                },
                AggSpec {
                    func: AggFunction::Max,
                    column: Some("v".into()),
                    alias: "mx".into(),
                },
            ],
        );
        let rows = vec![
            make_row(&[("v", AggValue::Float(5.0))]),
            make_row(&[("v", AggValue::Float(1.0))]),
            make_row(&[("v", AggValue::Float(9.0))]),
        ];
        let (_, result) = exec.execute(&rows);
        assert_eq!(result[0][0], AggValue::Float(1.0));
        assert_eq!(result[0][1], AggValue::Float(9.0));
    }

    #[test]
    fn test_having_filter() {
        let exec = AggregateExecutor::new(
            vec!["dept".into()],
            vec![AggSpec {
                func: AggFunction::Count,
                column: None,
                alias: "cnt".into(),
            }],
        );
        let rows = vec![
            make_row(&[("dept", AggValue::Text("eng".into()))]),
            make_row(&[("dept", AggValue::Text("eng".into()))]),
            make_row(&[("dept", AggValue::Text("eng".into()))]),
            make_row(&[("dept", AggValue::Text("hr".into()))]),
        ];
        let (cols, result) = exec.execute(&rows);
        let filtered = apply_having(&cols, result, &[HavingPredicate::Gt("cnt".into(), 2.0)]);
        assert_eq!(filtered.len(), 1); // Only "eng" with count=3
    }
}

// ── Sort-Merge Join ─────────────────────────────────────────────────────

/// A row in the sort-merge join result.
pub struct JoinResultRow {
    pub left_values: Vec<String>,
    pub right_values: Vec<String>,
}

/// Sort-Merge Join on two pre-sorted slices of (join_key, row_values).
/// Both inputs must be sorted ascending by the first element (join key).
pub fn sort_merge_join(
    left: &[(String, Vec<String>)],
    right: &[(String, Vec<String>)],
) -> Vec<JoinResultRow> {
    let mut result = Vec::new();
    let (mut i, mut j) = (0, 0);

    while i < left.len() && j < right.len() {
        match left[i].0.cmp(&right[j].0) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                let key = &left[i].0;
                // Find all matching left rows
                while i < left.len() && left[i].0 == *key {
                    let mut jj = j;
                    while jj < right.len() && right[jj].0 == *key {
                        result.push(JoinResultRow {
                            left_values: left[i].1.clone(),
                            right_values: right[jj].1.clone(),
                        });
                        jj += 1;
                    }
                    i += 1;
                }
                // Advance right pointer past matching keys
                while j < right.len() && right[j].0 == *key {
                    j += 1;
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod sort_merge_tests {
    use super::*;

    #[test]
    fn test_sort_merge_join_basic() {
        let left = vec![
            ("1".into(), vec!["alice".into()]),
            ("2".into(), vec!["bob".into()]),
            ("3".into(), vec!["charlie".into()]),
        ];
        let right = vec![
            ("1".into(), vec!["order_a".into()]),
            ("1".into(), vec!["order_b".into()]),
            ("3".into(), vec!["order_c".into()]),
            ("4".into(), vec!["order_d".into()]),
        ];
        let result = sort_merge_join(&left, &right);
        assert_eq!(result.len(), 3); // alice×2 + charlie×1
        assert_eq!(result[0].left_values[0], "alice");
        assert_eq!(result[0].right_values[0], "order_a");
        assert_eq!(result[1].left_values[0], "alice");
        assert_eq!(result[1].right_values[0], "order_b");
        assert_eq!(result[2].left_values[0], "charlie");
        assert_eq!(result[2].right_values[0], "order_c");
    }

    #[test]
    fn test_sort_merge_join_no_match() {
        let left = vec![("1".into(), vec!["a".into()])];
        let right = vec![("2".into(), vec!["b".into()])];
        let result = sort_merge_join(&left, &right);
        assert!(result.is_empty());
    }
}
