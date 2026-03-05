pub mod cli;
pub mod config;
pub mod deps;
pub mod diff_position_mapper;
pub mod error;
pub mod fix;
pub mod fix_comment;
pub mod fix_deps;
pub mod fix_scheduler;
pub mod orchestrator;
pub mod prd;
pub mod process;
pub mod prompts;
pub(crate) mod resolve_threads;
pub mod review_schema;
pub mod runner;
pub(crate) mod scc;
pub mod sources;
pub mod state;
pub mod submission;
pub mod worktree;

/// Max parallel `gh api` calls per batch to avoid exhausting file descriptors.
pub(crate) const GH_BATCH_SIZE: usize = 10;

#[doc(hidden)]
pub mod test_helpers;
