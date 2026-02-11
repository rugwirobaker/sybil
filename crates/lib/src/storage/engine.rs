use bytes::Bytes;

use super::error::StorageError;

/// Result type for storage operations.
pub type Result<T> = std::result::Result<T, StorageError>;

// ─────────────────────────────────────────────────────────────────────────────
// ByteRange
// ─────────────────────────────────────────────────────────────────────────────

/// Specifies a byte range for partial object reads.
///
/// Maps directly to S3's Range header capabilities:
/// - `Range { start, end }` → `Range: bytes=start-end`
/// - `From(offset)` → `Range: bytes=offset-`
/// - `Suffix(len)` → `Range: bytes=-len`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// Bytes from start to end (inclusive on both ends).
    Range { start: u64, end: u64 },
    /// From offset to end of object.
    From(u64),
    /// Last `len` bytes of the object.
    Suffix(u64),
}

impl std::fmt::Display for ByteRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ByteRange::Range { start, end } => write!(f, "bytes {start}-{end}"),
            ByteRange::From(offset) => write!(f, "bytes {offset}-"),
            ByteRange::Suffix(len) => write!(f, "last {len} bytes"),
        }
    }
}

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

    /// Get a byte range from an object.
    ///
    /// Returns `InvalidRange` if the range cannot be satisfied (e.g., object
    /// is smaller than requested range, or start > end).
    async fn get_range(&self, key: &str, range: ByteRange) -> Result<Bytes>;

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
        format!("namespaces/{namespace}/segments/L0/{hour}/{id}.seg")
    }

    /// L1 segment: `namespaces/{ns}/segments/L1/{date}/{hour}.seg`
    pub fn segment_l1(namespace: &str, date: &str, hour: &str) -> String {
        format!("namespaces/{namespace}/segments/L1/{date}/{hour}.seg")
    }

    /// L2 segment: `namespaces/{ns}/segments/L2/{date}.seg`
    pub fn segment_l2(namespace: &str, date: &str) -> String {
        format!("namespaces/{namespace}/segments/L2/{date}.seg")
    }

    /// Prefix for all segments in a namespace.
    pub fn segments_prefix(namespace: &str) -> String {
        format!("namespaces/{namespace}/segments/")
    }

    /// Prefix for L0 segments in a namespace.
    pub fn segments_l0_prefix(namespace: &str) -> String {
        format!("namespaces/{namespace}/segments/L0/")
    }

    // ─────────────────────────────────────────────────────────────────────
    // Manifest
    // ─────────────────────────────────────────────────────────────────────

    /// Manifest head: `namespaces/{ns}/manifest/head.json`
    pub fn manifest_head(namespace: &str) -> String {
        format!("namespaces/{namespace}/manifest/head.json")
    }

    /// Manifest snapshot: `namespaces/{ns}/manifest/snapshot-{ts}.json`
    pub fn manifest_snapshot(namespace: &str, timestamp: &str) -> String {
        format!("namespaces/{namespace}/manifest/snapshot-{timestamp}.json")
    }

    /// Manifest log entry: `namespaces/{ns}/manifest/log/{id}.json`
    pub fn manifest_log_entry(namespace: &str, id: &str) -> String {
        format!("namespaces/{namespace}/manifest/log/{id}.json")
    }

    /// Prefix for manifest log entries.
    pub fn manifest_log_prefix(namespace: &str) -> String {
        format!("namespaces/{namespace}/manifest/log/")
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
        format!("metadata/snapshots/{timestamp}.json")
    }

    /// Catalog log entry: `metadata/log/{id}.json`
    pub fn catalog_log_entry(id: &str) -> String {
        format!("metadata/log/{id}.json")
    }

    // ─────────────────────────────────────────────────────────────────────
    // Indexes (secondary, e.g., full-text)
    // ─────────────────────────────────────────────────────────────────────

    /// Full-text index: `namespaces/{ns}/indexes/fulltext/{date}.idx`
    pub fn fulltext_index(namespace: &str, date: &str) -> String {
        format!("namespaces/{namespace}/indexes/fulltext/{date}.idx")
    }
}
