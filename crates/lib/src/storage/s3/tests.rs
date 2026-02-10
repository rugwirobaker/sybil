//! S3Store tests.
//!
//! Includes both unit tests and integration tests using rustfs via testcontainers.
//! Integration tests require Docker to be running.

use super::*;
use crate::storage::test_harness::{object_store_tests, TestHarness};

use std::time::Duration;
use testcontainers::core::{ContainerPort, ContainerRequest, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt};

// ─────────────────────────────────────────────────────────────────────────────
// Unit tests
// ─────────────────────────────────────────────────────────────────────────────

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

// ─────────────────────────────────────────────────────────────────────────────
// Integration tests (require Docker)
// ─────────────────────────────────────────────────────────────────────────────

/// Default credentials for rustfs.
const RUSTFS_USER: &str = "rustfsadmin";
const RUSTFS_PASSWORD: &str = "rustfsadmin";
const RUSTFS_BUCKET: &str = "test-bucket";

/// Create a rustfs container image configured for testing.
fn rustfs_image() -> ContainerRequest<GenericImage> {
    GenericImage::new("rustfs/rustfs", "latest")
        .with_wait_for(WaitFor::message_on_stderr("API:"))
        .with_exposed_port(ContainerPort::Tcp(9000))
        .with_env_var("RUSTFS_ROOT_USER", RUSTFS_USER)
        .with_env_var("RUSTFS_ROOT_PASSWORD", RUSTFS_PASSWORD)
}

/// Create an S3Store connected to the rustfs container.
fn create_store(host_port: u16) -> S3Store {
    let endpoint = format!("http://127.0.0.1:{}", host_port);

    S3Store::new(
        RUSTFS_BUCKET,
        "us-east-1",
        Some(&endpoint),
        RUSTFS_USER,
        RUSTFS_PASSWORD,
    )
    .expect("failed to create S3Store")
}

/// Create a bucket in rustfs (auto-created on first write).
async fn ensure_bucket(host_port: u16) {
    use object_store::aws::AmazonS3Builder;
    use object_store::ObjectStore as _;

    let endpoint = format!("http://127.0.0.1:{}", host_port);

    let store = AmazonS3Builder::new()
        .with_endpoint(&endpoint)
        .with_bucket_name(RUSTFS_BUCKET)
        .with_region("us-east-1")
        .with_access_key_id(RUSTFS_USER)
        .with_secret_access_key(RUSTFS_PASSWORD)
        .with_allow_http(true)
        .build()
        .expect("failed to build store for bucket creation");

    // Put a dummy object to implicitly create the bucket
    let _ = store
        .put(
            &object_store::path::Path::from("_init"),
            bytes::Bytes::new().into(),
        )
        .await;

    // Clean it up
    let _ = store
        .delete(&object_store::path::Path::from("_init"))
        .await;
}

#[tokio::test]
async fn s3_conformance() {
    // Start rustfs container
    let container = match rustfs_image().start().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Skipping S3 integration test: failed to start container: {e}");
            eprintln!("Is Docker running?");
            return;
        }
    };

    let host_port = container
        .get_host_port_ipv4(9000)
        .await
        .expect("failed to get host port");

    // Give rustfs a moment to fully initialize
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Ensure the bucket exists
    ensure_bucket(host_port).await;

    // Create store and run conformance tests
    let store = create_store(host_port);
    let harness = TestHarness::new(store);

    object_store_tests::run_all(&harness).await;

    harness.cleanup().await.expect("cleanup failed");
}
