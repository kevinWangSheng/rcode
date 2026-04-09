# Phase 2 — Detailed Design

**Status:** COMPLETE (2026-04-09) — All 10 design items detailed
**Prerequisites:** All Phase 2 entry gate items resolved (see `phase2-entry.md`)

---

## Foundation Decisions (from Phase 2 Entry Gate)

| Decision | Outcome |
|----------|---------|
| Async Runtime | Tokio (multi-threaded) |
| TUI Library | Ratatui + crossterm |
| Crate Layout | 14 library crates + 1 binary |
| 80% Feature Cut | Explicit in/out lists in `phase2-entry.md` §Decision 4 |
| MCP Transports | stdio + sse + http + mTLS |
| Task Subsystem | 4 stable types (local_bash, local_agent, in_process_teammate, remote_agent) |
| Tokenizer | Reuse usage.input_tokens + rough delta estimate |

## Crate Architecture (Decision 3)

```
Layer 0:  cc-core           (types, traits, error)
Layer 1:  cc-config          (settings load/merge, project discovery)
          cc-http            (shared mTLS-aware HTTP client builder)
Layer 2:  cc-auth            (OAuth, Keychain, API key)
          cc-permissions     (allow/deny/ask rules)
          cc-git             (git root, worktree, ignore)
          cc-memory          (CLAUDE.md loading, walk-up, rules, skills, plugins)
Layer 3:  cc-api             (Anthropic streaming client)
          cc-tools           (built-in tools)
          cc-hooks           (hook execution engine)
Layer 4:  cc-mcp             (stdio + sse + http transports)
          cc-session         (JSONL transcript, resume)
Layer 5:  cc-agents          (task framework, 4 task types, swarm)
          cc-query           (tool loop, auto-compact, token tracking)
Layer 6:  cc-tui             (Ratatui TUI + slash commands)
          cc-bridge          (SDK --print path)
---
Binary:   claude-cli         (CLI entry point)
```

## Reference Documents

| Document | Content |
|----------|---------|
| `RUST_REWRITE_PLAN.md` | Master plan — goals, scope, milestones |
| `phase2-entry.md` | All 7 decisions + 4 gap patches + spike audit |
| `02b-project-discovery.md` | A3: project root, .claude/, CLAUDE.md, settings |
| `03c-behavior-contracts-hooks.md` | A2: 27 hook events, 5 types, execution semantics |
| `03b-behavior-contracts-tasks-agent.md` | A1: 4 task types lifecycle, cancellation, streaming |
| `implementation-notes.md` | Coding discoveries not in TS source |

---

## Phase 2 Work Items

The following design work must be completed before implementation begins:

### 1. Core Type Definitions
- [x] `cc-core` public trait surface: `Tool`, `Permission`, `HookEvent`, `TaskState`, `Message`, `ContentBlock`
- [x] Error type hierarchy across crates
- [x] Serialization contracts (serde derives, JSON formats)

**Spike audit verdict:** cc-core types are **REUSE** — spike message/tool/permission types are
structurally sound and match the Anthropic API wire format. Expand, don't rewrite.

#### 1.1 Message Types (`cc_core::message`) — REUSE from spike

```rust
// ── Roles & Cache ──────────────────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role { User, Assistant }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    #[serde(rename = "type")]
    pub kind: String,                        // "ephemeral"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,               // "global" | "org"
}

// ── Content Blocks ─────────────────────────────────────────────
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextBlock {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUseBlock {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultBlock {
    pub tool_use_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,              // string or array of content blocks
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

// NEW: Thinking block (extended thinking / chain-of-thought)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingBlock {
    pub thinking: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

// NEW: Redacted thinking (server-side redaction)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactedThinkingBlock {
    pub data: String,                        // opaque base64 blob
}

// NEW: Image content (for tool_result with screenshots etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageBlock {
    pub source: ImageSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ImageSource {
    #[serde(rename = "base64")]
    Base64 { media_type: String, data: String },
    #[serde(rename = "url")]
    Url { url: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text(TextBlock),
    ToolUse(ToolUseBlock),
    ToolResult(ToolResultBlock),
    Thinking(ThinkingBlock),                 // NEW
    RedactedThinking(RedactedThinkingBlock), // NEW
    Image(ImageBlock),                       // NEW
    // Forward-compat: unknown block types preserved as raw JSON
    #[serde(other)]
    Unknown,
}

// ── System Blocks ──────────────────────────────────────────────
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub kind: String,                        // "text"
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

// ── Conversation Messages ──────────────────────────────────────
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageParam {
    pub role: Role,
    pub content: MessageContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

// ── API Response ───────────────────────────────────────────────
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason { EndTurn, MaxTokens, StopSequence, ToolUse }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,                        // "message"
    pub role: Role,
    pub content: Vec<ContentBlock>,
    pub model: String,
    pub stop_reason: Option<StopReason>,
    pub stop_sequence: Option<String>,
    pub usage: Usage,
}
```

**Key changes from spike:**
- Added `ThinkingBlock`, `RedactedThinkingBlock`, `ImageBlock` content block variants
- Added `#[serde(other)] Unknown` variant for forward-compat with future API changes
- `Role` now derives `Copy` (it's a two-variant enum)

#### 1.2 Tool Trait (`cc_core::tool`) — REWRITE interface, keep concept

```rust
/// Result of executing a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: false }
    }
    pub fn error(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: true }
    }
}

/// JSON Schema for a tool's input parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInputSchema {
    #[serde(rename = "type")]
    pub kind: String,                        // "object"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_properties: Option<bool>,
}

/// A tool definition sent to the Anthropic API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: ToolInputSchema,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

/// The trait all tools (built-in + MCP) implement.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name (e.g. "Bash", "mcp__fs__read_file").
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> ToolInputSchema;

    /// Whether this tool only reads state (no side effects).
    /// Read-only tools may run concurrently; mutating tools are serialized.
    fn is_read_only(&self) -> bool { false }

    /// Execute the tool with the given JSON input.
    /// `cancel` is checked periodically — tools should return early on cancellation.
    async fn execute(
        &self,
        input: Value,
        cancel: &CancellationToken,
    ) -> CcResult<ToolResult>;

    /// Convert to API ToolDefinition.
    fn to_definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.input_schema(),
            cache_control: None,
        }
    }
}

/// Type-erased tool container used by the query engine.
pub type BoxTool = Box<dyn Tool>;
```

**Key changes from spike:**
- `execute()` now takes `&CancellationToken` for cooperative cancellation
- `to_definition()` default method (was a free function in spike)
- Added `additional_properties` to `ToolInputSchema`

#### 1.3 Permission Types (`cc_core::permission`) — EXPAND from spike

```rust
/// The outcome of a permission check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionBehavior { Allow, Deny, Ask }

/// Source of a permission decision (for audit trail).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionSource {
    SettingsAllow,       // matched an allow rule in settings
    SettingsDeny,        // matched a deny rule in settings
    SessionAllow,        // user chose "always allow" earlier this session
    ModeDefault,         // default behavior for current permission mode
    BypassFlag,          // --bypass-permissions CLI flag
    UserPrompt,          // interactive user decision
    Hook,                // hook blocked the action
}

/// Result of evaluating permission rules for a tool invocation.
#[derive(Debug, Clone)]
pub struct PermissionResult {
    pub behavior: PermissionBehavior,
    pub source: PermissionSource,
    pub reason: Option<String>,
}

/// A parsed permission rule (from settings.json allow/deny lists).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PermissionRule {
    /// Simple string pattern: "Bash", "Bash(*)", "Write(**)", "mcp__*"
    Simple(String),
    /// Structured rule: { tool: "Bash", input: { command: "git *" } }
    Structured {
        tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<Value>,
    },
}

/// User's decision when prompted for permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptDecision {
    Allow,               // allow this one invocation
    AllowAlways,         // allow this tool for the rest of the session
    Deny,                // deny this invocation (is_error: true)
}

/// The permission prompter interface — implemented by TUI and headless modes.
#[async_trait::async_trait]
pub trait PermissionPrompter: Send + Sync {
    async fn prompt(
        &self,
        tool_name: &str,
        tool_input: &Value,
        cancel: &CancellationToken,
    ) -> CcResult<PromptDecision>;
}
```

**Key changes from spike:**
- Added `PermissionSource` for audit trail
- `PermissionRule` now supports structured rules (tool + input matcher)
- `PermissionPrompter` trait extracted to cc-core (was inline in cc-query)
- `PromptDecision` moved to cc-core (was in cc-query::prompter)

#### 1.4 Hook Types (`cc_core::hook`) — NEW

```rust
/// All hook event names (27 events from TS source).
/// Only the 4 stable categories are in scope for Phase 2.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum HookEvent {
    // Tool lifecycle
    PreToolUse,
    PostToolUse,
    // Notification
    Notification,
    // Session lifecycle
    SessionStart,
    SessionStop,
    // Query lifecycle
    PreApiCall,
    PostApiCall,
    // Model output
    ModelResponse,
    // Subagent
    SubagentStart,
    SubagentStop,
}

/// Hook execution type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookKind {
    Command,             // shell command, JSON on stdin
    Prompt,              // text injected into context
    Http,                // POST JSON to URL
    Agent,               // delegate to subagent
}

/// Matcher for filtering which tool invocations trigger a hook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookMatcher {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,           // glob pattern
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_contains: Option<String>,      // substring match on input JSON
}

/// A single hook configuration entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookConfig {
    #[serde(rename = "type", default = "default_hook_kind")]
    pub kind: HookKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default = "default_hook_timeout")]
    pub timeout: u64,                        // seconds, default 600
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<HookMatcher>,
    /// Forward-compat: preserve unknown fields.
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

fn default_hook_kind() -> HookKind { HookKind::Command }
fn default_hook_timeout() -> u64 { 600 }

/// Result of running a single hook.
#[derive(Debug, Clone)]
pub enum HookOutcome {
    Ok,
    Block(String),                           // exit 2 or {"block": true}
    Failed(String),                          // non-blocking error
}
```

#### 1.5 Task Types (`cc_core::task`) — NEW

```rust
/// The 4 stable task types (from Decision 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    LocalBash,
    LocalAgent,
    InProcessTeammate,
    RemoteAgent,
}

/// Task lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Unique task identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub String);

impl TaskId {
    pub fn new() -> Self { Self(uuid::Uuid::new_v4().to_string()) }
}

/// Base state shared by all task types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStateBase {
    pub id: TaskId,
    pub kind: TaskKind,
    pub status: TaskStatus,
    pub description: String,
    pub is_backgrounded: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Last output lines (for status display).
    #[serde(default)]
    pub output_tail: Vec<String>,
}

/// Task notification sent to the parent agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskNotification {
    pub task_id: TaskId,
    pub status: TaskStatus,
    pub summary: Option<String>,
}
```

#### 1.6 Error Hierarchy (`cc_core::error`) — EXPAND from spike

```rust
#[derive(Debug, Error)]
pub enum CcError {
    #[error("API error: {message}")]
    Api { message: String, status: Option<u16>, retryable: bool },

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Tool error: {tool}: {message}")]
    Tool { tool: String, message: String },

    #[error("MCP error: {server}: {message}")]
    Mcp { server: String, message: String },

    #[error("Hook error: {event}: {message}")]
    Hook { event: String, message: String },

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Permission denied: {tool}: {reason}")]
    PermissionDenied { tool: String, reason: String },

    #[error("Cancelled")]
    Cancelled,

    #[error("Rate limited (retry after {retry_after:?}s)")]
    RateLimited { retry_after: Option<u64> },

    #[error("{0}")]
    Other(String),
}

impl CcError {
    /// Whether this error is safe to retry.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Api { retryable: true, .. } | Self::RateLimited { .. })
    }

    /// Whether this error should abort the entire session.
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::Auth(_) | Self::Config(_))
    }
}

pub type CcResult<T> = Result<T, CcError>;
```

**Key changes from spike:**
- `Api` variant now has `status` and `retryable` fields
- `Io` uses `#[from]` directly (spike stringified the error, losing context)
- Added `Cancelled`, `RateLimited`, `Mcp`, `Hook` variants
- `Tool` and `PermissionDenied` now include the tool name
- Added `is_retryable()` and `is_fatal()` classification methods

#### 1.7 Serialization Contracts

| Type | Serde Strategy | Wire Format |
|------|---------------|-------------|
| `ContentBlock` | `#[serde(tag = "type", rename_all = "snake_case")]` | `{"type":"text","text":"..."}` |
| `MessageContent` | `#[serde(untagged)]` | String or array of blocks |
| `Role` | `#[serde(rename_all = "lowercase")]` | `"user"` / `"assistant"` |
| `StopReason` | `#[serde(rename_all = "snake_case")]` | `"end_turn"` / `"tool_use"` |
| `PermissionRule` | `#[serde(untagged)]` | String or `{tool, input}` object |
| `HookEvent` | `#[serde(rename_all = "PascalCase")]` | `"PreToolUse"` etc. |
| `TaskKind` | `#[serde(rename_all = "snake_case")]` | `"local_bash"` etc. |
| `ImageSource` | `#[serde(tag = "type")]` | `{"type":"base64",...}` |
| Unknown fields | `#[serde(flatten)] extra: HashMap<String, Value>` | Preserved round-trip |

**Rule:** All types that cross crate boundaries or hit disk/wire derive `Serialize + Deserialize`.
Internal-only types (like `PermissionSource`) need not.

### 2. State & Concurrency Model
- [x] `AppState` structure and access pattern (`Arc<RwLock<T>>` vs finer-grained)
- [x] `TeamContext` shared state for swarm teammates
- [x] Cancellation token tree (maps to TS AbortController nesting)
- [x] Channel topology (mpsc for mailbox, watch for broadcasts)

#### 2.1 AppState — Fine-grained Arc wrapping

The TS version uses React state + mutable singletons. The Rust version uses
a struct-of-Arcs pattern: each independently-updatable piece of state gets
its own `Arc<RwLock<T>>` (or `Arc<Mutex<T>>` for write-heavy fields).

```rust
use tokio::sync::{RwLock, Mutex, mpsc, watch};
use tokio_util::sync::CancellationToken;

/// Top-level application state. Cheaply cloneable (all fields are Arc'd).
#[derive(Clone)]
pub struct AppState {
    // ── Immutable after init (no lock needed) ──────────────────
    pub config: Arc<ResolvedConfig>,          // settings, model, project root
    pub tools: Arc<Vec<BoxTool>>,             // registered tools (built-in + MCP)
    pub auth: Arc<AuthCredential>,            // API key or OAuth token

    // ── Read-heavy, write-rare ─────────────────────────────────
    pub session: Arc<RwLock<Session>>,         // transcript + session ID
    pub permissions: Arc<RwLock<PermissionEngine>>, // rules + session allows

    // ── Write-heavy ────────────────────────────────────────────
    pub tasks: Arc<Mutex<TaskRegistry>>,       // background task tracking
    pub usage: Arc<Mutex<UsageTracker>>,       // cumulative token/cost stats

    // ── Cancellation ───────────────────────────────────────────
    pub cancel: CancellationToken,             // root cancellation (Ctrl+C)

    // ── Channels ───────────────────────────────────────────────
    pub events_tx: mpsc::Sender<AppEvent>,     // TUI ← engine event bus
    pub status_rx: watch::Receiver<StatusLine>, // status bar broadcast
}
```

**Why struct-of-Arcs over `Arc<RwLock<AppState>>`:**
- Avoids contention: streaming updates to `usage` don't block `session` reads
- Each subsystem takes only the Arcs it needs (query engine gets `session + permissions + tools + cancel`)
- Adding new state fields doesn't widen existing lock scopes

**Lifetime rule:** `AppState` is constructed once in `main()` and cloned into
each subsystem. No `&'a AppState` references — everything is owned via Arc.

#### 2.2 CancellationToken Tree

Maps to TS's nested `AbortController` pattern. `tokio_util::sync::CancellationToken`
supports child tokens that cancel when the parent cancels.

```
root_cancel (Ctrl+C / /exit)
├── turn_cancel (current API turn — reset between turns)
│   ├── tool_cancel (individual tool execution)
│   └── stream_cancel (SSE stream reading)
└── task_cancels (one per background task)
    ├── teammate_turn_cancel (in-process teammate's current turn)
    └── ...
```

```rust
/// Created at the start of each turn; cancelled on Ctrl+C or when
/// the turn naturally completes.
fn start_turn(root: &CancellationToken) -> CancellationToken {
    root.child_token()
}

/// Created per tool execution; cancelled if the turn is cancelled.
fn start_tool(turn: &CancellationToken) -> CancellationToken {
    turn.child_token()
}
```

**Two-level abort for teammates (from A1 contracts):**
1. **Soft cancel** (teammate turn): cancels `teammate_turn_cancel` → teammate finishes current
   tool, returns partial result.
2. **Hard kill** (task): cancels the task-level token → drops the entire task.

#### 2.3 Channel Topology

```
                       ┌─────────────┐
                       │  TUI render  │
                       │    loop      │
                       └──────▲───────┘
                              │ recv
                    ┌─────────┴──────────┐
                    │   events (mpsc)     │  AppEvent enum
                    └─────────▲──────────┘
          ┌───────────────────┼───────────────────┐
          │ send              │ send               │ send
   ┌──────┴──────┐   ┌───────┴───────┐   ┌───────┴───────┐
   │ QueryEngine │   │  HookRunner   │   │  TaskRunner   │
   └─────────────┘   └───────────────┘   └───────────────┘

   status_tx (watch) ─── broadcasts StatusLine to all subscribers
   mailbox (mpsc per teammate) ─── parent ↔ teammate messages
```

```rust
/// Events sent from engine/hooks/tasks to the TUI.
#[derive(Debug, Clone)]
pub enum AppEvent {
    // Streaming
    StreamDelta(String),                     // partial text token
    StreamToolUse(ToolUseBlock),             // model wants to call a tool
    StreamEnd(StopReason),                   // model stopped

    // Tool execution
    ToolStart { name: String, input: Value },
    ToolEnd { name: String, result: ToolResult },

    // Permission
    PermissionRequest {
        id: u64,
        tool_name: String,
        tool_input: Value,
        response_tx: oneshot::Sender<PromptDecision>,
    },

    // Session
    CompactBoundary,                         // auto-compact happened
    TurnComplete { usage: Usage },

    // Tasks
    TaskUpdate(TaskNotification),

    // Fatal
    Error(CcError),
}

/// Status bar state (broadcast to all TUI components).
#[derive(Debug, Clone, Default)]
pub struct StatusLine {
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cost_usd: f64,
    pub active_tasks: usize,
    pub session_id: String,
}
```

**Permission request flow:**
1. QueryEngine encounters `Ask` → sends `PermissionRequest` with a `oneshot::Sender`
2. TUI receives event, renders dialog, waits for user input
3. TUI sends `PromptDecision` back through the oneshot channel
4. QueryEngine receives decision and continues

This avoids the engine needing to know about the TUI directly.

#### 2.4 TeamContext (Swarm Shared State)

```rust
/// Shared context for in-process teammates.
/// Each teammate gets a clone; the parent holds the original.
#[derive(Clone)]
pub struct TeamContext {
    pub config: Arc<ResolvedConfig>,
    pub auth: Arc<AuthCredential>,
    pub tools: Arc<Vec<BoxTool>>,
    /// Parent's permission engine (teammates inherit parent's rules).
    pub permissions: Arc<RwLock<PermissionEngine>>,
    /// Mailbox for sending messages to the parent.
    pub parent_tx: mpsc::Sender<TeammateMessage>,
    /// Task registry (shared — parent can see teammate task state).
    pub tasks: Arc<Mutex<TaskRegistry>>,
    /// Teammate's own cancellation token (child of parent's root).
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeammateMessage {
    pub task_id: TaskId,
    pub kind: TeammateMessageKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TeammateMessageKind {
    StatusUpdate(String),
    Output(String),
    PermissionEscalation { tool_name: String, tool_input: Value },
    Completed { summary: String },
    Failed { error: String },
}
```

### 3. API Client Design
- [x] `cc-http` shared client builder (mTLS, proxy, timeouts)
- [x] `cc-api` streaming interface (SSE → typed event stream)
- [x] Usage extraction and cost tracking
- [x] Retry/backoff policy

**Spike audit:** cc-api is **REWRITE** — spike client works but lacks mTLS, retry,
cancellation support, and proper error classification.

#### 3.1 cc-http — Shared HTTP Client Builder (NEW crate)

```rust
/// Configuration for building a shared HTTP client.
/// Used by both cc-api (Anthropic API) and cc-mcp (HTTP transports).
pub struct HttpClientConfig {
    /// mTLS client certificate path (env: CLAUDE_CODE_CLIENT_CERT)
    pub client_cert: Option<PathBuf>,
    /// mTLS client key path (env: CLAUDE_CODE_CLIENT_KEY)
    pub client_key: Option<PathBuf>,
    /// CA bundle override (env: CLAUDE_CODE_CA_BUNDLE or NODE_EXTRA_CA_CERTS)
    pub ca_bundle: Option<PathBuf>,
    /// HTTP/HTTPS proxy (env: HTTPS_PROXY or HTTP_PROXY)
    pub proxy: Option<String>,
    /// Connect timeout (default: 30s)
    pub connect_timeout: Duration,
    /// Request timeout (default: 10min for streaming, 30s for RPC)
    pub request_timeout: Option<Duration>,
}

impl HttpClientConfig {
    /// Build from environment variables.
    pub fn from_env() -> Self { ... }
}

/// Build a reqwest::Client with mTLS, proxy, and timeout configuration.
pub fn build_client(config: &HttpClientConfig) -> CcResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(config.connect_timeout);

    if let Some(ref proxy) = config.proxy {
        builder = builder.proxy(reqwest::Proxy::all(proxy)?);
    }
    if let Some(ref ca) = config.ca_bundle {
        let cert = reqwest::Certificate::from_pem(&std::fs::read(ca)?)?;
        builder = builder.add_root_certificate(cert);
    }
    if let (Some(ref cert), Some(ref key)) = (&config.client_cert, &config.client_key) {
        let identity = reqwest::Identity::from_pem(
            &[std::fs::read(cert)?, std::fs::read(key)?].concat()
        )?;
        builder = builder.identity(identity);
    }
    if let Some(timeout) = config.request_timeout {
        builder = builder.timeout(timeout);
    }
    Ok(builder.build()?)
}
```

**Env var contract (from Decision 5):**
- `CLAUDE_CODE_CLIENT_CERT` / `CLAUDE_CODE_CLIENT_KEY` → mTLS identity
- `CLAUDE_CODE_CA_BUNDLE` or `NODE_EXTRA_CA_CERTS` → custom CA
- `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY` → proxy
- `ANTHROPIC_BASE_URL` → API base URL override

#### 3.2 cc-api — Streaming Interface (REWRITE)

```rust
/// Authentication credential.
#[derive(Clone)]
pub enum AuthCredential {
    ApiKey(String),
    OAuthToken(String),
}

/// Anthropic Messages API client.
pub struct ApiClient {
    http: reqwest::Client,
    auth: AuthCredential,
    base_url: String,
}

impl ApiClient {
    pub fn new(http: reqwest::Client, auth: AuthCredential) -> Self {
        let base_url = std::env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".into());
        Self { http, auth, base_url }
    }

    /// Stream a message, returning a typed event stream.
    /// Cancellable via the CancellationToken.
    pub async fn stream_message(
        &self,
        request: &CreateMessageRequest,
        cancel: &CancellationToken,
    ) -> CcResult<impl Stream<Item = CcResult<StreamEvent>>> {
        let response = self.send_request(request, cancel).await?;
        Ok(SseStream::new(response.bytes_stream(), cancel.clone()))
    }

    /// Convenience: stream and accumulate into a complete Message.
    pub async fn complete_message(
        &self,
        request: &CreateMessageRequest,
        on_delta: impl FnMut(StreamDelta),
        cancel: &CancellationToken,
    ) -> CcResult<(Message, Usage)> { ... }
}

/// Deltas emitted during streaming (for TUI display).
#[derive(Debug, Clone)]
pub enum StreamDelta {
    Text(String),
    Thinking(String),
    ToolUseStart { id: String, name: String },
    InputJsonDelta(String),
}
```

**Changes from spike:**
- Takes `reqwest::Client` from cc-http (not built internally)
- `stream_message()` returns `impl Stream` instead of `mpsc::Receiver`
- Cancellation token threaded through
- `StreamDelta` type for TUI consumption

#### 3.3 Usage Tracking

```rust
/// Cumulative usage across a session.
#[derive(Debug, Default)]
pub struct UsageTracker {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_creation_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub turn_count: u32,
}

impl UsageTracker {
    pub fn record(&mut self, usage: &Usage) {
        self.total_input_tokens += usage.input_tokens as u64;
        self.total_output_tokens += usage.output_tokens as u64;
        if let Some(c) = usage.cache_creation_input_tokens {
            self.total_cache_creation_tokens += c as u64;
        }
        if let Some(c) = usage.cache_read_input_tokens {
            self.total_cache_read_tokens += c as u64;
        }
        self.turn_count += 1;
    }

    /// Estimate cost in USD (rough, based on public pricing).
    pub fn estimated_cost_usd(&self, model: &str) -> f64 { ... }

    /// Total "effective" input tokens for auto-compact threshold check.
    /// = input_tokens from the most recent API response (per Decision 7).
    pub fn last_input_tokens(&self) -> u32 { ... }
}
```

#### 3.4 Retry/Backoff Policy

```rust
/// Retry configuration.
pub struct RetryPolicy {
    pub max_retries: u32,            // default: 2
    pub initial_backoff: Duration,    // default: 1s
    pub max_backoff: Duration,        // default: 30s
    pub backoff_multiplier: f64,      // default: 2.0
}

impl RetryPolicy {
    /// Determine whether and when to retry after an error.
    pub fn should_retry(&self, error: &CcError, attempt: u32) -> Option<Duration> {
        if attempt >= self.max_retries { return None; }
        match error {
            CcError::RateLimited { retry_after } => {
                Some(Duration::from_secs(retry_after.unwrap_or(5)))
            }
            CcError::Api { retryable: true, .. } => {
                let backoff = self.initial_backoff.mul_f64(
                    self.backoff_multiplier.powi(attempt as i32)
                );
                Some(backoff.min(self.max_backoff))
            }
            _ => None,
        }
    }
}
```

**Note:** The TS version does no auto-retry (Claude decides on retry).
We add minimal retry for transient 429/5xx only. Tool execution failures
are never retried automatically.

### 4. Tool Loop & Query Engine
- [x] `cc-query` state machine (prompt → stream → tool calls → permission → execute → loop)
- [x] Auto-compact check (sync, per Decision 7)
- [x] Task output attachment mechanism
- [x] Tool registration and dispatch

**Spike audit:** cc-query is **REWRITE** — spike engine works but is monolithic.
Phase 2 must split into a state machine with proper cancellation, event-driven
communication with TUI, and support for concurrent read-only tool execution.

#### 4.1 Query Engine State Machine

```
            ┌─────────┐
            │  Idle    │ ◄─────────────────────────────────┐
            └────┬─────┘                                   │
                 │ run_turn(user_text)                     │
                 ▼                                         │
            ┌─────────┐                                   │
            │Streaming │── StreamDelta → TUI               │
            │ Response │── StreamToolUse → accumulate       │
            └────┬─────┘                                   │
                 │ StreamEnd                               │
                 ▼                                         │
         ┌──────────────┐                                  │
         │ stop_reason?  │                                  │
         └──┬────────┬──┘                                  │
   EndTurn  │        │ ToolUse                             │
            │        ▼                                     │
            │  ┌───────────┐                               │
            │  │ Execute    │── hooks → permissions → run   │
            │  │ Tools      │── concurrent if all read-only │
            │  └─────┬─────┘                               │
            │        │ all results collected                │
            │        ▼                                     │
            │  ┌───────────┐                               │
            │  │ Compact?   │── if input_tokens > threshold │
            │  └─────┬─────┘                               │
            │        │                                     │
            │        └─── continue loop ───────────────────┘
            │
            ▼
       ┌──────────┐
       │ TurnDone  │ → emit TurnComplete event
       └──────────┘
```

#### 4.2 Redesigned QueryEngine

```rust
pub struct QueryEngine {
    // ── Dependencies (injected) ────────────────────────────────
    api: ApiClient,
    tools: Arc<ToolRegistry>,
    permissions: Arc<RwLock<PermissionEngine>>,
    hooks: Arc<HookRunner>,
    session: Arc<RwLock<Session>>,
    system_blocks: Vec<SystemBlock>,
    options: QueryOptions,

    // ── Communication ──────────────────────────────────────────
    events_tx: mpsc::Sender<AppEvent>,

    // ── State ──────────────────────────────────────────────────
    usage: UsageTracker,
    compacted_last_turn: bool,
}

impl QueryEngine {
    /// Run a complete conversation turn.
    /// Returns when the model produces EndTurn/MaxTokens/StopSequence,
    /// or when cancelled.
    pub async fn run_turn(
        &mut self,
        user_text: &str,
        messages: &mut Vec<MessageParam>,
        turn_cancel: &CancellationToken,
    ) -> CcResult<TurnResult> {
        self.compacted_last_turn = false;

        // Add user message
        let user_msg = MessageParam::user(user_text);
        self.session.write().await.append(&user_msg)?;
        messages.push(user_msg);

        let mut turn_count = 0;
        loop {
            turn_count += 1;
            if turn_count > MAX_TURNS { return Err(CcError::Other("max turns".into())); }

            // Stream API response
            let stream = self.api.stream_message(&self.build_request(messages), turn_cancel).await?;
            let (message, usage) = self.consume_stream(stream, turn_cancel).await?;

            self.usage.record(&usage);
            self.append_assistant_message(messages, &message).await?;

            // Handle stop reason
            let tool_uses = extract_tool_uses(&message.content);
            match message.stop_reason {
                Some(StopReason::ToolUse) if !tool_uses.is_empty() => {
                    let results = self.execute_tools(&tool_uses, turn_cancel).await?;
                    self.append_tool_results(messages, results).await?;

                    // Auto-compact check (Decision 7: use usage.input_tokens)
                    if usage.input_tokens > self.compact_threshold() {
                        compact_messages(messages);
                        self.compacted_last_turn = true;
                        self.events_tx.send(AppEvent::CompactBoundary).await.ok();
                    }
                }
                _ => break,
            }
        }

        let result = TurnResult {
            usage: self.usage.clone(),
            compacted: self.compacted_last_turn,
        };
        self.events_tx.send(AppEvent::TurnComplete { usage: self.usage.last_usage() }).await.ok();
        Ok(result)
    }
}
```

#### 4.3 Concurrent Tool Execution

```rust
impl QueryEngine {
    async fn execute_tools(
        &self,
        tool_uses: &[ToolUseBlock],
        cancel: &CancellationToken,
    ) -> CcResult<Vec<ToolResultBlock>> {
        // Partition into read-only and mutating tools
        let (read_only, mutating): (Vec<_>, Vec<_>) = tool_uses.iter()
            .partition(|tu| self.tools.get(&tu.name).map_or(false, |t| t.is_read_only()));

        let mut results = Vec::with_capacity(tool_uses.len());

        // Run read-only tools concurrently
        if !read_only.is_empty() {
            let futures: Vec<_> = read_only.iter()
                .map(|tu| self.execute_one_tool(tu, cancel))
                .collect();
            let concurrent_results = futures::future::join_all(futures).await;
            results.extend(concurrent_results);
        }

        // Run mutating tools sequentially
        for tu in &mutating {
            results.push(self.execute_one_tool(tu, cancel).await);
        }

        // Re-sort to match original tool_use order
        // (API expects tool_results in same order as tool_uses)
        results.sort_by_key(|r| {
            tool_uses.iter().position(|tu| tu.id == r.tool_use_id).unwrap_or(usize::MAX)
        });

        Ok(results)
    }
}
```

#### 4.4 Tool Registry

```rust
/// Central registry of all available tools (built-in + MCP).
pub struct ToolRegistry {
    tools: HashMap<String, BoxTool>,
    /// Ordered list for API tool definitions.
    ordered: Vec<String>,
}

impl ToolRegistry {
    pub fn new() -> Self { Self { tools: HashMap::new(), ordered: Vec::new() } }

    pub fn register(&mut self, tool: BoxTool) {
        let name = tool.name().to_string();
        self.ordered.push(name.clone());
        self.tools.insert(name, tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.ordered.iter()
            .filter_map(|n| self.tools.get(n))
            .map(|t| t.to_definition())
            .collect()
    }
}
```

#### 4.5 Auto-Compact (Decision 7)

Per Decision 7, we use `usage.input_tokens` from the API response (not our own tokenizer).

```rust
/// Compact threshold = effective_context_window - 13,000
/// Default effective_context_window = 200,000 (claude-sonnet-4-6)
fn compact_threshold(&self) -> u32 {
    let context_window = self.effective_context_window();
    context_window.saturating_sub(13_000)
}

fn effective_context_window(&self) -> u32 {
    // Configurable via CLAUDE_CODE_MAX_CONTEXT_TOKENS
    std::env::var("CLAUDE_CODE_MAX_CONTEXT_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200_000)
}
```

#### 4.6 Task Output Attachment

When a background task completes, its output is injected into the next user
message as a system-like prefix:

```rust
/// Collect pending task outputs and prepend to the user message.
fn attach_task_outputs(user_text: &str, tasks: &mut TaskRegistry) -> String {
    let outputs = tasks.drain_completed_outputs();
    if outputs.is_empty() {
        return user_text.to_string();
    }
    let mut text = String::new();
    for (task_id, summary) in &outputs {
        text.push_str(&format!("[Task {task_id} completed: {summary}]\n"));
    }
    text.push_str(user_text);
    text
}

### 5. Hook Engine
- [x] Hook config snapshot and trust enforcement
- [x] 27-event dispatch with matcher filtering
- [x] Parallel execution with timeout and abort
- [x] `CLAUDE_ENV_FILE` mechanism

**Spike audit:** cc-hooks is **REWRITE** — spike had 4 hook kinds but only PreToolUse
wiring. Phase 2 needs all 27 events, matcher filtering, parallel execution,
structured JSON output, deduplication, and config snapshotting.

#### 5.1 Hook Configuration Model

```rust
/// A single hook configuration entry (from settings.json).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookConfig {
    #[serde(rename = "type", default = "default_command")]
    pub kind: HookKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,               // default: "bash"
    #[serde(rename = "if", default, skip_serializing_if = "Option::is_none")]
    pub if_condition: Option<String>,        // e.g. "Bash(git *)"
    #[serde(default = "default_timeout")]
    pub timeout: u64,                        // seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    #[serde(default)]
    pub once: bool,                          // run at most once per session
    #[serde(rename = "async", default)]
    pub is_async: bool,
    #[serde(default)]
    pub async_rewake: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_env_vars: Option<Vec<String>>,
    /// Preserve unknown fields.
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// A matcher group: event + matcher pattern + list of hooks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookMatcherGroup {
    pub matcher: Option<String>,             // glob pattern for tool_name, source, etc.
    pub hooks: Vec<HookConfig>,
}

/// All hooks for all events, as stored in settings.json.
pub type HooksSettings = HashMap<String, Vec<HookMatcherGroup>>;
```

#### 5.2 Hook Runner Architecture

```rust
pub struct HookRunner {
    /// Frozen snapshot of hook configuration (taken at session start).
    config_snapshot: HooksSettings,
    /// Session-scoped hooks (added at runtime, e.g. by function hooks).
    session_hooks: RwLock<HooksSettings>,
    /// Set of hooks that have already fired (for `once: true`).
    fired_once: Mutex<HashSet<HookKey>>,
    /// HTTP client (shared with cc-http).
    http: reqwest::Client,
    /// Policy flags.
    managed_only: bool,
    disabled: bool,
}

/// Deduplication key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct HookKey {
    command_or_url: String,
    if_condition: Option<String>,
    namespace: Option<String>,
}

impl HookRunner {
    /// Snapshot the config at session start (prevents race with settings changes).
    pub fn new(settings: &HooksSettings, http: reqwest::Client) -> Self { ... }

    /// Run all hooks for an event. Hooks execute in parallel with individual timeouts.
    pub async fn run(
        &self,
        event: &str,
        input: &HookInput,
        cancel: &CancellationToken,
    ) -> HookRunResult { ... }
}
```

#### 5.3 Execution Flow

```rust
impl HookRunner {
    pub async fn run(
        &self,
        event: &str,
        input: &HookInput,
        cancel: &CancellationToken,
    ) -> HookRunResult {
        if self.disabled { return HookRunResult::empty(); }

        // 1. Collect matching hooks from snapshot + session
        let hooks = self.collect_matching_hooks(event, input);

        // 2. Deduplicate by (command_or_url, if_condition, namespace)
        let hooks = self.deduplicate(hooks);

        // 3. Filter once-already-fired
        let hooks = self.filter_once(hooks);

        // 4. Execute ALL in parallel with individual timeouts
        let futures: Vec<_> = hooks.into_iter().map(|h| {
            let cancel = cancel.clone();
            async move {
                let timeout = Duration::from_secs(h.config.timeout);
                tokio::time::timeout(timeout, self.execute_one(&h, input, &cancel)).await
            }
        }).collect();

        let outcomes = futures::future::join_all(futures).await;

        // 5. Aggregate results
        HookRunResult::aggregate(outcomes)
    }
}
```

#### 5.4 Hook Input (Stdin JSON)

```rust
/// Data sent to hooks on stdin as JSON.
#[derive(Debug, Clone, Serialize)]
pub struct HookInput {
    pub session_id: String,
    pub transcript_path: String,
    pub cwd: String,
    pub permission_mode: String,
    pub hook_event_name: String,
    // Event-specific fields
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_response: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,              // SessionStart source
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,             // Notification
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}
```

#### 5.5 Structured JSON Output

```rust
/// Structured JSON response from a hook (stdout).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct HookJsonResponse {
    #[serde(default, rename = "continue")]
    pub should_continue: Option<bool>,
    pub stop_reason: Option<String>,
    pub decision: Option<String>,           // "block" | "allow"
    pub reason: Option<String>,
    pub system_message: Option<String>,
    pub suppress_output: Option<bool>,
    pub hook_specific_output: Option<HookSpecificOutput>,
    #[serde(rename = "async", default)]
    pub is_async: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct HookSpecificOutput {
    pub permission_decision: Option<String>,
    pub permission_decision_reason: Option<String>,
    pub updated_input: Option<Value>,
    pub additional_context: Option<String>,
}
```

#### 5.6 CLAUDE_ENV_FILE Mechanism

Hooks can export environment variables by writing to a temp file whose path is
in `$CLAUDE_ENV_FILE`:

```rust
/// Create a temp file for hook env exports and set CLAUDE_ENV_FILE.
fn setup_env_file() -> CcResult<(PathBuf, TempFile)> {
    let temp = tempfile::NamedTempFile::new()?;
    let path = temp.path().to_owned();
    std::env::set_var("CLAUDE_ENV_FILE", &path);
    Ok((path, temp))
}

/// After hook execution, read env vars from CLAUDE_ENV_FILE.
fn read_env_exports(path: &Path) -> HashMap<String, String> {
    let Ok(content) = std::fs::read_to_string(path) else { return HashMap::new(); };
    content.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}
```

### 6. Project Discovery & Memory
- [x] Git walk-up + canonical root (with security validations)
- [x] Settings 6-layer merge
- [x] CLAUDE.md walk-up with 6 memory types
- [x] @-include resolution

**Spike audit:** cc-config is **REWRITE** (3-layer merge, no project discovery).
cc-memory is **REWRITE** (flat `~/.claude/memory/` only, no walk-up).
Phase 2 consolidates both + cc-skills + cc-plugins into the **cc-memory** crate
(per Decision 3) and adds full project discovery to **cc-config**.

#### 6.1 Project Discovery (in cc-config)

```rust
/// Resolved project context — computed once at startup, immutable thereafter.
#[derive(Debug, Clone)]
pub struct ProjectContext {
    /// The original working directory (captured at startup, never changed).
    pub original_cwd: PathBuf,
    /// Git root of the current working directory (None if not in a git repo).
    pub git_root: Option<PathBuf>,
    /// Canonical root (resolves through worktree chains to main repo).
    /// Falls back to git_root if not a worktree, or original_cwd if no git.
    pub canonical_root: PathBuf,
    /// True if we're running inside a git worktree.
    pub is_worktree: bool,
    /// Normalized path used as key in project-scoped config.
    pub config_key: String,
}

impl ProjectContext {
    /// Discover project context from the given starting directory.
    pub fn discover(start_path: &Path) -> Self {
        // 1. NFC-normalize path (macOS HFS+ decomposition)
        // 2. Walk up to find .git
        // 3. Resolve canonical root through worktree chain
        // 4. Security: validate symlinks, back-links
        ...
    }
}

/// Walk up from `start` looking for `.git` (file or directory).
fn find_git_root(start: &Path) -> Option<PathBuf> { ... }

/// Resolve through worktree .git file → gitdir → commondir → main repo.
/// Includes security validations (anti-symlink-attack).
fn find_canonical_root(git_root: &Path) -> Option<PathBuf> { ... }
```

#### 6.2 Settings 6-Layer Merge (in cc-config)

```rust
/// All settings sources, in merge priority order.
#[derive(Debug)]
pub struct SettingsSources {
    pub plugin_base: Option<Value>,         // 1. allowlisted keys only
    pub user: Option<Value>,                // 2. ~/.claude/settings.json
    pub project: Option<Value>,             // 3. .claude/settings.json
    pub local: Option<Value>,               // 4. .claude/settings.local.json
    pub flag: Option<Value>,                // 5. --settings CLI / SDK
    pub policy: Option<Value>,              // 6. managed (first-source-wins)
}

/// Resolved settings (after 6-layer merge).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedConfig {
    pub project: ProjectContext,
    pub model: String,
    pub max_tokens: u32,
    pub permissions: PermissionsConfig,
    pub hooks: HooksSettings,
    pub env: HashMap<String, String>,
    pub mcp_servers: HashMap<String, Value>,
    /// Full merged JSON (for forward-compat: unknown fields preserved).
    pub raw: Value,
}

/// Load and merge all settings sources.
pub fn load_settings(
    project: &ProjectContext,
    cli_settings_path: Option<&Path>,
) -> CcResult<ResolvedConfig> {
    let sources = discover_sources(project, cli_settings_path)?;
    let merged = merge_sources(sources)?;
    resolve_config(project.clone(), merged)
}

/// Deep merge: arrays union+dedup, objects recursive, primitives overwrite.
fn merge_json(base: Value, overlay: Value) -> Value { ... }

/// Security: strip dangerous keys from projectSettings.
fn sanitize_project_settings(mut settings: Value) -> Value {
    const DANGEROUS_KEYS: &[&str] = &[
        "skipDangerousModePermissionPrompt",
        "skipAutoPermissionPrompt",
        "useAutoModeDuringPlan",
        "autoMode",
    ];
    if let Some(obj) = settings.as_object_mut() {
        for key in DANGEROUS_KEYS {
            obj.remove(*key);
        }
    }
    settings
}
```

#### 6.3 CLAUDE.md Walk-Up (in cc-memory)

```rust
/// Memory type (determines loading priority and source gating).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryType {
    Managed,   // 1. policy path
    User,      // 2. ~/.claude/
    Project,   // 3. walk-up (root→CWD)
    Local,     // 4. walk-up CLAUDE.local.md
    AutoMem,   // 5. auto-memory
    TeamMem,   // 6. team memory
}

/// A loaded memory file.
#[derive(Debug, Clone)]
pub struct MemoryFile {
    pub path: PathBuf,
    pub memory_type: MemoryType,
    pub content: String,
    /// Frontmatter globs (for rules files).
    pub globs: Option<Vec<String>>,
    /// Path of file that @-included this one (None if top-level).
    pub parent: Option<PathBuf>,
}

/// Load all memory files for the current project.
pub fn load_memory_files(
    project: &ProjectContext,
    settings_sources_enabled: &SettingsSourcesEnabled,
) -> CcResult<Vec<MemoryFile>> {
    let mut files = Vec::new();

    // 1. Managed
    files.extend(load_managed_memory()?);

    // 2. User
    if settings_sources_enabled.user {
        files.extend(load_user_memory()?);
    }

    // 3 + 4. Project + Local (walk-up from root → CWD)
    if settings_sources_enabled.project || settings_sources_enabled.local {
        let dirs = walk_up_dirs(&project.original_cwd, &project.canonical_root);
        // Process in reverse (root first → CWD last = highest priority)
        for dir in dirs.iter().rev() {
            if settings_sources_enabled.project && !is_skipped_worktree_dir(dir, project) {
                files.extend(load_dir_project_memory(dir)?);
            }
            if settings_sources_enabled.local {
                files.extend(load_dir_local_memory(dir)?);
            }
        }
    }

    // 5. AutoMem
    files.extend(load_auto_memory()?);

    // 6. TeamMem (if enabled)
    files.extend(load_team_memory()?);

    Ok(files)
}

/// Collect directories from CWD up to root.
fn walk_up_dirs(cwd: &Path, root: &Path) -> Vec<PathBuf> { ... }

/// Skip Project-type files from dirs above worktree but within main repo.
fn is_skipped_worktree_dir(dir: &Path, project: &ProjectContext) -> bool {
    project.is_worktree
        && dir.starts_with(&project.canonical_root)
        && !dir.starts_with(project.git_root.as_deref().unwrap_or(dir))
}
```

#### 6.4 @-Include Resolution

```rust
/// Resolve @-includes in a memory file's content.
fn resolve_includes(
    content: &str,
    file_dir: &Path,
    memory_type: MemoryType,
    approval_config: &IncludeApprovalConfig,
    processed: &mut HashSet<PathBuf>,
) -> CcResult<Vec<MemoryFile>> {
    let mut included = Vec::new();

    for line in content.lines() {
        if let Some(path_ref) = parse_include_directive(line) {
            let resolved = resolve_include_path(path_ref, file_dir)?;

            // Circular reference prevention
            let canonical = resolved.canonicalize().unwrap_or(resolved.clone());
            if !processed.insert(canonical.clone()) { continue; }

            // Security gating
            if !is_include_allowed(memory_type, approval_config) { continue; }

            // Supported extensions check
            if !is_supported_extension(&resolved) { continue; }

            // Read and recursively resolve
            if let Ok(text) = std::fs::read_to_string(&resolved) {
                let nested = resolve_includes(
                    &text, resolved.parent().unwrap_or(file_dir),
                    memory_type, approval_config, processed,
                )?;
                included.extend(nested);
                included.push(MemoryFile {
                    path: resolved,
                    memory_type,
                    content: text,
                    globs: None,
                    parent: Some(file_dir.join("")), // parent file
                });
            }
        }
    }
    Ok(included)
}

/// Parse @path, @./rel, @~/home, @/abs from a line.
fn parse_include_directive(line: &str) -> Option<&str> { ... }

/// Resolve an include path relative to the including file's directory.
fn resolve_include_path(path_ref: &str, base_dir: &Path) -> CcResult<PathBuf> { ... }
```

#### 6.5 Skills & Plugin Loading (merged into cc-memory)

```rust
/// A skill definition (from ~/.claude/skills/*.md or plugin).
#[derive(Debug, Clone)]
pub struct SkillDef {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub content: String,
    /// Optional arguments schema.
    pub arguments: Option<Vec<SkillArgument>>,
    /// Tools this skill is allowed to use.
    pub allowed_tools: Option<Vec<String>>,
    /// Whether this skill is user-invocable as a slash command.
    pub user_invocable: bool,
}

/// Discover skills from ~/.claude/skills/ and plugin directories.
pub fn discover_skills(config_dir: &Path) -> CcResult<Vec<SkillDef>> { ... }
```

### 7. TUI Component Tree
- [x] Screen layout (input area, output area, status bar, permission dialog)
- [x] Event routing (crossterm → AppAction → state → render)
- [x] Slash command parsing and dispatch
- [x] Streaming render pipeline

**Spike audit:** cc-tui is **DISCARD** — abandoned in place per state.md.
Phase 2 redesigns from scratch using Ratatui + crossterm (Decision 2).

#### 7.1 Screen Layout

```
┌──────────────────────────────────────────────────────┐
│ ╭ Status Bar                                         │
│ │ model: claude-sonnet-4-6 | tokens: 1.2k/0.5k | $0.01 │
│ ╰────────────────────────────────────────────────────│
│                                                      │
│ ╭ Transcript Area (scrollable)                       │
│ │ > User: How do I...                                │
│ │                                                    │
│ │ Assistant: You can...                              │
│ │ [Tool: Bash] ls -la                                │
│ │ [Result] file1.rs file2.rs                         │
│ │ ...streaming text...█                              │
│ ╰────────────────────────────────────────────────────│
│                                                      │
│ ╭ Input Area (multi-line editor)                     │
│ │ > _                                                │
│ ╰────────────────────────────────────────────────────│
│                                                      │
│ ╭ Permission Dialog (overlay, shown when needed)     │
│ │ Allow Write to /path/file.rs? [y]es [n]o [a]lways │
│ ╰────────────────────────────────────────────────────│
└──────────────────────────────────────────────────────┘
```

**Component tree:**
```rust
pub struct App {
    // ── State ──────────────────────────────────────────
    pub mode: AppMode,
    pub transcript: Vec<TranscriptItem>,
    pub scroll_offset: usize,
    pub input: InputEditor,
    pub status: StatusLine,
    pub permission_dialog: Option<PermissionDialog>,
    pub spinner: Option<SpinnerState>,

    // ── Engine communication ───────────────────────────
    pub events_rx: mpsc::Receiver<AppEvent>,
    pub state: AppState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppMode {
    Input,               // User typing
    Streaming,           // Model responding
    PermissionPrompt,    // Waiting for permission decision
    CommandPalette,      // Slash command autocomplete
}

/// An item in the conversation transcript.
#[derive(Debug, Clone)]
pub enum TranscriptItem {
    UserMessage(String),
    AssistantText(String),
    ToolCall { name: String, input_summary: String },
    ToolResult { name: String, output: String, is_error: bool },
    CompactBoundary,
    SystemNotice(String),
}
```

#### 7.2 Event Routing

```
crossterm::Event ──► map_key_event() ──► AppAction
AppEvent (from engine) ──────────────► AppAction

AppAction ──► update(&mut App) ──► state change
state change ──► render(&App, &mut Frame) ──► terminal
```

```rust
/// All actions the TUI can take.
#[derive(Debug, Clone)]
pub enum AppAction {
    // Input
    InsertChar(char),
    Backspace,
    DeleteWord,
    Submit,                                  // Enter → send message
    NewLine,                                 // Shift+Enter → multi-line

    // Navigation
    ScrollUp(usize),
    ScrollDown(usize),
    ScrollToBottom,

    // Streaming
    StreamDelta(String),
    StreamEnd(StopReason),
    ToolStart { name: String, input: Value },
    ToolEnd { name: String, result: ToolResult },

    // Permission
    ShowPermission(PermissionDialog),
    PermissionAllow,
    PermissionAllowAlways,
    PermissionDeny,

    // Slash commands
    SlashCommand(String),
    AutocompleteNext,
    AutocompletePrev,
    AutocompleteAccept,

    // Control
    Abort,                                   // Ctrl+C
    Quit,                                    // Ctrl+Q or /exit
    CompactBoundary,
    TurnComplete { usage: Usage },
    Error(String),
}
```

#### 7.3 Main Loop

```rust
pub async fn run_tui(state: AppState) -> CcResult<()> {
    let mut terminal = ratatui::init();
    let mut app = App::new(state);

    loop {
        // Render
        terminal.draw(|frame| render(&app, frame))?;

        // Wait for next event
        let action = tokio::select! {
            // Terminal event (keypress, resize)
            event = crossterm_event_stream.next() => {
                map_terminal_event(event?, &app)
            }
            // Engine event (stream delta, tool result, etc.)
            event = app.events_rx.recv() => {
                map_app_event(event?)
            }
            // Tick (spinner animation, ~10 Hz)
            _ = tick_interval.tick() => {
                Some(AppAction::Tick)
            }
        };

        if let Some(action) = action {
            if app.update(action) == UpdateResult::Quit {
                break;
            }
        }
    }

    ratatui::restore();
    Ok(())
}
```

#### 7.4 Slash Command Dispatch

```rust
/// Registry of all slash commands (builtins + user skills).
pub struct CommandRegistry {
    builtins: Vec<BuiltinCommand>,
    skills: Vec<SkillCommand>,
}

/// A builtin slash command.
pub struct BuiltinCommand {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub execute: fn(&mut App, args: &str) -> CommandOutcome,
}

pub enum CommandOutcome {
    /// Message to send to the engine.
    Send(String),
    /// Action to apply locally.
    Action(AppAction),
    /// Exit the application.
    Exit,
    /// Display help text.
    Help(String),
    /// No-op.
    None,
}

/// Core builtins:
/// /help, /exit, /clear, /compact, /model, /memory, /config,
/// /resume, /continue, /status, /cost, /vim, /bug
const BUILTINS: &[BuiltinCommand] = &[ ... ];
```

#### 7.5 Streaming Render Pipeline

```rust
/// Render the transcript area with streaming text.
fn render_transcript(app: &App, area: Rect, buf: &mut Buffer) {
    // Virtual scrolling: only render visible lines
    let visible_lines = area.height as usize;
    let total_lines = app.transcript_line_count();
    let scroll = app.scroll_offset.min(total_lines.saturating_sub(visible_lines));

    for (i, line) in app.visible_lines(scroll, visible_lines).enumerate() {
        let y = area.y + i as u16;
        match line {
            TranscriptLine::UserText(text) => render_user_text(text, area.x, y, buf),
            TranscriptLine::AssistantText(text) => render_assistant_text(text, area.x, y, buf),
            TranscriptLine::ToolHeader(name) => render_tool_header(name, area.x, y, buf),
            TranscriptLine::ToolOutput(text) => render_tool_output(text, area.x, y, buf),
            TranscriptLine::Boundary => render_boundary(area.x, y, area.width, buf),
        }
    }

    // Auto-scroll to bottom when new content arrives (unless user scrolled up)
    if app.scroll_offset == 0 || app.mode == AppMode::Streaming {
        // Keep viewport at bottom
    }
}
```

---

### 8. Task Subsystem
- [x] Task framework (register, update, evict, notify)
- [x] Per-type implementations (local_bash, local_agent, in_process_teammate, remote_agent)
- [x] Swarm mailbox and permission delegation

**Spike audit:** cc-tasks and cc-agent are **DISCARD** (1-line stubs).
Phase 2 merges into **cc-agents** (Decision 3/6).

#### 8.1 Task Framework

```rust
/// Central registry for all running/completed tasks.
pub struct TaskRegistry {
    tasks: HashMap<TaskId, TaskEntry>,
    /// Completed task outputs waiting to be attached to next user message.
    pending_outputs: Vec<(TaskId, String)>,
    /// Maximum concurrent tasks.
    max_concurrent: usize,
}

struct TaskEntry {
    state: TaskStateBase,
    /// Handle to the spawned Tokio task.
    handle: JoinHandle<CcResult<TaskOutput>>,
    /// Cancellation token for this task.
    cancel: CancellationToken,
}

impl TaskRegistry {
    pub fn spawn(
        &mut self,
        kind: TaskKind,
        description: String,
        cancel: CancellationToken,
        future: impl Future<Output = CcResult<TaskOutput>> + Send + 'static,
    ) -> TaskId { ... }

    pub fn cancel(&mut self, id: &TaskId) { ... }
    pub fn cancel_all(&mut self) { ... }
    pub fn status(&self, id: &TaskId) -> Option<&TaskStateBase> { ... }
    pub fn running_count(&self) -> usize { ... }
    pub fn drain_completed_outputs(&mut self) -> Vec<(TaskId, String)> { ... }

    /// Evict completed tasks older than `max_age`.
    pub fn evict_old(&mut self, max_age: Duration) { ... }
}

#[derive(Debug, Clone)]
pub struct TaskOutput {
    pub summary: String,
    pub content: String,
}
```

#### 8.2 Task Type Implementations

```rust
/// Local Bash: run a shell command in the background.
pub async fn run_local_bash(
    command: String,
    cwd: PathBuf,
    cancel: CancellationToken,
) -> CcResult<TaskOutput> {
    let child = tokio::process::Command::new("bash")
        .arg("-c").arg(&command)
        .current_dir(&cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    tokio::select! {
        result = child.wait_with_output() => {
            let output = result?;
            Ok(TaskOutput {
                summary: format!("exit {}", output.status.code().unwrap_or(-1)),
                content: String::from_utf8_lossy(&output.stdout).into(),
            })
        }
        _ = cancel.cancelled() => {
            child.kill().await.ok();
            Err(CcError::Cancelled)
        }
    }
}

/// Local Agent: spawn a subagent with its own query engine.
pub async fn run_local_agent(
    prompt: String,
    team_ctx: TeamContext,
    cancel: CancellationToken,
) -> CcResult<TaskOutput> {
    let mut engine = QueryEngine::new_teammate(team_ctx, cancel.clone());
    let mut messages = Vec::new();
    let result = engine.run_turn(&prompt, &mut messages, &cancel).await?;
    Ok(TaskOutput {
        summary: "agent completed".into(),
        content: result.final_text,
    })
}

/// In-Process Teammate: like local_agent but with mailbox for parent↔child messages.
pub async fn run_in_process_teammate(
    prompt: String,
    team_ctx: TeamContext,
    mailbox_rx: mpsc::Receiver<TeammateMessage>,
    cancel: CancellationToken,
) -> CcResult<TaskOutput> { ... }

/// Remote Agent: delegate to a remote Claude Code instance via HTTP API.
pub async fn run_remote_agent(
    prompt: String,
    endpoint: String,
    http: reqwest::Client,
    cancel: CancellationToken,
) -> CcResult<TaskOutput> { ... }
```

#### 8.3 Swarm Mailbox

```rust
/// Mailbox for parent ↔ teammate communication.
pub struct Mailbox {
    /// Parent → teammate messages.
    pub to_teammate_tx: mpsc::Sender<TeammateMessage>,
    pub to_teammate_rx: mpsc::Receiver<TeammateMessage>,
    /// Teammate → parent messages.
    pub to_parent_tx: mpsc::Sender<TeammateMessage>,
    pub to_parent_rx: mpsc::Receiver<TeammateMessage>,
}

impl Mailbox {
    pub fn new(buffer: usize) -> (Self, MailboxHandle) {
        let (to_teammate_tx, to_teammate_rx) = mpsc::channel(buffer);
        let (to_parent_tx, to_parent_rx) = mpsc::channel(buffer);
        // Split into parent-side and teammate-side handles
        ...
    }
}
```

---

### 9. MCP Client
- [x] Transport abstraction (stdio, sse, http)
- [x] Server lifecycle management
- [x] Tool/resource discovery

**Spike audit:** cc-mcp is **REWRITE** — spike works for stdio + HTTP but lacks
mTLS (Decision 5), server lifecycle management, and proper error recovery.

#### 9.1 Transport Abstraction

```rust
/// Unified MCP transport interface.
#[async_trait::async_trait]
pub trait McpTransport: Send + Sync {
    /// Send a JSON-RPC request and receive the response.
    async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        cancel: &CancellationToken,
    ) -> CcResult<Value>;

    /// Send a JSON-RPC notification (no response expected).
    async fn notify(&self, method: &str, params: Option<Value>) -> CcResult<()>;

    /// Close the transport.
    async fn close(&self) -> CcResult<()>;
}

/// Stdio transport: child process with JSON-RPC over stdin/stdout.
pub struct StdioTransport {
    child: tokio::process::Child,
    stdin: tokio::io::BufWriter<ChildStdin>,
    stdout: tokio::io::BufReader<ChildStdout>,
    next_id: AtomicU64,
}

/// SSE transport: Server-Sent Events over HTTP.
pub struct SseTransport {
    http: reqwest::Client,
    base_url: String,
    session_id: Option<String>,
    next_id: AtomicU64,
}

/// Streamable HTTP transport (new MCP spec).
pub struct HttpTransport {
    http: reqwest::Client,
    endpoint: String,
    session_id: Option<String>,
    next_id: AtomicU64,
}
```

#### 9.2 Server Lifecycle Management

```rust
/// MCP server connection state.
pub enum McpServerState {
    Pending,
    Connected(ConnectedServer),
    Failed { error: String, retry_at: Option<Instant> },
    NeedsAuth { auth_url: String },
    Disabled,
}

/// A connected MCP server.
pub struct ConnectedServer {
    pub name: String,
    pub transport: Box<dyn McpTransport>,
    pub tools: Vec<McpTool>,
    pub resources: Vec<McpResource>,
}

/// MCP server manager — handles all configured servers.
pub struct McpManager {
    servers: HashMap<String, McpServerState>,
    http: reqwest::Client,
}

impl McpManager {
    /// Initialize all servers from config.
    pub async fn init_from_config(
        config: &HashMap<String, Value>,
        http: reqwest::Client,
    ) -> Self { ... }

    /// Connect a single server (with timeout and error handling).
    async fn connect_server(
        &mut self,
        name: &str,
        config: &Value,
    ) -> CcResult<()> {
        // 1. Determine transport type (stdio if `command`, sse/http if `url`)
        // 2. Create transport
        // 3. Initialize (JSON-RPC `initialize` handshake)
        // 4. List tools and resources
        ...
    }

    /// Get all tools from all connected servers.
    /// Tools are prefixed: `mcp__<server>__<tool>`.
    pub fn all_tools(&self) -> Vec<BoxTool> { ... }

    /// Reconnect a failed server.
    pub async fn reconnect(&mut self, name: &str) -> CcResult<()> { ... }

    /// Gracefully close all servers.
    pub async fn shutdown(&mut self) { ... }
}
```

#### 9.3 MCP Tool Adapter

```rust
/// Wraps an MCP tool as a cc-core Tool implementation.
pub struct McpToolAdapter {
    server_name: String,
    tool: McpTool,
    transport: Arc<dyn McpTransport>,
}

impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        // "mcp__<server>__<tool>"
        &self.prefixed_name
    }

    fn is_read_only(&self) -> bool { false } // MCP tools assumed mutating

    async fn execute(&self, input: Value, cancel: &CancellationToken) -> CcResult<ToolResult> {
        let result = self.transport.request(
            "tools/call",
            Some(json!({ "name": self.tool.name, "arguments": input })),
            cancel,
        ).await?;
        // Parse result.content[].text
        ...
    }
}
```

---

### 10. Session Management
- [x] JSONL transcript format
- [x] Resume/continue from existing session
- [x] Session list and selection

**Spike audit:** cc-session is **REUSE** — spike handles JSONL transcripts,
TS format resume, and session listing. Expand, don't rewrite.

#### 10.1 JSONL Transcript Format

```rust
/// A single entry in the transcript JSONL file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub message: MessageParam,
    pub timestamp: String,                   // ISO 8601
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_boundary: Option<bool>,
}
```

Session file layout:
```
~/.claude/sessions/<session-id>/
├── transcript.jsonl    # conversation messages
├── metadata.json       # session metadata (model, project, start time)
└── todos.json          # todo list state (if used)
```

#### 10.2 Session Struct

```rust
pub struct Session {
    pub id: String,
    dir: PathBuf,
    file: BufWriter<File>,
}

impl Session {
    pub fn new() -> CcResult<Self> {
        let id = uuid::Uuid::new_v4().to_string();
        let dir = config_dir().join("sessions").join(&id);
        std::fs::create_dir_all(&dir)?;
        let file = BufWriter::new(File::create(dir.join("transcript.jsonl"))?);
        Ok(Self { id, dir, file })
    }

    pub fn resume(id: &str) -> CcResult<Self> {
        // Try Rust format first: ~/.claude/sessions/<id>/transcript.jsonl
        // Fall back to TS format: ~/.claude/projects/<slug>/<id>.jsonl
        ...
    }

    pub fn append(&mut self, entry: &TranscriptEntry) -> CcResult<()> {
        let line = serde_json::to_string(entry)?;
        writeln!(self.file, "{}", line)?;
        self.file.flush()?;
        Ok(())
    }

    pub fn load_messages(&self) -> CcResult<Vec<MessageParam>> { ... }

    /// List all sessions, most recent first.
    pub fn list_sessions() -> CcResult<Vec<SessionInfo>> { ... }
}

/// Session metadata for listing.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub project: Option<String>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub message_count: usize,
}
```

#### 10.3 TS Format Compatibility

```rust
/// Load a TypeScript-format JSONL transcript.
/// TS format: each line is `{ "type": "user"|"assistant", "message": {...} }`
fn load_ts_transcript(path: &Path) -> CcResult<Vec<MessageParam>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut messages = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() { continue; }
        let entry: Value = serde_json::from_str(&line)?;

        // TS format wraps messages differently
        if let Some(msg) = entry.get("message") {
            let role = match entry.get("type").and_then(|t| t.as_str()) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                _ => continue,
            };
            let content = serde_json::from_value(msg.clone())?;
            messages.push(MessageParam { role, content });
        }
    }
    Ok(messages)
}

/// Find a TS-format session file by ID.
/// Searches ~/.claude/projects/<slug>/<id>.jsonl
fn find_ts_session(id: &str) -> Option<PathBuf> {
    let projects_dir = config_dir().join("projects");
    // Walk all project slugs looking for matching session ID
    ...
}
```
