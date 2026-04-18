## Why

`cc-tools/src/web_fetch.rs:140-142` currently gatekeeps outbound requests
with a prefix-only check:

```rust
fn is_allowed_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}
```

This blocks `file://` / `ftp://` but leaves the entire RFC1918 / loopback /
link-local space reachable. In particular:

- `http://169.254.169.254/...` is the AWS/GCP/Azure instance metadata
  endpoint. A successful fetch returns short-lived IAM credentials.
- `http://127.0.0.1:6379` / `http://localhost:11434` exposes any local
  service (Redis, Ollama, internal admin UIs) with no authentication.
- `http://10.0.0.0/8` and `http://192.168.0.0/16` reach internal corporate
  networks from developer laptops that are VPN'd in.

The WebFetch tool runs inside the model's tool loop. A prompt-injection
payload in any fetched web page (issue tracker comment, README snippet,
search result) can request one of these URLs. With the current check the
fetch succeeds and the body is handed back to the model, which then
happily quotes the credentials into a tool_result — exfiltration in a
single turn.

This is a **security contract** gap, not a feature gap. No ambient permission
exists that was supposed to catch this.

## What Changes

- Introduce `cc-tools/src/web_fetch/ssrf.rs` (new module) that:
  - Parses the URL with `url::Url`.
  - Resolves the host to IP(s) via `tokio::net::lookup_host` (respects
    `/etc/hosts` and DNS) **before** the HTTP request fires.
  - Rejects any address matching: loopback (`127.0.0.0/8`, `::1`),
    link-local IPv4 (`169.254.0.0/16`, explicit block for
    `169.254.169.254`), link-local IPv6 (`fe80::/10`), unique-local IPv6
    (`fc00::/7`), RFC1918 (`10/8`, `172.16/12`, `192.168/16`), CGNAT
    (`100.64/10`), and unspecified (`0.0.0.0`, `::`).
- Integrate the guard into `WebFetchTool::execute` ahead of `reqwest::Client`.
- Return a `ToolResult::error` with a human-readable explanation (so the
  model understands why, rather than retrying with a different encoding).
- Provide an opt-out env var `CC_WEBFETCH_ALLOW_PRIVATE=1` for the narrow
  case of running against a local MCP server during development, gated
  behind explicit user action.
- DNS-rebinding guard: pin the resolved IP, and issue the actual request
  against that IP with a `Host:` header set from the original URL, so a
  rebind attack cannot swap the IP between check and fetch.
- Apply the same guard to `cc-tools::web_search` where the URL of each
  result is followed.

## Capabilities

### Modified Capabilities
- `tool-web-fetch`: WebFetch MUST reject requests whose resolved IP
  targets a loopback, link-local, cloud-metadata, or RFC1918 address
  unless `CC_WEBFETCH_ALLOW_PRIVATE=1` is set.

## Impact

- **Affected code:** `cc-tools/src/web_fetch.rs`, new
  `cc-tools/src/web_fetch/ssrf.rs`, `cc-tools/src/web_search.rs`.
- **Dependencies:** no new crate required (`url` and `tokio::net` already
  pulled in transitively). If we want `ipnet` for cleaner range checks
  we add it; otherwise hand-rolled `Ipv4Addr::is_*` methods suffice.
- **User-visible behavior:** any WebFetch to `localhost`, `127.*`,
  `169.254.*`, RFC1918, or IPv6-private now returns a clear error unless
  opted in. This is a **new** restriction; no prior workflow depended on
  it.
- **Risk:** LOW from a correctness angle (the code change is small and
  isolated). MEDIUM from an ergonomics angle — the dev opt-out flag and
  its documentation are the most important detail to get right.
