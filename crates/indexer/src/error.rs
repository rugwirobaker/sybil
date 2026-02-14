use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum IndexerError {
    #[error("namespace not found: {0}")]
    NamespaceNotFound(String),

    #[error("namespace already exists: {0}")]
    NamespaceAlreadyExists(String),

    #[error("invalid namespace name: {0}")]
    InvalidNamespaceName(String),

    #[error("storage error: {0}")]
    Storage(#[from] lib::storage::StorageError),

    #[error("buffer error: {0}")]
    Buffer(#[from] lib::buffer::BufferError),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("internal error: {0}")]
    Internal(String),
}

impl IntoResponse for IndexerError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            IndexerError::NamespaceNotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
            IndexerError::NamespaceAlreadyExists(_) => (StatusCode::CONFLICT, self.to_string()),
            IndexerError::InvalidNamespaceName(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            IndexerError::Storage(_) => {
                tracing::error!(error = %self, "storage error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal storage error".to_string(),
                )
            }
            IndexerError::Buffer(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            IndexerError::Serialization(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            IndexerError::Internal(_) => {
                tracing::error!(error = %self, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal error".to_string(),
                )
            }
        };

        let body = serde_json::json!({ "error": message });
        (status, axum::Json(body)).into_response()
    }
}
