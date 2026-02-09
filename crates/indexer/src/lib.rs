use lib::Config;

/// Run the indexer service.
///
/// The indexer accepts logs via HTTP, extracts indexed fields,
/// buffers in memory, and flushes L0 segments to S3.
pub async fn run(_config: Config) -> Result<(), IndexerError> {
    todo!("indexer not implemented")
}

#[derive(Debug, thiserror::Error)]
pub enum IndexerError {
    #[error("indexer error: {0}")]
    Internal(String),
}
