# Phase 3 Quality Audit — 2026-04-17

Authoritative list of defects found while cross-checking the Rust rewrite
against `RUST_REWRITE_PLAN.md` compatibility contracts (§3) and behavior
contracts (§4). Produced with static code review of the crates under
`rust/crates/` at commit `9dc8411`.

This file is **reference only** — it is not an OpenSpec change. Each
CRITICAL, HIGH, and MEDIUM defect has its own actionable proposal under
`openspec/changes/fix-*` (20 total, all validated with `openspec
validate --all --strict`). LOW items below stay in this audit as
ergonomic polish — they are not tracked as individual proposals until
user impact is demonstrated.

---

## CRITICAL (blocks release)

| # | Location | Defect | Proposal |
|---|---|---|---|
| C1 | `cc/src/main.rs:549-551`, `cc-query/src/engine.rs` memory-block path | `CacheControl.scope` is always `None` — three-tier caching (uncached / global / org) promised in §3 never actually happens. Every turn re-sends the full system prompt. | `openspec/changes/fix-cache-control-scope/` |
| C2 | `cc-tools/src/web_fetch.rs:140-142` | `is_allowed_url` only checks `http://` / `https://` prefix. No block on loopback, RFC1918, link-local, or cloud-metadata (`169.254.169.254`). Prompt-injection SSRF hands back IAM creds. | `openspec/changes/fix-webfetch-ssrf/` |
| C3 | `cc-query/src/engine.rs:182,187,188,192` | `tx.try_send(...)` + `let _ =` silently drops `StreamDelta` / `StreamThinking` / `StreamToolUse` under TUI backpressure. M3 "token-by-token" AC fails under load. | `openspec/changes/fix-tui-event-dropping/` |
| C4 | `cc-session/src/lib.rs:142` | `writeln!(file, "{line}")` with no `sync_all()`; §3 "persisted after each turn" breaks on SIGKILL / power loss. | `openspec/changes/fix-session-writeln-fsync/` |

## HIGH (fix within one iteration)

| # | Location | Defect | Proposal |
|---|---|---|---|
| H1 | `cc-api/src/stream.rs:174` | Tool-use JSON fails to parse at end of stream → silently becomes `{}`; Claude then calls Bash / Edit / Write with **empty args**. | `openspec/changes/fix-api-stream-toolinput-fallback/` |
| H2 | `cc-tools/src/read.rs:72 → 87` | `metadata()` then `read_to_string()` on the same path — TOCTOU. A symlink swap between the two reads a file much larger than the 50 MB cap → OOM. | `openspec/changes/fix-read-size-cap-toctou/` |
| H3 | `cc-tools/src/edit.rs` | Edit is `read → replace → write` with no tmpfile + rename. Two concurrent Edits both succeed, one silently wins. | `openspec/changes/fix-edit-atomic-write/` |
| H4 | `cc-mcp/src/http_client.rs:290-303` | 404 clears `session_id` under the lock, but another in-flight request already captured the old ID. No auto-reconnect; caller bubbles the error. | `openspec/changes/fix-mcp-404-session-race/` |
| H5 | `cc-hooks` command-hook path | `bash -c $command` with no shell-escape on the settings.json-sourced `command` field. If settings.json is ever writable by a less-trusted path, command injection. | `openspec/changes/fix-hook-command-injection/` |
| H6 | `cc-tui/src/lib.rs:102` | `Keybindings::load()` called **per key event** — disk I/O on every keystroke. On slow filesystems it's visible lag; amplifies fs races. | `openspec/changes/fix-tui-keybindings-reload-per-key/` |

## MEDIUM (next quarter)

| # | Location | Defect | Proposal |
|---|---|---|---|
| M1 | `cc-session/src/lib.rs:55, 189` | `path.parent().unwrap()` + `Default::default().expect()` are startup-path panics. Rare but CLI-fatal. | `openspec/changes/fix-session-startup-panic/` |
| M2 | `cc-tools/src/grep.rs:~105` | Cancel token is checked **before** the per-file line loop but not **inside**. Ctrl+C on a 10 GB log goes unnoticed until the file finishes. | `openspec/changes/fix-grep-cancel-in-file-loop/` |
| M3 | `cc-tools/src/bash.rs:78` | Cancel branch returns without explicit `.kill().await`; relies on tokio `Child::Drop` → kill. Usually fine, but a short race window exists. | `openspec/changes/fix-bash-cancel-explicit-kill/` |
| M4 | `cc-tui/src/action.rs` Ctrl+C handling | No "second press = force quit" escalation. If abort stalls, user has no recovery. | `openspec/changes/fix-tui-ctrl-c-force-quit/` |
| M5 | `cc-tui` permission dialog | Deny path hard-resets `AppMode::Streaming`; wrong if the stream already finished. | `openspec/changes/fix-tui-permission-dialog-mode/` |
| M6 | `cc-memory` `@`-includes | Only recognises `@~/` prefix; doesn't handle `@/abs/path`; cycle detection relies on `canonicalize()` which fails silently on permission-denied symlinks. | `openspec/changes/fix-memory-include-path-expansion/` |
| M7 | `cc-git::filter_git_ignored` | `git check-ignore --stdin` launched without a timeout. Large repos can hang for minutes. | `openspec/changes/fix-git-filter-timeout/` |
| M8 | `cc-config::merge` | Round-trip through `serde_json::Value`. If base has `{"a":{"x":1}}` and overlay has `{"a":[1,2]}` the type flips silently — contradicts the "unknown fields preserved" promise. | `openspec/changes/fix-config-merge-type-flip/` |
| M9 | `cc-tools/src/write.rs` | Doesn't preserve file mode. A `chmod +x` script written via `Write` loses the executable bit. | `openspec/changes/fix-write-preserve-mode/` |
| M10 | `cc-hooks` stdin feed | `let _ = stdin.write_all(...)` swallows `EPIPE`; hook sees half a JSON object and fails parse. | `openspec/changes/fix-hook-stdin-error-propagation/` |

## LOW

- ~~`cc-tools/src/web_fetch.rs:150-171` — regex-based HTML strip is best-effort;
  non-greedy `<script>` closing tag confuses it in rare CDATA / nested cases.~~
  **Accepted 2026-04-18.** The strip is deliberately a "good enough" tokenizer
  before the optional LLM summarizer takes over (see
  `cc-tools::attach_webfetch_summarizer`). Adding a full HTML parser is cost
  without measurable benefit given the summarizer handles fidelity downstream.
- ~~`cc-tools/src/read.rs:94` — offset/limit line numbering uses `str::lines`
  which normalises `\r\n` and `\r`. On a CRLF file the reported line numbers
  can drift from what the user's editor shows.~~
  **Accepted 2026-04-18.** `str::lines` splits on `\n` and strips a preceding
  `\r`, so `"a\r\nb\r\n"` yields `["a", "b"]` — line counts match editors on
  pure-CRLF files. Drift only occurs with truly mixed line endings in the
  same file, which is pathological and happens to confuse editors too.
- ~~`cc-session/src/lib.rs:180-183` — `load_metadata` returns
  `CcError::Io(NotFound)` instead of `Ok(None)`, forcing callers into
  `.unwrap_or_default()` patterns.~~
  **Fixed 2026-04-18.** `load_metadata` now returns
  `CcResult<Option<SessionMetadata>>`. NotFound maps to `Ok(None)`, other
  I/O errors still surface, JSON corruption surfaces as `CcError::Json`.
  Regression test `load_metadata_returns_none_when_file_missing` +
  5 call sites updated.
- ~~`cc-auth/src/oauth.rs:99` — hard-coded URL + `.expect("valid authorize_url")`
  at runtime; should be a compile-time const or surface `CcError::Auth`.~~
  **Fixed 2026-04-18.** `build_authorize_url` now returns
  `CcResult<String>`; a malformed `authorize_url` in a user-supplied config
  surfaces as `CcError::Auth("invalid OAuth authorize_url …")`. Regression
  test `authorize_url_surfaces_parse_error` locks the new contract.

---

## Gap Against M3 Exit Criteria

`RUST_REWRITE_PLAN.md` §8 Milestone 3 has two items that are **not** met
and are currently recorded as "runtime unverified":

1. **Terminal.app 80-col launch + no artifacts.** Never manually
   validated since the TUI was re-wired into `main.rs`.
2. **Ctrl+C <100ms end-to-end abort + partial text preserved.** The
   `cc-tui` crate tests cover the state-machine side but not the true
   end-to-end latency; the 10 spike tests in
   `rust/spikes/tui/tests/headless.rs` were never ported to `cc-tui`.

   Missing acceptance criteria inside `cc-tui/tests/`:
   - **AC-2b** "tokens arriving after abort are dropped"
   - **AC-2c** "end-to-end abort latency <100ms"

These gaps are **not** tracked as OpenSpec changes yet because they are
verification work, not behavior changes. If the verification surfaces a
real defect, promote to a `fix-*` change at that point.

---

## Audit Coverage

| Crate | Reviewed | Notes |
|---|---|---|
| cc-api | ✅ | Streaming, retry, cancel, cache-control |
| cc-query | ✅ | Tool loop, auto-compact, permission flow, events_tx |
| cc-tools | ✅ | All 8 tools + TodoList / Task* |
| cc-mcp | ✅ | stdio + HTTP Streamable, session IDs, 404 handling |
| cc-hooks | ✅ | 27 events, four kinds (cmd/prompt/http/agent) |
| cc-tui | ✅ | Event loop, render, permission, keybindings, commands |
| cc-commands | ✅ | Slash parsing, skill discovery |
| cc-session | ✅ | JSONL transcript, TS-format resume, fsync |
| cc-memory | ✅ | `@`-includes, six memory types, CLAUDE.md walk-up |
| cc-config | ✅ | Six-layer merge, NFC, sanitize, project context |
| cc-auth | ✅ | env/file/Keychain chain, OAuth, refresh |
| cc-git | ✅ | GitContext, is_git_ignored, filter_git_ignored |
| cc-agents | — | Audit not yet run; added 2026-04-16, 15 tests. Queue for next pass. |
| cc-bridge | — | 106 LOC, `--print` path. Low priority. |

The "cc-agents" audit is a known follow-up. Everything else was in
scope for this pass.
