## 1. SSRF Guard Module

- [x] 1.1 New file `cc-tools/src/web_fetch/ssrf.rs` exporting
      `pub async fn guard_url(url: &Url) -> Result<IpAddr, CcError>`.
- [x] 1.2 Implement private-range checks for IPv4 and IPv6 using stable
      `Ipv4Addr::is_loopback` / `is_link_local` / `is_private` plus
      explicit bans on `169.254.169.254`, `100.64.0.0/10`,
      `fc00::/7`, `fe80::/10`.
- [x] 1.3 Respect `CC_WEBFETCH_ALLOW_PRIVATE=1` to bypass for local dev
      **and emit `tracing::warn!` naming the bypassed host** so the
      transparency the spec requires is observable in logs.
      (QA 2026-04-18: env-var handling landed in `ssrf.rs:68-77`, but no
      `warn!` is emitted; spec Scenario "Developer opt-out" explicitly
      requires the log. Reverting to [ ].)
      Fixed 2026-04-18: `tracing::warn!` now fires on both the literal-IP
      and DNS opt-out paths, naming host + resolved IP.
- [x] 1.4 Unit tests covering: public DNS name (pass), `localhost` (fail),
      `127.0.0.1` (fail), `169.254.169.254` (fail), `10.0.0.1` (fail),
      `[::1]` (fail), opt-out env var (pass **and assert the warn! was
      logged**, e.g. via `tracing_test::traced_test`).
      (QA 2026-04-18: IP/DNS tests pass; warn-log assertion depends on 1.3
      and is not present. Reverting to [ ].)
      Fixed 2026-04-18: added `opt_out_emits_warn_log` +
      `public_host_does_not_emit_bypass_warn` using
      `#[tracing_test::traced_test]` / `logs_contain`.

## 2. Integrate Into web_fetch

- [x] 2.1 Call `guard_url` at the top of `WebFetchTool::execute` after
      `is_allowed_url`; on error return `ToolResult::error` with message:
      `"refused to fetch private/internal address <host>; set
      CC_WEBFETCH_ALLOW_PRIVATE=1 to override"`.
- [x] 2.2 Use the resolved IP to build the outbound request (`reqwest`
      `.resolve(host, SocketAddr)`) so DNS rebinding between the check and
      the fetch cannot swap targets.

## 3. Apply To web_search

- [x] 3.1 When following an individual search result URL, same guard
      applies. Failing results are skipped (not fatal).
      (QA 2026-04-18: `web_search.rs:97-108` guards the Brave API endpoint
      itself — the only outbound URL. The tool does not currently follow
      per-result URLs, so the spec scenario is vacuously satisfied. If
      result-URL following is added later, the guard MUST be reapplied.)

## 4. Regression Tests

- [x] 4.1 Integration test that boots a local HTTP server on a random
      loopback port and asserts WebFetch refuses it (unless opt-out).
- [x] 4.2 Integration test that asserts a request to
      `http://169.254.169.254/latest/meta-data/` is refused before any
      socket is opened (use a `reqwest` middleware / counter).

## 5. Docs + Sign-off

- [ ] 5.1 Mention `CC_WEBFETCH_ALLOW_PRIVATE` in `RUST_REWRITE_PLAN.md`
      implementation-notes.
- [x] 5.2 `cargo test -p cc-tools` + `cargo clippy --workspace -- -D warnings`
      clean.
