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

/// Run `f` on each element of `items` in parallel batches of [`GH_BATCH_SIZE`],
/// collecting results in order.
pub(crate) fn run_batched<T, R, F>(items: &[T], f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    let mut results = Vec::with_capacity(items.len());
    for chunk in items.chunks(GH_BATCH_SIZE) {
        std::thread::scope(|s| {
            let handles: Vec<_> = chunk.iter().map(|item| s.spawn(|| f(item))).collect();
            for handle in handles {
                results.push(handle.join().expect("batched thread panicked"));
            }
        });
    }
    results
}

#[doc(hidden)]
pub mod test_helpers;
