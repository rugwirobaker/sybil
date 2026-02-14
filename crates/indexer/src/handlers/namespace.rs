use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::error::IndexerError;
use crate::state::IndexerState;

#[derive(Deserialize)]
pub struct CreateNamespaceRequest {
    name: String,
    #[serde(default = "default_retention")]
    retention_days: u32,
}

fn default_retention() -> u32 {
    7
}

#[derive(Serialize)]
pub struct NamespaceResponse {
    id: String,
    name: String,
    retention_days: u32,
    created_at: DateTime<Utc>,
}

impl From<lib::catalog::Namespace> for NamespaceResponse {
    fn from(ns: lib::catalog::Namespace) -> Self {
        Self {
            id: ns.id,
            name: ns.name,
            retention_days: ns.retention_days,
            created_at: ns.created_at,
        }
    }
}

#[instrument(skip(state, request), fields(namespace = %request.name))]
pub async fn create_namespace(
    State(state): State<Arc<IndexerState>>,
    Json(request): Json<CreateNamespaceRequest>,
) -> Result<impl IntoResponse, IndexerError> {
    let ns = state
        .create_namespace(&request.name, request.retention_days)
        .await?;

    Ok((StatusCode::CREATED, Json(NamespaceResponse::from(ns))))
}

#[instrument(skip(state))]
pub async fn list_namespaces(
    State(state): State<Arc<IndexerState>>,
) -> Result<Json<Vec<NamespaceResponse>>, IndexerError> {
    let catalog_state = state.ensure_catalog().await?;
    let namespaces: Vec<NamespaceResponse> = catalog_state
        .list()
        .cloned()
        .map(NamespaceResponse::from)
        .collect();
    tracing::debug!(count = namespaces.len(), "listing namespaces");
    Ok(Json(namespaces))
}

#[instrument(skip(state))]
pub async fn get_namespace(
    State(state): State<Arc<IndexerState>>,
    Path(name): Path<String>,
) -> Result<Json<NamespaceResponse>, IndexerError> {
    let ns = state.resolve_namespace(&name).await?;
    Ok(Json(NamespaceResponse::from(ns)))
}

#[instrument(skip(state))]
pub async fn delete_namespace(
    State(state): State<Arc<IndexerState>>,
    Path(name): Path<String>,
) -> Result<StatusCode, IndexerError> {
    state.delete_namespace(&name).await?;
    Ok(StatusCode::NO_CONTENT)
}
