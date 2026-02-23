use std::collections::HashSet;
use std::process::Command;
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use tracing::{debug, warn};

use crate::config::Config;
use crate::error::{Error, Result};

use super::{Priority, Task, TaskSource};

const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 500;

#[derive(Debug, Deserialize)]
struct GhLabel {
    name: String,
}

#[derive(Debug, Deserialize)]
struct GhIssue {
    number: u64,
    title: String,
    body: Option<String>,
    labels: Vec<GhLabel>,
    url: String,
}

/// Abstraction over `gh` CLI execution for testability.
pub trait GhClient {
    fn run(&self, args: &[&str]) -> Result<String>;
}

/// Real `gh` CLI client with retry and exponential backoff.
struct DefaultGhClient;

impl GhClient for DefaultGhClient {
    fn run(&self, args: &[&str]) -> Result<String> {
        retry_with_backoff(|| {
            let output = Command::new("gh")
                .args(args)
                .output()
                .map_err(|e| Error::TaskSource(format!("failed to run gh: {e}")))?;

            if output.status.success() {
                String::from_utf8(output.stdout)
                    .map_err(|e| Error::TaskSource(format!("invalid utf8 from gh: {e}")))
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(Error::TaskSource(format!("gh failed: {stderr}")))
            }
        })
    }
}

pub struct GitHubSource {
    label: String,
    client: Box<dyn GhClient>,
}

impl GitHubSource {
    pub fn new(config: &Config) -> Self {
        Self {
            label: config.label.clone(),
            client: Box::new(DefaultGhClient),
        }
    }

    #[cfg(test)]
    fn with_client(label: &str, client: Box<dyn GhClient>) -> Self {
        Self {
            label: label.to_string(),
            client,
        }
    }

    fn parse_issue(gh: GhIssue) -> Task {
        let labels: Vec<String> = gh.labels.iter().map(|l| l.name.clone()).collect();
        let priority = labels.iter().find_map(|l| Priority::from_label(l));
        Task {
            id: gh.number.to_string(),
            title: gh.title,
            body: gh.body.unwrap_or_default(),
            labels,
            url: gh.url,
            priority,
        }
    }

    fn is_eligible(issue: &GhIssue) -> bool {
        !issue.labels.iter().any(|l| {
            l.name.eq_ignore_ascii_case("in-progress")
                || l.name.eq_ignore_ascii_case("in-review")
                || l.name.eq_ignore_ascii_case("done")
        })
    }
}

impl TaskSource for GitHubSource {
    fn fetch_eligible_tasks(&self) -> Result<Vec<Task>> {
        let json = self.client.run(&[
            "issue",
            "list",
            "--label",
            &self.label,
            "--state",
            "open",
            "--json",
            "number,title,body,labels,url",
            "--limit",
            "100",
        ])?;

        let issues: Vec<GhIssue> = serde_json::from_str(&json)
            .map_err(|e| Error::TaskSource(format!("failed to parse gh output: {e}")))?;

        let tasks: Vec<Task> = issues
            .into_iter()
            .filter(Self::is_eligible)
            .map(Self::parse_issue)
            .collect();

        debug!(count = tasks.len(), "fetched eligible tasks");
        Ok(tasks)
    }

    fn mark_in_progress(&self, task_id: &str) -> Result<()> {
        if let Err(e) = self.client.run(&["issue", "reopen", task_id]) {
            warn!(task_id, error = %e, "failed to reopen issue");
        }
        if let Err(e) = self.client.run(&[
            "issue",
            "edit",
            task_id,
            "--add-label",
            "in-progress",
            "--remove-label",
            "in-review",
        ]) {
            warn!(task_id, error = %e, "failed to update labels for in-progress");
        }
        debug!(task_id, "marked in-progress");
        Ok(())
    }

    fn mark_in_review(&self, task_id: &str) -> Result<()> {
        if let Err(e) = self.client.run(&[
            "issue",
            "edit",
            task_id,
            "--add-label",
            "in-review",
            "--remove-label",
            "in-progress",
        ]) {
            warn!(task_id, error = %e, "failed to update labels for in-review");
        }
        debug!(task_id, "marked in-review");
        Ok(())
    }

    fn fetch_closed_task_ids(&self) -> Result<HashSet<u64>> {
        let json = self.client.run(&[
            "issue", "list", "--state", "closed", "--json", "number", "--limit", "200",
        ])?;

        #[derive(Deserialize)]
        struct Num {
            number: u64,
        }

        let nums: Vec<Num> = serde_json::from_str(&json)
            .map_err(|e| Error::TaskSource(format!("failed to parse closed issues: {e}")))?;

        let ids = nums.into_iter().map(|n| n.number).collect();
        debug!(?ids, "fetched closed task ids");
        Ok(ids)
    }

    fn get_task_details(&self, task_id: &str) -> Result<Task> {
        let json = self.client.run(&[
            "issue",
            "view",
            task_id,
            "--json",
            "number,title,body,labels,url",
        ])?;

        let issue: GhIssue = serde_json::from_str(&json)
            .map_err(|e| Error::TaskSource(format!("failed to parse gh output: {e}")))?;

        Ok(Self::parse_issue(issue))
    }
}

fn retry_with_backoff<F, T>(f: F) -> Result<T>
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

    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct MockGhClient {
        responses: RefCell<Vec<Result<String>>>,
    }

    impl MockGhClient {
        fn new(responses: Vec<Result<String>>) -> Self {
            Self {
                responses: RefCell::new(responses),
            }
        }
    }

    impl GhClient for MockGhClient {
        fn run(&self, _args: &[&str]) -> Result<String> {
            let mut responses = self.responses.borrow_mut();
            if responses.is_empty() {
                Err(Error::TaskSource("no more mock responses".to_string()))
            } else {
                responses.remove(0)
            }
        }
    }

    fn mock_issues_json(issues: &[serde_json::Value]) -> String {
        serde_json::to_string(issues).unwrap()
    }

    fn issue_json(number: u64, title: &str, labels: &[&str], body: &str) -> serde_json::Value {
        serde_json::json!({
            "number": number,
            "title": title,
            "body": body,
            "labels": labels.iter().map(|l| serde_json::json!({"name": l})).collect::<Vec<_>>(),
            "url": format!("https://github.com/test/repo/issues/{number}")
        })
    }

    #[test]
    fn test_fetch_filters_eligible_only() {
        let json = mock_issues_json(&[
            issue_json(1, "Task 1", &["rlph"], "body 1"),
            issue_json(2, "Task 2", &["rlph", "in-progress"], "body 2"),
            issue_json(3, "Task 3", &["rlph", "done"], "body 3"),
            issue_json(4, "Task 4", &["rlph"], "body 4"),
        ]);
        let client = MockGhClient::new(vec![Ok(json)]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        let tasks = source.fetch_eligible_tasks().unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "1");
        assert_eq!(tasks[1].id, "4");
    }

    #[test]
    fn test_fetch_excludes_in_review() {
        let json = mock_issues_json(&[
            issue_json(1, "Task 1", &["rlph"], "body"),
            issue_json(2, "Task 2", &["rlph", "in-review"], "body"),
        ]);
        let client = MockGhClient::new(vec![Ok(json)]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        let tasks = source.fetch_eligible_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "1");
    }

    #[test]
    fn test_fetch_parses_priority() {
        let json = mock_issues_json(&[
            issue_json(1, "High pri", &["rlph", "p1"], "body"),
            issue_json(2, "Low pri", &["rlph", "priority-low"], "body"),
            issue_json(3, "No pri", &["rlph"], "body"),
        ]);
        let client = MockGhClient::new(vec![Ok(json)]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        let tasks = source.fetch_eligible_tasks().unwrap();
        assert_eq!(tasks[0].priority, Some(Priority(1)));
        assert_eq!(tasks[1].priority, Some(Priority(9)));
        assert_eq!(tasks[2].priority, None);
    }

    #[test]
    fn test_mark_in_progress() {
        let client = MockGhClient::new(vec![Ok(String::new()), Ok(String::new())]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        source.mark_in_progress("42").unwrap();
    }

    #[test]
    fn test_get_task_details() {
        let json = serde_json::to_string(&issue_json(
            7,
            "Detail task",
            &["rlph", "todo", "p3"],
            "task body",
        ))
        .unwrap();
        let client = MockGhClient::new(vec![Ok(json)]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        let task = source.get_task_details("7").unwrap();
        assert_eq!(task.id, "7");
        assert_eq!(task.title, "Detail task");
        assert_eq!(task.body, "task body");
        assert_eq!(task.priority, Some(Priority(3)));
    }

    #[test]
    fn test_fetch_includes_issues_without_active_labels() {
        let json = mock_issues_json(&[
            issue_json(1, "Just rlph", &["rlph"], "body"),
            issue_json(2, "Extra label", &["rlph", "bug"], "body"),
            issue_json(3, "No labels", &[], "body"),
        ]);
        let client = MockGhClient::new(vec![Ok(json)]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        let tasks = source.fetch_eligible_tasks().unwrap();
        assert_eq!(tasks.len(), 3);
    }

    #[test]
    fn test_fetch_excludes_mixed_case_active_labels() {
        let json = mock_issues_json(&[
            issue_json(1, "In progress", &["rlph", "In-Progress"], "body"),
            issue_json(2, "In review", &["rlph", "IN-REVIEW"], "body"),
            issue_json(3, "Done", &["rlph", "Done"], "body"),
            issue_json(4, "Eligible", &["rlph"], "body"),
        ]);
        let client = MockGhClient::new(vec![Ok(json)]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        let tasks = source.fetch_eligible_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "4");
    }

    #[test]
    fn test_fetch_error_propagated() {
        let client = MockGhClient::new(vec![Err(Error::TaskSource("gh not found".to_string()))]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        let err = source.fetch_eligible_tasks().unwrap_err();
        assert!(err.to_string().contains("gh not found"));
    }

    #[test]
    fn test_fetch_handles_null_body() {
        let json = r#"[{"number":1,"title":"No body","body":null,"labels":[{"name":"todo"}],"url":"https://example.com/1"}]"#;
        let client = MockGhClient::new(vec![Ok(json.to_string())]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        let tasks = source.fetch_eligible_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].body, "");
    }

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
