use bytes::Bytes;

use super::error::StorageError;

/// Result type for storage operations.
pub type Result<T> = std::result::Result<T, StorageError>;

// ─────────────────────────────────────────────────────────────────────────────
// ObjectStore trait
// ─────────────────────────────────────────────────────────────────────────────

/// Low-level object storage interface.
///
/// This is an abstraction over S3-compatible storage. Keys are paths,
/// values are byte blobs. Higher-level constructs (segments, manifests)
/// are built on top of this.
#[async_trait::async_trait]
pub trait ObjectStore: Send + Sync {
    /// Get an object by key.
    async fn get(&self, key: &str) -> Result<Bytes>;

    /// Get an object with its ETag (for conditional updates).
    async fn get_with_etag(&self, key: &str) -> Result<(Bytes, String)>;

    /// Put an object.
    async fn put(&self, key: &str, data: Bytes) -> Result<()>;

    /// Put an object only if ETag matches (optimistic locking).
    ///
    /// Returns `StorageError::Conflict` if the object was modified.
    async fn put_if_match(&self, key: &str, data: Bytes, etag: &str) -> Result<()>;

    /// Delete an object.
    async fn delete(&self, key: &str) -> Result<()>;

    /// List objects with a given prefix.
    async fn list(&self, prefix: &str) -> Result<Vec<String>>;

    /// Check if an object exists.
    async fn exists(&self, key: &str) -> Result<bool>;
}

// ─────────────────────────────────────────────────────────────────────────────
// ObjectKey
// ─────────────────────────────────────────────────────────────────────────────

/// Constructs object keys (paths) for storage.
///
/// All keys follow a hierarchical structure that maps to S3 prefixes,
/// enabling efficient listing and isolation by namespace.
pub struct ObjectKey;

impl ObjectKey {
    // ─────────────────────────────────────────────────────────────────────
    // Segments
    // ─────────────────────────────────────────────────────────────────────

    /// L0 segment: `namespaces/{ns}/segments/L0/{hour}/{id}.seg`
    pub fn segment_l0(namespace: &str, hour: &str, id: &str) -> String {
        format!("namespaces/{}/segments/L0/{}/{}.seg", namespace, hour, id)
    }

    /// L1 segment: `namespaces/{ns}/segments/L1/{date}/{hour}.seg`
    pub fn segment_l1(namespace: &str, date: &str, hour: &str) -> String {
        format!("namespaces/{}/segments/L1/{}/{}.seg", namespace, date, hour)
    }

    /// L2 segment: `namespaces/{ns}/segments/L2/{date}.seg`
    pub fn segment_l2(namespace: &str, date: &str) -> String {
        format!("namespaces/{}/segments/L2/{}.seg", namespace, date)
    }

    /// Prefix for all segments in a namespace.
    pub fn segments_prefix(namespace: &str) -> String {
        format!("namespaces/{}/segments/", namespace)
    }

    /// Prefix for L0 segments in a namespace.
    pub fn segments_l0_prefix(namespace: &str) -> String {
        format!("namespaces/{}/segments/L0/", namespace)
    }

    // ─────────────────────────────────────────────────────────────────────
    // Manifest
    // ─────────────────────────────────────────────────────────────────────

    /// Manifest head: `namespaces/{ns}/manifest/head.json`
    pub fn manifest_head(namespace: &str) -> String {
        format!("namespaces/{}/manifest/head.json", namespace)
    }

    /// Manifest snapshot: `namespaces/{ns}/manifest/snapshot-{ts}.json`
    pub fn manifest_snapshot(namespace: &str, timestamp: &str) -> String {
        format!(
            "namespaces/{}/manifest/snapshot-{}.json",
            namespace, timestamp
        )
    }

    /// Manifest log entry: `namespaces/{ns}/manifest/log/{id}.json`
    pub fn manifest_log_entry(namespace: &str, id: &str) -> String {
        format!("namespaces/{}/manifest/log/{}.json", namespace, id)
    }

    /// Prefix for manifest log entries.
    pub fn manifest_log_prefix(namespace: &str) -> String {
        format!("namespaces/{}/manifest/log/", namespace)
    }

    // ─────────────────────────────────────────────────────────────────────
    // Catalog
    // ─────────────────────────────────────────────────────────────────────

    /// Catalog head: `metadata/head.json`
    pub fn catalog_head() -> String {
        "metadata/head.json".to_string()
    }

    /// Catalog snapshot: `metadata/snapshots/{ts}.json`
    pub fn catalog_snapshot(timestamp: &str) -> String {
        format!("metadata/snapshots/{}.json", timestamp)
    }

    /// Catalog log entry: `metadata/log/{id}.json`
    pub fn catalog_log_entry(id: &str) -> String {
        format!("metadata/log/{}.json", id)
    }

    // ─────────────────────────────────────────────────────────────────────
    // Indexes (secondary, e.g., full-text)
    // ─────────────────────────────────────────────────────────────────────

    /// Full-text index: `namespaces/{ns}/indexes/fulltext/{date}.idx`
    pub fn fulltext_index(namespace: &str, date: &str) -> String {
        format!("namespaces/{}/indexes/fulltext/{}.idx", namespace, date)
    }
}
