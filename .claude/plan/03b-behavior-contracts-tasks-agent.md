# Phase 1 Gap Patch A1 — Tasks/Agent Behavior Contracts

**Status:** COMPLETE (2026-04-09)
**Depends on:** Decision 6 (scope), A2 (hooks — `agent` hook kind), A3 (project discovery)
**Blocks:** Decisions 1–4, Phase 2 detailed design

---

## Scope

Per Decision 6, this document covers the **4 stable task types** in depth:

1. `local_bash` — background subprocess
2. `local_agent` — in-process sub-agent (AgentTool)
3. `in_process_teammate` — multi-teammate swarm (InProcessBackend only)
4. `remote_agent` — HTTP polling of Anthropic remote session API

**Excluded** (Decision 6 deferral): `local_workflow`, `monitor_mcp`, `dream`.

---

## 1. Task Framework (Cross-Cutting)

**TS Source:** `src/utils/task/framework.ts`

### Unified State Model

All 4 task types share a common framework:

```
TaskState {
  taskId: string,              // UUID
  type: TaskType,              // 'local_bash' | 'local_agent' | 'in_process_teammate' | 'remote_agent'
  status: TaskStatus,          // 'pending' | 'running' | 'completed' | 'failed' | 'killed'
  isBackgrounded: boolean,     // false=foreground, true=backgrounded
  notified: boolean,           // Notification sent (idempotency guard)
  startTime: number,           // Epoch ms
  endTime?: number,            // Set when terminal
  outputOffset: number,        // Last reported byte position in output file
  retain: boolean,             // UI holding task (blocks eviction)
  evictAfter?: number,         // Deadline ms for eviction after terminal
}
```

### Registration

```
registerTask(task, setAppState):
  - Adds task to AppState.tasks[taskId]
  - Emits task_started SDK event (first time only, not on resume)
  - Merges UI-held state on re-register (retain, startTime, messages)
```

### State Updates

```
updateTaskState<T>(taskId, setAppState, updater):
  - Reference-identity optimization (no re-render if updater returns same ref)
```

### Output Delta Tracking

- Each task tracks `outputOffset` (last reported byte position in output file)
- `getTaskOutputDelta(taskId, offset)` returns new content since offset
- Framework polls and patches offsets on each query loop iteration
- Attachments sent to parent only when output changes

### Eviction

```
evictTerminalTask(taskId, setAppState):
  - Removes task from AppState.tasks when ALL of:
    - Terminal status (completed | failed | killed)
    - notified = true (user already saw notification)
    - evictAfter deadline passed (grace period for UI)
```

### Notification Format

All task types use the same notification envelope:

```xml
<task_notification>
  <task_id>{taskId}</task_id>
  <tool_use_id>{toolUseId}</tool_use_id>
  <output_file>{outputFilePath}</output_file>
  <status>completed|failed|killed</status>
  <summary>{human-readable summary}</summary>
  <result>{optional structured result}</result>
</task_notification>
```

Delivered via `enqueuePendingNotification()` → message queue + SDK event emission.

**Idempotency:** Each task atomically checks `notified` flag before enqueueing. Prevents duplicate notifications if kill races with completion.

### Cleanup Registry

All tasks register cleanup handlers:
```
unregisterCleanup = registerCleanup(async () => { ... })
```
Ensures resources freed even if main session terminates unexpectedly.

---

## 2. `local_bash` — Background Subprocess

**TS Source:** `src/tasks/LocalShellTask/LocalShellTask.tsx`, `killShellTasks.ts`

### Lifecycle State Machine

```
                 ┌─────────────────┐
                 │    (created)     │
                 └────────┬────────┘
                          │ spawnShellTask() / registerForeground()
                          ▼
                 ┌─────────────────┐
          ┌──────│    running       │──────┐
          │      └────────┬────────┘      │
          │               │               │
   background()     exit code 0    exit code ≠ 0
          │               │               │
          ▼               ▼               ▼
   ┌────────────┐  ┌───────────┐  ┌──────────┐
   │backgrounded│  │ completed │  │  failed   │
   └────────────┘  └───────────┘  └──────────┘
          │                              
   kill() │                              
          ▼                              
   ┌───────────┐                         
   │  killed    │                         
   └───────────┘                         
```

### State Properties (Beyond Base)

```
{
  shellCommand: ShellCommand | null,  // Nulled on completion/kill
  result?: { code: number, interrupted: boolean },
}
```

### Cancellation / Abort

- **Kill:** `killTask()` → `shellCommand.kill()` (SIGTERM → SIGKILL) + `shellCommand.cleanup()`
- **No graceful shutdown:** Kill is always hard
- **Stall watchdog:** 45-second timer monitors output stall; emits notification if command appears to wait for interactive input (not a kill — just a warning)
- **No automatic timeout:** User must manually stop via TaskStopTool

### Orphan Cleanup

`killShellTasksForAgent(agentId)` kills all bash tasks spawned by an exiting agent. Prevents zombie processes.

### Output Streaming

- ShellCommand manages process I/O → writes directly to task output file
- Real-time, no task-level buffering
- Parent polls via `getTaskOutputDelta()` for new bytes

### Parent↔Child Communication

**None.** Bash tasks are fire-and-forget:
- Parent provides command + working directory
- Child runs independently
- Parent polls output file
- Completion notification on terminal state

### Rust Contract

| Behavior | Classification |
|----------|---------------|
| Subprocess spawn with foreground→background transition | MUST REPLICATE |
| Hard kill via process group signal (SIGTERM/SIGKILL) | MUST REPLICATE |
| Output file streaming with delta tracking | MUST REPLICATE |
| Stall watchdog (45s) | SHOULD REPLICATE |
| Orphan cleanup on agent exit | MUST REPLICATE |
| Notification format | MUST REPLICATE EXACTLY |

---

## 3. `local_agent` — In-Process Sub-Agent

**TS Source:** `src/tasks/LocalAgentTask/LocalAgentTask.tsx`, `src/tools/AgentTool/runAgent.ts`

### Lifecycle State Machine

```
                 ┌─────────────────┐
                 │    (created)     │
                 └────────┬────────┘
                          │ AgentTool invocation
                          ▼
                 ┌─────────────────┐
          ┌──────│    running       │──────┐
          │      └────────┬────────┘      │
          │               │               │
   abort()          returns result    throws error
          │               │               │
          ▼               ▼               ▼
   ┌───────────┐  ┌───────────┐  ┌──────────┐
   │  killed    │  │ completed │  │  failed   │
   └───────────┘  └───────────┘  └──────────┘
```

### State Properties (Beyond Base)

```
{
  agentId: string,
  prompt: string,
  abortController?: AbortController,
  error?: string,
  result?: AgentToolResult,
  progress?: { toolUses: number, tokens: number },
  messages?: Message[],             // Conversation history (capped at 50)
  diskLoaded: boolean,
  lastReportedToolCount: number,
  lastReportedTokenCount: number,
}
```

### Cancellation / Abort

- **Graceful:** `killAsyncAgent()` → `abortController.abort()` → agent loop detects AbortSignal
- **Cleanup on abort/completion (runAgent.ts finally block):**
  - Clean up agent-specific MCP servers
  - Clear session hooks registered by this agent
  - Release file state cache
  - Release transcript subdir mapping
  - Kill orphaned bash tasks spawned by this agent
  - Kill monitor MCP tasks

### Output Streaming

- Agent conversation stored in `task.messages` (append-only, capped at 50 items)
- Progress reported via `updateAgentProgress()` with tool use count + token count
- SDK emission via `emitTaskProgress()`

### Parent↔Child Communication

- **One-way:** Parent spawns with initial prompt; agent runs Claude API query loop independently
- No direct parent-child message passing during execution
- Completion result returned via `task.result`
- Messages visible to parent via `task.messages` (shared state)

### MCP Server Lifecycle

- Agent-specific MCP servers initialized at start (`initializeAgentMcpServers()`)
- Cleaned up in finally block when agent finishes
- Shared parent servers NOT cleaned up (memoized, shared across agents)

### Rust Contract

| Behavior | Classification |
|----------|---------------|
| In-process query loop with own conversation | MUST REPLICATE |
| AbortController-based graceful cancellation | MUST REPLICATE (use `CancellationToken` or `tokio` equivalent) |
| Message cap (50 items for UI) | SHOULD REPLICATE |
| Per-agent MCP server lifecycle | MUST REPLICATE |
| Cleanup: kill orphan bash tasks on agent exit | MUST REPLICATE |
| Progress reporting (tool count + tokens) | MUST REPLICATE |

---

## 4. `in_process_teammate` — Multi-Teammate Swarm

**TS Source:** `src/tasks/InProcessTeammateTask/`, `src/utils/swarm/spawnInProcess.ts`, `src/utils/swarm/inProcessRunner.ts`

### Lifecycle State Machine

```
                 ┌─────────────────┐
                 │    (created)     │
                 └────────┬────────┘
                          │ spawnInProcessTeammate()
                          ▼
                 ┌─────────────────┐
          ┌──────│    running       │◄─────────┐
          │      └───┬─────────┬───┘           │
          │          │         │               │
   abort()     idle=true   idle=false     new work
          │          │         │               │
          │          ▼         ▼               │
          │   ┌──────────┐ ┌──────────┐       │
          │   │   idle    │ │  active  │───────┘
          │   └──────────┘ └──────────┘
          │
          ▼
   ┌───────────┐     (3s grace)     ┌───────────┐
   │  killed    │──────────────────►│  evicted   │
   └───────────┘                    └───────────┘
```

### State Properties (Beyond Base)

```
{
  identity: {
    agentId: string,           // "name@teamName"
    agentName: string,
    teamName: string,
    color?: string,
    planModeRequired: boolean,
    parentSessionId: string,
  },
  prompt: string,
  model?: string,
  abortController?: AbortController,             // Kill entire teammate
  currentWorkAbortController?: AbortController,   // Abort current turn only
  awaitingPlanApproval: boolean,
  permissionMode: PermissionMode,
  isIdle: boolean,
  shutdownRequested: boolean,
  messages?: Message[],               // Capped at 50
  pendingUserMessages: string[],      // Mailbox queue
  inProgressToolUseIDs?: Set<string>,
  onIdleCallbacks?: Array<() => void>,
  lastReportedToolCount: number,
  lastReportedTokenCount: number,
}
```

### Cancellation / Abort

**Two-level abort:**

1. **Current turn abort:** `currentWorkAbortController.abort()` — aborts in-flight tool call without killing teammate. Teammate stays alive for new work.

2. **Full kill:** `killInProcessTeammate()`:
   - `abortController.abort()` — signals graceful shutdown
   - Calls all `onIdleCallbacks` (notify waiters)
   - Removes from `teamContext.teammates`
   - Sets `status = 'killed'`
   - Evicts task after 3-second grace period (`STOPPED_DISPLAY_MS`)

### Parent↔Child Communication: Mailbox System

**Bidirectional via shared AppState:**

- All teammates + leader share single `AppState.teamContext.teammates`
- Changes to any teammate's state are immediately visible (no IPC delay)

**Mailbox API:**
- `writeToMailbox(agentId, message)` — queues message for teammate
- `readMailbox(agentId)` — teammate polls for pending messages
- `processMailboxPermissionResponse()` — permission responses flow back

**Permission delegation:**
- Teammate uses `createInProcessCanUseTool()` for permission requests
- Falls back to mailbox if UI bridge unavailable
- Leader's ToolUseConfirm dialog handles with worker badge

### Context Isolation

- `runWithTeammateContext()` wraps execution with AsyncLocalStorage
- TeammateContext provides isolated identity + agentId
- Teammate name@team available to all async operations within scope

### Plan Mode Approval

- If `planModeRequired = true`, teammate enters plan mode
- User approves plan before implementation proceeds
- `awaitingPlanApproval` flag gates execution

### Rust Contract

| Behavior | Classification |
|----------|---------------|
| Two-level abort (turn vs full kill) | MUST REPLICATE |
| Mailbox system (pendingUserMessages queue) | MUST REPLICATE |
| Shared state (teammates visible to all) | MUST REPLICATE (Rust: `Arc<RwLock<TeamContext>>`) |
| Context isolation per teammate | MUST REPLICATE (Rust: `tokio::task_local!` or similar) |
| Idle/active state tracking | MUST REPLICATE |
| Plan mode approval flow | MUST REPLICATE |
| 3-second eviction grace period | SHOULD REPLICATE |
| Permission delegation via mailbox | MUST REPLICATE |
| Message cap (50 items) | SHOULD REPLICATE |
| `onIdleCallbacks` for leader coordination | MUST REPLICATE |

---

## 5. `remote_agent` — HTTP Polling

**TS Source:** `src/tasks/RemoteAgentTask/RemoteAgentTask.tsx`, `src/utils/teleport.tsx`, `src/utils/teleport/api.ts`

### Lifecycle State Machine

```
                 ┌─────────────────┐
                 │    (created)     │
                 └────────┬────────┘
                          │ registerRemoteAgentTask()
                          ▼
                 ┌─────────────────┐
          ┌──────│    running       │──────────────────┐
          │      │   (polling)      │                  │
          │      └────────┬────────┘                  │
          │               │                           │
   stopTask()     completion signal            timeout/error
          │               │                           │
          ▼               ▼                           ▼
   ┌───────────┐  ┌───────────┐              ┌──────────┐
   │  killed    │  │ completed │              │  failed   │
   └───────────┘  └───────────┘              └──────────┘
```

### State Properties (Beyond Base)

```
{
  remoteTaskType: 'remote-agent' | 'ultraplan' | 'ultrareview' | 'autofix-pr' | 'background-pr',
  sessionId: string,               // CCR session ID
  command: string,
  title: string,
  log: SDKMessage[],               // Accumulated event log
  todoList: TodoList,              // Extracted from last TodoWrite event
  pollStartedAt: number,           // For timeout calculation
  isRemoteReview?: boolean,
  reviewProgress?: {
    stage?: 'finding' | 'verifying' | 'synthesizing',
    bugsFound: number,
    bugsVerified: number,
    bugsRefuted: number,
  },
  isUltraplan?: boolean,
  ultraplanPhase?: 'needs_input' | 'plan_ready',
}
```

### Polling Protocol

**API Endpoints:**

```
GET /v1/sessions/{sessionId}/events?after_id={lastEventId}
  Headers: OAuth token, org UUID, beta flag
  Timeout: 30,000 ms
  Response: { data: SDKMessage[], has_more, first_id, last_id }

GET /v1/sessions/{sessionId}
  Response: SessionResource with session_status
```

**Poll Loop:**

```
POLL_INTERVAL_MS = 1000

loop:
  1. Fetch events since lastEventId (cursor-based, delta-only)
  2. Paginate: up to MAX_EVENT_PAGES=50 pages per poll
  3. Append new events to accumulatedLog
  4. Write delta to task output file
  5. Check completion signals:
     - result event (success → completed, failure → failed)
     - session_status = 'archived' → completed
     - remote-review tag in output → completed (bughunter)
     - stableIdle: 5+ consecutive idles with output → completed
     - 30-minute timeout (remote-review only) → failed
  6. Sleep POLL_INTERVAL_MS
  7. Check task.status !== 'running' → break
```

**Retry logic:**
- Exponential backoff: 2s, 4s, 8s, 16s (4 retries)
- Retry on transient errors (5xx, network failure)
- Don't retry on client errors (4xx)

### Cancellation / Abort

- `stopTask()` sets `status = 'killed'`, poll loop breaks on next iteration
- **Does NOT abort remote session** — it continues running on CCR
- Remote session cleaned up by CCR TTL
- Rationale: user can revisit the session URL on claude.ai

### Session Restore on Resume

`restoreRemoteAgentTasks()` called on session resume:
- Fetches live CCR session status
- Re-registers active tasks in running state
- Resumes polling from last known eventId

### Rust Contract

| Behavior | Classification |
|----------|---------------|
| 1-second polling interval | MUST REPLICATE EXACTLY |
| Cursor-based event pagination | MUST REPLICATE EXACTLY |
| Completion detection (5 signal types) | MUST REPLICATE EXACTLY |
| Exponential backoff retry (4 attempts) | MUST REPLICATE EXACTLY |
| Kill = stop polling only (don't abort remote) | MUST REPLICATE EXACTLY |
| Session restore on resume | MUST REPLICATE |
| Delta-only output file append | MUST REPLICATE |
| 30-minute timeout for remote-review | MUST REPLICATE |

---

## 6. Cross-Cutting: Hook Integration

Per A2 (hooks lifecycle contracts), these hook events interact with the task system:

| Hook Event | Task Types Affected | Trigger |
|------------|-------------------|---------|
| `SubagentStart` | `local_agent`, `in_process_teammate` | Agent spawned |
| `SubagentStop` | `local_agent`, `in_process_teammate` | Agent concluding response |
| `TeammateIdle` | `in_process_teammate` | Teammate becomes idle |
| `TaskCreated` | All types | Task registered |
| `TaskCompleted` | All types | Task reaches terminal state |

---

## 7. Summary: Key Differences

| Aspect | local_bash | local_agent | in_process_teammate | remote_agent |
|--------|-----------|------------|---------------------|-------------|
| **Execution** | Subprocess | Same process | Same process | HTTP polling |
| **Abort** | SIGTERM/SIGKILL | AbortSignal | 2-level (turn/full) | Stop polling |
| **Communication** | File polling | Shared messages | Mailbox + shared state | Event log |
| **Timeout** | None (45s stall warn) | Manual | Leader-driven | 30min (review) |
| **Output** | File stream | Message append (50 cap) | Message append (50 cap) | Event log append |
| **Cleanup** | Kill process group | Abort + MCP cleanup | Remove from team | Stop polling |
