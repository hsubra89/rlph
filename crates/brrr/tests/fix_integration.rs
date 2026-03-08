mod common;

use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use brrr::config::{Config, ReviewStepConfig};
use brrr::error::{Error, Result};
use brrr::fix::run_fix_loop;
use brrr::ids::{CommentId, PrNumber, ReactionId};
use brrr::orchestrator::CorrectionRunner;
use brrr::review_schema::ReviewFinding;
use brrr::runner::{RunResult, RunnerKind};
use brrr::submission::{PrReviewComment, Reaction, SubmissionBackend, SubmitResult};
use brrr::test_helpers::{
    make_finding, make_finding_critical, make_finding_info, make_review_comment,
};
use tokio::sync::watch;

use common::{default_test_config, run_git, setup_git_repo};

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

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
fn rocket_reactions(comment_id: CommentId) -> (CommentId, Vec<Reaction>) {
    (
        comment_id,
        vec![Reaction {
            id: ReactionId::new(comment_id.get() * 1000 + 1),
            content: "rocket".to_string(),
        }],
    )
}

fn fixed_reactions(comment_id: CommentId) -> (CommentId, Vec<Reaction>) {
    (
        comment_id,
        vec![Reaction {
            id: ReactionId::new(comment_id.get() * 1000 + 2),
            content: "+1".to_string(),
        }],
    )
}

fn no_reactions(comment_id: CommentId) -> (CommentId, Vec<Reaction>) {
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
        agent_timeout: Some(std::time::Duration::from_secs(30)),
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
    reactions: Mutex<Vec<(CommentId, Vec<Reaction>)>>,
    added_reactions: Mutex<Vec<(CommentId, String)>>,
    deleted_reactions: Mutex<Vec<(CommentId, ReactionId)>>,
    replies: Mutex<Vec<(PrNumber, CommentId, String)>>,
    next_reaction_id: AtomicU64,
}

impl MockFixSubmission {
    fn new(comments: Vec<PrReviewComment>, reactions: Vec<(CommentId, Vec<Reaction>)>) -> Self {
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

    fn added_reactions(&self) -> Vec<(CommentId, String)> {
        self.added_reactions.lock().unwrap().clone()
    }

    fn set_reactions(&self, comment_id: CommentId, new_reactions: Vec<Reaction>) {
        let mut reactions = self.reactions.lock().unwrap();
        if let Some((_, existing_reactions)) =
            reactions.iter_mut().find(|(id, _)| *id == comment_id)
        {
            *existing_reactions = new_reactions;
        } else {
            reactions.push((comment_id, new_reactions));
        }
    }
}

impl SubmissionBackend for MockFixSubmission {
    fn submit(&self, _: &str, _: &str, _: &str, _: &str) -> Result<SubmitResult> {
        unimplemented!("submit not needed for fix tests")
    }

    fn fetch_pr_review_comments(&self, _pr_number: PrNumber) -> Result<Vec<PrReviewComment>> {
        Ok(self.review_comments.lock().unwrap().clone())
    }

    fn list_review_comment_reactions(&self, comment_id: CommentId) -> Result<Vec<Reaction>> {
        let reactions = self.reactions.lock().unwrap();
        Ok(reactions
            .iter()
            .find(|(id, _)| *id == comment_id)
            .map(|(_, r)| r.clone())
            .unwrap_or_default())
    }

    fn add_review_comment_reaction(&self, comment_id: CommentId, reaction: &str) -> Result<()> {
        let new_id = ReactionId::new(self.next_reaction_id.fetch_add(1, Ordering::SeqCst));
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

    fn delete_review_comment_reaction(
        &self,
        comment_id: CommentId,
        reaction_id: ReactionId,
    ) -> Result<()> {
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

    fn reply_to_review_comment(
        &self,
        pr_number: PrNumber,
        comment_id: CommentId,
        body: &str,
    ) -> Result<()> {
        self.replies
            .lock()
            .unwrap()
            .push((pr_number, comment_id, body.to_string()));
        Ok(())
    }

    fn fetch_review_comment_by_id(&self, comment_id: CommentId) -> Result<PrReviewComment> {
        let comments = self.review_comments.lock().unwrap();
        comments
            .iter()
            .find(|c| c.id == comment_id)
            .cloned()
            .ok_or_else(|| Error::Submission(format!("comment {comment_id} not found")))
    }
}

/// Configurable mock correction runner for tests.
///
/// By default returns an error (no-op). Use `with_handler` to supply custom
/// resume logic (e.g. for batch tests that need real commits).
type ResumeHandler = Box<dyn Fn(&Path, usize) -> Result<RunResult> + Send + Sync>;

struct MockCorrectionRunner {
    handler: ResumeHandler,
    call_count: AtomicUsize,
}

impl MockCorrectionRunner {
    /// No-op runner that always returns an error.
    fn noop() -> Self {
        Self {
            handler: Box::new(|_, _| {
                Err(Error::AgentRunner("no-op correction runner".to_string()))
            }),
            call_count: AtomicUsize::new(0),
        }
    }

    /// Runner that delegates to the given handler, passing `(working_dir, call_index)`.
    fn with_handler(
        handler: impl Fn(&Path, usize) -> Result<RunResult> + Send + Sync + 'static,
    ) -> Self {
        Self {
            handler: Box::new(handler),
            call_count: AtomicUsize::new(0),
        }
    }
}

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
        working_dir: &Path,
        _timeout: Option<std::time::Duration>,
        _stream_prefix: Option<&str>,
    ) -> Result<RunResult> {
        let n = self.call_count.fetch_add(1, Ordering::SeqCst);
        (self.handler)(working_dir, n)
    }
}

// --- Tests ---

/// Create a mock agent script that makes a commit and outputs fix JSON.
fn create_mock_agent_script(dir: &Path) -> String {
    create_mock_agent_script_inner(dir, true)
}

/// Create a mock agent script that omits session_id from output.
fn create_mock_agent_script_no_session_id(dir: &Path) -> String {
    create_mock_agent_script_inner(dir, false)
}

fn create_mock_agent_script_inner(dir: &Path, emit_session_id: bool) -> String {
    let script_path = dir.join("mock-fix-agent.sh");
    let session_line = if emit_session_id {
        r#"echo "{\"session_id\":\"mock-session-$ID\"}""#
    } else {
        ""
    };
    let script = format!(
        r#"#!/bin/bash
# Mock fix agent: creates a file, commits it, outputs fix result
ID="$$-$RANDOM"
echo "fix-$ID" > "fix-$ID.txt"
git add .
git commit -m "fix: applied-$ID" 2>/dev/null
{session_line}
echo "{{\"type\":\"result\",\"result\":\"{{\\\"status\\\":\\\"fixed\\\",\\\"commit_message\\\":\\\"fix: applied-$ID\\\"}}\"}}"
"#
    );
    std::fs::write(&script_path, &script).unwrap();
    #[cfg(unix)]
    make_executable(&script_path);
    script_path.to_str().unwrap().to_string()
}

/// Create a mock agent script that returns WontFix with a session_id (no commit).
fn create_mock_wontfix_agent_script(dir: &Path) -> String {
    let script_path = dir.join("mock-wontfix-agent.sh");
    let script = r#"#!/bin/bash
# Mock fix agent: returns WontFix with session_id (no commit)
ID="$$-$RANDOM"
echo "{\"session_id\":\"mock-session-$ID\"}"
echo "{\"type\":\"result\",\"result\":\"{\\\"status\\\":\\\"wont_fix\\\",\\\"reason\\\":\\\"False positive\\\"}\"}"
"#;
    std::fs::write(&script_path, script).unwrap();
    #[cfg(unix)]
    make_executable(&script_path);
    script_path.to_str().unwrap().to_string()
}

/// Test that `run_fix_loop` processes multiple 🚀-reacted items.
#[tokio::test]
async fn test_fix_loop_multiple_queued_items() {
    let findings = vec![
        make_finding_critical("bug-alpha"),
        make_finding_critical("bug-beta"),
        make_finding_critical("bug-gamma"),
    ];
    let mut f = FixLoopFixture::new(
        &findings,
        &[
            CommentId::new(100),
            CommentId::new(101),
            CommentId::new(102),
        ],
        None,
    );

    let shutdown_handle =
        spawn_shutdown_poller(Arc::clone(&f.submission), f.take_shutdown_tx(), 3, None);

    let result = f.run().await;
    shutdown_handle.abort();

    assert!(result.is_ok(), "run_fix_loop failed: {:?}", result.err());

    // Each fix should have: removed 🚀, added 👍, posted reply
    assert_eq!(
        f.submission.deleted_reaction_count(),
        3,
        "expected 3 🚀 reactions removed"
    );
    assert_eq!(
        f.submission.added_reaction_count(),
        3,
        "expected 3 result reactions added"
    );
    assert_eq!(f.submission.reply_count(), 3, "expected 3 replies posted");

    // All added reactions should be "+1" (fixed)
    for (_, reaction) in f.submission.added_reactions() {
        assert_eq!(reaction, "+1");
    }
}

/// Test that `run_fix_loop` with no 🚀-reacted items returns Ok and does nothing.
#[tokio::test]
async fn test_fix_loop_no_queued_items() {
    let findings = vec![make_finding("a"), make_finding("b")];
    let mut f = FixLoopFixture::new(&findings, &[], None);

    // Shutdown after 2 fetches (ensures at least one full cycle with no work)
    let shutdown_handle =
        spawn_shutdown_poller(Arc::clone(&f.submission), f.take_shutdown_tx(), 0, Some(2));

    let result = f.run().await;
    shutdown_handle.abort();

    assert!(result.is_ok());
}

/// Test that worktrees are cleaned up after fixes complete.
#[tokio::test]
async fn test_fix_loop_worktrees_cleaned_up() {
    let findings = vec![
        make_finding_critical("clean-a"),
        make_finding_critical("clean-b"),
    ];
    let mut f = FixLoopFixture::new(&findings, &[CommentId::new(100), CommentId::new(101)], None);

    let shutdown_handle =
        spawn_shutdown_poller(Arc::clone(&f.submission), f.take_shutdown_tx(), 2, None);

    let result = f.run().await;
    shutdown_handle.abort();

    assert!(result.is_ok(), "run_fix_loop failed: {:?}", result.err());

    // After completion, no fix worktree directories should remain
    let wt_entries: Vec<_> = std::fs::read_dir(&f.config.worktree_dir)
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
async fn test_fix_loop_skips_already_fixed_items() {
    let findings = vec![make_finding("a"), make_finding("b")];
    let mut f = FixLoopFixture::new(&findings, &[], None);

    // a is already fixed (has 👍), b has no reactions — neither is queued
    f.submission
        .set_reactions(CommentId::new(100), fixed_reactions(CommentId::new(100)).1);

    let shutdown_handle =
        spawn_shutdown_poller(Arc::clone(&f.submission), f.take_shutdown_tx(), 0, Some(2));

    let result = f.run().await;
    shutdown_handle.abort();

    assert!(result.is_ok());
    // Nothing should have been processed
    assert_eq!(f.submission.reply_count(), 0);
    assert_eq!(f.submission.added_reaction_count(), 0);
}

// --- Polling loop tests ---

/// Mock submission that dynamically adds 🚀 reactions after initial fixes complete.
struct PollingMockSubmission {
    base: MockFixSubmission,
    fetch_count: AtomicUsize,
    /// Finding comment ID to dynamically add 🚀 reaction after the first fix completes.
    deferred_rocket_comment_id: Option<CommentId>,
}

impl PollingMockSubmission {
    fn new(
        comments: Vec<PrReviewComment>,
        reactions: Vec<(CommentId, Vec<Reaction>)>,
        deferred_rocket_comment_id: Option<CommentId>,
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

    fn added_reaction_count(&self) -> usize {
        self.base.added_reaction_count()
    }

    fn deleted_reaction_count(&self) -> usize {
        self.base.deleted_reaction_count()
    }

    fn added_reactions(&self) -> Vec<(CommentId, String)> {
        self.base.added_reactions()
    }

    fn set_reactions(&self, comment_id: CommentId, reactions: Vec<Reaction>) {
        self.base.set_reactions(comment_id, reactions);
    }
}

impl SubmissionBackend for PollingMockSubmission {
    fn submit(&self, _: &str, _: &str, _: &str, _: &str) -> Result<SubmitResult> {
        unimplemented!("submit not needed for fix tests")
    }

    fn fetch_pr_review_comments(&self, pr_number: PrNumber) -> Result<Vec<PrReviewComment>> {
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

    fn list_review_comment_reactions(&self, comment_id: CommentId) -> Result<Vec<Reaction>> {
        self.base.list_review_comment_reactions(comment_id)
    }

    fn add_review_comment_reaction(&self, comment_id: CommentId, reaction: &str) -> Result<()> {
        self.base.add_review_comment_reaction(comment_id, reaction)
    }

    fn delete_review_comment_reaction(
        &self,
        comment_id: CommentId,
        reaction_id: ReactionId,
    ) -> Result<()> {
        self.base
            .delete_review_comment_reaction(comment_id, reaction_id)
    }

    fn reply_to_review_comment(
        &self,
        pr_number: PrNumber,
        comment_id: CommentId,
        body: &str,
    ) -> Result<()> {
        self.base
            .reply_to_review_comment(pr_number, comment_id, body)
    }

    fn fetch_review_comment_by_id(&self, comment_id: CommentId) -> Result<PrReviewComment> {
        self.base.fetch_review_comment_by_id(comment_id)
    }
}

/// Spawn a poller that sends shutdown once reply/fetch thresholds are met.
fn spawn_shutdown_poller(
    submission: Arc<PollingMockSubmission>,
    shutdown_tx: watch::Sender<bool>,
    min_replies: usize,
    min_fetches: Option<usize>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let replies_ok = submission.reply_count() >= min_replies;
            let fetches_ok = min_fetches
                .map(|f| submission.fetch_count.load(Ordering::SeqCst) >= f)
                .unwrap_or(true);
            if replies_ok && fetches_ok {
                let _ = shutdown_tx.send(true);
                return;
            }
        }
    })
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
        queued_comment_ids: &[CommentId],
        deferred_rocket_comment_id: Option<CommentId>,
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
        let correction_runner = Arc::new(MockCorrectionRunner::noop());

        let wt_dir = tempfile::TempDir::new().unwrap();
        let mut config = make_config();
        config.fix = make_fix_step_config(agent_script);
        config.worktree_dir = wt_dir.path().to_str().unwrap().to_string();
        config.poll_seconds = std::time::Duration::from_secs(1);

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
            PrNumber::new(42),
            &self.pr_branch,
            &self.config,
            Arc::clone(&self.submission),
            &brrr::prompts::PromptEngine::new(None),
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
        &[CommentId::new(100)],    // alpha (comment 100) starts queued
        Some(CommentId::new(101)), // beta (comment 101) gets 🚀 after first fix
    );

    let shutdown_handle =
        spawn_shutdown_poller(Arc::clone(&f.submission), f.take_shutdown_tx(), 2, None);

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
        &[CommentId::new(100)], // only-one starts queued
        None,
    );

    let shutdown_handle =
        spawn_shutdown_poller(Arc::clone(&f.submission), f.take_shutdown_tx(), 1, Some(4));

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
        &[CommentId::new(100)], // slow-item starts queued
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
    make_executable(&script_path);
    f.config.fix = make_fix_step_config(script_path.to_str().unwrap().to_string());
    f.config.poll_seconds = std::time::Duration::from_secs(5);

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

/// Test that `run_fix_loop` processes WARNING findings via RunBatch.
///
/// With the no-op correction runner, only the first finding in the batch
/// succeeds (via runner.run()); the second fails on session resume.
#[tokio::test]
async fn test_fix_loop_handles_warning_findings_via_batch() {
    let mut f = FixLoopFixture::new(
        &[make_finding("warn-a"), make_finding("warn-b")],
        &[CommentId::new(100), CommentId::new(101)], // both queued
        None,
    );

    let shutdown_handle =
        spawn_shutdown_poller(Arc::clone(&f.submission), f.take_shutdown_tx(), 1, Some(2));

    let result = f.run().await;
    shutdown_handle.abort();

    assert!(result.is_ok(), "run_fix_loop failed: {:?}", result.err());

    // First batch finding succeeds; second fails on correction_runner.resume()
    assert_eq!(
        f.submission.reply_count(),
        1,
        "first batch finding should be fixed, second fails on session resume"
    );
}

/// Test that `run_fix_loop` processes CRITICAL first, then batches WARNING/INFO.
///
/// With no-op correction runner: CRITICAL runs solo, then batch starts with
/// the first WARNING/INFO (succeeds), second fails on session resume.
#[tokio::test]
async fn test_fix_loop_processes_criticals_then_batches_lower_severity() {
    let mut f = FixLoopFixture::new(
        &[
            make_finding_critical("crit-item"),
            make_finding("warn-item"),
            make_finding_info("info-item"),
        ],
        &[
            CommentId::new(100),
            CommentId::new(101),
            CommentId::new(102),
        ], // all queued
        None,
    );

    let shutdown_handle =
        spawn_shutdown_poller(Arc::clone(&f.submission), f.take_shutdown_tx(), 2, Some(2));

    let result = f.run().await;
    shutdown_handle.abort();

    assert!(result.is_ok(), "run_fix_loop failed: {:?}", result.err());

    // CRITICAL processed solo + first batch finding succeeds
    assert_eq!(
        f.submission.reply_count(),
        2,
        "CRITICAL + first batch finding should be fixed"
    );
}

/// Test full batch session reuse: all WARNING findings processed in one session.
///
/// Uses `MockCorrectionRunner::with_handler` which creates real commits on resume,
/// so all 3 findings complete successfully with commit/push/reaction per finding.
#[tokio::test]
async fn test_fix_loop_batch_full_session_reuse() {
    let (_bare_dir, repo_dir) = setup_git_repo();
    let repo_root = repo_dir.path();

    let pr_branch = "feature/batch-session-test";
    create_pr_branch(repo_root, pr_branch);

    let agent_script = create_mock_agent_script(repo_root);

    let findings = vec![
        make_finding("warn-a"),
        make_finding("warn-b"),
        make_finding("warn-c"),
    ];
    let comments = make_review_comments(&findings);
    let reactions = vec![
        rocket_reactions(CommentId::new(100)),
        rocket_reactions(CommentId::new(101)),
        rocket_reactions(CommentId::new(102)),
    ];

    let submission = Arc::new(PollingMockSubmission::new(comments, reactions, None));
    let correction_runner = Arc::new(MockCorrectionRunner::with_handler(|working_dir, n| {
        let filename = format!("batch-resume-{n}.txt");
        let commit_msg = format!("fix: batch-resume-{n}");

        std::fs::write(working_dir.join(&filename), "batch fix content").unwrap();
        run_git(working_dir, &["add", "."]);
        run_git(working_dir, &["commit", "-m", &commit_msg]);

        Ok(RunResult {
            exit_code: 0,
            stdout: format!(r#"{{"status":"fixed","commit_message":"{commit_msg}"}}"#),
            stderr: String::new(),
            session_id: Some("mock-batch-session".to_string()),
        })
    }));

    let wt_dir = tempfile::TempDir::new().unwrap();
    let mut config = make_config();
    config.fix = make_fix_step_config(agent_script);
    config.worktree_dir = wt_dir.path().to_str().unwrap().to_string();
    config.poll_seconds = std::time::Duration::from_secs(1);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let shutdown_handle = spawn_shutdown_poller(Arc::clone(&submission), shutdown_tx, 3, None);

    let result = run_fix_loop(
        PrNumber::new(42),
        pr_branch,
        &config,
        Arc::clone(&submission),
        &brrr::prompts::PromptEngine::new(None),
        repo_root,
        correction_runner,
        shutdown_rx,
    )
    .await;

    shutdown_handle.abort();

    assert!(result.is_ok(), "run_fix_loop failed: {:?}", result.err());

    // All 3 findings should have been processed in a single batch session
    assert_eq!(
        submission.reply_count(),
        3,
        "expected 3 replies (one per finding in batch)"
    );

    // All reactions should be "+1" (fixed)
    for (_, reaction) in submission.added_reactions() {
        assert_eq!(reaction, "+1");
    }

    // 3 rocket reactions should have been removed (one per finding)
    assert_eq!(
        submission.deleted_reaction_count(),
        3,
        "expected 3 🚀 reactions removed"
    );
}

/// Test that batch abort leaves remaining findings as Queued for next poll.
///
/// 3 WARNING findings batched. First succeeds, second fails on correction
/// resume (no-op runner), third is never attempted. On the next poll cycle,
/// the third finding is picked up as a new single-element batch.
#[tokio::test]
async fn test_fix_loop_batch_abort_remaining_picked_up_next_poll() {
    let mut f = FixLoopFixture::new(
        &[
            make_finding("warn-a"),
            make_finding("warn-b"),
            make_finding("warn-c"),
        ],
        &[
            CommentId::new(100),
            CommentId::new(101),
            CommentId::new(102),
        ], // all queued
        None,
    );

    let shutdown_handle =
        spawn_shutdown_poller(Arc::clone(&f.submission), f.take_shutdown_tx(), 2, Some(3));

    let result = f.run().await;
    shutdown_handle.abort();

    assert!(result.is_ok(), "run_fix_loop failed: {:?}", result.err());

    // First batch: warn-a succeeds, warn-b fails (correction runner error)
    // Second batch (next poll): warn-c runs as single-element batch, succeeds
    // warn-b stays failed
    assert_eq!(
        f.submission.reply_count(),
        2,
        "warn-a + warn-c should be fixed (warn-b failed on correction resume)"
    );
}

/// Test that a WontFix result in a batch marks the finding complete and continues.
///
/// 2 WARNING findings batched. First returns WontFix (via agent script), second
/// returns Fixed (via correction runner resume). Both should be marked complete
/// with appropriate reactions: 😕 for WontFix, 👍 for Fixed.
#[tokio::test]
async fn test_fix_loop_batch_wontfix_continues() {
    let (_bare_dir, repo_dir) = setup_git_repo();
    let repo_root = repo_dir.path();

    let pr_branch = "feature/batch-wontfix-test";
    create_pr_branch(repo_root, pr_branch);

    // Agent script that returns WontFix (no commit needed) with a session_id
    let agent_script = create_mock_wontfix_agent_script(repo_root);

    let findings = vec![make_finding("warn-wontfix"), make_finding("warn-fixable")];
    let comments = make_review_comments(&findings);
    let reactions = vec![
        rocket_reactions(CommentId::new(100)),
        rocket_reactions(CommentId::new(101)),
    ];

    let submission = Arc::new(PollingMockSubmission::new(comments, reactions, None));
    let correction_runner = Arc::new(MockCorrectionRunner::with_handler(|working_dir, n| {
        let filename = format!("batch-resume-{n}.txt");
        let commit_msg = format!("fix: batch-resume-{n}");

        std::fs::write(working_dir.join(&filename), "batch fix content").unwrap();
        run_git(working_dir, &["add", "."]);
        run_git(working_dir, &["commit", "-m", &commit_msg]);

        Ok(RunResult {
            exit_code: 0,
            stdout: format!(r#"{{"status":"fixed","commit_message":"{commit_msg}"}}"#),
            stderr: String::new(),
            session_id: Some("mock-batch-session".to_string()),
        })
    }));

    let wt_dir = tempfile::TempDir::new().unwrap();
    let mut config = make_config();
    config.fix = make_fix_step_config(agent_script);
    config.worktree_dir = wt_dir.path().to_str().unwrap().to_string();
    config.poll_seconds = std::time::Duration::from_secs(1);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let shutdown_handle = spawn_shutdown_poller(Arc::clone(&submission), shutdown_tx, 2, None);

    let result = run_fix_loop(
        PrNumber::new(42),
        pr_branch,
        &config,
        Arc::clone(&submission),
        &brrr::prompts::PromptEngine::new(None),
        repo_root,
        correction_runner,
        shutdown_rx,
    )
    .await;

    shutdown_handle.abort();

    assert!(result.is_ok(), "run_fix_loop failed: {:?}", result.err());

    // Both findings should have been processed
    assert_eq!(
        submission.reply_count(),
        2,
        "expected 2 replies: WontFix + Fixed"
    );

    // Verify reactions: first finding gets "confused" (WontFix), second gets "+1" (Fixed)
    let reactions = submission.added_reactions();
    assert!(
        reactions.iter().any(|(_, r)| r == "confused"),
        "expected 😕 reaction for WontFix finding, got: {reactions:?}"
    );
    assert!(
        reactions.iter().any(|(_, r)| r == "+1"),
        "expected 👍 reaction for Fixed finding, got: {reactions:?}"
    );

    // Both rocket reactions should have been removed
    assert_eq!(
        submission.deleted_reaction_count(),
        2,
        "expected 2 🚀 reactions removed"
    );
}

/// Test that batch aborts on the second finding when the first finding's agent
/// returns no session_id (cannot resume without one).
///
/// 2 WARNING findings batched. First succeeds but returns no session_id,
/// so the second finding cannot resume and the batch aborts. The first
/// non-completed finding (warn-b) is marked as failed.
#[tokio::test]
async fn test_fix_loop_batch_abort_when_no_session_id() {
    let (_bare_dir, repo_dir) = setup_git_repo();
    let repo_root = repo_dir.path();

    let pr_branch = "feature/no-session-id-test";
    create_pr_branch(repo_root, pr_branch);

    // Agent script that does NOT emit session_id
    let agent_script = create_mock_agent_script_no_session_id(repo_root);

    let findings = vec![make_finding("warn-a"), make_finding("warn-b")];
    let comments = make_review_comments(&findings);
    let reactions = vec![
        rocket_reactions(CommentId::new(100)),
        rocket_reactions(CommentId::new(101)),
    ];

    let submission = Arc::new(PollingMockSubmission::new(comments, reactions, None));
    let correction_runner = Arc::new(MockCorrectionRunner::noop());

    let wt_dir = tempfile::TempDir::new().unwrap();
    let mut config = make_config();
    config.fix = make_fix_step_config(agent_script);
    config.worktree_dir = wt_dir.path().to_str().unwrap().to_string();
    config.poll_seconds = std::time::Duration::from_secs(1);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let shutdown_handle = spawn_shutdown_poller(Arc::clone(&submission), shutdown_tx, 1, Some(2));

    let result = run_fix_loop(
        PrNumber::new(42),
        pr_branch,
        &config,
        Arc::clone(&submission),
        &brrr::prompts::PromptEngine::new(None),
        repo_root,
        correction_runner,
        shutdown_rx,
    )
    .await;

    shutdown_handle.abort();

    assert!(result.is_ok(), "run_fix_loop failed: {:?}", result.err());

    // Only warn-a should be fixed; warn-b failed (no session_id to resume)
    assert_eq!(
        submission.reply_count(),
        1,
        "only warn-a should be fixed (warn-b failed: no session_id for resume)"
    );
}

/// Test that rebase conflicts during push trigger agent resume for resolution.
///
/// The mock agent script pushes a conflicting commit to origin (via a subshell
/// clone that writes all output to /dev/null), then makes its own conflicting
/// commit in the worktree. When the push fails and the rebase conflicts,
/// `resolve_conflict_and_push` is invoked and the mock correction runner
/// resolves the conflict.
#[tokio::test]
async fn test_fix_loop_rebase_conflict_triggers_agent_resume() {
    let (bare_dir, repo_dir) = setup_git_repo();
    let repo_root = repo_dir.path();

    let pr_branch = "feature/conflict-test";
    create_pr_branch(repo_root, pr_branch);

    // Pre-create shared.txt on the PR branch so both sides can conflict on it
    run_git(repo_root, &["checkout", pr_branch]);
    std::fs::write(repo_root.join("shared.txt"), "original content").unwrap();
    run_git(repo_root, &["add", "."]);
    run_git(repo_root, &["commit", "-m", "add shared.txt"]);
    run_git(repo_root, &["push", "origin", pr_branch]);
    run_git(repo_root, &["checkout", "main"]);

    // Agent script: pushes a conflicting commit to origin (from a subshell clone
    // with all output redirected to /dev/null), then makes a conflicting commit
    // in the worktree on the same file.
    let bare_path = bare_dir.path().to_str().unwrap().to_string();
    let script_path = repo_root.join("mock-conflict-agent.sh");
    let script = format!(
        r#"#!/bin/bash
ID="$$-$RANDOM"

# Push a conflicting commit to origin from a throwaway clone (subshell, all
# output to /dev/null so it doesn't pollute stdout which the runner parses).
TMPCLONE=$(mktemp -d)
(
  git clone "{bare_path}" "$TMPCLONE"
  cd "$TMPCLONE"
  git checkout feature/conflict-test
  echo "origin-side-change" > shared.txt
  git add shared.txt
  git commit -m "conflict: origin-side"
  git push origin feature/conflict-test
) >/dev/null 2>&1
rm -rf "$TMPCLONE"

# Make a conflicting change in the worktree
echo "worktree-side-change" > shared.txt
git add shared.txt
git commit -m "fix: worktree-side-$ID" >/dev/null 2>&1

echo "{{\"session_id\":\"mock-conflict-session\"}}"
echo "{{\"type\":\"result\",\"result\":\"{{\\\"status\\\":\\\"fixed\\\",\\\"commit_message\\\":\\\"fix: conflict-resolved\\\"}}\"}}"
"#
    );
    std::fs::write(&script_path, &script).unwrap();
    #[cfg(unix)]
    make_executable(&script_path);
    let agent_script = script_path.to_str().unwrap().to_string();

    let findings = vec![make_finding("conflict-finding")];
    let comments = make_review_comments(&findings);
    let reactions = vec![rocket_reactions(CommentId::new(100))];

    let submission = Arc::new(PollingMockSubmission::new(comments, reactions, None));

    // Mock correction runner: resolves merge conflicts by writing a resolution,
    // git-adding, and continuing the rebase.
    let correction_runner = Arc::new(MockCorrectionRunner::with_handler(|working_dir, _n| {
        std::fs::write(working_dir.join("shared.txt"), "resolved content").unwrap();
        run_git(working_dir, &["add", "shared.txt"]);

        // Use Command directly to avoid panic if rebase --continue "fails"
        // (git sometimes exits non-zero for editor-related reasons)
        let output = std::process::Command::new("git")
            .args(["rebase", "--continue"])
            .current_dir(working_dir)
            .env("GIT_EDITOR", "true")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "rebase --continue failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        Ok(RunResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            session_id: Some("mock-conflict-session".to_string()),
        })
    }));

    let wt_dir = tempfile::TempDir::new().unwrap();
    let mut config = make_config();
    config.fix = make_fix_step_config(agent_script);
    config.worktree_dir = wt_dir.path().to_str().unwrap().to_string();
    config.poll_seconds = std::time::Duration::from_secs(1);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let shutdown_handle = spawn_shutdown_poller(Arc::clone(&submission), shutdown_tx, 1, None);

    let result = run_fix_loop(
        PrNumber::new(42),
        pr_branch,
        &config,
        Arc::clone(&submission),
        &brrr::prompts::PromptEngine::new(None),
        repo_root,
        correction_runner,
        shutdown_rx,
    )
    .await;

    shutdown_handle.abort();

    assert!(result.is_ok(), "run_fix_loop failed: {:?}", result.err());

    // The finding should have been fixed (conflict resolved by agent)
    assert_eq!(
        submission.reply_count(),
        1,
        "expected 1 reply (conflict-finding fixed after resolution)"
    );

    // Reaction should be +1 (fixed)
    let reactions = submission.added_reactions();
    assert_eq!(reactions.len(), 1);
    assert_eq!(reactions[0].1, "+1");
}
