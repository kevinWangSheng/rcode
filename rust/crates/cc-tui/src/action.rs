//! AppAction — all actions the TUI can take (§7.2).
//!
//! Terminal events and engine events are mapped to AppActions,
//! which are then applied to the App state via `update()`.

use crate::app::{App, AppMode, PendingPermission};
use crate::commands::{parse, CommandOutcome, CommandRegistry};
use cc_core::Usage;
use cc_core::PromptDecision;
use tokio::sync::oneshot;

/// All actions the TUI can take.
#[derive(Debug)]
pub enum AppAction {
    // Input
    InsertChar(char),
    Backspace,
    Submit,
    NewLine,

    // Navigation
    ScrollUp(u16),
    ScrollDown(u16),
    ScrollToBottom,

    // Streaming
    StreamDelta(String),
    StreamEnd,
    ToolStart { name: String, input_summary: String },
    ToolEnd { name: String, output: String, is_error: bool },

    // Permission
    ShowPermission {
        tool_name: String,
        summary: String,
        reply: oneshot::Sender<PromptDecision>,
    },
    PermissionAllow,
    PermissionAllowAlways,
    PermissionDeny,

    // Slash commands
    SlashCommand(String),

    // Control
    Abort,
    Quit,
    CompactBoundary,
    TurnComplete { usage: Usage },
    Error(String),
    Tick,
}

/// Result of applying an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateResult {
    Continue,
    Quit,
}

/// Context needed for action handling.
pub struct UpdateContext<'a> {
    pub commands: &'a CommandRegistry,
    pub command_ctx: &'a crate::commands::CommandContext,
}

/// Apply an action to the app state.
pub fn update(app: &mut App, action: AppAction, ctx: &UpdateContext) -> UpdateResult {
    match action {
        AppAction::InsertChar(c) => {
            if app.mode == AppMode::Input || app.mode == AppMode::CommandPalette {
                app.input.push(c);
            }
        }
        AppAction::Backspace => {
            if app.mode == AppMode::Input || app.mode == AppMode::CommandPalette {
                app.input.pop();
            }
        }
        AppAction::Submit => {
            let text = app.input.trim().to_string();
            if text.is_empty() {
                return UpdateResult::Continue;
            }
            app.input.clear();

            // Check if it's a slash command
            if let Some(cmd) = parse(&text) {
                let outcome = ctx.commands.execute(&cmd, ctx.command_ctx);
                match outcome {
                    CommandOutcome::Info(msg) => app.push_system(msg),
                    CommandOutcome::Exit => return UpdateResult::Quit,
                    CommandOutcome::Clear => {
                        app.transcript.clear();
                        app.push_system("Transcript cleared.".into());
                    }
                    CommandOutcome::Compact => {
                        app.push_compact_boundary();
                    }
                    CommandOutcome::SwitchModel(name) => {
                        app.status.model = name.clone();
                        app.push_system(format!("Switched model to {name}"));
                    }
                    CommandOutcome::SubmitUserMessage(msg) => {
                        app.push_user(msg);
                        app.start_stream();
                    }
                    CommandOutcome::Unknown(msg) => {
                        app.push_system(msg);
                    }
                }
            } else {
                // Regular user message
                if app.mode == AppMode::Streaming {
                    app.queued.push_back(text);
                } else {
                    app.push_user(text);
                    app.start_stream();
                }
            }
        }
        AppAction::NewLine => {
            app.input.push('\n');
        }
        AppAction::ScrollUp(n) => {
            app.scroll = app.scroll.saturating_add(n);
        }
        AppAction::ScrollDown(n) => {
            app.scroll = app.scroll.saturating_sub(n);
        }
        AppAction::ScrollToBottom => {
            app.scroll = 0;
        }
        AppAction::StreamDelta(delta) => {
            app.on_token(&delta);
        }
        AppAction::StreamEnd => {
            app.finish_stream();
        }
        AppAction::ToolStart { name, input_summary } => {
            app.push_tool_call(name, input_summary);
        }
        AppAction::ToolEnd { name, output, is_error } => {
            app.push_tool_result(name, output, is_error);
        }
        AppAction::ShowPermission { tool_name, summary, reply } => {
            app.permission = Some(PendingPermission {
                tool_name,
                summary,
            });
            app.mode = AppMode::PermissionPrompt;
            // Store reply channel for later
            app.pending_reply = Some(reply);
        }
        AppAction::PermissionAllow => {
            if let Some(reply) = app.pending_reply.take() {
                let _ = reply.send(PromptDecision::Allow);
            }
            app.permission = None;
            app.mode = AppMode::Streaming;
        }
        AppAction::PermissionAllowAlways => {
            if let Some(reply) = app.pending_reply.take() {
                let _ = reply.send(PromptDecision::AllowAlways);
            }
            app.permission = None;
            app.mode = AppMode::Streaming;
        }
        AppAction::PermissionDeny => {
            if let Some(reply) = app.pending_reply.take() {
                let _ = reply.send(PromptDecision::Deny);
            }
            app.permission = None;
            app.mode = AppMode::Streaming;
        }
        AppAction::Abort => {
            if app.mode == AppMode::Streaming {
                app.abort_stream();
            }
        }
        AppAction::Quit => {
            app.should_quit = true;
            return UpdateResult::Quit;
        }
        AppAction::CompactBoundary => {
            app.push_compact_boundary();
        }
        AppAction::TurnComplete { usage } => {
            app.status.input_tokens += usage.input_tokens as u64;
            app.status.output_tokens += usage.output_tokens as u64;
            app.status.turn_count += 1;
        }
        AppAction::Error(msg) => {
            app.push_system(format!("Error: {msg}"));
            if app.mode == AppMode::Streaming {
                app.finish_stream();
            }
        }
        AppAction::Tick => {
            // No-op for now — used for spinner animation
        }
        AppAction::SlashCommand(_) => {
            // Handled via Submit path
        }
    }

    UpdateResult::Continue
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{CommandContext, CommandRegistry};

    fn test_ctx() -> (CommandRegistry, CommandContext) {
        (
            CommandRegistry::empty(),
            CommandContext::new("0.1.0", "test-model"),
        )
    }

    #[test]
    fn insert_and_submit_adds_user_message() {
        let mut app = App::new("s".into(), "m".into());
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

        update(&mut app, AppAction::InsertChar('h'), &uctx);
        update(&mut app, AppAction::InsertChar('i'), &uctx);
        assert_eq!(app.input, "hi");

        update(&mut app, AppAction::Submit, &uctx);
        assert!(app.input.is_empty());
        assert_eq!(app.mode, AppMode::Streaming);
    }

    #[test]
    fn quit_action_returns_quit() {
        let mut app = App::new("s".into(), "m".into());
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };
        let result = update(&mut app, AppAction::Quit, &uctx);
        assert_eq!(result, UpdateResult::Quit);
    }

    #[test]
    fn slash_exit_returns_quit() {
        let mut app = App::new("s".into(), "m".into());
        app.input = "/exit".into();
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };
        let result = update(&mut app, AppAction::Submit, &uctx);
        assert_eq!(result, UpdateResult::Quit);
    }

    #[test]
    fn scroll_up_and_down() {
        let mut app = App::new("s".into(), "m".into());
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };
        update(&mut app, AppAction::ScrollUp(5), &uctx);
        assert_eq!(app.scroll, 5);
        update(&mut app, AppAction::ScrollDown(3), &uctx);
        assert_eq!(app.scroll, 2);
        update(&mut app, AppAction::ScrollToBottom, &uctx);
        assert_eq!(app.scroll, 0);
    }
}
