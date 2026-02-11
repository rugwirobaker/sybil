//! Segment cache (L2) - compressed segments on disk/NVMe.
//!
//! Stores compressed segments exactly as they appear in S3, avoiding
//! repeated downloads. Cache paths mirror S3 keys for simplicity.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;

use crate::segment::SegmentMeta;

use super::config::SegmentCacheConfig;
use super::error::{CacheError, Result};
use super::metrics::LayerMetrics;

/// A cached segment with access tracking.
#[derive(Debug, Clone)]
struct CachedSegment {
    /// Segment metadata.
    meta: SegmentMeta,
    /// Last access time (for LRU eviction).
    last_access: Instant,
}

/// Internal state for segment cache.
struct SegmentCacheInner {
    /// Map of segment key to cached segment info.
    index: HashMap<String, CachedSegment>,
    /// Current total size in bytes.
    current_size_bytes: u64,
}

/// Disk-based cache for compressed segments.
///
/// This is the L2 cache layer, storing compressed segment data on local
/// disk (ideally NVMe) to avoid repeated S3 fetches. Cache paths mirror
/// the S3 key structure.
pub struct SegmentCache {
    inner: RwLock<SegmentCacheInner>,
    cache_dir: PathBuf,
    max_size_bytes: u64,
    time_bias: f64,
    metrics: Arc<LayerMetrics>,
    enabled: bool,
}

impl SegmentCache {
    /// Create a new segment cache.
    pub async fn new(config: &SegmentCacheConfig, metrics: Arc<LayerMetrics>) -> Result<Self> {
        let cache = Self {
            inner: RwLock::new(SegmentCacheInner {
                index: HashMap::new(),
                current_size_bytes: 0,
            }),
            cache_dir: config.path.clone(),
            max_size_bytes: config.max_size_bytes,
            time_bias: config.time_bias,
            metrics,
            enabled: config.enabled,
        };

        if config.enabled {
            cache.init().await?;
        }

        Ok(cache)
    }

    /// Initialize the cache directory.
    async fn init(&self) -> Result<()> {
        fs::create_dir_all(&self.cache_dir)
            .await
            .map_err(|e| CacheError::CreateDir {
                path: self.cache_dir.clone(),
                source: e,
            })?;
        Ok(())
    }

    /// Get the local path for a segment key.
    fn path_for(&self, key: &str) -> PathBuf {
        self.cache_dir.join(key)
    }

    /// Get a cached segment.
    pub async fn get(&self, key: &str) -> Option<Bytes> {
        if !self.enabled {
            return None;
        }

        // Check index and update access time
        {
            let mut inner = self.inner.write().await;
            if let Some(cached) = inner.index.get_mut(key) {
                cached.last_access = Instant::now();
            } else {
                self.metrics.record_miss();
                return None;
            }
        }

        // Read from disk
        let path = self.path_for(key);
        match fs::read(&path).await {
            Ok(data) => {
                self.metrics.record_hit();
                Some(Bytes::from(data))
            }
            Err(_) => {
                // File disappeared, remove from index
                let mut inner = self.inner.write().await;
                if let Some(cached) = inner.index.remove(key) {
                    inner.current_size_bytes = inner
                        .current_size_bytes
                        .saturating_sub(cached.meta.size_bytes);
                }
                self.metrics.record_miss();
                None
            }
        }
    }

    /// Get segment metadata without reading the data.
    pub async fn get_meta(&self, key: &str) -> Option<SegmentMeta> {
        if !self.enabled {
            return None;
        }

        let inner = self.inner.read().await;
        inner.index.get(key).map(|c| c.meta.clone())
    }

    /// Insert a segment into the cache.
    pub async fn insert(&self, meta: SegmentMeta, data: Bytes) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let size_bytes = data.len() as u64;

        // Evict if necessary
        self.evict_to_fit(size_bytes).await?;

        // Ensure parent directories exist
        let path = self.path_for(&meta.key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        // Write segment data
        let mut file = fs::File::create(&path).await?;
        file.write_all(&data).await?;
        file.flush().await?;

        // Update index
        let mut inner = self.inner.write().await;

        // Remove old entry if exists
        if let Some(old) = inner.index.remove(&meta.key) {
            inner.current_size_bytes = inner.current_size_bytes.saturating_sub(old.meta.size_bytes);
            self.metrics.record_eviction(old.meta.size_bytes);
        }

        let key = meta.key.clone();
        inner.index.insert(
            key,
            CachedSegment {
                meta: SegmentMeta { size_bytes, ..meta },
                last_access: Instant::now(),
            },
        );
        inner.current_size_bytes += size_bytes;
        self.metrics.record_insert(size_bytes);

        Ok(())
    }

    /// Check if a segment is cached.
    pub async fn contains(&self, key: &str) -> bool {
        if !self.enabled {
            return false;
        }
        self.inner.read().await.index.contains_key(key)
    }

    /// Remove a segment from the cache.
    pub async fn remove(&self, key: &str) -> Result<()> {
        let mut inner = self.inner.write().await;

        if let Some(cached) = inner.index.remove(key) {
            inner.current_size_bytes = inner
                .current_size_bytes
                .saturating_sub(cached.meta.size_bytes);

            // Remove file
            let path = self.path_for(key);
            let _ = fs::remove_file(&path).await;

            self.metrics.record_eviction(cached.meta.size_bytes);
        }

        Ok(())
    }

    /// Evict segments until there's room for `needed_bytes`.
    async fn evict_to_fit(&self, needed_bytes: u64) -> Result<()> {
        loop {
            let should_evict = {
                let inner = self.inner.read().await;
                inner.current_size_bytes + needed_bytes > self.max_size_bytes
                    && !inner.index.is_empty()
            };

            if !should_evict {
                break;
            }

            self.evict_one().await?;
        }

        Ok(())
    }

    /// Evict the segment with the lowest score.
    async fn evict_one(&self) -> Result<()> {
        let now = Instant::now();
        let current_ts = chrono::Utc::now().timestamp_millis();

        let evict_key = {
            let inner = self.inner.read().await;
            inner
                .index
                .iter()
                .map(|(k, cached)| {
                    let score = self.compute_score(cached, now, current_ts);
                    (k.clone(), score)
                })
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(k, _)| k)
        };

        if let Some(key) = evict_key {
            self.remove(&key).await?;
        }

        Ok(())
    }

    /// Compute eviction score.
    ///
    /// Higher score = less likely to be evicted.
    fn compute_score(&self, cached: &CachedSegment, now: Instant, current_ts: i64) -> f64 {
        let access_age_secs = now.duration_since(cached.last_access).as_secs_f64();
        let access_recency = 1.0 / (1.0 + access_age_secs / 60.0);

        let age_hours = (current_ts - cached.meta.min_timestamp) as f64 / 3_600_000.0;
        let timestamp_recency = 1.0 / (1.0 + age_hours / 24.0);

        access_recency * (1.0 - self.time_bias) + timestamp_recency * self.time_bias
    }

    /// Get current size in bytes.
    pub async fn size_bytes(&self) -> u64 {
        self.inner.read().await.current_size_bytes
    }

    /// Get current entry count.
    pub async fn len(&self) -> usize {
        self.inner.read().await.index.len()
    }

    /// Check if cache is empty.
    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.index.is_empty()
    }

    /// Clear all cached segments.
    pub async fn clear(&self) -> Result<()> {
        let mut inner = self.inner.write().await;

        for (key, _) in inner.index.drain() {
            let path = self.path_for(&key);
            let _ = fs::remove_file(&path).await;
        }

        inner.current_size_bytes = 0;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_meta(key: &str, size: u64) -> SegmentMeta {
        SegmentMeta {
            key: key.to_string(),
            size_bytes: size,
            min_timestamp: chrono::Utc::now().timestamp_millis(),
            max_timestamp: chrono::Utc::now().timestamp_millis() + 1000,
            entry_count: 100,
        }
    }

    async fn test_cache(max_size: u64) -> (SegmentCache, TempDir) {
        let tmp = TempDir::new().unwrap();
        let config = SegmentCacheConfig {
            enabled: true,
            path: tmp.path().to_path_buf(),
            max_size_bytes: max_size,
            time_bias: 0.0,
        };
        let metrics = Arc::new(LayerMetrics::new());
        let cache = SegmentCache::new(&config, metrics).await.unwrap();
        (cache, tmp)
    }

    #[tokio::test]
    async fn test_basic_operations() {
        let (cache, _tmp) = test_cache(10000).await;

        let meta = test_meta("ns/segments/001.seg", 100);
        let data = Bytes::from(vec![1u8; 100]);
        cache.insert(meta, data.clone()).await.unwrap();

        assert!(cache.contains("ns/segments/001.seg").await);

        let retrieved = cache.get("ns/segments/001.seg").await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap(), data);
    }

    #[tokio::test]
    async fn test_cache_miss() {
        let (cache, _tmp) = test_cache(10000).await;

        let result = cache.get("nonexistent").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_eviction() {
        let (cache, _tmp) = test_cache(200).await;

        // Insert 3 segments of 100 bytes each, but max is 200
        cache
            .insert(test_meta("seg-001", 100), Bytes::from(vec![1u8; 100]))
            .await
            .unwrap();
        cache
            .insert(test_meta("seg-002", 100), Bytes::from(vec![2u8; 100]))
            .await
            .unwrap();
        cache
            .insert(test_meta("seg-003", 100), Bytes::from(vec![3u8; 100]))
            .await
            .unwrap();

        // Should have evicted one
        assert_eq!(cache.len().await, 2);
        assert!(cache.size_bytes().await <= 200);
    }

    #[tokio::test]
    async fn test_remove() {
        let (cache, _tmp) = test_cache(10000).await;

        cache
            .insert(test_meta("segment-001", 100), Bytes::from(vec![1u8; 100]))
            .await
            .unwrap();
        assert!(cache.contains("segment-001").await);

        cache.remove("segment-001").await.unwrap();
        assert!(!cache.contains("segment-001").await);
        assert_eq!(cache.len().await, 0);
    }

    #[tokio::test]
    async fn test_nested_paths() {
        let (cache, _tmp) = test_cache(10000).await;

        // Test that nested S3 key paths work
        let meta = test_meta("namespaces/prod/segments/2024/01/segment-001.seg", 100);
        let data = Bytes::from(vec![1u8; 100]);
        cache.insert(meta, data.clone()).await.unwrap();

        let retrieved = cache
            .get("namespaces/prod/segments/2024/01/segment-001.seg")
            .await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap(), data);
    }

    #[tokio::test]
    async fn test_clear() {
        let (cache, _tmp) = test_cache(10000).await;

        cache
            .insert(test_meta("seg-001", 100), Bytes::from(vec![1u8; 100]))
            .await
            .unwrap();
        cache
            .insert(test_meta("seg-002", 100), Bytes::from(vec![2u8; 100]))
            .await
            .unwrap();

        cache.clear().await.unwrap();

        assert!(cache.is_empty().await);
        assert_eq!(cache.size_bytes().await, 0);
    }

    #[tokio::test]
    async fn test_get_meta() {
        let (cache, _tmp) = test_cache(10000).await;

        let meta = test_meta("segment-001", 100);
        cache
            .insert(meta.clone(), Bytes::from(vec![1u8; 100]))
            .await
            .unwrap();

        let retrieved_meta = cache.get_meta("segment-001").await;
        assert!(retrieved_meta.is_some());
        let retrieved_meta = retrieved_meta.unwrap();
        assert_eq!(retrieved_meta.key, "segment-001");
        assert_eq!(retrieved_meta.entry_count, 100);
    }

    #[tokio::test]
    async fn test_disabled_cache() {
        let tmp = TempDir::new().unwrap();
        let config = SegmentCacheConfig {
            enabled: false,
            path: tmp.path().to_path_buf(),
            max_size_bytes: 10000,
            time_bias: 0.0,
        };
        let metrics = Arc::new(LayerMetrics::new());
        let cache = SegmentCache::new(&config, metrics).await.unwrap();

        cache
            .insert(test_meta("segment-001", 100), Bytes::from(vec![1u8; 100]))
            .await
            .unwrap();

        assert!(!cache.contains("segment-001").await);
        assert!(cache.get("segment-001").await.is_none());
    }
}
