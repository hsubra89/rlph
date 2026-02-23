use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::error::{Error, Result};

use super::{Priority, Task, TaskSource};

const LINEAR_API_URL: &str = "https://api.linear.app/graphql";
const LINEAR_CLI_CREDENTIALS: &str = ".config/linear/credentials.toml";
const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 500;

/// Resolve the Linear API key: env var first, then Linear CLI credentials file.
fn resolve_api_key(api_key_env: &str) -> Result<String> {
    if let Ok(key) = std::env::var(api_key_env) {
        return Ok(key);
    }

    if let Some(home) = std::env::var_os("HOME") {
        let creds_path = std::path::Path::new(&home).join(LINEAR_CLI_CREDENTIALS);
        if let Ok(contents) = std::fs::read_to_string(&creds_path)
            && let Ok(table) = contents.parse::<toml::Table>()
            && let Some(default_profile) = table.get("default").and_then(|v| v.as_str())
            && let Some(token) = table.get(default_profile).and_then(|v| v.as_str())
        {
            debug!(
                profile = default_profile,
                "using Linear API key from CLI credentials"
            );
            return Ok(token.to_string());
        }
    }

    Err(Error::TaskSource(format!(
        "Linear API key not found in ${} or ~/.config/linear/credentials.toml",
        api_key_env
    )))
}

// ---------------------------------------------------------------------------
// Client abstraction (for testability)
// ---------------------------------------------------------------------------

pub trait LinearClient {
    fn graphql(&self, query: &str, variables: serde_json::Value) -> Result<serde_json::Value>;
}

struct DefaultLinearClient {
    api_key: String,
}

impl LinearClient for DefaultLinearClient {
    fn graphql(&self, query: &str, variables: serde_json::Value) -> Result<serde_json::Value> {
        let body = serde_json::json!({
            "query": query,
            "variables": variables,
        });

        let mut backoff_ms = INITIAL_BACKOFF_MS;
        for attempt in 1..=MAX_RETRIES {
            // Linear API uses raw API key, not "Bearer <key>"
            match ureq::post(LINEAR_API_URL)
                .set("Authorization", &self.api_key)
                .set("Content-Type", "application/json")
                .send_json(&body)
            {
                Ok(response) => {
                    let json: serde_json::Value = response.into_json().map_err(|e| {
                        Error::TaskSource(format!("failed to parse Linear response: {e}"))
                    })?;

                    if let Some(errors) = json.get("errors") {
                        return Err(Error::TaskSource(format!("Linear API errors: {errors}")));
                    }

                    return json.get("data").cloned().ok_or_else(|| {
                        Error::TaskSource("Linear API response missing data".to_string())
                    });
                }
                Err(ref e) if attempt < MAX_RETRIES && is_retryable(e) => {
                    warn!(
                        attempt,
                        error = %e,
                        backoff_ms,
                        "retrying Linear API after transient error"
                    );
                    thread::sleep(Duration::from_millis(backoff_ms));
                    backoff_ms *= 2;
                }
                Err(e) => {
                    return Err(Error::TaskSource(format!("Linear API request failed: {e}")));
                }
            }
        }
        unreachable!()
    }
}

/// Only retry rate-limits (429), server errors (5xx), and transport/network errors.
fn is_retryable(err: &ureq::Error) -> bool {
    match err {
        ureq::Error::Status(code, _) => *code == 429 || *code >= 500,
        ureq::Error::Transport(_) => true,
    }
}

// ---------------------------------------------------------------------------
// GraphQL response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct IssueNode {
    #[allow(dead_code)]
    identifier: String,
    number: u64,
    title: String,
    description: Option<String>,
    url: String,
    priority: u8,
    #[allow(dead_code)]
    state: StateNode,
    labels: LabelConnection,
}

#[derive(Debug, Deserialize)]
struct StateNode {
    #[allow(dead_code)]
    name: String,
    #[serde(rename = "type")]
    #[allow(dead_code)]
    state_type: String,
}

#[derive(Debug, Deserialize)]
struct LabelConnection {
    nodes: Vec<LabelNode>,
}

#[derive(Debug, Deserialize)]
struct LabelNode {
    name: String,
}

#[derive(Debug, Deserialize)]
struct IssueConnection {
    nodes: Vec<IssueNode>,
}

/// Lightweight types for queries that only need the issue UUID.
#[derive(Debug, Deserialize)]
struct IssueIdNode {
    id: String,
}

#[derive(Debug, Deserialize)]
struct IssueIdConnection {
    nodes: Vec<IssueIdNode>,
}

#[derive(Debug, Deserialize)]
struct WorkflowStateNode {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct WorkflowStateConnection {
    nodes: Vec<WorkflowStateNode>,
}

#[derive(Debug, Deserialize)]
struct TeamNode {
    id: String,
}

#[derive(Debug, Deserialize)]
struct TeamConnection {
    nodes: Vec<TeamNode>,
}

// ---------------------------------------------------------------------------
// LinearSource
// ---------------------------------------------------------------------------

pub struct LinearSource {
    label: String,
    team: String,
    project: Option<String>,
    in_progress_state: String,
    in_review_state: String,
    done_state: String,
    client: Box<dyn LinearClient>,
}

impl LinearSource {
    pub fn new(config: &Config) -> Result<Self> {
        let linear = config.linear.as_ref().ok_or_else(|| {
            Error::ConfigValidation(
                "[linear] config section required when source = \"linear\"".to_string(),
            )
        })?;

        let api_key = resolve_api_key(&linear.api_key_env)?;

        Ok(Self {
            label: config.label.clone(),
            team: linear.team.clone(),
            project: linear.project.clone(),
            in_progress_state: linear.in_progress_state.clone(),
            in_review_state: linear.in_review_state.clone(),
            done_state: linear.done_state.clone(),
            client: Box::new(DefaultLinearClient {
                api_key: api_key.to_string(),
            }),
        })
    }

    #[cfg(test)]
    fn with_client(label: &str, team: &str, client: Box<dyn LinearClient>) -> Self {
        Self {
            label: label.to_string(),
            team: team.to_string(),
            project: None,
            in_progress_state: "In Progress".to_string(),
            in_review_state: "In Review".to_string(),
            done_state: "Done".to_string(),
            client,
        }
    }

    /// Map Linear priority (0-4) to our Priority (1-9).
    /// Linear: 0=None, 1=Urgent, 2=High, 3=Medium, 4=Low.
    fn map_priority(linear_priority: u8) -> Option<Priority> {
        match linear_priority {
            1 => Some(Priority(1)), // Urgent
            2 => Some(Priority(2)), // High
            3 => Some(Priority(5)), // Medium
            4 => Some(Priority(8)), // Low
            _ => None,              // 0 = No priority
        }
    }

    fn parse_issue(node: &IssueNode) -> Task {
        let labels: Vec<String> = node.labels.nodes.iter().map(|l| l.name.clone()).collect();
        let priority = Self::map_priority(node.priority)
            .or_else(|| labels.iter().find_map(|l| Priority::from_label(l)));

        Task {
            id: node.number.to_string(),
            title: node.title.clone(),
            body: node.description.clone().unwrap_or_default(),
            labels,
            url: node.url.clone(),
            priority,
        }
    }

    /// Resolve a workflow state name → UUID for the configured team.
    fn find_state_id(&self, state_name: &str) -> Result<String> {
        let query = r#"
            query WorkflowStates($team: String!) {
                workflowStates(filter: { team: { key: { eq: $team } } }) {
                    nodes { id name }
                }
            }
        "#;

        let data = self
            .client
            .graphql(query, serde_json::json!({ "team": self.team }))?;

        let states: WorkflowStateConnection =
            serde_json::from_value(data.get("workflowStates").cloned().unwrap_or_default())
                .map_err(|e| Error::TaskSource(format!("failed to parse workflow states: {e}")))?;

        states
            .nodes
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(state_name))
            .map(|s| s.id.clone())
            .ok_or_else(|| {
                Error::TaskSource(format!(
                    "workflow state '{state_name}' not found for team '{}'",
                    self.team
                ))
            })
    }

    /// Resolve an issue number → UUID within the configured team.
    fn find_issue_id(&self, issue_number: &str) -> Result<String> {
        let number: f64 = issue_number
            .parse::<u64>()
            .map_err(|_| Error::TaskSource(format!("invalid issue number: {issue_number}")))?
            as f64;

        let query = r#"
            query IssueByNumber($team: String!, $number: Float!) {
                issues(
                    filter: { team: { key: { eq: $team } }, number: { eq: $number } }
                    first: 1
                ) {
                    nodes { id }
                }
            }
        "#;

        let data = self.client.graphql(
            query,
            serde_json::json!({ "team": self.team, "number": number }),
        )?;

        let issues: IssueIdConnection =
            serde_json::from_value(data.get("issues").cloned().unwrap_or_default())
                .map_err(|e| Error::TaskSource(format!("failed to parse issue lookup: {e}")))?;

        issues.nodes.first().map(|i| i.id.clone()).ok_or_else(|| {
            Error::TaskSource(format!(
                "issue #{issue_number} not found in team '{}'",
                self.team
            ))
        })
    }

    /// Update an issue's workflow state by name.
    fn update_issue_state(&self, task_id: &str, state_name: &str) -> Result<()> {
        let issue_id = self.find_issue_id(task_id)?;
        let state_id = self.find_state_id(state_name)?;

        let query = r#"
            mutation UpdateIssueState($issueId: String!, $stateId: String!) {
                issueUpdate(id: $issueId, input: { stateId: $stateId }) {
                    success
                }
            }
        "#;

        let data = self.client.graphql(
            query,
            serde_json::json!({ "issueId": issue_id, "stateId": state_id }),
        )?;

        let success = data
            .get("issueUpdate")
            .and_then(|u| u.get("success"))
            .and_then(|s| s.as_bool())
            .unwrap_or(false);

        if !success {
            return Err(Error::TaskSource(format!(
                "failed to update issue #{task_id} to state '{state_name}'"
            )));
        }

        Ok(())
    }

    fn build_issue_filter(&self) -> serde_json::Value {
        let mut filter = serde_json::json!({
            "team": { "key": { "eq": self.team } },
            "labels": { "name": { "eq": self.label } },
        });

        if let Some(ref project) = self.project {
            filter["project"] = serde_json::json!({ "name": { "eq": project } });
        }

        filter
    }
}

impl TaskSource for LinearSource {
    fn fetch_eligible_tasks(&self) -> Result<Vec<Task>> {
        let mut filter = self.build_issue_filter();
        filter["state"] = serde_json::json!({
            "name": { "nin": [&self.in_progress_state, &self.in_review_state, &self.done_state] },
            "type": { "nin": ["completed", "canceled"] },
        });

        let query = r#"
            query Issues($filter: IssueFilter!) {
                issues(filter: $filter, first: 100) {
                    nodes {
                        id identifier number title description url priority
                        state { name type }
                        labels { nodes { name } }
                    }
                }
            }
        "#;

        let data = self
            .client
            .graphql(query, serde_json::json!({ "filter": filter }))?;

        let issues: IssueConnection =
            serde_json::from_value(data.get("issues").cloned().unwrap_or_default())
                .map_err(|e| Error::TaskSource(format!("failed to parse Linear issues: {e}")))?;

        let tasks: Vec<Task> = issues.nodes.iter().map(Self::parse_issue).collect();

        debug!(count = tasks.len(), "fetched eligible Linear tasks");
        Ok(tasks)
    }

    fn mark_in_progress(&self, task_id: &str) -> Result<()> {
        self.update_issue_state(task_id, &self.in_progress_state)?;
        debug!(task_id, "marked in-progress on Linear");
        Ok(())
    }

    fn mark_in_review(&self, task_id: &str) -> Result<()> {
        self.update_issue_state(task_id, &self.in_review_state)?;
        debug!(task_id, "marked in-review on Linear");
        Ok(())
    }

    fn get_task_details(&self, task_id: &str) -> Result<Task> {
        let number: f64 = task_id
            .parse::<u64>()
            .map_err(|_| Error::TaskSource(format!("invalid task id: {task_id}")))?
            as f64;

        let query = r#"
            query IssueDetails($team: String!, $number: Float!) {
                issues(
                    filter: { team: { key: { eq: $team } }, number: { eq: $number } }
                    first: 1
                ) {
                    nodes {
                        id identifier number title description url priority
                        state { name type }
                        labels { nodes { name } }
                    }
                }
            }
        "#;

        let data = self.client.graphql(
            query,
            serde_json::json!({ "team": self.team, "number": number }),
        )?;

        let issues: IssueConnection =
            serde_json::from_value(data.get("issues").cloned().unwrap_or_default())
                .map_err(|e| Error::TaskSource(format!("failed to parse issue details: {e}")))?;

        let node = issues.nodes.first().ok_or_else(|| {
            Error::TaskSource(format!(
                "issue #{task_id} not found in team '{}'",
                self.team
            ))
        })?;

        Ok(Self::parse_issue(node))
    }

    fn fetch_closed_task_ids(&self) -> Result<HashSet<u64>> {
        let mut filter = self.build_issue_filter();
        filter["state"] = serde_json::json!({ "type": { "in": ["completed", "canceled"] } });

        let query = r#"
            query ClosedIssues($filter: IssueFilter!) {
                issues(filter: $filter, first: 200) {
                    nodes { number }
                }
            }
        "#;

        let data = self
            .client
            .graphql(query, serde_json::json!({ "filter": filter }))?;

        #[derive(Deserialize)]
        struct NumberNode {
            number: u64,
        }
        #[derive(Deserialize)]
        struct NumberConnection {
            nodes: Vec<NumberNode>,
        }

        let nums: NumberConnection =
            serde_json::from_value(data.get("issues").cloned().unwrap_or_default())
                .map_err(|e| Error::TaskSource(format!("failed to parse closed issues: {e}")))?;

        let ids: HashSet<u64> = nums.nodes.into_iter().map(|n| n.number).collect();
        debug!(?ids, "fetched closed Linear task ids");
        Ok(ids)
    }
}

// ---------------------------------------------------------------------------
// rlph init — label bootstrapping
// ---------------------------------------------------------------------------

/// Create the configured label in a Linear team if it doesn't already exist.
pub fn init_label(config: &Config) -> Result<()> {
    let linear = config.linear.as_ref().ok_or_else(|| {
        Error::ConfigValidation("[linear] config section required for init".to_string())
    })?;

    let api_key = resolve_api_key(&linear.api_key_env)?;

    let client = DefaultLinearClient {
        api_key: api_key.to_string(),
    };
    init_label_with_client(&config.label, &linear.team, &client)
}

fn init_label_with_client(label: &str, team_key: &str, client: &dyn LinearClient) -> Result<()> {
    let label_name = label;

    // Check if label already exists
    let query = r#"
        query FindLabel($team: String!, $label: String!) {
            issueLabels(filter: { team: { key: { eq: $team } }, name: { eq: $label } }) {
                nodes { id name }
            }
        }
    "#;

    let data = client.graphql(
        query,
        serde_json::json!({ "team": team_key, "label": label_name }),
    )?;

    #[derive(Deserialize)]
    struct LabelCheckNode {
        #[allow(dead_code)]
        id: String,
    }
    #[derive(Deserialize)]
    struct LabelCheckConnection {
        nodes: Vec<LabelCheckNode>,
    }

    let labels: LabelCheckConnection =
        serde_json::from_value(data.get("issueLabels").cloned().unwrap_or_default())
            .map_err(|e| Error::TaskSource(format!("failed to parse labels: {e}")))?;

    if !labels.nodes.is_empty() {
        info!(
            "Label '{}' already exists in team '{}'; skipping",
            label_name, team_key
        );
        return Ok(());
    }

    // Resolve team key → UUID
    let team_query = r#"
        query FindTeam($key: String!) {
            teams(filter: { key: { eq: $key } }) {
                nodes { id }
            }
        }
    "#;

    let team_data = client.graphql(team_query, serde_json::json!({ "key": team_key }))?;

    let teams: TeamConnection =
        serde_json::from_value(team_data.get("teams").cloned().unwrap_or_default())
            .map_err(|e| Error::TaskSource(format!("failed to parse teams: {e}")))?;

    let team_id = teams
        .nodes
        .first()
        .map(|t| t.id.clone())
        .ok_or_else(|| Error::TaskSource(format!("team '{team_key}' not found")))?;

    // Create label
    let create_query = r#"
        mutation CreateLabel($teamId: String!, $name: String!) {
            issueLabelCreate(input: { teamId: $teamId, name: $name }) {
                success
                issueLabel { id name }
            }
        }
    "#;

    client.graphql(
        create_query,
        serde_json::json!({ "teamId": team_id, "name": label_name }),
    )?;

    info!("Created label '{}' in team '{}'", label_name, team_key);
    Ok(())
}

// ---------------------------------------------------------------------------
// rlph init — interactive team discovery + label bootstrapping
// ---------------------------------------------------------------------------

/// Interactive init: discover teams, prompt user to pick one, create label, write config.
pub fn init_interactive(label: &str) -> Result<()> {
    let api_key = resolve_api_key("LINEAR_API_KEY")?;
    let client = DefaultLinearClient {
        api_key: api_key.to_string(),
    };

    let teams = list_teams(&client)?;
    if teams.is_empty() {
        return Err(Error::TaskSource(
            "no teams found for your Linear account".to_string(),
        ));
    }

    let team_key = if teams.len() == 1 {
        eprintln!("Found one team: {} ({})", teams[0].1, teams[0].0);
        teams[0].0.clone()
    } else {
        prompt_team_selection(&teams, &mut std::io::stdin().lock(), &mut std::io::stderr())?
    };

    init_label_with_client(label, &team_key, &client)?;

    let config_dir = std::path::Path::new(".rlph");
    write_linear_config(&team_key, config_dir)?;

    eprintln!("Wrote [linear] config to .rlph/config.toml");
    Ok(())
}

fn list_teams(client: &dyn LinearClient) -> Result<Vec<(String, String)>> {
    let query = r#"
        query { viewer { teams { nodes { key name } } } }
    "#;

    let data = client.graphql(query, serde_json::json!({}))?;

    #[derive(Deserialize)]
    struct TeamInfoNode {
        key: String,
        name: String,
    }
    #[derive(Deserialize)]
    struct TeamInfoConnection {
        nodes: Vec<TeamInfoNode>,
    }
    #[derive(Deserialize)]
    struct Viewer {
        teams: TeamInfoConnection,
    }

    let viewer: Viewer = serde_json::from_value(data.get("viewer").cloned().unwrap_or_default())
        .map_err(|e| Error::TaskSource(format!("failed to parse teams: {e}")))?;

    Ok(viewer
        .teams
        .nodes
        .into_iter()
        .map(|t| (t.key, t.name))
        .collect())
}

fn prompt_team_selection(
    teams: &[(String, String)],
    stdin: &mut dyn BufRead,
    stderr: &mut dyn Write,
) -> Result<String> {
    writeln!(stderr, "Select a Linear team:").ok();
    for (i, (key, name)) in teams.iter().enumerate() {
        writeln!(stderr, "  {}) {} ({})", i + 1, name, key).ok();
    }
    write!(stderr, "Choice [1-{}]: ", teams.len()).ok();
    stderr.flush().ok();

    let mut line = String::new();
    stdin
        .read_line(&mut line)
        .map_err(|e| Error::TaskSource(format!("failed to read stdin: {e}")))?;

    let choice: usize = line
        .trim()
        .parse()
        .map_err(|_| Error::TaskSource(format!("invalid choice: {}", line.trim())))?;

    if choice < 1 || choice > teams.len() {
        return Err(Error::TaskSource(format!(
            "choice out of range: {} (expected 1-{})",
            choice,
            teams.len()
        )));
    }

    Ok(teams[choice - 1].0.clone())
}

fn write_linear_config(team_key: &str, dir: &std::path::Path) -> Result<()> {
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
    }

    let path = dir.join("config.toml");
    let mut table = if path.exists() {
        let existing = std::fs::read_to_string(&path)?;
        if existing.trim().is_empty() {
            toml::Table::new()
        } else {
            existing.parse::<toml::Table>().map_err(|e| {
                Error::TaskSource(format!("failed to parse {}: {e}", path.display()))
            })?
        }
    } else {
        toml::Table::new()
    };

    table.insert(
        "source".to_string(),
        toml::Value::String("linear".to_string()),
    );

    match table.get_mut("linear") {
        Some(toml::Value::Table(linear)) => {
            linear.insert(
                "team".to_string(),
                toml::Value::String(team_key.to_string()),
            );
        }
        _ => {
            let mut linear = toml::Table::new();
            linear.insert(
                "team".to_string(),
                toml::Value::String(team_key.to_string()),
            );
            table.insert("linear".to_string(), toml::Value::Table(linear));
        }
    }

    let content = toml::to_string(&table)
        .map_err(|e| Error::TaskSource(format!("failed to serialize {}: {e}", path.display())))?;
    std::fs::write(&path, content)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Retry with exponential backoff
// ---------------------------------------------------------------------------

#[cfg(test)]
fn retry_with_backoff_ms<F, T>(f: F, initial_backoff_ms: u64, max_retries: u32) -> Result<T>
where
    F: Fn() -> Result<T>,
{
    let mut backoff_ms = initial_backoff_ms;

    for attempt in 1..=max_retries {
        match f() {
            Ok(val) => return Ok(val),
            Err(e) if attempt < max_retries => {
                warn!(
                    attempt,
                    error = %e,
                    backoff_ms,
                    "retrying Linear API after transient error"
                );
                thread::sleep(Duration::from_millis(backoff_ms));
                backoff_ms *= 2;
            }
            Err(e) => return Err(e),
        }
    }

    unreachable!()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct MockLinearClient {
        responses: RefCell<Vec<Result<serde_json::Value>>>,
    }

    impl MockLinearClient {
        fn new(responses: Vec<Result<serde_json::Value>>) -> Self {
            Self {
                responses: RefCell::new(responses),
            }
        }
    }

    impl LinearClient for MockLinearClient {
        fn graphql(
            &self,
            _query: &str,
            _variables: serde_json::Value,
        ) -> Result<serde_json::Value> {
            let mut responses = self.responses.borrow_mut();
            if responses.is_empty() {
                Err(Error::TaskSource("no more mock responses".to_string()))
            } else {
                responses.remove(0)
            }
        }
    }

    fn issue_node(
        number: u64,
        title: &str,
        priority: u8,
        state_name: &str,
        state_type: &str,
        labels: &[&str],
    ) -> serde_json::Value {
        serde_json::json!({
            "id": format!("uuid-{number}"),
            "identifier": format!("ENG-{number}"),
            "number": number,
            "title": title,
            "description": format!("body for {title}"),
            "url": format!("https://linear.app/team/issue/ENG-{number}"),
            "priority": priority,
            "state": { "name": state_name, "type": state_type },
            "labels": { "nodes": labels.iter().map(|l| serde_json::json!({"name": l})).collect::<Vec<_>>() }
        })
    }

    fn issues_response(nodes: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({ "issues": { "nodes": nodes } })
    }

    #[test]
    fn test_fetch_eligible_returns_all_from_api() {
        // Server-side filter handles state exclusion; client gets only eligible issues
        let data = issues_response(vec![
            issue_node(1, "Task 1", 0, "Todo", "unstarted", &["rlph"]),
            issue_node(4, "Task 4", 2, "Backlog", "backlog", &["rlph"]),
        ]);
        let client = MockLinearClient::new(vec![Ok(data)]);
        let source = LinearSource::with_client("rlph", "ENG", Box::new(client));
        let tasks = source.fetch_eligible_tasks().unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "1");
        assert_eq!(tasks[1].id, "4");
    }

    #[test]
    fn test_priority_mapping() {
        assert_eq!(LinearSource::map_priority(0), None);
        assert_eq!(LinearSource::map_priority(1), Some(Priority(1))); // Urgent
        assert_eq!(LinearSource::map_priority(2), Some(Priority(2))); // High
        assert_eq!(LinearSource::map_priority(3), Some(Priority(5))); // Medium
        assert_eq!(LinearSource::map_priority(4), Some(Priority(8))); // Low
        assert_eq!(LinearSource::map_priority(5), None);
    }

    #[test]
    fn test_fetch_parses_priority() {
        let data = issues_response(vec![
            issue_node(1, "Urgent", 1, "Todo", "unstarted", &["rlph"]),
            issue_node(2, "High", 2, "Todo", "unstarted", &["rlph"]),
            issue_node(3, "None", 0, "Todo", "unstarted", &["rlph"]),
        ]);
        let client = MockLinearClient::new(vec![Ok(data)]);
        let source = LinearSource::with_client("rlph", "ENG", Box::new(client));
        let tasks = source.fetch_eligible_tasks().unwrap();
        assert_eq!(tasks[0].priority, Some(Priority(1)));
        assert_eq!(tasks[1].priority, Some(Priority(2)));
        assert_eq!(tasks[2].priority, None);
    }

    #[test]
    fn test_fetch_handles_null_description() {
        let data = serde_json::json!({
            "issues": { "nodes": [{
                "id": "uuid-1",
                "identifier": "ENG-1",
                "number": 1,
                "title": "No desc",
                "description": null,
                "url": "https://linear.app/team/issue/ENG-1",
                "priority": 0,
                "state": { "name": "Todo", "type": "unstarted" },
                "labels": { "nodes": [] }
            }]}
        });
        let client = MockLinearClient::new(vec![Ok(data)]);
        let source = LinearSource::with_client("rlph", "ENG", Box::new(client));
        let tasks = source.fetch_eligible_tasks().unwrap();
        assert_eq!(tasks[0].body, "");
    }

    #[test]
    fn test_get_task_details() {
        let data = issues_response(vec![issue_node(
            7,
            "Detail task",
            3,
            "Todo",
            "unstarted",
            &["rlph", "bug"],
        )]);
        let client = MockLinearClient::new(vec![Ok(data)]);
        let source = LinearSource::with_client("rlph", "ENG", Box::new(client));
        let task = source.get_task_details("7").unwrap();
        assert_eq!(task.id, "7");
        assert_eq!(task.title, "Detail task");
        assert_eq!(task.priority, Some(Priority(5))); // Medium
        assert_eq!(task.labels, vec!["rlph", "bug"]);
    }

    #[test]
    fn test_get_task_details_not_found() {
        let data = issues_response(vec![]);
        let client = MockLinearClient::new(vec![Ok(data)]);
        let source = LinearSource::with_client("rlph", "ENG", Box::new(client));
        let err = source.get_task_details("999").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_fetch_closed_task_ids() {
        let data = serde_json::json!({
            "issues": { "nodes": [
                { "number": 10 },
                { "number": 20 },
                { "number": 30 },
            ]}
        });
        let client = MockLinearClient::new(vec![Ok(data)]);
        let source = LinearSource::with_client("rlph", "ENG", Box::new(client));
        let ids = source.fetch_closed_task_ids().unwrap();
        assert_eq!(ids, HashSet::from([10, 20, 30]));
    }

    #[test]
    fn test_mark_in_progress() {
        // find_issue_id response (lightweight: only id), find_state_id, issueUpdate
        let issue_data = serde_json::json!({
            "issues": { "nodes": [{ "id": "uuid-42" }] }
        });
        let state_data = serde_json::json!({
            "workflowStates": { "nodes": [
                { "id": "state-1", "name": "In Progress" },
                { "id": "state-2", "name": "Done" },
            ]}
        });
        let update_data = serde_json::json!({ "issueUpdate": { "success": true } });

        let client = MockLinearClient::new(vec![Ok(issue_data), Ok(state_data), Ok(update_data)]);
        let source = LinearSource::with_client("rlph", "ENG", Box::new(client));
        source.mark_in_progress("42").unwrap();
    }

    #[test]
    fn test_fetch_error_propagated() {
        let client = MockLinearClient::new(vec![Err(Error::TaskSource(
            "connection refused".to_string(),
        ))]);
        let source = LinearSource::with_client("rlph", "ENG", Box::new(client));
        let err = source.fetch_eligible_tasks().unwrap_err();
        assert!(err.to_string().contains("connection refused"));
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

    #[test]
    fn test_init_label_creates_when_missing() {
        let label_data = serde_json::json!({ "issueLabels": { "nodes": [] } });
        let team_data = serde_json::json!({ "teams": { "nodes": [{ "id": "team-uuid" }] } });
        let create_data = serde_json::json!({
            "issueLabelCreate": { "success": true, "issueLabel": { "id": "lbl-1", "name": "rlph" } }
        });

        let client = MockLinearClient::new(vec![Ok(label_data), Ok(team_data), Ok(create_data)]);
        init_label_with_client("rlph", "ENG", &client).unwrap();
    }

    #[test]
    fn test_init_label_skips_when_exists() {
        let label_data = serde_json::json!({
            "issueLabels": { "nodes": [{ "id": "lbl-1", "name": "rlph" }] }
        });

        let client = MockLinearClient::new(vec![Ok(label_data)]);
        init_label_with_client("rlph", "ENG", &client).unwrap();
    }

    #[test]
    fn test_list_teams() {
        let data = serde_json::json!({
            "viewer": { "teams": { "nodes": [
                { "key": "ENG", "name": "Engineering" },
                { "key": "DES", "name": "Design" },
            ]}}
        });
        let client = MockLinearClient::new(vec![Ok(data)]);
        let teams = list_teams(&client).unwrap();
        assert_eq!(
            teams,
            vec![
                ("ENG".to_string(), "Engineering".to_string()),
                ("DES".to_string(), "Design".to_string()),
            ]
        );
    }

    #[test]
    fn test_prompt_team_selection_valid() {
        let teams = vec![
            ("ENG".to_string(), "Engineering".to_string()),
            ("DES".to_string(), "Design".to_string()),
        ];
        let mut input = std::io::Cursor::new(b"2\n");
        let mut output = Vec::new();
        let result = prompt_team_selection(&teams, &mut input, &mut output).unwrap();
        assert_eq!(result, "DES");
    }

    #[test]
    fn test_prompt_team_selection_out_of_range() {
        let teams = vec![("ENG".to_string(), "Engineering".to_string())];
        let mut input = std::io::Cursor::new(b"5\n");
        let mut output = Vec::new();
        let err = prompt_team_selection(&teams, &mut input, &mut output).unwrap_err();
        assert!(err.to_string().contains("out of range"));
    }

    #[test]
    fn test_prompt_team_selection_not_a_number() {
        let teams = vec![("ENG".to_string(), "Engineering".to_string())];
        let mut input = std::io::Cursor::new(b"abc\n");
        let mut output = Vec::new();
        let err = prompt_team_selection(&teams, &mut input, &mut output).unwrap_err();
        assert!(err.to_string().contains("invalid choice"));
    }

    #[test]
    fn test_write_linear_config_creates_new() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("cfg");

        write_linear_config("ENG", &cfg_dir).unwrap();

        let content = std::fs::read_to_string(cfg_dir.join("config.toml")).unwrap();
        assert!(content.contains("source = \"linear\""));
        assert!(content.contains("[linear]"));
        assert!(content.contains("team = \"ENG\""));
    }

    #[test]
    fn test_write_linear_config_appends_to_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), "source = \"linear\"\n").unwrap();

        write_linear_config("DES", &cfg_dir).unwrap();

        let content = std::fs::read_to_string(cfg_dir.join("config.toml")).unwrap();
        assert!(content.contains("source = \"linear\""));
        assert!(content.contains("[linear]"));
        assert!(content.contains("team = \"DES\""));
    }

    #[test]
    fn test_write_linear_config_replaces_existing_section() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            "source = \"linear\"\n\n[linear]\nteam = \"OLD\"\n",
        )
        .unwrap();

        write_linear_config("NEW", &cfg_dir).unwrap();

        let content = std::fs::read_to_string(cfg_dir.join("config.toml")).unwrap();
        assert!(content.contains("source = \"linear\""));
        assert!(content.contains("team = \"NEW\""));
        assert!(!content.contains("OLD"));
    }

    #[test]
    fn test_write_linear_config_overwrites_non_linear_source() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), "source = \"github\"\n").unwrap();

        write_linear_config("ENG", &cfg_dir).unwrap();

        let content = std::fs::read_to_string(cfg_dir.join("config.toml")).unwrap();
        assert!(content.contains("source = \"linear\""));
        assert!(!content.contains("source = \"github\""));
    }

    #[test]
    fn test_write_linear_config_preserves_existing_linear_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            "source = \"linear\"\n\n[linear]\nteam = \"OLD\"\nproject = \"Core\"\n",
        )
        .unwrap();

        write_linear_config("NEW", &cfg_dir).unwrap();

        let content = std::fs::read_to_string(cfg_dir.join("config.toml")).unwrap();
        assert!(content.contains("team = \"NEW\""));
        assert!(content.contains("project = \"Core\""));
    }
}
