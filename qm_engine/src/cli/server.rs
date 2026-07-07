//! `qm start` / `qm stop` / `qm status` — daemon lifecycle.
//!
//! Replaces the Python `qm_app.py` with a pure-Rust server that:
//! - Starts a PostgreSQL wire protocol gateway (tokio)
//! - Manages a PID file for single-instance enforcement
//! - Supports foreground or daemonised operation
//! - Enforces password change away from the default

use crate::gateway::native_sql::NativeSqlEngine;
use crate::gateway::{ConnectionConfig, Server};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const PID_FILE: &str = "qm.pid";

/// Read PID from the PID file. Returns `None` if the file does not exist or
/// is malformed.
fn read_pid(data_dir: &Path) -> Option<u32> {
    let path = data_dir.join(PID_FILE);
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

/// Check if a process with the given PID is still running.
fn pid_alive(pid: u32) -> bool {
    // On Unix, signal 0 checks existence without actually signalling.
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Write the current PID to the lock file using O_EXCL to avoid symlink races
/// (SEC-11).
fn write_pid(data_dir: &Path) -> Result<(), String> {
    let path = data_dir.join(PID_FILE);

    // Check for stale PID file
    if path.exists() {
        if let Some(old_pid) = read_pid(data_dir) {
            if pid_alive(old_pid) {
                return Err(format!(
                    "Another QMvir instance is already running (PID {}). \
                     Remove {} if this is stale.",
                    old_pid,
                    path.display()
                ));
            }
        }
        // Stale — remove it.
        let _ = fs::remove_file(&path);
    }

    // SEC-11: Use O_EXCL + symlink guard to avoid overwriting arbitrary files.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true) // O_EXCL
            .mode(0o600)
            .open(&path)
            .map_err(|e| format!("cannot create PID file {}: {}", path.display(), e))?;
        writeln!(f, "{}", std::process::id()).map_err(|e| format!("write PID: {}", e))?;
    }

    #[cfg(not(unix))]
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("cannot create PID file {}: {}", path.display(), e))?;
        writeln!(f, "{}", std::process::id()).map_err(|e| format!("write PID: {}", e))?;
    }

    Ok(())
}

fn remove_pid(data_dir: &Path) {
    let _ = fs::remove_file(data_dir.join(PID_FILE));
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

#[cfg(unix)]
fn spawn_daemon(
    data_dir: &Path,
    host: &str,
    port: u16,
    max_connections: usize,
    unix_socket: Option<&str>,
    admin_password: Option<&str>,
) -> Result<u32, String> {
    use std::fs::OpenOptions;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe().map_err(|e| format!("cannot locate qm executable: {e}"))?;
    let log_path = data_dir.join("qm.log");
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("cannot open daemon log {}: {e}", log_path.display()))?;
    let stderr_file = log_file
        .try_clone()
        .map_err(|e| format!("cannot clone daemon log handle: {e}"))?;
    let stdin_file = OpenOptions::new()
        .read(true)
        .open("/dev/null")
        .map_err(|e| format!("cannot open /dev/null: {e}"))?;

    let mut cmd = Command::new(exe);
    cmd.arg("--data-dir")
        .arg(data_dir)
        .arg("start")
        .arg("--host")
        .arg(host)
        .arg("--port")
        .arg(port.to_string())
        .arg("--max-connections")
        .arg(max_connections.to_string())
        .env("QM_DAEMONIZED", "1")
        .stdin(Stdio::from(stdin_file))
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(stderr_file));

    if let Some(path) = unix_socket {
        cmd.arg("--unix-socket").arg(path);
    }
    if let Some(password) = admin_password {
        cmd.arg("--admin-password").arg(password);
    }

    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("cannot spawn daemon: {e}"))?;
    Ok(child.id())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// `qm start` — launch the database server.
pub fn run_start(
    data_dir: &PathBuf,
    host: &str,
    port: u16,
    max_connections: usize,
    unix_socket: Option<String>,
    admin_password: Option<String>,
    daemon: bool,
) {
    if daemon && std::env::var_os("QM_DAEMONIZED").is_none() {
        #[cfg(unix)]
        {
            match spawn_daemon(
                data_dir,
                host,
                port,
                max_connections,
                unix_socket.as_deref(),
                admin_password.as_deref(),
            ) {
                Ok(pid) => {
                    println!("QMvir daemon started (PID {})", pid);
                    // Poll the TCP port until the server is accepting connections
                    // (up to 5 s) so that callers don't need an explicit `sleep`.
                    let addr = format!("{}:{}", host, port);
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                    let mut ready = false;
                    while std::time::Instant::now() < deadline {
                        if std::net::TcpStream::connect(&addr).is_ok() {
                            ready = true;
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    if ready {
                        println!("QMvir is ready on {}", addr);
                    } else {
                        eprintln!(
                            "warning: server did not become ready within 5 s — \
                                   check {} for errors",
                            data_dir.join("qm.log").display()
                        );
                    }
                    return;
                }
                Err(e) => {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }
        }

        #[cfg(not(unix))]
        {
            eprintln!("error: --daemon is only supported on Unix in this build");
            std::process::exit(1);
        }
    }

    // Ensure data directory exists.
    fs::create_dir_all(data_dir).unwrap_or_else(|e| {
        eprintln!(
            "error: cannot create data directory {}: {}",
            data_dir.display(),
            e
        );
        std::process::exit(1);
    });

    // Single-instance guard.
    if let Err(e) = write_pid(data_dir) {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }

    // Build engine.
    let engine = Arc::new(NativeSqlEngine::with_data_dir(data_dir.clone()));

    if let Ok(policy) = std::env::var("QMVIR_WAL_SYNC_POLICY") {
        if let Err(err) = engine.set_wal_sync_policy(&policy) {
            eprintln!("warning: invalid QMVIR_WAL_SYNC_POLICY ({err}); using default");
        } else {
            eprintln!("[WAL] sync policy: {}", policy);
        }
    } else if let Err(err) = engine.set_wal_sync_policy("group_commit_sync") {
        eprintln!("warning: failed to set default group_commit_sync ({err})");
    } else {
        eprintln!("[WAL] sync policy: group_commit_sync (gateway default)");
    }

    // SEC-02: Override default admin password.
    if let Some(ref pw) = admin_password {
        if pw != "admin" {
            let _ = engine.auth.alter_user_password("admin", pw);
        }
    }

    // pgwire: allow remote bind when TLS is configured.
    let tls = crate::gateway::pg_tls::PgTlsConfig::from_env();
    let is_loopback = is_loopback_host(host);
    if !is_loopback && !tls.enabled {
        eprintln!(
            "error: Binding pgwire to non-loopback address {} requires TLS.\n\
             Set QM_TLS_CERT and QM_TLS_KEY (or QM_PG_TLS_CERT / QM_PG_TLS_KEY).",
            host
        );
        remove_pid(data_dir);
        std::process::exit(1);
    }

    // Tokio runtime (shared by pgwire + optional cluster transport).
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(num_cpus::get().max(4))
        .enable_all()
        .build()
        .expect("failed to create tokio runtime");

    let attach = crate::cluster::prepare_gateway_cluster(engine.clone(), &rt);
    let (handler, authed_handler) = crate::cluster::routed_query_handlers(&attach, engine.clone());

    // Build config.
    let config = ConnectionConfig {
        host: host.to_string(),
        port,
        unix_socket_path: unix_socket,
        max_connections,
        ..Default::default()
    };

    // Create query handlers.
    let auth_mgr = engine.auth.clone();
    let engine_for_shutdown = engine.clone();

    let server = Arc::new(Server::new_with_auth(
        config.clone(),
        handler,
        authed_handler,
        auth_mgr,
    ));

    // Install signal handlers for graceful shutdown.
    let server_clone = server.clone();
    let data_dir_clone = data_dir.clone();
    ctrlc_handler(move || {
        eprintln!("\nShutting down...");
        eprintln!("[Shutdown] Checkpointing...");
        engine_for_shutdown.checkpoint();
        eprintln!("[Shutdown] Checkpoint done.");
        server_clone.stop_sync();
        remove_pid(&data_dir_clone);
    });

    // Run the server (blocking on tokio runtime).
    rt.block_on(async { server.run().await })
        .unwrap_or_else(|e| eprintln!("server error: {}", e));

    remove_pid(data_dir);
}

/// `qm stop` — send SIGTERM to a running QMvir process.
pub fn run_stop(data_dir: &PathBuf) {
    match read_pid(data_dir) {
        None => {
            eprintln!(
                "No running QMvir instance found (no PID file in {})",
                data_dir.display()
            );
            std::process::exit(1);
        }
        Some(pid) => {
            if !pid_alive(pid) {
                eprintln!("PID {} is not running. Removing stale PID file.", pid);
                remove_pid(data_dir);
                return;
            }
            #[cfg(unix)]
            {
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGTERM);
                }
                println!("Sent SIGTERM to QMvir (PID {})", pid);
            }
            #[cfg(not(unix))]
            {
                eprintln!(
                    "Stop is not supported on this platform. Kill PID {} manually.",
                    pid
                );
            }
        }
    }
}

/// `qm status` — show whether a QMvir server is running.
pub fn run_status(data_dir: &PathBuf) {
    match read_pid(data_dir) {
        None => {
            println!(
                "QMvir is not running (no PID file in {})",
                data_dir.display()
            );
        }
        Some(pid) => {
            if pid_alive(pid) {
                println!("QMvir is running (PID {})", pid);
            } else {
                println!("QMvir is not running (stale PID file, PID {})", pid);
            }
        }
    }
}

/// Simple Ctrl-C handler (no external crate needed).
fn ctrlc_handler<F: FnOnce() + Send + 'static>(f: F) {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let f = std::sync::Mutex::new(Some(f));
        // Unix: install SIGINT + SIGTERM handlers.
        #[cfg(unix)]
        {
            use std::sync::atomic::{AtomicBool, Ordering};
            static SIGNALLED: AtomicBool = AtomicBool::new(false);
            // We can't call the closure from a signal handler directly.
            // Instead, set a flag and have a background thread wait for it.
            unsafe {
                libc::signal(
                    libc::SIGINT,
                    signal_handler as *const () as libc::sighandler_t,
                );
                libc::signal(
                    libc::SIGTERM,
                    signal_handler as *const () as libc::sighandler_t,
                );
            }
            extern "C" fn signal_handler(_: libc::c_int) {
                SIGNALLED.store(true, Ordering::SeqCst);
            }
            std::thread::spawn(move || {
                while !SIGNALLED.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                if let Ok(mut guard) = f.lock() {
                    if let Some(cb) = guard.take() {
                        cb();
                    }
                }
                std::process::exit(0);
            });
        }
        #[cfg(not(unix))]
        {
            // On non-Unix, we just rely on Ctrl-C via std.
            std::thread::spawn(move || {
                // Busy-wait is the simplest portable fallback.
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                }
            });
        }
    });
}
