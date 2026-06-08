//! QM Engine Performance Benchmark Suite
//!
//! Run with: cargo test --release bench_ -- --nocapture --ignored
//!
//! Tests all core subsystems with precise timing:
//! 1. Ring Buffer IPC (publish/consume throughput)
//! 2. W-TinyLFU Cache (hit/miss, mixed workloads)
//! 3. JIT SQL Compiler (batch_filter, batch_project)
//! 4. io_uring WAL (append, flush, recovery)
//! 5. Native Hub Dispatcher (dispatch, batch throughput)
//! 6. LSN Sequencer (allocation throughput)

use std::time::{Duration, Instant};

/// Run a benchmark N iterations, return (total_time, per_op_ns, ops_per_sec).
fn bench_fn<F: FnMut()>(name: &str, iterations: u64, mut f: F) -> (Duration, f64, f64) {
    // Warmup
    for _ in 0..iterations.min(100) {
        f();
    }

    let start = Instant::now();
    for _ in 0..iterations {
        f();
    }
    let elapsed = start.elapsed();

    let per_op_ns = elapsed.as_nanos() as f64 / iterations as f64;
    let ops_sec = if elapsed.as_secs_f64() > 0.0 {
        iterations as f64 / elapsed.as_secs_f64()
    } else {
        f64::INFINITY
    };

    println!(
        "  {:<45} {:>10.1} ns/op  {:>12.0} ops/sec  ({:.3}s total, {} iters)",
        name,
        per_op_ns,
        ops_sec,
        elapsed.as_secs_f64(),
        iterations
    );

    (elapsed, per_op_ns, ops_sec)
}

/// Measure throughput with known element counts.
fn bench_throughput<F: FnMut()>(name: &str, iterations: u64, elements_per_iter: u64, mut f: F) {
    for _ in 0..iterations.min(50) {
        f();
    }

    let start = Instant::now();
    for _ in 0..iterations {
        f();
    }
    let elapsed = start.elapsed();

    let total_elements = iterations * elements_per_iter;
    let elem_per_sec = total_elements as f64 / elapsed.as_secs_f64();
    let per_op_ns = elapsed.as_nanos() as f64 / iterations as f64;

    println!(
        "  {:<45} {:>10.1} ns/op  {:>12.0} elem/sec  ({} elements)",
        name, per_op_ns, elem_per_sec, total_elements
    );
}

// ═══════════════════════════════════════════════════════════════════
// Ring Buffer IPC
// ═══════════════════════════════════════════════════════════════════

#[test]
#[ignore]
fn bench_ring_buffer_publish_consume() {
    use qm_engine::ipc::ring_buffer::{CommandType, SharedRingBuffer};

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║           RING BUFFER IPC BENCHMARK                         ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    let tmp = tempfile::TempDir::new().unwrap();

    // Helper: drain all pending entries from ring buffer
    fn drain_ring(ring: &SharedRingBuffer) {
        while let Some(c) = ring.consume() {
            ring.complete(c.slot_idx, None);
        }
    }

    // Helper: manual bench loop with periodic drain (avoids warmup overflow)
    fn bench_ring_op(name: &str, ring: &SharedRingBuffer, iterations: u64, mut op: impl FnMut()) {
        // warmup with drain
        for _ in 0..100 {
            op();
            drain_ring(ring);
        }
        drain_ring(ring);

        let start = Instant::now();
        for i in 0..iterations {
            op();
            if i % 512 == 511 {
                drain_ring(ring);
            }
        }
        drain_ring(ring);
        let elapsed = start.elapsed();
        let per_op_ns = elapsed.as_nanos() as f64 / iterations as f64;
        let ops_sec = iterations as f64 / elapsed.as_secs_f64();
        println!(
            "  {:<50} {:>10.1} ns/op  {:>12.0} ops/sec",
            name, per_op_ns, ops_sec
        );
    }

    // 1KB payload publish + consume cycle
    {
        let path = tmp.path().join("bench_1k.shm");
        let ring = SharedRingBuffer::create(&path, 1024, 4096).unwrap();
        let payload = vec![0xABu8; 1024];

        bench_ring_op(
            "publish+consume+complete (1KB payload)",
            &ring,
            100_000,
            || {
                let _ = ring.publish(1, CommandType::Insert, &payload);
                if let Some(c) = ring.consume() {
                    ring.complete(c.slot_idx, None);
                }
            },
        );
    }

    // 64B payload (hot path / small commands)
    {
        let path = tmp.path().join("bench_64b.shm");
        let ring = SharedRingBuffer::create(&path, 1024, 4096).unwrap();
        let payload = vec![0xCDu8; 64];

        bench_ring_op(
            "publish+consume+complete (64B payload)",
            &ring,
            500_000,
            || {
                let _ = ring.publish(1, CommandType::Query, &payload);
                if let Some(c) = ring.consume() {
                    ring.complete(c.slot_idx, None);
                }
            },
        );
    }

    // Pipeline throughput: 1000 messages
    {
        let path = tmp.path().join("bench_pipe.shm");
        let ring = SharedRingBuffer::create(&path, 2048, 256).unwrap();
        let payload = vec![0u8; 64];

        // Manual timing for pipeline
        let iters = 100u64;
        for _ in 0..5 {
            for _ in 0..1000 {
                let _ = ring.publish(1, CommandType::Insert, &payload);
                if let Some(c) = ring.consume() {
                    ring.complete(c.slot_idx, None);
                }
            }
            drain_ring(&ring);
        }
        drain_ring(&ring);

        let start = Instant::now();
        for _ in 0..iters {
            for _ in 0..1000 {
                let _ = ring.publish(1, CommandType::Insert, &payload);
                if let Some(c) = ring.consume() {
                    ring.complete(c.slot_idx, None);
                }
            }
            drain_ring(&ring);
        }
        let elapsed = start.elapsed();
        let total_msgs = iters * 1000;
        let per_msg_ns = elapsed.as_nanos() as f64 / total_msgs as f64;
        let msgs_sec = total_msgs as f64 / elapsed.as_secs_f64();
        println!(
            "  {:<50} {:>10.1} ns/op  {:>12.0} elem/sec",
            "pipeline 1000 msgs (64B each)", per_msg_ns, msgs_sec
        );
    }

    // Status diagnostic read
    {
        let path = tmp.path().join("bench_status.shm");
        let ring = SharedRingBuffer::create(&path, 256, 1024).unwrap();

        bench_fn("status() diagnostic read", 1_000_000, || {
            std::hint::black_box(ring.status());
        });
    }

    // Crash recovery scan
    {
        let path = tmp.path().join("bench_recovery.shm");
        let ring = SharedRingBuffer::create(&path, 64, 256).unwrap();
        for i in 0..32 {
            ring.publish(i, CommandType::Insert, b"data").unwrap();
        }

        bench_fn("crash recovery scan (64 slots)", 10_000, || {
            std::hint::black_box(ring.recover_after_crash());
        });
    }
}

// ═══════════════════════════════════════════════════════════════════
// LSN Sequencer
// ═══════════════════════════════════════════════════════════════════

#[test]
#[ignore]
fn bench_lsn_sequencer() {
    use qm_engine::ipc::lsn::LsnSequencer;

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║           LSN SEQUENCER BENCHMARK                           ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    let seq = LsnSequencer::new(1);

    bench_fn("next() single allocation", 10_000_000, || {
        std::hint::black_box(seq.next());
    });

    bench_fn("next_batch(100)", 1_000_000, || {
        std::hint::black_box(seq.next_batch(100));
    });

    bench_fn("current_lsn() read", 10_000_000, || {
        std::hint::black_box(seq.current_lsn());
    });
}

// ═══════════════════════════════════════════════════════════════════
// W-TinyLFU Cache
// ═══════════════════════════════════════════════════════════════════

#[test]
#[ignore]
fn bench_wtinylfu_cache() {
    use qm_engine::storage::WTinyLfuCache;

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║           W-TinyLFU CACHE BENCHMARK                         ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // Insert throughput
    bench_fn("insert 1000 entries (64B each)", 10_000, || {
        let mut cache = WTinyLfuCache::<u64, Vec<u8>>::new(1_048_576);
        for i in 0..1000u64 {
            cache.insert(i, vec![0u8; 64], 64);
        }
        std::hint::black_box(cache.len());
    });

    // Get hot path (hit)
    {
        let mut cache = WTinyLfuCache::<u64, Vec<u8>>::new(1_048_576);
        for i in 0..1000u64 {
            cache.insert(i, vec![i as u8; 64], 64);
        }

        bench_fn("get() hot key (cache hit)", 1_000_000, || {
            std::hint::black_box(cache.get(&42));
        });
    }

    // Get miss path
    {
        let mut cache = WTinyLfuCache::<u64, Vec<u8>>::new(65536);
        for i in 0..100u64 {
            cache.insert(i, vec![0u8; 64], 64);
        }

        bench_fn("get() cold key (cache miss)", 1_000_000, || {
            std::hint::black_box(cache.get(&999999));
        });
    }

    // Mixed workload: 80% reads, 20% writes
    {
        let mut cache = WTinyLfuCache::<u64, Vec<u8>>::new(524288);
        for i in 0..500u64 {
            cache.insert(i, vec![0u8; 128], 128);
        }
        let mut counter = 0u64;

        bench_fn("mixed 80%R/20%W (512KB cache)", 1_000_000, || {
            counter += 1;
            if counter % 5 == 0 {
                cache.insert(counter % 2000, vec![0u8; 128], 128);
            } else {
                std::hint::black_box(cache.get(&(counter % 500)));
            }
        });

        println!("    → hit rate: {:.2}%", cache.hit_rate() * 100.0);
    }

    // Hit rate with Zipfian-like pattern
    {
        let mut cache = WTinyLfuCache::<u64, Vec<u8>>::new(131072);
        for i in 0..2000u64 {
            cache.insert(i, vec![0u8; 64], 64);
        }

        let start = Instant::now();
        let ops = 100_000u64;
        for i in 0..ops {
            let key = if i % 5 < 4 { i % 400 } else { 400 + (i % 1600) };
            if cache.get(&key).is_none() {
                cache.insert(key, vec![0u8; 64], 64);
            }
        }
        let elapsed = start.elapsed();
        println!(
            "  {:<45} {:>10.1} ns/op  hit_rate={:.1}%",
            "Zipfian workload (100K ops)",
            elapsed.as_nanos() as f64 / ops as f64,
            cache.hit_rate() * 100.0
        );
    }

    // ConcurrentCache
    {
        use qm_engine::storage::ConcurrentCache;
        let cache = ConcurrentCache::<u64, Vec<u8>>::new(1_048_576);
        for i in 0..1000u64 {
            cache.insert(i, vec![0u8; 64], 64);
        }
        let mut counter = 0u64;

        bench_fn("ConcurrentCache get/insert (RwLock)", 500_000, || {
            counter += 1;
            if counter % 3 == 0 {
                cache.insert(counter % 2000, vec![0u8; 64], 64);
            } else {
                std::hint::black_box(cache.get(&(counter % 1000)));
            }
        });
    }
}

// ═══════════════════════════════════════════════════════════════════
// JIT SQL Compiler
// ═══════════════════════════════════════════════════════════════════

#[test]
#[ignore]
fn bench_jit_compiler() {
    use qm_engine::executor::jit::*;

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║           JIT SQL EXPRESSION COMPILER BENCHMARK             ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // Simple equality interpretation
    {
        let expr = builder::eq_i64(0, 42);
        let row = RowAccessor {
            i64_cols: &[42, 10, 20],
            f64_cols: &[],
        };

        bench_fn("interpret: col0 = 42", 10_000_000, || {
            std::hint::black_box(interpret(&expr, &row));
        });
    }

    // Complex expression interpretation
    {
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

        bench_fn(
            "interpret: (c0>10 AND c1<100) AND BETWEEN",
            5_000_000,
            || {
                std::hint::black_box(interpret(&expr, &row));
            },
        );
    }

    // Batch filter: equality predicate
    for &n in &[1_000u64, 10_000, 100_000, 1_000_000] {
        let expr = builder::eq_i64(0, 42);
        let col: Vec<i64> = (0..n as usize)
            .map(|i| if i % 100 == 0 { 42 } else { i as i64 })
            .collect();

        bench_throughput(
            &format!("batch_filter eq ({})", format_count(n)),
            100,
            n,
            || {
                std::hint::black_box(batch_filter(&expr, &[&col], &[], col.len()));
            },
        );
    }

    // Batch filter: BETWEEN predicate
    for &n in &[1_000u64, 10_000, 100_000] {
        let expr = builder::between_f64(0, 10.0, 90.0);
        let col: Vec<f64> = (0..n as usize).map(|i| (i % 100) as f64).collect();

        bench_throughput(
            &format!("batch_filter BETWEEN ({})", format_count(n)),
            100,
            n,
            || {
                std::hint::black_box(batch_filter(&expr, &[], &[&col], col.len()));
            },
        );
    }

    // Batch project: col_a * col_b + 100
    for &n in &[1_000u64, 10_000, 100_000] {
        let expr = builder::compute_f64(0, 1, 100.0);
        let a: Vec<f64> = (0..n as usize).map(|i| i as f64).collect();
        let b: Vec<f64> = (0..n as usize).map(|i| (i * 2) as f64).collect();

        bench_throughput(
            &format!("batch_project f64*f64+100 ({})", format_count(n)),
            100,
            n,
            || {
                std::hint::black_box(batch_project(&expr, &[], &[&a, &b], a.len()));
            },
        );
    }

    // JitCache register+get
    {
        let cache = JitCache::new(100);
        bench_fn("JitCache: register + record + get_expr", 1_000_000, || {
            let expr = builder::eq_i64(0, 42);
            cache.register(12345, expr);
            cache.record_execution(12345);
            std::hint::black_box(cache.get_expr(12345));
        });
    }
}

// ═══════════════════════════════════════════════════════════════════
// io_uring WAL
// ═══════════════════════════════════════════════════════════════════

#[test]
#[ignore]
fn bench_uring_wal() {
    use qm_engine::storage::UringWalWriter;

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║           io_uring WAL BENCHMARK                            ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // Append 100B (small record)
    {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut wal = UringWalWriter::open(tmp.path(), false).unwrap();
        let data = vec![0xABu8; 100];

        bench_fn("append 100B (buffered, no flush)", 200_000, || {
            std::hint::black_box(wal.append(1, 1, &data).unwrap());
        });
    }

    // Append 4KB (page-sized)
    {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut wal = UringWalWriter::open(tmp.path(), false).unwrap();
        let data = vec![0xCDu8; 4096];

        bench_fn("append 4KB (buffered, no flush)", 100_000, || {
            std::hint::black_box(wal.append(1, 1, &data).unwrap());
        });
    }

    // Append+flush (durability)
    {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut wal = UringWalWriter::open(tmp.path(), false).unwrap();
        let data = vec![0xEFu8; 100];

        bench_fn("append 100B + flush (durability)", 10_000, || {
            wal.append(1, 1, &data).unwrap();
            wal.flush().unwrap();
        });
    }

    // Group commit: batch append then flush
    for &batch in &[32u64, 128, 512] {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut wal = UringWalWriter::open(tmp.path(), false).unwrap();
        let data = vec![0u8; 200];

        bench_throughput(
            &format!("group commit ({} records, 200B)", batch),
            100,
            batch,
            || {
                for i in 0..batch {
                    wal.append(i, 1, &data).unwrap();
                }
                wal.flush().unwrap();
            },
        );
    }

    // WAL byte throughput
    {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut wal = UringWalWriter::open(tmp.path(), false).unwrap();
        let data = vec![0u8; 4096]; // 4KB per record

        let start = Instant::now();
        let iters = 10_000;
        for i in 0..iters {
            wal.append(i, 1, &data).unwrap();
        }
        wal.flush().unwrap();
        let elapsed = start.elapsed();
        let total_mb = (iters * 4096) as f64 / (1024.0 * 1024.0);
        let mb_sec = total_mb / elapsed.as_secs_f64();
        println!(
            "  {:<45} {:>10.1} MB/s  ({:.1} MB total)",
            "WAL write throughput (4KB records)", mb_sec, total_mb
        );

        println!("    → total_bytes_written: {}", wal.total_bytes_written());
    }

    // Recovery benchmark
    {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut wal = UringWalWriter::open(tmp.path(), false).unwrap();
        let data = vec![0u8; 100];
        for i in 0..1000 {
            wal.append(i, 1, &data).unwrap();
        }
        wal.flush().unwrap();

        bench_fn("recover 1000 records", 1_000, || {
            std::hint::black_box(wal.recover().unwrap().len());
        });
    }
}

// ═══════════════════════════════════════════════════════════════════
// Native Hub Dispatcher
// ═══════════════════════════════════════════════════════════════════

#[test]
#[ignore]
fn bench_native_dispatcher() {
    use qm_engine::ipc::dispatcher::{DispatcherConfig, NativeDispatcher};
    use qm_engine::ipc::ring_buffer::SharedRingBuffer;

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║           NATIVE HUB DISPATCHER BENCHMARK                   ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    fn drain(ring: &SharedRingBuffer) {
        while let Some(c) = ring.consume() {
            ring.complete(c.slot_idx, None);
        }
    }

    // Single insert dispatch (manual loop with periodic drain)
    {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = DispatcherConfig {
            ring_dir: tmp.path().to_path_buf(),
            slot_count: 4096,
            slot_data_size: 4096,
            timeout_ms: 5000,
        };
        let dispatcher = NativeDispatcher::new(config).unwrap();
        let payload = b"INSERT INTO users VALUES (1, 'alice')";

        // warmup
        for _ in 0..100 {
            let _ = dispatcher.dispatch_insert(payload);
        }
        drain(dispatcher.ring_general());

        let iters = 100_000u64;
        let start = Instant::now();
        for i in 0..iters {
            let _ = dispatcher.dispatch_insert(payload);
            if i % 2000 == 1999 {
                drain(dispatcher.ring_general());
            }
        }
        drain(dispatcher.ring_general());
        let elapsed = start.elapsed();
        let per_op_ns = elapsed.as_nanos() as f64 / iters as f64;
        let ops_sec = iters as f64 / elapsed.as_secs_f64();
        println!(
            "  {:<50} {:>10.1} ns/op  {:>12.0} ops/sec",
            "dispatch_insert (single)", per_op_ns, ops_sec
        );
    }

    // Vector op dispatch
    {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = DispatcherConfig {
            ring_dir: tmp.path().to_path_buf(),
            slot_count: 4096,
            slot_data_size: 4096,
            timeout_ms: 5000,
        };
        let dispatcher = NativeDispatcher::new(config).unwrap();
        let payload = b"VECTOR SEARCH dim=128 k=10";

        for _ in 0..100 {
            let _ = dispatcher.dispatch_vector_op(payload);
        }
        drain(dispatcher.ring_vector());

        let iters = 100_000u64;
        let start = Instant::now();
        for i in 0..iters {
            let _ = dispatcher.dispatch_vector_op(payload);
            if i % 2000 == 1999 {
                drain(dispatcher.ring_vector());
            }
        }
        drain(dispatcher.ring_vector());
        let elapsed = start.elapsed();
        let per_op_ns = elapsed.as_nanos() as f64 / iters as f64;
        let ops_sec = iters as f64 / elapsed.as_secs_f64();
        println!(
            "  {:<50} {:>10.1} ns/op  {:>12.0} ops/sec",
            "dispatch_vector_op (single)", per_op_ns, ops_sec
        );
    }

    // Mixed dispatch workload
    {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = DispatcherConfig {
            ring_dir: tmp.path().to_path_buf(),
            slot_count: 8192,
            slot_data_size: 4096,
            timeout_ms: 5000,
        };
        let dispatcher = NativeDispatcher::new(config).unwrap();

        let iters = 100u64;
        // warmup
        for _ in 0..5 {
            for _ in 0..40 {
                let _ = dispatcher.dispatch_insert(b"INSERT data");
            }
            for _ in 0..40 {
                let _ = dispatcher.dispatch_query(b"SELECT * FROM t");
            }
            for _ in 0..20 {
                let _ = dispatcher.dispatch_vector_op(b"VECTOR SEARCH");
            }
            drain(dispatcher.ring_general());
            drain(dispatcher.ring_vector());
        }

        let start = Instant::now();
        for _ in 0..iters {
            for _ in 0..40 {
                let _ = dispatcher.dispatch_insert(b"INSERT data");
            }
            for _ in 0..40 {
                let _ = dispatcher.dispatch_query(b"SELECT * FROM t");
            }
            for _ in 0..20 {
                let _ = dispatcher.dispatch_vector_op(b"VECTOR SEARCH");
            }
            drain(dispatcher.ring_general());
            drain(dispatcher.ring_vector());
        }
        let elapsed = start.elapsed();
        let total_ops = iters * 100;
        let per_op_ns = elapsed.as_nanos() as f64 / total_ops as f64;
        let ops_sec = total_ops as f64 / elapsed.as_secs_f64();
        println!(
            "  {:<50} {:>10.1} ns/op  {:>12.0} elem/sec",
            "mixed dispatch (40I+40Q+20V)", per_op_ns, ops_sec
        );
    }

    // LSN ordering verification
    {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = DispatcherConfig {
            ring_dir: tmp.path().to_path_buf(),
            slot_count: 4096,
            slot_data_size: 4096,
            timeout_ms: 5000,
        };
        let dispatcher = NativeDispatcher::new(config).unwrap();

        let start = Instant::now();
        let n = 10_000;
        let mut prev_lsn = 0u64;
        for i in 0..n {
            if let Ok(r) = dispatcher.dispatch_insert(b"data") {
                assert!(r.lsn > prev_lsn, "LSN ordering violated");
                prev_lsn = r.lsn;
            }
            if i % 2000 == 0 {
                drain(dispatcher.ring_general());
            }
        }
        let elapsed = start.elapsed();
        println!(
            "  {:<45} ✓ monotonic ({} ops in {:.3}s)",
            "LSN monotonicity verification",
            n,
            elapsed.as_secs_f64()
        );
    }
}

// ═══════════════════════════════════════════════════════════════════
// Sharded Cache Scalability
// ═══════════════════════════════════════════════════════════════════

#[test]
#[ignore]
fn bench_sharded_cache_scalability() {
    use qm_engine::storage::{ConcurrentCache, WTinyLfuCache};
    use std::sync::Arc;

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║     SHARDED CACHE SCALABILITY BENCHMARK (64 shards)         ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // Single-thread: WTinyLfuCache baseline
    {
        let mut cache = WTinyLfuCache::<u64, Vec<u8>>::new(1_048_576);
        for i in 0..5000u64 {
            cache.insert(i, vec![0u8; 64], 64);
        }
        let mut counter = 0u64;

        let (_, base_ns, _) = bench_fn("WTinyLfuCache single-thread (baseline)", 1_000_000, || {
            counter += 1;
            if counter % 5 == 0 {
                cache.insert(counter % 10000, vec![0u8; 64], 64);
            } else {
                std::hint::black_box(cache.get(&(counter % 5000)));
            }
        });

        // Single-thread: ConcurrentCache
        let cc = ConcurrentCache::<u64, Vec<u8>>::new(1_048_576);
        for i in 0..5000u64 {
            cc.insert(i, vec![0u8; 64], 64);
        }
        counter = 0;

        let (_, shard_ns, _) = bench_fn(
            "ConcurrentCache single-thread (64 shards)",
            1_000_000,
            || {
                counter += 1;
                if counter % 5 == 0 {
                    cc.insert(counter % 10000, vec![0u8; 64], 64);
                } else {
                    std::hint::black_box(cc.get(&(counter % 5000)));
                }
            },
        );

        println!(
            "    → shard overhead: {:.1}× vs baseline\n",
            shard_ns / base_ns
        );
    }

    // Multi-thread contention test with ConcurrentCache
    for &threads in &[2u32, 4, 8] {
        let cache = Arc::new(ConcurrentCache::<u64, Vec<u8>>::new(2_097_152));
        for i in 0..10000u64 {
            cache.insert(i, vec![0u8; 64], 64);
        }

        let ops_per_thread = 200_000u64;
        let start = Instant::now();
        std::thread::scope(|s| {
            for t in 0..threads {
                let cache = Arc::clone(&cache);
                s.spawn(move || {
                    for i in 0..ops_per_thread {
                        let key = (t as u64) * 100_000 + i;
                        if i % 5 == 0 {
                            cache.insert(key % 20000, vec![0u8; 64], 64);
                        } else {
                            std::hint::black_box(cache.get(&(key % 10000)));
                        }
                    }
                });
            }
        });
        let elapsed = start.elapsed();
        let total_ops = threads as u64 * ops_per_thread;
        let per_op_ns = elapsed.as_nanos() as f64 / total_ops as f64;
        let ops_sec = total_ops as f64 / elapsed.as_secs_f64();
        println!("  ConcurrentCache {:>2} threads (80R/20W)              {:>10.1} ns/op  {:>12.0} ops/sec",
            threads, per_op_ns, ops_sec);
    }
    println!();
}

// ═══════════════════════════════════════════════════════════════════
// Aggregate Executor
// ═══════════════════════════════════════════════════════════════════

#[test]
#[ignore]
fn bench_aggregate_executor() {
    use ahash::AHashMap;
    use qm_engine::executor::agg::*;

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║           AGGREGATE EXECUTOR BENCHMARK                      ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // Build rows: 100K rows, 100 groups
    let build_rows = |n: usize, groups: usize| -> Vec<AHashMap<String, AggValue>> {
        (0..n)
            .map(|i| {
                let mut row = AHashMap::new();
                row.insert(
                    "category".into(),
                    AggValue::Text(format!("cat_{}", i % groups)),
                );
                row.insert("amount".into(), AggValue::Float(i as f64 * 1.5));
                row.insert("count".into(), AggValue::Int(1));
                row
            })
            .collect()
    };

    // GROUP BY + COUNT + SUM + AVG on 10K rows, 50 groups
    {
        let rows = build_rows(10_000, 50);
        let specs = vec![
            AggSpec {
                func: AggFunction::Count,
                column: None,
                alias: "cnt".into(),
            },
            AggSpec {
                func: AggFunction::Sum,
                column: Some("amount".into()),
                alias: "total".into(),
            },
            AggSpec {
                func: AggFunction::Avg,
                column: Some("amount".into()),
                alias: "avg_amt".into(),
            },
        ];

        bench_fn("GROUP BY 50 groups (10K rows, 3 aggs)", 1_000, || {
            let executor = AggregateExecutor::new(vec!["category".into()], specs.clone());
            let result = executor.execute(&rows);
            std::hint::black_box(result);
        });
    }

    // GROUP BY on 100K rows, 100 groups
    {
        let rows = build_rows(100_000, 100);
        let specs = vec![
            AggSpec {
                func: AggFunction::Count,
                column: None,
                alias: "cnt".into(),
            },
            AggSpec {
                func: AggFunction::Sum,
                column: Some("amount".into()),
                alias: "total".into(),
            },
        ];

        bench_fn("GROUP BY 100 groups (100K rows, 2 aggs)", 100, || {
            let executor = AggregateExecutor::new(vec!["category".into()], specs.clone());
            let result = executor.execute(&rows);
            std::hint::black_box(result);
        });
    }

    // HAVING filter on pre-grouped result
    {
        let rows = build_rows(10_000, 50);
        let specs = vec![
            AggSpec {
                func: AggFunction::Count,
                column: None,
                alias: "cnt".into(),
            },
            AggSpec {
                func: AggFunction::Sum,
                column: Some("amount".into()),
                alias: "total".into(),
            },
        ];
        let executor = AggregateExecutor::new(vec!["category".into()], specs);
        let (col_names, grouped) = executor.execute(&rows);
        let having = vec![HavingPredicate::Gt("cnt".into(), 100.0)];

        bench_fn("HAVING filter (50 groups → filtered)", 10_000, || {
            let filtered = apply_having(&col_names, grouped.clone(), &having);
            std::hint::black_box(filtered);
        });
    }

    // Sort-merge join
    {
        let left: Vec<(String, Vec<String>)> = (0..1000)
            .map(|i| (format!("k_{:04}", i), vec![format!("lv_{}", i)]))
            .collect();
        let right: Vec<(String, Vec<String>)> = (0..1000)
            .map(|i| (format!("k_{:04}", i), vec![format!("rv_{}", i)]))
            .collect();

        bench_fn("sort_merge_join (1K×1K, full match)", 5_000, || {
            std::hint::black_box(sort_merge_join(&left, &right));
        });

        // Partial overlap (50%)
        let right_half: Vec<(String, Vec<String>)> = (500..1500)
            .map(|i| (format!("k_{:04}", i), vec![format!("rv_{}", i)]))
            .collect();

        bench_fn("sort_merge_join (1K×1K, 50% overlap)", 5_000, || {
            std::hint::black_box(sort_merge_join(&left, &right_half));
        });
    }
}

// ═══════════════════════════════════════════════════════════════════
// Zero-Copy IPC Types
// ═══════════════════════════════════════════════════════════════════

#[test]
#[ignore]
fn bench_zero_copy_types() {
    use qm_engine::types::*;

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║           ZERO-COPY IPC TYPES BENCHMARK                     ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // ZeroCopyBuffer creation from f32 vector
    for &n in &[1_000usize, 10_000, 100_000] {
        let data: Vec<f32> = (0..n).map(|i| i as f32).collect();

        bench_fn(
            &format!("ZeroCopyBuffer::from_f32_vec ({})", format_count(n as u64)),
            10_000,
            || {
                std::hint::black_box(ZeroCopyBuffer::from_f32_vec(&data));
            },
        );
    }

    // Zero-copy slice (no allocation)
    {
        let data: Vec<f32> = (0..100_000).map(|i| i as f32).collect();
        let buf = ZeroCopyBuffer::from_f32_vec(&data);

        bench_fn("slice (10K from 100K buf)", 1_000_000, || {
            std::hint::black_box(buf.slice(1000, 10000));
        });
    }

    // as_f32_slice read
    {
        let data: Vec<f32> = (0..100_000).map(|i| i as f32).collect();
        let buf = ZeroCopyBuffer::from_f32_vec(&data);

        bench_fn("as_f32_slice (100K elements)", 1_000_000, || {
            let s = buf.as_f32_slice();
            std::hint::black_box(s[50_000]);
        });
    }

    // IpcSchema encode/decode roundtrip
    {
        let fields: Vec<IpcField> = (0..20)
            .map(|i| IpcField {
                name: format!("col_{}", i),
                type_id: if i % 2 == 0 {
                    IpcTypeId::Float64
                } else {
                    IpcTypeId::Int64
                },
                nullable: i % 3 == 0,
            })
            .collect();
        let schema = IpcSchema::new(fields);

        bench_fn("IpcSchema encode+decode (20 fields)", 100_000, || {
            let encoded = schema.encode();
            std::hint::black_box(IpcSchema::decode(&encoded).unwrap());
        });
    }

    // SharedColumn clone + access
    {
        let data: Vec<f32> = (0..10_000).map(|i| i as f32).collect();
        let buf = ZeroCopyBuffer::from_f32_vec(&data);
        let col = SharedColumn::new("test_col".into(), IpcTypeId::Float32, buf);

        bench_fn("SharedColumn clone + as_f32_slice", 1_000_000, || {
            let c2 = col.clone();
            let s = c2.buffer.as_f32_slice();
            std::hint::black_box(s[5000]);
        });
    }
}

// ═══════════════════════════════════════════════════════════════════
// MVCC Insert Latency (Phase 7)
// ═══════════════════════════════════════════════════════════════════

#[test]
#[ignore]
fn bench_mvcc_insert_latency() {
    use qm_engine::executor::txn::MvccStore;
    use std::sync::Arc;

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║              MVCC INSERT LATENCY BENCHMARK                  ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // ── Scenario 1: Single-txn put latency (no contention) ──
    println!("  ── Scenario 1: Single-txn put latency ──");
    for &val_size in &[64usize, 256, 1024, 4096] {
        let store = MvccStore::new();
        let value = vec![0xABu8; val_size];
        let iters = 50_000u64;

        bench_fn(
            &format!("  begin+put+commit (val={}B)", val_size),
            iters,
            || {
                let txn = store.begin();
                store.put(txn, vec![0u8; 16], value.clone()).unwrap();
                store.commit(txn).unwrap();
            },
        );
    }

    // ── Scenario 2: Multi-key transaction (batch insert) ──
    println!("\n  ── Scenario 2: Batch insert per transaction ──");
    for &batch_size in &[1u64, 10, 100, 1000] {
        let store = MvccStore::new();
        let value = vec![0xCDu8; 128];
        let iters = 10_000u64 / batch_size.max(1);

        bench_fn(
            &format!("  txn with {} puts + commit", batch_size),
            iters.max(100),
            || {
                let txn = store.begin();
                for j in 0..batch_size {
                    let key = format!("key_{}", j).into_bytes();
                    store.put(txn, key, value.clone()).unwrap();
                }
                store.commit(txn).unwrap();
            },
        );
    }

    // ── Scenario 3: Multi-version overhead (same key, many versions) ──
    println!("\n  ── Scenario 3: Multi-version overhead ──");
    for &versions in &[10u64, 100, 1000] {
        let store = MvccStore::new();
        let key = b"hot_key".to_vec();
        let value = vec![0xEFu8; 128];

        // Pre-populate with `versions` committed versions
        for _ in 0..versions {
            let txn = store.begin();
            store.put(txn, key.clone(), value.clone()).unwrap();
            store.commit(txn).unwrap();
        }

        // Now measure insert latency on top of existing versions
        let iters = 20_000u64;
        bench_fn(
            &format!("  put+commit on key with {} versions", versions),
            iters,
            || {
                let txn = store.begin();
                store.put(txn, key.clone(), value.clone()).unwrap();
                store.commit(txn).unwrap();
            },
        );

        // Also measure read latency with many versions
        let txn_r = store.begin();
        bench_fn(
            &format!("  get on key with {} versions", versions),
            iters,
            || {
                std::hint::black_box(store.get(txn_r, &key).unwrap());
            },
        );
        store.abort(txn_r).unwrap();
    }

    // ── Scenario 4: GC impact ──
    println!("\n  ── Scenario 4: GC impact on read latency ──");
    {
        let store = MvccStore::new();
        let key = b"gc_key".to_vec();
        let value = vec![0x42u8; 128];

        // Create 5000 versions
        for _ in 0..5000 {
            let txn = store.begin();
            store.put(txn, key.clone(), value.clone()).unwrap();
            store.commit(txn).unwrap();
        }

        // Read latency BEFORE GC
        let iters = 20_000u64;
        let txn_pre = store.begin();
        let (_, pre_gc_ns, _) = bench_fn("  get (5000 versions, before GC)", iters, || {
            std::hint::black_box(store.get(txn_pre, &key).unwrap());
        });
        store.abort(txn_pre).unwrap();

        // Run GC
        let gc_start = Instant::now();
        store.gc();
        let gc_elapsed = gc_start.elapsed();
        println!("  GC took: {:.2?}", gc_elapsed);

        // Read latency AFTER GC
        let txn_post = store.begin();
        let (_, post_gc_ns, _) = bench_fn("  get (after GC)", iters, || {
            std::hint::black_box(store.get(txn_post, &key).unwrap());
        });
        store.abort(txn_post).unwrap();

        let speedup = pre_gc_ns / post_gc_ns;
        println!("  GC speedup on read: {:.1}x", speedup);
    }

    // ── Scenario 5: Concurrent transaction scaling ──
    println!("\n  ── Scenario 5: Concurrent active transactions ──");
    for &active_txns in &[1u64, 10, 100, 1000] {
        let store = Arc::new(MvccStore::new());
        let value = vec![0xBBu8; 128];

        // Hold `active_txns` open transactions
        let mut held: Vec<u64> = Vec::new();
        for i in 0..active_txns {
            let txn = store.begin();
            store
                .put(txn, format!("held_{}", i).into_bytes(), value.clone())
                .unwrap();
            held.push(txn);
        }

        let iters = 20_000u64;
        bench_fn(
            &format!("  begin+put+commit ({} active txns)", active_txns),
            iters,
            || {
                let txn = store.begin();
                store.put(txn, b"new_key".to_vec(), value.clone()).unwrap();
                store.commit(txn).unwrap();
            },
        );

        // Clean up held txns
        for txn in held {
            let _ = store.abort(txn);
        }
    }

    println!("\n  ════════════════════════════════════════════════");
    println!("  Key insights:");
    println!("  • MVCC overhead scales with version chain length");
    println!("  • GC reclaims dead versions → faster reads");
    println!("  • Snapshot isolation: begin() cost grows with active txn count");
    println!("  • Batch inserts amortize txn overhead per-key");
}

// ═══════════════════════════════════════════════════════════════════
// Benchmark 10: Consistent Hash Ring + Shard Routing
// ═══════════════════════════════════════════════════════════════════

#[test]
#[ignore]
fn bench_consistent_hash_routing() {
    use qm_engine::cluster::ShardManager;

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║  Benchmark: Consistent Hash Ring + Shard Routing            ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // Setup: 8 shards, 256 vnodes each, replication factor 3
    let mgr = ShardManager::new(8, 256, 3);

    let n_routes = 1_000_000u64;

    // Single routing
    let (_, ns_per, ops) = bench_fn("route() single key", n_routes, || {
        std::hint::black_box(mgr.route(12345));
    });

    // Batch routing
    let keys: Vec<i64> = (0..1000).collect();
    let (_, ns_batch, _batch_ops) = bench_fn("route_batch() 1000 keys", n_routes / 1000, || {
        std::hint::black_box(mgr.route_batch(&keys));
    });

    // Distribution check
    let stats = mgr.balance_stats();
    println!(
        "\n  Distribution stats: min={}, max={}, stddev={:.1}",
        stats.0, stats.1, stats.2
    );

    // Add shard during routing
    let add_start = Instant::now();
    mgr.add_shard(8);
    let add_elapsed = add_start.elapsed();
    println!("  Add shard: {:.2?}", add_elapsed);

    println!("\n  Summary:");
    println!(
        "  • Route: {:.0} ns/key, {:.0} M routes/sec",
        ns_per,
        ops / 1_000_000.0
    );
    println!("  • Batch route 1K keys: {:.0} ns", ns_batch);
}

// ═══════════════════════════════════════════════════════════════════
// Benchmark 11: Hybrid Search (RRF + Weighted Linear + DBSF)
// ═══════════════════════════════════════════════════════════════════

#[test]
#[ignore]
fn bench_hybrid_search() {
    use qm_engine::executor::hybrid_search::*;

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║  Benchmark: Hybrid Search (BM25 + Vector Fusion)            ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // Generate synthetic results
    let n_bm25 = 500;
    let n_vec = 500;
    let bm25_results: Vec<ScoredDoc> = (0..n_bm25)
        .map(|i| ScoredDoc {
            id: i,
            score: 50.0 - (i as f64 * 0.1),
        })
        .collect();
    let vector_results: Vec<ScoredDoc> = (0..n_vec)
        .map(|i| ScoredDoc {
            id: i * 2,
            score: (i as f64) * 0.002,
        })
        .collect();

    let iters = 10_000u64;

    // RRF
    let (_, rrf_ns, rrf_ops) = bench_fn("RRF (500+500 → top 10)", iters, || {
        std::hint::black_box(reciprocal_rank_fusion(
            &[bm25_results.clone(), vector_results.clone()],
            60.0,
            10,
        ));
    });

    // Weighted Linear
    let (_, wl_ns, wl_ops) = bench_fn("Weighted Linear α=0.5 (500+500 → top 10)", iters, || {
        std::hint::black_box(weighted_linear_fusion(
            &bm25_results,
            &vector_results,
            0.5,
            10,
        ));
    });

    // DBSF
    let (_, dbsf_ns, dbsf_ops) = bench_fn("DBSF z-score (500+500 → top 10)", iters, || {
        std::hint::black_box(distribution_based_fusion(
            &bm25_results,
            &vector_results,
            10,
        ));
    });

    // Full pipeline with reranker
    let mut embeddings = ahash::AHashMap::new();
    for i in 0..1000i64 {
        embeddings.insert(i, vec![0.5; 8]);
    }
    let reranker = DotProductReranker::new(embeddings);
    let config = HybridSearchConfig {
        final_top_k: 10,
        rerank: true,
        ..Default::default()
    };

    let (_, pipe_ns, pipe_ops) = bench_fn("Full pipeline + reranker", iters, || {
        std::hint::black_box(hybrid_search(
            &bm25_results,
            &vector_results,
            &config,
            Some(&reranker),
            "test query",
        ));
    });

    println!("\n  Summary:");
    println!(
        "  • RRF: {:.0} ns, {:.0}K ops/sec",
        rrf_ns,
        rrf_ops / 1000.0
    );
    println!(
        "  • Weighted: {:.0} ns, {:.0}K ops/sec",
        wl_ns,
        wl_ops / 1000.0
    );
    println!(
        "  • DBSF: {:.0} ns, {:.0}K ops/sec",
        dbsf_ns,
        dbsf_ops / 1000.0
    );
    println!(
        "  • Full pipeline: {:.0} ns, {:.0}K ops/sec",
        pipe_ns,
        pipe_ops / 1000.0
    );
}

// ═══════════════════════════════════════════════════════════════════
// Benchmark 12: Incremental Snapshots
// ═══════════════════════════════════════════════════════════════════

#[test]
#[ignore]
fn bench_incremental_snapshots() {
    use qm_engine::storage::snapshot::*;
    use std::collections::{BTreeMap, BTreeSet};

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║  Benchmark: Incremental Snapshots                           ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    struct BenchPageProvider {
        pages: BTreeMap<u64, Vec<u8>>,
    }
    impl PageProvider for BenchPageProvider {
        fn read_page(&self, page_id: PageId) -> Option<Vec<u8>> {
            self.pages.get(&page_id).cloned()
        }
    }

    let dir = std::env::temp_dir().join("bench_snapshot");
    let _ = std::fs::remove_dir_all(&dir);

    let page_size = 8192usize;
    let n_pages = 1000u64;

    // Create provider with 1000 pages
    let mut pages = BTreeMap::new();
    for i in 0..n_pages {
        pages.insert(i, vec![(i & 0xFF) as u8; page_size]);
    }
    let provider = BenchPageProvider { pages };

    // Write snapshot with all pages
    let writer = SnapshotWriter::new(&dir).unwrap();
    let dirty_all: BTreeSet<u64> = (0..n_pages).collect();

    let write_start = Instant::now();
    let snap_path = writer.write_snapshot(&dirty_all, 1, &provider).unwrap();
    let write_elapsed = write_start.elapsed();
    let snap_size = std::fs::metadata(&snap_path).unwrap().len();
    let throughput_mb = (snap_size as f64 / 1024.0 / 1024.0) / write_elapsed.as_secs_f64();
    println!(
        "  Write full snapshot ({} pages × {}B): {:.2?} ({:.0} MB/s)",
        n_pages, page_size, write_elapsed, throughput_mb
    );
    println!("  Snapshot size: {} KB", snap_size / 1024);

    // Read snapshot back
    let read_start = Instant::now();
    let recovered = SnapshotReader::read_snapshot(&snap_path).unwrap();
    let read_elapsed = read_start.elapsed();
    let read_throughput = (snap_size as f64 / 1024.0 / 1024.0) / read_elapsed.as_secs_f64();
    println!(
        "  Read snapshot: {:.2?} ({:.0} MB/s), {} pages recovered",
        read_elapsed,
        read_throughput,
        recovered.len()
    );

    // Incremental: only 10% dirty pages
    let dirty_10pct: BTreeSet<u64> = (0..n_pages / 10).collect();
    let incr_start = Instant::now();
    let _incr_path = writer.write_snapshot(&dirty_10pct, 2, &provider).unwrap();
    let incr_elapsed = incr_start.elapsed();
    println!(
        "  Incremental snapshot (10% = {} pages): {:.2?}",
        n_pages / 10,
        incr_elapsed
    );

    // Dirty tracker performance
    let tracker = DirtyTracker::new();
    let track_iters = 1_000_000u64;
    let track_start = Instant::now();
    for i in 0..track_iters {
        tracker.mark_dirty(i % n_pages);
    }
    let track_elapsed = track_start.elapsed();
    let ns_per_mark = track_elapsed.as_nanos() as f64 / track_iters as f64;
    println!(
        "  DirtyTracker mark_dirty: {:.0} ns/op ({:.0}M ops/sec)",
        ns_per_mark,
        track_iters as f64 / track_elapsed.as_secs_f64() / 1_000_000.0
    );

    let _ = std::fs::remove_dir_all(&dir);

    println!("\n  Summary:");
    println!("  • Full snapshot write: {:.0} MB/s", throughput_mb);
    println!("  • Snapshot read: {:.0} MB/s", read_throughput);
    println!("  • Incremental (10%): {:.2?}", incr_elapsed);
}

// ═══════════════════════════════════════════════════════════════════
// Benchmark 13: Prometheus Metrics
// ═══════════════════════════════════════════════════════════════════

#[test]
#[ignore]
fn bench_prometheus_metrics() {
    use qm_engine::metrics::MetricsRegistry;

    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║  Benchmark: Prometheus Metrics Collection                   ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    let m = MetricsRegistry::new();
    let iters = 5_000_000u64;

    // Counter increment
    let (_, cnt_ns, _cnt_ops) = bench_fn("Counter.inc()", iters, || {
        m.queries_total.inc();
    });

    // Gauge set
    let (_, gauge_ns, _gauge_ops) = bench_fn("Gauge.set()", iters, || {
        m.active_txns.set(42.0);
    });

    // Histogram observe
    let (_, hist_ns, _hist_ops) = bench_fn("Histogram.observe()", iters, || {
        m.query_latency.observe(0.015);
    });

    // Full render
    let render_iters = 10_000u64;
    let (_, render_ns, render_ops) = bench_fn("MetricsRegistry.render()", render_iters, || {
        std::hint::black_box(m.render());
    });

    // Concurrent counter (8 threads)
    let m_ref = &m;
    let conc_start = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                for _ in 0..iters / 8 {
                    m_ref.inserts_total.inc();
                }
            });
        }
    });
    let conc_elapsed = conc_start.elapsed();
    let conc_ns = conc_elapsed.as_nanos() as f64 / iters as f64;
    println!(
        "  Concurrent Counter.inc() (8 threads): {:.1} ns/op",
        conc_ns
    );

    println!("\n  Summary:");
    println!("  • Counter: {:.1} ns/op", cnt_ns);
    println!("  • Gauge: {:.1} ns/op", gauge_ns);
    println!("  • Histogram: {:.1} ns/op", hist_ns);
    println!(
        "  • Render: {:.0} ns ({:.0} renders/sec)",
        render_ns, render_ops
    );
}

// ═══════════════════════════════════════════════════════════════════
// Combined Summary
// ═══════════════════════════════════════════════════════════════════

#[test]
#[ignore]
fn bench_all_summary() {
    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║           QMvir ENGINE - FULL BENCHMARK SUMMARY             ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Run individual benchmarks:                                 ║");
    println!("║    cargo test --release bench_ring_buffer     -- --ignored   ║");
    println!("║    cargo test --release bench_lsn_sequencer   -- --ignored   ║");
    println!("║    cargo test --release bench_wtinylfu_cache  -- --ignored   ║");
    println!("║    cargo test --release bench_jit_compiler    -- --ignored   ║");
    println!("║    cargo test --release bench_uring_wal       -- --ignored   ║");
    println!("║    cargo test --release bench_native_dispatch -- --ignored   ║");
    println!("║    cargo test --release bench_sharded_cache   -- --ignored   ║");
    println!("║    cargo test --release bench_aggregate       -- --ignored   ║");
    println!("║    cargo test --release bench_zero_copy       -- --ignored   ║");
    println!("║    cargo test --release bench_mvcc            -- --ignored   ║");
    println!("║    cargo test --release bench_consistent_hash -- --ignored   ║");
    println!("║    cargo test --release bench_hybrid_search   -- --ignored   ║");
    println!("║    cargo test --release bench_incremental     -- --ignored   ║");
    println!("║    cargo test --release bench_prometheus      -- --ignored   ║");
    println!("║                                                             ║");
    println!("║  Run ALL benchmarks:                                        ║");
    println!("║    cargo test --release bench_ -- --nocapture --ignored      ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000 {
        format!("{}K", n / 1_000)
    } else {
        format!("{}", n)
    }
}
