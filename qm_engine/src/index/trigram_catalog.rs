/*
 * Trigram index for substring / LIKE '%text%' queries (pg_trgm-style).
 */

use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const TRIGRAM_SCAN_SELECTIVITY: f64 = 0.20;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct TrigramKey {
    pub table: String,
    pub column: String,
}

#[derive(Debug, Clone)]
pub struct TrigramIndexMeta {
    pub name: String,
    pub key: TrigramKey,
}

pub struct ManagedTrigramIndex {
    pub meta: TrigramIndexMeta,
    postings: RwLock<HashMap<String, Vec<i64>>>,
}

fn trigrams_for_index(text: &str) -> Vec<String> {
    let padded = format!("  {} ", text.to_lowercase());
    let chars: Vec<char> = padded.chars().collect();
    let mut out = Vec::new();
    if chars.len() < 3 {
        return out;
    }
    let mut seen = HashSet::new();
    for i in 0..=chars.len() - 3 {
        let gram: String = chars[i..i + 3].iter().collect();
        if seen.insert(gram.clone()) {
            out.push(gram);
        }
    }
    out
}

fn trigrams_for_query(pattern: &str) -> Vec<String> {
    let s = pattern.to_lowercase();
    let chars: Vec<char> = s.chars().collect();
    let mut grams = Vec::new();
    let mut seen = HashSet::new();
    if chars.len() >= 3 {
        for i in 0..=chars.len() - 3 {
            let gram: String = chars[i..i + 3].iter().collect();
            if seen.insert(gram.clone()) {
                grams.push(gram);
            }
        }
    } else if !s.is_empty() {
        grams = trigrams_for_index(pattern);
    }
    grams.sort();
    grams
}

fn insert_sorted_unique(list: &mut Vec<i64>, row_id: i64) {
    match list.binary_search(&row_id) {
        Ok(_) => {}
        Err(pos) => list.insert(pos, row_id),
    }
}

fn remove_sorted(list: &mut Vec<i64>, row_id: i64) {
    if let Ok(pos) = list.binary_search(&row_id) {
        list.remove(pos);
    }
}

fn intersect_sorted(a: &[i64], b: &[i64]) -> Vec<i64> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

impl ManagedTrigramIndex {
    pub fn new(name: String, table: String, column: String) -> Self {
        Self {
            meta: TrigramIndexMeta {
                name,
                key: TrigramKey { table, column },
            },
            postings: RwLock::new(HashMap::new()),
        }
    }

    pub fn index_text(&self, row_id: i64, text: &str) {
        let mut postings = self.postings.write();
        for gram in trigrams_for_index(text) {
            insert_sorted_unique(postings.entry(gram).or_default(), row_id);
        }
    }

    pub fn remove_text(&self, row_id: i64, text: &str) {
        let mut postings = self.postings.write();
        for gram in trigrams_for_index(text) {
            if let Some(rows) = postings.get_mut(&gram) {
                remove_sorted(rows, row_id);
                if rows.is_empty() {
                    postings.remove(&gram);
                }
            }
        }
    }

    /// Intersect posting lists starting from the rarest trigram (pg_trgm-style).
    pub fn search_contains(&self, pattern: &str) -> Vec<i64> {
        let grams = trigrams_for_query(pattern);
        if grams.is_empty() {
            return Vec::new();
        }
        let postings = self.postings.read();
        let mut lists: Vec<&[i64]> = Vec::with_capacity(grams.len());
        for gram in &grams {
            let Some(rows) = postings.get(gram) else {
                return Vec::new();
            };
            lists.push(rows.as_slice());
        }
        lists.sort_by_key(|rows| rows.len());

        let mut acc: Vec<i64> = lists[0].to_vec();
        for rows in lists.iter().skip(1) {
            if acc.is_empty() {
                return Vec::new();
            }
            if acc.len() > 4_096 || rows.len() < acc.len() / 3 {
                let probe: HashSet<i64> = rows.iter().copied().collect();
                acc.retain(|id| probe.contains(id));
            } else if rows.len() > 4_096 {
                let acc_set: HashSet<i64> = acc.iter().copied().collect();
                acc = rows
                    .iter()
                    .copied()
                    .filter(|id| acc_set.contains(id))
                    .collect();
            } else {
                acc = intersect_sorted(&acc, rows);
            }
        }
        acc
    }

    /// True when trigram intersection alone is sufficient (no LIKE re-check needed).
    pub fn contains_match_is_exact(&self, pattern: &str) -> bool {
        !trigrams_for_query(pattern).is_empty()
    }

    pub fn rarest_gram_selectivity(&self, pattern: &str, table_rows: usize) -> Option<f64> {
        if table_rows == 0 {
            return None;
        }
        let grams = trigrams_for_query(pattern);
        let postings = self.postings.read();
        let mut min_len = usize::MAX;
        for gram in grams {
            let Some(rows) = postings.get(&gram) else {
                return Some(0.0);
            };
            min_len = min_len.min(rows.len());
        }
        if min_len == usize::MAX {
            None
        } else {
            Some(min_len as f64 / table_rows as f64)
        }
    }

    /// Prefer sequential scan when trigram intersection would touch most rows.
    pub fn should_scan_table(&self, pattern: &str, table_rows: usize) -> bool {
        self.rarest_gram_selectivity(pattern, table_rows)
            .map(|s| s > TRIGRAM_SCAN_SELECTIVITY)
            .unwrap_or(false)
    }

    pub fn clear(&self) {
        self.postings.write().clear();
    }

    pub fn snapshot_postings(&self) -> HashMap<String, Vec<i64>> {
        self.postings.read().clone()
    }

    pub fn restore_postings(&self, postings: HashMap<String, Vec<i64>>) {
        *self.postings.write() = postings;
    }
}

#[derive(Clone)]
pub struct TrigramCatalogSnapshot {
    pub entries: HashMap<String, (TrigramIndexMeta, HashMap<String, Vec<i64>>)>,
}

pub struct TrigramCatalog {
    by_name: RwLock<HashMap<String, Arc<ManagedTrigramIndex>>>,
    by_key: RwLock<HashMap<TrigramKey, Arc<ManagedTrigramIndex>>>,
}

impl TrigramCatalog {
    pub fn new() -> Self {
        Self {
            by_name: RwLock::new(HashMap::new()),
            by_key: RwLock::new(HashMap::new()),
        }
    }

    pub fn create_index(
        &self,
        name: String,
        table: String,
        column: String,
    ) -> Arc<ManagedTrigramIndex> {
        let entry = Arc::new(ManagedTrigramIndex::new(
            name.clone(),
            table.clone(),
            column.clone(),
        ));
        self.by_name.write().insert(name, Arc::clone(&entry));
        self.by_key
            .write()
            .insert(entry.meta.key.clone(), Arc::clone(&entry));
        entry
    }

    pub fn drop_index(&self, name: &str) -> bool {
        let Some(entry) = self.by_name.write().remove(name) else {
            return false;
        };
        self.by_key.write().remove(&entry.meta.key);
        true
    }

    pub fn find(&self, table: &str, column: &str) -> Option<Arc<ManagedTrigramIndex>> {
        self.by_key.read().get(&TrigramKey {
            table: table.to_string(),
            column: column.to_string(),
        }).cloned()
    }

    pub fn indexes_for_table(&self, table: &str) -> Vec<Arc<ManagedTrigramIndex>> {
        self.by_name
            .read()
            .values()
            .filter(|entry| entry.meta.key.table == table)
            .cloned()
            .collect()
    }

    pub fn snapshot(&self) -> TrigramCatalogSnapshot {
        let entries = self
            .by_name
            .read()
            .iter()
            .map(|(name, entry)| {
                (
                    name.clone(),
                    (entry.meta.clone(), entry.snapshot_postings()),
                )
            })
            .collect();
        TrigramCatalogSnapshot { entries }
    }

    pub fn restore_snapshot(&self, snapshot: TrigramCatalogSnapshot) {
        self.by_name.write().clear();
        self.by_key.write().clear();
        for (name, (meta, postings)) in snapshot.entries {
            let entry = Arc::new(ManagedTrigramIndex::new(
                name.clone(),
                meta.key.table.clone(),
                meta.key.column.clone(),
            ));
            entry.restore_postings(postings);
            self.by_name.write().insert(name, Arc::clone(&entry));
            self.by_key.write().insert(entry.meta.key.clone(), entry);
        }
    }
}
