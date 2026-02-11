//! Cache metrics for observability.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Metrics for a single cache layer.
#[derive(Debug, Default)]
pub struct LayerMetrics {
    /// Number of cache hits.
    pub hits: AtomicU64,
    /// Number of cache misses.
    pub misses: AtomicU64,
    /// Number of cache insertions.
    pub inserts: AtomicU64,
    /// Number of cache evictions.
    pub evictions: AtomicU64,
    /// Current size in bytes (approximate).
    pub size_bytes: AtomicU64,
    /// Current entry count.
    pub entry_count: AtomicU64,
}

impl LayerMetrics {
    /// Create a new metrics instance.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a cache hit.
    pub fn record_hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache miss.
    pub fn record_miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Record an insertion.
    pub fn record_insert(&self, size_bytes: u64) {
        self.inserts.fetch_add(1, Ordering::Relaxed);
        self.entry_count.fetch_add(1, Ordering::Relaxed);
        self.size_bytes.fetch_add(size_bytes, Ordering::Relaxed);
    }

    /// Record an eviction.
    pub fn record_eviction(&self, size_bytes: u64) {
        self.evictions.fetch_add(1, Ordering::Relaxed);
        self.entry_count.fetch_sub(1, Ordering::Relaxed);
        self.size_bytes.fetch_sub(size_bytes, Ordering::Relaxed);
    }

    /// Get the hit rate (0.0 to 1.0).
    pub fn hit_rate(&self) -> f64 {
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total = hits + misses;
        if total == 0 {
            0.0
        } else {
            hits as f64 / total as f64
        }
    }

    /// Get a snapshot of the current metrics.
    pub fn snapshot(&self) -> LayerMetricsSnapshot {
        LayerMetricsSnapshot {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            inserts: self.inserts.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            size_bytes: self.size_bytes.load(Ordering::Relaxed),
            entry_count: self.entry_count.load(Ordering::Relaxed),
        }
    }

    /// Reset all counters to zero.
    pub fn reset(&self) {
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
        self.inserts.store(0, Ordering::Relaxed);
        self.evictions.store(0, Ordering::Relaxed);
        self.size_bytes.store(0, Ordering::Relaxed);
        self.entry_count.store(0, Ordering::Relaxed);
    }
}

/// A point-in-time snapshot of layer metrics.
#[derive(Debug, Clone, Copy)]
pub struct LayerMetricsSnapshot {
    pub hits: u64,
    pub misses: u64,
    pub inserts: u64,
    pub evictions: u64,
    pub size_bytes: u64,
    pub entry_count: u64,
}

impl LayerMetricsSnapshot {
    /// Get the hit rate (0.0 to 1.0).
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// Aggregated metrics across all cache layers.
#[derive(Debug)]
pub struct CacheMetrics {
    /// Segment cache (L2) metrics.
    pub segment: Arc<LayerMetrics>,
    /// Block cache (L1) metrics.
    pub block: Arc<LayerMetrics>,
    /// Bloom filter cache metrics.
    pub bloom: Arc<LayerMetrics>,
    /// Manifest cache metrics.
    pub manifest: Arc<LayerMetrics>,
    /// Catalog cache metrics.
    pub catalog: Arc<LayerMetrics>,
}

impl Default for CacheMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl CacheMetrics {
    /// Create a new metrics instance.
    pub fn new() -> Self {
        Self {
            segment: Arc::new(LayerMetrics::new()),
            block: Arc::new(LayerMetrics::new()),
            bloom: Arc::new(LayerMetrics::new()),
            manifest: Arc::new(LayerMetrics::new()),
            catalog: Arc::new(LayerMetrics::new()),
        }
    }

    /// Get a snapshot of all metrics.
    pub fn snapshot(&self) -> CacheMetricsSnapshot {
        CacheMetricsSnapshot {
            segment: self.segment.snapshot(),
            block: self.block.snapshot(),
            bloom: self.bloom.snapshot(),
            manifest: self.manifest.snapshot(),
            catalog: self.catalog.snapshot(),
        }
    }

    /// Reset all metrics.
    pub fn reset(&self) {
        self.segment.reset();
        self.block.reset();
        self.bloom.reset();
        self.manifest.reset();
        self.catalog.reset();
    }
}

/// A point-in-time snapshot of all cache metrics.
#[derive(Debug, Clone)]
pub struct CacheMetricsSnapshot {
    pub segment: LayerMetricsSnapshot,
    pub block: LayerMetricsSnapshot,
    pub bloom: LayerMetricsSnapshot,
    pub manifest: LayerMetricsSnapshot,
    pub catalog: LayerMetricsSnapshot,
}

impl CacheMetricsSnapshot {
    /// Total size across all caches in bytes.
    pub fn total_size_bytes(&self) -> u64 {
        self.segment.size_bytes
            + self.block.size_bytes
            + self.bloom.size_bytes
            + self.manifest.size_bytes
            + self.catalog.size_bytes
    }

    /// Total entry count across all caches.
    pub fn total_entry_count(&self) -> u64 {
        self.segment.entry_count
            + self.block.entry_count
            + self.bloom.entry_count
            + self.manifest.entry_count
            + self.catalog.entry_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_layer_metrics() {
        let metrics = LayerMetrics::new();

        metrics.record_hit();
        metrics.record_hit();
        metrics.record_miss();

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.hits, 2);
        assert_eq!(snapshot.misses, 1);
        assert!((snapshot.hit_rate() - 0.666).abs() < 0.01);
    }

    #[test]
    fn test_layer_metrics_insert_evict() {
        let metrics = LayerMetrics::new();

        metrics.record_insert(1000);
        metrics.record_insert(2000);
        assert_eq!(metrics.snapshot().size_bytes, 3000);
        assert_eq!(metrics.snapshot().entry_count, 2);

        metrics.record_eviction(1000);
        assert_eq!(metrics.snapshot().size_bytes, 2000);
        assert_eq!(metrics.snapshot().entry_count, 1);
    }

    #[test]
    fn test_cache_metrics_reset() {
        let metrics = CacheMetrics::new();
        metrics.segment.record_hit();
        metrics.block.record_miss();

        metrics.reset();

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.segment.hits, 0);
        assert_eq!(snapshot.block.misses, 0);
    }

    #[test]
    fn test_hit_rate_empty() {
        let metrics = LayerMetrics::new();
        assert_eq!(metrics.hit_rate(), 0.0);
    }
}
