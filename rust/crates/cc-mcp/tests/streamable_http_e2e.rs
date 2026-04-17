//! Streamable-HTTP-specific integration tests for `McpHttpClient`:
//! session ID capture + replay, session expiry reset on 404, and custom
//! Authorization headers from per-server config.

use std::collections::HashMap;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use cc_mcp::{McpHttpClient, McpTransport};

/// Parse one HTTP request off `sock`. Returns (headers_lowercased, json_body).
/// Crude but sufficient for test fixtures — no chunked encoding, tiny bodies.
async fn read_request(sock: &mut TcpStream) -> (HashMap<String, String>, Value) {
    let mut buf = [0u8; 8192];
    let mut total = Vec::new();

    loop {
        let n = sock.read(&mut buf).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        total.extend_from_slice(&buf[..n]);
        if let Some(headers_end) = find_subseq(&total, b"\r\n\r\n") {
            let header_str = std::str::from_utf8(&total[..headers_end]).unwrap_or("");
            let mut headers = HashMap::new();
            let mut content_length: usize = 0;
            for line in header_str.lines().skip(1) {
                if let Some((k, v)) = line.split_once(':') {
                    let key = k.trim().to_ascii_lowercase();
                    let val = v.trim().to_string();
                    if key == "content-length" {
                        content_length = val.parse().unwrap_or(0);
                    }
                    headers.insert(key, val);
                }
            }
            let body_start = headers_end + 4;
            if total.len().saturating_sub(body_start) >= content_length {
                let body = &total[body_start..body_start + content_length];
                let v = serde_json::from_slice(body).unwrap_or(Value::Null);
                return (headers, v);
            }
        }
    }
    (HashMap::new(), Value::Null)
}

fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

async fn write_response(
    sock: &mut TcpStream,
    status: u16,
    extra_headers: &[(&str, &str)],
    body: &Value,
) {
    let body_str = serde_json::to_string(body).unwrap();
    let mut headers = format!(
        "HTTP/1.1 {status} OK\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n",
        body_str.len()
    );
    for (k, v) in extra_headers {
        headers.push_str(&format!("{k}: {v}\r\n"));
    }
    headers.push_str("\r\n");
    let _ = sock.write_all(headers.as_bytes()).await;
    let _ = sock.write_all(body_str.as_bytes()).await;
    let _ = sock.shutdown().await;
}

async fn write_empty_202(sock: &mut TcpStream) {
    let _ = sock
        .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .await;
    let _ = sock.shutdown().await;
}

#[tokio::test]
async fn session_id_from_initialize_is_replayed_on_next_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    const SESSION_ID: &str = "sess-abc-123";

    let server = tokio::spawn(async move {
        // 1. initialize — respond with session header.
        let (mut sock, _) = listener.accept().await.unwrap();
        let (headers, req) = read_request(&mut sock).await;
        assert_eq!(req["method"], "initialize");
        // No session ID on the first request (fresh client).
        assert!(!headers.contains_key("mcp-session-id"));
        // Protocol version header required on every request.
        assert_eq!(
            headers.get("mcp-protocol-version").map(String::as_str),
            Some("2024-11-05")
        );
        let id = req["id"].as_u64().unwrap_or(0);
        write_response(
            &mut sock,
            200,
            &[("Mcp-Session-Id", SESSION_ID)],
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "serverInfo": {"name": "stub", "version": "0.0.1"}
                }
            }),
        )
        .await;

        // 2. notifications/initialized — should echo the session ID.
        let (mut sock, _) = listener.accept().await.unwrap();
        let (headers, _) = read_request(&mut sock).await;
        assert_eq!(
            headers.get("mcp-session-id").map(String::as_str),
            Some(SESSION_ID)
        );
        write_empty_202(&mut sock).await;

        // 3. tools/list — session ID must still be present.
        let (mut sock, _) = listener.accept().await.unwrap();
        let (headers, req) = read_request(&mut sock).await;
        assert_eq!(req["method"], "tools/list");
        assert_eq!(
            headers.get("mcp-session-id").map(String::as_str),
            Some(SESSION_ID),
            "session ID must be replayed on every request after initialize"
        );
        let id = req["id"].as_u64().unwrap_or(0);
        write_response(
            &mut sock,
            200,
            &[],
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"tools": []}
            }),
        )
        .await;
    });

    let mut client = McpHttpClient::connect("stub", &url).await.expect("connect");
    assert_eq!(client.session_id().await.as_deref(), Some(SESSION_ID));
    let _ = client.list_tools().await.expect("list_tools");
    server.await.unwrap();
}

#[tokio::test]
async fn custom_headers_from_config_reach_server() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());

    let server = tokio::spawn(async move {
        // initialize
        let (mut sock, _) = listener.accept().await.unwrap();
        let (headers, req) = read_request(&mut sock).await;
        assert_eq!(req["method"], "initialize");
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some("Bearer secret-token-xyz"),
            "Authorization header from config must reach the server"
        );
        assert_eq!(
            headers.get("x-api-version").map(String::as_str),
            Some("2025-01"),
        );
        let id = req["id"].as_u64().unwrap_or(0);
        write_response(
            &mut sock,
            200,
            &[],
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "serverInfo": {"name": "stub", "version": "0.0.1"}
                }
            }),
        )
        .await;

        // notifications/initialized — also must carry the auth header.
        let (mut sock, _) = listener.accept().await.unwrap();
        let (headers, _) = read_request(&mut sock).await;
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some("Bearer secret-token-xyz")
        );
        write_empty_202(&mut sock).await;
    });

    let mut extra = HashMap::new();
    extra.insert("Authorization".to_string(), "Bearer secret-token-xyz".to_string());
    extra.insert("X-Api-Version".to_string(), "2025-01".to_string());

    let _client = McpHttpClient::connect_with_headers("stub", &url, extra)
        .await
        .expect("connect");
    server.await.unwrap();
}

#[tokio::test]
async fn close_sends_delete_with_session_id() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    const SESSION_ID: &str = "sess-to-be-killed";

    let server = tokio::spawn(async move {
        // 1. initialize — hand back a session ID.
        let (mut sock, _) = listener.accept().await.unwrap();
        let (_h, req) = read_request(&mut sock).await;
        let id = req["id"].as_u64().unwrap_or(0);
        write_response(
            &mut sock,
            200,
            &[("Mcp-Session-Id", SESSION_ID)],
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "serverInfo": {"name": "stub", "version": "0.0.1"}
                }
            }),
        )
        .await;

        // 2. notifications/initialized 202.
        let (mut sock, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut sock).await;
        write_empty_202(&mut sock).await;

        // 3. Expect a DELETE with the session ID.
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let mut total = Vec::new();
        loop {
            let n = sock.read(&mut buf).await.unwrap_or(0);
            if n == 0 { break; }
            total.extend_from_slice(&buf[..n]);
            if find_subseq(&total, b"\r\n\r\n").is_some() { break; }
        }
        let request = std::str::from_utf8(&total).unwrap_or("");
        assert!(
            request.starts_with("DELETE "),
            "expected DELETE request, got: {}",
            request.lines().next().unwrap_or("")
        );
        assert!(
            request.to_ascii_lowercase().contains(&format!("mcp-session-id: {SESSION_ID}").to_ascii_lowercase()),
            "DELETE must include Mcp-Session-Id header"
        );
        let _ = sock
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        let _ = sock.shutdown().await;
    });

    let client = McpHttpClient::connect("stub", &url).await.expect("connect");
    assert_eq!(client.session_id().await.as_deref(), Some(SESSION_ID));

    // Call close via the McpTransport trait — this is what McpManager::shutdown does.
    let transport = tokio::sync::Mutex::new(client);
    transport.close().await.expect("close");

    // After close, the session ID must be cleared.
    let client = transport.into_inner();
    assert_eq!(client.session_id().await, None);

    server.await.unwrap();
}

#[tokio::test]
async fn session_404_clears_stored_session_id() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    const SESSION_ID: &str = "sess-expiring-soon";

    let server = tokio::spawn(async move {
        // 1. initialize sets a session ID.
        let (mut sock, _) = listener.accept().await.unwrap();
        let (_headers, req) = read_request(&mut sock).await;
        let id = req["id"].as_u64().unwrap_or(0);
        write_response(
            &mut sock,
            200,
            &[("Mcp-Session-Id", SESSION_ID)],
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "serverInfo": {"name": "stub", "version": "0.0.1"}
                }
            }),
        )
        .await;

        // 2. notifications/initialized 202.
        let (mut sock, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut sock).await;
        write_empty_202(&mut sock).await;

        // 3. tools/list gets 404 (session expired).
        let (mut sock, _) = listener.accept().await.unwrap();
        let (headers, _) = read_request(&mut sock).await;
        // Client should still be sending the old session ID on this request.
        assert_eq!(
            headers.get("mcp-session-id").map(String::as_str),
            Some(SESSION_ID)
        );
        let body = json!({"jsonrpc":"2.0","error":{"code":-32001,"message":"Session expired"}});
        let body_str = body.to_string();
        let _ = sock
            .write_all(
                format!(
                    "HTTP/1.1 404 Not Found\r\n\
                     Content-Type: application/json\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\r\n\
                     {body_str}",
                    body_str.len()
                )
                .as_bytes(),
            )
            .await;
        let _ = sock.shutdown().await;
    });

    let mut client = McpHttpClient::connect("stub", &url).await.expect("connect");
    assert_eq!(client.session_id().await.as_deref(), Some(SESSION_ID));

    // tools/list returns 404 → error surfaces, session_id is cleared.
    let err = client.list_tools().await.expect_err("expected 404 error");
    assert!(err.contains("session expired") || err.contains("404"), "got: {err}");
    assert_eq!(
        client.session_id().await,
        None,
        "session_id must be cleared after 404 so the next call re-initializes"
    );
    server.await.unwrap();
}
