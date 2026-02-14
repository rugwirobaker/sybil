use serde::Deserialize;
use std::path::Path;

/// Top-level configuration for Sybil.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    /// Enable debug mode.
    #[serde(default)]
    pub debug: bool,

    /// Storage configuration (S3/memory).
    #[serde(default)]
    pub storage: StorageConfig,

    /// Indexer-specific configuration.
    #[serde(default)]
    pub indexer: IndexerConfig,

    /// Querier-specific configuration.
    #[serde(default)]
    pub querier: QuerierConfig,

    /// Steward-specific configuration.
    #[serde(default)]
    pub steward: StewardConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct StorageConfig {
    /// Storage backend: "memory" or "s3".
    #[serde(default)]
    pub backend: StorageBackend,

    /// S3-compatible storage configuration.
    #[serde(default)]
    pub s3: S3Config,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StorageBackend {
    #[default]
    Memory,
    S3,
}

#[derive(Debug, Clone, Deserialize)]
pub struct S3Config {
    /// S3 endpoint URL.
    #[serde(default = "default_endpoint")]
    pub endpoint: String,

    /// Access key ID.
    #[serde(default)]
    pub access_key_id: String,

    /// Secret access key.
    #[serde(default)]
    pub secret_access_key: String,

    /// S3 bucket name.
    #[serde(default = "default_bucket")]
    pub bucket: String,

    /// AWS region.
    #[serde(default = "default_region")]
    pub region: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IndexerConfig {
    /// Address to bind to (e.g. "0.0.0.0:9080").
    #[serde(default = "default_indexer_bind")]
    pub bind: String,

    /// Buffer configuration.
    #[serde(default)]
    pub buffer: BufferConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BufferConfig {
    /// Maximum time before flushing buffer to S3 (seconds).
    #[serde(default = "default_flush_interval")]
    pub flush_interval_secs: u64,

    /// Target L0 segment size threshold (bytes).
    ///
    /// Set to 48KB to ensure final segments stay ≤64KB after adding dictionary,
    /// bloom filters, and footer. Tigris inlines objects ≤64KB for lower latency.
    #[serde(default = "default_segment_size")]
    pub segment_size_bytes: usize,

    /// Maximum size of a single entry in bytes.
    #[serde(default = "default_max_entry_size")]
    pub max_entry_size: usize,

    /// Maximum number of entries before forcing a flush.
    #[serde(default = "default_max_entries")]
    pub max_entries: usize,
}

impl BufferConfig {
    /// Create a new BufferConfig with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the segment size threshold in bytes.
    pub fn with_segment_size_bytes(mut self, size: usize) -> Self {
        self.segment_size_bytes = size;
        self
    }

    /// Set the flush interval in seconds.
    pub fn with_flush_interval_secs(mut self, secs: u64) -> Self {
        self.flush_interval_secs = secs;
        self
    }

    /// Set the maximum entry size in bytes.
    pub fn with_max_entry_size(mut self, size: usize) -> Self {
        self.max_entry_size = size;
        self
    }

    /// Set the maximum entries before flush.
    pub fn with_max_entries(mut self, count: usize) -> Self {
        self.max_entries = count;
        self
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuerierConfig {
    /// Address to bind to (e.g. "0.0.0.0:8080").
    #[serde(default = "default_querier_bind")]
    pub bind: String,

    /// NVMe cache directory path.
    #[serde(default = "default_cache_dir")]
    pub cache_dir: String,

    /// Maximum cache size (bytes).
    #[serde(default = "default_cache_size")]
    pub cache_size_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StewardConfig {
    /// L0 segment count threshold for compaction.
    #[serde(default = "default_compaction_threshold")]
    pub compaction_threshold: usize,

    /// Number of concurrent worker tasks.
    #[serde(default = "default_worker_count")]
    pub worker_count: usize,
}

// Default value functions

fn default_endpoint() -> String {
    "https://fly.storage.tigris.dev".to_string()
}

fn default_bucket() -> String {
    "sybil".to_string()
}

fn default_region() -> String {
    "auto".to_string()
}

fn default_indexer_bind() -> String {
    "0.0.0.0:9080".to_string()
}

fn default_querier_bind() -> String {
    "0.0.0.0:8080".to_string()
}

fn default_flush_interval() -> u64 {
    10
}

fn default_segment_size() -> usize {
    // 48KB threshold leaves ~16KB headroom for dictionary, bloom filters, and footer.
    // This ensures final L0 segments stay ≤64KB to benefit from Tigris object inlining.
    48 * 1024
}

fn default_max_entry_size() -> usize {
    64 * 1024
}

fn default_max_entries() -> usize {
    10_000
}

fn default_cache_dir() -> String {
    "/var/cache/sybil".to_string()
}

fn default_cache_size() -> u64 {
    10 * 1024 * 1024 * 1024 // 10GB
}

fn default_compaction_threshold() -> usize {
    10
}

fn default_worker_count() -> usize {
    10
}

// Default trait implementations

impl Default for S3Config {
    fn default() -> Self {
        Self {
            endpoint: default_endpoint(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            bucket: default_bucket(),
            region: default_region(),
        }
    }
}

impl Default for IndexerConfig {
    fn default() -> Self {
        Self {
            bind: default_indexer_bind(),
            buffer: BufferConfig::default(),
        }
    }
}

impl Default for BufferConfig {
    fn default() -> Self {
        Self {
            flush_interval_secs: default_flush_interval(),
            segment_size_bytes: default_segment_size(),
            max_entry_size: default_max_entry_size(),
            max_entries: default_max_entries(),
        }
    }
}

impl Default for QuerierConfig {
    fn default() -> Self {
        Self {
            bind: default_querier_bind(),
            cache_dir: default_cache_dir(),
            cache_size_bytes: default_cache_size(),
        }
    }
}

impl Default for StewardConfig {
    fn default() -> Self {
        Self {
            compaction_threshold: default_compaction_threshold(),
            worker_count: default_worker_count(),
        }
    }
}

impl Config {
    /// Load configuration from a TOML file.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path.as_ref())
            .map_err(|e| ConfigError::ReadError(e.to_string()))?;

        toml::from_str(&content).map_err(|e| ConfigError::ParseError(e.to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file: {0}")]
    ReadError(String),

    #[error("failed to parse config: {0}")]
    ParseError(String),
}
