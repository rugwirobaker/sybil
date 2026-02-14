//! Background flusher: drains namespace buffers and writes L0 segments to S3.
//!
//! The flusher runs on a timer, checks which namespace buffers are ready
//! (by size, count, or age), and for each one:
//!
//! 1. Drains the memtable into a segment (destructive)
//! 2. PUTs the segment to S3 (immutable)
//! 3. PUTs a manifest log entry (immutable)
//! 4. Updates manifest head.json (optimistic locking, retry on conflict)
//! 5. Invalidates the manifest cache

use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::Semaphore;
use tokio::time;

use lib::manifest;
use lib::storage::{ObjectKey, StorageError};

use crate::state::IndexerState;

/// Maximum concurrent flush tasks.
const MAX_CONCURRENT_FLUSHES: usize = 4;

#[derive(Debug, thiserror::Error)]
enum FlushError {
    #[error("buffer error: {0}")]
    Buffer(#[from] lib::buffer::BufferError),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("internal error: {0}")]
    Internal(String),
}

/// Run the flusher loop.
///
/// Periodically checks all namespace buffers for flush readiness and spawns
/// tasks to flush them to S3.
pub async fn flusher_loop(state: Arc<IndexerState>) {
    let interval_secs = state.config.indexer.buffer.flush_interval_secs;
    let mut interval = time::interval(time::Duration::from_secs(interval_secs));
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_FLUSHES));

    tracing::info!(interval_secs, "flusher loop started");

    loop {
        interval.tick().await;

        let ready = state.buffers.ready_for_flush();
        if ready.is_empty() {
            continue;
        }

        tracing::debug!(count = ready.len(), "namespaces ready for flush");

        for (ns_id, reason) in ready {
            let state = Arc::clone(&state);
            let semaphore = Arc::clone(&semaphore);

            tokio::spawn(async move {
                let _permit = semaphore.acquire().await.expect("semaphore closed");
                tracing::debug!(namespace = %ns_id, reason = ?reason, "flushing namespace");

                if let Err(e) = flush_namespace(&state, &ns_id).await {
                    tracing::error!(namespace = %ns_id, error = %e, "flush failed");
                }
            });
        }
    }
}

/// Flush a single namespace: drain buffer, write segment and manifest to S3.
async fn flush_namespace(state: &IndexerState, ns_id: &str) -> Result<(), FlushError> {
    // 1. Drain the buffer
    let drain_result = match state.buffers.drain(ns_id)? {
        Some(result) => result,
        None => return Ok(()), // Buffer was empty (race with another flush)
    };

    let segment_bytes = drain_result.segment_bytes;
    let meta = drain_result.meta;

    tracing::info!(
        namespace = %ns_id,
        entries = meta.entry_count,
        size = segment_bytes.len(),
        min_ts = meta.min_timestamp,
        max_ts = meta.max_timestamp,
        "drained buffer"
    );

    // Derive hour bucket from min_timestamp for S3 key partitioning
    let hour = chrono::DateTime::from_timestamp_millis(meta.min_timestamp)
        .map(|dt| dt.format("%Y-%m-%d/%H").to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let segment_id = ulid::Ulid::new().to_string();
    let segment_key = ObjectKey::segment_l0(ns_id, &hour, &segment_id);

    // 2. PUT segment to S3 (immutable, PutMode::Create)
    state
        .storage
        .put(&segment_key, Bytes::from(segment_bytes))
        .await?;

    // 3. PUT manifest log entry to S3 (immutable)
    let segment_info = manifest::SegmentInfo::new(
        segment_key.clone(),
        manifest::SegmentLevel::L0,
        meta.min_timestamp,
        meta.max_timestamp,
        meta.entry_count as u64,
        meta.size_bytes as u64,
    );

    let log_entry = manifest::LongEntry::add_segment(segment_info);
    let log_key = ObjectKey::manifest_log_entry(ns_id, &log_entry.id);
    let log_bytes = log_entry
        .to_bytes()
        .map_err(|e| FlushError::Internal(e.to_string()))?;

    state.storage.put(&log_key, Bytes::from(log_bytes)).await?;

    // 4. Update manifest head.json (optimistic locking with retry)
    update_manifest_head(state, ns_id, &log_entry.id).await?;

    // 5. Invalidate manifest cache
    state.cache.invalidate_namespace(ns_id);

    tracing::info!(
        namespace = %ns_id,
        segment = %segment_key,
        "flush complete"
    );

    Ok(())
}

/// Update the manifest head.json with the new log entry ID.
///
/// Uses optimistic locking (ETag) with retry on conflict.
async fn update_manifest_head(
    state: &IndexerState,
    ns_id: &str,
    entry_id: &str,
) -> Result<(), FlushError> {
    let head_key = ObjectKey::manifest_head(ns_id);

    loop {
        let (head_bytes, etag) = state.storage.get_with_etag(&head_key).await?;

        let mut head = manifest::Head::from_bytes(&head_bytes)
            .map_err(|e| FlushError::Internal(e.to_string()))?;

        head.log_tail.push(entry_id.to_string());

        let new_head_bytes = head
            .to_bytes()
            .map_err(|e| FlushError::Internal(e.to_string()))?;

        match state
            .storage
            .put_if_match(&head_key, Bytes::from(new_head_bytes), &etag)
            .await
        {
            Ok(()) => return Ok(()),
            Err(StorageError::Conflict { .. }) => {
                tracing::debug!(namespace = %ns_id, "manifest head conflict, retrying");
                continue;
            }
            Err(e) => return Err(FlushError::Storage(e)),
        }
    }
}
