//! QM Engine Comprehensive Benchmark Suite
//!
//! Benchmarks all core subsystems:
//! 1. Ring Buffer IPC (publish/consume throughput)
//! 2. W-TinyLFU Cache (get/insert hit-rate vs LRU)
//! 3. JIT SQL Compiler (batch_filter, batch_project, interpret)
//! 4. io_uring WAL (append throughput, flush latency)
//! 5. Native Hub Dispatcher (dispatch latency, batch throughput)
//! 6. LSN Sequencer (single + batch allocation)

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::path::Path;
use tempfile::TempDir;

// ── Ring Buffer Benchmarks ────────────────────────────────────────

fn bench_ring_buffer(c: &mut Criterion) {
    use qm_engine::ipc::ring_buffer::{CommandType, SharedRingBuffer};

    let mut group = c.benchmark_group("ring_buffer");

    // Setup: create a ring buffer with 1024 slots of 4KB each
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("bench_ring.shm");
    let ring = SharedRingBuffer::create(&path, 1024, 4096).unwrap();
    let payload_1k = vec![0xABu8; 1024];

    group.throughput(Throughput::Elements(1));
    group.bench_function("publish_1KB", |b| {
        b.iter(|| {
            // Publish then immediately consume+complete to free the slot
            let seq = ring
                .publish(1, CommandType::Insert, black_box(&payload_1k))
                .unwrap();
            if let Some(consumed) = ring.consume() {
                ring.complete(consumed.slot_idx, None);
            }
            seq
        })
    });

    let payload_64b = vec![0xCDu8; 64];
    group.bench_function("publish_64B", |b| {
        b.iter(|| {
            let seq = ring
                .publish(1, CommandType::Query, black_box(&payload_64b))
                .unwrap();
            if let Some(consumed) = ring.consume() {
                ring.complete(consumed.slot_idx, None);
            }
            seq
        })
    });

    // Throughput test: publish N messages (single-threaded pipeline)
    for &batch_size in &[100, 1000] {
        group.throughput(Throughput::Elements(batch_size));
        group.bench_with_input(
            BenchmarkId::new("pipeline_throughput", batch_size),
            &batch_size,
            |b, &n| {
                let tmp2 = TempDir::new().unwrap();
                let path2 = tmp2.path().join("bench_pipe.shm");
                let ring2 = SharedRingBuffer::create(&path2, 1024, 256).unwrap();
                let payload = vec![0u8; 64];
                b.iter(|| {
                    for _ in 0..n {
                        ring2.publish(1, CommandType::Insert, &payload).unwrap();
                        if let Some(consumed) = ring2.consume() {
                            ring2.complete(consumed.slot_idx, None);
                        }
                    }
                })
            },
        );
    }

    group.bench_function("status_diagnostic", |b| b.iter(|| black_box(ring.status())));

    group.bench_function("crash_recovery", |b| {
        let tmp3 = TempDir::new().unwrap();
        let path3 = tmp3.path().join("bench_recovery.shm");
        let ring3 = SharedRingBuffer::create(&path3, 64, 256).unwrap();
        // Publish some data
        for i in 0..32 {
            ring3.publish(i, CommandType::Insert, b"data").unwrap();
        }
        b.iter(|| black_box(ring3.recover_after_crash()))
    });

    group.finish();
}

// ── LSN Sequencer Benchmarks ──────────────────────────────────────

fn bench_lsn_sequencer(c: &mut Criterion) {
    use qm_engine::ipc::lsn::LsnSequencer;

    let mut group = c.benchmark_group("lsn_sequencer");
    let seq = LsnSequencer::new(1);

    group.bench_function("next_single", |b| b.iter(|| black_box(seq.next())));

    group.bench_function("next_batch_100", |b| {
        b.iter(|| black_box(seq.next_batch(100)))
    });

    group.bench_function("current_lsn_read", |b| {
        b.iter(|| black_box(seq.current_lsn()))
    });

    group.finish();
}

// ── W-TinyLFU Cache Benchmarks ────────────────────────────────────

fn bench_cache(c: &mut Criterion) {
    use qm_engine::storage::WTinyLfuCache;

    let mut group = c.benchmark_group("wtinylfu_cache");

    // Insert benchmark
    group.bench_function("insert_1000", |b| {
        b.iter(|| {
            let mut cache = WTinyLfuCache::<u64, Vec<u8>>::new(1_048_576); // 1MB
            for i in 0..1000u64 {
                cache.insert(i, vec![0u8; 64], 64);
            }
            black_box(cache.len())
        })
    });

    // Get benchmark (hot path)
    group.bench_function("get_hot_path", |b| {
        let mut cache = WTinyLfuCache::<u64, Vec<u8>>::new(1_048_576);
        for i in 0..1000u64 {
            cache.insert(i, vec![i as u8; 64], 64);
        }
        b.iter(|| {
            // Access hot keys repeatedly
            for i in 0..100u64 {
                black_box(cache.get(&i));
            }
        })
    });

    // Get benchmark (miss path)
    group.bench_function("get_miss", |b| {
        let mut cache = WTinyLfuCache::<u64, Vec<u8>>::new(65536);
        for i in 0..100u64 {
            cache.insert(i, vec![0u8; 64], 64);
        }
        b.iter(|| {
            black_box(cache.get(&999999));
        })
    });

    // Mixed workload: 80% reads, 20% writes (simulates real DB)
    group.bench_function("mixed_80r_20w", |b| {
        let mut cache = WTinyLfuCache::<u64, Vec<u8>>::new(524288); // 512KB
        for i in 0..500u64 {
            cache.insert(i, vec![0u8; 128], 128);
        }
        let mut counter = 0u64;
        b.iter(|| {
            counter += 1;
            if counter % 5 == 0 {
                // Write (20%)
                cache.insert(counter % 2000, vec![0u8; 128], 128);
            } else {
                // Read (80%)
                black_box(cache.get(&(counter % 500)));
            }
        })
    });

    // Hit rate measurement: Zipfian-like access pattern
    group.bench_function("hit_rate_zipfian_1M_ops", |b| {
        b.iter(|| {
            let mut cache = WTinyLfuCache::<u64, Vec<u8>>::new(131072); // 128KB = ~2048 entries
                                                                        // Insert initial set
            for i in 0..2000u64 {
                cache.insert(i, vec![0u8; 64], 64);
            }
            // Access with Zipfian-like hotspot pattern
            for i in 0..10000u64 {
                // 80% of accesses go to 20% of keys (hot set)
                let key = if i % 5 < 4 {
                    i % 400 // hot set (20% = 400 keys)
                } else {
                    400 + (i % 1600) // cold set (80% = 1600 keys)
                };
                cache.get(&key);
                if cache.get(&key).is_none() {
                    cache.insert(key, vec![0u8; 64], 64);
                }
            }
            black_box(cache.hit_rate())
        })
    });

    // ConcurrentCache benchmark
    {
        use qm_engine::storage::ConcurrentCache;

        group.bench_function("concurrent_insert_get", |b| {
            let cache = ConcurrentCache::<u64, Vec<u8>>::new(1_048_576);
            for i in 0..1000u64 {
                cache.insert(i, vec![0u8; 64], 64);
            }
            let mut counter = 0u64;
            b.iter(|| {
                counter += 1;
                if counter % 3 == 0 {
                    cache.insert(counter % 2000, vec![0u8; 64], 64);
                } else {
                    black_box(cache.get(&(counter % 1000)));
                }
            })
        });
    }

    group.finish();
}

// ── JIT SQL Compiler Benchmarks ───────────────────────────────────

fn bench_jit(c: &mut Criterion) {
    use qm_engine::executor::jit::*;

    let mut group = c.benchmark_group("jit_compiler");

    // ── Interpret single expression ──
    group.bench_function("interpret_eq_i64", |b| {
        let expr = builder::eq_i64(0, 42);
        let row = RowAccessor {
            i64_cols: &[42, 10, 20],
            f64_cols: &[],
        };
        b.iter(|| black_box(interpret(&expr, &row)))
    });

    group.bench_function("interpret_complex_expr", |b| {
        // (col0 > 10) AND (col1 < 100) AND (col_f0 BETWEEN 0.5 AND 1.5)
        let expr = JitExpr::And(
            Box::new(JitExpr::And(
                Box::new(JitExpr::Gt(
                    Box::new(JitExpr::ColI64(0)),
                    Box::new(JitExpr::LitI64(10)),
                )),
                Box::new(JitExpr::Lt(
                    Box::new(JitExpr::ColI64(1)),
                    Box::new(JitExpr::LitI64(100)),
                )),
            )),
            Box::new(JitExpr::Between(
                Box::new(JitExpr::ColF64(0)),
                Box::new(JitExpr::LitF64(0.5)),
                Box::new(JitExpr::LitF64(1.5)),
            )),
        );
        let row = RowAccessor {
            i64_cols: &[42, 50],
            f64_cols: &[1.0],
        };
        b.iter(|| black_box(interpret(&expr, &row)))
    });

    // ── Batch filter benchmarks ──
    for &row_count in &[1_000, 10_000, 100_000, 1_000_000] {
        group.throughput(Throughput::Elements(row_count as u64));
        group.bench_with_input(
            BenchmarkId::new("batch_filter_eq", row_count),
            &row_count,
            |b, &n| {
                let expr = builder::eq_i64(0, 42);
                let col: Vec<i64> = (0..n)
                    .map(|i| if i % 100 == 0 { 42 } else { i as i64 })
                    .collect();
                b.iter(|| black_box(batch_filter(&expr, &[&col], &[], n)))
            },
        );
    }

    for &row_count in &[1_000, 10_000, 100_000] {
        group.throughput(Throughput::Elements(row_count as u64));
        group.bench_with_input(
            BenchmarkId::new("batch_filter_between", row_count),
            &row_count,
            |b, &n| {
                let expr = builder::between_f64(0, 10.0, 90.0);
                let col: Vec<f64> = (0..n).map(|i| (i % 100) as f64).collect();
                b.iter(|| black_box(batch_filter(&expr, &[], &[&col], n)))
            },
        );
    }

    // ── Batch project benchmark (compute col_a * col_b + 100) ──
    for &row_count in &[1_000, 10_000, 100_000] {
        group.throughput(Throughput::Elements(row_count as u64));
        group.bench_with_input(
            BenchmarkId::new("batch_project_mul_add", row_count),
            &row_count,
            |b, &n| {
                let expr = builder::compute_f64(0, 1, 100.0);
                let a: Vec<f64> = (0..n).map(|i| i as f64).collect();
                let b_col: Vec<f64> = (0..n).map(|i| (i * 2) as f64).collect();
                b.iter(|| black_box(batch_project(&expr, &[], &[&a, &b_col], n)))
            },
        );
    }

    // ── JitCache benchmark ──
    group.bench_function("jit_cache_register_get", |b| {
        let cache = JitCache::new(100);
        b.iter(|| {
            let expr = builder::eq_i64(0, 42);
            cache.register(12345, expr);
            cache.record_execution(12345);
            black_box(cache.get_expr(12345))
        })
    });

    group.finish();
}

// ── io_uring WAL Benchmarks ──────────────────────────────────────

fn bench_uring_wal(c: &mut Criterion) {
    use qm_engine::storage::UringWalWriter;

    let mut group = c.benchmark_group("uring_wal");

    // Small record append (100 bytes)
    group.bench_function("append_100B", |b| {
        let tmp = TempDir::new().unwrap();
        let mut wal = UringWalWriter::open(tmp.path(), false).unwrap();
        let data = vec![0xABu8; 100];
        b.iter(|| black_box(wal.append(1, 1, &data).unwrap()))
    });

    // Medium record append (4KB)
    group.bench_function("append_4KB", |b| {
        let tmp = TempDir::new().unwrap();
        let mut wal = UringWalWriter::open(tmp.path(), false).unwrap();
        let data = vec![0xCDu8; 4096];
        b.iter(|| black_box(wal.append(1, 1, &data).unwrap()))
    });

    // Append + flush cycle (durability guarantee)
    group.bench_function("append_flush_100B", |b| {
        let tmp = TempDir::new().unwrap();
        let mut wal = UringWalWriter::open(tmp.path(), false).unwrap();
        let data = vec![0xEFu8; 100];
        b.iter(|| {
            wal.append(1, 1, &data).unwrap();
            wal.flush().unwrap();
        })
    });

    // Batch append throughput (32 records then flush = group commit)
    for &batch_size in &[32, 128, 512] {
        group.throughput(Throughput::Elements(batch_size as u64));
        group.bench_with_input(
            BenchmarkId::new("batch_append_flush", batch_size),
            &batch_size,
            |b, &n| {
                let tmp = TempDir::new().unwrap();
                let mut wal = UringWalWriter::open(tmp.path(), false).unwrap();
                let data = vec![0u8; 200];
                b.iter(|| {
                    for i in 0..n {
                        wal.append(i as u64, 1, &data).unwrap();
                    }
                    wal.flush().unwrap();
                })
            },
        );
    }

    // Recovery benchmark (scan segments and decode)
    group.bench_function("recover_1000_records", |b| {
        let tmp = TempDir::new().unwrap();
        let mut wal = UringWalWriter::open(tmp.path(), false).unwrap();
        let data = vec![0u8; 100];
        for i in 0..1000 {
            wal.append(i, 1, &data).unwrap();
        }
        wal.flush().unwrap();

        b.iter(|| black_box(wal.recover().unwrap().len()))
    });

    group.finish();
}

// ── Native Dispatcher Benchmarks ──────────────────────────────────

fn bench_dispatcher(c: &mut Criterion) {
    use qm_engine::ipc::dispatcher::{DispatcherConfig, NativeDispatcher};

    let mut group = c.benchmark_group("native_dispatcher");

    // Dispatch single insert
    group.bench_function("dispatch_insert", |b| {
        let tmp = TempDir::new().unwrap();
        let config = DispatcherConfig {
            ring_dir: tmp.path().to_path_buf(),
            slot_count: 1024,
            slot_data_size: 4096,
            timeout_ms: 5000,
        };
        let dispatcher = NativeDispatcher::new(config).unwrap();
        let payload = b"INSERT INTO users VALUES (1, 'alice')";

        b.iter(|| {
            // Dispatch then drain to avoid ring full
            let r = dispatcher.dispatch_insert(black_box(payload)).unwrap();
            black_box(r.lsn)
        })
    });

    // Dispatch vector operation
    group.bench_function("dispatch_vector_op", |b| {
        let tmp = TempDir::new().unwrap();
        let config = DispatcherConfig {
            ring_dir: tmp.path().to_path_buf(),
            slot_count: 1024,
            slot_data_size: 4096,
            timeout_ms: 5000,
        };
        let dispatcher = NativeDispatcher::new(config).unwrap();
        let payload = b"VECTOR SEARCH dim=128 k=10";

        b.iter(|| {
            let r = dispatcher.dispatch_vector_op(black_box(payload)).unwrap();
            black_box(r.lsn)
        })
    });

    // Mixed dispatch workload
    group.bench_function("mixed_dispatch_100", |b| {
        let tmp = TempDir::new().unwrap();
        let config = DispatcherConfig {
            ring_dir: tmp.path().to_path_buf(),
            slot_count: 2048,
            slot_data_size: 4096,
            timeout_ms: 5000,
        };
        let dispatcher = NativeDispatcher::new(config).unwrap();
        let insert_payload = b"INSERT data";
        let query_payload = b"SELECT * FROM t";
        let vec_payload = b"VECTOR SEARCH";

        b.iter(|| {
            for _ in 0..40 {
                dispatcher.dispatch_insert(insert_payload).unwrap();
            }
            for _ in 0..40 {
                dispatcher.dispatch_query(query_payload).unwrap();
            }
            for _ in 0..20 {
                dispatcher.dispatch_vector_op(vec_payload).unwrap();
            }
        })
    });

    group.finish();
}

// ── Criterion Groups ──────────────────────────────────────────────

criterion_group!(
    benches,
    bench_ring_buffer,
    bench_lsn_sequencer,
    bench_cache,
    bench_jit,
    bench_uring_wal,
    bench_dispatcher,
);
criterion_main!(benches);
