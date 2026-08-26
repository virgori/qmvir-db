//! Probe: time engine-level SELECT hot paths (LIKE / full scan / filter)
//! without PG wire/tokio overhead.

use qm_engine::gateway::native_sql::NativeSqlEngine;
use std::time::Instant;

fn main() {
    let dir = format!(
        "/tmp/qm_select_probe_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    let engine = NativeSqlEngine::with_data_dir(dir.clone().into());
    engine
        .set_wal_sync_policy("relaxed_os_buffered")
        .expect("set policy");

    engine
        .execute(
            "CREATE TABLE probe (id BIGINT PRIMARY KEY, sense_id BIGINT, language TEXT, text_type TEXT, text TEXT)",
        )
        .expect("create table");
    let langs = ["zh", "en", "ja"];
    let types = ["definition", "example", "note"];
    for chunk in 0..100 {
        let mut vals = String::new();
        for j in 1..=100 {
            let id = chunk * 100 + j;
            if j > 1 {
                vals.push(',');
            }
            vals.push_str(&format!(
                "({id}, {}, '{}', '{}', '中文意思是 sense {id} 的解释与说明 pattern{}')",
                id % 500,
                langs[id % 3],
                types[id % 3],
                id % 37
            ));
        }
        engine
            .execute(&format!(
                "INSERT INTO probe (id,sense_id,language,text_type,text) VALUES {vals}"
            ))
            .expect("seed");
    }

    let run = |label: &str, sql: &str, n: usize| {
        // warmup
        let r = engine.execute(sql).expect("query");
        let rows = r.row_count();
        let start = Instant::now();
        for _ in 0..n {
            let _ = engine.execute(sql).expect("query");
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        println!(
            "{label}: {n}x total={ms:.1}ms per={:.3}ms rows={rows}",
            ms / n as f64
        );
    };

    run(
        "like_common",
        "SELECT id FROM probe WHERE text LIKE '%中%' LIMIT 50",
        100,
    );
    run(
        "like_rare",
        "SELECT id FROM probe WHERE text LIKE '%pattern13%' LIMIT 50",
        100,
    );
    run("full_scan", "SELECT id, text FROM probe", 10);
    run(
        "filter",
        "SELECT id FROM probe WHERE language = 'zh' AND text_type = 'definition' LIMIT 100",
        100,
    );
    run("count", "SELECT COUNT(*) FROM probe", 50);
    println!("data_dir={dir}");
}
