use cc_core::CcResult;
use keyring::Entry;
use serde::Deserialize;

// ─── Keychain layout (must match TS version exactly) ──────────────────────
// Service:  "Claude Code-credentials"  (CREDENTIALS_SERVICE_SUFFIX = "-credentials")
// Account:  $USER (the OS username)
// Value:    JSON string — see SecureStorageData below
// DO NOT change service/account names — they are part of the keychain lookup
// key and would orphan existing stored credentials.

const KEYCHAIN_SERVICE_CREDENTIALS: &str = "Claude Code-credentials";

// ─── SecureStorageData schema ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuthTokens {
    access_token: String,
    // We only need access_token for Phase 1; refresh_token / expiry handled later.
    #[allow(dead_code)]
    refresh_token: Option<String>,
    #[allow(dead_code)]
    expires_at: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecureStorageData {
    claude_ai_oauth: Option<OAuthTokens>,
}

// ─── Public API ────────────────────────────────────────────────────────────

/// Where the auth token came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiKeySource {
    EnvVar,
    Keychain,
}

impl std::fmt::Display for ApiKeySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiKeySource::EnvVar => write!(f, "ANTHROPIC_API_KEY env var"),
            ApiKeySource::Keychain => write!(f, "system keychain"),
        }
    }
}

/// Credentials used to authenticate API calls.
#[derive(Debug, Clone)]
pub enum Credentials {
    /// Direct API key → sent as `x-api-key` header.
    ApiKey(String),
    /// Claude.ai OAuth token → sent as `Authorization: Bearer` header.
    OAuthToken(String),
}

/// Retrieve credentials from the system keychain.
/// Returns `Ok(None)` when no credentials are stored (not an error).
pub fn get_credentials_from_keychain() -> CcResult<Option<Credentials>> {
    let username = std::env::var("USER").unwrap_or_else(|_| "claude-code-user".to_string());

    let entry = Entry::new(KEYCHAIN_SERVICE_CREDENTIALS, &username)
        .map_err(|e| cc_core::CcError::Auth(e.to_string()))?;

    let raw = match entry.get_password() {
        Ok(s) => s,
        Err(keyring::Error::NoEntry) => return Ok(None),
        Err(e) => return Err(cc_core::CcError::Auth(e.to_string())),
    };

    let data: SecureStorageData = serde_json::from_str(&raw)
        .map_err(|e| cc_core::CcError::Auth(format!("keychain JSON parse error: {e}")))?;

    if let Some(oauth) = data.claude_ai_oauth {
        return Ok(Some(Credentials::OAuthToken(oauth.access_token)));
    }

    Ok(None)
}

/// Get just an API key from the keychain (legacy path, not currently used by default).
pub fn get_api_key() -> CcResult<Option<String>> {
    match get_credentials_from_keychain()? {
        Some(Credentials::ApiKey(k)) => Ok(Some(k)),
        _ => Ok(None),
    }
}

/// Save an API key to the system keychain (not used in OAuth flow).
pub fn save_api_key(_key: &str) -> CcResult<()> {
    // Phase 1: not implemented — OAuth is the primary auth path.
    Err(cc_core::CcError::Auth(
        "save_api_key not implemented — use `claude /login` for OAuth setup".into(),
    ))
}

/// Remove credentials from the system keychain.
pub fn delete_api_key() -> CcResult<()> {
    let username = std::env::var("USER").unwrap_or_else(|_| "claude-code-user".to_string());
    let entry = Entry::new(KEYCHAIN_SERVICE_CREDENTIALS, &username)
        .map_err(|e| cc_core::CcError::Auth(e.to_string()))?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(cc_core::CcError::Auth(e.to_string())),
    }
}
