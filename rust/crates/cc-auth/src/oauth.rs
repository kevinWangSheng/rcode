//! OAuth 2.0 authorization code flow with PKCE, porting the minimal path from
//! `src/services/oauth/` in the TS source. Supports only the automatic flow
//! (localhost callback), matching how dev builds would normally authenticate.
//!
//! Flow:
//!   1. Generate PKCE verifier + challenge, random state.
//!   2. Spawn a one-shot localhost HTTP server on an OS-assigned port.
//!   3. Build the Anthropic authorize URL, open it in the user's browser.
//!   4. Wait for `GET /callback?code=...&state=...` from the browser.
//!   5. Validate state; POST (code, verifier, redirect_uri) to the token endpoint.
//!   6. Return the OAuth tokens.
//!
//! The caller typically writes the returned tokens to `~/.claude/credentials.json`
//! via `crate::write_oauth_token`.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cc_core::{CcError, CcResult};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// OAuth endpoints + client identifier. Mirrors PROD_OAUTH_CONFIG in
/// src/constants/oauth.ts.
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub authorize_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    /// Default manual-flow redirect URL (not used by automatic flow — kept for
    /// parity with the TS source).
    pub manual_redirect_url: String,
}

impl Default for OAuthConfig {
    fn default() -> Self {
        Self {
            client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e".into(),
            authorize_url: "https://claude.com/cai/oauth/authorize".into(),
            token_url: "https://platform.claude.com/v1/oauth/token".into(),
            scopes: vec![
                "org:create_api_key".into(),
                "user:profile".into(),
                "user:inference".into(),
                "user:sessions:claude_code".into(),
                "user:mcp_servers".into(),
                "user:file_upload".into(),
            ],
            manual_redirect_url: "https://platform.claude.com/oauth/code/callback".into(),
        }
    }
}

/// Tokens returned by the OAuth token endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct OAuthTokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
}

/// Generate a PKCE code verifier: 32 random bytes, base64url-encoded.
pub fn generate_code_verifier() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Derive the code challenge from the verifier per PKCE S256.
pub fn generate_code_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hash)
}

/// Generate a random 32-byte state parameter (CSRF protection).
pub fn generate_state() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Build the authorize URL matching `buildAuthUrl` in client.ts (automatic flow).
pub fn build_authorize_url(
    cfg: &OAuthConfig,
    code_challenge: &str,
    state: &str,
    port: u16,
) -> String {
    let redirect_uri = format!("http://localhost:{port}/callback");
    let scope = cfg.scopes.join(" ");
    let mut url = reqwest::Url::parse(&cfg.authorize_url).expect("valid authorize_url");
    url.query_pairs_mut()
        .append_pair("code", "true")
        .append_pair("client_id", &cfg.client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", &scope)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state);
    url.to_string()
}

/// Spawn a localhost HTTP listener on an OS-assigned port. Returns the listener
/// plus the port number. Caller awaits `wait_for_callback` to consume it.
pub async fn start_callback_listener() -> CcResult<(TcpListener, u16)> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| CcError::Auth(format!("failed to bind callback listener: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| CcError::Auth(e.to_string()))?
        .port();
    Ok((listener, port))
}

/// Block until the first `GET /callback?...` request arrives. Returns the
/// `code` parameter after validating `state`. Responds to the browser with a
/// 302 redirect to the claude.ai success page so the user sees confirmation.
pub async fn wait_for_callback(
    listener: TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> CcResult<String> {
    let fut = async {
        loop {
            let (stream, _) = listener
                .accept()
                .await
                .map_err(|e| CcError::Auth(format!("accept failed: {e}")))?;
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut request_line = String::new();
            let n = reader
                .read_line(&mut request_line)
                .await
                .map_err(|e| CcError::Auth(format!("read request line: {e}")))?;
            if n == 0 {
                continue;
            }

            // Drain remaining headers (ignore body — callback is a GET).
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) if line == "\r\n" || line == "\n" => break,
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }

            let parts: Vec<&str> = request_line.split_whitespace().collect();
            if parts.len() < 2 {
                let _ = writer.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
                continue;
            }
            let (method, target) = (parts[0], parts[1]);
            if method != "GET" {
                let _ = writer
                    .write_all(b"HTTP/1.1 405 Method Not Allowed\r\n\r\n")
                    .await;
                continue;
            }

            // Parse path + query. Anything other than /callback → 404, keep listening.
            let url = reqwest::Url::parse(&format!("http://localhost{target}"))
                .map_err(|e| CcError::Auth(format!("bad callback URL: {e}")))?;
            if url.path() != "/callback" {
                let _ = writer.write_all(b"HTTP/1.1 404 Not Found\r\n\r\n").await;
                continue;
            }

            let code = url
                .query_pairs()
                .find(|(k, _)| k == "code")
                .map(|(_, v)| v.to_string());
            let state = url
                .query_pairs()
                .find(|(k, _)| k == "state")
                .map(|(_, v)| v.to_string());

            match (code, state) {
                (None, _) => {
                    let _ = writer
                        .write_all(
                            b"HTTP/1.1 400 Bad Request\r\n\
                              Content-Type: text/plain\r\n\
                              Content-Length: 29\r\n\
                              \r\n\
                              Authorization code not found\n",
                        )
                        .await;
                    return Err(CcError::Auth("no authorization code in callback".into()));
                }
                (_, Some(s)) if s != expected_state => {
                    let _ = writer
                        .write_all(
                            b"HTTP/1.1 400 Bad Request\r\n\
                              Content-Type: text/plain\r\n\
                              Content-Length: 24\r\n\
                              \r\n\
                              Invalid state parameter\n",
                        )
                        .await;
                    return Err(CcError::Auth("callback state mismatch (CSRF)".into()));
                }
                (Some(code), Some(_)) => {
                    // Success — redirect browser to the claude.ai success page.
                    let body = b"<html><body>Login complete - you can close this tab.</body></html>";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    );
                    let _ = writer.write_all(response.as_bytes()).await;
                    let _ = writer.write_all(body).await;
                    let _ = writer.shutdown().await;
                    return Ok(code);
                }
                (Some(_), None) => {
                    let _ = writer
                        .write_all(
                            b"HTTP/1.1 400 Bad Request\r\n\
                              Content-Type: text/plain\r\n\
                              Content-Length: 24\r\n\
                              \r\n\
                              Missing state parameter\n",
                        )
                        .await;
                    return Err(CcError::Auth("callback missing state".into()));
                }
            }
        }
    };

    tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| CcError::Auth(format!("timed out after {:?} waiting for OAuth callback", timeout)))?
}

/// Exchange an authorization code for OAuth tokens. Matches `exchangeCodeForTokens`.
pub async fn exchange_code_for_tokens(
    cfg: &OAuthConfig,
    http: &reqwest::Client,
    code: &str,
    code_verifier: &str,
    state: &str,
    port: u16,
) -> CcResult<OAuthTokenResponse> {
    let redirect_uri = format!("http://localhost:{port}/callback");
    #[derive(Serialize)]
    struct TokenRequest<'a> {
        grant_type: &'a str,
        code: &'a str,
        redirect_uri: &'a str,
        client_id: &'a str,
        code_verifier: &'a str,
        state: &'a str,
    }
    let body = TokenRequest {
        grant_type: "authorization_code",
        code,
        redirect_uri: &redirect_uri,
        client_id: &cfg.client_id,
        code_verifier,
        state,
    };
    let resp = http
        .post(&cfg.token_url)
        .header("content-type", "application/json")
        .json(&body)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| CcError::Auth(format!("token exchange request failed: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(CcError::Auth(format!(
            "token exchange failed ({status}): {text}"
        )));
    }
    resp.json::<OAuthTokenResponse>()
        .await
        .map_err(|e| CcError::Auth(format!("token exchange response parse error: {e}")))
}

/// End-to-end login flow. Opens the browser, waits for the callback, exchanges
/// the code, writes tokens to `~/.claude/credentials.json`, and returns the
/// path written to. On macOS the browser opens via `/usr/bin/open`, on Linux
/// via `xdg-open`, on Windows via `cmd /c start`.
///
/// `authorize_url_handler` lets the caller customize what happens when the URL
/// is ready (e.g. print it to stdout). If `None`, the default is to print + open.
pub async fn run_login_flow(cfg: OAuthConfig) -> CcResult<std::path::PathBuf> {
    let verifier = generate_code_verifier();
    let challenge = generate_code_challenge(&verifier);
    let state = generate_state();
    let (listener, port) = start_callback_listener().await?;
    let auth_url = build_authorize_url(&cfg, &challenge, &state, port);

    println!("Opening your browser to:\n  {auth_url}\n");
    println!("If the browser doesn't open automatically, copy the URL above.");
    println!("Waiting for login... (timeout: 10 minutes)");
    let _ = open_in_browser(&auth_url);

    let code = wait_for_callback(listener, &state, Duration::from_secs(600)).await?;
    let http = reqwest::Client::new();
    let tokens = exchange_code_for_tokens(&cfg, &http, &code, &verifier, &state, port).await?;

    let expires_at = tokens
        .expires_in
        .map(|s| std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64 + s * 1000)
            .unwrap_or(0));
    let path = crate::file_store::write_oauth_token(
        tokens.access_token,
        tokens.refresh_token,
        expires_at,
    )?;
    Ok(path)
}

fn open_in_browser(url: &str) -> std::io::Result<std::process::ExitStatus> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/open").arg(url).status()
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open").arg(url).status()
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd").args(["/c", "start", "", url]).status()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "unsupported platform for browser open",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_verifier_is_43_chars_base64url() {
        let v = generate_code_verifier();
        // 32 bytes base64url-no-pad = 43 chars
        assert_eq!(v.len(), 43);
        assert!(v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn pkce_challenge_matches_rfc_test_vector() {
        // RFC 7636 Appendix B test vector
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = generate_code_challenge(verifier);
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn state_values_differ_per_call() {
        let a = generate_state();
        let b = generate_state();
        assert_ne!(a, b);
    }

    #[test]
    fn authorize_url_includes_required_params() {
        let cfg = OAuthConfig::default();
        let url = build_authorize_url(&cfg, "chal", "st", 54321);
        assert!(url.contains("client_id="));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A54321%2Fcallback"));
        assert!(url.contains("code_challenge=chal"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=st"));
        assert!(url.contains("scope="));
    }

    #[tokio::test]
    async fn callback_listener_captures_code() {
        let (listener, port) = start_callback_listener().await.unwrap();
        let state = "my-state".to_string();
        let url = format!("http://localhost:{port}/callback?code=abc123&state={state}");

        // Drive a fake browser hit in parallel.
        let client = reqwest::Client::new();
        let state_clone = state.clone();
        let hit = tokio::spawn(async move {
            // Tiny delay so the listener is accepting first.
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = client.get(&url).send().await;
            state_clone
        });

        let code = wait_for_callback(listener, &state, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(code, "abc123");
        hit.await.unwrap();
    }

    #[tokio::test]
    async fn callback_listener_rejects_bad_state() {
        let (listener, port) = start_callback_listener().await.unwrap();

        let client = reqwest::Client::new();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = client
                .get(format!("http://localhost:{port}/callback?code=x&state=wrong"))
                .send()
                .await;
        });

        let err = wait_for_callback(listener, "right", Duration::from_secs(2))
            .await
            .unwrap_err();
        match err {
            CcError::Auth(msg) => assert!(msg.contains("state mismatch"), "got: {msg}"),
            other => panic!("expected Auth error, got {other:?}"),
        }
    }
}
