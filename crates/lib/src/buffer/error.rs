//! Buffer error types.

use std::io;

use thiserror::Error;

/// Errors that can occur in buffer operations.
#[derive(Debug, Error)]
pub enum BufferError {
    /// I/O error during buffer operations.
    #[error("buffer I/O error: {0}")]
    Io(#[from] io::Error),

    /// Entry exceeds maximum allowed size.
    #[error("entry too large: {size} bytes exceeds max {max} bytes")]
    EntryTooLarge { size: usize, max: usize },

    /// Invalid JSON payload.
    #[error("invalid JSON payload: {0}")]
    InvalidJson(#[from] serde_json::Error),

    /// Namespace not found.
    #[error("namespace not found: {namespace_id}")]
    NamespaceNotFound { namespace_id: String },

    /// Buffer is empty (cannot drain).
    #[error("buffer is empty for namespace: {namespace_id}")]
    EmptyBuffer { namespace_id: String },

    /// Segment build failed.
    #[error("segment build failed: {0}")]
    SegmentBuild(String),
}

impl BufferError {
    /// Check if this is an entry too large error.
    pub fn is_entry_too_large(&self) -> bool {
        matches!(self, BufferError::EntryTooLarge { .. })
    }

    /// Check if this is a namespace not found error.
    pub fn is_namespace_not_found(&self) -> bool {
        matches!(self, BufferError::NamespaceNotFound { .. })
    }
}

/// Result type for buffer operations.
pub type Result<T> = std::result::Result<T, BufferError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_error_entry_too_large() {
        let err = BufferError::EntryTooLarge {
            size: 100_000,
            max: 65_536,
        };
        assert!(err.is_entry_too_large());
        assert!(err.to_string().contains("100000"));
        assert!(err.to_string().contains("65536"));
    }

    #[test]
    fn test_buffer_error_namespace_not_found() {
        let err = BufferError::NamespaceNotFound {
            namespace_id: "test-ns".to_string(),
        };
        assert!(err.is_namespace_not_found());
        assert!(err.to_string().contains("test-ns"));
    }

    #[test]
    fn test_buffer_error_io() {
        let io_err = io::Error::new(io::ErrorKind::Other, "disk full");
        let err: BufferError = io_err.into();
        assert!(!err.is_entry_too_large());
    }
}
