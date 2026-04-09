use serde_json::Value;
use tokio::io::AsyncWriteExt;

/// Result of an interactive permission prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptDecision {
    /// Allow this once.
    Allow,
    /// Allow always for this tool (engine adds session rule).
    AllowAlways,
    /// Deny this call.
    Deny,
}

/// Stdin/stderr prompt — used by the headless `StdinPrompter`.
///
/// Returns `Deny` on Ctrl+C (SIGINT) or in non-interactive mode. The engine
/// is responsible for upgrading `AllowAlways` into a session permission rule.
pub async fn stdin_prompt(
    tool_name: &str,
    input: &Value,
    non_interactive: bool,
) -> PromptDecision {
    if non_interactive {
        return PromptDecision::Deny;
    }

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

    let line = read_with_sigint().await;

    match line.trim().to_lowercase().as_str() {
        "y" | "yes" => PromptDecision::Allow,
        "a" | "always" => PromptDecision::AllowAlways,
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
            String::new()
        }
    }
}

/// Format a one-line summary of the tool input for the permission prompt.
pub fn summarize_input(tool_name: &str, input: &Value) -> String {
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
