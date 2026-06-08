//! Web Dashboard module — axum-based HTTP server for QMvir monitoring.

pub mod api;
pub mod dashboard;
#[cfg(feature = "python")]
pub mod pyo3;

use axum::{
    middleware,
    routing::{get, post},
    Router,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::gateway::native_sql::NativeSqlEngine;
use crate::metrics::MetricsRegistry;

/// Shared state for all web handlers.
pub struct WebState {
    pub engine: Arc<NativeSqlEngine>,
    pub metrics: Arc<MetricsRegistry>,
    pub start_time: Instant,
    pub backup_dir: Option<PathBuf>,
}

/// Build the axum router with all endpoints.
pub fn router(state: Arc<WebState>) -> Router {
    let protected = Router::new()
        .route("/", get(dashboard::dashboard_html))
        .route("/api/health", get(api::api_health))
        .route("/api/stats", get(api::api_stats))
        .route("/api/tables", get(api::api_tables))
        .route("/api/tables/{name}", get(api::api_table_detail))
        .route("/api/wal/status", get(api::api_wal_status))
        .route("/metrics", get(api::api_metrics))
        .route("/api/query", post(api::api_query))
        .route("/api/backup", post(api::api_backup))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            api::require_basic_auth,
        ));

    Router::new().merge(protected).with_state(state)
}

/// Start the web server on the given address.
pub async fn start_web(state: Arc<WebState>, addr: &str) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("QMvir Dashboard running at http://{addr}");
    axum::serve(listener, router(state))
        .await
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
}
