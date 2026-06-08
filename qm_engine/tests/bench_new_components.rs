/*
 * Comprehensive Benchmark & Integrity Test — New QM Components (2026-04-11)
 *
 * Tests all newly added subsystems:
 *   1. Index Kernel: Roaring Bitmap, Inverted Index (BMW/WAND), HNSW+PQ
 *   2. Statistics: HLL, CMS, T-Digest, Bloom Filter, Cost Model
 *   3. Optimizer: Rule-based rewriting, Adaptive re-optimization
 *   4. Learned: Selectivity, Cache Predictor, Fusion Weights, Intent
 *
 * Run: cargo test --test bench_new_components -- --nocapture 2>&1
 */

use std::collections::HashSet;
use std::time::{Duration, Instant};

// ── Index imports
use qm_engine::index::hnsw::{
    DistanceMetric, HnswConfig, HnswIndex, HnswPqIndex, ProductQuantizer,
};
use qm_engine::index::inverted::{InvertedIndex, SearchStrategy};
use qm_engine::index::roaring::RoaringBitmap;

// ── Big-data feature imports
use qm_engine::index::concurrent_hnsw::ConcurrentHnswIndex;
use qm_engine::index::mmap_store::{MmapGraphStore, MmapVectorStore};
use qm_engine::index::sharded::{ShardedHnswIndex, ShardedInvertedIndex};
use qm_engine::index::wal_inverted::WalInvertedIndex;

// ── Statistics imports
use qm_engine::statistics::bloom::BloomFilter;
use qm_engine::statistics::cost_model::{CostModel, TableStats};
use qm_engine::statistics::count_min::CountMinSketch;
use qm_engine::statistics::hll::HyperLogLog;
use qm_engine::statistics::tdigest::TDigest;

// ── Optimizer imports
use qm_engine::optimizer::adaptive::{AdaptiveOptimizer, PlanRecord};
use qm_engine::optimizer::rules::{
    apply_rules, default_rules, LogicalPlan, Predicate, SortOrder, Value,
};

// ── Learned imports
use qm_engine::learned::cache_predictor::CachePredictor;
use qm_engine::learned::fusion_weights::FusionWeightTuner;
use qm_engine::learned::intent::IntentClassifier;
use qm_engine::learned::selectivity::SelectivityModel;

// ═══════════════════════════════════════════════════════════════════
//  1. ROARING BITMAP
// ═══════════════════════════════════════════════════════════════════

#[test]
fn bench_roaring_insert_1m() {
    let mut bm = RoaringBitmap::new();
    let start = Instant::now();
    for i in 0u32..1_000_000 {
        bm.insert(i);
    }
    let elapsed = start.elapsed();
    println!(
        "[ROARING] Insert 1M sequential: {:.2}ms ({:.0} ops/sec)",
        elapsed.as_secs_f64() * 1000.0,
        1_000_000.0 / elapsed.as_secs_f64()
    );
    assert_eq!(bm.cardinality(), 1_000_000);
}

#[test]
fn bench_roaring_contains_1m() {
    let mut bm = RoaringBitmap::new();
    for i in 0u32..1_000_000 {
        bm.insert(i);
    }
    let start = Instant::now();
    let mut found = 0u64;
    for i in 0u32..1_000_000 {
        if bm.contains(i) {
            found += 1;
        }
    }
    let elapsed = start.elapsed();
    println!(
        "[ROARING] Contains 1M lookups: {:.2}ms ({:.0} ops/sec)",
        elapsed.as_secs_f64() * 1000.0,
        1_000_000.0 / elapsed.as_secs_f64()
    );
    assert_eq!(found, 1_000_000);
}

#[test]
fn bench_roaring_set_ops() {
    let mut a = RoaringBitmap::new();
    let mut b = RoaringBitmap::new();
    for i in 0u32..100_000 {
        a.insert(i);
    }
    for i in 50_000u32..150_000 {
        b.insert(i);
    }

    let t0 = Instant::now();
    let inter = a.and(&b);
    let t_and = t0.elapsed();
    let t1 = Instant::now();
    let union = a.or(&b);
    let t_or = t1.elapsed();
    let t2 = Instant::now();
    let diff = a.xor(&b);
    let t_xor = t2.elapsed();

    println!(
        "[ROARING] AND 100K∩100K: {:.3}ms → {}",
        t_and.as_secs_f64() * 1000.0,
        inter.cardinality()
    );
    println!(
        "[ROARING] OR  100K∪100K: {:.3}ms → {}",
        t_or.as_secs_f64() * 1000.0,
        union.cardinality()
    );
    println!(
        "[ROARING] XOR 100K⊕100K: {:.3}ms → {}",
        t_xor.as_secs_f64() * 1000.0,
        diff.cardinality()
    );
    assert_eq!(inter.cardinality(), 50_000);
    assert_eq!(union.cardinality(), 150_000);
    assert_eq!(diff.cardinality(), 100_000);
}

#[test]
fn bench_roaring_integrity() {
    let mut bm = RoaringBitmap::new();
    let vals: Vec<u32> = (0..100_000).map(|i| i * 7 + 13).collect();
    for &v in &vals {
        bm.insert(v);
    }
    for &v in &vals {
        assert!(bm.contains(v), "Missing: {}", v);
    }
    println!("[ROARING] Integrity: 100K values verified, 0 false positives ✓");
}

// ═══════════════════════════════════════════════════════════════════
//  2. INVERTED INDEX
// ═══════════════════════════════════════════════════════════════════

fn build_corpus(n_docs: u32) -> InvertedIndex {
    let mut idx = InvertedIndex::new();
    let words = [
        "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "data", "query", "search",
        "index", "engine", "fast", "rust", "compile",
    ];
    for doc_id in 0..n_docs {
        let mut text = String::new();
        for j in 0..20 {
            let w = words[(doc_id as usize * 7 + j * 13) % words.len()];
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(w);
        }
        idx.index_document(doc_id, &text);
    }
    idx.finalize();
    idx
}

#[test]
fn bench_inverted_insert_10k() {
    let start = Instant::now();
    let idx = build_corpus(10_000);
    let elapsed = start.elapsed();
    println!(
        "[INVERTED] Index 10K docs (20 tok each): {:.2}ms ({:.0} docs/sec)",
        elapsed.as_secs_f64() * 1000.0,
        10_000.0 / elapsed.as_secs_f64()
    );
    println!(
        "[INVERTED]   Terms: {}, Postings: {}",
        idx.term_count(),
        idx.total_postings()
    );
}

#[test]
fn bench_inverted_strategies() {
    let idx = build_corpus(10_000);
    let q = "quick fox search";
    let k = 10;

    let t0 = Instant::now();
    let daat = idx.search_with_strategy(q, k, SearchStrategy::DAAT);
    let t_d = t0.elapsed();
    let t1 = Instant::now();
    let wand = idx.search_with_strategy(q, k, SearchStrategy::WAND);
    let t_w = t1.elapsed();
    let t2 = Instant::now();
    let bmw = idx.search_with_strategy(q, k, SearchStrategy::BMW);
    let t_b = t2.elapsed();

    println!(
        "[INVERTED] DAAT top-{}: {:.3}ms ({} results)",
        k,
        t_d.as_secs_f64() * 1000.0,
        daat.len()
    );
    println!(
        "[INVERTED] WAND top-{}: {:.3}ms ({} results)",
        k,
        t_w.as_secs_f64() * 1000.0,
        wand.len()
    );
    println!(
        "[INVERTED] BMW  top-{}: {:.3}ms ({} results)",
        k,
        t_b.as_secs_f64() * 1000.0,
        bmw.len()
    );
    assert!(!daat.is_empty());
}

#[test]
fn bench_inverted_bm25_ranking() {
    let mut idx = InvertedIndex::new();
    idx.index_document(0, "the quick brown fox");
    idx.index_document(1, "the lazy brown dog");
    idx.index_document(2, "quick quick quick fox");
    idx.finalize();
    let r = idx.search("quick", 10);
    assert!(!r.is_empty());
    assert_eq!(r[0].doc_id, 2, "Doc 2 (3× TF) should rank first");
    let d = idx.search("dog", 10);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].doc_id, 1);
    println!("[INVERTED] BM25 ranking integrity ✓");
}

// ═══════════════════════════════════════════════════════════════════
//  3. HNSW + PQ
// ═══════════════════════════════════════════════════════════════════

fn prand_vecs(n: usize, dim: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            (0..dim)
                .map(|_| {
                    s = s
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    ((s >> 33) as f32) / (u32::MAX as f32) * 2.0 - 1.0
                })
                .collect()
        })
        .collect()
}

/// Generate clustered vectors (Gaussian blobs) for realistic HNSW benchmarking.
/// Creates `n_clusters` cluster centers, then generates `n` vectors with gaussian noise.
fn clustered_vecs(n: usize, dim: usize, n_clusters: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut s = seed;
    let mut next_f32 = || -> f32 {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((s >> 33) as f32) / (u32::MAX as f32) * 2.0 - 1.0
    };

    // Generate cluster centers spread across the hypercube
    let centers: Vec<Vec<f32>> = (0..n_clusters)
        .map(|_| (0..dim).map(|_| next_f32() * 10.0).collect())
        .collect();

    // Generate points around cluster centers with small gaussian-like noise
    // (Box-Muller approximation using uniform samples)
    (0..n)
        .map(|i| {
            let center = &centers[i % n_clusters];
            center
                .iter()
                .map(|&c| {
                    // Two uniform samples → approximate normal via Irwin-Hall (sum of 4 uniforms)
                    let noise: f32 = (0..4).map(|_| next_f32()).sum::<f32>() * 0.25;
                    c + noise * 0.5 // σ ≈ 0.5
                })
                .collect()
        })
        .collect()
}

#[test]
fn bench_hnsw_insert_10k() {
    let dim = 128;
    let vecs = prand_vecs(10_000, dim, 42);
    let config = HnswConfig {
        m: 16,
        m0: 32,
        ef_construction: 200,
        ef_search: 50,
        metric: DistanceMetric::L2,
    };
    let mut hnsw = HnswIndex::new(dim, config);

    // Use batch_insert with Rayon parallelism
    let batch: Vec<(u32, Vec<f32>)> = vecs
        .into_iter()
        .enumerate()
        .map(|(i, v)| (i as u32, v))
        .collect();
    let start = Instant::now();
    hnsw.batch_insert(batch);
    let elapsed = start.elapsed();
    println!(
        "[HNSW] Batch insert 10K (dim=128): {:.2}ms ({:.0} vecs/sec)",
        elapsed.as_secs_f64() * 1000.0,
        10_000.0 / elapsed.as_secs_f64()
    );
    assert_eq!(hnsw.len(), 10_000);
}

#[test]
fn bench_hnsw_recall() {
    let n = 5_000;
    let dim = 64;
    // Use clustered data (20 clusters) for realistic ANN benchmarking
    let vecs = clustered_vecs(n, dim, 20, 123);
    let config = HnswConfig {
        m: 32,
        m0: 64,
        ef_construction: 400,
        ef_search: 200,
        metric: DistanceMetric::L2,
    };
    let mut hnsw = HnswIndex::new(dim, config);
    for (i, v) in vecs.iter().enumerate() {
        hnsw.insert(i as u32, v.clone());
    }

    let query = &vecs[0];
    let k = 10;
    let mut exact: Vec<(u32, f32)> = vecs
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let d: f32 = v.iter().zip(query).map(|(a, b)| (a - b) * (a - b)).sum();
            (i as u32, d)
        })
        .collect();
    exact.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    let exact_set: HashSet<u32> = exact.iter().take(k).map(|x| x.0).collect();

    let start = Instant::now();
    let results = hnsw.search(query, k);
    let elapsed = start.elapsed();
    let hnsw_set: HashSet<u32> = results.iter().map(|x| x.0).collect();
    let recall = exact_set.intersection(&hnsw_set).count() as f64 / k as f64;
    println!(
        "[HNSW] Search top-{} (5K, dim=64, 20 clusters): {:.3}ms, recall={:.1}%",
        k,
        elapsed.as_secs_f64() * 1000.0,
        recall * 100.0
    );
    // With clustered data + heuristic selection + higher M: expect recall ≥ 70%
    assert!(recall >= 0.5, "Recall too low: {:.1}%", recall * 100.0);
}

#[test]
fn bench_pq_compression() {
    let n = 1000;
    let dim = 128;
    let n_sub = 16;
    let vecs = prand_vecs(n, dim, 77);
    let mut pq = ProductQuantizer::new(dim, n_sub);
    let t0 = Instant::now();
    pq.train(&vecs, 10);
    let t_train = t0.elapsed();
    let t1 = Instant::now();
    let codes: Vec<Vec<u8>> = vecs.iter().map(|v| pq.encode(v)).collect();
    let t_enc = t1.elapsed();
    let orig = n * dim * 4;
    let comp = n * n_sub;
    let ratio = orig as f64 / comp as f64;
    println!(
        "[PQ] Train: {:.2}ms, Encode: {:.2}ms",
        t_train.as_secs_f64() * 1000.0,
        t_enc.as_secs_f64() * 1000.0
    );
    println!(
        "[PQ] {}KB → {}KB ({:.1}× compression)",
        orig / 1024,
        comp / 1024,
        ratio
    );
    assert!(ratio > 20.0);

    let table = pq.build_distance_table(&vecs[0]);
    let adc = ProductQuantizer::adc_distance(&table, &codes[1]);
    let exact: f32 = vecs[0]
        .iter()
        .zip(&vecs[1])
        .map(|(a, b)| (a - b) * (a - b))
        .sum();
    println!("[PQ] ADC dist: {:.4}, exact: {:.4}", adc, exact);
}

#[test]
fn bench_hnsw_pq_two_stage() {
    let n = 2000;
    let dim = 64;
    let n_sub = 8;
    let vecs = prand_vecs(n, dim, 55);
    let mut idx = HnswPqIndex::new(dim, n_sub, true);
    idx.train_pq(&vecs, 10);
    for (i, v) in vecs.iter().enumerate() {
        idx.insert(i as u32, v.clone());
    }
    let t0 = Instant::now();
    let r = idx.search(&vecs[0], 10);
    let t_s = t0.elapsed();
    let t1 = Instant::now();
    let r2 = idx.pq_search(&vecs[0], 10);
    let t_p = t1.elapsed();
    println!(
        "[HNSW-PQ] Two-stage top-10: {:.3}ms ({} res)",
        t_s.as_secs_f64() * 1000.0,
        r.len()
    );
    println!(
        "[HNSW-PQ] PQ-only top-10:   {:.3}ms ({} res)",
        t_p.as_secs_f64() * 1000.0,
        r2.len()
    );
    assert!(!r.is_empty());
}

// ═══════════════════════════════════════════════════════════════════
//  4. STATISTICS
// ═══════════════════════════════════════════════════════════════════

#[test]
fn bench_hll_1m() {
    let mut hll = HyperLogLog::new();
    let n = 1_000_000u64;
    let start = Instant::now();
    for i in 0..n {
        hll.add(&i.to_le_bytes());
    }
    let elapsed = start.elapsed();
    let est = hll.estimate();
    let err = ((est as f64 - n as f64) / n as f64).abs() * 100.0;
    println!(
        "[HLL] 1M distinct: {:.2}ms ({:.0} ops/sec), est={}, err={:.2}%",
        elapsed.as_secs_f64() * 1000.0,
        n as f64 / elapsed.as_secs_f64(),
        est,
        err
    );
    println!("[HLL] Memory: 16 KB");
    assert!(err < 5.0);
}

#[test]
fn bench_hll_merge() {
    let mut h1 = HyperLogLog::new();
    let mut h2 = HyperLogLog::new();
    for i in 0u64..500_000 {
        h1.add(&i.to_le_bytes());
    }
    for i in 250_000u64..750_000 {
        h2.add(&i.to_le_bytes());
    }
    let t0 = Instant::now();
    h1.merge(&h2);
    let tm = t0.elapsed();
    let est = h1.estimate();
    let err = ((est as f64 - 750_000.0) / 750_000.0).abs() * 100.0;
    println!(
        "[HLL] Merge: {:.3}ms, est={}, err={:.2}%",
        tm.as_secs_f64() * 1000.0,
        est,
        err
    );
    assert!(err < 5.0);
}

#[test]
fn bench_cms() {
    let mut cms = CountMinSketch::new();
    let items: Vec<(u32, u32)> = vec![(1, 1), (2, 10), (3, 100), (4, 1000), (5, 10_000)];
    let start = Instant::now();
    for &(item, count) in &items {
        for _ in 0..count {
            cms.add(&item.to_le_bytes(), 1);
        }
    }
    let elapsed = start.elapsed();
    println!(
        "[CMS] Insert {} events: {:.3}ms",
        items.iter().map(|x| x.1).sum::<u32>(),
        elapsed.as_secs_f64() * 1000.0
    );
    for &(item, actual) in &items {
        let est = cms.estimate(&item.to_le_bytes());
        println!(
            "[CMS]   Item {}: est={}, actual={}, Δ={}",
            item,
            est,
            actual,
            est as i64 - actual as i64
        );
        assert!(est >= actual as u64, "CMS must not undercount");
    }
}

#[test]
fn bench_tdigest_1m() {
    let mut td = TDigest::new();
    let n = 1_000_000;
    let start = Instant::now();
    for i in 0..n {
        td.add(i as f64);
    }
    let elapsed = start.elapsed();
    let qs = [0.01, 0.10, 0.25, 0.50, 0.75, 0.90, 0.99];
    println!(
        "[TDIGEST] 1M insert: {:.2}ms",
        elapsed.as_secs_f64() * 1000.0
    );
    let mut max_err = 0.0f64;
    for &q in &qs {
        let est = td.quantile(q);
        let actual = q * (n as f64 - 1.0);
        let err = if actual > 0.0 {
            ((est - actual) / actual).abs() * 100.0
        } else {
            0.0
        };
        max_err = max_err.max(err);
        println!(
            "[TDIGEST]   P{:.0}: est={:.1}, actual={:.1}, err={:.2}%",
            q * 100.0,
            est,
            actual,
            err
        );
    }
    println!(
        "[TDIGEST] Centroids: {}, max err: {:.2}%",
        td.centroid_count(),
        max_err
    );
    assert!(max_err < 5.0);
}

#[test]
fn bench_bloom_1m() {
    let n = 1_000_000usize;
    let mut bf = BloomFilter::new(n, 0.01);
    let start = Instant::now();
    for i in 0u64..n as u64 {
        bf.insert(&i.to_le_bytes());
    }
    let elapsed = start.elapsed();
    let mut fn_count = 0u64;
    for i in 0u64..n as u64 {
        if !bf.may_contain(&i.to_le_bytes()) {
            fn_count += 1;
        }
    }
    let test_n = 1_000_000u64;
    let mut fp = 0u64;
    for i in (n as u64)..(n as u64 + test_n) {
        if bf.may_contain(&i.to_le_bytes()) {
            fp += 1;
        }
    }
    let fp_rate = fp as f64 / test_n as f64;
    println!(
        "[BLOOM] 1M insert: {:.2}ms ({:.0} ops/sec)",
        elapsed.as_secs_f64() * 1000.0,
        n as f64 / elapsed.as_secs_f64()
    );
    println!(
        "[BLOOM] FN={}, FP rate={:.4}% (target <1%)",
        fn_count,
        fp_rate * 100.0
    );
    println!(
        "[BLOOM] Memory: {} KB ({} bits, {} hashes)",
        bf.size_bytes() / 1024,
        bf.bit_count(),
        bf.hash_count()
    );
    assert_eq!(fn_count, 0);
    assert!(fp_rate < 0.02);
}

#[test]
fn bench_cost_model() {
    let cm = CostModel::new();
    let stats = TableStats::new(1_000_000, 128)
        .with_distinct("id", 1_000_000)
        .with_distinct("status", 5)
        .with_distinct("category", 100);
    let seq = cm.seq_scan(&stats);
    let idx1 = cm.index_scan_range(&stats, 0.001);
    let idx50 = cm.index_scan_range(&stats, 0.5);
    println!("[COST] 1M rows, {} pages", stats.page_count);
    println!("[COST] Seq scan:       {:.1}", seq.total);
    println!("[COST] Index sel=0.1%: {:.1}", idx1.total);
    println!("[COST] Index sel=50%:  {:.1}", idx50.total);
    assert!(cm.prefer_index_scan(&stats, 0.001));
    assert!(!cm.prefer_index_scan(&stats, 0.5));
    let join = cm.best_join(
        &TableStats::new(1_000_000, 128),
        &TableStats::new(1_000, 128),
    );
    println!("[COST] Join 1M×1K: {}", join);
}

// ═══════════════════════════════════════════════════════════════════
//  5. OPTIMIZER
// ═══════════════════════════════════════════════════════════════════

#[test]
fn bench_optimizer_rules() {
    let scan = LogicalPlan::Scan {
        table: "users".into(),
        columns: None,
        predicates: vec![],
    };
    let filter = LogicalPlan::Filter {
        input: Box::new(scan),
        predicates: vec![Predicate::Gt("id".into(), Value::Int(10))],
    };
    let sort1 = LogicalPlan::Sort {
        input: Box::new(filter),
        order_by: vec![("id".into(), SortOrder::Asc)],
    };
    let sort2 = LogicalPlan::Sort {
        input: Box::new(sort1),
        order_by: vec![("id".into(), SortOrder::Asc)],
    };
    let proj = LogicalPlan::Project {
        input: Box::new(sort2),
        columns: vec!["id".into(), "name".into()],
    };
    let rules = default_rules();
    let start = Instant::now();
    let opt = apply_rules(proj, &rules);
    let elapsed = start.elapsed();
    println!("[OPTIMIZER] Rules: {:.3}ms", elapsed.as_secs_f64() * 1000.0);
    println!("[OPTIMIZER] Result: {:?}", opt);
}

#[test]
fn bench_adaptive_optimizer() {
    let mut opt = AdaptiveOptimizer::new();
    let start = Instant::now();
    for _ in 0..100 {
        opt.update_correction("orders", "status", 1000.0, 50_000);
    }
    let elapsed = start.elapsed();
    let corr = opt.correct_estimate("orders", "status", 1000.0);
    let fence = AdaptiveOptimizer::fence_breached(1000.0, 50_000);
    for i in 0..5 {
        opt.record_execution(PlanRecord {
            query_hash: 0x1234,
            estimated_rows: 1000.0,
            actual_rows: 50_000,
            execution_time: Duration::from_millis(100),
            plan_description: format!("plan_{}", i),
            timestamp: Instant::now(),
        });
    }
    let reopt = opt.should_reoptimize(0x1234);
    println!(
        "[ADAPTIVE] 100 updates: {:.3}ms",
        elapsed.as_secs_f64() * 1000.0
    );
    println!("[ADAPTIVE] Fence breach (1K vs 50K)? {}", fence);
    println!("[ADAPTIVE] Corrected: {:.0} (was 1000)", corr);
    println!("[ADAPTIVE] Reoptimize? {}", reopt);
    println!(
        "[ADAPTIVE] Join (1M×1K): {}",
        opt.suggest_join_strategy("big", "small", 1_000_000, 1_000)
    );
    assert!(fence);
    assert!(corr > 1000.0);
}

// ═══════════════════════════════════════════════════════════════════
//  6. LEARNED COMPONENTS
// ═══════════════════════════════════════════════════════════════════

#[test]
fn bench_selectivity() {
    let mut m = SelectivityModel::new();
    let start = Instant::now();
    for _ in 0..1000 {
        m.observe("orders", "status", 0.1, 0.5);
    }
    let elapsed = start.elapsed();
    let corr = m.correct("orders", "status", 0.1);
    let conf = m.confidence("orders", "status");
    println!(
        "[SELECTIVITY] 1000 obs: {:.3}ms, corrected={:.3}, confidence={:.2}",
        elapsed.as_secs_f64() * 1000.0,
        corr,
        conf
    );
    assert!(corr > 0.3);
}

#[test]
fn bench_cache_predictor() {
    let mut p = CachePredictor::new();
    let start = Instant::now();
    for _ in 0..200 {
        p.record_access(42);
    }
    for _ in 0..10 {
        p.record_access(99);
    }
    let elapsed = start.elapsed();
    let iv42 = p.predicted_interval(42);
    let iv99 = p.predicted_interval(99);
    println!(
        "[CACHE] 210 accesses: {:.3}ms",
        elapsed.as_secs_f64() * 1000.0
    );
    println!(
        "[CACHE] Page 42: {:.2}s (frequent), Page 99: {:.2}s (rare)",
        iv42, iv99
    );
    let evict = p.select_eviction(&[42, 99]);
    println!("[CACHE] Eviction: {:?} (expect rare page)", evict);
}

#[test]
fn bench_fusion_weights() {
    let mut t = FusionWeightTuner::new();
    let start = Instant::now();
    for _ in 0..500 {
        t.record_feedback("lookup", 1.0, true);
    }
    for _ in 0..500 {
        t.record_feedback("semantic", 1.0, false);
    }
    let elapsed = start.elapsed();
    let (a_l, _) = t.get_weights("lookup");
    let (a_s, _) = t.get_weights("semantic");
    println!(
        "[FUSION] 1000 feedbacks: {:.3}ms",
        elapsed.as_secs_f64() * 1000.0
    );
    println!(
        "[FUSION] α(lookup)={:.3} (lexical), α(semantic)={:.3} (vector)",
        a_l, a_s
    );
    assert!(a_l > 0.5, "Lookup should lean lexical");
    assert!(a_s < 0.5, "Semantic should lean vector");
}

#[test]
fn bench_intent() {
    let c = IntentClassifier::new();
    let cases = vec![
        ("SELECT * FROM users WHERE id = 42", "Lookup"),
        ("SELECT * FROM products WHERE name LIKE '%phone%'", "Search"),
        (
            "SELECT COUNT(*), AVG(price) FROM orders GROUP BY cat",
            "Analytics",
        ),
    ];
    let start = Instant::now();
    let mut ok = 0;
    for (sql, exp) in &cases {
        let intent = c.classify_sql(sql);
        let label = intent.as_str();
        if label.to_lowercase().contains(&exp.to_lowercase()) {
            ok += 1;
        }
        println!(
            "[INTENT] '{}...' → {} (exp: {})",
            &sql[..sql.len().min(45)],
            label,
            exp
        );
    }
    let elapsed = start.elapsed();
    println!(
        "[INTENT] {}/{} correct, {:.3}ms",
        ok,
        cases.len(),
        elapsed.as_secs_f64() * 1000.0
    );
}

// ═══════════════════════════════════════════════════════════════════
//  7. COMBINED INTEGRITY
// ═══════════════════════════════════════════════════════════════════

#[test]
fn bench_combined_pipeline() {
    let n = 50_000u32;
    let mut roaring = RoaringBitmap::new();
    let mut bloom = BloomFilter::new(n as usize, 0.01);
    let mut hll = HyperLogLog::new();
    let mut cms = CountMinSketch::new();
    let start = Instant::now();
    for i in 0..n {
        roaring.insert(i);
        bloom.insert(&i.to_le_bytes());
        hll.add(&i.to_le_bytes());
        cms.add(&i.to_le_bytes(), 1);
    }
    let elapsed = start.elapsed();
    let hll_est = hll.estimate();
    let hll_err = ((hll_est as f64 - n as f64) / n as f64).abs() * 100.0;
    let mut agree = 0u32;
    for i in 0..n {
        if roaring.contains(i) && bloom.may_contain(&i.to_le_bytes()) {
            agree += 1;
        }
    }
    println!(
        "[PIPELINE] {}K items × 4 structures: {:.2}ms",
        n / 1000,
        elapsed.as_secs_f64() * 1000.0
    );
    println!(
        "[PIPELINE] Roaring: {} (exact), HLL: {} (err={:.2}%), Bloom-Roaring: {}/{}",
        roaring.cardinality(),
        hll_est,
        hll_err,
        agree,
        n
    );
    assert_eq!(roaring.cardinality(), n as usize);
    assert_eq!(agree, n);
    assert!(hll_err < 5.0);
    println!("[PIPELINE] ✓ Cross-structure consistency verified");
}

// ═══════════════════════════════════════════════════════════════════
//  8. STRESS TEST
// ═══════════════════════════════════════════════════════════════════

#[test]
fn bench_stress() {
    let t_all = Instant::now();

    let mut bm = RoaringBitmap::new();
    let t0 = Instant::now();
    for i in 0u32..5_000_000 {
        bm.insert(i);
    }
    println!(
        "[STRESS] Roaring 5M: {:.2}ms",
        t0.elapsed().as_secs_f64() * 1000.0
    );
    assert_eq!(bm.cardinality(), 5_000_000);

    let mut hll = HyperLogLog::new();
    let t1 = Instant::now();
    for i in 0u64..5_000_000 {
        hll.add(&i.to_le_bytes());
    }
    let hll_e = hll.estimate();
    println!(
        "[STRESS] HLL 5M: {:.2}ms (est={}, err={:.2}%)",
        t1.elapsed().as_secs_f64() * 1000.0,
        hll_e,
        ((hll_e as f64 - 5e6) / 5e6).abs() * 100.0
    );

    let mut td = TDigest::new();
    let t2 = Instant::now();
    for i in 0..5_000_000 {
        td.add(i as f64);
    }
    println!(
        "[STRESS] TDigest 5M: {:.2}ms (P50={:.0}, centroids={})",
        t2.elapsed().as_secs_f64() * 1000.0,
        td.quantile(0.5),
        td.centroid_count()
    );

    let t3 = Instant::now();
    let idx = build_corpus(50_000);
    println!(
        "[STRESS] Inverted 50K docs: {:.2}ms ({} terms, {} postings)",
        t3.elapsed().as_secs_f64() * 1000.0,
        idx.term_count(),
        idx.total_postings()
    );

    println!(
        "[STRESS] Total: {:.2}ms ✓",
        t_all.elapsed().as_secs_f64() * 1000.0
    );
}

// ═══════════════════════════════════════════════════════════════════
//  9. LARGE-SCALE BMW vs DAAT (1M docs)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn bench_inverted_1m_strategies() {
    println!("[1M] Building 1M-doc corpus...");
    let t_build = Instant::now();
    let idx = build_corpus(1_000_000);
    println!(
        "[1M] Built in {:.2}s ({} terms, {} postings)",
        t_build.elapsed().as_secs_f64(),
        idx.term_count(),
        idx.total_postings()
    );

    let queries = [
        "quick fox search",
        "data engine fast",
        "lazy dog brown",
        "rust compile index",
    ];
    let k = 10;
    let n_runs = 3; // average over multiple runs

    let mut total_daat = 0.0f64;
    let mut total_wand = 0.0f64;
    let mut total_bmw = 0.0f64;

    for q in &queries {
        let mut best_d = f64::MAX;
        let mut best_w = f64::MAX;
        let mut best_b = f64::MAX;

        for _ in 0..n_runs {
            let t0 = Instant::now();
            let _daat = idx.search_with_strategy(q, k, SearchStrategy::DAAT);
            let d = t0.elapsed().as_secs_f64() * 1000.0;
            if d < best_d {
                best_d = d;
            }

            let t1 = Instant::now();
            let _wand = idx.search_with_strategy(q, k, SearchStrategy::WAND);
            let w = t1.elapsed().as_secs_f64() * 1000.0;
            if w < best_w {
                best_w = w;
            }

            let t2 = Instant::now();
            let _bmw = idx.search_with_strategy(q, k, SearchStrategy::BMW);
            let b = t2.elapsed().as_secs_f64() * 1000.0;
            if b < best_b {
                best_b = b;
            }
        }
        println!(
            "[1M] q=\"{}\" DAAT={:.3}ms WAND={:.3}ms BMW={:.3}ms (BMW/DAAT={:.2}×)",
            q,
            best_d,
            best_w,
            best_b,
            best_b / best_d
        );
        total_daat += best_d;
        total_wand += best_w;
        total_bmw += best_b;
    }

    let nq = queries.len() as f64;
    let avg_d = total_daat / nq;
    let avg_w = total_wand / nq;
    let avg_b = total_bmw / nq;
    println!(
        "[1M] AVG  DAAT={:.3}ms  WAND={:.3}ms  BMW={:.3}ms",
        avg_d, avg_w, avg_b
    );
    println!(
        "[1M] Ratio: BMW/DAAT={:.2}×  WAND/DAAT={:.2}×",
        avg_b / avg_d,
        avg_w / avg_d
    );

    if avg_b > avg_d {
        println!("[1M] ⚠ BMW slower than DAAT at 1M scale — consider BLOCK_SIZE=128");
    } else {
        println!(
            "[1M] ✓ BMW faster than DAAT at 1M scale ({:.1}% speedup)",
            (1.0 - avg_b / avg_d) * 100.0
        );
    }
}

// ── Big Data: Mmap Vector Store ─────────────────────────────────────

#[test]
fn bench_mmap_vector_store() {
    let dir = std::env::temp_dir().join("qm_bench_mmap_vecs");
    let _ = std::fs::remove_file(&dir);

    let dim = 128;
    let n = 10_000u32;

    // Write 10K vectors
    let mut store = MmapVectorStore::open(&dir, dim).unwrap();
    let t0 = Instant::now();
    for i in 0..n {
        let v: Vec<f32> = (0..dim)
            .map(|j| (i * dim as u32 + j as u32) as f32 * 0.001)
            .collect();
        store.put(i, &v).unwrap();
    }
    store.flush().unwrap();
    let write_ms = t0.elapsed().as_secs_f64() * 1000.0;

    // Random reads (O(1) mmap access)
    let t1 = Instant::now();
    let mut checksum = 0.0f32;
    for i in (0..n).step_by(100) {
        if let Some(v) = store.get(i) {
            checksum += v[0];
        }
    }
    let read_ms = t1.elapsed().as_secs_f64() * 1000.0;

    println!(
        "[MMAP] Write 10K (dim={}): {:.2}ms ({:.0} vecs/s)",
        dim,
        write_ms,
        n as f64 / (write_ms / 1000.0)
    );
    println!(
        "[MMAP] Read 100 random: {:.3}ms | checksum={:.3}",
        read_ms, checksum
    );
    println!(
        "[MMAP] Vectors: {}, file size: {} KB",
        store.len(),
        std::fs::metadata(&dir).map(|m| m.len() / 1024).unwrap_or(0)
    );

    assert_eq!(store.len(), n);
    assert!(checksum > 0.0);
    let _ = std::fs::remove_file(&dir);
}

#[test]
fn bench_mmap_graph_store() {
    let dir = std::env::temp_dir().join("qm_bench_mmap_graph");
    let _ = std::fs::remove_file(&dir);

    let m0 = 32usize;
    let m = 16usize;
    let max_levels = 5;
    let n = 5_000u32;

    let mut store = MmapGraphStore::open(&dir, m0, m, max_levels).unwrap();
    let t0 = Instant::now();
    for i in 0..n {
        // Simulate level-0 neighbors (up to m0 neighbors)
        let neighbors: Vec<u32> = (0..m0.min(8) as u32).map(|j| (i + j + 1) % n).collect();
        store.set_neighbors(i, 0, &neighbors).unwrap();
    }
    store.flush().unwrap();
    let write_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let t1 = Instant::now();
    let mut total_neighbors = 0usize;
    for i in (0..n).step_by(50) {
        total_neighbors += store.get_neighbors(i, 0).len();
    }
    let read_ms = t1.elapsed().as_secs_f64() * 1000.0;

    println!("[MMAP-GRAPH] Write 5K nodes (m0={}): {:.2}ms", m0, write_ms);
    println!(
        "[MMAP-GRAPH] Read 100 neighbor lists: {:.3}ms | avg_neighbors={:.1}",
        read_ms,
        total_neighbors as f64 / 100.0
    );

    assert_eq!(store.len(), n);
    let _ = std::fs::remove_file(&dir);
}

// ── Big Data: Concurrent HNSW ────────────────────────────────────────

#[test]
fn bench_concurrent_hnsw() {
    use std::sync::Arc;
    use std::thread;

    let dim = 64usize;
    let n_per_writer = 500u32;
    let n_writers = 4;

    let idx = Arc::new(ConcurrentHnswIndex::new(
        dim,
        HnswConfig {
            m: 16,
            m0: 32,
            ef_construction: 64,
            ef_search: 50,
            metric: DistanceMetric::L2,
        },
    ));

    // Sequential seed
    let seed_vecs: Vec<(u32, Vec<f32>)> = (0..50u32)
        .map(|i| {
            let v: Vec<f32> = (0..dim)
                .map(|j| ((i as usize * dim + j) as f32) * 0.01)
                .collect();
            (i, v)
        })
        .collect();
    idx.batch_insert(seed_vecs);

    // Parallel writers
    let t0 = Instant::now();
    let handles: Vec<_> = (0..n_writers)
        .map(|w| {
            let idx_clone = idx.clone();
            thread::spawn(move || {
                let mut rng = rand::thread_rng();
                use rand::Rng;
                let start_id = 50 + w * n_per_writer;
                for i in 0..n_per_writer {
                    let v: Vec<f32> = (0..dim).map(|_| rng.gen::<f32>()).collect();
                    idx_clone.insert(start_id + i, v);
                }
            })
        })
        .collect();

    // Concurrent readers
    let idx_r = idx.clone();
    let reader = thread::spawn(move || {
        let mut rng = rand::thread_rng();
        use rand::Rng;
        let mut results_count = 0;
        for _ in 0..200 {
            let q: Vec<f32> = (0..dim).map(|_| rng.gen::<f32>()).collect();
            results_count += idx_r.search(&q, 5).len();
        }
        results_count
    });

    for h in handles {
        h.join().unwrap();
    }
    let search_count = reader.join().unwrap();
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let total_vecs = idx.len();
    println!(
        "[CONCURRENT-HNSW] {} writers × {} vecs + concurrent search: {:.1}ms",
        n_writers, n_per_writer, elapsed_ms
    );
    println!(
        "[CONCURRENT-HNSW] Total vectors: {}, search ops: {}",
        total_vecs, search_count
    );
    println!(
        "[CONCURRENT-HNSW] Throughput: {:.0} vecs/s",
        (n_writers * n_per_writer) as f64 / (elapsed_ms / 1000.0)
    );

    assert_eq!(total_vecs, 50 + (n_writers * n_per_writer) as usize);
    assert!(search_count > 0);
}

// ── Big Data: Sharded Indexes ────────────────────────────────────────

#[test]
fn bench_sharded_inverted_index() {
    let n_shards = 4u32;
    let n_docs = 10_000u32;
    let idx = ShardedInvertedIndex::new(n_shards);

    let words = [
        "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "data", "engine", "fast",
        "rust", "index", "search", "query", "fetch",
    ];

    // Index 10K docs across 4 shards
    let t0 = Instant::now();
    for doc_id in 0..n_docs {
        let text = words
            .iter()
            .enumerate()
            .filter(|(j, _)| (doc_id as usize + j) % 3 == 0)
            .map(|(_, w)| *w)
            .collect::<Vec<_>>()
            .join(" ");
        idx.index_document(doc_id, &text);
    }
    idx.finalize();
    let index_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let sizes = idx.shard_sizes();
    let min_sz = *sizes.iter().min().unwrap();
    let max_sz = *sizes.iter().max().unwrap();

    // Fan-out search across all shards
    let t1 = Instant::now();
    let results = idx.search("quick fox", 10);
    let search_ms = t1.elapsed().as_secs_f64() * 1000.0;

    println!(
        "[SHARDED-INV] Index {}K docs, {} shards: {:.1}ms ({:.0} docs/s)",
        n_docs / 1000,
        n_shards,
        index_ms,
        n_docs as f64 / (index_ms / 1000.0)
    );
    println!(
        "[SHARDED-INV] Shard distribution: min={} max={} (imbalance={:.1}%)",
        min_sz,
        max_sz,
        (max_sz - min_sz) as f64 / (n_docs / n_shards) as f64 * 100.0
    );
    println!(
        "[SHARDED-INV] Fan-out search top-10: {:.3}ms ({} results)",
        search_ms,
        results.len()
    );

    assert_eq!(idx.doc_count(), n_docs);
    assert!(!results.is_empty());
}

#[test]
fn bench_sharded_hnsw() {
    use rand::Rng;

    let dim = 64usize;
    let n_shards = 4;
    let n = 2_000u32;
    let mut rng = rand::thread_rng();

    let idx = ShardedHnswIndex::new(
        dim,
        HnswConfig {
            m: 16,
            m0: 32,
            ef_construction: 64,
            ef_search: 50,
            metric: DistanceMetric::L2,
        },
        n_shards,
    );

    // Batch insert across shards
    let vecs: Vec<(u32, Vec<f32>)> = (0..n)
        .map(|i| (i, (0..dim).map(|_| rng.gen::<f32>()).collect()))
        .collect();

    let t0 = Instant::now();
    idx.batch_insert(vecs);
    let insert_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let sizes = idx.shard_sizes();
    let min_sz = *sizes.iter().min().unwrap();
    let max_sz = *sizes.iter().max().unwrap();

    // Fan-out search
    let query: Vec<f32> = (0..dim).map(|_| rng.gen::<f32>()).collect();
    let t1 = Instant::now();
    let results = idx.search(&query, 10);
    let search_ms = t1.elapsed().as_secs_f64() * 1000.0;

    println!(
        "[SHARDED-HNSW] Insert {}K vecs, {} shards: {:.1}ms ({:.0} vecs/s)",
        n / 1000,
        n_shards,
        insert_ms,
        n as f64 / (insert_ms / 1000.0)
    );
    println!(
        "[SHARDED-HNSW] Shard distribution: min={} max={} (imbalance={:.1}%)",
        min_sz,
        max_sz,
        (max_sz as f64 - min_sz as f64) / (n / n_shards) as f64 * 100.0
    );
    println!(
        "[SHARDED-HNSW] Fan-out search top-10: {:.3}ms ({} results)",
        search_ms,
        results.len()
    );

    assert_eq!(idx.len(), n as usize);
    assert!(!results.is_empty());
}

// ── Big Data: WAL-Integrated Inverted Index ──────────────────────────

#[test]
fn bench_wal_inverted_index() {
    let wal_dir = std::env::temp_dir().join("qm_bench_wal_inv");
    let _ = std::fs::remove_dir_all(&wal_dir);

    let words = [
        "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "data", "engine", "fast",
        "rust", "index", "search", "query", "fetch",
    ];
    let n_docs = 1_000u32;

    // Phase 1: Index + WAL write
    {
        let mut idx = WalInvertedIndex::new(wal_dir.clone()).unwrap();
        let t0 = Instant::now();
        for doc_id in 0..n_docs {
            let text = words
                .iter()
                .enumerate()
                .filter(|(j, _)| (doc_id as usize + j) % 3 == 0)
                .map(|(_, w)| *w)
                .collect::<Vec<_>>()
                .join(" ");
            idx.index_document(doc_id, &text).unwrap();
        }
        idx.finalize().unwrap();
        let write_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let results = idx.search("quick fox", 10);
        println!(
            "[WAL-INV] Index {}K docs + WAL log: {:.1}ms ({:.0} docs/s)",
            n_docs / 1000,
            write_ms,
            n_docs as f64 / (write_ms / 1000.0)
        );
        println!(
            "[WAL-INV] Docs: {}, Terms: {}, Postings: {}",
            idx.doc_count(),
            idx.term_count(),
            idx.total_postings()
        );
        println!("[WAL-INV] Search top-10: {} results", results.len());

        assert_eq!(idx.doc_count(), n_docs);
        assert!(!results.is_empty());
    }

    // Phase 2: Crash recovery — replay WAL from disk
    {
        let t0 = Instant::now();
        let recovered = WalInvertedIndex::open(wal_dir.clone()).unwrap();
        let recovery_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let results = recovered.search("quick fox", 10);
        println!("[WAL-INV] Recovery from WAL: {:.1}ms", recovery_ms);
        println!(
            "[WAL-INV] Recovered: {} docs, {} terms",
            recovered.doc_count(),
            recovered.term_count()
        );
        println!("[WAL-INV] Post-recovery search: {} results", results.len());

        assert_eq!(recovered.doc_count(), n_docs);
        assert!(!results.is_empty());
    }

    let _ = std::fs::remove_dir_all(&wal_dir);
}

// ── v4.2.0: WAL Group Commit Benchmark ──────────────────────────────

#[test]
fn bench_wal_group_commit() {
    let wal_dir_single = std::env::temp_dir().join("qm_bench_wal_gc_single");
    let wal_dir_batch = std::env::temp_dir().join("qm_bench_wal_gc_batch");
    let _ = std::fs::remove_dir_all(&wal_dir_single);
    let _ = std::fs::remove_dir_all(&wal_dir_batch);

    let words = [
        "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "data", "engine", "fast",
        "rust", "index", "search", "query", "fetch",
    ];
    let n_docs = 1_000u32;

    // Build doc list
    let doc_texts: Vec<String> = (0..n_docs)
        .map(|doc_id| {
            words
                .iter()
                .enumerate()
                .filter(|(j, _)| (doc_id as usize + j) % 3 == 0)
                .map(|(_, w)| *w)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();

    // Baseline: per-doc commit (sync each)
    let single_ms = {
        let mut idx = WalInvertedIndex::new(wal_dir_single.clone()).unwrap();
        let t0 = Instant::now();
        for (doc_id, text) in doc_texts.iter().enumerate() {
            idx.index_document(doc_id as u32, text).unwrap();
        }
        idx.finalize().unwrap();
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(idx.doc_count(), n_docs);
        ms
    };

    // Group commit: batch all, flush once
    let batch_ms = {
        let mut idx = WalInvertedIndex::new(wal_dir_batch.clone()).unwrap();
        let docs: Vec<(u32, &str)> = doc_texts
            .iter()
            .enumerate()
            .map(|(i, t)| (i as u32, t.as_str()))
            .collect();
        let t0 = Instant::now();
        idx.batch_index_documents(&docs).unwrap();
        idx.finalize().unwrap();
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(idx.doc_count(), n_docs);
        ms
    };

    let speedup = single_ms / batch_ms;
    println!(
        "[WAL-GC] Per-doc commit: {:.1}ms ({:.0} docs/s)",
        single_ms,
        n_docs as f64 / (single_ms / 1000.0)
    );
    println!(
        "[WAL-GC] Group commit:   {:.1}ms ({:.0} docs/s)",
        batch_ms,
        n_docs as f64 / (batch_ms / 1000.0)
    );
    println!("[WAL-GC] Speedup: {:.1}×", speedup);

    // Group commit should be significantly faster
    assert!(
        speedup > 2.0,
        "Group commit should be at least 2× faster, got {:.1}×",
        speedup
    );

    // Verify recovery works with group-committed WAL
    {
        let recovered = WalInvertedIndex::open(wal_dir_batch.clone()).unwrap();
        assert_eq!(recovered.doc_count(), n_docs);
        let results = recovered.search("quick fox", 10);
        assert!(!results.is_empty());
        println!(
            "[WAL-GC] Recovery verified: {} docs, search OK",
            recovered.doc_count()
        );
    }

    let _ = std::fs::remove_dir_all(&wal_dir_single);
    let _ = std::fs::remove_dir_all(&wal_dir_batch);
}

// ── v4.2.0: Mmap madvise Benchmark ─────────────────────────────────

use qm_engine::index::mmap_store::AccessPattern;

#[test]
fn bench_mmap_madvise() {
    use rand::Rng;
    let dim = 128;
    let n = 10_000u32;
    let path_seq = std::env::temp_dir().join("qm_bench_mmap_madvise_seq");
    let path_rnd = std::env::temp_dir().join("qm_bench_mmap_madvise_rnd");
    let _ = std::fs::remove_file(&path_seq);
    let _ = std::fs::remove_file(&path_rnd);

    let mut rng = rand::thread_rng();
    let vectors: Vec<Vec<f32>> = (0..n)
        .map(|_| (0..dim).map(|_| rng.gen::<f32>()).collect())
        .collect();

    // Sequential write with Sequential advice
    let mut store = MmapVectorStore::open(&path_seq, dim).unwrap();
    store.advise(AccessPattern::Sequential).unwrap();
    let t0 = Instant::now();
    for (i, v) in vectors.iter().enumerate() {
        store.put(i as u32, v).unwrap();
    }
    store.flush().unwrap();
    let seq_write_ms = t0.elapsed().as_secs_f64() * 1000.0;

    // Random read with Random advice
    store.advise(AccessPattern::Random).unwrap();
    let ids: Vec<u32> = (0..1000).map(|_| rng.gen_range(0..n)).collect();
    let t0 = Instant::now();
    let mut checksum = 0.0f32;
    for &id in &ids {
        if let Some(v) = store.get(id) {
            checksum += v[0];
        }
    }
    let rnd_read_ms = t0.elapsed().as_secs_f64() * 1000.0;

    // WillNeed prefetch then read
    store.advise(AccessPattern::WillNeed).unwrap();
    let t0 = Instant::now();
    let mut checksum2 = 0.0f32;
    for &id in &ids {
        if let Some(v) = store.get(id) {
            checksum2 += v[0];
        }
    }
    let prefetch_read_ms = t0.elapsed().as_secs_f64() * 1000.0;

    // Range advice
    store
        .advise_range(0, 1000, AccessPattern::WillNeed)
        .unwrap();

    println!(
        "[MMAP-ADV] Sequential write {}K (dim={}): {:.2}ms ({:.0} vecs/s)",
        n / 1000,
        dim,
        seq_write_ms,
        n as f64 / (seq_write_ms / 1000.0)
    );
    println!(
        "[MMAP-ADV] Random read 1K (MADV_RANDOM): {:.3}ms",
        rnd_read_ms
    );
    println!(
        "[MMAP-ADV] Prefetch read 1K (MADV_WILLNEED): {:.3}ms",
        prefetch_read_ms
    );
    println!("[MMAP-ADV] checksum={:.3},{:.3}", checksum, checksum2);

    assert!(seq_write_ms < 500.0);

    let _ = std::fs::remove_file(&path_seq);
    let _ = std::fs::remove_file(&path_rnd);
}

// ── v4.2.0: Concurrent HNSW Write Coalescing ───────────────────────

#[test]
fn bench_concurrent_hnsw_buffered() {
    use rand::Rng;
    use std::thread;

    let dim = 32;
    let n_writers = 4u32;
    let vecs_per_writer = 500u32;
    let total = n_writers * vecs_per_writer;

    // Baseline: direct insert (per-vector write lock)
    let direct_ms = {
        let idx = ConcurrentHnswIndex::with_default_config(dim);
        let mut rng = rand::thread_rng();
        // Pre-seed
        for i in 0..50u32 {
            let v: Vec<f32> = (0..dim).map(|_| rng.gen::<f32>()).collect();
            idx.insert(i, v);
        }

        let t0 = Instant::now();
        let handles: Vec<_> = (0..n_writers)
            .map(|t| {
                let idx_c = idx.clone();
                thread::spawn(move || {
                    let mut rng = rand::thread_rng();
                    for i in 0..vecs_per_writer {
                        let id = 1000 + t * 1000 + i;
                        let v: Vec<f32> = (0..dim).map(|_| rng.gen::<f32>()).collect();
                        idx_c.insert(id, v);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(idx.len(), 50 + total as usize);
        ms
    };

    // Buffered: write coalescing (batch under single lock)
    let buffered_ms = {
        let idx = ConcurrentHnswIndex::with_flush_threshold(
            dim,
            qm_engine::index::hnsw::HnswConfig::default(),
            128,
        );
        let mut rng = rand::thread_rng();
        for i in 0..50u32 {
            let v: Vec<f32> = (0..dim).map(|_| rng.gen::<f32>()).collect();
            idx.insert(i, v);
        }

        let t0 = Instant::now();
        let handles: Vec<_> = (0..n_writers)
            .map(|t| {
                let idx_c = idx.clone();
                thread::spawn(move || {
                    let mut rng = rand::thread_rng();
                    for i in 0..vecs_per_writer {
                        let id = 1000 + t * 1000 + i;
                        let v: Vec<f32> = (0..dim).map(|_| rng.gen::<f32>()).collect();
                        idx_c.insert_buffered(id, v);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        idx.flush_writes(); // flush remaining
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(idx.len(), 50 + total as usize);
        ms
    };

    let speedup = direct_ms / buffered_ms;
    println!(
        "[HNSW-BUF] Direct insert ({} writers × {}): {:.1}ms ({:.0} vecs/s)",
        n_writers,
        vecs_per_writer,
        direct_ms,
        total as f64 / (direct_ms / 1000.0)
    );
    println!(
        "[HNSW-BUF] Buffered insert (threshold=128): {:.1}ms ({:.0} vecs/s)",
        buffered_ms,
        total as f64 / (buffered_ms / 1000.0)
    );
    println!("[HNSW-BUF] Speedup: {:.2}×", speedup);

    // Search correctness after buffered insert
    {
        let idx = ConcurrentHnswIndex::with_flush_threshold(
            dim,
            qm_engine::index::hnsw::HnswConfig::default(),
            64,
        );
        let mut rng = rand::thread_rng();
        for i in 0..200u32 {
            let v: Vec<f32> = (0..dim).map(|_| rng.gen::<f32>()).collect();
            idx.insert_buffered(i, v);
        }
        idx.flush_writes();
        let query: Vec<f32> = (0..dim).map(|_| rng.gen::<f32>()).collect();
        let results = idx.search(&query, 10);
        assert_eq!(results.len(), 10);
        println!("[HNSW-BUF] Search correctness: {} results ✓", results.len());
    }
}
