//! Memtable for buffering log entries before flushing to S3.
//!
//! Uses a lock-free queue for concurrent writes with atomic counters for
//! flush decisions. Building the segment is deferred to flush time and
//! executed by a single thread.
//!
//! # Concurrency Model
//!
//! ```text
//! HTTP Client 1 ──┐
//! HTTP Client 2 ──┼──► SegQueue (lock-free) ──► Single Flusher ──► S3
//! HTTP Client 3 ──┘         ↑
//!                    Atomics for size/count/age
//! ```
//!
//! Multiple writers push to the queue concurrently without locking.
//! A single flusher drains the queue and builds the segment.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crossbeam::queue::SegQueue;

use crate::segment::{Builder, SegmentFooter, FOOTER_SIZE};

use super::error::{BufferError, Result};

/// A raw log entry before processing.
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// Timestamp in epoch milliseconds.
    pub timestamp: i64,
    /// The original JSON payload as bytes.
    pub payload: Vec<u8>,
}

impl LogEntry {
    /// Create a new log entry.
    pub fn new(timestamp: i64, payload: Vec<u8>) -> Self {
        Self { timestamp, payload }
    }

    /// Estimated size in bytes (for flush decisions).
    pub fn size(&self) -> usize {
        // timestamp (8) + payload length
        8 + self.payload.len()
    }
}

/// Metadata about a built segment.
#[derive(Debug, Clone)]
pub struct SegmentMeta {
    /// Number of log entries.
    pub entry_count: u32,
    /// Minimum timestamp (epoch millis).
    pub min_timestamp: i64,
    /// Maximum timestamp (epoch millis).
    pub max_timestamp: i64,
    /// Size of the segment in bytes.
    pub size_bytes: usize,
}

impl SegmentMeta {
    /// Create from a segment footer and data.
    pub fn from_footer(footer: &SegmentFooter, size_bytes: usize) -> Self {
        Self {
            entry_count: footer.log_count,
            min_timestamp: footer.min_timestamp,
            max_timestamp: footer.max_timestamp,
            size_bytes,
        }
    }
}

/// A memtable that buffers log entries for a single namespace.
///
/// Uses a lock-free queue for concurrent writes. Multiple threads can call
/// `add()` simultaneously without contention. The `drain_and_build()` method
/// should be called by a single thread to build the segment.
pub struct Memtable {
    /// Namespace this memtable belongs to.
    namespace_id: String,
    /// Lock-free queue for pending entries.
    pending: SegQueue<LogEntry>,
    /// Estimated total size of pending entries (for flush decisions).
    pending_size: AtomicUsize,
    /// Number of pending entries.
    pending_count: AtomicUsize,
    /// Timestamp (nanos since UNIX_EPOCH) when first entry was added, 0 if empty.
    first_entry_nanos: AtomicU64,
    /// Instant when the memtable was created (for age calculations).
    created_at: Instant,
}

impl Memtable {
    /// Create a new empty memtable for the given namespace.
    pub fn new(namespace_id: String) -> Self {
        Self {
            namespace_id,
            pending: SegQueue::new(),
            pending_size: AtomicUsize::new(0),
            pending_count: AtomicUsize::new(0),
            first_entry_nanos: AtomicU64::new(0),
            created_at: Instant::now(),
        }
    }

    /// Get the namespace ID.
    pub fn namespace_id(&self) -> &str {
        &self.namespace_id
    }

    /// Add a log entry to the memtable (lock-free, concurrent-safe).
    ///
    /// Multiple threads can call this method simultaneously without blocking.
    pub fn add(&self, entry: LogEntry) {
        let size = entry.size();

        // Try to set first_entry_nanos if this is the first entry
        let now_nanos = self.created_at.elapsed().as_nanos() as u64;
        let _ = self.first_entry_nanos.compare_exchange(
            0,
            now_nanos.max(1), // Ensure non-zero
            Ordering::AcqRel,
            Ordering::Relaxed,
        );

        // Push to lock-free queue
        self.pending.push(entry);

        // Update counters
        self.pending_size.fetch_add(size, Ordering::Relaxed);
        self.pending_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Get the number of pending entries.
    pub fn len(&self) -> usize {
        self.pending_count.load(Ordering::Relaxed)
    }

    /// Check if the memtable is empty.
    pub fn is_empty(&self) -> bool {
        self.pending_count.load(Ordering::Relaxed) == 0
    }

    /// Get the estimated size of pending entries in bytes.
    pub fn estimated_size(&self) -> usize {
        self.pending_size.load(Ordering::Relaxed)
    }

    /// Get the age of this memtable (time since first entry).
    ///
    /// Returns `None` if no entries have been added.
    pub fn age(&self) -> Option<Duration> {
        let first_nanos = self.first_entry_nanos.load(Ordering::Relaxed);
        if first_nanos == 0 {
            return None;
        }
        let first_duration = Duration::from_nanos(first_nanos);
        let elapsed = self.created_at.elapsed();
        Some(elapsed.saturating_sub(first_duration))
    }

    /// Drain all pending entries and build a segment.
    ///
    /// This method should be called by a single thread. It drains the lock-free
    /// queue, processes entries through the segment builder, and returns the
    /// built segment bytes with metadata.
    ///
    /// # Returns
    /// - `Ok((bytes, meta))` on success
    /// - `Err(EmptyBuffer)` if no entries are pending
    pub fn drain_and_build(&self) -> Result<(Vec<u8>, SegmentMeta)> {
        let mut builder = Builder::new();
        let mut drained_size = 0usize;
        let mut drained_count = 0usize;

        // Drain the queue
        while let Some(entry) = self.pending.pop() {
            drained_size += entry.size();
            drained_count += 1;
            builder
                .add(entry.timestamp, &entry.payload)
                .map_err(BufferError::InvalidJson)?;
        }

        if builder.is_empty() {
            return Err(BufferError::EmptyBuffer {
                namespace_id: self.namespace_id.clone(),
            });
        }

        // Update atomics (subtract what we drained)
        self.pending_size.fetch_sub(drained_size, Ordering::Relaxed);
        self.pending_count
            .fetch_sub(drained_count, Ordering::Relaxed);

        // Reset first_entry_nanos if we drained everything
        if self.pending_count.load(Ordering::Relaxed) == 0 {
            self.first_entry_nanos.store(0, Ordering::Relaxed);
        }

        // Build segment
        let segment_bytes = builder
            .build()
            .map_err(|e| BufferError::SegmentBuild(e.to_string()))?;

        // Parse footer to get metadata
        let footer_start = segment_bytes.len() - FOOTER_SIZE;
        let footer = SegmentFooter::from_bytes(&segment_bytes[footer_start..])
            .map_err(|e| BufferError::SegmentBuild(e.to_string()))?;

        let meta = SegmentMeta::from_footer(&footer, segment_bytes.len());

        Ok((segment_bytes, meta))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_memtable_basic() {
        let memtable = Memtable::new("test-ns".to_string());

        assert!(memtable.is_empty());
        assert_eq!(memtable.len(), 0);
        assert!(memtable.age().is_none());

        memtable.add(LogEntry::new(
            1705323600000,
            br#"{"level":"info","service":"api","message":"test"}"#.to_vec(),
        ));

        assert!(!memtable.is_empty());
        assert_eq!(memtable.len(), 1);
        assert!(memtable.age().is_some());
    }

    #[test]
    fn test_memtable_multiple_entries() {
        let memtable = Memtable::new("test-ns".to_string());

        for i in 0..10 {
            memtable.add(LogEntry::new(
                1705323600000 + i * 1000,
                format!(r#"{{"level":"info","message":"msg {i}"}}"#).into_bytes(),
            ));
        }

        assert_eq!(memtable.len(), 10);
        assert!(memtable.estimated_size() > 0);
    }

    #[test]
    fn test_memtable_drain_and_build() {
        let memtable = Memtable::new("test-ns".to_string());

        memtable.add(LogEntry::new(
            1705323600000,
            br#"{"level":"error","service":"api","message":"error 1"}"#.to_vec(),
        ));
        memtable.add(LogEntry::new(
            1705323600100,
            br#"{"level":"warn","service":"api","message":"warning 1"}"#.to_vec(),
        ));

        let (segment_bytes, meta) = memtable.drain_and_build().unwrap();

        assert!(!segment_bytes.is_empty());
        assert_eq!(meta.entry_count, 2);
        assert_eq!(meta.min_timestamp, 1705323600000);
        assert_eq!(meta.max_timestamp, 1705323600100);
        assert!(meta.size_bytes > 0);

        // Memtable should be empty after drain
        assert!(memtable.is_empty());
    }

    #[test]
    fn test_memtable_drain_empty_fails() {
        let memtable = Memtable::new("test-ns".to_string());
        let result = memtable.drain_and_build();
        assert!(result.is_err());
    }

    #[test]
    fn test_memtable_concurrent_writes() {
        let memtable = Arc::new(Memtable::new("test-ns".to_string()));
        let num_threads = 4;
        let entries_per_thread = 100;

        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                let memtable = Arc::clone(&memtable);
                thread::spawn(move || {
                    for i in 0..entries_per_thread {
                        let ts = 1705323600000 + (t * 1000 + i) as i64;
                        memtable.add(LogEntry::new(
                            ts,
                            format!(r#"{{"thread":{t},"index":{i}}}"#).into_bytes(),
                        ));
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(memtable.len(), num_threads * entries_per_thread);

        // Verify all entries can be drained and built
        let (segment_bytes, meta) = memtable.drain_and_build().unwrap();
        assert_eq!(meta.entry_count, (num_threads * entries_per_thread) as u32);
        assert!(!segment_bytes.is_empty());
    }

    #[test]
    fn test_memtable_namespace_id() {
        let memtable = Memtable::new("my-namespace".to_string());
        assert_eq!(memtable.namespace_id(), "my-namespace");
    }

    #[test]
    fn test_log_entry() {
        let entry = LogEntry::new(1705323600000, b"test payload".to_vec());
        assert_eq!(entry.timestamp, 1705323600000);
        assert_eq!(entry.payload, b"test payload");
        assert_eq!(entry.size(), 8 + 12); // timestamp + payload
    }
}
