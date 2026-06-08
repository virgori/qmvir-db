//! PyO3 bindings for the Web Dashboard.
//!
//! Exposes `start_web_server(engine, host, port, background)` to Python.
//!
//! Usage:
//! ```python
//! import qm_engine, threading
//!
//! engine = qm_engine.NativeSqlEngine()
//! # Non-blocking: starts server in background thread, returns immediately
//! qm_engine.start_web_server(engine, "127.0.0.1", 8080, background=True)
//!
//! # Blocking: runs server in the current thread (use in a daemon thread)
//! qm_engine.start_web_server(engine, "127.0.0.1", 8080, background=False)
//! ```

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::fs;
use std::sync::Arc;
use std::time::Instant;

use super::{start_web, WebState};
use crate::gateway::PyNativeSqlEngine;
use crate::metrics::MetricsRegistry;

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

/// Start the QMvir web dashboard server.
///
/// Parameters
/// ----------
/// engine : NativeSqlEngine
///     The engine instance to expose through the dashboard.
/// host : str
///     Bind address (default ``"127.0.0.1"``).
/// port : int
///     TCP port (default ``8080``).
/// background : bool
///     When *True* (default) the server runs in a background OS thread and
///     this function returns immediately. When *False* the function blocks
///     until the server exits (use inside your own daemon thread).
#[pyfunction]
#[pyo3(signature = (engine, host="127.0.0.1", port=8080, background=true))]
pub fn start_web_server(
    engine: &PyNativeSqlEngine,
    host: &str,
    port: u16,
    background: bool,
) -> PyResult<()> {
    if !is_loopback_host(host) {
        return Err(PyRuntimeError::new_err(
            "web dashboard only supports loopback addresses in this build",
        ));
    }

    // Build shared state — clone the inner engine so the thread owns it.
    let inner = engine.inner.clone();
    if inner.auth.authenticate("admin", "admin").is_ok() {
        return Err(PyRuntimeError::new_err(
            "default admin password is still active; change it before starting the web dashboard",
        ));
    }

    let metrics = Arc::new(MetricsRegistry::new());
    let addr = format!("{host}:{port}");
    let backup_dir = match inner.data_dir.clone() {
        Some(dir) => {
            let backup_dir = dir.join("backups");
            fs::create_dir_all(&backup_dir).map_err(|e| {
                PyRuntimeError::new_err(format!(
                    "cannot create backup directory {}: {e}",
                    backup_dir.display()
                ))
            })?;
            Some(backup_dir)
        }
        None => None,
    };

    let run = move || -> Result<(), String> {
        let state = Arc::new(WebState {
            engine: Arc::new(inner),
            metrics,
            start_time: Instant::now(),
            backup_dir,
        });
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?
            .block_on(start_web(state, &addr))
            .map_err(|e| e.to_string())
    };

    if background {
        std::thread::Builder::new()
            .name("qm-web".into())
            .spawn(move || {
                if let Err(e) = run() {
                    eprintln!("qm-web error: {e}");
                }
            })
            .map_err(|e| PyRuntimeError::new_err(format!("Failed to spawn web thread: {e}")))?;
        Ok(())
    } else {
        run().map_err(|e| PyRuntimeError::new_err(e))
    }
}
