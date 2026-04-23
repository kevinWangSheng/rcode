# Parity Gap Roadmap — Rust rewrite vs TS original (2026-04-23)

**Scope:** every gap here is inside an area the Rust rewrite already
claims to cover — these are **partial ports**, not out-of-scope
features. Items listed in `RUST_REWRITE_PLAN.md` §2 as "Excluded"
(JS plugins, Windows/Linux, full telemetry, mouse, image
rendering, themeable palette) are deliberately absent.

**Source:** 10 parallel Explore-agent audits on 2026-04-23. Raw
per-crate findings live in this session's memory file
`project_parity_gap_audit_2026_04_23.md`. Line-number citations
below come from those reports.

**Legend:** Priority H (blocks daily-driver correctness or security)
· M (noticeable UX / integration gaps) · L (polish). "Openspec?"
column marks items that should be drafted as a spec-driven change
before coding.

---

## P0 — correctness / security / data-loss bugs

These either silently lose data, bypass a TS safety check, or make
a shipped feature unusable. Target: next batch of commits.

| # | Crate | Gap | Evidence | Openspec? |
|---|---|---|---|---|
| 1 | cc-api | ✅ DONE (2026-04-23, `fix-content-block-cache-control` + `fix-cache-control-engine-wiring`): `CacheControl` types + `ContentBlock::with_cache_control` helper + new `cc-query::cache_breakpoint::tag_last_block_for_caching`; called in `run_turn` before every `CreateMessageRequest::new`. Trailing block of the trailing message carries `{"type":"ephemeral"}` on the wire. | TS api.ts:72–76, 297–320; Rust `cc-query/src/cache_breakpoint.rs` + `engine.rs:215` | yes |
| 2 | cc-api | ✅ DONE (2026-04-23, `fix-request-thinking` + `fix-thinking-engine-wiring`): `ThinkingConfig` + `CreateMessageRequest::with_thinking` + `QueryOptions.thinking` + `BridgeRequest.thinking` + `--thinking BUDGET|adaptive|off` CLI flag with clamp-to-max-tokens parser. Engine plumbs through on every request. | TS thinking.ts, sideQuery.ts:58/174; Rust `cc/src/main.rs:parse_thinking` + `cc-query/src/engine.rs:248-250` | yes |
| 3 | cc-git | `is_git_ignored()` / `filter_git_ignored()` missing — Read/Glob/Grep can leak ignored files | TS git.ts `check-ignore` path; Rust cc-git has no equivalent | yes |
| 4 | cc-session | JSONL entry variants not deserialised (file-history-snapshot, attribution-snapshot, pr-link, tag, agent-name, mode, worktree-state, context-collapse-*) — silent data loss on resume | TS types/logs.ts:297–317; Rust lib.rs:44–54 | yes |
| 5 | cc-session | ✅ DONE (2026-04-23, `fix-session-resume-integrity` + `fix-session-resume-wiring` §1): `INTERRUPT_MESSAGE` / `INTERRUPT_MESSAGE_FOR_TOOL_USE` consts + `Session::append_interrupt_marker` now called from `cc-query` cancel path; hard-coded `"[Interrupted by user]"` literals replaced with the canonical consts byte-for-byte matching TS. | TS utils/messages.ts INTERRUPT_MESSAGE; Rust `cc-query/src/engine.rs:275-340` | yes |
| 6 | cc-session | ⏳ PARTIAL (2026-04-23): types + `FileHistorySnapshot` + `Session::append_file_history_snapshot` shipped in `fix-session-resume-integrity`. Edit/Write producer-side wiring deferred to a follow-up change because it needs a breaking `Tool::execute` signature change (~28 tool migration). Tracked in `fix-session-resume-wiring` §2-§4 (unchecked). | TS utils/fileHistory.ts:39–52; Rust SessionMetadata 56–65 | yes |
| 7 | cc-hooks | ✅ DONE (2026-04-23, `fix-hook-correctness` + `fix-hook-correctness-wiring`): asyncRewake exit-code-2 → `HookOutcome::AsyncRewake` in cc-hooks; cc-query renders it as a distinct tool_result error with the `"Hook requested async rewake:"` prefix. TODO comment marks the queue-routing follow-up. | TS hooks.ts:1843–1875; Rust `cc-hooks/src/lib.rs:345-352` + `cc-query/src/engine.rs:540-547` | yes |
| 8 | cc-hooks | ✅ DONE (2026-04-23, `fix-hook-correctness` + `fix-hook-correctness-wiring`): `additional_contexts` collected by HookRunner now flow through `QueryEngine.pending_additional_contexts` and are drained as `MessageParam::user` entries at the top of each `run_turn` iteration. SubagentStart path was already wired. | TS hooks.ts:2783–2788; Rust `cc-query/src/engine.rs:188-198,533-534` | yes |
| 9 | cc-hooks | HTTP hook: no env-var interpolation in headers, no CR/LF/NUL sanitisation → CRLF-injection risk + no secret support | TS execHttpHook.ts:76–108; Rust lib.rs:498–509 | yes (security) |
| 10 | cc-mcp | mTLS (env-var driven TLS_CERT/TLS_KEY) not wired — plan §2 Decision 5 lists this as in-scope cross-cutting feature | Plan §2 Dec.5; Rust no code path | yes |
| 11 | cc-mcp | `-32001` session-expired reconnect-once logic missing (checks HTTP 404 only) | TS client.ts:189–206, 1313–1328; Rust http_client.rs:327–365 | yes |
| 12 | cc-tools Grep | Schema missing `-A`, `-B`, `-C/context`, `-n`, `type`, `head_limit`, `offset`, `multiline` — Claude issues these and gets schema errors | TS GrepTool.ts:58–88; Rust grep.rs:30–37 | yes (widely used) |
| 13 | cc-tools Read | `pages` input for PDFs + BLOCKED_DEVICE_PATHS missing — /dev/zero opens blindly | TS FileReadTool.ts:236–244,98; Rust read.rs:33–45 | yes |
| 14 | cc-permissions | 10-stage precedence collapsed to 5 stages — tool-level ask rules, content-specific ask, safety checks, classifier path all absent | TS permissions.ts:473–625; Rust lib.rs:204–260 | yes (large) |
| 15 | cc-permissions | "Allow always" never persisted to settings.json — session-only memory, lost on restart | TS permissions.ts:424–434; Rust lib.rs:199–201 | yes |
| 16 | cc-agents | Watchdog fires repeatedly on long non-prompt stalls (no lastGrowth reset) | TS LocalShellTask.tsx:66–68; Rust tasks.rs:185–218 | yes |
| 17 | cc-agents | Cancel does not cascade leader → teammates — orphaned subagents on Ctrl+C | TS cleanupRegistry/inProcessRunner; Rust registry.rs:77–83 | yes |
| 18 | cc-agents | Remote-agent is a one-shot POST — no session polling, no `RemoteAgentMetadata` persistence | TS teleport.ts / RemoteAgentTask.tsx:92–100; Rust tasks.rs:275–313 | yes |
| 19 | cc binary | `--resume` doesn't merge resumed messages into headless initial_messages — silent failure with no `--message` | Rust cc/src/main.rs:160–168 vs 224–237 | yes |
| 20 | cc-query | Auto-compact simplified to hardcoded 13 K buffer — missing `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`, output-token reserve, consecutive-failure circuit breaker | TS autoCompact.ts:71–90 etc.; Rust cc-query engine | yes |

---

## P1 — UX parity that users will notice

Visible to anyone side-by-siding the binaries. Not a crash or data
loss, but "feels incomplete."

| # | Crate | Gap | Evidence | Openspec? |
|---|---|---|---|---|
| 21 | cc-tui | `ThinkingBlock` variant missing on `TranscriptItem` — StreamThinking events have nowhere to render | TS Message.tsx:53–609; Rust app.rs:31–51 | yes |
| 22 | cc-tui | Permission dialog is a generic modal — no tool-specific reason text, diff preview (Edit), command preview (Bash) | TS src/components/permissions/{Bash,FileEdit,WebFetch,SedEdit,…}PermissionRequest.tsx; Rust render.rs:741–790 | yes |
| 23 | cc-tui | Tool-use card shows name + first arg line only — no command preview (Bash), no inline diff (Edit), no output length | TS per-tool card components; Rust render.rs:303–356 | yes |
| 24 | cc-tui | Streaming status bar missing tokens/sec rate + cumulative cost breakdown | TS StatusLine.tsx:83–96; Rust render.rs:458–489 | no |
| 25 | cc-tui | No `CLAUDE_CODE_ACCESSIBILITY` support (cursor animation suppression) + no narrow-terminal table reflow | TS TextInput.tsx:41, MarkdownTable.tsx:240 | no |
| 26 | cc-query | Hook events missing: Stop / StopFailure / Setup / UserPromptSubmit / Notification / PreCompact / PostCompact / TeammateIdle / TaskCreated / TaskCompleted / ConfigChange / WorktreeCreate / WorktreeRemove / InstructionsLoaded / CwdChanged / FileChanged. *(Cross-check: memory claims SessionStart/SessionEnd/Stop are wired; verify which are actually already emitted before drafting change.)* | TS coreTypes.ts ~1–30; Rust hook.rs:8–29 | yes (per event) |
| 27 | cc-hooks | Hook env vars CLAUDE_PROJECT_DIR / CLAUDE_PLUGIN_ROOT / CLAUDE_PLUGIN_DATA / CLAUDE_PLUGIN_OPTION_* not exported | TS hooks.ts:884–925; Rust lib.rs:417–421 | yes |
| 28 | cc-permissions | Dialog `suggestions` metadata (PermissionUpdate[]) not flowing from engine — TUI has to synthesise Allow / Allow always / Deny by hand | TS PermissionAskDecision.suggestions; Rust `PermissionResult` missing field | yes |
| 29 | cc-mcp | `resources/list`, `resources/read`, `prompts/list`, `getPrompt` unsupported — Rust is tools-only | TS client.ts:2009–2046; Rust adapter.rs/manager.rs | yes |
| 30 | cc-mcp | Server-pushed notifications (tools/list_changed etc.) ignored — no GET/SSE listener | TS useManageMCPConnections.ts:624/673/711; Rust http_client.rs:23 | yes |
| 31 | cc-tools Bash | `run_in_background`, `dangerouslyDisableSandbox` flags missing, no sandbox enforcement | TS BashTool.tsx:221,243, line 48; Rust bash.rs:47–67 | yes |
| 32 | cc-tools Edit/Write | Output should be structured `{filePath, originalFile, structuredPatch, userModified, replaceAll, gitDiff?, …}` — Rust returns plain strings | TS FileEditTool/types.ts:63–80, FileWriteTool:71–90; Rust edit.rs / write.rs | yes |
| 33 | cc-tools WebSearch | `allowed_domains` / `blocked_domains` / query min-length missing; no Anthropic beta `web_search_20250305` wrapper; no citation blocks | TS WebSearchTool.ts:25–36, 76–83, 96–100; Rust web_search.rs | yes |
| 34 | cc-agents | Mailbox envelope lacks sender/recipient identity — multiteam routing broken | TS InProcessTeammateTask/types.ts:22–76; Rust mailbox.rs:5–27 | yes |
| 35 | cc-agents | Subagent tool failures bubble `CcError` instead of synthesising `tool_result {is_error: true}` to parent | TS AgentTool / agentToolUtils.ts; Rust run_local_agent | yes |
| 36 | cc-agents | Teammates see the full tool list — TS filters to team-safe subset via `createInProcessCanUseTool` | TS inProcessRunner.ts:200+; Rust run_in_process_teammate | yes |
| 37 | cc binary | `mcp` + `doctor` subcommands missing — essential ops UX | TS main.tsx:4100+; Rust main.rs:88–93 | no |
| 38 | cc binary | `--print` JSON output missing tool calls / citations / hook events | TS cli/print.ts; Rust main.rs:499–517 | yes |
| 39 | cc binary | Startup errors show raw strings with no remediation hint — onboarding UX | TS cli.tsx:135–150; Rust main.rs:115–118 | no |
| 40 | cc-auth | Logout does not revoke OAuth token server-side — only deletes local creds | TS oauth/client.ts revoke path; Rust cc-auth logout | no |
| 41 | cc-memory | Frontmatter uses prefix-matching instead of full YAML parse — breaks quoted values, multi-line, list literals | TS frontmatterParser.ts:149; Rust lib.rs:309–319 | yes |

---

## P2 — polish, internal plumbing, defensive code

Small gaps, edge cases, and things that matter later.

| # | Crate | Gap | Evidence |
|---|---|---|---|
| 42 | cc-tools Bash | No progress threshold hints (PROGRESS_THRESHOLD_MS), no sed-edit preview parsing | TS sedEditParser.ts, BashTool.tsx:55 |
| 43 | cc-tools Read | Image downsampling, Jupyter notebook support missing; output always plain text, not `{type: text|image|pdf}` discriminated | TS imageProcessor.ts, readNotebook:57, FileReadTool:249+ |
| 44 | cc-tools Write/Edit | No file-history snapshot tracking, no LSP diagnostic clearing, no skill activation on touched paths | TS fileHistoryTrackEdit:24–25, clearDeliveredDiagnosticsForFile:6, activateConditionalSkillsForPaths:10 |
| 45 | cc-tools Grep | No default head_limit (250) + offset slicing; no VCS_DIRECTORIES_TO_EXCLUDE | TS:108–120, 95 |
| 46 | cc-permissions | `ask` rule type entirely absent (TS has alwaysAskRules) | TS permissions.ts:223–230, 1091–1111 |
| 47 | cc-permissions | `dontAsk` semantic drift — TS denies-by-default, Rust auto-allows | TS:503–517; Rust lib.rs:122 |
| 48 | cc-permissions | Audit source coarser than TS (5 bins vs per-stage 1a..3 tracking) | Rust PermissionSource enum |
| 49 | cc-hooks | Matcher is glob-only; TS supports regex metadata | TS hooksConfigManager.ts:11–45; Rust lib.rs:625–636 |
| 50 | cc-hooks | `unsafe_shell` validation at dispatch time, not parse time — worse UX | TS hooks.ts:338–344; Rust lib.rs:557–599 |
| 51 | cc-hooks | Agent-hook dispatch is no-op — should spawn a subagent | TS hooks.ts:2259–2276; Rust lib.rs:369–372 |
| 52 | cc-mcp | No typed error enum (auth vs session vs tool) — all collapsed to `Result<_, String>` | Rust types.rs, client.rs |
| 53 | cc-mcp | stdio child supervision lacks SIGTERM grace period + restart on crash | Rust client.rs:243/271 |
| 54 | cc-mcp | MCP JSON Schema → Anthropic shape translation passes through untransformed | Rust adapter.rs:56–62 |
| 55 | cc-mcp | Streamable HTTP has no chunked request bodies | Rust http_client.rs |
| 56 | cc-agents | Plan-mode approval gate (`awaitingPlanApproval`) missing for teammates | TS types.ts:40–41 |
| 57 | cc-agents | No background-task metadata persistence across session resume | TS RemoteAgentTask.tsx:92–98; Rust registry.rs in-memory only |
| 58 | cc-agents | `is_control` flag has no consumer — no graceful-shutdown protocol | Rust mailbox.rs:10 |
| 59 | cc-config | `apiKeyHelper` not explicitly sanitised before exec (TS has same weakness but worth hardening) | Rust settings.rs:52–54 |
| 60 | cc-git | GitContext missing dirty-file count + remote URL; no `is_bare_repo()` | Rust git.rs:29–33 |
| 61 | cc-auth | No org / workspace scope switching in `buildAuthUrl` | TS oauth/client.ts |
| 62 | cc-api | No explicit `tool_choice` validation / builder (implicit through serde only) | Rust request.rs |
| 63 | cc binary | No explicit SIGTERM/SIGHUP handler on headless path — SessionEnd hook may not fire | Rust main.rs:452–454 |
| 64 | cc binary | CLAUDE_CODE_* feature env vars not read at startup | TS cli.tsx:9–26; Rust main.rs |
| 65 | cc binary | Most subcommands absent (logout, config, reset, agents, plugin, setup-token, auth, …) — many out of scope per §2; flag only the ones users would expect day-one | TS main.tsx:4100+ |

---

## Implementation batches (suggested order)

**Batch A — API + cache + thinking** *(P0 1–2, small)*
Lands prompt-cache correctness + thinking support together; both
live in cc-api request construction.

**Batch B — Session & resume integrity** *(P0 4–6, P0 19)*
Single unit of work: richer JSONL entries + interrupt marker +
file-history + resume-message plumbing. Without this, `--resume`
is broken.

**Batch C — Hook correctness** *(P0 7–9, P1 27)*
asyncRewake + additional_contexts injection + HTTP-hook
sanitisation + plugin env vars. Consolidate in one pass because
they all change `HookRunResult` flow.

**Batch D — Permission engine rework** *(P0 14–15, P1 28)*
Largest P0 — ~1 000 LOC. Restore 10-stage precedence, ask-rule
type, persistence of Allow-always, dialog suggestions. Wire up
tool-specific acceptEdits hook.

**Batch E — Tools schema fidelity** *(P0 12–13, P1 31–33)*
Grep flag expansion, Read `pages` + device blocklist, Bash
background/sandbox flags, structured Edit/Write output,
WebSearch domain filters + citation shape. Gate on §4 spec
updates.

**Batch F — Git-ignore filtering** *(P0 3)*
`is_git_ignored` + `filter_git_ignored` helper, wired into Read,
Glob, Grep before they emit results.

**Batch G — MCP parity** *(P0 10–11, P1 29–30)*
mTLS via env vars, -32001 reconnect-once, resources/prompts, push
notifications. All in cc-mcp.

**Batch H — Agents** *(P0 16–18, P1 34–36)*
Watchdog reset, cancel cascade, remote-agent polling, mailbox
envelope, subagent error synthesis, team-scoped tool filter.

**Batch I — TUI parity** *(P1 21–25)*
Thinking-block transcript variant, per-tool permission dialogs
and cards, streaming status line, accessibility knobs. Biggest
user-visible batch.

**Batch J — Hook events + subcommands** *(P1 26, P1 37)*
Fire the missing hook events. Add `claude mcp` + `claude doctor`
subcommands.

**Batch K — Polish** *(P2 42–65)*
Opportunistic; tackle during regressions or while nearby.

---

## Open verifications (do before starting any batch)

1. **Hook-event coverage:** the cc-query and cc-hooks audits
   partly contradict memory. Grep `cc-hooks::HookEvent` variants
   and their `emit` / `trigger` call sites once before deciding
   which events (SessionStart, UserPromptSubmit, Stop, etc.) are
   genuinely missing vs. already wired under a different name.
2. **WebFetch scope:** the cc-tools agent flagged it as missing,
   but the crate ships a `web_fetch/` submodule (2026-04-23
   local check). Confirm it has the Summarizer + SSRF guard before
   closing any WebFetch-related follow-up.
3. **cc-tui `TranscriptItem` thinking variant:** skim
   cc-tui/src/app.rs to verify the variant truly is absent before
   drafting a change.
4. **cc-session entry coverage:** inspect the current union in
   lib.rs before porting the full TS type — TS has accreted
   experimental entry types that may not all be needed at the
   80 % parity bar.

---

## How to work this list

- Each P0 item is expected to become an openspec change under
  `openspec/changes/fix-<area>-<slug>/`. Follow the pattern the
  20 audited defects used in 2026-04-17.
- P1 items group naturally by crate — draft one change per
  "Batch" above rather than one per row.
- P2 items don't need openspec coverage; backlog them as issues
  or pick up during adjacent work.
- Keep `.claude/plan/implementation-notes.md` updated with
  contracts as each batch lands.
