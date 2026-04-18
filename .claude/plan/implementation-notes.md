# Implementation Notes — Rust Rewrite

Discoveries made during coding that are not derivable from the TS source alone.
**Read this before starting any new Milestone session.**

---

## Auth / API Layer (cc-auth, cc-api)

### OAuth Bearer requires `anthropic-beta: oauth-2025-04-20`

When authenticating with a Claude.ai OAuth token (`Authorization: Bearer sk-ant-oat01-...`),
the request MUST include the beta header:

```
anthropic-beta: oauth-2025-04-20
```

Without it the API returns HTTP 401:
```json
{"type":"error","error":{"type":"authentication_error","message":"OAuth authentication is currently not supported."}}
```

**Where this is implemented:** `cc-api/src/client.rs` — `ANTHROPIC_BETAS_OAUTH` constant.
**Reference in TS source:** `src/constants/oauth.ts:36` — `OAUTH_BETA_HEADER = 'oauth-2025-04-20'`.

---

### macOS Keychain structure (NOT a bare API key)

The TS Claude Code stores credentials in macOS Keychain as a **JSON blob**, not a raw API key string.

| Field | Value |
|-------|-------|
| Service name | `Claude Code-credentials` |
| Account name | `$USER` (OS username, e.g. `shenghuikevin`) |
| Stored value | UTF-8 JSON string |

JSON shape (`SecureStorageData`):
```json
{
  "claudeAiOauth": {
    "accessToken":    "sk-ant-oat01-...",
    "refreshToken":   "sk-ant-ort01-...",
    "expiresAt":      1775586787576,
    "scopes":         ["user:inference", "user:file_upload", ...],
    "subscriptionType": "pro",
    "rateLimitTier":  "default_claude_ai"
  }
}
```

**Where this is implemented:** `cc-auth/src/keychain.rs`.
**Reference in TS source:** `src/utils/secureStorage/macOsKeychainHelpers.ts` (`getMacOsKeychainStorageServiceName`, `CREDENTIALS_SERVICE_SUFFIX`), `src/utils/secureStorage/macOsKeychainStorage.ts`.

First-access macOS keychain prompt adds ~6 s latency (security dialog). Subsequent reads: ~100–200 ms (OS cache).

---

### OAuth token scope for inference

The OAuth token must include the `user:inference` scope to call `/v1/messages`.
Check `scopes` array in the JSON above. Token managed by `claude /login`; do not manually create.

---

### Rate limits: `claude-sonnet-4-6` vs `claude-haiku-4-5-20251001`

`claude-sonnet-4-6` returns HTTP 429 (`rate_limit_error`) on Claude.ai OAuth subscriptions
under typical testing frequency. `claude-haiku-4-5-20251001` succeeds.

**This is an account-level rate limit, not a code defect.**

Implications:
- Tests that call the live API should prefer `claude-haiku-4-5-20251001` (or mock the HTTP layer).
- Production users with API keys (`ANTHROPIC_API_KEY`) are not affected — API key auth has separate rate limits.
- End-to-end integration tests should set `DEFAULT_TEST_MODEL=claude-haiku-4-5-20251001` or use `ANTHROPIC_API_KEY`.

---

### Auth header selection (two paths)

| Credential type | HTTP header sent |
|----------------|-----------------|
| `ANTHROPIC_API_KEY` env var | `x-api-key: sk-ant-api03-...` |
| OAuth token from Keychain | `Authorization: Bearer sk-ant-oat01-...` + beta header |

**Where this is implemented:** `cc-auth/src/lib.rs` (`Credentials` enum), `cc-api/src/client.rs` (`Auth` enum, `headers()`).

---

## Latency Observations (Milestone 1, M1 Mac, proxy at 127.0.0.1:1087)

| Path | First-token latency |
|------|-------------------|
| OAuth + proxy, haiku | ~2–3 s (includes keychain lookup) |
| `ANTHROPIC_API_KEY` + direct | expected < 800 ms (not yet measured) |

The 800 ms target in the plan assumes direct API key, no proxy.

---

## Workspace Layout Notes

- Main binary crate is named **`claude-cli`** (package name), binary target is **`claude`**.
  The name `cc` conflicts with the `cc` crate (C compiler wrapper) on crates.io.
- All 20 library crates live under `rust/crates/`.
- Phase 2–4 stubs have minimal `Cargo.toml` + empty `src/lib.rs`; fill them in per milestone.

---

## Milestone 2 Pre-Work Notes

See `RUST_REWRITE_PLAN.md § Milestone 2` and the dependency analysis in `state.md` for build order.

Key TS source files to read before implementing each crate:

| Crate | Primary TS source |
|-------|------------------|
| cc-permissions | `src/utils/permissions.ts` (lines 473–625), `src/types/permissions.ts` |
| cc-tools | `src/Tool.ts`, `src/tools/*.ts` (Bash, Read, Write, Edit, Glob, Grep, WebFetch, WebSearch) |
| cc-hooks | `src/utils/hooks.ts`, `src/schemas/hooks.js` |
| cc-git | `src/utils/git.ts` |
| cc-mcp | `src/services/mcp/client.ts`, `src/services/mcp/types.ts` |
| cc-memory | `src/memdir/memoryTypes.ts` |
| cc-session | `src/utils/sessionStorage.ts` (lines 1039–1065), `src/history.ts` (lines 219–225) |
| cc-tasks | `src/tasks.ts`, `src/Task.ts` |
| cc-agent | `src/tools/AgentTool/`, `src/coordinator/` |
| cc-query | `src/query.ts`, `src/QueryEngine.ts` |

---

## Session / Persistence Layer (cc-session)

### fsync contract

`cc-session::Session::append_entry` and `Session::write_metadata` MUST
call `sync_all()` after every write. The transcript JSONL is written
through a `SyncAll` trait helper (`write_line_and_sync`) specifically
so that removing the sync step trips the unit test
`write_line_and_sync_issues_one_fsync_per_append`. Metadata writes go
through `NamedTempFile::persist` which ordering-guarantees an atomic
rename after the sync.

**Why this matters:** RUST_REWRITE_PLAN.md §3 promises the transcript
is "persisted after each turn". Without `sync_all`, a crash between
the write and the kernel flush (power loss, OOM kill of the whole
session, kernel panic) drops the last N turns even though `.write()`
returned success. The failure mode is asymmetric — the caller believes
the write succeeded but resume will miss it.

**Do not** wrap the file in `BufWriter` or any layer that defers the
flush. The cost of fsync per-turn is ~100–300 µs on APFS and <1 ms
on ext4; turns are human-scale so it is imperceptible.

Reference: `openspec/changes/fix-session-writeln-fsync/`.

### Metadata atomic rename contract

`write_metadata` serializes JSON into a same-dir `NamedTempFile`, calls
`sync_all()`, then `persist(&path)`. A crash mid-write leaves either
the old metadata.json intact or the tempfile orphaned (harmless) —
`metadata.json` itself is never observed in a truncated state.

### WebFetch SSRF guard contract (cc-tools)

WebFetch rejects `localhost`, RFC1918 (10/8, 172.16/12, 192.168/16),
link-local (169.254/16 incl. the cloud metadata address 169.254.169.254),
CGNAT (100.64/10), IPv6 loopback (::1), IPv6 link-local (fe80::/10),
and IPv6 unique local (fc00::/7) **before** opening any outbound socket.
The resolved IP is pinned to the request via `reqwest::ClientBuilder::resolve`
so DNS rebinding between the check and the fetch cannot swap targets.

**Opt-out:** `CC_WEBFETCH_ALLOW_PRIVATE=1` bypasses the guard and emits
a `tracing::warn!` naming the bypassed host + resolved IP. Intended for
local dev against `127.0.0.1` dev servers; never set this in production.

Reference: `openspec/changes/fix-webfetch-ssrf/`, `cc-tools/src/web_fetch/ssrf.rs`.
