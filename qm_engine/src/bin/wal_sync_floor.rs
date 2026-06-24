use serde_json::json;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy)]
enum SyncKind {
    SyncAll,
    SyncData,
}

#[derive(Default)]
struct Sample {
    serialize_ns: u128,
    write_ns: u128,
    flush_ns: u128,
    sync_ns: u128,
    total_ns: u128,
}

struct Case {
    name: &'static str,
    record_size: usize,
    records: usize,
    sync_kind: SyncKind,
    write: bool,
}

fn percentile(mut values: Vec<u128>, pct: f64) -> u128 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let idx = (((values.len() - 1) as f64) * (pct / 100.0)).round() as usize;
    values[idx.min(values.len() - 1)]
}

fn ns_to_ms(ns: u128) -> f64 {
    ns as f64 / 1_000_000.0
}

fn summarize(samples: &[Sample], f: impl Fn(&Sample) -> u128) -> serde_json::Value {
    let values: Vec<u128> = samples.iter().map(f).collect();
    json!({
        "p50_ns": percentile(values.clone(), 50.0),
        "p95_ns": percentile(values.clone(), 95.0),
        "p99_ns": percentile(values, 99.0),
    })
}

fn open_wal(path: &Path) -> std::io::Result<BufWriter<File>> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)?;
    Ok(BufWriter::with_capacity(64 * 1024, file))
}

fn make_record(case_name: &str, iteration: usize, size: usize) -> String {
    let mut record = format!("INSERT INTO wal_floor VALUES ({iteration}, '{case_name}', '");
    if record.len() < size {
        record.push_str(&"x".repeat(size - record.len()));
    }
    record.push_str("');");
    record
}

fn run_case(path: &Path, case: &Case, iterations: usize) -> std::io::Result<serde_json::Value> {
    let mut samples = Vec::with_capacity(iterations);
    let bytes_per_record = if case.write {
        make_record(case.name, 0, case.record_size).len() + 1
    } else {
        0
    };

    for iteration in 0..iterations {
        let mut writer = open_wal(path)?;
        let total_start = Instant::now();
        let mut serialize_ns = Duration::ZERO;
        let mut write_ns = Duration::ZERO;

        if case.write {
            let serialize_start = Instant::now();
            let records: Vec<String> = (0..case.records)
                .map(|record_idx| {
                    make_record(
                        case.name,
                        iteration * case.records + record_idx,
                        case.record_size,
                    )
                })
                .collect();
            serialize_ns = serialize_start.elapsed();

            let write_start = Instant::now();
            for record in &records {
                writeln!(writer, "{record}")?;
            }
            write_ns = write_start.elapsed();
        }

        let flush_start = Instant::now();
        writer.flush()?;
        let flush_ns = flush_start.elapsed();

        let file = writer.get_ref();
        let sync_start = Instant::now();
        match case.sync_kind {
            SyncKind::SyncAll => file.sync_all()?,
            SyncKind::SyncData => file.sync_data()?,
        }
        let sync_ns = sync_start.elapsed();

        samples.push(Sample {
            serialize_ns: serialize_ns.as_nanos(),
            write_ns: write_ns.as_nanos(),
            flush_ns: flush_ns.as_nanos(),
            sync_ns: sync_ns.as_nanos(),
            total_ns: total_start.elapsed().as_nanos(),
        });
    }

    let sync_name = match case.sync_kind {
        SyncKind::SyncAll => "sync_all",
        SyncKind::SyncData => "sync_data",
    };
    Ok(json!({
        "name": case.name,
        "sync_kind": sync_name,
        "iterations": iterations,
        "record_size": case.record_size,
        "records_per_iteration": case.records,
        "bytes_per_iteration": bytes_per_record * case.records,
        "serialize_ns": summarize(&samples, |s| s.serialize_ns),
        "write_ns": summarize(&samples, |s| s.write_ns),
        "flush_ns": summarize(&samples, |s| s.flush_ns),
        "sync_ns": summarize(&samples, |s| s.sync_ns),
        "total_ns": summarize(&samples, |s| s.total_ns),
        "total_p50_ms": ns_to_ms(percentile(samples.iter().map(|s| s.total_ns).collect(), 50.0)),
        "sync_p50_ms": ns_to_ms(percentile(samples.iter().map(|s| s.sync_ns).collect(), 50.0)),
    }))
}

fn parse_arg(args: &[String], flag: &str, default: &str) -> String {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .unwrap_or_else(|| default.to_string())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let iterations = parse_arg(&args, "--iterations", "200").parse::<usize>()?;
    let output = PathBuf::from(parse_arg(
        &args,
        "--output",
        "/private/tmp/qmvir_pg_perf_pass7_2026_06_12/rust_wal_sync_floor.json",
    ));
    let root = PathBuf::from(parse_arg(
        &args,
        "--root",
        "/private/tmp/qmvir_pg_perf_pass7_2026_06_12/rust_wal_sync_floor",
    ));
    fs::create_dir_all(&root)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let wal_path = root.join("native_sql_like.wal");
    let _ = fs::remove_file(&wal_path);
    File::create(&wal_path)?;

    let cases = [
        Case {
            name: "no_write_sync_all",
            record_size: 0,
            records: 0,
            sync_kind: SyncKind::SyncAll,
            write: false,
        },
        Case {
            name: "tiny_append_flush_sync_all",
            record_size: 96,
            records: 1,
            sync_kind: SyncKind::SyncAll,
            write: true,
        },
        Case {
            name: "append_1kb_flush_sync_all",
            record_size: 1024,
            records: 1,
            sync_kind: SyncKind::SyncAll,
            write: true,
        },
        Case {
            name: "append_4kb_flush_sync_all",
            record_size: 4096,
            records: 1,
            sync_kind: SyncKind::SyncAll,
            write: true,
        },
        Case {
            name: "append_64kb_flush_sync_all",
            record_size: 65536,
            records: 1,
            sync_kind: SyncKind::SyncAll,
            write: true,
        },
        Case {
            name: "batch_10_tiny_flush_sync_all",
            record_size: 96,
            records: 10,
            sync_kind: SyncKind::SyncAll,
            write: true,
        },
        Case {
            name: "batch_100_tiny_flush_sync_all",
            record_size: 96,
            records: 100,
            sync_kind: SyncKind::SyncAll,
            write: true,
        },
        Case {
            name: "batch_1000_tiny_flush_sync_all",
            record_size: 96,
            records: 1000,
            sync_kind: SyncKind::SyncAll,
            write: true,
        },
        Case {
            name: "tiny_append_flush_sync_data",
            record_size: 96,
            records: 1,
            sync_kind: SyncKind::SyncData,
            write: true,
        },
        Case {
            name: "append_4kb_flush_sync_data",
            record_size: 4096,
            records: 1,
            sync_kind: SyncKind::SyncData,
            write: true,
        },
        Case {
            name: "batch_100_tiny_flush_sync_data",
            record_size: 96,
            records: 100,
            sync_kind: SyncKind::SyncData,
            write: true,
        },
    ];

    let mut results = Vec::with_capacity(cases.len());
    for case in &cases {
        results.push(run_case(&wal_path, case, iterations)?);
    }

    let report = json!({
        "label": "RUST_WAL_SYNC_FLOOR_CALIBRATION_ONLY",
        "schema_version": 1,
        "iterations": iterations,
        "filesystem_path": wal_path,
        "file_open_flags": "create=true, append=true, read=true; BufWriter capacity=65536",
        "generated_unix_nanos": SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        "results": results,
    });
    fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!("wrote {}", output.display());
    Ok(())
}
