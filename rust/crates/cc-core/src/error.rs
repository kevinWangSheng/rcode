use thiserror::Error;

#[derive(Debug, Error)]
pub enum CcError {
    #[error("API error: {message}")]
    Api {
        message: String,
        status: Option<u16>,
        retryable: bool,
    },

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Tool error: {tool}: {message}")]
    Tool { tool: String, message: String },

    #[error("MCP error: {server}: {message}")]
    Mcp { server: String, message: String },

    #[error("Hook error: {event}: {message}")]
    Hook { event: String, message: String },

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Permission denied: {tool}: {reason}")]
    PermissionDenied { tool: String, reason: String },

    #[error("Cancelled")]
    Cancelled,

    #[error("Rate limited{}{}",
        retry_after.map(|s| format!(" (retry after {s}s)")).unwrap_or_default(),
        message.as_deref().map(|m| format!(": {m}")).unwrap_or_default())]
    RateLimited {
        retry_after: Option<u64>,
        /// Server-provided human-readable reason (parsed from the 429
        /// response body's `error.message` field when present). Preserved
        /// so logs surface "organization rate limit exceeded" etc. instead
        /// of a context-free "Rate limited".
        message: Option<String>,
    },

    #[error("{0}")]
    Other(String),
}

impl CcError {
    /// Whether this error is safe to retry.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Api {
                retryable: true,
                ..
            } | Self::RateLimited { .. }
        )
    }

    /// Whether this error should abort the entire session.
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::Auth(_) | Self::Config(_))
    }

    /// Convenience constructor for API errors.
    pub fn api(message: impl Into<String>) -> Self {
        Self::Api {
            message: message.into(),
            status: None,
            retryable: false,
        }
    }

    /// Convenience constructor for retryable API errors.
    pub fn api_retryable(message: impl Into<String>, status: u16) -> Self {
        Self::Api {
            message: message.into(),
            status: Some(status),
            retryable: true,
        }
    }

    /// Convenience constructor for tool errors.
    pub fn tool(tool: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Tool {
            tool: tool.into(),
            message: message.into(),
        }
    }

    /// Convenience constructor for IO errors from strings.
    pub fn io(message: impl Into<String>) -> Self {
        Self::Io(std::io::Error::other(message.into()))
    }

    /// Convenience constructor for MCP errors.
    pub fn mcp(server: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Mcp {
            server: server.into(),
            message: message.into(),
        }
    }

    /// Convenience constructor for permission denied errors.
    pub fn permission_denied(tool: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::PermissionDenied {
            tool: tool.into(),
            reason: reason.into(),
        }
    }
}

pub type CcResult<T> = Result<T, CcError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limited_display_without_retry_after() {
        // Regression: formatting with `{:?}` used to produce "Nones".
        let e = CcError::RateLimited {
            retry_after: None,
            message: None,
        };
        assert_eq!(e.to_string(), "Rate limited");
    }

    #[test]
    fn rate_limited_display_with_retry_after_and_message() {
        let e = CcError::RateLimited {
            retry_after: Some(30),
            message: Some("organization rate limit exceeded".into()),
        };
        assert_eq!(
            e.to_string(),
            "Rate limited (retry after 30s): organization rate limit exceeded"
        );
    }

    #[test]
    fn error_retryable() {
        assert!(CcError::RateLimited {
            retry_after: Some(5),
            message: None,
        }
        .is_retryable());
        assert!(CcError::api_retryable("server error", 500).is_retryable());
        assert!(!CcError::api("bad request").is_retryable());
        assert!(!CcError::Cancelled.is_retryable());
    }

    #[test]
    fn error_fatal() {
        assert!(CcError::Auth("bad token".into()).is_fatal());
        assert!(CcError::Config("missing file".into()).is_fatal());
        assert!(!CcError::Cancelled.is_fatal());
        assert!(!CcError::api("error").is_fatal());
    }

    #[test]
    fn error_display() {
        let e = CcError::Tool {
            tool: "Bash".into(),
            message: "exit 1".into(),
        };
        assert_eq!(e.to_string(), "Tool error: Bash: exit 1");

        let e = CcError::PermissionDenied {
            tool: "Write".into(),
            reason: "denied by user".into(),
        };
        assert_eq!(e.to_string(), "Permission denied: Write: denied by user");
    }
}
