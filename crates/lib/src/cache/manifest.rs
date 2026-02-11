//! Manifest cache - per-namespace segment registry cache.
//!
//! Caches manifest state with ETag-based validation to avoid repeated
//! S3 fetches while ensuring consistency.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::manifest::{Head, LongEntry, Snapshot, State};

use super::config::ManifestCacheConfig;
use super::metrics::LayerMetrics;

/// Cached manifest data for a namespace.
#[derive(Debug, Clone)]
pub struct CachedManifest {
    /// The manifest head pointer.
    pub head: Head,
    /// ETag of the head file for validation.
    pub head_etag: Option<String>,
    /// The base snapshot.
    pub snapshot: Snapshot,
    /// Log entries applied on top of the snapshot.
    pub log_entries: Vec<LongEntry>,
    /// Pre-computed state for fast access.
    pub state: State,
    /// When this cache entry was last validated against S3.
    pub last_validated: Instant,
}

impl CachedManifest {
    /// Create a new cached manifest.
    pub fn new(
        head: Head,
        head_etag: Option<String>,
        snapshot: Snapshot,
        log_entries: Vec<LongEntry>,
    ) -> Self {
        let state = State::from_snapshot_and_log(&snapshot, &log_entries);
        Self {
            head,
            head_etag,
            snapshot,
            log_entries,
            state,
            last_validated: Instant::now(),
        }
    }

    /// Check if the cached state needs revalidation.
    pub fn needs_validation(&self, interval: Duration) -> bool {
        self.last_validated.elapsed() > interval
    }

    /// Mark as freshly validated.
    pub fn mark_validated(&mut self) {
        self.last_validated = Instant::now();
    }
}

/// Internal state for manifest cache.
struct ManifestCacheInner {
    /// Map of namespace_id to cached manifest.
    manifests: HashMap<String, CachedManifest>,
}

/// Cache for per-namespace manifests.
///
/// Provides fast access to namespace segment state with ETag-based
/// validation to maintain consistency.
pub struct ManifestCache {
    inner: RwLock<ManifestCacheInner>,
    metrics: Arc<LayerMetrics>,
    max_namespaces: usize,
    validation_interval: Duration,
    enabled: bool,
}

impl ManifestCache {
    /// Create a new manifest cache.
    pub fn new(config: &ManifestCacheConfig, metrics: Arc<LayerMetrics>) -> Self {
        Self {
            inner: RwLock::new(ManifestCacheInner {
                manifests: HashMap::new(),
            }),
            metrics,
            max_namespaces: config.max_namespaces,
            validation_interval: config.validation_interval,
            enabled: config.enabled,
        }
    }

    /// Get a cached manifest for a namespace.
    ///
    /// Returns the cached manifest if it exists. Caller should check
    /// `needs_validation()` and validate the ETag if needed.
    pub fn get(&self, namespace_id: &str) -> Option<CachedManifest> {
        if !self.enabled {
            return None;
        }

        let inner = self.inner.read();
        match inner.manifests.get(namespace_id) {
            Some(cached) => {
                self.metrics.record_hit();
                Some(cached.clone())
            }
            None => {
                self.metrics.record_miss();
                None
            }
        }
    }

    /// Get the computed state for a namespace.
    pub fn get_state(&self, namespace_id: &str) -> Option<State> {
        self.get(namespace_id).map(|c| c.state)
    }

    /// Check if a namespace needs validation.
    pub fn needs_validation(&self, namespace_id: &str) -> bool {
        if !self.enabled {
            return true;
        }

        let inner = self.inner.read();
        match inner.manifests.get(namespace_id) {
            Some(cached) => cached.needs_validation(self.validation_interval),
            None => true,
        }
    }

    /// Get the cached ETag for validation.
    pub fn get_etag(&self, namespace_id: &str) -> Option<String> {
        if !self.enabled {
            return None;
        }

        let inner = self.inner.read();
        inner
            .manifests
            .get(namespace_id)
            .and_then(|c| c.head_etag.clone())
    }

    /// Insert or update a cached manifest.
    pub fn insert(&self, namespace_id: impl Into<String>, manifest: CachedManifest) {
        if !self.enabled {
            return;
        }

        let namespace_id = namespace_id.into();
        let mut inner = self.inner.write();

        // Evict if at capacity and this is a new namespace
        if !inner.manifests.contains_key(&namespace_id)
            && inner.manifests.len() >= self.max_namespaces
        {
            self.evict_one(&mut inner);
        }

        // Estimate size for metrics (rough approximation)
        let size_bytes = Self::estimate_size(&manifest);

        if inner.manifests.insert(namespace_id, manifest).is_none() {
            self.metrics.record_insert(size_bytes);
        }
    }

    /// Mark a namespace's cache as validated (ETag matched).
    pub fn mark_validated(&self, namespace_id: &str) {
        if !self.enabled {
            return;
        }

        let mut inner = self.inner.write();
        if let Some(cached) = inner.manifests.get_mut(namespace_id) {
            cached.mark_validated();
        }
    }

    /// Invalidate a namespace's cached manifest.
    pub fn invalidate(&self, namespace_id: &str) {
        let mut inner = self.inner.write();
        if let Some(cached) = inner.manifests.remove(namespace_id) {
            let size = Self::estimate_size(&cached);
            self.metrics.record_eviction(size);
        }
    }

    /// Evict one namespace (oldest validated).
    fn evict_one(&self, inner: &mut ManifestCacheInner) {
        // Find namespace with oldest validation time
        let evict_key = inner
            .manifests
            .iter()
            .min_by_key(|(_, v)| v.last_validated)
            .map(|(k, _)| k.clone());

        if let Some(key) = evict_key {
            if let Some(cached) = inner.manifests.remove(&key) {
                let size = Self::estimate_size(&cached);
                self.metrics.record_eviction(size);
            }
        }
    }

    /// Estimate memory size of a cached manifest.
    fn estimate_size(cached: &CachedManifest) -> u64 {
        // Rough estimate: head + snapshot segments + log entries
        let segments_size = cached.snapshot.segments.len() * 200; // ~200 bytes per segment
        let log_size = cached.log_entries.len() * 300; // ~300 bytes per entry
        (100 + segments_size + log_size) as u64
    }

    /// Get number of cached namespaces.
    pub fn len(&self) -> usize {
        self.inner.read().manifests.len()
    }

    /// Check if cache is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.read().manifests.is_empty()
    }

    /// Clear all cached manifests.
    pub fn clear(&self) {
        let mut inner = self.inner.write();
        inner.manifests.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{SegmentInfo, SegmentLevel};

    fn test_config() -> ManifestCacheConfig {
        ManifestCacheConfig {
            enabled: true,
            max_namespaces: 10,
            validation_interval: Duration::from_secs(30),
        }
    }

    fn make_manifest() -> CachedManifest {
        let head = Head::new("snapshot-test.json".to_string());
        let snapshot = Snapshot::new(vec![SegmentInfo::new(
            "test.seg".to_string(),
            SegmentLevel::L0,
            1000,
            2000,
            100,
            65536,
        )]);
        CachedManifest::new(head, Some("etag-123".to_string()), snapshot, vec![])
    }

    #[test]
    fn test_basic_operations() {
        let metrics = Arc::new(LayerMetrics::new());
        let cache = ManifestCache::new(&test_config(), Arc::clone(&metrics));

        cache.insert("ns_001", make_manifest());

        let cached = cache.get("ns_001");
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().state.len(), 1);

        assert_eq!(metrics.snapshot().hits, 1);
        assert_eq!(metrics.snapshot().inserts, 1);
    }

    #[test]
    fn test_cache_miss() {
        let metrics = Arc::new(LayerMetrics::new());
        let cache = ManifestCache::new(&test_config(), Arc::clone(&metrics));

        let result = cache.get("nonexistent");
        assert!(result.is_none());
        assert_eq!(metrics.snapshot().misses, 1);
    }

    #[test]
    fn test_validation_check() {
        let config = ManifestCacheConfig {
            enabled: true,
            max_namespaces: 10,
            validation_interval: Duration::from_millis(10),
        };
        let metrics = Arc::new(LayerMetrics::new());
        let cache = ManifestCache::new(&config, Arc::clone(&metrics));

        cache.insert("ns_001", make_manifest());

        // Initially should not need validation
        assert!(!cache.needs_validation("ns_001"));

        // Wait for validation interval
        std::thread::sleep(Duration::from_millis(15));

        // Now should need validation
        assert!(cache.needs_validation("ns_001"));

        // Mark validated
        cache.mark_validated("ns_001");

        // Should not need validation again
        assert!(!cache.needs_validation("ns_001"));
    }

    #[test]
    fn test_get_etag() {
        let metrics = Arc::new(LayerMetrics::new());
        let cache = ManifestCache::new(&test_config(), Arc::clone(&metrics));

        cache.insert("ns_001", make_manifest());

        let etag = cache.get_etag("ns_001");
        assert_eq!(etag, Some("etag-123".to_string()));

        let missing_etag = cache.get_etag("nonexistent");
        assert!(missing_etag.is_none());
    }

    #[test]
    fn test_invalidate() {
        let metrics = Arc::new(LayerMetrics::new());
        let cache = ManifestCache::new(&test_config(), Arc::clone(&metrics));

        cache.insert("ns_001", make_manifest());
        assert_eq!(cache.len(), 1);

        cache.invalidate("ns_001");
        assert_eq!(cache.len(), 0);
        assert!(cache.get("ns_001").is_none());
    }

    #[test]
    fn test_eviction() {
        let config = ManifestCacheConfig {
            enabled: true,
            max_namespaces: 2,
            validation_interval: Duration::from_secs(30),
        };
        let metrics = Arc::new(LayerMetrics::new());
        let cache = ManifestCache::new(&config, Arc::clone(&metrics));

        cache.insert("ns_001", make_manifest());
        cache.insert("ns_002", make_manifest());

        assert_eq!(cache.len(), 2);

        // Should evict one
        cache.insert("ns_003", make_manifest());

        assert_eq!(cache.len(), 2);
        assert!(metrics.snapshot().evictions > 0);
    }

    #[test]
    fn test_disabled_cache() {
        let config = ManifestCacheConfig {
            enabled: false,
            ..test_config()
        };
        let metrics = Arc::new(LayerMetrics::new());
        let cache = ManifestCache::new(&config, Arc::clone(&metrics));

        cache.insert("ns_001", make_manifest());

        assert!(cache.get("ns_001").is_none());
        assert!(cache.needs_validation("ns_001"));
    }
}
