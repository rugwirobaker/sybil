/// Errors that can occur during storage operations.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// Object not found at the given key.
    #[error("not found: {key}")]
    NotFound { key: String },

    /// Conflict - object was modified concurrently.
    #[error("conflict: {key} was modified")]
    Conflict { key: String },

    /// Invalid byte range for the object.
    #[error("invalid range for {key}: {reason}")]
    InvalidRange { key: String, reason: String },

    /// I/O error (network, filesystem, etc).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization/deserialization error.
    #[error("serialization: {0}")]
    Serialization(String),
}

impl StorageError {
    pub fn not_found(key: impl Into<String>) -> Self {
        Self::NotFound { key: key.into() }
    }

    pub fn conflict(key: impl Into<String>) -> Self {
        Self::Conflict { key: key.into() }
    }

    pub fn invalid_range(key: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidRange {
            key: key.into(),
            reason: reason.into(),
        }
    }

    pub fn serialization(msg: impl Into<String>) -> Self {
        Self::Serialization(msg.into())
    }
}
