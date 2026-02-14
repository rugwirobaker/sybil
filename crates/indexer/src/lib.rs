mod error;
mod flusher;
mod handlers;
mod routes;
mod state;

use std::sync::Arc;

use lib::buffer::BufferManager;
use lib::cache::{CacheConfig, CacheManager};
use lib::config::Config;
use lib::storage::ObjectStore;

use crate::state::IndexerState;

/// The indexer service.
///
/// Accepts logs via HTTP, extracts indexed fields,
/// buffers in memory, and flushes L0 segments to S3.
pub struct IndexerService {
    config: Config,
    state: Arc<IndexerState>,
}

impl IndexerService {
    /// Create a new indexer service from shared config and storage.
    pub async fn new(
        config: Config,
        storage: Arc<dyn ObjectStore>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        tracing::info!("initializing indexer service");

        // Build buffer manager from indexer.buffer config
        let buffers = Arc::new(BufferManager::new(config.indexer.buffer.clone()));
        tracing::debug!(
            segment_size_bytes = config.indexer.buffer.segment_size_bytes,
            flush_interval_secs = config.indexer.buffer.flush_interval_secs,
            max_entries = config.indexer.buffer.max_entries,
            "buffer manager configured"
        );

        // Build cache manager (minimal config — indexer doesn't need segment/block caches)
        let cache_config = CacheConfig::minimal();
        let cache = Arc::new(CacheManager::new(&cache_config).await?);
        tracing::debug!("cache manager initialized");

        let state = Arc::new(IndexerState {
            buffers,
            storage,
            cache,
            config: Arc::new(config.clone()),
        });

        tracing::info!("indexer service initialized");
        Ok(Self { config, state })
    }

    /// Run the indexer: spawn background flusher and serve HTTP.
    pub async fn run(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Spawn flusher background task
        tracing::info!("spawning flusher background task");
        tokio::spawn(flusher::flusher_loop(self.state.clone()));

        // Bind to configured address
        let addr = &self.config.indexer.bind;
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!(addr = %addr, "indexer listening");

        axum::serve(listener, routes::router(self.state)).await?;

        tracing::info!("indexer shutting down");
        Ok(())
    }
}
