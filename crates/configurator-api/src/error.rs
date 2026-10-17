use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{1}")]
    BadRequest(&'static str, String),
    #[error("missing or invalid token")]
    Unauthorized,
    #[error("this needs the {0} role")]
    Forbidden(&'static str),
    /// 403 with its own code, e.g. `no_workspace`.
    #[error("{1}")]
    Denied(&'static str, String),
    #[error("{0} not found")]
    NotFound(String),
    /// 404 with its own code, e.g. `tenant_not_found`.
    #[error("{1}")]
    Missing(&'static str, String),
    /// 409 with its own code, e.g. `last_admin`.
    #[error("{1}")]
    Conflict(&'static str, String),
    #[error("this route does not accept that method")]
    MethodNotAllowed,
    #[error("{1}")]
    Unavailable(&'static str, String),
    /// An error that lists every item at fault: `{"error":{"code","message"},"errors":[…]}`
    /// (e.g. 400 `invalid_fields` with each field error).
    #[error("{message}")]
    Detailed {
        status: StatusCode,
        code: &'static str,
        message: String,
        errors: Vec<Value>,
    },
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
            ApiError::Denied(c, _) => (StatusCode::FORBIDDEN, *c),
            ApiError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            ApiError::Missing(c, _) => (StatusCode::NOT_FOUND, *c),
            ApiError::Conflict(c, _) => (StatusCode::CONFLICT, *c),
            ApiError::MethodNotAllowed => (StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed"),
            ApiError::Unavailable(c, _) => (StatusCode::SERVICE_UNAVAILABLE, *c),
            ApiError::Detailed { status, code, .. } => (*status, *code),
            ApiError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::Internal(e.into())
    }
}

/// A query string that did not parse: the error envelope.
impl From<axum::extract::rejection::QueryRejection> for ApiError {
    fn from(e: axum::extract::rejection::QueryRejection) -> Self {
        ApiError::BadRequest("invalid_query", e.body_text())
    }
}

/// A JSON body that did not parse: the error envelope, never axum's plain-text rejection.
impl From<axum::extract::rejection::JsonRejection> for ApiError {
    fn from(e: axum::extract::rejection::JsonRejection) -> Self {
        ApiError::BadRequest("invalid_body", e.body_text())
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
        let mut body = json!({"error": {"code": code, "message": message}});
        if let ApiError::Detailed { errors, .. } = self {
            body["errors"] = Value::Array(errors);
        }
        (status, Json(body)).into_response()
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
