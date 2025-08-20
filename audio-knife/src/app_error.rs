//! <https://github.com/tokio-rs/axum/blob/main/examples/anyhow-error-response/src/main.rs>
//! Axum support for `anyhow::Error`.
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

#[allow(unused)]
pub struct AppError(anyhow::Error);

// Tell axum how to convert `AppError` into a response.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, self.0.to_string()).into_response()
    }
}

// This enables using `?` on functions that return `Result<_, anyhow::Error>` to turn them into
// `Result<_, AppError>`. That way you don't need to do that manually.
impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}
