mod file_store;
mod keychain;

pub use file_store::{credentials_file_path, read_credentials_from_file, write_oauth_token};
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

/// Read credentials from `ANTHROPIC_API_KEY` env var. Returns `None` when the
/// var is unset or empty. Pure (no Keychain access) so tests can exercise
/// the env-var path without provoking a Keychain ACL prompt.
fn env_var_credentials() -> Option<Credentials> {
    std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
        .map(Credentials::ApiKey)
}

/// Resolve the credentials to use for API calls.
///
/// Priority order (first hit wins):
///   1. `ANTHROPIC_API_KEY` environment variable → `Credentials::ApiKey`
///   2. Credentials file at `~/.claude/credentials.json` (or `CLAUDE_CREDENTIALS_FILE`)
///   3. System keychain (`Claude Code-credentials`) → `Credentials::OAuthToken`
///
/// The file path was added to let dev builds skip Keychain entirely: `cargo build`
/// produces a fresh CDHash on every build, which invalidates Keychain ACLs and
/// triggers a password prompt each run. A plain file (mode 0600) sidesteps that.
pub fn resolve_credentials() -> CcResult<(Credentials, ApiKeySource)> {
    // 1. Environment variable (direct API key).
    if let Some(creds) = env_var_credentials() {
        return Ok((creds, ApiKeySource::EnvVar));
    }

    // 2. File fallback — for dev builds and scripted setups.
    match read_credentials_from_file() {
        Ok(Some(creds)) => return Ok((creds, ApiKeySource::File)),
        Ok(None) => {}
        Err(e) => {
            tracing::debug!("credentials file lookup failed: {e}");
        }
    }

    // 3. System keychain (OAuth token stored by `claude /login`).
    match get_credentials_from_keychain() {
        Ok(Some(creds)) => return Ok((creds, ApiKeySource::Keychain)),
        Ok(None) => {}
        Err(e) => {
            tracing::debug!("keychain lookup failed: {e}");
        }
    }

    Err(CcError::Auth(
        "No credentials found. Set ANTHROPIC_API_KEY, write ~/.claude/credentials.json, or run `claude /login`.".into(),
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
    use std::sync::Mutex;

    // Serialize all env-var-touching tests — Rust runs tests in parallel threads
    // by default and env vars are process-global, so concurrent sets race.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn env_var_credentials_take_priority() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Test env_var_credentials directly so we never fall through to the
        // Keychain path (which would trigger an ACL prompt for the test binary).
        let saved = std::env::var("ANTHROPIC_API_KEY").ok();
        std::env::set_var("ANTHROPIC_API_KEY", "");
        assert!(env_var_credentials().is_none());
        std::env::remove_var("ANTHROPIC_API_KEY");
        assert!(env_var_credentials().is_none());

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
        assert_eq!(ApiKeySource::File.to_string(), "credentials file");
        assert_eq!(ApiKeySource::Keychain.to_string(), "system keychain");
    }

    #[test]
    fn file_credentials_win_over_keychain() {
        // Set CLAUDE_CREDENTIALS_FILE to a fresh path, write a known token,
        // and verify resolve_credentials returns File (never asking Keychain).
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved_env = std::env::var("ANTHROPIC_API_KEY").ok();
        let saved_file = std::env::var("CLAUDE_CREDENTIALS_FILE").ok();
        std::env::remove_var("ANTHROPIC_API_KEY");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        std::env::set_var("CLAUDE_CREDENTIALS_FILE", &path);
        write_oauth_token("file-token-abc", None, None).unwrap();

        let (creds, source) = resolve_credentials().unwrap();
        assert!(matches!(creds, Credentials::OAuthToken(t) if t == "file-token-abc"));
        assert_eq!(source, ApiKeySource::File);

        match saved_env {
            Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
        match saved_file {
            Some(v) => std::env::set_var("CLAUDE_CREDENTIALS_FILE", v),
            None => std::env::remove_var("CLAUDE_CREDENTIALS_FILE"),
        }
    }
}
