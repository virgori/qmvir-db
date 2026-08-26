//! Probe: time engine-level INSERT hot path without PG wire/tokio overhead.
//! Usage: insert_hotpath_probe [rows] [data_dir]

use qm_engine::gateway::native_sql::NativeSqlEngine;
use std::time::Instant;

fn vector_literal(seed: usize, dim: usize) -> String {
    let mut out = String::with_capacity(dim * 10);
    out.push('[');
    for i in 0..dim {
        if i > 0 {
            out.push(',');
        }
        let v = ((seed * 31 + i * 17) % 1000) as f64 / 1000.0;
        out.push_str(&format!("{:.6}", v));
    }
    out.push(']');
    out
}

fn main() {
    let rows: usize = std::env::args()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(500);
    let dir = std::env::args().nth(2).unwrap_or_else(|| {
        format!(
            "/tmp/qm_insert_probe_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        )
    });
    let engine = NativeSqlEngine::with_data_dir(dir.clone().into());
    engine
        .set_wal_sync_policy("relaxed_os_buffered")
        .expect("set policy");

    engine
        .execute("CREATE TABLE text_probe (id BIGINT PRIMARY KEY, body TEXT)")
        .expect("create text table");
    let body = "x".repeat(7000);
    let start = Instant::now();
    for i in 1..=rows {
        let sql = format!(
            "INSERT INTO text_probe (id, body) VALUES ({}, '{}')",
            i, body
        );
        engine.execute(&sql).expect("text insert");
    }
    let text_ms = start.elapsed().as_secs_f64() * 1000.0;
    println!(
        "text7k rows={} total_ms={:.1} per_op_ms={:.3}",
        rows,
        text_ms,
        text_ms / rows as f64
    );

    engine
        .execute("CREATE TABLE vec_probe (id BIGINT PRIMARY KEY, embedding vector(768))")
        .expect("create vec table");
    let start = Instant::now();
    for i in 1..=rows {
        let sql = format!(
            "INSERT INTO vec_probe (id, embedding) VALUES ({}, '{}')",
            i,
            vector_literal(i, 768)
        );
        engine.execute(&sql).expect("vec insert");
    }
    let vec_ms = start.elapsed().as_secs_f64() * 1000.0;
    println!(
        "vec768 rows={} total_ms={:.1} per_op_ms={:.3}",
        rows,
        vec_ms,
        vec_ms / rows as f64
    );
    println!("data_dir={}", dir);
}
