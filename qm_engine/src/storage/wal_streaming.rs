/*
 * WAL Replication Streaming — Phase C3
 *
 * Architecture:
 *   Primary (WalSender)  ──TCP──▶  Replica (WalReceiver)
 *
 *   WalSender tails the WAL file and ships new entries as they appear.
 *   WalReceiver applies each entry to its local engine after CRC verification.
 *
 * WAL file format (one line per entry, text):
 *   <lsn_dec>\t<crc32_hex>\t<sql>\n
 *
 * Python API:
 *   from qm_engine import WalSender, WalReceiver
 *   sender   = WalSender(wal_path, replica_host, replica_port)
 *   receiver = WalReceiver(engine)        # engine = PyNativeSqlEngine
 */

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[cfg(feature = "python")]
use pyo3::prelude::*;

use crate::cluster::transport::{NodeClient, WalEntry};
#[cfg(feature = "python")]
use crate::gateway::PyNativeSqlEngine;

// ── WAL file helpers ────────────────────────────────────────────────

/// Append a WAL entry to the local WAL file.
pub fn wal_append(wal_path: &std::path::Path, lsn: u64, sql: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(wal_path)?;
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(sql.as_bytes());
    let checksum = hasher.finalize();
    writeln!(file, "{}\t{:08x}\t{}", lsn, checksum, sql)?;
    Ok(())
}

/// Parse a single WAL log line into a `WalEntry`.
fn parse_wal_line(line: &str) -> Option<WalEntry> {
    let mut parts = line.splitn(3, '\t');
    let lsn_s  = parts.next()?;
    let crc_s  = parts.next()?;
    let sql    = parts.next()?;
    let lsn    = lsn_s.trim().parse::<u64>().ok()?;
    let checksum = u32::from_str_radix(crc_s.trim(), 16).ok()?;
    Some(WalEntry { lsn, sql: sql.to_string(), checksum })
}

// ── WalStreamState — shared metrics ──────────────────────────────

#[derive(Debug, Default)]
pub struct WalStreamState {
    pub sent_lsn:    AtomicU64,
    pub acked_lsn:   AtomicU64,
    pub active:      AtomicBool,
    pub lag_entries: AtomicU64,
}

impl WalStreamState {
    pub fn new() -> Arc<Self> {
        let s = Arc::new(Self::default());
        s.active.store(false, Ordering::Release);
        s
    }
}

// ── WalSender ──────────────────────────────────────────────────────

pub struct WalSender {
    wal_path:     PathBuf,
    replica_addr: SocketAddr,
    pub state:    Arc<WalStreamState>,
    stop_flag:    Arc<AtomicBool>,
}

impl WalSender {
    pub fn new(wal_path: PathBuf, replica_addr: SocketAddr) -> (Self, Arc<WalStreamState>) {
        let state = WalStreamState::new();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let s = Self { wal_path, replica_addr, state: state.clone(), stop_flag };
        (s, state)
    }

    /// Start the sender loop in the background.  
    /// Reads new WAL entries every 100 ms and ships them to the replica.
    pub fn run(self) -> thread::JoinHandle<()> {
        thread::Builder::new()
            .name("wal-sender".into())
            .spawn(move || {
                self.state.active.store(true, Ordering::Release);
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("WalSender runtime");

                let mut last_offset: u64 = 0;

                loop {
                    if self.stop_flag.load(Ordering::Acquire) {
                        break;
                    }

                    if let Ok(mut file) = File::open(&self.wal_path) {
                        let _ = file.seek(SeekFrom::Start(last_offset));
                        let sent = self.state.sent_lsn.load(Ordering::Relaxed);
                        let mut entries: Vec<WalEntry> = {
                            let reader = BufReader::new(&mut file);
                            reader.lines()
                                .flatten()
                                .filter(|l| !l.trim().is_empty())
                                .filter_map(|l| parse_wal_line(l.trim()))
                                .filter(|e| e.lsn > sent)
                                .collect()
                            // reader is dropped here, releasing borrow on file
                        };
                        // Now file is free to seek again.
                        if let Ok(pos) = file.seek(SeekFrom::Current(0)) {
                            last_offset = pos;
                        } else if let Ok(m) = std::fs::metadata(&self.wal_path) {
                            last_offset = m.len();
                        }

                        self.state.lag_entries.store(entries.len() as u64, Ordering::Relaxed);

                        let client = NodeClient::new(0, self.replica_addr);
                        for entry in entries {
                            let lsn = entry.lsn;
                            let r = rt.block_on(async { client.send_wal_entry(&entry).await });
                            match r {
                                Ok(acked_lsn) => {
                                    self.state.sent_lsn.store(lsn, Ordering::Release);
                                    self.state.acked_lsn.store(acked_lsn, Ordering::Release);
                                    self.state.lag_entries.fetch_sub(1, Ordering::Relaxed);
                                }
                                Err(e) => {
                                    tracing::warn!("WAL send error at LSN {}: {}", lsn, e);
                                    // Back off and retry next iteration.
                                    break;
                                }
                            }
                        }
                    }

                    thread::sleep(Duration::from_millis(100));
                }

                self.state.active.store(false, Ordering::Release);
            })
            .expect("spawn wal-sender thread")
    }

    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Release);
    }
}

// ── WalReceiver ───────────────────────────────────────────────────

pub struct WalReceiver {
    engine:             Arc<crate::gateway::native_sql::NativeSqlEngine>,
    pub applied_count:  AtomicU64,
    pub last_applied:   AtomicU64,
}

impl WalReceiver {
    pub fn new(engine: Arc<crate::gateway::native_sql::NativeSqlEngine>) -> Self {
        Self {
            engine,
            applied_count: AtomicU64::new(0),
            last_applied:  AtomicU64::new(0),
        }
    }

    /// Apply a single WAL entry: verify CRC32, then execute SQL.
    pub fn apply(&self, entry: &WalEntry) -> bool {
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(entry.sql.as_bytes());
        if hasher.finalize() != entry.checksum {
            tracing::error!("WAL CRC mismatch at LSN {}", entry.lsn);
            return false;
        }
        match self.engine.execute(&entry.sql) {
            Ok(_) => {
                self.applied_count.fetch_add(1, Ordering::Relaxed);
                self.last_applied.store(entry.lsn, Ordering::Release);
                true
            }
            Err(e) => {
                tracing::error!("WAL apply error at LSN {}: {}", entry.lsn, e);
                false
            }
        }
    }
}

// ── Python-facing WalSender ────────────────────────────────────────
#[cfg(feature = "python")]#[pyclass(name = "WalSender")]
pub struct PyWalSender {
    stop_flag: Arc<AtomicBool>,
    state:     Arc<WalStreamState>,
    handle:    Mutex<Option<thread::JoinHandle<()>>>,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyWalSender {
    /// `wal_path` – path to the primary WAL file.  
    /// `replica_host`, `replica_port` – where the replica's transport server listens.
    #[new]
    #[pyo3(signature = (wal_path, replica_host, replica_port))]
    pub fn new(wal_path: &str, replica_host: &str, replica_port: u16) -> PyResult<Self> {
        let addr: SocketAddr = format!("{}:{}", replica_host, replica_port)
            .parse()
            .map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {}", e))
            })?;

        let path = PathBuf::from(wal_path);
        let (sender, state) = WalSender::new(path, addr);
        let stop_flag = sender.stop_flag.clone();
        let handle = sender.run();

        Ok(Self {
            stop_flag,
            state,
            handle: Mutex::new(Some(handle)),
        })
    }

    /// Stop the sender background thread.
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Release);
    }

    /// Current highest LSN shipped to the replica.
    #[getter]
    pub fn sent_lsn(&self) -> u64 {
        self.state.sent_lsn.load(Ordering::Relaxed)
    }

    /// Highest LSN acknowledged by the replica.
    #[getter]
    pub fn acked_lsn(&self) -> u64 {
        self.state.acked_lsn.load(Ordering::Relaxed)
    }

    /// Number of pending WAL entries not yet sent.
    #[getter]
    pub fn lag_entries(&self) -> u64 {
        self.state.lag_entries.load(Ordering::Relaxed)
    }

    /// Whether the sender thread is running.
    #[getter]
    pub fn active(&self) -> bool {
        self.state.active.load(Ordering::Relaxed)
    }
}

// ── Python-facing WalReceiver ─────────────────────────────────────
#[cfg(feature = "python")]#[pyclass(name = "WalReceiver")]
pub struct PyWalReceiver {
    inner: Arc<WalReceiver>,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyWalReceiver {
    #[new]
    pub fn new(engine: &PyNativeSqlEngine) -> Self {
        let eng = Arc::new(engine.inner.clone());
        Self { inner: Arc::new(WalReceiver::new(eng)) }
    }

    /// Apply a single WAL entry dict `{lsn, sql, checksum}`.
    pub fn apply_entry(&self, lsn: u64, sql: &str, checksum: u32) -> bool {
        let entry = WalEntry { lsn, sql: sql.to_string(), checksum };
        self.inner.apply(&entry)
    }

    /// Total number of WAL entries successfully applied.
    #[getter]
    pub fn applied_count(&self) -> u64 {
        self.inner.applied_count.load(Ordering::Relaxed)
    }

    /// LSN of the last successfully applied entry.
    #[getter]
    pub fn last_applied_lsn(&self) -> u64 {
        self.inner.last_applied.load(Ordering::Relaxed)
    }
}

// ── Convenience function ────────────────────────────────────────────

/// Start a WAL sender in one call.  
/// Returns `(PyWalSender, state_dict)` — the dict has `sent_lsn`, `acked_lsn`, `lag_entries`.
#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (wal_path, replica_host, replica_port))]
pub fn start_wal_sender(
    wal_path: &str,
    replica_host: &str,
    replica_port: u16,
) -> PyResult<PyWalSender> {
    PyWalSender::new(wal_path, replica_host, replica_port)
}
