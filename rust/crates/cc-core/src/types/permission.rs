use serde::{Deserialize, Serialize};

/// The outcome of a permission check for a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionBehavior {
    Allow,
    Deny,
    Ask,
}

/// Result of evaluating permission rules for a tool invocation.
#[derive(Debug, Clone)]
pub struct PermissionResult {
    pub behavior: PermissionBehavior,
    pub reason: Option<String>,
}

impl PermissionResult {
    pub fn allow() -> Self {
        PermissionResult {
            behavior: PermissionBehavior::Allow,
            reason: None,
        }
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        PermissionResult {
            behavior: PermissionBehavior::Deny,
            reason: Some(reason.into()),
        }
    }
}
