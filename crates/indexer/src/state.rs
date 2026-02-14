use std::sync::Arc;

use bytes::Bytes;

use lib::buffer::BufferManager;
use lib::cache::{CacheManager, CachedCatalog};
use lib::catalog::{self, Namespace, State as CatalogState};
use lib::config::Config;
use lib::storage::{ObjectKey, ObjectStore};

use crate::error::IndexerError;

/// Shared state for the indexer service.
#[derive(Clone)]
pub struct IndexerState {
    /// Per-namespace memtables (lock-free writes).
    pub buffers: Arc<BufferManager>,
    /// S3 client for writing segments and catalog operations.
    pub storage: Arc<dyn ObjectStore>,
    /// Unified cache manager.
    pub cache: Arc<CacheManager>,
    /// Configuration.
    pub config: Arc<Config>,
}

impl IndexerState {
    /// Load the catalog from cache, or fetch from S3 on cache miss.
    pub async fn ensure_catalog(&self) -> Result<CatalogState, IndexerError> {
        // Fast path: cache hit
        if let Some(state) = self.cache.catalog.get_state() {
            tracing::debug!("catalog cache hit");
            return Ok(state);
        }

        // Slow path: fetch from S3
        tracing::info!("catalog cache miss, refreshing from S3");
        self.refresh_catalog().await
    }

    /// Fetch the catalog from S3 and populate the cache.
    async fn refresh_catalog(&self) -> Result<CatalogState, IndexerError> {
        let head_key = ObjectKey::catalog_head();
        let (head_bytes, etag) =
            self.storage
                .get_with_etag(&head_key)
                .await
                .map_err(|e| match e {
                    lib::storage::StorageError::NotFound { .. } => {
                        tracing::warn!("catalog head not found in S3, not initialized yet");
                        IndexerError::Internal("catalog not initialized".to_string())
                    }
                    other => IndexerError::Storage(other),
                })?;

        let head = catalog::Head::from_bytes(&head_bytes)?;
        tracing::debug!(
            log_tail_len = head.log_tail.len(),
            "loaded catalog head from S3"
        );

        // Load snapshot
        let snapshot_bytes = self.storage.get(&head.snapshot).await?;
        let snapshot = catalog::Snapshot::from_bytes(&snapshot_bytes)?;

        // Load log entries
        let mut log_entries = Vec::with_capacity(head.log_tail.len());
        for entry_id in &head.log_tail {
            let key = ObjectKey::catalog_log_entry(entry_id);
            let entry_bytes = self.storage.get(&key).await?;
            let entry = catalog::LongEntry::from_bytes(&entry_bytes)?;
            log_entries.push(entry);
        }

        let cached = CachedCatalog::new(head, Some(etag), snapshot, log_entries);
        let state = cached.state.clone();
        let ns_count = state.list().count();
        self.cache.catalog.set(cached);

        tracing::info!(namespaces = ns_count, "catalog refreshed from S3");
        Ok(state)
    }

    /// Look up a namespace by name. Returns the NamespaceInfo or 404.
    pub async fn resolve_namespace(&self, name: &str) -> Result<Namespace, IndexerError> {
        let state = self.ensure_catalog().await?;
        match state.get_by_name(name).cloned() {
            Some(ns) => {
                tracing::debug!(namespace = %name, id = %ns.id, "namespace resolved");
                Ok(ns)
            }
            None => {
                tracing::debug!(namespace = %name, "namespace not found");
                Err(IndexerError::NamespaceNotFound(name.to_string()))
            }
        }
    }

    /// Create a new namespace: write log entry to S3, update head, invalidate cache.
    pub async fn create_namespace(
        &self,
        name: &str,
        retention_days: u32,
    ) -> Result<Namespace, IndexerError> {
        tracing::info!(namespace = %name, retention_days, "creating namespace");

        // Validate name
        if !is_valid_namespace_name(name) {
            tracing::warn!(namespace = %name, "invalid namespace name");
            return Err(IndexerError::InvalidNamespaceName(name.to_string()));
        }

        // Check for duplicates (skip if catalog doesn't exist yet)
        match self.ensure_catalog().await {
            Ok(state) => {
                if state.get_by_name(name).is_some() {
                    tracing::warn!(namespace = %name, "namespace already exists");
                    return Err(IndexerError::NamespaceAlreadyExists(name.to_string()));
                }
            }
            Err(IndexerError::Internal(ref msg)) if msg == "catalog not initialized" => {
                tracing::info!("catalog not initialized, skipping duplicate check");
            }
            Err(e) => return Err(e),
        }

        let ns = Namespace::with_retention(name, retention_days);

        // Write log entry to S3
        let log_entry = catalog::LongEntry::add(ns.clone());
        let log_key = ObjectKey::catalog_log_entry(&log_entry.id);
        let log_bytes = log_entry.to_bytes()?;
        self.storage.put(&log_key, Bytes::from(log_bytes)).await?;

        // Read-modify-write head.json with ETag for optimistic locking
        let head_key = ObjectKey::catalog_head();
        match self.storage.get_with_etag(&head_key).await {
            Ok((head_bytes, etag)) => {
                let mut head = catalog::Head::from_bytes(&head_bytes)?;
                head.log_tail.push(log_entry.id);
                let new_head_bytes = head.to_bytes()?;
                self.storage
                    .put_if_match(&head_key, Bytes::from(new_head_bytes), &etag)
                    .await?;
            }
            Err(lib::storage::StorageError::NotFound { .. }) => {
                // First namespace ever: create initial snapshot + head
                let snapshot = catalog::Snapshot::empty();
                let snapshot_ts = chrono::Utc::now().to_rfc3339();
                let snapshot_key = ObjectKey::catalog_snapshot(&snapshot_ts);
                let snapshot_bytes = snapshot.to_bytes()?;
                self.storage
                    .put(&snapshot_key, Bytes::from(snapshot_bytes))
                    .await?;

                let head = catalog::Head {
                    version: 1,
                    snapshot: snapshot_key,
                    log_tail: vec![log_entry.id],
                };
                let head_bytes = head.to_bytes()?;
                self.storage.put(&head_key, Bytes::from(head_bytes)).await?;
            }
            Err(e) => return Err(IndexerError::Storage(e)),
        }

        // Invalidate cache so next read picks up the new namespace
        self.cache.invalidate_catalog();

        // Initialize empty manifest for the new namespace
        self.init_namespace_manifest(&ns.id).await?;

        tracing::info!(
            namespace = %name,
            id = %ns.id,
            "namespace created"
        );

        Ok(ns)
    }

    /// Initialize an empty manifest for a new namespace.
    async fn init_namespace_manifest(&self, namespace_id: &str) -> Result<(), IndexerError> {
        let snapshot = lib::manifest::Snapshot::empty();
        let snapshot_ts = chrono::Utc::now().to_rfc3339();
        let snapshot_key = ObjectKey::manifest_snapshot(namespace_id, &snapshot_ts);
        let snapshot_bytes = snapshot
            .to_bytes()
            .map_err(|e| IndexerError::Internal(e.to_string()))?;
        self.storage
            .put(&snapshot_key, Bytes::from(snapshot_bytes))
            .await?;

        let head = lib::manifest::Head::new(snapshot_key);
        let head_key = ObjectKey::manifest_head(namespace_id);
        let head_bytes = head
            .to_bytes()
            .map_err(|e| IndexerError::Internal(e.to_string()))?;
        self.storage.put(&head_key, Bytes::from(head_bytes)).await?;

        Ok(())
    }

    /// Delete (soft) a namespace: write Remove log entry, update head.
    pub async fn delete_namespace(&self, name: &str) -> Result<(), IndexerError> {
        tracing::info!(namespace = %name, "deleting namespace");
        let ns = self.resolve_namespace(name).await?;

        let log_entry = catalog::LongEntry::remove(&ns.id);
        let log_key = ObjectKey::catalog_log_entry(&log_entry.id);
        let log_bytes = log_entry.to_bytes()?;
        self.storage.put(&log_key, Bytes::from(log_bytes)).await?;

        // Update head
        let head_key = ObjectKey::catalog_head();
        let (head_bytes, etag) = self.storage.get_with_etag(&head_key).await?;
        let mut head = catalog::Head::from_bytes(&head_bytes)?;
        head.log_tail.push(log_entry.id);
        let new_head_bytes = head.to_bytes()?;
        self.storage
            .put_if_match(&head_key, Bytes::from(new_head_bytes), &etag)
            .await?;

        // Remove from buffer manager and invalidate caches
        self.buffers.remove(&ns.id);
        self.cache.invalidate_namespace(&ns.id);
        self.cache.invalidate_catalog();

        tracing::info!(namespace = %name, id = %ns.id, "namespace deleted");
        Ok(())
    }
}

/// Validate namespace name: alphanumeric, hyphens, underscores, 1-128 chars.
fn is_valid_namespace_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_namespace_names() {
        assert!(is_valid_namespace_name("api-gateway"));
        assert!(is_valid_namespace_name("my_service"));
        assert!(is_valid_namespace_name("service123"));
        assert!(is_valid_namespace_name("a"));
    }

    #[test]
    fn test_invalid_namespace_names() {
        assert!(!is_valid_namespace_name(""));
        assert!(!is_valid_namespace_name("has spaces"));
        assert!(!is_valid_namespace_name("has.dots"));
        assert!(!is_valid_namespace_name("has/slashes"));
        assert!(!is_valid_namespace_name(&"a".repeat(129)));
    }
}
