//! Per-allocation log ring buffer with asynchronous S3 flush.
//!
//! Each allocation gets a fixed-size ring buffer (default 64 MB) that stores
//! live log output. The dual-path log strategy (ring buffer for live tailing,
//! S3 for persistent storage) is described in the architecture docs.

use async_trait::async_trait;

/// Default ring buffer capacity: 64 MB.
const DEFAULT_CAPACITY: usize = 64 * 1024 * 1024;

/// Trait for uploading log data to an S3-compatible object store.
///
/// This is trait-based so it can be mocked in tests without requiring
/// a real S3 endpoint.
#[async_trait]
pub trait S3Sink: Send + Sync {
    /// Upload `data` to the given `bucket` and `key`.
    async fn upload(&self, bucket: &str, key: &str, data: Vec<u8>) -> Result<(), String>;
}

/// A fixed-size ring buffer for per-allocation log data.
///
/// Writes wrap around when the buffer reaches capacity, overwriting the
/// oldest data first. This matches the "ring buffer live + S3 persistent"
/// dual-path log design.
pub struct LogRingBuffer {
    /// Backing storage.
    buf: Vec<u8>,
    /// Maximum number of bytes the buffer can hold.
    capacity: usize,
    /// Write cursor: the position where the next byte will be written.
    /// Wraps modulo `capacity`.
    write_pos: usize,
    /// Total number of bytes written since creation (never wraps).
    /// Used to determine whether the buffer has wrapped.
    total_written: usize,
}

impl LogRingBuffer {
    /// Create a new ring buffer with the default capacity (64 MB).
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// Create a new ring buffer with a custom capacity in bytes.
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "ring buffer capacity must be > 0");
        Self {
            buf: vec![0u8; capacity],
            capacity,
            write_pos: 0,
            total_written: 0,
        }
    }

    /// Append `data` to the buffer. If the data exceeds remaining space,
    /// it wraps around from the beginning, overwriting the oldest bytes.
    pub fn write(&mut self, data: &[u8]) {
        for &byte in data {
            self.buf[self.write_pos] = byte;
            self.write_pos = (self.write_pos + 1) % self.capacity;
            self.total_written += 1;
        }
    }

    /// Read all currently buffered data in chronological order.
    ///
    /// If the buffer has not yet wrapped, returns only the written portion.
    /// If it has wrapped, returns data from the write cursor (oldest) through
    /// the end, then from the beginning up to the write cursor (newest).
    pub fn read_all(&self) -> Vec<u8> {
        if self.total_written == 0 {
            return Vec::new();
        }

        if self.total_written < self.capacity {
            // Buffer has not wrapped yet; valid data is [0..write_pos).
            self.buf[..self.write_pos].to_vec()
        } else if self.write_pos == 0 {
            // Buffer is exactly full (or a multiple thereof); all data is valid
            // and write_pos wrapped back to 0, so the entire buffer is in order.
            self.buf.clone()
        } else {
            // Buffer has wrapped; oldest data starts at write_pos.
            let mut result = Vec::with_capacity(self.capacity);
            result.extend_from_slice(&self.buf[self.write_pos..]);
            result.extend_from_slice(&self.buf[..self.write_pos]);
            result
        }
    }

    /// The maximum capacity in bytes.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The number of valid bytes currently in the buffer.
    pub fn len(&self) -> usize {
        std::cmp::min(self.total_written, self.capacity)
    }

    /// Whether the buffer is empty (no data written yet).
    pub fn is_empty(&self) -> bool {
        self.total_written == 0
    }

    /// Flush the buffered data to S3 via the provided sink.
    ///
    /// This reads all current data and uploads it. The buffer is NOT
    /// cleared after flush -- the caller may continue writing and
    /// flush again later with newer data.
    pub async fn flush_to_s3(
        &self,
        sink: &dyn S3Sink,
        bucket: &str,
        key: &str,
    ) -> Result<(), String> {
        let data = self.read_all();
        if data.is_empty() {
            return Ok(());
        }
        sink.upload(bucket, key, data).await
    }
}

impl Default for LogRingBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type UploadRecord = Vec<(String, String, Vec<u8>)>;

    /// Mock S3 sink that records uploads for assertion.
    struct MockS3Sink {
        uploads: Arc<Mutex<UploadRecord>>,
    }

    impl MockS3Sink {
        fn new() -> (Self, Arc<Mutex<UploadRecord>>) {
            let store = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    uploads: store.clone(),
                },
                store,
            )
        }
    }

    #[async_trait]
    impl S3Sink for MockS3Sink {
        async fn upload(&self, bucket: &str, key: &str, data: Vec<u8>) -> Result<(), String> {
            self.uploads
                .lock()
                .unwrap()
                .push((bucket.to_string(), key.to_string(), data));
            Ok(())
        }
    }

    /// Failing S3 sink that always returns an error.
    struct FailingS3Sink;

    #[async_trait]
    impl S3Sink for FailingS3Sink {
        async fn upload(&self, _bucket: &str, _key: &str, _data: Vec<u8>) -> Result<(), String> {
            Err("simulated S3 failure".to_string())
        }
    }

    #[test]
    fn new_buffer_is_empty() {
        let buf = LogRingBuffer::with_capacity(128);
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.read_all(), Vec::<u8>::new());
    }

    #[test]
    fn write_and_read_without_wrap() {
        let mut buf = LogRingBuffer::with_capacity(128);
        buf.write(b"hello world");

        let data = buf.read_all();
        assert_eq!(data, b"hello world");
        assert_eq!(buf.len(), 11);
        assert!(!buf.is_empty());
    }

    #[test]
    fn write_wraps_at_capacity() {
        let mut buf = LogRingBuffer::with_capacity(8);

        // Write exactly 8 bytes (fills buffer).
        buf.write(b"ABCDEFGH");
        assert_eq!(buf.len(), 8);
        assert_eq!(buf.read_all(), b"ABCDEFGH");

        // Write 4 more bytes -- wraps around, overwriting A-D.
        buf.write(b"1234");
        assert_eq!(buf.len(), 8);

        let data = buf.read_all();
        // Oldest data starts at write_pos (4), so: EFGH1234
        assert_eq!(data, b"EFGH1234");
    }

    #[test]
    fn write_more_than_capacity_keeps_latest() {
        let mut buf = LogRingBuffer::with_capacity(4);

        // Write 10 bytes into a 4-byte buffer.
        buf.write(b"0123456789");
        assert_eq!(buf.len(), 4);

        let data = buf.read_all();
        // Only the last 4 bytes survive.
        assert_eq!(data, b"6789");
    }

    #[test]
    fn multiple_writes_accumulate() {
        let mut buf = LogRingBuffer::with_capacity(64);
        buf.write(b"aaa");
        buf.write(b"bbb");
        buf.write(b"ccc");

        let data = buf.read_all();
        assert_eq!(data, b"aaabbbccc");
        assert_eq!(buf.len(), 9);
    }

    #[test]
    fn capacity_returns_configured_value() {
        let buf = LogRingBuffer::with_capacity(256);
        assert_eq!(buf.capacity(), 256);
    }

    #[test]
    fn default_capacity_is_64mb() {
        let buf = LogRingBuffer::new();
        assert_eq!(buf.capacity(), 64 * 1024 * 1024);
    }

    #[tokio::test]
    async fn flush_to_s3_calls_sink() {
        let mut buf = LogRingBuffer::with_capacity(128);
        buf.write(b"log line 1\nlog line 2\n");

        let (sink, uploads) = MockS3Sink::new();
        buf.flush_to_s3(&sink, "my-bucket", "logs/alloc-123.log")
            .await
            .unwrap();

        let uploads = uploads.lock().unwrap();
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].0, "my-bucket");
        assert_eq!(uploads[0].1, "logs/alloc-123.log");
        assert_eq!(uploads[0].2, b"log line 1\nlog line 2\n");
    }

    #[tokio::test]
    async fn flush_empty_buffer_is_noop() {
        let buf = LogRingBuffer::with_capacity(128);
        let (sink, uploads) = MockS3Sink::new();

        buf.flush_to_s3(&sink, "bucket", "key").await.unwrap();

        let uploads = uploads.lock().unwrap();
        assert_eq!(uploads.len(), 0);
    }

    #[tokio::test]
    async fn flush_propagates_sink_error() {
        let mut buf = LogRingBuffer::with_capacity(128);
        buf.write(b"some data");

        let result = buf.flush_to_s3(&FailingS3Sink, "bucket", "key").await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "simulated S3 failure");
    }

    #[tokio::test]
    async fn flush_with_wrapped_buffer() {
        let mut buf = LogRingBuffer::with_capacity(8);
        buf.write(b"ABCDEFGH1234"); // wraps: only EFGH1234 survives (wait, 8 cap, wrote 12)

        let (sink, uploads) = MockS3Sink::new();
        buf.flush_to_s3(&sink, "bucket", "key").await.unwrap();

        let uploads = uploads.lock().unwrap();
        // Last 8 bytes: 56781234 -- actually let's compute:
        // Write 12 bytes into 8-byte buffer. write_pos goes 0..12 mod 8 = 4.
        // total_written = 12 > 8, so wrapped.
        // read_all: buf[4..8] + buf[0..4] = "EFGH" + "1234" = "EFGH1234"
        assert_eq!(uploads[0].2, b"EFGH1234");
    }
}
