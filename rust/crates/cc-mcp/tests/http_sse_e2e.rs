//! End-to-end test for the MCP HTTP/SSE transport.
//!
//! Spins up a tiny TCP server that pretends to be an MCP server speaking
//! Streamable HTTP. It handles three requests in sequence: `initialize`,
//! `tools/list`, then `tools/call`. The first response is sent as plain JSON;
//! the second as SSE (to exercise the SSE parsing path); the third as JSON.
//!
//! This proves M4 exit criterion 2: the MCP HTTP SSE transport connects to a
//! remote MCP server and completes a tool call.

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use cc_mcp::McpHttpClient;

/// Read one HTTP request (headers + body) from `sock`. Returns the JSON-RPC body.
async fn read_request(sock: &mut tokio::net::TcpStream) -> Value {
    let mut buf = [0u8; 8192];
    let mut total = Vec::new();

    // Read until we have headers + body. Cheap heuristic: read until we see
    // \r\n\r\n, then parse Content-Length and read the rest.
    loop {
        let n = sock.read(&mut buf).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        total.extend_from_slice(&buf[..n]);
        if let Some(headers_end) = find_subseq(&total, b"\r\n\r\n") {
            let header_str = std::str::from_utf8(&total[..headers_end]).unwrap_or("");
            let content_length: usize = header_str
                .lines()
                .find_map(|line| {
                    let lower = line.to_ascii_lowercase();
                    lower
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap_or(0))
                })
                .unwrap_or(0);
            let body_start = headers_end + 4;
            let have_body = total.len().saturating_sub(body_start);
            if have_body >= content_length {
                let body = &total[body_start..body_start + content_length];
                return serde_json::from_slice(body).unwrap_or(Value::Null);
            }
        }
    }
    Value::Null
}

fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

async fn write_json_response(sock: &mut tokio::net::TcpStream, body: &Value) {
    let body_str = serde_json::to_string(body).unwrap();
    let resp = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body_str.len(),
        body_str
    );
    let _ = sock.write_all(resp.as_bytes()).await;
    let _ = sock.shutdown().await;
}

async fn write_sse_response(sock: &mut tokio::net::TcpStream, body: &Value) {
    let payload = format!("event: message\ndata: {}\n\n", body);
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
async fn http_sse_initialize_list_call_roundtrip() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");

    // Server task: handle exactly four connections in order:
    //   1. initialize          → JSON response
    //   2. notifications/initialized → 202 (no body)
    //   3. tools/list          → SSE response
    //   4. tools/call          → JSON response
    tokio::spawn(async move {
        // 1. initialize
        let (mut sock, _) = listener.accept().await.unwrap();
        let req = read_request(&mut sock).await;
        assert_eq!(req["method"], "initialize");
        let id = req["id"].as_u64().unwrap_or(0);
        write_json_response(
            &mut sock,
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

        // 2. notifications/initialized — accept and 202.
        let (mut sock, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut sock).await;
        let _ = sock
            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        let _ = sock.shutdown().await;

        // 3. tools/list — reply via SSE.
        let (mut sock, _) = listener.accept().await.unwrap();
        let req = read_request(&mut sock).await;
        assert_eq!(req["method"], "tools/list");
        let id = req["id"].as_u64().unwrap_or(0);
        write_sse_response(
            &mut sock,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [
                        {
                            "name": "echo",
                            "description": "echoes input",
                            "inputSchema": {
                                "type": "object",
                                "properties": {"text": {"type": "string"}}
                            }
                        }
                    ]
                }
            }),
        )
        .await;

        // 4. tools/call — JSON response.
        let (mut sock, _) = listener.accept().await.unwrap();
        let req = read_request(&mut sock).await;
        assert_eq!(req["method"], "tools/call");
        assert_eq!(req["params"]["name"], "echo");
        assert_eq!(req["params"]["arguments"]["text"], "hi there");
        let id = req["id"].as_u64().unwrap_or(0);
        write_json_response(
            &mut sock,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [
                        {"type": "text", "text": "hi there"}
                    ],
                    "isError": false
                }
            }),
        )
        .await;
    });

    let mut client = McpHttpClient::connect("stub", &url).await.expect("connect");
    let tools = client.list_tools().await.expect("list_tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");
    assert_eq!(tools[0].server_name, "stub");

    let result = client.call_tool("echo", json!({"text": "hi there"})).await;
    assert!(!result.is_error);
    assert_eq!(result.content, "hi there");
}
