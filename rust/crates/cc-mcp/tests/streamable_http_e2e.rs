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

/// Write an SSE response with an explicit `id:` line for the final event.
async fn write_sse_with_event_id(
    sock: &mut TcpStream,
    event_id: &str,
    body_json: &Value,
) {
    let payload = format!(
        "event: message\nid: {event_id}\ndata: {}\n\n",
        serde_json::to_string(body_json).unwrap()
    );
    let resp = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/event-stream\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        payload.len(),
        payload
    );
    let _ = sock.write_all(resp.as_bytes()).await;
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
async fn last_event_id_captured_from_sse_response() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());

    let server = tokio::spawn(async move {
        // 1. initialize — plain JSON response.
        let (mut sock, _) = listener.accept().await.unwrap();
        let (_h, req) = read_request(&mut sock).await;
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

        // 2. notifications/initialized 202.
        let (mut sock, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut sock).await;
        write_empty_202(&mut sock).await;

        // 3. tools/list — SSE with an explicit event id.
        let (mut sock, _) = listener.accept().await.unwrap();
        let (_h, req) = read_request(&mut sock).await;
        let id = req["id"].as_u64().unwrap_or(0);
        write_sse_with_event_id(
            &mut sock,
            "evt-42",
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"tools": []}
            }),
        )
        .await;
    });

    let mut client = McpHttpClient::connect("stub", &url).await.expect("connect");
    // No SSE response seen yet.
    assert_eq!(client.last_event_id().await, None);
    let _ = client.list_tools().await.expect("list_tools");
    // After the SSE response, the event id is captured.
    assert_eq!(client.last_event_id().await.as_deref(), Some("evt-42"));
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

/// After a 404 session-expiry, the client must transparently replay
/// `initialize` + `notifications/initialized` and retry the failed
/// request exactly once. Caller sees a normal success — no visible 404.
#[tokio::test]
async fn session_404_triggers_reconnect_and_retry() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    const SESSION_ID_V1: &str = "sess-expiring-soon";
    const SESSION_ID_V2: &str = "sess-fresh-after-reconnect";

    let server = tokio::spawn(async move {
        // 1. initial `initialize` sets session id v1.
        let (mut sock, _) = listener.accept().await.unwrap();
        let (_headers, req) = read_request(&mut sock).await;
        let id = req["id"].as_u64().unwrap_or(0);
        write_response(
            &mut sock,
            200,
            &[("Mcp-Session-Id", SESSION_ID_V1)],
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

        // 3. tools/list with v1 → 404 (session expired).
        let (mut sock, _) = listener.accept().await.unwrap();
        let (headers, req) = read_request(&mut sock).await;
        assert_eq!(req["method"], "tools/list");
        assert_eq!(
            headers.get("mcp-session-id").map(String::as_str),
            Some(SESSION_ID_V1),
            "first tools/list must carry the original (soon-to-be-expired) session ID"
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

        // 4. The client should now auto-reconnect: a fresh `initialize`
        //    with no session ID, for which we hand back v2.
        let (mut sock, _) = listener.accept().await.unwrap();
        let (headers, req) = read_request(&mut sock).await;
        assert_eq!(req["method"], "initialize", "reconnect must replay initialize");
        assert!(
            !headers.contains_key("mcp-session-id"),
            "reconnected initialize must not carry the expired session id"
        );
        let id = req["id"].as_u64().unwrap_or(0);
        write_response(
            &mut sock,
            200,
            &[("Mcp-Session-Id", SESSION_ID_V2)],
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

        // 5. notifications/initialized (post-reconnect) — carries v2.
        let (mut sock, _) = listener.accept().await.unwrap();
        let (headers, _) = read_request(&mut sock).await;
        assert_eq!(
            headers.get("mcp-session-id").map(String::as_str),
            Some(SESSION_ID_V2)
        );
        write_empty_202(&mut sock).await;

        // 6. The retried tools/list — carries v2 and succeeds.
        let (mut sock, _) = listener.accept().await.unwrap();
        let (headers, req) = read_request(&mut sock).await;
        assert_eq!(req["method"], "tools/list");
        assert_eq!(
            headers.get("mcp-session-id").map(String::as_str),
            Some(SESSION_ID_V2),
            "retry must carry the new session id from the reconnect"
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
    assert_eq!(client.session_id().await.as_deref(), Some(SESSION_ID_V1));
    assert_eq!(client.session_version(), 0);

    // tools/list triggers a 404 → reconnect → retry → success.
    // Caller sees a successful response, not the transient 404.
    let tools = client.list_tools().await.expect("tools/list after reconnect");
    assert!(tools.is_empty());

    // Session state reflects the reconnect: new id + bumped version.
    assert_eq!(client.session_id().await.as_deref(), Some(SESSION_ID_V2));
    assert_eq!(
        client.session_version(),
        1,
        "exactly one reconnect must have bumped the version"
    );
    server.await.unwrap();
}

/// Two consecutive 404s: the first triggers a reconnect + retry, the
/// retry also 404s. That second error must surface to the caller — we
/// don't want an infinite reconnect loop.
#[tokio::test]
async fn second_404_in_a_row_surfaces_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    const SESSION_ID_V1: &str = "sess-v1";
    const SESSION_ID_V2: &str = "sess-v2";

    let server = tokio::spawn(async move {
        // initialize v1
        let (mut sock, _) = listener.accept().await.unwrap();
        let (_h, req) = read_request(&mut sock).await;
        let id = req["id"].as_u64().unwrap_or(0);
        write_response(
            &mut sock,
            200,
            &[("Mcp-Session-Id", SESSION_ID_V1)],
            &json!({
                "jsonrpc":"2.0","id":id,
                "result":{"protocolVersion":"2024-11-05","capabilities":{},
                    "serverInfo":{"name":"stub","version":"0.0.1"}}
            }),
        ).await;

        // initialized notif
        let (mut sock, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut sock).await;
        write_empty_202(&mut sock).await;

        // tools/list → 404
        let (mut sock, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut sock).await;
        let body_str = r#"{"jsonrpc":"2.0","error":{"code":-32001,"message":"expired"}}"#;
        let _ = sock.write_all(
            format!("HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body_str}",
                    body_str.len()).as_bytes()).await;
        let _ = sock.shutdown().await;

        // reconnect initialize v2
        let (mut sock, _) = listener.accept().await.unwrap();
        let (_h, req) = read_request(&mut sock).await;
        let id = req["id"].as_u64().unwrap_or(0);
        write_response(
            &mut sock,
            200,
            &[("Mcp-Session-Id", SESSION_ID_V2)],
            &json!({
                "jsonrpc":"2.0","id":id,
                "result":{"protocolVersion":"2024-11-05","capabilities":{},
                    "serverInfo":{"name":"stub","version":"0.0.1"}}
            }),
        ).await;

        // reconnect notif
        let (mut sock, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut sock).await;
        write_empty_202(&mut sock).await;

        // retried tools/list → 404 AGAIN.
        let (mut sock, _) = listener.accept().await.unwrap();
        let (headers, _) = read_request(&mut sock).await;
        assert_eq!(
            headers.get("mcp-session-id").map(String::as_str),
            Some(SESSION_ID_V2),
            "retry carries the post-reconnect session id"
        );
        let _ = sock.write_all(
            format!("HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body_str}",
                    body_str.len()).as_bytes()).await;
        let _ = sock.shutdown().await;
    });

    let mut client = McpHttpClient::connect("stub", &url).await.expect("connect");
    let err = client.list_tools().await.expect_err("second 404 must surface");
    assert!(err.contains("session expired") || err.contains("404"), "got: {err}");
    // The doomed v2 session id is dropped so the *next* caller starts fresh.
    assert_eq!(
        client.session_id().await,
        None,
        "stale session id from failed reconnect must be cleared"
    );
    // Exactly one reconnect attempt was made.
    assert_eq!(client.session_version(), 1);
    server.await.unwrap();
}

/// Five in-flight `tools/call` requests race into a 404 at the same time.
/// Only one reconnect (`initialize` + `notifications/initialized`) must
/// happen; the other four tasks observe the version bump and retry with
/// the fresh session id. All five must ultimately succeed.
#[tokio::test]
async fn concurrent_404s_trigger_only_one_reconnect() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    const SESSION_ID_V1: &str = "sess-v1";
    const SESSION_ID_V2: &str = "sess-v2";
    const CONCURRENCY: usize = 5;

    // Count how many `initialize` calls the server handles. We assert
    // this ends at 2 — the original connect + exactly one reconnect —
    // regardless of how many tools/call requests concurrently 404.
    let initialize_count = Arc::new(AtomicUsize::new(0));
    let init_count_server = Arc::clone(&initialize_count);

    let server = tokio::spawn(async move {
        // Initial `initialize` hands back v1.
        let (mut sock, _) = listener.accept().await.unwrap();
        let (_h, req) = read_request(&mut sock).await;
        assert_eq!(req["method"], "initialize");
        init_count_server.fetch_add(1, Ordering::SeqCst);
        let id = req["id"].as_u64().unwrap_or(0);
        write_response(
            &mut sock,
            200,
            &[("Mcp-Session-Id", SESSION_ID_V1)],
            &json!({
                "jsonrpc":"2.0","id":id,
                "result":{"protocolVersion":"2024-11-05","capabilities":{},
                    "serverInfo":{"name":"stub","version":"0.0.1"}}
            }),
        ).await;
        // initialized notif
        let (mut sock, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut sock).await;
        write_empty_202(&mut sock).await;

        // Now the main phase: five concurrent tools/call requests arrive
        // carrying v1. Accept all five, buffer the sockets, then respond
        // 404 to each. This maximises the chance that all five are in
        // flight *before* the client observes the first 404 — the
        // exact race the coordinator is supposed to win.
        let mut pending_v1: Vec<TcpStream> = Vec::with_capacity(CONCURRENCY);
        for _ in 0..CONCURRENCY {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (headers, req) = read_request(&mut sock).await;
            assert_eq!(req["method"], "tools/call");
            assert_eq!(
                headers.get("mcp-session-id").map(String::as_str),
                Some(SESSION_ID_V1),
                "pre-404 requests all carry the old session id"
            );
            pending_v1.push(sock);
        }
        // Now respond 404 to all five simultaneously.
        for mut sock in pending_v1 {
            let body_str = r#"{"jsonrpc":"2.0","error":{"code":-32001,"message":"expired"}}"#;
            let _ = sock.write_all(
                format!("HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body_str}",
                        body_str.len()).as_bytes()).await;
            let _ = sock.shutdown().await;
        }

        // Exactly one of the five clients will drive the reconnect;
        // the others wait. The server sees a single `initialize` +
        // `notifications/initialized` pair.
        let (mut sock, _) = listener.accept().await.unwrap();
        let (headers, req) = read_request(&mut sock).await;
        assert_eq!(req["method"], "initialize", "reconnect must replay initialize");
        assert!(
            !headers.contains_key("mcp-session-id"),
            "reconnect must not carry the stale session id"
        );
        init_count_server.fetch_add(1, Ordering::SeqCst);
        let id = req["id"].as_u64().unwrap_or(0);
        write_response(
            &mut sock,
            200,
            &[("Mcp-Session-Id", SESSION_ID_V2)],
            &json!({
                "jsonrpc":"2.0","id":id,
                "result":{"protocolVersion":"2024-11-05","capabilities":{},
                    "serverInfo":{"name":"stub","version":"0.0.1"}}
            }),
        ).await;
        let (mut sock, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut sock).await;
        write_empty_202(&mut sock).await;

        // Each of the five tasks retries its tools/call — now with v2.
        for _ in 0..CONCURRENCY {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (headers, req) = read_request(&mut sock).await;
            assert_eq!(req["method"], "tools/call");
            assert_eq!(
                headers.get("mcp-session-id").map(String::as_str),
                Some(SESSION_ID_V2),
                "post-reconnect retries must carry the fresh session id"
            );
            let id = req["id"].as_u64().unwrap_or(0);
            write_response(
                &mut sock,
                200,
                &[],
                &json!({
                    "jsonrpc":"2.0","id":id,
                    "result":{"content":[{"type":"text","text":"ok"}]}
                }),
            ).await;
        }
    });

    let client = Arc::new(McpHttpClient::connect("stub", &url).await.expect("connect"));
    assert_eq!(client.session_id().await.as_deref(), Some(SESSION_ID_V1));
    assert_eq!(client.session_version(), 0);

    // Kick off CONCURRENCY tools/call tasks simultaneously. Using
    // `tools/call` instead of `tools/list` because the latter takes
    // `&mut self` and would serialise; `McpHttpClient::call` takes
    // `&self` and is the path used by the real transport adapter.
    let mut tasks = Vec::new();
    for i in 0..CONCURRENCY {
        let c = Arc::clone(&client);
        tasks.push(tokio::spawn(async move {
            let params = json!({"name": format!("t{i}"), "arguments": {}});
            c.call("tools/call", Some(params)).await
        }));
    }
    for t in tasks {
        let resp = t.await.unwrap().expect("tools/call after reconnect");
        assert!(resp.error.is_none(), "unexpected JSON-RPC error: {:?}", resp.error);
    }

    // Final state: exactly two `initialize` calls hit the server
    // (original connect + one reconnect), session version bumped once,
    // fresh session id installed.
    assert_eq!(initialize_count.load(Ordering::SeqCst), 2,
        "must be exactly one reconnect for the whole 5-way race");
    assert_eq!(client.session_version(), 1);
    assert_eq!(client.session_id().await.as_deref(), Some(SESSION_ID_V2));
    server.await.unwrap();
}
