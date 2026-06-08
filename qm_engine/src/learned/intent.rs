/*
 * Intent Classifier — Query Intent Classification
 *
 * Classifies incoming queries into intent categories to enable
 * engine routing and fusion weight selection:
 *
 *   • Lookup     — point queries, PK access, GET by ID
 *   • Search     — full-text, keyword, fuzzy matching
 *   • Analytics  — aggregation, GROUP BY, window functions
 *   • Hybrid     — combined lexical + vector retrieval
 *
 * Method: feature-based heuristics (no ML training required).
 * Fallback: Hybrid (safest default).
 */

/// Query intent categories.
#[derive(Clone, Debug, PartialEq)]
pub enum QueryIntent {
    /// Point lookup: SELECT by PK, WHERE id = X
    Lookup,
    /// Full-text search: LIKE, MATCH, full-text predicates
    Search,
    /// Analytics: SUM, COUNT, AVG, GROUP BY, HAVING
    Analytics,
    /// Hybrid: combines text + vector similarity
    Hybrid,
}

impl QueryIntent {
    pub fn as_str(&self) -> &str {
        match self {
            QueryIntent::Lookup => "lookup",
            QueryIntent::Search => "search",
            QueryIntent::Analytics => "analytics",
            QueryIntent::Hybrid => "hybrid",
        }
    }
}

/// Features extracted from a query for intent classification.
#[derive(Clone, Debug, Default)]
pub struct QueryFeatures {
    pub has_pk_eq: bool,        // WHERE id = X
    pub has_limit_1: bool,      // LIMIT 1
    pub has_like: bool,         // LIKE / ILIKE
    pub has_fulltext: bool,     // MATCH, ts_query, search()
    pub has_vector_sim: bool,   // cosine_similarity, vector_search()
    pub has_group_by: bool,     // GROUP BY
    pub has_aggregate: bool,    // SUM, COUNT, AVG, MIN, MAX
    pub has_window: bool,       // OVER(), PARTITION BY
    pub has_order_by: bool,     // ORDER BY
    pub has_join: bool,         // JOIN
    pub column_count: usize,    // number of selected columns
    pub predicate_count: usize, // number of WHERE conditions
}

/// Feature-based query intent classifier.
pub struct IntentClassifier {
    /// Weight for each intent (higher = more likely when features match)
    _weights: Vec<f64>,
}

impl IntentClassifier {
    pub fn new() -> Self {
        Self {
            _weights: vec![1.0; 4],
        }
    }

    /// Classify a query based on extracted features.
    pub fn classify(&self, features: &QueryFeatures) -> QueryIntent {
        // Score each intent
        let lookup_score = self.score_lookup(features);
        let search_score = self.score_search(features);
        let analytics_score = self.score_analytics(features);
        let hybrid_score = self.score_hybrid(features);

        // Winner takes all
        let scores = [
            (QueryIntent::Lookup, lookup_score),
            (QueryIntent::Search, search_score),
            (QueryIntent::Analytics, analytics_score),
            (QueryIntent::Hybrid, hybrid_score),
        ];

        scores
            .iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(intent, _)| intent.clone())
            .unwrap_or(QueryIntent::Hybrid) // fallback
    }

    fn score_lookup(&self, f: &QueryFeatures) -> f64 {
        let mut score = 0.0;
        if f.has_pk_eq {
            score += 5.0;
        }
        if f.has_limit_1 {
            score += 2.0;
        }
        if f.column_count <= 5 {
            score += 1.0;
        }
        if !f.has_aggregate && !f.has_group_by {
            score += 1.0;
        }
        if !f.has_like && !f.has_fulltext {
            score += 1.0;
        }
        score
    }

    fn score_search(&self, f: &QueryFeatures) -> f64 {
        let mut score = 0.0;
        if f.has_like {
            score += 3.0;
        }
        if f.has_fulltext {
            score += 5.0;
        }
        if f.has_order_by {
            score += 1.0;
        }
        if !f.has_aggregate {
            score += 0.5;
        }
        if !f.has_vector_sim {
            score += 1.0;
        } // pure text search
        score
    }

    fn score_analytics(&self, f: &QueryFeatures) -> f64 {
        let mut score = 0.0;
        if f.has_aggregate {
            score += 4.0;
        }
        if f.has_group_by {
            score += 4.0;
        }
        if f.has_window {
            score += 3.0;
        }
        if f.has_join {
            score += 1.0;
        }
        if f.column_count > 5 {
            score += 0.5;
        }
        score
    }

    fn score_hybrid(&self, f: &QueryFeatures) -> f64 {
        let mut score = 0.0;
        if f.has_vector_sim {
            score += 4.0;
        }
        if f.has_fulltext && f.has_vector_sim {
            score += 5.0;
        } // both = clearly hybrid
        if f.has_like && f.has_vector_sim {
            score += 3.0;
        }
        score
    }

    /// Classify from a raw SQL string (simple heuristic parsing).
    pub fn classify_sql(&self, sql: &str) -> QueryIntent {
        let upper = sql.to_uppercase();
        let features = QueryFeatures {
            has_pk_eq: upper.contains("WHERE") && (upper.contains("= ?") || upper.contains("= $")),
            has_limit_1: upper.contains("LIMIT 1"),
            has_like: upper.contains("LIKE") || upper.contains("ILIKE"),
            has_fulltext: upper.contains("MATCH")
                || upper.contains("TS_QUERY")
                || upper.contains("SEARCH("),
            has_vector_sim: upper.contains("COSINE")
                || upper.contains("VECTOR")
                || upper.contains("EMBEDDING")
                || upper.contains("ANN("),
            has_group_by: upper.contains("GROUP BY"),
            has_aggregate: upper.contains("SUM(")
                || upper.contains("COUNT(")
                || upper.contains("AVG(")
                || upper.contains("MIN(")
                || upper.contains("MAX("),
            has_window: upper.contains("OVER(") || upper.contains("PARTITION BY"),
            has_order_by: upper.contains("ORDER BY"),
            has_join: upper.contains("JOIN"),
            column_count: 0, // not easily parseable from raw SQL
            predicate_count: upper.matches("AND").count() + 1,
        };

        self.classify(&features)
    }
}

impl Default for IntentClassifier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lookup_intent() {
        let clf = IntentClassifier::new();
        let features = QueryFeatures {
            has_pk_eq: true,
            has_limit_1: true,
            column_count: 3,
            ..Default::default()
        };
        assert_eq!(clf.classify(&features), QueryIntent::Lookup);
    }

    #[test]
    fn test_search_intent() {
        let clf = IntentClassifier::new();
        let features = QueryFeatures {
            has_fulltext: true,
            has_order_by: true,
            ..Default::default()
        };
        assert_eq!(clf.classify(&features), QueryIntent::Search);
    }

    #[test]
    fn test_analytics_intent() {
        let clf = IntentClassifier::new();
        let features = QueryFeatures {
            has_aggregate: true,
            has_group_by: true,
            has_join: true,
            ..Default::default()
        };
        assert_eq!(clf.classify(&features), QueryIntent::Analytics);
    }

    #[test]
    fn test_hybrid_intent() {
        let clf = IntentClassifier::new();
        let features = QueryFeatures {
            has_fulltext: true,
            has_vector_sim: true,
            ..Default::default()
        };
        assert_eq!(clf.classify(&features), QueryIntent::Hybrid);
    }

    #[test]
    fn test_sql_classification() {
        let clf = IntentClassifier::new();

        assert_eq!(
            clf.classify_sql("SELECT * FROM users WHERE id = $1 LIMIT 1"),
            QueryIntent::Lookup
        );

        assert_eq!(
            clf.classify_sql("SELECT * FROM docs WHERE body MATCH 'database' ORDER BY score"),
            QueryIntent::Search
        );

        assert_eq!(
            clf.classify_sql(
                "SELECT category, COUNT(*), SUM(amount) FROM orders GROUP BY category"
            ),
            QueryIntent::Analytics
        );

        assert_eq!(
            clf.classify_sql(
                "SELECT * FROM docs WHERE MATCH(body, 'query') AND COSINE(embedding, $1) > 0.8"
            ),
            QueryIntent::Hybrid
        );
    }
}
