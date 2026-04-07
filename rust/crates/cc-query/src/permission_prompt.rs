use cc_permissions::PermissionEngine;
use serde_json::Value;
use tokio::io::AsyncWriteExt;

/// Result of an interactive permission prompt.
pub enum PromptDecision {
    /// Allow this once.
    Allow,
    /// Allow always for this tool (adds session rule).
    AllowAlways,
    /// Deny this call.
    Deny,
}

/// Show a permission prompt on stderr/stdin for headless mode.
/// Returns `Deny` on Ctrl+C (SIGINT) or if stdin is not a tty.
///
/// Behavior contract: Ctrl+C during permission prompt → `is_error: true` tool_result.
pub async fn prompt_for_permission(
    tool_name: &str,
    input: &Value,
    engine: &mut PermissionEngine,
    non_interactive: bool,
) -> PromptDecision {
    if non_interactive {
        // Non-interactive sessions auto-deny
        return PromptDecision::Deny;
    }

    // Print a brief prompt to stderr
    let preview = summarize_input(tool_name, input);
    let mut stderr = tokio::io::stderr();
    let _ = stderr
        .write_all(
            format!(
                "\n[Permission] {tool_name} wants to run: {preview}\nAllow? [y/N/a(lways)] "
            )
            .as_bytes(),
        )
        .await;
    let _ = stderr.flush().await;

    // Read one line from stdin with SIGINT handling
    let decision = read_with_sigint().await;

    match decision.trim().to_lowercase().as_str() {
        "y" | "yes" => PromptDecision::Allow,
        "a" | "always" => {
            engine.add_session_allow(tool_name);
            PromptDecision::AllowAlways
        }
        _ => PromptDecision::Deny,
    }
}

/// Read a line from stdin, returning empty string on SIGINT.
async fn read_with_sigint() -> String {
    use tokio::io::AsyncBufReadExt;

    let stdin = tokio::io::stdin();
    let mut reader = tokio::io::BufReader::new(stdin);

    tokio::select! {
        result = async {
            let mut line = String::new();
            reader.read_line(&mut line).await.ok();
            line
        } => result,
        _ = tokio::signal::ctrl_c() => {
            // Ctrl+C → deny
            String::new()
        }
    }
}

fn summarize_input(tool_name: &str, input: &Value) -> String {
    match tool_name {
        "Bash" => input["command"]
            .as_str()
            .unwrap_or("<command>")
            .chars()
            .take(80)
            .collect(),
        "Write" | "Edit" => input["file_path"]
            .as_str()
            .unwrap_or("<file>")
            .to_string(),
        _ => input.to_string().chars().take(80).collect(),
    }
}
