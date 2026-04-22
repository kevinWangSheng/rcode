//! AppAction — all actions the TUI can take (§7.2).
//!
//! Terminal events and engine events are mapped to AppActions,
//! which are then applied to the App state via `update()`.

use std::time::Instant;

use crate::app::{App, AppMode, PendingPermission};
use crate::commands::{parse, CommandOutcome, CommandRegistry};
use cc_core::PromptDecision;
use cc_core::Usage;
use tokio::sync::oneshot;

/// All actions the TUI can take.
#[derive(Debug)]
pub enum AppAction {
    // Input
    InsertChar(char),
    Backspace,
    /// Delete the char AT the caret (Delete key — opposite direction
    /// from Backspace).
    DeleteChar,
    /// Move the insertion caret by one char. +1 = right, -1 = left.
    /// Anything else is ignored so callers can't silently move by
    /// multiple chars at a time.
    CursorMove(i32),
    /// Jump the caret to the start of the input buffer (Home).
    CursorHome,
    /// Jump the caret to the end of the input buffer (End).
    CursorEnd,
    Submit,
    NewLine,

    // Navigation
    ScrollUp(u16),
    ScrollDown(u16),
    ScrollToBottom,

    // Streaming
    StreamDelta(String),
    ToolStart {
        name: String,
        input_summary: String,
        /// Full JSON input — retained so the renderer can build a diff view
        /// for Edit or show a Bash command preview without re-parsing.
        raw_input: serde_json::Value,
    },
    ToolEnd {
        name: String,
        output: String,
        is_error: bool,
    },

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
    /// Re-load `~/.claude/keybindings.json` and swap the active map on `App`.
    /// Triggered by the `/reload-keybindings` slash command.
    ReloadKeybindings,

    // Slash-command palette (M5 Phase C / AC-V5)
    /// Enter palette mode. Snapshots the current buffer so Esc can restore it.
    PaletteOpen,
    /// Adjust the highlighted row. Positive = down, negative = up.
    PaletteMove(i32),
    /// Replace the input with the highlighted command + trailing space and
    /// return to normal input mode. Equivalent to Tab / Enter.
    PaletteAccept,
    /// Dismiss the palette and restore the snapshotted buffer byte-for-byte.
    PaletteCancel,

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
    TurnComplete {
        usage: Usage,
    },
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

/// Recompute `palette_matches` + clamp `palette_selected` based on the
/// current `input` buffer and the available commands. A no-op when the
/// app is not in palette mode.
pub fn refresh_palette(app: &mut App, commands: &CommandRegistry) {
    if app.mode != AppMode::CommandPalette {
        return;
    }
    let filter = palette_filter(&app.input).to_ascii_lowercase();
    let mut names = commands.names();
    names.sort();
    names.dedup();
    app.palette_matches = names
        .into_iter()
        .filter(|n| n.to_ascii_lowercase().starts_with(&filter))
        .collect();
    if app.palette_selected >= app.palette_matches.len() {
        app.palette_selected = app.palette_matches.len().saturating_sub(1);
    }
}

fn palette_filter(input: &str) -> &str {
    input.trim_start().strip_prefix('/').unwrap_or("")
}

/// Reply to the pending permission prompt, close the dialog, and restore
/// the pre-dialog mode. Factored out so the three Permission arms don't
/// each repeat the send-take-clear-restore sequence.
fn resolve_permission(app: &mut App, decision: PromptDecision) {
    if let Some(reply) = app.pending_reply.take() {
        let _ = reply.send(decision);
    }
    app.permission = None;
    app.mode = restore_after_permission(app);
}

/// Close the slash-command palette: flip back to Input mode, clear the
/// match list + selection, and replace the input buffer with
/// `replacement` (`Some("/foo ")` for Accept, the snapshot for Cancel,
/// `None` to leave whatever the user typed so far).
fn close_palette(app: &mut App, replacement: Option<String>) {
    app.mode = AppMode::Input;
    app.palette_original = None;
    app.palette_matches.clear();
    app.palette_selected = 0;
    if let Some(new_input) = replacement {
        app.set_input(new_input);
    }
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
                app.input_insert_char(c);
                if app.mode == AppMode::CommandPalette {
                    refresh_palette(app, ctx.commands);
                }
            }
        }
        AppAction::Backspace => {
            if app.mode == AppMode::Input || app.mode == AppMode::CommandPalette {
                app.input_backspace();
                if app.mode == AppMode::CommandPalette {
                    // Backspacing the leading `/` cancels the palette so the
                    // user isn't stuck picking from stale results.
                    if !app.input.trim_start().starts_with('/') {
                        close_palette(app, None);
                    } else {
                        refresh_palette(app, ctx.commands);
                    }
                }
            }
        }
        AppAction::DeleteChar => {
            if app.mode == AppMode::Input || app.mode == AppMode::CommandPalette {
                app.input_delete();
                if app.mode == AppMode::CommandPalette {
                    refresh_palette(app, ctx.commands);
                }
            }
        }
        AppAction::CursorMove(delta) => {
            if app.mode == AppMode::Input || app.mode == AppMode::CommandPalette {
                match delta {
                    d if d < 0 => app.input_cursor_left(),
                    d if d > 0 => app.input_cursor_right(),
                    _ => {}
                }
            }
        }
        AppAction::CursorHome => {
            if app.mode == AppMode::Input || app.mode == AppMode::CommandPalette {
                app.input_cursor_home();
            }
        }
        AppAction::CursorEnd => {
            if app.mode == AppMode::Input || app.mode == AppMode::CommandPalette {
                app.input_cursor_end();
            }
        }
        AppAction::Submit => {
            let text = app.input.trim().to_string();
            if text.is_empty() {
                return UpdateResult::Continue;
            }
            app.clear_input();

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
                    CommandOutcome::Exit => {
                        // If a stream is running when the user types /exit,
                        // cancel the engine turn and preserve any partial
                        // assistant text under an "[aborted]" marker so the
                        // transcript replay on resume still makes sense.
                        if app.mode == AppMode::Streaming {
                            if let Some(tok) = app.current_turn_cancel.take() {
                                tok.cancel();
                            }
                            app.abort_stream();
                        }
                        return UpdateResult::Quit;
                    }
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
                    CommandOutcome::ReloadKeybindings => {
                        reload_keybindings(app, crate::keybindings::Keybindings::try_load);
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
            app.input_insert_char('\n');
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
            // AC-2b: tokens arriving after Abort must be dropped. The engine
            // may still deliver in-flight deltas after cancellation (the
            // network read loop is async), so guard at the action boundary
            // rather than relying on the engine to stop emitting instantly.
            if app.mode == AppMode::Streaming {
                app.on_token(&delta);
            }
        }
        AppAction::ToolStart {
            name,
            input_summary,
            raw_input,
        } => {
            app.push_tool_call_with_input(name, input_summary, raw_input);
        }
        AppAction::ToolEnd {
            name,
            output,
            is_error,
        } => {
            app.push_tool_result(name, output, is_error);
        }
        AppAction::ShowPermission {
            tool_name,
            summary,
            reply,
        } => {
            // Snapshot the pre-dialog mode so decision arms can restore it
            // rather than hard-coding Streaming (which is wrong when the
            // dialog arrives after the final assistant block has landed and
            // mode is already Input).
            app.pre_permission_mode = Some(app.mode);
            app.permission = Some(PendingPermission { tool_name, summary });
            app.mode = AppMode::PermissionPrompt;
            app.pending_reply = Some(reply);
        }
        AppAction::PermissionAllow => resolve_permission(app, PromptDecision::Allow),
        AppAction::PermissionAllowAlways => resolve_permission(app, PromptDecision::AllowAlways),
        AppAction::PermissionDeny => resolve_permission(app, PromptDecision::Deny),
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
        AppAction::PaletteOpen => {
            if app.mode == AppMode::Input {
                app.palette_original = Some(app.input.clone());
                app.mode = AppMode::CommandPalette;
                app.palette_selected = 0;
                // Insert the leading `/` that triggered the palette so the
                // filter starts at "/" (user sees the char they typed).
                // Tests that call PaletteOpen directly on a non-empty buffer
                // preserve whatever is already there.
                if !app.input.starts_with('/') {
                    app.input_insert_char('/');
                }
                refresh_palette(app, ctx.commands);
            }
        }
        AppAction::PaletteMove(delta) => {
            if app.mode != AppMode::CommandPalette {
                return UpdateResult::Continue;
            }
            let n = app.palette_matches.len();
            if n == 0 {
                return UpdateResult::Continue;
            }
            let cur = app.palette_selected as i32;
            let next = (cur + delta).rem_euclid(n as i32);
            app.palette_selected = next as usize;
        }
        AppAction::PaletteAccept => {
            if app.mode != AppMode::CommandPalette {
                return UpdateResult::Continue;
            }
            let accepted = app
                .palette_matches
                .get(app.palette_selected)
                .map(|name| format!("/{name} "));
            close_palette(app, accepted);
        }
        AppAction::PaletteCancel => {
            if app.mode == AppMode::CommandPalette {
                let restored = app.palette_original.take();
                close_palette(app, restored);
            }
        }
        AppAction::Tick => {
            // Clear the Ctrl+C status hint once the force-quit window lapses
            // so stale hints don't linger in the status line.
            if app.status_hint.is_some() && !app.within_force_quit_window(Instant::now()) {
                app.status_hint = None;
            }
            // Advance spinner animation only while a stream is live. A wrapping
            // add is fine even across the 2^64 boundary — we only take the
            // low-order modulo in `spinner_glyph`.
            if app.stream_started_at.is_some() {
                app.spinner_frame = app.spinner_frame.wrapping_add(1);
            }
        }
        AppAction::SlashCommand(_) => {
            // Handled via Submit path.
        }
        AppAction::ReloadKeybindings => {
            reload_keybindings(app, crate::keybindings::Keybindings::try_load);
        }
    }

    UpdateResult::Continue
}

/// Re-read the keybinding map via `loader`, swap it on `App`, and push a
/// short status-line toast + transcript notice reporting success (with
/// binding count) or a parse error referencing the offending file.
///
/// Preserves the previously-cached map when the loader reports an error, so
/// an editor that saved a malformed file mid-session never wipes the active
/// bindings (see the "malformed file during edit" scenario in the spec).
///
/// Factored out so the `Submit` slash-command path and the direct
/// `AppAction::ReloadKeybindings` arm stay in lock-step.
pub(crate) fn reload_keybindings<F>(app: &mut App, loader: F)
where
    F: FnOnce() -> Result<Option<crate::keybindings::Keybindings>, String>,
{
    match app.reload_keybindings_with(loader) {
        Ok(count) => {
            let msg = format!("Reloaded {count} keybindings");
            app.status_hint = Some(msg.clone());
            app.push_system(msg);
        }
        Err(e) => {
            let msg = format!("Reload failed: {e}");
            app.status_hint = Some(msg.clone());
            app.push_system(msg);
        }
    }
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

    /// Pins the 2026-04-22 "can't move cursor left/right" regression:
    /// InsertChar must honour `input_cursor` so the user can edit the
    /// middle of a buffer, and Left/Right/Home/End/Delete must mutate
    /// the caret position / buffer as expected.
    #[test]
    fn cursor_movement_inserts_mid_buffer_and_deletes_at_caret() {
        let mut app = App::new("s".into(), "m".into());
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

        // Type "helo" end-on.
        for c in "helo".chars() {
            update(&mut app, AppAction::InsertChar(c), &uctx);
        }
        assert_eq!(app.input, "helo");
        assert_eq!(app.input_cursor, 4);

        // Move one char left → between 'l' and 'o'.
        update(&mut app, AppAction::CursorMove(-1), &uctx);
        assert_eq!(app.input_cursor, 3);

        // Insert 'l' — buffer becomes "hello", caret advances past it.
        update(&mut app, AppAction::InsertChar('l'), &uctx);
        assert_eq!(app.input, "hello");
        assert_eq!(app.input_cursor, 4);

        // Home jumps to start; Delete removes 'h'.
        update(&mut app, AppAction::CursorHome, &uctx);
        assert_eq!(app.input_cursor, 0);
        update(&mut app, AppAction::DeleteChar, &uctx);
        assert_eq!(app.input, "ello");
        assert_eq!(app.input_cursor, 0);

        // End jumps to buffer end.
        update(&mut app, AppAction::CursorEnd, &uctx);
        assert_eq!(app.input_cursor, app.input.len());

        // Backspace at start of buffer is a no-op (invariant guard).
        update(&mut app, AppAction::CursorHome, &uctx);
        let before = app.input.clone();
        update(&mut app, AppAction::Backspace, &uctx);
        assert_eq!(app.input, before);
        assert_eq!(app.input_cursor, 0);
    }

    /// CJK / emoji insertion must keep the caret on UTF-8 char
    /// boundaries — otherwise a follow-up left-arrow panics on
    /// `String::remove`. This anchors the invariant.
    #[test]
    fn cursor_movement_respects_utf8_boundaries() {
        let mut app = App::new("s".into(), "m".into());
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

        update(&mut app, AppAction::InsertChar('中'), &uctx);
        update(&mut app, AppAction::InsertChar('文'), &uctx);
        assert_eq!(app.input, "中文");
        assert_eq!(app.input_cursor, "中文".len());

        update(&mut app, AppAction::CursorMove(-1), &uctx);
        assert_eq!(app.input_cursor, "中".len());

        update(&mut app, AppAction::Backspace, &uctx);
        assert_eq!(app.input, "文");
        assert_eq!(app.input_cursor, 0);
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
        app.set_input("/exit");
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };
        let result = update(&mut app, AppAction::Submit, &uctx);
        assert!(matches!(result, UpdateResult::Quit));
    }

    /// BUG-4 regression: `/exit` while a stream is running must cancel the
    /// turn and commit partial assistant text under the `[aborted]` marker
    /// rather than silently dropping it. If this test regresses, users who
    /// type `/exit` mid-stream will lose anything the model had emitted so
    /// far — session replay would then jump from the user prompt straight
    /// to the next turn with no record of what the model had started.
    #[test]
    fn slash_exit_during_stream_aborts_and_preserves_partial_text() {
        let mut app = App::new("s".into(), "m".into());
        app.start_stream();
        app.on_token("so far the model said");
        // Install a cancel token so we can observe it being fired.
        let tok = tokio_util::sync::CancellationToken::new();
        app.current_turn_cancel = Some(tok.clone());

        app.set_input("/exit");
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };
        let result = update(&mut app, AppAction::Submit, &uctx);
        assert!(matches!(result, UpdateResult::Quit));
        assert!(tok.is_cancelled(), "engine turn must be cancelled");
        match app.transcript.last() {
            Some(crate::app::TranscriptItem::AssistantText(t)) => {
                assert!(t.contains("so far the model said"));
                assert!(t.contains("aborted"));
            }
            other => panic!("expected aborted assistant text, got {other:?}"),
        }
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
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

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
        assert!(matches!(
            app.transcript.last(),
            Some(crate::app::TranscriptItem::AssistantText(_))
        ));
    }

    #[test]
    fn turn_complete_drains_queued_message() {
        let mut app = App::new("s".into(), "m".into());
        app.mode = AppMode::Streaming;
        app.queued.push_back("queued msg".into());
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

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
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

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
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

        assert!(app.last_abort_at.is_none());
        assert!(app.status_hint.is_none());

        update(&mut app, AppAction::Abort, &uctx);

        assert!(
            app.last_abort_at.is_some(),
            "first Abort must stamp last_abort_at"
        );
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
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

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
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

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
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

        app.status_hint = Some("press Ctrl+C again to force quit".into());
        app.last_abort_at = Instant::now().checked_sub(Duration::from_secs(3));

        update(&mut app, AppAction::Tick, &uctx);
        assert!(
            app.status_hint.is_none(),
            "stale hint must be cleared on tick"
        );
    }

    #[test]
    fn tick_preserves_fresh_status_hint() {
        let mut app = App::new("s".into(), "m".into());
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

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
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

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
        assert!(
            app.pre_permission_mode.is_none(),
            "snapshot must be consumed"
        );
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
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

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
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

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
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

        let (show, _rx) = show_permission();
        update(&mut app, show, &uctx);
        update(&mut app, AppAction::PermissionAllowAlways, &uctx);
        assert_eq!(app.mode, AppMode::Input);
    }

    // ── /reload-keybindings ─────────────────────────────────────────────

    /// Task 4.2: dispatching the reload path with a modified fixture swaps
    /// the active map on `App`, pushes a success toast to the status line,
    /// and records a system notice in the transcript.
    #[test]
    fn reload_keybindings_swaps_active_map_and_shows_toast() {
        use crate::keybindings::{Chord, Keybindings};
        use std::fs;
        use tempfile::tempdir;

        let mut app = App::new("s".into(), "m".into());
        // Pre-condition: default bindings active (ctrl+q quit).
        assert_eq!(app.keybindings.quit, Chord::ctrl('q'));

        // Simulate the initial startup-cache load from a tempdir fixture.
        let dir = tempdir().unwrap();
        let path = dir.path().join("keybindings.json");
        fs::write(&path, r#"{"quit":"ctrl+q"}"#).unwrap();
        let path_for_initial = path.clone();
        app.reload_keybindings_with(move || Keybindings::try_load_from(&path_for_initial))
            .expect("initial load");

        // User edits the file — rebind quit to ctrl+x.
        fs::write(&path, r#"{"quit":"ctrl+x"}"#).unwrap();

        // Dispatch the reload (simulates `/reload-keybindings`).
        let path_for_reload = path.clone();
        reload_keybindings(&mut app, move || {
            Keybindings::try_load_from(&path_for_reload)
        });

        // New binding is live.
        assert_eq!(
            app.keybindings.quit,
            Chord::ctrl('x'),
            "reload must swap the active map"
        );
        // Toast on status line.
        assert!(
            app.status_hint
                .as_deref()
                .is_some_and(|h| h.contains("Reloaded") && h.contains("keybindings")),
            "status hint should confirm reload: {:?}",
            app.status_hint
        );
        // Transcript notice.
        assert!(
            matches!(
                app.transcript.last(),
                Some(crate::app::TranscriptItem::SystemNotice(m)) if m.contains("Reloaded")
            ),
            "transcript must show reload confirmation"
        );
    }

    /// Task 2.2: a malformed file must NOT wipe the previously-cached map;
    /// the toast must surface the parse error with the file path.
    #[test]
    fn reload_keybindings_with_malformed_file_preserves_cache_and_reports_error() {
        use crate::keybindings::{Chord, Keybindings};
        use std::fs;
        use tempfile::tempdir;

        let mut app = App::new("s".into(), "m".into());
        let dir = tempdir().unwrap();
        let path = dir.path().join("keybindings.json");

        // Seed a good map first.
        fs::write(&path, r#"{"quit":"ctrl+x"}"#).unwrap();
        let p1 = path.clone();
        app.reload_keybindings_with(move || Keybindings::try_load_from(&p1))
            .expect("good initial load");
        assert_eq!(app.keybindings.quit, Chord::ctrl('x'));

        // Corrupt the file mid-session.
        fs::write(&path, "{broken json").unwrap();

        // Reload — must fail without wiping the cached ctrl+x map.
        let p2 = path.clone();
        reload_keybindings(&mut app, move || Keybindings::try_load_from(&p2));

        assert_eq!(
            app.keybindings.quit,
            Chord::ctrl('x'),
            "malformed file must NOT wipe the cached bindings"
        );
        let hint = app.status_hint.as_deref().unwrap_or_default();
        assert!(
            hint.contains("Reload failed"),
            "status hint should report failure: {hint}"
        );
        assert!(
            hint.contains(&*path.display().to_string()) || hint.contains("parse"),
            "error should reference the file or parse context: {hint}"
        );
    }

    /// Dispatching via the public `AppAction::ReloadKeybindings` variant also
    /// invokes the real loader (reads from `~/.claude/keybindings.json`).
    /// We only check that the action runs cleanly and pushes a transcript
    /// notice — the precise content depends on the user's home directory,
    /// which we do not mutate in this unit test.
    #[test]
    fn reload_keybindings_action_runs_and_records_notice() {
        let mut app = App::new("s".into(), "m".into());
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };
        let before = app.transcript.len();
        update(&mut app, AppAction::ReloadKeybindings, &uctx);
        assert_eq!(
            app.transcript.len(),
            before + 1,
            "action must push one system notice"
        );
        match app.transcript.last() {
            Some(crate::app::TranscriptItem::SystemNotice(msg)) => {
                assert!(
                    msg.contains("Reloaded") || msg.contains("Reload failed"),
                    "notice must be a reload outcome: {msg}"
                );
            }
            other => panic!("expected SystemNotice, got {other:?}"),
        }
    }

    /// End-to-end via Submit: `/reload-keybindings` typed in the input box
    /// must dispatch the reload path (not submit the raw string to the
    /// model). We verify indirectly: `UpdateResult` is `Continue` (not
    /// `SubmitToEngine`) and a system notice is recorded.
    #[test]
    fn slash_reload_keybindings_dispatches_reload_not_submit_to_engine() {
        let mut app = App::new("s".into(), "m".into());
        app.set_input("/reload-keybindings");
        let (reg, ctx) = test_ctx();
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };
        let result = update(&mut app, AppAction::Submit, &uctx);
        assert!(
            matches!(result, UpdateResult::Continue),
            "reload must not submit the raw command to the model: {result:?}"
        );
        assert!(
            matches!(
                app.transcript.last(),
                Some(crate::app::TranscriptItem::SystemNotice(m))
                    if m.contains("Reloaded") || m.contains("Reload failed")
            ),
            "Submit path should produce a reload notice"
        );
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
        let uctx = UpdateContext {
            commands: &reg,
            command_ctx: &ctx,
        };

        update(&mut app, AppAction::PermissionDeny, &uctx);
        assert_eq!(app.mode, AppMode::Input);
    }
}
