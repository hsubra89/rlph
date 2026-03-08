//! Task source abstraction: fetch eligible tasks from GitHub Issues or Linear.

pub mod github;
pub mod linear;

use std::collections::HashSet;
use std::thread;
use std::time::Duration;

use tracing::warn;

use crate::error::Result;
use crate::ids::IssueNumber;
use crate::task::Task;

const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 500;

pub(crate) fn retry_with_backoff<F, T>(f: F) -> Result<T>
where
    F: Fn() -> Result<T>,
{
    retry_with_backoff_ms(f, INITIAL_BACKOFF_MS, MAX_RETRIES)
}

fn retry_with_backoff_ms<F, T>(f: F, initial_backoff_ms: u64, max_retries: u32) -> Result<T>
where
    F: Fn() -> Result<T>,
{
    let mut backoff_ms = initial_backoff_ms;

    for attempt in 1..=max_retries {
        match f() {
            Ok(val) => return Ok(val),
            Err(e) if attempt < max_retries => {
                warn!(attempt, error = %e, backoff_ms, "retrying after transient error");
                thread::sleep(Duration::from_millis(backoff_ms));
                backoff_ms *= 2;
            }
            Err(e) => return Err(e),
        }
    }

    // max_retries == 0: no loop iterations, so make a single attempt
    f()
}

pub trait TaskSource {
    /// Fetch tasks matching the label filter, excluding blocked ones.
    fn fetch_eligible_tasks(&self) -> Result<Vec<Task>>;

    /// Mark a task as in-progress in the remote system.
    fn mark_in_progress(&self, task_id: &str) -> Result<()>;

    /// Mark a task as in-review in the remote system.
    fn mark_in_review(&self, task_id: &str) -> Result<()>;

    /// Get full details for a task.
    fn get_task_details(&self, task_id: &str) -> Result<Task>;

    /// Fetch IDs of closed/done tasks (used for dependency resolution).
    fn fetch_closed_task_ids(&self) -> Result<HashSet<IssueNumber>>;
}

pub enum AnySource {
    GitHub(github::GitHubSource),
    Linear(linear::LinearSource),
}

impl TaskSource for AnySource {
    fn fetch_eligible_tasks(&self) -> Result<Vec<Task>> {
        match self {
            AnySource::GitHub(s) => s.fetch_eligible_tasks(),
            AnySource::Linear(s) => s.fetch_eligible_tasks(),
        }
    }

    fn mark_in_progress(&self, task_id: &str) -> Result<()> {
        match self {
            AnySource::GitHub(s) => s.mark_in_progress(task_id),
            AnySource::Linear(s) => s.mark_in_progress(task_id),
        }
    }

    fn mark_in_review(&self, task_id: &str) -> Result<()> {
        match self {
            AnySource::GitHub(s) => s.mark_in_review(task_id),
            AnySource::Linear(s) => s.mark_in_review(task_id),
        }
    }

    fn get_task_details(&self, task_id: &str) -> Result<Task> {
        match self {
            AnySource::GitHub(s) => s.get_task_details(task_id),
            AnySource::Linear(s) => s.get_task_details(task_id),
        }
    }

    fn fetch_closed_task_ids(&self) -> Result<HashSet<IssueNumber>> {
        match self {
            AnySource::GitHub(s) => s.fetch_closed_task_ids(),
            AnySource::Linear(s) => s.fetch_closed_task_ids(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    use crate::error::Error;

    #[test]
    fn test_retry_succeeds_after_transient_failure() {
        let attempts = RefCell::new(0);
        let result = retry_with_backoff_ms(
            || {
                let mut a = attempts.borrow_mut();
                *a += 1;
                if *a < 3 {
                    Err(Error::TaskSource("transient".to_string()))
                } else {
                    Ok("success".to_string())
                }
            },
            1,
            3,
        );
        assert_eq!(result.unwrap(), "success");
        assert_eq!(*attempts.borrow(), 3);
    }

    #[test]
    fn test_retry_fails_after_max_attempts() {
        let result: Result<String> =
            retry_with_backoff_ms(|| Err(Error::TaskSource("permanent".to_string())), 1, 3);
        assert!(result.is_err());
    }
}
