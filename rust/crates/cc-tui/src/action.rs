//! AppAction — all actions the TUI can take (§7.2).
//!
//! Terminal events and engine events are mapped to AppActions,
//! which are then applied to the App state via `update()`.

use std::time::Instant;

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
    /// Emergency escape hatch — a second Ctrl+C within
    /// `app::FORCE_QUIT_WINDOW_MS` when a previous `Abort` has not yet
    /// unblocked the UI. The main loop must restore the terminal and
    /// `exit(130)` without waiting for in-flight tasks.
    ForceQuit,
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
    /// Caller must restore the terminal immediately and `exit(130)` —
    /// do not wait on the event loop's normal teardown path.
    ForceQuit,
    /// User submitted a message that must be sent to the engine.
    /// `start_stream()` has already been called on the App.
    SubmitToEngine(String),
}

/// Context needed for action handling.
pub struct UpdateContext<'a> {
    pub commands: &'a CommandRegistry,
    pub command_ctx: &'a crate::commands::CommandContext,
}

/// Resolve the post-permission-dialog mode.
///
/// Prefer the snapshot captured when `ShowPermission` fired. If the snapshot
/// turns out to be `PermissionPrompt` (shouldn't happen, but defensive
/// against nested / stale states) or is missing entirely, fall back to
/// `Input` — a stream that has already completed MUST not be re-entered.
fn restore_after_permission(app: &mut App) -> AppMode {
    let snap = app.pre_permission_mode.take();
    match snap {
        Some(AppMode::PermissionPrompt) | None => AppMode::Input,
        Some(m) => m,
    }
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
                // Snapshot current usage into CommandContext before dispatch
                // so /cost and similar commands see up-to-date numbers.
                let mut cmd_ctx = ctx.command_ctx.clone();
                cmd_ctx.input_tokens = app.status.input_tokens;
                cmd_ctx.output_tokens = app.status.output_tokens;
                cmd_ctx.estimated_cost_usd = app.status.estimated_cost_usd;
                cmd_ctx.turn_count = app.status.turn_count;
                let outcome = ctx.commands.execute(&cmd, &cmd_ctx);
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
            // Snapshot the pre-dialog mode so decision arms can restore it
            // rather than hard-coding Streaming (which is wrong when the
            // dialog arrives after the final assistant block has landed and
            // mode is already Input).
            app.pre_permission_mode = Some(app.mode);
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
            app.mode = restore_after_permission(app);
        }
        AppAction::PermissionAllowAlways => {
            if let Some(reply) = app.pending_reply.take() {
                let _ = reply.send(PromptDecision::AllowAlways);
            }
            app.permission = None;
            app.mode = restore_after_permission(app);
        }
        AppAction::PermissionDeny => {
            if let Some(reply) = app.pending_reply.take() {
                let _ = reply.send(PromptDecision::Deny);
            }
            app.permission = None;
            app.mode = restore_after_permission(app);
        }
        AppAction::Abort => {
            // Stamp regardless of mode so `ForceQuit` escalation works even
            // if the first Ctrl+C happened outside an active stream (e.g. a
            // stuck permission dialog cleanup).
            app.last_abort_at = Some(Instant::now());
            // Transient status-line hint — nudges the user toward the escape
            // hatch without polluting the transcript.
            app.status_hint = Some("press Ctrl+C again to force quit".into());
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
        AppAction::ForceQuit => {
            // Best-effort cancel any in-flight work; the caller is responsible
            // for restoring the terminal and exiting immediately.
            if let Some(cancel) = app.current_turn_cancel.take() {
                cancel.cancel();
            }
            if let Some(reply) = app.pending_reply.take() {
                let _ = reply.send(PromptDecision::Deny);
            }
            app.should_quit = true;
            return UpdateResult::ForceQuit;
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
            // Clear the Ctrl+C status hint once the force-quit window lapses
            // so stale hints don't linger in the status line.
            if app.status_hint.is_some() && !app.within_force_quit_window(Instant::now()) {
                app.status_hint = None;
            }
            // Spinner animation driven by Ratatui blink modifier.
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

    // ── Ctrl+C force-quit escalation ──────────────────────────────────────

    #[test]
    fn abort_stamps_last_abort_at_and_shows_hint() {
        let mut app = App::new("s".into(), "m".into());
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

        assert!(app.last_abort_at.is_none());
        assert!(app.status_hint.is_none());

        update(&mut app, AppAction::Abort, &uctx);

        assert!(app.last_abort_at.is_some(), "first Abort must stamp last_abort_at");
        assert!(
            app.status_hint
                .as_deref()
                .is_some_and(|h| h.contains("force quit")),
            "status hint must mention force quit; got {:?}",
            app.status_hint
        );
    }

    #[test]
    fn two_ctrl_c_within_window_yields_force_quit() {
        let mut app = App::new("s".into(), "m".into());
        app.start_stream();
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

        // First Ctrl+C → graceful Abort.
        let r1 = update(&mut app, AppAction::Abort, &uctx);
        assert!(matches!(r1, UpdateResult::Continue));

        // Within the 2s window — force quit.
        assert!(app.within_force_quit_window(Instant::now()));
        let r2 = update(&mut app, AppAction::ForceQuit, &uctx);
        assert!(matches!(r2, UpdateResult::ForceQuit), "got {r2:?}");
        assert!(app.should_quit, "ForceQuit must flag should_quit");
    }

    #[test]
    fn single_ctrl_c_after_window_still_aborts() {
        use std::time::Duration;

        let mut app = App::new("s".into(), "m".into());
        app.start_stream();
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

        // Prime a stale abort 3s ago.
        app.last_abort_at = Instant::now().checked_sub(Duration::from_secs(3));
        assert!(!app.within_force_quit_window(Instant::now()));

        // A fresh Abort should behave like a first-press graceful abort.
        let r = update(&mut app, AppAction::Abort, &uctx);
        assert!(matches!(r, UpdateResult::Continue));
        assert_eq!(app.mode, AppMode::Input);
    }

    #[test]
    fn tick_clears_stale_status_hint() {
        use std::time::Duration;

        let mut app = App::new("s".into(), "m".into());
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

        app.status_hint = Some("press Ctrl+C again to force quit".into());
        app.last_abort_at = Instant::now().checked_sub(Duration::from_secs(3));

        update(&mut app, AppAction::Tick, &uctx);
        assert!(app.status_hint.is_none(), "stale hint must be cleared on tick");
    }

    #[test]
    fn tick_preserves_fresh_status_hint() {
        let mut app = App::new("s".into(), "m".into());
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

        app.status_hint = Some("press Ctrl+C again to force quit".into());
        app.last_abort_at = Some(Instant::now());

        update(&mut app, AppAction::Tick, &uctx);
        assert!(app.status_hint.is_some(), "fresh hint must survive tick");
    }

    // ── Permission-dialog mode restoration ─────────────────────────────────

    fn show_permission() -> (AppAction, oneshot::Receiver<PromptDecision>) {
        let (tx, rx) = oneshot::channel::<PromptDecision>();
        (
            AppAction::ShowPermission {
                tool_name: "Bash".into(),
                summary: "ls".into(),
                reply: tx,
            },
            rx,
        )
    }

    #[test]
    fn permission_deny_during_streaming_restores_streaming() {
        let mut app = App::new("s".into(), "m".into());
        app.start_stream();
        assert_eq!(app.mode, AppMode::Streaming);

        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

        let (show, _rx) = show_permission();
        update(&mut app, show, &uctx);
        assert_eq!(app.mode, AppMode::PermissionPrompt);
        assert_eq!(app.pre_permission_mode, Some(AppMode::Streaming));

        update(&mut app, AppAction::PermissionDeny, &uctx);
        assert_eq!(
            app.mode,
            AppMode::Streaming,
            "dialog opened mid-stream must return to Streaming on Deny"
        );
        assert!(app.pre_permission_mode.is_none(), "snapshot must be consumed");
    }

    #[test]
    fn permission_deny_after_stream_ended_restores_input_not_streaming() {
        // This is the regression the fix targets: a tool call request arriving
        // just as the final assistant block lands means the App is already in
        // Input when ShowPermission fires. Hard-coding Streaming on Deny made
        // the next keystroke land in the wrong handler.
        let mut app = App::new("s".into(), "m".into());
        assert_eq!(app.mode, AppMode::Input, "pre-condition: Input mode");

        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

        let (show, _rx) = show_permission();
        update(&mut app, show, &uctx);
        assert_eq!(app.mode, AppMode::PermissionPrompt);
        assert_eq!(app.pre_permission_mode, Some(AppMode::Input));

        update(&mut app, AppAction::PermissionDeny, &uctx);
        assert_eq!(
            app.mode,
            AppMode::Input,
            "dialog opened after stream ended must return to Input on Deny, \
             not forced back into Streaming"
        );
    }

    #[test]
    fn permission_allow_restores_snapshot_mode() {
        let mut app = App::new("s".into(), "m".into());
        app.start_stream();

        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

        let (show, _rx) = show_permission();
        update(&mut app, show, &uctx);
        update(&mut app, AppAction::PermissionAllow, &uctx);
        assert_eq!(app.mode, AppMode::Streaming);
    }

    #[test]
    fn permission_allow_always_restores_snapshot_mode() {
        let mut app = App::new("s".into(), "m".into());
        // Mode is Input (stream already done).
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

        let (show, _rx) = show_permission();
        update(&mut app, show, &uctx);
        update(&mut app, AppAction::PermissionAllowAlways, &uctx);
        assert_eq!(app.mode, AppMode::Input);
    }

    #[test]
    fn permission_decision_without_snapshot_falls_back_to_input() {
        // Defensive: if somehow the decision arm runs without a prior
        // ShowPermission having captured a snapshot, restore to Input rather
        // than leaving the app stuck in PermissionPrompt or fabricating
        // Streaming.
        let mut app = App::new("s".into(), "m".into());
        app.mode = AppMode::PermissionPrompt;
        app.pre_permission_mode = None;

        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext { commands: &reg, command_ctx: &ctx };

        update(&mut app, AppAction::PermissionDeny, &uctx);
        assert_eq!(app.mode, AppMode::Input);
    }
}
