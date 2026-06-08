//! QMvir Web Dashboard — `qm-web` binary entry point.
//!
//! Usage:
//!   qm-web --data-dir ./data --port 8080

use clap::Parser;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use qm_engine::metrics::MetricsRegistry;
use qm_engine::web::{start_web, WebState};
use qm_engine::NativeSqlEngine;

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

#[derive(Parser)]
#[command(name = "qm-web", version = "2.0.0", about = "QMvir Web Dashboard")]
struct Args {
    /// Data directory
    #[arg(long, default_value = "data")]
    data_dir: PathBuf,

    /// Listen address
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Listen port
    #[arg(long, default_value = "8080")]
    port: u16,

    /// Rotate the built-in admin password before starting the dashboard
    #[arg(long, env = "QM_ADMIN_PASSWORD")]
    admin_password: Option<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    if !is_loopback_host(&args.host) {
        eprintln!(
            "error: qm-web only supports loopback addresses in this build. Use 127.0.0.1, ::1, or localhost."
        );
        std::process::exit(1);
    }

    fs::create_dir_all(&args.data_dir).unwrap_or_else(|e| {
        eprintln!(
            "error: cannot create data directory {}: {e}",
            args.data_dir.display()
        );
        std::process::exit(1);
    });

    let backup_dir = args.data_dir.join("backups");
    fs::create_dir_all(&backup_dir).unwrap_or_else(|e| {
        eprintln!(
            "error: cannot create backup directory {}: {e}",
            backup_dir.display()
        );
        std::process::exit(1);
    });

    let engine = NativeSqlEngine::with_data_dir(args.data_dir.clone());

    if let Some(password) = args.admin_password.as_deref() {
        if password == "admin" {
            eprintln!("error: --admin-password must not be the default password");
            std::process::exit(1);
        }
        if let Err(e) = engine.auth.alter_user_password("admin", password) {
            eprintln!("error: cannot update admin password: {e}");
            std::process::exit(1);
        }
    } else if engine.auth.authenticate("admin", "admin").is_ok() {
        eprintln!(
            "error: admin password is still the default. Pass --admin-password or set QM_ADMIN_PASSWORD before starting qm-web."
        );
        std::process::exit(1);
    }

    let engine = Arc::new(engine);
    let metrics = Arc::new(MetricsRegistry::new());

    let state = Arc::new(WebState {
        engine,
        metrics,
        start_time: Instant::now(),
        backup_dir: Some(backup_dir),
    });

    let addr = format!("{}:{}", args.host, args.port);
    if let Err(e) = start_web(state, &addr).await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
