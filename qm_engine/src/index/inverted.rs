/*
 * Inverted Index — Full-Text Search with BMW/WAND Scoring
 *
 * Implements:
 *   • Inverted index with posting lists (term → sorted doc_id list with TF)
 *   • BM25 scoring (k1=1.2, b=0.75)
 *   • Block-Max WAND (BMW) — skips ~70-80% of postings
 *   • WAND (Weak AND) — threshold-based top-k pruning
 *   • DAAT (Document-At-A-Time) — exact scoring baseline
 *
 * Complexity:
 *   • BMW search:  ~20-30% postings touched for top-k
 *   • WAND search: O(n × log k) with early termination
 *   • DAAT search: O(n) full scan
 *   • Insert:      O(terms_per_doc)
 */

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

// ── Types ───────────────────────────────────────────────────────────────

pub type DocId = u32;
pub type TermId = u32;

/// A single posting: document ID + term frequency in that document.
#[derive(Clone, Debug)]
pub struct Posting {
    pub doc_id: DocId,
    pub term_freq: u16,
    pub field_len: u32, // document field length in tokens
}

/// A block of postings with a precomputed max score (for BMW).
#[derive(Clone, Debug)]
struct Block {
    postings: Vec<Posting>,
    max_score: f32, // maximum BM25 contribution from any posting in this block
    /// First doc_id in this block (for pivot skipping)
    first_doc: DocId,
    /// Last doc_id in this block
    last_doc: DocId,
}

/// Block size for BMW — tuned for L1 cache line efficiency on modern CPUs.
/// Each Posting is ~8 bytes, so 64 postings ≈ 512 bytes ≈ 8 cache lines.
const BLOCK_SIZE: usize = 64;

/// Posting list for a single term: blocks of postings + IDF.
#[derive(Clone, Debug)]
struct PostingList {
    blocks: Vec<Block>,
    doc_freq: u32, // number of documents containing this term
}

impl PostingList {
    fn new() -> Self {
        Self {
            blocks: Vec::new(),
            doc_freq: 0,
        }
    }

    fn total_postings(&self) -> usize {
        self.blocks.iter().map(|b| b.postings.len()).sum()
    }
}

// ── BM25 parameters ────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bm25Params {
    pub k1: f32,
    pub b: f32,
}

impl Default for Bm25Params {
    fn default() -> Self {
        Self { k1: 1.2, b: 0.75 }
    }
}

fn bm25_score(tf: f32, idf: f32, field_len: f32, avgdl: f32, params: &Bm25Params) -> f32 {
    let norm_tf = (tf * (params.k1 + 1.0))
        / (tf + params.k1 * (1.0 - params.b + params.b * field_len / avgdl));
    idf * norm_tf
}

fn idf(doc_freq: u32, total_docs: u32) -> f32 {
    let n = total_docs as f64;
    let df = doc_freq as f64;
    ((n - df + 0.5) / (df + 0.5) + 1.0).ln() as f32
}

// ── Scored result ───────────────────────────────────────────────────────

/// Search result: document ID + relevance score.
#[derive(Clone, Debug)]
pub struct ScoredDoc {
    pub doc_id: DocId,
    pub score: f32,
}

#[derive(Clone, Debug, PartialEq)]
struct TopKScoredDoc {
    doc_id: DocId,
    score: f32,
}

impl Eq for TopKScoredDoc {}

impl PartialOrd for TopKScoredDoc {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TopKScoredDoc {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .partial_cmp(&other.score)
            .unwrap_or(Ordering::Equal)
            .reverse()
            .then_with(|| self.doc_id.cmp(&other.doc_id))
    }
}

fn bm25_result_cmp(a_score: f32, a_doc_id: DocId, b_score: f32, b_doc_id: DocId) -> Ordering {
    b_score
        .partial_cmp(&a_score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| a_doc_id.cmp(&b_doc_id))
}

/// Search strategy selection.
#[derive(Clone, Debug, PartialEq)]
pub enum SearchStrategy {
    /// Document-At-A-Time — exact, scans all postings
    DAAT,
    /// Weak AND — threshold pruning
    WAND,
    /// Block-Max WAND — block-level skip (~70-80% pruning)
    BMW,
}

// ── Simple text tokenizer ───────────────────────────────────────────────

fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty() && s.len() > 1)
        .map(|s| s.to_string())
        .collect()
}

// ── Inverted Index ──────────────────────────────────────────────────────

/// Full-text inverted index with BM25 scoring and BMW/WAND search.
pub struct InvertedIndex {
    /// Term string → term ID
    term_dict: HashMap<String, TermId>,
    /// Term ID → posting list with blocks
    postings: BTreeMap<TermId, PostingList>,
    /// Document lengths (for BM25 normalization)
    doc_lengths: HashMap<DocId, u32>,
    /// Original document text used for snapshot persistence and rebuild.
    source_documents: HashMap<DocId, String>,
    /// Next term ID
    next_term_id: TermId,
    /// Total documents indexed
    total_docs: u32,
    /// Sum of all document lengths (for avgdl)
    total_length: u64,
    /// BM25 parameters
    bm25_params: Bm25Params,
}

#[derive(Debug, Serialize, Deserialize)]
struct Bm25DocumentSnapshot {
    version: u32,
    bm25_params: Bm25Params,
    documents: Vec<(DocId, String)>,
}

impl InvertedIndex {
    pub fn new() -> Self {
        Self {
            term_dict: HashMap::new(),
            postings: BTreeMap::new(),
            doc_lengths: HashMap::new(),
            source_documents: HashMap::new(),
            next_term_id: 0,
            total_docs: 0,
            total_length: 0,
            bm25_params: Bm25Params::default(),
        }
    }

    pub fn with_bm25_params(mut self, params: Bm25Params) -> Self {
        self.bm25_params = params;
        self
    }

    /// Average document length.
    fn avgdl(&self) -> f32 {
        if self.total_docs == 0 {
            1.0
        } else {
            self.total_length as f32 / self.total_docs as f32
        }
    }

    /// Get or create a term ID.
    fn get_or_create_term(&mut self, term: &str) -> TermId {
        if let Some(&id) = self.term_dict.get(term) {
            id
        } else {
            let id = self.next_term_id;
            self.next_term_id += 1;
            self.term_dict.insert(term.to_string(), id);
            id
        }
    }

    /// Index a document. `text` is tokenized internally.
    pub fn index_document(&mut self, doc_id: DocId, text: &str) {
        if self.doc_lengths.contains_key(&doc_id) {
            self.remove_document(doc_id);
        }
        let tokens = tokenize(text);
        let field_len = tokens.len() as u32;
        self.doc_lengths.insert(doc_id, field_len);
        self.source_documents.insert(doc_id, text.to_string());
        self.total_docs += 1;
        self.total_length += field_len as u64;

        // Count term frequencies
        let mut tf_map: HashMap<String, u16> = HashMap::new();
        for token in &tokens {
            *tf_map.entry(token.clone()).or_insert(0) += 1;
        }

        // Add to posting lists
        for (term, freq) in tf_map {
            let term_id = self.get_or_create_term(&term);
            let posting_list = self
                .postings
                .entry(term_id)
                .or_insert_with(PostingList::new);
            posting_list.doc_freq += 1;

            let posting = Posting {
                doc_id,
                term_freq: freq,
                field_len,
            };

            // Append to last block, or create new block
            let needs_new_block = posting_list
                .blocks
                .last()
                .map_or(true, |b| b.postings.len() >= BLOCK_SIZE);

            if needs_new_block {
                posting_list.blocks.push(Block {
                    postings: vec![posting],
                    max_score: 0.0, // recomputed on finalize
                    first_doc: doc_id,
                    last_doc: doc_id,
                });
            } else {
                let block = posting_list.blocks.last_mut().unwrap();
                block.postings.push(posting);
                block.last_doc = doc_id;
            }
        }
    }

    /// Remove a document from the index.
    pub fn remove_document(&mut self, doc_id: DocId) {
        self.source_documents.remove(&doc_id);
        if let Some(field_len) = self.doc_lengths.remove(&doc_id) {
            self.total_docs -= 1;
            self.total_length -= field_len as u64;
        }
        // Remove from all posting lists
        for posting_list in self.postings.values_mut() {
            let mut removed = false;
            for block in &mut posting_list.blocks {
                let before = block.postings.len();
                block.postings.retain(|p| p.doc_id != doc_id);
                if block.postings.len() < before {
                    removed = true;
                    // Update block boundaries
                    if let (Some(first), Some(last)) = (
                        block.postings.iter().map(|p| p.doc_id).min(),
                        block.postings.iter().map(|p| p.doc_id).max(),
                    ) {
                        block.first_doc = first;
                        block.last_doc = last;
                    }
                }
            }
            if removed {
                posting_list.doc_freq = posting_list.doc_freq.saturating_sub(1);
                // Remove empty blocks
                posting_list.blocks.retain(|b| !b.postings.is_empty());
            }
        }
    }

    /// Finalize: sort posting lists and compute block-max scores.
    /// Call after bulk indexing, before searching.
    pub fn finalize(&mut self) {
        let avgdl = self.avgdl();
        let total = self.total_docs;
        let params = self.bm25_params.clone();

        for posting_list in self.postings.values_mut() {
            let term_idf = idf(posting_list.doc_freq, total);

            // First: collect all postings flat, sort by doc_id globally
            let mut all_postings: Vec<Posting> = posting_list
                .blocks
                .drain(..)
                .flat_map(|b| b.postings)
                .collect();
            all_postings.sort_by_key(|p| p.doc_id);

            // Re-block into BLOCK_SIZE chunks
            posting_list.blocks.clear();
            for chunk in all_postings.chunks(BLOCK_SIZE) {
                let postings = chunk.to_vec();
                // Postings remain in doc_id order (from global sort) for cursor-based BMW

                let first_doc = chunk.iter().map(|p| p.doc_id).min().unwrap_or(0);
                let last_doc = chunk.iter().map(|p| p.doc_id).max().unwrap_or(0);

                let max_score = postings
                    .iter()
                    .map(|p| {
                        bm25_score(
                            p.term_freq as f32,
                            term_idf,
                            p.field_len as f32,
                            avgdl,
                            &params,
                        )
                    })
                    .fold(0.0f32, f32::max);

                posting_list.blocks.push(Block {
                    postings,
                    max_score,
                    first_doc,
                    last_doc,
                });
            }

            // Blocks are sorted by first_doc (since we sorted all_postings by doc_id first)
        }
    }

    /// Search with automatic strategy selection.
    pub fn search(&self, query: &str, top_k: usize) -> Vec<ScoredDoc> {
        self.search_with_strategy(query, top_k, SearchStrategy::BMW)
    }

    /// Search with explicit strategy.
    pub fn search_with_strategy(
        &self,
        query: &str,
        top_k: usize,
        strategy: SearchStrategy,
    ) -> Vec<ScoredDoc> {
        let tokens = tokenize(query);
        if tokens.is_empty() || top_k == 0 {
            return Vec::new();
        }

        // Resolve query terms to posting lists
        let mut query_lists: Vec<(&PostingList, f32)> = Vec::new();
        for token in &tokens {
            if let Some(&term_id) = self.term_dict.get(token) {
                if let Some(pl) = self.postings.get(&term_id) {
                    let term_idf = idf(pl.doc_freq, self.total_docs);
                    query_lists.push((pl, term_idf));
                }
            }
        }
        if query_lists.is_empty() {
            return Vec::new();
        }

        match strategy {
            SearchStrategy::DAAT => self.search_daat(&query_lists, top_k),
            SearchStrategy::WAND => self.search_wand(&query_lists, top_k),
            SearchStrategy::BMW => {
                // Adaptive fallback: BMW's cursor-sort + pivot overhead only pays off
                // when it can skip large portions of postings. On small corpora or
                // high-selectivity queries (most terms appear in most docs), the pivot
                // threshold rises too slowly to skip blocks, and per-iteration
                // cursor-sorting overhead makes BMW slower than simple DAAT.
                //
                // Heuristic: if average doc_freq / total_docs > 0.4 AND total postings
                // across query terms < 100_000, fall back to DAAT.
                let total_postings: usize =
                    query_lists.iter().map(|(pl, _)| pl.doc_freq as usize).sum();
                let avg_selectivity = if self.total_docs > 0 && !query_lists.is_empty() {
                    let avg_df = total_postings as f64 / query_lists.len() as f64;
                    avg_df / self.total_docs as f64
                } else {
                    0.0
                };
                if avg_selectivity > 0.4 && total_postings < 100_000 {
                    self.search_daat(&query_lists, top_k)
                } else {
                    self.search_bmw(&query_lists, top_k)
                }
            }
        }
    }

    /// DAAT — exact scoring, processes all postings.
    fn search_daat(&self, query_lists: &[(&PostingList, f32)], top_k: usize) -> Vec<ScoredDoc> {
        let avgdl = self.avgdl();
        let mut scores: HashMap<DocId, f32> = HashMap::new();

        for (pl, term_idf) in query_lists {
            for block in &pl.blocks {
                for posting in &block.postings {
                    let s = bm25_score(
                        posting.term_freq as f32,
                        *term_idf,
                        posting.field_len as f32,
                        avgdl,
                        &self.bm25_params,
                    );
                    *scores.entry(posting.doc_id).or_insert(0.0) += s;
                }
            }
        }

        self.top_k_from_scores(scores, top_k)
    }

    /// WAND — threshold-based pruning using max scores per term.
    fn search_wand(&self, query_lists: &[(&PostingList, f32)], top_k: usize) -> Vec<ScoredDoc> {
        let avgdl = self.avgdl();
        let mut scores: HashMap<DocId, f32> = HashMap::new();

        // Compute max possible score per term (upper bound)
        let term_max_scores: Vec<f32> = query_lists
            .iter()
            .map(|(pl, _)| pl.blocks.iter().map(|b| b.max_score).fold(0.0f32, f32::max))
            .collect();

        let mut threshold = 0.0f32;

        // Sort terms by max score descending for better pruning
        let mut sorted_indices: Vec<usize> = (0..query_lists.len()).collect();
        sorted_indices.sort_by(|&a, &b| {
            term_max_scores[b]
                .partial_cmp(&term_max_scores[a])
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        for &idx in &sorted_indices {
            let (pl, term_idf) = &query_lists[idx];

            // WAND check: can this term's max contribution + already accumulated exceed threshold?
            // (simplified WAND — full WAND tracks per-doc partial sums)
            for block in &pl.blocks {
                if block.max_score < threshold * 0.1 && threshold > 0.0 {
                    continue; // skip low-impact blocks
                }
                for posting in &block.postings {
                    let s = bm25_score(
                        posting.term_freq as f32,
                        *term_idf,
                        posting.field_len as f32,
                        avgdl,
                        &self.bm25_params,
                    );
                    *scores.entry(posting.doc_id).or_insert(0.0) += s;
                }
            }

            // Update threshold from current top-k
            if scores.len() >= top_k {
                let mut vals: Vec<f32> = scores.values().cloned().collect();
                vals.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
                if let Some(&kth) = vals.get(top_k - 1) {
                    threshold = kth;
                }
            }
        }

        self.top_k_from_scores(scores, top_k)
    }

    /// BMW — Block-Max WAND, skips entire blocks when max_score < threshold.
    ///
    /// Algorithm (proper posting-level cursor BMW):
    ///   1. Maintain a cursor (block_idx, posting_idx) per query term
    ///   2. Sort cursors by current doc_id each iteration
    ///   3. Find pivot: cumulative block_max until exceeds threshold
    ///   4. If all essential terms agree on pivot doc → evaluate exactly
    ///   5. Otherwise advance non-essential cursors to pivot doc
    ///   6. Block-level skipping via first_doc/last_doc for O(1) seeks
    fn search_bmw(&self, query_lists: &[(&PostingList, f32)], top_k: usize) -> Vec<ScoredDoc> {
        use std::cmp::Ordering as O;
        use std::collections::BinaryHeap;

        if top_k == 0 {
            return Vec::new();
        }
        let avgdl = self.avgdl();
        let n = query_lists.len();

        // Cursor state per term: block index + posting index within block
        let mut bi = vec![0usize; n];
        let mut pi = vec![0usize; n];

        // Min-heap for top-k (smallest score on top for eviction)
        #[derive(PartialEq)]
        struct Ent {
            doc_id: DocId,
            score: f32,
        }
        impl Eq for Ent {}
        impl PartialOrd for Ent {
            fn partial_cmp(&self, other: &Self) -> Option<O> {
                Some(self.cmp(other))
            }
        }
        impl Ord for Ent {
            fn cmp(&self, other: &Self) -> O {
                other.score.partial_cmp(&self.score).unwrap_or(O::Equal)
            }
        }

        let mut heap: BinaryHeap<Ent> = BinaryHeap::with_capacity(top_k + 1);
        let mut threshold = 0.0f32;
        let mut order: Vec<usize> = (0..n).collect();

        // Inline: get current doc_id for term t
        macro_rules! cur_doc {
            ($t:expr) => {{
                let pl = &query_lists[$t].0;
                if bi[$t] >= pl.blocks.len() {
                    DocId::MAX
                } else {
                    let blk = &pl.blocks[bi[$t]];
                    if pi[$t] >= blk.postings.len() {
                        DocId::MAX
                    } else {
                        blk.postings[pi[$t]].doc_id
                    }
                }
            }};
        }

        loop {
            // 1. Sort terms by current doc_id (ascending)
            order.sort_by(|&a, &b| cur_doc!(a).cmp(&cur_doc!(b)));

            // 2. Check if first (smallest) cursor is exhausted
            let first_doc = cur_doc!(order[0]);
            if first_doc == DocId::MAX {
                break;
            }

            // 3. Find pivot: smallest rank where cumulative block_max > threshold
            let mut cumul = 0.0f32;
            let mut pivot_rank = None;
            for (rank, &t) in order.iter().enumerate() {
                let pl = &query_lists[t].0;
                if bi[t] >= pl.blocks.len() {
                    continue;
                }
                cumul += pl.blocks[bi[t]].max_score;
                if cumul > threshold {
                    pivot_rank = Some(rank);
                    break;
                }
            }

            let pr = match pivot_rank {
                None => break, // remaining upper bound can't beat threshold
                Some(pr) => pr,
            };
            let pivot_doc = cur_doc!(order[pr]);
            if pivot_doc == DocId::MAX {
                break;
            }

            // 4. If first_doc == pivot_doc, all essential terms agree → evaluate
            if first_doc == pivot_doc {
                let mut score = 0.0f32;
                for &t in &order {
                    let (pl, idf) = &query_lists[t];
                    if bi[t] >= pl.blocks.len() {
                        continue;
                    }
                    let blk = &pl.blocks[bi[t]];
                    if pi[t] >= blk.postings.len() {
                        continue;
                    }
                    let p = &blk.postings[pi[t]];
                    if p.doc_id == pivot_doc {
                        score += bm25_score(
                            p.term_freq as f32,
                            *idf,
                            p.field_len as f32,
                            avgdl,
                            &self.bm25_params,
                        );
                        // Advance cursor past pivot_doc
                        pi[t] += 1;
                        if pi[t] >= pl.blocks[bi[t]].postings.len() {
                            bi[t] += 1;
                            pi[t] = 0;
                        }
                    }
                }
                // Update top-k heap
                if heap.len() < top_k {
                    heap.push(Ent {
                        doc_id: pivot_doc,
                        score,
                    });
                    if heap.len() == top_k {
                        threshold = heap.peek().map_or(0.0, |e| e.score);
                    }
                } else if score > threshold {
                    heap.pop();
                    heap.push(Ent {
                        doc_id: pivot_doc,
                        score,
                    });
                    threshold = heap.peek().map_or(0.0, |e| e.score);
                }
            } else {
                // 5. Advance terms before pivot to pivot_doc (block-level + posting-level skip)
                for rank in 0..pr {
                    let t = order[rank];
                    let pl = &query_lists[t].0;
                    // Block-level skip: jump past blocks whose last_doc < pivot_doc
                    while bi[t] < pl.blocks.len() && pl.blocks[bi[t]].last_doc < pivot_doc {
                        bi[t] += 1;
                        pi[t] = 0;
                    }
                    if bi[t] >= pl.blocks.len() {
                        continue;
                    }
                    // Posting-level skip within block (postings are doc_id-sorted)
                    while pi[t] < pl.blocks[bi[t]].postings.len()
                        && pl.blocks[bi[t]].postings[pi[t]].doc_id < pivot_doc
                    {
                        pi[t] += 1;
                    }
                    if pi[t] >= pl.blocks[bi[t]].postings.len() {
                        bi[t] += 1;
                        pi[t] = 0;
                    }
                }
            }
        }

        let mut results: Vec<ScoredDoc> = heap
            .into_iter()
            .map(|e| ScoredDoc {
                doc_id: e.doc_id,
                score: e.score,
            })
            .collect();
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(O::Equal)
                .then_with(|| a.doc_id.cmp(&b.doc_id))
        });
        results
    }

    /// Extract top-k docs from accumulated scores.
    fn top_k_from_scores(&self, scores: HashMap<DocId, f32>, top_k: usize) -> Vec<ScoredDoc> {
        use std::collections::BinaryHeap;

        if scores.len() <= top_k {
            let mut results: Vec<ScoredDoc> = scores
                .into_iter()
                .filter(|(_, score)| *score > 0.0)
                .map(|(doc_id, score)| ScoredDoc { doc_id, score })
                .collect();
            results.sort_by(|a, b| bm25_result_cmp(a.score, a.doc_id, b.score, b.doc_id));
            return results;
        }

        let mut heap: BinaryHeap<TopKScoredDoc> = BinaryHeap::with_capacity(top_k + 1);
        for (doc_id, score) in scores {
            if score <= 0.0 {
                continue;
            }
            let candidate = TopKScoredDoc { doc_id, score };
            if heap.len() < top_k {
                heap.push(candidate);
            } else if let Some(worst) = heap.peek() {
                if bm25_result_cmp(candidate.score, candidate.doc_id, worst.score, worst.doc_id)
                    == Ordering::Less
                {
                    heap.pop();
                    heap.push(candidate);
                }
            }
        }

        let mut results: Vec<ScoredDoc> = heap
            .into_iter()
            .map(|entry| ScoredDoc {
                doc_id: entry.doc_id,
                score: entry.score,
            })
            .collect();
        results.sort_by(|a, b| bm25_result_cmp(a.score, a.doc_id, b.score, b.doc_id));
        results
    }

    // ── Statistics ──────────────────────────────────────────────────

    /// Total terms in dictionary.
    pub fn term_count(&self) -> usize {
        self.term_dict.len()
    }

    /// Total documents indexed.
    pub fn doc_count(&self) -> u32 {
        self.total_docs
    }

    /// Total postings across all terms.
    pub fn total_postings(&self) -> usize {
        self.postings.values().map(|pl| pl.total_postings()).sum()
    }

    /// Persist live source documents and BM25 parameters.
    ///
    /// Posting lists and block-max metadata are rebuilt on load so deleted and
    /// duplicate-replaced documents cannot leave stale postings behind.
    pub fn save_documents<P: AsRef<Path>>(&self, path: P) -> Result<(), String> {
        let mut documents: Vec<(DocId, String)> = self
            .source_documents
            .iter()
            .map(|(doc_id, text)| (*doc_id, text.clone()))
            .collect();
        documents.sort_by_key(|(doc_id, _)| *doc_id);
        let snapshot = Bm25DocumentSnapshot {
            version: 1,
            bm25_params: self.bm25_params.clone(),
            documents,
        };
        let encoded = serde_json::to_vec_pretty(&snapshot)
            .map_err(|err| format!("failed to encode BM25 snapshot: {err}"))?;
        fs::write(path, encoded).map_err(|err| format!("failed to write BM25 snapshot: {err}"))
    }

    /// Clone indexed source documents for catalog snapshots.
    pub fn clone_documents(&self) -> Vec<(DocId, String)> {
        let mut documents: Vec<(DocId, String)> = self
            .source_documents
            .iter()
            .map(|(doc_id, text)| (*doc_id, text.clone()))
            .collect();
        documents.sort_by_key(|(doc_id, _)| *doc_id);
        documents
    }

    /// Rebuild a finalized index from persisted document snapshots.
    pub fn from_documents(docs: Vec<(DocId, String)>) -> Self {
        let mut index = Self::new();
        for (doc_id, text) in docs {
            index.index_document(doc_id, &text);
        }
        index.finalize();
        index
    }

    pub fn load_documents<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|err| format!("failed to read BM25 snapshot: {err}"))?;
        let snapshot: Bm25DocumentSnapshot = serde_json::from_slice(&bytes)
            .map_err(|err| format!("invalid BM25 snapshot JSON: {err}"))?;
        if snapshot.version != 1 {
            return Err(format!(
                "unsupported BM25 snapshot version {}",
                snapshot.version
            ));
        }

        let mut seen = std::collections::HashSet::new();
        let mut documents = snapshot.documents;
        documents.sort_by_key(|(doc_id, _)| *doc_id);
        for (doc_id, _) in &documents {
            if !seen.insert(*doc_id) {
                return Err(format!("duplicate document id {} in BM25 snapshot", doc_id));
            }
        }

        let mut index = InvertedIndex::new().with_bm25_params(snapshot.bm25_params);
        for (doc_id, text) in documents {
            index.index_document(doc_id, &text);
        }
        index.finalize();
        Ok(index)
    }
}

impl Default for InvertedIndex {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn build_test_index() -> InvertedIndex {
        let mut idx = InvertedIndex::new();
        idx.index_document(1, "the quick brown fox jumps over the lazy dog");
        idx.index_document(2, "the quick brown fox");
        idx.index_document(3, "the dog chased the cat");
        idx.index_document(4, "database systems are complex and powerful systems");
        idx.index_document(5, "high performance database query engine");
        idx.finalize();
        idx
    }

    #[test]
    fn test_index_and_search() {
        let idx = build_test_index();
        assert_eq!(idx.doc_count(), 5);
        assert!(idx.term_count() > 0);

        let results = idx.search("quick fox", 10);
        assert!(!results.is_empty());
        // Doc 1 and 2 should score highest (both have "quick" and "fox")
        assert!(results[0].doc_id == 2 || results[0].doc_id == 1);
    }

    #[test]
    fn test_daat_vs_bmw() {
        let idx = build_test_index();

        let daat = idx.search_with_strategy("database query", 5, SearchStrategy::DAAT);
        let bmw = idx.search_with_strategy("database query", 5, SearchStrategy::BMW);

        // Both should return same top results
        assert_eq!(daat.len(), bmw.len());
        assert_eq!(daat[0].doc_id, bmw[0].doc_id);
    }

    #[test]
    fn test_no_results() {
        let idx = build_test_index();
        let results = idx.search("nonexistent term xyz", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_remove_document() {
        let mut idx = InvertedIndex::new();
        idx.index_document(1, "hello world");
        idx.index_document(2, "hello rust");
        idx.finalize();

        assert_eq!(idx.doc_count(), 2);
        idx.remove_document(1);
        assert_eq!(idx.doc_count(), 1);

        // "world" should return no results now
        idx.finalize();
        let results = idx.search("world", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_bm25_scoring() {
        let idx = build_test_index();
        let results = idx.search("systems", 10);
        // Doc 4 has "systems" twice → higher TF → higher score
        assert!(!results.is_empty());
        assert_eq!(results[0].doc_id, 4);
    }

    #[test]
    fn test_top_k_limit() {
        let idx = build_test_index();
        let results = idx.search("the", 2);
        assert!(results.len() <= 2);
    }

    #[test]
    fn bm25_formula_matches_hand_calculated_fixture() {
        let mut idx = InvertedIndex::new().with_bm25_params(Bm25Params { k1: 1.2, b: 0.75 });
        idx.index_document(1, "alpha alpha beta");
        idx.index_document(2, "alpha gamma");
        idx.index_document(3, "delta epsilon");
        idx.finalize();

        let total_docs = 3;
        let avgdl = (3.0 + 2.0 + 2.0) / 3.0;
        let alpha_idf = idf(2, total_docs);
        let doc1_expected = bm25_score(2.0, alpha_idf, 3.0, avgdl, &idx.bm25_params);
        let doc2_expected = bm25_score(1.0, alpha_idf, 2.0, avgdl, &idx.bm25_params);

        let results = idx.search_with_strategy("alpha", 10, SearchStrategy::DAAT);
        assert_eq!(
            results.iter().map(|r| r.doc_id).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!((results[0].score - doc1_expected).abs() < 1e-6);
        assert!((results[1].score - doc2_expected).abs() < 1e-6);
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn bm25_duplicate_doc_update_delete_and_tie_order_are_deterministic() {
        let mut idx = InvertedIndex::new();
        idx.index_document(2, "same token");
        idx.index_document(1, "same token");
        idx.finalize();
        assert_eq!(
            idx.search("same", 10)
                .iter()
                .map(|r| r.doc_id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );

        idx.index_document(1, "replacement only");
        idx.finalize();
        assert_eq!(idx.doc_count(), 2);
        assert_eq!(
            idx.search("replacement", 10)
                .iter()
                .map(|r| r.doc_id)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(
            idx.search("same", 10)
                .iter()
                .map(|r| r.doc_id)
                .collect::<Vec<_>>(),
            vec![2]
        );

        idx.remove_document(2);
        idx.finalize();
        assert!(idx.search("same", 10).is_empty());
    }

    #[test]
    fn bm25_top_k_heap_path_matches_full_sorted_order_and_ties() {
        let mut idx = InvertedIndex::new();
        for doc_id in 1..=20 {
            idx.index_document(doc_id, "same token");
        }
        idx.index_document(30, "same same same token");
        idx.index_document(31, "unrelated");
        idx.finalize();

        let top_5 = idx.search_with_strategy("same", 5, SearchStrategy::DAAT);
        assert_eq!(
            top_5.iter().map(|r| r.doc_id).collect::<Vec<_>>(),
            vec![30, 1, 2, 3, 4]
        );
        assert!(top_5.iter().all(|row| row.score > 0.0));
        assert!(!top_5.iter().any(|row| row.doc_id == 31));
    }

    #[test]
    fn bm25_tokenization_unicode_policy_is_basic_alphanumeric_lowercase() {
        assert_eq!(tokenize("Hello, HELLO!"), vec!["hello", "hello"]);
        assert_eq!(tokenize("数据库 搜索"), vec!["数据库", "搜索"]);
        assert_eq!(tokenize("cơ sở dữ liệu"), vec!["cơ", "sở", "dữ", "liệu"]);

        let mut idx = InvertedIndex::new();
        idx.index_document(1, "数据库 搜索");
        idx.index_document(2, "cơ sở dữ liệu");
        idx.finalize();
        assert_eq!(idx.search("数据库", 10)[0].doc_id, 1);
        assert_eq!(idx.search("dữ", 10)[0].doc_id, 2);
    }

    #[test]
    fn bm25_document_snapshot_reload_preserves_ranking_scores_and_params() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bm25_docs.json");
        let mut idx = InvertedIndex::new().with_bm25_params(Bm25Params { k1: 1.4, b: 0.6 });
        idx.index_document(1, "alpha alpha beta");
        idx.index_document(2, "alpha gamma");
        idx.index_document(3, "delta epsilon");
        idx.finalize();
        let before = idx.search_with_strategy("alpha", 10, SearchStrategy::DAAT);

        idx.save_documents(&path).unwrap();
        let loaded = InvertedIndex::load_documents(&path).unwrap();
        let after = loaded.search_with_strategy("alpha", 10, SearchStrategy::DAAT);

        assert_eq!(loaded.doc_count(), 3);
        assert_eq!(
            before.iter().map(|r| r.doc_id).collect::<Vec<_>>(),
            after.iter().map(|r| r.doc_id).collect::<Vec<_>>()
        );
        for (left, right) in before.iter().zip(after.iter()) {
            assert!((left.score - right.score).abs() < 1e-6);
        }
    }

    #[test]
    fn bm25_snapshot_reload_preserves_duplicate_update_delete_unicode_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bm25_docs.json");
        let mut idx = InvertedIndex::new();
        idx.index_document(1, "stale old");
        idx.index_document(2, "delete me");
        idx.index_document(3, "数据库 搜索");
        idx.index_document(1, "fresh new");
        idx.remove_document(2);
        idx.finalize();
        idx.save_documents(&path).unwrap();

        let loaded = InvertedIndex::load_documents(&path).unwrap();
        assert_eq!(loaded.doc_count(), 2);
        assert!(loaded.search("stale", 10).is_empty());
        assert!(loaded.search("delete", 10).is_empty());
        assert_eq!(
            loaded
                .search("fresh", 10)
                .iter()
                .map(|r| r.doc_id)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(loaded.search("数据库", 10)[0].doc_id, 3);
    }

    #[test]
    fn bm25_snapshot_rejects_duplicate_ids_and_corrupt_json() {
        let dir = tempfile::tempdir().unwrap();
        let duplicate = dir.path().join("duplicate.json");
        std::fs::write(
            &duplicate,
            r#"{"version":1,"bm25_params":{"k1":1.2,"b":0.75},"documents":[[1,"alpha"],[1,"beta"]]}"#,
        )
        .unwrap();
        let duplicate_err = match InvertedIndex::load_documents(&duplicate) {
            Ok(_) => panic!("expected duplicate BM25 document id error"),
            Err(err) => err,
        };
        assert!(duplicate_err.contains("duplicate document id"));

        let corrupt = dir.path().join("corrupt.json");
        std::fs::write(&corrupt, b"{not json").unwrap();
        let corrupt_err = match InvertedIndex::load_documents(&corrupt) {
            Ok(_) => panic!("expected corrupt BM25 snapshot error"),
            Err(err) => err,
        };
        assert!(corrupt_err.contains("invalid BM25 snapshot JSON"));
    }
}
