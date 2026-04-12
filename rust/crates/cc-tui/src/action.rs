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
    /// Turn completed: update token usage, finish stream, drain queued input.
    TurnComplete { usage: Usage },
    Error(String),
    Tick,
}

/// Result of applying an action.
#[derive(Debug)]
pub enum UpdateResult {
    Continue,
    Quit,
    /// User submitted a message that must be sent to the engine.
    /// `start_stream()` has already been called on the App.
    SubmitToEngine(String),
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
                        app.push_user(msg.clone());
                        app.start_stream();
                        return UpdateResult::SubmitToEngine(msg);
                    }
                    CommandOutcome::Unknown(msg) => {
                        app.push_system(msg);
                    }
                }
            } else {
                // Regular user message
                if app.mode == AppMode::Streaming {
                    // Queue for after the current turn finishes.
                    app.queued.push_back(text);
                } else {
                    app.push_user(text.clone());
                    app.start_stream();
                    return UpdateResult::SubmitToEngine(text);
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
            if app.mode == AppMode::Streaming || app.mode == AppMode::PermissionPrompt {
                // Cancel running turn before aborting UI state.
                if let Some(cancel) = app.current_turn_cancel.take() {
                    cancel.cancel();
                }
                // Deny any pending permission prompt.
                if let Some(reply) = app.pending_reply.take() {
                    let _ = reply.send(PromptDecision::Deny);
                }
                app.permission = None;
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
            app.current_turn_cancel = None;
            app.finish_stream();

            // Drain one queued message (submitted while streaming was running).
            if let Some(queued) = app.queued.pop_front() {
                app.push_user(queued.clone());
                app.start_stream();
                return UpdateResult::SubmitToEngine(queued);
            }
        }
        AppAction::Error(msg) => {
            app.push_system(format!("Error: {msg}"));
            app.current_turn_cancel = None;
            if app.mode == AppMode::Streaming {
                app.finish_stream();
            }
        }
        AppAction::Tick => {
            // No-op — spinner animation driven by Ratatui blink modifier.
        }
        AppAction::SlashCommand(_) => {
            // Handled via Submit path.
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
    fn insert_and_submit_adds_user_message_and_returns_submit_to_engine() {
        let mut app = App::new("s".into(), "m".into());
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

        update(&mut app, AppAction::InsertChar('h'), &uctx);
        update(&mut app, AppAction::InsertChar('i'), &uctx);
        assert_eq!(app.input, "hi");

        let result = update(&mut app, AppAction::Submit, &uctx);
        assert!(app.input.is_empty());
        assert_eq!(app.mode, AppMode::Streaming);
        assert!(matches!(result, UpdateResult::SubmitToEngine(t) if t == "hi"));
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
        assert!(matches!(result, UpdateResult::Quit));
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
        assert!(matches!(result, UpdateResult::Quit));
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

    #[test]
    fn turn_complete_finishes_stream_and_updates_status() {
        let mut app = App::new("s".into(), "m".into());
        app.start_stream();
        app.on_token("hello");
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

        let usage = cc_core::Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        };
        update(&mut app, AppAction::TurnComplete { usage }, &uctx);

        assert_eq!(app.mode, AppMode::Input);
        assert_eq!(app.status.input_tokens, 10);
        assert_eq!(app.status.output_tokens, 5);
        assert_eq!(app.status.turn_count, 1);
        assert!(matches!(app.transcript.last(), Some(crate::app::TranscriptItem::AssistantText(_))));
    }

    #[test]
    fn turn_complete_drains_queued_message() {
        let mut app = App::new("s".into(), "m".into());
        app.mode = AppMode::Streaming;
        app.queued.push_back("queued msg".into());
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

        let usage = cc_core::Usage::default();
        let result = update(&mut app, AppAction::TurnComplete { usage }, &uctx);

        assert!(matches!(result, UpdateResult::SubmitToEngine(t) if t == "queued msg"));
        assert_eq!(app.mode, AppMode::Streaming);
    }

    #[test]
    fn abort_during_streaming_preserves_partial_text() {
        let mut app = App::new("s".into(), "m".into());
        app.start_stream();
        app.on_token("partial");
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

        update(&mut app, AppAction::Abort, &uctx);

        assert_eq!(app.mode, AppMode::Input);
        match app.transcript.last() {
            Some(crate::app::TranscriptItem::AssistantText(t)) => {
                assert!(t.contains("partial") && t.contains("aborted"))
            }
            _ => panic!("expected aborted assistant text"),
        }
    }
}
