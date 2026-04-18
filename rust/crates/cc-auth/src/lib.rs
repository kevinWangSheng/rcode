mod file_store;
mod keychain;
pub mod oauth;

pub use file_store::{
    credentials_file_path, read_credentials_from_file, read_oauth_tokens_from_file,
    read_oauth_tokens_from_path, write_oauth_token, write_oauth_token_to_path,
};
pub use keychain::{
    delete_api_key, get_api_key, get_credentials_from_keychain, save_api_key, ApiKeySource,
    Credentials, OAuthTokens,
};
pub use oauth::{run_login_flow, OAuthConfig};

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

/// Resolve credentials, refreshing the file-stored OAuth token if it's near
/// expiry. This is the preferred entry point for the CLI — it keeps long
/// sessions alive without forcing the user to re-run `claude login` every
/// 8 hours.
///
/// Refresh is only attempted for file-stored tokens (the default for dev
/// builds). Keychain-stored tokens pass through unchanged: writing back to
/// the keychain on every refresh would re-trigger the CDHash ACL prompt
/// that prompted us to introduce the file store in the first place.
///
/// Env-var credentials (`ANTHROPIC_API_KEY`) never need refreshing — direct
/// API keys don't expire.
pub async fn ensure_fresh_credentials() -> CcResult<(Credentials, ApiKeySource)> {
    ensure_fresh_credentials_with(&oauth::OAuthConfig::default()).await
}

/// Like [`ensure_fresh_credentials`] but lets tests point the refresh flow
/// at a local mock token endpoint.
pub async fn ensure_fresh_credentials_with(
    oauth_cfg: &oauth::OAuthConfig,
) -> CcResult<(Credentials, ApiKeySource)> {
    // Env var short-circuits everything — direct API keys don't expire.
    if let Some(creds) = env_var_credentials() {
        return Ok((creds, ApiKeySource::EnvVar));
    }

    // File store: read the full bundle so we can inspect expires_at + refresh_token.
    match read_oauth_tokens_from_file() {
        Ok(Some((path, tokens))) => {
            let tokens = maybe_refresh_file_tokens(oauth_cfg, &path, tokens).await?;
            return Ok((
                Credentials::OAuthToken(tokens.access_token),
                ApiKeySource::File,
            ));
        }
        Ok(None) => {}
        Err(e) => {
            tracing::debug!("credentials file lookup failed: {e}");
        }
    }

    // Keychain: pass through without touching the stored value.
    match get_credentials_from_keychain() {
        Ok(Some(creds)) => return Ok((creds, ApiKeySource::Keychain)),
        Ok(None) => {}
        Err(e) => {
            tracing::debug!("keychain lookup failed: {e}");
        }
    }

    Err(CcError::Auth(
        "No credentials found. Set ANTHROPIC_API_KEY, write ~/.claude/credentials.json, or run `claude login`.".into(),
    ))
}

/// Refresh the file-stored tokens in-place when they're near expiry. Returns
/// the fresh bundle, or the original if no refresh was needed/possible.
///
/// Failure modes are intentionally lenient: if the refresh request itself
/// fails (network, 5xx, revoked refresh_token), we log and return the old
/// tokens. The next API call will get a 401 and surface the real error,
/// which is more actionable than blocking the CLI on a refresh error here.
async fn maybe_refresh_file_tokens(
    oauth_cfg: &oauth::OAuthConfig,
    path: &std::path::Path,
    tokens: OAuthTokens,
) -> CcResult<OAuthTokens> {
    if !oauth::is_oauth_token_expired(tokens.expires_at) {
        return Ok(tokens);
    }
    let Some(refresh_token) = tokens.refresh_token.as_deref() else {
        // Near expiry but no refresh_token — nothing we can do, hand back
        // the old access_token and let the API 401 surface the problem.
        tracing::warn!("OAuth token near expiry but no refresh_token stored");
        return Ok(tokens);
    };

    let http = reqwest::Client::new();
    match oauth::refresh_oauth_token(oauth_cfg, &http, refresh_token).await {
        Ok(resp) => {
            let expires_at = resp.expires_in.map(|s| oauth::now_ms() + s * 1000);
            let new_tokens = OAuthTokens {
                access_token: resp.access_token.clone(),
                refresh_token: resp.refresh_token.clone(),
                expires_at,
            };
            if let Err(e) = write_oauth_token_to_path(
                path,
                &new_tokens.access_token,
                new_tokens.refresh_token.clone(),
                expires_at,
            ) {
                tracing::warn!("refreshed token but failed to persist: {e}");
            }
            Ok(new_tokens)
        }
        Err(e) => {
            tracing::warn!("OAuth refresh failed, using stale token: {e}");
            Ok(tokens)
        }
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
        assert_eq!(
            ApiKeySource::EnvVar.to_string(),
            "ANTHROPIC_API_KEY env var"
        );
        assert_eq!(ApiKeySource::File.to_string(), "credentials file");
        assert_eq!(ApiKeySource::Keychain.to_string(), "system keychain");
    }

    /// Same hand-rolled token endpoint used by `oauth::tests`. Duplicated here
    /// to keep the refresh-through-file integration test self-contained —
    /// the oauth module's copy is `mod tests`-private.
    async fn spawn_mock_token_endpoint(
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::TcpListener;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/v1/oauth/token");
        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            let _ = reader.read_line(&mut line).await;
            loop {
                let mut header = String::new();
                match reader.read_line(&mut header).await {
                    Ok(0) => break,
                    Ok(_) if header == "\r\n" || header == "\n" => break,
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 \r\n\
                 {}",
                body.len(),
                body
            );
            let _ = writer.write_all(response.as_bytes()).await;
            let _ = writer.shutdown().await;
        });
        (url, handle)
    }

    #[tokio::test]
    // ENV_LOCK must stay held across the `.await` — the test mutates process-
    // global env vars that every other auth test also touches, and dropping
    // the guard mid-test would let parallel sync tests race on
    // ANTHROPIC_API_KEY / CLAUDE_CREDENTIALS_FILE. clippy's await-holding-lock
    // warning is the general case; here the lock *is* the correctness boundary.
    #[allow(clippy::await_holding_lock)]
    async fn ensure_fresh_refreshes_expired_file_token() {
        // Arrange: write an expired token + valid refresh_token to a temp file.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved_env = std::env::var("ANTHROPIC_API_KEY").ok();
        let saved_file = std::env::var("CLAUDE_CREDENTIALS_FILE").ok();
        std::env::remove_var("ANTHROPIC_API_KEY");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        std::env::set_var("CLAUDE_CREDENTIALS_FILE", &path);
        // expires_at in the past — definitely expired.
        write_oauth_token("old-access", Some("old-refresh".into()), Some(1_000)).unwrap();

        let (token_url, server) = spawn_mock_token_endpoint(
            r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":28800}"#,
        )
        .await;
        let cfg = oauth::OAuthConfig {
            token_url,
            ..oauth::OAuthConfig::default()
        };

        // Act
        let (creds, source) = ensure_fresh_credentials_with(&cfg).await.unwrap();

        // Assert: returned creds use the new access token + source is File.
        assert!(matches!(creds, Credentials::OAuthToken(t) if t == "new-access"));
        assert_eq!(source, ApiKeySource::File);

        // Assert: the file on disk now holds the refreshed token bundle.
        let on_disk = read_oauth_tokens_from_path(&path).unwrap().unwrap();
        assert_eq!(on_disk.access_token, "new-access");
        assert_eq!(on_disk.refresh_token.as_deref(), Some("new-refresh"));
        assert!(on_disk.expires_at.unwrap() > oauth::now_ms());

        server.await.unwrap();

        match saved_env {
            Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
        match saved_file {
            Some(v) => std::env::set_var("CLAUDE_CREDENTIALS_FILE", v),
            None => std::env::remove_var("CLAUDE_CREDENTIALS_FILE"),
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // see sibling test for rationale
    async fn ensure_fresh_passes_through_fresh_file_token() {
        // Arrange: a token with expires_at 1 hour in the future — no refresh needed.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved_env = std::env::var("ANTHROPIC_API_KEY").ok();
        let saved_file = std::env::var("CLAUDE_CREDENTIALS_FILE").ok();
        std::env::remove_var("ANTHROPIC_API_KEY");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        std::env::set_var("CLAUDE_CREDENTIALS_FILE", &path);
        let expires_at = oauth::now_ms() + 60 * 60 * 1000;
        write_oauth_token(
            "fresh-access",
            Some("fresh-refresh".into()),
            Some(expires_at),
        )
        .unwrap();

        // Point at an unreachable URL — if the code tried to refresh, the
        // network call would fail and we'd fall back to the stale token,
        // but the test still verifies the returned access_token is the
        // original, so we know no refresh happened.
        let cfg = oauth::OAuthConfig {
            token_url: "http://127.0.0.1:1/should-not-be-called".into(),
            ..oauth::OAuthConfig::default()
        };

        let (creds, source) = ensure_fresh_credentials_with(&cfg).await.unwrap();
        assert!(matches!(creds, Credentials::OAuthToken(t) if t == "fresh-access"));
        assert_eq!(source, ApiKeySource::File);

        // File on disk must be untouched.
        let on_disk = read_oauth_tokens_from_path(&path).unwrap().unwrap();
        assert_eq!(on_disk.access_token, "fresh-access");

        match saved_env {
            Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
        match saved_file {
            Some(v) => std::env::set_var("CLAUDE_CREDENTIALS_FILE", v),
            None => std::env::remove_var("CLAUDE_CREDENTIALS_FILE"),
        }
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
