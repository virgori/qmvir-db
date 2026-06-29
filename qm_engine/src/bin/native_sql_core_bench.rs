use qm_engine::gateway::native_sql::{
    Cell, ColType, ColumnConstraint, ForeignKey, NativeRow, NativeSqlEngine, NativeTable,
};
use qm_engine::index::{BPlusTree, IndexKey, IndexLookupKeyRef};
use qm_engine::mvcc::{visibility, Isolation, MvccRowVersion, Snapshot, TransactionRecord};
use serde::Serialize;
use ahash::AHashMap;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

#[derive(Serialize)]
struct BenchRow {
    name: String,
    category: String,
    iterations: usize,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    min_ms: f64,
    max_ms: f64,
    mean_ms: f64,
    throughput_ops_sec: f64,
    duration_sec: f64,
    error_count: usize,
}

#[derive(Serialize)]
struct CoreBenchReport {
    schema_version: u32,
    mode: String,
    results: Vec<BenchRow>,
}

fn percentile(values: &[f64], pct: f64) -> f64 {
    let mut ordered = values.to_vec();
    ordered.sort_by(|a, b| a.total_cmp(b));
    let idx = (((pct / 100.0) * ((ordered.len() - 1) as f64)).round() as usize)
        .min(ordered.len().saturating_sub(1));
    ordered[idx]
}

fn bench<F>(name: &str, iterations: usize, mut f: F) -> BenchRow
where
    F: FnMut(),
{
    for _ in 0..iterations.min(10) {
        f();
    }
    let mut latencies = Vec::with_capacity(iterations);
    let start = Instant::now();
    for _ in 0..iterations {
        let op = Instant::now();
        f();
        latencies.push(op.elapsed().as_secs_f64() * 1000.0);
    }
    let elapsed = start.elapsed().as_secs_f64();
    let mean_ms = latencies.iter().sum::<f64>() / latencies.len().max(1) as f64;
    BenchRow {
        name: name.to_string(),
        category: "core_direct".to_string(),
        iterations,
        p50_ms: percentile(&latencies, 50.0),
        p95_ms: percentile(&latencies, 95.0),
        p99_ms: percentile(&latencies, 99.0),
        min_ms: latencies.iter().copied().fold(f64::INFINITY, f64::min),
        max_ms: latencies.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        mean_ms,
        throughput_ops_sec: iterations as f64 / elapsed.max(f64::EPSILON),
        duration_sec: elapsed,
        error_count: 0,
    }
}

fn table(columns: &[&str], types: &[ColType]) -> NativeTable {
    NativeTable {
        columns: columns.iter().map(|c| c.to_string()).collect(),
        column_types: types.to_vec(),
        rows: HashMap::new(),
        next_auto_id: 1,
        foreign_keys: Vec::<ForeignKey>::new(),
        constraints: columns
            .iter()
            .map(|_| ColumnConstraint {
                not_null: false,
                unique: false,
                default_value: None,
                check_exprs: Vec::new(),
            })
            .collect(),
        table_checks: Vec::new(),
        sequences: HashMap::new(),
    }
}

fn row(id: i64, score: i64, label: &str) -> NativeRow {
    let mut cols = HashMap::with_capacity(3);
    cols.insert("id".to_string(), Cell::Int(id));
    cols.insert("score".to_string(), Cell::Int(score));
    cols.insert("label".to_string(), Cell::Text(label.to_string()));
    NativeRow {
        cols,
        last_modified_lsn: 0,
    }
}

fn l2(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| {
            let d = x - y;
            d * d
        })
        .sum::<f32>()
        .sqrt()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        1.0
    } else {
        1.0 - dot / (na.sqrt() * nb.sqrt())
    }
}

fn inner_product(a: &[f32], b: &[f32]) -> f32 {
    -a.iter().zip(b.iter()).map(|(x, y)| x * y).sum::<f32>()
}

fn parse_args() -> (usize, Option<PathBuf>) {
    let mut iterations = 1000usize;
    let mut output = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--iterations" => {
                if let Some(v) = args.next() {
                    iterations = v.parse().unwrap_or(iterations);
                }
            }
            "--output" => {
                if let Some(v) = args.next() {
                    output = Some(PathBuf::from(v));
                }
            }
            _ => {}
        }
    }
    (iterations, output)
}

fn main() {
    let (iterations, output) = parse_args();
    let mut results = Vec::new();

    results.push(bench("core.noop", iterations, || {
        black_box(());
    }));

    let engine = NativeSqlEngine::new();
    {
        let mut t = table(
            &["id", "score", "label"],
            &[ColType::Integer, ColType::Integer, ColType::Text],
        );
        for id in 1..=iterations.max(10_000) as i64 {
            t.rows.insert(id, row(id, id % 101, "seed"));
        }
        engine
            .tables
            .insert_native("core".to_string(), t);
    }

    results.push(bench("core.table_lookup_by_id", iterations, || {
        let shared = engine.tables.get_shared("core").unwrap();
        black_box(shared.read().rows.len());
    }));

    let insert_engine = NativeSqlEngine::new();
    insert_engine.tables.insert_native(
        "insert_core".to_string(),
        table(
            &["id", "score", "label"],
            &[ColType::Integer, ColType::Integer, ColType::Text],
        ),
    );
    let mut next_id = 1i64;
    results.push(bench("core.row_insert_direct_no_index", iterations, || {
        let id = next_id;
        next_id += 1;
        let shared = insert_engine.tables.get_shared("insert_core").unwrap();
        shared
            .write()
            .rows
            .insert(id, row(id, id % 101, "insert"));
    }));

    let mut select_id = 1i64;
    results.push(bench("core.row_select_direct_by_id", iterations, || {
        let shared = engine.tables.get_shared("core").unwrap();
        let table = shared.read();
        black_box(table.rows.get(&select_id));
        select_id += 1;
        if select_id > iterations.max(10_000) as i64 {
            select_id = 1;
        }
    }));

    let mut update_id = 1i64;
    results.push(bench("core.row_update_direct_by_id", iterations, || {
        let shared = engine.tables.get_shared("core").unwrap();
        let mut table = shared.write();
        if let Some(row) = table.rows.get_mut(&update_id) {
            row.cols.insert("score".to_string(), Cell::Int(update_id));
        }
        update_id += 1;
        if update_id > iterations.max(10_000) as i64 {
            update_id = 1;
        }
    }));

    let delete_engine = NativeSqlEngine::new();
    {
        let mut t = table(
            &["id", "score", "label"],
            &[ColType::Integer, ColType::Integer, ColType::Text],
        );
        for id in 1..=iterations.max(10_000) as i64 {
            t.rows.insert(id, row(id, id % 101, "delete"));
        }
        delete_engine
            .tables
            .insert_native("delete_core".to_string(), t);
    }
    let mut delete_id = 1i64;
    results.push(bench("core.row_delete_direct_by_id", iterations, || {
        let shared = delete_engine.tables.get_shared("delete_core").unwrap();
        shared
            .write()
            .rows
            .remove(&delete_id);
        delete_id += 1;
    }));

    let numeric_index = BPlusTree::new("idx_num".into(), "core".into(), vec!["score".into()]);
    let string_index = BPlusTree::new("idx_str".into(), "core".into(), vec!["label".into()]);
    let duplicate_string_index =
        BPlusTree::new("idx_str_dup".into(), "core".into(), vec!["label".into()]);
    for id in 1..=10_000i64 {
        numeric_index.insert(IndexKey::Integer(id), id);
        string_index.insert(IndexKey::Str(format!("label_{id}")), id);
        duplicate_string_index.insert(IndexKey::Str(format!("bucket_{}", id % 10)), id);
    }
    results.push(bench(
        "core.index_lookup_numeric_direct",
        iterations,
        || {
            black_box(numeric_index.search(&IndexKey::Integer(7777)));
        },
    ));
    let string_key = IndexKey::Str("label_7777".to_string());
    results.push(bench("core.index_lookup_string_direct", iterations, || {
        black_box(string_index.search(&string_key));
    }));
    results.push(bench(
        "core.index_lookup_string_borrowed_direct",
        iterations,
        || {
            black_box(string_index.search_ref(IndexLookupKeyRef::Str("label_7777")));
        },
    ));
    results.push(bench("string_index.key_build_owned", iterations, || {
        black_box(IndexKey::Str("label_7777".to_string()));
    }));
    results.push(bench("string_index.lookup_unique", iterations, || {
        black_box(string_index.search(&string_key));
    }));
    results.push(bench(
        "string_index.lookup_borrowed_key",
        iterations,
        || {
            black_box(string_index.search_ref(IndexLookupKeyRef::Str("label_7777")));
        },
    ));
    let duplicate_key = IndexKey::Str("bucket_7".to_string());
    results.push(bench(
        "string_index.lookup_duplicate_heavy",
        iterations,
        || {
            black_box(duplicate_string_index.search(&duplicate_key));
        },
    ));
    results.push(bench(
        "string_index.compare_numeric_baseline",
        iterations,
        || {
            black_box(numeric_index.search(&IndexKey::Integer(7777)));
        },
    ));

    let mut payload_cols = HashMap::new();
    payload_cols.insert("id".to_string(), Cell::Int(1));
    let mut version = MvccRowVersion::new(
        1,
        1,
        1,
        Arc::new(NativeRow {
            cols: payload_cols,
            last_modified_lsn: 0,
        }),
    );
    version.publish_create(1);
    let snapshot = Snapshot {
        read_ts: 1,
        own_tx_id: 2,
        active_tx_ids: HashSet::new(),
        isolation: Isolation::ReadCommitted,
    };
    let registry: AHashMap<u64, TransactionRecord> = AHashMap::new();
    results.push(bench(
        "core.mvcc_visibility_check_direct",
        iterations,
        || {
            black_box(visibility::is_visible(&version, &snapshot, &registry));
        },
    ));

    let a: Vec<f32> = (0..128).map(|i| i as f32 * 0.01).collect();
    let b: Vec<f32> = (0..128).map(|i| (128 - i) as f32 * 0.01).collect();
    results.push(bench("core.vector_distance_l2_direct", iterations, || {
        black_box(l2(&a, &b));
    }));
    results.push(bench(
        "core.vector_distance_cosine_direct",
        iterations,
        || {
            black_box(cosine(&a, &b));
        },
    ));
    results.push(bench("core.vector_distance_ip_direct", iterations, || {
        black_box(inner_product(&a, &b));
    }));

    let report = CoreBenchReport {
        schema_version: 1,
        mode: "core_direct".to_string(),
        results,
    };
    let json = serde_json::to_string_pretty(&report).expect("serialize report");
    if let Some(path) = output {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create output dir");
        }
        fs::write(path, json).expect("write output");
    } else {
        println!("{json}");
    }
}
