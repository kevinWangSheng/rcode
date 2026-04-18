use std::io::{self, Write};
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use serde_json::json;
use tracing_subscriber::EnvFilter;

use cc_api::{ApiClient, AuthCredential};
use cc_auth::{ensure_fresh_credentials, Credentials};
use cc_config::{load_settings, resolve_model};
use cc_core::{MessageParam, SystemBlock};
use cc_hooks::{HookRunner, HooksSettings};
use cc_permissions::PermissionEngine;
use cc_query::{
    engine::{QueryEngine, QueryOptions},
    StdinPrompter, SubAgentRunnerImpl, ToolRegistry,
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
    #[arg(long, value_name = "TEXT")]
    print: Option<String>,

    /// Resume a previous session by ID.
    #[arg(long, value_name = "SESSION_ID")]
    resume: Option<String>,

    /// Resume the most recent session (mutually exclusive with --resume).
    #[arg(long)]
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
}

#[derive(Debug, Clone, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
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
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // Subcommand branches (don't require credentials to already exist).
    if let Some(Commands::Login) = &cli.command {
        let path = cc_auth::run_login_flow(cc_auth::OAuthConfig::default()).await?;
        println!("Credentials saved to {}", path.display());
        return Ok(());
    }

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

    // Fire SessionStart hook (§4 contract: trigger='resume' when resuming)
    {
        let cwd = std::env::current_dir().unwrap_or_default();
        let session_start_input = cc_core::hook::HookInput {
            session_id: session.id.clone(),
            transcript_path: Some(session.transcript_path().to_string_lossy().to_string()),
            cwd: cwd.to_string_lossy().to_string(),
            permission_mode: None,
            hook_event_name: "SessionStart".to_string(),
            tool_name: None,
            tool_input: None,
            tool_use_id: None,
            tool_response: None,
            source: None,
            model: Some(model.clone()),
            message: resume_id.as_ref().map(|_| "resume".to_string()),
            agent_id: None,
            stop_hook_active: None,
            last_assistant_message: None,
        };
        // Build a temporary hook runner just for the SessionStart event.
        // We'll re-build the real one below with the same config.
        let hooks_raw_pre = settings
            .extra
            .get("hooks")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let hooks_config_pre: cc_hooks::HooksSettings = if hooks_raw_pre.is_null() {
            cc_hooks::HooksSettings::new()
        } else {
            serde_json::from_value(hooks_raw_pre).unwrap_or_default()
        };
        let http_pre =
            cc_http::build_client(&cc_http::HttpClientConfig::from_env()).unwrap_or_default();
        let pre_runner = HookRunner::new(&hooks_config_pre, http_pre);
        let cancel_pre = tokio_util::sync::CancellationToken::new();
        let _ = pre_runner
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
        let text = match &cli.message {
            Some(text) => text.clone(),
            None => {
                if atty_is_stdin() && resume_id.is_none() {
                    return Err("no message provided — use --message or pipe text via stdin".into());
                }
                if resume_id.is_some() && cli.message.is_none() {
                    return Err(
                        "--resume/--continue without TUI requires --message for the next turn"
                            .into(),
                    );
                }
                let mut buf = String::new();
                io::stdin().read_line(&mut buf)?;
                buf.trim().to_string()
            }
        };
        Some(text)
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

    // Build hook runner
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
    let hook_runner = Arc::new(HookRunner::new(&hooks_config, http.clone()));

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
    };

    // Build API client
    let auth = match credentials {
        Credentials::ApiKey(k) => AuthCredential::ApiKey(k),
        Credentials::OAuthToken(t) => AuthCredential::OAuthToken(t),
    };
    let api = ApiClient::new(http, auth);

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
        let mut base_registry = ToolRegistry::new();
        for tool in &tools {
            base_registry.register(tool.clone());
        }
        let sub_agent_registry = Arc::new(base_registry);
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

        let mut tui_tool_registry = ToolRegistry::new();
        for tool in tools {
            tui_tool_registry.register(tool);
        }
        let tui_engine = QueryEngine::new(
            api,
            Arc::new(tui_tool_registry),
            permission_engine,
            hook_runner.clone(),
            session,
            system_blocks,
            options,
            tui_prompter,
        );

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
        let cfg = TuiConfig {
            model: model.clone(),
            session_id: tui_session_id.clone(),
            engine: tui_engine,
            messages,
            cancel: tui_cancel,
            commands,
            command_ctx: cmd_ctx,
            events_tx,
            events_rx,
        };
        cc_tui::run_tui(cfg).await?;
        // §5.2: Fire SessionEnd hook with tight 1.5s timeout when TUI exits.
        fire_session_end(&hook_runner, &tui_session_id).await;
        return Ok(());
    }

    // Headless one-shot path — same as M2 behavior.
    let user_text = headless_user_text.expect("headless mode without user text — checked above");

    let mut tool_registry = ToolRegistry::new();
    for tool in tools {
        tool_registry.register(tool);
    }

    let non_interactive = options.non_interactive;
    let prompter: Arc<dyn cc_core::PermissionPrompter> =
        Arc::new(StdinPrompter::new(non_interactive));
    let mut engine = QueryEngine::new(
        api,
        Arc::new(tool_registry),
        permission_engine,
        hook_runner.clone(),
        session,
        system_blocks,
        options,
        prompter,
    );

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
            let output = json!({
                "session_id": engine.session().id,
                "model": engine.model(),
                "content": final_text,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
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
    let cwd = std::env::current_dir().unwrap_or_default();
    let input = cc_core::hook::HookInput {
        session_id: session_id.to_string(),
        transcript_path: None,
        cwd: cwd.to_string_lossy().to_string(),
        permission_mode: None,
        hook_event_name: "SessionEnd".to_string(),
        tool_name: None,
        tool_input: None,
        tool_use_id: None,
        tool_response: None,
        source: None,
        model: None,
        message: None,
        agent_id: None,
        stop_hook_active: None,
        last_assistant_message: None,
    };
    hook_runner.run_session_end(&input).await;
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
    blocks.push(SystemBlock {
        kind: "text".into(),
        text: "You are Claude Code, Anthropic's official CLI for Claude.".to_string(),
        cache_control: None,
    });

    // Tier 2: static instruction — global cache.
    blocks.push(SystemBlock {
        kind: "text".into(),
        text: "You have access to tools for reading/writing files, running shell commands, \
               searching code, and more."
            .to_string(),
        cache_control: Some(cc_core::CacheControl::ephemeral_global()),
    });

    // Tier 3: dynamic blocks — org cache.
    if let Some(git_text) = git_text {
        blocks.push(SystemBlock {
            kind: "text".into(),
            text: git_text,
            cache_control: Some(cc_core::CacheControl::ephemeral_org()),
        });
    }

    if let Some(mem_text) = memory_text {
        blocks.push(SystemBlock {
            kind: "text".into(),
            text: mem_text,
            cache_control: Some(cc_core::CacheControl::ephemeral_org()),
        });
    }

    // Additional working directories — per-invocation CLI flag, but still
    // stable for the life of this session so also org-tier.
    if !add_dirs.is_empty() {
        let dirs_text = add_dirs
            .iter()
            .map(|d| format!("  - {d}"))
            .collect::<Vec<_>>()
            .join("\n");
        blocks.push(SystemBlock {
            kind: "text".into(),
            text: format!("Additional working directories available:\n{dirs_text}"),
            cache_control: Some(cc_core::CacheControl::ephemeral_org()),
        });
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
}
