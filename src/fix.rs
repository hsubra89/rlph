use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Semaphore, watch};
use tracing::{info, warn};

/// Serializes comment re-fetch + update to prevent concurrent fix agents from racing.
static COMMENT_UPDATE_LOCK: Semaphore = Semaphore::const_new(1);

/// Maximum number of push attempts before giving up (rebase+retry on conflict).
const MAX_PUSH_ATTEMPTS: u32 = 3;

/// Maximum number of fetch retry attempts (git lock contention under concurrency).
const MAX_FETCH_ATTEMPTS: u32 = 3;

/// Maximum number of fix agents running concurrently.
const MAX_CONCURRENT_FIXES: usize = 2;

use crate::config::{Config, ReviewStepConfig};
use crate::error::{Error, Result};
use crate::fix_comment::{CheckboxState, FixItem, FixResultKind, parse_fix_items, update_comment};
use crate::orchestrator::{CorrectionRunner, retry_with_correction};
use crate::prompts::PromptEngine;
use crate::review_schema::{SchemaName, StandaloneFixOutput, parse_standalone_fix_output};
use crate::runner::{AgentRunner, Phase, RunResult, build_runner};
use crate::submission::{REVIEW_MARKER, SubmissionBackend};
use crate::worktree::{WorktreeManager, git_in_dir, validate_branch_name};

/// Run the standalone fix flow for ALL checked findings on a PR concurrently.
///
/// Steps:
/// 1. Fetch review comment, parse checked items
/// 2. Collect all eligible checked items
/// 3. Spawn a fix agent for each item in parallel (JoinSet)
///    - Each gets its own worktree off `origin/<pr-branch>`
///    - Parse StandaloneFixOutput JSON (with retry)
///    - If fixed: rebase onto `origin/<pr-branch>`, push with retry
///    - Update review comment checkbox with result
///    - Clean up worktree
/// 4. Collect results, log any errors
pub async fn run_fix<C: CorrectionRunner + 'static>(
    pr_number: u64,
    pr_branch: &str,
    config: &Config,
    submission: Arc<impl SubmissionBackend + 'static>,
    prompt_engine: &PromptEngine,
    repo_root: &Path,
    correction_runner: Arc<C>,
) -> Result<()> {
    // Validate pr_branch from GitHub API at the trust boundary
    validate_branch_name(pr_branch)?;

    // 1. Fetch review comment and parse checked items
    info!(pr_number, "polling GitHub for PR comments");
    let (items, _comment_id) = fetch_and_parse_items(pr_number, &*submission, None)?;
    info!(total = items.len(), "parsed fix items from review comment");

    // 2. Collect ALL eligible checked items
    let eligible: Vec<&FixItem> = items
        .iter()
        .filter(|item| item.state == CheckboxState::Checked)
        .collect();

    if eligible.is_empty() {
        info!("no checked items found — nothing to fix");
        return Ok(());
    }

    info!(
        count = eligible.len(),
        "found checked items for parallel fix"
    );

    // 3. Pre-compute per-item data and spawn into JoinSet
    let shared = SharedFixState {
        fix_config: Arc::new(config.fix.clone()),
        worktree_dir: Arc::from(config.worktree_dir.as_str()),
        repo_root: Arc::from(repo_root),
        pr_branch: Arc::from(pr_branch),
        submission: Arc::clone(&submission),
        correction_runner: Arc::clone(&correction_runner),
        concurrency: Arc::new(Semaphore::new(MAX_CONCURRENT_FIXES)),
        agent_timeout_retries: config.agent_timeout_retries,
    };

    let mut join_set = tokio::task::JoinSet::new();

    let mut skipped: usize = 0;
    for item in &eligible {
        let item = (*item).clone();

        let Some(prepared) = prepare_fix_item(item, pr_number, &shared.fix_config, prompt_engine) else {
            skipped += 1;
            continue;
        };

        let shared = shared.clone();

        join_set.spawn(async move {
            let _permit = shared.concurrency
                .acquire()
                .await
                .expect("concurrency semaphore closed unexpectedly");
            let ctx = FixContext {
                item: prepared.item,
                pr_number,
                pr_branch: &shared.pr_branch,
                fix_branch: &prepared.fix_branch,
                fix_config: &shared.fix_config,
                agent_timeout_retries: shared.agent_timeout_retries,
                prompt: &prepared.prompt,
            };
            run_single_fix(
                ctx,
                &shared.worktree_dir,
                &shared.repo_root,
                &*shared.submission,
                &*shared.correction_runner,
            )
            .await
        });
    }

    if skipped == eligible.len() {
        return Err(Error::Orchestrator(format!(
            "all {skipped} eligible fix item(s) were skipped due to validation errors"
        )));
    } else if skipped > 0 {
        warn!(
            skipped,
            total = eligible.len(),
            "some fix items were skipped due to validation errors"
        );
    }

    // 4. Collect results as each fix completes
    let mut errors = Vec::new();
    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                warn!(error = %e, "fix agent failed");
                errors.push(e);
            }
            Err(e) => {
                warn!(error = %e, "fix task panicked");
                errors.push(Error::Orchestrator(format!("fix task panicked: {e}")));
            }
        }
    }

    if errors.is_empty() {
        info!(pr_number, "all fixes completed successfully");
        Ok(())
    } else {
        let count = errors.len();
        // Return the first error but log all
        Err(Error::Orchestrator(format!(
            "{count} fix(es) failed; first: {}",
            errors[0]
        )))
    }
}

/// Run the fix command as a continuous polling loop.
///
/// Polls for newly-checked checkboxes every `poll_seconds`, spawns fix agents
/// for new items, and tracks in-flight/completed items to avoid re-processing.
///
/// On shutdown signal: stops accepting new tasks, waits for in-flight agents
/// to complete, then exits cleanly.
#[allow(clippy::too_many_arguments)]
pub async fn run_fix_loop<C: CorrectionRunner + 'static>(
    pr_number: u64,
    pr_branch: &str,
    config: &Config,
    submission: Arc<impl SubmissionBackend + 'static>,
    prompt_engine: &PromptEngine,
    repo_root: &Path,
    correction_runner: Arc<C>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    validate_branch_name(pr_branch)?;

    let shared = SharedFixState {
        fix_config: Arc::new(config.fix.clone()),
        worktree_dir: Arc::from(config.worktree_dir.as_str()),
        repo_root: Arc::from(repo_root),
        pr_branch: Arc::from(pr_branch),
        submission: Arc::clone(&submission),
        correction_runner: Arc::clone(&correction_runner),
        concurrency: Arc::new(Semaphore::new(MAX_CONCURRENT_FIXES)),
        agent_timeout_retries: config.agent_timeout_retries,
    };
    let poll_duration = Duration::from_secs(config.poll_seconds);

    let mut join_set: tokio::task::JoinSet<(String, Result<()>)> = tokio::task::JoinSet::new();
    let mut task_finding_ids: HashMap<tokio::task::Id, String> = HashMap::new();
    let mut in_flight: HashSet<String> = HashSet::new();
    let mut completed: HashSet<String> = HashSet::new();
    let mut failed: HashSet<String> = HashSet::new();
    let mut cycle: u64 = 0;
    let mut cached_comment_id: Option<u64> = None;

    loop {
        cycle += 1;

        if *shutdown.borrow() {
            info!("shutdown requested, stopping poll loop");
            break;
        }

        // Drain completed tasks (non-blocking)
        drain_completed(&mut join_set, &mut task_finding_ids, &mut in_flight, &mut completed, &mut failed);

        // Fetch and parse
        info!(pr_number, cycle, in_flight = in_flight.len(), completed = completed.len(), "polling for newly-checked items");
        let (items, comment_id) = match fetch_and_parse_items(pr_number, &*shared.submission, cached_comment_id) {
            Ok(result) => result,
            Err(e) => {
                warn!(error = %e, cycle, "failed to fetch review comment, retrying next cycle");
                if wait_or_shutdown(poll_duration, &mut shutdown).await {
                    break;
                }
                continue;
            }
        };
        cached_comment_id = Some(comment_id);

        if *shutdown.borrow() {
            info!("shutdown requested after fetch, stopping poll loop");
            break;
        }

        // Filter: checked AND not already tracked
        let newly_checked: Vec<FixItem> = items
            .into_iter()
            .filter(|item| {
                item.state == CheckboxState::Checked
                    && !in_flight.contains(&item.finding.id)
                    && !completed.contains(&item.finding.id)
                    && !failed.contains(&item.finding.id)
            })
            .collect();

        info!(
            cycle,
            newly_checked = newly_checked.len(),
            in_flight = in_flight.len(),
            completed = completed.len(),
            failed = failed.len(),
            "poll cycle summary"
        );

        // Spawn fix agents for newly checked items
        let mut skipped: usize = 0;
        for item in newly_checked {
            let finding_id = item.finding.id.clone();

            let Some(prepared) = prepare_fix_item(item, pr_number, &shared.fix_config, prompt_engine) else {
                failed.insert(finding_id);
                skipped += 1;
                continue;
            };

            in_flight.insert(finding_id.clone());
            let finding_id_for_map = finding_id.clone();

            let shared = shared.clone();

            let abort_handle = join_set.spawn(async move {
                let _permit = shared.concurrency
                    .acquire()
                    .await
                    .expect("concurrency semaphore closed unexpectedly");
                let ctx = FixContext {
                    item: prepared.item,
                    pr_number,
                    pr_branch: &shared.pr_branch,
                    fix_branch: &prepared.fix_branch,
                    fix_config: &shared.fix_config,
                    agent_timeout_retries: shared.agent_timeout_retries,
                    prompt: &prepared.prompt,
                };
                let result = run_single_fix(
                    ctx,
                    &shared.worktree_dir,
                    &shared.repo_root,
                    &*shared.submission,
                    &*shared.correction_runner,
                )
                .await;
                (finding_id, result)
            });
            task_finding_ids.insert(abort_handle.id(), finding_id_for_map);
        }

        if skipped > 0 {
            warn!(skipped, "some fix items skipped due to validation errors");
        }

        // Wait for poll interval or shutdown
        if wait_or_shutdown(poll_duration, &mut shutdown).await {
            info!("shutdown requested during poll wait");
            break;
        }
    }

    // Graceful shutdown: wait for all in-flight tasks to complete
    if !join_set.is_empty() {
        info!(count = in_flight.len(), "graceful shutdown: waiting for in-flight fix agents");
        while let Some(result) = join_set.join_next().await {
            handle_join_result(result, &mut task_finding_ids, &mut in_flight, &mut completed, &mut failed);
        }
    }

    info!(
        completed = completed.len(),
        failed = failed.len(),
        "fix loop finished"
    );

    Ok(())
}

/// Fetch the review comment and parse fix items from it.
///
/// If `cached_comment_id` is `Some`, fetches only that single comment instead
/// of all PR comments — avoiding redundant API calls on subsequent poll cycles.
/// Returns the parsed items and the comment ID for caching.
fn fetch_and_parse_items(
    pr_number: u64,
    submission: &(impl SubmissionBackend + ?Sized),
    cached_comment_id: Option<u64>,
) -> Result<(Vec<FixItem>, u64)> {
    if let Some(comment_id) = cached_comment_id {
        let comment = submission.fetch_comment_by_id(comment_id)?;
        return Ok((parse_fix_items(&comment.body), comment_id));
    }

    let comments = submission.fetch_pr_comments(pr_number)?;
    let review_comment = comments
        .iter()
        .find(|c| c.body.contains(REVIEW_MARKER))
        .ok_or_else(|| {
            Error::Orchestrator(format!("no rlph review comment found on PR #{pr_number}"))
        })?;
    let comment_id = review_comment.id;
    Ok((parse_fix_items(&review_comment.body), comment_id))
}

/// Drain completed tasks from the JoinSet without blocking.
fn drain_completed(
    join_set: &mut tokio::task::JoinSet<(String, Result<()>)>,
    task_finding_ids: &mut HashMap<tokio::task::Id, String>,
    in_flight: &mut HashSet<String>,
    completed: &mut HashSet<String>,
    failed: &mut HashSet<String>,
) {
    while let Some(result) = join_set.try_join_next() {
        handle_join_result(result, task_finding_ids, in_flight, completed, failed);
    }
}

/// Process one completed task result, updating tracking sets.
fn handle_join_result(
    result: std::result::Result<(String, Result<()>), tokio::task::JoinError>,
    task_finding_ids: &mut HashMap<tokio::task::Id, String>,
    in_flight: &mut HashSet<String>,
    completed: &mut HashSet<String>,
    failed: &mut HashSet<String>,
) {
    match result {
        Ok((finding_id, Ok(()))) => {
            info!(%finding_id, "fix completed");
            in_flight.remove(&finding_id);
            completed.insert(finding_id);
        }
        Ok((finding_id, Err(e))) => {
            warn!(%finding_id, error = %e, "fix failed");
            in_flight.remove(&finding_id);
            failed.insert(finding_id);
        }
        Err(e) => {
            let finding_id = task_finding_ids.remove(&e.id());
            if let Some(ref id) = finding_id {
                warn!(finding_id = id, error = %e, "fix task panicked");
                in_flight.remove(id);
                failed.insert(id.clone());
            } else {
                warn!(error = %e, "fix task panicked (unknown finding_id)");
            }
        }
    }
}

/// Sleep for the poll duration, but return early if shutdown is requested.
/// Returns `true` if shutdown was requested.
async fn wait_or_shutdown(
    duration: Duration,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(duration) => false,
        changed = shutdown.changed() => {
            if changed.is_ok() {
                *shutdown.borrow()
            } else {
                // Sender dropped — no one can signal shutdown anymore, so exit gracefully.
                true
            }
        }
    }
}

/// Run a single fix: create worktree, run agent, push, update comment, cleanup.
async fn run_single_fix(
    ctx: FixContext<'_>,
    worktree_dir: &str,
    repo_root: &Path,
    submission: &(impl SubmissionBackend + ?Sized),
    correction_runner: &(impl CorrectionRunner + ?Sized),
) -> Result<()> {
    let wm = WorktreeManager::new(
        repo_root.to_path_buf(),
        repo_root.join(worktree_dir),
        ctx.pr_branch.to_string(),
    );
    let worktree_path = wm.create_fresh(ctx.fix_branch, ctx.pr_branch)?.path;
    info!(
        finding_id = %ctx.item.finding.id,
        path = %worktree_path.display(),
        branch = %ctx.fix_branch,
        "created fix worktree"
    );

    // Run the fix agent and handle results, ensuring worktree cleanup
    let result = run_fix_agent_and_apply(&ctx, &worktree_path, submission, correction_runner).await;

    // Clean up worktree (always, even on error)
    info!(
        finding_id = %ctx.item.finding.id,
        path = %worktree_path.display(),
        "cleaning up fix worktree"
    );
    if let Err(e) = wm.remove(&worktree_path) {
        warn!(error = %e, "failed to clean up fix worktree");
    }

    result
}

/// Bundled context for a single fix operation, replacing long parameter lists.
struct FixContext<'a> {
    item: FixItem,
    pr_number: u64,
    pr_branch: &'a str,
    fix_branch: &'a str,
    fix_config: &'a ReviewStepConfig,
    agent_timeout_retries: u32,
    prompt: &'a str,
}

/// Shared state cloned into each spawned fix task.
///
/// Groups the Arc-wrapped values that both `run_fix` and `run_fix_loop` need
/// to clone per spawned task, replacing individual `Arc::clone` lines with
/// a single `shared.clone()`.
struct SharedFixState<S, C> {
    fix_config: Arc<ReviewStepConfig>,
    worktree_dir: Arc<str>,
    repo_root: Arc<Path>,
    pr_branch: Arc<str>,
    submission: Arc<S>,
    correction_runner: Arc<C>,
    concurrency: Arc<Semaphore>,
    agent_timeout_retries: u32,
}

impl<S, C> Clone for SharedFixState<S, C> {
    fn clone(&self) -> Self {
        Self {
            fix_config: Arc::clone(&self.fix_config),
            worktree_dir: Arc::clone(&self.worktree_dir),
            repo_root: Arc::clone(&self.repo_root),
            pr_branch: Arc::clone(&self.pr_branch),
            submission: Arc::clone(&self.submission),
            correction_runner: Arc::clone(&self.correction_runner),
            concurrency: Arc::clone(&self.concurrency),
            agent_timeout_retries: self.agent_timeout_retries,
        }
    }
}

/// Build template variables from a fix item's finding.
fn build_finding_vars(item: &FixItem) -> HashMap<String, String> {
    let mut vars = HashMap::with_capacity(6);
    vars.insert("finding_id".to_string(), item.finding.id.clone());
    vars.insert("finding_file".to_string(), item.finding.file.clone());
    vars.insert("finding_line".to_string(), item.finding.line.to_string());
    vars.insert(
        "finding_severity".to_string(),
        item.finding.severity.label().to_string(),
    );
    vars.insert(
        "finding_description".to_string(),
        item.finding.description.clone(),
    );
    vars.insert(
        "finding_depends_on".to_string(),
        item.finding.depends_on.join(", "),
    );
    vars
}

/// Validated and pre-rendered data for spawning a fix agent.
struct PreparedFixItem {
    item: FixItem,
    fix_branch: String,
    prompt: String,
}

/// Validate branch name, render the prompt, and log the spawn.
///
/// Returns `None` if the item should be skipped (invalid branch name or prompt
/// rendering failure), with a warning already logged.
fn prepare_fix_item(
    item: FixItem,
    pr_number: u64,
    fix_config: &ReviewStepConfig,
    prompt_engine: &PromptEngine,
) -> Option<PreparedFixItem> {
    let fix_branch = format!("rlph-fix-{pr_number}-{}", item.finding.id);
    if let Err(e) = validate_branch_name(&fix_branch) {
        warn!(finding_id = %item.finding.id, error = %e, "invalid fix branch name, skipping");
        return None;
    }

    let vars = build_finding_vars(&item);
    let prompt = match prompt_engine.render_phase(&fix_config.prompt, &vars) {
        Ok(p) => p,
        Err(e) => {
            warn!(finding_id = %item.finding.id, error = %e, "failed to render prompt, skipping");
            return None;
        }
    };

    info!(
        finding_id = %item.finding.id,
        file = %item.finding.file,
        line = item.finding.line,
        severity = %item.finding.severity.label(),
        "spawning fix agent"
    );

    Some(PreparedFixItem {
        item,
        fix_branch,
        prompt,
    })
}

/// Inner function: spawn agent, parse output, rebase/push with retry, update comment.
async fn run_fix_agent_and_apply(
    ctx: &FixContext<'_>,
    worktree_path: &Path,
    submission: &(impl SubmissionBackend + ?Sized),
    correction_runner: &(impl CorrectionRunner + ?Sized),
) -> Result<()> {
    // Spawn fix agent
    info!(finding_id = %ctx.item.finding.id, "spawning fix agent");
    let runner = build_runner(
        ctx.fix_config.runner,
        &ctx.fix_config.agent_binary,
        ctx.fix_config.agent_model.as_deref(),
        ctx.fix_config.agent_effort.as_deref(),
        ctx.fix_config.agent_variant.as_deref(),
        ctx.fix_config.agent_timeout.map(Duration::from_secs),
        ctx.agent_timeout_retries,
    )
    .with_stream_prefix("fix".to_string());

    let run_result = runner.run(Phase::Fix, ctx.prompt, worktree_path).await?;

    // Parse StandaloneFixOutput JSON (with retry on failure)
    let fix_output = parse_fix_with_retry(
        &run_result,
        ctx.fix_config,
        worktree_path,
        correction_runner,
    )
    .await?;

    info!(finding_id = %ctx.item.finding.id, ?fix_output, "fix agent completed");

    // Apply result and update comment
    let fix_result = match fix_output {
        StandaloneFixOutput::Fixed { commit_message } => {
            info!(finding_id = %ctx.item.finding.id, commit_message, "fix applied — rebasing and pushing");
            push_to_pr_branch_with_retry(worktree_path, ctx.fix_branch, ctx.pr_branch).await?;
            FixResultKind::Fixed {
                commit_message: commit_message.clone(),
            }
        }
        StandaloneFixOutput::WontFix { reason } => {
            info!(finding_id = %ctx.item.finding.id, reason, "finding marked as won't fix");
            FixResultKind::WontFix {
                reason: reason.clone(),
            }
        }
    };

    // Re-fetch and update review comment under lock to avoid racing with concurrent fix agents
    let _permit = COMMENT_UPDATE_LOCK
        .acquire()
        .await
        .expect("comment update semaphore closed unexpectedly");

    info!(pr_number = ctx.pr_number, finding_id = %ctx.item.finding.id, "polling GitHub to re-fetch review comment");
    let comments = submission.fetch_pr_comments(ctx.pr_number)?;
    let fresh_body = comments
        .iter()
        .find(|c| c.body.contains(REVIEW_MARKER))
        .map(|c| c.body.as_str())
        .ok_or_else(|| {
            Error::Orchestrator(format!(
                "review comment disappeared from PR #{}",
                ctx.pr_number
            ))
        })?;

    let updated_body = update_comment(fresh_body, &ctx.item.finding.id, &fix_result);
    info!(pr_number = ctx.pr_number, finding_id = %ctx.item.finding.id, "updating review comment");
    submission.upsert_review_comment(ctx.pr_number, &updated_body)?;

    Ok(())
}

/// Parse fix output with up to 2 retries via session resume.
async fn parse_fix_with_retry(
    run_result: &RunResult,
    fix_config: &ReviewStepConfig,
    working_dir: &Path,
    correction_runner: &(impl CorrectionRunner + ?Sized),
) -> Result<StandaloneFixOutput> {
    match parse_standalone_fix_output(&run_result.stdout) {
        Ok(output) => Ok(output),
        Err(initial_err) => {
            let err_str = initial_err.to_string();
            retry_with_correction(
                correction_runner,
                run_result.session_id.as_deref(),
                fix_config.runner,
                &fix_config.agent_binary,
                fix_config.agent_model.as_deref(),
                fix_config.agent_effort.as_deref(),
                fix_config.agent_variant.as_deref(),
                fix_config.agent_timeout,
                SchemaName::StandaloneFix,
                &err_str,
                working_dir,
                parse_standalone_fix_output,
            )
            .await
            .ok_or_else(|| {
                Error::Orchestrator(format!(
                    "fix agent JSON parse failed and correction unsuccessful: {initial_err}"
                ))
            })
        }
    }
}

/// Fetch a ref from origin with retries to handle git lock contention under concurrency.
async fn fetch_with_retry(cwd: &Path, refspec: &str) -> Result<()> {
    let mut last_err = String::new();
    for attempt in 1..=MAX_FETCH_ATTEMPTS {
        match git_in_dir(cwd, &["fetch", "origin", refspec]) {
            Ok(_) => return Ok(()),
            Err(e) => {
                warn!(
                    attempt,
                    max_attempts = MAX_FETCH_ATTEMPTS,
                    error = %e.trim(),
                    "git fetch origin {} failed",
                    refspec
                );
                last_err = e;
                if attempt < MAX_FETCH_ATTEMPTS {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    }
    Err(Error::Orchestrator(format!(
        "git fetch origin {refspec} failed after {MAX_FETCH_ATTEMPTS} attempts: {}",
        last_err.trim()
    )))
}

/// Rebase current branch onto origin/<pr-branch>.
async fn rebase_onto(worktree_path: &Path, pr_branch: &str) -> Result<()> {
    fetch_with_retry(worktree_path, pr_branch).await?;

    let remote_ref = format!("origin/{pr_branch}");

    if let Err(stderr) = git_in_dir(worktree_path, &["rebase", &remote_ref]) {
        let _ = git_in_dir(worktree_path, &["rebase", "--abort"]);
        return Err(Error::Orchestrator(format!(
            "git rebase onto {remote_ref} failed: {stderr}"
        )));
    }

    info!(remote_ref, "rebased onto latest PR branch");
    Ok(())
}

/// Push fix branch to PR branch with rebase+retry on conflict.
///
/// On push failure (likely because another fix pushed first), fetches latest,
/// rebases, and retries up to [`MAX_PUSH_ATTEMPTS`] times.
async fn push_to_pr_branch_with_retry(
    worktree_path: &Path,
    fix_branch: &str,
    pr_branch: &str,
) -> Result<()> {
    let refspec = format!("{fix_branch}:{pr_branch}");
    let mut last_err = String::new();
    for attempt in 1..=MAX_PUSH_ATTEMPTS {
        // Skip rebase on first attempt: worktree was just created from origin/<pr-branch>
        if attempt > 1 {
            rebase_onto(worktree_path, pr_branch).await?;
        }

        match git_in_dir(worktree_path, &["push", "origin", &refspec]) {
            Ok(_) => {
                info!(refspec, attempt, "pushed fix to PR branch");
                return Ok(());
            }
            Err(stderr) => {
                let is_conflict = stderr.contains("non-fast-forward")
                    || stderr.contains("fetch first")
                    || stderr.contains("[rejected]");
                if is_conflict && attempt < MAX_PUSH_ATTEMPTS {
                    warn!(
                        attempt,
                        max = MAX_PUSH_ATTEMPTS,
                        error = %stderr.trim(),
                        "push conflict — retrying with fetch+rebase"
                    );
                }
                last_err = stderr;
            }
        }
    }
    Err(Error::Orchestrator(format!(
        "git push origin {refspec} failed after {MAX_PUSH_ATTEMPTS} attempts: {last_err}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_schema::render_findings_for_github;
    use crate::test_helpers::make_finding;

    #[test]
    fn test_fix_branch_name_is_valid() {
        let branch = "rlph-fix-42-sql-injection";
        assert!(validate_branch_name(branch).is_ok());
    }

    #[test]
    fn test_prompt_renders_with_finding_vars() {
        let engine = PromptEngine::new(None);
        let mut vars = HashMap::new();
        vars.insert("finding_id".to_string(), "sql-injection".to_string());
        vars.insert("finding_file".to_string(), "src/db.rs".to_string());
        vars.insert("finding_line".to_string(), "42".to_string());
        vars.insert("finding_severity".to_string(), "CRITICAL".to_string());
        vars.insert(
            "finding_description".to_string(),
            "SQL injection vulnerability".to_string(),
        );
        vars.insert("finding_depends_on".to_string(), String::new());

        let result = engine.render_phase("fix", &vars).unwrap();
        assert!(result.contains("sql-injection"));
        assert!(result.contains("src/db.rs"));
        assert!(result.contains("42"));
        assert!(result.contains("CRITICAL"));
        assert!(result.contains("SQL injection vulnerability"));
        assert!(result.contains("commit_message"));
        assert!(result.contains("wont_fix"));
    }

    #[test]
    fn test_prompt_renders_with_depends_on() {
        let engine = PromptEngine::new(None);
        let mut vars = HashMap::new();
        vars.insert("finding_id".to_string(), "null-deref".to_string());
        vars.insert("finding_file".to_string(), "src/lib.rs".to_string());
        vars.insert("finding_line".to_string(), "10".to_string());
        vars.insert("finding_severity".to_string(), "WARNING".to_string());
        vars.insert(
            "finding_description".to_string(),
            "Null dereference".to_string(),
        );
        vars.insert("finding_depends_on".to_string(), "null-check".to_string());

        let result = engine.render_phase("fix", &vars).unwrap();
        assert!(result.contains("null-check"));
    }

    #[test]
    fn test_update_comment_after_fixed() {
        let finding = make_finding("bug-1");
        let comment = render_findings_for_github(&[finding], "Summary.");
        let comment = comment.replace("- [ ] ", "- [x] ");

        let updated = update_comment(
            &comment,
            "bug-1",
            &FixResultKind::Fixed {
                commit_message: "bug-1: fixed the bug".to_string(),
            },
        );

        assert!(updated.contains("✅"));
        assert!(updated.contains("> Fixed: bug-1: fixed the bug"));
        assert!(!updated.contains("- [x]"));
    }

    #[test]
    fn test_update_comment_after_wont_fix() {
        let finding = make_finding("nit-1");
        let comment = render_findings_for_github(&[finding], "Summary.");
        let comment = comment.replace("- [ ] ", "- [x] ");

        let updated = update_comment(
            &comment,
            "nit-1",
            &FixResultKind::WontFix {
                reason: "false positive".to_string(),
            },
        );

        assert!(updated.contains("\u{1F635}"));
        assert!(updated.contains("> Won't fix: false positive"));
    }

    #[test]
    fn test_eligible_item_selection() {
        let findings = vec![make_finding("a"), make_finding("b"), make_finding("c")];
        let comment = render_findings_for_github(&findings, "Summary.");

        // Check only "b"
        let mut lines: Vec<String> = comment.lines().map(String::from).collect();
        for line in &mut lines {
            if line.contains("b description") {
                *line = line.replace("- [ ] ", "- [x] ");
            }
        }
        let comment = lines.join("\n");

        let items = parse_fix_items(&comment);
        let eligible: Vec<_> = items
            .iter()
            .filter(|item| item.state == CheckboxState::Checked)
            .collect();

        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].finding.id, "b");
    }

    #[test]
    fn test_multiple_eligible_items() {
        let findings = vec![make_finding("a"), make_finding("b"), make_finding("c")];
        let comment = render_findings_for_github(&findings, "Summary.");

        // Check "a" and "c"
        let mut lines: Vec<String> = comment.lines().map(String::from).collect();
        for line in &mut lines {
            if line.contains("a description") || line.contains("c description") {
                *line = line.replace("- [ ] ", "- [x] ");
            }
        }
        let comment = lines.join("\n");

        let items = parse_fix_items(&comment);
        let eligible: Vec<_> = items
            .iter()
            .filter(|item| item.state == CheckboxState::Checked)
            .collect();

        assert_eq!(eligible.len(), 2);
        assert!(eligible.iter().any(|i| i.finding.id == "a"));
        assert!(eligible.iter().any(|i| i.finding.id == "c"));
    }

    #[test]
    fn test_no_eligible_items() {
        let findings = vec![make_finding("a")];
        let comment = render_findings_for_github(&findings, "Summary.");

        let items = parse_fix_items(&comment);
        let eligible: Vec<_> = items
            .iter()
            .filter(|item| item.state == CheckboxState::Checked)
            .collect();

        assert!(eligible.is_empty());
    }

    #[test]
    fn test_already_fixed_items_not_eligible() {
        let findings = vec![make_finding("a")];
        let comment = render_findings_for_github(&findings, "Summary.");
        let comment = comment.replace("- [ ] ", "- ✅ ");

        let items = parse_fix_items(&comment);
        let eligible: Vec<_> = items
            .iter()
            .filter(|item| item.state == CheckboxState::Checked)
            .collect();

        assert!(eligible.is_empty());
    }
}
