//! Swarm Mailbox — bidirectional parent ↔ teammate communication (§8.3).

use tokio::sync::mpsc;

/// A message exchanged between parent and teammate.
#[derive(Debug, Clone)]
pub struct TeammateMessage {
    pub content: String,
    /// If true, this is a control message (e.g., cancel, pause).
    pub is_control: bool,
}

impl TeammateMessage {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_control: false,
        }
    }

    pub fn control(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_control: true,
        }
    }
}

/// Parent-side handle to the mailbox.
pub struct ParentHandle {
    pub send: mpsc::Sender<TeammateMessage>,
    pub recv: mpsc::Receiver<TeammateMessage>,
}

/// Teammate-side handle to the mailbox.
pub struct TeammateHandle {
    pub send: mpsc::Sender<TeammateMessage>,
    pub recv: mpsc::Receiver<TeammateMessage>,
}

/// Create a new mailbox pair with the given buffer size.
/// Returns (parent_handle, teammate_handle).
pub fn create_mailbox(buffer: usize) -> (ParentHandle, TeammateHandle) {
    let (parent_to_teammate_tx, parent_to_teammate_rx) = mpsc::channel(buffer);
    let (teammate_to_parent_tx, teammate_to_parent_rx) = mpsc::channel(buffer);

    let parent = ParentHandle {
        send: parent_to_teammate_tx,
        recv: teammate_to_parent_rx,
    };

    let teammate = TeammateHandle {
        send: teammate_to_parent_tx,
        recv: parent_to_teammate_rx,
    };

    (parent, teammate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mailbox_bidirectional_communication() {
        let (parent, mut teammate) = create_mailbox(8);

        // Parent sends to teammate
        parent
            .send
            .send(TeammateMessage::text("hello teammate"))
            .await
            .unwrap();

        let msg = teammate.recv.recv().await.unwrap();
        assert_eq!(msg.content, "hello teammate");
        assert!(!msg.is_control);

        // Teammate sends back to parent
        teammate
            .send
            .send(TeammateMessage::text("hello parent"))
            .await
            .unwrap();

        // parent.recv consumed in a mutable way
        let mut parent = parent;
        let msg = parent.recv.recv().await.unwrap();
        assert_eq!(msg.content, "hello parent");
    }

    #[tokio::test]
    async fn control_messages() {
        let (parent, mut teammate) = create_mailbox(8);

        parent
            .send
            .send(TeammateMessage::control("cancel"))
            .await
            .unwrap();

        let msg = teammate.recv.recv().await.unwrap();
        assert!(msg.is_control);
        assert_eq!(msg.content, "cancel");
    }
}
