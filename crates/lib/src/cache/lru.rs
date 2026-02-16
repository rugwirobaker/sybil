//! Time-biased LRU cache implementation.
//!
//! This LRU implementation favors both recently accessed entries and entries
//! with recent timestamps (e.g., recent segments are more likely to be queried).
//!
//! # Current Implementation
//!
//! Uses a simple `parking_lot::RwLock` around the entire structure. This is
//! correct and simple, but `get()` requires a write lock to update access time.
//!
//! # Future Optimization Options
//!
//! ## 1. Lock-Sharded
//! ```text
//! struct ShardedLru<K, V, const N: usize = 16> {
//!     shards: [RwLock<LruShard<K, V>>; N],
//! }
//! // shard = hash(key) % N
//! ```
//! - Reduces contention by factor of N
//! - Each shard is independent, correct LRU within shard
//! - Global size limit becomes approximate (sum of shard limits)
//! - Good balance of correctness and performance
//!
//! ## 2. Lock-Free with Deferred Ordering
//! ```text
//! struct LockFreeLru<K, V> {
//!     entries: DashMap<K, (V, AtomicU64)>,  // AtomicU64 = last access timestamp
//!     size_bytes: AtomicU64,
//! }
//! ```
//! - True parallel reads with no blocking
//! - `get()` just does an atomic store for access time
//! - Eviction requires O(n) scan or sampling (probabilistic, like Redis)
//! - Best read throughput, approximate LRU
//!
//! ## 3. Thread-Local Cache + Shared Backing
//! ```text
//! struct TlsLru<K, V> {
//!     local: ThreadLocal<RefCell<SmallLru<K, V>>>,  // small, per-thread
//!     shared: Arc<SharedLru<K, V>>,                  // larger, shared
//! }
//! ```
//! - Hot entries stay in thread-local cache (no locks at all)
//! - Misses fall through to shared cache
//! - Excellent for skewed access patterns (some keys much hotter)
//! - Complexity: promotion/demotion between layers, memory overhead
//!
//! ## Recommendation by Use Case
//!
//! | Cache Layer | Recommendation |
//! |-------------|----------------|
//! | Segment (few entries, large) | Simple lock (current) |
//! | Block (many entries, hot path) | Lock-free or thread-local |
//! | Bloom/Manifest (read-heavy) | Lock-sharded or lock-free |

use std::collections::HashMap;
use std::hash::Hash;
use std::time::Instant;

use parking_lot::RwLock;

/// Metadata stored for each cache entry.
#[derive(Debug, Clone)]
pub struct EntryMeta {
    /// When this entry was last accessed.
    pub last_access: Instant,
    /// Size of this entry in bytes.
    pub size_bytes: u64,
    /// Minimum timestamp in the data (for time-biased eviction).
    pub min_timestamp: i64,
}

impl EntryMeta {
    /// Create new metadata with the given size and timestamp.
    pub fn new(size_bytes: u64, min_timestamp: i64) -> Self {
        Self {
            last_access: Instant::now(),
            size_bytes,
            min_timestamp,
        }
    }

    /// Update the last access time.
    pub fn touch(&mut self) {
        self.last_access = Instant::now();
    }
}

/// Internal state protected by the lock.
struct LruInner<K, V> {
    entries: HashMap<K, (V, EntryMeta)>,
    current_size_bytes: u64,
}

/// A time-biased LRU cache with interior mutability.
///
/// Eviction score combines recency of access with recency of the data itself.
/// This is useful for log storage where recent logs are queried more often.
pub struct TimeBiasedLru<K, V> {
    inner: RwLock<LruInner<K, V>>,
    max_size_bytes: u64,
    time_bias: f64,
}

impl<K: Eq + Hash + Clone, V: Clone> TimeBiasedLru<K, V> {
    /// Create a new time-biased LRU cache.
    ///
    /// # Arguments
    /// * `max_size_bytes` - Maximum total size in bytes
    /// * `time_bias` - Weight for timestamp recency (0.0-1.0), higher favors recent data
    pub fn new(max_size_bytes: u64, time_bias: f64) -> Self {
        Self {
            inner: RwLock::new(LruInner {
                entries: HashMap::new(),
                current_size_bytes: 0,
            }),
            max_size_bytes,
            time_bias: time_bias.clamp(0.0, 1.0),
        }
    }

    /// Get an entry, updating its access time. Returns a clone of the value.
    pub fn get(&self, key: &K) -> Option<V> {
        let mut inner = self.inner.write();
        if let Some((value, meta)) = inner.entries.get_mut(key) {
            meta.touch();
            Some(value.clone())
        } else {
            None
        }
    }

    /// Get an entry without updating access time. Returns a clone of the value.
    pub fn peek(&self, key: &K) -> Option<V> {
        let inner = self.inner.read();
        inner.entries.get(key).map(|(v, _)| v.clone())
    }

    /// Check if a key exists.
    pub fn contains(&self, key: &K) -> bool {
        let inner = self.inner.read();
        inner.entries.contains_key(key)
    }

    /// Insert an entry, evicting if necessary.
    ///
    /// Returns the evicted entries.
    pub fn insert(&self, key: K, value: V, meta: EntryMeta) -> Vec<(K, V)> {
        let mut inner = self.inner.write();
        let mut evicted = Vec::new();

        // Remove existing entry if present
        if let Some((old_value, old_meta)) = inner.entries.remove(&key) {
            inner.current_size_bytes = inner.current_size_bytes.saturating_sub(old_meta.size_bytes);
            evicted.push((key.clone(), old_value));
        }

        // Evict entries until we have room
        while inner.current_size_bytes + meta.size_bytes > self.max_size_bytes
            && !inner.entries.is_empty()
        {
            if let Some((evict_key, evict_value)) = self.evict_one_inner(&mut inner) {
                evicted.push((evict_key, evict_value));
            }
        }

        // Insert the new entry
        inner.current_size_bytes += meta.size_bytes;
        inner.entries.insert(key, (value, meta));

        evicted
    }

    /// Remove an entry.
    pub fn remove(&self, key: &K) -> Option<V> {
        let mut inner = self.inner.write();
        if let Some((value, meta)) = inner.entries.remove(key) {
            inner.current_size_bytes = inner.current_size_bytes.saturating_sub(meta.size_bytes);
            Some(value)
        } else {
            None
        }
    }

    /// Evict the entry with the lowest score.
    fn evict_one_inner(&self, inner: &mut LruInner<K, V>) -> Option<(K, V)> {
        let now = Instant::now();
        let current_ts = chrono::Utc::now().timestamp_millis();

        // Find entry with lowest score
        let evict_key = inner
            .entries
            .iter()
            .map(|(k, (_, meta))| {
                let score = self.compute_score(meta, now, current_ts);
                (k.clone(), score)
            })
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(k, _)| k);

        if let Some(key) = evict_key {
            if let Some((value, meta)) = inner.entries.remove(&key) {
                inner.current_size_bytes = inner.current_size_bytes.saturating_sub(meta.size_bytes);
                return Some((key, value));
            }
        }
        None
    }

    /// Compute eviction score for an entry.
    ///
    /// Higher score = less likely to be evicted.
    /// Score = access_recency * (1 - time_bias) + timestamp_recency * time_bias
    fn compute_score(&self, meta: &EntryMeta, now: Instant, current_ts: i64) -> f64 {
        // Access recency: 1.0 for just accessed, decays over time
        let access_age_secs = now.duration_since(meta.last_access).as_secs_f64();
        let access_recency = 1.0 / (1.0 + access_age_secs / 60.0); // Half-life of 1 minute

        // Timestamp recency: 1.0 for current time, decays for older data
        let age_hours = (current_ts - meta.min_timestamp) as f64 / 3_600_000.0;
        let timestamp_recency = 1.0 / (1.0 + age_hours / 24.0); // Half-life of 1 day

        // Combine scores
        access_recency * (1.0 - self.time_bias) + timestamp_recency * self.time_bias
    }

    /// Get current size in bytes.
    pub fn size_bytes(&self) -> u64 {
        self.inner.read().current_size_bytes
    }

    /// Get current entry count.
    pub fn len(&self) -> usize {
        self.inner.read().entries.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.inner.read().entries.is_empty()
    }

    /// Get maximum size in bytes.
    pub fn max_size_bytes(&self) -> u64 {
        self.max_size_bytes
    }

    /// Clear all entries.
    pub fn clear(&self) {
        let mut inner = self.inner.write();
        inner.entries.clear();
        inner.current_size_bytes = 0;
    }

    /// Get all keys (snapshot).
    pub fn keys(&self) -> Vec<K> {
        self.inner.read().entries.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_operations() {
        let now = chrono::Utc::now().timestamp_millis();
        let lru: TimeBiasedLru<String, Vec<u8>> = TimeBiasedLru::new(1000, 0.0);

        let evicted = lru.insert("key1".to_string(), vec![1, 2, 3], EntryMeta::new(100, now));
        assert!(evicted.is_empty());
        assert_eq!(lru.len(), 1);
        assert_eq!(lru.size_bytes(), 100);

        assert!(lru.get(&"key1".to_string()).is_some());
        assert!(lru.get(&"key2".to_string()).is_none());
    }

    #[test]
    fn test_eviction() {
        let now = chrono::Utc::now().timestamp_millis();
        let lru: TimeBiasedLru<String, Vec<u8>> = TimeBiasedLru::new(200, 0.0);

        lru.insert("key1".to_string(), vec![1], EntryMeta::new(100, now));
        lru.insert("key2".to_string(), vec![2], EntryMeta::new(100, now));

        // This should evict key1 (oldest accessed)
        let evicted = lru.insert("key3".to_string(), vec![3], EntryMeta::new(100, now));

        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].0, "key1");
        assert_eq!(lru.len(), 2);
        assert!(lru.contains(&"key2".to_string()));
        assert!(lru.contains(&"key3".to_string()));
        assert!(!lru.contains(&"key1".to_string()));
    }

    #[test]
    fn test_remove() {
        let now = chrono::Utc::now().timestamp_millis();
        let lru: TimeBiasedLru<String, Vec<u8>> = TimeBiasedLru::new(1000, 0.0);

        lru.insert("key1".to_string(), vec![1, 2, 3], EntryMeta::new(100, now));
        assert_eq!(lru.size_bytes(), 100);

        let removed = lru.remove(&"key1".to_string());
        assert!(removed.is_some());
        assert_eq!(lru.size_bytes(), 0);
        assert_eq!(lru.len(), 0);
    }

    #[test]
    fn test_update_existing() {
        let now = chrono::Utc::now().timestamp_millis();
        let lru: TimeBiasedLru<String, Vec<u8>> = TimeBiasedLru::new(1000, 0.0);

        lru.insert("key1".to_string(), vec![1], EntryMeta::new(100, now));
        assert_eq!(lru.size_bytes(), 100);

        // Update with larger value
        let evicted = lru.insert("key1".to_string(), vec![1, 2, 3], EntryMeta::new(200, now));
        assert_eq!(evicted.len(), 1); // Old value evicted
        assert_eq!(lru.size_bytes(), 200);
        assert_eq!(lru.len(), 1);
    }

    #[test]
    fn test_time_biased_eviction() {
        let lru: TimeBiasedLru<String, Vec<u8>> = TimeBiasedLru::new(200, 0.8);

        // Insert old data first
        let old_ts = chrono::Utc::now().timestamp_millis() - 86_400_000; // 1 day ago
        lru.insert("old".to_string(), vec![1], EntryMeta::new(100, old_ts));

        // Insert recent data
        let recent_ts = chrono::Utc::now().timestamp_millis();
        lru.insert(
            "recent".to_string(),
            vec![2],
            EntryMeta::new(100, recent_ts),
        );

        // This should evict "old" due to time bias
        let evicted = lru.insert("new".to_string(), vec![3], EntryMeta::new(100, recent_ts));

        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].0, "old");
    }

    #[test]
    fn test_clear() {
        let now = chrono::Utc::now().timestamp_millis();
        let lru: TimeBiasedLru<String, Vec<u8>> = TimeBiasedLru::new(1000, 0.0);

        lru.insert("key1".to_string(), vec![1], EntryMeta::new(100, now));
        lru.insert("key2".to_string(), vec![2], EntryMeta::new(100, now));

        lru.clear();

        assert!(lru.is_empty());
        assert_eq!(lru.size_bytes(), 0);
    }

    #[test]
    fn test_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let now = chrono::Utc::now().timestamp_millis();
        let lru = Arc::new(TimeBiasedLru::<u32, u32>::new(10000, 0.0));

        let handles: Vec<_> = (0..4)
            .map(|t| {
                let lru = Arc::clone(&lru);
                thread::spawn(move || {
                    for i in 0..100 {
                        let key = t * 100 + i;
                        lru.insert(key, key, EntryMeta::new(10, now));
                        lru.get(&key);
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // Should have some entries (exact count depends on eviction)
        assert!(lru.is_empty());
    }

    #[test]
    fn test_peek_does_not_update_access_time() {
        let now = chrono::Utc::now().timestamp_millis();
        let lru: TimeBiasedLru<String, Vec<u8>> = TimeBiasedLru::new(200, 0.0);

        lru.insert("key1".to_string(), vec![1], EntryMeta::new(100, now));
        std::thread::sleep(std::time::Duration::from_millis(10));
        lru.insert("key2".to_string(), vec![2], EntryMeta::new(100, now));

        // Peek at key1 - should not update its access time
        let val = lru.peek(&"key1".to_string());
        assert_eq!(val, Some(vec![1]));

        // key1 should still be evicted first since peek didn't update access time
        let evicted = lru.insert("key3".to_string(), vec![3], EntryMeta::new(100, now));

        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].0, "key1");
    }

    #[test]
    fn test_contains() {
        let now = chrono::Utc::now().timestamp_millis();
        let lru: TimeBiasedLru<String, Vec<u8>> = TimeBiasedLru::new(1000, 0.0);

        assert!(!lru.contains(&"key1".to_string()));

        lru.insert("key1".to_string(), vec![1], EntryMeta::new(100, now));

        assert!(lru.contains(&"key1".to_string()));
        assert!(!lru.contains(&"key2".to_string()));

        lru.remove(&"key1".to_string());

        assert!(!lru.contains(&"key1".to_string()));
    }

    #[test]
    fn test_max_size_bytes() {
        let now = chrono::Utc::now().timestamp_millis();
        let lru: TimeBiasedLru<String, Vec<u8>> = TimeBiasedLru::new(500, 0.0);

        assert_eq!(lru.max_size_bytes(), 500);

        // Verify eviction respects max_size_bytes
        lru.insert("key1".to_string(), vec![1], EntryMeta::new(200, now));
        lru.insert("key2".to_string(), vec![2], EntryMeta::new(200, now));

        assert!(lru.size_bytes() <= lru.max_size_bytes());

        // Adding more should trigger eviction to stay under max
        lru.insert("key3".to_string(), vec![3], EntryMeta::new(200, now));

        assert!(lru.size_bytes() <= lru.max_size_bytes());
        assert_eq!(lru.len(), 2); // Only 2 entries fit
    }
}
