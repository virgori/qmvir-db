use parking_lot::RwLock;
use qm_engine::gateway::NativeSqlEngine;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

/// A live engine instance (one per data directory / in-memory connection).
pub struct EngineInstance {
    pub engine: NativeSqlEngine,
    pub name: String,
    pub data_dir: Option<PathBuf>,
    pub created_at: Instant,
    /// Authenticated username for this connection (default: "admin").
    pub username: String,
}

/// Global application state managed by Tauri.
pub struct AppState {
    /// Active connections: connection_id → engine instance.
    pub connections: RwLock<HashMap<String, Arc<EngineInstance>>>,
    /// Running queries: query_id → cancellation token.
    pub running_queries: RwLock<HashMap<String, CancellationToken>>,
    /// Query history (last 1000).
    pub history: RwLock<Vec<HistoryEntry>>,
    counter: std::sync::atomic::AtomicU64,
}

#[derive(Clone, serde::Serialize)]
pub struct HistoryEntry {
    pub id: String,
    pub timestamp: String,
    pub sql: String,
    pub duration_us: u64,
    pub row_count: usize,
    pub error: Option<String>,
    pub connection_name: String,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            connections: RwLock::new(HashMap::new()),
            running_queries: RwLock::new(HashMap::new()),
            history: RwLock::new(Vec::new()),
            counter: std::sync::atomic::AtomicU64::new(1),
        }
    }

    pub fn next_id(&self) -> String {
        let n = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("conn-{}", n)
    }

    pub fn next_query_id(&self) -> String {
        let n = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("q-{}", n)
    }

    pub fn get_instance(&self, connection_id: &str) -> Result<Arc<EngineInstance>, String> {
        self.connections
            .read()
            .get(connection_id)
            .cloned()
            .ok_or_else(|| format!("Connection '{}' not found", connection_id))
    }

    pub fn add_history(&self, entry: HistoryEntry) {
        let mut h = self.history.write();
        h.push(entry);
        let len = h.len();
        if len > 1000 {
            h.drain(..len - 1000);
        }
    }
}
