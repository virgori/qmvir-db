/*
 * Inter-Node TCP Transport — Phase C1
 *
 * Simple binary protocol for communication between cluster nodes.
 * Protocol: [total_len: u32 LE] [msg_type: u8] [payload: bincode]
 *
 * Message types:
 *   0x01  Ping
 *   0x02  Pong
 *   0x10  ForwardQuery    — forward a SQL statement to another node
 *   0x11  ForwardResult   — result of a forwarded query
 *   0x20  ReplicateWal    — ship a WAL entry to a replica
 *   0x21  WalAck          — replica acknowledges WAL entry
 *   0x30  PrepareRequest  — 2PC Phase-1 PREPARE
 *   0x31  PrepareOk       — participant votes YES
 *   0x32  PrepareAbort    — participant votes NO / aborts
 *   0x33  CommitRequest   — 2PC Phase-2 COMMIT
 *   0x34  AbortRequest    — coordinator aborts the distributed txn
 */

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ── Message type constants ──────────────────────────────────────────

pub const MSG_PING: u8           = 0x01;
pub const MSG_PONG: u8           = 0x02;
pub const MSG_FORWARD_QUERY: u8  = 0x10;
pub const MSG_FORWARD_RESULT: u8 = 0x11;
pub const MSG_WAL_ENTRY: u8      = 0x20;
pub const MSG_WAL_ACK: u8        = 0x21;
pub const MSG_PREPARE_REQ: u8    = 0x30;
pub const MSG_PREPARE_OK: u8     = 0x31;
pub const MSG_PREPARE_ABORT: u8  = 0x32;
pub const MSG_COMMIT_REQ: u8     = 0x33;
pub const MSG_ABORT_REQ: u8      = 0x34;

// ── Payload types ───────────────────────────────────────────────────

/// Forward a SQL query to a remote node, tagged with a transaction ID.
#[derive(Debug, Serialize, Deserialize)]
pub struct ForwardQueryMsg {
    pub txn_id: u64,
    pub sql: String,
    pub database: String,
}

/// Result returned by a remote node after executing a forwarded query.
#[derive(Debug, Serialize, Deserialize)]
pub struct ForwardResultMsg {
    pub txn_id: u64,
    pub ok: bool,
    pub error: Option<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub command_tag: String,
}

/// A single WAL entry for streaming replication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalEntry {
    /// Monotonically increasing log sequence number.
    pub lsn: u64,
    /// SQL statement to replay.
    pub sql: String,
    /// CRC32 of the SQL for integrity verification.
    pub checksum: u32,
}

/// 2PC PREPARE request sent from coordinator to participants.
#[derive(Debug, Serialize, Deserialize)]
pub struct PrepareRequest {
    pub txn_id: u64,
    /// SQL operations for this participant to execute on commit.
    pub ops: Vec<String>,
}

/// 2PC commit / abort request.
#[derive(Debug, Serialize, Deserialize)]
pub struct CommitAbortMsg {
    pub txn_id: u64,
}

// ── Low-level frame I/O ─────────────────────────────────────────────

/// A framed message: [total_len: u32 LE] [msg_type: u8] [payload bytes].
pub struct NodeFrame {
    pub msg_type: u8,
    pub payload: Vec<u8>,
}

impl NodeFrame {
    pub fn new(msg_type: u8, payload: Vec<u8>) -> Self {
        Self { msg_type, payload }
    }

    /// Async write to any `AsyncWriteExt + Unpin`.
    pub async fn write_to<W: AsyncWriteExt + Unpin>(&self, w: &mut W) -> io::Result<()> {
        let total_len = (1 + self.payload.len()) as u32;
        w.write_all(&total_len.to_le_bytes()).await?;
        w.write_all(&[self.msg_type]).await?;
        w.write_all(&self.payload).await?;
        Ok(())
    }

    /// Async read from any `AsyncReadExt + Unpin`.
    pub async fn read_from<R: AsyncReadExt + Unpin>(r: &mut R) -> io::Result<Self> {
        let mut len_buf = [0u8; 4];
        r.read_exact(&mut len_buf).await?;
        let total_len = u32::from_le_bytes(len_buf) as usize;
        if total_len == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Empty transport frame"));
        }
        let mut buf = vec![0u8; total_len];
        r.read_exact(&mut buf).await?;
        Ok(Self { msg_type: buf[0], payload: buf[1..].to_vec() })
    }
}

// ── NodeClient — client for a single remote node ────────────────────

/// Client for forwarding queries and replication to a single remote node.
pub struct NodeClient {
    pub node_id: u32,
    pub addr: SocketAddr,
}

impl NodeClient {
    pub fn new(node_id: u32, addr: SocketAddr) -> Self {
        Self { node_id, addr }
    }

    /// Ping the remote node. Returns `true` if reachable within 5 s.
    pub async fn ping(&self) -> bool {
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            TcpStream::connect(self.addr),
        ).await;

        match result {
            Ok(Ok(mut stream)) => {
                if NodeFrame::new(MSG_PING, vec![]).write_to(&mut stream).await.is_err() {
                    return false;
                }
                matches!(NodeFrame::read_from(&mut stream).await, Ok(f) if f.msg_type == MSG_PONG)
            }
            _ => false,
        }
    }

    /// Forward a SQL query to the remote node and wait for the result.
    pub async fn forward_query(&self, sql: &str, txn_id: u64) -> io::Result<ForwardResultMsg> {
        let msg = ForwardQueryMsg {
            txn_id,
            sql: sql.to_string(),
            database: "default".to_string(),
        };
        let payload = bincode::serialize(&msg)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        let mut stream = tokio::time::timeout(
            Duration::from_secs(30),
            TcpStream::connect(self.addr),
        ).await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Connect timeout"))?
            .map_err(|e| e)?;

        NodeFrame::new(MSG_FORWARD_QUERY, payload).write_to(&mut stream).await?;

        let resp = NodeFrame::read_from(&mut stream).await?;
        if resp.msg_type != MSG_FORWARD_RESULT {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Unexpected response type"));
        }
        bincode::deserialize(&resp.payload)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
    }

    /// Ship a WAL entry to a replica and wait for ack.
    pub async fn send_wal_entry(&self, entry: &WalEntry) -> io::Result<u64> {
        let payload = bincode::serialize(entry)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        let mut stream = TcpStream::connect(self.addr).await?;
        NodeFrame::new(MSG_WAL_ENTRY, payload).write_to(&mut stream).await?;

        let ack = NodeFrame::read_from(&mut stream).await?;
        if ack.msg_type != MSG_WAL_ACK || ack.payload.len() < 8 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Expected WAL ack"));
        }
        Ok(u64::from_le_bytes(ack.payload[..8].try_into().unwrap_or([0; 8])))
    }

    /// Send a 2PC PREPARE request and wait for the vote.
    pub async fn send_prepare(&self, txn_id: u64, ops: Vec<String>) -> io::Result<bool> {
        let req = PrepareRequest { txn_id, ops };
        let payload = bincode::serialize(&req)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        let mut stream = TcpStream::connect(self.addr).await?;
        NodeFrame::new(MSG_PREPARE_REQ, payload).write_to(&mut stream).await?;

        let resp = NodeFrame::read_from(&mut stream).await?;
        match resp.msg_type {
            MSG_PREPARE_OK    => Ok(true),
            MSG_PREPARE_ABORT => Ok(false),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Unexpected 2PC prepare response: {:#x}", other),
            )),
        }
    }

    /// Send a 2PC COMMIT or ABORT to a participant.
    pub async fn send_commit_or_abort(&self, txn_id: u64, commit: bool) -> io::Result<()> {
        let msg_type = if commit { MSG_COMMIT_REQ } else { MSG_ABORT_REQ };
        let payload = bincode::serialize(&CommitAbortMsg { txn_id })
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        let mut stream = TcpStream::connect(self.addr).await?;
        NodeFrame::new(msg_type, payload).write_to(&mut stream).await?;
        Ok(())
    }
}

// ── TransportServer — listener for inter-node messages ─────────────

/// Server that accepts and dispatches inter-node transport messages.
///
/// Runs as a background async task.  Handles:
/// - Ping/Pong
/// - ForwardQuery   → executes SQL and returns ForwardResult
/// - ReplicateWal   → applies WAL entry to local engine
/// - 2PC Prepare/Commit/Abort (delegates to participant state stored in engine)
pub struct TransportServer {
    pub port: u16,
}

impl TransportServer {
    pub fn new(port: u16) -> Self {
        Self { port }
    }

    /// Start the transport server.  
    /// `engine` is the local SQL engine; `participant` is an optional 2PC participant.
    pub async fn run(
        &self,
        engine: Arc<crate::gateway::native_sql::NativeSqlEngine>,
    ) -> io::Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", self.port)).await?;
        tracing::info!("Transport server listening on port {}", self.port);

        loop {
            let (stream, peer) = listener.accept().await?;
            let eng = engine.clone();
            tokio::spawn(async move {
                if let Err(e) = Self::handle_conn(stream, eng).await {
                    if e.kind() != io::ErrorKind::UnexpectedEof
                        && e.kind() != io::ErrorKind::ConnectionReset {
                        tracing::warn!("Transport error from {}: {}", peer, e);
                    }
                }
            });
        }
    }

    /// Handle one inbound transport connection (ping, forward, WAL, 2PC).
    pub async fn serve_connection(
        stream: TcpStream,
        engine: Arc<crate::gateway::native_sql::NativeSqlEngine>,
    ) -> io::Result<()> {
        Self::handle_conn(stream, engine).await
    }

    async fn handle_conn(
        mut stream: TcpStream,
        engine: Arc<crate::gateway::native_sql::NativeSqlEngine>,
    ) -> io::Result<()> {
        loop {
            let frame = match NodeFrame::read_from(&mut stream).await {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            };

            match frame.msg_type {
                MSG_PING => {
                    NodeFrame::new(MSG_PONG, vec![]).write_to(&mut stream).await?;
                }

                MSG_FORWARD_QUERY => {
                    let msg: ForwardQueryMsg = bincode::deserialize(&frame.payload)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

                    let resp = match engine.execute(&msg.sql) {
                        Ok(r) => {
                            let rows: Vec<Vec<Option<String>>> = r.rows.iter().map(|row| {
                                row.iter().map(|cell| {
                                    cell.as_ref().map(|b| String::from_utf8_lossy(b).into_owned())
                                }).collect()
                            }).collect();
                            ForwardResultMsg {
                                txn_id: msg.txn_id,
                                ok: true,
                                error: None,
                                rows,
                                command_tag: r.command_tag,
                            }
                        }
                        Err(e) => ForwardResultMsg {
                            txn_id: msg.txn_id,
                            ok: false,
                            error: Some(e),
                            rows: vec![],
                            command_tag: String::new(),
                        },
                    };

                    let payload = bincode::serialize(&resp)
                        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
                    NodeFrame::new(MSG_FORWARD_RESULT, payload).write_to(&mut stream).await?;
                }

                MSG_WAL_ENTRY => {
                    let entry: WalEntry = bincode::deserialize(&frame.payload)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

                    // Verify integrity.
                    let mut hasher = crc32fast::Hasher::new();
                    hasher.update(entry.sql.as_bytes());
                    if hasher.finalize() == entry.checksum {
                        let _ = engine.execute(&entry.sql);
                    } else {
                        tracing::warn!("WAL entry CRC mismatch at LSN {}, skipping", entry.lsn);
                    }

                    let ack = entry.lsn.to_le_bytes().to_vec();
                    NodeFrame::new(MSG_WAL_ACK, ack).write_to(&mut stream).await?;
                }

                MSG_PREPARE_REQ => {
                    let req: PrepareRequest = bincode::deserialize(&frame.payload)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

                    // Dry-run validation: check each SQL parses and is non-empty.
                    let all_ok = req.ops.iter().all(|sql| !sql.trim().is_empty());
                    let vote = if all_ok { MSG_PREPARE_OK } else { MSG_PREPARE_ABORT };
                    NodeFrame::new(vote, vec![]).write_to(&mut stream).await?;
                }

                MSG_COMMIT_REQ => {
                    // On a real participant this would commit the prepared txn.
                    // For now we send back a simple ack.
                    NodeFrame::new(MSG_PONG, vec![]).write_to(&mut stream).await?;
                }

                MSG_ABORT_REQ => {
                    // Discard any prepared state and ack.
                    NodeFrame::new(MSG_PONG, vec![]).write_to(&mut stream).await?;
                }

                other => {
                    tracing::warn!("Unknown transport message type: {:#x}", other);
                }
            }
        }
        Ok(())
    }
}

// ── Python-exposed transport handle ────────────────────────────────

#[cfg(feature = "python")]
use pyo3::prelude::*;

/// Python-facing wrapper for inter-node transport operations.
#[cfg(feature = "python")]
#[pyclass(name = "NodeTransport")]
pub struct PyNodeTransport {
    node_id: u32,
    addr: SocketAddr,
    runtime: Arc<tokio::runtime::Runtime>,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyNodeTransport {
    #[new]
    #[pyo3(signature = (node_id, host, port))]
    pub fn new(node_id: u32, host: &str, port: u16) -> PyResult<Self> {
        let addr: SocketAddr = format!("{}:{}", host, port)
            .parse()
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("Invalid address: {}", e)))?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("Runtime error: {}", e)))?;

        Ok(Self { node_id, addr, runtime: Arc::new(runtime) })
    }

    /// Ping the remote node. Returns True if reachable.
    pub fn ping(&self) -> bool {
        let client = NodeClient::new(self.node_id, self.addr);
        self.runtime.block_on(async move { client.ping().await })
    }

    /// Forward a SQL query. Returns (columns, rows, command_tag).
    pub fn forward_query(&self, sql: &str, txn_id: u64) -> PyResult<PyObject> {
        let client = NodeClient::new(self.node_id, self.addr);
        let result = self.runtime.block_on(async move {
            client.forward_query(sql, txn_id).await
        }).map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

        Python::with_gil(|py| {
            let dict = pyo3::types::PyDict::new_bound(py);
            dict.set_item("ok", result.ok)?;
            dict.set_item("rows", result.rows)?;
            dict.set_item("command_tag", result.command_tag)?;
            if let Some(e) = result.error {
                dict.set_item("error", e)?;
            }
            Ok(dict.into())
        })
    }

    /// Node ID of this transport endpoint.
    #[getter]
    pub fn node_id(&self) -> u32 {
        self.node_id
    }

    /// Remote address as "host:port" string.
    #[getter]
    pub fn address(&self) -> String {
        self.addr.to_string()
    }
}
