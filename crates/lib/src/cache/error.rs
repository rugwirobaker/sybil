//! Cache error types.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

/// Errors that can occur in cache operations.
#[derive(Debug, Error)]
pub enum CacheError {
    /// I/O error during cache operations.
    #[error("cache I/O error: {0}")]
    Io(#[from] io::Error),

    /// Failed to create cache directory.
    #[error("failed to create cache directory {path}: {source}")]
    CreateDir { path: PathBuf, source: io::Error },

    /// Cache entry not found.
    #[error("cache miss: {key}")]
    NotFound { key: String },

    /// Cache entry corrupted or invalid.
    #[error("cache entry corrupted: {key}")]
    Corrupted { key: String },

    /// Cache capacity exceeded.
    #[error("cache capacity exceeded: {current} > {max}")]
    CapacityExceeded { current: u64, max: u64 },

    /// Segment decoding error.
    #[error("segment decode error: {0}")]
    SegmentDecode(String),

    /// ETag mismatch during validation.
    #[error("etag mismatch: expected {expected}, got {actual}")]
    EtagMismatch { expected: String, actual: String },

    /// Serialization error.
    #[error("serialization error: {0}")]
    Serialization(String),
}

impl CacheError {
    /// Check if this is a cache miss (not found) error.
    pub fn is_not_found(&self) -> bool {
        matches!(self, CacheError::NotFound { .. })
    }
}

/// Result type for cache operations.
pub type Result<T> = std::result::Result<T, CacheError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_error_not_found() {
        let err = CacheError::NotFound {
            key: "test_key".to_string(),
        };
        assert!(err.is_not_found());
        assert!(err.to_string().contains("test_key"));
    }

    #[test]
    fn test_cache_error_io() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "file not found");
        let err: CacheError = io_err.into();
        assert!(!err.is_not_found());
    }
}
