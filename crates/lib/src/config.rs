use serde::Deserialize;
use std::path::Path;

/// Top-level configuration for Sybil.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    /// Storage configuration (S3/Tigris).
    #[serde(default)]
    pub storage: StorageConfig,

    /// HTTP server configuration.
    #[serde(default)]
    pub server: ServerConfig,

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

#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    /// S3 endpoint URL.
    #[serde(default = "default_endpoint")]
    pub endpoint: String,

    /// S3 bucket name.
    #[serde(default = "default_bucket")]
    pub bucket: String,

    /// AWS region.
    #[serde(default = "default_region")]
    pub region: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    /// Host to bind to.
    #[serde(default = "default_host")]
    pub host: String,

    /// Port to listen on.
    #[serde(default = "default_port")]
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IndexerConfig {
    /// Maximum time before flushing buffer to S3 (seconds).
    #[serde(default = "default_flush_interval")]
    pub flush_interval_secs: u64,

    /// Target L0 segment size threshold (bytes).
    ///
    /// Set to 48KB to ensure final segments stay ≤64KB after adding dictionary,
    /// bloom filters, and footer. Tigris inlines objects ≤64KB for lower latency.
    #[serde(default = "default_segment_size")]
    pub segment_size_bytes: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuerierConfig {
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

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    8080
}

fn default_flush_interval() -> u64 {
    10
}

fn default_segment_size() -> usize {
    // 48KB threshold leaves ~16KB headroom for dictionary, bloom filters, and footer.
    // This ensures final L0 segments stay ≤64KB to benefit from Tigris object inlining.
    48 * 1024
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

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            endpoint: default_endpoint(),
            bucket: default_bucket(),
            region: default_region(),
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
        }
    }
}

impl Default for IndexerConfig {
    fn default() -> Self {
        Self {
            flush_interval_secs: default_flush_interval(),
            segment_size_bytes: default_segment_size(),
        }
    }
}

impl Default for QuerierConfig {
    fn default() -> Self {
        Self {
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
