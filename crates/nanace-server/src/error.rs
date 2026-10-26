use actix_web::http::StatusCode;
use actix_web::{HttpResponse, ResponseError};
use nanace_core::validate::ValidationError;
use serde::Serialize;

use crate::LOG_COMPONENT;

pub type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("authentication required")]
    Unauthorized,
    #[error("permission denied")]
    Forbidden,
    /// Also returned for resources the caller may not know exist, so that
    /// probing ids does not reveal other tenants' data.
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("{0} is unavailable")]
    Unavailable(&'static str),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl From<ValidationError> for ApiError {
    fn from(err: ValidationError) -> Self {
        Self::BadRequest(err.to_string())
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        if let Some(db) = err.as_database_error()
            && db.is_unique_violation()
        {
            return Self::Conflict("resource already exists".to_owned());
        }
        Self::Internal(err.into())
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    message: String,
}

impl ResponseError for ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn error_response(&self) -> HttpResponse {
        let (code, message) = match self {
            Self::BadRequest(_) => ("bad_request", self.to_string()),
            Self::Unauthorized => ("unauthorized", self.to_string()),
            Self::Forbidden => ("forbidden", self.to_string()),
            Self::NotFound => ("not_found", self.to_string()),
            Self::Conflict(_) => ("conflict", self.to_string()),
            Self::Unavailable(_) => ("unavailable", self.to_string()),
            Self::Internal(err) => {
                tracing::error!(
                    event = "request_failed",
                    component = LOG_COMPONENT,
                    error = %format!("{err:#}"),
                    "internal error"
                );
                ("internal", "internal server error".to_owned())
            }
        };
        HttpResponse::build(self.status_code()).json(ErrorBody { error: code, message })
    }
}
