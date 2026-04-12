//! Engine → TUI event type.
//!
//! Re-exports `cc_core::AppEvent` as the canonical engine event type.
//! The main loop receives these from the `QueryEngine` via an mpsc channel.

pub use cc_core::AppEvent;
