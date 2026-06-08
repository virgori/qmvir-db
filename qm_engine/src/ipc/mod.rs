//! IPC — Lock-free Shared Memory Ring Buffer (LMAX Disruptor Model)
//!
//! Rust implementation replacing the Python SharedRingBuffer.
//! Provides true atomic state transitions, crash recovery, and CRC32 integrity.

pub mod dispatcher;
pub mod lsn;
pub mod ring_buffer;
