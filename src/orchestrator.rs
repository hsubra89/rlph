use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::sync::watch;
use tracing::{info, warn};

use crate::config::{Config, ReviewPhaseConfig, ReviewStepConfig};
use crate::deps::DependencyGraph;
use crate::error::{Error, Result};
use crate::prompts::PromptEngine;
use crate::runner::{AgentRunner, AnyRunner, Phase, build_runner};
use crate::sources::{Task, TaskSource};
use crate::state::StateManager;
use crate::submission::{REVIEW_MARKER, SubmissionBackend, format_pr_comments_for_prompt};
use crate::worktree::{WorktreeInfo, WorktreeManager, validate_branch_name};

#[derive(Debug)]
struct ReviewPhaseOutput {
    name: String,
    stdout: String,
    stderr: String,
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
    fn create_step_runner(&self, step: &ReviewStepConfig, timeout_retries: u32) -> AnyRunner;
}

/// Default factory that creates real runners from config.
pub struct DefaultReviewRunnerFactory;

impl ReviewRunnerFactory for DefaultReviewRunnerFactory {
    fn create_phase_runner(&self, phase: &ReviewPhaseConfig, timeout_retries: u32) -> AnyRunner {
        build_runner(
            &phase.runner,
            &phase.agent_binary,
            phase.agent_model.as_deref(),
            phase.agent_effort.as_deref(),
            phase.agent_timeout.map(Duration::from_secs),
            timeout_retries,
        )
    }

    fn create_step_runner(&self, step: &ReviewStepConfig, timeout_retries: u32) -> AnyRunner {
        build_runner(
            &step.runner,
            &step.agent_binary,
            step.agent_model.as_deref(),
            step.agent_effort.as_deref(),
            step.agent_timeout.map(Duration::from_secs),
            timeout_retries,
        )
    }
}

/// Observer for structured review-pipeline progress events.
pub trait ReviewReporter: Send + Sync {
    fn phases_started(&self, names: &[String]);
    fn phase_complete(&self, name: &str);
    fn review_summary(&self, body: &str);
    fn pr_url(&self, url: &str);
}

/// Default reporter that prints to stderr.
pub struct StderrReporter;

impl ReviewReporter for StderrReporter {
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

pub struct Orchestrator<S, R, B, F = DefaultReviewRunnerFactory, P = StderrReporter> {
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
            review_factory: DefaultReviewRunnerFactory,
            reporter: StderrReporter,
        }
    }
}

impl<S: TaskSource, R: AgentRunner, B: SubmissionBackend, F: ReviewRunnerFactory>
    Orchestrator<S, R, B, F>
{
    #[allow(clippy::too_many_arguments)]
    pub fn with_review_factory(
        source: S,
        runner: R,
        submission: B,
        worktree_mgr: WorktreeManager,
        state_mgr: StateManager,
        prompt_engine: PromptEngine,
        config: Config,
        repo_root: PathBuf,
        review_factory: F,
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
            review_factory,
            reporter: StderrReporter,
        }
    }
}

impl<
        S: TaskSource,
        R: AgentRunner,
        B: SubmissionBackend,
        F: ReviewRunnerFactory,
        P: ReviewReporter,
    > Orchestrator<S, R, B, F, P>
{
    #[allow(clippy::too_many_arguments)]
    pub fn with_reporter(
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
            review_factory,
            reporter,
        }
    }

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

        // 2. Choose phase — agent selects a task
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
        info!(elapsed_secs = choose_started.elapsed().as_secs(), "choose phase complete");

        // 3. Parse task selection from .ralph/task.toml
        let task_id = self.parse_task_selection()?;
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
        let vars = self.build_task_vars(task, worktree_info);

        // 7. Implement phase
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

        for round in 1..=max_reviews {
            info!(round, max_reviews, "review round");

            // Fetch current PR comments for this round
            let pr_comments_text = if let Some(pr_num) = pr_number {
                match self.submission.fetch_pr_comments(pr_num) {
                    Ok(comments) => format_pr_comments_for_prompt(&comments, pr_num),
                    Err(e) => {
                        warn!(error = %e, "failed to fetch PR comments");
                        "Failed to fetch PR comments.".to_string()
                    }
                }
            } else {
                "No PR associated with this review.".to_string()
            };

            let pr_number_str = pr_number.map(|n| n.to_string()).unwrap_or_default();

            // Run all configured review phases in parallel.
            let phase_names: Vec<String> = self
                .config
                .review_phases
                .iter()
                .map(|p| p.name.clone())
                .collect();
            self.reporter.phases_started(&phase_names);

            let mut join_set = tokio::task::JoinSet::new();
            for phase_config in &self.config.review_phases {
                let phase_runner = self
                    .review_factory
                    .create_phase_runner(phase_config, self.config.agent_timeout_retries);

                let mut phase_vars = vars.clone();
                phase_vars.insert("review_phase_name".to_string(), phase_config.name.clone());
                phase_vars.insert("pr_comments".to_string(), pr_comments_text.clone());
                phase_vars.insert("pr_number".to_string(), pr_number_str.clone());

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
                        stderr: result.stderr,
                    })
                });
            }

            let mut review_outputs = Vec::new();
            while let Some(result) = join_set.join_next().await {
                let output = result.map_err(|e| Error::AgentRunner(e.to_string()))??;
                self.reporter.phase_complete(&output.name);
                review_outputs.push(output);
            }

            let review_outputs_text = review_outputs
                .iter()
                .map(|o| {
                    let mut section =
                        format!("## Review Phase: {}\n\n### stdout\n{}", o.name, o.stdout);
                    if !o.stderr.trim().is_empty() {
                        section.push_str(&format!("\n\n### stderr\n{}", o.stderr));
                    }
                    section
                })
                .collect::<Vec<_>>()
                .join("\n\n---\n\n");

            let agg_config = &self.config.review_aggregate;
            let agg_runner = self
                .review_factory
                .create_step_runner(agg_config, self.config.agent_timeout_retries);

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

            let comment_body = extract_comment_body(&agg_result.stdout);
            let summary = comment_body
                .strip_prefix(REVIEW_MARKER)
                .unwrap_or(&comment_body)
                .trim();
            if !summary.is_empty() {
                self.reporter.review_summary(summary);
            }

            if let Some(pr_num) = pr_number
                && !self.config.dry_run
                && let Err(e) = self.submission.upsert_review_comment(pr_num, &comment_body)
            {
                warn!(error = %e, "failed to comment on PR");
            }

            if let Some(url) = vars.get("pr_url")
                && !url.is_empty()
            {
                self.reporter.pr_url(url);
            }

            if agg_result.stdout.contains("REVIEW_APPROVED") {
                info!(round, "review approved");
                review_passed = true;
                break;
            }

            if review_only {
                info!("review-only mode — skipping fix phase");
                break;
            }

            let fix_instructions = match extract_fix_instructions(&agg_result.stdout) {
                Some(instructions) => instructions,
                None => {
                    warn!("aggregator produced neither REVIEW_APPROVED nor REVIEW_NEEDS_FIX — retrying");
                    continue;
                }
            };

            info!(round, "review needs fix, running fix agent");

            let fix_config = &self.config.review_fix;
            let fix_runner = self
                .review_factory
                .create_step_runner(fix_config, self.config.agent_timeout_retries);

            let mut fix_vars = vars.clone();
            fix_vars.insert("fix_instructions".to_string(), fix_instructions);

            let fix_prompt = self
                .prompt_engine
                .render_phase(&fix_config.prompt, &fix_vars)?;
            fix_runner
                .run(Phase::ReviewFix, &fix_prompt, &worktree_info.path)
                .await?;

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

        if !review_passed {
            return Err(Error::Orchestrator(format!(
                "review did not complete after {max_reviews} round(s)"
            )));
        }

        Ok(())
    }

    /// Parse the task selection from `.ralph/task.toml` written by the choose agent.
    fn parse_task_selection(&self) -> Result<String> {
        let path = self.repo_root.join(".ralph").join("task.toml");
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

    fn build_task_vars(&self, task: &Task, worktree: &WorktreeInfo) -> HashMap<String, String> {
        let mut vars = HashMap::new();
        vars.insert("issue_title".to_string(), task.title.clone());
        vars.insert("issue_body".to_string(), task.body.clone());
        vars.insert("issue_number".to_string(), task.id.clone());
        vars.insert("issue_url".to_string(), task.url.clone());
        vars.insert(
            "repo_path".to_string(),
            self.repo_root.display().to_string(),
        );
        vars.insert("branch_name".to_string(), worktree.branch.clone());
        vars.insert(
            "worktree_path".to_string(),
            worktree.path.display().to_string(),
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

/// Extract the comment body from aggregation output (everything before the REVIEW_ signal),
/// prepended with the rlph review marker for upsert identification.
fn extract_comment_body(stdout: &str) -> String {
    let body = if let Some(pos) = stdout.find("REVIEW_APPROVED") {
        stdout[..pos].trim()
    } else if let Some(pos) = stdout.find("REVIEW_NEEDS_FIX:") {
        stdout[..pos].trim()
    } else {
        stdout.trim()
    };
    format!("{REVIEW_MARKER}\n{body}")
}

/// Extract fix instructions from `REVIEW_NEEDS_FIX: <instructions>`.
/// Captures everything from the marker to end of output (multi-line).
fn extract_fix_instructions(stdout: &str) -> Option<String> {
    if let Some(pos) = stdout.find("REVIEW_NEEDS_FIX:") {
        let after = &stdout[pos + "REVIEW_NEEDS_FIX:".len()..];
        let trimmed = after.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    } else {
        None
    }
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
    fn test_extract_comment_body_approved() {
        let stdout = "Some findings here\n\nREVIEW_APPROVED";
        assert_eq!(
            extract_comment_body(stdout),
            "<!-- rlph-review -->\nSome findings here"
        );
    }

    #[test]
    fn test_extract_comment_body_needs_fix() {
        let stdout = "Findings\nREVIEW_NEEDS_FIX: fix stuff";
        assert_eq!(
            extract_comment_body(stdout),
            "<!-- rlph-review -->\nFindings"
        );
    }

    #[test]
    fn test_extract_comment_body_no_signal() {
        let stdout = "Just some output";
        assert_eq!(
            extract_comment_body(stdout),
            "<!-- rlph-review -->\nJust some output"
        );
    }

    #[test]
    fn test_extract_fix_instructions() {
        assert_eq!(
            extract_fix_instructions("REVIEW_NEEDS_FIX: fix the bug"),
            Some("fix the bug".to_string())
        );
        assert_eq!(
            extract_fix_instructions("Some output\nREVIEW_NEEDS_FIX: do stuff\nmore lines\nhere"),
            Some("do stuff\nmore lines\nhere".to_string())
        );
        assert_eq!(extract_fix_instructions("REVIEW_APPROVED"), None);
        assert_eq!(extract_fix_instructions("REVIEW_NEEDS_FIX:   "), None);
    }
}
