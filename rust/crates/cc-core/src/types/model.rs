/// Well-known model identifiers.
pub mod models {
    pub const CLAUDE_OPUS_4_6: &str = "claude-opus-4-6";
    pub const CLAUDE_SONNET_4_6: &str = "claude-sonnet-4-6";
    pub const CLAUDE_HAIKU_4_5: &str = "claude-haiku-4-5-20251001";

    /// Default model used when none is specified.
    pub const DEFAULT: &str = CLAUDE_SONNET_4_6;
}
