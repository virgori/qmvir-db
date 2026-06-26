//! QMvir CLI — `qm` binary entry point.
//!
//! Usage:
//!   qm start --admin-password mypass           # daemon (default)
//!   qm start --foreground --admin-password pw  # foreground
//!   qm sql "SELECT * FROM users" --data-dir ./data
//!   qm backup -o backup.qmvb --data-dir ./data
//!   qm stop

use clap::Parser;
use std::path::PathBuf;
use std::time::Instant;

use qm_engine::cli::i18n::Lang;
use qm_engine::cli::{Cli, Commands, SchemaAction};
use qm_engine::NativeSqlEngine;

fn load_engine(data_dir: &PathBuf, lang: &Lang) -> NativeSqlEngine {
    let engine = NativeSqlEngine::with_data_dir(data_dir.clone());
    if !data_dir.exists() {
        eprintln!(
            "{}",
            lang.msg("err_no_data_dir", &data_dir.display().to_string())
        );
    }
    engine
}

fn smart_error(lang: &Lang, e: &str) {
    eprintln!("{}: {e}", lang.msg("error", ""));
    let eu = e.to_uppercase();
    if eu.contains("TABLE") && eu.contains("NOT FOUND") {
        eprintln!("  {} CREATE TABLE <name> (...);", lang.msg("hint_try", ""));
        eprintln!("  {} qm inspect --tables", lang.msg("hint_list", ""));
    } else if eu.contains("SYNTAX") || eu.contains("PARSE") {
        eprintln!("  {} qm guide quickstart", lang.msg("hint_syntax", ""));
    } else if eu.contains("PERMISSION") || eu.contains("AUTH") || eu.contains("DENIED") {
        eprintln!("  {} --admin-password <pw>", lang.msg("hint_auth", ""));
    } else if eu.contains("CONNECT") || eu.contains("REFUSED") {
        eprintln!(
            "  {} qm start --admin-password <pw>",
            lang.msg("hint_start", "")
        );
        eprintln!("  {} qm status", lang.msg("hint_status", ""));
    }
}

fn run_benchtest(profile: &str, json: bool) {
    use qm_engine::executor::jit::{self, JitExpr};
    use qm_engine::index::{BPlusTree, HnswConfig, HnswIndex, IndexKey, RoaringBitmap};
    use qm_engine::ipc::lsn::LsnSequencer;
    use qm_engine::ipc::ring_buffer::{CommandType, SharedRingBuffer};
    use qm_engine::statistics::{BloomFilter, HyperLogLog};
    use qm_engine::storage::WTinyLfuCache;

    let iterations: u64 = match profile {
        "standard" => 100_000,
        _ => 10_000, // quick
    };

    struct BenchResult {
        name: &'static str,
        ops: u64,
        elapsed_us: u64,
    }
    impl BenchResult {
        fn ops_per_sec(&self) -> f64 {
            if self.elapsed_us == 0 {
                return 0.0;
            }
            self.ops as f64 / (self.elapsed_us as f64 / 1_000_000.0)
        }
    }

    let mut results: Vec<BenchResult> = Vec::new();

    if !json {
        println!("QMvir Benchmark Suite v6.0.0");
        println!("Profile: {profile}  Iterations: {iterations}");
        println!("{}", "=".repeat(70));
    }

    // ── 1. Ring Buffer IPC ──
    {
        let tmp_dir = std::env::temp_dir().join(format!("qm_bench_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp_dir);
        let path = tmp_dir.join("bench_ring.shm");
        let ring = SharedRingBuffer::create(&path, 4096, 4096).unwrap();
        let payload = vec![0xABu8; 1024];
        let t = Instant::now();
        let mut ok_count = 0u64;
        for _ in 0..iterations {
            if let Ok(_seq) = ring.publish(1, CommandType::Insert, &payload) {
                ok_count += 1;
            }
            if let Some(consumed) = ring.consume() {
                ring.complete(consumed.slot_idx, None);
            }
        }
        let elapsed = t.elapsed().as_micros() as u64;
        results.push(BenchResult {
            name: "Ring Buffer IPC (1KB publish+consume)",
            ops: ok_count,
            elapsed_us: elapsed,
        });
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    // ── 2. LSN Sequencer ──
    {
        let seq = LsnSequencer::new(1);
        let t = Instant::now();
        for _ in 0..iterations * 10 {
            let _ = seq.next();
        }
        let elapsed = t.elapsed().as_micros() as u64;
        results.push(BenchResult {
            name: "LSN Sequencer (next)",
            ops: iterations * 10,
            elapsed_us: elapsed,
        });
    }

    // ── 3. W-TinyLFU Cache ──
    {
        let mut cache = WTinyLfuCache::<u64, Vec<u8>>::new(1_048_576);
        let t = Instant::now();
        for i in 0..iterations {
            cache.insert(i, vec![0u8; 64], 64);
        }
        let elapsed_insert = t.elapsed().as_micros() as u64;
        results.push(BenchResult {
            name: "Cache insert (64B values)",
            ops: iterations,
            elapsed_us: elapsed_insert,
        });

        let t = Instant::now();
        let reads = iterations * 5;
        for i in 0..reads {
            let _ = cache.get(&(i % iterations));
        }
        let elapsed_get = t.elapsed().as_micros() as u64;
        results.push(BenchResult {
            name: "Cache get (mixed hit/miss)",
            ops: reads,
            elapsed_us: elapsed_get,
        });
    }

    // ── 4. B+Tree Index ──
    {
        let btree = BPlusTree::new("bench_idx".into(), "bench_t".into(), vec!["id".into()]);
        let t = Instant::now();
        for i in 0..iterations {
            btree.insert(IndexKey::Integer(i as i64), i as i64);
        }
        let elapsed_insert = t.elapsed().as_micros() as u64;
        results.push(BenchResult {
            name: "B+Tree insert",
            ops: iterations,
            elapsed_us: elapsed_insert,
        });

        let t = Instant::now();
        for i in 0..iterations {
            let _ = btree.search(&IndexKey::Integer(i as i64));
        }
        let elapsed_search = t.elapsed().as_micros() as u64;
        results.push(BenchResult {
            name: "B+Tree point lookup",
            ops: iterations,
            elapsed_us: elapsed_search,
        });
    }

    // ── 5. Roaring Bitmap ──
    {
        let mut bm = RoaringBitmap::new();
        let t = Instant::now();
        for i in 0..iterations {
            bm.insert(i as u32);
        }
        let elapsed = t.elapsed().as_micros() as u64;
        results.push(BenchResult {
            name: "Roaring Bitmap insert",
            ops: iterations,
            elapsed_us: elapsed,
        });

        let t = Instant::now();
        for i in 0..iterations {
            let _ = bm.contains(i as u32);
        }
        let elapsed = t.elapsed().as_micros() as u64;
        results.push(BenchResult {
            name: "Roaring Bitmap contains",
            ops: iterations,
            elapsed_us: elapsed,
        });
    }

    // ── 6. JIT Batch Filter ──
    {
        let n = iterations.min(100_000) as usize;
        let col: Vec<i64> = (0..n as i64).collect();
        let expr = JitExpr::Eq(
            Box::new(JitExpr::ColI64(0)),
            Box::new(JitExpr::LitI64(n as i64 / 2)),
        );
        let t = Instant::now();
        let _result = jit::batch_filter(&expr, &[&col], &[], n);
        let elapsed = t.elapsed().as_micros() as u64;
        results.push(BenchResult {
            name: "JIT batch_filter (eq scan)",
            ops: n as u64,
            elapsed_us: elapsed,
        });
    }

    // ── 7. HNSW Vector Index ──
    {
        let dim = 128;
        let count = iterations.min(5_000);
        let mut hnsw = HnswIndex::new(dim, HnswConfig::default());
        let t = Instant::now();
        for i in 0..count {
            let vec: Vec<f32> = (0..dim)
                .map(|d| ((i as f32 * 0.01) + d as f32 * 0.001).sin())
                .collect();
            hnsw.insert(i as u32, vec);
        }
        let elapsed_insert = t.elapsed().as_micros() as u64;
        results.push(BenchResult {
            name: "HNSW insert (128d, cosine)",
            ops: count,
            elapsed_us: elapsed_insert,
        });

        let query: Vec<f32> = (0..dim).map(|d| (d as f32 * 0.002).cos()).collect();
        let searches = count.min(1_000);
        let t = Instant::now();
        for _ in 0..searches {
            let _ = hnsw.search(&query, 10);
        }
        let elapsed_search = t.elapsed().as_micros() as u64;
        results.push(BenchResult {
            name: "HNSW search top-10 (128d)",
            ops: searches,
            elapsed_us: elapsed_search,
        });
    }

    // ── 8. HyperLogLog ──
    {
        let mut hll = HyperLogLog::new();
        let t = Instant::now();
        for i in 0..iterations {
            hll.add(&i);
        }
        let elapsed = t.elapsed().as_micros() as u64;
        results.push(BenchResult {
            name: "HyperLogLog add",
            ops: iterations,
            elapsed_us: elapsed,
        });
    }

    // ── 9. Bloom Filter ──
    {
        let mut bf = BloomFilter::new(iterations as usize, 0.01);
        let t = Instant::now();
        for i in 0..iterations {
            bf.insert(&i);
        }
        let elapsed = t.elapsed().as_micros() as u64;
        results.push(BenchResult {
            name: "Bloom Filter insert",
            ops: iterations,
            elapsed_us: elapsed,
        });

        let t = Instant::now();
        for i in 0..iterations {
            let _ = bf.may_contain(&i);
        }
        let elapsed = t.elapsed().as_micros() as u64;
        results.push(BenchResult {
            name: "Bloom Filter lookup",
            ops: iterations,
            elapsed_us: elapsed,
        });
    }

    // ── 10. SQL end-to-end ──
    {
        let tmp_dir = std::env::temp_dir().join(format!("qm_bench_sql_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp_dir);
        let engine = qm_engine::NativeSqlEngine::with_data_dir(tmp_dir.clone());
        let _ = engine.execute("CREATE TABLE bench_t (id INTEGER PRIMARY KEY, val TEXT)");

        let insert_n = iterations.min(10_000);
        let t = Instant::now();
        for i in 0..insert_n {
            let _ = engine.execute(&format!("INSERT INTO bench_t VALUES ({i}, 'row_{i}')"));
        }
        let elapsed_insert = t.elapsed().as_micros() as u64;
        results.push(BenchResult {
            name: "SQL INSERT (end-to-end)",
            ops: insert_n,
            elapsed_us: elapsed_insert,
        });

        let select_n = insert_n.min(5_000);
        let t = Instant::now();
        for i in 0..select_n {
            let _ = engine.execute(&format!("SELECT * FROM bench_t WHERE id = {i}"));
        }
        let elapsed_select = t.elapsed().as_micros() as u64;
        results.push(BenchResult {
            name: "SQL SELECT by PK (end-to-end)",
            ops: select_n,
            elapsed_us: elapsed_select,
        });

        let t = Instant::now();
        let agg_n = 100u64;
        for _ in 0..agg_n {
            let _ = engine.execute("SELECT COUNT(*) FROM bench_t");
        }
        let elapsed_agg = t.elapsed().as_micros() as u64;
        results.push(BenchResult {
            name: "SQL COUNT(*) aggregation",
            ops: agg_n,
            elapsed_us: elapsed_agg,
        });
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    // ── Output ──
    if json {
        let entries: Vec<String> = results
            .iter()
            .map(|r| {
                format!(
                "  {{\"name\": \"{}\", \"ops\": {}, \"elapsed_us\": {}, \"ops_per_sec\": {:.0}}}",
                r.name, r.ops, r.elapsed_us, r.ops_per_sec()
            )
            })
            .collect();
        println!(
            "{{\"version\": \"4.3.4\", \"profile\": \"{profile}\", \"results\": [\n{}\n]}}",
            entries.join(",\n")
        );
    } else {
        println!("{:<42} {:>14} {:>10}", "Benchmark", "ops/s", "time");
        println!("{}", "-".repeat(70));
        for r in &results {
            let ops_s = r.ops_per_sec();
            let time_str = if r.elapsed_us < 1_000 {
                format!("{} us", r.elapsed_us)
            } else if r.elapsed_us < 1_000_000 {
                format!("{:.1} ms", r.elapsed_us as f64 / 1_000.0)
            } else {
                format!("{:.2} s", r.elapsed_us as f64 / 1_000_000.0)
            };
            let ops_str = if ops_s >= 1_000_000.0 {
                format!("{:.2}M", ops_s / 1_000_000.0)
            } else if ops_s >= 1_000.0 {
                format!("{:.1}K", ops_s / 1_000.0)
            } else {
                format!("{:.0}", ops_s)
            };
            println!("{:<42} {:>14} {:>10}", r.name, ops_str, time_str);
        }
        println!("{}", "=".repeat(70));
        println!("Done. {} benchmarks completed.", results.len());
    }
}

fn main() {
    let cli = Cli::parse();
    let lang = Lang::from_str(&cli.lang);

    match cli.command {
        Commands::Backup {
            output,
            compress,
            tables,
            pitr,
        } => {
            let engine = load_engine(&cli.data_dir, &lang);
            qm_engine::cli::backup_cmd::run_backup(&engine, &output, &compress, tables, pitr);
        }

        Commands::Verify { file, info } => {
            qm_engine::cli::backup_cmd::run_verify(&file, info);
        }

        Commands::Inspect { table, tables } => {
            let engine = load_engine(&cli.data_dir, &lang);
            if tables || table.is_none() {
                qm_engine::cli::inspect::run_list_tables(&engine);
            }
            if let Some(t) = &table {
                qm_engine::cli::inspect::run_inspect_table(&engine, t);
            }
        }

        Commands::Stat { json } => {
            let engine = load_engine(&cli.data_dir, &lang);
            qm_engine::cli::stat::run_stat(&engine, json);
        }

        Commands::Check { all: _, table } => {
            let engine = load_engine(&cli.data_dir, &lang);
            qm_engine::cli::check::run_check(&engine, table.as_deref());
        }

        Commands::Restore {
            input,
            drop_existing,
            tables,
        } => {
            let mut engine = load_engine(&cli.data_dir, &lang);
            qm_engine::cli::backup_cmd::run_restore(&mut engine, &input, drop_existing, tables);
        }

        Commands::Checkpoint => {
            let engine = load_engine(&cli.data_dir, &lang);
            engine.checkpoint();
            println!(
                "{}: {}",
                lang.msg("checkpoint_done", ""),
                cli.data_dir.display()
            );
        }

        Commands::Sql { query } => {
            let engine = load_engine(&cli.data_dir, &lang);
            match engine.execute(&query) {
                Ok(result) => {
                    let col_names: Vec<String> =
                        result.columns.iter().map(|(n, _, _)| n.clone()).collect();
                    println!("{}", col_names.join(" | "));
                    println!("{}", "─".repeat(col_names.len() * 15));
                    for row in &result.rows {
                        let vals: Vec<String> = row
                            .iter()
                            .map(|cell| {
                                cell.as_ref()
                                    .map(|b| String::from_utf8_lossy(b).into_owned())
                                    .unwrap_or_else(|| "NULL".to_string())
                            })
                            .collect();
                        println!("{}", vals.join(" | "));
                    }
                    println!("\n{}", result.command_tag);
                }
                Err(e) => {
                    smart_error(&lang, &e.to_string());
                    std::process::exit(1);
                }
            }
        }

        Commands::Dump {
            format,
            table,
            output,
            stdout,
        } => {
            let engine = load_engine(&cli.data_dir, &lang);
            qm_engine::cli::dump::run_dump(
                &engine,
                table.as_deref(),
                &format,
                output.as_deref(),
                stdout,
            );
        }

        Commands::Schema { action } => match action {
            SchemaAction::Diff {
                dir_a,
                dir_b,
                output,
            } => {
                qm_engine::cli::schema::run_diff(&dir_a, &dir_b, output.as_deref());
            }
            SchemaAction::Export => {
                let engine = load_engine(&cli.data_dir, &lang);
                qm_engine::cli::schema::run_export(&engine);
            }
            SchemaAction::Migrate { file, dry_run } => {
                let engine = load_engine(&cli.data_dir, &lang);
                qm_engine::cli::schema::run_migrate(&engine, &file, dry_run);
            }
        },

        Commands::Predict { compress, json } => {
            let engine = load_engine(&cli.data_dir, &lang);
            let compression = qm_engine::backup::Compression::from_str(&compress)
                .unwrap_or(qm_engine::backup::Compression::Lz4);
            let predict = qm_engine::backup::predict::PredictEngine::new(&engine);
            match predict.predict_backup(compression) {
                Ok(result) => {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&result).unwrap());
                    } else {
                        println!("Backup size estimate:");
                        println!("  Tables:     {}", result.table_count);
                        println!("  Rows:       {}", result.row_count);
                        println!(
                            "  Compressed: {} bytes ({:.1} KB)",
                            result.estimated_size_bytes,
                            result.estimated_size_bytes as f64 / 1024.0
                        );
                        println!("  Duration:   ~{} ms", result.estimated_duration_ms);
                        println!("  Compression: {}", result.compression);
                    }
                }
                Err(e) => {
                    smart_error(&lang, &e.to_string());
                    std::process::exit(1);
                }
            }
        }

        Commands::Encrypt { file, password } => {
            match qm_engine::backup::encrypt::encrypt_file(&file, &password) {
                Ok(out_path) => println!("Encrypted -> {out_path}"),
                Err(e) => {
                    smart_error(&lang, &e.to_string());
                    std::process::exit(1);
                }
            }
        }

        Commands::Decrypt {
            file,
            output,
            password,
        } => match qm_engine::backup::encrypt::decrypt_file_to(&file, &output, &password) {
            Ok(()) => println!("Decrypted -> {}", output.display()),
            Err(e) => {
                smart_error(&lang, &e.to_string());
                std::process::exit(1);
            }
        },

        Commands::DiffBackup {
            base,
            output,
            compress,
        } => {
            let engine = load_engine(&cli.data_dir, &lang);
            let compression = qm_engine::backup::Compression::from_str(&compress)
                .unwrap_or(qm_engine::backup::Compression::Lz4);
            let diff_config = qm_engine::backup::snapshot_diff::DiffConfig {
                base_backup: base,
                compression,
                output,
            };
            let diff_engine = qm_engine::backup::snapshot_diff::DiffEngine::new(&engine);
            match diff_engine.create_diff(&diff_config) {
                Ok(result) => {
                    println!("Differential backup created:");
                    println!("  Path:           {}", result.path);
                    println!("  Tables changed: {}", result.tables_changed);
                    println!("  Rows modified:  {}", result.rows_modified);
                    println!("  Rows deleted:   {}", result.rows_deleted);
                    println!("  Size:           {} bytes", result.diff_size);
                    println!(
                        "  LSN range:      {} -> {}",
                        result.base_lsn, result.end_lsn
                    );
                    println!("  Duration:       {} ms", result.duration_ms);
                }
                Err(e) => {
                    smart_error(&lang, &e.to_string());
                    std::process::exit(1);
                }
            }
        }

        Commands::Version => {
            println!("QMvir v{}", env!("CARGO_PKG_VERSION"));
            println!("Engine: qm_engine (Rust)");
            println!(
                "Build: {}",
                if cfg!(debug_assertions) {
                    "debug"
                } else {
                    "release"
                }
            );
            println!("Guide:  qm guide [quickstart|backup|studio|cli|notes]");
        }

        Commands::Guide { topic } => {
            qm_engine::cli::guide::run_guide(&lang, &topic);
        }

        Commands::Benchtest { profile, json } => {
            run_benchtest(&profile, json);
        }

        Commands::Start {
            host,
            port,
            max_connections,
            foreground,
            unix_socket,
            admin_password,
        } => {
            qm_engine::cli::server::run_start(
                &cli.data_dir,
                &host,
                port,
                max_connections,
                unix_socket,
                admin_password,
                !foreground, // default is daemon=true, --foreground sets daemon=false
            );
        }

        Commands::Stop => {
            qm_engine::cli::server::run_stop(&cli.data_dir);
        }

        Commands::Status => {
            qm_engine::cli::server::run_status(&cli.data_dir);
        }

        Commands::Cluster { action } => match action {
            qm_engine::cli::ClusterAction::Status => {
                qm_engine::cli::cluster::run_status();
            }
            qm_engine::cli::ClusterAction::Health => {
                qm_engine::cli::cluster::run_health();
            }
            qm_engine::cli::ClusterAction::Readiness => {
                qm_engine::cli::cluster::run_readiness();
            }
            qm_engine::cli::ClusterAction::Join {
                shard_id,
                primary,
                replicas,
            } => {
                let primary_addr: std::net::SocketAddr = primary
                    .parse()
                    .unwrap_or_else(|_| {
                        eprintln!("invalid primary address: {primary}");
                        std::process::exit(1);
                    });
                let replica_addrs: Vec<std::net::SocketAddr> = replicas
                    .iter()
                    .filter_map(|s| {
                        s.parse().map_err(|_| {
                            eprintln!("invalid replica address: {s}");
                        }).ok()
                    })
                    .collect();
                qm_engine::cli::cluster::run_join(shard_id, primary_addr, replica_addrs);
            }
            qm_engine::cli::ClusterAction::Leave { shard_id } => {
                qm_engine::cli::cluster::run_leave(shard_id);
            }
            qm_engine::cli::ClusterAction::Lag => {
                qm_engine::cli::cluster::run_lag();
            }
            qm_engine::cli::ClusterAction::Metrics => {
                qm_engine::cli::cluster::run_metrics();
            }
            qm_engine::cli::ClusterAction::Certify => {
                let code = qm_engine::cli::cluster::run_certify(true);
                if code != 0 {
                    std::process::exit(code);
                }
            }
        },
    }
}
