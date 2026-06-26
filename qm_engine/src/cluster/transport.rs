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

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream as StdTcpStream};
use std::sync::Arc;
use std::time::Duration;

use crate::cluster::two_phase_commit::TwoPhaseParticipant;
use crate::cluster::tls_config::{self, ClusterTlsConfig};
use crate::cluster::wal_apply::{apply_wal_entry, WalApplyTracker};
use crate::cluster::fencing;
use crate::cluster::meta_network;
use crate::cluster::topology;
use crate::cluster::wal_buffer;
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
pub const MSG_TOPOLOGY_JOIN: u8  = 0x40;
pub const MSG_TOPOLOGY_LEAVE: u8 = 0x41;
pub const MSG_TOPOLOGY_ACK: u8   = 0x42;
pub const MSG_WAL_CATCHUP_REQ: u8 = 0x43;
pub const MSG_WAL_CATCHUP_RESP: u8 = 0x44;
pub const MSG_META_APPEND: u8    = 0x50;
pub const MSG_META_ACK: u8       = 0x51;

/// Forward a SQL query to a remote node, tagged with a transaction ID.
#[derive(Debug, Serialize, Deserialize)]
pub struct ForwardQueryMsg {
    pub txn_id: u64,
    pub sql: String,
    pub database: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub epoch: u64,
}

/// Join a shard to the cluster catalog on a data node.
#[derive(Debug, Serialize, Deserialize)]
pub struct TopologyJoinMsg {
    pub shard_id: u32,
    pub primary: SocketAddr,
    #[serde(default)]
    pub replicas: Vec<SocketAddr>,
}

/// Remove a shard from the cluster catalog.
#[derive(Debug, Serialize, Deserialize)]
pub struct TopologyLeaveMsg {
    pub shard_id: u32,
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
        Ok(Self {
            msg_type: buf[0],
            payload: buf[1..].to_vec(),
        })
    }

    /// Blocking frame write (gateway sync query path).
    pub fn write_to_sync(&self, w: &mut StdTcpStream) -> io::Result<()> {
        self.write_to_io(w)
    }

    pub fn write_to_io<W: Write>(&self, w: &mut W) -> io::Result<()> {
        let total_len = (1 + self.payload.len()) as u32;
        w.write_all(&total_len.to_le_bytes())?;
        w.write_all(&[self.msg_type])?;
        w.write_all(&self.payload)?;
        Ok(())
    }

    /// Blocking frame read (gateway sync query path).
    pub fn read_from_sync(r: &mut StdTcpStream) -> io::Result<Self> {
        Self::read_from_io(r)
    }

    pub fn read_from_io<R: Read>(r: &mut R) -> io::Result<Self> {
        let mut len_buf = [0u8; 4];
        r.read_exact(&mut len_buf)?;
        Self::decode_payload(len_buf, |n| {
            let mut buf = vec![0u8; n];
            r.read_exact(&mut buf)?;
            Ok(buf)
        })
    }

    fn decode_payload<F>(len_buf: [u8; 4], read_body: F) -> io::Result<Self>
    where
        F: FnOnce(usize) -> io::Result<Vec<u8>>,
    {
        let total_len = u32::from_le_bytes(len_buf) as usize;
        if total_len == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Empty transport frame"));
        }
        let buf = read_body(total_len)?;
        Ok(Self {
            msg_type: buf[0],
            payload: buf[1..].to_vec(),
        })
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

    /// Blocking ping (CLI health probes).
    pub fn ping_blocking(&self, timeout: Duration) -> io::Result<bool> {
        let tls = tls_config::ClusterTlsConfig::from_env();
        let mut stream = tls_config::connect_blocking(self.addr, &tls, timeout)?;
        NodeFrame::new(MSG_PING, vec![]).write_to_io(&mut stream)?;
        let resp = NodeFrame::read_from_io(&mut stream)?;
        Ok(resp.msg_type == MSG_PONG)
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

    /// Forward a SQL query using blocking I/O (safe from sync gateway handlers).
    pub fn forward_query_blocking(
        &self,
        sql: &str,
        txn_id: u64,
        user: Option<&str>,
    ) -> io::Result<ForwardResultMsg> {
        let msg = ForwardQueryMsg {
            txn_id,
            sql: sql.to_string(),
            database: "default".to_string(),
            user: user.map(str::to_string),
            epoch: fencing::current_epoch(),
        };
        let payload = bincode::serialize(&msg)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        let tls = ClusterTlsConfig::from_env();
        let timeout = Duration::from_secs(30);
        let mut stream = tls_config::connect_blocking(self.addr, &tls, timeout)?;

        NodeFrame::new(MSG_FORWARD_QUERY, payload).write_to_io(&mut stream)?;

        let resp = NodeFrame::read_from_io(&mut stream)?;
        if resp.msg_type != MSG_FORWARD_RESULT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unexpected response type",
            ));
        }
        bincode::deserialize(&resp.payload)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
    }

    /// Forward a SQL query to the remote node and wait for the result.
    pub async fn forward_query(&self, sql: &str, txn_id: u64) -> io::Result<ForwardResultMsg> {
        let msg = ForwardQueryMsg {
            txn_id,
            sql: sql.to_string(),
            database: "default".to_string(),
            user: None,
            epoch: fencing::current_epoch(),
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

    /// Ship a WAL entry to a replica (blocking).
    pub fn send_topology_join_blocking(&self, msg: &TopologyJoinMsg) -> io::Result<()> {
        let payload = bincode::serialize(msg)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        let tls = ClusterTlsConfig::from_env();
        let mut stream =
            tls_config::connect_blocking(self.addr, &tls, Duration::from_secs(10))?;
        NodeFrame::new(MSG_TOPOLOGY_JOIN, payload).write_to_io(&mut stream)?;
        let ack = NodeFrame::read_from_io(&mut stream)?;
        if ack.msg_type != MSG_TOPOLOGY_ACK {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "expected topology ack"));
        }
        Ok(())
    }

    pub fn send_topology_leave_blocking(&self, shard_id: u32) -> io::Result<()> {
        let msg = TopologyLeaveMsg { shard_id };
        let payload = bincode::serialize(&msg)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        let tls = ClusterTlsConfig::from_env();
        let mut stream =
            tls_config::connect_blocking(self.addr, &tls, Duration::from_secs(10))?;
        NodeFrame::new(MSG_TOPOLOGY_LEAVE, payload).write_to_io(&mut stream)?;
        let ack = NodeFrame::read_from_io(&mut stream)?;
        if ack.msg_type != MSG_TOPOLOGY_ACK {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "expected topology ack"));
        }
        Ok(())
    }

    pub fn send_meta_append_blocking(&self, payload: &[u8]) -> io::Result<()> {
        let tls = ClusterTlsConfig::from_env();
        let mut stream =
            tls_config::connect_blocking(self.addr, &tls, Duration::from_secs(10))?;
        NodeFrame::new(MSG_META_APPEND, payload.to_vec()).write_to_io(&mut stream)?;
        let ack = NodeFrame::read_from_io(&mut stream)?;
        if ack.msg_type != MSG_META_ACK {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected meta ack",
            ));
        }
        Ok(())
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

    /// Blocking WAL ship (gateway sync path / background replication threads).
    pub fn send_wal_entry_blocking(&self, entry: &WalEntry) -> io::Result<u64> {
        let payload = bincode::serialize(entry)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        let tls = ClusterTlsConfig::from_env();
        let timeout = Duration::from_secs(30);
        let mut stream = tls_config::connect_blocking(self.addr, &tls, timeout)?;

        NodeFrame::new(MSG_WAL_ENTRY, payload).write_to_io(&mut stream)?;

        let ack = NodeFrame::read_from_io(&mut stream)?;
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

    /// Send a 2PC COMMIT or ABORT to a participant and wait for acknowledgement.
    pub async fn send_commit_or_abort(&self, txn_id: u64, commit: bool) -> io::Result<()> {
        let msg_type = if commit { MSG_COMMIT_REQ } else { MSG_ABORT_REQ };
        let payload = bincode::serialize(&CommitAbortMsg { txn_id })
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        let mut stream = TcpStream::connect(self.addr).await?;
        NodeFrame::new(msg_type, payload).write_to(&mut stream).await?;
        let resp = NodeFrame::read_from(&mut stream).await?;
        match resp.msg_type {
            MSG_PONG => Ok(()),
            MSG_PREPARE_ABORT if commit => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("2PC commit for txn {txn_id} rejected"),
            )),
            MSG_PREPARE_ABORT => Ok(()),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Unexpected 2PC commit response: {:#x}", other),
            )),
        }
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
    pub async fn run(
        &self,
        engine: Arc<crate::gateway::native_sql::NativeSqlEngine>,
        cluster_runtime: Option<Arc<crate::cluster::runtime::ClusterRuntime>>,
    ) -> io::Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", self.port)).await?;
        tracing::info!("Transport server listening on port {}", self.port);
        let participant = Arc::new(TwoPhaseParticipant::new(Arc::clone(&engine)));
        let wal_tracker = Arc::new(WalApplyTracker::new());
        let tls = ClusterTlsConfig::from_env();
        let tls_acceptor = tls.server.as_ref().map(|cfg| tokio_rustls::TlsAcceptor::from(Arc::clone(cfg)));

        loop {
            let (stream, peer) = listener.accept().await?;
            let eng = engine.clone();
            let part = Arc::clone(&participant);
            let wal = Arc::clone(&wal_tracker);
            let rt = cluster_runtime.clone();
            let acceptor = tls_acceptor.clone();
            tokio::spawn(async move {
                let result = if let Some(acc) = acceptor {
                    match acc.accept(stream).await {
                        Ok(tls_stream) => {
                            Self::handle_conn(tls_stream, eng, part, wal, rt).await
                        }
                        Err(e) => {
                            tracing::warn!("TLS accept from {peer}: {e}");
                            return;
                        }
                    }
                } else {
                    Self::handle_conn(stream, eng, part, wal, rt).await
                };
                if let Err(e) = result {
                    if e.kind() != io::ErrorKind::UnexpectedEof
                        && e.kind() != io::ErrorKind::ConnectionReset
                    {
                        tracing::warn!("Transport error from {}: {}", peer, e);
                    }
                }
            });
        }
    }

    /// Handle one inbound connection with a shared 2PC participant (multi-request safe).
    pub async fn serve_connection_shared(
        stream: TcpStream,
        engine: Arc<crate::gateway::native_sql::NativeSqlEngine>,
        participant: Arc<TwoPhaseParticipant>,
        wal_tracker: Arc<WalApplyTracker>,
    ) -> io::Result<()> {
        Self::serve_on_stream(stream, engine, participant, wal_tracker).await
    }

    /// Accept one inbound TCP connection, optionally wrapping TLS per env, then serve.
    pub async fn accept_and_serve(
        stream: TcpStream,
        engine: Arc<crate::gateway::native_sql::NativeSqlEngine>,
        participant: Arc<TwoPhaseParticipant>,
        wal_tracker: Arc<WalApplyTracker>,
    ) {
        let tls = ClusterTlsConfig::from_env();
        let result = if let Some(server) = tls.server {
            let acceptor = tokio_rustls::TlsAcceptor::from(server);
            match acceptor.accept(stream).await {
                Ok(tls_stream) => {
                    Self::serve_on_stream(tls_stream, engine, participant, wal_tracker).await
                }
                Err(_) => return,
            }
        } else {
            Self::serve_on_stream(stream, engine, participant, wal_tracker).await
        };
        let _ = result;
    }

    /// Same as [`serve_connection_shared`] but accepts any async I/O stream (e.g. TLS).
    pub async fn serve_on_stream<S>(
        stream: S,
        engine: Arc<crate::gateway::native_sql::NativeSqlEngine>,
        participant: Arc<TwoPhaseParticipant>,
        wal_tracker: Arc<WalApplyTracker>,
    ) -> io::Result<()>
    where
        S: AsyncReadExt + AsyncWriteExt + Unpin,
    {
        Self::handle_conn(stream, engine, participant, wal_tracker, None).await
    }

    /// Handle one inbound transport connection (ping, forward, WAL, 2PC).
    pub async fn serve_connection(
        stream: TcpStream,
        engine: Arc<crate::gateway::native_sql::NativeSqlEngine>,
    ) -> io::Result<()> {
        let participant = Arc::new(TwoPhaseParticipant::new(Arc::clone(&engine)));
        let wal_tracker = Arc::new(WalApplyTracker::new());
        Self::handle_conn(stream, engine, participant, wal_tracker, None).await
    }

    async fn handle_conn<S>(
        mut stream: S,
        engine: Arc<crate::gateway::native_sql::NativeSqlEngine>,
        participant: Arc<TwoPhaseParticipant>,
        wal_tracker: Arc<WalApplyTracker>,
        cluster_runtime: Option<Arc<crate::cluster::runtime::ClusterRuntime>>,
    ) -> io::Result<()>
    where
        S: AsyncReadExt + AsyncWriteExt + Unpin,
    {
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

                    let cfg = super::config::ClusterNodeConfig::from_env();
                    if !fencing::accept_epoch(msg.epoch, cfg.fencing_enabled) {
                        let resp = ForwardResultMsg {
                            txn_id: msg.txn_id,
                            ok: false,
                            error: Some(format!(
                                "fenced: stale epoch {} < {}",
                                msg.epoch,
                                fencing::current_epoch()
                            )),
                            rows: vec![],
                            command_tag: String::new(),
                        };
                        let payload = bincode::serialize(&resp)
                            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
                        NodeFrame::new(MSG_FORWARD_RESULT, payload)
                            .write_to(&mut stream)
                            .await?;
                        continue;
                    }

                    let exec = if let Some(ref user) = msg.user {
                        engine.execute_as(&msg.sql, user)
                    } else {
                        engine.execute(&msg.sql)
                    };

                    let resp = match exec {
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

                    if let Err(e) = apply_wal_entry(&wal_tracker, &engine, &entry) {
                        tracing::warn!("WAL apply at LSN {} failed: {e}", entry.lsn);
                    }

                    let ack = entry.lsn.to_le_bytes().to_vec();
                    NodeFrame::new(MSG_WAL_ACK, ack).write_to(&mut stream).await?;
                }

                MSG_PREPARE_REQ => {
                    let req: PrepareRequest = bincode::deserialize(&frame.payload)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

                    let vote = if participant.prepare(req.txn_id, req.ops) {
                        MSG_PREPARE_OK
                    } else {
                        MSG_PREPARE_ABORT
                    };
                    NodeFrame::new(vote, vec![]).write_to(&mut stream).await?;
                }

                MSG_COMMIT_REQ => {
                    let msg: CommitAbortMsg = bincode::deserialize(&frame.payload)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                    let ok = participant.commit(msg.txn_id);
                    let vote = if ok { MSG_PONG } else { MSG_PREPARE_ABORT };
                    NodeFrame::new(vote, vec![]).write_to(&mut stream).await?;
                }

                MSG_ABORT_REQ => {
                    let msg: CommitAbortMsg = bincode::deserialize(&frame.payload)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                    participant.abort(msg.txn_id);
                    NodeFrame::new(MSG_PONG, vec![]).write_to(&mut stream).await?;
                }

                MSG_TOPOLOGY_JOIN => {
                    let msg: TopologyJoinMsg = bincode::deserialize(&frame.payload)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                    if let Some(ref rt) = cluster_runtime {
                        topology::apply_join(rt, msg.shard_id, msg.primary, msg.replicas);
                    }
                    NodeFrame::new(MSG_TOPOLOGY_ACK, vec![]).write_to(&mut stream).await?;
                }

                MSG_TOPOLOGY_LEAVE => {
                    let msg: TopologyLeaveMsg = bincode::deserialize(&frame.payload)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                    if let Some(ref rt) = cluster_runtime {
                        topology::apply_leave(rt, msg.shard_id);
                    }
                    NodeFrame::new(MSG_TOPOLOGY_ACK, vec![]).write_to(&mut stream).await?;
                }

                MSG_WAL_CATCHUP_REQ => {
                    #[derive(serde::Deserialize)]
                    struct WalCatchupReq {
                        from_lsn: u64,
                    }
                    let req: WalCatchupReq = bincode::deserialize(&frame.payload)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                    let entries = wal_buffer::entries_after_lsn(req.from_lsn);
                    let payload = bincode::serialize(&entries)
                        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
                    NodeFrame::new(MSG_WAL_CATCHUP_RESP, payload)
                        .write_to(&mut stream)
                        .await?;
                }

                MSG_META_APPEND => {
                    let msg: meta_network::MetaAppendMsg = bincode::deserialize(&frame.payload)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                    if let Some(ref rt) = cluster_runtime {
                        meta_network::apply_meta_to_runtime(rt, &msg.entries);
                    }
                    NodeFrame::new(MSG_META_ACK, vec![]).write_to(&mut stream).await?;
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
