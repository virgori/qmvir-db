/*
 * Server - Async TCP listener and connection manager
 *
 * High-performance server using tokio with connection pooling.
 */

use super::auth::AuthManager;
use super::connection::{AuthQueryHandler, Connection, QueryHandler, QueryResult};
use super::pg_tls::PgTlsConfig;
use super::stream::ServerIo;
use super::ConnectionConfig;
use std::future::pending;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::net::TcpSocket;
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::sync::{Notify, Semaphore};
use tracing::{error, info, warn};

/// Server statistics
pub struct ServerStats {
    pub total_connections: AtomicU64,
    pub active_connections: AtomicU64,
    pub total_queries: AtomicU64,
    pub failed_queries: AtomicU64,
}

impl Default for ServerStats {
    fn default() -> Self {
        Self {
            total_connections: AtomicU64::new(0),
            active_connections: AtomicU64::new(0),
            total_queries: AtomicU64::new(0),
            failed_queries: AtomicU64::new(0),
        }
    }
}

/// PostgreSQL-compatible server
pub struct Server {
    config: ConnectionConfig,
    query_handler: QueryHandler,
    authed_handler: Option<AuthQueryHandler>,
    auth: Option<AuthManager>,
    tls: PgTlsConfig,
    running: Arc<AtomicBool>,
    shutdown: Arc<Notify>,
    stats: Arc<ServerStats>,
}

impl Server {
    pub fn new(config: ConnectionConfig, query_handler: QueryHandler) -> Self {
        Self {
            config,
            query_handler,
            authed_handler: None,
            auth: None,
            tls: PgTlsConfig::from_env(),
            running: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(Notify::new()),
            stats: Arc::new(ServerStats::default()),
        }
    }

    /// Create server with authorization support.
    pub fn new_with_auth(
        config: ConnectionConfig,
        query_handler: QueryHandler,
        authed_handler: AuthQueryHandler,
        auth: AuthManager,
    ) -> Self {
        Self {
            config,
            query_handler,
            authed_handler: Some(authed_handler),
            auth: Some(auth),
            tls: PgTlsConfig::from_env(),
            running: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(Notify::new()),
            stats: Arc::new(ServerStats::default()),
        }
    }

    /// Same as `new_with_auth` but with an explicit TLS config (tests / programmatic setup).
    pub fn new_with_auth_tls(
        config: ConnectionConfig,
        query_handler: QueryHandler,
        authed_handler: AuthQueryHandler,
        auth: AuthManager,
        tls: PgTlsConfig,
    ) -> Self {
        Self {
            config,
            query_handler,
            authed_handler: Some(authed_handler),
            auth: Some(auth),
            tls,
            running: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(Notify::new()),
            stats: Arc::new(ServerStats::default()),
        }
    }

    /// Server without auth; optional TLS from env or explicit config.
    pub fn new_with_tls(config: ConnectionConfig, query_handler: QueryHandler, tls: PgTlsConfig) -> Self {
        Self {
            config,
            query_handler,
            authed_handler: None,
            auth: None,
            tls,
            running: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(Notify::new()),
            stats: Arc::new(ServerStats::default()),
        }
    }

    async fn bind_listener(&self) -> std::io::Result<(tokio::net::TcpListener, SocketAddr)> {
        if let Ok(addr) = format!("{}:{}", self.config.host, self.config.port).parse::<SocketAddr>()
        {
            let socket = if addr.is_ipv4() {
                TcpSocket::new_v4()?
            } else {
                TcpSocket::new_v6()?
            };
            socket.set_reuseaddr(true)?;
            socket.bind(addr)?;
            let listener = socket.listen(1024)?;
            let bound = listener.local_addr()?;
            return Ok((listener, bound));
        }

        let mut last_error = None;

        for addr in tokio::net::lookup_host((self.config.host.as_str(), self.config.port)).await? {
            let socket = if addr.is_ipv4() {
                TcpSocket::new_v4()?
            } else {
                TcpSocket::new_v6()?
            };
            socket.set_reuseaddr(true)?;

            match socket.bind(addr) {
                Ok(()) => {
                    let listener = socket.listen(1024)?;
                    let bound = listener.local_addr()?;
                    return Ok((listener, bound));
                }
                Err(err) => {
                    last_error = Some(err);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                format!(
                    "could not resolve a usable socket address for {}:{}",
                    self.config.host, self.config.port
                ),
            )
        }))
    }

    /// Start the server (blocking)
    pub async fn run(&self) -> std::io::Result<()> {
        self.run_with_startup_signal(None).await
    }

    /// Start the server and optionally report the startup result once the listener is bound.
    pub async fn run_with_startup_signal(
        &self,
        startup: Option<std::sync::mpsc::Sender<Result<SocketAddr, String>>>,
    ) -> std::io::Result<()> {
        let (listener, bound_addr) = match self.bind_listener().await {
            Ok(bound) => bound,
            Err(err) => {
                if let Some(tx) = startup {
                    let _ = tx.send(Err(err.to_string()));
                }
                return Err(err);
            }
        };

        #[cfg(unix)]
        let unix_listener = if let Some(path) = self.config.unix_socket_path.as_ref() {
            let _ = std::fs::remove_file(path);
            let uds = match UnixListener::bind(path) {
                Ok(uds) => uds,
                Err(err) => {
                    if let Some(tx) = startup {
                        let _ = tx.send(Err(err.to_string()));
                    }
                    return Err(err);
                }
            };
            info!("QM Engine listening on unix://{}", path);
            Some(uds)
        } else {
            None
        };

        println!(
            "QMvir v{} — listening on {}",
            env!("CARGO_PKG_VERSION"),
            bound_addr
        );
        info!("QM Engine listening on {}", bound_addr);
        self.running.store(true, Ordering::SeqCst);
        if let Some(tx) = startup {
            let _ = tx.send(Ok(bound_addr));
        }

        // Connection limiter
        let semaphore = Arc::new(Semaphore::new(self.config.max_connections));

        let shutdown = self.shutdown.clone();

        #[cfg(unix)]
        {
            loop {
                enum Incoming {
                    Tcp(tokio::net::TcpStream, String),
                    Unix(tokio::net::UnixStream),
                }

                let accepted = tokio::select! {
                    tcp = listener.accept() => {
                        tcp.map(|(stream, peer)| Incoming::Tcp(stream, peer.to_string()))
                    }
                    uds = async {
                        match &unix_listener {
                            Some(l) => l.accept().await.map(|(stream, _addr)| Incoming::Unix(stream)),
                            None => pending().await,
                        }
                    } => uds,
                    _ = shutdown.notified() => {
                        info!("Shutdown signal received, stopping accept loop");
                        break;
                    }
                };

                match accepted {
                    Ok(Incoming::Tcp(stream, peer_addr)) => {
                        let permit = match semaphore.clone().try_acquire_owned() {
                            Ok(p) => p,
                            Err(_) => {
                                warn!("Connection limit reached, rejecting {}", peer_addr);
                                continue;
                            }
                        };

                        let handler = self.query_handler.clone();
                        let authed = self.authed_handler.clone();
                        let auth = self.auth.clone();
                        let tls = self.tls.clone();
                        let stats = self.stats.clone();

                        stats.total_connections.fetch_add(1, Ordering::Relaxed);
                        stats.active_connections.fetch_add(1, Ordering::Relaxed);

                        tokio::spawn(async move {
                            let tls_acceptor = tls.acceptor.clone();
                            let mut conn = match (authed, auth) {
                                (Some(ah), Some(am)) => Connection::new_with_auth(
                                    stream,
                                    handler,
                                    ah,
                                    am,
                                    tls_acceptor,
                                ),
                                _ => Connection::new_io(
                                    ServerIo::from_tcp(stream),
                                    handler,
                                    None,
                                    None,
                                    tls_acceptor,
                                ),
                            };

                            if let Err(e) = conn.run().await {
                                if e.kind() != std::io::ErrorKind::ConnectionReset {
                                    warn!("Connection error: {}", e);
                                }
                            }

                            stats.active_connections.fetch_sub(1, Ordering::Relaxed);
                            drop(permit);
                        });
                    }
                    Ok(Incoming::Unix(stream)) => {
                        let permit = match semaphore.clone().try_acquire_owned() {
                            Ok(p) => p,
                            Err(_) => {
                                warn!("Connection limit reached, rejecting unix client");
                                continue;
                            }
                        };

                        let handler = self.query_handler.clone();
                        let authed = self.authed_handler.clone();
                        let auth = self.auth.clone();
                        let tls = self.tls.clone();
                        let stats = self.stats.clone();

                        stats.total_connections.fetch_add(1, Ordering::Relaxed);
                        stats.active_connections.fetch_add(1, Ordering::Relaxed);

                        tokio::spawn(async move {
                            let mut conn = match (authed, auth) {
                                (Some(ah), Some(am)) => Connection::new_io(
                                    ServerIo::from_unix(stream),
                                    handler,
                                    Some(ah),
                                    Some(am),
                                    None,
                                ),
                                _ => Connection::new_io(
                                    ServerIo::from_unix(stream),
                                    handler,
                                    None,
                                    None,
                                    None,
                                ),
                            };

                            if let Err(e) = conn.run().await {
                                if e.kind() != std::io::ErrorKind::ConnectionReset {
                                    warn!("Unix connection error: {}", e);
                                }
                            }

                            stats.active_connections.fetch_sub(1, Ordering::Relaxed);
                            drop(permit);
                        });
                    }
                    Err(e) => {
                        error!("Accept error: {}", e);
                    }
                }
            }
        } // end #[cfg(unix)]

        #[cfg(not(unix))]
        {
            loop {
                let accepted = tokio::select! {
                    tcp = listener.accept() => { tcp }
                    _ = shutdown.notified() => {
                        info!("Shutdown signal received, stopping accept loop");
                        break;
                    }
                };

                match accepted {
                    Ok((stream, peer_addr)) => {
                        let permit = match semaphore.clone().try_acquire_owned() {
                            Ok(p) => p,
                            Err(_) => {
                                warn!("Connection limit reached, rejecting {}", peer_addr);
                                continue;
                            }
                        };

                        let handler = self.query_handler.clone();
                        let authed = self.authed_handler.clone();
                        let auth = self.auth.clone();
                        let tls = self.tls.clone();
                        let stats = self.stats.clone();

                        stats.total_connections.fetch_add(1, Ordering::Relaxed);
                        stats.active_connections.fetch_add(1, Ordering::Relaxed);

                        tokio::spawn(async move {
                            let tls_acceptor = tls.acceptor.clone();
                            let mut conn = match (authed, auth) {
                                (Some(ah), Some(am)) => Connection::new_with_auth(
                                    stream,
                                    handler,
                                    ah,
                                    am,
                                    tls_acceptor,
                                ),
                                _ => Connection::new_io(
                                    ServerIo::from_tcp(stream),
                                    handler,
                                    None,
                                    None,
                                    tls_acceptor,
                                ),
                            };

                            if let Err(e) = conn.run().await {
                                if e.kind() != std::io::ErrorKind::ConnectionReset {
                                    warn!("Connection error: {}", e);
                                }
                            }

                            stats.active_connections.fetch_sub(1, Ordering::Relaxed);
                            drop(permit);
                        });
                    }
                    Err(e) => {
                        error!("Accept error: {}", e);
                    }
                }
            }
        } // end #[cfg(not(unix))]

        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Stop the server and drain active connections (up to 5s timeout)
    pub async fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.shutdown.notify_one();

        // Wait for active connections to drain
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let active = self.stats.active_connections.load(Ordering::Relaxed);
            if active == 0 {
                info!("All connections drained");
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                warn!("{} connections still active after drain timeout", active);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// Stop synchronously (fire-and-forget, no drain wait)
    pub fn stop_sync(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.shutdown.notify_one();
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Get server statistics
    pub fn stats(&self) -> &ServerStats {
        &self.stats
    }
}

/// Create a default query handler that returns empty results
pub fn dummy_query_handler() -> QueryHandler {
    Arc::new(|_sql: String| {
        Ok(QueryResult {
            columns: vec![],
            rows: vec![],
            command_tag: "SELECT 0".to_string(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_server_config() {
        let config = ConnectionConfig {
            host: "127.0.0.1".to_string(),
            port: 15432,
            max_connections: 10,
            ..Default::default()
        };

        assert_eq!(config.port, 15432);
    }
}
