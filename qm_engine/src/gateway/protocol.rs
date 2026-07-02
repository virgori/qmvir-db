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
        param_formats: Vec<i16>,
        params: Vec<Option<Bytes>>,
        result_formats: Vec<i16>,
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
    Password(Vec<u8>),
    CancelRequest {
        process_id: i32,
        secret_key: i32,
    },
    SSLRequest,
    GssEncRequest,
    CopyData(Vec<u8>),
    CopyDone,
    CopyFail(String),
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

        // GSSAPI encryption request (PostgreSQL 12+ clients may probe this first)
        if protocol == 80877104 {
            return Ok(Message::GssEncRequest);
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

                // Parameter format codes
                let n_param_formats = buf.get_i16() as usize;
                let mut param_formats = Vec::with_capacity(n_param_formats);
                for _ in 0..n_param_formats {
                    param_formats.push(buf.get_i16());
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

                // Result-column format codes
                let n_result_formats = buf.get_i16() as usize;
                let mut result_formats = Vec::with_capacity(n_result_formats);
                for _ in 0..n_result_formats {
                    result_formats.push(buf.get_i16());
                }

                Ok(Message::Bind {
                    portal,
                    statement,
                    param_formats,
                    params,
                    result_formats,
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
                let pos = buf.position() as usize;
                let data = buf.get_ref()[pos..].to_vec();
                buf.set_position(buf.get_ref().len() as u64);
                Ok(Message::Password(data))
            }
            b'd' => {
                let pos = buf.position() as usize;
                let data = buf.get_ref()[pos..].to_vec();
                buf.set_position(buf.get_ref().len() as u64);
                Ok(Message::CopyData(data))
            }
            b'c' => Ok(Message::CopyDone),
            b'f' => {
                let msg = Self::read_cstring(buf).unwrap_or_default();
                Ok(Message::CopyFail(msg))
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

    /// Encode CopyInResponse (text format).
    pub fn encode_copy_in_response(buf: &mut BytesMut, columns: u16) {
        buf.put_u8(b'G');
        buf.put_i32(7 + columns as i32 * 2);
        buf.put_i8(0); // text format
        buf.put_i16(columns as i16);
        for _ in 0..columns {
            buf.put_i16(0); // text format per column
        }
    }

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

    /// Encode AuthenticationSASL (type 10) with mechanism list.
    pub fn encode_auth_sasl(buf: &mut BytesMut, mechanisms: &[&str]) {
        let mut body = BytesMut::new();
        body.put_i32(10);
        for mech in mechanisms {
            body.put_slice(mech.as_bytes());
            body.put_u8(0);
        }
        body.put_u8(0);
        buf.put_u8(b'R');
        buf.put_i32(body.len() as i32 + 4);
        buf.put(body);
    }

    /// Encode AuthenticationSASLContinue (type 11).
    pub fn encode_auth_sasl_continue(buf: &mut BytesMut, payload: &str) {
        let bytes = payload.as_bytes();
        buf.put_u8(b'R');
        buf.put_i32(8 + bytes.len() as i32);
        buf.put_i32(11);
        buf.put_slice(bytes);
    }

    /// Encode AuthenticationSASLFinal (type 12).
    pub fn encode_auth_sasl_final(buf: &mut BytesMut, payload: &str) {
        let bytes = payload.as_bytes();
        buf.put_u8(b'R');
        buf.put_i32(8 + bytes.len() as i32);
        buf.put_i32(12);
        buf.put_slice(bytes);
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

    /// Format code for one result column (Bind `result_formats`).
    pub fn column_format_code(formats: &[i16], index: usize) -> i16 {
        if formats.is_empty() {
            0
        } else if formats.len() == 1 {
            formats[0]
        } else {
            formats.get(index).copied().unwrap_or(0)
        }
    }

    /// Convert engine text cell bytes to PostgreSQL binary representation.
    pub fn encode_cell_binary(oid: i32, text_bytes: &[u8]) -> Option<Vec<u8>> {
        let s = std::str::from_utf8(text_bytes).ok()?;
        match oid {
            oid::BOOL => {
                let b = matches!(s, "t" | "true" | "1" | "yes" | "T" | "TRUE");
                Some(vec![u8::from(b)])
            }
            oid::INT2 => {
                let v: i16 = s.parse().ok()?;
                Some(v.to_be_bytes().to_vec())
            }
            oid::INT4 => {
                let v: i32 = s.parse().ok()?;
                Some(v.to_be_bytes().to_vec())
            }
            oid::INT8 => {
                let v: i64 = s.parse().ok()?;
                Some(v.to_be_bytes().to_vec())
            }
            oid::FLOAT4 => {
                let v: f32 = s.parse().ok()?;
                Some(v.to_be_bytes().to_vec())
            }
            oid::FLOAT8 => {
                let v: f64 = s.parse().ok()?;
                Some(v.to_be_bytes().to_vec())
            }
            oid::BYTEA => {
                if let Some(hex) = s.strip_prefix("\\x") {
                    hex::decode(hex).ok()
                } else {
                    Some(s.as_bytes().to_vec())
                }
            }
            oid::TEXT | oid::VARCHAR | oid::JSON | oid::JSONB | oid::UUID => {
                Some(text_bytes.to_vec())
            }
            _ => Some(text_bytes.to_vec()),
        }
    }

    /// Encode DataRow honoring per-column binary/text format codes.
    pub fn encode_data_row_formatted(
        buf: &mut BytesMut,
        row: &[Option<Vec<u8>>],
        columns: &[(String, i32, i16)],
        formats: &[i16],
    ) {
        let mut scratch = Vec::with_capacity(row.len());
        for (i, cell) in row.iter().enumerate() {
            let fmt = Self::column_format_code(formats, i);
            let oid = columns.get(i).map(|c| c.1).unwrap_or(oid::TEXT);
            scratch.push(match cell {
                None => None,
                Some(bytes) if fmt == 0 => {
                    if oid == oid::INT8 && bytes.len() == 8 {
                        let arr: [u8; 8] = bytes.as_slice().try_into().unwrap_or([0; 8]);
                        Some(i64::from_le_bytes(arr).to_string().into_bytes())
                    } else {
                        Some(bytes.clone())
                    }
                }
                Some(bytes) => Self::encode_cell_binary(oid, bytes).or_else(|| Some(bytes.clone())),
            });
        }
        let refs: Vec<Option<&[u8]>> = scratch
            .iter()
            .map(|v| v.as_ref().map(|b| b.as_slice()))
            .collect();
        Self::encode_data_row(buf, &refs);
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

const SMALL_INT_STR_MAX: usize = 2_000_000;

static SMALL_INT_STR: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| {
    (0..SMALL_INT_STR_MAX).map(|i| i.to_string()).collect()
});

/// Decimal text for int64 — hot path for primary-key columns (0..2M cached).
#[inline]
pub fn format_i64_display(id: i64) -> String {
    format_i64_display_cow(id).into_owned()
}

/// Borrowed decimal text when `id` is in the static primary-key cache.
#[inline]
pub fn format_i64_display_cow(id: i64) -> std::borrow::Cow<'static, str> {
    use std::borrow::Cow;
    if id >= 0 {
        let u = id as usize;
        if u < SMALL_INT_STR_MAX {
            return Cow::Borrowed(&SMALL_INT_STR[u]);
        }
    }
    let mut buf = itoa::Buffer::new();
    Cow::Owned(buf.format(id).to_string())
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

#[cfg(test)]
mod tests {
    use super::oid;
    use super::ProtocolCodec;
    use bytes::Buf;

    #[test]
    fn encode_cell_binary_int4() {
        let bin = ProtocolCodec::encode_cell_binary(oid::INT4, b"42").expect("int4");
        assert_eq!(bin, 42i32.to_be_bytes());
    }

    #[test]
    fn encode_cell_binary_int8() {
        let bin = ProtocolCodec::encode_cell_binary(oid::INT8, b"1000").expect("int8");
        assert_eq!(bin, 1000i64.to_be_bytes());
    }

    #[test]
    fn encode_data_row_formatted_binary_int4() {
        let mut buf = bytes::BytesMut::new();
        let row = vec![Some(b"7".to_vec())];
        let cols = vec![("id".to_string(), oid::INT4, 4i16)];
        ProtocolCodec::encode_data_row_formatted(&mut buf, &row, &cols, &[1]);
        assert_eq!(buf[0], b'D');
        let mut cur = std::io::Cursor::new(&buf[1..]);
        let _len = cur.get_i32();
        let ncols = cur.get_i16();
        assert_eq!(ncols, 1);
        let field_len = cur.get_i32();
        assert_eq!(field_len, 4);
        let mut bytes = [0u8; 4];
        cur.copy_to_slice(&mut bytes);
        assert_eq!(i32::from_be_bytes(bytes), 7);
    }
}
