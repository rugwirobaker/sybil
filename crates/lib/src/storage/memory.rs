use std::collections::HashMap;
use std::sync::RwLock;

use bytes::Bytes;

use super::engine::{ByteRange, ObjectStore, Result};
use super::error::StorageError;

/// In-memory object store for testing.
///
/// Stores objects in a HashMap. ETags are generated from a simple counter.
#[derive(Debug, Default)]
pub struct MemoryStore {
    objects: RwLock<HashMap<String, Object>>,
}

#[derive(Debug, Clone)]
struct Object {
    data: Bytes,
    etag: String,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn generate_etag() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{:x}", nanos)
    }
}

#[async_trait::async_trait]
impl ObjectStore for MemoryStore {
    async fn get(&self, key: &str) -> Result<Bytes> {
        let objects = self.objects.read().unwrap();
        objects
            .get(key)
            .map(|obj| obj.data.clone())
            .ok_or_else(|| StorageError::not_found(key))
    }

    async fn get_with_etag(&self, key: &str) -> Result<(Bytes, String)> {
        let objects = self.objects.read().unwrap();
        objects
            .get(key)
            .map(|obj| (obj.data.clone(), obj.etag.clone()))
            .ok_or_else(|| StorageError::not_found(key))
    }

    async fn get_range(&self, key: &str, range: ByteRange) -> Result<Bytes> {
        let objects = self.objects.read().unwrap();
        let obj = objects
            .get(key)
            .ok_or_else(|| StorageError::not_found(key))?;

        let len = obj.data.len() as u64;

        let (start, end) = match range {
            ByteRange::Range { start, end } => {
                if start > end {
                    return Err(StorageError::invalid_range(
                        key,
                        format!("start ({}) > end ({})", start, end),
                    ));
                }
                (start, end)
            }
            ByteRange::From(offset) => {
                if offset >= len {
                    return Err(StorageError::invalid_range(
                        key,
                        format!("offset {} >= object size {}", offset, len),
                    ));
                }
                (offset, len - 1)
            }
            ByteRange::Suffix(suffix_len) => {
                if suffix_len == 0 {
                    return Err(StorageError::invalid_range(key, "suffix length is 0"));
                }
                if suffix_len > len {
                    // S3 behavior: return entire object if suffix > size
                    (0, len - 1)
                } else {
                    (len - suffix_len, len - 1)
                }
            }
        };

        // Clamp end to object bounds (S3 behavior)
        let end = end.min(len - 1);

        if start >= len {
            return Err(StorageError::invalid_range(
                key,
                format!("start ({}) >= object size ({})", start, len),
            ));
        }

        Ok(obj.data.slice(start as usize..=end as usize))
    }

    async fn put(&self, key: &str, data: Bytes) -> Result<()> {
        let mut objects = self.objects.write().unwrap();

        // Objects are immutable - fail if already exists
        if objects.contains_key(key) {
            return Err(StorageError::already_exists(key));
        }

        objects.insert(
            key.to_string(),
            Object {
                data,
                etag: Self::generate_etag(),
            },
        );
        Ok(())
    }

    async fn put_if_match(&self, key: &str, data: Bytes, etag: &str) -> Result<()> {
        let mut objects = self.objects.write().unwrap();

        match objects.get(key) {
            Some(existing) if existing.etag == etag => {
                objects.insert(
                    key.to_string(),
                    Object {
                        data,
                        etag: Self::generate_etag(),
                    },
                );
                Ok(())
            }
            Some(_) => Err(StorageError::conflict(key)),
            None => Err(StorageError::not_found(key)),
        }
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let mut objects = self.objects.write().unwrap();
        objects.remove(key);
        Ok(())
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let objects = self.objects.read().unwrap();
        let keys: Vec<String> = objects
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        Ok(keys)
    }

    async fn exists(&self, key: &str) -> Result<bool> {
        let objects = self.objects.read().unwrap();
        Ok(objects.contains_key(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::test_harness::{object_store_tests, TestHarness};

    /// Run all standard ObjectStore tests against MemoryStore.
    #[tokio::test]
    async fn object_store_conformance() {
        let harness = TestHarness::new(MemoryStore::new());
        object_store_tests::run_all(&harness).await;
        // Cleanup is optional for MemoryStore but good practice
        harness.cleanup().await.unwrap();
    }
}
