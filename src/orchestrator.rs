use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use serde::Deserialize;
use tracing::{info, warn};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::prompts::PromptEngine;
use crate::runner::{AgentRunner, Phase};
use crate::sources::{Task, TaskSource};
use crate::state::StateManager;
use crate::submission::SubmissionBackend;
use crate::worktree::{WorktreeInfo, WorktreeManager};

#[derive(Deserialize)]
struct TaskSelection {
    id: String,
}

pub struct Orchestrator<S, R, B> {
    source: S,
    runner: R,
    submission: B,
    worktree_mgr: WorktreeManager,
    state_mgr: StateManager,
    prompt_engine: PromptEngine,
    config: Config,
    repo_root: PathBuf,
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
        }
    }

    /// Run a single iteration of the orchestrator loop.
    pub async fn run_once(&self) -> Result<()> {
        // 1. Fetch eligible tasks
        info!("[rlph:orchestrator] Fetching eligible tasks...");
        let tasks = self.source.fetch_eligible_tasks()?;
        if tasks.is_empty() {
            info!("[rlph:orchestrator] No eligible tasks found");
            return Ok(());
        }
        info!("[rlph:orchestrator] Found {} eligible task(s)", tasks.len());

        // 2. Choose phase — agent selects a task
        info!("[rlph:orchestrator] Running choose phase...");
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
            "[rlph:orchestrator] Choose phase complete in {}s",
            choose_started.elapsed().as_secs()
        );

        // 3. Parse task selection from .ralph/task.toml
        let task_id = self.parse_task_selection()?;
        let issue_number = parse_issue_number(&task_id)?;
        info!("[rlph:orchestrator] Selected task: {task_id} (issue #{issue_number})");
        let existing_pr_number = if self.config.dry_run {
            info!("[rlph:orchestrator] Dry run — skipping existing PR lookup");
            None
        } else {
            let pr_number = self.submission.find_existing_pr_for_issue(issue_number)?;
            if let Some(pr) = pr_number {
                info!("[rlph:orchestrator] Existing PR #{pr} found for issue #{issue_number}");
            } else {
                info!("[rlph:orchestrator] No existing PR found for issue #{issue_number}");
            }
            pr_number
        };

        // 4. Get task details
        let task = self.source.get_task_details(&issue_number.to_string())?;
        info!("[rlph:orchestrator] Task: {} — {}", task.id, task.title);

        // 5. Mark in-progress
        if !self.config.dry_run {
            info!("[rlph:orchestrator] Marking task in-progress...");
            self.source.mark_in_progress(&task.id)?;
        }

        // 6. Create worktree
        info!("[rlph:orchestrator] Creating worktree...");
        let slug = WorktreeManager::slugify(&task.title);
        let worktree_info = self.worktree_mgr.create(issue_number, &slug)?;
        info!(
            "[rlph:orchestrator] Worktree at {} (branch: {})",
            worktree_info.path.display(),
            worktree_info.branch
        );

        // Update state
        self.state_mgr.set_current_task(
            &task_id,
            "implement",
            &worktree_info.path.display().to_string(),
        )?;

        // Run the implement → submit → review pipeline, cleaning up on success
        let result = self
            .run_implement_review(
                &task,
                &task_id,
                issue_number,
                &worktree_info,
                existing_pr_number,
            )
            .await;

        match result {
            Ok(()) => {
                // 11. Mark done — skipped; GitHub auto-closes the issue when the PR merges
                self.state_mgr.complete_current_task()?;

                // 12. Clean up worktree
                info!("[rlph:orchestrator] Cleaning up worktree...");
                if let Err(e) = self.worktree_mgr.remove(&worktree_info.path) {
                    warn!("[rlph:orchestrator] Failed to clean up worktree: {e}");
                }
                let _ = self.state_mgr.remove_worktree_mapping(&task_id);

                info!("[rlph:orchestrator] Iteration complete");
                Ok(())
            }
            Err(e) => {
                warn!("[rlph:orchestrator] Iteration failed: {e}");
                Err(e)
            }
        }
    }

    /// Implement, submit PR, and review — the inner pipeline after worktree creation.
    async fn run_implement_review(
        &self,
        task: &Task,
        _task_id: &str,
        issue_number: u64,
        worktree_info: &WorktreeInfo,
        existing_pr_number: Option<u64>,
    ) -> Result<()> {
        let vars = self.build_task_vars(task, worktree_info);

        // 7. Implement phase
        info!("[rlph:orchestrator] Running implement phase...");
        let impl_prompt = self.prompt_engine.render_phase("implement", &vars)?;
        self.runner
            .run(Phase::Implement, &impl_prompt, &worktree_info.path)
            .await?;

        // 8. Push branch
        if !self.config.dry_run {
            info!("[rlph:orchestrator] Pushing branch...");
            self.push_branch(worktree_info)?;
        }

        // 9. Submit PR (skip if choose agent reported an existing PR)
        if let Some(pr) = existing_pr_number {
            info!("[rlph:orchestrator] Skipping PR submission — existing PR #{pr}");
        } else if !self.config.dry_run {
            info!("[rlph:orchestrator] Submitting PR...");
            let pr_body = format!("Resolves #{issue_number}\n\nAutomated implementation by rlph.");
            let result =
                self.submission
                    .submit(&worktree_info.branch, "main", &task.title, &pr_body)?;
            info!("[rlph:orchestrator] PR: {}", result.url);
        } else {
            info!("[rlph:orchestrator] Dry run — skipping PR submission");
        }

        // 10. Review loop
        self.state_mgr.update_phase("review")?;
        let max_reviews = self.config.max_review_rounds;
        let mut review_passed = false;
        for round in 1..=max_reviews {
            info!("[rlph:orchestrator] Review round {round}/{max_reviews}...");
            let review_prompt = self.prompt_engine.render_phase("review", &vars)?;
            let review_result = self
                .runner
                .run(Phase::Review, &review_prompt, &worktree_info.path)
                .await?;

            // Push any review fixes
            if !self.config.dry_run
                && let Err(e) = self.push_branch(worktree_info)
            {
                warn!("[rlph:orchestrator] Failed to push review fixes: {e}");
            }

            if review_result.stdout.contains("REVIEW_COMPLETE:") {
                info!("[rlph:orchestrator] Review complete at round {round}");
                review_passed = true;
                break;
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

        info!("[rlph:orchestrator] Pushed branch {}", worktree.branch);
        Ok(())
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
}
