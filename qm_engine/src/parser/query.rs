/*
 * Query Planning & Optimization
 */

use super::{ParsedQuery, QueryType};
use std::collections::HashMap;

/// Query plan node types
#[derive(Debug, Clone)]
pub enum PlanNode {
    Scan {
        table: String,
        columns: Vec<String>,
        filter: Option<String>,
    },
    IndexScan {
        table: String,
        index: String,
        columns: Vec<String>,
        filter: Option<String>,
    },
    VectorScan {
        table: String,
        vector_column: String,
        top_k: usize,
        query_vector: Vec<f32>,
    },
    Filter {
        input: Box<PlanNode>,
        condition: String,
    },
    Project {
        input: Box<PlanNode>,
        columns: Vec<String>,
    },
    Sort {
        input: Box<PlanNode>,
        keys: Vec<(String, bool)>, // (column, asc)
    },
    Limit {
        input: Box<PlanNode>,
        limit: usize,
        offset: usize,
    },
    Join {
        left: Box<PlanNode>,
        right: Box<PlanNode>,
        condition: String,
        join_type: JoinType,
    },
    Aggregate {
        input: Box<PlanNode>,
        group_by: Vec<String>,
        aggregates: Vec<AggregateOp>,
    },
    Insert {
        table: String,
        columns: Vec<String>,
        values: Vec<Vec<String>>,
    },
    Update {
        table: String,
        assignments: Vec<(String, String)>,
        filter: Option<String>,
    },
    Delete {
        table: String,
        filter: Option<String>,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum JoinType {
    Inner,
    Left,
    Right,
    Full,
    Cross,
}

#[derive(Debug, Clone)]
pub struct AggregateOp {
    pub function: AggFunc,
    pub column: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum AggFunc {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

/// Query planner
pub struct QueryPlanner {
    /// Index information for optimization
    indexes: HashMap<String, Vec<IndexInfo>>,
}

#[derive(Debug, Clone)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub is_unique: bool,
    pub is_vector: bool,
}

impl QueryPlanner {
    pub fn new() -> Self {
        Self {
            indexes: HashMap::new(),
        }
    }

    /// Register an index for optimization
    pub fn register_index(&mut self, table: &str, info: IndexInfo) {
        self.indexes
            .entry(table.to_string())
            .or_default()
            .push(info);
    }

    /// Create execution plan from parsed query
    pub fn plan(&self, parsed: &ParsedQuery) -> Option<PlanNode> {
        match parsed.query_type {
            QueryType::Select => self.plan_select(parsed),
            QueryType::Insert => self.plan_insert(parsed),
            QueryType::Update => self.plan_update(parsed),
            QueryType::Delete => self.plan_delete(parsed),
            _ => None,
        }
    }

    fn plan_select(&self, parsed: &ParsedQuery) -> Option<PlanNode> {
        if parsed.tables.is_empty() {
            return None;
        }

        let table = &parsed.tables[0];

        // Check if we can use vector index
        if parsed.is_vector_query {
            if let Some(indexes) = self.indexes.get(table) {
                if let Some(vec_idx) = indexes.iter().find(|i| i.is_vector) {
                    return Some(PlanNode::VectorScan {
                        table: table.clone(),
                        vector_column: vec_idx.columns[0].clone(),
                        top_k: parsed.vector_top_k.unwrap_or(10),
                        query_vector: Vec::new(), // Filled at execution time
                    });
                }
            }
        }

        // Check for index scan opportunity
        let mut plan: PlanNode = if let Some(ref filter) = parsed.where_clause {
            if let Some(indexes) = self.indexes.get(table) {
                if let Some(idx) = self.find_usable_index(filter, indexes) {
                    PlanNode::IndexScan {
                        table: table.clone(),
                        index: idx.name.clone(),
                        columns: parsed.columns.clone(),
                        filter: Some(filter.clone()),
                    }
                } else {
                    PlanNode::Scan {
                        table: table.clone(),
                        columns: parsed.columns.clone(),
                        filter: Some(filter.clone()),
                    }
                }
            } else {
                PlanNode::Scan {
                    table: table.clone(),
                    columns: parsed.columns.clone(),
                    filter: Some(filter.clone()),
                }
            }
        } else {
            PlanNode::Scan {
                table: table.clone(),
                columns: parsed.columns.clone(),
                filter: None,
            }
        };

        // Add limit if present
        if let Some(limit) = parsed.limit {
            plan = PlanNode::Limit {
                input: Box::new(plan),
                limit: limit as usize,
                offset: parsed.offset.unwrap_or(0) as usize,
            };
        }

        Some(plan)
    }

    fn plan_insert(&self, parsed: &ParsedQuery) -> Option<PlanNode> {
        if parsed.tables.is_empty() {
            return None;
        }

        Some(PlanNode::Insert {
            table: parsed.tables[0].clone(),
            columns: parsed.columns.clone(),
            values: Vec::new(), // Filled at execution time
        })
    }

    fn plan_update(&self, parsed: &ParsedQuery) -> Option<PlanNode> {
        if parsed.tables.is_empty() {
            return None;
        }

        Some(PlanNode::Update {
            table: parsed.tables[0].clone(),
            assignments: Vec::new(),
            filter: parsed.where_clause.clone(),
        })
    }

    fn plan_delete(&self, parsed: &ParsedQuery) -> Option<PlanNode> {
        if parsed.tables.is_empty() {
            return None;
        }

        Some(PlanNode::Delete {
            table: parsed.tables[0].clone(),
            filter: parsed.where_clause.clone(),
        })
    }

    fn find_usable_index<'a>(
        &self,
        filter: &str,
        indexes: &'a [IndexInfo],
    ) -> Option<&'a IndexInfo> {
        // Simple heuristic: check if any index column appears in filter
        for idx in indexes {
            if !idx.is_vector {
                for col in &idx.columns {
                    if filter.to_lowercase().contains(&col.to_lowercase()) {
                        return Some(idx);
                    }
                }
            }
        }
        None
    }
}

impl Default for QueryPlanner {
    fn default() -> Self {
        Self::new()
    }
}
