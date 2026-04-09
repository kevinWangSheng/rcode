//! cc-tui — interactive terminal UI for `claude`.
//!
//! Placeholder — will be fully implemented in Layer 6 per Phase 2 design §7.
//! Absorbs cc-commands (slash command registry).

/// Placeholder TUI config.
pub struct TuiConfig {
    pub model: String,
}

/// Placeholder entry point.
pub async fn run_tui(_config: TuiConfig) -> cc_core::CcResult<()> {
    Err(cc_core::CcError::Other(
        "TUI not yet implemented (Phase 3 Layer 6)".into(),
    ))
}
