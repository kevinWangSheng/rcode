//! TaskRegistry — central registry for all running/completed tasks.

use cc_core::task::{TaskId, TaskKind, TaskStateBase, TaskStatus};
use cc_core::{CcError, CcResult};
use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// Output produced by a completed task.
#[derive(Debug, Clone)]
pub struct TaskOutput {
    pub summary: String,
    pub content: String,
}

/// Internal entry tracking a single task.
struct TaskEntry {
    state: TaskStateBase,
    handle: JoinHandle<CcResult<TaskOutput>>,
    cancel: CancellationToken,
}

/// Central registry for all running/completed tasks.
pub struct TaskRegistry {
    tasks: HashMap<TaskId, TaskEntry>,
    /// Completed task outputs waiting to be attached to next user message.
    pending_outputs: Vec<(TaskId, String)>,
    /// Maximum concurrent tasks.
    max_concurrent: usize,
}

impl TaskRegistry {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            tasks: HashMap::new(),
            pending_outputs: Vec::new(),
            max_concurrent,
        }
    }

    /// Spawn a new background task.
    pub fn spawn(
        &mut self,
        kind: TaskKind,
        description: String,
        cancel: CancellationToken,
        future: impl Future<Output = CcResult<TaskOutput>> + Send + 'static,
    ) -> CcResult<TaskId> {
        if self.running_count() >= self.max_concurrent {
            return Err(CcError::Other(format!(
                "max concurrent tasks reached ({})",
                self.max_concurrent
            )));
        }

        let id = TaskId::new();
        let state = TaskStateBase::new(kind, description);
        let handle = tokio::spawn(future);

        self.tasks.insert(
            id.clone(),
            TaskEntry {
                state,
                handle,
                cancel,
            },
        );

        debug!("spawned task {id:?}");
        Ok(id)
    }

    /// Cancel a specific task.
    pub fn cancel(&mut self, id: &TaskId) {
        if let Some(entry) = self.tasks.get_mut(id) {
            entry.cancel.cancel();
            entry.state.status = TaskStatus::Cancelled;
            debug!("cancelled task {id:?}");
        }
    }

    /// Cancel all running tasks.
    pub fn cancel_all(&mut self) {
        let ids: Vec<TaskId> = self.tasks.keys().cloned().collect();
        for id in ids {
            self.cancel(&id);
        }
    }

    /// Get the status of a task.
    pub fn status(&self, id: &TaskId) -> Option<&TaskStateBase> {
        self.tasks.get(id).map(|e| &e.state)
    }

    /// Count of currently running tasks.
    pub fn running_count(&self) -> usize {
        self.tasks
            .values()
            .filter(|e| e.state.status == TaskStatus::Running)
            .count()
    }

    /// Poll completed tasks and collect their outputs.
    pub fn poll_completed(&mut self) {
        let mut completed = Vec::new();
        for (id, entry) in &self.tasks {
            if entry.handle.is_finished() && entry.state.status == TaskStatus::Running {
                completed.push(id.clone());
            }
        }

        for id in completed {
            if let Some(mut entry) = self.tasks.remove(&id) {
                // Poll the handle to get the result without blocking
                let result = poll_join_handle(&mut entry.handle);
                match result {
                    Some(Ok(Ok(output))) => {
                        entry.state.status = TaskStatus::Completed;
                        self.pending_outputs.push((id, output.content));
                    }
                    Some(Ok(Err(e))) => {
                        entry.state.status = TaskStatus::Failed;
                        debug!("task {id:?} failed: {e}");
                    }
                    Some(Err(e)) => {
                        entry.state.status = TaskStatus::Failed;
                        debug!("task {id:?} panicked: {e}");
                    }
                    None => {
                        // Not actually finished — put it back
                        self.tasks.insert(id, entry);
                    }
                }
            }
        }
    }

    /// Drain completed task outputs (attach to next user message).
    pub fn drain_completed_outputs(&mut self) -> Vec<(TaskId, String)> {
        std::mem::take(&mut self.pending_outputs)
    }

    /// Evict completed/failed tasks older than `max_age`.
    pub fn evict_old(&mut self, _max_age: Duration) {
        // For now, evict all non-running tasks
        self.tasks
            .retain(|_, e| e.state.status == TaskStatus::Running);
    }
}

/// Poll a JoinHandle without blocking. Returns Some if the task is done.
fn poll_join_handle<T>(handle: &mut JoinHandle<T>) -> Option<Result<T, tokio::task::JoinError>> {
    use std::task::{Context, Poll};
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    match std::pin::Pin::new(handle).poll(&mut cx) {
        Poll::Ready(result) => Some(result),
        Poll::Pending => None,
    }
}

fn noop_waker() -> std::task::Waker {
    use std::task::{RawWaker, RawWakerVTable};
    fn noop(_: *const ()) {}
    fn clone(p: *const ()) -> RawWaker {
        RawWaker::new(p, &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    // SAFETY: The vtable functions are all no-ops and the data pointer is null.
    unsafe { std::task::Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_and_poll() {
        let mut registry = TaskRegistry::new(10);
        let cancel = CancellationToken::new();
        let id = registry
            .spawn(TaskKind::LocalBash, "test task".into(), cancel, async {
                Ok(TaskOutput {
                    summary: "done".into(),
                    content: "output".into(),
                })
            })
            .unwrap();

        // Give the task time to complete
        tokio::time::sleep(Duration::from_millis(10)).await;

        registry.poll_completed();
        let outputs = registry.drain_completed_outputs();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].0, id);
        assert_eq!(outputs[0].1, "output");
    }

    #[tokio::test]
    async fn max_concurrent_enforced() {
        let mut registry = TaskRegistry::new(1);
        let cancel1 = CancellationToken::new();
        let cancel2 = CancellationToken::new();

        registry
            .spawn(TaskKind::LocalBash, "task1".into(), cancel1, async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(TaskOutput {
                    summary: "".into(),
                    content: "".into(),
                })
            })
            .unwrap();

        let result = registry.spawn(TaskKind::LocalBash, "task2".into(), cancel2, async {
            Ok(TaskOutput {
                summary: "".into(),
                content: "".into(),
            })
        });
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn cancel_task() {
        let mut registry = TaskRegistry::new(10);
        let cancel = CancellationToken::new();
        let id = registry
            .spawn(TaskKind::LocalBash, "test".into(), cancel, async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(TaskOutput {
                    summary: "".into(),
                    content: "".into(),
                })
            })
            .unwrap();

        registry.cancel(&id);
        let status = registry.status(&id).unwrap();
        assert_eq!(status.status, TaskStatus::Cancelled);
    }
}
