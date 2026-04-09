//! cc-agents — Task framework and task type implementations.
//!
//! Merges cc-tasks + cc-agent per Phase 2 Decision 3.
//! Provides TaskRegistry for managing background tasks,
//! and implementations for 4 task types:
//!   - local_bash: shell command in the background
//!   - local_agent: subagent with its own query engine
//!   - in_process_teammate: like local_agent but with mailbox
//!   - remote_agent: delegate to remote instance via HTTP

mod registry;
pub mod tasks;

pub use registry::{TaskOutput, TaskRegistry};
