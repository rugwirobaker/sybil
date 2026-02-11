//! Bloom filter cache.
//!
//! Caches bloom filters extracted from segments to enable fast field value
//! filtering without reading segment data.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::segment::BloomFilter;

use super::config::BloomCacheConfig;
use super::metrics::LayerMetrics;

/// Key for bloom cache entries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BloomKey {
    /// The segment's S3 key.
    pub segment_key: String,
    /// The field ID (index into AUTO_INDEX_FIELDS).
    pub field_id: u8,
}

impl BloomKey {
    /// Create a new bloom key.
    pub fn new(segment_key: impl Into<String>, field_id: u8) -> Self {
        Self {
            segment_key: segment_key.into(),
            field_id,
        }
    }
}

/// Internal state for bloom cache.
struct BloomCacheInner {
    /// Map of segment key to field blooms.
    /// Using nested structure for efficient per-segment operations.
    segments: HashMap<String, HashMap<u8, BloomFilter>>,
    /// Total number of bloom filters cached.
    total_blooms: usize,
}

/// Cache for bloom filters.
///
/// Stores bloom filters keyed by (segment_key, field_id) for fast field
/// value filtering during queries.
pub struct BloomCache {
    inner: RwLock<BloomCacheInner>,
    metrics: Arc<LayerMetrics>,
    max_segments: usize,
    enabled: bool,
}

impl BloomCache {
    /// Create a new bloom cache.
    pub fn new(config: &BloomCacheConfig, metrics: Arc<LayerMetrics>) -> Self {
        Self {
            inner: RwLock::new(BloomCacheInner {
                segments: HashMap::new(),
                total_blooms: 0,
            }),
            metrics,
            max_segments: config.max_segments,
            enabled: config.enabled,
        }
    }

    /// Check if a field might contain a value.
    ///
    /// Returns:
    /// - `Some(true)` if the bloom filter says it might contain the value
    /// - `Some(false)` if the bloom filter says it definitely doesn't
    /// - `None` if the bloom filter is not cached
    pub fn check(&self, segment_key: &str, field_id: u8, value: &[u8]) -> Option<bool> {
        self.get(segment_key, field_id)
            .map(|bloom| bloom.might_contain(value))
    }

    /// Get a specific bloom filter.
    pub fn get(&self, segment_key: &str, field_id: u8) -> Option<BloomFilter> {
        if !self.enabled {
            return None;
        }

        let inner = self.inner.read();
        if let Some(field_blooms) = inner.segments.get(segment_key) {
            if let Some(bloom) = field_blooms.get(&field_id) {
                self.metrics.record_hit();
                return Some(bloom.clone());
            }
        }

        self.metrics.record_miss();
        None
    }

    /// Insert all bloom filters for a segment at once.
    ///
    /// This is the preferred way to populate the cache since segments
    /// are typically read all at once.
    pub fn insert_all(&self, segment_key: impl Into<String>, blooms: Vec<(u8, BloomFilter)>) {
        if !self.enabled || blooms.is_empty() {
            return;
        }

        let segment_key = segment_key.into();
        let mut inner = self.inner.write();

        // Evict if at capacity
        while inner.segments.len() >= self.max_segments && !inner.segments.is_empty() {
            self.evict_one(&mut inner);
        }

        // Calculate size for metrics
        let size_bytes: u64 = blooms.iter().map(|(_, b)| b.size_bytes() as u64).sum();

        // Insert new blooms
        let bloom_count = blooms.len();
        let field_blooms: HashMap<u8, BloomFilter> = blooms.into_iter().collect();

        if let Some(existing) = inner.segments.insert(segment_key, field_blooms) {
            // Replace existing - update counts
            inner.total_blooms -= existing.len();
            self.metrics
                .record_eviction(existing.values().map(|b| b.size_bytes() as u64).sum());
        }

        inner.total_blooms += bloom_count;
        self.metrics.record_insert(size_bytes);
    }

    /// Insert a single bloom filter.
    pub fn insert(&self, segment_key: impl Into<String>, field_id: u8, bloom: BloomFilter) {
        if !self.enabled {
            return;
        }

        let segment_key = segment_key.into();
        let size_bytes = bloom.size_bytes() as u64;
        let mut inner = self.inner.write();

        // Evict if at capacity and this is a new segment
        if !inner.segments.contains_key(&segment_key) && inner.segments.len() >= self.max_segments {
            self.evict_one(&mut inner);
        }

        let field_blooms = inner.segments.entry(segment_key).or_default();

        if field_blooms.insert(field_id, bloom).is_none() {
            inner.total_blooms += 1;
            self.metrics.record_insert(size_bytes);
        }
    }

    /// Invalidate all bloom filters for a segment.
    pub fn invalidate_segment(&self, segment_key: &str) {
        let mut inner = self.inner.write();

        if let Some(field_blooms) = inner.segments.remove(segment_key) {
            inner.total_blooms -= field_blooms.len();
            let size: u64 = field_blooms.values().map(|b| b.size_bytes() as u64).sum();
            self.metrics.record_eviction(size);
        }
    }

    /// Evict one segment (simple FIFO for now).
    fn evict_one(&self, inner: &mut BloomCacheInner) {
        // Get any key to evict (HashMap iteration order is arbitrary)
        if let Some(key) = inner.segments.keys().next().cloned() {
            if let Some(field_blooms) = inner.segments.remove(&key) {
                inner.total_blooms -= field_blooms.len();
                let size: u64 = field_blooms.values().map(|b| b.size_bytes() as u64).sum();
                self.metrics.record_eviction(size);
            }
        }
    }

    /// Get the number of segments cached.
    pub fn segment_count(&self) -> usize {
        self.inner.read().segments.len()
    }

    /// Get the total number of bloom filters cached.
    pub fn bloom_count(&self) -> usize {
        self.inner.read().total_blooms
    }

    /// Clear all cached bloom filters.
    pub fn clear(&self) {
        let mut inner = self.inner.write();
        inner.segments.clear();
        inner.total_blooms = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> BloomCacheConfig {
        BloomCacheConfig {
            enabled: true,
            max_segments: 10,
        }
    }

    fn make_bloom(values: &[&[u8]]) -> BloomFilter {
        let mut bloom = BloomFilter::new(values.len().max(10));
        for v in values {
            bloom.insert(v);
        }
        bloom
    }

    #[test]
    fn test_basic_operations() {
        let metrics = Arc::new(LayerMetrics::new());
        let cache = BloomCache::new(&test_config(), Arc::clone(&metrics));

        let bloom = make_bloom(&[b"error", b"warn"]);
        cache.insert("segment-001", 8, bloom);

        // Should find "error"
        let result = cache.check("segment-001", 8, b"error");
        assert_eq!(result, Some(true));

        // Should not find "info" (definitely not in bloom)
        // Note: this could be a false positive, but unlikely with small set
        let result = cache.check("segment-001", 8, b"definitely_not_there_xyz");
        // Can't assert false due to FP possibility, but at least it returns Some
        assert!(result.is_some());

        assert_eq!(metrics.snapshot().hits, 2);
    }

    #[test]
    fn test_cache_miss() {
        let metrics = Arc::new(LayerMetrics::new());
        let cache = BloomCache::new(&test_config(), Arc::clone(&metrics));

        let result = cache.check("nonexistent", 0, b"value");
        assert!(result.is_none());
        assert_eq!(metrics.snapshot().misses, 1);
    }

    #[test]
    fn test_insert_all() {
        let metrics = Arc::new(LayerMetrics::new());
        let cache = BloomCache::new(&test_config(), Arc::clone(&metrics));

        let blooms = vec![
            (0, make_bloom(&[b"api", b"web"])),    // service
            (8, make_bloom(&[b"error", b"warn"])), // level
        ];

        cache.insert_all("segment-001", blooms);

        assert_eq!(cache.segment_count(), 1);
        assert_eq!(cache.bloom_count(), 2);

        assert!(cache.check("segment-001", 0, b"api").is_some());
        assert!(cache.check("segment-001", 8, b"error").is_some());
    }

    #[test]
    fn test_invalidate_segment() {
        let metrics = Arc::new(LayerMetrics::new());
        let cache = BloomCache::new(&test_config(), Arc::clone(&metrics));

        cache.insert_all(
            "segment-001",
            vec![(0, make_bloom(&[b"api"])), (8, make_bloom(&[b"error"]))],
        );
        cache.insert_all("segment-002", vec![(0, make_bloom(&[b"web"]))]);

        assert_eq!(cache.segment_count(), 2);
        assert_eq!(cache.bloom_count(), 3);

        cache.invalidate_segment("segment-001");

        assert_eq!(cache.segment_count(), 1);
        assert_eq!(cache.bloom_count(), 1);
        assert!(cache.check("segment-001", 0, b"api").is_none());
        assert!(cache.check("segment-002", 0, b"web").is_some());
    }

    #[test]
    fn test_eviction() {
        let config = BloomCacheConfig {
            enabled: true,
            max_segments: 2,
        };
        let metrics = Arc::new(LayerMetrics::new());
        let cache = BloomCache::new(&config, Arc::clone(&metrics));

        cache.insert_all("segment-001", vec![(0, make_bloom(&[b"a"]))]);
        cache.insert_all("segment-002", vec![(0, make_bloom(&[b"b"]))]);

        assert_eq!(cache.segment_count(), 2);

        // Should evict one segment
        cache.insert_all("segment-003", vec![(0, make_bloom(&[b"c"]))]);

        assert_eq!(cache.segment_count(), 2);
        assert!(metrics.snapshot().evictions > 0);
    }

    #[test]
    fn test_disabled_cache() {
        let config = BloomCacheConfig {
            enabled: false,
            max_segments: 10,
        };
        let metrics = Arc::new(LayerMetrics::new());
        let cache = BloomCache::new(&config, Arc::clone(&metrics));

        cache.insert("segment-001", 0, make_bloom(&[b"test"]));

        assert!(cache.check("segment-001", 0, b"test").is_none());
        assert_eq!(cache.segment_count(), 0);
    }
}
