# Behavior Contracts — Phase 3

All behaviors discovered via subagent code exploration. Each entry includes source file:line evidence.

---

## Area A: Permission Dialog Behavior

### A1 — What triggers a permission prompt vs. auto-allow

**Observed behavior:**
Multi-stage decision cascade in `hasPermissionsToUseTool()`:
1. Check `alwaysAllowRules` (from settings, CLI args, or session) → auto-allow
2. Check mode: `dontAsk` → auto-allow; `plan` mode → auto-deny writes
3. Run safety classifier (when enabled) → may auto-allow
4. Fall through to interactive permission dialog

**Evidence:** `src/utils/permissions/permissions.ts:473-625`, `:275-281`

**Classification:** MUST REPLICATE EXACTLY
Permission system is a core safety boundary. The allow/deny/ask rules from user settings must
be respected exactly. Auto-allow logic must match — users rely on this for scripts and automation.

---

### A2 — Ctrl+C during a permission prompt

**Observed behavior:**
- Ctrl+C (captured as Escape) calls `toolUseConfirmQueue[0]?.onAbort()`
- Tool use is **rejected** — a `tool_result` block with `is_error: true` is generated
- Execution does NOT retry; moves to next message processing step

**Evidence:** `src/screens/REPL.tsx:2137-2140`, `src/components/permissions/FilePermissionDialog/usePermissionHandler.ts:141-176`

**Classification:** MUST REPLICATE EXACTLY
The abort → reject → error result chain must be preserved. Any deviation could let tools
run that the user intended to cancel.

---

### A3 — No permission timeout

**Observed behavior:**
- There is **no timeout** on permission dialogs
- Dialog blocks indefinitely until user responds or presses Escape
- Escape = reject (tool fails with error)

**Evidence:** `src/screens/REPL.tsx:980-984, 2024-2030`

**Classification:** ACCEPTABLE VARIANCE
In a Rust TUI, the same "block until response" behavior is correct.
However, a configurable timeout (absent in TS) could be added as an improvement.

---

## Area B: Streaming Interruption

### B1 — Ctrl+C during streaming

**Observed behavior:**
- `abortController.abort('user-cancel')` or `abort('interrupt')` is called
- Streaming terminates immediately
- In-flight API request is cancelled via AbortController signal

**Evidence:** `src/screens/REPL.tsx:2121-2154, 4102`

**Classification:** MUST REPLICATE EXACTLY
Users expect immediate stop. The abort signal must propagate to the HTTP client layer.

---

### B2 — Partial streamed output is saved

**Observed behavior:**
- If `streamingText` is non-empty at interrupt, it is saved:
  `setMessages(prev => [...prev, createAssistantMessage({ content: streamingText })])`
- Saved BEFORE abort completes and BEFORE interrupt marker
- The partial message appears in conversation history

**Evidence:** `src/screens/REPL.tsx:2125-2130`

**Classification:** MUST REPLICATE EXACTLY
Users read partial output after Ctrl+C. Losing it would be a noticeable regression.

---

### B3 — Session continues after interruption

**Observed behavior:**
- AbortController is set to `null` after abort
- Session is NOT reset; conversation history is intact
- User can submit a new message immediately

**Evidence:** `src/screens/REPL.tsx:2155-2162`

**Classification:** MUST REPLICATE EXACTLY
Continuability after interrupt is a core usability feature.

---

## Area C: Context Compaction

### C1 — Auto-compact trigger

**Observed behavior:**
- Triggered by token count: `autoCompactThreshold = effectiveContextWindow - 13,000`
- `effectiveContextWindow = contextWindow - reservedOutputTokens (max 20,000)`
- Configurable via env vars: `CLAUDE_CODE_AUTO_COMPACT_WINDOW`, `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`
- Disableable via `DISABLE_COMPACT=1`
- Warning shown at 20,000 tokens below threshold

**Evidence:** `src/services/compact/autoCompact.ts:32-90`, `src/query.ts:453-470`

**Classification:** MUST REPLICATE EXACTLY (threshold formula and env vars) / MUST REPLICATE APPROACH (token counting method)

> **Reclassified 2026-04-09 per Decision 7:** The threshold formula itself
> (constants, env var overrides) remains MUST REPLICATE EXACTLY. The **token
> counting method** is reclassified from MUST EXACT to **MUST REPLICATE
> APPROACH**: the TS version uses `usage.input_tokens` from the last API
> response + a rough character-length heuristic (`content.length / 4`) for the
> delta — not an exact tokenizer. The Rust version must use the same hybrid
> approach (API usage anchor + rough delta estimate). Acceptable variance: ±10%
> on the delta, absorbed by the 13,000-token buffer.
> See `.claude/plan/phase2-entry.md` §Decision 7 for full analysis.

---

### C2 — What is preserved vs. discarded

**Observed behavior:**
- **Preserved**: Summary of old messages (from API), last N recent messages, tool definitions,
  file history snapshots, attribution snapshots, context-collapse logs, task output attachments, plan state
- **Discarded**: Old message content (replaced by summary), images (stripped before API call),
  orphaned tool_results from summarized section, progress messages

**Evidence:** `src/services/compact/compact.ts:144-171, 326-340`, `src/utils/conversationRecovery.ts:570-592`

**Classification:** MUST REPLICATE EXACTLY
Compaction behavior affects what context Claude has in long sessions. The summary + messagesToKeep
structure must be preserved, and images must be stripped before the compaction API call.

---

### C3 — Tool_result blocks during compaction

**Observed behavior:**
- `tool_result` blocks in the **summarized** portion are discarded (replaced by summary)
- `tool_result` blocks in `messagesToKeep` (recent messages) are retained
- Images inside `tool_result` blocks are explicitly stripped before compact API call
- Orphaned `tool_result` blocks (without matching `tool_use`) are handled by the API naturally

**Evidence:** `src/services/compact/compact.ts:166-167, 283`

**Classification:** MUST REPLICATE EXACTLY
Improper handling causes malformed message sequences rejected by the API.

---

## Area D: Tool Error Handling

### D1 — Failed Bash command output

**Observed behavior:**
- Non-zero exit code is captured and included in response data
- stdout and stderr are merged in shell execution
- Bash tool returns: `{ data: { stdout, stderr, exit_code, returnCodeInterpretation? } }`
- The tool does NOT throw; it always returns successfully

**Evidence:** `src/tools/BashTool/BashTool.tsx:750-819`

**Classification:** MUST REPLICATE EXACTLY
Claude reads `exit_code` to decide if the command succeeded. Changing this format
would break Claude's ability to reason about command failures.

---

### D2 — Error presented to Claude as tool_result (not is_error)

**Observed behavior:**
- Bash failures → normal `tool_result` block (without `is_error: true`)
- `is_error: true` is only set for **permission denials**, not command execution failures
- Exit code and stderr are visible to Claude as text content

**Evidence:** `src/tools/BashTool/BashTool.tsx:803-819`, `src/services/tools/toolExecution.ts:1029-1037`

**Classification:** MUST REPLICATE EXACTLY
The distinction between `is_error` (permission) and normal tool_result (command failure)
is semantically meaningful to Claude. Mixing them changes Claude's behavior.

---

### D3 — No automatic retry on tool failure

**Observed behavior:**
- System does NOT retry tool calls automatically
- Claude decides whether to retry based on the error content
- Exception: PermissionDenied hooks can suggest (but not force) retry with advisory message

**Evidence:** `src/services/tools/toolExecution.ts:1073-1101`

**Classification:** ACCEPTABLE VARIANCE
Current behavior (no retry) is the correct default. The advisory retry message for
permission hooks is a minor detail that can be replicated if needed.

---

## Area E: Session Resume

### E1 — State saved to disk after each turn

**Observed behavior:**
- **Transcript JSONL** (`~/.claude/sessions/{sessionId}/transcript.jsonl`): All messages after each turn
- **Session metadata JSON** (`~/.claude/sessions/{sessionId}/metadata.json`): title, model, mode, PR links, agent config
- **File history snapshots**: IDE file state tracking (persisted in session logs)
- **Attribution snapshots**: Commit/blame data (persisted in session logs)
- **Task outputs**: `~/.claude/sessions/{sessionId}/task-output/`
- **Plan files**: `~/.claude/sessions/{sessionId}/plans/`

**Evidence:** `src/utils/sessionStorage.ts:289, 343, 1039-1065, 1660`

**Classification:** MUST REPLICATE EXACTLY
The session file format must be compatible so existing sessions can be resumed by the Rust version.

---

### E2 — Ephemeral state (not saved)

**Observed behavior:**
- `streamingText` — React state, not persisted
- AbortController instances
- Progress messages (explicitly excluded from transcript)
- In-flight tool execution state
- UI state (scroll position, selection)
- API response caches

**Evidence:** `src/utils/sessionStorage.ts:134-135`, `src/screens/REPL.tsx:1461, 1473`

**Classification:** ACCEPTABLE VARIANCE
Ephemeral state is inherently session-local. Rust implementation can use whatever
in-memory representation suits the architecture.

---

### E3 — What --resume restores

**Observed behavior:**
- `--resume <uuid>`: Restores all persisted data — messages, file history, attribution,
  content replacements, context-collapse state, todos, metadata, PR links
  - Runs SessionStart hooks with `trigger='resume'`
- `--continue`: Loads the most recent non-live session (skips live background sessions)
- NOT restored by either: streaming state, open dialogs, tool progress, caches, UI state

**Evidence:** `src/utils/conversationRecovery.ts:456-597`, `src/utils/sessionRestore.ts:98-149`, `src/main.tsx:764-765`

**Classification:** MUST REPLICATE EXACTLY
Users rely on `--resume` and `--continue` to continue interrupted work. The restored
state must be equivalent. SessionStart hooks with `trigger='resume'` must fire.

---

## Intentionally Changing

The following behaviors from the TS version are intentionally NOT replicated:
- **Permission dialog timeout**: No timeout in TS; Rust version MAY add a configurable timeout
  (improvement, not regression)
- **React/Ink TUI state management**: Internal rendering architecture will change completely
  (Ratatui vs Ink/React). The observable UI behavior must be equivalent, not the implementation.
- **Progress message handling**: Ephemeral; implementation is TUI-framework-specific.
  Observable behavior (spinner shown during tool execution) must be preserved.
