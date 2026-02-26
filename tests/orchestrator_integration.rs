use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rlph::config::{
    Config, ReviewPhaseConfig, ReviewStepConfig, default_review_phases, default_review_step,
};
use rlph::error::{Error, Result};
use rlph::orchestrator::{
    CorrectionRunner, Orchestrator, ProgressReporter, ReviewInvocation, ReviewRunnerFactory,
};
use rlph::prompts::PromptEngine;
use rlph::runner::{AgentRunner, AnyRunner, CallbackRunner, Phase, RunResult, RunnerKind};
use rlph::sources::{Task, TaskSource};
use rlph::state::StateManager;
use rlph::submission::{SubmissionBackend, SubmitResult};
use rlph::worktree::WorktreeManager;
use tokio::sync::watch;

// --- Shared tracking state ---

#[derive(Default)]
struct SourceTracker {
    marked_in_progress: Vec<String>,
    marked_in_review: Vec<String>,
}

#[derive(Default)]
struct SubmissionTracker {
    submissions: Vec<(String, String, String, String)>,
    comments: Vec<(u64, String)>,
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

    fn mark_in_review(&self, task_id: &str) -> Result<()> {
        self.tracker
            .lock()
            .unwrap()
            .marked_in_review
            .push(task_id.to_string());
        Ok(())
    }

    fn get_task_details(&self, task_id: &str) -> Result<Task> {
        self.task_details
            .get(task_id)
            .cloned()
            .ok_or_else(|| Error::TaskSource(format!("task not found: {task_id}")))
    }

    fn fetch_closed_task_ids(&self) -> Result<HashSet<u64>> {
        Ok(HashSet::new())
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
                    session_id: None,
                })
            }
            Phase::Implement => Ok(RunResult {
                exit_code: 0,
                stdout: "IMPLEMENTATION_COMPLETE: done".into(),
                stderr: String::new(),
                session_id: None,
            }),
            Phase::Review => Ok(RunResult {
                exit_code: 0,
                stdout: "NO_ISSUES_FOUND".into(),
                stderr: String::new(),
                session_id: None,
            }),
            Phase::ReviewAggregate => Ok(RunResult {
                exit_code: 0,
                stdout: r#"{"verdict":"approved","comment":"All looks good.","findings":[],"fix_instructions":null}"#.into(),
                stderr: String::new(),
                session_id: None,
            }),
            Phase::ReviewFix => Ok(RunResult {
                exit_code: 0,
                stdout: r#"{"status":"fixed","summary":"applied fixes","files_changed":["src/main.rs"]}"#.into(),
                stderr: String::new(),
                session_id: None,
            }),
        }
    }
}

struct SequenceSource {
    tasks_by_fetch: Arc<Mutex<VecDeque<Vec<Task>>>>,
    task_details: HashMap<String, Task>,
}

impl SequenceSource {
    fn new(tasks_by_fetch: Vec<Vec<Task>>) -> Self {
        let mut task_details = HashMap::new();
        for tasks in &tasks_by_fetch {
            for task in tasks {
                task_details.insert(task.id.clone(), task.clone());
            }
        }
        Self {
            tasks_by_fetch: Arc::new(Mutex::new(VecDeque::from(tasks_by_fetch))),
            task_details,
        }
    }
}

impl TaskSource for SequenceSource {
    fn fetch_eligible_tasks(&self) -> Result<Vec<Task>> {
        let next = self.tasks_by_fetch.lock().unwrap().pop_front();
        Ok(next.unwrap_or_default())
    }

    fn mark_in_progress(&self, _task_id: &str) -> Result<()> {
        Ok(())
    }

    fn mark_in_review(&self, _task_id: &str) -> Result<()> {
        Ok(())
    }

    fn get_task_details(&self, task_id: &str) -> Result<Task> {
        self.task_details
            .get(task_id)
            .cloned()
            .ok_or_else(|| Error::TaskSource(format!("task not found: {task_id}")))
    }

    fn fetch_closed_task_ids(&self) -> Result<HashSet<u64>> {
        Ok(HashSet::new())
    }
}

#[derive(Default)]
struct RunnerCounts {
    choose: AtomicUsize,
    implement: AtomicUsize,
    review: AtomicUsize,
}

struct CountingRunner {
    task_id: String,
    counts: Arc<RunnerCounts>,
    shutdown_tx: Option<watch::Sender<bool>>,
}

impl CountingRunner {
    fn new(task_id: &str, counts: Arc<RunnerCounts>) -> Self {
        Self {
            task_id: task_id.to_string(),
            counts,
            shutdown_tx: None,
        }
    }

    fn with_shutdown(
        task_id: &str,
        counts: Arc<RunnerCounts>,
        shutdown_tx: watch::Sender<bool>,
    ) -> Self {
        Self {
            task_id: task_id.to_string(),
            counts,
            shutdown_tx: Some(shutdown_tx),
        }
    }
}

impl AgentRunner for CountingRunner {
    async fn run(&self, phase: Phase, _prompt: &str, working_dir: &Path) -> Result<RunResult> {
        match phase {
            Phase::Choose => {
                self.counts.choose.fetch_add(1, Ordering::SeqCst);
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
                    session_id: None,
                })
            }
            Phase::Implement => {
                self.counts.implement.fetch_add(1, Ordering::SeqCst);
                if let Some(tx) = &self.shutdown_tx {
                    let _ = tx.send(true);
                }
                Ok(RunResult {
                    exit_code: 0,
                    stdout: "IMPLEMENTATION_COMPLETE: done".into(),
                    stderr: String::new(),
                    session_id: None,
                })
            }
            Phase::Review => {
                self.counts.review.fetch_add(1, Ordering::SeqCst);
                Ok(RunResult {
                    exit_code: 0,
                    stdout: "NO_ISSUES_FOUND".into(),
                    stderr: String::new(),
                    session_id: None,
                })
            }
            Phase::ReviewAggregate => Ok(RunResult {
                exit_code: 0,
                stdout: r#"{"verdict":"approved","comment":"All looks good.","findings":[],"fix_instructions":null}"#.into(),
                stderr: String::new(),
                session_id: None,
            }),
            Phase::ReviewFix => Ok(RunResult {
                exit_code: 0,
                stdout: r#"{"status":"fixed","summary":"done","files_changed":[]}"#.into(),
                stderr: String::new(),
                session_id: None,
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
                    session_id: None,
                })
            }
            Phase::Implement => Ok(RunResult {
                exit_code: 0,
                stdout: "IMPLEMENTATION_COMPLETE: done".into(),
                stderr: String::new(),
                session_id: None,
            }),
            Phase::Review => Ok(RunResult {
                exit_code: 0,
                stdout: "NO_ISSUES_FOUND".into(),
                stderr: String::new(),
                session_id: None,
            }),
            Phase::ReviewAggregate => Ok(RunResult {
                exit_code: 0,
                stdout: r#"{"verdict":"approved","comment":"All looks good.","findings":[],"fix_instructions":null}"#.into(),
                stderr: String::new(),
                session_id: None,
            }),
            Phase::ReviewFix => Ok(RunResult {
                exit_code: 0,
                stdout: r#"{"status":"fixed","summary":"done","files_changed":[]}"#.into(),
                stderr: String::new(),
                session_id: None,
            }),
        }
    }
}

struct MockSubmission {
    tracker: Arc<Mutex<SubmissionTracker>>,
    existing_pr_for_issue: Option<u64>,
}

impl MockSubmission {
    fn new(tracker: Arc<Mutex<SubmissionTracker>>, existing_pr_for_issue: Option<u64>) -> Self {
        Self {
            tracker,
            existing_pr_for_issue,
        }
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
            number: Some(1),
        })
    }

    fn find_existing_pr_for_issue(&self, _issue_number: u64) -> Result<Option<u64>> {
        Ok(self.existing_pr_for_issue)
    }

    fn upsert_review_comment(&self, pr_number: u64, body: &str) -> Result<()> {
        self.tracker
            .lock()
            .unwrap()
            .comments
            .push((pr_number, body.to_string()));
        Ok(())
    }

    fn fetch_pr_comments(&self, _pr_number: u64) -> Result<Vec<rlph::submission::PrComment>> {
        Ok(vec![])
    }
}

struct FailSubmission;

impl SubmissionBackend for FailSubmission {
    fn submit(&self, _: &str, _: &str, _: &str, _: &str) -> Result<SubmitResult> {
        Err(Error::Submission("mock submission failure".to_string()))
    }

    fn find_existing_pr_for_issue(&self, _issue_number: u64) -> Result<Option<u64>> {
        Ok(None)
    }

    fn upsert_review_comment(&self, _pr_number: u64, _body: &str) -> Result<()> {
        Ok(())
    }

    fn fetch_pr_comments(&self, _pr_number: u64) -> Result<Vec<rlph::submission::PrComment>> {
        Ok(vec![])
    }
}

/// Review runner factory that returns mock runners producing REVIEW_APPROVED.
struct ApprovedReviewFactory;

impl ReviewRunnerFactory for ApprovedReviewFactory {
    fn create_phase_runner(&self, _phase: &ReviewPhaseConfig, _timeout_retries: u32) -> AnyRunner {
        AnyRunner::Callback(CallbackRunner::new(Arc::new(|_phase, _prompt, _dir| {
            Box::pin(async {
                Ok(RunResult {
                    exit_code: 0,
                    stdout: r#"{"findings":[]}"#.into(),
                    stderr: String::new(),
                    session_id: None,
                })
            })
        })))
    }

    fn create_step_runner(&self, _step: &ReviewStepConfig, _timeout_retries: u32) -> AnyRunner {
        AnyRunner::Callback(CallbackRunner::new(Arc::new(|phase, _prompt, _dir| {
            Box::pin(async move {
                let stdout = match phase {
                    Phase::ReviewAggregate => r#"{"verdict":"approved","comment":"All good.","findings":[],"fix_instructions":null}"#.to_string(),
                    Phase::ReviewFix => r#"{"status":"fixed","summary":"done","files_changed":[]}"#.to_string(),
                    _ => String::new(),
                };
                Ok(RunResult {
                    exit_code: 0,
                    stdout,
                    stderr: String::new(),
                    session_id: None,
                })
            })
        })))
    }
}

/// Review runner factory where aggregation always requests fixes (never approves).
struct NeverApproveReviewFactory;

impl ReviewRunnerFactory for NeverApproveReviewFactory {
    fn create_phase_runner(&self, _phase: &ReviewPhaseConfig, _timeout_retries: u32) -> AnyRunner {
        AnyRunner::Callback(CallbackRunner::new(Arc::new(|_phase, _prompt, _dir| {
            Box::pin(async {
                Ok(RunResult {
                    exit_code: 0,
                    stdout: r#"{"findings":[{"file":"src/main.rs","line":1,"severity":"warning","description":"issues found"}]}"#.into(),
                    stderr: String::new(),
                    session_id: None,
                })
            })
        })))
    }

    fn create_step_runner(&self, _step: &ReviewStepConfig, _timeout_retries: u32) -> AnyRunner {
        AnyRunner::Callback(CallbackRunner::new(Arc::new(|phase, _prompt, _dir| {
            Box::pin(async move {
                let stdout = match phase {
                    Phase::ReviewAggregate => {
                        r#"{"verdict":"needs_fix","comment":"Issues found","findings":[{"file":"src/main.rs","line":1,"severity":"warning","description":"issue"}],"fix_instructions":"fix everything"}"#.to_string()
                    }
                    Phase::ReviewFix => r#"{"status":"fixed","summary":"attempted fixes","files_changed":["src/main.rs"]}"#.to_string(),
                    _ => String::new(),
                };
                Ok(RunResult {
                    exit_code: 0,
                    stdout,
                    stderr: String::new(),
                    session_id: None,
                })
            })
        })))
    }
}

/// Review runner factory where the review phase itself fails.
struct FailReviewFactory;

impl ReviewRunnerFactory for FailReviewFactory {
    fn create_phase_runner(&self, _phase: &ReviewPhaseConfig, _timeout_retries: u32) -> AnyRunner {
        AnyRunner::Callback(CallbackRunner::new(Arc::new(|_phase, _prompt, _dir| {
            Box::pin(async { Err(Error::AgentRunner("mock failure at review".to_string())) })
        })))
    }

    fn create_step_runner(&self, _step: &ReviewStepConfig, _timeout_retries: u32) -> AnyRunner {
        AnyRunner::Callback(CallbackRunner::new(Arc::new(|_phase, _prompt, _dir| {
            Box::pin(async {
                Ok(RunResult {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                    session_id: None,
                })
            })
        })))
    }
}

/// Events captured by `CapturingReporter` for test assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PipelineEvent {
    FetchingTasks,
    TasksFound { count: usize },
    TaskSelected { issue_number: u64, title: String },
    ImplementStarted,
    PrCreated { url: String },
    IterationComplete { issue_number: u64, title: String },
    PhasesStarted { count: usize, names: Vec<String> },
    PhaseComplete { name: String },
    ReviewSummary { body: String },
    PrUrl { url: String },
}

/// Test-only reporter that collects events into a shared vec.
struct CapturingReporter {
    events: Arc<Mutex<Vec<PipelineEvent>>>,
}

impl CapturingReporter {
    fn new() -> (Self, Arc<Mutex<Vec<PipelineEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                events: Arc::clone(&events),
            },
            events,
        )
    }
}

impl ProgressReporter for CapturingReporter {
    fn fetching_tasks(&self) {
        self.events
            .lock()
            .unwrap()
            .push(PipelineEvent::FetchingTasks);
    }

    fn tasks_found(&self, count: usize) {
        self.events
            .lock()
            .unwrap()
            .push(PipelineEvent::TasksFound { count });
    }

    fn task_selected(&self, issue_number: u64, title: &str) {
        self.events
            .lock()
            .unwrap()
            .push(PipelineEvent::TaskSelected {
                issue_number,
                title: title.to_string(),
            });
    }

    fn implement_started(&self) {
        self.events
            .lock()
            .unwrap()
            .push(PipelineEvent::ImplementStarted);
    }

    fn pr_created(&self, url: &str) {
        self.events.lock().unwrap().push(PipelineEvent::PrCreated {
            url: url.to_string(),
        });
    }

    fn iteration_complete(&self, issue_number: u64, title: &str) {
        self.events
            .lock()
            .unwrap()
            .push(PipelineEvent::IterationComplete {
                issue_number,
                title: title.to_string(),
            });
    }

    fn phases_started(&self, names: &[String]) {
        self.events
            .lock()
            .unwrap()
            .push(PipelineEvent::PhasesStarted {
                count: names.len(),
                names: names.to_vec(),
            });
    }

    fn phase_complete(&self, name: &str) {
        self.events
            .lock()
            .unwrap()
            .push(PipelineEvent::PhaseComplete {
                name: name.to_string(),
            });
    }

    fn review_summary(&self, body: &str) {
        self.events
            .lock()
            .unwrap()
            .push(PipelineEvent::ReviewSummary {
                body: body.to_string(),
            });
    }

    fn pr_url(&self, url: &str) {
        self.events.lock().unwrap().push(PipelineEvent::PrUrl {
            url: url.to_string(),
        });
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
        runner: RunnerKind::Claude,
        submission: "github".to_string(),
        label: "rlph".to_string(),
        poll_seconds: 30,
        worktree_dir: String::new(),
        base_branch: "main".to_string(),
        max_iterations: None,
        dry_run,
        once: true,
        continuous: false,
        agent_binary: "claude".to_string(),
        agent_model: None,
        agent_timeout: None,
        agent_effort: None,
        agent_variant: None,
        max_review_rounds: 3,
        agent_timeout_retries: 2,
        review_phases: default_review_phases(),
        review_aggregate: default_review_step("review-aggregate"),
        review_fix: default_review_step("review-fix"),
        linear: None,
    }
}

fn make_review_vars(
    task: &Task,
    repo_path: &Path,
    branch: &str,
    worktree_path: &Path,
) -> HashMap<String, String> {
    HashMap::from([
        ("issue_title".to_string(), task.title.clone()),
        ("issue_body".to_string(), task.body.clone()),
        ("issue_number".to_string(), task.id.clone()),
        ("issue_url".to_string(), task.url.clone()),
        ("repo_path".to_string(), repo_path.display().to_string()),
        ("branch_name".to_string(), branch.to_string()),
        (
            "worktree_path".to_string(),
            worktree_path.display().to_string(),
        ),
        (
            "pr_url".to_string(),
            format!("https://github.com/test/repo/pull/{}", task.id),
        ),
    ])
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
    let submission = MockSubmission::new(Arc::clone(&sub_tracker), None);
    let worktree_mgr = WorktreeManager::new(
        repo_dir.path().to_path_buf(),
        wt_dir.path().to_path_buf(),
        "main".to_string(),
    );
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
    )
    .with_review_factory(ApprovedReviewFactory);

    orchestrator.run_once().await.unwrap();

    // In dry_run, source should NOT be marked
    let tracker = source_tracker.lock().unwrap();
    assert!(tracker.marked_in_progress.is_empty());
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
    let submission = MockSubmission::new(Arc::clone(&sub_tracker), None);
    let worktree_mgr = WorktreeManager::new(
        repo_dir.path().to_path_buf(),
        wt_dir.path().to_path_buf(),
        "main".to_string(),
    );
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
    )
    .with_review_factory(ApprovedReviewFactory);

    orchestrator.run_once().await.unwrap();

    // Source should be marked in-progress (done is handled by GitHub on PR merge)
    let tracker = source_tracker.lock().unwrap();
    assert_eq!(tracker.marked_in_progress, vec!["42".to_string()]);
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
        MockSubmission::new(Arc::clone(&sub_tracker), None),
        WorktreeManager::new(
            repo_dir.path().to_path_buf(),
            wt_dir.path().to_path_buf(),
            "main".to_string(),
        ),
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
        MockSubmission::new(Arc::clone(&sub_tracker), None),
        WorktreeManager::new(
            repo_dir.path().to_path_buf(),
            wt_dir.path().to_path_buf(),
            "main".to_string(),
        ),
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
        MockSubmission::new(Arc::clone(&sub_tracker), None),
        WorktreeManager::new(
            repo_dir.path().to_path_buf(),
            wt_dir.path().to_path_buf(),
            "main".to_string(),
        ),
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

    let state_dir = repo_dir.path().join(".rlph-test-state");
    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));

    let orchestrator = Orchestrator::new(
        MockSource::new(vec![task], Arc::clone(&source_tracker)),
        MockRunner::new("gh-42"),
        MockSubmission::new(Arc::clone(&sub_tracker), None),
        WorktreeManager::new(
            repo_dir.path().to_path_buf(),
            wt_dir.path().to_path_buf(),
            "main".to_string(),
        ),
        StateManager::new(&state_dir),
        PromptEngine::new(None),
        make_config(true),
        repo_dir.path().to_path_buf(),
    )
    .with_review_factory(FailReviewFactory);

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
        WorktreeManager::new(
            repo_dir.path().to_path_buf(),
            wt_dir.path().to_path_buf(),
            "main".to_string(),
        ),
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
        MockSubmission::new(Arc::clone(&sub_tracker), None),
        WorktreeManager::new(
            repo_dir.path().to_path_buf(),
            wt_dir.path().to_path_buf(),
            "main".to_string(),
        ),
        StateManager::new(&state_dir),
        PromptEngine::new(None),
        make_config(true),
        repo_dir.path().to_path_buf(),
    )
    .with_review_factory(ApprovedReviewFactory);

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
        MockSubmission::new(Arc::clone(&sub_tracker), None),
        WorktreeManager::new(
            repo_dir.path().to_path_buf(),
            wt_dir.path().to_path_buf(),
            "main".to_string(),
        ),
        StateManager::new(repo_dir.path().join(".rlph-test-state")),
        PromptEngine::new(None),
        make_config(true),
        repo_dir.path().to_path_buf(),
    )
    .with_review_factory(ApprovedReviewFactory);

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
async fn test_review_exhaustion_preserves_state() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let state_dir = repo_dir.path().join(".rlph-test-state");
    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));

    let mut config = make_config(true);
    config.max_review_rounds = 2;

    let orchestrator = Orchestrator::new(
        MockSource::new(vec![task], Arc::clone(&source_tracker)),
        MockRunner::new("gh-42"),
        MockSubmission::new(Arc::clone(&sub_tracker), None),
        WorktreeManager::new(
            repo_dir.path().to_path_buf(),
            wt_dir.path().to_path_buf(),
            "main".to_string(),
        ),
        StateManager::new(&state_dir),
        PromptEngine::new(None),
        config,
        repo_dir.path().to_path_buf(),
    )
    .with_review_factory(NeverApproveReviewFactory);

    let err = orchestrator.run_once().await.unwrap_err();
    assert!(
        err.to_string().contains("review did not complete"),
        "unexpected error: {err}"
    );

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
        MockRunner::new("gh-42"),
        MockSubmission::new(Arc::clone(&sub_tracker), Some(99)),
        WorktreeManager::new(
            repo_dir.path().to_path_buf(),
            wt_dir.path().to_path_buf(),
            "main".to_string(),
        ),
        StateManager::new(repo_dir.path().join(".rlph-test-state")),
        PromptEngine::new(None),
        make_config(false), // non-dry-run so submission would normally fire
        repo_dir.path().to_path_buf(),
    )
    .with_review_factory(ApprovedReviewFactory);

    orchestrator.run_once().await.unwrap();

    // Submission should NOT have been called
    let subs = sub_tracker.lock().unwrap();
    assert!(
        subs.submissions.is_empty(),
        "expected no submissions when existing PR is found, got {}",
        subs.submissions.len()
    );
}

#[tokio::test]
async fn test_continuous_mode_polls_with_empty_results() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");
    let counts = Arc::new(RunnerCounts::default());
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));
    let source = SequenceSource::new(vec![vec![task], vec![]]);

    let mut config = make_config(true);
    config.once = false;
    config.continuous = true;
    config.max_iterations = Some(2);
    config.poll_seconds = 1;

    let orchestrator = Orchestrator::new(
        source,
        CountingRunner::new("gh-42", Arc::clone(&counts)),
        MockSubmission::new(Arc::clone(&sub_tracker), None),
        WorktreeManager::new(
            repo_dir.path().to_path_buf(),
            wt_dir.path().to_path_buf(),
            "main".to_string(),
        ),
        StateManager::new(repo_dir.path().join(".rlph-test-state")),
        PromptEngine::new(None),
        config,
        repo_dir.path().to_path_buf(),
    )
    .with_review_factory(ApprovedReviewFactory);

    orchestrator.run_loop(None).await.unwrap();

    assert_eq!(counts.choose.load(Ordering::SeqCst), 1);
    assert_eq!(counts.implement.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_max_iterations_stops_at_limit() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");
    let counts = Arc::new(RunnerCounts::default());
    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));

    let mut config = make_config(true);
    config.once = false;
    config.continuous = false;
    config.max_iterations = Some(3);

    let orchestrator = Orchestrator::new(
        MockSource::new(vec![task], Arc::clone(&source_tracker)),
        CountingRunner::new("gh-42", Arc::clone(&counts)),
        MockSubmission::new(Arc::clone(&sub_tracker), None),
        WorktreeManager::new(
            repo_dir.path().to_path_buf(),
            wt_dir.path().to_path_buf(),
            "main".to_string(),
        ),
        StateManager::new(repo_dir.path().join(".rlph-test-state")),
        PromptEngine::new(None),
        config,
        repo_dir.path().to_path_buf(),
    )
    .with_review_factory(ApprovedReviewFactory);

    orchestrator.run_loop(None).await.unwrap();

    assert_eq!(counts.choose.load(Ordering::SeqCst), 3);
    assert_eq!(counts.implement.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn test_continuous_shutdown_exits_between_iterations() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");
    let counts = Arc::new(RunnerCounts::default());
    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let mut config = make_config(true);
    config.once = false;
    config.continuous = true;
    config.max_iterations = None;
    config.poll_seconds = 1;

    let orchestrator = Orchestrator::new(
        MockSource::new(vec![task], Arc::clone(&source_tracker)),
        CountingRunner::with_shutdown("gh-42", Arc::clone(&counts), shutdown_tx),
        MockSubmission::new(Arc::clone(&sub_tracker), None),
        WorktreeManager::new(
            repo_dir.path().to_path_buf(),
            wt_dir.path().to_path_buf(),
            "main".to_string(),
        ),
        StateManager::new(repo_dir.path().join(".rlph-test-state")),
        PromptEngine::new(None),
        config,
        repo_dir.path().to_path_buf(),
    )
    .with_review_factory(ApprovedReviewFactory);

    orchestrator.run_loop(Some(shutdown_rx)).await.unwrap();

    // Shutdown signal fires during implement, but the current iteration
    // completes fully. Shutdown is only checked between iterations.
    assert_eq!(counts.choose.load(Ordering::SeqCst), 1);
    assert_eq!(counts.implement.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_review_only_success_posts_comment_and_marks_review() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));
    let source = MockSource::new(vec![task.clone()], Arc::clone(&source_tracker));
    let submission = MockSubmission::new(Arc::clone(&sub_tracker), None);
    let worktree_mgr = WorktreeManager::new(
        repo_dir.path().to_path_buf(),
        wt_dir.path().to_path_buf(),
        "main".to_string(),
    );
    let worktree_info = worktree_mgr.create(42, "review-only").unwrap();
    let state_dir = repo_dir.path().join(".rlph-test-state");
    let vars = make_review_vars(
        &task,
        repo_dir.path(),
        &worktree_info.branch,
        &worktree_info.path,
    );

    let orchestrator = Orchestrator::new(
        source,
        MockRunner::new("gh-42"),
        submission,
        worktree_mgr,
        StateManager::new(&state_dir),
        PromptEngine::new(None),
        make_config(false),
        repo_dir.path().to_path_buf(),
    )
    .with_review_factory(ApprovedReviewFactory);

    let invocation = ReviewInvocation {
        task_id_for_state: "gh-42".to_string(),
        mark_in_review_task_id: Some("42".to_string()),
        worktree_info: worktree_info.clone(),
        vars,
        comment_pr_number: Some(77),
        push_remote_branch: None,
    };
    orchestrator
        .run_review_for_existing_pr(invocation)
        .await
        .unwrap();

    let source_data = source_tracker.lock().unwrap();
    assert_eq!(source_data.marked_in_review, vec!["42".to_string()]);
    drop(source_data);

    let submission_data = sub_tracker.lock().unwrap();
    assert_eq!(submission_data.comments.len(), 1);
    assert_eq!(submission_data.comments[0].0, 77);
    drop(submission_data);

    let state = StateManager::new(&state_dir).load();
    assert!(state.current_task.is_none());
    assert_eq!(state.history.len(), 1);
    assert_eq!(state.history[0].id, "gh-42");
    assert!(!worktree_info.path.exists());
}

#[tokio::test]
async fn test_review_only_without_linked_issue_skips_mark_in_review() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(88, "Review branch");

    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));
    let source = MockSource::new(vec![task.clone()], Arc::clone(&source_tracker));
    let submission = MockSubmission::new(Arc::clone(&sub_tracker), None);
    let worktree_mgr = WorktreeManager::new(
        repo_dir.path().to_path_buf(),
        wt_dir.path().to_path_buf(),
        "main".to_string(),
    );
    let worktree_info = worktree_mgr.create(88, "review-pr-only").unwrap();
    let state_dir = repo_dir.path().join(".rlph-test-state");
    let vars = make_review_vars(
        &task,
        repo_dir.path(),
        &worktree_info.branch,
        &worktree_info.path,
    );

    let orchestrator = Orchestrator::new(
        source,
        MockRunner::new("gh-88"),
        submission,
        worktree_mgr,
        StateManager::new(&state_dir),
        PromptEngine::new(None),
        make_config(false),
        repo_dir.path().to_path_buf(),
    )
    .with_review_factory(ApprovedReviewFactory);

    let invocation = ReviewInvocation {
        task_id_for_state: "pr-88".to_string(),
        mark_in_review_task_id: None,
        worktree_info,
        vars,
        comment_pr_number: Some(88),
        push_remote_branch: None,
    };
    orchestrator
        .run_review_for_existing_pr(invocation)
        .await
        .unwrap();

    let source_data = source_tracker.lock().unwrap();
    assert!(source_data.marked_in_review.is_empty());
    drop(source_data);

    let state = StateManager::new(&state_dir).load();
    assert!(state.current_task.is_none());
    assert_eq!(state.history.len(), 1);
    assert_eq!(state.history[0].id, "pr-88");
}

#[tokio::test]
async fn test_review_only_exhaustion_preserves_state() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(99, "Needs fixes");

    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));
    let source = MockSource::new(vec![task.clone()], Arc::clone(&source_tracker));
    let submission = MockSubmission::new(Arc::clone(&sub_tracker), None);
    let worktree_mgr = WorktreeManager::new(
        repo_dir.path().to_path_buf(),
        wt_dir.path().to_path_buf(),
        "main".to_string(),
    );
    let worktree_info = worktree_mgr.create(99, "review-exhaustion").unwrap();
    let state_dir = repo_dir.path().join(".rlph-test-state");
    let vars = make_review_vars(
        &task,
        repo_dir.path(),
        &worktree_info.branch,
        &worktree_info.path,
    );

    let mut config = make_config(true);
    config.max_review_rounds = 2;
    let orchestrator = Orchestrator::new(
        source,
        MockRunner::new("gh-99"),
        submission,
        worktree_mgr,
        StateManager::new(&state_dir),
        PromptEngine::new(None),
        config,
        repo_dir.path().to_path_buf(),
    )
    .with_review_factory(NeverApproveReviewFactory);

    let invocation = ReviewInvocation {
        task_id_for_state: "pr-99".to_string(),
        mark_in_review_task_id: None,
        worktree_info: worktree_info.clone(),
        vars,
        comment_pr_number: Some(99),
        push_remote_branch: None,
    };
    let err = orchestrator
        .run_review_for_existing_pr(invocation)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("review did not complete"));

    let state = StateManager::new(&state_dir).load();
    assert!(state.current_task.is_some());
    assert_eq!(state.current_task.unwrap().phase, "review");
    assert!(state.history.is_empty());
    assert!(worktree_info.path.exists());
}

// --- ProgressReporter output tests ---

/// Creates orchestrator + invocation with capturing reporter, returning events handle.
#[allow(clippy::type_complexity)]
fn build_review_orchestrator_with_reporter<F: ReviewRunnerFactory>(
    repo_dir: &Path,
    wt_dir: &Path,
    task: &Task,
    factory: F,
    dry_run: bool,
) -> (
    Orchestrator<MockSource, MockRunner, MockSubmission, F, CapturingReporter>,
    ReviewInvocation,
    Arc<Mutex<Vec<PipelineEvent>>>,
) {
    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));
    let source = MockSource::new(vec![task.clone()], Arc::clone(&source_tracker));
    let submission = MockSubmission::new(Arc::clone(&sub_tracker), None);
    let worktree_mgr = WorktreeManager::new(
        repo_dir.to_path_buf(),
        wt_dir.to_path_buf(),
        "main".to_string(),
    );
    let worktree_info = worktree_mgr.create(42, "review-reporter").unwrap();
    let state_dir = repo_dir.join(".rlph-test-state");
    let vars = make_review_vars(task, repo_dir, &worktree_info.branch, &worktree_info.path);

    let (reporter, events) = CapturingReporter::new();

    let orchestrator = Orchestrator::new(
        source,
        MockRunner::new("gh-42"),
        submission,
        worktree_mgr,
        StateManager::new(&state_dir),
        PromptEngine::new(None),
        make_config(dry_run),
        repo_dir.to_path_buf(),
    )
    .with_review_factory(factory)
    .with_reporter(reporter);

    let invocation = ReviewInvocation {
        task_id_for_state: "gh-42".to_string(),
        mark_in_review_task_id: Some("42".to_string()),
        worktree_info,
        vars,
        comment_pr_number: Some(77),
        push_remote_branch: None,
    };

    (orchestrator, invocation, events)
}

#[tokio::test]
async fn test_review_reports_phases_started() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let (orchestrator, invocation, events) = build_review_orchestrator_with_reporter(
        repo_dir.path(),
        wt_dir.path(),
        &task,
        ApprovedReviewFactory,
        false,
    );

    orchestrator
        .run_review_for_existing_pr(invocation)
        .await
        .unwrap();

    let events = events.lock().unwrap();
    let started = events
        .iter()
        .find(|e| matches!(e, PipelineEvent::PhasesStarted { .. }))
        .expect("should have PhasesStarted event");
    match started {
        PipelineEvent::PhasesStarted { count, names } => {
            assert_eq!(*count, 3);
            assert!(names.contains(&"correctness".to_string()));
            assert!(names.contains(&"security".to_string()));
            assert!(names.contains(&"style".to_string()));
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn test_review_reports_phase_completions() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let (orchestrator, invocation, events) = build_review_orchestrator_with_reporter(
        repo_dir.path(),
        wt_dir.path(),
        &task,
        ApprovedReviewFactory,
        false,
    );

    orchestrator
        .run_review_for_existing_pr(invocation)
        .await
        .unwrap();

    let events = events.lock().unwrap();
    let completions: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            PipelineEvent::PhaseComplete { name } => Some(name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        completions.len(),
        3,
        "expected 3 phase completions, got {completions:?}"
    );
    let completion_set: HashSet<_> = completions.into_iter().collect();
    assert!(completion_set.contains("correctness"));
    assert!(completion_set.contains("security"));
    assert!(completion_set.contains("style"));
}

#[tokio::test]
async fn test_review_reports_summary() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let (orchestrator, invocation, events) = build_review_orchestrator_with_reporter(
        repo_dir.path(),
        wt_dir.path(),
        &task,
        ApprovedReviewFactory,
        false,
    );

    orchestrator
        .run_review_for_existing_pr(invocation)
        .await
        .unwrap();

    let events = events.lock().unwrap();
    let summary = events
        .iter()
        .find_map(|e| match e {
            PipelineEvent::ReviewSummary { body } => Some(body.clone()),
            _ => None,
        })
        .expect("should have ReviewSummary event");
    assert_eq!(summary, "All good.");
}

#[tokio::test]
async fn test_review_reports_pr_url() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let (orchestrator, invocation, events) = build_review_orchestrator_with_reporter(
        repo_dir.path(),
        wt_dir.path(),
        &task,
        ApprovedReviewFactory,
        false,
    );

    orchestrator
        .run_review_for_existing_pr(invocation)
        .await
        .unwrap();

    let events = events.lock().unwrap();
    let url = events
        .iter()
        .find_map(|e| match e {
            PipelineEvent::PrUrl { url } => Some(url.clone()),
            _ => None,
        })
        .expect("should have PrUrl event");
    assert_eq!(url, "https://github.com/test/repo/pull/42");
}

// --- Iteration-level ProgressReporter tests ---

/// Creates orchestrator with capturing reporter for iteration-level (`run_once`) tests.
#[allow(clippy::type_complexity)]
fn build_iteration_orchestrator_with_reporter<F: ReviewRunnerFactory>(
    repo_dir: &Path,
    wt_dir: &Path,
    tasks: Vec<Task>,
    factory: F,
    dry_run: bool,
    existing_pr_for_issue: Option<u64>,
) -> (
    Orchestrator<MockSource, MockRunner, MockSubmission, F, CapturingReporter>,
    Arc<Mutex<Vec<PipelineEvent>>>,
) {
    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));
    let source = MockSource::new(tasks, Arc::clone(&source_tracker));
    let submission = MockSubmission::new(Arc::clone(&sub_tracker), existing_pr_for_issue);
    let worktree_mgr = WorktreeManager::new(
        repo_dir.to_path_buf(),
        wt_dir.to_path_buf(),
        "main".to_string(),
    );
    let state_dir = repo_dir.join(".rlph-test-state");

    let (reporter, events) = CapturingReporter::new();

    let orchestrator = Orchestrator::new(
        source,
        MockRunner::new("gh-42"),
        submission,
        worktree_mgr,
        StateManager::new(&state_dir),
        PromptEngine::new(None),
        make_config(dry_run),
        repo_dir.to_path_buf(),
    )
    .with_review_factory(factory)
    .with_reporter(reporter);

    (orchestrator, events)
}

#[tokio::test]
async fn test_iteration_reports_fetching_tasks() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let (orchestrator, events) = build_iteration_orchestrator_with_reporter(
        repo_dir.path(),
        wt_dir.path(),
        vec![task],
        ApprovedReviewFactory,
        true,
        None,
    );

    orchestrator.run_once().await.unwrap();

    let events = events.lock().unwrap();
    assert!(
        events.contains(&PipelineEvent::FetchingTasks),
        "expected FetchingTasks event"
    );
}

#[tokio::test]
async fn test_iteration_reports_tasks_found() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let (orchestrator, events) = build_iteration_orchestrator_with_reporter(
        repo_dir.path(),
        wt_dir.path(),
        vec![task],
        ApprovedReviewFactory,
        true,
        None,
    );

    orchestrator.run_once().await.unwrap();

    let events = events.lock().unwrap();
    assert!(
        events.contains(&PipelineEvent::TasksFound { count: 1 }),
        "expected TasksFound {{ count: 1 }}"
    );
}

#[tokio::test]
async fn test_iteration_reports_task_selected() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let (orchestrator, events) = build_iteration_orchestrator_with_reporter(
        repo_dir.path(),
        wt_dir.path(),
        vec![task],
        ApprovedReviewFactory,
        true,
        None,
    );

    orchestrator.run_once().await.unwrap();

    let events = events.lock().unwrap();
    assert!(
        events.contains(&PipelineEvent::TaskSelected {
            issue_number: 42,
            title: "Fix bug".to_string(),
        }),
        "expected TaskSelected event"
    );
}

#[tokio::test]
async fn test_iteration_reports_implement_started() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let (orchestrator, events) = build_iteration_orchestrator_with_reporter(
        repo_dir.path(),
        wt_dir.path(),
        vec![task],
        ApprovedReviewFactory,
        true,
        None,
    );

    orchestrator.run_once().await.unwrap();

    let events = events.lock().unwrap();
    assert!(
        events.contains(&PipelineEvent::ImplementStarted),
        "expected ImplementStarted event"
    );
}

#[tokio::test]
async fn test_iteration_reports_pr_created() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let (orchestrator, events) = build_iteration_orchestrator_with_reporter(
        repo_dir.path(),
        wt_dir.path(),
        vec![task],
        ApprovedReviewFactory,
        false, // not dry_run — pr_created only fires on real submit
        None,
    );

    orchestrator.run_once().await.unwrap();

    let events = events.lock().unwrap();
    let pr_created = events
        .iter()
        .find(|e| matches!(e, PipelineEvent::PrCreated { .. }))
        .expect("expected PrCreated event");
    match pr_created {
        PipelineEvent::PrCreated { url } => {
            assert!(
                url.contains("github.com"),
                "PR URL should contain github.com, got: {url}"
            );
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn test_iteration_reports_iteration_complete() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let (orchestrator, events) = build_iteration_orchestrator_with_reporter(
        repo_dir.path(),
        wt_dir.path(),
        vec![task],
        ApprovedReviewFactory,
        true,
        None,
    );

    orchestrator.run_once().await.unwrap();

    let events = events.lock().unwrap();
    assert!(
        events.contains(&PipelineEvent::IterationComplete {
            issue_number: 42,
            title: "Fix bug".to_string(),
        }),
        "expected IterationComplete event"
    );
}

#[tokio::test]
async fn test_iteration_reports_full_event_sequence() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let (orchestrator, events) = build_iteration_orchestrator_with_reporter(
        repo_dir.path(),
        wt_dir.path(),
        vec![task],
        ApprovedReviewFactory,
        true,
        None,
    );

    orchestrator.run_once().await.unwrap();

    let events = events.lock().unwrap();
    // Extract only iteration-level events (exclude review-level events)
    let iteration_events: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                PipelineEvent::FetchingTasks
                    | PipelineEvent::TasksFound { .. }
                    | PipelineEvent::TaskSelected { .. }
                    | PipelineEvent::ImplementStarted
                    | PipelineEvent::IterationComplete { .. }
            )
        })
        .collect();

    assert_eq!(
        iteration_events.len(),
        5,
        "expected 5 iteration events, got {iteration_events:?}"
    );
    assert_eq!(*iteration_events[0], PipelineEvent::FetchingTasks);
    assert_eq!(*iteration_events[1], PipelineEvent::TasksFound { count: 1 });
    assert_eq!(
        *iteration_events[2],
        PipelineEvent::TaskSelected {
            issue_number: 42,
            title: "Fix bug".to_string(),
        }
    );
    assert_eq!(*iteration_events[3], PipelineEvent::ImplementStarted);
    assert_eq!(
        *iteration_events[4],
        PipelineEvent::IterationComplete {
            issue_number: 42,
            title: "Fix bug".to_string(),
        }
    );
}

// --- Malformed JSON correction tests ---

/// Mock correction runner that returns a sequence of responses.
/// Each call to `resume` pops the next response from the queue.
struct MockCorrectionRunner {
    responses: Mutex<VecDeque<Result<RunResult>>>,
}

impl MockCorrectionRunner {
    fn new(responses: Vec<Result<RunResult>>) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from(responses)),
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
        _working_dir: &Path,
        _timeout: Option<Duration>,
    ) -> Result<RunResult> {
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(Error::AgentRunner("no more correction responses".into())))
    }
}

/// Review factory where the phase runner returns malformed JSON (with session_id),
/// but aggregator returns valid approved JSON.
struct MalformedPhaseFactory {
    /// If true, correction will eventually succeed (via MockCorrectionRunner).
    /// The factory itself always returns the same malformed output.
    stdout: String,
}

impl ReviewRunnerFactory for MalformedPhaseFactory {
    fn create_phase_runner(&self, _phase: &ReviewPhaseConfig, _timeout_retries: u32) -> AnyRunner {
        let stdout = self.stdout.clone();
        AnyRunner::Callback(CallbackRunner::new(Arc::new(
            move |_phase, _prompt, _dir| {
                let stdout = stdout.clone();
                Box::pin(async move {
                    Ok(RunResult {
                        exit_code: 0,
                        stdout,
                        stderr: String::new(),
                        session_id: Some("sess-phase-123".into()),
                    })
                })
            },
        )))
    }

    fn create_step_runner(&self, _step: &ReviewStepConfig, _timeout_retries: u32) -> AnyRunner {
        AnyRunner::Callback(CallbackRunner::new(Arc::new(|phase, _prompt, _dir| {
            Box::pin(async move {
                let stdout = match phase {
                    Phase::ReviewAggregate => r#"{"verdict":"approved","comment":"All good.","findings":[],"fix_instructions":null}"#.to_string(),
                    Phase::ReviewFix => r#"{"status":"fixed","summary":"done","files_changed":[]}"#.to_string(),
                    _ => String::new(),
                };
                Ok(RunResult {
                    exit_code: 0,
                    stdout,
                    stderr: String::new(),
                    session_id: None,
                })
            })
        })))
    }
}

/// Review factory where aggregator returns malformed JSON (with session_id),
/// but phase returns valid JSON.
struct MalformedAggregatorFactory {
    agg_stdout: String,
}

impl ReviewRunnerFactory for MalformedAggregatorFactory {
    fn create_phase_runner(&self, _phase: &ReviewPhaseConfig, _timeout_retries: u32) -> AnyRunner {
        AnyRunner::Callback(CallbackRunner::new(Arc::new(|_phase, _prompt, _dir| {
            Box::pin(async {
                Ok(RunResult {
                    exit_code: 0,
                    stdout: r#"{"findings":[]}"#.into(),
                    stderr: String::new(),
                    session_id: None,
                })
            })
        })))
    }

    fn create_step_runner(&self, _step: &ReviewStepConfig, _timeout_retries: u32) -> AnyRunner {
        let agg_stdout = self.agg_stdout.clone();
        AnyRunner::Callback(CallbackRunner::new(Arc::new(
            move |phase, _prompt, _dir| {
                let agg_stdout = agg_stdout.clone();
                Box::pin(async move {
                    let stdout = match phase {
                        Phase::ReviewAggregate => agg_stdout,
                        Phase::ReviewFix => {
                            r#"{"status":"fixed","summary":"done","files_changed":[]}"#.to_string()
                        }
                        _ => String::new(),
                    };
                    Ok(RunResult {
                        exit_code: 0,
                        stdout,
                        stderr: String::new(),
                        session_id: Some("sess-agg-456".into()),
                    })
                })
            },
        )))
    }
}

/// Review factory where the fix runner returns malformed JSON (with session_id),
/// aggregator returns needs_fix on the first round so the fix phase runs,
/// then approved on subsequent rounds.
struct MalformedFixFactory {
    fix_stdout: String,
    agg_calls: Arc<AtomicUsize>,
}

impl MalformedFixFactory {
    fn new(fix_stdout: &str) -> Self {
        Self {
            fix_stdout: fix_stdout.into(),
            agg_calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl ReviewRunnerFactory for MalformedFixFactory {
    fn create_phase_runner(&self, _phase: &ReviewPhaseConfig, _timeout_retries: u32) -> AnyRunner {
        AnyRunner::Callback(CallbackRunner::new(Arc::new(|_phase, _prompt, _dir| {
            Box::pin(async {
                Ok(RunResult {
                    exit_code: 0,
                    stdout: r#"{"findings":[]}"#.into(),
                    stderr: String::new(),
                    session_id: None,
                })
            })
        })))
    }

    fn create_step_runner(&self, _step: &ReviewStepConfig, _timeout_retries: u32) -> AnyRunner {
        let fix_stdout = self.fix_stdout.clone();
        let agg_calls = Arc::clone(&self.agg_calls);
        AnyRunner::Callback(CallbackRunner::new(Arc::new(
            move |phase, _prompt, _dir| {
                let fix_stdout = fix_stdout.clone();
                let agg_calls = Arc::clone(&agg_calls);
                Box::pin(async move {
                    let stdout = match phase {
                        Phase::ReviewAggregate => {
                            let call = agg_calls.fetch_add(1, Ordering::SeqCst);
                            if call == 0 {
                                // First round: needs_fix
                                r#"{"verdict":"needs_fix","comment":"Issues","findings":[],"fix_instructions":"fix it"}"#.to_string()
                            } else {
                                // Second round: approved
                                r#"{"verdict":"approved","comment":"Fixed.","findings":[],"fix_instructions":null}"#.to_string()
                            }
                        }
                        Phase::ReviewFix => fix_stdout,
                        _ => String::new(),
                    };
                    Ok(RunResult {
                        exit_code: 0,
                        stdout,
                        stderr: String::new(),
                        session_id: Some("sess-fix-789".into()),
                    })
                })
            },
        )))
    }
}

/// Helper to build an orchestrator for correction tests using `run_review_for_existing_pr`.
fn build_correction_test_orchestrator<F: ReviewRunnerFactory>(
    repo_dir: &Path,
    wt_dir: &Path,
    task: &Task,
    factory: F,
    correction_runner: MockCorrectionRunner,
) -> (
    impl std::future::Future<Output = Result<()>>,
    Arc<Mutex<Vec<PipelineEvent>>>,
) {
    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));
    let source = MockSource::new(vec![task.clone()], Arc::clone(&source_tracker));
    let submission = MockSubmission::new(Arc::clone(&sub_tracker), None);
    let worktree_mgr = WorktreeManager::new(
        repo_dir.to_path_buf(),
        wt_dir.to_path_buf(),
        "main".to_string(),
    );
    let worktree_info = worktree_mgr.create(42, "correction-test").unwrap();
    let state_dir = repo_dir.join(".rlph-test-state");
    let vars = make_review_vars(task, repo_dir, &worktree_info.branch, &worktree_info.path);

    let (reporter, events) = CapturingReporter::new();

    let orchestrator = Orchestrator::new(
        source,
        MockRunner::new("gh-42"),
        submission,
        worktree_mgr,
        StateManager::new(&state_dir),
        PromptEngine::new(None),
        make_config(true),
        repo_dir.to_path_buf(),
    )
    .with_review_factory(factory)
    .with_reporter(reporter)
    .with_correction_runner(correction_runner);

    let invocation = ReviewInvocation {
        task_id_for_state: "gh-42".to_string(),
        mark_in_review_task_id: None,
        worktree_info,
        vars,
        comment_pr_number: Some(77),
        push_remote_branch: None,
    };

    let fut = async move { orchestrator.run_review_for_existing_pr(invocation).await };
    (fut, events)
}

// --- Phase malformed JSON correction tests ---

#[tokio::test]
async fn test_phase_malformed_json_correction_succeeds() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let factory = MalformedPhaseFactory {
        stdout: "NOT VALID JSON {{{{".into(),
    };
    // Correction returns valid phase JSON — one response per phase (3 phases)
    let valid_phase = || {
        Ok(RunResult {
        exit_code: 0,
        stdout: r#"{"findings":[{"file":"src/main.rs","line":1,"severity":"warning","description":"corrected finding"}]}"#.into(),
        stderr: String::new(),
        session_id: Some("sess-phase-123".into()),
    })
    };
    let correction = MockCorrectionRunner::new(vec![valid_phase(), valid_phase(), valid_phase()]);

    let (fut, events) = build_correction_test_orchestrator(
        repo_dir.path(),
        wt_dir.path(),
        &task,
        factory,
        correction,
    );
    fut.await.unwrap();

    // Review should complete successfully with corrected findings rendered in summary.
    let events = events.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PipelineEvent::ReviewSummary { .. })),
        "expected ReviewSummary after successful correction"
    );
}

#[tokio::test]
async fn test_phase_malformed_json_correction_exhausted_fails_round() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let factory = MalformedPhaseFactory {
        stdout: "MALFORMED PHASE OUTPUT".into(),
    };
    // Both correction attempts return invalid JSON
    let correction = MockCorrectionRunner::new(vec![
        Ok(RunResult {
            exit_code: 0,
            stdout: "still not valid json".into(),
            stderr: String::new(),
            session_id: Some("sess-phase-123".into()),
        }),
        Ok(RunResult {
            exit_code: 0,
            stdout: "yet more garbage".into(),
            stderr: String::new(),
            session_id: Some("sess-phase-123".into()),
        }),
    ]);

    let (fut, _events) = build_correction_test_orchestrator(
        repo_dir.path(),
        wt_dir.path(),
        &task,
        factory,
        correction,
    );
    // Should fail — phase JSON recovery exhausted retries the round until max_review_rounds
    let err = fut.await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("review did not complete"),
        "expected review failure after correction exhaustion, got: {msg}"
    );
    assert!(
        msg.contains("review phase") && msg.contains("malformed JSON"),
        "expected descriptive parse-failure context in error, got: {msg}"
    );
}

// --- Aggregator malformed JSON correction tests ---

#[tokio::test]
async fn test_aggregator_malformed_json_correction_succeeds() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let factory = MalformedAggregatorFactory {
        agg_stdout: "BROKEN AGG JSON".into(),
    };
    // Correction returns valid aggregator JSON (approved)
    let correction = MockCorrectionRunner::new(vec![
        Ok(RunResult {
            exit_code: 0,
            stdout: r#"{"verdict":"approved","comment":"Corrected review.","findings":[],"fix_instructions":null}"#.into(),
            stderr: String::new(),
            session_id: Some("sess-agg-456".into()),
        }),
    ]);

    let (fut, events) = build_correction_test_orchestrator(
        repo_dir.path(),
        wt_dir.path(),
        &task,
        factory,
        correction,
    );
    fut.await.unwrap();

    let events = events.lock().unwrap();
    let summary = events.iter().find_map(|e| match e {
        PipelineEvent::ReviewSummary { body } => Some(body.clone()),
        _ => None,
    });
    assert_eq!(
        summary.as_deref(),
        Some("Corrected review."),
        "expected corrected aggregator comment in summary"
    );
}

#[tokio::test]
async fn test_aggregator_malformed_json_correction_exhausted_retries_round() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    // Aggregator always returns malformed JSON — after correction exhaustion the
    // orchestrator will `continue` to the next review round. With max_review_rounds=3
    // and 3 review phases, we need 3 rounds * (3 phases + 1 agg) * 2 corrections = 18
    // correction responses. But actually, only the aggregator triggers correction,
    // so we need 3 rounds * 2 retries = 6 correction responses, all invalid.
    let factory = MalformedAggregatorFactory {
        agg_stdout: "BROKEN AGG JSON".into(),
    };
    let mut correction_responses = Vec::new();
    for _ in 0..6 {
        correction_responses.push(Ok(RunResult {
            exit_code: 0,
            stdout: "still broken".into(),
            stderr: String::new(),
            session_id: Some("sess-agg-456".into()),
        }));
    }
    let correction = MockCorrectionRunner::new(correction_responses);

    let (fut, _events) = build_correction_test_orchestrator(
        repo_dir.path(),
        wt_dir.path(),
        &task,
        factory,
        correction,
    );
    // Aggregator correction exhaustion → `continue` each round → eventually exhausts
    // all max_review_rounds and hits "review did not complete"
    let err = fut.await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("review did not complete"),
        "expected review exhaustion error, got: {msg}"
    );
    assert!(
        msg.contains("aggregator malformed JSON"),
        "expected descriptive parse-failure context in error, got: {msg}"
    );
}

// --- Fix malformed JSON correction tests ---
// Fix phase only runs when review_only=false, so we use run_once() which goes
// through the full choose→implement→review→fix flow.

#[allow(clippy::type_complexity)]
fn build_fix_correction_orchestrator(
    repo_dir: &Path,
    wt_dir: &Path,
    task: Task,
    factory: MalformedFixFactory,
    correction: MockCorrectionRunner,
) -> (
    Orchestrator<
        MockSource,
        MockRunner,
        MockSubmission,
        MalformedFixFactory,
        CapturingReporter,
        MockCorrectionRunner,
    >,
    Arc<Mutex<Vec<PipelineEvent>>>,
) {
    let source_tracker = Arc::new(Mutex::new(SourceTracker::default()));
    let sub_tracker = Arc::new(Mutex::new(SubmissionTracker::default()));
    let source = MockSource::new(vec![task], Arc::clone(&source_tracker));
    let submission = MockSubmission::new(Arc::clone(&sub_tracker), None);
    let worktree_mgr = WorktreeManager::new(
        repo_dir.to_path_buf(),
        wt_dir.to_path_buf(),
        "main".to_string(),
    );
    let state_dir = repo_dir.join(".rlph-test-state");
    let (reporter, events) = CapturingReporter::new();

    let orchestrator = Orchestrator::new(
        source,
        MockRunner::new("gh-42"),
        submission,
        worktree_mgr,
        StateManager::new(&state_dir),
        PromptEngine::new(None),
        make_config(true),
        repo_dir.to_path_buf(),
    )
    .with_review_factory(factory)
    .with_reporter(reporter)
    .with_correction_runner(correction);

    (orchestrator, events)
}

#[tokio::test]
async fn test_fix_malformed_json_correction_succeeds() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let factory = MalformedFixFactory::new("BROKEN FIX JSON");
    // Correction returns valid fix JSON on first attempt
    let correction = MockCorrectionRunner::new(vec![Ok(RunResult {
        exit_code: 0,
        stdout: r#"{"status":"fixed","summary":"corrected fix","files_changed":["src/main.rs"]}"#
            .into(),
        stderr: String::new(),
        session_id: Some("sess-fix-789".into()),
    })]);

    let (orchestrator, events) = build_fix_correction_orchestrator(
        repo_dir.path(),
        wt_dir.path(),
        task,
        factory,
        correction,
    );
    orchestrator.run_once().await.unwrap();

    // Fix correction succeeded → review continues to round 2 where aggregator approves
    let events = events.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PipelineEvent::ReviewSummary { .. })),
        "expected review to complete after fix correction"
    );
}

#[tokio::test]
async fn test_fix_malformed_json_correction_exhausted_retries_round() {
    let (_bare, repo_dir, wt_dir) = setup_git_repo();
    let task = make_task(42, "Fix bug");

    let factory = MalformedFixFactory::new("BROKEN FIX JSON");
    // Both correction attempts return invalid JSON
    let correction = MockCorrectionRunner::new(vec![
        Ok(RunResult {
            exit_code: 0,
            stdout: "still broken fix".into(),
            stderr: String::new(),
            session_id: Some("sess-fix-789".into()),
        }),
        Ok(RunResult {
            exit_code: 0,
            stdout: "yet more garbage fix".into(),
            stderr: String::new(),
            session_id: Some("sess-fix-789".into()),
        }),
    ]);

    let (orchestrator, events) = build_fix_correction_orchestrator(
        repo_dir.path(),
        wt_dir.path(),
        task,
        factory,
        correction,
    );
    // Fix correction exhaustion → retries round → round 2 aggregator approves
    orchestrator.run_once().await.unwrap();

    let events = events.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PipelineEvent::ReviewSummary { .. })),
        "expected review to complete after fix correction exhaustion triggers round retry"
    );
}
