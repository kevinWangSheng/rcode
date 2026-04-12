//! TeammateDirectory — registry of named in-process teammates.
//!
//! Maps teammate names to their mailbox senders and task IDs so that
//! SendMessage can route messages to running sub-agents.

use cc_core::task::TaskId;
use std::collections::HashMap;
use tokio::sync::mpsc;

/// A running teammate entry.
pub struct TeammateEntry {
    pub task_id: TaskId,
    pub inbox: mpsc::Sender<String>,
}

/// Central directory: name → TeammateEntry.
#[derive(Default)]
pub struct TeammateDirectory {
    entries: HashMap<String, TeammateEntry>,
}

impl TeammateDirectory {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Register a named teammate.
    pub fn register(&mut self, name: String, task_id: TaskId, inbox: mpsc::Sender<String>) {
        self.entries.insert(name, TeammateEntry { task_id, inbox });
    }

    /// Remove a named teammate.
    pub fn remove(&mut self, name: &str) -> Option<TeammateEntry> {
        self.entries.remove(name)
    }

    /// Get the inbox sender for a named teammate.
    pub fn get_sender(&self, name: &str) -> Option<&mpsc::Sender<String>> {
        self.entries.get(name).map(|e| &e.inbox)
    }

    /// List all registered teammate names.
    pub fn names(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    /// Check if a teammate is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_and_send() {
        let mut dir = TeammateDirectory::new();
        let (tx, mut rx) = mpsc::channel(8);
        let id = TaskId::new();
        dir.register("researcher".into(), id, tx);
        assert!(dir.contains("researcher"));

        // Send through directory
        dir.get_sender("researcher")
            .unwrap()
            .send("hello".into())
            .await
            .unwrap();
        assert_eq!(rx.recv().await.unwrap(), "hello");
    }

    #[test]
    fn remove_clears_entry() {
        let mut dir = TeammateDirectory::new();
        let (tx, _rx) = mpsc::channel(1);
        let id = TaskId::new();
        dir.register("worker".into(), id, tx);
        dir.remove("worker");
        assert!(!dir.contains("worker"));
    }
}
