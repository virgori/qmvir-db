/*
 * Join Executor — Phase 7b
 *
 * Implements three join strategies with cost-based selection:
 *   - NestedLoopJoin   — O(n·m), always correct, low memory
 *   - HashJoin          — O(n+m), best for large unordered inputs
 *   - SortMergeJoin     — O(n·log n + m·log m), best when inputs are pre-sorted
 *
 * The executor auto-selects the strategy based on input sizes and sort hints.
 */

use ahash::AHashMap;
use std::cmp::Ordering;

// ── Join types ──────────────────────────────────────────────────────────

/// Join kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    LeftOuter,
}

/// Join strategy hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinStrategy {
    NestedLoop,
    HashJoin,
    SortMerge,
    /// Let the executor pick the best strategy.
    Auto,
}

// ── Row representation ──────────────────────────────────────────────────

/// A lightweight row: vector of cells.  Column semantics are positional.
#[derive(Debug, Clone, PartialEq)]
pub enum JoinCell {
    Int(i64),
    Float(f64),
    Text(String),
    Null,
}

impl JoinCell {
    fn sort_key(&self) -> (u8, i64, u64, &str) {
        match self {
            JoinCell::Null => (0, 0, 0, ""),
            JoinCell::Int(v) => (1, *v, 0, ""),
            JoinCell::Float(v) => (2, 0, v.to_bits(), ""),
            JoinCell::Text(v) => (3, 0, 0, v.as_str()),
        }
    }
}

impl Eq for JoinCell {}
impl PartialOrd for JoinCell {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for JoinCell {
    fn cmp(&self, other: &Self) -> Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

impl std::hash::Hash for JoinCell {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            JoinCell::Int(v) => v.hash(state),
            JoinCell::Float(v) => v.to_bits().hash(state),
            JoinCell::Text(v) => v.hash(state),
            JoinCell::Null => {}
        }
    }
}

pub type JoinRow = Vec<JoinCell>;

// ── Join Executor ───────────────────────────────────────────────────────

pub struct JoinExecutor;

/// Cost thresholds for auto-selection.
const HASH_JOIN_THRESHOLD: usize = 64;

impl JoinExecutor {
    /// Execute a join between `left` and `right`.
    ///
    /// * `left_key` / `right_key` — column index for the join predicate.
    /// * `strategy` — `Auto` lets the executor pick.
    /// * `left_sorted` / `right_sorted` — hints about pre-sorted state.
    pub fn execute(
        left: &[JoinRow],
        right: &[JoinRow],
        left_key: usize,
        right_key: usize,
        kind: JoinKind,
        strategy: JoinStrategy,
        left_sorted: bool,
        right_sorted: bool,
    ) -> Vec<JoinRow> {
        let strat = match strategy {
            JoinStrategy::Auto => {
                Self::choose(left.len(), right.len(), left_sorted && right_sorted)
            }
            other => other,
        };
        match strat {
            JoinStrategy::NestedLoop => Self::nested_loop(left, right, left_key, right_key, kind),
            JoinStrategy::HashJoin => Self::hash_join(left, right, left_key, right_key, kind),
            JoinStrategy::SortMerge => Self::sort_merge_join(
                left,
                right,
                left_key,
                right_key,
                kind,
                left_sorted,
                right_sorted,
            ),
            JoinStrategy::Auto => unreachable!(),
        }
    }

    fn choose(left_n: usize, right_n: usize, both_sorted: bool) -> JoinStrategy {
        if both_sorted {
            return JoinStrategy::SortMerge;
        }
        let smaller = left_n.min(right_n);
        if smaller < HASH_JOIN_THRESHOLD {
            JoinStrategy::NestedLoop
        } else {
            JoinStrategy::HashJoin
        }
    }

    // ── Nested Loop ─────────────────────────────────────────────────────

    fn nested_loop(
        left: &[JoinRow],
        right: &[JoinRow],
        lk: usize,
        rk: usize,
        kind: JoinKind,
    ) -> Vec<JoinRow> {
        let right_width = right.first().map(|r| r.len()).unwrap_or(0);
        let mut out = Vec::new();

        for lrow in left {
            let mut matched = false;
            for rrow in right {
                if lrow[lk] == rrow[rk] {
                    let mut combined = lrow.clone();
                    combined.extend_from_slice(rrow);
                    out.push(combined);
                    matched = true;
                }
            }
            if !matched && kind == JoinKind::LeftOuter {
                let mut combined = lrow.clone();
                combined.extend(std::iter::repeat_n(JoinCell::Null, right_width));
                out.push(combined);
            }
        }
        out
    }

    // ── Hash Join ───────────────────────────────────────────────────────

    fn hash_join(
        left: &[JoinRow],
        right: &[JoinRow],
        lk: usize,
        rk: usize,
        kind: JoinKind,
    ) -> Vec<JoinRow> {
        let right_width = right.first().map(|r| r.len()).unwrap_or(0);

        // Build hash table on the smaller side
        let (build, probe, bk, pk, swapped) = if right.len() <= left.len() {
            (right, left, rk, lk, true)
        } else {
            (left, right, lk, rk, false)
        };

        let mut ht: AHashMap<&JoinCell, Vec<usize>> = AHashMap::with_capacity(build.len());
        for (i, row) in build.iter().enumerate() {
            ht.entry(&row[bk]).or_default().push(i);
        }

        let mut out = Vec::new();
        for prow in probe {
            if let Some(indices) = ht.get(&prow[pk]) {
                for &bi in indices {
                    let combined = if swapped {
                        let mut c = prow.clone();
                        c.extend_from_slice(&build[bi]);
                        c
                    } else {
                        let mut c = build[bi].clone();
                        c.extend_from_slice(prow);
                        c
                    };
                    out.push(combined);
                }
            } else if kind == JoinKind::LeftOuter && !swapped {
                // probe is right side, no match => skip (LeftOuter is about left rows)
                // Actually: if !swapped, probe=right, so a miss here means right row
                // has no left match — that's not LeftOuter semantics.
            } else if kind == JoinKind::LeftOuter && swapped {
                // probe=left, no match in build=right => emit left + NULLs
                let mut combined = prow.clone();
                combined.extend(std::iter::repeat_n(JoinCell::Null, right_width));
                out.push(combined);
            }
        }

        // If !swapped and LeftOuter, we need to track unmatched left rows
        if kind == JoinKind::LeftOuter && !swapped {
            // build=left.  Check which left rows had NO match in probe=right.
            let mut left_matched = vec![false; build.len()];
            for prow in probe {
                if let Some(indices) = ht.get(&prow[pk]) {
                    for &bi in indices {
                        left_matched[bi] = true;
                    }
                }
            }
            for (i, row) in build.iter().enumerate() {
                if !left_matched[i] {
                    let mut combined = row.clone();
                    combined.extend(std::iter::repeat_n(JoinCell::Null, right_width));
                    out.push(combined);
                }
            }
        }

        out
    }

    // ── Sort-Merge Join ─────────────────────────────────────────────────

    fn sort_merge_join(
        left: &[JoinRow],
        right: &[JoinRow],
        lk: usize,
        rk: usize,
        kind: JoinKind,
        left_sorted: bool,
        right_sorted: bool,
    ) -> Vec<JoinRow> {
        let right_width = right.first().map(|r| r.len()).unwrap_or(0);
        let mut lsorted: Vec<JoinRow>;
        let mut rsorted: Vec<JoinRow>;

        let left_ref = if left_sorted {
            left
        } else {
            lsorted = left.to_vec();
            lsorted.sort_by(|a, b| a[lk].cmp(&b[lk]));
            &lsorted
        };
        let right_ref = if right_sorted {
            right
        } else {
            rsorted = right.to_vec();
            rsorted.sort_by(|a, b| a[rk].cmp(&b[rk]));
            &rsorted
        };

        let mut out = Vec::new();
        let mut ri = 0;

        for lrow in left_ref {
            let mut matched = false;
            // Advance right pointer past smaller keys
            while ri < right_ref.len() && right_ref[ri][rk] < lrow[lk] {
                ri += 1;
            }
            // Collect all matching right rows (handle duplicates)
            let mut rj = ri;
            while rj < right_ref.len() && right_ref[rj][rk] == lrow[lk] {
                let mut combined = lrow.clone();
                combined.extend_from_slice(&right_ref[rj]);
                out.push(combined);
                matched = true;
                rj += 1;
            }
            if !matched && kind == JoinKind::LeftOuter {
                let mut combined = lrow.clone();
                combined.extend(std::iter::repeat_n(JoinCell::Null, right_width));
                out.push(combined);
            }
        }

        out
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cells: &[JoinCell]) -> JoinRow {
        cells.to_vec()
    }

    #[test]
    fn test_inner_nested_loop() {
        let left = vec![
            row(&[JoinCell::Int(1), JoinCell::Text("a".into())]),
            row(&[JoinCell::Int(2), JoinCell::Text("b".into())]),
            row(&[JoinCell::Int(3), JoinCell::Text("c".into())]),
        ];
        let right = vec![
            row(&[JoinCell::Int(2), JoinCell::Float(10.0)]),
            row(&[JoinCell::Int(3), JoinCell::Float(20.0)]),
        ];
        let result = JoinExecutor::execute(
            &left,
            &right,
            0,
            0,
            JoinKind::Inner,
            JoinStrategy::NestedLoop,
            false,
            false,
        );
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_left_outer_hash() {
        let left = vec![
            row(&[JoinCell::Int(1)]),
            row(&[JoinCell::Int(2)]),
            row(&[JoinCell::Int(3)]),
        ];
        let right = vec![row(&[JoinCell::Int(2), JoinCell::Text("x".into())])];
        let result = JoinExecutor::execute(
            &left,
            &right,
            0,
            0,
            JoinKind::LeftOuter,
            JoinStrategy::HashJoin,
            false,
            false,
        );
        assert_eq!(result.len(), 3); // 1→NULL, 2→x, 3→NULL
    }

    #[test]
    fn test_sort_merge_inner() {
        let left = vec![
            row(&[JoinCell::Int(1)]),
            row(&[JoinCell::Int(2)]),
            row(&[JoinCell::Int(2)]),
            row(&[JoinCell::Int(3)]),
        ];
        let right = vec![
            row(&[JoinCell::Int(2), JoinCell::Text("y".into())]),
            row(&[JoinCell::Int(3), JoinCell::Text("z".into())]),
        ];
        let result = JoinExecutor::execute(
            &left,
            &right,
            0,
            0,
            JoinKind::Inner,
            JoinStrategy::SortMerge,
            true,
            true,
        );
        assert_eq!(result.len(), 3); // 2→y, 2→y, 3→z
    }

    #[test]
    fn test_auto_selects_nested_loop_for_small() {
        let left = vec![row(&[JoinCell::Int(1)])];
        let right = vec![row(&[JoinCell::Int(1)])];
        let result = JoinExecutor::execute(
            &left,
            &right,
            0,
            0,
            JoinKind::Inner,
            JoinStrategy::Auto,
            false,
            false,
        );
        assert_eq!(result.len(), 1);
    }
}
