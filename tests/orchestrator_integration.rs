use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use rlph::config::Config;
use rlph::error::{Error, Result};
use rlph::orchestrator::Orchestrator;
use rlph::prompts::PromptEngine;
use rlph::runner::{AgentRunner, Phase, RunResult};
use rlph::sources::{Task, TaskSource};
use rlph::state::StateManager;
use rlph::submission::{SubmissionBackend, SubmitResult};
use rlph::worktree::WorktreeManager;

// --- Shared tracking state ---

#[derive(Default)]
struct SourceTracker {
    marked_in_progress: Vec<String>,
    marked_done: Vec<String>,
}

#[derive(Default)]
struct SubmissionTracker {
    submissions: Vec<(String, String, String, String)>,
}

// --- Mock implementations ---

struct MockSource {
    tasks: Vec<Task>,
    task_details: HashMap<String, Task>,
    tracker: Arc<Mutex<SourceTracker>>,
}

impl MockSource {
    fn new(tasks: Vec<Task>, tracker: Arc<Mutex<SourceTracker>>) -> Self {
        let task_details: HashMap<String, Task> =
            tasks.iter().map(|t| (t.id.clone(), t.clone())).collect();
        Self {
            tasks,
            task_details,
            tracker,
        }
    }
}

impl TaskSource for MockSource {
    fn fetch_eligible_tasks(&self) -> Result<Vec<Task>> {
        Ok(self.tasks.clone())
    }

    fn mark_in_progress(&self, task_id: &str) -> Result<()> {
        self.tracker
            .lock()
            .unwrap()
            .marked_in_progress
            .push(task_id.to_string());
        Ok(())
    }

    fn mark_done(&self, task_id: &str) -> Result<()> {
        self.tracker
            .lock()
            .unwrap()
            .marked_done
            .push(task_id.to_string());
        Ok(())
    }

    fn get_task_details(&self, task_id: &str) -> Result<Task> {
        self.task_details
            .get(task_id)
            .cloned()
            .ok_or_else(|| Error::TaskSource(format!("task not found: {task_id}")))
    }
}

struct MockRunner {
    task_id: String,
}

impl MockRunner {
    fn new(task_id: &str) -> Self {
        Self {
            task_id: task_id.to_string(),
        }
    }
}

impl AgentRunner for MockRunner {
    async fn run(&self, phase: Phase, _prompt: &str, working_dir: &Path) -> Result<RunResult> {
        match phase {
            Phase::Choose => {
                let ralph_dir = working_dir.join(".ralph");
                std::fs::create_dir_all(&ralph_dir)
                    .map_err(|e| Error::AgentRunner(e.to_string()))?;
                std::fs::write(
                    ralph_dir.join("task.toml"),
                    format!("id = \"{}\"", self.task_id),
                )
                .map_err(|e| Error::AgentRunner(e.to_string()))?;
                Ok(RunResult {
                    exit_code: 0,
                    stdout: "Selected task".into(),
                    stderr: String::new(),
                })
            }
            Phase::Implement => Ok(RunResult {
                exit_code: 0,
                stdout: "IMPLEMENTATION_COMPLETE: done".into(),
                stderr: String::new(),
            }),
            Phase::Review => Ok(RunResult {
                exit_code: 0,
                stdout: "REVIEW_COMPLETE: no changes needed".into(),
                stderr: String::new(),
            }),
        }
    }
}

/// Runner whose choose phase reports an existing PR number.
struct ExistingPrRunner {
    task_id: String,
    pr_number: u64,
}

impl AgentRunner for ExistingPrRunner {
    async fn run(&self, phase: Phase, _prompt: &str, working_dir: &Path) -> Result<RunResult> {
        match phase {
            Phase::Choose => {
                let ralph_dir = working_dir.join(".ralph");
                std::fs::create_dir_all(&ralph_dir)
                    .map_err(|e| Error::AgentRunner(e.to_string()))?;
                std::fs::write(
                    ralph_dir.join("task.toml"),
                    format!(
                        "id = \"{}\"\ngithubPrNumber = {}",
                        self.task_id, self.pr_number
                    ),
                )
                .map_err(|e| Error::AgentRunner(e.to_string()))?;
                Ok(RunResult {
                    exit_code: 0,
                    stdout: "Selected task".into(),
                    stderr: String::new(),
                })
            }
            Phase::Implement => Ok(RunResult {
                exit_code: 0,
                stdout: "IMPLEMENTATION_COMPLETE: done".into(),
                stderr: String::new(),
            }),
            Phase::Review => Ok(RunResult {
                exit_code: 0,
                stdout: "REVIEW_COMPLETE: ok".into(),
                stderr: String::new(),
            }),
        }
    }
}

/// Runner that fails at a specific phase.
struct FailAtPhaseRunner {
    fail_at: Phase,
    task_id: String,
}

impl AgentRunner for FailAtPhaseRunner {
    async fn run(&self, phase: Phase, _prompt: &str, working_dir: &Path) -> Result<RunResult> {
        if phase == self.fail_at {
            return Err(Error::AgentRunner(format!("mock failure at {phase}")));
        }
        match phase {
            Phase::Choose => {
                let ralph_dir = working_dir.join(".ralph");
                std::fs::create_dir_all(&ralph_dir)
                    .map_err(|e| Error::AgentRunner(e.to_string()))?;
                std::fs::write(
                    ralph_dir.join("task.toml"),
                    format!("id = \"{}\"", self.task_id),
                )
                .map_err(|e| Error::AgentRunner(e.to_string()))?;
                Ok(RunResult {
                    exit_code: 0,
                    stdout: "Selected".into(),
                    stderr: String::new(),
                })
            }
            Phase::Implement => Ok(RunResult {
                exit_code: 0,
                stdout: "IMPLEMENTATION_COMPLETE: done".into(),
                stderr: String::new(),
            }),
            Phase::Review => Ok(RunResult {
                exit_code: 0,
                stdout: "REVIEW_COMPLETE: ok".into(),
                stderr: String::new(),
            }),
        }
    }
}

struct MockSubmission {
    tracker: Arc<Mutex<SubmissionTracker>>,
}

impl MockSubmission {
    fn new(tracker: Arc<Mutex<SubmissionTracker>>) -> Self {
        Self { tracker }
    }
}

impl SubmissionBackend for MockSubmission {
    fn submit(&self, branch: &str, base: &str, title: &str, body: &str) -> Result<SubmitResult> {
        self.tracker.lock().unwrap().submissions.push((
            branch.to_string(),
            base.to_string(),
            title.to_string(),
            body.to_string(),
        ));
        Ok(SubmitResult {
            url: "https://github.com/test/repo/pull/1".to_string(),
        })
    }
}

/// Runner whose review phase never emits REVIEW_COMPLETE.
struct ReviewNeverCompleteRunner {
    task_id: String,
}

impl AgentRunner for ReviewNeverCompleteRunner {
    async fn run(&self, phase: Phase, _prompt: &str, working_dir: &Path) -> Result<RunResult> {
        match phase {
            Phase::Choose => {
                let ralph_dir = working_dir.join(".ralph");
                std::fs::create_dir_all(&ralph_dir)
                    .map_err(|e| Error::AgentRunner(e.to_string()))?;
                std::fs::write(
                    ralph_dir.join("task.toml"),
                    format!("id = \"{}\"", self.task_id),
                )
                .map_err(|e| Error::AgentRunner(e.to_string()))?;
                Ok(RunResult {
                    exit_code: 0,
                    stdout: "Selected task".into(),
                    stderr: String::new(),
                })
            }
            Phase::Implement => Ok(RunResult {
                exit_code: 0,
                stdout: "IMPLEMENTATION_COMPLETE: done".into(),
                stderr: String::new(),
            }),
            Phase::Review => Ok(RunResult {
                exit_code: 0,
                stdout: "Some review feedback but not complete".into(),
                stderr: String::new(),
            }),
        }
    }
}

struct FailSubmission;

impl SubmissionBackend for FailSubmission {
    fn submit(&self, _: &str, _: &str, _: &str, _: &str) -> Result<SubmitResult> {
        Err(Error::Submission("mock submission failure".to_string()))
    }
}

// --- Test helpers ---

fn make_task(number: u64, title: &str) -> Task {
    Task {
        id: number.to_string(),
        title: title.to_string(),
        body: format!("Body for {title}"),
        labels: vec!["todo".to_string()],
        url: format!("https://github.com/test/repo/issues/{number}"),
        priority: None,
    }
}

fn make_config(dry_run: bool) -> Config {
    Config {
        source: "github".to_string(),
        runner: "bare".to_string(),
        submission: "github".to_string(),
        label: "rlph".to_string(),
        poll_interval: 60,
        worktree_dir: String::new(),
        max_iterations: None,
        dry_run,
        once: true,
        continuous: false,
        agent_binary: "claude".to_string(),
        agent_model: None,
        agent_timeout: None,
        max_review_rounds: 3,
    }
}

/// Set up a git repo with a bare remote for testing.
fn setup_git_repo() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
    let bare_dir = tempfile::TempDir::new().unwrap();
    run_git(bare_dir.path(), &["init", "--bare"]);

    let repo_dir = tempfile::TempDir::new().unwrap();
    run_git(repo_dir.path(), &["init"]);
    run_git(repo_dir.path(), &["config", "user.email", "test@test.com"]);
    run_git(repo_dir.path(), &["config", "user.name", "Test"]);
    run_git(repo_dir.path(), &["commit", "--allow-empty", "-m", "init"]);
    run_git(repo_dir.path(), &["branch", "-M", "main"]);
    run_git(
        repo_dir.path(),
        &["remote", "add", "origin", bare_dir.path().to_str().unwrap()],
    );
    run_git(repo_dir.path(), &["push", "-u", "origin", "main"]);

    let wt_dir = tempfile::TempDir::new().unwrap();

    (bare_dir, repo_dir, wt_dir)
}

fn run_git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

// --- Tests ---

#[tokio::test]
async fn test_full_loop_dry_run() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix the bug");

    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));

    let source = MockSource::new(vec![task], Arc::clone(&source_tracker));
    let runner = MockRunner::new("gh-42");
    let submission = MockSubmission::new(Arc::clone(&sub_tracker));
    let worktree_mgr =
        WorktreeManager::new(repo_dir.path().to_path_buf(), wt_dir.path().to_path_buf());
    let state_dir = repo_dir.path().join(".rlph-test-state");
    let state_mgr = StateManager::new(&state_dir);
    let prompt_engine = PromptEngine::new(None);

    let orchestrator = Orchestrator::new(
        source,
        runner,
        submission,
        worktree_mgr,
        state_mgr,
        prompt_engine,
        make_config(true), // dry_run
        repo_dir.path().to_path_buf(),
    );

    orchestrator.run_once().await.unwrap();

    // In dry_run, source should NOT be marked
    let tracker = source_tracker.lock().unwrap();
    assert!(tracker.marked_in_progress.is_empty());
    assert!(tracker.marked_done.is_empty());
    drop(tracker);

    // State should be completed
    let state_mgr = StateManager::new(&state_dir);
    let state = state_mgr.load();
    assert!(state.current_task.is_none());
    assert_eq!(state.history.len(), 1);
    assert_eq!(state.history[0].id, "gh-42");

    // .ralph/task.toml should be cleaned up
    assert!(!repo_dir.path().join(".ralph").join("task.toml").exists());
}

#[tokio::test]
async fn test_full_loop_with_push() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix the bug");

    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));

    let source = MockSource::new(vec![task], Arc::clone(&source_tracker));
    let runner = MockRunner::new("gh-42");
    let submission = MockSubmission::new(Arc::clone(&sub_tracker));
    let worktree_mgr =
        WorktreeManager::new(repo_dir.path().to_path_buf(), wt_dir.path().to_path_buf());
    let state_dir = repo_dir.path().join(".rlph-test-state");
    let state_mgr = StateManager::new(&state_dir);
    let prompt_engine = PromptEngine::new(None);

    let orchestrator = Orchestrator::new(
        source,
        runner,
        submission,
        worktree_mgr,
        state_mgr,
        prompt_engine,
        make_config(false), // not dry_run
        repo_dir.path().to_path_buf(),
    );

    orchestrator.run_once().await.unwrap();

    // Source should be marked in-progress and done
    let tracker = source_tracker.lock().unwrap();
    assert_eq!(tracker.marked_in_progress, vec!["42".to_string()]);
    assert_eq!(tracker.marked_done, vec!["42".to_string()]);
    drop(tracker);

    // Submission should have been called
    let subs = sub_tracker.lock().unwrap();
    assert_eq!(subs.submissions.len(), 1);
    assert!(subs.submissions[0].0.contains("rlph-42")); // branch name
    assert_eq!(subs.submissions[0].1, "main"); // base
    assert_eq!(subs.submissions[0].2, "Fix the bug"); // title
    assert!(subs.submissions[0].3.contains("Resolves #42")); // body
    drop(subs);

    // Branch should exist on remote
    let output = Command::new("git")
        .args(["branch", "-r"])
        .current_dir(repo_dir.path())
        .output()
        .unwrap();
    let branches = String::from_utf8_lossy(&output.stdout);
    assert!(
        branches.contains("rlph-42"),
        "remote branch not found: {branches}"
    );
}

#[tokio::test]
async fn test_no_eligible_tasks() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();

    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));
    let source = MockSource::new(vec![], Arc::clone(&source_tracker));

    let orchestrator = Orchestrator::new(
        source,
        MockRunner::new("gh-1"),
        MockSubmission::new(Arc::clone(&sub_tracker)),
        WorktreeManager::new(repo_dir.path().to_path_buf(), wt_dir.path().to_path_buf()),
        StateManager::new(repo_dir.path().join(".rlph-test-state")),
        PromptEngine::new(None),
        make_config(true),
        repo_dir.path().to_path_buf(),
    );

    // Should succeed without doing anything
    orchestrator.run_once().await.unwrap();
}

#[tokio::test]
async fn test_error_at_choose_phase() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let runner = FailAtPhaseRunner {
        fail_at: Phase::Choose,
        task_id: "gh-42".to_string(),
    };

    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));

    let orchestrator = Orchestrator::new(
        MockSource::new(vec![task], Arc::clone(&source_tracker)),
        runner,
        MockSubmission::new(Arc::clone(&sub_tracker)),
        WorktreeManager::new(repo_dir.path().to_path_buf(), wt_dir.path().to_path_buf()),
        StateManager::new(repo_dir.path().join(".rlph-test-state")),
        PromptEngine::new(None),
        make_config(true),
        repo_dir.path().to_path_buf(),
    );

    let err = orchestrator.run_once().await.unwrap_err();
    assert!(err.to_string().contains("mock failure at choose"));
}

#[tokio::test]
async fn test_error_at_implement_phase() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let runner = FailAtPhaseRunner {
        fail_at: Phase::Implement,
        task_id: "gh-42".to_string(),
    };

    let state_dir = repo_dir.path().join(".rlph-test-state");
    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));

    let orchestrator = Orchestrator::new(
        MockSource::new(vec![task], Arc::clone(&source_tracker)),
        runner,
        MockSubmission::new(Arc::clone(&sub_tracker)),
        WorktreeManager::new(repo_dir.path().to_path_buf(), wt_dir.path().to_path_buf()),
        StateManager::new(&state_dir),
        PromptEngine::new(None),
        make_config(true),
        repo_dir.path().to_path_buf(),
    );

    let err = orchestrator.run_once().await.unwrap_err();
    assert!(err.to_string().contains("mock failure at implement"));

    // State should still show current task (not completed)
    let state_mgr = StateManager::new(&state_dir);
    let state = state_mgr.load();
    assert!(state.current_task.is_some());
    assert_eq!(state.current_task.unwrap().phase, "implement");
}

#[tokio::test]
async fn test_error_at_review_phase() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let runner = FailAtPhaseRunner {
        fail_at: Phase::Review,
        task_id: "gh-42".to_string(),
    };

    let state_dir = repo_dir.path().join(".rlph-test-state");
    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));

    let orchestrator = Orchestrator::new(
        MockSource::new(vec![task], Arc::clone(&source_tracker)),
        runner,
        MockSubmission::new(Arc::clone(&sub_tracker)),
        WorktreeManager::new(repo_dir.path().to_path_buf(), wt_dir.path().to_path_buf()),
        StateManager::new(&state_dir),
        PromptEngine::new(None),
        make_config(true),
        repo_dir.path().to_path_buf(),
    );

    let err = orchestrator.run_once().await.unwrap_err();
    assert!(err.to_string().contains("mock failure at review"));

    // State should show review phase
    let state_mgr = StateManager::new(&state_dir);
    let state = state_mgr.load();
    assert!(state.current_task.is_some());
    assert_eq!(state.current_task.unwrap().phase, "review");
}

#[tokio::test]
async fn test_error_at_submission() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));

    let orchestrator = Orchestrator::new(
        MockSource::new(vec![task], Arc::clone(&source_tracker)),
        MockRunner::new("gh-42"),
        FailSubmission,
        WorktreeManager::new(repo_dir.path().to_path_buf(), wt_dir.path().to_path_buf()),
        StateManager::new(repo_dir.path().join(".rlph-test-state")),
        PromptEngine::new(None),
        make_config(false), // need non-dry-run to trigger submission
        repo_dir.path().to_path_buf(),
    );

    let err = orchestrator.run_once().await.unwrap_err();
    assert!(err.to_string().contains("mock submission failure"));
}

#[tokio::test]
async fn test_state_transitions_through_phases() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(7, "Add feature");

    let state_dir = repo_dir.path().join(".rlph-test-state");
    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));

    let orchestrator = Orchestrator::new(
        MockSource::new(vec![task], Arc::clone(&source_tracker)),
        MockRunner::new("gh-7"),
        MockSubmission::new(Arc::clone(&sub_tracker)),
        WorktreeManager::new(repo_dir.path().to_path_buf(), wt_dir.path().to_path_buf()),
        StateManager::new(&state_dir),
        PromptEngine::new(None),
        make_config(true),
        repo_dir.path().to_path_buf(),
    );

    orchestrator.run_once().await.unwrap();

    // After completion: current_task is None, history has the task
    let state_mgr = StateManager::new(&state_dir);
    let state = state_mgr.load();
    assert!(state.current_task.is_none());
    assert_eq!(state.history.len(), 1);
    assert_eq!(state.history[0].id, "gh-7");
}

#[tokio::test]
async fn test_worktree_cleaned_up_after_success() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));

    let orchestrator = Orchestrator::new(
        MockSource::new(vec![task], Arc::clone(&source_tracker)),
        MockRunner::new("gh-42"),
        MockSubmission::new(Arc::clone(&sub_tracker)),
        WorktreeManager::new(repo_dir.path().to_path_buf(), wt_dir.path().to_path_buf()),
        StateManager::new(repo_dir.path().join(".rlph-test-state")),
        PromptEngine::new(None),
        make_config(true),
        repo_dir.path().to_path_buf(),
    );

    orchestrator.run_once().await.unwrap();

    // Worktree directory should be removed
    let wt_path = wt_dir.path().join("rlph-42-fix-bug");
    assert!(
        !wt_path.exists(),
        "worktree should be cleaned up: {}",
        wt_path.display()
    );
}

#[tokio::test]
async fn test_review_exhaustion_does_not_mark_done() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let state_dir = repo_dir.path().join(".rlph-test-state");
    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));

    let mut config = make_config(true);
    config.max_review_rounds = 2;

    let orchestrator = Orchestrator::new(
        MockSource::new(vec![task], Arc::clone(&source_tracker)),
        ReviewNeverCompleteRunner {
            task_id: "gh-42".to_string(),
        },
        MockSubmission::new(Arc::clone(&sub_tracker)),
        WorktreeManager::new(repo_dir.path().to_path_buf(), wt_dir.path().to_path_buf()),
        StateManager::new(&state_dir),
        PromptEngine::new(None),
        config,
        repo_dir.path().to_path_buf(),
    );

    let err = orchestrator.run_once().await.unwrap_err();
    assert!(
        err.to_string().contains("review did not complete"),
        "unexpected error: {err}"
    );

    // Task must NOT be marked done
    let tracker = source_tracker.lock().unwrap();
    assert!(tracker.marked_done.is_empty());
    drop(tracker);

    // State should still show current task in review phase (resumable)
    let state_mgr = StateManager::new(&state_dir);
    let state = state_mgr.load();
    assert!(state.current_task.is_some());
    assert_eq!(state.current_task.unwrap().phase, "review");
    assert!(state.history.is_empty());
}

#[tokio::test]
async fn test_existing_pr_skips_submission() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));

    let orchestrator = Orchestrator::new(
        MockSource::new(vec![task], Arc::clone(&source_tracker)),
        ExistingPrRunner {
            task_id: "gh-42".to_string(),
            pr_number: 99,
        },
        MockSubmission::new(Arc::clone(&sub_tracker)),
        WorktreeManager::new(repo_dir.path().to_path_buf(), wt_dir.path().to_path_buf()),
        StateManager::new(repo_dir.path().join(".rlph-test-state")),
        PromptEngine::new(None),
        make_config(false), // non-dry-run so submission would normally fire
        repo_dir.path().to_path_buf(),
    );

    orchestrator.run_once().await.unwrap();

    // Submission should NOT have been called
    let subs = sub_tracker.lock().unwrap();
    assert!(
        subs.submissions.is_empty(),
        "expected no submissions when existing PR reported, got {}",
        subs.submissions.len()
    );
}
