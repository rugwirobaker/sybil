//! Reusable test harness for ObjectStore implementations.
//!
//! This module provides a test harness that can be used to test any
//! `ObjectStore` implementation without duplicating test code.
//!
//! # Usage
//!
//! ```ignore
//! use sybil_lib::storage::test_harness::{TestHarness, object_store_tests};
//!
//! #[tokio::test]
//! async fn test_my_store() {
//!     let store = MyStore::new();
//!     let harness = TestHarness::new(store);
//!     object_store_tests::run_all(&harness).await;
//!     harness.cleanup().await.unwrap();
//! }
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;

use super::engine::{ByteRange, ObjectStore, Result};
use super::error::StorageError;

/// Test harness for ObjectStore implementations.
///
/// Wraps an ObjectStore and provides:
/// - Automatic key prefixing for test isolation
/// - Key tracking for cleanup
/// - Cleanup method to remove all created objects
pub struct TestHarness<S: ObjectStore> {
    store: S,
    prefix: String,
    created_keys: Arc<Mutex<Vec<String>>>,
    key_counter: AtomicU64,
}

impl<S: ObjectStore> TestHarness<S> {
    /// Create a new test harness with a unique prefix.
    pub fn new(store: S) -> Self {
        Self::with_prefix(store, generate_test_prefix())
    }

    /// Create a new test harness with a specific prefix.
    pub fn with_prefix(store: S, prefix: String) -> Self {
        Self {
            store,
            prefix,
            created_keys: Arc::new(Mutex::new(Vec::new())),
            key_counter: AtomicU64::new(0),
        }
    }

    /// Get the underlying store.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Get the test prefix.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Generate a unique key within this test's namespace.
    pub fn key(&self, name: &str) -> String {
        let key = format!("{}{}", self.prefix, name);
        self.created_keys.lock().unwrap().push(key.clone());
        key
    }

    /// Generate a unique numbered key (useful for multiple objects).
    pub fn unique_key(&self, base: &str) -> String {
        let n = self.key_counter.fetch_add(1, Ordering::SeqCst);
        self.key(&format!("{base}-{n}"))
    }

    /// Clean up all objects created during the test.
    ///
    /// This should be called at the end of each test, especially for
    /// remote storage backends like S3 where objects persist.
    pub async fn cleanup(&self) -> Result<()> {
        let keys = self.created_keys.lock().unwrap().clone();
        for key in keys {
            // Ignore errors - object might not exist if test failed early
            let _ = self.store.delete(&key).await;
        }

        // Also try to clean up anything with our prefix (in case we missed some)
        if let Ok(remaining) = self.store.list(&self.prefix).await {
            for key in remaining {
                let _ = self.store.delete(&key).await;
            }
        }

        Ok(())
    }
}

/// Generate a unique prefix for test isolation.
fn generate_test_prefix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("_test_{nanos:x}/")
}

// ─────────────────────────────────────────────────────────────────────────────
// Test Suite
// ─────────────────────────────────────────────────────────────────────────────

/// Collection of tests for ObjectStore implementations.
///
/// Each test function is public so implementations can run individual tests
/// or the full suite via `run_all`.
pub mod object_store_tests {
    use super::*;

    /// Run all ObjectStore tests.
    pub async fn run_all<S: ObjectStore>(h: &TestHarness<S>) {
        test_put_and_get(h).await;
        test_put_already_exists(h).await;
        test_get_not_found(h).await;
        test_get_with_etag(h).await;
        test_put_if_match_success(h).await;
        test_put_if_match_conflict(h).await;
        test_put_if_match_not_found(h).await;
        test_delete(h).await;
        test_delete_nonexistent(h).await;
        test_list(h).await;
        test_list_empty(h).await;
        test_exists(h).await;
        test_get_range_specific(h).await;
        test_get_range_from_offset(h).await;
        test_get_range_suffix(h).await;
        test_get_range_suffix_larger_than_object(h).await;
        test_get_range_clamps_end(h).await;
        test_get_range_invalid_start_greater_than_end(h).await;
        test_get_range_invalid_start_beyond_object(h).await;
        test_get_range_not_found(h).await;
    }

    pub async fn test_put_and_get<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("object.txt");
        let data = Bytes::from("hello world");

        h.store().put(&key, data.clone()).await.unwrap();
        let retrieved = h.store().get(&key).await.unwrap();

        assert_eq!(retrieved, data);
    }

    pub async fn test_put_already_exists<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("immutable.txt");

        // First put succeeds
        h.store().put(&key, Bytes::from("v1")).await.unwrap();

        // Second put to same key fails - objects are immutable
        let result = h.store().put(&key, Bytes::from("v2")).await;

        assert!(matches!(result, Err(StorageError::AlreadyExists { .. })));

        // Original data unchanged
        let data = h.store().get(&key).await.unwrap();
        assert_eq!(data, Bytes::from("v1"));
    }

    pub async fn test_get_not_found<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("nonexistent");

        let result = h.store().get(&key).await;

        assert!(matches!(result, Err(StorageError::NotFound { .. })));
    }

    pub async fn test_get_with_etag<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("with-etag.txt");
        let data = Bytes::from("hello");

        h.store().put(&key, data.clone()).await.unwrap();
        let (retrieved, etag) = h.store().get_with_etag(&key).await.unwrap();

        assert_eq!(retrieved, data);
        assert!(!etag.is_empty());
    }

    pub async fn test_put_if_match_success<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("if-match-success.txt");

        h.store().put(&key, Bytes::from("v1")).await.unwrap();
        let (_, etag) = h.store().get_with_etag(&key).await.unwrap();

        h.store()
            .put_if_match(&key, Bytes::from("v2"), &etag)
            .await
            .unwrap();

        let data = h.store().get(&key).await.unwrap();
        assert_eq!(data, Bytes::from("v2"));
    }

    pub async fn test_put_if_match_conflict<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("if-match-conflict.txt");

        h.store().put(&key, Bytes::from("v1")).await.unwrap();
        let (_, etag_v1) = h.store().get_with_etag(&key).await.unwrap();

        // Simulate concurrent modification by another writer
        h.store()
            .put_if_match(&key, Bytes::from("v2"), &etag_v1)
            .await
            .unwrap();

        // Now put_if_match with stale etag should fail
        let result = h
            .store()
            .put_if_match(&key, Bytes::from("v3"), &etag_v1)
            .await;

        assert!(matches!(result, Err(StorageError::Conflict { .. })));
    }

    pub async fn test_put_if_match_not_found<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("if-match-not-found.txt");

        let result = h
            .store()
            .put_if_match(&key, Bytes::from("data"), "etag")
            .await;

        // S3-compatible stores return 412 PreconditionFailed for non-existent objects,
        // while in-memory store returns NotFound. Both are acceptable.
        assert!(matches!(
            result,
            Err(StorageError::NotFound { .. }) | Err(StorageError::Conflict { .. })
        ));
    }

    pub async fn test_delete<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("to-delete.txt");

        h.store().put(&key, Bytes::from("data")).await.unwrap();
        assert!(h.store().exists(&key).await.unwrap());

        h.store().delete(&key).await.unwrap();
        assert!(!h.store().exists(&key).await.unwrap());
    }

    pub async fn test_delete_nonexistent<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("never-existed.txt");

        // Should not error
        h.store().delete(&key).await.unwrap();
    }

    pub async fn test_list<S: ObjectStore>(h: &TestHarness<S>) {
        // Use unique_key to get a base, all sub-keys will be cleaned via prefix listing
        let prefix = h.unique_key("list");

        h.store()
            .put(&format!("{prefix}/1.txt"), Bytes::from("1"))
            .await
            .unwrap();
        h.store()
            .put(&format!("{prefix}/2.txt"), Bytes::from("2"))
            .await
            .unwrap();
        h.store()
            .put(&format!("{prefix}-other/1.txt"), Bytes::from("3"))
            .await
            .unwrap();

        let mut keys = h.store().list(&format!("{prefix}/")).await.unwrap();
        keys.sort();

        assert_eq!(
            keys,
            vec![format!("{prefix}/1.txt"), format!("{}/2.txt", prefix)]
        );
    }

    pub async fn test_list_empty<S: ObjectStore>(h: &TestHarness<S>) {
        let prefix = h.unique_key("empty-list");

        let keys = h.store().list(&format!("{prefix}/")).await.unwrap();

        assert!(keys.is_empty());
    }

    pub async fn test_exists<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("exists-test.txt");

        assert!(!h.store().exists(&key).await.unwrap());

        h.store().put(&key, Bytes::from("data")).await.unwrap();

        assert!(h.store().exists(&key).await.unwrap());
    }

    pub async fn test_get_range_specific<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("range-specific.txt");
        h.store()
            .put(&key, Bytes::from("hello world"))
            .await
            .unwrap();

        // Bytes 0-4 inclusive = "hello"
        let data = h
            .store()
            .get_range(&key, ByteRange::Range { start: 0, end: 4 })
            .await
            .unwrap();
        assert_eq!(data, Bytes::from("hello"));

        // Bytes 6-10 inclusive = "world"
        let data = h
            .store()
            .get_range(&key, ByteRange::Range { start: 6, end: 10 })
            .await
            .unwrap();
        assert_eq!(data, Bytes::from("world"));
    }

    pub async fn test_get_range_from_offset<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("range-from.txt");
        h.store()
            .put(&key, Bytes::from("hello world"))
            .await
            .unwrap();

        // From byte 6 to end = "world"
        let data = h.store().get_range(&key, ByteRange::From(6)).await.unwrap();
        assert_eq!(data, Bytes::from("world"));
    }

    pub async fn test_get_range_suffix<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("range-suffix.txt");
        h.store()
            .put(&key, Bytes::from("hello world"))
            .await
            .unwrap();

        // Last 5 bytes = "world"
        let data = h
            .store()
            .get_range(&key, ByteRange::Suffix(5))
            .await
            .unwrap();
        assert_eq!(data, Bytes::from("world"));
    }

    pub async fn test_get_range_suffix_larger_than_object<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("range-suffix-large.txt");
        h.store().put(&key, Bytes::from("hello")).await.unwrap();

        // Request last 100 bytes, object is only 5 bytes - returns entire object
        let data = h
            .store()
            .get_range(&key, ByteRange::Suffix(100))
            .await
            .unwrap();
        assert_eq!(data, Bytes::from("hello"));
    }

    pub async fn test_get_range_clamps_end<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("range-clamp.txt");
        h.store().put(&key, Bytes::from("hello")).await.unwrap();

        // Request bytes 0-100, object is only 5 bytes - clamps to 0-4
        let data = h
            .store()
            .get_range(&key, ByteRange::Range { start: 0, end: 100 })
            .await
            .unwrap();
        assert_eq!(data, Bytes::from("hello"));
    }

    pub async fn test_get_range_invalid_start_greater_than_end<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("range-invalid-order.txt");
        h.store().put(&key, Bytes::from("hello")).await.unwrap();

        let result = h
            .store()
            .get_range(&key, ByteRange::Range { start: 5, end: 2 })
            .await;
        assert!(matches!(result, Err(StorageError::InvalidRange { .. })));
    }

    pub async fn test_get_range_invalid_start_beyond_object<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("range-invalid-start.txt");
        h.store().put(&key, Bytes::from("hello")).await.unwrap();

        let result = h
            .store()
            .get_range(&key, ByteRange::Range { start: 10, end: 15 })
            .await;
        assert!(matches!(result, Err(StorageError::InvalidRange { .. })));
    }

    pub async fn test_get_range_not_found<S: ObjectStore>(h: &TestHarness<S>) {
        let key = h.key("range-not-found.txt");

        let result = h
            .store()
            .get_range(&key, ByteRange::Range { start: 0, end: 10 })
            .await;
        assert!(matches!(result, Err(StorageError::NotFound { .. })));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Macro for generating test functions
// ─────────────────────────────────────────────────────────────────────────────

/// Macro to generate individual test functions for an ObjectStore implementation.
///
/// This generates `#[tokio::test]` functions that create a harness, run one test,
/// and clean up. This is useful when you want individual test isolation and
/// per-test reporting.
///
/// # Usage
///
/// ```ignore
/// use sybil_lib::storage::test_harness::object_store_test_suite;
///
/// object_store_test_suite!(my_tests, || async {
///     MyStore::new()
/// });
/// ```
#[macro_export]
macro_rules! object_store_test_suite {
    ($mod_name:ident, $store_factory:expr) => {
        #[cfg(test)]
        mod $mod_name {
            use super::*;
            use $crate::storage::test_harness::{object_store_tests, TestHarness};

            macro_rules! make_test {
                ($test_name:ident) => {
                    #[tokio::test]
                    async fn $test_name() {
                        let store = $store_factory.await;
                        let harness = TestHarness::new(store);
                        object_store_tests::$test_name(&harness).await;
                        harness.cleanup().await.unwrap();
                    }
                };
            }

            make_test!(test_put_and_get);
            make_test!(test_put_already_exists);
            make_test!(test_get_not_found);
            make_test!(test_get_with_etag);
            make_test!(test_put_if_match_success);
            make_test!(test_put_if_match_conflict);
            make_test!(test_put_if_match_not_found);
            make_test!(test_delete);
            make_test!(test_delete_nonexistent);
            make_test!(test_list);
            make_test!(test_list_empty);
            make_test!(test_exists);
            make_test!(test_get_range_specific);
            make_test!(test_get_range_from_offset);
            make_test!(test_get_range_suffix);
            make_test!(test_get_range_suffix_larger_than_object);
            make_test!(test_get_range_clamps_end);
            make_test!(test_get_range_invalid_start_greater_than_end);
            make_test!(test_get_range_invalid_start_beyond_object);
            make_test!(test_get_range_not_found);
        }
    };
}

pub use object_store_test_suite;
