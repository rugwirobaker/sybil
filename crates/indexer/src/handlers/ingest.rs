use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use tracing::instrument;

use lib::buffer::LogEntry;

use crate::error::IndexerError;
use crate::state::IndexerState;

#[derive(Deserialize)]
pub struct IngestRequest {
    logs: Vec<LogLine>,
}

#[derive(Deserialize)]
pub struct LogLine {
    /// Epoch milliseconds. Defaults to server time if omitted.
    timestamp: Option<i64>,
    /// The log payload (arbitrary JSON).
    payload: serde_json::Value,
}

#[derive(Serialize)]
pub struct IngestResponse {
    accepted: usize,
}

#[instrument(skip(state, request), fields(log_count))]
pub async fn ingest_logs(
    State(state): State<Arc<IndexerState>>,
    Path(namespace): Path<String>,
    Json(request): Json<IngestRequest>,
) -> Result<impl IntoResponse, IndexerError> {
    let log_count = request.logs.len();
    tracing::Span::current().record("log_count", log_count);

    if request.logs.is_empty() {
        tracing::debug!(namespace = %namespace, "empty ingest request");
        return Ok((StatusCode::ACCEPTED, Json(IngestResponse { accepted: 0 })));
    }

    tracing::debug!(namespace = %namespace, log_count, "processing ingest request");

    // Resolve namespace (cache hit or S3 fetch)
    let ns = state.resolve_namespace(&namespace).await?;

    // Convert to LogEntry
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut entries = Vec::with_capacity(request.logs.len());
    for log in &request.logs {
        let timestamp = log.timestamp.unwrap_or(now_ms);
        let payload = serde_json::to_vec(&log.payload)?;
        entries.push(LogEntry::new(timestamp, payload));
    }

    let count = entries.len();
    state.buffers.add_batch(&ns.id, entries)?;

    tracing::info!(namespace = %namespace, accepted = count, "logs ingested");

    Ok((
        StatusCode::ACCEPTED,
        Json(IngestResponse { accepted: count }),
    ))
}
