use lib::Config;

/// Run the steward service.
///
/// The steward handles background housekeeping: compaction,
/// garbage collection, retention enforcement, and secondary indexes.
pub async fn run(_config: Config) -> Result<(), StewardError> {
    todo!("steward not implemented")
}

#[derive(Debug, thiserror::Error)]
pub enum StewardError {
    #[error("steward error: {0}")]
    Internal(String),
}
