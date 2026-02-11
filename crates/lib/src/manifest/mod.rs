//! Manifest: Per-namespace segment registry.
//!
//! Each namespace has its own manifest tracking all segments. Uses the same
//! append-only log pattern as the catalog:
//!
//! ```text
//! /namespaces/{ns_id}/manifest/
//!   ├── head.json                    # Current state pointer
//!   ├── snapshot-{timestamp}.json    # Compacted segment list
//!   └── log/
//!       └── {ulid}.json              # Individual operations
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The manifest head pointer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestHead {
    pub version: u32,
    pub snapshot: String,
    pub log_tail: Vec<String>,
}

impl ManifestHead {
    pub fn new(snapshot: String) -> Self {
        Self {
            version: 1,
            snapshot,
            log_tail: Vec::new(),
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(data)
    }
}

/// A manifest snapshot containing the segment list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestSnapshot {
    pub created_at: DateTime<Utc>,
    pub segments: Vec<SegmentInfo>,
}

impl ManifestSnapshot {
    pub fn empty() -> Self {
        Self {
            created_at: Utc::now(),
            segments: Vec::new(),
        }
    }

    pub fn new(segments: Vec<SegmentInfo>) -> Self {
        Self {
            created_at: Utc::now(),
            segments,
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(data)
    }
}

/// Segment level in the LSM hierarchy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SegmentLevel {
    /// Fresh writes, 64-128KB, Tigris-inlined.
    L0 = 0,
    /// Hourly compacted, ~32MB.
    L1 = 1,
    /// Daily compacted, ~256MB.
    L2 = 2,
}

/// Segment lifecycle state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum SegmentState {
    /// Current, visible to queries.
    #[default]
    Active,
    /// Replaced by compaction, awaiting cleanup.
    Superseded,
}

/// Metadata about a segment stored in S3.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SegmentInfo {
    /// S3 object key for this segment.
    pub key: String,
    /// Segment level (L0, L1, L2).
    pub level: SegmentLevel,
    /// Minimum timestamp of logs in this segment (epoch millis).
    pub min_ts: i64,
    /// Maximum timestamp of logs in this segment (epoch millis).
    pub max_ts: i64,
    /// Number of log entries in this segment.
    pub log_count: u64,
    /// Compressed size in bytes.
    pub size_bytes: u64,
    /// Current state of the segment.
    #[serde(default)]
    pub state: SegmentState,
    /// When this segment was created.
    pub created_at: DateTime<Utc>,
    /// When this segment was superseded (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_at: Option<DateTime<Utc>>,
}

impl SegmentInfo {
    pub fn new(
        key: String,
        level: SegmentLevel,
        min_ts: i64,
        max_ts: i64,
        log_count: u64,
        size_bytes: u64,
    ) -> Self {
        Self {
            key,
            level,
            min_ts,
            max_ts,
            log_count,
            size_bytes,
            state: SegmentState::Active,
            created_at: Utc::now(),
            superseded_at: None,
        }
    }

    /// Check if this segment overlaps with a time range.
    pub fn overlaps(&self, start_ts: i64, end_ts: i64) -> bool {
        self.min_ts <= end_ts && self.max_ts >= start_ts
    }

    /// Check if this segment is active (visible to queries).
    pub fn is_active(&self) -> bool {
        self.state == SegmentState::Active
    }

    /// Mark this segment as superseded.
    pub fn supersede(&mut self) {
        self.state = SegmentState::Superseded;
        self.superseded_at = Some(Utc::now());
    }
}

/// A manifest log entry representing segment mutations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestLogEntry {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub actions: Vec<ManifestAction>,
}

impl ManifestLogEntry {
    pub fn new(actions: Vec<ManifestAction>) -> Self {
        Self {
            id: ulid::Ulid::new().to_string(),
            timestamp: Utc::now(),
            actions,
        }
    }

    /// Create a log entry that adds a single segment.
    pub fn add_segment(segment: SegmentInfo) -> Self {
        Self::new(vec![ManifestAction::Add { segment }])
    }

    /// Create a log entry for compaction: add new segment, supersede old ones.
    pub fn compact(new_segment: SegmentInfo, superseded_keys: Vec<String>) -> Self {
        let mut actions = vec![ManifestAction::Add {
            segment: new_segment,
        }];
        for key in superseded_keys {
            actions.push(ManifestAction::Supersede { key });
        }
        Self::new(actions)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(data)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ManifestAction {
    /// Add a new segment.
    Add { segment: SegmentInfo },
    /// Mark a segment as superseded.
    Supersede { key: String },
    /// Remove a segment from the manifest (after GC).
    Remove { key: String },
}

/// Computed manifest state after applying log entries to a snapshot.
#[derive(Debug, Clone, Default)]
pub struct ManifestState {
    segments: std::collections::HashMap<String, SegmentInfo>,
}

impl ManifestState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_snapshot_and_log(
        snapshot: &ManifestSnapshot,
        log_entries: &[ManifestLogEntry],
    ) -> Self {
        let mut state = Self::new();
        for seg in &snapshot.segments {
            state.segments.insert(seg.key.clone(), seg.clone());
        }
        for entry in log_entries {
            for action in &entry.actions {
                state.apply(action);
            }
        }
        state
    }

    pub fn apply(&mut self, action: &ManifestAction) {
        match action {
            ManifestAction::Add { segment } => {
                self.segments.insert(segment.key.clone(), segment.clone());
            }
            ManifestAction::Supersede { key } => {
                if let Some(seg) = self.segments.get_mut(key) {
                    seg.supersede();
                }
            }
            ManifestAction::Remove { key } => {
                self.segments.remove(key);
            }
        }
    }

    /// Get all active segments.
    pub fn active_segments(&self) -> impl Iterator<Item = &SegmentInfo> {
        self.segments.values().filter(|s| s.is_active())
    }

    /// Get active segments overlapping a time range.
    pub fn segments_in_range(&self, start_ts: i64, end_ts: i64) -> Vec<&SegmentInfo> {
        self.active_segments()
            .filter(|s| s.overlaps(start_ts, end_ts))
            .collect()
    }

    /// Get active segments at a specific level.
    pub fn segments_at_level(&self, level: SegmentLevel) -> impl Iterator<Item = &SegmentInfo> {
        self.active_segments().filter(move |s| s.level == level)
    }

    /// Get superseded segments ready for garbage collection.
    pub fn superseded_segments(&self) -> impl Iterator<Item = &SegmentInfo> {
        self.segments
            .values()
            .filter(|s| s.state == SegmentState::Superseded)
    }

    /// Get a segment by key.
    pub fn get(&self, key: &str) -> Option<&SegmentInfo> {
        self.segments.get(key)
    }

    pub fn len(&self) -> usize {
        self.segments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn to_snapshot(&self) -> ManifestSnapshot {
        ManifestSnapshot::new(self.segments.values().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_segment(key: &str, level: SegmentLevel, min_ts: i64, max_ts: i64) -> SegmentInfo {
        SegmentInfo::new(key.to_string(), level, min_ts, max_ts, 1000, 65536)
    }

    #[test]
    fn test_segment_overlaps() {
        let seg = make_segment("test.seg", SegmentLevel::L0, 100, 200);

        assert!(seg.overlaps(50, 150)); // overlaps start
        assert!(seg.overlaps(150, 250)); // overlaps end
        assert!(seg.overlaps(120, 180)); // contained
        assert!(seg.overlaps(50, 250)); // contains
        assert!(!seg.overlaps(0, 50)); // before
        assert!(!seg.overlaps(300, 400)); // after
    }

    #[test]
    fn test_manifest_head_serialization() {
        let head = ManifestHead::new("snapshot-2025-01-15T00:00:00Z.json".to_string());
        let bytes = head.to_bytes().unwrap();
        let parsed = ManifestHead::from_bytes(&bytes).unwrap();
        assert_eq!(head, parsed);
    }

    #[test]
    fn test_manifest_snapshot_serialization() {
        let seg = make_segment("test.seg", SegmentLevel::L0, 100, 200);
        let snapshot = ManifestSnapshot::new(vec![seg]);
        let bytes = snapshot.to_bytes().unwrap();
        let parsed = ManifestSnapshot::from_bytes(&bytes).unwrap();
        assert_eq!(snapshot.segments.len(), parsed.segments.len());
    }

    #[test]
    fn test_manifest_log_entry_compact() {
        let new_seg = make_segment("L1/2025-01-15/14.seg", SegmentLevel::L1, 100, 200);
        let entry = ManifestLogEntry::compact(
            new_seg,
            vec!["L0/14/001.seg".to_string(), "L0/14/002.seg".to_string()],
        );

        assert_eq!(entry.actions.len(), 3);
        assert!(matches!(&entry.actions[0], ManifestAction::Add { .. }));
        assert!(matches!(&entry.actions[1], ManifestAction::Supersede { .. }));
        assert!(matches!(&entry.actions[2], ManifestAction::Supersede { .. }));
    }

    #[test]
    fn test_manifest_state_compaction_flow() {
        // Start with two L0 segments
        let seg1 = make_segment("L0/14/001.seg", SegmentLevel::L0, 100, 150);
        let seg2 = make_segment("L0/14/002.seg", SegmentLevel::L0, 151, 200);
        let snapshot = ManifestSnapshot::new(vec![seg1.clone(), seg2.clone()]);

        // Compact into L1
        let l1_seg = make_segment("L1/2025-01-15/14.seg", SegmentLevel::L1, 100, 200);
        let log = vec![ManifestLogEntry::compact(
            l1_seg.clone(),
            vec![seg1.key.clone(), seg2.key.clone()],
        )];

        let state = ManifestState::from_snapshot_and_log(&snapshot, &log);

        // Should have 3 segments total, but only 1 active
        assert_eq!(state.len(), 3);
        assert_eq!(state.active_segments().count(), 1);
        assert_eq!(state.superseded_segments().count(), 2);

        // Active segment should be the L1
        let active: Vec<_> = state.active_segments().collect();
        assert_eq!(active[0].key, l1_seg.key);
    }

    #[test]
    fn test_manifest_state_segments_in_range() {
        let seg1 = make_segment("seg1", SegmentLevel::L0, 0, 100);
        let seg2 = make_segment("seg2", SegmentLevel::L0, 101, 200);
        let seg3 = make_segment("seg3", SegmentLevel::L0, 201, 300);
        let snapshot = ManifestSnapshot::new(vec![seg1, seg2, seg3]);
        let state = ManifestState::from_snapshot_and_log(&snapshot, &[]);

        let in_range = state.segments_in_range(50, 150);
        assert_eq!(in_range.len(), 2); // seg1 and seg2
    }
}
