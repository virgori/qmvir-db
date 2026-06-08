/*
 * SynonymEngine — Bidirectional and one-way synonym expansion.
 *
 * Port of _py_legacy/search_platform/synonym_engine/synonyms.py
 *
 * Usage:
 *   let mut eng = SynonymEngine::default();
 *   eng.add_synonym_group(&["car", "automobile", "vehicle"]);
 *   eng.add_one_way("usa", &["united states", "america"]);
 *   let expanded = eng.expand_query(&["car", "usa"]);
 */

use std::collections::{HashMap, HashSet};

/// Bidirectional + one-way synonym expansion engine.
///
/// * `add_synonym_group` — all terms in the group are equivalent (bidirectional).
/// * `add_one_way` — source expands to targets but NOT vice-versa.
/// * `expand` / `expand_query` — substitute each token with itself + its synonyms.
#[derive(Default, Clone)]
pub struct SynonymEngine {
    /// term (lowercase) → set of synonym terms (lowercase)
    map: HashMap<String, HashSet<String>>,
}

impl SynonymEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a group of equivalent synonyms (all bidirectional).
    pub fn add_synonym_group(&mut self, terms: &[&str]) {
        for &term in terms {
            for &other in terms {
                if term != other {
                    self.map
                        .entry(term.to_ascii_lowercase())
                        .or_default()
                        .insert(other.to_ascii_lowercase());
                }
            }
        }
    }

    /// One-way expansion: `source` → `targets`, but not the reverse.
    pub fn add_one_way(&mut self, source: &str, targets: &[&str]) {
        let entry = self.map.entry(source.to_ascii_lowercase()).or_default();
        for &t in targets {
            entry.insert(t.to_ascii_lowercase());
        }
    }

    /// Expand a single term: returns `[term] + sorted(synonyms)`.
    pub fn expand(&self, term: &str) -> Vec<String> {
        let lower = term.to_ascii_lowercase();
        let mut out = vec![lower.clone()];
        if let Some(syns) = self.map.get(&lower) {
            let mut sorted: Vec<String> = syns.iter().cloned().collect();
            sorted.sort();
            out.extend(sorted);
        }
        out
    }

    /// Expand all tokens in a query (deduplication preserves order of first occurrence).
    pub fn expand_query(&self, tokens: &[&str]) -> Vec<String> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        for &tok in tokens {
            for expanded in self.expand(tok) {
                if seen.insert(expanded.clone()) {
                    out.push(expanded);
                }
            }
        }
        out
    }

    /// Load synonyms from a `HashMap<term, vec<synonym>>` (bidirectional groups).
    pub fn load_from_map(&mut self, synonym_map: &HashMap<String, Vec<String>>) {
        for (key, values) in synonym_map {
            let mut group: Vec<&str> = vec![key.as_str()];
            let refs: Vec<&str> = values.iter().map(|s| s.as_str()).collect();
            group.extend(refs);
            self.add_synonym_group(&group);
        }
    }

    /// Number of terms that have at least one synonym registered.
    pub fn term_count(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bidirectional_group_expands_both_ways() {
        let mut e = SynonymEngine::new();
        e.add_synonym_group(&["car", "automobile", "vehicle"]);
        let car = e.expand("car");
        assert!(car.contains(&"automobile".to_string()));
        assert!(car.contains(&"vehicle".to_string()));
        let auto = e.expand("automobile");
        assert!(auto.contains(&"car".to_string()));
    }

    #[test]
    fn one_way_does_not_reverse() {
        let mut e = SynonymEngine::new();
        e.add_one_way("usa", &["united states", "america"]);
        let usa = e.expand("usa");
        assert!(usa.contains(&"united states".to_string()));
        let us = e.expand("united states");
        assert_eq!(us, vec!["united states"]);
    }

    #[test]
    fn expand_query_deduplicates() {
        let mut e = SynonymEngine::new();
        e.add_synonym_group(&["fast", "quick", "rapid"]);
        let result = e.expand_query(&["fast", "quick"]);
        let unique: HashSet<_> = result.iter().collect();
        assert_eq!(unique.len(), result.len(), "no duplicates");
        assert!(result.contains(&"rapid".to_string()));
    }

    #[test]
    fn unknown_term_returns_itself() {
        let e = SynonymEngine::new();
        assert_eq!(e.expand("unknown"), vec!["unknown"]);
    }
}
