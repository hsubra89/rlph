use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::config::{Config, ReviewPhaseConfig, ReviewStepConfig};
use crate::deps::DependencyGraph;
use crate::error::{Error, Result};
use crate::prompts::PromptEngine;
use crate::review_schema::{
    SchemaName, Verdict, correction_prompt, parse_aggregator_output, parse_fix_output,
    parse_phase_output, render_findings_for_github, render_findings_for_prompt,
};
use crate::runner::{
    AgentRunner, AnyRunner, Phase, RunResult, RunnerKind, build_runner, resume_with_correction,
};
use crate::sources::{Task, TaskSource};
use crate::state::StateManager;
use crate::submission::{REVIEW_MARKER, SubmissionBackend, format_pr_comments_for_prompt};
use crate::worktree::{WorktreeInfo, WorktreeManager, validate_branch_name};

#[derive(Debug)]
struct ReviewPhaseOutput {
    name: String,
    stdout: String,
    session_id: Option<String>,
}

#[derive(Deserialize)]
struct TaskSelection {
    id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IterationOutcome {
    ProcessedTask,
    NoEligibleTasks,
}

pub struct ReviewInvocation {
    pub task_id_for_state: String,
    pub mark_in_review_task_id: Option<String>,
    pub worktree_info: WorktreeInfo,
    pub vars: HashMap<String, String>,
    pub comment_pr_number: Option<u64>,
    pub push_remote_branch: Option<String>,
}

/// Factory for creating review-phase runners. Defaults to `build_runner`.
/// Override in tests to inject mock runners.
pub trait ReviewRunnerFactory: Send + Sync {
    fn create_phase_runner(&self, phase: &ReviewPhaseConfig, timeout_retries: u32) -> AnyRunner;
    fn create_step_runner(
        &self,
        step: &ReviewStepConfig,
        timeout_retries: u32,
        name: &str,
    ) -> AnyRunner;
}

/// Default factory that creates real runners from config.
#[derive(Default)]
pub struct DefaultReviewRunnerFactory {
    /// When true, runners stream formatted agent messages to stderr.
    pub stream: bool,
}

impl ReviewRunnerFactory for DefaultReviewRunnerFactory {
    fn create_phase_runner(&self, phase: &ReviewPhaseConfig, timeout_retries: u32) -> AnyRunner {
        let runner = build_runner(
            phase.runner,
            &phase.agent_binary,
            phase.agent_model.as_deref(),
            phase.agent_effort.as_deref(),
            phase.agent_variant.as_deref(),
            phase.agent_timeout.map(Duration::from_secs),
            timeout_retries,
        );
        if self.stream {
            runner.with_stream_prefix(format!("review:{}", phase.name))
        } else {
            runner
        }
    }

    fn create_step_runner(
        &self,
        step: &ReviewStepConfig,
        timeout_retries: u32,
        name: &str,
    ) -> AnyRunner {
        let runner = build_runner(
            step.runner,
            &step.agent_binary,
            step.agent_model.as_deref(),
            step.agent_effort.as_deref(),
            step.agent_variant.as_deref(),
            step.agent_timeout.map(Duration::from_secs),
            timeout_retries,
        );
        if self.stream {
            runner.with_stream_prefix(format!("review:{name}"))
        } else {
            runner
        }
    }
}

/// Abstraction over session-resume correction calls.
/// Override in tests to avoid spawning real agent processes.
pub trait CorrectionRunner: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    fn resume(
        &self,
        runner_type: RunnerKind,
        agent_binary: &str,
        model: Option<&str>,
        effort: Option<&str>,
        variant: Option<&str>,
        session_id: &str,
        correction_prompt: &str,
        working_dir: &Path,
        timeout: Option<Duration>,
    ) -> impl std::future::Future<Output = Result<RunResult>> + Send;
}

/// Default implementation that calls the real `resume_with_correction`.
pub struct DefaultCorrectionRunner;

impl CorrectionRunner for DefaultCorrectionRunner {
    async fn resume(
        &self,
        runner_type: RunnerKind,
        agent_binary: &str,
        model: Option<&str>,
        effort: Option<&str>,
        variant: Option<&str>,
        session_id: &str,
        correction_prompt: &str,
        working_dir: &Path,
        timeout: Option<Duration>,
    ) -> Result<RunResult> {
        resume_with_correction(
            runner_type,
            agent_binary,
            model,
            effort,
            variant,
            session_id,
            correction_prompt,
            working_dir,
            timeout,
        )
        .await
    }
}

/// Observer for structured pipeline progress events.
pub trait ProgressReporter: Send + Sync {
    // Iteration-level
    fn fetching_tasks(&self);
    fn tasks_found(&self, count: usize);
    fn task_selected(&self, issue_number: u64, title: &str);
    fn implement_started(&self);
    /// Fires after a new PR is submitted (inside `run_implement_review`).
    /// Skipped in dry-run mode and when an existing PR is reused.
    fn pr_created(&self, url: &str);
    fn iteration_complete(&self, issue_number: u64, title: &str);

    // Review (existing, unchanged)
    fn phases_started(&self, names: &[String]);
    fn phase_complete(&self, name: &str);
    fn review_summary(&self, body: &str);
    /// Fires at the end of `run_review_pipeline` after all review rounds complete.
    /// Fires even when an existing PR was reused.
    fn pr_url(&self, url: &str);
}

/// Default reporter that prints to stderr.
pub struct StderrReporter;

impl ProgressReporter for StderrReporter {
    fn fetching_tasks(&self) {
        eprintln!("[rlph] Fetching eligible tasks...");
    }

    fn tasks_found(&self, count: usize) {
        eprintln!("[rlph] Found {count} eligible task(s)");
    }

    fn task_selected(&self, issue_number: u64, title: &str) {
        eprintln!("[rlph] Selected #{issue_number}: {title}");
    }

    fn implement_started(&self) {
        eprintln!("[rlph] Implementing...");
    }

    fn pr_created(&self, url: &str) {
        eprintln!("[rlph] PR created: {url}");
    }

    fn iteration_complete(&self, issue_number: u64, title: &str) {
        eprintln!("[rlph] Done with #{issue_number}: {title}");
    }

    fn phases_started(&self, names: &[String]) {
        eprintln!(
            "[rlph] Running {} review agents: {}",
            names.len(),
            names.join(", ")
        );
    }

    fn phase_complete(&self, name: &str) {
        eprintln!("[rlph] Review phase complete: {name}");
    }

    fn review_summary(&self, body: &str) {
        eprintln!("[rlph] Review summary:\n{body}");
    }

    fn pr_url(&self, url: &str) {
        eprintln!("[rlph] PR: {url}");
    }
}

pub struct Orchestrator<
    S,
    R,
    B,
    F = DefaultReviewRunnerFactory,
    P = StderrReporter,
    C = DefaultCorrectionRunner,
> {
    source: S,
    runner: R,
    submission: B,
    worktree_mgr: WorktreeManager,
    state_mgr: StateManager,
    prompt_engine: PromptEngine,
    config: Config,
    repo_root: PathBuf,
    review_factory: F,
    reporter: P,
    correction_runner: C,
}

impl<S: TaskSource, R: AgentRunner, B: SubmissionBackend> Orchestrator<S, R, B> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source: S,
        runner: R,
        submission: B,
        worktree_mgr: WorktreeManager,
        state_mgr: StateManager,
        prompt_engine: PromptEngine,
        config: Config,
        repo_root: PathBuf,
    ) -> Self {
        Self {
            source,
            runner,
            submission,
            worktree_mgr,
            state_mgr,
            prompt_engine,
            config,
            repo_root,
            review_factory: DefaultReviewRunnerFactory::default(),
            reporter: StderrReporter,
            correction_runner: DefaultCorrectionRunner,
        }
    }
}

impl<S: TaskSource, R: AgentRunner, B: SubmissionBackend, F, P, C> Orchestrator<S, R, B, F, P, C> {
    pub fn with_review_factory<F2>(self, review_factory: F2) -> Orchestrator<S, R, B, F2, P, C> {
        Orchestrator {
            source: self.source,
            runner: self.runner,
            submission: self.submission,
            worktree_mgr: self.worktree_mgr,
            state_mgr: self.state_mgr,
            prompt_engine: self.prompt_engine,
            config: self.config,
            repo_root: self.repo_root,
            review_factory,
            reporter: self.reporter,
            correction_runner: self.correction_runner,
        }
    }

    pub fn with_reporter<P2>(self, reporter: P2) -> Orchestrator<S, R, B, F, P2, C> {
        Orchestrator {
            source: self.source,
            runner: self.runner,
            submission: self.submission,
            worktree_mgr: self.worktree_mgr,
            state_mgr: self.state_mgr,
            prompt_engine: self.prompt_engine,
            config: self.config,
            repo_root: self.repo_root,
            review_factory: self.review_factory,
            reporter,
            correction_runner: self.correction_runner,
        }
    }

    pub fn with_correction_runner<C2>(
        self,
        correction_runner: C2,
    ) -> Orchestrator<S, R, B, F, P, C2> {
        Orchestrator {
            source: self.source,
            runner: self.runner,
            submission: self.submission,
            worktree_mgr: self.worktree_mgr,
            state_mgr: self.state_mgr,
            prompt_engine: self.prompt_engine,
            config: self.config,
            repo_root: self.repo_root,
            review_factory: self.review_factory,
            reporter: self.reporter,
            correction_runner,
        }
    }
}

impl<
    S: TaskSource,
    R: AgentRunner,
    B: SubmissionBackend,
    F: ReviewRunnerFactory,
    P: ProgressReporter,
    C: CorrectionRunner,
> Orchestrator<S, R, B, F, P, C>
{
    /// Run according to configured loop mode.
    ///
    /// When `shutdown` becomes true, the orchestrator exits between iterations.
    pub async fn run_loop(&self, mut shutdown: Option<watch::Receiver<bool>>) -> Result<()> {
        if self.config.once {
            return self.run_once().await;
        }

        let mut iterations = 0u32;

        loop {
            if Self::shutdown_requested(shutdown.as_ref()) {
                info!("shutdown requested, exiting loop");
                break;
            }

            self.run_iteration().await?;
            iterations += 1;

            if let Some(max) = self.config.max_iterations
                && iterations >= max
            {
                info!(max, "reached max iterations, exiting");
                break;
            }

            if !self.config.continuous {
                if self.config.max_iterations.is_none() {
                    break;
                }
                continue;
            }

            if Self::shutdown_requested(shutdown.as_ref()) {
                info!("shutdown requested, exiting loop");
                break;
            }

            info!(poll_seconds = self.config.poll_seconds, "polling again");
            let stop = Self::wait_for_poll_or_shutdown(
                Duration::from_secs(self.config.poll_seconds),
                &mut shutdown,
            )
            .await;
            if stop {
                info!("shutdown requested, exiting loop");
                break;
            }
        }

        Ok(())
    }

    /// Run a single iteration of the orchestrator loop.
    pub async fn run_once(&self) -> Result<()> {
        let _ = self.run_iteration().await?;
        Ok(())
    }

    /// Run only the review pipeline for an already-selected PR/worktree context.
    pub async fn run_review_for_existing_pr(&self, invocation: ReviewInvocation) -> Result<()> {
        self.state_mgr.set_current_task(
            &invocation.task_id_for_state,
            "review",
            &invocation.worktree_info.path.display().to_string(),
        )?;

        if !self.config.dry_run
            && let Some(task_id) = invocation.mark_in_review_task_id.as_deref()
        {
            self.source.mark_in_review(task_id)?;
        }

        let result = self
            .run_review_pipeline(
                &invocation.vars,
                &invocation.worktree_info,
                invocation.comment_pr_number,
                invocation.push_remote_branch.as_deref(),
                true,
            )
            .await;

        match result {
            Ok(()) => {
                self.state_mgr.complete_current_task()?;

                info!("cleaning up worktree");
                if let Err(e) = self.worktree_mgr.remove(&invocation.worktree_info.path) {
                    warn!(error = %e, "failed to clean up worktree");
                }
                let _ = self
                    .state_mgr
                    .remove_worktree_mapping(&invocation.task_id_for_state);

                info!("review-only run complete");
                Ok(())
            }
            Err(e) => {
                warn!(error = %e, "review-only run failed");
                Err(e)
            }
        }
    }

    async fn run_iteration(&self) -> Result<IterationOutcome> {
        // 1. Fetch eligible tasks and filter by dependency graph
        self.reporter.fetching_tasks();
        info!("fetching eligible tasks");
        let tasks = self.source.fetch_eligible_tasks()?;
        if tasks.is_empty() {
            info!("no eligible tasks found");
            return Ok(IterationOutcome::NoEligibleTasks);
        }

        let done_ids = self.source.fetch_closed_task_ids()?;
        let graph = DependencyGraph::build(&tasks);
        let tasks = graph.filter_eligible(tasks, &done_ids);
        if tasks.is_empty() {
            info!("no unblocked tasks found");
            return Ok(IterationOutcome::NoEligibleTasks);
        }
        info!(count = tasks.len(), "found eligible tasks");
        self.reporter.tasks_found(tasks.len());

        // 2. Choose phase — agent selects a task (skip if only one)
        let task_id = if tasks.len() == 1 {
            let only = &tasks[0];
            let id = format!("gh-{}", only.id);
            info!(task_id = id, "auto-selected only eligible task");
            id
        } else {
            info!("running choose phase");
            let mut choose_vars = HashMap::new();
            choose_vars.insert(
                "repo_path".to_string(),
                self.repo_root.display().to_string(),
            );
            let issues_json = serde_json::to_string_pretty(&tasks)
                .map_err(|e| Error::Orchestrator(format!("failed to serialize tasks: {e}")))?;
            choose_vars.insert("issues_json".to_string(), issues_json);
            let choose_prompt = self.prompt_engine.render_phase("choose", &choose_vars)?;
            let choose_started = Instant::now();
            self.runner
                .run(Phase::Choose, &choose_prompt, &self.repo_root)
                .await?;
            info!(
                elapsed_secs = choose_started.elapsed().as_secs(),
                "choose phase complete"
            );

            // Parse task selection from .rlph/task.toml
            self.parse_task_selection()?
        };
        let issue_number = parse_issue_number(&task_id)?;
        info!(task_id, issue_number, "selected task");
        let existing_pr_number = if self.config.dry_run {
            info!("dry run — skipping existing PR lookup");
            None
        } else {
            let pr_number = self.submission.find_existing_pr_for_issue(issue_number)?;
            if let Some(pr) = pr_number {
                info!(pr, issue_number, "existing PR found");
            } else {
                info!(issue_number, "no existing PR found");
            }
            pr_number
        };

        // 4. Get task details
        let task = self.source.get_task_details(&issue_number.to_string())?;
        info!(id = task.id, title = task.title, "task details");
        self.reporter.task_selected(issue_number, &task.title);

        // 5. Mark in-progress
        if !self.config.dry_run {
            info!("marking task in-progress");
            self.source.mark_in_progress(&task.id)?;
        }

        // 6. Create worktree
        info!("creating worktree");
        let slug = WorktreeManager::slugify(&task.title);
        let worktree_info = self.worktree_mgr.create(issue_number, &slug)?;
        info!(
            path = %worktree_info.path.display(),
            branch = worktree_info.branch,
            "worktree created"
        );

        // Update state
        self.state_mgr.set_current_task(
            &task_id,
            "implement",
            &worktree_info.path.display().to_string(),
        )?;

        // Run the implement → submit → review pipeline, cleaning up on success
        let result = self
            .run_implement_review(&task, issue_number, &worktree_info, existing_pr_number)
            .await;

        match result {
            Ok(()) => {
                // 11. Mark done — skipped; GitHub auto-closes the issue when the PR merges
                self.state_mgr.complete_current_task()?;

                // 12. Clean up worktree
                info!("cleaning up worktree");
                if let Err(e) = self.worktree_mgr.remove(&worktree_info.path) {
                    warn!(error = %e, "failed to clean up worktree");
                }
                let _ = self.state_mgr.remove_worktree_mapping(&task_id);

                info!("iteration complete");
                self.reporter.iteration_complete(issue_number, &task.title);
                Ok(IterationOutcome::ProcessedTask)
            }
            Err(e) => {
                warn!(error = %e, "iteration failed");
                Err(e)
            }
        }
    }

    fn shutdown_requested(shutdown: Option<&watch::Receiver<bool>>) -> bool {
        shutdown.is_some_and(|rx| *rx.borrow())
    }

    async fn wait_for_poll_or_shutdown(
        poll_duration: Duration,
        shutdown: &mut Option<watch::Receiver<bool>>,
    ) -> bool {
        if let Some(rx) = shutdown {
            tokio::select! {
                _ = tokio::time::sleep(poll_duration) => false,
                changed = rx.changed() => {
                    if changed.is_ok() {
                        *rx.borrow()
                    } else {
                        false
                    }
                }
            }
        } else {
            tokio::time::sleep(poll_duration).await;
            false
        }
    }

    /// Implement, submit PR, and review — the inner pipeline after worktree creation.
    async fn run_implement_review(
        &self,
        task: &Task,
        issue_number: u64,
        worktree_info: &WorktreeInfo,
        existing_pr_number: Option<u64>,
    ) -> Result<()> {
        let mut vars = self.initial_task_vars(task, worktree_info);

        // 7. Implement phase
        self.reporter.implement_started();
        info!("running implement phase");
        let impl_prompt = self.prompt_engine.render_phase("implement", &vars)?;
        self.runner
            .run(Phase::Implement, &impl_prompt, &worktree_info.path)
            .await?;

        // 8. Push branch
        if !self.config.dry_run {
            info!("pushing branch");
            self.push_branch(worktree_info)?;
        }

        // 9. Submit PR (skip if choose agent reported an existing PR)
        let pr_number = if let Some(pr) = existing_pr_number {
            info!(pr, "skipping PR submission — existing PR");
            Some(pr)
        } else if !self.config.dry_run {
            info!("submitting PR");
            let pr_body = format!("Resolves #{issue_number}\n\nAutomated implementation by rlph.");
            let result = self.submission.submit(
                &worktree_info.branch,
                &self.config.base_branch,
                &task.title,
                &pr_body,
            )?;
            info!(url = result.url, "PR created");
            self.reporter.pr_created(&result.url);
            vars.insert("pr_url".to_string(), result.url);
            result.number
        } else {
            info!("dry run — skipping PR submission");
            None
        };

        // 10. Mark in-review
        if !self.config.dry_run {
            self.source.mark_in_review(&task.id)?;
        }

        self.run_review_pipeline(&vars, worktree_info, pr_number, None, false)
            .await
    }

    async fn run_review_pipeline(
        &self,
        vars: &HashMap<String, String>,
        worktree_info: &WorktreeInfo,
        pr_number: Option<u64>,
        push_remote_branch: Option<&str>,
        review_only: bool,
    ) -> Result<()> {
        self.state_mgr.update_phase("review")?;
        let max_reviews = if review_only {
            1
        } else {
            self.config.max_review_rounds
        };
        let mut review_passed = false;
        let mut last_json_failure: Option<String> = None;

        // Report phase names once before the loop (they don't change between rounds).
        let phase_names: Vec<String> = self
            .config
            .review_phases
            .iter()
            .map(|p| p.name.clone())
            .collect();
        self.reporter.phases_started(&phase_names);

        for round in 1..=max_reviews {
            info!(round, max_reviews, "review round");

            // Fetch current PR comments for this round
            let (pr_comments_text, has_pr_comments) = if let Some(pr_num) = pr_number {
                match self.submission.fetch_pr_comments(pr_num) {
                    Ok(comments) => {
                        let has = !comments.is_empty();
                        (format_pr_comments_for_prompt(&comments, pr_num), has)
                    }
                    Err(e) => {
                        warn!(error = %e, "failed to fetch PR comments");
                        ("Failed to fetch PR comments.".to_string(), false)
                    }
                }
            } else {
                ("No PR associated with this review.".to_string(), false)
            };

            let pr_number_str = pr_number.map(|n| n.to_string()).unwrap_or_default();

            let mut join_set = tokio::task::JoinSet::new();
            for phase_config in &self.config.review_phases {
                let phase_runner = self
                    .review_factory
                    .create_phase_runner(phase_config, self.config.agent_timeout_retries);

                let mut phase_vars = vars.clone();
                phase_vars.insert("review_phase_name".to_string(), phase_config.name.clone());
                phase_vars.insert("pr_comments".to_string(), pr_comments_text.clone());
                phase_vars.insert("pr_number".to_string(), pr_number_str.clone());
                // upon templates treat empty strings as falsy in {% if has_pr_comments %}
                phase_vars.insert(
                    "has_pr_comments".to_string(),
                    if has_pr_comments {
                        "true".to_string()
                    } else {
                        String::new()
                    },
                );

                let prompt = self
                    .prompt_engine
                    .render_phase(&phase_config.prompt, &phase_vars)?;
                let working_dir = worktree_info.path.clone();
                let phase_name = phase_config.name.clone();

                join_set.spawn(async move {
                    let result = phase_runner
                        .run(Phase::Review, &prompt, &working_dir)
                        .await?;
                    Ok::<ReviewPhaseOutput, Error>(ReviewPhaseOutput {
                        name: phase_name,
                        stdout: result.stdout,
                        session_id: result.session_id,
                    })
                });
            }

            let mut review_outputs = Vec::new();
            while let Some(result) = join_set.join_next().await {
                let output = result.map_err(|e| Error::AgentRunner(e.to_string()))??;
                self.reporter.phase_complete(&output.name);
                review_outputs.push(output);
            }

            let mut review_texts = Vec::new();
            let mut phase_parse_failed = false;
            for o in &review_outputs {
                let rendered = match parse_phase_output(&o.stdout) {
                    Ok(phase) => render_findings_for_prompt(&phase.findings, Some(&o.name)),
                    Err(e) => {
                        // Try correction via session resume
                        let phase_config =
                            self.config.review_phases.iter().find(|p| p.name == o.name);
                        let recovered = if let Some(pc) = phase_config {
                            self.retry_with_correction(
                                o.session_id.as_deref(),
                                pc.runner,
                                &pc.agent_binary,
                                pc.agent_model.as_deref(),
                                pc.agent_effort.as_deref(),
                                pc.agent_variant.as_deref(),
                                pc.agent_timeout,
                                SchemaName::Phase,
                                &e.to_string(),
                                &worktree_info.path,
                                parse_phase_output,
                            )
                            .await
                        } else {
                            None
                        };
                        match recovered {
                            Some(phase) => {
                                render_findings_for_prompt(&phase.findings, Some(&o.name))
                            }
                            None => {
                                warn!(phase = %o.name, error = %e, "phase JSON correction exhausted — retrying round");
                                last_json_failure =
                                    Some(format!("review phase '{}' malformed JSON: {e}", o.name));
                                phase_parse_failed = true;
                                break;
                            }
                        }
                    }
                };
                review_texts.push(format!("## Review Phase: {}\n\n{}", o.name, rendered));
            }
            if phase_parse_failed {
                continue;
            }
            let review_outputs_text = review_texts.join("\n\n---\n\n");

            let agg_config = &self.config.review_aggregate;
            let agg_runner = self.review_factory.create_step_runner(
                agg_config,
                self.config.agent_timeout_retries,
                "aggregate",
            );

            let mut agg_vars = vars.clone();
            agg_vars.insert("review_outputs".to_string(), review_outputs_text);
            agg_vars.insert("pr_comments".to_string(), pr_comments_text.clone());
            agg_vars.insert("pr_number".to_string(), pr_number_str.clone());

            let agg_prompt = self
                .prompt_engine
                .render_phase(&agg_config.prompt, &agg_vars)?;
            let agg_result = agg_runner
                .run(Phase::ReviewAggregate, &agg_prompt, &worktree_info.path)
                .await?;

            let agg_output = match parse_aggregator_output(&agg_result.stdout) {
                Ok(output) => output,
                Err(e) => {
                    // Attempt session resume with correction prompt
                    let recovered = self
                        .retry_with_correction(
                            agg_result.session_id.as_deref(),
                            agg_config.runner,
                            &agg_config.agent_binary,
                            agg_config.agent_model.as_deref(),
                            agg_config.agent_effort.as_deref(),
                            agg_config.agent_variant.as_deref(),
                            agg_config.agent_timeout,
                            SchemaName::Aggregator,
                            &e.to_string(),
                            &worktree_info.path,
                            parse_aggregator_output,
                        )
                        .await;
                    match recovered {
                        Some(output) => output,
                        None => {
                            warn!(error = %e, "aggregator JSON correction failed — retrying round");
                            last_json_failure = Some(format!("aggregator malformed JSON: {e}"));
                            continue;
                        }
                    }
                }
            };

            let comment_body = format!(
                "{REVIEW_MARKER}\n{}",
                render_findings_for_github(&agg_output.findings, &agg_output.comment),
            );
            let summary = agg_output.comment.trim();
            if !summary.is_empty() {
                self.reporter.review_summary(summary);
            }

            if let Some(pr_num) = pr_number
                && !self.config.dry_run
                && let Err(e) = self.submission.upsert_review_comment(pr_num, &comment_body)
            {
                warn!(error = %e, "failed to comment on PR");
            }

            if agg_output.verdict == Verdict::Approved {
                info!(round, "review approved");
                review_passed = true;
                break;
            }

            if review_only {
                info!("review-only mode — skipping fix phase");
                break;
            }

            let fix_instructions = match agg_output.fix_instructions {
                Some(instructions) if !instructions.trim().is_empty() => instructions,
                _ => {
                    warn!(
                        "aggregator verdict is needs_fix but fix_instructions is empty — retrying"
                    );
                    continue;
                }
            };

            info!(round, "review needs fix, running fix agent");

            let fix_config = &self.config.review_fix;
            let fix_runner = self.review_factory.create_step_runner(
                fix_config,
                self.config.agent_timeout_retries,
                "fix",
            );

            let mut fix_vars = vars.clone();
            fix_vars.insert("fix_instructions".to_string(), fix_instructions);

            let fix_prompt = self
                .prompt_engine
                .render_phase(&fix_config.prompt, &fix_vars)?;
            let fix_result = fix_runner
                .run(Phase::ReviewFix, &fix_prompt, &worktree_info.path)
                .await?;

            match parse_fix_output(&fix_result.stdout) {
                Ok(fix_output) => {
                    info!(
                        status = ?fix_output.status,
                        summary = fix_output.summary,
                        files_changed = ?fix_output.files_changed,
                        "fix agent complete"
                    );
                }
                Err(e) => {
                    // Attempt session resume with correction prompt for fix output
                    let recovered = self
                        .retry_with_correction(
                            fix_result.session_id.as_deref(),
                            fix_config.runner,
                            &fix_config.agent_binary,
                            fix_config.agent_model.as_deref(),
                            fix_config.agent_effort.as_deref(),
                            fix_config.agent_variant.as_deref(),
                            fix_config.agent_timeout,
                            SchemaName::Fix,
                            &e.to_string(),
                            &worktree_info.path,
                            parse_fix_output,
                        )
                        .await;
                    match recovered {
                        Some(fix_output) => {
                            info!(
                                status = ?fix_output.status,
                                summary = fix_output.summary,
                                files_changed = ?fix_output.files_changed,
                                "fix agent complete (after correction)"
                            );
                        }
                        None => {
                            warn!(error = %e, "fix agent JSON correction failed — retrying round");
                            last_json_failure = Some(format!("fix agent malformed JSON: {e}"));
                            continue;
                        }
                    }
                }
            }

            if !self.config.dry_run {
                let push_result = if let Some(remote_branch) = push_remote_branch {
                    self.push_branch_to(worktree_info, remote_branch)
                } else {
                    self.push_branch(worktree_info)
                };
                if let Err(e) = push_result {
                    warn!(error = %e, "failed to push review fixes");
                }
            }
        }

        // Report PR URL once after the review loop.
        if let Some(url) = vars.get("pr_url")
            && !url.is_empty()
        {
            self.reporter.pr_url(url);
        }

        if !review_passed {
            let reason = last_json_failure
                .map(|f| format!(" (last failure: {f})"))
                .unwrap_or_default();
            return Err(Error::Orchestrator(format!(
                "review did not complete after {max_reviews} round(s){reason}"
            )));
        }

        Ok(())
    }

    /// Attempt to resume a session with a correction prompt when JSON parsing fails.
    ///
    /// Re-parses the output inside the retry loop so that each subsequent attempt
    /// uses the *actual* parse error from the previous correction (not the original).
    /// Returns `Some(T)` on success, or `None` if no session_id is available or
    /// all retry attempts fail.
    #[allow(clippy::too_many_arguments)]
    async fn retry_with_correction<T>(
        &self,
        session_id: Option<&str>,
        runner_type: RunnerKind,
        agent_binary: &str,
        agent_model: Option<&str>,
        agent_effort: Option<&str>,
        agent_variant: Option<&str>,
        agent_timeout: Option<u64>,
        schema: SchemaName,
        initial_error: &str,
        working_dir: &Path,
        parser: impl Fn(&str) -> Result<T>,
    ) -> Option<T> {
        const MAX_RETRIES: u32 = 2;
        let session_id = session_id?;
        let mut last_error = initial_error.to_string();

        for attempt in 1..=MAX_RETRIES {
            let prompt = correction_prompt(schema, &last_error);
            info!(
                session_id,
                attempt, MAX_RETRIES, "resuming session with correction prompt"
            );

            match self
                .correction_runner
                .resume(
                    runner_type,
                    agent_binary,
                    agent_model,
                    agent_effort,
                    agent_variant,
                    session_id,
                    &prompt,
                    working_dir,
                    agent_timeout.map(Duration::from_secs),
                )
                .await
            {
                Ok(corrected) => match parser(&corrected.stdout) {
                    Ok(parsed) => return Some(parsed),
                    Err(e) => {
                        last_error = e.to_string();
                        warn!(attempt, error = %last_error, "correction output still invalid");
                    }
                },
                Err(e) => {
                    warn!(attempt, error = %e, "correction resume failed");
                    return None;
                }
            }
        }

        None
    }

    /// Parse the task selection from `.rlph/task.toml` written by the choose agent.
    fn parse_task_selection(&self) -> Result<String> {
        let path = self.repo_root.join(".rlph").join("task.toml");
        let content = std::fs::read_to_string(&path).map_err(|e| {
            Error::Orchestrator(format!(
                "failed to read task selection {}: {e}",
                path.display()
            ))
        })?;
        let selection: TaskSelection = toml::from_str(&content)
            .map_err(|e| Error::Orchestrator(format!("failed to parse task selection: {e}")))?;

        // Clean up the selection file
        let _ = std::fs::remove_file(&path);

        Ok(selection.id)
    }

    fn initial_task_vars(&self, task: &Task, worktree: &WorktreeInfo) -> HashMap<String, String> {
        let mut vars = build_task_vars(
            task,
            &self.repo_root,
            &worktree.branch,
            &worktree.path,
            &self.config.base_branch,
        );
        vars.insert("pr_number".to_string(), String::new());
        vars.insert("pr_branch".to_string(), String::new());
        vars
    }

    fn push_branch(&self, worktree: &WorktreeInfo) -> Result<()> {
        let output = Command::new("git")
            .args(["push", "-u", "origin", &worktree.branch])
            .current_dir(&worktree.path)
            .output()
            .map_err(|e| Error::Orchestrator(format!("failed to run git push: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Orchestrator(format!("git push failed: {stderr}")));
        }

        info!(branch = worktree.branch, "pushed branch");
        Ok(())
    }

    fn push_branch_to(&self, worktree: &WorktreeInfo, remote_branch: &str) -> Result<()> {
        validate_branch_name(remote_branch)
            .map_err(|e| Error::Orchestrator(format!("invalid remote branch name: {e}")))?;

        let refspec = format!("HEAD:{remote_branch}");
        let output = Command::new("git")
            .args(["push", "-u", "origin", &refspec])
            .current_dir(&worktree.path)
            .output()
            .map_err(|e| Error::Orchestrator(format!("failed to run git push: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Orchestrator(format!("git push failed: {stderr}")));
        }

        info!(branch = worktree.branch, remote_branch, "pushed branch");
        Ok(())
    }
}

/// Build the base set of template variables for a task.
///
/// Used by the orchestrator internally and available for tests to avoid
/// duplicating the variable map.
pub fn build_task_vars(
    task: &Task,
    repo_path: &Path,
    branch: &str,
    worktree_path: &Path,
    base_branch: &str,
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
        ("base_branch".to_string(), base_branch.to_string()),
    ])
}

/// Extract the issue number from a task ID like "gh-42".
pub fn parse_issue_number(task_id: &str) -> Result<u64> {
    task_id
        .strip_prefix("gh-")
        .and_then(|n| n.parse::<u64>().ok())
        .ok_or_else(|| {
            Error::Orchestrator(format!("invalid task id: {task_id}, expected gh-<number>"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_issue_number_valid() {
        assert_eq!(parse_issue_number("gh-1").unwrap(), 1);
        assert_eq!(parse_issue_number("gh-42").unwrap(), 42);
        assert_eq!(parse_issue_number("gh-999").unwrap(), 999);
    }

    #[test]
    fn test_parse_issue_number_invalid() {
        assert!(parse_issue_number("42").is_err());
        assert!(parse_issue_number("gh-").is_err());
        assert!(parse_issue_number("gh-abc").is_err());
        assert!(parse_issue_number("").is_err());
        assert!(parse_issue_number("linear-42").is_err());
    }

    #[test]
    fn test_parse_aggregator_approved_json() {
        use crate::review_schema::{Verdict, parse_aggregator_output};

        let json = r#"{"verdict":"approved","comment":"All looks good.","findings":[],"fix_instructions":null}"#;
        let output = parse_aggregator_output(json).unwrap();
        assert_eq!(output.verdict, Verdict::Approved);
        assert_eq!(output.comment, "All looks good.");
        assert!(output.findings.is_empty());
        assert!(output.fix_instructions.is_none());
    }

    #[test]
    fn test_parse_aggregator_needs_fix_json() {
        use crate::review_schema::{Verdict, parse_aggregator_output};

        let json = r#"{"verdict":"needs_fix","comment":"Issues found.","findings":[{"id":"bug-main","file":"src/main.rs","line":42,"severity":"critical","description":"bug"}],"fix_instructions":"Fix the bug."}"#;
        let output = parse_aggregator_output(json).unwrap();
        assert_eq!(output.verdict, Verdict::NeedsFix);
        assert_eq!(output.fix_instructions.as_deref(), Some("Fix the bug."));
    }

    #[test]
    fn test_parse_aggregator_invalid_json_errors() {
        use crate::review_schema::parse_aggregator_output;

        assert!(parse_aggregator_output("not json at all").is_err());
    }
}
