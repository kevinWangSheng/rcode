/// Analytics is a no-op in the initial Rust implementation.
/// All functions are stubs that compile away to nothing in release mode.
/// Log an analytics event. Currently a no-op.
#[inline(always)]
pub fn log_event(_event: &str, _properties: Option<&serde_json::Value>) {
    // no-op: analytics disabled initially
}

/// Check whether analytics is enabled.
#[inline(always)]
pub fn is_analytics_enabled() -> bool {
    false
}
