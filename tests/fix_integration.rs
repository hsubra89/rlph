mod common;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rlph::config::{Config, ReviewStepConfig};
use rlph::error::{Error, Result};
use rlph::fix::{run_fix, run_fix_loop};
use rlph::orchestrator::CorrectionRunner;
use rlph::review_schema::{ReviewFinding, render_findings_for_github};
use rlph::runner::{RunResult, RunnerKind};
use rlph::submission::{PrComment, REVIEW_MARKER, SubmissionBackend, SubmitResult};
use rlph::test_helpers::make_finding;
use tokio::sync::watch;

use common::{default_test_config, run_git, setup_git_repo};

/// Create a remote PR branch with a commit.
fn create_pr_branch(repo: &Path, branch: &str) {
    run_git(repo, &["checkout", "-b", branch]);
    std::fs::write(repo.join("pr_file.txt"), "pr content").unwrap();
    run_git(repo, &["add", "."]);
    run_git(repo, &["commit", "-m", "PR initial commit"]);
    run_git(repo, &["push", "-u", "origin", branch]);
    run_git(repo, &["checkout", "main"]);
}

// --- Mocks ---

/// Build a review comment with specific findings checked, including the rlph marker.
fn make_review_comment(findings: &[ReviewFinding], checked_ids: &[&str]) -> String {
    let mut body = format!(
        "{REVIEW_MARKER}\n{}",
        render_findings_for_github(findings, "Test review summary.")
    );
    for id in checked_ids {
        let lines: Vec<String> = body
            .lines()
            .map(|line| {
                if line.contains(&format!("{id} description")) {
                    line.replace("- [ ] ", "- [x] ")
                } else {
                    line.to_string()
                }
            })
            .collect();
        body = lines.join("\n");
    }
    body
}

/// Create a PrComment from a body string for testing.
fn make_pr_comment(body: &str) -> PrComment {
    let json = serde_json::json!({
        "id": 1,
        "body": body,
        "created_at": "2025-01-01T00:00:00Z",
        "author_association": "OWNER"
    });
    serde_json::from_value(json).unwrap()
}

fn make_fix_step_config(agent_binary: String) -> ReviewStepConfig {
    ReviewStepConfig {
        prompt: "fix".to_string(),
        runner: RunnerKind::Claude,
        agent_binary,
        agent_model: None,
        agent_effort: None,
        agent_variant: None,
        agent_timeout: Some(30),
    }
}

fn make_config() -> Config {
    Config {
        max_review_rounds: 1,
        agent_timeout_retries: 0,
        ..default_test_config()
    }
}

/// Mock submission that returns a configurable review comment and tracks updates.
struct MockFixSubmission {
    comment_body: Mutex<String>,
    upsert_calls: Mutex<Vec<String>>,
}

impl MockFixSubmission {
    fn new(initial_comment: String) -> Self {
        Self {
            comment_body: Mutex::new(initial_comment),
            upsert_calls: Mutex::new(Vec::new()),
        }
    }

    fn get_comment_body(&self) -> String {
        self.comment_body.lock().unwrap().clone()
    }

    fn upsert_count(&self) -> usize {
        self.upsert_calls.lock().unwrap().len()
    }
}

impl SubmissionBackend for MockFixSubmission {
    fn submit(&self, _: &str, _: &str, _: &str, _: &str) -> Result<SubmitResult> {
        unimplemented!("submit not needed for fix tests")
    }

    fn find_existing_pr_for_issue(&self, _: u64) -> Result<Option<u64>> {
        Ok(None)
    }

    fn upsert_review_comment(&self, _pr_number: u64, body: &str) -> Result<()> {
        *self.comment_body.lock().unwrap() = body.to_string();
        self.upsert_calls.lock().unwrap().push(body.to_string());
        Ok(())
    }

    fn fetch_pr_comments(&self, _pr_number: u64) -> Result<Vec<PrComment>> {
        let body = self.comment_body.lock().unwrap().clone();
        Ok(vec![make_pr_comment(&body)])
    }

    fn fetch_comment_by_id(&self, _comment_id: u64) -> Result<PrComment> {
        let body = self.comment_body.lock().unwrap().clone();
        Ok(make_pr_comment(&body))
    }
}

/// No-op correction runner for tests.
struct MockCorrectionRunner;

impl CorrectionRunner for MockCorrectionRunner {
    async fn resume(
        &self,
        _runner_type: RunnerKind,
        _agent_binary: &str,
        _model: Option<&str>,
        _effort: Option<&str>,
        _variant: Option<&str>,
        _session_id: &str,
        _correction_prompt: &str,
        _working_dir: &Path,
        _timeout: Option<std::time::Duration>,
    ) -> Result<RunResult> {
        Err(Error::AgentRunner("no-op correction runner".to_string()))
    }
}

// --- Tests ---

/// Create a mock agent script that makes a commit and outputs fix JSON.
fn create_mock_agent_script(dir: &Path) -> String {
    let script_path = dir.join("mock-fix-agent.sh");
    // Script: creates a unique file, commits it, outputs stream-json result.
    // Uses $RANDOM and PID to ensure unique filenames across parallel invocations.
    let script = r#"#!/bin/bash
# Mock fix agent: creates a file, commits it, outputs fix result
ID="$$-$RANDOM"
echo "fix-$ID" > "fix-$ID.txt"
git add .
git commit -m "fix: applied-$ID" 2>/dev/null
echo "{\"type\":\"result\",\"result\":\"{\\\"status\\\":\\\"fixed\\\",\\\"commit_message\\\":\\\"fix: applied-$ID\\\"}\"}"
"#;
    std::fs::write(&script_path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    script_path.to_str().unwrap().to_string()
}

/// Test that `run_fix` processes multiple checked items concurrently.
#[tokio::test]
async fn test_parallel_fix_multiple_checked_items() {
    let (_bare_dir, repo_dir) = setup_git_repo();
    let repo_root = repo_dir.path();

    let pr_branch = "feature/test-pr";
    create_pr_branch(repo_root, pr_branch);

    let agent_script = create_mock_agent_script(repo_root);

    // 3 checked items → 3 parallel fixes
    let findings = vec![
        make_finding("bug-alpha"),
        make_finding("bug-beta"),
        make_finding("bug-gamma"),
    ];
    let review_comment = make_review_comment(&findings, &["bug-alpha", "bug-beta", "bug-gamma"]);

    let submission = Arc::new(MockFixSubmission::new(review_comment));
    let correction_runner = Arc::new(MockCorrectionRunner);

    let wt_dir = tempfile::TempDir::new().unwrap();
    let mut config = make_config();
    config.fix = make_fix_step_config(agent_script);
    config.worktree_dir = wt_dir.path().to_str().unwrap().to_string();

    let result = run_fix(
        42,
        pr_branch,
        &config,
        Arc::clone(&submission),
        &rlph::prompts::PromptEngine::new(None),
        repo_root,
        correction_runner,
    )
    .await;

    assert!(result.is_ok(), "run_fix failed: {:?}", result.err());

    // Each fix should have updated the comment
    assert_eq!(
        submission.upsert_count(),
        3,
        "expected 3 comment updates (one per finding)"
    );

    // Final comment: no checked items should remain
    let final_body = submission.get_comment_body();
    assert!(
        !final_body.contains("- [x]"),
        "no items should remain checked after all fixes"
    );
}

/// Test that `run_fix` with no checked items returns Ok and does nothing.
#[tokio::test]
async fn test_fix_no_checked_items() {
    let (_bare_dir, repo_dir) = setup_git_repo();
    let repo_root = repo_dir.path();

    let findings = vec![make_finding("a"), make_finding("b")];
    let review_comment = make_review_comment(&findings, &[]); // none checked

    let submission = Arc::new(MockFixSubmission::new(review_comment));
    let correction_runner = Arc::new(MockCorrectionRunner);

    let config = make_config();

    let result = run_fix(
        42,
        "main",
        &config,
        submission,
        &rlph::prompts::PromptEngine::new(None),
        repo_root,
        correction_runner,
    )
    .await;

    assert!(result.is_ok());
}

/// Test that worktrees are cleaned up after parallel fixes complete.
#[tokio::test]
async fn test_parallel_fix_worktrees_cleaned_up() {
    let (_bare_dir, repo_dir) = setup_git_repo();
    let repo_root = repo_dir.path();

    let pr_branch = "feature/cleanup-test";
    create_pr_branch(repo_root, pr_branch);

    let agent_script = create_mock_agent_script(repo_root);

    let findings = vec![make_finding("clean-a"), make_finding("clean-b")];
    let review_comment = make_review_comment(&findings, &["clean-a", "clean-b"]);

    let submission = Arc::new(MockFixSubmission::new(review_comment));
    let correction_runner = Arc::new(MockCorrectionRunner);

    let wt_dir = tempfile::TempDir::new().unwrap();
    let mut config = make_config();
    config.fix = make_fix_step_config(agent_script);
    config.worktree_dir = wt_dir.path().to_str().unwrap().to_string();

    let result = run_fix(
        42,
        pr_branch,
        &config,
        submission,
        &rlph::prompts::PromptEngine::new(None),
        repo_root,
        correction_runner,
    )
    .await;

    assert!(result.is_ok(), "run_fix failed: {:?}", result.err());

    // After completion, no fix worktree directories should remain
    let wt_entries: Vec<_> = std::fs::read_dir(wt_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("rlph-fix-"))
        })
        .collect();

    assert!(
        wt_entries.is_empty(),
        "expected all fix worktrees to be cleaned up, found: {:?}",
        wt_entries.iter().map(|e| e.file_name()).collect::<Vec<_>>()
    );
}

// --- Polling loop tests ---

/// Mock submission that dynamically checks new items after initial fixes complete.
///
/// On `fetch_pr_comments`: after the first upsert (alpha fixed), the returned
/// comment will also have "beta" checked — simulating a user checking a new box
/// between poll cycles.
struct PollingMockSubmission {
    comment_body: Mutex<String>,
    upsert_calls: Mutex<Vec<String>>,
    fetch_count: AtomicUsize,
    /// Finding ID to dynamically "check" after the first fix completes.
    deferred_check_id: Option<String>,
}

impl PollingMockSubmission {
    fn new(initial_comment: String, deferred_check_id: Option<String>) -> Self {
        Self {
            comment_body: Mutex::new(initial_comment),
            upsert_calls: Mutex::new(Vec::new()),
            fetch_count: AtomicUsize::new(0),
            deferred_check_id,
        }
    }

    fn upsert_count(&self) -> usize {
        self.upsert_calls.lock().unwrap().len()
    }

    /// Compute the current comment body, applying deferred checkbox checks
    /// after the first fix completes.
    fn current_body(&self) -> String {
        let mut body = self.comment_body.lock().unwrap().clone();
        if let Some(ref id) = self.deferred_check_id {
            let upsert_count = self.upsert_calls.lock().unwrap().len();
            if upsert_count >= 1 {
                body = body
                    .lines()
                    .map(|line| {
                        if line.contains(&format!("{id} description"))
                            && line.contains("- [ ]")
                        {
                            line.replace("- [ ] ", "- [x] ")
                        } else {
                            line.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
            }
        }
        body
    }
}

impl SubmissionBackend for PollingMockSubmission {
    fn submit(&self, _: &str, _: &str, _: &str, _: &str) -> Result<SubmitResult> {
        unimplemented!()
    }

    fn find_existing_pr_for_issue(&self, _: u64) -> Result<Option<u64>> {
        Ok(None)
    }

    fn upsert_review_comment(&self, _pr_number: u64, body: &str) -> Result<()> {
        *self.comment_body.lock().unwrap() = body.to_string();
        self.upsert_calls.lock().unwrap().push(body.to_string());
        Ok(())
    }

    fn fetch_pr_comments(&self, _pr_number: u64) -> Result<Vec<PrComment>> {
        self.fetch_count.fetch_add(1, Ordering::SeqCst);
        Ok(vec![make_pr_comment(&self.current_body())])
    }

    fn fetch_comment_by_id(&self, _comment_id: u64) -> Result<PrComment> {
        self.fetch_count.fetch_add(1, Ordering::SeqCst);
        Ok(make_pr_comment(&self.current_body()))
    }
}

/// Test that `run_fix_loop` picks up newly-checked items across poll cycles.
///
/// Cycle 1: "alpha" is checked → fix agent spawned and completes
/// Cycle 2: "beta" becomes checked (simulated) → fix agent spawned and completes
/// After both are done, shutdown is triggered.
#[tokio::test]
async fn test_fix_loop_picks_up_newly_checked_items() {
    let (_bare_dir, repo_dir) = setup_git_repo();
    let repo_root = repo_dir.path();

    let pr_branch = "feature/poll-test";
    create_pr_branch(repo_root, pr_branch);

    let agent_script = create_mock_agent_script(repo_root);

    let findings = vec![make_finding("alpha"), make_finding("beta")];
    // Start with only "alpha" checked; "beta" will be checked after alpha completes
    let initial_comment = make_review_comment(&findings, &["alpha"]);

    let submission = Arc::new(PollingMockSubmission::new(
        initial_comment,
        Some("beta".to_string()),
    ));
    let correction_runner = Arc::new(MockCorrectionRunner);

    let wt_dir = tempfile::TempDir::new().unwrap();
    let mut config = make_config();
    config.fix = make_fix_step_config(agent_script);
    config.worktree_dir = wt_dir.path().to_str().unwrap().to_string();
    config.poll_seconds = 1; // Fast polling for test

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let submission_clone = Arc::clone(&submission);

    // Monitor upsert count and trigger shutdown after both fixes complete
    let shutdown_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            if submission_clone.upsert_count() >= 2 {
                let _ = shutdown_tx.send(true);
                return;
            }
        }
    });

    let result = run_fix_loop(
        42,
        pr_branch,
        &config,
        Arc::clone(&submission),
        &rlph::prompts::PromptEngine::new(None),
        repo_root,
        correction_runner,
        shutdown_rx,
    )
    .await;

    shutdown_handle.abort();

    assert!(result.is_ok(), "run_fix_loop failed: {:?}", result.err());

    // Both items should have been fixed
    assert_eq!(
        submission.upsert_count(),
        2,
        "expected 2 comment updates (one per finding across different poll cycles)"
    );

    // Multiple fetch calls (at least 2 poll cycles)
    let fetches = submission.fetch_count.load(Ordering::SeqCst);
    assert!(
        fetches >= 2,
        "expected at least 2 poll cycles, got {fetches}"
    );
}

/// Test that already-completed items are not re-processed by the polling loop.
#[tokio::test]
async fn test_fix_loop_skips_completed_items() {
    let (_bare_dir, repo_dir) = setup_git_repo();
    let repo_root = repo_dir.path();

    let pr_branch = "feature/skip-test";
    create_pr_branch(repo_root, pr_branch);

    let agent_script = create_mock_agent_script(repo_root);

    let findings = vec![make_finding("only-one")];
    let initial_comment = make_review_comment(&findings, &["only-one"]);

    // No deferred checks — just the one item
    let submission = Arc::new(PollingMockSubmission::new(initial_comment, None));
    let correction_runner = Arc::new(MockCorrectionRunner);

    let wt_dir = tempfile::TempDir::new().unwrap();
    let mut config = make_config();
    config.fix = make_fix_step_config(agent_script);
    config.worktree_dir = wt_dir.path().to_str().unwrap().to_string();
    config.poll_seconds = 1;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let submission_clone = Arc::clone(&submission);

    // Let the loop run for a few cycles after the fix completes, then shutdown.
    // If the item gets re-processed, upsert_count will be > 1.
    let shutdown_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let fetches = submission_clone.fetch_count.load(Ordering::SeqCst);
            // Wait for at least 3 poll cycles after the fix completes
            if submission_clone.upsert_count() >= 1 && fetches >= 4 {
                let _ = shutdown_tx.send(true);
                return;
            }
        }
    });

    let result = run_fix_loop(
        42,
        pr_branch,
        &config,
        Arc::clone(&submission),
        &rlph::prompts::PromptEngine::new(None),
        repo_root,
        correction_runner,
        shutdown_rx,
    )
    .await;

    shutdown_handle.abort();

    assert!(result.is_ok(), "run_fix_loop failed: {:?}", result.err());

    // Item should have been processed exactly once
    assert_eq!(
        submission.upsert_count(),
        1,
        "completed item should not be re-processed"
    );
}

/// Test that `run_fix_loop` gracefully shuts down: waits for in-flight tasks,
/// then exits cleanly.
#[tokio::test]
async fn test_fix_loop_graceful_shutdown() {
    let (_bare_dir, repo_dir) = setup_git_repo();
    let repo_root = repo_dir.path();

    let pr_branch = "feature/shutdown-test";
    create_pr_branch(repo_root, pr_branch);

    // Use a slow agent (sleeps 2 seconds before committing)
    let script_path = repo_root.join("mock-slow-agent.sh");
    let script = r#"#!/bin/bash
sleep 2
ID="$$-$RANDOM"
echo "fix-$ID" > "fix-$ID.txt"
git add .
git commit -m "fix: slow-$ID" 2>/dev/null
echo "{\"type\":\"result\",\"result\":\"{\\\"status\\\":\\\"fixed\\\",\\\"commit_message\\\":\\\"fix: slow-$ID\\\"}\"}"
"#;
    std::fs::write(&script_path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let findings = vec![make_finding("slow-item")];
    let initial_comment = make_review_comment(&findings, &["slow-item"]);

    let submission = Arc::new(PollingMockSubmission::new(initial_comment, None));
    let correction_runner = Arc::new(MockCorrectionRunner);

    let wt_dir = tempfile::TempDir::new().unwrap();
    let mut config = make_config();
    config.fix = make_fix_step_config(script_path.to_str().unwrap().to_string());
    config.worktree_dir = wt_dir.path().to_str().unwrap().to_string();
    config.poll_seconds = 5; // Short poll to avoid long hangs if agent errors before shutdown

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Send shutdown after 1 second (agent takes 2s, so it should be in-flight)
    let shutdown_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let _ = shutdown_tx.send(true);
    });

    let result = run_fix_loop(
        42,
        pr_branch,
        &config,
        Arc::clone(&submission),
        &rlph::prompts::PromptEngine::new(None),
        repo_root,
        correction_runner,
        shutdown_rx,
    )
    .await;

    shutdown_handle.abort();

    assert!(result.is_ok(), "run_fix_loop failed: {:?}", result.err());

    // The slow agent should have completed during graceful shutdown
    assert_eq!(
        submission.upsert_count(),
        1,
        "in-flight fix should complete during graceful shutdown"
    );
}
