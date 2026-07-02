/*
 * Connection Handler - Async connection management
 *
 * Handles individual client connections with zero-copy I/O.
 */

use super::stream::ServerIo;
use super::auth::AuthManager;
use super::cancel_registry::CancelHandle;
use super::protocol::{Message, ProtocolCodec, TransactionStatus};
use super::scram::{parse_sasl_initial, ScramServer};
use super::session_pool::global_session_pool;
use ::rand::rngs::OsRng;
use ::rand::RngCore;
use bytes::{Buf, Bytes, BytesMut};
use super::protocol::format_i64_display;
use std::collections::HashMap;
use std::io::{self, Cursor};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsAcceptor;

static CONNECTION_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct CopyInState {
    table: String,
    buffer: Vec<u8>,
}

/// Connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Startup,
    Authentication,
    Ready,
    Query,
    CopyIn,
    Closing,
}

/// Query result to send back
#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<(String, i32, i16)>, // name, type_oid, type_len
    pub rows: Vec<Vec<Option<Vec<u8>>>>,
    pub command_tag: String,
    /// Column-0 int64 values without per-row cell materialization (SELECT id hot paths).
    pub packed_int64_col0: Option<Arc<Vec<i64>>>,
}

impl QueryResult {
    pub fn row_count(&self) -> usize {
        self.packed_int64_col0
            .as_ref()
            .map(|ids| ids.len())
            .unwrap_or(self.rows.len())
    }

    pub fn select_ids(row_ids: Arc<Vec<i64>>) -> Self {
        let n = row_ids.len();
        Self {
            columns: vec![(
                "id".to_string(),
                super::protocol::oid::INT8,
                8,
            )],
            rows: Vec::new(),
            command_tag: format!("SELECT {}", n),
            packed_int64_col0: Some(row_ids),
        }
    }
}

fn emit_query_result_rows(
    buf: &mut BytesMut,
    result: &QueryResult,
    formats: &[i16],
    row_limit: usize,
) {
    if let Some(ids) = &result.packed_int64_col0 {
        let limit = if row_limit == 0 {
            ids.len()
        } else {
            row_limit.min(ids.len())
        };
        for id in ids.iter().take(limit) {
            let row = vec![Some(format_i64_display(*id).into_bytes())];
            ProtocolCodec::encode_data_row_formatted(buf, &row, &result.columns, formats);
        }
        return;
    }
    let limit = if row_limit == 0 {
        result.rows.len()
    } else {
        row_limit.min(result.rows.len())
    };
    for row in result.rows.iter().take(limit) {
        ProtocolCodec::encode_data_row_formatted(buf, row, &result.columns, formats);
    }
}

/// Prepared statement info
#[derive(Debug, Clone)]
struct PreparedStatement {
    query: String,
    param_types: Vec<i32>,
}

/// Bound portal info
#[derive(Debug, Clone)]
struct Portal {
    query: String,
    result_formats: Vec<i16>,
    cached_result: Option<QueryResult>,
}

/// Query handler callback type
pub type QueryHandler = Arc<dyn Fn(String) -> Result<QueryResult, String> + Send + Sync>;

/// Query handler with user context (for authorization).
pub type AuthQueryHandler =
    Arc<dyn Fn(String, String) -> Result<QueryResult, String> + Send + Sync>;

/// Single client connection
pub struct Connection {
    id: u64,
    stream: ServerIo,
    read_buf: BytesMut,
    write_buf: BytesMut,
    state: ConnectionState,
    user: Arc<str>,
    database: Arc<str>,
    process_id: i32,
    secret_key: i32,
    transaction_status: TransactionStatus,
    query_handler: QueryHandler,
    prepared_statements: HashMap<String, PreparedStatement>,
    portals: HashMap<String, Portal>,
    authed_handler: Option<AuthQueryHandler>,
    auth: Option<AuthManager>,
    scram_server: Option<ScramServer>,
    cancel: CancelHandle,
    tls_acceptor: Option<TlsAcceptor>,
    copy_in: Option<CopyInState>,
}

impl Connection {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn new_tcp(stream: TcpStream, query_handler: QueryHandler) -> Self {
        Self::new_io(ServerIo::from_tcp(stream), query_handler, None, None, None)
    }

    pub fn new_with_auth_tcp(
        stream: TcpStream,
        query_handler: QueryHandler,
        authed_handler: AuthQueryHandler,
        auth: AuthManager,
        tls_acceptor: Option<TlsAcceptor>,
    ) -> Self {
        Self::new_io(
            ServerIo::from_tcp(stream),
            query_handler,
            Some(authed_handler),
            Some(auth),
            tls_acceptor,
        )
    }

    pub fn new_io(
        stream: ServerIo,
        query_handler: QueryHandler,
        authed_handler: Option<AuthQueryHandler>,
        auth: Option<AuthManager>,
        tls_acceptor: Option<TlsAcceptor>,
    ) -> Self {
        let id = CONNECTION_COUNTER.fetch_add(1, Ordering::Relaxed);
        let process_id = (id & 0x7FFFFFFF) as i32;
        let secret_key = OsRng.next_u32() as i32;
        let cancel = CancelHandle::register(process_id, secret_key);

        Self {
            id,
            stream,
            read_buf: BytesMut::with_capacity(8192),
            write_buf: BytesMut::with_capacity(8192),
            state: ConnectionState::Startup,
            user: Arc::from(""),
            database: Arc::from(""),
            process_id,
            secret_key,
            transaction_status: TransactionStatus::Idle,
            query_handler,
            prepared_statements: HashMap::new(),
            portals: HashMap::new(),
            authed_handler,
            auth,
            scram_server: None,
            cancel,
            tls_acceptor,
            copy_in: None,
        }
    }

    /// Create a connection with authorization support (TCP).
    pub fn new_with_auth(
        stream: TcpStream,
        query_handler: QueryHandler,
        authed_handler: AuthQueryHandler,
        auth: AuthManager,
        tls_acceptor: Option<TlsAcceptor>,
    ) -> Self {
        Self::new_with_auth_tcp(stream, query_handler, authed_handler, auth, tls_acceptor)
    }

    /// Main connection loop
    pub async fn run(&mut self) -> io::Result<()> {
        let result = self.run_inner().await;
        global_session_pool().remove_connection(self.id);
        result
    }

    async fn run_inner(&mut self) -> io::Result<()> {
        loop {
            // Read data
            let n = self.stream.read_buf(&mut self.read_buf).await?;
            if n == 0 {
                return Ok(()); // Connection closed
            }

            // Process messages
            while let Some(msg) = self.try_decode()? {
                match self.handle_message(msg).await {
                    Ok(true) => {}              // Continue
                    Ok(false) => return Ok(()), // Terminate
                    Err(e) => {
                        self.send_error("ERROR", "XX000", &e.to_string()).await?;
                    }
                }
            }

            // Flush write buffer
            if !self.write_buf.is_empty() {
                self.stream.write_all(&self.write_buf).await?;
                self.write_buf.clear();
            }
        }
    }

    /// Try to decode a message from the buffer
    fn try_decode(&mut self) -> io::Result<Option<Message>> {
        // M-11/M-12: Maximum message size (256 MB) to prevent OOM from malicious clients.
        const MAX_MESSAGE_SIZE: usize = 256 * 1024 * 1024;

        if self.read_buf.is_empty() {
            return Ok(None);
        }

        match self.state {
            ConnectionState::Startup => {
                // Startup messages don't have a type byte
                if self.read_buf.len() < 4 {
                    return Ok(None);
                }

                let len = i32::from_be_bytes([
                    self.read_buf[0],
                    self.read_buf[1],
                    self.read_buf[2],
                    self.read_buf[3],
                ]) as usize;

                if len > MAX_MESSAGE_SIZE {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("startup message too large: {} bytes", len),
                    ));
                }

                if self.read_buf.len() < len {
                    return Ok(None);
                }

                let data = self.read_buf.split_to(len);
                let mut cursor = Cursor::new(&data[..]);
                ProtocolCodec::decode_startup(&mut cursor).map(Some)
            }
            _ => {
                // Regular messages: type byte + length + body
                if self.read_buf.len() < 5 {
                    return Ok(None);
                }

                let msg_type = self.read_buf[0];
                let len = i32::from_be_bytes([
                    self.read_buf[1],
                    self.read_buf[2],
                    self.read_buf[3],
                    self.read_buf[4],
                ]) as usize;

                if len > MAX_MESSAGE_SIZE {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "message too large: {} bytes (type: {})",
                            len, msg_type as char
                        ),
                    ));
                }

                if self.read_buf.len() < 1 + len {
                    return Ok(None);
                }

                self.read_buf.advance(1); // Skip type byte
                let data = self.read_buf.split_to(len);
                let mut cursor = Cursor::new(&data[4..]); // Skip length
                ProtocolCodec::decode_message(msg_type, &mut cursor).map(Some)
            }
        }
    }

    /// Handle a decoded message
    async fn handle_message(&mut self, msg: Message) -> io::Result<bool> {
        match msg {
            Message::GssEncRequest => {
                self.stream.write_all(&[b'N']).await?;
                Ok(true)
            }

            Message::SSLRequest => {
                if let Some(acceptor) = self.tls_acceptor.clone() {
                    if !self.write_buf.is_empty() {
                        self.stream.write_all(&self.write_buf).await?;
                        self.write_buf.clear();
                    }
                    self.stream.write_all(&[b'S']).await?;
                    self.stream.flush().await?;
                    let plain = std::mem::replace(&mut self.stream, super::stream::ServerIo::Empty);
                    self.stream = plain.upgrade_tls(&acceptor).await?;
                } else {
                    self.stream.write_all(&[b'N']).await?;
                }
                Ok(true)
            }

            Message::Startup(startup) => {
                self.user = Arc::from(startup.user.as_str());
                self.database = if startup.database.is_empty() {
                    Arc::clone(&self.user)
                } else {
                    Arc::from(startup.database.as_str())
                };

                if self.auth.is_some() {
                    ProtocolCodec::encode_auth_sasl(&mut self.write_buf, &["SCRAM-SHA-256"]);
                    self.state = ConnectionState::Authentication;
                } else {
                    // No auth — accept immediately (backward compat).
                    ProtocolCodec::encode_auth_ok(&mut self.write_buf);
                    self.send_parameters().await?;
                    ProtocolCodec::encode_backend_key_data(
                        &mut self.write_buf,
                        self.process_id,
                        self.secret_key,
                    );
                    ProtocolCodec::encode_ready_for_query(
                        &mut self.write_buf,
                        self.transaction_status,
                    );
                    self.state = ConnectionState::Ready;
                }
                Ok(true)
            }

            Message::Query(sql) => {
                self.state = ConnectionState::Query;
                self.handle_simple_query(&sql).await?;
                if !matches!(self.state, ConnectionState::CopyIn) {
                    self.state = ConnectionState::Ready;
                    ProtocolCodec::encode_ready_for_query(
                        &mut self.write_buf,
                        self.transaction_status,
                    );
                }
                Ok(true)
            }

            Message::Parse {
                name,
                query,
                param_types,
            } => {
                // Extended query protocol - Parse
                // Store prepared statement for later binding
                self.prepared_statements
                    .insert(name, PreparedStatement { query, param_types });
                ProtocolCodec::encode_parse_complete(&mut self.write_buf);
                Ok(true)
            }

            Message::Bind {
                portal,
                statement,
                param_formats,
                params,
                result_formats,
            } => {
                if let Some(stmt) = self.prepared_statements.get(&statement) {
                    let bound_query = Self::substitute_params_typed(
                        &stmt.query,
                        &stmt.param_types,
                        &param_formats,
                        &params,
                    );
                    self.portals.insert(
                        portal,
                        Portal {
                            query: bound_query,
                            result_formats,
                            cached_result: None,
                        },
                    );
                }
                ProtocolCodec::encode_bind_complete(&mut self.write_buf);
                Ok(true)
            }

            Message::Execute { portal, max_rows } => {
                let cached_and_query = self.portals.get_mut(&portal).map(|p| {
                    (
                        p.cached_result.take(),
                        p.query.clone(),
                        p.result_formats.clone(),
                    )
                });
                if let Some((cached, query, result_formats)) = cached_and_query {
                    let result = if let Some(c) = cached {
                        Ok(c)
                    } else {
                        Self::run_query_blocking(
                            self.id,
                            self.query_handler.clone(),
                            self.authed_handler.clone(),
                            self.user.to_string(),
                            query,
                            &self.cancel,
                        )
                        .await
                    };
                    match result {
                        Ok(result) => {
                            let row_limit = if max_rows > 0 {
                                max_rows as usize
                            } else {
                                result.row_count()
                            };
                            emit_query_result_rows(
                                &mut self.write_buf,
                                &result,
                                &result_formats,
                                row_limit,
                            );
                            ProtocolCodec::encode_command_complete(
                                &mut self.write_buf,
                                &result.command_tag,
                            );
                        }
                        Err(e) => {
                            self.send_error("ERROR", "42000", &e).await?;
                        }
                    }
                } else {
                    ProtocolCodec::encode_command_complete(&mut self.write_buf, "SELECT 0");
                }
                Ok(true)
            }

            Message::Describe { kind, name } => {
                // Describe statement/portal
                match kind {
                    b'S' => {
                        // Describe prepared statement
                        if let Some(stmt) = self.prepared_statements.get(&name) {
                            // Execute query to get column info (dry run)
                            let test_query = Self::substitute_params_with_defaults(
                                &stmt.query,
                                stmt.param_types.len(),
                            );
                            if let Ok(result) = (self.query_handler)(test_query) {
                                // Parameter description
                                ProtocolCodec::encode_parameter_description(
                                    &mut self.write_buf,
                                    &stmt.param_types,
                                );
                                // Row description
                                if !result.columns.is_empty() {
                                    ProtocolCodec::encode_row_description(
                                        &mut self.write_buf,
                                        &result.columns,
                                    );
                                } else {
                                    ProtocolCodec::encode_no_data(&mut self.write_buf);
                                }
                            } else {
                                ProtocolCodec::encode_parameter_description(
                                    &mut self.write_buf,
                                    &stmt.param_types,
                                );
                                ProtocolCodec::encode_no_data(&mut self.write_buf);
                            }
                        } else {
                            ProtocolCodec::encode_no_data(&mut self.write_buf);
                        }
                    }
                    b'P' => {
                        // Describe portal - row description; cache result to avoid re-execution in Execute.
                        let query = self.portals.get(&name).map(|p| p.query.clone());
                        if let Some(query) = query {
                            if let Ok(result) = self.dispatch_query(&query) {
                                if !result.columns.is_empty() {
                                    ProtocolCodec::encode_row_description(
                                        &mut self.write_buf,
                                        &result.columns,
                                    );
                                } else {
                                    ProtocolCodec::encode_no_data(&mut self.write_buf);
                                }
                                if let Some(p) = self.portals.get_mut(&name) {
                                    p.cached_result = Some(result);
                                }
                            } else {
                                ProtocolCodec::encode_no_data(&mut self.write_buf);
                            }
                        } else {
                            ProtocolCodec::encode_no_data(&mut self.write_buf);
                        }
                    }
                    _ => {
                        ProtocolCodec::encode_no_data(&mut self.write_buf);
                    }
                }
                Ok(true)
            }

            Message::Sync => {
                ProtocolCodec::encode_ready_for_query(&mut self.write_buf, self.transaction_status);
                Ok(true)
            }

            Message::Flush => {
                // Flush write buffer immediately
                self.stream.write_all(&self.write_buf).await?;
                self.write_buf.clear();
                Ok(true)
            }

            Message::Close { kind, name } => {
                // Close statement/portal
                match kind {
                    b'S' => {
                        self.prepared_statements.remove(&name);
                    }
                    b'P' => {
                        self.portals.remove(&name);
                    }
                    _ => {}
                }
                self.write_buf.extend_from_slice(&[b'3', 0, 0, 0, 4]);
                Ok(true)
            }

            Message::Terminate => {
                self.state = ConnectionState::Closing;
                Ok(false) // Close connection
            }

            Message::Password(password) => {
                if self.auth.is_some() {
                    if self.scram_server.is_none() {
                        let auth = self.auth.clone().unwrap();
                        match self.handle_scram_first(&auth, &password).await {
                            Ok(true) => {}
                            Ok(false) => return Ok(false),
                            Err(e) => {
                                self.send_error("FATAL", "28P01", &e.to_string()).await?;
                                return Ok(false);
                            }
                        }
                    } else {
                        match self.handle_scram_final_bytes(&password).await {
                            Ok(true) => {}
                            Ok(false) => return Ok(false),
                            Err(e) => {
                                self.send_error("FATAL", "28P01", &e.to_string()).await?;
                                return Ok(false);
                            }
                        }
                    }
                } else {
                    ProtocolCodec::encode_auth_ok(&mut self.write_buf);
                    self.state = ConnectionState::Ready;
                }
                Ok(true)
            }

            Message::CopyData(data) => {
                if let Some(ref mut copy) = self.copy_in {
                    copy.buffer.extend_from_slice(&data);
                }
                Ok(true)
            }

            Message::CopyDone => {
                if let Some(copy) = self.copy_in.take() {
                    self.state = ConnectionState::Ready;
                    let sql = format!(
                        "COPY {} FROM STDIN QM_INLINE {}",
                        copy.table,
                        hex::encode(&copy.buffer)
                    );
                    self.handle_simple_query(&sql).await?;
                    ProtocolCodec::encode_ready_for_query(
                        &mut self.write_buf,
                        self.transaction_status,
                    );
                }
                Ok(true)
            }

            Message::CopyFail(msg) => {
                self.copy_in = None;
                self.state = ConnectionState::Ready;
                self.send_error("ERROR", "57014", &msg).await?;
                Ok(true)
            }

            Message::CancelRequest {
                process_id,
                secret_key,
            } => {
                let _ = super::cancel_registry::CancelRegistry::cancel(process_id, secret_key);
                Ok(false)
            }
        }
    }

    async fn handle_scram_first(&mut self, auth: &AuthManager, raw: &[u8]) -> io::Result<bool> {
        let (_, data) = parse_sasl_initial(raw)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let client_first = std::str::from_utf8(&data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let secret = auth
            .scram_secret_for(&self.user)
            .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e))?;
        let (server_first, scram) =
            ScramServer::new(client_first, secret, false).map_err(|e| {
                io::Error::new(io::ErrorKind::PermissionDenied, e)
            })?;
        self.scram_server = Some(scram);
        ProtocolCodec::encode_auth_sasl_continue(&mut self.write_buf, &server_first);
        Ok(true)
    }

    async fn handle_scram_final_bytes(&mut self, raw: &[u8]) -> io::Result<bool> {
        let client_final = std::str::from_utf8(raw)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let scram = self
            .scram_server
            .take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "SCRAM out of order"))?;
        let server_final = scram
            .finish(client_final)
            .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e))?;
        ProtocolCodec::encode_auth_sasl_final(&mut self.write_buf, &server_final);
        self.complete_authentication().await?;
        Ok(true)
    }

    async fn complete_authentication(&mut self) -> io::Result<()> {
        ProtocolCodec::encode_auth_ok(&mut self.write_buf);
        self.send_parameters().await?;
        ProtocolCodec::encode_backend_key_data(
            &mut self.write_buf,
            self.process_id,
            self.secret_key,
        );
        ProtocolCodec::encode_ready_for_query(&mut self.write_buf, self.transaction_status);
        self.state = ConnectionState::Ready;
        Ok(())
    }

    fn update_txn_status(&mut self, sql: &str, had_error: bool) {
        if had_error {
            self.transaction_status = TransactionStatus::Failed;
            return;
        }
        let up = sql.trim().to_ascii_uppercase();
        if up == "BEGIN" || up.starts_with("BEGIN ") {
            self.transaction_status = TransactionStatus::InTransaction;
        } else if up == "COMMIT"
            || up.starts_with("COMMIT ")
            || up == "ROLLBACK"
            || up.starts_with("ROLLBACK ")
        {
            self.transaction_status = TransactionStatus::Idle;
        }
    }

    /// Send server parameters
    async fn send_parameters(&mut self) -> io::Result<()> {
        let params = [
            ("server_version", "15.0"),
            ("server_encoding", "UTF8"),
            ("client_encoding", "UTF8"),
            ("DateStyle", "ISO, MDY"),
            ("TimeZone", "UTC"),
            ("integer_datetimes", "on"),
            ("standard_conforming_strings", "on"),
        ];

        for (name, value) in params {
            ProtocolCodec::encode_parameter_status(&mut self.write_buf, name, value);
        }

        Ok(())
    }

    /// Handle simple query (Q message)
    async fn handle_simple_query(&mut self, sql: &str) -> io::Result<()> {
        let sql_trimmed = sql.trim();

        if sql_trimmed.is_empty() {
            ProtocolCodec::encode_empty_query(&mut self.write_buf);
            return Ok(());
        }

        if let Some(table) = Self::parse_copy_stdin_table(sql_trimmed) {
            self.copy_in = Some(CopyInState {
                table,
                buffer: Vec::new(),
            });
            self.state = ConnectionState::CopyIn;
            ProtocolCodec::encode_copy_in_response(&mut self.write_buf, 0);
            return Ok(());
        }

        // Call the query handler off the async runtime (cluster forward uses blocking I/O).
        match Self::run_query_blocking(
            self.id,
            self.query_handler.clone(),
            self.authed_handler.clone(),
            self.user.to_string(),
            sql_trimmed.to_string(),
            &self.cancel,
        )
        .await
        {
            Ok(result) => {
                // Send row description if we have columns
                if !result.columns.is_empty() {
                    ProtocolCodec::encode_row_description(&mut self.write_buf, &result.columns);

                    // Send data rows
                    let text_formats = vec![0i16; result.columns.len()];
                    emit_query_result_rows(
                        &mut self.write_buf,
                        &result,
                        &text_formats,
                        result.row_count(),
                    );
                }

                // Send command complete
                ProtocolCodec::encode_command_complete(&mut self.write_buf, &result.command_tag);
                self.update_txn_status(sql_trimmed, false);
            }
            Err(e) => {
                self.send_error("ERROR", "42000", &e).await?;
                self.update_txn_status(sql_trimmed, true);
            }
        }

        Ok(())
    }

    /// Run a query on the blocking thread pool (safe for cluster TCP forward).
    async fn run_query_blocking(
        conn_id: u64,
        handler: QueryHandler,
        authed: Option<AuthQueryHandler>,
        user: String,
        sql: String,
        cancel: &CancelHandle,
    ) -> Result<QueryResult, String> {
        if cancel.is_cancelled() {
            return Err("query canceled".to_string());
        }
        let cancel_flag = cancel.flag();
        tokio::task::spawn_blocking(move || {
            crate::cluster::set_connection_id(conn_id);
            if cancel_flag.load(Ordering::Acquire) {
                crate::cluster::clear_connection_id();
                return Err("query canceled".to_string());
            }
            let result = if let Some(ref h) = authed {
                h(sql, user)
            } else {
                handler(sql)
            };
            crate::cluster::clear_connection_id();
            result
        })
        .await
        .map_err(|e| format!("query worker failed: {e}"))?
    }

    /// Synchronous dispatch (non-async callers only).
    fn dispatch_query(&self, sql: &str) -> Result<QueryResult, String> {
        if let Some(ref h) = self.authed_handler {
            h(sql.to_string(), self.user.to_string())
        } else {
            (self.query_handler)(sql.to_string())
        }
    }

    /// Send error response
    async fn send_error(&mut self, severity: &str, code: &str, message: &str) -> io::Result<()> {
        ProtocolCodec::encode_error(&mut self.write_buf, severity, code, message);
        Ok(())
    }

    fn parse_copy_stdin_table(sql: &str) -> Option<String> {
        let trimmed = sql.trim();
        let up = trimmed.to_ascii_uppercase();
        if !up.starts_with("COPY ") || !up.contains("FROM STDIN") || up.contains("QM_INLINE") {
            return None;
        }
        let rest = trimmed[5..].trim();
        let end = rest.find(char::is_whitespace)?;
        Some(rest[..end].trim_matches('"').to_string())
    }

    fn param_format_code(formats: &[i16], index: usize) -> i16 {
        if formats.is_empty() {
            0
        } else if formats.len() == 1 {
            formats[0]
        } else {
            formats.get(index).copied().unwrap_or(0)
        }
    }

    fn decode_binary_param(oid: i32, bytes: &[u8]) -> String {
        match oid {
            16 => {
                let b = bytes.first().copied().unwrap_or(0);
                if b == 0 {
                    "false".to_string()
                } else {
                    "true".to_string()
                }
            }
            21 if bytes.len() >= 2 => i16::from_be_bytes([bytes[0], bytes[1]]).to_string(),
            23 if bytes.len() >= 4 => {
                i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]).to_string()
            }
            20 if bytes.len() >= 8 => {
                i64::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                    bytes[7],
                ])
                .to_string()
            }
            700 if bytes.len() >= 4 => {
                f32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]).to_string()
            }
            701 if bytes.len() >= 8 => {
                f64::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                    bytes[7],
                ])
                .to_string()
            }
            17 if bytes.len() >= 4 => {
                format!(
                    "'\\x{}'",
                    bytes
                        .iter()
                        .map(|b| format!("{:02x}", b))
                        .collect::<String>()
                )
            }
            _ => match std::str::from_utf8(bytes) {
                Ok(s) => {
                    if s.parse::<i64>().is_ok() || s.parse::<f64>().is_ok() {
                        s.to_string()
                    } else {
                        format!("'{}'", s.replace('\'', "''"))
                    }
                }
                Err(_) => format!(
                    "'\\x{}'",
                    bytes
                        .iter()
                        .map(|b| format!("{:02x}", b))
                        .collect::<String>()
                ),
            },
        }
    }

    fn format_param_sql(oid: i32, format: i16, bytes: &[u8]) -> String {
        if format == 0 {
            match std::str::from_utf8(bytes) {
                Ok(s) => {
                    if s.parse::<i64>().is_ok() || s.parse::<f64>().is_ok() {
                        s.to_string()
                    } else {
                        format!("'{}'", s.replace('\'', "''"))
                    }
                }
                Err(_) => "NULL".to_string(),
            }
        } else {
            Self::decode_binary_param(oid, bytes)
        }
    }

    fn substitute_params_typed(
        query: &str,
        param_types: &[i32],
        param_formats: &[i16],
        params: &[Option<Bytes>],
    ) -> String {
        let mut result = query.to_string();
        for (i, param) in params.iter().enumerate() {
            let placeholder = format!("${}", i + 1);
            let oid = param_types.get(i).copied().unwrap_or(0);
            let format = Self::param_format_code(param_formats, i);
            let value = match param {
                Some(bytes) => Self::format_param_sql(oid, format, bytes),
                None => "NULL".to_string(),
            };
            result = result.replace(&placeholder, &value);
        }
        result
    }

    /// Substitute $1, $2, ... placeholders with actual parameter values
    fn substitute_params(query: &str, params: &[Option<Bytes>]) -> String {
        let mut result = query.to_string();
        // Replace $n with actual parameter values (1-indexed)
        for (i, param) in params.iter().enumerate() {
            let placeholder = format!("${}", i + 1);
            let value = match param {
                Some(bytes) => {
                    // Try to parse as UTF-8 string
                    match std::str::from_utf8(bytes) {
                        Ok(s) => {
                            // Escape single quotes and wrap in quotes for strings
                            // Check if it's a number
                            if s.parse::<i64>().is_ok() || s.parse::<f64>().is_ok() {
                                s.to_string()
                            } else {
                                format!("'{}'", s.replace('\'', "''"))
                            }
                        }
                        Err(_) => "NULL".to_string(),
                    }
                }
                None => "NULL".to_string(),
            };
            result = result.replace(&placeholder, &value);
        }
        result
    }

    /// Substitute parameters with default values for schema discovery
    fn substitute_params_with_defaults(query: &str, num_params: usize) -> String {
        let mut result = query.to_string();
        for i in 1..=num_params {
            let placeholder = format!("${}", i);
            result = result.replace(&placeholder, "0");
        }
        result
    }
}
