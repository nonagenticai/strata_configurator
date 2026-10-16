use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{1}")]
    BadRequest(&'static str, String),
    #[error("missing or invalid token")]
    Unauthorized,
    #[error("this needs the {0} role")]
    Forbidden(&'static str),
    #[error("{0} not found")]
    NotFound(String),
    #[error("this route does not accept that method")]
    MethodNotAllowed,
    #[error("{1}")]
    Unavailable(&'static str, String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    /// HTTP status and machine code only; never carries message text.
    pub fn status_and_code(&self) -> (StatusCode, &'static str) {
        match self {
            ApiError::BadRequest(c, _) => (StatusCode::BAD_REQUEST, *c),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            ApiError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            ApiError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            ApiError::MethodNotAllowed => (StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed"),
            ApiError::Unavailable(c, _) => (StatusCode::SERVICE_UNAVAILABLE, *c),
            ApiError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let ApiError::Internal(e) = &self {
            tracing::error!(error = %format!("{e:#}"), "internal error");
        }
        let (status, code) = self.status_and_code();
        let message = match &self {
            ApiError::Internal(_) => "Something went wrong on our side.".to_string(),
            o => o.to_string(),
        };
        (
            status,
            Json(json!({"error": {"code": code, "message": message}})),
        )
            .into_response()
    }
}

/// Unknown route: the same error envelope as everything else.
pub async fn fallback() -> ApiError {
    ApiError::NotFound("route".into())
}

/// Known route, wrong method: the same error envelope.
pub async fn method_not_allowed() -> ApiError {
    ApiError::MethodNotAllowed
}
