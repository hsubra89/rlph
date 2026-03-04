mod common;

use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rlph::config::{Config, ReviewStepConfig};
use rlph::error::{Error, Result};
use rlph::fix::{run_fix, run_fix_loop};
use rlph::orchestrator::CorrectionRunner;
use rlph::review_schema::ReviewFinding;
use rlph::runner::{RunResult, RunnerKind};
use rlph::submission::{PrReviewComment, Reaction, SubmissionBackend, SubmitResult};
use rlph::test_helpers::{
    make_finding, make_finding_critical, make_finding_info, make_review_comment,
};
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

/// Build PrReviewComments from findings, assigning sequential IDs starting at 100.
fn make_review_comments(findings: &[ReviewFinding]) -> Vec<PrReviewComment> {
    findings
        .iter()
        .enumerate()
        .map(|(i, f)| make_review_comment((100 + i) as u64, f))
        .collect()
}

/// Build reactions for a comment: rocket reactions for "queued" items.
fn rocket_reactions(comment_id: u64) -> (u64, Vec<Reaction>) {
    (
        comment_id,
        vec![Reaction {
            id: comment_id * 1000 + 1,
            content: "rocket".to_string(),
        }],
    )
}

fn fixed_reactions(comment_id: u64) -> (u64, Vec<Reaction>) {
    (
        comment_id,
        vec![Reaction {
            id: comment_id * 1000 + 2,
            content: "+1".to_string(),
        }],
    )
}

fn no_reactions(comment_id: u64) -> (u64, Vec<Reaction>) {
    (comment_id, vec![])
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
        agent_timeout_retries: 0,
        ..default_test_config()
    }
}

/// Mock submission with reaction-based fix workflow.
///
/// Stores review comments and their reactions. Tracks reaction additions/removals
/// and reply posts.
struct MockFixSubmission {
    review_comments: Mutex<Vec<PrReviewComment>>,
    reactions: Mutex<Vec<(u64, Vec<Reaction>)>>,
    added_reactions: Mutex<Vec<(u64, String)>>,
    deleted_reactions: Mutex<Vec<(u64, u64)>>,
    replies: Mutex<Vec<(u64, u64, String)>>,
    next_reaction_id: AtomicU64,
}

impl MockFixSubmission {
    fn new(comments: Vec<PrReviewComment>, reactions: Vec<(u64, Vec<Reaction>)>) -> Self {
        Self {
            review_comments: Mutex::new(comments),
            reactions: Mutex::new(reactions),
            added_reactions: Mutex::new(Vec::new()),
            deleted_reactions: Mutex::new(Vec::new()),
            replies: Mutex::new(Vec::new()),
            next_reaction_id: AtomicU64::new(9000),
        }
    }

    fn added_reaction_count(&self) -> usize {
        self.added_reactions.lock().unwrap().len()
    }

    fn deleted_reaction_count(&self) -> usize {
        self.deleted_reactions.lock().unwrap().len()
    }

    fn reply_count(&self) -> usize {
        self.replies.lock().unwrap().len()
    }

    fn added_reactions(&self) -> Vec<(u64, String)> {
        self.added_reactions.lock().unwrap().clone()
    }
}

impl SubmissionBackend for MockFixSubmission {
    fn submit(&self, _: &str, _: &str, _: &str, _: &str) -> Result<SubmitResult> {
        unimplemented!("submit not needed for fix tests")
    }

    fn fetch_pr_review_comments(&self, _pr_number: u64) -> Result<Vec<PrReviewComment>> {
        Ok(self.review_comments.lock().unwrap().clone())
    }

    fn list_review_comment_reactions(&self, comment_id: u64) -> Result<Vec<Reaction>> {
        let reactions = self.reactions.lock().unwrap();
        Ok(reactions
            .iter()
            .find(|(id, _)| *id == comment_id)
            .map(|(_, r)| r.clone())
            .unwrap_or_default())
    }

    fn add_review_comment_reaction(&self, comment_id: u64, reaction: &str) -> Result<()> {
        let new_id = self.next_reaction_id.fetch_add(1, Ordering::SeqCst);
        self.added_reactions
            .lock()
            .unwrap()
            .push((comment_id, reaction.to_string()));

        // Also update the reactions store so subsequent fetches see the new state
        let mut reactions = self.reactions.lock().unwrap();
        let entry = reactions.iter_mut().find(|(id, _)| *id == comment_id);
        if let Some((_, list)) = entry {
            list.push(Reaction {
                id: new_id,
                content: reaction.to_string(),
            });
        } else {
            reactions.push((
                comment_id,
                vec![Reaction {
                    id: new_id,
                    content: reaction.to_string(),
                }],
            ));
        }
        Ok(())
    }

    fn delete_review_comment_reaction(&self, comment_id: u64, reaction_id: u64) -> Result<()> {
        self.deleted_reactions
            .lock()
            .unwrap()
            .push((comment_id, reaction_id));

        // Also update the reactions store
        let mut reactions = self.reactions.lock().unwrap();
        if let Some((_, list)) = reactions.iter_mut().find(|(id, _)| *id == comment_id) {
            list.retain(|r| r.id != reaction_id);
        }
        Ok(())
    }

    fn reply_to_review_comment(&self, pr_number: u64, comment_id: u64, body: &str) -> Result<()> {
        self.replies
            .lock()
            .unwrap()
            .push((pr_number, comment_id, body.to_string()));
        Ok(())
    }

    fn fetch_review_comment_by_id(&self, comment_id: u64) -> Result<PrReviewComment> {
        let comments = self.review_comments.lock().unwrap();
        comments
            .iter()
            .find(|c| c.id == comment_id)
            .cloned()
            .ok_or_else(|| Error::Submission(format!("comment {comment_id} not found")))
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

/// Test that `run_fix` processes multiple 🚀-reacted items.
#[tokio::test]
async fn test_parallel_fix_multiple_queued_items() {
    let (_bare_dir, repo_dir) = setup_git_repo();
    let repo_root = repo_dir.path();

    let pr_branch = "feature/test-pr";
    create_pr_branch(repo_root, pr_branch);

    let agent_script = create_mock_agent_script(repo_root);

    let findings = vec![
        make_finding("bug-alpha"),
        make_finding("bug-beta"),
        make_finding("bug-gamma"),
    ];
    let comments = make_review_comments(&findings);
    let reactions = vec![
        rocket_reactions(100), // bug-alpha queued
        rocket_reactions(101), // bug-beta queued
        rocket_reactions(102), // bug-gamma queued
    ];

    let submission = Arc::new(MockFixSubmission::new(comments, reactions));
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

    // Each fix should have: removed 🚀, added 👍, posted reply
    assert_eq!(
        submission.deleted_reaction_count(),
        3,
        "expected 3 🚀 reactions removed"
    );
    assert_eq!(
        submission.added_reaction_count(),
        3,
        "expected 3 result reactions added"
    );
    assert_eq!(submission.reply_count(), 3, "expected 3 replies posted");

    // All added reactions should be "+1" (fixed)
    for (_, reaction) in submission.added_reactions() {
        assert_eq!(reaction, "+1");
    }
}

/// Test that `run_fix` with no 🚀-reacted items returns Ok and does nothing.
#[tokio::test]
async fn test_fix_no_queued_items() {
    let (_bare_dir, repo_dir) = setup_git_repo();
    let repo_root = repo_dir.path();

    let findings = vec![make_finding("a"), make_finding("b")];
    let comments = make_review_comments(&findings);
    // No reactions at all
    let reactions = vec![no_reactions(100), no_reactions(101)];

    let submission = Arc::new(MockFixSubmission::new(comments, reactions));
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
    let comments = make_review_comments(&findings);
    let reactions = vec![rocket_reactions(100), rocket_reactions(101)];

    let submission = Arc::new(MockFixSubmission::new(comments, reactions));
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

/// Test that already-fixed items (with 👍 reaction) are skipped.
#[tokio::test]
async fn test_fix_skips_already_fixed_items() {
    let (_bare_dir, repo_dir) = setup_git_repo();
    let repo_root = repo_dir.path();

    let findings = vec![make_finding("a"), make_finding("b")];
    let comments = make_review_comments(&findings);
    // a is already fixed (has 👍), b has no reactions
    let reactions = vec![fixed_reactions(100), no_reactions(101)];

    let submission = Arc::new(MockFixSubmission::new(comments, reactions));
    let correction_runner = Arc::new(MockCorrectionRunner);

    let config = make_config();

    let result = run_fix(
        42,
        "main",
        &config,
        Arc::clone(&submission),
        &rlph::prompts::PromptEngine::new(None),
        repo_root,
        correction_runner,
    )
    .await;

    assert!(result.is_ok());
    // Nothing should have been processed
    assert_eq!(submission.reply_count(), 0);
    assert_eq!(submission.added_reaction_count(), 0);
}

// --- Polling loop tests ---

/// Mock submission that dynamically adds 🚀 reactions after initial fixes complete.
struct PollingMockSubmission {
    base: MockFixSubmission,
    fetch_count: AtomicUsize,
    /// Finding comment ID to dynamically add 🚀 reaction after the first fix completes.
    deferred_rocket_comment_id: Option<u64>,
}

impl PollingMockSubmission {
    fn new(
        comments: Vec<PrReviewComment>,
        reactions: Vec<(u64, Vec<Reaction>)>,
        deferred_rocket_comment_id: Option<u64>,
    ) -> Self {
        Self {
            base: MockFixSubmission::new(comments, reactions),
            fetch_count: AtomicUsize::new(0),
            deferred_rocket_comment_id,
        }
    }

    fn reply_count(&self) -> usize {
        self.base.reply_count()
    }
}

impl SubmissionBackend for PollingMockSubmission {
    fn submit(&self, _: &str, _: &str, _: &str, _: &str) -> Result<SubmitResult> {
        unimplemented!("submit not needed for fix tests")
    }

    fn fetch_pr_review_comments(&self, pr_number: u64) -> Result<Vec<PrReviewComment>> {
        self.fetch_count.fetch_add(1, Ordering::SeqCst);

        // After the first fix completes (reply posted), add 🚀 to deferred comment
        if let Some(comment_id) = self.deferred_rocket_comment_id
            && self.base.reply_count() >= 1
        {
            // Add rocket reaction if not already present
            let reactions = self.base.reactions.lock().unwrap();
            let has_rocket = reactions
                .iter()
                .find(|(id, _)| *id == comment_id)
                .map(|(_, r)| r.iter().any(|rx| rx.content == "rocket"))
                .unwrap_or(false);
            drop(reactions);

            if !has_rocket {
                let _ = self.base.add_review_comment_reaction(comment_id, "rocket");
            }
        }

        self.base.fetch_pr_review_comments(pr_number)
    }

    fn list_review_comment_reactions(&self, comment_id: u64) -> Result<Vec<Reaction>> {
        self.base.list_review_comment_reactions(comment_id)
    }

    fn add_review_comment_reaction(&self, comment_id: u64, reaction: &str) -> Result<()> {
        self.base.add_review_comment_reaction(comment_id, reaction)
    }

    fn delete_review_comment_reaction(&self, comment_id: u64, reaction_id: u64) -> Result<()> {
        self.base
            .delete_review_comment_reaction(comment_id, reaction_id)
    }

    fn reply_to_review_comment(&self, pr_number: u64, comment_id: u64, body: &str) -> Result<()> {
        self.base
            .reply_to_review_comment(pr_number, comment_id, body)
    }

    fn fetch_review_comment_by_id(&self, comment_id: u64) -> Result<PrReviewComment> {
        self.base.fetch_review_comment_by_id(comment_id)
    }
}

/// Test fixture for `run_fix_loop` tests.
struct FixLoopFixture {
    _bare_dir: tempfile::TempDir,
    repo_dir: tempfile::TempDir,
    _wt_dir: tempfile::TempDir,
    submission: Arc<PollingMockSubmission>,
    correction_runner: Arc<MockCorrectionRunner>,
    config: Config,
    pr_branch: String,
    shutdown_tx: Option<watch::Sender<bool>>,
    shutdown_rx: Option<watch::Receiver<bool>>,
}

impl FixLoopFixture {
    fn new(
        findings: &[ReviewFinding],
        queued_comment_ids: &[u64],
        deferred_rocket_comment_id: Option<u64>,
    ) -> Self {
        let (_bare_dir, repo_dir) = setup_git_repo();
        let repo_root = repo_dir.path();

        let pr_branch = "feature/fix-loop-test";
        create_pr_branch(repo_root, pr_branch);

        let agent_script = create_mock_agent_script(repo_root);

        let comments = make_review_comments(findings);
        let reactions: Vec<_> = comments
            .iter()
            .map(|c| {
                if queued_comment_ids.contains(&c.id) {
                    rocket_reactions(c.id)
                } else {
                    no_reactions(c.id)
                }
            })
            .collect();

        let submission = Arc::new(PollingMockSubmission::new(
            comments,
            reactions,
            deferred_rocket_comment_id,
        ));
        let correction_runner = Arc::new(MockCorrectionRunner);

        let wt_dir = tempfile::TempDir::new().unwrap();
        let mut config = make_config();
        config.fix = make_fix_step_config(agent_script);
        config.worktree_dir = wt_dir.path().to_str().unwrap().to_string();
        config.poll_seconds = 1;

        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        Self {
            _bare_dir,
            repo_dir,
            _wt_dir: wt_dir,
            submission,
            correction_runner,
            config,
            pr_branch: pr_branch.to_string(),
            shutdown_tx: Some(shutdown_tx),
            shutdown_rx: Some(shutdown_rx),
        }
    }

    fn repo_root(&self) -> &Path {
        self.repo_dir.path()
    }

    fn take_shutdown_tx(&mut self) -> watch::Sender<bool> {
        self.shutdown_tx.take().expect("shutdown_tx already taken")
    }

    async fn run(&mut self) -> Result<()> {
        let shutdown_rx = self.shutdown_rx.take().expect("shutdown_rx already taken");
        run_fix_loop(
            42,
            &self.pr_branch,
            &self.config,
            Arc::clone(&self.submission),
            &rlph::prompts::PromptEngine::new(None),
            self.repo_root(),
            Arc::clone(&self.correction_runner),
            shutdown_rx,
        )
        .await
    }
}

/// Test that `run_fix_loop` picks up newly 🚀-reacted items across poll cycles.
#[tokio::test]
async fn test_fix_loop_picks_up_newly_queued_items() {
    // alpha starts with 🚀, beta gets 🚀 after alpha is fixed
    let mut f = FixLoopFixture::new(
        &[
            make_finding_critical("alpha"),
            make_finding_critical("beta"),
        ],
        &[100],    // alpha (comment 100) starts queued
        Some(101), // beta (comment 101) gets 🚀 after first fix
    );

    let submission = Arc::clone(&f.submission);
    let shutdown_tx = f.take_shutdown_tx();
    let shutdown_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            if submission.reply_count() >= 2 {
                let _ = shutdown_tx.send(true);
                return;
            }
        }
    });

    let result = f.run().await;
    shutdown_handle.abort();

    assert!(result.is_ok(), "run_fix_loop failed: {:?}", result.err());

    // Both items should have been fixed (2 replies)
    assert_eq!(
        f.submission.reply_count(),
        2,
        "expected 2 replies (one per finding across different poll cycles)"
    );

    // Multiple fetch calls (at least 2 poll cycles)
    let fetches = f.submission.fetch_count.load(Ordering::SeqCst);
    assert!(
        fetches >= 2,
        "expected at least 2 poll cycles, got {fetches}"
    );
}

/// Test that already-completed items are not re-processed by the polling loop.
#[tokio::test]
async fn test_fix_loop_skips_completed_items() {
    let mut f = FixLoopFixture::new(
        &[make_finding_critical("only-one")],
        &[100], // only-one starts queued
        None,
    );

    let submission = Arc::clone(&f.submission);
    let shutdown_tx = f.take_shutdown_tx();
    let shutdown_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let fetches = submission.fetch_count.load(Ordering::SeqCst);
            if submission.reply_count() >= 1 && fetches >= 4 {
                let _ = shutdown_tx.send(true);
                return;
            }
        }
    });

    let result = f.run().await;
    shutdown_handle.abort();

    assert!(result.is_ok(), "run_fix_loop failed: {:?}", result.err());

    // Item should have been processed exactly once
    assert_eq!(
        f.submission.reply_count(),
        1,
        "completed item should not be re-processed"
    );
}

/// Test that `run_fix_loop` gracefully shuts down.
#[tokio::test]
async fn test_fix_loop_graceful_shutdown() {
    let mut f = FixLoopFixture::new(
        &[make_finding_critical("slow-item")],
        &[100], // slow-item starts queued
        None,
    );

    // Override with a slow agent (sleeps 2 seconds before committing)
    let script_path = f.repo_root().join("mock-slow-agent.sh");
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
    f.config.fix = make_fix_step_config(script_path.to_str().unwrap().to_string());
    f.config.poll_seconds = 5;

    let shutdown_tx = f.take_shutdown_tx();
    let shutdown_handle = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let _ = shutdown_tx.send(true);
    });

    let result = f.run().await;
    shutdown_handle.abort();

    assert!(result.is_ok(), "run_fix_loop failed: {:?}", result.err());

    // The slow agent should have completed during graceful shutdown
    assert_eq!(
        f.submission.reply_count(),
        1,
        "in-flight fix should complete during graceful shutdown"
    );
}

/// Test that `run_fix_loop` gracefully handles WARNING-only findings (RunBatch path).
#[tokio::test]
async fn test_fix_loop_handles_warning_findings_gracefully() {
    let mut f = FixLoopFixture::new(
        &[make_finding("warn-a"), make_finding("warn-b")],
        &[100, 101], // both queued
        None,
    );

    let submission = Arc::clone(&f.submission);
    let shutdown_tx = f.take_shutdown_tx();
    let shutdown_handle = tokio::spawn(async move {
        // Wait for at least 2 poll cycles to confirm loop doesn't crash
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let fetches = submission.fetch_count.load(Ordering::SeqCst);
            if fetches >= 2 {
                let _ = shutdown_tx.send(true);
                return;
            }
        }
    });

    let result = f.run().await;
    shutdown_handle.abort();

    assert!(result.is_ok(), "run_fix_loop failed: {:?}", result.err());

    // RunBatch path warns and breaks — no replies expected
    assert_eq!(
        f.submission.reply_count(),
        0,
        "WARNING findings should be skipped (RunBatch not yet implemented)"
    );
}

/// Test that `run_fix_loop` processes CRITICAL findings and gracefully skips WARNING/INFO.
#[tokio::test]
async fn test_fix_loop_processes_criticals_skips_lower_severity() {
    let mut f = FixLoopFixture::new(
        &[
            make_finding_critical("crit-item"),
            make_finding("warn-item"),
            make_finding_info("info-item"),
        ],
        &[100, 101, 102], // all queued
        None,
    );

    let submission = Arc::clone(&f.submission);
    let shutdown_tx = f.take_shutdown_tx();
    let shutdown_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let fetches = submission.fetch_count.load(Ordering::SeqCst);
            // After critical is fixed (1 reply) and at least 2 poll cycles
            if submission.reply_count() >= 1 && fetches >= 2 {
                let _ = shutdown_tx.send(true);
                return;
            }
        }
    });

    let result = f.run().await;
    shutdown_handle.abort();

    assert!(result.is_ok(), "run_fix_loop failed: {:?}", result.err());

    // Only the CRITICAL finding should have been processed
    assert_eq!(
        f.submission.reply_count(),
        1,
        "only CRITICAL finding should be fixed (WARNING/INFO skipped via RunBatch)"
    );
}
