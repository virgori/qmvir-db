/*
 * Connection Handler - Async connection management
 *
 * Handles individual client connections with zero-copy I/O.
 */

use super::auth::AuthManager;
use super::protocol::{Message, ProtocolCodec, TransactionStatus};
use ::rand::rngs::OsRng;
use ::rand::RngCore;
use bytes::{Buf, Bytes, BytesMut};
use std::collections::HashMap;
use std::io::{self, Cursor};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

static CONNECTION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Startup,
    Authentication,
    Ready,
    Query,
    Closing,
}

/// Query result to send back
#[derive(Debug, Clone)]
pub struct QueryResult {
    pub columns: Vec<(String, i32, i16)>, // name, type_oid, type_len
    pub rows: Vec<Vec<Option<Vec<u8>>>>,
    pub command_tag: String,
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
    query: String,                      // Query with parameters substituted
    cached_result: Option<QueryResult>, // Cached from Describe to avoid re-execution in Execute
}

/// Query handler callback type
pub type QueryHandler = Arc<dyn Fn(String) -> Result<QueryResult, String> + Send + Sync>;

/// Query handler with user context (for authorization).
pub type AuthQueryHandler =
    Arc<dyn Fn(String, String) -> Result<QueryResult, String> + Send + Sync>;

trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T> AsyncStream for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

/// Single client connection
pub struct Connection {
    id: u64,
    stream: Box<dyn AsyncStream>,
    read_buf: BytesMut,
    write_buf: BytesMut,
    state: ConnectionState,
    user: Arc<str>,
    database: Arc<str>,
    process_id: i32,
    secret_key: i32,
    transaction_status: TransactionStatus,
    query_handler: QueryHandler,
    /// Prepared statements: name -> (query, param_types)
    prepared_statements: HashMap<String, PreparedStatement>,
    /// Bound portals: name -> bound query with params
    portals: HashMap<String, Portal>,
    /// Optional auth-aware query handler (native engine).
    authed_handler: Option<AuthQueryHandler>,
    /// Optional auth manager for connection authentication.
    auth: Option<AuthManager>,
}

impl Connection {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn new<S>(stream: S, query_handler: QueryHandler) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let id = CONNECTION_COUNTER.fetch_add(1, Ordering::Relaxed);
        let process_id = (id & 0x7FFFFFFF) as i32;
        let secret_key = OsRng.next_u32() as i32;

        Self {
            id,
            stream: Box::new(stream),
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
            authed_handler: None,
            auth: None,
        }
    }

    /// Create a connection with authorization support.
    pub fn new_with_auth<S>(
        stream: S,
        query_handler: QueryHandler,
        authed_handler: AuthQueryHandler,
        auth: AuthManager,
    ) -> Self
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let mut conn = Self::new(stream, query_handler);
        conn.authed_handler = Some(authed_handler);
        conn.auth = Some(auth);
        conn
    }

    /// Main connection loop
    pub async fn run(&mut self) -> io::Result<()> {
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
            Message::SSLRequest => {
                // Deny SSL for now (send 'N')
                self.stream.write_all(&[b'N']).await?;
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
                    // Request cleartext password for authentication.
                    ProtocolCodec::encode_auth_cleartext(&mut self.write_buf);
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
                self.state = ConnectionState::Ready;
                ProtocolCodec::encode_ready_for_query(&mut self.write_buf, self.transaction_status);
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
                params,
            } => {
                // Extended query protocol - Bind
                // Substitute parameters into the query
                if let Some(stmt) = self.prepared_statements.get(&statement) {
                    let bound_query = Self::substitute_params(&stmt.query, &params);
                    self.portals.insert(
                        portal,
                        Portal {
                            query: bound_query,
                            cached_result: None,
                        },
                    );
                }
                ProtocolCodec::encode_bind_complete(&mut self.write_buf);
                Ok(true)
            }

            Message::Execute { portal, max_rows } => {
                // Extended query protocol - Execute
                // Use cached result from Describe if available; otherwise execute.
                let cached_and_query = self
                    .portals
                    .get_mut(&portal)
                    .map(|p| (p.cached_result.take(), p.query.clone()));
                if let Some((cached, query)) = cached_and_query {
                    let result = if let Some(c) = cached {
                        Ok(c)
                    } else {
                        Self::run_query_blocking(
                            self.id,
                            self.query_handler.clone(),
                            self.authed_handler.clone(),
                            self.user.to_string(),
                            query,
                        )
                        .await
                    };
                    match result {
                        Ok(result) => {
                            // Send data rows (row description already sent in Describe)
                            let row_limit = if max_rows > 0 {
                                max_rows as usize
                            } else {
                                result.rows.len()
                            };
                            for row in result.rows.iter().take(row_limit) {
                                let refs: Vec<Option<&[u8]>> = row
                                    .iter()
                                    .map(|v| v.as_ref().map(|b| b.as_slice()))
                                    .collect();
                                ProtocolCodec::encode_data_row(&mut self.write_buf, &refs);
                            }
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
                if let Some(ref auth) = self.auth {
                    match auth.authenticate(&self.user, &password) {
                        Ok(()) => {
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
                        Err(e) => {
                            self.send_error("FATAL", "28P01", &e).await?;
                            return Ok(false); // Close connection
                        }
                    }
                } else {
                    // No auth manager — accept all (backward compat).
                    ProtocolCodec::encode_auth_ok(&mut self.write_buf);
                    self.state = ConnectionState::Ready;
                }
                Ok(true)
            }

            Message::CancelRequest {
                process_id: _,
                secret_key: _,
            } => {
                // Handle cancel request
                Ok(false)
            }
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

        // Call the query handler off the async runtime (cluster forward uses blocking I/O).
        match Self::run_query_blocking(
            self.id,
            self.query_handler.clone(),
            self.authed_handler.clone(),
            self.user.to_string(),
            sql_trimmed.to_string(),
        )
        .await
        {
            Ok(result) => {
                // Send row description if we have columns
                if !result.columns.is_empty() {
                    ProtocolCodec::encode_row_description(&mut self.write_buf, &result.columns);

                    // Send data rows
                    for row in &result.rows {
                        let refs: Vec<Option<&[u8]>> = row
                            .iter()
                            .map(|v| v.as_ref().map(|b| b.as_slice()))
                            .collect();
                        ProtocolCodec::encode_data_row(&mut self.write_buf, &refs);
                    }
                }

                // Send command complete
                ProtocolCodec::encode_command_complete(&mut self.write_buf, &result.command_tag);
            }
            Err(e) => {
                self.send_error("ERROR", "42000", &e).await?;
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
    ) -> Result<QueryResult, String> {
        tokio::task::spawn_blocking(move || {
            crate::cluster::set_connection_id(conn_id);
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
