//! File-based credential store — bypasses Keychain for dev builds.
//!
//! Dev binaries compiled by `cargo build` get a fresh CDHash on every build,
//! which invalidates the Keychain ACL and triggers a prompt each run.
//! Reading from a plain file avoids that entirely: the file doesn't care
//! about the caller's code signature, only POSIX permissions (`0600`).
//!
//! JSON schema matches what Keychain stores (`SecureStorageData`) so a user
//! can copy their existing Keychain entry to a file verbatim, or we can
//! serialize freshly-obtained OAuth tokens in the same shape.
//!
//! Default path: `~/.claude/credentials.json`
//! Override:     `CLAUDE_CREDENTIALS_FILE` env var (any absolute path)

use crate::keychain::{Credentials, OAuthTokens, SecureStorageData};
use cc_core::{CcError, CcResult};
use std::path::PathBuf;

const DEFAULT_CREDENTIALS_DIR: &str = ".claude";
const DEFAULT_CREDENTIALS_FILE: &str = "credentials.json";

/// Resolve the credentials file path. Returns `None` when `HOME` is unset
/// and no override env var is provided.
pub fn credentials_file_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CLAUDE_CREDENTIALS_FILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(DEFAULT_CREDENTIALS_DIR)
            .join(DEFAULT_CREDENTIALS_FILE)
    })
}

/// Read credentials from a specific path. Returns `Ok(None)` when the
/// file doesn't exist.
pub fn read_credentials_from_path(path: &std::path::Path) -> CcResult<Option<Credentials>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(path).map_err(|e| {
        CcError::Auth(format!(
            "failed to read credentials file {}: {e}",
            path.display()
        ))
    })?;
    let data: SecureStorageData = serde_json::from_str(&content).map_err(|e| {
        CcError::Auth(format!("credentials file parse error: {e}"))
    })?;
    Ok(data
        .claude_ai_oauth
        .map(|t| Credentials::OAuthToken(t.access_token)))
}

/// Read credentials from the resolved default/override path. Thin wrapper
/// over `read_credentials_from_path` that consults env vars + HOME.
pub fn read_credentials_from_file() -> CcResult<Option<Credentials>> {
    let Some(path) = credentials_file_path() else {
        return Ok(None);
    };
    read_credentials_from_path(&path)
}

/// Write an OAuth token to an explicit path with mode 0600. Creates the
/// parent directory (mode 0700) if missing.
pub fn write_oauth_token_to_path(
    path: &std::path::Path,
    access_token: impl Into<String>,
    refresh_token: Option<String>,
    expires_at: Option<u64>,
) -> CcResult<()> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CcError::Auth(format!(
                    "failed to create credentials dir {}: {e}",
                    parent.display()
                ))
            })?;
            set_mode(parent, 0o700).ok();
        }
    }

    let data = SecureStorageData {
        claude_ai_oauth: Some(OAuthTokens {
            access_token: access_token.into(),
            refresh_token,
            expires_at,
        }),
    };
    let json = serde_json::to_string_pretty(&data)
        .map_err(|e| CcError::Auth(format!("credentials serialize error: {e}")))?;
    std::fs::write(path, json).map_err(|e| {
        CcError::Auth(format!(
            "failed to write credentials file {}: {e}",
            path.display()
        ))
    })?;
    set_mode(path, 0o600).ok();
    Ok(())
}

/// Write an OAuth token to the resolved default/override path.
pub fn write_oauth_token(
    access_token: impl Into<String>,
    refresh_token: Option<String>,
    expires_at: Option<u64>,
) -> CcResult<PathBuf> {
    let path = credentials_file_path()
        .ok_or_else(|| CcError::Auth("HOME not set and CLAUDE_CREDENTIALS_FILE unset".into()))?;
    write_oauth_token_to_path(&path, access_token, refresh_token, expires_at)?;
    Ok(path)
}

#[cfg(unix)]
fn set_mode(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &std::path::Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    // Tests use the path-taking APIs so parallel test execution can't race
    // on the shared CLAUDE_CREDENTIALS_FILE env var.
    use super::*;

    #[test]
    fn read_returns_none_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let creds = read_credentials_from_path(&dir.path().join("nope.json")).unwrap();
        assert!(creds.is_none());
    }

    #[test]
    fn write_and_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        write_oauth_token_to_path(&path, "sk-ant-test-token", None, None).unwrap();
        let creds = read_credentials_from_path(&path).unwrap();
        match creds {
            Some(Credentials::OAuthToken(t)) => assert_eq!(t, "sk-ant-test-token"),
            other => panic!("expected OAuthToken, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn write_sets_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        write_oauth_token_to_path(&path, "t", None, None).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "credentials file must be 0600, got {mode:o}");
    }

    #[test]
    fn write_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested").join("creds.json");
        write_oauth_token_to_path(&nested, "t", None, None).unwrap();
        assert!(nested.exists());
    }

    #[test]
    fn read_rejects_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "not json at all").unwrap();
        let result = read_credentials_from_path(&path);
        assert!(result.is_err());
    }

    #[test]
    fn read_handles_empty_oauth_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.json");
        std::fs::write(&path, "{}").unwrap();
        let creds = read_credentials_from_path(&path).unwrap();
        assert!(creds.is_none());
    }
}
