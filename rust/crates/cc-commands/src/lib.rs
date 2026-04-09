//! cc-commands — slash command registry for the interactive TUI.
//!
//! Commands are entered as `/name [args...]` at the input prompt. There are two
//! kinds:
//!
//!   - **Built-ins** — implemented in this crate (e.g. `/help`, `/exit`, `/memory`,
//!     `/clear`, `/sessions`).
//!   - **Skills** — markdown files loaded via `cc-skills` and `cc-plugins`. Their
//!     name becomes the slash command and their body becomes the user message
//!     submitted to the model.
//!
//! Execution returns a [`CommandOutcome`] that the TUI host interprets — this crate
//! deliberately performs no I/O beyond reading memories and listing sessions.

use std::path::{Path, PathBuf};

use cc_memory::{load_memories, MemoryFile};
use cc_skills::{load_skills, Skill};
use serde::{Deserialize, Serialize};

/// Runtime context the registry needs to render commands that depend on engine
/// state (model name, MCP server list, hook events, project root for `/init`).
/// Built by the TUI host before each `execute` call.
#[derive(Debug, Clone, Default)]
pub struct CommandContext {
    pub version: String,
    pub model: String,
    pub mcp_servers: Vec<String>,
    pub hook_events: Vec<String>,
    pub project_root: Option<PathBuf>,
}

impl CommandContext {
    pub fn new(version: impl Into<String>, model: impl Into<String>) -> Self {
        CommandContext {
            version: version.into(),
            model: model.into(),
            ..Default::default()
        }
    }
}

/// A parsed slash-command invocation: name + raw argument string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    pub name: String,
    pub args: String,
}

/// Parse user input as a slash command. Returns `None` if the input is not a
/// command (i.e. doesn't start with `/`, or is just `/` alone).
pub fn parse(input: &str) -> Option<ParsedCommand> {
    let trimmed = input.trim_start();
    let rest = trimmed.strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }
    let (name, args) = match rest.find(char::is_whitespace) {
        Some(idx) => (&rest[..idx], rest[idx..].trim_start()),
        None => (rest, ""),
    };
    if name.is_empty() {
        return None;
    }
    Some(ParsedCommand {
        name: name.to_ascii_lowercase(),
        args: args.to_string(),
    })
}

/// What the TUI host should do after a command runs.
#[derive(Debug, Clone)]
pub enum CommandOutcome {
    /// Display this text in the transcript as a system/info message — no model call.
    Info(String),
    /// Submit this string as the user's next turn to the model.
    SubmitUserMessage(String),
    /// Clear the current transcript and start a fresh session in-place.
    Clear,
    /// Quit the TUI cleanly.
    Exit,
    /// Manually compact the running transcript (the host calls
    /// `cc_query::compact_messages` on its messages vec).
    Compact,
    /// Switch the active model. The host updates `QueryOptions.model` and
    /// rebuilds the engine. The string is the resolved (full) model id.
    SwitchModel(String),
    /// Unknown command — host shows an error.
    Unknown(String),
}

/// Built-in command names. Kept as a typed enum so the TUI can match exhaustively
/// when constructing help text or autocomplete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Builtin {
    Help,
    Exit,
    Clear,
    Memory,
    Sessions,
    Version,
    Model,
    Cost,
    Compact,
    Config,
    Init,
    Mcp,
    Hooks,
}

impl Builtin {
    pub const ALL: &'static [Builtin] = &[
        Builtin::Help,
        Builtin::Exit,
        Builtin::Clear,
        Builtin::Memory,
        Builtin::Sessions,
        Builtin::Version,
        Builtin::Model,
        Builtin::Cost,
        Builtin::Compact,
        Builtin::Config,
        Builtin::Init,
        Builtin::Mcp,
        Builtin::Hooks,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Builtin::Help => "help",
            Builtin::Exit => "exit",
            Builtin::Clear => "clear",
            Builtin::Memory => "memory",
            Builtin::Sessions => "sessions",
            Builtin::Version => "version",
            Builtin::Model => "model",
            Builtin::Cost => "cost",
            Builtin::Compact => "compact",
            Builtin::Config => "config",
            Builtin::Init => "init",
            Builtin::Mcp => "mcp",
            Builtin::Hooks => "hooks",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Builtin::Help => "show this help",
            Builtin::Exit => "quit the TUI",
            Builtin::Clear => "clear transcript and start a new session",
            Builtin::Memory => "list loaded memory files",
            Builtin::Sessions => "list resumable sessions",
            Builtin::Version => "show version",
            Builtin::Model => "show or switch the active model (`/model <name>`)",
            Builtin::Cost => "show token usage / estimated cost so far",
            Builtin::Compact => "manually compact the conversation transcript",
            Builtin::Config => "show resolved configuration paths and effective model",
            Builtin::Init => "create a starter CLAUDE.md in the project root",
            Builtin::Mcp => "list configured MCP servers",
            Builtin::Hooks => "list configured hook events",
        }
    }

    pub fn from_name(name: &str) -> Option<Builtin> {
        Builtin::ALL.iter().copied().find(|b| b.name() == name)
    }
}

/// Combined registry of built-ins + skills available as slash commands.
#[derive(Debug, Clone, Default)]
pub struct CommandRegistry {
    pub skills: Vec<Skill>,
}

impl CommandRegistry {
    /// Build a registry by discovering skills from disk (`~/.claude/skills` and
    /// project skills) plus all skills bundled by installed plugins.
    pub fn discover() -> Self {
        let mut skills = load_skills();

        let plugins = cc_plugins::load_plugins();
        for p in plugins {
            for s in p.skills {
                // Plugin skills don't override user/project skills with the same name —
                // user customizations win.
                if !skills.iter().any(|x| x.name == s.name) {
                    skills.push(s);
                }
            }
        }

        skills.sort_by(|a, b| a.name.cmp(&b.name));
        CommandRegistry { skills }
    }

    /// Build an empty registry. Useful for tests.
    pub fn empty() -> Self {
        CommandRegistry { skills: Vec::new() }
    }

    /// Build a registry from a fixed skill list. Useful for tests.
    pub fn from_skills(skills: Vec<Skill>) -> Self {
        CommandRegistry { skills }
    }

    /// All command names available, sorted (built-ins first then skills).
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = Builtin::ALL.iter().map(|b| b.name().to_string()).collect();
        for s in &self.skills {
            names.push(s.name.clone());
        }
        names
    }

    /// Look up a skill by name (case-sensitive — names are normalized at load time).
    pub fn skill(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// Execute a parsed command and return the outcome for the TUI to act on.
    pub fn execute(&self, cmd: &ParsedCommand, ctx: &CommandContext) -> CommandOutcome {
        // Built-ins take precedence over skills with the same name.
        if let Some(builtin) = Builtin::from_name(&cmd.name) {
            return self.execute_builtin(builtin, &cmd.args, ctx);
        }

        if let Some(skill) = self.skill(&cmd.name) {
            return CommandOutcome::SubmitUserMessage(render_skill(skill, &cmd.args));
        }

        CommandOutcome::Unknown(format!("unknown command: /{}", cmd.name))
    }

    fn execute_builtin(&self, b: Builtin, args: &str, ctx: &CommandContext) -> CommandOutcome {
        match b {
            Builtin::Help => CommandOutcome::Info(self.help_text()),
            Builtin::Exit => CommandOutcome::Exit,
            Builtin::Clear => CommandOutcome::Clear,
            Builtin::Memory => CommandOutcome::Info(format_memories(&load_memories())),
            Builtin::Sessions => {
                CommandOutcome::Info(format_sessions(&cc_session::list_sessions()))
            }
            Builtin::Version => CommandOutcome::Info(format!("claude {}", ctx.version)),
            Builtin::Model => execute_model(args, ctx),
            Builtin::Cost => CommandOutcome::Info(
                "Token usage tracking is not yet implemented. \
                 Cost will appear here once cc-query starts persisting per-turn usage."
                    .to_string(),
            ),
            Builtin::Compact => CommandOutcome::Compact,
            Builtin::Config => CommandOutcome::Info(format_config(ctx)),
            Builtin::Init => CommandOutcome::Info(execute_init(ctx)),
            Builtin::Mcp => CommandOutcome::Info(format_mcp_servers(&ctx.mcp_servers)),
            Builtin::Hooks => CommandOutcome::Info(format_hook_events(&ctx.hook_events)),
        }
    }

    /// Build the `/help` text. Public so the TUI can render it on first launch.
    pub fn help_text(&self) -> String {
        let mut out = String::new();
        out.push_str("Slash commands:\n\n");
        out.push_str("Built-ins:\n");
        for b in Builtin::ALL {
            out.push_str(&format!("  /{:<10} {}\n", b.name(), b.description()));
        }
        if !self.skills.is_empty() {
            out.push_str("\nSkills:\n");
            for s in &self.skills {
                let desc = if s.description.is_empty() {
                    "(no description)"
                } else {
                    s.description.as_str()
                };
                out.push_str(&format!("  /{:<10} {}\n", s.name, desc));
            }
        }
        out.push_str("\nType / to start a command. Press Esc to cancel a prompt. Ctrl+Q or /exit to quit.\n");
        out
    }
}

/// Render a skill body, substituting `{{args}}` if present. If the skill has no
/// body but has a description, fall back to the description so something useful
/// is sent.
fn render_skill(skill: &Skill, args: &str) -> String {
    let template = if skill.body.is_empty() {
        skill.description.as_str()
    } else {
        skill.body.as_str()
    };

    if template.contains("{{args}}") {
        template.replace("{{args}}", args)
    } else if args.is_empty() {
        template.to_string()
    } else {
        format!("{template}\n\n{args}")
    }
}

fn format_memories(memories: &[MemoryFile]) -> String {
    if memories.is_empty() {
        return "No memories loaded. Add markdown files to ~/.claude/memory/.".to_string();
    }
    let mut out = String::from("Memories:\n");
    for m in memories {
        let desc = if m.description.is_empty() {
            "(no description)"
        } else {
            m.description.as_str()
        };
        out.push_str(&format!("  {} [{}]  {}\n", m.name, m.memory_type, desc));
    }
    out
}

fn format_sessions(ids: &[String]) -> String {
    if ids.is_empty() {
        return "No saved sessions found.".to_string();
    }
    let mut out = String::from("Sessions:\n");
    for id in ids {
        out.push_str(&format!("  {id}\n"));
    }
    out.push_str("\nResume with: claude --resume <id>\n");
    out
}

/// `/model` — with no args, show current. With an arg, request a switch.
/// We don't expand aliases here — the host (`cc-config::resolve_model`) does it
/// when it actually applies the change.
fn execute_model(args: &str, ctx: &CommandContext) -> CommandOutcome {
    let arg = args.trim();
    if arg.is_empty() {
        CommandOutcome::Info(format!("Current model: {}", ctx.model))
    } else {
        CommandOutcome::SwitchModel(arg.to_string())
    }
}

fn format_config(ctx: &CommandContext) -> String {
    let mut out = String::from("Effective configuration:\n");
    out.push_str(&format!("  version       {}\n", ctx.version));
    out.push_str(&format!("  model         {}\n", ctx.model));
    out.push_str(&format!("  mcp servers   {}\n", ctx.mcp_servers.len()));
    out.push_str(&format!("  hook events   {}\n", ctx.hook_events.len()));
    out.push_str(&format!(
        "  project root  {}\n",
        ctx.project_root
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none)".to_string())
    ));
    out.push_str(
        "\nSee ~/.claude/settings.json and .claude/settings.json for editable config.\n",
    );
    out
}

fn format_mcp_servers(servers: &[String]) -> String {
    if servers.is_empty() {
        return "No MCP servers configured.\n\
                Add servers under `mcpServers` in ~/.claude/settings.json or .claude/settings.json.\n"
            .to_string();
    }
    let mut out = String::from("MCP servers:\n");
    for s in servers {
        out.push_str(&format!("  {s}\n"));
    }
    out
}

fn format_hook_events(events: &[String]) -> String {
    if events.is_empty() {
        return "No hooks configured.\n\
                Add hooks under `hooks` in ~/.claude/settings.json or .claude/settings.json.\n"
            .to_string();
    }
    let mut out = String::from("Hook events with at least one handler:\n");
    for e in events {
        out.push_str(&format!("  {e}\n"));
    }
    out
}

/// `/init` — write a starter `CLAUDE.md` to the project root if one doesn't
/// already exist. Returns a message describing what happened.
fn execute_init(ctx: &CommandContext) -> String {
    let Some(root) = ctx.project_root.as_deref() else {
        return "Cannot run /init: no project root in context. \
                Run claude from inside a project directory."
            .to_string();
    };
    let path = root.join("CLAUDE.md");
    if path.exists() {
        return format!(
            "{} already exists — leaving it alone. Edit it directly to update project guidance.",
            path.display()
        );
    }
    let template = init_template(root);
    match std::fs::write(&path, template) {
        Ok(_) => format!("Wrote starter project guide to {}", path.display()),
        Err(e) => format!("Failed to write {}: {e}", path.display()),
    }
}

fn init_template(root: &Path) -> String {
    let project_name = root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "this project".to_string());
    format!(
        "# {project_name}\n\n\
         This file is loaded into Claude's system prompt for sessions started in this directory.\n\n\
         ## What this project is\n\n\
         (Describe the purpose, audience, and tech stack.)\n\n\
         ## How to run / test\n\n\
         (Commands the agent should use to build, test, and lint.)\n\n\
         ## Conventions\n\n\
         (Coding style, naming, file layout the agent should respect.)\n\n\
         ## Things to avoid\n\n\
         (Anti-patterns or directories the agent shouldn't touch.)\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cc_skills::SkillSource;
    use std::path::PathBuf;

    fn skill(name: &str, body: &str, desc: &str) -> Skill {
        Skill {
            name: name.into(),
            description: desc.into(),
            model: None,
            body: body.into(),
            source: SkillSource::User,
            path: PathBuf::from("/tmp"),
        }
    }

    #[test]
    fn parses_simple_command() {
        let p = parse("/help").unwrap();
        assert_eq!(p.name, "help");
        assert_eq!(p.args, "");
    }

    #[test]
    fn parses_command_with_args() {
        let p = parse("/review-pr 123 with focus on tests").unwrap();
        assert_eq!(p.name, "review-pr");
        assert_eq!(p.args, "123 with focus on tests");
    }

    #[test]
    fn parses_lowercases_name() {
        let p = parse("/HELP").unwrap();
        assert_eq!(p.name, "help");
    }

    #[test]
    fn non_command_returns_none() {
        assert!(parse("hello").is_none());
        assert!(parse("/").is_none());
        assert!(parse("").is_none());
    }

    fn ctx() -> CommandContext {
        CommandContext::new("0.1.0", "claude-sonnet-4-6")
    }

    #[test]
    fn builtin_exit_returns_exit_outcome() {
        let reg = CommandRegistry::empty();
        let cmd = parse("/exit").unwrap();
        assert!(matches!(reg.execute(&cmd, &ctx()), CommandOutcome::Exit));
    }

    #[test]
    fn builtin_help_lists_skills_and_new_commands() {
        let reg = CommandRegistry::from_skills(vec![skill("foo", "do foo", "Foo skill")]);
        let cmd = parse("/help").unwrap();
        match reg.execute(&cmd, &ctx()) {
            CommandOutcome::Info(text) => {
                assert!(text.contains("/help"));
                assert!(text.contains("/exit"));
                assert!(text.contains("/foo"));
                assert!(text.contains("Foo skill"));
                // New commands appear in help.
                assert!(text.contains("/model"));
                assert!(text.contains("/compact"));
                assert!(text.contains("/cost"));
                assert!(text.contains("/init"));
                assert!(text.contains("/mcp"));
                assert!(text.contains("/hooks"));
                assert!(text.contains("/config"));
            }
            other => panic!("expected Info, got {other:?}"),
        }
    }

    #[test]
    fn unknown_command_returns_unknown() {
        let reg = CommandRegistry::empty();
        let cmd = parse("/nope").unwrap();
        match reg.execute(&cmd, &ctx()) {
            CommandOutcome::Unknown(msg) => assert!(msg.contains("nope")),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn skill_command_submits_rendered_body() {
        let reg = CommandRegistry::from_skills(vec![skill(
            "review",
            "Review {{args}} please",
            "review code",
        )]);
        let cmd = parse("/review src/main.rs").unwrap();
        match reg.execute(&cmd, &ctx()) {
            CommandOutcome::SubmitUserMessage(msg) => {
                assert_eq!(msg, "Review src/main.rs please");
            }
            other => panic!("expected SubmitUserMessage, got {other:?}"),
        }
    }

    #[test]
    fn skill_without_args_template_appends_args() {
        let reg = CommandRegistry::from_skills(vec![skill("plain", "Be helpful.", "")]);
        let cmd = parse("/plain extra context").unwrap();
        match reg.execute(&cmd, &ctx()) {
            CommandOutcome::SubmitUserMessage(msg) => {
                assert!(msg.starts_with("Be helpful."));
                assert!(msg.contains("extra context"));
            }
            other => panic!("expected SubmitUserMessage, got {other:?}"),
        }
    }

    #[test]
    fn builtin_overrides_skill_with_same_name() {
        let reg = CommandRegistry::from_skills(vec![skill("help", "shadowed", "")]);
        let cmd = parse("/help").unwrap();
        assert!(matches!(reg.execute(&cmd, &ctx()), CommandOutcome::Info(_)));
    }

    #[test]
    fn model_no_args_shows_current() {
        let reg = CommandRegistry::empty();
        let cmd = parse("/model").unwrap();
        match reg.execute(&cmd, &ctx()) {
            CommandOutcome::Info(text) => assert!(text.contains("claude-sonnet-4-6")),
            other => panic!("expected Info, got {other:?}"),
        }
    }

    #[test]
    fn model_with_arg_returns_switch_outcome() {
        let reg = CommandRegistry::empty();
        let cmd = parse("/model opus").unwrap();
        match reg.execute(&cmd, &ctx()) {
            CommandOutcome::SwitchModel(name) => assert_eq!(name, "opus"),
            other => panic!("expected SwitchModel, got {other:?}"),
        }
    }

    #[test]
    fn compact_returns_compact_outcome() {
        let reg = CommandRegistry::empty();
        let cmd = parse("/compact").unwrap();
        assert!(matches!(reg.execute(&cmd, &ctx()), CommandOutcome::Compact));
    }

    #[test]
    fn config_renders_effective_settings() {
        let reg = CommandRegistry::empty();
        let mut c = ctx();
        c.mcp_servers = vec!["filesystem".into(), "github".into()];
        c.hook_events = vec!["PreToolUse".into()];
        match reg.execute(&parse("/config").unwrap(), &c) {
            CommandOutcome::Info(text) => {
                assert!(text.contains("claude-sonnet-4-6"));
                assert!(text.contains("mcp servers   2"));
                assert!(text.contains("hook events   1"));
            }
            other => panic!("expected Info, got {other:?}"),
        }
    }

    #[test]
    fn mcp_lists_servers() {
        let reg = CommandRegistry::empty();
        let mut c = ctx();
        c.mcp_servers = vec!["filesystem".into()];
        match reg.execute(&parse("/mcp").unwrap(), &c) {
            CommandOutcome::Info(text) => assert!(text.contains("filesystem")),
            other => panic!("expected Info, got {other:?}"),
        }
    }

    #[test]
    fn hooks_lists_events() {
        let reg = CommandRegistry::empty();
        let mut c = ctx();
        c.hook_events = vec!["PreToolUse".into(), "Stop".into()];
        match reg.execute(&parse("/hooks").unwrap(), &c) {
            CommandOutcome::Info(text) => {
                assert!(text.contains("PreToolUse"));
                assert!(text.contains("Stop"));
            }
            other => panic!("expected Info, got {other:?}"),
        }
    }

    #[test]
    fn init_creates_claude_md_in_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = ctx();
        c.project_root = Some(dir.path().to_path_buf());

        let reg = CommandRegistry::empty();
        match reg.execute(&parse("/init").unwrap(), &c) {
            CommandOutcome::Info(msg) => assert!(msg.contains("Wrote starter project guide")),
            other => panic!("expected Info, got {other:?}"),
        }
        assert!(dir.path().join("CLAUDE.md").exists());

        // Second run should leave it alone.
        match reg.execute(&parse("/init").unwrap(), &c) {
            CommandOutcome::Info(msg) => assert!(msg.contains("already exists")),
            other => panic!("expected Info, got {other:?}"),
        }
    }
}
