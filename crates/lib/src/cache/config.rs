//! Cache configuration.

use std::path::PathBuf;
use std::time::Duration;

/// Configuration for all cache layers.
///
/// Note: Catalog cache has no config - it's always enabled and holds
/// the entire catalog in memory (small and rarely changes).
#[derive(Debug, Clone, Default)]
pub struct CacheConfig {
    /// Segment cache (L2) configuration.
    pub segment: SegmentCacheConfig,
    /// Block cache (L1) configuration.
    pub block: BlockCacheConfig,
    /// Bloom filter cache configuration.
    pub bloom: BloomCacheConfig,
    /// Manifest cache configuration.
    pub manifest: ManifestCacheConfig,
}

impl CacheConfig {
    /// Create a minimal configuration for testing.
    pub fn minimal() -> Self {
        Self {
            segment: SegmentCacheConfig {
                enabled: true,
                path: PathBuf::from("/tmp/sybil-test-cache"),
                max_size_bytes: 10 * 1024 * 1024, // 10MB
                ..Default::default()
            },
            block: BlockCacheConfig {
                enabled: true,
                max_entries: 100,
                max_size_bytes: 10 * 1024 * 1024, // 10MB
            },
            bloom: BloomCacheConfig {
                enabled: true,
                max_segments: 50,
            },
            manifest: ManifestCacheConfig {
                enabled: true,
                max_namespaces: 10,
                validation_interval: Duration::from_secs(5),
            },
        }
    }
}

/// Segment cache (L2) configuration.
///
/// Stores compressed segments on disk/NVMe for fast retrieval.
#[derive(Debug, Clone)]
pub struct SegmentCacheConfig {
    /// Whether the segment cache is enabled.
    pub enabled: bool,
    /// Directory path for cached segments.
    pub path: PathBuf,
    /// Maximum cache size in bytes.
    pub max_size_bytes: u64,
    /// Time bias factor for LRU eviction (0.0-1.0).
    /// Higher values favor recently created segments.
    pub time_bias: f64,
}

impl Default for SegmentCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: PathBuf::from("/var/cache/sybil/segments"),
            max_size_bytes: 10 * 1024 * 1024 * 1024, // 10GB
            time_bias: 0.3,
        }
    }
}

/// Block cache (L1) configuration.
///
/// Stores decoded entries in memory for fast scanning.
#[derive(Debug, Clone)]
pub struct BlockCacheConfig {
    /// Whether the block cache is enabled.
    pub enabled: bool,
    /// Maximum number of blocks to cache.
    pub max_entries: usize,
    /// Maximum total size in bytes (approximate).
    pub max_size_bytes: u64,
}

impl Default for BlockCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_entries: 1000,
            max_size_bytes: 512 * 1024 * 1024, // 512MB
        }
    }
}

/// Bloom filter cache configuration.
#[derive(Debug, Clone)]
pub struct BloomCacheConfig {
    /// Whether the bloom cache is enabled.
    pub enabled: bool,
    /// Maximum number of segments to cache blooms for.
    pub max_segments: usize,
}

impl Default for BloomCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_segments: 10000,
        }
    }
}

/// Manifest cache configuration.
#[derive(Debug, Clone)]
pub struct ManifestCacheConfig {
    /// Whether the manifest cache is enabled.
    pub enabled: bool,
    /// Maximum number of namespaces to cache manifests for.
    pub max_namespaces: usize,
    /// How often to validate ETags against S3.
    pub validation_interval: Duration,
}

impl Default for ManifestCacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_namespaces: 1000,
            validation_interval: Duration::from_secs(30),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = CacheConfig::default();
        assert!(config.segment.enabled);
        assert!(config.block.enabled);
        assert!(config.bloom.enabled);
        assert!(config.manifest.enabled);
    }

    #[test]
    fn test_minimal_config() {
        let config = CacheConfig::minimal();
        assert!(config.segment.max_size_bytes < CacheConfig::default().segment.max_size_bytes);
        assert!(config.block.max_entries < CacheConfig::default().block.max_entries);
    }
}
