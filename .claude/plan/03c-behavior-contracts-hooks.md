# Phase 1 Gap Patch A2 — Hooks Lifecycle Behavior Contracts

**Status:** COMPLETE (2026-04-09)
**Depends on:** Phase 1 outputs (01–08), A3 (settings/config contracts)
**Blocks:** A1 (tasks/agent — `agent` hook kind interacts with agent lifecycle), Decisions 1–4

---

## 1. Hook Events (27 Trigger Points)

**TS Source:** `src/utils/hooks/hooksConfigManager.ts:25-302`

### Tool-Related Events

| Event | Matcher | Description |
|-------|---------|-------------|
| `PreToolUse` | `tool_name` | Before tool execution. Exit 2 blocks tool call. |
| `PostToolUse` | `tool_name` | After successful tool execution. Exit 2 shows stderr to model. |
| `PostToolUseFailure` | `tool_name` | After tool execution fails. |
| `PermissionDenied` | `tool_name` | After auto-mode classifier denies a tool call. |
| `PermissionRequest` | `tool_name` | When permission dialog is displayed to user. |

### Session Lifecycle Events

| Event | Matcher | Description |
|-------|---------|-------------|
| `SessionStart` | `source` (`startup`\|`resume`\|`clear`\|`compact`) | New session begins. Exit 0 stdout shown to Claude. |
| `SessionEnd` | none | Session ending. Tighter timeout (1.5s default). |
| `Stop` | none | Before Claude concludes response. Exit 2 continues conversation. |
| `StopFailure` | error type | Turn ends due to API error. |

### Subagent Events

| Event | Matcher | Description |
|-------|---------|-------------|
| `SubagentStart` | agent type | Subagent (Agent tool) started. |
| `SubagentStop` | agent type | Before subagent concludes response. |

### Setup & Config Events

| Event | Matcher | Description |
|-------|---------|-------------|
| `Setup` | setup type | Repo setup hooks (init/maintenance). |
| `ConfigChange` | file path | Settings/skills files changed during session. |
| `InstructionsLoaded` | file path | Instruction file (CLAUDE.md) loaded. |

### Compaction Events

| Event | Matcher | Description |
|-------|---------|-------------|
| `PreCompact` | compact type | Before conversation compaction. |
| `PostCompact` | compact type | After conversation compaction. |

### Notification & Team Events

| Event | Matcher | Description |
|-------|---------|-------------|
| `Notification` | `notification_type` | When notifications are sent. Fire-and-forget. |
| `TeammateIdle` | none | Teammate about to go idle. |
| `TaskCreated` | none | Task being created. |
| `TaskCompleted` | none | Task being marked completed. |

### MCP Elicitation Events

| Event | Matcher | Description |
|-------|---------|-------------|
| `Elicitation` | MCP server name | MCP server requests user input. |
| `ElicitationResult` | MCP server name | User responded to MCP elicitation. |

### Filesystem & Worktree Events

| Event | Matcher | Description |
|-------|---------|-------------|
| `WorktreeCreate` | none | Create isolated worktree. |
| `WorktreeRemove` | none | Remove previously created worktree. |
| `CwdChanged` | none | Working directory changed. |
| `FileChanged` | filename pattern | Watched file changed (add/change/unlink). |

### Rust Scope Decision

**Phase 2 IN SCOPE (core events, needed for basic parity):**
All 27 events. The hook system is a core extensibility mechanism — partial implementation creates confusing gaps.

**Phase 2 ACCEPTABLE SIMPLIFICATION:**
- `WorktreeCreate` / `WorktreeRemove`: May use simplified VCS-agnostic implementation
- `TeammateIdle` / `TaskCreated` / `TaskCompleted`: Depend on A1 task contracts

---

## 2. Hook Command Types (5 Types)

**TS Source:** `src/schemas/hooks.ts:32-163`

### 2.1 Command Hook (`type: "command"`)

```json
{
  "type": "command",
  "command": "shell command here",
  "shell": "bash",
  "if": "Bash(git *)",
  "timeout": 60,
  "statusMessage": "Running check...",
  "once": false,
  "async": false,
  "asyncRewake": false
}
```

- Spawns shell process with JSON on stdin
- Default shell: `bash` (also supports `powershell`)
- Default timeout: 600 seconds (10 minutes)

### 2.2 Prompt Hook (`type: "prompt"`)

```json
{
  "type": "prompt",
  "prompt": "Verify this write operation: $ARGUMENTS",
  "if": "Write(*.rs)",
  "timeout": 60,
  "model": "claude-sonnet-4-6",
  "statusMessage": "Verifying...",
  "once": false
}
```

- Evaluates prompt with LLM for blocking/allow decisions
- `$ARGUMENTS` placeholder replaced with JSON input

### 2.3 Agent Hook (`type: "agent"`)

```json
{
  "type": "agent",
  "prompt": "Verify the code change is safe",
  "if": "Write(*.rs)",
  "timeout": 60,
  "model": "claude-haiku-4-5",
  "statusMessage": "Agent verifying...",
  "once": false
}
```

- Spins up agentic subagent for verification
- Default model: Haiku (cheaper)
- Default timeout: 60 seconds

### 2.4 HTTP Hook (`type: "http"`)

```json
{
  "type": "http",
  "url": "https://hooks.example.com/check",
  "if": "Bash(*)",
  "timeout": 30,
  "headers": { "Authorization": "Bearer $API_KEY" },
  "allowedEnvVars": ["API_KEY"],
  "statusMessage": "Checking...",
  "once": false
}
```

- POSTs JSON input to HTTP endpoint
- Response must be JSON
- Header values can interpolate env vars listed in `allowedEnvVars`

### 2.5 Function Hook (`type: "function"`) — Internal Only

```json
{
  "type": "function",
  "callback": "(messages, signal) => boolean",
  "errorMessage": "Validation failed",
  "timeout": 5000,
  "statusMessage": "Validating...",
  "id": "unique-id"
}
```

- In-process callback, cannot be persisted to settings files
- Session-scoped only
- Used for structured output enforcement, attribution tracking
- ID-based removal support

### Rust Scope Decision

**Phase 2 IN SCOPE:** `command`, `http`
**Phase 2 DEFERRED:** `prompt`, `agent` (depend on having a working LLM call path for hooks specifically — can be added after core loop works)
**Phase 2 INTERNAL ONLY:** `function` equivalent (Rust closures or trait objects for session-scoped hooks)

---

## 3. Hook Configuration Format

### Settings File Structure

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Write",
        "hooks": [
          { "type": "command", "command": "echo checking..." }
        ]
      }
    ],
    "SessionStart": [
      {
        "matcher": "startup",
        "hooks": [
          { "type": "command", "command": "git status" }
        ]
      }
    ]
  }
}
```

### Configuration Sources (priority order)

**TS Source:** `src/utils/hooks/hooksSettings.ts:230-271`

1. User settings (`~/.claude/settings.json`)
2. Project settings (`.claude/settings.json`)
3. Local settings (`.claude/settings.local.json`)
4. Plugin hooks (`~/.claude/plugins/*/hooks/hooks.json`)
5. Built-in hooks (registered internally)
6. Session hooks (in-memory, temporary)

### Policy Controls

- `allowManagedHooksOnly: true` — only policy/managed hooks execute; plugin and session hooks skipped
- `shouldDisableAllHooksIncludingManaged` — kill all hooks globally

---

## 4. Hook Execution Semantics

### Execution Flow

**TS Source:** `src/utils/hooks.ts:1952-2900`

```
1. Trust Check
   - Interactive mode: require workspace trust accepted
   - Non-interactive/SDK: implicit trust

2. Snapshot Capture
   - Freeze hook config before execution (prevents race with settings changes)

3. Matching
   - Collect hooks from all sources (snapshot + registered + session)
   - Filter by matcher pattern (e.g., tool_name for PreToolUse)
   - Apply `if` condition filtering
   - Deduplicate by (command + if + namespace)

4. Parallel Execution
   - ALL matching hooks execute in parallel (not sequential)
   - Each hook gets individual timeout
   - Combined abort signal for cancellation

5. Result Aggregation
   - Collect outcomes: success | blocking | non_blocking_error | cancelled
   - First blocking error wins
   - Multiple additionalContext values concatenated
```

### Exit Code Semantics

| Exit Code | Meaning | Behavior |
|-----------|---------|----------|
| `0` | Success | Process stdout/stderr per event rules |
| `2` | Blocking error | Block operation; show stderr to model |
| Other (1, 3, ...) | Non-blocking error | Show stderr to user only; continue |

### Per-Event Exit Code Behavior

| Event | Exit 0 | Exit 2 | Other |
|-------|--------|--------|-------|
| `PreToolUse` | stdout/stderr NOT shown | **Block tool call**, stderr → model | stderr → user, continue |
| `PostToolUse` | stdout shown in transcript mode | stderr → model immediately | stderr → user |
| `Stop` | stdout/stderr NOT shown | stderr → model, **continue conversation** | stderr → user |
| `SessionStart` | stdout → Claude | blocking errors ignored | stderr → user |
| `SessionEnd` | normal | N/A | stderr → user |

### JSON Output (Structured Response)

Hooks can return JSON on stdout for richer control:

```json
{
  "continue": false,
  "stopReason": "Policy violation detected",
  "decision": "block",
  "reason": "Unsafe operation",
  "systemMessage": "System context to prepend",
  "suppressOutput": true,
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "Approved by policy",
    "updatedInput": { "modified": "tool input" },
    "additionalContext": "Extra context for model"
  }
}
```

### Async Response

```json
{
  "async": true,
  "asyncTimeout": 300000
}
```

Hook continues in background; stdout/stderr not shown until completion.

---

## 5. Hook Input (Stdin JSON)

### Base Fields (All Events)

```json
{
  "session_id": "uuid",
  "transcript_path": "/path/to/transcript.jsonl",
  "cwd": "/current/working/directory",
  "permission_mode": "ask",
  "hook_event_name": "PreToolUse"
}
```

Optional base fields: `agent_id`, `agent_type` (present for subagent calls).

### Event-Specific Fields

**PreToolUse:**
```json
{ "tool_name": "Write", "tool_input": { ... }, "tool_use_id": "uuid" }
```

**PostToolUse:**
```json
{ "tool_name": "Write", "tool_input": { ... }, "tool_response": { ... }, "tool_use_id": "uuid" }
```

**SessionStart:**
```json
{ "source": "startup|resume|clear|compact", "model": "claude-sonnet-4-6" }
```

**Stop / SubagentStop:**
```json
{
  "stop_hook_active": true,
  "last_assistant_message": "...",
  "agent_id": "uuid",
  "agent_transcript_path": "/path/..."
}
```

**Notification:**
```json
{ "message": "...", "title": "...", "notification_type": "permission_prompt|idle_prompt|..." }
```

**FileChanged:**
```json
{ "file_path": "/absolute/path", "event": "change|add|unlink" }
```

---

## 6. Ordering and Deduplication

### Ordering

**TS Source:** `src/utils/hooks/hooksSettings.ts:230-271`

1. Sources processed in priority order (user → project → local → plugin → builtin)
2. Within same source: insertion order preserved
3. **All matching hooks execute in parallel** — no sequential ordering between hooks
4. Within a matcher group: hooks listed in array order

### Deduplication

**TS Source:** `src/utils/hooks.ts:1712-1806`

- Key: `(command_or_url_or_prompt, if_condition, namespace)`
- Namespace: `pluginRoot` or `skillRoot` (prevents cross-plugin collisions)
- Two plugins with identical template do NOT collapse
- Hooks with different `if` conditions are distinct hooks
- Function/callback hooks skip deduplication

---

## 7. Special Fields

### `async` Field

```json
{ "async": true }
```

- Hook runs in background (fire-and-forget)
- Does NOT block the operation
- Tracked in `AsyncHookRegistry` for completion polling
- stdout/stderr not shown to user or model

### `asyncRewake` Field

```json
{ "asyncRewake": true }
```

- Hook runs in background (implies `async`)
- On **exit code 2**: enqueues task-notification → model wakes up
- On exit code 0 or other: completes silently
- **Bypasses** AsyncHookRegistry (not tracked for polling)
- Abort: responds to new prompts (interrupt) but NOT hard cancel (Escape kills it)

**Key difference:** `async` = fire-and-forget; `asyncRewake` = fire-and-callback-on-blocking-error.

### `once` Field

```json
{ "once": true }
```

- After **successful** execution (outcome = `success`), hook is removed from session
- Blocking errors and non-blocking errors do NOT trigger removal — hook persists for retry
- Session-scoped only (settings-file hooks don't support `once` semantically)
- Implementation: `onHookSuccess` callback invokes `removeSessionHook()`

### `if` Field (Conditional Execution)

```json
{ "if": "Bash(git *)" }
```

- Uses permission rule syntax to filter when hook runs
- Evaluated against tool name + input for tool events
- Hooks without `if` always execute
- Silently skipped for non-tool events
- Part of hook identity for deduplication (same command + different `if` = distinct hooks)
- Performance optimization: avoids process spawn for non-matching conditions

---

## 8. Timeouts

| Context | Default | Override |
|---------|---------|---------|
| Normal hooks | 600,000 ms (10 min) | `hook.timeout * 1000` |
| SessionEnd hooks | 1,500 ms | `CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS` env var |
| Agent hooks | 60,000 ms (1 min) | `hook.timeout * 1000` |
| Function hooks | 5,000 ms | `hook.timeout` (already in ms) |

---

## 9. Environment Variables for Hooks

### Subprocess Environment

**TS Source:** `src/utils/hooks.ts:815-926`

All hook subprocesses inherit the base subprocess environment plus:

| Variable | Value | Scope |
|----------|-------|-------|
| `CLAUDE_SESSION_ID` | Session UUID | All hooks |
| `CLAUDE_CWD` | Current working directory | All hooks |
| `CLAUDE_PLUGIN_ROOT` | Plugin base directory | Plugin hooks only |
| `CLAUDE_PLUGIN_OPTION_*` | Plugin config values | Plugin hooks only |
| `CLAUDE_ENV_FILE` | Path to `.sh` file for env injection | SessionStart, Setup, CwdChanged, FileChanged |

### `CLAUDE_ENV_FILE` Mechanism

- Hooks write shell `export` statements to `$CLAUDE_ENV_FILE`
- `getSessionEnvironmentScript()` reads and injects into subsequent bash invocations
- PowerShell hooks skip this (incompatible syntax)
- Used to propagate env vars from hooks to tool invocations

---

## 10. Security and Trust

### Trust Enforcement

**TS Source:** `src/utils/hooks.ts:267-296`

- **All hooks** require workspace trust in interactive mode
- Non-interactive (SDK) mode has implicit trust
- Centralized check in `executeHooks()` generator before spawning
- Prevents RCE via hooks in untrusted workspaces

### Managed Hooks Only Policy

When `allowManagedHooksOnly: true` in policy settings:
- Plugin hooks skipped (checked via `pluginRoot` presence)
- Session hooks skipped entirely
- Only policy/managed hooks execute

### Snapshot Isolation

**TS Source:** `src/utils/hooks/hooksConfigSnapshot.ts`

- Hook config captured **before trust dialog** to prevent race conditions
- `captureHooksConfigSnapshot()` freezes config at point-in-time
- Policy settings cached separately
- Ensures deterministic execution even if settings change mid-turn

---

## 11. Performance Optimizations

**TS Source:** `src/utils/hooks.ts:1582-1593, 2036-2067, 1723-1729`

1. **Fast path for callback-only hooks:** Skip span/progress/abortSignal overhead for internal hooks (~70% faster)
2. **Lazy hook checking:** `hasHookForEvent()` stops at first match — avoids building full merged config
3. **Dedup fast path:** Function hooks skip 6-pass dedup filter

### Rust Implications

- Use `Vec<Hook>` with stable ordering, not `HashMap`
- Pre-compute "has any hooks for event X" bitmap at config load time
- Async hooks via `tokio::spawn` with `JoinHandle` tracking

---

## 12. Platform-Specific Handling

### PowerShell Hooks

- Skip bash-specific prep (path conversion, .sh auto-prepend, SHELL_PREFIX)
- Use `pwsh -NoProfile -NonInteractive -Command`
- `CLAUDE_ENV_FILE` incompatible (PowerShell syntax differs)

### Git Bash (Cygwin) on Windows

- Requires POSIX paths (`/c/Users/foo`)
- Windows paths (`C:\Users\foo`) will fail in Git Bash

### Rust Scope Note

Phase 2 targets macOS only. PowerShell and Git Bash handling is deferred to post-1.0 (Windows support).

---

## 13. Rust Contracts Summary

| Behavior | Classification |
|----------|---------------|
| 27 hook events with documented trigger points | MUST REPLICATE EXACTLY |
| Command + HTTP hook types | MUST REPLICATE EXACTLY |
| Prompt + Agent hook types | DEFERRED to post-core |
| Exit code semantics (0/2/other) | MUST REPLICATE EXACTLY |
| JSON stdin input format per event | MUST REPLICATE EXACTLY |
| JSON stdout structured response | MUST REPLICATE EXACTLY |
| Parallel execution of matching hooks | MUST REPLICATE EXACTLY |
| Deduplication by (command, if, namespace) | MUST REPLICATE EXACTLY |
| `async` / `asyncRewake` / `once` / `if` fields | MUST REPLICATE EXACTLY |
| Trust enforcement before hook execution | MUST REPLICATE EXACTLY (security) |
| Managed-only policy enforcement | MUST REPLICATE EXACTLY (security) |
| Snapshot isolation | MUST REPLICATE EXACTLY |
| `CLAUDE_ENV_FILE` mechanism | MUST REPLICATE EXACTLY |
| SessionEnd tight timeout (1.5s) | MUST REPLICATE EXACTLY |
| Per-hook timeout override | MUST REPLICATE EXACTLY |
| PowerShell support | DEFERRED (macOS-only Phase 2) |
