mod keychain;

pub use keychain::{
    delete_api_key, get_api_key, get_credentials_from_keychain, save_api_key, ApiKeySource,
    Credentials,
};

use cc_core::{CcError, CcResult};

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
