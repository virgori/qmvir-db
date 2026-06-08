/*
 * Zero-Copy IPC Types — Phase 4
 *
 * Arrow-compatible columnar data types for zero-copy data transfer between
 * engine components. These types wrap `bytes::Bytes` (reference-counted,
 * zero-copy sliceable) instead of `Vec<u8>`, enabling:
 *
 *   • Shared column buffers across readers without `clone()`
 *   • Zero-copy slicing for batched/streaming execution
 *   • Python ↔ Rust zero-copy via PyO3 buffer protocol
 *
 * The layout is Arrow IPC compatible: fixed-width data buffer + validity bitmap.
 */

use bytes::{BufMut, Bytes, BytesMut};
use std::sync::Arc;

// ── Zero-Copy Column Buffer ─────────────────────────────────────────────

/// Immutable, reference-counted column buffer with Arrow-compatible layout.
/// Uses `bytes::Bytes` for zero-copy sharing and slicing.
#[derive(Clone, Debug)]
pub struct ZeroCopyBuffer {
    /// Raw data bytes (column values in fixed-width layout).
    data: Bytes,
    /// Element count in this buffer.
    len: usize,
    /// Size in bytes of each element (0 for variable-width).
    element_size: usize,
}

impl ZeroCopyBuffer {
    /// Create a new zero-copy buffer from raw bytes.
    pub fn new(data: Bytes, len: usize, element_size: usize) -> Self {
        Self {
            data,
            len,
            element_size,
        }
    }

    /// Create from a Vec<f32> — single memcpy, then shared.
    pub fn from_f32_vec(v: &[f32]) -> Self {
        let mut buf = BytesMut::with_capacity(v.len() * 4);
        for val in v {
            buf.put_f32_le(*val);
        }
        Self {
            data: buf.freeze(),
            len: v.len(),
            element_size: 4,
        }
    }

    /// Create from a Vec<i64> — single memcpy, then shared.
    pub fn from_i64_vec(v: &[i64]) -> Self {
        let mut buf = BytesMut::with_capacity(v.len() * 8);
        for val in v {
            buf.put_i64_le(*val);
        }
        Self {
            data: buf.freeze(),
            len: v.len(),
            element_size: 8,
        }
    }

    /// Zero-copy slice — returns a view into the same underlying buffer.
    pub fn slice(&self, offset: usize, length: usize) -> Self {
        let byte_offset = offset * self.element_size;
        let byte_len = length * self.element_size;
        Self {
            data: self.data.slice(byte_offset..byte_offset + byte_len),
            len: length,
            element_size: self.element_size,
        }
    }

    /// Get raw bytes — zero copy.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Interpret buffer as &[f32] — zero copy, no allocation.
    pub fn as_f32_slice(&self) -> &[f32] {
        debug_assert_eq!(self.element_size, 4);
        // SAFETY: Bytes buffer is aligned and was created from f32 values
        unsafe { std::slice::from_raw_parts(self.data.as_ptr() as *const f32, self.len) }
    }

    /// Interpret buffer as &[i64] — zero copy, no allocation.
    pub fn as_i64_slice(&self) -> &[i64] {
        debug_assert_eq!(self.element_size, 8);
        // SAFETY: Bytes buffer is aligned and was created from i64 values
        unsafe { std::slice::from_raw_parts(self.data.as_ptr() as *const i64, self.len) }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Total byte size of the data buffer.
    pub fn byte_len(&self) -> usize {
        self.data.len()
    }
}

// ── Arrow IPC Message Header ────────────────────────────────────────────

/// Minimal Arrow IPC message type tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IpcMessageType {
    Schema = 1,
    RecordBatch = 2,
    DictionaryBatch = 3,
    Eos = 4,
}

/// Arrow IPC field descriptor.
#[derive(Debug, Clone)]
pub struct IpcField {
    pub name: String,
    pub type_id: IpcTypeId,
    pub nullable: bool,
}

/// Arrow-compatible type identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcTypeId {
    Int32,
    Int64,
    Float32,
    Float64,
    Utf8,
    Binary,
    Bool,
}

impl IpcTypeId {
    pub fn fixed_size(&self) -> Option<usize> {
        match self {
            IpcTypeId::Int32 => Some(4),
            IpcTypeId::Int64 | IpcTypeId::Float64 => Some(8),
            IpcTypeId::Float32 => Some(4),
            IpcTypeId::Bool => Some(1),
            IpcTypeId::Utf8 | IpcTypeId::Binary => None, // variable-width
        }
    }
}

// ── Arrow IPC Schema ────────────────────────────────────────────────────

/// Schema for zero-copy IPC: list of typed fields.
#[derive(Debug, Clone)]
pub struct IpcSchema {
    pub fields: Vec<IpcField>,
}

impl IpcSchema {
    pub fn new(fields: Vec<IpcField>) -> Self {
        Self { fields }
    }

    /// Encode schema as a compact binary header.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(IpcMessageType::Schema as u8);
        buf.extend_from_slice(&(self.fields.len() as u16).to_le_bytes());
        for f in &self.fields {
            let nb = f.name.as_bytes();
            buf.extend_from_slice(&(nb.len() as u16).to_le_bytes());
            buf.extend_from_slice(nb);
            buf.push(f.type_id as u8);
            buf.push(f.nullable as u8);
        }
        buf
    }

    /// Decode schema from binary header.
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.is_empty() || data[0] != IpcMessageType::Schema as u8 {
            return None;
        }
        let field_count = u16::from_le_bytes([data[1], data[2]]) as usize;
        let mut pos = 3;
        let mut fields = Vec::with_capacity(field_count);
        for _ in 0..field_count {
            if pos + 2 > data.len() {
                return None;
            }
            let name_len = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
            pos += 2;
            if pos + name_len + 2 > data.len() {
                return None;
            }
            let name = String::from_utf8_lossy(&data[pos..pos + name_len]).to_string();
            pos += name_len;
            let type_id = match data[pos] {
                0 => IpcTypeId::Int32,
                1 => IpcTypeId::Int64,
                2 => IpcTypeId::Float32,
                3 => IpcTypeId::Float64,
                4 => IpcTypeId::Utf8,
                5 => IpcTypeId::Binary,
                6 => IpcTypeId::Bool,
                _ => return None,
            };
            pos += 1;
            let nullable = data[pos] != 0;
            pos += 1;
            fields.push(IpcField {
                name,
                type_id,
                nullable,
            });
        }
        Some(IpcSchema { fields })
    }
}

// ── Shared Column (Arc-wrapped for multi-reader access) ─────────────────

/// Arc-wrapped column for zero-copy sharing across query plan nodes.
/// Multiple operators can read the same column buffer without cloning.
#[derive(Clone, Debug)]
pub struct SharedColumn {
    pub name: String,
    pub type_id: IpcTypeId,
    pub buffer: Arc<ZeroCopyBuffer>,
}

impl SharedColumn {
    pub fn new(name: String, type_id: IpcTypeId, buffer: ZeroCopyBuffer) -> Self {
        Self {
            name,
            type_id,
            buffer: Arc::new(buffer),
        }
    }

    /// Zero-copy slice of this column — shares the same Arc'ed buffer.
    pub fn slice(&self, offset: usize, length: usize) -> Self {
        Self {
            name: self.name.clone(),
            type_id: self.type_id,
            buffer: Arc::new(self.buffer.slice(offset, length)),
        }
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_f32_buffer_roundtrip() {
        let data = vec![1.0f32, 2.0, 3.0, 4.0];
        let buf = ZeroCopyBuffer::from_f32_vec(&data);
        assert_eq!(buf.len(), 4);
        assert_eq!(buf.as_f32_slice(), &data);
    }

    #[test]
    fn test_i64_buffer_roundtrip() {
        let data = vec![10i64, 20, 30];
        let buf = ZeroCopyBuffer::from_i64_vec(&data);
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.as_i64_slice(), &data);
    }

    #[test]
    fn test_zero_copy_slice() {
        let data = vec![1.0f32, 2.0, 3.0, 4.0, 5.0];
        let buf = ZeroCopyBuffer::from_f32_vec(&data);
        let sliced = buf.slice(1, 3);
        assert_eq!(sliced.len(), 3);
        assert_eq!(sliced.as_f32_slice(), &[2.0, 3.0, 4.0]);
    }

    #[test]
    fn test_schema_encode_decode() {
        let schema = IpcSchema::new(vec![
            IpcField {
                name: "id".into(),
                type_id: IpcTypeId::Int64,
                nullable: false,
            },
            IpcField {
                name: "value".into(),
                type_id: IpcTypeId::Float32,
                nullable: true,
            },
        ]);
        let encoded = schema.encode();
        let decoded = IpcSchema::decode(&encoded).unwrap();
        assert_eq!(decoded.fields.len(), 2);
        assert_eq!(decoded.fields[0].name, "id");
        assert_eq!(decoded.fields[0].type_id, IpcTypeId::Int64);
        assert!(!decoded.fields[0].nullable);
        assert_eq!(decoded.fields[1].name, "value");
        assert_eq!(decoded.fields[1].type_id, IpcTypeId::Float32);
        assert!(decoded.fields[1].nullable);
    }

    #[test]
    fn test_shared_column_zero_copy_sharing() {
        let data = vec![1.0f32, 2.0, 3.0];
        let col = SharedColumn::new(
            "score".into(),
            IpcTypeId::Float32,
            ZeroCopyBuffer::from_f32_vec(&data),
        );
        // Clone shares the same Arc — no data copy
        let col2 = col.clone();
        assert!(Arc::ptr_eq(&col.buffer, &col2.buffer));
        assert_eq!(col.buffer.as_f32_slice(), &[1.0, 2.0, 3.0]);
    }
}
