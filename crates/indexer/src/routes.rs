use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;

use crate::handlers::{health, ingest, namespace};
use crate::state::IndexerState;

pub fn router(state: Arc<IndexerState>) -> Router {
    Router::new()
        .route("/v1/logs/{namespace}", post(ingest::ingest_logs))
        .route(
            "/v1/namespaces",
            post(namespace::create_namespace).get(namespace::list_namespaces),
        )
        .route(
            "/v1/namespaces/{name}",
            get(namespace::get_namespace).delete(namespace::delete_namespace),
        )
        .route("/health", get(health::health))
        .route("/ready", get(health::ready))
        .with_state(state)
}
