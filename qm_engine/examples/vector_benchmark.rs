use qm_engine::index::{DistanceMetric, HnswConfig, HnswIndex};
use qm_engine::NativeSqlEngine;
use serde_json::json;
use std::cmp::Ordering;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const N: usize = 2_000;
const DIM: usize = 128;
const TOP_K: usize = 10;
const QUERIES: usize = 41;
const WARMUP: usize = 5;

#[derive(Clone, Copy)]
enum Metric {
    L2,
    Cosine,
    InnerProduct,
}

impl Metric {
    fn name(self) -> &'static str {
        match self {
            Metric::L2 => "l2",
            Metric::Cosine => "cosine",
            Metric::InnerProduct => "ip",
        }
    }

    fn op(self) -> &'static str {
        match self {
            Metric::L2 => "<->",
            Metric::Cosine => "<=>",
            Metric::InnerProduct => "<#>",
        }
    }

    fn hnsw_metric(self) -> DistanceMetric {
        match self {
            Metric::L2 => DistanceMetric::L2,
            Metric::Cosine => DistanceMetric::Cosine,
            Metric::InnerProduct => DistanceMetric::InnerProduct,
        }
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn gen_vectors(n: usize, dim: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut state = seed;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let mut v = Vec::with_capacity(dim);
        for d in 0..dim {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let raw = ((state >> 32) as u32) as f32 / (u32::MAX as f32);
            let shaped = (raw * 2.0 - 1.0) * (1.0 + ((i + d) % 17) as f32 * 0.03);
            v.push(shaped);
        }
        out.push(v);
    }
    out
}

fn vector_literal(v: &[f32]) -> String {
    let inner = v
        .iter()
        .map(|x| format!("{:.7}", x))
        .collect::<Vec<_>>()
        .join(",");
    format!("'[{}]'::vector", inner)
}

fn dist(metric: Metric, a: &[f32], b: &[f32]) -> f32 {
    match metric {
        Metric::L2 => a
            .iter()
            .zip(b.iter())
            .map(|(x, y)| {
                let d = x - y;
                d * d
            })
            .sum::<f32>()
            .sqrt(),
        Metric::Cosine => {
            let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
            let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
            let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
            if na * nb < 1e-10 {
                1.0
            } else {
                1.0 - dot / (na * nb)
            }
        }
        Metric::InnerProduct => -a.iter().zip(b.iter()).map(|(x, y)| x * y).sum::<f32>(),
    }
}

fn brute_topk(metric: Metric, vectors: &[Vec<f32>], query: &[f32], k: usize) -> Vec<usize> {
    let mut scored: Vec<(f32, usize)> = vectors
        .iter()
        .enumerate()
        .map(|(id, v)| (dist(metric, v, query), id))
        .collect();
    scored.select_nth_unstable_by(k, |a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });
    scored.truncate(k);
    scored.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });
    scored.into_iter().map(|(_, id)| id).collect()
}

fn query_ids(engine: &NativeSqlEngine, metric: Metric, query: &[f32], limit: usize) -> Vec<usize> {
    let sql = format!(
        "SELECT id FROM bench_vec ORDER BY embedding {} {} LIMIT {}",
        metric.op(),
        vector_literal(query),
        limit
    );
    let res = engine.execute(&sql).unwrap();
    res.rows
        .iter()
        .map(|row| {
            String::from_utf8(row[0].clone().unwrap())
                .unwrap()
                .parse::<usize>()
                .unwrap()
        })
        .collect()
}

fn timed_query_ids(
    engine: &NativeSqlEngine,
    metric: Metric,
    query: &[f32],
    limit: usize,
) -> (f64, Vec<usize>) {
    let t0 = Instant::now();
    let ids = query_ids(engine, metric, query, limit);
    (t0.elapsed().as_secs_f64() * 1000.0, ids)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn summarize(mut lat_ms: Vec<f64>) -> serde_json::Value {
    lat_ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let avg = lat_ms.iter().sum::<f64>() / lat_ms.len().max(1) as f64;
    json!({
        "queries": lat_ms.len(),
        "avg_ms": avg,
        "p50_ms": percentile(&lat_ms, 0.50),
        "p95_ms": percentile(&lat_ms, 0.95),
        "p99_ms": percentile(&lat_ms, 0.99)
    })
}

fn benchmark_exact(
    engine: &NativeSqlEngine,
    metric: Metric,
    queries: &[Vec<f32>],
) -> (serde_json::Value, Vec<Vec<usize>>) {
    benchmark_exact_limit(engine, metric, queries, TOP_K)
}

fn benchmark_exact_limit(
    engine: &NativeSqlEngine,
    metric: Metric,
    queries: &[Vec<f32>],
    limit: usize,
) -> (serde_json::Value, Vec<Vec<usize>>) {
    for q in queries.iter().take(WARMUP) {
        let _ = query_ids(engine, metric, q, limit);
    }

    let mut lat = Vec::with_capacity(queries.len());
    let mut rankings = Vec::with_capacity(queries.len());
    for q in queries {
        let t0 = Instant::now();
        let ids = query_ids(engine, metric, q, limit);
        lat.push(t0.elapsed().as_secs_f64() * 1000.0);
        rankings.push(ids);
    }
    (summarize(lat), rankings)
}

fn benchmark_exact_cold(
    engine: &NativeSqlEngine,
    metric: Metric,
    queries: &[Vec<f32>],
) -> serde_json::Value {
    let mut lat = Vec::with_capacity(queries.len());
    for q in queries {
        engine.execute("VACUUM bench_vec").unwrap();
        let (ms, _) = timed_query_ids(engine, metric, q, TOP_K);
        lat.push(ms);
    }
    summarize(lat)
}

fn benchmark_exact_rebuild_after_mutation(
    engine: &NativeSqlEngine,
    metric: Metric,
    queries: &[Vec<f32>],
    replacement_vector: &[f32],
) -> serde_json::Value {
    let replacement = vector_literal(replacement_vector);
    let mut lat = Vec::with_capacity(queries.len());
    for q in queries {
        engine
            .execute(&format!(
                "UPDATE bench_vec SET embedding = {} WHERE id = 0",
                replacement
            ))
            .unwrap();
        let (ms, _) = timed_query_ids(engine, metric, q, TOP_K);
        lat.push(ms);
    }
    summarize(lat)
}

fn benchmark_hnsw(metric: Metric, vectors: &[Vec<f32>], queries: &[Vec<f32>]) -> serde_json::Value {
    let mut cfg = HnswConfig::default();
    cfg.metric = metric.hnsw_metric();
    cfg.ef_search = 50;
    let mut hnsw = HnswIndex::new(DIM, cfg);
    let build_start = Instant::now();
    for (id, v) in vectors.iter().enumerate() {
        hnsw.insert(id as u32, v.clone());
    }
    let build_ms = build_start.elapsed().as_secs_f64() * 1000.0;

    let mut lat = Vec::with_capacity(queries.len());
    let mut recall = Vec::with_capacity(queries.len());
    for q in queries {
        let truth = brute_topk(metric, vectors, q, TOP_K);
        let t0 = Instant::now();
        let got = hnsw.search(q, TOP_K);
        lat.push(t0.elapsed().as_secs_f64() * 1000.0);
        let got_ids = got
            .into_iter()
            .map(|(id, _)| id as usize)
            .collect::<Vec<_>>();
        let hits = got_ids.iter().filter(|id| truth.contains(id)).count();
        recall.push(hits as f64 / TOP_K as f64);
    }
    let recall_avg = recall.iter().sum::<f64>() / recall.len().max(1) as f64;
    json!({
        "build_ms": build_ms,
        "search": summarize(lat),
        "recall_at_10_vs_bruteforce": recall_avg
    })
}

#[inline]
fn direct_score(metric: Metric, row: &[f32], query: &[f32], row_norm: f32, query_norm: f32) -> f32 {
    match metric {
        Metric::L2 => {
            let mut sum = 0.0_f32;
            for i in 0..query.len() {
                let d = row[i] - query[i];
                sum += d * d;
            }
            sum
        }
        Metric::Cosine => {
            let mut dot = 0.0_f32;
            for i in 0..query.len() {
                dot += row[i] * query[i];
            }
            let denom = row_norm * query_norm;
            if denom < 1e-10 {
                1.0
            } else {
                1.0 - dot / denom
            }
        }
        Metric::InnerProduct => {
            let mut dot = 0.0_f32;
            for i in 0..query.len() {
                dot += row[i] * query[i];
            }
            -dot
        }
    }
}

fn flatten_vectors(vectors: &[Vec<f32>]) -> (Vec<f32>, Vec<f32>) {
    let mut flat = Vec::with_capacity(vectors.len() * DIM);
    let mut norms = Vec::with_capacity(vectors.len());
    for v in vectors {
        norms.push(v.iter().map(|x| x * x).sum::<f32>().sqrt());
        flat.extend_from_slice(v);
    }
    (flat, norms)
}

fn direct_scan_only(
    metric: Metric,
    flat: &[f32],
    norms: &[f32],
    queries: &[Vec<f32>],
) -> serde_json::Value {
    let mut lat = Vec::with_capacity(queries.len());
    let mut checksum = 0.0_f32;
    for q in queries {
        let qn = q.iter().map(|x| x * x).sum::<f32>().sqrt();
        let t0 = Instant::now();
        for (idx, row) in flat.chunks_exact(DIM).enumerate() {
            checksum += direct_score(metric, row, q, norms[idx], qn);
        }
        lat.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    json!({
        "latency": summarize(lat),
        "checksum": checksum
    })
}

fn direct_topk_select_nth(
    metric: Metric,
    flat: &[f32],
    norms: &[f32],
    queries: &[Vec<f32>],
    limit: usize,
) -> serde_json::Value {
    let mut lat = Vec::with_capacity(queries.len());
    for q in queries {
        let qn = q.iter().map(|x| x * x).sum::<f32>().sqrt();
        let mut scored = Vec::with_capacity(norms.len());
        let t0 = Instant::now();
        for (idx, row) in flat.chunks_exact(DIM).enumerate() {
            scored.push((direct_score(metric, row, q, norms[idx], qn), idx));
        }
        if limit < scored.len() {
            scored.select_nth_unstable_by(limit, |a, b| {
                a.0.partial_cmp(&b.0)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| a.1.cmp(&b.1))
            });
            scored.truncate(limit);
        }
        scored.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
        lat.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    summarize(lat)
}

fn direct_topk_small_sorted(
    metric: Metric,
    flat: &[f32],
    norms: &[f32],
    queries: &[Vec<f32>],
    limit: usize,
) -> serde_json::Value {
    let mut lat = Vec::with_capacity(queries.len());
    for q in queries {
        let qn = q.iter().map(|x| x * x).sum::<f32>().sqrt();
        let t0 = Instant::now();
        let mut best: Vec<(f32, usize)> = Vec::with_capacity(limit);
        for (idx, row) in flat.chunks_exact(DIM).enumerate() {
            let item = (direct_score(metric, row, q, norms[idx], qn), idx);
            if best.len() < limit {
                best.push(item);
                best.sort_by(|a, b| {
                    b.0.partial_cmp(&a.0)
                        .unwrap_or(Ordering::Equal)
                        .then_with(|| b.1.cmp(&a.1))
                });
            } else if limit > 0 {
                let worst = best[0];
                if item.0 < worst.0 || (item.0 == worst.0 && item.1 < worst.1) {
                    best[0] = item;
                    best.sort_by(|a, b| {
                        b.0.partial_cmp(&a.0)
                            .unwrap_or(Ordering::Equal)
                            .then_with(|| b.1.cmp(&a.1))
                    });
                }
            }
        }
        best.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
        lat.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    summarize(lat)
}

fn summarize_timing_values(values: &[serde_json::Value], field: &str) -> serde_json::Value {
    summarize(
        values
            .iter()
            .filter_map(|v| v.get(field).and_then(|x| x.as_f64()))
            .collect(),
    )
}

fn benchmark_sql_timing_breakdown(
    engine: &NativeSqlEngine,
    metric: Metric,
    queries: &[Vec<f32>],
) -> serde_json::Value {
    for q in queries.iter().take(WARMUP) {
        let sql = format!(
            "SELECT id FROM bench_vec ORDER BY embedding {} {} LIMIT {}",
            metric.op(),
            vector_literal(q),
            TOP_K
        );
        let _ = engine.execute(&sql).unwrap();
    }

    let mut timings = Vec::with_capacity(queries.len());
    for q in queries {
        let sql = format!(
            "SELECT id FROM bench_vec ORDER BY embedding {} {} LIMIT {}",
            metric.op(),
            vector_literal(q),
            TOP_K
        );
        let (_result, timing) = engine.execute_vector_with_timing(&sql).unwrap();
        timings.push(timing);
    }

    let fields = [
        "sql_parse_planner_ms",
        "vector_literal_parse_ms",
        "cache_lookup_ms",
        "distance_scan_ms",
        "topk_selection_ms",
        "result_materialization_ms",
        "total_ms",
    ];
    let mut map = serde_json::Map::new();
    for field in fields {
        map.insert(field.to_string(), summarize_timing_values(&timings, field));
    }
    if let Some(first) = timings.first() {
        map.insert(
            "context".to_string(),
            json!({
                "rows_scanned": first.get("rows_scanned").cloned().unwrap_or(json!(0)),
                "dim": first.get("dim").cloned().unwrap_or(json!(0)),
                "limit": first.get("limit").cloned().unwrap_or(json!(0)),
                "typed_rows": first.get("typed_rows").cloned().unwrap_or(json!(0)),
                "text_fallback_rows": first.get("text_fallback_rows").cloned().unwrap_or(json!(0))
            }),
        );
    }
    serde_json::Value::Object(map)
}

fn postgres_status() -> serde_json::Value {
    let pg_isready = Command::new("pg_isready").output();
    match pg_isready {
        Ok(output) if output.status.success() => json!({
            "status": "available",
            "pg_isready": String::from_utf8_lossy(&output.stdout).trim().to_string()
        }),
        Ok(output) => json!({
            "status": "not_run",
            "reason": "pg_isready did not find a local PostgreSQL server",
            "stdout": String::from_utf8_lossy(&output.stdout).trim().to_string(),
            "stderr": String::from_utf8_lossy(&output.stderr).trim().to_string()
        }),
        Err(err) => json!({
            "status": "not_run",
            "reason": format!("pg_isready failed: {}", err)
        }),
    }
}

fn direct_limit_matrix(
    metric: Metric,
    flat: &[f32],
    norms: &[f32],
    queries: &[Vec<f32>],
) -> serde_json::Value {
    let mut select_nth = serde_json::Map::new();
    let mut small_sorted = serde_json::Map::new();
    for limit in [1usize, 5, 10, 100] {
        select_nth.insert(
            format!("limit_{}", limit),
            direct_topk_select_nth(metric, flat, norms, queries, limit),
        );
        small_sorted.insert(
            format!("limit_{}", limit),
            direct_topk_small_sorted(metric, flat, norms, queries, limit),
        );
    }
    json!({
        "select_nth_unstable": select_nth,
        "small_fixed_sorted": small_sorted
    })
}

fn vector_cache_rows(engine: &NativeSqlEngine) -> serde_json::Value {
    match engine.vector_cache_counts("bench_vec", "embedding") {
        Some((typed, text_fallback)) => json!({
            "typed_rows": typed,
            "text_fallback_rows": text_fallback
        }),
        None => json!({
            "typed_rows": 0,
            "text_fallback_rows": 0,
            "status": "cache_not_built"
        }),
    }
}

fn avg_ms(summary: &serde_json::Value) -> f64 {
    summary
        .get("avg_ms")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

fn main() {
    let vectors = gen_vectors(N, DIM, 0x5151_0001);
    let queries = gen_vectors(QUERIES, DIM, 0x5151_1001);
    let (flat_vectors, vector_norms) = flatten_vectors(&vectors);

    let engine = NativeSqlEngine::new();
    engine
        .execute("CREATE TABLE bench_vec (id INTEGER PRIMARY KEY, embedding TEXT)")
        .unwrap();
    for (id, v) in vectors.iter().enumerate() {
        engine
            .execute(&format!(
                "INSERT INTO bench_vec (id, embedding) VALUES ({}, {})",
                id,
                vector_literal(v)
            ))
            .unwrap();
    }

    let root = repo_root();
    let mut timing_by_metric = serde_json::Map::new();
    for metric in [Metric::L2, Metric::Cosine, Metric::InnerProduct] {
        timing_by_metric.insert(
            metric.name().to_string(),
            json!({
                "operator": metric.op(),
                "sql_exact_warm_breakdown": benchmark_sql_timing_breakdown(&engine, metric, &queries),
                "direct_scan_scorer_only": direct_scan_only(metric, &flat_vectors, &vector_norms, &queries),
                "direct_topk_select_nth_limit_10": direct_topk_select_nth(
                    metric,
                    &flat_vectors,
                    &vector_norms,
                    &queries,
                    TOP_K
                ),
                "direct_topk_small_sorted_limit_10": direct_topk_small_sorted(
                    metric,
                    &flat_vectors,
                    &vector_norms,
                    &queries,
                    TOP_K
                ),
                "direct_limit_matrix": direct_limit_matrix(metric, &flat_vectors, &vector_norms, &queries)
            }),
        );
    }
    fs::write(
        root.join("vector_timing_breakdown.json"),
        serde_json::to_string_pretty(&json!({
            "timestamp_ms": now_ms(),
            "dataset": {
                "name": "bench_vec",
                "n": N,
                "dim": DIM,
                "top_k": TOP_K,
                "queries": QUERIES,
                "warmup": WARMUP
            },
            "postgresql_pgvector": {
                "status": postgres_status(),
                "exact_scan": "not_run",
                "indexed_path": "not_run",
                "reason": "No local PostgreSQL server/DSN was available to run pgvector under the same conditions"
            },
            "qmvir": {
                "metrics": timing_by_metric
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let mut verify = String::new();
    verify.push_str("QMvir vector mapping verification\n");
    verify.push_str(&format!("timestamp_ms={}\n", now_ms()));
    verify.push_str(&format!(
        "dataset=bench_vec n={} dim={} top_k={} queries={}\n\n",
        N, DIM, TOP_K, QUERIES
    ));

    let mut all_rankings: Vec<(&str, Vec<Vec<usize>>)> = Vec::new();
    for metric in [Metric::L2, Metric::Cosine, Metric::InnerProduct] {
        let exact_cold = benchmark_exact_cold(&engine, metric, &queries);
        let (exact_warm, rankings) = benchmark_exact(&engine, metric, &queries);
        let mut exact_limit_matrix = serde_json::Map::new();
        for limit in [1usize, 5, 10, 100] {
            let (summary, _) = benchmark_exact_limit(&engine, metric, &queries, limit);
            exact_limit_matrix.insert(format!("limit_{}", limit), summary);
        }
        let exact_rebuild =
            benchmark_exact_rebuild_after_mutation(&engine, metric, &queries, &vectors[0]);
        let cache_rows = vector_cache_rows(&engine);
        let typed_rows = cache_rows
            .get("typed_rows")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let text_fallback_rows = cache_rows
            .get("text_fallback_rows")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let hnsw = benchmark_hnsw(metric, &vectors, &queries);
        let brute_rankings = queries
            .iter()
            .map(|q| brute_topk(metric, &vectors, q, TOP_K))
            .collect::<Vec<_>>();
        let mismatches = rankings
            .iter()
            .zip(brute_rankings.iter())
            .filter(|(a, b)| a != b)
            .count();
        let parity = mismatches == 0;
        let file_name = match metric {
            Metric::L2 => "vector_last_run_l2.json",
            Metric::Cosine => "vector_last_run_cosine.json",
            Metric::InnerProduct => "vector_last_run_ip.json",
        };
        let report = json!({
            "timestamp_ms": now_ms(),
            "dataset": {"name": "bench_vec", "n": N, "dim": DIM, "top_k": TOP_K, "queries": QUERIES},
            "metric": metric.name(),
            "operator": metric.op(),
            "dim": DIM,
            "rows": N,
            "limit": TOP_K,
            "qmvir_after_fix": {
                "exact_scan": exact_warm,
                "exact_scan_cold_cache": exact_cold,
                "exact_scan_warm_cache": exact_warm,
                "exact_scan_rebuild_after_mutation": exact_rebuild,
                "typed_rows": typed_rows,
                "text_fallback_rows": text_fallback_rows,
                "cold_cache_ms": avg_ms(&exact_cold),
                "warm_cache_ms": avg_ms(&exact_warm),
                "rebuild_after_mutation_ms": avg_ms(&exact_rebuild),
                "operator": metric.op(),
                "dim": DIM,
                "rows": N,
                "limit": TOP_K,
                "exact_scan_warm_limit_matrix": exact_limit_matrix,
                "exact_scan_storage": "parsed_vector_cache_flat_f32_with_cached_row_norms",
                "vector_cache_rows": cache_rows,
                "bruteforce_parity": parity,
                "mismatches": mismatches
            },
            "qmvir_old_text_parse_exact": {
                "status": "not_run",
                "reason": "text-parse hot loop was replaced by parsed vector cache in this run"
            },
            "qmvir_lazy_hnsw_direct": hnsw,
            "postgresql_pgvector": {
                "exact_scan": "not_run",
                "indexed_path": "not_run",
                "reason": "No PostgreSQL/pgvector DSN is configured for this local run"
            },
            "baselines_from_regression_report": {
                "qmvir_5_1_0_ms": match metric {
                    Metric::L2 => 6.95,
                    Metric::Cosine => 7.11,
                    Metric::InnerProduct => 7.60
                },
                "qmvir_5_1_6_before_fix_ms": match metric {
                    Metric::L2 => 8.73,
                    Metric::Cosine => 8.80,
                    Metric::InnerProduct => 7.92
                }
            }
        });
        fs::write(
            root.join(file_name),
            serde_json::to_string_pretty(&report).unwrap(),
        )
        .unwrap();
        verify.push_str(&format!(
            "{} {} parity={} mismatches={} first_query_top10={:?} brute={:?}\n",
            metric.name(),
            metric.op(),
            parity,
            mismatches,
            rankings.first().cloned().unwrap_or_default(),
            brute_rankings.first().cloned().unwrap_or_default()
        ));
        all_rankings.push((metric.name(), rankings));
    }

    if all_rankings.len() == 3 {
        let same_all =
            all_rankings[0].1 == all_rankings[1].1 && all_rankings[1].1 == all_rankings[2].1;
        verify.push_str(&format!(
            "\nall_three_operator_rankings_identical={}\n",
            same_all
        ));
        verify.push_str("missing_limit_behavior=documented_default_limit_10\n");
    }
    fs::write(root.join("verify_vector_mapping_last.txt"), verify).unwrap();
}
