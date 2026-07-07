/*
 * Gateway Module - PostgreSQL Wire Protocol Implementation
 *
 * High-performance async TCP server using tokio.
 * Implements PostgreSQL v3 wire protocol for client compatibility.
 */

pub mod auth;
mod cancel_registry;
mod connection;
pub mod native_sql;
pub mod pg_tls;
mod protocol;
pub mod scram;
mod server;
pub mod session_pool;
mod stream;
pub mod table_store;

pub use auth::AuthManager;
pub use connection::*;
pub use native_sql::*;
pub use pg_tls::PgTlsConfig;
pub use protocol::*;
pub use server::*;
pub use table_store::{SharedTable, TableStore};

#[cfg(feature = "python")]
use pyo3::exceptions::PyRuntimeError;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::PyAny;
use std::sync::Arc;
#[cfg(feature = "python")]
use std::sync::Mutex;
#[cfg(feature = "python")]
use std::thread::JoinHandle;
#[cfg(feature = "python")]
use std::time::Duration;
use tokio::runtime::Runtime;

/// Connection configuration
#[derive(Clone, Debug)]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub unix_socket_path: Option<String>,
    pub max_connections: usize,
    pub read_timeout_ms: u64,
    pub write_timeout_ms: u64,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 55433,
            unix_socket_path: None,
            max_connections: 1000,
            read_timeout_ms: 30000,
            write_timeout_ms: 30000,
        }
    }
}

/// PostgreSQL Gateway Server
pub struct PostgresGateway {
    config: ConnectionConfig,
    runtime: Arc<Runtime>,
}

impl PostgresGateway {
    pub fn new(config: ConnectionConfig) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(num_cpus::get().max(4))
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime");

        Self {
            config,
            runtime: Arc::new(runtime),
        }
    }

    pub fn config(&self) -> &ConnectionConfig {
        &self.config
    }

    pub fn runtime(&self) -> Arc<Runtime> {
        self.runtime.clone()
    }
}

/// Python-exposed Gateway
#[cfg(feature = "python")]
#[pyclass(name = "PostgresGateway")]
pub struct PyPostgresGateway {
    inner: Arc<PostgresGateway>,
    server: Arc<Mutex<Option<Arc<Server>>>>,
    runner: Arc<Mutex<Option<JoinHandle<()>>>>,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyPostgresGateway {
    #[new]
    #[pyo3(signature = (host="127.0.0.1", port=55433, max_connections=1000, unix_socket_path=None))]
    pub fn new(
        host: &str,
        port: u16,
        max_connections: usize,
        unix_socket_path: Option<String>,
    ) -> Self {
        let config = ConnectionConfig {
            host: host.to_string(),
            port,
            unix_socket_path,
            max_connections,
            ..Default::default()
        };
        Self {
            inner: Arc::new(PostgresGateway::new(config)),
            server: Arc::new(Mutex::new(None)),
            runner: Arc::new(Mutex::new(None)),
        }
    }

    pub fn get_config(&self) -> String {
        let uds = self
            .inner
            .config
            .unix_socket_path
            .as_ref()
            .map(|p| format!(", uds={}", p))
            .unwrap_or_default();
        format!(
            "{}:{}{} (max: {})",
            self.inner.config.host, self.inner.config.port, uds, self.inner.config.max_connections
        )
    }

    pub fn start(&self, execute_fn: Py<PyAny>) -> PyResult<()> {
        self.ensure_supported_host()?;

        let mut runner_guard = self.runner.lock().map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("runner lock poisoned: {e}"))
        })?;
        if runner_guard.is_some() {
            return Ok(());
        }

        let callback = Python::with_gil(|py| execute_fn.clone_ref(py));
        let handler: QueryHandler = Arc::new(move |sql: String| {
            Python::with_gil(|py| -> Result<QueryResult, String> {
                let ret = callback
                    .call1(py, (sql.clone(),))
                    .map_err(|e| format!("python execute_fn failed: {e}"))?;

                let (col_names, col_oids, py_rows): (
                    Vec<String>,
                    Vec<i32>,
                    Vec<Vec<Option<String>>>,
                ) = ret.extract(py).map_err(|_| {
                    "execute_fn must return (list[str], list[int], list[list[Optional[str]]])"
                        .to_string()
                })?;

                let mut rows: Vec<Vec<Option<Vec<u8>>>> = Vec::with_capacity(py_rows.len());
                for row in py_rows {
                    let mut out_row: Vec<Option<Vec<u8>>> = Vec::with_capacity(row.len());
                    for val in row {
                        out_row.push(val.map(|v| v.into_bytes()));
                    }
                    rows.push(out_row);
                }

                let mut columns: Vec<(String, i32, i16)> = Vec::with_capacity(col_names.len());
                for (idx, name) in col_names.into_iter().enumerate() {
                    let oid = *col_oids.get(idx).unwrap_or(&oid::TEXT);
                    let ty_len = match oid {
                        oid::BOOL => 1,
                        oid::INT2 => 2,
                        oid::INT4 => 4,
                        oid::INT8 => 8,
                        oid::FLOAT4 => 4,
                        oid::FLOAT8 => 8,
                        _ => -1,
                    };
                    columns.push((name, oid, ty_len));
                }

                let upper = sql.trim().to_ascii_uppercase();
                let command_tag = if upper.starts_with("SELECT") {
                    format!("SELECT {}", rows.len())
                } else if upper.starts_with("INSERT") {
                    "INSERT 0 1".to_string()
                } else if upper.starts_with("UPDATE") {
                    format!("UPDATE {}", rows.len())
                } else if upper.starts_with("DELETE") {
                    format!("DELETE {}", rows.len())
                } else {
                    "OK".to_string()
                };

                Ok(QueryResult {
                    columns,
                    rows,
                    command_tag,
                ..Default::default()
                })
            })
        });

        let authed_handler: AuthQueryHandler = {
            let handler = handler.clone();
            Arc::new(move |sql: String, _user: String| handler(sql))
        };

        let server = Arc::new(Server::new_with_auth(
            self.inner.config.clone(),
            handler,
            authed_handler,
            AuthManager::new(),
        ));
        self.launch_server(server, &mut runner_guard)
    }

    pub fn start_native(&self) -> PyResult<()> {
        self._start_native_impl(None)
    }

    #[pyo3(signature = (data_dir=None))]
    pub fn start_native_persist(&self, data_dir: Option<String>) -> PyResult<()> {
        self._start_native_impl(data_dir)
    }

    pub fn stop(&self) -> PyResult<()> {
        let server = {
            let mut srv_guard = self.server.lock().map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!("server lock poisoned: {e}"))
            })?;
            srv_guard.take()
        };
        if let Some(server) = server.as_ref() {
            server.stop_sync();
        }

        let runner = {
            let mut runner_guard = self.runner.lock().map_err(|e| {
                pyo3::exceptions::PyRuntimeError::new_err(format!("runner lock poisoned: {e}"))
            })?;
            runner_guard.take()
        };
        if let Some(handle) = runner {
            handle
                .join()
                .map_err(|_| PyRuntimeError::new_err("gateway server thread panicked"))?;
        }
        Ok(())
    }

    #[getter]
    pub fn is_running(&self) -> PyResult<bool> {
        let guard = self.server.lock().map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("server lock poisoned: {e}"))
        })?;
        Ok(guard.as_ref().map(|s| s.is_running()).unwrap_or(false))
    }

    #[getter]
    pub fn connection_count(&self) -> PyResult<u64> {
        let guard = self.server.lock().map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("server lock poisoned: {e}"))
        })?;
        Ok(guard
            .as_ref()
            .map(|s| {
                s.stats()
                    .active_connections
                    .load(std::sync::atomic::Ordering::Relaxed)
            })
            .unwrap_or(0))
    }
}

#[cfg(feature = "python")]
impl PyPostgresGateway {
    fn launch_server(
        &self,
        server: Arc<Server>,
        runner_guard: &mut Option<JoinHandle<()>>,
    ) -> PyResult<()> {
        let (startup_tx, startup_rx) = std::sync::mpsc::channel();
        let runtime = self.inner.runtime.clone();
        let thread_server = server.clone();
        let handle = std::thread::spawn(move || {
            if let Err(err) = runtime.block_on(async move {
                thread_server
                    .run_with_startup_signal(Some(startup_tx))
                    .await
            }) {
                tracing::warn!("gateway server exited: {}", err);
            }
        });

        match startup_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(_bound_addr)) => {
                let mut srv_guard = self.server.lock().map_err(|e| {
                    pyo3::exceptions::PyRuntimeError::new_err(format!("server lock poisoned: {e}"))
                })?;
                *srv_guard = Some(server);
                *runner_guard = Some(handle);
                Ok(())
            }
            Ok(Err(err)) => {
                let _ = handle.join();
                Err(PyRuntimeError::new_err(format!(
                    "gateway server failed to start: {err}"
                )))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let _ = handle.join();
                Err(PyRuntimeError::new_err(
                    "gateway server exited before reporting startup status",
                ))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                server.stop_sync();
                let _ = handle.join();
                Err(PyRuntimeError::new_err(
                    "timed out waiting for gateway listener to start",
                ))
            }
        }
    }

    fn ensure_supported_host(&self) -> PyResult<()> {
        if matches!(
            self.inner.config.host.as_str(),
            "127.0.0.1" | "::1" | "localhost"
        ) {
            return Ok(());
        }
        if crate::gateway::pg_tls::PgTlsConfig::from_env().acceptor.is_some() {
            return Ok(());
        }
        Err(PyRuntimeError::new_err(
            "remote TCP bindings require TLS; set QM_TLS_CERT and QM_TLS_KEY (or bind to 127.0.0.1/::1/localhost)",
        ))
    }

    fn _start_native_impl(&self, data_dir: Option<String>) -> PyResult<()> {
        self.ensure_supported_host()?;

        let mut runner_guard = self.runner.lock().map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!("runner lock poisoned: {e}"))
        })?;
        if runner_guard.is_some() {
            return Ok(());
        }

        let native = Arc::new(match data_dir {
            Some(ref d) => {
                let engine = NativeSqlEngine::with_data_dir(std::path::PathBuf::from(d));
                if std::env::var("QMVIR_WAL_SYNC_POLICY").is_err() {
                    let _ = engine.set_wal_sync_policy("group_commit_sync");
                }
                engine
            }
            None => NativeSqlEngine::new(),
        });
        let auth_mgr = native.auth.clone();

        let attach = crate::cluster::prepare_gateway_cluster(
            native.clone(),
            self.inner.runtime.as_ref(),
        );
        let (handler, authed_handler) =
            crate::cluster::routed_query_handlers(&attach, native.clone());

        let server = Arc::new(Server::new_with_auth(
            self.inner.config.clone(),
            handler,
            authed_handler,
            auth_mgr,
        ));
        self.launch_server(server, &mut runner_guard)
    }
}
