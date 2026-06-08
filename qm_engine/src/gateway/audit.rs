/*
 * Audit Logger — Structured audit trail for DDL, DML, and auth events.
 *
 * Records who did what, when, on which table, with outcome (ok / error).
 * Writes JSON lines to a configurable audit log file or stderr.
 */

use parking_lot::Mutex;
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Verbosity levels for audit logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuditLevel {
    /// Log nothing.
    Off,
    /// DDL + auth events only.
    Ddl,
    /// DDL + DML + auth events.
    All,
}

impl AuditLevel {
    pub fn from_env() -> Self {
        match std::env::var("QM_AUDIT_LEVEL")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "off" | "none" | "0" => Self::Off,
            "ddl" => Self::Ddl,
            "all" | "1" => Self::All,
            _ => Self::Ddl, // default: log DDL + auth
        }
    }
}

/// Category of the audited operation.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum AuditCategory {
    Auth,
    Ddl,
    Dml,
    Admin,
}

/// A single audit event.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEvent {
    /// ISO-8601 timestamp.
    pub ts: String,
    /// Category of the operation.
    pub category: AuditCategory,
    /// SQL command or action name (e.g. "CREATE TABLE", "INSERT", "LOGIN").
    pub action: String,
    /// User who performed the action.
    pub username: String,
    /// Target object (table name, index name, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    /// Whether the operation succeeded.
    pub ok: bool,
    /// Error message (if failed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl AuditEvent {
    fn now_iso() -> String {
        let dur = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let secs = dur.as_secs();
        // Simple UTC timestamp without chrono dependency.
        let days = secs / 86400;
        let rem = secs % 86400;
        let h = rem / 3600;
        let m = (rem % 3600) / 60;
        let s = rem % 60;
        // Rough year/month/day (good enough for audit logs; not leap-second-precise).
        let (y, mo, d) = days_to_ymd(days);
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, m, s)
    }

    pub fn auth_ok(username: &str, action: &str) -> Self {
        Self {
            ts: Self::now_iso(),
            category: AuditCategory::Auth,
            action: action.to_string(),
            username: username.to_string(),
            object: None,
            ok: true,
            error: None,
        }
    }

    pub fn auth_fail(username: &str, action: &str, err: &str) -> Self {
        Self {
            ts: Self::now_iso(),
            category: AuditCategory::Auth,
            action: action.to_string(),
            username: username.to_string(),
            object: None,
            ok: false,
            error: Some(err.to_string()),
        }
    }

    pub fn ddl(username: &str, action: &str, object: &str, ok: bool, err: Option<&str>) -> Self {
        Self {
            ts: Self::now_iso(),
            category: AuditCategory::Ddl,
            action: action.to_string(),
            username: username.to_string(),
            object: Some(object.to_string()),
            ok,
            error: err.map(|e| e.to_string()),
        }
    }

    pub fn dml(username: &str, action: &str, table: &str, ok: bool, err: Option<&str>) -> Self {
        Self {
            ts: Self::now_iso(),
            category: AuditCategory::Dml,
            action: action.to_string(),
            username: username.to_string(),
            object: Some(table.to_string()),
            ok,
            error: err.map(|e| e.to_string()),
        }
    }
}

/// Simple days-since-epoch to (year, month, day) conversion.
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

enum AuditSink {
    File(BufWriter<File>),
    Stderr,
}

/// Thread-safe audit logger.
#[derive(Clone)]
pub struct AuditLogger {
    inner: Arc<Mutex<AuditSink>>,
    level: AuditLevel,
}

impl AuditLogger {
    /// Create a logger that writes to a file.
    pub fn new(path: PathBuf) -> Self {
        let level = AuditLevel::from_env();
        let sink = match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => AuditSink::File(BufWriter::new(f)),
            Err(_) => AuditSink::Stderr,
        };
        Self {
            inner: Arc::new(Mutex::new(sink)),
            level,
        }
    }

    /// Create a logger that writes to stderr.
    pub fn stderr() -> Self {
        Self {
            inner: Arc::new(Mutex::new(AuditSink::Stderr)),
            level: AuditLevel::from_env(),
        }
    }

    /// Log an audit event if the current level permits it.
    pub fn log(&self, event: &AuditEvent) {
        let dominated = match event.category {
            AuditCategory::Auth | AuditCategory::Ddl | AuditCategory::Admin => self.level >= AuditLevel::Ddl,
            AuditCategory::Dml => self.level >= AuditLevel::All,
        };
        if !dominated {
            return;
        }
        let line = match serde_json::to_string(event) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut sink = self.inner.lock();
        match &mut *sink {
            AuditSink::File(w) => {
                let _ = writeln!(w, "{}", line);
                let _ = w.flush();
            }
            AuditSink::Stderr => {
                eprintln!("[AUDIT] {}", line);
            }
        }
    }
}
