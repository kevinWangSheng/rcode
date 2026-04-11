mod keychain;

pub use keychain::{
    delete_api_key, get_api_key, get_credentials_from_keychain, save_api_key, ApiKeySource,
    Credentials,
};

use cc_core::{CcError, CcResult};

/// Authentication credential — used by cc-api for request signing.
#[derive(Debug, Clone)]
pub enum AuthCredential {
    /// Direct API key → `x-api-key` header.
    ApiKey(String),
    /// OAuth token → `Authorization: Bearer` header + `anthropic-beta: oauth-2025-04-20`.
    OAuthToken(String),
}

impl From<Credentials> for AuthCredential {
    fn from(creds: Credentials) -> Self {
        match creds {
            Credentials::ApiKey(k) => AuthCredential::ApiKey(k),
            Credentials::OAuthToken(t) => AuthCredential::OAuthToken(t),
        }
    }
}

/// Resolve the credentials to use for API calls.
///
/// Priority order:
///   1. `ANTHROPIC_API_KEY` environment variable → `Credentials::ApiKey`
///   2. System keychain (`Claude Code-credentials`) → `Credentials::OAuthToken`
pub fn resolve_credentials() -> CcResult<(Credentials, ApiKeySource)> {
    // 1. Environment variable (direct API key).
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            return Ok((Credentials::ApiKey(key), ApiKeySource::EnvVar));
        }
    }

    // 2. System keychain (OAuth token stored by `claude /login`).
    match get_credentials_from_keychain() {
        Ok(Some(creds)) => return Ok((creds, ApiKeySource::Keychain)),
        Ok(None) => {}
        Err(e) => {
            tracing::debug!("keychain lookup failed: {e}");
        }
    }

    Err(CcError::Auth(
        "No credentials found. Set ANTHROPIC_API_KEY or run `claude /login`.".into(),
    ))
}

/// Convenience: resolve an API key string for use in `x-api-key` header.
/// Returns an error if only OAuth credentials are available.
pub fn resolve_api_key() -> CcResult<(String, ApiKeySource)> {
    match resolve_credentials()? {
        (Credentials::ApiKey(k), src) => Ok((k, src)),
        (Credentials::OAuthToken(_), _) => Err(CcError::Auth(
            "OAuth token found but direct API key required for this operation".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_credentials_take_priority() {
        let saved = std::env::var("ANTHROPIC_API_KEY").ok();
        std::env::set_var("ANTHROPIC_API_KEY", "sk-test-key-12345");

        let result = resolve_credentials();
        assert!(result.is_ok());
        let (creds, source) = result.unwrap();
        assert!(matches!(creds, Credentials::ApiKey(k) if k == "sk-test-key-12345"));
        assert_eq!(source, ApiKeySource::EnvVar);

        match saved {
            Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
    }

    #[test]
    fn empty_env_var_is_skipped() {
        let saved = std::env::var("ANTHROPIC_API_KEY").ok();
        std::env::set_var("ANTHROPIC_API_KEY", "");

        // Should not match the env var path — may fallback to keychain or error
        let result = resolve_credentials();
        // We can't assert Ok/Err because keychain state varies, but we verify
        // it doesn't return an empty API key
        if let Ok((Credentials::ApiKey(k), _)) = &result {
            assert!(!k.is_empty());
        }

        match saved {
            Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
    }

    #[test]
    fn auth_credential_from_credentials() {
        let api = AuthCredential::from(Credentials::ApiKey("key".into()));
        assert!(matches!(api, AuthCredential::ApiKey(k) if k == "key"));

        let oauth = AuthCredential::from(Credentials::OAuthToken("token".into()));
        assert!(matches!(oauth, AuthCredential::OAuthToken(t) if t == "token"));
    }

    #[test]
    fn api_key_source_display() {
        assert_eq!(ApiKeySource::EnvVar.to_string(), "ANTHROPIC_API_KEY env var");
        assert_eq!(ApiKeySource::Keychain.to_string(), "system keychain");
    }
}
