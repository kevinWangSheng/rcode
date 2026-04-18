//! cc-http — shared mTLS-aware HTTP client builder.
//!
//! Used by both cc-api (Anthropic API) and cc-mcp (HTTP transports).
//! Per Phase 2 Decision 5: mTLS is env-var driven, shared across subsystems.

use cc_core::CcResult;
use std::path::PathBuf;
use std::time::Duration;

/// Configuration for building a shared HTTP client.
pub struct HttpClientConfig {
    /// mTLS client certificate path (env: CLAUDE_CODE_CLIENT_CERT)
    pub client_cert: Option<PathBuf>,
    /// mTLS client key path (env: CLAUDE_CODE_CLIENT_KEY)
    pub client_key: Option<PathBuf>,
    /// CA bundle override (env: CLAUDE_CODE_CA_BUNDLE or NODE_EXTRA_CA_CERTS)
    pub ca_bundle: Option<PathBuf>,
    /// HTTP/HTTPS proxy (env: HTTPS_PROXY or HTTP_PROXY)
    pub proxy: Option<String>,
    /// Connect timeout (default: 30s)
    pub connect_timeout: Duration,
    /// Request timeout (default: None for streaming, 30s for RPC)
    pub request_timeout: Option<Duration>,
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            client_cert: None,
            client_key: None,
            ca_bundle: None,
            proxy: None,
            connect_timeout: Duration::from_secs(30),
            request_timeout: None,
        }
    }
}

impl HttpClientConfig {
    /// Build configuration from environment variables.
    pub fn from_env() -> Self {
        Self {
            client_cert: std::env::var("CLAUDE_CODE_CLIENT_CERT")
                .ok()
                .map(PathBuf::from),
            client_key: std::env::var("CLAUDE_CODE_CLIENT_KEY")
                .ok()
                .map(PathBuf::from),
            ca_bundle: std::env::var("CLAUDE_CODE_CA_BUNDLE")
                .ok()
                .or_else(|| std::env::var("NODE_EXTRA_CA_CERTS").ok())
                .map(PathBuf::from),
            proxy: std::env::var("HTTPS_PROXY")
                .ok()
                .or_else(|| std::env::var("HTTP_PROXY").ok()),
            connect_timeout: Duration::from_secs(30),
            request_timeout: None,
        }
    }

    /// Set the request timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }
}

/// Build a `reqwest::Client` with mTLS, proxy, and timeout configuration.
pub fn build_client(config: &HttpClientConfig) -> CcResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder().connect_timeout(config.connect_timeout);

    if let Some(ref proxy_url) = config.proxy {
        let proxy = reqwest::Proxy::all(proxy_url).map_err(|e| {
            cc_core::CcError::Config(format!("invalid proxy URL '{proxy_url}': {e}"))
        })?;
        builder = builder.proxy(proxy);
    }

    if let Some(ref ca) = config.ca_bundle {
        let cert_pem = std::fs::read(ca).map_err(|e| {
            cc_core::CcError::Config(format!("failed to read CA bundle {}: {e}", ca.display()))
        })?;
        let cert = reqwest::Certificate::from_pem(&cert_pem).map_err(|e| {
            cc_core::CcError::Config(format!("invalid CA certificate {}: {e}", ca.display()))
        })?;
        builder = builder.add_root_certificate(cert);
    }

    if let (Some(ref cert_path), Some(ref key_path)) = (&config.client_cert, &config.client_key) {
        let mut identity_pem = std::fs::read(cert_path).map_err(|e| {
            cc_core::CcError::Config(format!(
                "failed to read client cert {}: {e}",
                cert_path.display()
            ))
        })?;
        let key_pem = std::fs::read(key_path).map_err(|e| {
            cc_core::CcError::Config(format!(
                "failed to read client key {}: {e}",
                key_path.display()
            ))
        })?;
        identity_pem.extend_from_slice(&key_pem);
        let identity = reqwest::Identity::from_pem(&identity_pem)
            .map_err(|e| cc_core::CcError::Config(format!("invalid mTLS identity: {e}")))?;
        builder = builder.identity(identity);
    }

    if let Some(timeout) = config.request_timeout {
        builder = builder.timeout(timeout);
    }

    builder
        .build()
        .map_err(|e| cc_core::CcError::Config(format!("failed to build HTTP client: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = HttpClientConfig::default();
        assert!(config.client_cert.is_none());
        assert!(config.proxy.is_none());
        assert_eq!(config.connect_timeout, Duration::from_secs(30));
    }

    #[test]
    fn build_default_client() {
        let config = HttpClientConfig::default();
        let client = build_client(&config);
        assert!(client.is_ok());
    }

    #[test]
    fn with_timeout() {
        let config = HttpClientConfig::default().with_timeout(Duration::from_secs(60));
        assert_eq!(config.request_timeout, Some(Duration::from_secs(60)));
    }

    #[test]
    fn missing_ca_bundle_returns_config_error() {
        let config = HttpClientConfig {
            ca_bundle: Some(PathBuf::from("/nonexistent/ca.pem")),
            ..Default::default()
        };
        let err = build_client(&config).unwrap_err();
        assert!(matches!(err, cc_core::CcError::Config(_)));
        assert!(err.to_string().contains("/nonexistent/ca.pem"));
    }

    #[test]
    fn missing_client_cert_returns_config_error() {
        let config = HttpClientConfig {
            client_cert: Some(PathBuf::from("/nonexistent/cert.pem")),
            client_key: Some(PathBuf::from("/nonexistent/key.pem")),
            ..Default::default()
        };
        let err = build_client(&config).unwrap_err();
        assert!(matches!(err, cc_core::CcError::Config(_)));
    }

    #[test]
    fn from_env_reads_variables() {
        // Save and clear existing env vars to avoid interference
        let saved_cert = std::env::var("CLAUDE_CODE_CLIENT_CERT").ok();
        let saved_key = std::env::var("CLAUDE_CODE_CLIENT_KEY").ok();
        let saved_ca = std::env::var("CLAUDE_CODE_CA_BUNDLE").ok();
        let saved_proxy = std::env::var("HTTPS_PROXY").ok();

        std::env::set_var("CLAUDE_CODE_CLIENT_CERT", "/tmp/cert.pem");
        std::env::set_var("CLAUDE_CODE_CLIENT_KEY", "/tmp/key.pem");
        std::env::set_var("CLAUDE_CODE_CA_BUNDLE", "/tmp/ca.pem");
        std::env::set_var("HTTPS_PROXY", "http://proxy:8080");

        let config = HttpClientConfig::from_env();
        assert_eq!(config.client_cert, Some(PathBuf::from("/tmp/cert.pem")));
        assert_eq!(config.client_key, Some(PathBuf::from("/tmp/key.pem")));
        assert_eq!(config.ca_bundle, Some(PathBuf::from("/tmp/ca.pem")));
        assert_eq!(config.proxy, Some("http://proxy:8080".to_string()));

        // Restore
        match saved_cert {
            Some(v) => std::env::set_var("CLAUDE_CODE_CLIENT_CERT", v),
            None => std::env::remove_var("CLAUDE_CODE_CLIENT_CERT"),
        }
        match saved_key {
            Some(v) => std::env::set_var("CLAUDE_CODE_CLIENT_KEY", v),
            None => std::env::remove_var("CLAUDE_CODE_CLIENT_KEY"),
        }
        match saved_ca {
            Some(v) => std::env::set_var("CLAUDE_CODE_CA_BUNDLE", v),
            None => std::env::remove_var("CLAUDE_CODE_CA_BUNDLE"),
        }
        match saved_proxy {
            Some(v) => std::env::set_var("HTTPS_PROXY", v),
            None => std::env::remove_var("HTTPS_PROXY"),
        }
    }
}
