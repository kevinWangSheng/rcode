use std::io::{self, Write};

use clap::{Parser, ValueEnum};
use serde_json::json;
use tracing_subscriber::EnvFilter;

use cc_api::ApiClient;
use cc_auth::{resolve_credentials, Credentials};
use cc_config::load_settings;
use cc_core::{models, MessageParam, SystemBlock};
use cc_hooks::{HookRunner, HooksConfig};
use cc_permissions::PermissionEngine;
use cc_query::{
    engine::{QueryEngine, QueryOptions},
};
use cc_session::Session;
use cc_tools::default_tools;

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

    /// Resume a previous session by ID.
    #[arg(long, value_name = "SESSION_ID")]
    resume: Option<String>,

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
    let model = cli
        .model
        .or_else(|| settings.model.clone())
        .unwrap_or_else(|| models::DEFAULT.to_string());

    // Get user input
    let user_text = match &cli.message {
        Some(text) => text.clone(),
        None => {
            if atty_is_stdin() && cli.resume.is_none() {
                return Err("no message provided — use --message or pipe text via stdin".into());
            }
            if cli.resume.is_some() && cli.message.is_none() {
                // Resume without a new message just restores; still need input for next turn
                // For now require --message with --resume
                return Err("--resume requires --message to provide the next turn's input".into());
            }
            let mut buf = String::new();
            io::stdin().read_line(&mut buf)?;
            buf.trim().to_string()
        }
    };

    // Build session (new or resumed)
    let (session, mut messages) = if let Some(session_id) = &cli.resume {
        let (s, msgs) = Session::resume(session_id)?;
        eprintln!("Resumed session {} ({} messages)", session_id, msgs.len());
        (s, msgs)
    } else {
        let s = Session::new()?;
        tracing::debug!("new session: {}", s.id);
        (s, Vec::<MessageParam>::new())
    };

    eprintln!("Session: {}", session.id);

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

    // Build tools
    let tools = default_tools();

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

    // Short-circuit: if no tools needed (simple query mode without tool loop)
    // For --output json or when tools aren't needed, could use simple path.
    // But for M2, always use the query engine.
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
    build_system_blocks_inner(
        model,
        {
            let cwd = std::env::current_dir().unwrap_or_default();
            cc_git::GitContext::collect(&cwd).await.to_system_text()
        },
        cc_memory::memories_to_system_text(&cc_memory::load_memories()),
    )
}

/// Three-tier system-prompt tagging per `RUST_REWRITE_PLAN.md` §3:
///
/// - attribution (tier 1 / uncached): no `cache_control`
/// - static instruction (tier 2 / global cache): `ephemeral_global`
/// - git / memory / dynamic (tier 3 / org cache): `ephemeral_org`
///
/// Block ordering is attribution → static → dynamic so the server caches
/// them in the right tier.
fn build_system_blocks_inner(
    model: &str,
    git_text: Option<String>,
    memory_text: Option<String>,
) -> Vec<SystemBlock> {
    let mut blocks = Vec::new();

    // Tier 1: attribution — uncached (matches TS client).
    blocks.push(SystemBlock {
        kind: "text".into(),
        text: format!(
            "You are Claude Code, an AI assistant for software engineering tasks. \
             Model: {model}."
        ),
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
        block.cache_control.as_ref().and_then(|c| c.scope.as_deref())
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
        );

        assert_eq!(blocks.len(), 4, "expected 4 blocks (attr + static + git + memory)");

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
        let blocks = build_system_blocks_inner(
            "claude-sonnet-4-6",
            None,
            Some("mem".into()),
        );
        let wire = serde_json::to_value(&blocks).expect("serialize");

        // Attribution block: no cache_control key at all (skip_serializing_if).
        let attr = &wire[0];
        assert_eq!(attr["type"], "text");
        assert!(attr.get("cache_control").is_none());

        // Static instruction block: global cache.
        let static_blk = &wire[1];
        assert_eq!(
            static_blk["cache_control"],
            json!({"type": "ephemeral", "scope": "global"})
        );

        // Memory block: org cache.
        let mem_blk = &wire[2];
        assert_eq!(
            mem_blk["cache_control"],
            json!({"type": "ephemeral", "scope": "org"})
        );
    }

    #[test]
    fn three_tier_tagging_handles_absent_dynamic_blocks() {
        let blocks = build_system_blocks_inner("claude-sonnet-4-6", None, None);
        assert_eq!(blocks.len(), 2, "attribution + static only");
        assert!(blocks[0].cache_control.is_none());
        assert_eq!(cc_scope(&blocks[1]), Some("global"));
    }
}
