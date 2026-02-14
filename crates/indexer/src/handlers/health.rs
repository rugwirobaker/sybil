use axum::http::StatusCode;
use axum::response::IntoResponse;

pub async fn health() -> impl IntoResponse {
    StatusCode::OK
}

pub async fn ready() -> impl IntoResponse {
    // TODO: check that we can reach S3 / catalog is loaded
    StatusCode::OK
}
