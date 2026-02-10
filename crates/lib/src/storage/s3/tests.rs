//! S3Store tests.
//!
//! Includes both unit tests and integration tests using Tigris (S3-compatible).
//! Integration tests require a `.tigris.creds` file in the workspace root.

use super::*;
use crate::storage::engine::ObjectStore;
use crate::storage::test_harness::{object_store_tests, TestHarness};

use std::collections::HashMap;
use std::path::Path;

#[test]
fn test_new_builds_store() {
    let result = S3Store::new(
        "test-bucket",
        "us-east-1",
        Some("http://localhost:9000"),
        "fake-key",
        "fake-secret",
    );

    // Should succeed in building (connection happens lazily)
    assert!(result.is_ok());
}

/// Configuration for Tigris S3-compatible storage.
struct TigrisConfig {
    endpoint: String,
    region: String,
    access_key: String,
    secret_key: String,
    bucket: String,
}

impl TigrisConfig {
    /// Load configuration from `.tigris.creds` file.
    /// Format: KEY: value (one per line)
    fn from_creds_file(path: &Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;

        let creds: HashMap<&str, &str> = content
            .lines()
            .filter_map(|line| {
                let mut parts = line.splitn(2, ':');
                let key = parts.next()?.trim();
                let value = parts.next()?.trim();
                Some((key, value))
            })
            .collect();

        Some(Self {
            endpoint: creds.get("AWS_ENDPOINT_URL_S3")?.to_string(),
            region: creds.get("AWS_REGION")?.to_string(),
            access_key: creds.get("AWS_ACCESS_KEY_ID")?.to_string(),
            secret_key: creds.get("AWS_SECRET_ACCESS_KEY")?.to_string(),
            bucket: creds.get("BUCKET_NAME")?.to_string(),
        })
    }
}

/// Create an S3Store connected to Tigris.
fn create_store(config: &TigrisConfig) -> S3Store {
    S3Store::new(
        &config.bucket,
        &config.region,
        Some(&config.endpoint),
        &config.access_key,
        &config.secret_key,
    )
    .expect("failed to create S3Store")
}

/// Find the workspace root by looking for Cargo.toml with [workspace].
fn find_workspace_root() -> Option<std::path::PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let content = std::fs::read_to_string(&cargo_toml).ok()?;
            if content.contains("[workspace]") {
                return Some(dir);
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Clean up stale test data from previous runs that may have failed.
async fn cleanup_stale_test_data(store: &S3Store) {
    // List all objects with _test_ prefix and delete them
    if let Ok(keys) = store.list("_test_").await {
        for key in keys {
            let _ = store.delete(&key).await;
        }
    }
}

#[tokio::test]
async fn s3_conformance() {
    let workspace_root = match find_workspace_root() {
        Some(r) => r,
        None => {
            eprintln!("Skipping S3 integration test: could not find workspace root");
            return;
        }
    };

    let creds_path = workspace_root.join(".tigris.creds");
    let config = match TigrisConfig::from_creds_file(&creds_path) {
        Some(c) => c,
        None => {
            eprintln!("Skipping S3 integration test: .tigris.creds not found or invalid");
            eprintln!("Create .tigris.creds with: AWS_ENDPOINT_URL_S3, AWS_REGION, AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, BUCKET_NAME");
            return;
        }
    };

    let store = create_store(&config);

    // Clean up any stale test data from previous failed runs
    cleanup_stale_test_data(&store).await;

    let harness = TestHarness::new(store);

    object_store_tests::run_all(&harness).await;

    harness.cleanup().await.expect("cleanup failed");
}
