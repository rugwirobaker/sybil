//! Filesystem-backed object store for local development and debugging.
//!
//! Maps object keys to files under a root directory. Useful for inspecting
//! stored data locally before committing to S3.
//!
//! ETags are computed as CRC32 checksums of file content (not MD5 like real S3,
//! but functionally equivalent for optimistic concurrency).

use std::path::{Path, PathBuf};

use bytes::Bytes;
use tokio::fs;

use super::engine::{ByteRange, ObjectStore, Result};
use super::error::StorageError;

/// Filesystem-backed object store.
///
/// Keys are mapped to file paths relative to the root directory.
/// ETags are computed as CRC32 checksums of file content.
#[derive(Debug)]
pub struct FilesystemStore {
    root: PathBuf,
}

impl FilesystemStore {
    /// Create a new filesystem store rooted at the given directory.
    ///
    /// The directory will be created if it doesn't exist.
    pub async fn new(root: impl AsRef<Path>) -> std::io::Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).await?;
        Ok(Self { root })
    }

    /// Resolve a key to an absolute file path.
    fn path(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }

    /// Compute an ETag from file content using CRC32.
    fn content_etag(data: &[u8]) -> String {
        let crc = crc32fast::hash(data);
        format!("\"{crc:08x}\"")
    }
}

#[async_trait::async_trait]
impl ObjectStore for FilesystemStore {
    async fn get(&self, key: &str) -> Result<Bytes> {
        let path = self.path(key);
        let data = fs::read(&path).await.map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => StorageError::not_found(key),
            _ => StorageError::Io(e),
        })?;
        Ok(Bytes::from(data))
    }

    async fn get_with_etag(&self, key: &str) -> Result<(Bytes, String)> {
        let data = self.get(key).await?;
        let etag = Self::content_etag(&data);
        Ok((data, etag))
    }

    async fn get_range(&self, key: &str, range: ByteRange) -> Result<Bytes> {
        let path = self.path(key);

        let meta = fs::metadata(&path).await.map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => StorageError::not_found(key),
            _ => StorageError::Io(e),
        })?;
        let len = meta.len();

        if len == 0 {
            return Err(StorageError::invalid_range(key, "object is empty"));
        }

        let (start, end) = match range {
            ByteRange::Range { start, end } => {
                if start > end {
                    return Err(StorageError::invalid_range(
                        key,
                        format!("start ({start}) > end ({end})"),
                    ));
                }
                (start, end)
            }
            ByteRange::From(offset) => {
                if offset >= len {
                    return Err(StorageError::invalid_range(
                        key,
                        format!("offset {offset} >= object size {len}"),
                    ));
                }
                (offset, len - 1)
            }
            ByteRange::Suffix(suffix_len) => {
                if suffix_len == 0 {
                    return Err(StorageError::invalid_range(key, "suffix length is 0"));
                }
                let start = len.saturating_sub(suffix_len);
                (start, len - 1)
            }
        };

        // Clamp end to object bounds (S3 behavior)
        let end = end.min(len - 1);

        if start >= len {
            return Err(StorageError::invalid_range(
                key,
                format!("start ({start}) >= object size ({len})"),
            ));
        }

        let read_len = (end - start + 1) as usize;
        let data = tokio::task::spawn_blocking(move || -> std::io::Result<Bytes> {
            use std::os::unix::fs::FileExt;
            let file = std::fs::File::open(&path)?;
            let mut buf = vec![0u8; read_len];
            file.read_exact_at(&mut buf, start)?;
            Ok(Bytes::from(buf))
        })
        .await
        .map_err(|e| StorageError::Io(std::io::Error::other(e)))?
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => StorageError::not_found(key),
            _ => StorageError::Io(e),
        })?;

        Ok(data)
    }

    async fn put(&self, key: &str, data: Bytes) -> Result<()> {
        let path = self.path(key);

        // Create parent directories
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        // Run create + write + close in a single blocking call so the file is
        // fully written and closed before we return.
        let key_owned = key.to_string();
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| match e.kind() {
                    std::io::ErrorKind::AlreadyExists => StorageError::already_exists(&key_owned),
                    _ => StorageError::Io(e),
                })?;
            file.write_all(&data).map_err(StorageError::Io)?;
            Ok(())
        })
        .await
        .map_err(|e| StorageError::Io(std::io::Error::other(e)))?
    }

    async fn put_if_match(&self, key: &str, data: Bytes, etag: &str) -> Result<()> {
        let path = self.path(key);

        // Read existing content to verify ETag
        let existing = fs::read(&path).await.map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => StorageError::not_found(key),
            _ => StorageError::Io(e),
        })?;

        let current_etag = Self::content_etag(&existing);
        if current_etag != etag {
            return Err(StorageError::conflict(key));
        }

        fs::write(&path, &data).await?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let path = self.path(key);
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StorageError::Io(e)),
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let search_dir = self.path(prefix);

        // Determine which directory to walk: the prefix itself if it's a
        // directory, otherwise its parent (for partial-path prefixes).
        let dir = if search_dir.is_dir() {
            search_dir
        } else {
            match search_dir.parent() {
                Some(p) => p.to_path_buf(),
                None => return Ok(Vec::new()),
            }
        };

        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut keys = Vec::new();
        let mut stack = vec![dir];

        while let Some(current) = stack.pop() {
            let mut entries = fs::read_dir(&current).await?;
            while let Some(entry) = entries.next_entry().await? {
                let file_type = entry.file_type().await?;
                if file_type.is_dir() {
                    stack.push(entry.path());
                } else if file_type.is_file() {
                    if let Ok(rel) = entry.path().strip_prefix(&self.root) {
                        let key = rel.to_string_lossy().to_string();
                        if key.starts_with(prefix) {
                            keys.push(key);
                        }
                    }
                }
            }
        }

        Ok(keys)
    }

    async fn exists(&self, key: &str) -> Result<bool> {
        match fs::metadata(self.path(key)).await {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(StorageError::Io(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::test_harness::{object_store_tests, TestHarness};

    async fn make_store() -> FilesystemStore {
        let dir = tempfile::tempdir().unwrap();
        // Leak the TempDir so it isn't deleted while tests run.
        // Test cleanup removes the objects; the OS cleans temp dirs eventually.
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        FilesystemStore::new(&path).await.unwrap()
    }

    #[tokio::test]
    async fn object_store_conformance() {
        let store = make_store().await;
        let harness = TestHarness::new(store);
        object_store_tests::run_all(&harness).await;
        harness.cleanup().await.unwrap();
    }
}
