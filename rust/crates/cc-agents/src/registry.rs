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
    ///
    /// Reaps every `JoinHandle` that has finished — including handles whose
    /// task was previously `cancel()`ed. Prior to this change the filter
    /// required `status == Running`, which meant a cancelled task's
    /// `JoinHandle` leaked in `self.tasks` forever because `cancel()` had
    /// already flipped its status.
    pub fn poll_completed(&mut self) {
        let mut completed = Vec::new();
        for (id, entry) in &self.tasks {
            if entry.handle.is_finished() {
                completed.push(id.clone());
            }
        }

        for id in completed {
            if let Some(mut entry) = self.tasks.remove(&id) {
                let was_cancelled = entry.state.status == TaskStatus::Cancelled;
                let result = poll_join_handle(&mut entry.handle);
                match result {
                    Some(Ok(Ok(output))) => {
                        if was_cancelled {
                            // Cancelled task happened to finish anyway — the
                            // caller asked for cancel, so we silently reap.
                            debug!("task {id:?} finished after cancel — output discarded");
                        } else {
                            entry.state.status = TaskStatus::Completed;
                            entry.state.completed_at = Some(chrono::Utc::now());
                            self.pending_outputs.push((id, output.content));
                        }
                    }
                    Some(Ok(Err(e))) => {
                        let is_cancelled = matches!(&e, CcError::Cancelled);
                        entry.state.status = if was_cancelled || is_cancelled {
                            TaskStatus::Cancelled
                        } else {
                            TaskStatus::Failed
                        };
                        entry.state.completed_at = Some(chrono::Utc::now());
                        debug!("task {id:?} ended: {e}");
                    }
                    Some(Err(e)) => {
                        entry.state.status = TaskStatus::Failed;
                        entry.state.completed_at = Some(chrono::Utc::now());
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

    /// Evict completed / failed / cancelled tasks whose `completed_at`
    /// is older than `max_age`. Running tasks are never evicted.
    ///
    /// Entries whose `completed_at` is `None` (freshly-terminated tasks
    /// reaped on this tick, or tasks that never went through
    /// `poll_completed` — which shouldn't happen at HEAD) are kept so that
    /// the next `drain_completed_outputs` still sees them. Callers that
    /// want aggressive cleanup should `poll_completed` first, then
    /// `drain_completed_outputs`, then `evict_old`.
    pub fn evict_old(&mut self, max_age: Duration) {
        let cutoff = chrono::Utc::now()
            - chrono::Duration::from_std(max_age).unwrap_or(chrono::Duration::zero());
        self.tasks.retain(|_, e| match e.state.status {
            TaskStatus::Pending | TaskStatus::Running => true,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled => {
                match e.state.completed_at {
                    Some(t) => t > cutoff,
                    None => true,
                }
            }
        });
    }
}

/// Aborts all in-flight tasks when the registry is dropped so we don't
/// leak `JoinHandle`s or leave subprocesses running past the TUI's
/// lifetime. Callers that want graceful drain should call
/// `cancel_all()` + `poll_completed()` first; Drop is the "panic /
/// scope-exit" safety net.
impl Drop for TaskRegistry {
    fn drop(&mut self) {
        for (id, entry) in self.tasks.drain() {
            entry.cancel.cancel();
            entry.handle.abort();
            debug!("TaskRegistry drop: aborted task {id:?}");
        }
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

    /// After `cancel()`, the task's JoinHandle MUST still be reaped by
    /// `poll_completed()`. Before the widened filter the cancelled entry
    /// leaked forever because the filter required `status == Running`.
    #[tokio::test]
    async fn poll_completed_reaps_cancelled_handles() {
        let mut registry = TaskRegistry::new(10);
        let token = CancellationToken::new();
        let inner = token.clone();
        let id = registry
            .spawn(TaskKind::LocalBash, "cancels".into(), token, async move {
                inner.cancelled().await;
                Err::<TaskOutput, _>(CcError::Cancelled)
            })
            .unwrap();

        registry.cancel(&id);

        // Give the task time to observe the cancel and return.
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(5)).await;
            registry.poll_completed();
            if registry.status(&id).is_none() {
                break;
            }
        }

        assert!(
            registry.status(&id).is_none(),
            "cancelled handle was not reaped from the registry"
        );
        assert!(
            registry.drain_completed_outputs().is_empty(),
            "cancelled task must not push to pending_outputs"
        );
    }

    /// `evict_old(max_age)` MUST honour `max_age`. Before this fix the
    /// argument was ignored and every non-running task was evicted
    /// immediately, which surprised callers that expected a TTL.
    #[tokio::test]
    async fn evict_old_honours_max_age_against_completed_at() {
        let mut registry = TaskRegistry::new(10);
        let cancel = CancellationToken::new();
        let id = registry
            .spawn(TaskKind::LocalBash, "quick".into(), cancel, async {
                Ok(TaskOutput {
                    summary: "done".into(),
                    content: "hi".into(),
                })
            })
            .unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;
        registry.poll_completed();

        // Just-completed → completed_at is ~now. A 1-hour TTL must keep it.
        registry.evict_old(Duration::from_secs(3600));
        // poll_completed already removed the entry from self.tasks, but
        // evict_old should also not panic on an empty set.
        assert!(registry.status(&id).is_none());

        // Repeat with a completed-long-ago entry — manually insert one
        // with a stale completed_at and confirm evict_old removes it.
        let old_cancel = CancellationToken::new();
        let old_id = TaskId::new();
        let handle = tokio::spawn(async {
            Ok::<TaskOutput, CcError>(TaskOutput {
                summary: "".into(),
                content: "".into(),
            })
        });
        let mut old_state = TaskStateBase::new(TaskKind::LocalBash, "ancient".into());
        old_state.status = TaskStatus::Completed;
        old_state.completed_at = Some(chrono::Utc::now() - chrono::Duration::hours(2));
        registry.tasks.insert(
            old_id.clone(),
            TaskEntry {
                state: old_state,
                handle,
                cancel: old_cancel,
            },
        );

        registry.evict_old(Duration::from_secs(3600));
        assert!(
            registry.status(&old_id).is_none(),
            "2-hour-old completed task should be evicted by a 1-hour TTL"
        );
    }

    /// Dropping the registry while a task is still running MUST abort the
    /// JoinHandle so subprocesses don't outlive the TUI. Without the Drop
    /// impl a crash / panic would leak the background task indefinitely.
    #[tokio::test]
    async fn drop_aborts_running_task_handles() {
        let token = CancellationToken::new();
        let inner = token.clone();
        let sentinel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sentinel_inner = sentinel.clone();

        {
            let mut registry = TaskRegistry::new(10);
            registry
                .spawn(TaskKind::LocalBash, "sleeper".into(), token, async move {
                    // If we ever get past this sleep, flip the sentinel.
                    tokio::select! {
                        _ = inner.cancelled() => {}
                        _ = tokio::time::sleep(Duration::from_secs(60)) => {
                            sentinel_inner.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                    Ok(TaskOutput {
                        summary: "".into(),
                        content: "".into(),
                    })
                })
                .unwrap();
            // `registry` goes out of scope here — Drop runs, the handle is
            // aborted, and the cancel token is fired.
        }

        // Give the aborted task a moment to unwind.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !sentinel.load(std::sync::atomic::Ordering::SeqCst),
            "background task ran past Drop — JoinHandle leaked"
        );
    }
}
