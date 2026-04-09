use std::io::{self, Write};

use clap::{Parser, ValueEnum};
use serde_json::json;
use tracing_subscriber::EnvFilter;

use cc_api::ApiClient;
use cc_auth::{resolve_credentials, Credentials};
use cc_config::{load_settings, resolve_model};
use cc_core::{MessageParam, SystemBlock};
use cc_hooks::{HookRunner, HooksConfig};
use cc_permissions::PermissionEngine;
use cc_query::{
    engine::{QueryEngine, QueryOptions},
};
use cc_session::{list_sessions, Session};
use cc_tools::default_tools;
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

    /// Enable verbose / debug logging.
    #[arg(short, long)]
    verbose: bool,
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
    let (credentials, key_source) = resolve_credentials()?;
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

    eprintln!("Session: {}", session.id);

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
                    return Err(
                        "no message provided — use --message or pipe text via stdin".into(),
                    );
                }
                if resume_id.is_some() && cli.message.is_none() {
                    return Err(
                        "--resume/--continue without TUI requires --message for the next turn".into(),
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
    let system_blocks = build_system_blocks(&model).await;

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
    let hooks_config: HooksConfig = if hooks_raw.is_null() {
        HooksConfig::new()
    } else {
        serde_json::from_value(hooks_raw).unwrap_or_default()
    };
    let hook_runner = HookRunner::new(hooks_config);

    // Build tools (built-in + MCP servers from settings.json `mcpServers`).
    let mut tools = default_tools();
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
    let api = match credentials {
        Credentials::ApiKey(k) => ApiClient::with_api_key(k)?,
        Credentials::OAuthToken(t) => ApiClient::with_oauth_token(t)?,
    };

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
        // Extract MCP server names from settings so the TUI can render /mcp
        // and /config without having to re-parse the config itself.
        let mcp_server_names: Vec<String> = settings
            .extra
            .get("mcpServers")
            .and_then(|v| v.as_object())
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();

        // TUI path — hand everything to cc-tui and let it own the run loop.
        let cfg = TuiConfig {
            api,
            tools,
            permissions: permission_engine,
            hooks: hook_runner,
            session,
            initial_messages: messages,
            system_blocks,
            options,
            version: env!("CARGO_PKG_VERSION").to_string(),
            mcp_server_names,
            project_root: std::env::current_dir().ok(),
        };
        cc_tui::run_tui(cfg).await?;
        return Ok(());
    }

    // Headless one-shot path — same as M2 behavior.
    let user_text = headless_user_text.expect("headless mode without user text — checked above");

    let mut engine = QueryEngine::new(
        api,
        tools,
        permission_engine,
        hook_runner,
        session,
        system_blocks,
        options,
    );

    match cli.output {
        OutputFormat::Text => {
            let stdout = io::stdout();
            let mut out = stdout.lock();
            let final_text = engine
                .run_turn(user_text, |delta| {
                    let _ = out.write_all(delta.as_bytes());
                    let _ = out.flush();
                }, &mut messages)
                .await?;
            writeln!(out)?;
            tracing::debug!("session: {}", engine.session().id);
            let _ = final_text;
        }
        OutputFormat::Json => {
            let mut full_text = String::new();
            let final_text = engine
                .run_turn(user_text, |delta| {
                    full_text.push_str(delta);
                }, &mut messages)
                .await?;
            let output = json!({
                "session_id": engine.session().id,
                "content": final_text,
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
    }

    Ok(())
}

async fn build_system_blocks(model: &str) -> Vec<SystemBlock> {
    let mut blocks = Vec::new();

    // Static attribution block
    blocks.push(SystemBlock {
        kind: "text".into(),
        text: format!(
            "You are Claude Code, an AI assistant for software engineering tasks. \
             Model: {model}. \
             You have access to tools for reading/writing files, running shell commands, \
             searching code, and more."
        ),
        cache_control: Some(cc_core::CacheControl {
            kind: "ephemeral".into(),
            scope: None,
        }),
    });

    // Git context
    let cwd = std::env::current_dir().unwrap_or_default();
    let git_ctx = cc_git::GitContext::collect(&cwd).await;
    if let Some(git_text) = git_ctx.to_system_text() {
        blocks.push(SystemBlock {
            kind: "text".into(),
            text: git_text,
            cache_control: None,
        });
    }

    // Memory files
    let memories = cc_memory::load_memories();
    if let Some(mem_text) = cc_memory::memories_to_system_text(&memories) {
        blocks.push(SystemBlock {
            kind: "text".into(),
            text: mem_text,
            cache_control: Some(cc_core::CacheControl {
                kind: "ephemeral".into(),
                scope: None,
            }),
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
