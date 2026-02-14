//! Buffer module for memtable-based log ingestion.
//!
//! This module provides a concurrent buffer system for the Indexer. Incoming
//! logs are buffered per-namespace before being flushed to S3 as L0 segments.
//!
//! # Concurrency Model
//!
//! - **Writes**: Lock-free via `crossbeam::SegQueue`
//! - **Namespace lookup**: `RwLock` on HashMap (read-heavy)
//! - **Flush**: Serialized per-namespace via `Mutex`
//!
//! Multiple HTTP handlers can write to the same namespace buffer concurrently
//! without contention. Only one flush operation runs per namespace at a time.

pub mod error;
pub mod memtable;

pub use error::{BufferError, Result};
pub use memtable::{LogEntry, Memtable, SegmentMeta};

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};

use crate::config::BufferConfig;

/// Why a flush was triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushReason {
    /// Size threshold exceeded.
    Size { current: usize, threshold: usize },
    /// Time threshold exceeded.
    Time { age_secs: u64, threshold_secs: u64 },
    /// Entry count threshold exceeded.
    Count { current: usize, threshold: usize },
    /// Manual flush requested.
    Manual,
}

/// Result of checking if a buffer is ready to flush.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushDecision {
    /// No flush needed.
    None,
    /// Buffer should be flushed.
    Ready(FlushReason),
}

/// Result of a drain operation.
pub struct DrainResult {
    /// The built segment bytes.
    pub segment_bytes: Vec<u8>,
    /// Metadata about the segment.
    pub meta: SegmentMeta,
    /// The namespace that was drained.
    pub namespace_id: String,
}

/// Metrics for buffer operations.
#[derive(Debug, Default)]
pub struct BufferMetrics {
    /// Total entries added across all namespaces.
    pub entries_added: AtomicU64,
    /// Total flushes performed.
    pub flushes: AtomicU64,
    /// Total bytes flushed.
    pub bytes_flushed: AtomicU64,
    /// Current number of active namespaces.
    pub active_namespaces: AtomicU64,
}

impl BufferMetrics {
    /// Create new metrics.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an entry being added.
    pub fn record_add(&self, count: u64) {
        self.entries_added.fetch_add(count, Ordering::Relaxed);
    }

    /// Record a flush operation.
    pub fn record_flush(&self, bytes: u64) {
        self.flushes.fetch_add(1, Ordering::Relaxed);
        self.bytes_flushed.fetch_add(bytes, Ordering::Relaxed);
    }

    /// Update active namespace count.
    pub fn set_active_namespaces(&self, count: u64) {
        self.active_namespaces.store(count, Ordering::Relaxed);
    }
}

/// Per-namespace buffer state.
struct NamespaceBuffer {
    /// The memtable (lock-free writes).
    memtable: Arc<Memtable>,
    /// Lock for flush serialization (only one flush at a time per namespace).
    flush_lock: Mutex<()>,
}

impl NamespaceBuffer {
    fn new(namespace_id: String) -> Self {
        Self {
            memtable: Arc::new(Memtable::new(namespace_id)),
            flush_lock: Mutex::new(()),
        }
    }
}

/// Manager for namespace buffers.
///
/// Provides concurrent access to per-namespace memtables with lock-free writes
/// and serialized flushes.
pub struct BufferManager {
    /// Per-namespace buffers.
    buffers: RwLock<HashMap<String, Arc<NamespaceBuffer>>>,
    /// Configuration.
    config: BufferConfig,
    /// Metrics.
    metrics: Arc<BufferMetrics>,
}

impl BufferManager {
    /// Create a new buffer manager with the given configuration.
    pub fn new(config: BufferConfig) -> Self {
        Self {
            buffers: RwLock::new(HashMap::new()),
            config,
            metrics: Arc::new(BufferMetrics::new()),
        }
    }

    /// Create a buffer manager with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(BufferConfig::default())
    }

    /// Get the metrics for this buffer manager.
    pub fn metrics(&self) -> Arc<BufferMetrics> {
        Arc::clone(&self.metrics)
    }

    /// Get the configuration.
    pub fn config(&self) -> &BufferConfig {
        &self.config
    }

    /// Add a log entry to the specified namespace buffer.
    ///
    /// This is lock-free for existing namespaces. A write lock is only taken
    /// when creating a new namespace (rare operation).
    ///
    /// Returns a flush decision indicating whether the buffer should be flushed.
    pub fn add(&self, namespace_id: &str, entry: LogEntry) -> Result<FlushDecision> {
        // Validate entry size
        if entry.payload.len() > self.config.max_entry_size {
            return Err(BufferError::EntryTooLarge {
                size: entry.payload.len(),
                max: self.config.max_entry_size,
            });
        }

        // Fast path: read lock to get existing buffer
        let buffer = {
            let buffers = self.buffers.read();
            buffers.get(namespace_id).map(Arc::clone)
        };

        let buffer = match buffer {
            Some(b) => b,
            None => {
                // Slow path: write lock to create new buffer
                let mut buffers = self.buffers.write();
                // Double-check after acquiring write lock
                let buffer = buffers.entry(namespace_id.to_string()).or_insert_with(|| {
                    self.metrics
                        .active_namespaces
                        .fetch_add(1, Ordering::Relaxed);
                    Arc::new(NamespaceBuffer::new(namespace_id.to_string()))
                });
                Arc::clone(buffer)
            }
        };

        // Lock-free write to memtable
        buffer.memtable.add(entry);
        self.metrics.record_add(1);

        // Check flush decision
        Ok(self.check_flush_decision(&buffer.memtable))
    }

    /// Add multiple log entries to the specified namespace buffer.
    ///
    /// More efficient than calling `add()` multiple times.
    pub fn add_batch(&self, namespace_id: &str, entries: Vec<LogEntry>) -> Result<FlushDecision> {
        if entries.is_empty() {
            return Ok(FlushDecision::None);
        }

        // Validate entry sizes
        for entry in &entries {
            if entry.payload.len() > self.config.max_entry_size {
                return Err(BufferError::EntryTooLarge {
                    size: entry.payload.len(),
                    max: self.config.max_entry_size,
                });
            }
        }

        // Get or create buffer (same logic as add())
        let buffer = {
            let buffers = self.buffers.read();
            buffers.get(namespace_id).map(Arc::clone)
        };

        let buffer = match buffer {
            Some(b) => b,
            None => {
                let mut buffers = self.buffers.write();
                let buffer = buffers.entry(namespace_id.to_string()).or_insert_with(|| {
                    self.metrics
                        .active_namespaces
                        .fetch_add(1, Ordering::Relaxed);
                    Arc::new(NamespaceBuffer::new(namespace_id.to_string()))
                });
                Arc::clone(buffer)
            }
        };

        // Lock-free writes
        let count = entries.len() as u64;
        for entry in entries {
            buffer.memtable.add(entry);
        }
        self.metrics.record_add(count);

        Ok(self.check_flush_decision(&buffer.memtable))
    }

    /// Check if a buffer needs to be flushed based on configuration thresholds.
    fn check_flush_decision(&self, memtable: &Memtable) -> FlushDecision {
        // Check size threshold
        let size = memtable.estimated_size();
        if size >= self.config.segment_size_bytes {
            return FlushDecision::Ready(FlushReason::Size {
                current: size,
                threshold: self.config.segment_size_bytes,
            });
        }

        // Check entry count threshold
        let count = memtable.len();
        if count >= self.config.max_entries {
            return FlushDecision::Ready(FlushReason::Count {
                current: count,
                threshold: self.config.max_entries,
            });
        }

        // Check time threshold
        let flush_interval = Duration::from_secs(self.config.flush_interval_secs);
        if let Some(age) = memtable.age() {
            if age >= flush_interval {
                return FlushDecision::Ready(FlushReason::Time {
                    age_secs: age.as_secs(),
                    threshold_secs: self.config.flush_interval_secs,
                });
            }
        }

        FlushDecision::None
    }

    /// Drain a namespace buffer and build a segment.
    ///
    /// Only one flush can run per namespace at a time. If another flush is
    /// already in progress, this will block until it completes.
    ///
    /// Returns `None` if the namespace doesn't exist or the buffer is empty.
    pub fn drain(&self, namespace_id: &str) -> Result<Option<DrainResult>> {
        let buffer = {
            let buffers = self.buffers.read();
            buffers.get(namespace_id).map(Arc::clone)
        };

        let buffer = match buffer {
            Some(b) => b,
            None => return Ok(None),
        };

        // Serialize flush operations for this namespace
        let _flush_guard = buffer.flush_lock.lock();

        // Check if there's anything to flush
        if buffer.memtable.is_empty() {
            return Ok(None);
        }

        // Drain and build segment
        let (segment_bytes, meta) = buffer.memtable.drain_and_build()?;

        self.metrics.record_flush(segment_bytes.len() as u64);

        Ok(Some(DrainResult {
            segment_bytes,
            meta,
            namespace_id: namespace_id.to_string(),
        }))
    }

    /// Get all namespaces that are ready for flushing.
    ///
    /// Returns namespace IDs and their flush reasons.
    pub fn ready_for_flush(&self) -> Vec<(String, FlushReason)> {
        let buffers = self.buffers.read();
        let mut ready = Vec::new();

        for (ns_id, buffer) in buffers.iter() {
            if let FlushDecision::Ready(reason) = self.check_flush_decision(&buffer.memtable) {
                ready.push((ns_id.clone(), reason));
            }
        }

        ready
    }

    /// Get all namespace IDs with active buffers.
    pub fn namespaces(&self) -> Vec<String> {
        self.buffers.read().keys().cloned().collect()
    }

    /// Get buffer stats for a namespace.
    pub fn stats(&self, namespace_id: &str) -> Option<BufferStats> {
        let buffers = self.buffers.read();
        buffers.get(namespace_id).map(|b| BufferStats {
            entry_count: b.memtable.len(),
            estimated_size: b.memtable.estimated_size(),
            age_secs: b.memtable.age().map(|d| d.as_secs()),
        })
    }

    /// Remove a namespace buffer (e.g., after it's been idle).
    pub fn remove(&self, namespace_id: &str) -> bool {
        let mut buffers = self.buffers.write();
        if buffers.remove(namespace_id).is_some() {
            self.metrics
                .active_namespaces
                .fetch_sub(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
}

/// Statistics for a single namespace buffer.
#[derive(Debug, Clone)]
pub struct BufferStats {
    /// Number of entries in the buffer.
    pub entry_count: usize,
    /// Estimated size in bytes.
    pub estimated_size: usize,
    /// Age in seconds (time since first entry), if any entries exist.
    pub age_secs: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_buffer_manager_basic() {
        let manager = BufferManager::with_defaults();

        let entry = LogEntry::new(
            1705323600000,
            br#"{"level":"info","message":"test"}"#.to_vec(),
        );

        let decision = manager.add("test-ns", entry).unwrap();
        assert_eq!(decision, FlushDecision::None);

        let stats = manager.stats("test-ns").unwrap();
        assert_eq!(stats.entry_count, 1);
    }

    #[test]
    fn test_buffer_manager_size_threshold() {
        let config = BufferConfig {
            segment_size_bytes: 100,
            ..Default::default()
        };
        let manager = BufferManager::new(config);

        // Add entries until we exceed threshold
        for i in 0..10 {
            let entry = LogEntry::new(
                1705323600000 + i,
                format!(
                    r#"{{"level":"info","message":"test message number {}"}}"#,
                    i
                )
                .into_bytes(),
            );
            let decision = manager.add("test-ns", entry).unwrap();

            if let FlushDecision::Ready(FlushReason::Size { current, threshold }) = decision {
                assert!(current >= threshold);
                return;
            }
        }

        // Should have triggered by now
        panic!("Size threshold should have triggered a flush");
    }

    #[test]
    fn test_buffer_manager_entry_too_large() {
        let config = BufferConfig::new().with_max_entry_size(10);
        let manager = BufferManager::new(config);

        let entry = LogEntry::new(1705323600000, b"this is way too large".to_vec());

        let result = manager.add("test-ns", entry);
        assert!(matches!(result, Err(BufferError::EntryTooLarge { .. })));
    }

    #[test]
    fn test_buffer_manager_drain() {
        let manager = BufferManager::with_defaults();

        manager
            .add(
                "test-ns",
                LogEntry::new(
                    1705323600000,
                    br#"{"level":"info","message":"test 1"}"#.to_vec(),
                ),
            )
            .unwrap();
        manager
            .add(
                "test-ns",
                LogEntry::new(
                    1705323600100,
                    br#"{"level":"info","message":"test 2"}"#.to_vec(),
                ),
            )
            .unwrap();

        let result = manager.drain("test-ns").unwrap().unwrap();

        assert_eq!(result.namespace_id, "test-ns");
        assert_eq!(result.meta.entry_count, 2);
        assert!(!result.segment_bytes.is_empty());

        // Buffer should be empty after drain
        let stats = manager.stats("test-ns").unwrap();
        assert_eq!(stats.entry_count, 0);
    }

    #[test]
    fn test_buffer_manager_drain_nonexistent() {
        let manager = BufferManager::with_defaults();
        let result = manager.drain("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_buffer_manager_concurrent_writes() {
        let manager = Arc::new(BufferManager::with_defaults());
        let num_threads = 4;
        let entries_per_thread = 100;

        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                let manager = Arc::clone(&manager);
                thread::spawn(move || {
                    for i in 0..entries_per_thread {
                        let ts = 1705323600000 + (t * 1000 + i) as i64;
                        let entry = LogEntry::new(
                            ts,
                            format!(r#"{{"thread":{},"index":{}}}"#, t, i).into_bytes(),
                        );
                        manager.add("shared-ns", entry).unwrap();
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        let stats = manager.stats("shared-ns").unwrap();
        assert_eq!(stats.entry_count, num_threads * entries_per_thread);
    }

    #[test]
    fn test_buffer_manager_multiple_namespaces() {
        let manager = BufferManager::with_defaults();

        for ns in ["ns-1", "ns-2", "ns-3"] {
            manager
                .add(
                    ns,
                    LogEntry::new(
                        1705323600000,
                        format!(r#"{{"namespace":"{}"}}"#, ns).into_bytes(),
                    ),
                )
                .unwrap();
        }

        let namespaces = manager.namespaces();
        assert_eq!(namespaces.len(), 3);
        assert!(namespaces.contains(&"ns-1".to_string()));
        assert!(namespaces.contains(&"ns-2".to_string()));
        assert!(namespaces.contains(&"ns-3".to_string()));
    }

    #[test]
    fn test_buffer_manager_ready_for_flush() {
        let config = BufferConfig::new()
            .with_segment_size_bytes(100)
            .with_flush_interval_secs(0);
        let manager = BufferManager::new(config);

        // Add enough data to trigger size threshold
        for i in 0..5 {
            manager
                .add(
                    "ns-size",
                    LogEntry::new(
                        1705323600000 + i,
                        format!(r#"{{"message":"large payload {}"}}"#, i).into_bytes(),
                    ),
                )
                .unwrap();
        }

        // Add small amount and wait for time threshold
        manager
            .add(
                "ns-time",
                LogEntry::new(1705323600000, br#"{"small":"data"}"#.to_vec()),
            )
            .unwrap();
        thread::sleep(Duration::from_millis(10));

        let ready = manager.ready_for_flush();
        assert!(!ready.is_empty());
    }

    #[test]
    fn test_buffer_manager_remove() {
        let manager = BufferManager::with_defaults();

        manager
            .add(
                "test-ns",
                LogEntry::new(1705323600000, br#"{"level":"info"}"#.to_vec()),
            )
            .unwrap();

        assert!(manager.stats("test-ns").is_some());
        assert!(manager.remove("test-ns"));
        assert!(manager.stats("test-ns").is_none());
        assert!(!manager.remove("test-ns")); // Already removed
    }

    #[test]
    fn test_buffer_manager_add_batch() {
        let manager = BufferManager::with_defaults();

        let entries: Vec<_> = (0..5)
            .map(|i| {
                LogEntry::new(
                    1705323600000 + i * 100,
                    format!(r#"{{"index":{}}}"#, i).into_bytes(),
                )
            })
            .collect();

        manager.add_batch("test-ns", entries).unwrap();

        let stats = manager.stats("test-ns").unwrap();
        assert_eq!(stats.entry_count, 5);
    }

    #[test]
    fn test_buffer_stats() {
        let manager = BufferManager::with_defaults();

        manager
            .add(
                "test-ns",
                LogEntry::new(1705323600000, br#"{"data":"test"}"#.to_vec()),
            )
            .unwrap();

        let stats = manager.stats("test-ns").unwrap();
        assert_eq!(stats.entry_count, 1);
        assert!(stats.estimated_size > 0);
        assert!(stats.age_secs.is_some());

        assert!(manager.stats("nonexistent").is_none());
    }
}
