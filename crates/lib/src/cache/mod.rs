//! Caching layer for low-latency queries.
//!
//! This module provides a layered cache system:
//!
//! - **L2 (Disk/NVMe)**: Segment cache - compressed segments as stored in S3
//! - **L1 (Memory)**: Block cache - decoded entries ready for scanning
//! - **Memory**: Bloom cache, Manifest cache, Catalog cache
//!
//! # Architecture
//!
//! ```text
//! Query
//!   │
//!   ├─► Catalog Cache (namespace lookup)
//!   │
//!   ├─► Manifest Cache (segment list for namespace)
//!   │
//!   ├─► Bloom Cache (filter segments by field values)
//!   │
//!   ├─► Block Cache (L1 - decoded entries)
//!   │     │
//!   │     └─► Segment Cache (L2 - compressed on disk)
//!   │           │
//!   │           └─► S3 (cold storage)
//!   │
//!   └─► Results
//! ```

mod block;
mod bloom;
mod catalog;
mod config;
mod error;
mod lru;
mod manifest;
mod metrics;
mod segment;

pub use block::{BlockCache, BlockKey, CachedBlock};
pub use bloom::BloomCache;
pub use catalog::{CachedCatalog, CatalogCache};
pub use config::{
    BlockCacheConfig, BloomCacheConfig, CacheConfig, ManifestCacheConfig, SegmentCacheConfig,
};
pub use error::{CacheError, Result};
pub use manifest::{CachedManifest, ManifestCache};
pub use metrics::{CacheMetrics, CacheMetricsSnapshot, LayerMetrics, LayerMetricsSnapshot};
pub use segment::SegmentCache;

use std::sync::Arc;

/// Central cache manager providing access to all cache layers.
pub struct CacheManager {
    /// Segment cache (L2) - compressed segments on disk.
    pub segment: Arc<SegmentCache>,
    /// Block cache (L1) - decoded entries in memory.
    pub block: Arc<BlockCache>,
    /// Bloom filter cache.
    pub bloom: Arc<BloomCache>,
    /// Per-namespace manifest cache.
    pub manifest: Arc<ManifestCache>,
    /// Global catalog cache.
    pub catalog: Arc<CatalogCache>,
    /// Aggregated metrics.
    pub metrics: Arc<CacheMetrics>,
}

impl CacheManager {
    /// Create a new cache manager with the given configuration.
    pub async fn new(config: &CacheConfig) -> Result<Self> {
        let metrics = Arc::new(CacheMetrics::new());

        let segment =
            Arc::new(SegmentCache::new(&config.segment, Arc::clone(&metrics.segment)).await?);

        let block = Arc::new(BlockCache::new(&config.block, Arc::clone(&metrics.block)));

        let bloom = Arc::new(BloomCache::new(&config.bloom, Arc::clone(&metrics.bloom)));

        let manifest = Arc::new(ManifestCache::new(
            &config.manifest,
            Arc::clone(&metrics.manifest),
        ));

        let catalog = Arc::new(CatalogCache::new(Arc::clone(&metrics.catalog)));

        Ok(Self {
            segment,
            block,
            bloom,
            manifest,
            catalog,
            metrics,
        })
    }

    /// Invalidate all caches for a segment.
    ///
    /// Call this when a segment is superseded or deleted.
    pub async fn invalidate_segment(&self, segment_key: &str) {
        self.block.invalidate_segment(segment_key);
        self.bloom.invalidate_segment(segment_key);
        let _ = self.segment.remove(segment_key).await;
    }

    /// Invalidate all caches for a namespace.
    ///
    /// Call this when a namespace's manifest changes significantly.
    pub fn invalidate_namespace(&self, namespace_id: &str) {
        self.manifest.invalidate(namespace_id);
    }

    /// Invalidate the catalog cache.
    ///
    /// Call this when namespaces are created or deleted.
    pub fn invalidate_catalog(&self) {
        self.catalog.invalidate();
    }

    /// Get a metrics snapshot.
    pub fn metrics_snapshot(&self) -> CacheMetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Clear all caches.
    pub async fn clear(&self) -> Result<()> {
        self.block.clear();
        self.bloom.clear();
        self.manifest.clear();
        self.catalog.invalidate();
        self.segment.clear().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn test_manager() -> (CacheManager, TempDir) {
        let tmp = TempDir::new().unwrap();
        let mut config = CacheConfig::minimal();
        config.segment.path = tmp.path().to_path_buf();

        let manager = CacheManager::new(&config).await.unwrap();
        (manager, tmp)
    }

    #[tokio::test]
    async fn test_cache_manager_creation() {
        let (manager, _tmp) = test_manager().await;

        assert!(manager.segment.is_empty().await);
        assert!(manager.block.is_empty());
        assert_eq!(manager.bloom.segment_count(), 0);
        assert!(manager.manifest.is_empty());
        assert!(!manager.catalog.is_cached());
    }

    #[tokio::test]
    async fn test_cache_manager_metrics() {
        let (manager, _tmp) = test_manager().await;

        let snapshot = manager.metrics_snapshot();
        assert_eq!(snapshot.total_entry_count(), 0);
    }

    #[tokio::test]
    async fn test_invalidate_segment() {
        let (manager, _tmp) = test_manager().await;

        // Insert some data
        use crate::segment::{Block, BloomFilter, PresenceBitmap, ProcessedEntry};

        let entry = ProcessedEntry {
            timestamp: 1000,
            presence: PresenceBitmap::new(),
            dict_ids: vec![],
            payload: b"test".to_vec(),
        };

        manager.block.insert(
            BlockKey::l0("seg-001"),
            Block::from_entries(vec![entry]),
            1000,
        );

        let mut bloom = BloomFilter::new(10);
        bloom.insert(b"test");
        manager.bloom.insert("seg-001", 0, bloom);

        assert_eq!(manager.block.len(), 1);
        assert_eq!(manager.bloom.segment_count(), 1);

        // Invalidate
        manager.invalidate_segment("seg-001").await;

        assert_eq!(manager.block.len(), 0);
        assert_eq!(manager.bloom.segment_count(), 0);
    }
}
