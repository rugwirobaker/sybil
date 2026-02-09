use lib::Config;

/// Run the querier service.
///
/// The querier accepts SQL queries via HTTP, reads segments from S3,
/// and returns results.
pub async fn run(_config: Config) -> Result<(), QuerierError> {
    todo!("querier not implemented")
}

#[derive(Debug, thiserror::Error)]
pub enum QuerierError {
    #[error("querier error: {0}")]
    Internal(String),
}
