//! Catalog: Global namespace registry.
//!
//! The catalog tracks namespace name ↔ ID mappings using an append-only log
//! pattern for S3-native atomicity:
//!
//! ```text
//! /metadata/
//!   ├── head.json                    # Current state pointer
//!   ├── snapshots/
//!   │   └── {timestamp}.json         # Compacted namespace list
//!   └── log/
//!       └── {ulid}.json              # Individual operations
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const VERSION: u32 = 1;

/// The catalog head pointer.
///
/// Points to the current snapshot and lists log entries to apply on top.
/// This is the only mutable file in the catalog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Head {
    pub version: u32,
    /// Path to the current snapshot (relative to metadata/).
    pub snapshot: String,
    /// Log entry IDs to apply on top of the snapshot.
    pub log_tail: Vec<String>,
}

impl Head {
    pub fn new(snapshot: impl Into<String>) -> Self {
        Self {
            version: VERSION,
            snapshot: snapshot.into(),
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

/// A catalog snapshot containing the complete namespace list.
/// Snapshots are immutable and can be cached indefinitely.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Snapshot {
    pub created_at: DateTime<Utc>,
    pub namespaces: Vec<Namespace>,
}

impl Snapshot {
    pub fn empty() -> Self {
        Self {
            created_at: Utc::now(),
            namespaces: Vec::new(),
        }
    }

    pub fn new(namespaces: Vec<Namespace>) -> Self {
        Self {
            created_at: Utc::now(),
            namespaces,
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(data)
    }
}

/// Information about a namespace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Namespace {
    /// Unique namespace ID (e.g., "ns_01JQXYZ123ABCDEFGHJKMNPQRS").
    pub id: String,
    /// Human-readable name used in SQL FROM clause.
    pub name: String,
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
    pub created_at: DateTime<Utc>,
}

fn default_retention_days() -> u32 {
    7
}

impl Namespace {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: generate_namespace_id(),
            name: name.into(),
            retention_days: 7,
            created_at: Utc::now(),
        }
    }

    pub fn with_retention(name: impl Into<String>, retention_days: u32) -> Self {
        Self {
            id: generate_namespace_id(),
            name: name.into(),
            retention_days,
            created_at: Utc::now(),
        }
    }
}

pub fn generate_namespace_id() -> String {
    format!("ns_{}", ulid::Ulid::new())
}

/// A catalog log entry representing a single mutation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LongEntry {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub action: Action,
}

impl LongEntry {
    pub fn add(namespace: Namespace) -> Self {
        Self {
            id: ulid::Ulid::new().to_string(),
            timestamp: Utc::now(),
            action: Action::Add { namespace },
        }
    }

    pub fn remove(namespace_id: impl Into<String>) -> Self {
        Self {
            id: ulid::Ulid::new().to_string(),
            timestamp: Utc::now(),
            action: Action::Remove {
                namespace_id: namespace_id.into(),
            },
        }
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
pub enum Action {
    Add { namespace: Namespace },
    Remove { namespace_id: String },
}

/// Computed catalog state after applying log entries to a snapshot.
#[derive(Debug, Clone, Default)]
pub struct State {
    namespaces_by_name: HashMap<String, Namespace>,
    namespaces_by_id: HashMap<String, Namespace>,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_snapshot_and_log(snapshot: &Snapshot, log_entries: &[LongEntry]) -> Self {
        let mut state = Self::new();
        for ns in &snapshot.namespaces {
            state.insert(ns.clone());
        }
        for entry in log_entries {
            state.apply(&entry.action);
        }
        state
    }

    pub fn apply(&mut self, action: &Action) {
        match action {
            Action::Add { namespace } => self.insert(namespace.clone()),
            Action::Remove { namespace_id } => self.remove(namespace_id),
        }
    }

    fn insert(&mut self, namespace: Namespace) {
        self.namespaces_by_name
            .insert(namespace.name.clone(), namespace.clone());
        self.namespaces_by_id
            .insert(namespace.id.clone(), namespace);
    }

    fn remove(&mut self, namespace_id: &str) {
        if let Some(ns) = self.namespaces_by_id.remove(namespace_id) {
            self.namespaces_by_name.remove(&ns.name);
        }
    }

    pub fn get_by_name(&self, name: &str) -> Option<&Namespace> {
        self.namespaces_by_name.get(name)
    }

    pub fn get_by_id(&self, id: &str) -> Option<&Namespace> {
        self.namespaces_by_id.get(id)
    }

    pub fn list(&self) -> impl Iterator<Item = &Namespace> {
        self.namespaces_by_name.values()
    }

    pub fn len(&self) -> usize {
        self.namespaces_by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.namespaces_by_name.is_empty()
    }

    pub fn to_snapshot(&self) -> Snapshot {
        Snapshot::new(self.namespaces_by_name.values().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_namespace_info_creation() {
        let ns = Namespace::new("api_gateway");
        assert!(ns.id.starts_with("ns_"));
        assert_eq!(ns.name, "api_gateway");
        assert_eq!(ns.retention_days, 7);
    }

    #[test]
    fn test_catalog_head_serialization() {
        let head = Head::new("snapshots/2025-01-15T00:00:00Z.json".to_string());
        let bytes = head.to_bytes().unwrap();
        let parsed = Head::from_bytes(&bytes).unwrap();
        assert_eq!(head, parsed);
    }

    #[test]
    fn test_catalog_snapshot_serialization() {
        let ns = Namespace::new("test_namespace");
        let snapshot = Snapshot::new(vec![ns.clone()]);
        let bytes = snapshot.to_bytes().unwrap();
        let parsed = Snapshot::from_bytes(&bytes).unwrap();
        assert_eq!(snapshot.namespaces.len(), parsed.namespaces.len());
        assert_eq!(snapshot.namespaces[0].name, parsed.namespaces[0].name);
    }

    #[test]
    fn test_catalog_log_entry_add() {
        let ns = Namespace::new("my_app");
        let entry = LongEntry::add(ns.clone());
        let bytes = entry.to_bytes().unwrap();
        let parsed = LongEntry::from_bytes(&bytes).unwrap();

        match parsed.action {
            Action::Add { namespace } => assert_eq!(namespace.name, "my_app"),
            _ => panic!("Expected Add action"),
        }
    }

    #[test]
    fn test_catalog_log_entry_remove() {
        let entry = LongEntry::remove("ns_01JQXYZ123");
        let bytes = entry.to_bytes().unwrap();
        let parsed = LongEntry::from_bytes(&bytes).unwrap();

        match parsed.action {
            Action::Remove { namespace_id } => assert_eq!(namespace_id, "ns_01JQXYZ123"),
            _ => panic!("Expected Remove action"),
        }
    }

    #[test]
    fn test_catalog_state_from_snapshot_and_log() {
        let ns1 = Namespace::new("service_a");
        let snapshot = Snapshot::new(vec![ns1.clone()]);

        let ns2 = Namespace::new("service_b");
        let log = vec![
            LongEntry::add(ns2.clone()),
            LongEntry::remove(ns1.id.clone()),
        ];

        let state = State::from_snapshot_and_log(&snapshot, &log);

        assert_eq!(state.len(), 1);
        assert!(state.get_by_name("service_a").is_none());
        assert!(state.get_by_name("service_b").is_some());
    }

    #[test]
    fn test_catalog_state_to_snapshot() {
        let mut state = State::new();
        let ns = Namespace::new("my_service");
        state.apply(&Action::Add {
            namespace: ns.clone(),
        });

        let snapshot = state.to_snapshot();
        assert_eq!(snapshot.namespaces.len(), 1);
        assert_eq!(snapshot.namespaces[0].name, "my_service");
    }
}
