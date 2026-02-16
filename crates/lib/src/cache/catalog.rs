//! Catalog cache - global namespace registry cache.
//!
//! The catalog is small (even 10k namespaces ~1.5MB) and rarely changes,
//! so we simply hold the entire thing in memory. No eviction needed.

use std::sync::Arc;

use parking_lot::RwLock;

use crate::catalog::{Head, LongEntry, Snapshot, State};

use super::metrics::LayerMetrics;

/// Cached catalog data.
#[derive(Debug, Clone)]
pub struct CachedCatalog {
    /// The catalog head pointer.
    pub head: Head,
    /// ETag of the head file (for conditional refresh).
    pub head_etag: Option<String>,
    /// The base snapshot.
    pub snapshot: Snapshot,
    /// Log entries applied on top of the snapshot.
    pub log_entries: Vec<LongEntry>,
    /// Pre-computed state for fast access.
    pub state: State,
}

impl CachedCatalog {
    /// Create a new cached catalog.
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
        }
    }
}

/// Cache for the global catalog.
///
/// Holds the entire catalog in memory. The catalog is small and rarely
/// changes, so we keep it simple: load once, invalidate on changes.
pub struct CatalogCache {
    inner: RwLock<Option<CachedCatalog>>,
    metrics: Arc<LayerMetrics>,
}

impl CatalogCache {
    /// Create a new catalog cache.
    pub fn new(metrics: Arc<LayerMetrics>) -> Self {
        Self {
            inner: RwLock::new(None),
            metrics,
        }
    }

    /// Get the cached catalog.
    pub fn get(&self) -> Option<CachedCatalog> {
        let inner = self.inner.read();
        match inner.as_ref() {
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

    /// Get the computed state directly.
    pub fn get_state(&self) -> Option<State> {
        self.get().map(|c| c.state)
    }

    /// Get the cached ETag (for conditional refresh).
    pub fn get_etag(&self) -> Option<String> {
        self.inner.read().as_ref().and_then(|c| c.head_etag.clone())
    }

    /// Set the cached catalog.
    pub fn set(&self, catalog: CachedCatalog) {
        let size_bytes = Self::estimate_size(&catalog);
        let mut inner = self.inner.write();

        let is_new = inner.is_none();
        *inner = Some(catalog);

        if is_new {
            self.metrics.record_insert(size_bytes);
        }
    }

    /// Invalidate the cache, forcing a refresh on next access.
    pub fn invalidate(&self) {
        let mut inner = self.inner.write();
        if let Some(cached) = inner.take() {
            let size = Self::estimate_size(&cached);
            self.metrics.record_eviction(size);
        }
    }

    /// Check if the cache is populated.
    pub fn is_cached(&self) -> bool {
        self.inner.read().is_some()
    }

    /// Estimate memory size of the cached catalog.
    fn estimate_size(cached: &CachedCatalog) -> u64 {
        let namespaces_size = cached.snapshot.namespaces.len() * 150;
        let log_size = cached.log_entries.len() * 200;
        (100 + namespaces_size + log_size) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Namespace;

    fn make_catalog() -> CachedCatalog {
        let head = Head::new("snapshots/test.json");
        let snapshot = Snapshot::new(vec![Namespace::new("test_ns")]);
        CachedCatalog::new(head, Some("etag-456".to_string()), snapshot, vec![])
    }

    #[test]
    fn test_basic_operations() {
        let metrics = Arc::new(LayerMetrics::new());
        let cache = CatalogCache::new(Arc::clone(&metrics));

        cache.set(make_catalog());

        let cached = cache.get();
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().state.len(), 1);

        assert_eq!(metrics.snapshot().hits, 1);
        assert_eq!(metrics.snapshot().inserts, 1);
    }

    #[test]
    fn test_cache_miss() {
        let metrics = Arc::new(LayerMetrics::new());
        let cache = CatalogCache::new(Arc::clone(&metrics));

        let result = cache.get();
        assert!(result.is_none());
        assert_eq!(metrics.snapshot().misses, 1);
    }

    #[test]
    fn test_get_etag() {
        let metrics = Arc::new(LayerMetrics::new());
        let cache = CatalogCache::new(Arc::clone(&metrics));

        cache.set(make_catalog());

        let etag = cache.get_etag();
        assert_eq!(etag, Some("etag-456".to_string()));
    }

    #[test]
    fn test_invalidate() {
        let metrics = Arc::new(LayerMetrics::new());
        let cache = CatalogCache::new(Arc::clone(&metrics));

        cache.set(make_catalog());
        assert!(cache.is_cached());

        cache.invalidate();
        assert!(!cache.is_cached());
        assert!(cache.get().is_none());
    }

    #[test]
    fn test_get_state() {
        let metrics = Arc::new(LayerMetrics::new());
        let cache = CatalogCache::new(Arc::clone(&metrics));

        cache.set(make_catalog());

        let state = cache.get_state();
        assert!(state.is_some());
        assert!(state.unwrap().get_by_name("test_ns").is_some());
    }

    #[test]
    fn test_update_existing() {
        let metrics = Arc::new(LayerMetrics::new());
        let cache = CatalogCache::new(Arc::clone(&metrics));

        cache.set(make_catalog());
        cache.set(make_catalog()); // Update

        // Should only record one insert (first set)
        assert_eq!(metrics.snapshot().inserts, 1);
    }
}
