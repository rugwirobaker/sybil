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

    #[tokio::test]
    async fn test_put_and_get() {
        let store = MemoryStore::new();
        let key = "test/object.txt";
        let data = Bytes::from("hello world");

        store.put(key, data.clone()).await.unwrap();
        let retrieved = store.get(key).await.unwrap();

        assert_eq!(retrieved, data);
    }

    #[tokio::test]
    async fn test_get_not_found() {
        let store = MemoryStore::new();

        let result = store.get("nonexistent").await;

        assert!(matches!(result, Err(StorageError::NotFound { .. })));
    }

    #[tokio::test]
    async fn test_get_with_etag() {
        let store = MemoryStore::new();
        let key = "test/object.txt";
        let data = Bytes::from("hello");

        store.put(key, data.clone()).await.unwrap();
        let (retrieved, etag) = store.get_with_etag(key).await.unwrap();

        assert_eq!(retrieved, data);
        assert!(!etag.is_empty());
    }

    #[tokio::test]
    async fn test_put_if_match_success() {
        let store = MemoryStore::new();
        let key = "test/object.txt";

        store.put(key, Bytes::from("v1")).await.unwrap();
        let (_, etag) = store.get_with_etag(key).await.unwrap();

        store
            .put_if_match(key, Bytes::from("v2"), &etag)
            .await
            .unwrap();

        let data = store.get(key).await.unwrap();
        assert_eq!(data, Bytes::from("v2"));
    }

    #[tokio::test]
    async fn test_put_if_match_conflict() {
        let store = MemoryStore::new();
        let key = "test/object.txt";

        store.put(key, Bytes::from("v1")).await.unwrap();
        let (_, etag) = store.get_with_etag(key).await.unwrap();

        // Simulate concurrent modification
        store.put(key, Bytes::from("v2")).await.unwrap();

        // Now put_if_match should fail
        let result = store.put_if_match(key, Bytes::from("v3"), &etag).await;

        assert!(matches!(result, Err(StorageError::Conflict { .. })));
    }

    #[tokio::test]
    async fn test_put_if_match_not_found() {
        let store = MemoryStore::new();

        let result = store
            .put_if_match("nonexistent", Bytes::from("data"), "etag")
            .await;

        assert!(matches!(result, Err(StorageError::NotFound { .. })));
    }

    #[tokio::test]
    async fn test_delete() {
        let store = MemoryStore::new();
        let key = "test/object.txt";

        store.put(key, Bytes::from("data")).await.unwrap();
        assert!(store.exists(key).await.unwrap());

        store.delete(key).await.unwrap();
        assert!(!store.exists(key).await.unwrap());
    }

    #[tokio::test]
    async fn test_delete_nonexistent() {
        let store = MemoryStore::new();

        // Should not error
        store.delete("nonexistent").await.unwrap();
    }

    #[tokio::test]
    async fn test_list() {
        let store = MemoryStore::new();

        store.put("a/1.txt", Bytes::from("1")).await.unwrap();
        store.put("a/2.txt", Bytes::from("2")).await.unwrap();
        store.put("b/1.txt", Bytes::from("3")).await.unwrap();

        let mut keys = store.list("a/").await.unwrap();
        keys.sort();

        assert_eq!(keys, vec!["a/1.txt", "a/2.txt"]);
    }

    #[tokio::test]
    async fn test_list_empty() {
        let store = MemoryStore::new();

        let keys = store.list("nonexistent/").await.unwrap();

        assert!(keys.is_empty());
    }

    #[tokio::test]
    async fn test_exists() {
        let store = MemoryStore::new();
        let key = "test/object.txt";

        assert!(!store.exists(key).await.unwrap());

        store.put(key, Bytes::from("data")).await.unwrap();

        assert!(store.exists(key).await.unwrap());
    }

    #[tokio::test]
    async fn test_get_range_specific() {
        let store = MemoryStore::new();
        let key = "test/object.txt";
        store.put(key, Bytes::from("hello world")).await.unwrap();

        // Bytes 0-4 inclusive = "hello"
        let data = store
            .get_range(key, ByteRange::Range { start: 0, end: 4 })
            .await
            .unwrap();
        assert_eq!(data, Bytes::from("hello"));

        // Bytes 6-10 inclusive = "world"
        let data = store
            .get_range(key, ByteRange::Range { start: 6, end: 10 })
            .await
            .unwrap();
        assert_eq!(data, Bytes::from("world"));
    }

    #[tokio::test]
    async fn test_get_range_from_offset() {
        let store = MemoryStore::new();
        let key = "test/object.txt";
        store.put(key, Bytes::from("hello world")).await.unwrap();

        // From byte 6 to end = "world"
        let data = store
            .get_range(key, ByteRange::From(6))
            .await
            .unwrap();
        assert_eq!(data, Bytes::from("world"));
    }

    #[tokio::test]
    async fn test_get_range_suffix() {
        let store = MemoryStore::new();
        let key = "test/object.txt";
        store.put(key, Bytes::from("hello world")).await.unwrap();

        // Last 5 bytes = "world"
        let data = store
            .get_range(key, ByteRange::Suffix(5))
            .await
            .unwrap();
        assert_eq!(data, Bytes::from("world"));
    }

    #[tokio::test]
    async fn test_get_range_suffix_larger_than_object() {
        let store = MemoryStore::new();
        let key = "test/object.txt";
        store.put(key, Bytes::from("hello")).await.unwrap();

        // Request last 100 bytes, object is only 5 bytes - returns entire object
        let data = store
            .get_range(key, ByteRange::Suffix(100))
            .await
            .unwrap();
        assert_eq!(data, Bytes::from("hello"));
    }

    #[tokio::test]
    async fn test_get_range_clamps_end() {
        let store = MemoryStore::new();
        let key = "test/object.txt";
        store.put(key, Bytes::from("hello")).await.unwrap();

        // Request bytes 0-100, object is only 5 bytes - clamps to 0-4
        let data = store
            .get_range(key, ByteRange::Range { start: 0, end: 100 })
            .await
            .unwrap();
        assert_eq!(data, Bytes::from("hello"));
    }

    #[tokio::test]
    async fn test_get_range_invalid_start_greater_than_end() {
        let store = MemoryStore::new();
        let key = "test/object.txt";
        store.put(key, Bytes::from("hello")).await.unwrap();

        let result = store
            .get_range(key, ByteRange::Range { start: 5, end: 2 })
            .await;
        assert!(matches!(result, Err(StorageError::InvalidRange { .. })));
    }

    #[tokio::test]
    async fn test_get_range_invalid_start_beyond_object() {
        let store = MemoryStore::new();
        let key = "test/object.txt";
        store.put(key, Bytes::from("hello")).await.unwrap();

        let result = store
            .get_range(key, ByteRange::Range { start: 10, end: 15 })
            .await;
        assert!(matches!(result, Err(StorageError::InvalidRange { .. })));
    }

    #[tokio::test]
    async fn test_get_range_not_found() {
        let store = MemoryStore::new();

        let result = store
            .get_range("nonexistent", ByteRange::Range { start: 0, end: 10 })
            .await;
        assert!(matches!(result, Err(StorageError::NotFound { .. })));
    }
}
