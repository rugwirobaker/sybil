//! Block cache (L1) - decoded entries in memory.
//!
//! Stores decoded log entries ready for scanning. This is the fastest cache
//! layer, sitting between queries and the segment cache.

use std::sync::Arc;

use crate::segment::ProcessedEntry;

use super::config::BlockCacheConfig;
use super::lru::{EntryMeta, TimeBiasedLru};
use super::metrics::LayerMetrics;

/// Key for block cache entries.
///
/// For L0 segments, `block_index` is `None`.
/// For L1/L2 segments with multiple blocks, `block_index` is `Some(n)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlockKey {
    /// The segment's S3 key.
    pub segment_key: String,
    /// Block index within the segment (None for L0 single-block segments).
    pub block_index: Option<u32>,
}

impl BlockKey {
    /// Create a key for an L0 segment (single block).
    pub fn l0(segment_key: impl Into<String>) -> Self {
        Self {
            segment_key: segment_key.into(),
            block_index: None,
        }
    }

    /// Create a key for a specific block within a segment.
    pub fn block(segment_key: impl Into<String>, index: u32) -> Self {
        Self {
            segment_key: segment_key.into(),
            block_index: Some(index),
        }
    }
}

/// Cached block of decoded entries.
pub type CachedBlock = Arc<Vec<ProcessedEntry>>;

// TODO: We should defined A Block type in the segment module to avoid `Vec<ProcessedEntry>`

/// Block cache for decoded log entries.
///
/// This is the L1 cache layer, storing entries that have been decoded from
/// compressed segments and are ready for scanning.
pub struct BlockCache {
    cache: TimeBiasedLru<BlockKey, CachedBlock>,
    metrics: Arc<LayerMetrics>,
    enabled: bool,
}

impl BlockCache {
    /// Create a new block cache.
    pub fn new(config: &BlockCacheConfig, metrics: Arc<LayerMetrics>) -> Self {
        Self {
            cache: TimeBiasedLru::new(config.max_size_bytes, 0.0),
            metrics,
            enabled: config.enabled,
        }
    }

    /// Get a cached block, returning None on miss.
    pub fn get(&self, key: &BlockKey) -> Option<CachedBlock> {
        if !self.enabled {
            return None;
        }

        match self.cache.get(key) {
            Some(block) => {
                self.metrics.record_hit();
                Some(block)
            }
            None => {
                self.metrics.record_miss();
                None
            }
        }
    }

    /// Insert a block into the cache.
    pub fn insert(&self, key: BlockKey, entries: Vec<ProcessedEntry>, min_timestamp: i64) {
        if !self.enabled {
            return;
        }

        let block = Arc::new(entries);
        let size_bytes = Self::estimate_block_size(&block);
        let meta = EntryMeta::new(size_bytes, min_timestamp);

        let evicted = self.cache.insert(key, block, meta);

        self.metrics.record_insert(size_bytes);
        for (_, evicted_block) in evicted {
            let evicted_size = Self::estimate_block_size(&evicted_block);
            self.metrics.record_eviction(evicted_size);
        }
    }

    /// Insert a pre-wrapped Arc block.
    pub fn insert_arc(&self, key: BlockKey, block: CachedBlock, min_timestamp: i64) {
        if !self.enabled {
            return;
        }

        let size_bytes = Self::estimate_block_size(&block);
        let meta = EntryMeta::new(size_bytes, min_timestamp);

        let evicted = self.cache.insert(key, block, meta);

        self.metrics.record_insert(size_bytes);
        for (_, evicted_block) in evicted {
            let evicted_size = Self::estimate_block_size(&evicted_block);
            self.metrics.record_eviction(evicted_size);
        }
    }

    /// Invalidate all blocks for a segment.
    ///
    /// Call this when a segment is superseded or deleted.
    pub fn invalidate_segment(&self, segment_key: &str) {
        let keys: Vec<_> = self
            .cache
            .keys()
            .into_iter()
            .filter(|k| k.segment_key == segment_key)
            .collect();

        for key in keys {
            if let Some(block) = self.cache.remove(&key) {
                let size = Self::estimate_block_size(&block);
                self.metrics.record_eviction(size);
            }
        }
    }

    /// Estimate the memory size of a block in bytes.
    fn estimate_block_size(block: &[ProcessedEntry]) -> u64 {
        block
            .iter()
            .map(|e| {
                // Base size + dict_ids vec + payload vec
                32 + (e.dict_ids.len() * 4) + e.payload.len()
            })
            .sum::<usize>() as u64
    }

    /// Get current entry count.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Check if cache is empty.
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Get current size in bytes.
    pub fn size_bytes(&self) -> u64 {
        self.cache.size_bytes()
    }

    /// Get maximum size in bytes.
    pub fn max_size_bytes(&self) -> u64 {
        self.cache.max_size_bytes()
    }

    /// Check if a block is cached without fetching it.
    pub fn contains(&self, key: &BlockKey) -> bool {
        if !self.enabled {
            return false;
        }
        self.cache.contains(key)
    }

    /// Peek at a cached block without updating its access time.
    ///
    /// Useful for diagnostics or when you don't want to affect eviction order.
    pub fn peek(&self, key: &BlockKey) -> Option<CachedBlock> {
        if !self.enabled {
            return None;
        }
        self.cache.peek(key)
    }

    /// Clear all cached blocks.
    pub fn clear(&self) {
        self.cache.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::PresenceBitmap;

    fn make_entry(ts: i64, payload: &[u8]) -> ProcessedEntry {
        ProcessedEntry {
            timestamp: ts,
            presence: PresenceBitmap::new(),
            dict_ids: vec![],
            payload: payload.to_vec(),
        }
    }

    fn test_config() -> BlockCacheConfig {
        BlockCacheConfig {
            enabled: true,
            max_entries: 100,
            max_size_bytes: 10 * 1024, // 10KB
        }
    }

    #[test]
    fn test_basic_operations() {
        let metrics = Arc::new(LayerMetrics::new());
        let cache = BlockCache::new(&test_config(), Arc::clone(&metrics));

        let key = BlockKey::l0("segment-001");
        let entries = vec![make_entry(1000, b"test payload")];

        cache.insert(key.clone(), entries, 1000);

        let retrieved = cache.get(&key);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().len(), 1);

        assert_eq!(metrics.snapshot().hits, 1);
        assert_eq!(metrics.snapshot().inserts, 1);
    }

    #[test]
    fn test_cache_miss() {
        let metrics = Arc::new(LayerMetrics::new());
        let cache = BlockCache::new(&test_config(), Arc::clone(&metrics));

        let result = cache.get(&BlockKey::l0("nonexistent"));
        assert!(result.is_none());
        assert_eq!(metrics.snapshot().misses, 1);
    }

    #[test]
    fn test_invalidate_segment() {
        let metrics = Arc::new(LayerMetrics::new());
        let cache = BlockCache::new(&test_config(), Arc::clone(&metrics));

        // Insert multiple blocks for same segment
        cache.insert(
            BlockKey::block("segment-001", 0),
            vec![make_entry(1000, b"block 0")],
            1000,
        );
        cache.insert(
            BlockKey::block("segment-001", 1),
            vec![make_entry(2000, b"block 1")],
            2000,
        );
        cache.insert(
            BlockKey::l0("segment-002"),
            vec![make_entry(3000, b"other")],
            3000,
        );

        assert_eq!(cache.len(), 3);

        // Invalidate segment-001
        cache.invalidate_segment("segment-001");

        assert_eq!(cache.len(), 1);
        assert!(cache.get(&BlockKey::l0("segment-002")).is_some());
    }

    #[test]
    fn test_disabled_cache() {
        let config = BlockCacheConfig {
            enabled: false,
            ..test_config()
        };
        let metrics = Arc::new(LayerMetrics::new());
        let cache = BlockCache::new(&config, Arc::clone(&metrics));

        let key = BlockKey::l0("segment-001");
        cache.insert(key.clone(), vec![make_entry(1000, b"test")], 1000);

        // Should not be cached
        assert!(cache.get(&key).is_none());
        assert_eq!(metrics.snapshot().hits, 0);
        assert_eq!(metrics.snapshot().misses, 0);
    }

    #[test]
    fn test_block_key_variants() {
        let l0_key = BlockKey::l0("seg-001");
        assert_eq!(l0_key.segment_key, "seg-001");
        assert_eq!(l0_key.block_index, None);

        let block_key = BlockKey::block("seg-001", 5);
        assert_eq!(block_key.segment_key, "seg-001");
        assert_eq!(block_key.block_index, Some(5));

        assert_ne!(l0_key, block_key);
    }
}
