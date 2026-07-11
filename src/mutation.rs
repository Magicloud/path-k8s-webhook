use axum::http::StatusCode;

pub async fn mutate() -> (StatusCode, &'static str) {
    (StatusCode::NOT_IMPLEMENTED, "")
}
