use std::io::{self, Write};
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use serde_json::json;
use tracing_subscriber::EnvFilter;

use cc_api::{ApiClient, AuthCredential};
use cc_auth::{ensure_fresh_credentials, Credentials};
use cc_config::{load_settings, resolve_model};
use cc_core::{MessageParam, SystemBlock, ThinkingConfig};
use cc_hooks::{HookContext, HookRunner, HooksSettings};
use cc_permissions::PermissionEngine;
use cc_query::{
    engine::{QueryEngine, QueryEngineConfig, QueryOptions},
    ApiSummarizer, StdinPrompter, SubAgentRunnerImpl, ToolRegistry,
};
use cc_session::{list_sessions, Session, SessionMetadata};
use cc_tools::{
    agent_tool::AgentTool, all_tools, ask_user_question::AskUserQuestionTool,
    team_create::TeamCreateTool,
};
use cc_tui::TuiConfig;

/// Claude Code — Rust implementation (Milestone 2: Tool Execution + Session)
#[derive(Debug, Parser)]
#[command(
    name = "claude",
    version,
    about = "Claude Code — AI coding assistant",
    long_about = None,
)]
struct Cli {
    /// Send a single message and stream the response to stdout.
    #[arg(short, long, value_name = "TEXT")]
    message: Option<String>,

    /// SDK / non-interactive mode: send a single prompt, print the response,
    /// exit with code 0. Equivalent to `--message <TEXT> --no-tui --non-interactive`.
    #[arg(short = 'p', long, value_name = "TEXT")]
    print: Option<String>,

    /// Resume a previous session by ID.
    #[arg(short = 'r', long, value_name = "SESSION_ID")]
    resume: Option<String>,

    /// Resume the most recent session (mutually exclusive with --resume).
    #[arg(short = 'c', long)]
    r#continue: bool,

    /// Model override (default: claude-sonnet-4-6).
    #[arg(long, value_name = "MODEL")]
    model: Option<String>,

    /// Disable TUI; write response to stdout (implied when --message is used).
    #[arg(long)]
    no_tui: bool,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    output: OutputFormat,

    /// Maximum tokens in the response.
    #[arg(long, default_value_t = 8192)]
    max_tokens: u32,

    /// Extended-thinking budget. Pass a positive integer (>= 1024 tokens,
    /// Anthropic API minimum) to enable with that budget, `adaptive` to let
    /// the server pick a budget per turn, or `off` / `disabled` / `0` to
    /// explicitly disable. Omit to leave the field off the wire (server
    /// default).
    #[arg(long, value_name = "BUDGET|adaptive|off")]
    thinking: Option<String>,

    /// Bypass all permission checks (accept everything).
    #[arg(long)]
    bypass_permissions: bool,

    /// Non-interactive mode: auto-deny permission prompts.
    #[arg(long)]
    non_interactive: bool,

    /// Add additional working directory paths to the session context.
    /// The model will be informed about these directories and can read files from them.
    #[arg(long, value_name = "PATH", num_args = 1..)]
    add_dir: Vec<String>,

    /// Enable verbose / debug logging.
    #[arg(short, long)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Authenticate with claude.ai via OAuth and save the token to
    /// ~/.claude/credentials.json. Used by dev builds to avoid Keychain prompts.
    Login,
    /// Diagnose credential / MCP / config health and print remediation
    /// hints. Safe to run any time — reads state only, never writes.
    Doctor,
    /// Print an upgrade hint. Self-update isn't wired yet on the Rust
    /// build; the doctor command will show the same reminder when a
    /// newer release has been announced.
    Update,
}

#[derive(Debug, Clone, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

/// Bundle of CLI inputs that have been validated post-clap. Populated by
/// `parse_cli_options` so every "can fail to validate user input" step lives
/// in one place and runs *before* any disk side effect (session file
/// creation, credential refresh, settings load). New validators go here.
#[derive(Debug)]
struct ParsedCliOptions {
    thinking: Option<ThinkingConfig>,
}

/// Run the side-effect-free post-clap validators. MUST NOT perform disk
/// I/O, network calls, or env-var reads beyond the already-parsed `Cli`
/// struct — the "validate before disk" guarantee in the
/// `cli-startup-order` spec depends on this staying pure.
fn parse_cli_options(cli: &Cli) -> Result<ParsedCliOptions, Box<dyn std::error::Error>> {
    let thinking = match &cli.thinking {
        Some(raw) => Some(parse_thinking(raw, cli.max_tokens)?),
        None => None,
    };
    Ok(ParsedCliOptions { thinking })
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::from_default_env()
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .init();

    if let Err(e) = run(cli).await {
        let msg = e.to_string();
        eprintln!("error: {msg}");
        for hint in error_hints(&msg) {
            eprintln!("hint: {hint}");
        }
        std::process::exit(1);
    }
}

/// Map free-form error messages to one or more remediation hints.
/// Rough pattern-match on common startup failures so new users aren't
/// greeted with an opaque `error: …` line. Each hint is a single line,
/// printed under a `hint:` prefix so downstream users can grep / split
/// by that prefix.
fn error_hints(msg: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    let lower = msg.to_ascii_lowercase();

    if lower.contains("credential")
        || lower.contains("oauth")
        || lower.contains("401")
        || lower.contains("unauthorized")
        || lower.contains("anthropic_api_key")
    {
        out.push("run `claude login` to refresh your Anthropic credentials.");
        out.push("or set the ANTHROPIC_API_KEY env var if you prefer API-key auth.");
    }
    if lower.contains("rate limit") || lower.contains("429") || lower.contains("resets") {
        out.push("retry in a few minutes; Anthropic rate limits reset on a rolling window.");
    }
    if lower.contains("network")
        || lower.contains("dns")
        || lower.contains("connection refused")
        || lower.contains("timed out")
    {
        out.push("check your internet connection, then run `claude doctor` to re-test.");
    }
    if lower.contains("mcp") && (lower.contains("expired") || lower.contains("404")) {
        out.push("MCP session dropped — the next call will reinitialize automatically.");
    }
    if lower.contains("tls_cert") || lower.contains("tls_key") {
        out.push("mTLS is half-configured; set BOTH TLS_CERT and TLS_KEY, or unset both.");
    }
    if lower.contains("settings.json") && (lower.contains("parse") || lower.contains("deserialize"))
    {
        out.push(
            "settings.json has a syntax error; `claude doctor` prints the file paths it loads.",
        );
    }
    if lower.contains("permission denied") && lower.contains("path") {
        out.push("check filesystem permissions on the path; `chmod +r` / `chown` as needed.");
    }

    if out.is_empty() {
        out.push("run `claude doctor` for a health summary, or `claude --help` for usage.");
    }
    out
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // Subcommand branches (don't require credentials to already exist).
    match &cli.command {
        Some(Commands::Login) => {
            let path = cc_auth::run_login_flow(cc_auth::OAuthConfig::default()).await?;
            println!("Credentials saved to {}", path.display());
            return Ok(());
        }
        Some(Commands::Doctor) => {
            run_doctor().await;
            return Ok(());
        }
        Some(Commands::Update) => {
            println!(
                "Self-update is not yet wired on the Rust build. To upgrade, pull the \n\
                 latest release from https://github.com/anthropics/claude-code and \n\
                 rebuild with `cargo install --path rust/cc`."
            );
            return Ok(());
        }
        None => {}
    }

    // Validation goes first so failures don't leak session files, print a
    // misleading `Session: <uuid>` line, or trigger a credential refresh.
    // See openspec/changes/fix-cli-validate-before-session-create.
    let parsed = parse_cli_options(&cli)?;

    let (credentials, key_source) = ensure_fresh_credentials().await?;
    tracing::debug!("using credentials from {key_source}");

    let settings = load_settings(None)?;
    let model = resolve_model(cli.model.as_deref(), &settings);

    // Decide mode: interactive TUI vs headless one-shot vs SDK --print.
    // --print is the SDK mode: highest precedence, never opens a TUI, always
    // non-interactive (auto-deny permission prompts).
    let print_text: Option<String> = cli.print.clone();

    // TUI mode is used when:
    //   - no --message AND no --print AND
    //   - --no-tui is NOT set AND
    //   - stdin is a tty (so we have a real terminal to talk to)
    let interactive_tui =
        cli.message.is_none() && cli.print.is_none() && !cli.no_tui && atty_is_stdin();

    // Resolve --continue → most recent session id.
    let resume_id: Option<String> = if let Some(id) = cli.resume.clone() {
        Some(id)
    } else if cli.r#continue {
        match list_sessions().into_iter().max() {
            Some(id) => Some(id),
            None => return Err("--continue: no saved sessions found".into()),
        }
    } else {
        None
    };

    // Build session (new or resumed)
    let (session, mut messages) = if let Some(session_id) = &resume_id {
        let (s, msgs) = Session::resume(session_id)?;
        eprintln!("Resumed session {} ({} messages)", session_id, msgs.len());
        (s, msgs)
    } else {
        let s = Session::new()?;
        tracing::debug!("new session: {}", s.id);
        (s, Vec::<MessageParam>::new())
    };

    // Build the real hook runner up-front (used by both SessionStart below
    // and every later hook-firing site). Building it once here removes a
    // throwaway pre-runner that used to live only to fire SessionStart.
    let hooks_raw = settings
        .extra
        .get("hooks")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let hooks_config: HooksSettings = if hooks_raw.is_null() {
        HooksSettings::new()
    } else {
        serde_json::from_value(hooks_raw).unwrap_or_default()
    };
    let http_config = cc_http::HttpClientConfig::from_env();
    let http = cc_http::build_client(&http_config).unwrap_or_default();
    // TS parity (hooks.ts:881-926): expose CLAUDE_PROJECT_DIR to every
    // hook child process. Plugin fields stay unset until the plugin loader
    // lands (Batch G), but the plumbing is already in place.
    let hook_context = HookContext {
        project_dir: std::env::current_dir().ok(),
        ..Default::default()
    };
    let hook_runner =
        Arc::new(HookRunner::new(&hooks_config, http.clone()).with_context(hook_context));

    // Fire SessionStart hook (§4 contract: trigger='resume' when resuming)
    {
        let mut session_start_input =
            cc_core::hook::HookInput::base(session.id.clone(), "SessionStart")
                .with_transcript_path(session.transcript_path().to_string_lossy())
                .with_model(model.clone());
        if resume_id.is_some() {
            session_start_input = session_start_input.with_message("resume");
        }
        let cancel_pre = tokio_util::sync::CancellationToken::new();
        let _ = hook_runner
            .run("SessionStart", &session_start_input, &cancel_pre)
            .await;
    }

    eprintln!("Session: {}", session.id);

    // Write session metadata
    let cwd = std::env::current_dir().unwrap_or_default();
    let metadata = SessionMetadata {
        model: model.clone(),
        started_at: chrono::Utc::now().to_rfc3339(),
        project_path: Some(cwd.to_string_lossy().to_string()),
        cwd: Some(cwd.to_string_lossy().to_string()),
    };
    if let Err(e) = session.write_metadata(&metadata) {
        tracing::warn!("failed to write session metadata: {e}");
    }

    // For headless mode we still need a user message; gather it before
    // building heavy components so we can fail fast on bad input.
    // --print supplies its own text and never reads stdin.
    let headless_user_text: Option<String> = if let Some(p) = &print_text {
        Some(p.clone())
    } else if !interactive_tui {
        Some(resolve_headless_user_text(
            resume_id.as_deref(),
            cli.message.as_deref(),
            atty_is_stdin(),
            || {
                let mut buf = String::new();
                io::stdin().read_line(&mut buf)?;
                Ok(buf.trim().to_string())
            },
        )?)
    } else {
        None
    };

    // Build system prompt blocks
    let system_blocks = build_system_blocks(&model, &cli.add_dir).await;

    // Build permission engine
    let perms = settings.permissions.as_ref();
    let allow_rules = perms
        .and_then(|p| p.allow.as_ref())
        .cloned()
        .unwrap_or_default();
    let deny_rules = perms
        .and_then(|p| p.deny.as_ref())
        .cloned()
        .unwrap_or_default();
    let permission_engine = PermissionEngine::from_settings(allow_rules, deny_rules);

    // Build tools (built-in + task management + MCP servers from settings.json `mcpServers`).
    let (mut tools, _todo_list, _todo_write_list, _task_registry, teammate_dir) = all_tools();
    if let Some(mcp_servers) = settings.extra.get("mcpServers") {
        let (mcp_tools, errors) = cc_mcp::load_mcp_tools_from_config(mcp_servers).await;
        for err in &errors {
            eprintln!("warning: {err}");
        }
        if !mcp_tools.is_empty() {
            tracing::debug!("loaded {} MCP tool(s)", mcp_tools.len());
        }
        tools.extend(mcp_tools);
    }

    // Build query options
    let non_interactive = cli.non_interactive || !atty_is_stdin();
    let options = QueryOptions {
        model: model.clone(),
        max_tokens: cli.max_tokens,
        non_interactive,
        bypass_permissions: cli.bypass_permissions,
        thinking: parsed.thinking.clone(),
    };

    // Build API client
    let auth = match credentials {
        Credentials::ApiKey(k) => AuthCredential::ApiKey(k),
        Credentials::OAuthToken(t) => AuthCredential::OAuthToken(t),
    };
    let api = ApiClient::new(http, auth);

    // Wire the WebFetch summarizer: replace the default (no-op prompt)
    // WebFetchTool with one backed by the ApiClient so `WebFetch { url,
    // prompt }` runs a single-turn summarization the same way the TS
    // version does.
    {
        let summarizer: Arc<dyn cc_core::Summarizer> =
            Arc::new(ApiSummarizer::new(api.clone(), model.clone()));
        cc_tools::attach_webfetch_summarizer(&mut tools, summarizer);
    }

    // Add AskUserQuestionTool — wired with a StdinPrompter for headless and
    // TUI modes (TUI will get full dialog support via a future AppEvent variant).
    {
        let ask_prompter: Arc<dyn cc_core::PermissionPrompter> =
            Arc::new(StdinPrompter::new(non_interactive));
        tools.push(Arc::new(AskUserQuestionTool {
            prompter: ask_prompter,
        }));
    }

    // Wire AgentTool: build a base registry (without AgentTool) to give to the
    // SubAgentRunner, then add AgentTool to the full tool list.
    // This two-pass approach breaks the cc-tools → cc-query → cc-tools cycle.
    {
        let sub_agent_registry = Arc::new(ToolRegistry::from(tools.clone()));
        let sub_agent_prompter: Arc<dyn cc_core::PermissionPrompter> =
            Arc::new(StdinPrompter::new(true));
        let sub_agent_runner = Arc::new(SubAgentRunnerImpl {
            api: api.clone(),
            tools: sub_agent_registry,
            permissions: permission_engine.clone(),
            hooks: hook_runner.clone(),
            system_blocks: system_blocks.clone(),
            options: options.clone(),
            prompter: sub_agent_prompter,
        });
        tools.push(Arc::new(AgentTool {
            runner: Some(sub_agent_runner.clone()),
        }));
        tools.push(Arc::new(TeamCreateTool {
            runner: Some(sub_agent_runner),
            directory: teammate_dir.clone(),
        }));

        // Wire ToolSearchTool: snapshot the current tool list as a lister closure.
        let snapshot: Vec<Arc<dyn cc_core::tool::Tool>> = tools.clone();
        let lister: cc_tools::tool_search::ToolLister = Arc::new(move || {
            snapshot
                .iter()
                .map(|t| cc_tools::tool_search::ToolEntry {
                    name: t.name().to_string(),
                    description: t.description().to_string(),
                    schema: serde_json::to_value(t.input_schema()).unwrap_or_default(),
                })
                .collect()
        });
        tools.push(Arc::new(cc_tools::tool_search::ToolSearchTool {
            list_tools: Some(lister),
        }));
    }

    // SDK / --print path — runs through cc-bridge.
    if let Some(_print) = &print_text {
        let user_text = headless_user_text
            .clone()
            .expect("--print supplies headless_user_text — checked above");

        let req = cc_bridge::BridgeRequest {
            api,
            tools,
            permissions: permission_engine,
            hooks: hook_runner,
            session,
            system_blocks,
            initial_messages: messages,
            user_text,
            model: model.clone(),
            max_tokens: cli.max_tokens,
            non_interactive: true,
            bypass_permissions: cli.bypass_permissions,
            thinking: parsed.thinking.clone(),
        };

        let stdout = io::stdout();
        let mut out = stdout.lock();
        let response = cc_bridge::run_once(req, |delta| {
            let _ = out.write_all(delta.as_bytes());
            let _ = out.flush();
        })
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        writeln!(out)?;
        tracing::debug!("--print done, session={}", response.session_id);
        return Ok(());
    }

    if interactive_tui {
        // TUI path: build a full QueryEngine wired to the TUI permission prompter.
        //
        // The events channel must be pre-created here so that ChannelPrompter
        // (passed to QueryEngine) and the engine itself share the same sender.
        let (events_tx, events_rx) = tokio::sync::mpsc::channel::<cc_core::AppEvent>(512);
        let tui_prompter: Arc<dyn cc_core::PermissionPrompter> =
            Arc::new(cc_tui::ChannelPrompter::new(events_tx.clone()));

        let tui_tool_registry: ToolRegistry = tools.into();
        let tui_engine = QueryEngine::new(QueryEngineConfig {
            api,
            tools: Arc::new(tui_tool_registry),
            permissions: permission_engine,
            hooks: hook_runner.clone(),
            session: Arc::new(session),
            system_blocks,
            options,
            prompter: tui_prompter,
        });

        // Discover skills and build the command registry.
        let config_dir = dirs::home_dir().unwrap_or_default().join(".claude");
        let commands = cc_tui::CommandRegistry::discover(&config_dir);
        let cmd_ctx = cc_tui::CommandContext {
            version: env!("CARGO_PKG_VERSION").to_string(),
            model: model.clone(),
            ..Default::default()
        };

        let tui_cancel = tokio_util::sync::CancellationToken::new();
        let tui_session_id = tui_engine.session().id.clone();

        // Phase D welcome banner + status bar metadata. We re-collect git
        // context here (cheap; same `git rev-parse --abbrev-ref HEAD` call
        // build_system_blocks already runs) so the TUI shows the same
        // branch the model sees. Outside a repo this is `None`.
        let cwd_path = std::env::current_dir().ok();
        let cwd_display = cwd_path.as_ref().map(|p| {
            if let Some(home) = dirs::home_dir() {
                if let Ok(rest) = p.strip_prefix(&home) {
                    if rest.as_os_str().is_empty() {
                        return "~".to_string();
                    }
                    return format!("~/{}", rest.display());
                }
            }
            p.display().to_string()
        });
        let git_branch = if let Some(p) = &cwd_path {
            cc_git::GitContext::collect(p).await.branch
        } else {
            None
        };

        let cfg = TuiConfig {
            model: model.clone(),
            session_id: tui_session_id.clone(),
            engine: Some(tui_engine),
            messages,
            cancel: tui_cancel,
            commands,
            command_ctx: cmd_ctx,
            events_tx,
            events_rx,
            version: env!("CARGO_PKG_VERSION").to_string(),
            cwd: cwd_display,
            git_branch,
        };
        cc_tui::run_tui(cfg).await?;
        // §5.2: Fire SessionEnd hook with tight 1.5s timeout when TUI exits.
        fire_session_end(&hook_runner, &tui_session_id).await;
        return Ok(());
    }

    // Headless one-shot path — same as M2 behavior.
    let user_text = headless_user_text.expect("headless mode without user text — checked above");

    let tool_registry: ToolRegistry = tools.into();

    let non_interactive = options.non_interactive;
    let prompter: Arc<dyn cc_core::PermissionPrompter> =
        Arc::new(StdinPrompter::new(non_interactive));
    let mut engine = QueryEngine::new(QueryEngineConfig {
        api,
        tools: Arc::new(tool_registry),
        permissions: permission_engine,
        hooks: hook_runner.clone(),
        session: Arc::new(session),
        system_blocks,
        options,
        prompter,
    });

    let cancel = tokio_util::sync::CancellationToken::new();

    let headless_session_id = engine.session().id.clone();
    match cli.output {
        OutputFormat::Text => {
            let stdout = io::stdout();
            let mut out = stdout.lock();
            let final_text = engine
                .run_turn(
                    user_text,
                    |delta| {
                        let _ = out.write_all(delta.as_bytes());
                        let _ = out.flush();
                    },
                    &mut messages,
                    &cancel,
                )
                .await?;
            writeln!(out)?;
            tracing::debug!("session: {}", engine.session().id);
            let _ = final_text;
        }
        OutputFormat::Json => {
            // JSONL streaming output (P1 #38 of the 2026-04-24 parity-
            // gaps roadmap). Each line is a self-contained JSON object
            // typed via the `"type"` field. Order: a session_start
            // line, then live tool_use / tool_result / assistant_text
            // / permission_request / hook event lines as the engine
            // emits them, then a final turn_summary line. SDK
            // consumers can parse line-by-line and react to events
            // mid-turn instead of waiting for the whole reply.
            let session_id = engine.session().id.clone();
            let model = engine.model().to_string();
            // Initial session_start record.
            let header = json!({
                "type": "session_start",
                "session_id": session_id,
                "model": model,
            });
            println!("{header}");

            // Wire the event channel into the engine so it forwards
            // streaming deltas + tool calls + permission requests +
            // turn-complete to us. The text callback below also
            // captures final assistant text for the summary line.
            let (ev_tx, mut ev_rx) = tokio::sync::mpsc::channel::<cc_core::AppEvent>(64);
            let printer = tokio::spawn(async move {
                while let Some(ev) = ev_rx.recv().await {
                    if let Some(line) = event_to_jsonl(&ev) {
                        println!("{line}");
                    }
                }
            });
            engine = engine.with_events(ev_tx);

            let mut full_text = String::new();
            let final_text = engine
                .run_turn(
                    user_text,
                    |delta| {
                        full_text.push_str(delta);
                    },
                    &mut messages,
                    &cancel,
                )
                .await?;
            // Dropping the engine closes its tx clone of the event
            // channel; the printer drains and exits. Make sure the
            // summary line lands AFTER every streamed event.
            drop(engine);
            printer.await.ok();
            let summary = json!({
                "type": "turn_summary",
                "session_id": session_id,
                "model": model,
                "final_text": final_text,
            });
            println!("{summary}");
        }
    }

    // §5.2: Fire SessionEnd hook with tight 1.5s timeout.
    fire_session_end(&hook_runner, &headless_session_id).await;

    Ok(())
}

/// Fire SessionEnd hooks with the tight 1.5-second timeout required by the
/// behavior contract (§5.2).  Uses `CLAUDE_CODE_SESSIONEND_HOOKS_TIMEOUT_MS`
/// for override.
async fn fire_session_end(hook_runner: &HookRunner, session_id: &str) {
    let input = cc_core::hook::HookInput::base(session_id, "SessionEnd");
    hook_runner.run_session_end(&input).await;
}

/// Serialise an `AppEvent` for `--output json` streaming. Returns `None`
/// for events that are internal lifecycle signals (StreamToolUse — the
/// engine emits a richer `ToolStart` later; TaskUpdate — agent-task
/// queue noise) so SDK consumers don't have to filter them out.
///
/// Each kept variant becomes a self-contained JSON object with a
/// discriminating `"type"` field. Field names mirror TS
/// `src/cli/print.ts` so JSONL parsers built for the TS CLI keep
/// working against the Rust binary.
fn event_to_jsonl(ev: &cc_core::AppEvent) -> Option<String> {
    use cc_core::AppEvent::*;
    let value = match ev {
        StreamDelta(text) => json!({
            "type": "assistant_text_delta",
            "text": text,
        }),
        StreamThinking(text) => json!({
            "type": "thinking_delta",
            "text": text,
        }),
        StreamEnd(reason) => json!({
            "type": "stream_end",
            "stop_reason": format!("{reason:?}"),
        }),
        ToolStart { name, input } => json!({
            "type": "tool_use",
            "name": name,
            "input": input,
        }),
        ToolEnd { name, result } => json!({
            "type": "tool_result",
            "name": name,
            "is_error": result.is_error,
            "content": result.content,
        }),
        PermissionRequest {
            id,
            tool_name,
            tool_input,
            ..
        } => json!({
            "type": "permission_request",
            "id": id,
            "tool_name": tool_name,
            "tool_input": tool_input,
        }),
        CompactBoundary => json!({"type": "compact_boundary"}),
        TurnComplete { usage } => json!({
            "type": "turn_complete",
            "usage": {
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "cache_creation_input_tokens": usage.cache_creation_input_tokens,
                "cache_read_input_tokens": usage.cache_read_input_tokens,
            },
        }),
        Error(message) => json!({"type": "error", "message": message}),
        // StreamToolUse is superseded by ToolStart; TaskUpdate is
        // internal to the agent runtime and not part of the public
        // JSONL contract.
        StreamToolUse(_) | TaskUpdate(_) => return None,
    };
    Some(value.to_string())
}

async fn build_system_blocks(model: &str, add_dirs: &[String]) -> Vec<SystemBlock> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let git_text = cc_git::GitContext::collect(&cwd).await.to_system_text();
    let memory_text = cc_memory::memories_to_system_text(&cc_memory::load_memories());
    build_system_blocks_inner(model, git_text, memory_text, add_dirs)
}

/// Three-tier system-prompt tagging per `RUST_REWRITE_PLAN.md` §3:
///
/// - attribution (tier 1 / uncached): no `cache_control`
/// - static instruction (tier 2 / global cache): `ephemeral_global`
/// - git / memory / dynamic (tier 3 / org cache): `ephemeral_org`
///
/// Block ordering is attribution → static → dynamic so the server caches
/// them in the right tier. Factored out from the `async` wrapper so the
/// regression test in `tests` below can pin the tagging without having to
/// stub `git`/`memory` I/O.
fn build_system_blocks_inner(
    _model: &str,
    git_text: Option<String>,
    memory_text: Option<String>,
    add_dirs: &[String],
) -> Vec<SystemBlock> {
    let mut blocks = Vec::new();

    // Tier 1: attribution — uncached (matches TS client).
    //
    // ⚠️ Load-bearing magic string. Anthropic's backend routes OAuth
    // requests to the Claude Code subscription quota pool ONLY when the
    // first system block's text begins with this exact phrase. Any other
    // wording — including superficial variants like "You are Claude
    // Code." or "an AI assistant for software engineering tasks" — falls
    // through to a much tighter "naked OAuth inference" pool and returns
    // HTTP 429 on even the first request against Sonnet / Opus.
    //
    // Empirically verified 2026-04-18 by probing /v1/messages with a
    // matrix of phrasings; only the literal string below unlocked the
    // quota. If you change this, expect the CLI to start returning 429.
    // Do NOT append the model name here — the official CC does not, and
    // the match appears to be prefix-sensitive.
    blocks.push(SystemBlock::text(
        "You are Claude Code, Anthropic's official CLI for Claude.",
    ));

    // Tier 2: static instruction — global cache.
    blocks.push(SystemBlock::text_global_cached(
        "You have access to tools for reading/writing files, running shell commands, \
         searching code, and more.",
    ));

    // Tier 3: dynamic blocks — org cache.
    if let Some(git_text) = git_text {
        blocks.push(SystemBlock::text_org_cached(git_text));
    }

    if let Some(mem_text) = memory_text {
        blocks.push(SystemBlock::text_org_cached(mem_text));
    }

    // Additional working directories — per-invocation CLI flag, but still
    // stable for the life of this session so also org-tier.
    if !add_dirs.is_empty() {
        let dirs_text = add_dirs
            .iter()
            .map(|d| format!("  - {d}"))
            .collect::<Vec<_>>()
            .join("\n");
        blocks.push(SystemBlock::text_org_cached(format!(
            "Additional working directories available:\n{dirs_text}"
        )));
    }

    blocks
}

fn atty_is_stdin() -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let fd = io::stdin().as_raw_fd();
        libc_isatty(fd)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(unix)]
fn libc_isatty(fd: i32) -> bool {
    extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    // SAFETY: isatty(3) is a POSIX function taking a valid fd, returning 0 or 1.
    unsafe { isatty(fd) != 0 }
}

/// Parse the `--thinking` CLI argument into a `ThinkingConfig`.
///
/// Accepted forms:
///   - `adaptive` → `ThinkingConfig::Adaptive` (server picks budget per turn)
///   - `off` | `disabled` | `0` → `ThinkingConfig::Disabled`
///   - positive integer >= 1024 → `Enabled { budget_tokens }`,
///     clamped to `max_tokens - 1` when the budget would meet or exceed
///     `max_tokens` (matches TS `sideQuery.ts:172-177`). Emits a
///     `tracing::warn!` when clamping fires.
///   - anything else → `Err(message)` with a readable hint.
fn parse_thinking(raw: &str, max_tokens: u32) -> Result<ThinkingConfig, String> {
    let trimmed = raw.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "adaptive" => return Ok(ThinkingConfig::Adaptive),
        "off" | "disabled" => return Ok(ThinkingConfig::Disabled),
        _ => {}
    }
    let budget: u32 = trimmed.parse().map_err(|_| {
        format!("--thinking: expected integer, 'adaptive', or 'off', got {trimmed:?}")
    })?;
    if budget == 0 {
        return Ok(ThinkingConfig::Disabled);
    }
    if budget < 1024 {
        return Err(format!(
            "--thinking: budget_tokens must be >= 1024 (Anthropic API minimum), got {budget}"
        ));
    }
    let clamped = if budget >= max_tokens {
        let c = max_tokens.saturating_sub(1).max(1024);
        tracing::warn!("--thinking={budget} >= --max-tokens={max_tokens}; clamping budget to {c}");
        c
    } else {
        budget
    };
    Ok(ThinkingConfig::Enabled {
        budget_tokens: clamped,
    })
}

/// Headless mode: decide what the next user-turn text should be.
///
/// Four cases, each exercised by a unit test:
///   1. `--message X` → return X (regardless of resume or stdin).
///   2. No `--message`, stdin is a pipe → read one line from stdin
///      (works whether or not `--resume` is set — fixes roadmap P0 #19
///      which used to error out on resume+piped-stdin).
///   3. No `--message`, stdin is a tty, no `--resume` → error with a
///      hint to use `--message` or pipe stdin.
///   4. No `--message`, stdin is a tty, `--resume` is set → error with
///      a resume-specific hint (tell the user to pass `--message` or
///      run TUI).
///
/// The resumed message history is merged *independently* of this
/// decision — it flows through `Session::resume` → the `messages`
/// vector → `engine.run_turn(..., &mut messages, ...)` — so once this
/// function returns `Ok(user_text)`, the engine sees both the resumed
/// transcript (`initial_messages`) and the new turn text in one request.
fn resolve_headless_user_text<F>(
    resume_id: Option<&str>,
    cli_message: Option<&str>,
    is_tty: bool,
    read_stdin_line: F,
) -> Result<String, Box<dyn std::error::Error>>
where
    F: FnOnce() -> io::Result<String>,
{
    if let Some(m) = cli_message {
        return Ok(m.to_string());
    }
    if is_tty {
        // No piped stdin to fall back on. Distinct messages so operators
        // can tell "I forgot --message" from "--resume needs a turn".
        if resume_id.is_some() {
            return Err(
                "--resume/--continue in headless mode needs a next-turn message — \
                pass --message, pipe text via stdin, or run without --no-tui"
                    .into(),
            );
        }
        return Err("no message provided — use --message or pipe text via stdin".into());
    }
    // stdin is piped: read one line. Works for both new and resumed
    // sessions — the P0 #19 fix.
    Ok(read_stdin_line()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cc_scope(block: &SystemBlock) -> Option<&str> {
        block
            .cache_control
            .as_ref()
            .and_then(|c| c.scope.as_deref())
    }

    fn cc_kind(block: &SystemBlock) -> Option<&str> {
        block.cache_control.as_ref().map(|c| c.kind.as_str())
    }

    #[test]
    fn three_tier_tagging_attribution_static_dynamic() {
        let blocks = build_system_blocks_inner(
            "claude-sonnet-4-6",
            Some("## Git context\nbranch=main".into()),
            Some("## Memory\n- remember foo".into()),
            &[],
        );

        assert_eq!(
            blocks.len(),
            4,
            "expected 4 blocks (attr + static + git + memory)"
        );

        // Block 0: attribution, no cache_control.
        assert!(
            blocks[0].cache_control.is_none(),
            "attribution block must be uncached"
        );
        assert!(blocks[0].text.contains("Claude Code"));

        // Block 1: static instruction → global cache.
        assert_eq!(cc_kind(&blocks[1]), Some("ephemeral"));
        assert_eq!(cc_scope(&blocks[1]), Some("global"));

        // Block 2: git (dynamic) → org cache.
        assert_eq!(cc_kind(&blocks[2]), Some("ephemeral"));
        assert_eq!(cc_scope(&blocks[2]), Some("org"));

        // Block 3: memory (dynamic) → org cache.
        assert_eq!(cc_kind(&blocks[3]), Some("ephemeral"));
        assert_eq!(cc_scope(&blocks[3]), Some("org"));
    }

    #[test]
    fn three_tier_tagging_serialized_wire_shape() {
        // The Anthropic API currently rejects a populated
        // `cache_control.scope` field (HTTP 400: "Extra inputs are not
        // permitted"). We therefore emit ONLY `{"type":"ephemeral"}` on
        // the wire even when the in-memory tagging says global / org —
        // the three-tier intent is preserved in the block ordering and
        // the Rust-side `CacheControl::scope` field, so we can flip the
        // scope back on at the serializer the moment the API accepts it
        // without having to re-do the tagging sites.
        let blocks = build_system_blocks_inner("claude-sonnet-4-6", None, Some("mem".into()), &[]);
        let wire = serde_json::to_value(&blocks).expect("serialize");

        // Attribution block: no cache_control key at all (skip_serializing_if).
        let attr = &wire[0];
        assert_eq!(attr["type"], "text");
        assert!(attr.get("cache_control").is_none());

        // Static + dynamic blocks: cache_control present but scope omitted.
        let static_blk = &wire[1];
        assert_eq!(
            static_blk["cache_control"],
            json!({"type": "ephemeral"}),
            "wire shape must not include scope (API rejects it)"
        );

        let mem_blk = &wire[2];
        assert_eq!(
            mem_blk["cache_control"],
            json!({"type": "ephemeral"}),
            "wire shape must not include scope (API rejects it)"
        );
    }

    #[test]
    fn three_tier_tagging_handles_absent_dynamic_blocks() {
        let blocks = build_system_blocks_inner("claude-sonnet-4-6", None, None, &[]);
        assert_eq!(blocks.len(), 2, "attribution + static only");
        assert!(blocks[0].cache_control.is_none());
        assert_eq!(cc_scope(&blocks[1]), Some("global"));
    }

    // ---------------------------------------------------------------
    // fix-session-resume-integrity §4: resume → headless merge.
    // ---------------------------------------------------------------

    #[test]
    fn resume_with_message_flag_uses_it_directly() {
        let got = resolve_headless_user_text(Some("sess"), Some("hello"), true, || {
            panic!("stdin must not be read when --message is set")
        })
        .expect("ok");
        assert_eq!(got, "hello");
    }

    /// The P0 #19 regression guard: `echo hi | claude --resume X` used to
    /// error out with "--resume/--continue without TUI requires --message".
    /// It must now read stdin, same as a fresh session.
    #[test]
    fn resume_without_message_reads_stdin_when_piped() {
        let got = resolve_headless_user_text(Some("sess"), None, false, || Ok("next turn".into()))
            .expect("piped stdin should supply the turn text");
        assert_eq!(got, "next turn");
    }

    #[test]
    fn new_session_with_message_flag_works() {
        let got =
            resolve_headless_user_text(None, Some("hi"), true, || panic!("stdin unused")).unwrap();
        assert_eq!(got, "hi");
    }

    #[test]
    fn new_session_tty_no_message_errors() {
        let err = resolve_headless_user_text(None, None, true, || {
            panic!("stdin must not be read on a tty")
        })
        .expect_err("no message on a tty must error");
        let msg = err.to_string();
        assert!(
            msg.contains("--message"),
            "error should mention --message hint, got: {msg}"
        );
    }

    #[test]
    fn resume_tty_no_message_errors_with_resume_hint() {
        let err = resolve_headless_user_text(Some("sess"), None, true, || {
            panic!("stdin must not be read on a tty")
        })
        .expect_err("resume on a tty without --message must error");
        let msg = err.to_string();
        assert!(
            msg.contains("--resume") || msg.contains("--continue"),
            "error should mention resume-specific context, got: {msg}"
        );
    }

    #[test]
    fn parse_cli_options_rejects_invalid_thinking() {
        // Pure check: parse_cli_options is side-effect-free, so an invalid
        // `--thinking` value MUST surface as Err before run() can reach
        // Session::new / ensure_fresh_credentials / settings load.
        use clap::Parser;
        let cli = Cli::try_parse_from(["claude", "--thinking", "abc"]).expect("clap parses");
        let err = parse_cli_options(&cli).expect_err("invalid thinking must error");
        let msg = err.to_string();
        assert!(
            msg.contains("--thinking"),
            "error should mention --thinking, got: {msg}"
        );
    }

    #[test]
    fn parse_cli_options_accepts_valid_inputs() {
        use clap::Parser;

        // (a) --thinking adaptive → Some(Adaptive)
        let cli = Cli::try_parse_from(["claude", "--thinking", "adaptive"]).unwrap();
        let parsed = parse_cli_options(&cli).expect("adaptive is valid");
        assert!(matches!(parsed.thinking, Some(ThinkingConfig::Adaptive)));

        // (b) no --thinking → None
        let cli = Cli::try_parse_from(["claude"]).unwrap();
        let parsed = parse_cli_options(&cli).expect("no thinking is valid");
        assert!(parsed.thinking.is_none());

        // (c) --thinking 2048 → Some(Enabled { 2048 })
        let cli = Cli::try_parse_from(["claude", "--thinking", "2048"]).unwrap();
        let parsed = parse_cli_options(&cli).expect("2048 is valid");
        assert!(matches!(
            parsed.thinking,
            Some(ThinkingConfig::Enabled {
                budget_tokens: 2048
            })
        ));
    }

    #[test]
    fn print_short_flag_p_equivalent_to_long() {
        use clap::Parser;
        let long = Cli::parse_from(["claude", "--print", "hello"]);
        let short = Cli::parse_from(["claude", "-p", "hello"]);
        assert_eq!(long.print, Some("hello".into()));
        assert_eq!(short.print, Some("hello".into()));
        assert_eq!(long.print, short.print);
    }

    #[test]
    fn continue_short_flag_c_equivalent_to_long() {
        use clap::Parser;
        let long = Cli::parse_from(["claude", "--continue"]);
        let short = Cli::parse_from(["claude", "-c"]);
        assert!(long.r#continue);
        assert!(short.r#continue);
    }

    #[test]
    fn resume_short_flag_r_equivalent_to_long() {
        use clap::Parser;
        let long = Cli::parse_from(["claude", "--resume", "sess-abc"]);
        let short = Cli::parse_from(["claude", "-r", "sess-abc"]);
        assert_eq!(long.resume.as_deref(), Some("sess-abc"));
        assert_eq!(short.resume.as_deref(), Some("sess-abc"));
        assert_eq!(long.resume, short.resume);
    }

    #[test]
    fn parse_thinking_flag_variants() {
        assert!(matches!(
            parse_thinking("adaptive", 8192).unwrap(),
            ThinkingConfig::Adaptive
        ));
        assert!(matches!(
            parse_thinking("ADAPTIVE", 8192).unwrap(),
            ThinkingConfig::Adaptive
        ));
        assert!(matches!(
            parse_thinking("off", 8192).unwrap(),
            ThinkingConfig::Disabled
        ));
        assert!(matches!(
            parse_thinking("disabled", 8192).unwrap(),
            ThinkingConfig::Disabled
        ));
        assert!(matches!(
            parse_thinking("0", 8192).unwrap(),
            ThinkingConfig::Disabled
        ));
        assert!(matches!(
            parse_thinking("2048", 8192).unwrap(),
            ThinkingConfig::Enabled {
                budget_tokens: 2048
            }
        ));
        // Sub-minimum: explicit error with hint.
        let err = parse_thinking("512", 8192).unwrap_err();
        assert!(err.contains("1024"), "error should mention minimum: {err}");
        // Unparseable: readable error.
        let err = parse_thinking("abc", 8192).unwrap_err();
        assert!(
            err.contains("integer"),
            "error should mention expected forms: {err}"
        );
        // Clamp when budget >= max_tokens.
        let got = parse_thinking("10000", 8192).unwrap();
        if let ThinkingConfig::Enabled { budget_tokens } = got {
            assert!(
                (1024..8192).contains(&budget_tokens),
                "clamp must stay within [1024, max_tokens), got {budget_tokens}"
            );
        } else {
            panic!("expected Enabled after clamp");
        }
    }

    #[test]
    fn error_hints_cover_common_startup_cases() {
        // Auth / OAuth / 401 → two hints about login + env var.
        let hints = error_hints("oauth token has expired (401 Unauthorized)");
        assert!(
            hints.iter().any(|h| h.contains("claude login")),
            "auth error missing login hint: {hints:?}"
        );
        assert!(
            hints.iter().any(|h| h.contains("ANTHROPIC_API_KEY")),
            "auth error missing api-key hint: {hints:?}"
        );

        // Rate limit.
        let hints = error_hints("HTTP 429: rate limit exceeded");
        assert!(hints.iter().any(|h| h.contains("rate limit")), "{hints:?}");

        // Network fault.
        let hints = error_hints("DNS resolution failed: connection refused");
        assert!(hints.iter().any(|h| h.contains("doctor")), "{hints:?}");

        // mTLS misconfig.
        let hints = error_hints("TLS_CERT set but TLS_KEY is missing");
        assert!(hints.iter().any(|h| h.contains("mTLS")), "{hints:?}");

        // Settings.json parse error.
        let hints = error_hints("failed to parse settings.json: unexpected token");
        assert!(
            hints.iter().any(|h| h.contains("settings.json")),
            "{hints:?}"
        );

        // Unknown error → generic fallback.
        let hints = error_hints("something strange happened 42");
        assert_eq!(hints.len(), 1);
        assert!(hints[0].contains("doctor"));
    }

    /// P1 #38: every kept AppEvent variant serialises to a single
    /// self-contained JSONL line with a `"type"` discriminator.
    /// StreamToolUse / TaskUpdate intentionally drop (they're
    /// internal noise that the public JSONL contract excludes).
    #[test]
    fn event_to_jsonl_covers_public_variants() {
        use cc_core::message::Usage;
        use cc_core::tool::ToolResult;
        use cc_core::AppEvent;

        let line = event_to_jsonl(&AppEvent::StreamDelta("hi".into())).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["type"], "assistant_text_delta");
        assert_eq!(parsed["text"], "hi");

        let line = event_to_jsonl(&AppEvent::ToolStart {
            name: "Bash".into(),
            input: json!({"command": "ls"}),
        })
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["type"], "tool_use");
        assert_eq!(parsed["name"], "Bash");
        assert_eq!(parsed["input"]["command"], "ls");

        let line = event_to_jsonl(&AppEvent::ToolEnd {
            name: "Bash".into(),
            result: ToolResult::error("exit 1"),
        })
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["type"], "tool_result");
        assert_eq!(parsed["is_error"], true);
        assert_eq!(parsed["content"], "exit 1");

        let line = event_to_jsonl(&AppEvent::CompactBoundary).unwrap();
        assert!(line.contains("\"compact_boundary\""));

        let line = event_to_jsonl(&AppEvent::TurnComplete {
            usage: Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        })
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["type"], "turn_complete");
        assert_eq!(parsed["usage"]["input_tokens"], 100);
        assert_eq!(parsed["usage"]["output_tokens"], 50);

        // Internal events drop.
        let dropped = event_to_jsonl(&AppEvent::TaskUpdate(cc_core::task::TaskNotification {
            task_id: cc_core::task::TaskId::new(),
            status: cc_core::task::TaskStatus::Running,
            summary: None,
        }));
        assert!(dropped.is_none(), "TaskUpdate must not leak to JSONL");
    }
}

/// `claude doctor` implementation. Prints a health summary covering
/// credential state, settings load, and any configured MCP servers.
/// Safe to run any time — never writes state.
#[allow(clippy::items_after_test_module)]
async fn run_doctor() {
    println!("claude doctor — environment health check\n");

    // Credentials
    match cc_auth::resolve_credentials() {
        Ok((_creds, source)) => {
            println!("✓ credentials: loaded from {source:?}");
        }
        Err(e) => {
            println!("✗ credentials: {e}");
            println!("    → run `claude login` or set ANTHROPIC_API_KEY to authenticate.");
        }
    }

    // Settings
    let cwd = std::env::current_dir().ok();
    match load_settings(cwd.as_deref()) {
        Ok(_) => println!("✓ settings: layered load succeeded"),
        Err(e) => {
            println!("✗ settings: {e}");
            println!(
                "    → check JSON syntax; `jq . ~/.claude/settings.json` prints parse errors."
            );
        }
    }

    // MCP env (mTLS)
    let has_cert = std::env::var_os("TLS_CERT").is_some();
    let has_key = std::env::var_os("TLS_KEY").is_some();
    match (has_cert, has_key) {
        (false, false) => println!("  mTLS: off (TLS_CERT / TLS_KEY unset)"),
        (true, true) => println!("✓ mTLS: both TLS_CERT and TLS_KEY set"),
        _ => {
            println!("✗ mTLS: exactly one of TLS_CERT / TLS_KEY is set; this will fail at startup")
        }
    }

    // Version line
    println!(
        "\nclaude version: {} ({} build)",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
}
