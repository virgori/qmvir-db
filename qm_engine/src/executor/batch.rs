/*
 * Column Batch - Columnar data storage for vectorized execution
 *
 * Apache Arrow-inspired columnar format for efficient SIMD processing.
 */

/// Data types supported by the engine
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    Null,
    Bool,
    Int8,
    Int16,
    Int32,
    Int64,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    Float32,
    Float64,
    String,
    Binary,
    Timestamp,
    Vector(usize), // Vector with dimension
}

impl DataType {
    pub fn size(&self) -> usize {
        match self {
            DataType::Null => 0,
            DataType::Bool | DataType::Int8 | DataType::UInt8 => 1,
            DataType::Int16 | DataType::UInt16 => 2,
            DataType::Int32 | DataType::UInt32 | DataType::Float32 => 4,
            DataType::Int64 | DataType::UInt64 | DataType::Float64 | DataType::Timestamp => 8,
            DataType::String | DataType::Binary => 16, // Pointer + length
            DataType::Vector(dim) => dim * 4,          // f32 * dim
        }
    }
}

/// Validity bitmap for NULL tracking
#[derive(Debug, Clone)]
pub struct Bitmap {
    data: Vec<u64>,
    len: usize,
}

impl Bitmap {
    pub fn new(len: usize) -> Self {
        let n_words = (len + 63) / 64;
        Self {
            data: vec![!0u64; n_words], // All valid by default
            len,
        }
    }

    pub fn all_valid(len: usize) -> Self {
        Self::new(len)
    }

    pub fn all_null(len: usize) -> Self {
        let n_words = (len + 63) / 64;
        Self {
            data: vec![0u64; n_words],
            len,
        }
    }

    #[inline]
    pub fn is_valid(&self, idx: usize) -> bool {
        let word = idx / 64;
        let bit = idx % 64;
        (self.data[word] >> bit) & 1 == 1
    }

    #[inline]
    pub fn set_valid(&mut self, idx: usize, valid: bool) {
        let word = idx / 64;
        let bit = idx % 64;
        if valid {
            self.data[word] |= 1u64 << bit;
        } else {
            self.data[word] &= !(1u64 << bit);
        }
    }

    pub fn count_valid(&self) -> usize {
        let full_words = self.len / 64;
        let remaining = self.len % 64;

        let mut count = 0;
        for i in 0..full_words {
            count += self.data[i].count_ones() as usize;
        }

        if remaining > 0 {
            let mask = (1u64 << remaining) - 1;
            count += (self.data[full_words] & mask).count_ones() as usize;
        }

        count
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

/// Column data buffer
#[derive(Debug, Clone)]
pub enum ColumnData {
    Bool(Vec<bool>),
    Int8(Vec<i8>),
    Int16(Vec<i16>),
    Int32(Vec<i32>),
    Int64(Vec<i64>),
    UInt8(Vec<u8>),
    UInt16(Vec<u16>),
    UInt32(Vec<u32>),
    UInt64(Vec<u64>),
    Float32(Vec<f32>),
    Float64(Vec<f64>),
    String(Vec<String>),
    Binary(Vec<Vec<u8>>),
    Timestamp(Vec<i64>), // Microseconds since epoch
    Vector { dim: usize, data: Vec<f32> },
}

impl ColumnData {
    pub fn len(&self) -> usize {
        match self {
            ColumnData::Bool(v) => v.len(),
            ColumnData::Int8(v) => v.len(),
            ColumnData::Int16(v) => v.len(),
            ColumnData::Int32(v) => v.len(),
            ColumnData::Int64(v) => v.len(),
            ColumnData::UInt8(v) => v.len(),
            ColumnData::UInt16(v) => v.len(),
            ColumnData::UInt32(v) => v.len(),
            ColumnData::UInt64(v) => v.len(),
            ColumnData::Float32(v) => v.len(),
            ColumnData::Float64(v) => v.len(),
            ColumnData::String(v) => v.len(),
            ColumnData::Binary(v) => v.len(),
            ColumnData::Timestamp(v) => v.len(),
            ColumnData::Vector { dim, data } => data.len() / dim,
        }
    }

    pub fn data_type(&self) -> DataType {
        match self {
            ColumnData::Bool(_) => DataType::Bool,
            ColumnData::Int8(_) => DataType::Int8,
            ColumnData::Int16(_) => DataType::Int16,
            ColumnData::Int32(_) => DataType::Int32,
            ColumnData::Int64(_) => DataType::Int64,
            ColumnData::UInt8(_) => DataType::UInt8,
            ColumnData::UInt16(_) => DataType::UInt16,
            ColumnData::UInt32(_) => DataType::UInt32,
            ColumnData::UInt64(_) => DataType::UInt64,
            ColumnData::Float32(_) => DataType::Float32,
            ColumnData::Float64(_) => DataType::Float64,
            ColumnData::String(_) => DataType::String,
            ColumnData::Binary(_) => DataType::Binary,
            ColumnData::Timestamp(_) => DataType::Timestamp,
            ColumnData::Vector { dim, .. } => DataType::Vector(*dim),
        }
    }
}

/// A batch of column data (columnar format)
#[derive(Debug, Clone)]
pub struct ColumnBatch {
    pub name: String,
    pub data_type: DataType,
    pub data: ColumnData,
    pub validity: Bitmap,
}

impl ColumnBatch {
    pub fn new(name: String, data: ColumnData) -> Self {
        let len = data.len();
        let data_type = data.data_type();
        Self {
            name,
            data_type,
            data,
            validity: Bitmap::all_valid(len),
        }
    }

    pub fn with_validity(mut self, validity: Bitmap) -> Self {
        self.validity = validity;
        self
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn null_count(&self) -> usize {
        self.len() - self.validity.count_valid()
    }

    /// Get float32 slice for SIMD operations
    pub fn as_f32_slice(&self) -> Option<&[f32]> {
        match &self.data {
            ColumnData::Float32(v) => Some(v),
            ColumnData::Vector { data, .. } => Some(data),
            _ => None,
        }
    }

    /// Get mutable float32 slice
    pub fn as_f32_slice_mut(&mut self) -> Option<&mut [f32]> {
        match &mut self.data {
            ColumnData::Float32(v) => Some(v),
            ColumnData::Vector { data, .. } => Some(data),
            _ => None,
        }
    }

    /// Get int64 slice for SIMD operations
    pub fn as_i64_slice(&self) -> Option<&[i64]> {
        match &self.data {
            ColumnData::Int64(v) => Some(v),
            ColumnData::Timestamp(v) => Some(v),
            _ => None,
        }
    }
}

/// Record batch - collection of columns
#[derive(Debug, Clone)]
pub struct RecordBatch {
    pub columns: Vec<ColumnBatch>,
    pub row_count: usize,
}

impl RecordBatch {
    pub fn new(columns: Vec<ColumnBatch>) -> Self {
        let row_count = columns.first().map(|c| c.len()).unwrap_or(0);
        Self { columns, row_count }
    }

    pub fn column(&self, name: &str) -> Option<&ColumnBatch> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn column_by_index(&self, idx: usize) -> Option<&ColumnBatch> {
        self.columns.get(idx)
    }

    pub fn num_columns(&self) -> usize {
        self.columns.len()
    }

    pub fn num_rows(&self) -> usize {
        self.row_count
    }

    pub fn is_empty(&self) -> bool {
        self.row_count == 0
    }

    /// Slice batch to get a subset of rows
    pub fn slice(&self, offset: usize, length: usize) -> RecordBatch {
        let mut new_columns = Vec::with_capacity(self.columns.len());

        for col in &self.columns {
            let new_data = match &col.data {
                ColumnData::Int64(v) => ColumnData::Int64(v[offset..offset + length].to_vec()),
                ColumnData::Float32(v) => ColumnData::Float32(v[offset..offset + length].to_vec()),
                ColumnData::Float64(v) => ColumnData::Float64(v[offset..offset + length].to_vec()),
                ColumnData::String(v) => ColumnData::String(v[offset..offset + length].to_vec()),
                // Add other types as needed
                _ => col.data.clone(),
            };

            new_columns.push(ColumnBatch::new(col.name.clone(), new_data));
        }

        RecordBatch::new(new_columns)
    }
}
