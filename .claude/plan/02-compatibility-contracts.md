# Compatibility Contracts — Phase 2

## Category 1: Global Settings File (`~/.claude/settings.json`)

**Source:** `src/utils/settings/settings.ts`, `src/utils/settings/types.ts` (line 255+)

**Key schema fields (Zod-validated):**
```
$schema, apiKeyHelper, awsCredentialExport, awsAuthRefresh, gcpAuthRefresh,
fileSuggestion, respectGitignore, cleanupPeriodDays, env, attribution,
includeCoAuthoredBy (deprecated), includeGitInstructions,
permissions: { allow, deny, ask, defaultMode, disableBypassPermissionsMode, additionalDirectories },
model, availableModels, modelOverrides,
enableAllProjectMcpServers, enabledMcpjsonServers, disabledMcpjsonServers,
allowedMcpServers, deniedMcpServers,
hooks (HooksSettings), worktree, disableAllHooks, defaultShell,
allowManagedHooksOnly, allowedHttpHookUrls, httpHookAllowedEnvVars
```

**Settings merge order (lowest → highest priority):**
`userSettings` → `projectSettings` → `localSettings` → `policySettings`

Arrays are concatenated and deduplicated. Invalid fields are preserved (not dropped).

**Compatibility requirement:** MUST BE READABLE
The Rust version must parse all fields present in existing `~/.claude/settings.json` files.
Invalid/unknown fields must be preserved (not silently dropped) for forward/backward compat.

---

## Category 2: Project Settings File (`.claude/settings.json`)

**Source:** Same as Category 1

**Behavior:** Project settings override global settings (deep merge). Local settings
(`.claude/settings.local.json`) override project settings. Policy settings win over all.

**Compatibility requirement:** MUST BE READABLE
Same schema as global settings. Must parse existing project settings without error.

---

## Category 3: Session / History Files

**Source:** `src/history.ts` (line 219-225), `src/utils/config.ts` (line 54-72)

**File location:** `~/.claude/history.jsonl` (not per-session; single global JSONL)

**Each line structure:**
```typescript
type LogEntry = {
  display: string                                      // User input text
  pastedContents: Record<number, StoredPastedContent>  // Pasted content refs
  timestamp: number                                    // Unix milliseconds
  project: string                                      // Project root path
  sessionId?: string                                   // Session identifier
}

type StoredPastedContent = {
  id: number
  type: 'text' | 'image'
  content?: string        // Inline if ≤ 1024 bytes
  contentHash?: string    // Hash ref for large pastes (stored externally)
  mediaType?: string
  filename?: string
}
```

**Limits:** Max 100 entries per project across all sessions.
**Session filtering:** Entries are filtered by `project` (current project root path).

**Compatibility requirement:** MUST BE READABLE
Rust version must be able to read and resume sessions written by the TS version.
The JSONL format and field names must be preserved exactly.

---

## Category 4: Memory Files

**Source:** `src/memdir/memoryTypes.ts`, `src/memdir/memoryScan.ts`

**Format:** Markdown files with YAML frontmatter, stored in `~/.claude/memory/`

**Required frontmatter:**
```yaml
---
name: string
description: string  # one-line, used for relevance filtering
type: user | feedback | project | reference
---
```

**MEMORY.md index:** No frontmatter. One entry per line, ≤ 150 chars.
Format: `- [Title](file.md) — one-line hook`
Lines beyond MAX_ENTRYPOINT_LINES are truncated.

**Compatibility requirement:** MUST BE READABLE
Rust version must parse existing memory files with the same frontmatter fields.
Legacy files without `type:` must degrade gracefully (type = undefined/unknown).

---

## Category 5: Anthropic API Wire Format

**Source:** `src/utils/api.ts`, `src/utils/messages.ts`

**Message format:** Anthropic SDK `MessageParam` type (standard Anthropic API).

**Tool schema sent to API:**
```typescript
{
  name: string
  description: string
  input_schema: JSONSchema          // JSON Schema for parameters
  strict?: boolean                  // Feature-gated (tengu_tool_pear)
  defer_loading?: boolean           // Feature-gated (tool search)
  cache_control?: {
    type: 'ephemeral'
    scope?: 'global' | 'org'
    ttl?: '5m' | '1h'
  }
  eager_input_streaming?: boolean   // Feature-gated (1P only, tengu_fgts)
}
```

**System prompt:** Split into blocks with cache_control markers.
Block types: attribution (uncached) → static content (cache global, 1P) → dynamic content (cache org).

**Compatibility requirement:** MUST MATCH EXACTLY
This is an external API. The Rust version must send requests the Anthropic API accepts.
Standard Anthropic API SDK conventions apply. Beta flags must be sent correctly.

---

## Category 6: MCP Protocol

> **Corrected 2026-04-08.** The previous version of this section listed 4
> transport types. Code review of `src/services/mcp/client.ts:619-868` found
> **9 transport types**, and mTLS is orthogonal to transport (applies to all
> HTTP-based transports, not just WebSocket). Scope markers per Decision 5 of
> `.claude/plan/phase2-entry.md` have been added.

**Source:** `src/services/mcp/client.ts`, `src/utils/mcpWebSocketTransport.ts`, `src/utils/mtls.ts`

**JSON-RPC version:** 2.0 (from `@modelcontextprotocol/sdk/types.js`)

**Message types used:**
- `tools/list` → `ListToolsResult`
- `tools/call` → `CallToolResultSchema`
- `prompts/list` → `ListPromptsResult`
- `resources/list` → `ListResourcesResultSchema`
- `initialize` (session setup)

**Error codes:**
- `-32001`: Session not found / expired (detected via HTTP 404 + JSON-RPC code)

**Transport types (9 total in TS):**

| `serverRef.type` | Transport | Source line | Phase 2 scope |
|---|---|---|---|
| `stdio` (default) | `StdioClientTransport` — local MCP server subprocess | `client.ts:944` | **IN SCOPE** |
| `sse` | `SSEClientTransport` — remote HTTP SSE | `client.ts:619` | **IN SCOPE** |
| `http` | `StreamableHTTPClientTransport` — Streamable HTTP (current MCP spec) | `client.ts:784` | **IN SCOPE** |
| `ws` | `WebSocketTransport` (custom wrapper) — `wss://` | `client.ts:735` | DEFERRED |
| `ws-ide` | WebSocket variant for IDE integration | `client.ts:708` | DEFERRED |
| `sse-ide` | SSE variant for IDE integration | `client.ts:678` | DEFERRED |
| `sdk` | In-process SDK direct transport | `client.ts:866` | DEFERRED |
| `claudeai-proxy` | claude.ai login-state proxy | `client.ts:868` | DEFERRED |

**mTLS (cross-cutting — applies to HTTP-based transports):**

`src/utils/mtls.ts` provides three consumers, used by `sse`, `http`, `ws`, and `ws-ide`:
- `getMTLSAgent()` → `HttpsAgent` for Node HTTP/HTTPS clients
- `getWebSocketTLSOptions()` → `tls.ConnectionOptions` for WebSocket
- `getFetchOptions()` → undici fetch options

**Env vars driving mTLS (MUST MATCH EXACTLY):**
- `CLAUDE_CODE_CLIENT_CERT` — path to client cert (PEM)
- `CLAUDE_CODE_CLIENT_KEY` — path to client key (PEM)
- `CLAUDE_CODE_CLIENT_KEY_PASSPHRASE` — key passphrase
- `NODE_EXTRA_CA_CERTS` — extra CA certs (Node-specific; Rust replaces with `SSL_CERT_FILE` which is the OpenSSL/rustls-native-certs convention)

**Phase 2 scope per Decision 5:**
- Transports: `stdio` + `sse` + `http` (3 of 9)
- mTLS: IN SCOPE, applies to `sse` + `http` (the HTTP-based transports that are themselves in scope)
- All deferred transports require a future mini-RFC before inclusion; "quietly adding later" is not sanctioned

**Compatibility requirement for in-scope items:** MUST MATCH EXACTLY
- JSON-RPC 2.0 envelope preserved
- `serverRef.type` string values (`stdio`, `sse`, `http`) must be accepted unchanged in settings files written by the TS version
- mTLS env var names must be honored unchanged
- For the 6 deferred transport types: settings files that reference them must **parse without error** and produce a clear "transport X not supported in this build — see docs" message, not a crash or silent drop

---

## Category 7: Slash Command Interface

**Source:** `src/commands.ts`

**Command sources:** `builtin`, `plugin`, `managed`, `bundled`, `mcp`, `skills`, `commands_DEPRECATED`

**Core builtin commands (partial list):**
`/commit`, `/commit-push-pr`, `/config`, `/memory`, `/mcp`, `/skills`,
`/hooks`, `/tasks`, `/keybindings`, `/session`, `/model`, `/login`, `/logout`,
`/help`, `/status`

**Argument format:** `parseArgumentNames()` / `substituteArguments()` — positional args via `$ARGUMENTS` placeholder in skill prompts.

**Skill commands:** Loaded from `~/.claude/skills/` directory. Invoked as `/<skill-filename>`.

**Compatibility requirement:** MUST BE READABLE (for core commands), CAN CHANGE WITH TOOL (for aliases)
Core builtin command names must match exactly. Skill-derived commands depend on file names
(no migration needed — they load from user's skills directory).

---

## Category 8: Hook Script Interface

**Source:** `src/utils/hooks.ts`, `src/schemas/hooks.ts`

**Input delivery:** JSON written to **stdin** (trailing newline), NOT via environment variables.

**stdin JSON structure:**
```json
{
  "tool_name": "Write",
  "tool_input": { ... }
}
```

**Exit code semantics:**
- `0`: Success
- `2`: Blocking error — wakes model if `asyncRewake: true`
- Other non-zero: Failure (non-blocking)
- Timeout: Default 10 minutes (`TOOL_HOOK_EXECUTION_TIMEOUT_MS`)

**Hook types:** `command`, `prompt`, `http`, `agent`

**Command hook fields:**
```
command, shell (bash|powershell), timeout, statusMessage, once, async, asyncRewake, if
```

**HTTP hook:** POST request with headers (env var interpolation via allowlist).

**Compatibility requirement:** MUST MATCH EXACTLY
Existing user hook scripts must work without modification.
stdin format, exit code semantics, and timeout behavior must be preserved exactly.

---

## Category 9: Plugin / Skill Manifest Format

**Source:** `src/skills/loadSkillsDir.ts`

**Format:** Markdown `.md` files with YAML frontmatter.

**Frontmatter fields:**
```yaml
---
name: string                # optional display name
description: string         # one-line (required if no ## Skill Description heading)
when_to_use: string
argument-hint: string
arguments: string[]         # or comma-separated string
allowed-tools: string       # comma-separated tool names
user-invocable: boolean     # default: true
model: string               # model ID or "inherit"
disable-model-invocation: boolean
version: string
effort: easy|moderate|hard|integer
context: "fork"
agent: string
shell: bash|powershell
paths: string[]
hooks: HooksSettings        # YAML/JSON
---
```

**Required:** At least one of: `description:` field OR `## Skill Description` heading.

**Compatibility requirement:** MUST BE READABLE
Rust version must load existing user skills from `~/.claude/skills/` without modification.
All frontmatter fields must be parsed. Unknown fields should be ignored gracefully.

---

## Category 10: Keybindings File (`~/.claude/keybindings.json`)

**Source:** `src/keybindings/schema.ts`

**Top-level structure:**
```json
{
  "$schema": "optional",
  "$docs": "optional",
  "bindings": [
    {
      "context": "Global|Chat|Autocomplete|Confirmation|Help|Transcript|HistorySearch|Task|ThemePicker|Settings|Tabs|Attachments|Footer|MessageSelector|DiffDialog|ModelPicker|Select|Plugin",
      "bindings": {
        "ctrl+k": "action_id | command:slash_name | null"
      }
    }
  ]
}
```

**Binding value types:**
- Action ID (enum): `app:interrupt`, `app:exit`, `chat:submit`, `history:search`, etc.
- Command: `"command:<slash_command_name>"` (regex: `/^command:[a-zA-Z0-9:\-_]+$/`)
- Unbind: `null`

**Keystroke format:** `modifier+key` (e.g., `ctrl+shift+p`, `f1`, `up`)

**Compatibility requirement:** MUST BE READABLE
Existing keybinding customizations must work without modification.
All contexts and action IDs must be supported.
