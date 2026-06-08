//! REST API endpoints for the QMvir web dashboard.

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use std::path::Path as FsPath;
use std::sync::Arc;

use super::WebState;
use crate::backup::backup::{BackupConfig, BackupEngine};
use crate::backup::Compression;

// ── Response types ───────────────────────────────────────────────────

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub uptime_secs: u64,
}

#[derive(Serialize)]
pub struct StatsResponse {
    pub queries_total: u64,
    pub inserts_total: u64,
    pub deletes_total: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_hit_rate: f64,
    pub txn_commits: u64,
    pub txn_aborts: u64,
    pub active_txns: f64,
    pub wal_writes: u64,
    pub wal_size_bytes: f64,
    pub errors_total: u64,
    pub uptime_secs: u64,
}

#[derive(Serialize)]
pub struct TableInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub column_types: Vec<String>,
    pub row_count: usize,
}

#[derive(Serialize)]
pub struct TableDetail {
    pub name: String,
    pub columns: Vec<String>,
    pub column_types: Vec<String>,
    pub row_count: usize,
    pub sample_rows: Vec<Vec<String>>,
}

#[derive(Serialize)]
pub struct WalStatus {
    pub wal_writes: u64,
    pub wal_size_bytes: f64,
}

#[derive(Deserialize)]
pub struct QueryRequest {
    pub sql: String,
}

#[derive(Serialize)]
pub struct QueryResponse {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub command_tag: String,
}

#[derive(Deserialize)]
pub struct BackupRequest {
    pub output: String,
    #[serde(default = "default_compression")]
    pub compression: String,
    pub tables: Option<Vec<String>>,
}

fn default_compression() -> String {
    "lz4".to_string()
}

fn unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"QMvir Dashboard\"")],
        "authentication required",
    )
        .into_response()
}

fn parse_basic_auth(req: &Request<Body>) -> Option<(String, String)> {
    let value = req.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
    let encoded = value.strip_prefix("Basic ")?;
    let decoded = BASE64.decode(encoded).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (username, password) = decoded.split_once(':')?;
    Some((username.to_string(), password.to_string()))
}

fn sanitize_backup_name(output: &str) -> Result<String, String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Err("backup output name cannot be empty".to_string());
    }

    let path = FsPath::new(trimmed);
    if path.is_absolute() || path.components().count() != 1 {
        return Err(
            "backup output must be a file name inside the web backup directory".to_string(),
        );
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "backup output name must be valid UTF-8".to_string())?;

    if !file_name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(
            "backup output name may only contain ASCII letters, digits, '.', '-', and '_'"
                .to_string(),
        );
    }

    if file_name.ends_with(".qmvb") {
        Ok(file_name.to_string())
    } else {
        Ok(format!("{}.qmvb", file_name))
    }
}

pub async fn require_basic_auth(
    State(s): State<Arc<WebState>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let Some((username, password)) = parse_basic_auth(&req) else {
        return unauthorized_response();
    };

    match s.engine.auth.authenticate(&username, &password) {
        Ok(()) => next.run(req).await,
        Err(_) => unauthorized_response(),
    }
}

#[derive(Serialize)]
pub struct BackupResponse {
    pub path: String,
    pub tables_backed_up: u32,
    pub total_rows: u64,
    pub compressed_size: u64,
    pub duration_ms: u64,
}

// ── Handlers ─────────────────────────────────────────────────────────

pub async fn api_health(State(s): State<Arc<WebState>>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: "2.0.0",
        uptime_secs: s.start_time.elapsed().as_secs(),
    })
}

pub async fn api_stats(State(s): State<Arc<WebState>>) -> Json<StatsResponse> {
    let m = &s.metrics;
    let hits = m.cache_hits.get();
    let misses = m.cache_misses.get();
    let total = hits + misses;
    let hit_rate = if total > 0 {
        hits as f64 / total as f64 * 100.0
    } else {
        0.0
    };

    Json(StatsResponse {
        queries_total: m.queries_total.get(),
        inserts_total: m.inserts_total.get(),
        deletes_total: m.deletes_total.get(),
        cache_hits: hits,
        cache_misses: misses,
        cache_hit_rate: hit_rate,
        txn_commits: m.txn_commits.get(),
        txn_aborts: m.txn_aborts.get(),
        active_txns: m.active_txns.get(),
        wal_writes: m.wal_writes.get(),
        wal_size_bytes: m.wal_size_bytes.get(),
        errors_total: m.errors_total.get(),
        uptime_secs: s.start_time.elapsed().as_secs(),
    })
}

pub async fn api_tables(State(s): State<Arc<WebState>>) -> Json<Vec<TableInfo>> {
    let tables = s.engine.tables.read().unwrap();
    let infos: Vec<TableInfo> = tables
        .iter()
        .map(|(name, t)| TableInfo {
            name: name.clone(),
            columns: t.columns.clone(),
            column_types: t.column_types.iter().map(|c| format!("{c:?}")).collect(),
            row_count: t.rows.len(),
        })
        .collect();
    Json(infos)
}

pub async fn api_table_detail(
    State(s): State<Arc<WebState>>,
    Path(name): Path<String>,
) -> Result<Json<TableDetail>, StatusCode> {
    let tables = s.engine.tables.read().unwrap();
    let table = tables.get(&name).ok_or(StatusCode::NOT_FOUND)?;

    let sample_rows: Vec<Vec<String>> = table
        .rows
        .iter()
        .take(10)
        .map(|(_, row)| {
            table
                .columns
                .iter()
                .map(|col| row.cols.get(col).map(|c| c.as_text()).unwrap_or_default())
                .collect()
        })
        .collect();

    Ok(Json(TableDetail {
        name: name.clone(),
        columns: table.columns.clone(),
        column_types: table
            .column_types
            .iter()
            .map(|c| format!("{c:?}"))
            .collect(),
        row_count: table.rows.len(),
        sample_rows,
    }))
}

pub async fn api_wal_status(State(s): State<Arc<WebState>>) -> Json<WalStatus> {
    Json(WalStatus {
        wal_writes: s.metrics.wal_writes.get(),
        wal_size_bytes: s.metrics.wal_size_bytes.get(),
    })
}

pub async fn api_metrics(State(s): State<Arc<WebState>>) -> String {
    s.metrics.render()
}

pub async fn api_query(
    State(s): State<Arc<WebState>>,
    Json(req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, (StatusCode, String)> {
    // Input validation
    if req.sql.len() > 10_000 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Query too long (max 10KB)".to_string(),
        ));
    }

    let result = s
        .engine
        .execute(&req.sql)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let columns: Vec<String> = result.columns.iter().map(|(n, _, _)| n.clone()).collect();
    let rows: Vec<Vec<Option<String>>> = result
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| {
                    cell.as_ref()
                        .map(|b| String::from_utf8_lossy(b).into_owned())
                })
                .collect()
        })
        .collect();

    Ok(Json(QueryResponse {
        columns,
        rows,
        command_tag: result.command_tag,
    }))
}

pub async fn api_backup(
    State(s): State<Arc<WebState>>,
    Json(req): Json<BackupRequest>,
) -> Result<Json<BackupResponse>, (StatusCode, String)> {
    let comp = Compression::from_str(&req.compression).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            format!("Unknown compression: {}", req.compression),
        )
    })?;

    let backup_dir = s.backup_dir.clone().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "backup endpoint requires a persistent data directory".to_string(),
        )
    })?;
    let file_name = sanitize_backup_name(&req.output).map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let config = BackupConfig {
        tables: req.tables,
        compression: comp,
        include_wal: false,
        output: backup_dir.join(file_name),
    };

    let be = BackupEngine::new(&s.engine);
    let result = be
        .run(&config)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(BackupResponse {
        path: result.path,
        tables_backed_up: result.tables_backed_up as u32,
        total_rows: result.total_rows,
        compressed_size: result.compressed_size,
        duration_ms: result.duration_ms,
    }))
}
