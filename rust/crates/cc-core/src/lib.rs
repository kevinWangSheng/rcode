pub mod agent;
pub mod error;
pub mod file_history;
pub mod hook;
pub mod message;
pub mod model;
pub mod permission;
pub mod state;
pub mod summarizer;
pub mod task;
pub mod tool;

pub use agent::SubAgentRunner;
pub use error::{CcError, CcResult};
pub use file_history::*;
pub use hook::*;
pub use message::*;
pub use model::*;
pub use permission::*;
pub use state::*;
pub use summarizer::Summarizer;
pub use task::*;
pub use tool::*;

/// Analytics no-op stub (cc-analytics absorbed per Decision 3).
pub mod analytics {
    #[inline(always)]
    pub fn track_event(_name: &str, _properties: &serde_json::Value) {}

    #[inline(always)]
    pub fn track_error(_error: &str) {}
}
