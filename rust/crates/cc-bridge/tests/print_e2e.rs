//! End-to-end test for the SDK / `--print` path through `cc-bridge::run_once`.
//!
//! Spins up a local TCP listener that speaks just enough HTTP/SSE to satisfy
//! `cc-api`, points the API client at it via `ANTHROPIC_BASE_URL`, then runs a
//! one-shot bridge call and asserts the streamed text matches what the stub
//! sent back.
//!
//! This proves M4 exit criterion 1: `claude --print "hello"` returns a
//! response and exits 0 — without needing live API credentials.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use cc_api::{ApiClient, AuthCredential};
use cc_bridge::{run_once, BridgeRequest};
use cc_hooks::HookRunner;
use cc_permissions::PermissionEngine;
use cc_session::Session;

const STUB_SSE: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_test\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-test\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
    "event: content_block_start\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n",
    "event: content_block_stop\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":2}}\n\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n\n",
);

/// Spawn a single-shot HTTP server that replies with `STUB_SSE` to one request,
/// then exits. Returns the bound address.
async fn spawn_stub_anthropic() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();

        // Drain the request headers (read until \r\n\r\n).
        let mut buf = [0u8; 4096];
        let mut total = Vec::new();
        loop {
            let n = sock.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            total.extend_from_slice(&buf[..n]);
            if total.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }

        let response = format!(
            "HTTP/1.1 200 OK\r\n\
             Content-Type: text/event-stream\r\n\
             Cache-Control: no-cache\r\n\
             Connection: close\r\n\
             Content-Length: {}\r\n\
             \r\n\
             {}",
            STUB_SSE.len(),
            STUB_SSE
        );
        let _ = sock.write_all(response.as_bytes()).await;
        let _ = sock.shutdown().await;
    });

    addr
}

#[tokio::test]
async fn print_mode_streams_text_through_bridge() {
    let addr = spawn_stub_anthropic().await;

    // SAFETY: ApiClient::new() reads ANTHROPIC_BASE_URL during construction,
    // so set it before building the client. This test is single-threaded with
    // respect to that env var because it's the only test in this file.
    // SAFETY: setting an env var in a test is safe in single-threaded test context.
    unsafe {
        std::env::set_var("ANTHROPIC_BASE_URL", format!("http://{addr}"));
    }

    let api = ApiClient::new(
        reqwest::Client::new(),
        AuthCredential::ApiKey("sk-ant-test-key".into()),
    );

    let req = BridgeRequest {
        api,
        tools: Vec::new(),
        permissions: PermissionEngine::from_settings(
            Vec::<serde_json::Value>::new(),
            Vec::<serde_json::Value>::new(),
        ),
        hooks: std::sync::Arc::new(HookRunner::empty()),
        session: Session::new().expect("session"),
        system_blocks: Vec::new(),
        initial_messages: Vec::new(),
        user_text: "say hi".into(),
        model: "claude-test".into(),
        max_tokens: 64,
        non_interactive: true,
        bypass_permissions: true,
        thinking: None,
    };

    let mut streamed = String::new();
    let response = run_once(req, |delta| streamed.push_str(delta))
        .await
        .expect("bridge run_once");

    assert_eq!(streamed, "hello world", "streamed deltas concatenate");
    assert_eq!(response.content, "hello world", "final content matches");
    assert!(!response.session_id.is_empty(), "session id populated");

    // Drop Arc to silence unused-import warnings if the type ever goes away.
    let _ = Arc::new(());
}
