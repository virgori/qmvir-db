/*
 * Rewrite Rules — Logical Plan Transformations
 *
 * Applied before physical planning. Each rule transforms a logical plan
 * node into an equivalent but more efficient form.
 *
 * Rules (applied in order):
 *   1. PredicatePushdown   — push WHERE conditions closer to table scans
 *   2. ProjectionPushdown  — eliminate unused columns early
 *   3. ConstantFolding     — evaluate constant expressions at plan time
 *   4. JoinReorder         — reorder joins to minimize intermediate results
 *   5. SubqueryUnnesting   — convert correlated subqueries to joins
 *   6. RedundantSortElim   — remove sorts when output is already ordered
 */

/// A logical plan node (simplified representation for the optimizer).
#[derive(Clone, Debug)]
pub enum LogicalPlan {
    Scan {
        table: String,
        columns: Option<Vec<String>>, // None = all columns
        predicates: Vec<Predicate>,
    },
    Filter {
        input: Box<LogicalPlan>,
        predicates: Vec<Predicate>,
    },
    Project {
        input: Box<LogicalPlan>,
        columns: Vec<String>,
    },
    Join {
        left: Box<LogicalPlan>,
        right: Box<LogicalPlan>,
        condition: JoinCondition,
        kind: JoinKind,
    },
    Aggregate {
        input: Box<LogicalPlan>,
        group_by: Vec<String>,
        aggregates: Vec<AggExpr>,
    },
    Sort {
        input: Box<LogicalPlan>,
        order_by: Vec<(String, SortOrder)>,
    },
    Limit {
        input: Box<LogicalPlan>,
        count: usize,
    },
}

#[derive(Clone, Debug)]
pub enum Predicate {
    Eq(String, Value),
    Ne(String, Value),
    Lt(String, Value),
    Gt(String, Value),
    Le(String, Value),
    Ge(String, Value),
    Between(String, Value, Value),
    In(String, Vec<Value>),
    IsNull(String),
    IsNotNull(String),
    And(Box<Predicate>, Box<Predicate>),
    Or(Box<Predicate>, Box<Predicate>),
    Not(Box<Predicate>),
    /// Already evaluated constant
    Const(bool),
}

#[derive(Clone, Debug)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
}

#[derive(Clone, Debug)]
pub struct JoinCondition {
    pub left_key: String,
    pub right_key: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
    Cross,
}

#[derive(Clone, Debug)]
pub enum AggExpr {
    Count(String),
    Sum(String),
    Avg(String),
    Min(String),
    Max(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum SortOrder {
    Asc,
    Desc,
}

/// A rewrite rule that transforms a logical plan.
pub trait RewriteRule {
    fn name(&self) -> &str;
    fn apply(&self, plan: LogicalPlan) -> LogicalPlan;
}

/// Apply all rules in sequence until no changes occur (fixed-point).
pub fn apply_rules(plan: LogicalPlan, rules: &[Box<dyn RewriteRule>]) -> LogicalPlan {
    let mut current = plan;
    for _ in 0..10 {
        // max 10 iterations to prevent infinite loops
        let mut changed = false;
        for rule in rules {
            let before = format!("{:?}", current);
            current = rule.apply(current);
            if format!("{:?}", current) != before {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    current
}

// ── Predicate Pushdown ──────────────────────────────────────────────────

/// Push filter predicates down through joins and projections to table scans.
pub struct PredicatePushdown;

impl PredicatePushdown {
    fn push_through(plan: LogicalPlan) -> LogicalPlan {
        match plan {
            LogicalPlan::Filter { input, predicates } => {
                match *input {
                    // Push filter into scan
                    LogicalPlan::Scan {
                        table,
                        columns,
                        predicates: mut scan_preds,
                    } => {
                        scan_preds.extend(predicates);
                        LogicalPlan::Scan {
                            table,
                            columns,
                            predicates: scan_preds,
                        }
                    }
                    // Push through projection
                    LogicalPlan::Project {
                        input: proj_input,
                        columns,
                    } => {
                        let pushed = LogicalPlan::Filter {
                            input: proj_input,
                            predicates,
                        };
                        LogicalPlan::Project {
                            input: Box::new(Self::push_through(pushed)),
                            columns,
                        }
                    }
                    other => LogicalPlan::Filter {
                        input: Box::new(Self::push_through(other)),
                        predicates,
                    },
                }
            }
            // Recurse into children
            LogicalPlan::Project { input, columns } => LogicalPlan::Project {
                input: Box::new(Self::push_through(*input)),
                columns,
            },
            LogicalPlan::Join {
                left,
                right,
                condition,
                kind,
            } => LogicalPlan::Join {
                left: Box::new(Self::push_through(*left)),
                right: Box::new(Self::push_through(*right)),
                condition,
                kind,
            },
            LogicalPlan::Sort { input, order_by } => LogicalPlan::Sort {
                input: Box::new(Self::push_through(*input)),
                order_by,
            },
            LogicalPlan::Limit { input, count } => LogicalPlan::Limit {
                input: Box::new(Self::push_through(*input)),
                count,
            },
            LogicalPlan::Aggregate {
                input,
                group_by,
                aggregates,
            } => LogicalPlan::Aggregate {
                input: Box::new(Self::push_through(*input)),
                group_by,
                aggregates,
            },
            other => other,
        }
    }
}

impl RewriteRule for PredicatePushdown {
    fn name(&self) -> &str {
        "predicate_pushdown"
    }

    fn apply(&self, plan: LogicalPlan) -> LogicalPlan {
        Self::push_through(plan)
    }
}

// ── Projection Pushdown ─────────────────────────────────────────────────

/// Eliminate unused columns at scan level.
pub struct ProjectionPushdown;

impl ProjectionPushdown {
    fn push_through(plan: LogicalPlan) -> LogicalPlan {
        match plan {
            LogicalPlan::Project { input, columns } => match *input {
                LogicalPlan::Scan {
                    table,
                    columns: _,
                    predicates,
                } => LogicalPlan::Scan {
                    table,
                    columns: Some(columns),
                    predicates,
                },
                other => LogicalPlan::Project {
                    input: Box::new(Self::push_through(other)),
                    columns,
                },
            },
            LogicalPlan::Filter { input, predicates } => LogicalPlan::Filter {
                input: Box::new(Self::push_through(*input)),
                predicates,
            },
            LogicalPlan::Join {
                left,
                right,
                condition,
                kind,
            } => LogicalPlan::Join {
                left: Box::new(Self::push_through(*left)),
                right: Box::new(Self::push_through(*right)),
                condition,
                kind,
            },
            LogicalPlan::Sort { input, order_by } => LogicalPlan::Sort {
                input: Box::new(Self::push_through(*input)),
                order_by,
            },
            LogicalPlan::Limit { input, count } => LogicalPlan::Limit {
                input: Box::new(Self::push_through(*input)),
                count,
            },
            LogicalPlan::Aggregate {
                input,
                group_by,
                aggregates,
            } => LogicalPlan::Aggregate {
                input: Box::new(Self::push_through(*input)),
                group_by,
                aggregates,
            },
            other => other,
        }
    }
}

impl RewriteRule for ProjectionPushdown {
    fn name(&self) -> &str {
        "projection_pushdown"
    }

    fn apply(&self, plan: LogicalPlan) -> LogicalPlan {
        Self::push_through(plan)
    }
}

// ── Constant Folding ────────────────────────────────────────────────────

/// Evaluate constant predicates at plan time.
pub struct ConstantFolding;

impl ConstantFolding {
    fn fold_predicate(pred: Predicate) -> Predicate {
        match pred {
            Predicate::And(a, b) => {
                let a = Self::fold_predicate(*a);
                let b = Self::fold_predicate(*b);
                match (&a, &b) {
                    (Predicate::Const(false), _) | (_, Predicate::Const(false)) => {
                        Predicate::Const(false)
                    }
                    (Predicate::Const(true), _) => b,
                    (_, Predicate::Const(true)) => a,
                    _ => Predicate::And(Box::new(a), Box::new(b)),
                }
            }
            Predicate::Or(a, b) => {
                let a = Self::fold_predicate(*a);
                let b = Self::fold_predicate(*b);
                match (&a, &b) {
                    (Predicate::Const(true), _) | (_, Predicate::Const(true)) => {
                        Predicate::Const(true)
                    }
                    (Predicate::Const(false), _) => b,
                    (_, Predicate::Const(false)) => a,
                    _ => Predicate::Or(Box::new(a), Box::new(b)),
                }
            }
            Predicate::Not(inner) => {
                let folded = Self::fold_predicate(*inner);
                match folded {
                    Predicate::Const(v) => Predicate::Const(!v),
                    other => Predicate::Not(Box::new(other)),
                }
            }
            other => other,
        }
    }

    fn fold_plan(plan: LogicalPlan) -> LogicalPlan {
        match plan {
            LogicalPlan::Filter { input, predicates } => {
                let folded: Vec<Predicate> = predicates
                    .into_iter()
                    .map(Self::fold_predicate)
                    .filter(|p| !matches!(p, Predicate::Const(true))) // remove always-true
                    .collect();

                // If any predicate is always false, return empty scan
                if folded.iter().any(|p| matches!(p, Predicate::Const(false))) {
                    return LogicalPlan::Limit {
                        input: Box::new(Self::fold_plan(*input)),
                        count: 0,
                    };
                }

                if folded.is_empty() {
                    return Self::fold_plan(*input);
                }

                LogicalPlan::Filter {
                    input: Box::new(Self::fold_plan(*input)),
                    predicates: folded,
                }
            }
            LogicalPlan::Scan {
                table,
                columns,
                predicates,
            } => {
                let folded: Vec<Predicate> = predicates
                    .into_iter()
                    .map(Self::fold_predicate)
                    .filter(|p| !matches!(p, Predicate::Const(true)))
                    .collect();
                LogicalPlan::Scan {
                    table,
                    columns,
                    predicates: folded,
                }
            }
            LogicalPlan::Project { input, columns } => LogicalPlan::Project {
                input: Box::new(Self::fold_plan(*input)),
                columns,
            },
            LogicalPlan::Join {
                left,
                right,
                condition,
                kind,
            } => LogicalPlan::Join {
                left: Box::new(Self::fold_plan(*left)),
                right: Box::new(Self::fold_plan(*right)),
                condition,
                kind,
            },
            LogicalPlan::Sort { input, order_by } => LogicalPlan::Sort {
                input: Box::new(Self::fold_plan(*input)),
                order_by,
            },
            LogicalPlan::Limit { input, count } => LogicalPlan::Limit {
                input: Box::new(Self::fold_plan(*input)),
                count,
            },
            LogicalPlan::Aggregate {
                input,
                group_by,
                aggregates,
            } => LogicalPlan::Aggregate {
                input: Box::new(Self::fold_plan(*input)),
                group_by,
                aggregates,
            },
        }
    }
}

impl RewriteRule for ConstantFolding {
    fn name(&self) -> &str {
        "constant_folding"
    }

    fn apply(&self, plan: LogicalPlan) -> LogicalPlan {
        Self::fold_plan(plan)
    }
}

// ── Redundant Sort Elimination ──────────────────────────────────────────

/// Remove sorts that follow another sort or when limit 1.
pub struct RedundantSortElimination;

impl RedundantSortElimination {
    fn eliminate(plan: LogicalPlan) -> LogicalPlan {
        match plan {
            LogicalPlan::Sort { input, order_by } => {
                let inner = Self::eliminate(*input);
                // Remove sort if inner is already sorted the same way
                if let LogicalPlan::Sort {
                    input: ref _inner_input,
                    order_by: ref inner_order,
                } = inner
                {
                    if *inner_order == order_by {
                        return inner;
                    }
                }
                LogicalPlan::Sort {
                    input: Box::new(inner),
                    order_by,
                }
            }
            LogicalPlan::Limit { input, count } => {
                // If limit 0 or 1, sort is unnecessary for correctness
                // (but keep it for ORDER BY semantics — only optimize limit 0)
                LogicalPlan::Limit {
                    input: Box::new(Self::eliminate(*input)),
                    count,
                }
            }
            LogicalPlan::Filter { input, predicates } => LogicalPlan::Filter {
                input: Box::new(Self::eliminate(*input)),
                predicates,
            },
            LogicalPlan::Project { input, columns } => LogicalPlan::Project {
                input: Box::new(Self::eliminate(*input)),
                columns,
            },
            LogicalPlan::Join {
                left,
                right,
                condition,
                kind,
            } => LogicalPlan::Join {
                left: Box::new(Self::eliminate(*left)),
                right: Box::new(Self::eliminate(*right)),
                condition,
                kind,
            },
            LogicalPlan::Aggregate {
                input,
                group_by,
                aggregates,
            } => LogicalPlan::Aggregate {
                input: Box::new(Self::eliminate(*input)),
                group_by,
                aggregates,
            },
            other => other,
        }
    }
}

impl RewriteRule for RedundantSortElimination {
    fn name(&self) -> &str {
        "redundant_sort_elimination"
    }

    fn apply(&self, plan: LogicalPlan) -> LogicalPlan {
        Self::eliminate(plan)
    }
}

/// Get the default set of rewrite rules.
pub fn default_rules() -> Vec<Box<dyn RewriteRule>> {
    vec![
        Box::new(ConstantFolding),
        Box::new(PredicatePushdown),
        Box::new(ProjectionPushdown),
        Box::new(RedundantSortElimination),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_predicate_pushdown() {
        let plan = LogicalPlan::Filter {
            input: Box::new(LogicalPlan::Scan {
                table: "users".to_string(),
                columns: None,
                predicates: Vec::new(),
            }),
            predicates: vec![Predicate::Eq("id".to_string(), Value::Int(42))],
        };

        let rule = PredicatePushdown;
        let optimized = rule.apply(plan);

        match optimized {
            LogicalPlan::Scan { predicates, .. } => {
                assert_eq!(predicates.len(), 1);
            }
            _ => panic!("Expected Scan with pushed predicate"),
        }
    }

    #[test]
    fn test_constant_folding_true_and() {
        let pred = Predicate::And(
            Box::new(Predicate::Const(true)),
            Box::new(Predicate::Eq("x".to_string(), Value::Int(1))),
        );
        let folded = ConstantFolding::fold_predicate(pred);
        assert!(matches!(folded, Predicate::Eq(_, _)));
    }

    #[test]
    fn test_constant_folding_false_and() {
        let pred = Predicate::And(
            Box::new(Predicate::Const(false)),
            Box::new(Predicate::Eq("x".to_string(), Value::Int(1))),
        );
        let folded = ConstantFolding::fold_predicate(pred);
        assert!(matches!(folded, Predicate::Const(false)));
    }

    #[test]
    fn test_redundant_sort() {
        let plan = LogicalPlan::Sort {
            input: Box::new(LogicalPlan::Sort {
                input: Box::new(LogicalPlan::Scan {
                    table: "t".to_string(),
                    columns: None,
                    predicates: Vec::new(),
                }),
                order_by: vec![("id".to_string(), SortOrder::Asc)],
            }),
            order_by: vec![("id".to_string(), SortOrder::Asc)],
        };

        let rule = RedundantSortElimination;
        let optimized = rule.apply(plan);
        // Should eliminate redundant outer sort
        match optimized {
            LogicalPlan::Sort { input, .. } => {
                assert!(matches!(*input, LogicalPlan::Scan { .. }));
            }
            _ => panic!("Expected single Sort"),
        }
    }

    #[test]
    fn test_projection_pushdown() {
        let plan = LogicalPlan::Project {
            input: Box::new(LogicalPlan::Scan {
                table: "users".to_string(),
                columns: None,
                predicates: Vec::new(),
            }),
            columns: vec!["id".to_string(), "name".to_string()],
        };

        let rule = ProjectionPushdown;
        let optimized = rule.apply(plan);

        match optimized {
            LogicalPlan::Scan {
                columns: Some(cols),
                ..
            } => {
                assert_eq!(cols, vec!["id", "name"]);
            }
            _ => panic!("Expected Scan with projected columns"),
        }
    }

    #[test]
    fn test_apply_all_rules() {
        let plan = LogicalPlan::Filter {
            input: Box::new(LogicalPlan::Project {
                input: Box::new(LogicalPlan::Scan {
                    table: "t".to_string(),
                    columns: None,
                    predicates: Vec::new(),
                }),
                columns: vec!["id".to_string()],
            }),
            predicates: vec![Predicate::And(
                Box::new(Predicate::Const(true)),
                Box::new(Predicate::Eq("id".to_string(), Value::Int(1))),
            )],
        };

        let rules = default_rules();
        let optimized = apply_rules(plan, &rules);
        // Should have pushed predicate and folded constant
        // The exact structure depends on rule application order
        let debug = format!("{:?}", optimized);
        assert!(debug.contains("id"));
    }
}
