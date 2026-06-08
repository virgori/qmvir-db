/*
 * PostgreSQL Wire Protocol v3 Implementation
 *
 * Message types and encoding/decoding for PG protocol.
 * Reference: https://www.postgresql.org/docs/current/protocol-message-formats.html
 */

use bytes::{Buf, BufMut, Bytes, BytesMut};
use std::io::{self, Cursor};

/// PostgreSQL message types (frontend -> backend)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrontendMessage {
    Query = b'Q',
    Parse = b'P',
    Bind = b'B',
    Execute = b'E',
    Describe = b'D',
    Sync = b'S',
    Flush = b'H',
    Close = b'C',
    Terminate = b'X',
    PasswordMessage = b'p',
    CopyData = b'd',
    CopyDone = b'c',
    CopyFail = b'f',
}

/// PostgreSQL message types (backend -> frontend)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BackendMessage {
    AuthenticationOk = b'R',
    ParameterStatus = b'S',
    BackendKeyData = b'K',
    ReadyForQuery = b'Z',
    RowDescription = b'T',
    DataRow = b'D',
    CommandComplete = b'C',
    EmptyQueryResponse = b'I',
    ErrorResponse = b'E',
    NoticeResponse = b'N',
    ParseComplete = b'1',
    BindComplete = b'2',
    CloseComplete = b'3',
    NoData = b'n',
    ParameterDescription = b't',
    CopyInResponse = b'G',
    CopyOutResponse = b'H',
    CopyDone = b'c',
}

/// Transaction status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionStatus {
    Idle = b'I' as isize,
    InTransaction = b'T' as isize,
    Failed = b'E' as isize,
}

/// Parsed frontend message
#[derive(Debug)]
pub enum Message {
    Startup(StartupMessage),
    Query(String),
    Parse {
        name: String,
        query: String,
        param_types: Vec<i32>,
    },
    Bind {
        portal: String,
        statement: String,
        params: Vec<Option<Bytes>>,
    },
    Execute {
        portal: String,
        max_rows: i32,
    },
    Describe {
        kind: u8,
        name: String,
    },
    Sync,
    Flush,
    Close {
        kind: u8,
        name: String,
    },
    Terminate,
    Password(String),
    CancelRequest {
        process_id: i32,
        secret_key: i32,
    },
    SSLRequest,
}

/// Startup message parameters
#[derive(Debug, Default)]
pub struct StartupMessage {
    pub protocol_version: i32,
    pub user: String,
    pub database: String,
    pub options: Vec<(String, String)>,
}

/// Protocol codec for encoding/decoding messages
pub struct ProtocolCodec;

impl ProtocolCodec {
    /// Decode a startup message
    pub fn decode_startup(buf: &mut Cursor<&[u8]>) -> io::Result<Message> {
        let len = buf.get_i32() as usize;
        if buf.remaining() < len - 4 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete startup",
            ));
        }

        let protocol = buf.get_i32();

        // SSL request
        if protocol == 80877103 {
            return Ok(Message::SSLRequest);
        }

        // Cancel request
        if protocol == 80877102 {
            let process_id = buf.get_i32();
            let secret_key = buf.get_i32();
            return Ok(Message::CancelRequest {
                process_id,
                secret_key,
            });
        }

        // Regular startup
        let mut msg = StartupMessage {
            protocol_version: protocol,
            ..Default::default()
        };

        // Read parameters
        loop {
            let key = Self::read_cstring(buf)?;
            if key.is_empty() {
                break;
            }
            let value = Self::read_cstring(buf)?;

            match key.as_str() {
                "user" => msg.user = value,
                "database" => msg.database = value,
                _ => msg.options.push((key, value)),
            }
        }

        Ok(Message::Startup(msg))
    }

    /// Decode a regular message (after startup)
    pub fn decode_message(msg_type: u8, buf: &mut Cursor<&[u8]>) -> io::Result<Message> {
        match msg_type {
            b'Q' => {
                let query = Self::read_cstring(buf)?;
                Ok(Message::Query(query))
            }
            b'P' => {
                let name = Self::read_cstring(buf)?;
                let query = Self::read_cstring(buf)?;
                let n_params = buf.get_i16() as usize;
                let mut param_types = Vec::with_capacity(n_params);
                for _ in 0..n_params {
                    param_types.push(buf.get_i32());
                }
                Ok(Message::Parse {
                    name,
                    query,
                    param_types,
                })
            }
            b'B' => {
                let portal = Self::read_cstring(buf)?;
                let statement = Self::read_cstring(buf)?;

                // Format codes
                let n_formats = buf.get_i16() as usize;
                for _ in 0..n_formats {
                    let _ = buf.get_i16();
                }

                // Parameters
                let n_params = buf.get_i16() as usize;
                let mut params = Vec::with_capacity(n_params);
                for _ in 0..n_params {
                    let len = buf.get_i32();
                    if len == -1 {
                        params.push(None);
                    } else {
                        let mut data = vec![0u8; len as usize];
                        buf.copy_to_slice(&mut data);
                        params.push(Some(Bytes::from(data)));
                    }
                }

                Ok(Message::Bind {
                    portal,
                    statement,
                    params,
                })
            }
            b'E' => {
                let portal = Self::read_cstring(buf)?;
                let max_rows = buf.get_i32();
                Ok(Message::Execute { portal, max_rows })
            }
            b'D' => {
                let kind = buf.get_u8();
                let name = Self::read_cstring(buf)?;
                Ok(Message::Describe { kind, name })
            }
            b'S' => Ok(Message::Sync),
            b'H' => Ok(Message::Flush),
            b'C' => {
                let kind = buf.get_u8();
                let name = Self::read_cstring(buf)?;
                Ok(Message::Close { kind, name })
            }
            b'X' => Ok(Message::Terminate),
            b'p' => {
                let password = Self::read_cstring(buf)?;
                Ok(Message::Password(password))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown message type: {}", msg_type as char),
            )),
        }
    }

    /// Read null-terminated string
    fn read_cstring(buf: &mut Cursor<&[u8]>) -> io::Result<String> {
        let start = buf.position() as usize;
        let data = buf.get_ref();

        let mut end = start;
        while end < data.len() && data[end] != 0 {
            end += 1;
        }

        if end >= data.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "missing null terminator",
            ));
        }

        let s = String::from_utf8_lossy(&data[start..end]).to_string();
        buf.set_position((end + 1) as u64);
        Ok(s)
    }

    // =========================================================================
    // Encoding (Backend -> Frontend)
    // =========================================================================

    /// Encode AuthenticationOk
    pub fn encode_auth_ok(buf: &mut BytesMut) {
        buf.put_u8(b'R');
        buf.put_i32(8); // length
        buf.put_i32(0); // auth ok
    }

    /// Encode AuthenticationCleartextPassword
    pub fn encode_auth_cleartext(buf: &mut BytesMut) {
        buf.put_u8(b'R');
        buf.put_i32(8);
        buf.put_i32(3); // cleartext password
    }

    /// Encode AuthenticationMD5Password
    pub fn encode_auth_md5(buf: &mut BytesMut, salt: &[u8; 4]) {
        buf.put_u8(b'R');
        buf.put_i32(12);
        buf.put_i32(5); // MD5
        buf.put_slice(salt);
    }

    /// Encode ParameterStatus
    pub fn encode_parameter_status(buf: &mut BytesMut, name: &str, value: &str) {
        buf.put_u8(b'S');
        let len = 4 + name.len() + 1 + value.len() + 1;
        buf.put_i32(len as i32);
        buf.put_slice(name.as_bytes());
        buf.put_u8(0);
        buf.put_slice(value.as_bytes());
        buf.put_u8(0);
    }

    /// Encode BackendKeyData
    pub fn encode_backend_key_data(buf: &mut BytesMut, process_id: i32, secret_key: i32) {
        buf.put_u8(b'K');
        buf.put_i32(12);
        buf.put_i32(process_id);
        buf.put_i32(secret_key);
    }

    /// Encode ReadyForQuery
    pub fn encode_ready_for_query(buf: &mut BytesMut, status: TransactionStatus) {
        buf.put_u8(b'Z');
        buf.put_i32(5);
        buf.put_u8(status as u8);
    }

    /// Encode RowDescription
    pub fn encode_row_description(buf: &mut BytesMut, columns: &[(String, i32, i16)]) {
        buf.put_u8(b'T');

        let mut body = BytesMut::new();
        body.put_i16(columns.len() as i16);

        for (name, type_oid, type_len) in columns {
            body.put_slice(name.as_bytes());
            body.put_u8(0);
            body.put_i32(0); // table OID
            body.put_i16(0); // column number
            body.put_i32(*type_oid); // type OID
            body.put_i16(*type_len); // type size
            body.put_i32(-1); // type modifier
            body.put_i16(0); // format code (text)
        }

        buf.put_i32(body.len() as i32 + 4);
        buf.put(body);
    }

    /// Encode DataRow — writes directly to buf without a temporary body buffer.
    pub fn encode_data_row(buf: &mut BytesMut, values: &[Option<&[u8]>]) {
        // Pre-calculate body length to write in one pass.
        let body_len: usize = 2 + values
            .iter()
            .map(|v| match v {
                Some(data) => 4 + data.len(),
                None => 4,
            })
            .sum::<usize>();

        buf.reserve(1 + 4 + body_len);
        buf.put_u8(b'D');
        buf.put_i32(body_len as i32 + 4);
        buf.put_i16(values.len() as i16);

        for val in values {
            match val {
                Some(data) => {
                    buf.put_i32(data.len() as i32);
                    buf.put_slice(data);
                }
                None => {
                    buf.put_i32(-1); // NULL
                }
            }
        }
    }

    /// Encode CommandComplete
    pub fn encode_command_complete(buf: &mut BytesMut, tag: &str) {
        buf.put_u8(b'C');
        buf.put_i32(tag.len() as i32 + 5);
        buf.put_slice(tag.as_bytes());
        buf.put_u8(0);
    }

    /// Encode ErrorResponse
    pub fn encode_error(buf: &mut BytesMut, severity: &str, code: &str, message: &str) {
        buf.put_u8(b'E');

        let mut body = BytesMut::new();
        body.put_u8(b'S');
        body.put_slice(severity.as_bytes());
        body.put_u8(0);
        body.put_u8(b'V');
        body.put_slice(severity.as_bytes());
        body.put_u8(0);
        body.put_u8(b'C');
        body.put_slice(code.as_bytes());
        body.put_u8(0);
        body.put_u8(b'M');
        body.put_slice(message.as_bytes());
        body.put_u8(0);
        body.put_u8(0); // terminator

        buf.put_i32(body.len() as i32 + 4);
        buf.put(body);
    }

    /// Encode ParseComplete
    pub fn encode_parse_complete(buf: &mut BytesMut) {
        buf.put_u8(b'1');
        buf.put_i32(4);
    }

    /// Encode BindComplete
    pub fn encode_bind_complete(buf: &mut BytesMut) {
        buf.put_u8(b'2');
        buf.put_i32(4);
    }

    /// Encode EmptyQueryResponse
    pub fn encode_empty_query(buf: &mut BytesMut) {
        buf.put_u8(b'I');
        buf.put_i32(4);
    }

    /// Encode NoData
    pub fn encode_no_data(buf: &mut BytesMut) {
        buf.put_u8(b'n');
        buf.put_i32(4);
    }

    /// Encode ParameterDescription (for prepared statements)
    pub fn encode_parameter_description(buf: &mut BytesMut, param_types: &[i32]) {
        let len = 4 + 2 + (param_types.len() * 4);
        buf.put_u8(b't');
        buf.put_i32(len as i32);
        buf.put_i16(param_types.len() as i16);
        for oid in param_types {
            buf.put_i32(*oid);
        }
    }
}

// PostgreSQL OIDs for common types
pub mod oid {
    pub const BOOL: i32 = 16;
    pub const INT2: i32 = 21;
    pub const INT4: i32 = 23;
    pub const INT8: i32 = 20;
    pub const FLOAT4: i32 = 700;
    pub const FLOAT8: i32 = 701;
    pub const TEXT: i32 = 25;
    pub const VARCHAR: i32 = 1043;
    pub const BYTEA: i32 = 17;
    pub const TIMESTAMP: i32 = 1114;
    pub const TIMESTAMPTZ: i32 = 1184;
    pub const JSON: i32 = 114;
    pub const JSONB: i32 = 3802;
    pub const UUID: i32 = 2950;
    pub const NUMERIC: i32 = 1700;
    pub const VECTOR: i32 = 16385; // Custom OID for vectors
}
