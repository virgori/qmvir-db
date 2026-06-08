use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubConfig {
    pub data_dir: String,
    pub max_connections: usize,
    pub vector_mode: ExecutionMode,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            data_dir: "/tmp/qm_data".to_string(),
            max_connections: 1024,
            vector_mode: ExecutionMode::VectorSuggestionOnly,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableMeta {
    pub name: String,
    #[serde(default)]
    pub columns: Vec<String>,
    pub row_count: u64,
    pub primary_key: String,
    pub indexes: Vec<IndexInfo>,
    pub vector_dim: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionMode {
    Logical,
    VectorSuggestionOnly,
    NativeHotPath,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryRequest {
    pub sql: String,
    pub mode: ExecutionMode,
    pub purpose: Option<String>,
    pub allow_vector_join: bool,
}

impl QueryRequest {
    pub fn is_ai_context(&self) -> bool {
        if self.allow_vector_join {
            return true;
        }
        let p = self.purpose.as_deref().unwrap_or("").to_ascii_lowercase();
        matches!(
            p.as_str(),
            "suggestion"
                | "suggest"
                | "question-suggestion"
                | "recommendation"
                | "recommend"
                | "autocomplete"
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub affected_rows: u64,
}

impl QueryResult {
    pub fn empty() -> Self {
        Self {
            columns: Vec::new(),
            rows: Vec::new(),
            affected_rows: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubStatus {
    Init = 0,
    Starting = 1,
    Ready = 2,
    Stopping = 3,
    Stopped = 4,
}

#[derive(Debug)]
pub struct AtomicHubStatus {
    inner: AtomicU8,
}

impl AtomicHubStatus {
    pub fn new(v: HubStatus) -> Self {
        Self {
            inner: AtomicU8::new(v as u8),
        }
    }

    pub fn load(&self) -> HubStatus {
        match self.inner.load(Ordering::SeqCst) {
            1 => HubStatus::Starting,
            2 => HubStatus::Ready,
            3 => HubStatus::Stopping,
            4 => HubStatus::Stopped,
            _ => HubStatus::Init,
        }
    }

    pub fn store(&self, v: HubStatus) {
        self.inner.store(v as u8, Ordering::SeqCst);
    }
}
