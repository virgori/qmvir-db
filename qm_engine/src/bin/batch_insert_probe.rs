//! Isolate Batch INSERT cost: parse/materialize vs WAL sync.
//! Usage: batch_insert_probe [batches=100] [batch_size=100]

use qm_engine::gateway::native_sql::NativeSqlEngine;
use std::time::Instant;

fn sql_esc(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn make_batch_sql(table: &str, start_id: i64, batch_size: usize) -> String {
    let mut out = String::with_capacity(batch_size * 140);
    out.push_str("INSERT INTO ");
    out.push_str(table);
    out.push_str(" VALUES ");
    for i in 0..batch_size {
        if i > 0 {
            out.push(',');
        }
        let id = start_id + i as i64;
        // ~70 char text like sense_text avg
        let text = format!(
            "sense text body for id {} with some unicode 中文内容 padding xx",
            id
        );
        out.push_str(&format!(
            "({},{},{},{},{})",
            id,
            id % 1000,
            sql_esc("vi"),
            sql_esc("definition"),
            sql_esc(&text)
        ));
    }
    out
}

fn run_policy(label: &str, policy: Option<&str>, batches: usize, batch_size: usize) {
    // Same filesystem as fair bench — /tmp is tmpfs and hides fsync cost.
    let base = std::env::var("QM_PROBE_DIR").unwrap_or_else(|_| "/opt/benchmark".into());
    let dir = format!(
        "{}/qm_batch_probe_{}_{}",
        base.trim_end_matches('/'),
        label,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    let engine = if policy.is_none() {
        // in-memory: no WAL
        NativeSqlEngine::new()
    } else {
        let e = NativeSqlEngine::with_data_dir(dir.clone().into());
        e.set_wal_sync_policy(policy.unwrap()).expect("policy");
        e
    };
    engine
        .execute(
            "CREATE TABLE t (id BIGINT PRIMARY KEY, sense_id BIGINT, language TEXT, text_type TEXT, text TEXT)",
        )
        .expect("create");

    // warmup
    let warm = make_batch_sql("t", 1, batch_size);
    engine.execute(&warm).expect("warm");
    engine.execute("DELETE FROM t").expect("delete");

    let mut sqls = Vec::with_capacity(batches);
    for b in 0..batches {
        let start = (b * batch_size) as i64 + 1;
        sqls.push(make_batch_sql("t", start, batch_size));
    }
    let sql_bytes = sqls[0].len();

    let t0 = Instant::now();
    for sql in &sqls {
        engine.execute(sql).expect("insert");
    }
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    println!(
        "[{label}] policy={} batches={batches} batch_size={batch_size} sql_bytes={sql_bytes} wall_ms={:.1} per_batch_ms={:.2}",
        policy.unwrap_or("memory"),
        ms,
        ms / batches as f64
    );
    if policy.is_some() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn main() {
    let batches: usize = std::env::args()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let batch_size: usize = std::env::args()
        .nth(2)
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);

    for (label, policy) in [
        ("mem", None),
        ("append", Some("append_only_profile")),
        ("relaxed", Some("relaxed_os_buffered")),
        ("group", Some("group_commit_sync")),
        ("per_commit", Some("per_commit_sync")),
    ] {
        run_policy(label, policy, batches, batch_size);
    }
}
