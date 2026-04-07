use thiserror::Error;

#[derive(Debug, Error)]
pub enum CcError {
    #[error("API error: {0}")]
    Api(String),

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Tool error: {0}")]
    Tool(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("{0}")]
    Other(String),
}

impl From<std::io::Error> for CcError {
    fn from(e: std::io::Error) -> Self {
        CcError::Io(e.to_string())
    }
}

pub type CcResult<T> = Result<T, CcError>;
