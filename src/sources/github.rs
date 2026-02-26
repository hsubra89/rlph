use std::collections::HashSet;
use std::process::Command;
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use tracing::{debug, warn};

use crate::config::Config;
use crate::error::{Error, Result};

use super::{Priority, Task, TaskGroup, TaskSource};
use crate::deps;

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

    /// Run a GraphQL query via `gh api graphql`.
    fn graphql(&self, query: &str, variables: &[(&str, &str)], headers: &[&str]) -> Result<String> {
        let query_arg = format!("query={query}");
        let mut owned: Vec<String> = vec!["api".into(), "graphql".into(), "-f".into(), query_arg];
        for (key, value) in variables {
            owned.push("-f".into());
            owned.push(format!("{key}={value}"));
        }
        for header in headers {
            owned.push("-H".into());
            owned.push((*header).into());
        }
        let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
        self.run(&refs)
    }
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

    fn is_gql_eligible(issue: &GqlIssue) -> bool {
        !issue.labels.nodes.iter().any(|l| {
            l.name.eq_ignore_ascii_case("in-progress")
                || l.name.eq_ignore_ascii_case("in-review")
                || l.name.eq_ignore_ascii_case("done")
        })
    }

    fn parse_gql_issue(issue: &GqlIssue) -> Task {
        let labels: Vec<String> = issue.labels.nodes.iter().map(|l| l.name.clone()).collect();
        let priority = labels.iter().find_map(|l| Priority::from_label(l));
        Task {
            id: issue.number.to_string(),
            title: issue.title.clone(),
            body: issue.body.clone().unwrap_or_default(),
            labels,
            url: issue.url.clone(),
            priority,
        }
    }

    fn fetch_repo_nwo(&self) -> Result<(String, String)> {
        let json = self.client.run(&["repo", "view", "--json", "owner,name"])?;
        let info: RepoInfo = serde_json::from_str(&json)
            .map_err(|e| Error::TaskSource(format!("failed to parse repo info: {e}")))?;
        Ok((info.owner.login, info.name))
    }

    pub fn fetch_eligible_task_groups_impl(&self) -> Result<Vec<TaskGroup>> {
        let (owner, name) = self.fetch_repo_nwo()?;

        let query = r#"
            query($owner: String!, $name: String!, $label: String!) {
              repository(owner: $owner, name: $name) {
                issues(labels: [$label], states: [OPEN], first: 100) {
                  nodes {
                    number title body url
                    labels(first: 20) { nodes { name } }
                    subIssues(first: 50) {
                      nodes {
                        number title body url state
                        labels(first: 20) { nodes { name } }
                      }
                    }
                  }
                }
              }
            }
        "#;

        let response = self.client.graphql(
            query,
            &[("owner", &owner), ("name", &name), ("label", &self.label)],
            &["GraphQL-Features: sub_issues"],
        )?;

        let parsed: GqlResponse = serde_json::from_str(&response)
            .map_err(|e| Error::TaskSource(format!("failed to parse GraphQL response: {e}")))?;

        let issues = parsed.data.repository.issues.nodes;

        // Collect IDs of sub-issues that belong to a labeled parent
        let mut child_ids: HashSet<u64> = HashSet::new();
        for issue in &issues {
            let labeled_children: Vec<u64> = issue
                .sub_issues
                .nodes
                .iter()
                .filter(|si| si.state == "OPEN")
                .filter(|si| si.labels.nodes.iter().any(|l| l.name == self.label))
                .map(|si| si.number)
                .collect();
            if !labeled_children.is_empty() {
                child_ids.extend(labeled_children);
            }
        }

        let mut groups: Vec<TaskGroup> = Vec::new();

        for issue in &issues {
            // Skip children — they appear inside their parent's group
            if child_ids.contains(&issue.number) {
                continue;
            }
            if !Self::is_gql_eligible(issue) {
                continue;
            }

            let labeled_sub: Vec<&GqlSubIssue> = issue
                .sub_issues
                .nodes
                .iter()
                .filter(|si| si.state == "OPEN")
                .filter(|si| si.labels.nodes.iter().any(|l| l.name == self.label))
                .collect();

            if labeled_sub.is_empty() {
                groups.push(TaskGroup::Standalone(Self::parse_gql_issue(issue)));
            } else {
                let parent = Self::parse_gql_issue(issue);
                let sub_tasks: Vec<Task> = labeled_sub
                    .iter()
                    .map(|si| Self::parse_gql_sub(si))
                    .collect();
                let sorted = deps::topological_sort_within_group(sub_tasks);
                groups.push(TaskGroup::Group {
                    parent,
                    sub_issues: sorted,
                });
            }
        }

        debug!(count = groups.len(), "fetched eligible task groups");
        Ok(groups)
    }

    fn parse_gql_sub(si: &GqlSubIssue) -> Task {
        let labels: Vec<String> = si.labels.nodes.iter().map(|l| l.name.clone()).collect();
        let priority = labels.iter().find_map(|l| Priority::from_label(l));
        Task {
            id: si.number.to_string(),
            title: si.title.clone(),
            body: si.body.clone().unwrap_or_default(),
            labels,
            url: si.url.clone(),
            priority,
        }
    }
}

// --- GraphQL response types ---

#[derive(Debug, Deserialize)]
struct RepoInfo {
    name: String,
    owner: RepoOwner,
}

#[derive(Debug, Deserialize)]
struct RepoOwner {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GqlResponse {
    data: GqlData,
}

#[derive(Debug, Deserialize)]
struct GqlData {
    repository: GqlRepository,
}

#[derive(Debug, Deserialize)]
struct GqlRepository {
    issues: GqlIssueConnection,
}

#[derive(Debug, Deserialize)]
struct GqlIssueConnection {
    nodes: Vec<GqlIssue>,
}

#[derive(Debug, Deserialize)]
struct GqlIssue {
    number: u64,
    title: String,
    body: Option<String>,
    url: String,
    labels: GqlLabelConnection,
    #[serde(rename = "subIssues", default)]
    sub_issues: GqlSubIssueConnection,
}

#[derive(Debug, Deserialize)]
struct GqlSubIssue {
    number: u64,
    title: String,
    body: Option<String>,
    url: String,
    state: String,
    labels: GqlLabelConnection,
}

#[derive(Debug, Deserialize, Default)]
struct GqlSubIssueConnection {
    #[serde(default)]
    nodes: Vec<GqlSubIssue>,
}

#[derive(Debug, Deserialize, Default)]
struct GqlLabelConnection {
    #[serde(default)]
    nodes: Vec<GqlLabel>,
}

#[derive(Debug, Deserialize)]
struct GqlLabel {
    name: String,
}

impl TaskSource for GitHubSource {
    fn fetch_eligible_task_groups(&self) -> Result<Vec<TaskGroup>> {
        self.fetch_eligible_task_groups_impl()
    }

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

    fn mark_done(&self, task_id: &str) -> Result<()> {
        if let Err(e) = self.client.run(&[
            "issue",
            "edit",
            task_id,
            "--add-label",
            "done",
            "--remove-label",
            "in-progress",
        ]) {
            warn!(task_id, error = %e, "failed to update labels for done");
        }
        debug!(task_id, "marked done");
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

    // --- GraphQL task group tests ---

    fn repo_nwo_json() -> String {
        serde_json::json!({"name": "repo", "owner": {"login": "owner"}}).to_string()
    }

    fn gql_response(issues: Vec<serde_json::Value>) -> String {
        serde_json::json!({
            "data": {
                "repository": {
                    "issues": {
                        "nodes": issues
                    }
                }
            }
        })
        .to_string()
    }

    fn gql_issue(
        number: u64,
        title: &str,
        labels: &[&str],
        body: &str,
        sub_issues: Vec<serde_json::Value>,
    ) -> serde_json::Value {
        serde_json::json!({
            "number": number,
            "title": title,
            "body": body,
            "url": format!("https://github.com/owner/repo/issues/{number}"),
            "labels": { "nodes": labels.iter().map(|l| serde_json::json!({"name": l})).collect::<Vec<_>>() },
            "subIssues": { "nodes": sub_issues }
        })
    }

    fn gql_sub(number: u64, title: &str, labels: &[&str], body: &str) -> serde_json::Value {
        gql_sub_with_state(number, title, labels, body, "OPEN")
    }

    fn gql_sub_with_state(
        number: u64,
        title: &str,
        labels: &[&str],
        body: &str,
        state: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "number": number,
            "title": title,
            "body": body,
            "state": state,
            "url": format!("https://github.com/owner/repo/issues/{number}"),
            "labels": { "nodes": labels.iter().map(|l| serde_json::json!({"name": l})).collect::<Vec<_>>() }
        })
    }

    #[test]
    fn test_graphql_standalone_no_sub_issues() {
        let resp = gql_response(vec![gql_issue(1, "Solo", &["rlph"], "body", vec![])]);
        let client = MockGhClient::new(vec![Ok(repo_nwo_json()), Ok(resp)]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        let groups = source.fetch_eligible_task_groups_impl().unwrap();
        assert_eq!(groups.len(), 1);
        assert!(matches!(&groups[0], TaskGroup::Standalone(t) if t.id == "1"));
    }

    #[test]
    fn test_graphql_parent_with_labeled_sub_issues() {
        let resp = gql_response(vec![gql_issue(
            10,
            "Parent",
            &["rlph"],
            "",
            vec![
                gql_sub(11, "Child A", &["rlph"], ""),
                gql_sub(12, "Child B", &["rlph"], ""),
            ],
        )]);
        let client = MockGhClient::new(vec![Ok(repo_nwo_json()), Ok(resp)]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        let groups = source.fetch_eligible_task_groups_impl().unwrap();
        assert_eq!(groups.len(), 1);
        match &groups[0] {
            TaskGroup::Group { parent, sub_issues } => {
                assert_eq!(parent.id, "10");
                assert_eq!(sub_issues.len(), 2);
                let ids: Vec<&str> = sub_issues.iter().map(|t| t.id.as_str()).collect();
                assert!(ids.contains(&"11"));
                assert!(ids.contains(&"12"));
            }
            _ => panic!("expected Group variant"),
        }
    }

    #[test]
    fn test_graphql_ignores_unlabeled_sub_issues() {
        let resp = gql_response(vec![gql_issue(
            10,
            "Parent",
            &["rlph"],
            "",
            vec![
                gql_sub(11, "Labeled", &["rlph"], ""),
                gql_sub(12, "Unlabeled", &["bug"], ""),
            ],
        )]);
        let client = MockGhClient::new(vec![Ok(repo_nwo_json()), Ok(resp)]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        let groups = source.fetch_eligible_task_groups_impl().unwrap();
        assert_eq!(groups.len(), 1);
        match &groups[0] {
            TaskGroup::Group { sub_issues, .. } => {
                assert_eq!(sub_issues.len(), 1);
                assert_eq!(sub_issues[0].id, "11");
            }
            _ => panic!("expected Group variant"),
        }
    }

    #[test]
    fn test_graphql_all_sub_issues_unlabeled_becomes_standalone() {
        let resp = gql_response(vec![gql_issue(
            10,
            "Parent",
            &["rlph"],
            "",
            vec![gql_sub(11, "Unlabeled", &["bug"], "")],
        )]);
        let client = MockGhClient::new(vec![Ok(repo_nwo_json()), Ok(resp)]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        let groups = source.fetch_eligible_task_groups_impl().unwrap();
        assert_eq!(groups.len(), 1);
        assert!(matches!(&groups[0], TaskGroup::Standalone(t) if t.id == "10"));
    }

    #[test]
    fn test_graphql_child_not_duplicated_as_standalone() {
        // Issue 11 is a sub-issue of 10 AND appears at top level (has rlph label).
        // It should only appear inside the group, not also as standalone.
        let resp = gql_response(vec![
            gql_issue(
                10,
                "Parent",
                &["rlph"],
                "",
                vec![gql_sub(11, "Child", &["rlph"], "")],
            ),
            gql_issue(11, "Child top-level", &["rlph"], "", vec![]),
        ]);
        let client = MockGhClient::new(vec![Ok(repo_nwo_json()), Ok(resp)]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        let groups = source.fetch_eligible_task_groups_impl().unwrap();
        assert_eq!(groups.len(), 1);
        assert!(matches!(&groups[0], TaskGroup::Group { parent, .. } if parent.id == "10"));
    }

    #[test]
    fn test_graphql_sub_issues_sorted_by_deps() {
        // 12 depends on 11
        let resp = gql_response(vec![gql_issue(
            10,
            "Parent",
            &["rlph"],
            "",
            vec![
                gql_sub(12, "Second", &["rlph"], "Blocked by #11"),
                gql_sub(11, "First", &["rlph"], ""),
            ],
        )]);
        let client = MockGhClient::new(vec![Ok(repo_nwo_json()), Ok(resp)]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        let groups = source.fetch_eligible_task_groups_impl().unwrap();
        match &groups[0] {
            TaskGroup::Group { sub_issues, .. } => {
                assert_eq!(sub_issues[0].id, "11");
                assert_eq!(sub_issues[1].id, "12");
            }
            _ => panic!("expected Group variant"),
        }
    }

    #[test]
    fn test_graphql_filters_closed_sub_issues() {
        let resp = gql_response(vec![gql_issue(
            10,
            "Parent",
            &["rlph"],
            "",
            vec![
                gql_sub(11, "Open child", &["rlph"], ""),
                gql_sub_with_state(12, "Closed child", &["rlph"], "", "CLOSED"),
            ],
        )]);
        let client = MockGhClient::new(vec![Ok(repo_nwo_json()), Ok(resp)]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        let groups = source.fetch_eligible_task_groups_impl().unwrap();
        assert_eq!(groups.len(), 1);
        match &groups[0] {
            TaskGroup::Group { sub_issues, .. } => {
                assert_eq!(sub_issues.len(), 1);
                assert_eq!(sub_issues[0].id, "11");
            }
            _ => panic!("expected Group variant"),
        }
    }

    #[test]
    fn test_graphql_all_sub_issues_closed_becomes_standalone() {
        let resp = gql_response(vec![gql_issue(
            10,
            "Parent",
            &["rlph"],
            "",
            vec![gql_sub_with_state(
                11,
                "Closed child",
                &["rlph"],
                "",
                "CLOSED",
            )],
        )]);
        let client = MockGhClient::new(vec![Ok(repo_nwo_json()), Ok(resp)]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        let groups = source.fetch_eligible_task_groups_impl().unwrap();
        assert_eq!(groups.len(), 1);
        assert!(matches!(&groups[0], TaskGroup::Standalone(t) if t.id == "10"));
    }

    #[test]
    fn test_graphql_skips_in_progress_parent() {
        let resp = gql_response(vec![
            gql_issue(1, "Active", &["rlph", "in-progress"], "", vec![]),
            gql_issue(2, "Ready", &["rlph"], "", vec![]),
        ]);
        let client = MockGhClient::new(vec![Ok(repo_nwo_json()), Ok(resp)]);
        let source = GitHubSource::with_client("rlph", Box::new(client));
        let groups = source.fetch_eligible_task_groups_impl().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].parent().id, "2");
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
