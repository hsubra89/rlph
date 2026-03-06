//! Standalone fix flow for queued findings on a PR.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tracing::{info, warn};

/// Maximum number of push attempts before giving up (rebase+retry on conflict).
const MAX_PUSH_ATTEMPTS: u32 = 3;

/// Maximum number of fetch retry attempts (git lock contention under concurrency).
const MAX_FETCH_ATTEMPTS: u32 = 3;

/// Maximum number of attempts for a critical finding before marking it as failed.
const MAX_RETRY_ATTEMPTS: u8 = 2;

use crate::config::{Config, ReviewStepConfig};
use crate::error::{Error, Result};
use crate::fix_comment::{
    FindingState, FixItem, FixResultKind, REACTION_CONFUSED, REACTION_THUMBS_UP, ReplyMap,
    build_fix_items_from_review_comments, collect_reply_bodies, format_review_context,
};
use crate::fix_deps::{FindingDeps, resolved_finding_ids};
use crate::fix_scheduler::{self, ScheduleAction};
use crate::ids::{CommentId, PrNumber, ReactionId};
use crate::orchestrator::{CorrectionRunner, retry_with_correction};
use crate::prompts::PromptEngine;
use crate::review_schema::{
    FINDING_MARKER, SchemaName, Severity, StandaloneFixOutput, parse_standalone_fix_output,
};
use crate::runner::{AgentRunner, AnyRunner, Phase, RunResult, build_runner};
use crate::submission::{PrReviewComment, Reaction, SubmissionBackend};
use crate::worktree::{WorktreeManager, git_in_dir, resolve_setup_script, validate_branch_name};

/// Run the standalone fix flow for ALL 🚀-reacted findings on a PR sequentially.
///
/// Steps:
/// 1. Fetch inline review comments and their reactions, parse queued items
/// 2. Collect all eligible queued items (respecting dependencies)
/// 3. Run a fix agent for each item sequentially
///    - Each gets its own worktree off `origin/<pr-branch>`
///    - Parse StandaloneFixOutput JSON (with retry)
///    - If fixed: rebase onto `origin/<pr-branch>`, push with retry
///    - Update reactions and post reply
///    - Clean up worktree
/// 4. Collect results, log any errors
pub async fn run_fix<C: CorrectionRunner + 'static>(
    pr_number: PrNumber,
    pr_branch: &str,
    config: &Config,
    submission: Arc<impl SubmissionBackend + 'static>,
    prompt_engine: &PromptEngine,
    repo_root: &Path,
    correction_runner: Arc<C>,
) -> Result<()> {
    // Validate pr_branch from GitHub API at the trust boundary
    validate_branch_name(pr_branch)?;

    // 1. Fetch review comments and reactions, build fix items
    info!(pr_number = %pr_number, "polling GitHub for PR review comments");
    let (items, comments) = fetch_and_parse_items(pr_number, &*submission)?;
    info!(total = items.len(), "parsed fix items from review comments");

    // Clean up stale 🚀 reactions on already-resolved findings (best-effort)
    cleanup_stale_rockets(&items, &*submission);

    // 2. Build dependency graph and collect queued items for the scheduler
    let finding_deps = FindingDeps::build(&items);
    let queued_items: Vec<FixItem> = items
        .iter()
        .filter(|i| i.state == FindingState::Queued)
        .cloned()
        .collect();

    // 3. Pre-compute per-item data and run fixes via the scheduler
    let setup_script =
        resolve_setup_script(config.worktree_setup_script.as_deref(), repo_root)?.map(Arc::from);
    let shared = SharedFixState::new(
        config,
        pr_branch,
        repo_root,
        Arc::clone(&submission),
        Arc::clone(&correction_runner),
        setup_script,
    );

    let mut reply_map = collect_reply_bodies(&comments);
    let mut completed: HashSet<String> = HashSet::new();
    let mut failed: HashSet<String> = HashSet::new();
    let mut skipped: usize = 0;
    let mut errors = Vec::new();
    let mut total_scheduled: usize = 0;

    loop {
        let mut sched_completed = resolved_finding_ids(&items);
        sched_completed.extend(completed.iter().map(String::as_str));
        let sched_failed: HashSet<&str> = failed.iter().map(String::as_str).collect();

        let finding_ids = match fix_scheduler::next_action(
            &queued_items,
            &finding_deps,
            &sched_completed,
            &sched_failed,
        ) {
            ScheduleAction::RunBatch(ids) => ids,
            ScheduleAction::Idle => break,
        };

        for finding_id in finding_ids {
            total_scheduled += 1;

            let Some(item) = queued_items
                .iter()
                .find(|i| i.finding.id == finding_id)
                .cloned()
            else {
                warn!(%finding_id, "scheduler returned unknown finding ID, skipping");
                failed.insert(finding_id);
                skipped += 1;
                continue;
            };

            let Some(prepared) = prepare_fix_item(
                item,
                pr_number,
                &shared.fix_config,
                prompt_engine,
                &mut reply_map,
            ) else {
                skipped += 1;
                failed.insert(finding_id);
                continue;
            };

            match run_prepared_fix(&shared, prepared, pr_number).await {
                Ok(()) => {
                    completed.insert(finding_id);
                }
                Err(e) => {
                    warn!(error = %e, "fix agent failed");
                    errors.push(e);
                    failed.insert(finding_id);
                }
            }
        }
    }

    if total_scheduled == 0 {
        info!("no eligible items found — nothing to fix");
        return Ok(());
    }

    if skipped == total_scheduled {
        return Err(Error::Orchestrator(format!(
            "all {skipped} eligible fix item(s) were skipped due to validation errors"
        )));
    } else if skipped > 0 {
        warn!(
            skipped,
            total = total_scheduled,
            "some fix items were skipped due to validation errors"
        );
    }

    // 4. Report results
    if errors.is_empty() {
        info!(pr_number = %pr_number, "all fixes completed successfully");
        Ok(())
    } else {
        let count = errors.len();
        Err(Error::Orchestrator(format!(
            "{count} fix(es) failed; first: {}",
            errors[0]
        )))
    }
}

/// Run the fix command as a continuous polling loop.
///
/// Polls for newly 🚀-reacted comments every `poll_seconds`, then runs
/// the scheduler in an inner loop each cycle, processing all available
/// findings until the scheduler returns Idle before waiting again.
/// Failed CRITICALs are retried once before being added to the failed set.
///
/// On shutdown signal: completes the current fix (if any), then exits cleanly.
#[allow(clippy::too_many_arguments)]
pub async fn run_fix_loop<C: CorrectionRunner + 'static>(
    pr_number: PrNumber,
    pr_branch: &str,
    config: &Config,
    submission: Arc<impl SubmissionBackend + 'static>,
    prompt_engine: &PromptEngine,
    repo_root: &Path,
    correction_runner: Arc<C>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    validate_branch_name(pr_branch)?;

    let setup_script =
        resolve_setup_script(config.worktree_setup_script.as_deref(), repo_root)?.map(Arc::from);
    let shared = SharedFixState::new(
        config,
        pr_branch,
        repo_root,
        Arc::clone(&submission),
        Arc::clone(&correction_runner),
        setup_script,
    );
    let poll_duration = config.poll_seconds;

    let mut completed: HashSet<String> = HashSet::new();
    let mut failed: HashSet<String> = HashSet::new();
    let mut retries: HashMap<String, u8> = HashMap::new();
    let mut cycle: u64 = 0;
    let mut finding_deps: Option<FindingDeps> = None;

    loop {
        cycle += 1;

        if *shutdown.borrow() {
            info!("shutdown requested, stopping poll loop");
            break;
        }

        // Fetch and parse
        eprintln!("[rlph] polling for newly 🚀-reacted comments (cycle {cycle})");
        info!(
            pr_number = %pr_number,
            cycle,
            completed = completed.len(),
            failed = failed.len(),
            "polling for newly 🚀-reacted comments"
        );
        let (items, comments) = match fetch_and_parse_items(pr_number, &*shared.submission) {
            Ok(result) => result,
            Err(e) => {
                warn!(error = %e, cycle, "failed to fetch review comments, retrying next cycle");
                if wait_or_shutdown(poll_duration, &mut shutdown).await {
                    break;
                }
                continue;
            }
        };

        // Clean up stale 🚀 reactions on already-resolved findings (best-effort)
        cleanup_stale_rockets(&items, &*shared.submission);

        if *shutdown.borrow() {
            info!("shutdown requested after fetch, stopping poll loop");
            break;
        }

        // Build dependency graph, rebuilding if item count changed
        let deps = match &finding_deps {
            Some(existing) if !existing.is_stale(items.len()) => finding_deps.as_ref().unwrap(),
            Some(_) => {
                warn!(
                    old_count = finding_deps.as_ref().unwrap().item_count(),
                    new_count = items.len(),
                    "review comments changed: item count changed, rebuilding dependency graph"
                );
                finding_deps.insert(FindingDeps::build(&items))
            }
            None => finding_deps.insert(FindingDeps::build(&items)),
        };

        // Build queued items for scheduler (only items with 🚀 reaction)
        let queued_items: Vec<FixItem> = items
            .iter()
            .filter(|item| item.state == FindingState::Queued)
            .cloned()
            .collect();

        // Build reply map lazily — only when there are queued items
        let mut reply_map = if queued_items.is_empty() {
            ReplyMap::new()
        } else {
            collect_reply_bodies(&comments)
        };

        // Scheduler-driven inner loop: process all available work before waiting
        run_scheduler_cycle(
            &shared,
            &items,
            &queued_items,
            deps,
            &mut completed,
            &mut failed,
            &mut retries,
            pr_number,
            prompt_engine,
            &mut reply_map,
            &shutdown,
        )
        .await;

        eprintln!(
            "[rlph] poll cycle {cycle} summary: {} completed, {} failed",
            completed.len(),
            failed.len()
        );
        info!(
            cycle,
            completed = completed.len(),
            failed = failed.len(),
            "poll cycle summary"
        );

        // Wait for poll interval or shutdown
        if wait_or_shutdown(poll_duration, &mut shutdown).await {
            info!("shutdown requested during poll wait");
            break;
        }
    }

    info!(
        completed = completed.len(),
        failed = failed.len(),
        "fix loop finished"
    );

    Ok(())
}

/// Run one scheduler cycle: repeatedly ask the scheduler for work and execute
/// it until it returns `Idle` or a shutdown is requested.
#[allow(clippy::too_many_arguments)]
async fn run_scheduler_cycle<S: SubmissionBackend, C: CorrectionRunner>(
    shared: &SharedFixState<S, C>,
    items: &[FixItem],
    queued_items: &[FixItem],
    deps: &FindingDeps,
    completed: &mut HashSet<String>,
    failed: &mut HashSet<String>,
    retries: &mut HashMap<String, u8>,
    pr_number: PrNumber,
    prompt_engine: &PromptEngine,
    reply_map: &mut ReplyMap,
    shutdown: &watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            break;
        }

        // Build completed set for scheduler: GitHub-resolved + locally completed
        let mut sched_completed = resolved_finding_ids(items);
        sched_completed.extend(completed.iter().map(String::as_str));
        let sched_failed: HashSet<&str> = failed.iter().map(String::as_str).collect();

        match fix_scheduler::next_action(queued_items, deps, &sched_completed, &sched_failed) {
            ScheduleAction::RunBatch(finding_ids) => {
                let batch_size = finding_ids.len();
                eprintln!("[rlph] Batch: scheduling {batch_size} findings: {finding_ids:?}");
                info!(
                    batch_size,
                    ?finding_ids,
                    "batch processing mode: scheduling session"
                );

                // Prepare all items, skipping any that fail validation
                let mut prepared_items = Vec::with_capacity(batch_size);
                for finding_id in &finding_ids {
                    if let Some(prepared) = lookup_and_prepare(
                        finding_id,
                        queued_items,
                        pr_number,
                        &shared.fix_config,
                        prompt_engine,
                        reply_map,
                        failed,
                    ) {
                        prepared_items.push(prepared);
                    }
                }

                if prepared_items.is_empty() {
                    warn!("all batch items failed preparation, skipping batch");
                    continue;
                }

                let (batch_completed, batch_error) =
                    run_batch_fix(shared, prepared_items, pr_number).await;

                for id in &batch_completed {
                    eprintln!("[rlph] Finding completed successfully: {id}");
                    info!(%id, "finding completed successfully");
                }
                completed.extend(batch_completed);

                if let Some((failed_id, e)) = batch_error {
                    // Retry critical-severity findings before marking failed
                    let is_critical = queued_items.iter().any(|i| {
                        i.finding.id == failed_id && i.finding.severity == Severity::Critical
                    });

                    if is_critical {
                        let attempts = retries.entry(failed_id.clone()).or_insert(0);
                        *attempts += 1;
                        if *attempts >= MAX_RETRY_ATTEMPTS {
                            eprintln!("[rlph] Critical fix failed after retry: {failed_id}: {e}");
                            warn!(%failed_id, error = %e, "critical fix failed after retry");
                            failed.insert(failed_id);
                        } else {
                            eprintln!(
                                "[rlph] Critical fix failed (attempt {}, will retry): {failed_id}: {e}",
                                *attempts
                            );
                            warn!(%failed_id, error = %e, attempt = *attempts, "critical fix failed, will retry");
                        }
                    } else {
                        eprintln!("[rlph] Fix failed: {failed_id}: {e}");
                        warn!(finding_id = %failed_id, error = %e, "fix failed");
                        failed.insert(failed_id);
                    }
                }
            }
            ScheduleAction::Idle => {
                break;
            }
        }
    }
}

/// Fetch all inline review comments on a PR, check reactions for each that
/// contains a finding marker, and build `FixItem`s.
///
/// Returns the raw comments alongside the fix items so callers can build
/// reply maps lazily — only when there are newly-queued items to process.
pub fn fetch_and_parse_items(
    pr_number: PrNumber,
    submission: &(impl SubmissionBackend + ?Sized),
) -> Result<(Vec<FixItem>, Vec<PrReviewComment>)> {
    let comments = submission.fetch_pr_review_comments(pr_number)?;

    // Only fetch reactions for comments that contain the finding marker
    let finding_comments: Vec<_> = comments
        .iter()
        .filter(|c| c.in_reply_to_id.is_none() && c.body.contains(FINDING_MARKER))
        .collect();

    // Fetch reactions in batches to avoid exhausting file descriptors
    // (each `gh api` call opens multiple fds).
    let reactions_by_comment: Vec<Result<(CommentId, Vec<Reaction>)>> =
        crate::run_batched(&finding_comments, |comment| {
            let id = comment.id;
            submission
                .list_review_comment_reactions(id)
                .map(|reactions| (id, reactions))
        });

    let collected: Vec<_> = reactions_by_comment.into_iter().collect::<Result<_>>()?;

    Ok((
        build_fix_items_from_review_comments(&comments, &collected),
        comments,
    ))
}

/// Sleep for the poll duration, but return early if shutdown is requested.
/// Returns `true` if shutdown was requested.
pub(crate) async fn wait_or_shutdown(
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

/// Run a single fix: create worktree, run agent, push, update reactions, cleanup.
async fn run_single_fix(
    ctx: FixContext<'_>,
    wm: &WorktreeManager,
    submission: &(impl SubmissionBackend + ?Sized),
    correction_runner: &(impl CorrectionRunner + ?Sized),
) -> Result<()> {
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
    pr_number: PrNumber,
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
    agent_timeout_retries: u32,
    setup_script: Option<Arc<Path>>,
}

impl<S, C> SharedFixState<S, C> {
    fn new(
        config: &Config,
        pr_branch: &str,
        repo_root: &Path,
        submission: Arc<S>,
        correction_runner: Arc<C>,
        setup_script: Option<Arc<Path>>,
    ) -> Self {
        Self {
            fix_config: Arc::new(config.fix.clone()),
            worktree_dir: Arc::from(config.worktree_dir.as_str()),
            repo_root: Arc::from(repo_root),
            pr_branch: Arc::from(pr_branch),
            submission,
            correction_runner,
            agent_timeout_retries: config.agent_timeout_retries,
            setup_script,
        }
    }
}

impl<S, C> SharedFixState<S, C> {
    fn make_worktree_manager(&self) -> WorktreeManager {
        WorktreeManager::new(
            self.repo_root.to_path_buf(),
            self.repo_root.join(&*self.worktree_dir),
            self.pr_branch.to_string(),
        )
        .with_setup_script(self.setup_script.as_deref().map(Path::to_path_buf))
    }
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
            agent_timeout_retries: self.agent_timeout_retries,
            setup_script: self.setup_script.clone(),
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
    /// The GitHub review comment ID (for re-fetching fresh body at execution time).
    comment_id: CommentId,
    /// Reply bodies collected from the review thread.
    replies: Vec<String>,
}

/// Look up a finding by ID in `queued_items`, then prepare it via [`prepare_fix_item`].
///
/// Returns `None` (and inserts into `failed`) when the finding ID is unknown or
/// preparation fails.
fn lookup_and_prepare(
    finding_id: &str,
    queued_items: &[FixItem],
    pr_number: PrNumber,
    fix_config: &ReviewStepConfig,
    prompt_engine: &PromptEngine,
    reply_map: &mut ReplyMap,
    failed: &mut HashSet<String>,
) -> Option<PreparedFixItem> {
    let Some(item) = queued_items
        .iter()
        .find(|i| i.finding.id == finding_id)
        .cloned()
    else {
        warn!(%finding_id, "scheduler returned unknown finding ID, marking as failed");
        failed.insert(finding_id.to_owned());
        return None;
    };

    let prepared = prepare_fix_item(item, pr_number, fix_config, prompt_engine, reply_map);
    if prepared.is_none() {
        failed.insert(finding_id.to_owned());
    }
    prepared
}

/// Validate branch name, render the prompt, and log the spawn.
///
/// Returns `None` if the item should be skipped (invalid branch name or prompt
/// rendering failure), with a warning already logged.
fn prepare_fix_item(
    item: FixItem,
    pr_number: PrNumber,
    fix_config: &ReviewStepConfig,
    prompt_engine: &PromptEngine,
    reply_map: &mut ReplyMap,
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
        "prepared fix item"
    );

    let comment_id = item.comment_id;
    let replies = reply_map.get(&comment_id).cloned().unwrap_or_default();

    Some(PreparedFixItem {
        item,
        fix_branch,
        prompt,
        comment_id,
        replies,
    })
}

/// Re-fetch a review comment and append formatted review context to the prompt.
///
/// Warns and continues without context on fetch failure.
fn append_review_context(
    submission: &dyn SubmissionBackend,
    comment_id: CommentId,
    replies: &[String],
    prompt: &mut String,
) {
    match submission.fetch_review_comment_by_id(comment_id) {
        Ok(comment) => {
            prompt.push_str(&format_review_context(&comment.body, replies));
        }
        Err(e) => {
            warn!(
                comment_id = %comment_id,
                error = %e,
                "failed to re-fetch review comment body, proceeding without review context"
            );
        }
    }
}

/// Build [`FixContext`] and run a single fix.
///
/// Shared by both [`run_fix`] (one-shot) and [`run_fix_loop`] (polling).
async fn run_prepared_fix<S: SubmissionBackend, C: CorrectionRunner>(
    shared: &SharedFixState<S, C>,
    prepared: PreparedFixItem,
    pr_number: PrNumber,
) -> Result<()> {
    let PreparedFixItem {
        item,
        fix_branch,
        mut prompt,
        comment_id,
        replies,
    } = prepared;

    append_review_context(&*shared.submission, comment_id, &replies, &mut prompt);

    let ctx = FixContext {
        item,
        pr_number,
        pr_branch: &shared.pr_branch,
        fix_branch: &fix_branch,
        fix_config: &shared.fix_config,
        agent_timeout_retries: shared.agent_timeout_retries,
        prompt: &prompt,
    };
    let wm = shared.make_worktree_manager();
    run_single_fix(ctx, &wm, &*shared.submission, &*shared.correction_runner).await
}

/// Run a batch of WARNING/INFO findings in a single shared agent session.
///
/// Creates one worktree and one agent session. The first finding starts the
/// session normally; subsequent findings are fed via `resume_agent`
/// using the session ID from the previous run. Each finding gets its own
/// commit/push/reaction cycle. Aborts on the first failure.
///
/// # Preconditions
///
/// `prepared_items` must be non-empty (caller is responsible for filtering).
///
/// Returns `(completed_finding_ids, optional_error)`.
async fn run_batch_fix<S: SubmissionBackend, C: CorrectionRunner>(
    shared: &SharedFixState<S, C>,
    prepared_items: Vec<PreparedFixItem>,
    pr_number: PrNumber,
) -> (HashSet<String>, Option<(String, Error)>) {
    let batch_size = prepared_items.len();
    let mut completed_ids = HashSet::new();

    // Capture the first finding ID before we move prepared_items, so we can
    // attribute worktree-creation failures to a specific finding.
    let first_finding_id = prepared_items[0].item.finding.id.clone();

    // Use the first item's fix_branch as the worktree branch for the whole batch
    let batch_branch = prepared_items[0].fix_branch.clone();

    // Create a single worktree for the batch
    let wm = shared.make_worktree_manager();

    let worktree_path = match wm.create_fresh(&batch_branch, &shared.pr_branch) {
        Ok(info) => info.path,
        Err(e) => return (completed_ids, Some((first_finding_id, e))),
    };

    info!(
        batch_size,
        branch = %batch_branch,
        path = %worktree_path.display(),
        "created batch worktree"
    );

    // Build runner for the initial agent invocation
    let runner = build_fix_runner(&shared.fix_config, shared.agent_timeout_retries);

    // Run each finding sequentially, sharing the session
    let mut session_id: Option<String> = None;
    let error: Option<(String, Error)> = 'batch: {
        for (idx, prepared) in prepared_items.into_iter().enumerate() {
            let PreparedFixItem {
                item,
                fix_branch: _,
                mut prompt,
                comment_id,
                replies,
            } = prepared;

            let finding_id = item.finding.id.clone();
            let position = idx + 1;

            info!(
                %finding_id,
                position,
                batch_size,
                "batch session: fixing finding ({position} of {batch_size})"
            );

            // Append review context (re-fetch comment for freshness)
            append_review_context(&*shared.submission, comment_id, &replies, &mut prompt);

            // Run agent (first finding) or resume session (subsequent findings)
            let run_result = if idx == 0 {
                info!(%finding_id, "spawning batch fix agent");
                runner.run(Phase::Fix, &prompt, &worktree_path).await
            } else {
                let Some(ref sid) = session_id else {
                    let err = Error::Orchestrator(
                        "no session_id from previous finding, cannot resume batch".into(),
                    );
                    warn!(%finding_id, %err, "batch abort");
                    break 'batch Some((finding_id, err));
                };
                info!(%finding_id, session_id = %sid, "resuming batch session");
                resume_agent(
                    &*shared.correction_runner,
                    &shared.fix_config,
                    sid,
                    &prompt,
                    &worktree_path,
                    Some("fix"),
                )
                .await
            };

            let run_result = match run_result {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[rlph] Batch abort (agent failed): {finding_id}: {e}");
                    warn!(%finding_id, error = %e, "batch abort: agent failed");
                    break 'batch Some((finding_id, e));
                }
            };

            // Track session_id for subsequent resumes
            match (&run_result.session_id, idx) {
                (Some(_), _) => session_id.clone_from(&run_result.session_id),
                (None, 0) if batch_size > 1 => {
                    let err = Error::Orchestrator(
                        "agent returned no session_id on first finding, cannot resume batch".into(),
                    );
                    warn!(%finding_id, %err, "batch abort");
                    break 'batch Some((finding_id, err));
                }
                (None, 0) => {} // single-item batch, session_id not needed
                (None, _) => {
                    warn!(
                        %finding_id,
                        "runner returned no session_id mid-batch, keeping previous (possibly stale) session_id"
                    );
                }
            }

            // Parse output (with retry via session correction)
            let fix_output = match parse_fix_with_retry(
                &run_result,
                &shared.fix_config,
                &worktree_path,
                &*shared.correction_runner,
            )
            .await
            {
                Ok(output) => output,
                Err(e) => {
                    eprintln!("[rlph] Batch abort (parse failed): {finding_id}: {e}");
                    warn!(%finding_id, error = %e, "batch abort: parse failed");
                    break 'batch Some((finding_id, e));
                }
            };

            info!(%finding_id, ?fix_output, "batch fix agent completed");

            // Apply result: push if fixed, update reactions
            let fix_result = match apply_fix_output(
                fix_output,
                &finding_id,
                &worktree_path,
                &batch_branch,
                &shared.pr_branch,
                &ConflictResolutionCtx {
                    session_id: session_id.as_deref(),
                    fix_config: &shared.fix_config,
                    correction_runner: &*shared.correction_runner,
                },
            )
            .await
            {
                Ok(result) => result,
                Err(e) => {
                    eprintln!("[rlph] Batch abort (push failed): {finding_id}: {e}");
                    warn!(%finding_id, error = %e, "batch abort: push failed");
                    break 'batch Some((finding_id, e));
                }
            };

            let ctx = FixContext {
                item,
                pr_number,
                pr_branch: &shared.pr_branch,
                fix_branch: &batch_branch,
                fix_config: &shared.fix_config,
                agent_timeout_retries: shared.agent_timeout_retries,
                prompt: &prompt,
            };
            update_reactions_and_reply(&ctx, &*shared.submission, &fix_result);

            completed_ids.insert(finding_id);
        }
        None // all findings completed successfully
    };

    // Always clean up the worktree
    info!(path = %worktree_path.display(), "cleaning up batch worktree");
    if let Err(e) = wm.remove(&worktree_path) {
        warn!(error = %e, "failed to clean up batch worktree");
    }

    (completed_ids, error)
}

/// Params only needed for the conflict-resolution branch in [`apply_fix_output`].
struct ConflictResolutionCtx<'a, C: ?Sized> {
    session_id: Option<&'a str>,
    fix_config: &'a ReviewStepConfig,
    correction_runner: &'a C,
}

/// Map agent output to a fix result, pushing to the PR branch if the fix was applied.
async fn apply_fix_output<C: CorrectionRunner + ?Sized>(
    fix_output: StandaloneFixOutput,
    finding_id: &str,
    worktree_path: &Path,
    fix_branch: &str,
    pr_branch: &str,
    conflict_ctx: &ConflictResolutionCtx<'_, C>,
) -> Result<FixResultKind> {
    match fix_output {
        StandaloneFixOutput::Fixed { commit_message } => {
            eprintln!("[rlph] Fix applied — rebasing and pushing: {finding_id}");
            info!(%finding_id, commit_message, "fix applied — rebasing and pushing");
            match (
                push_to_pr_branch_with_retry(worktree_path, fix_branch, pr_branch).await,
                conflict_ctx.session_id,
            ) {
                (Ok(()), _) => Ok(FixResultKind::Fixed { commit_message }),
                (Err(Error::RebaseConflict { .. }), Some(sid)) => {
                    eprintln!("[rlph] Rebase conflict — resuming agent to resolve: {finding_id}");
                    resolve_conflict_and_push(
                        worktree_path,
                        fix_branch,
                        pr_branch,
                        sid,
                        conflict_ctx.fix_config,
                        conflict_ctx.correction_runner,
                    )
                    .await?;
                    Ok(FixResultKind::Fixed { commit_message })
                }
                (Err(e), _) => Err(e),
            }
        }
        StandaloneFixOutput::WontFix { reason } => {
            eprintln!("[rlph] Finding marked as won't fix: {finding_id}");
            info!(%finding_id, reason, "finding marked as won't fix");
            Ok(FixResultKind::WontFix { reason })
        }
    }
}

/// Resume an agent session using config fields from [`ReviewStepConfig`].
async fn resume_agent(
    correction_runner: &(impl CorrectionRunner + ?Sized),
    fix_config: &ReviewStepConfig,
    session_id: &str,
    prompt: &str,
    working_dir: &Path,
    stream_prefix: Option<&str>,
) -> Result<RunResult> {
    correction_runner
        .resume(
            fix_config.runner,
            &fix_config.agent_binary,
            fix_config.agent_model.as_deref(),
            fix_config.agent_effort.as_deref(),
            fix_config.agent_variant.as_deref(),
            session_id,
            prompt,
            working_dir,
            fix_config.agent_timeout,
            stream_prefix,
        )
        .await
}

/// Attempt to resolve rebase conflicts by resuming the agent session.
///
/// Starts a rebase (leaving conflicts in the worktree), then resumes the agent
/// asking it to resolve conflict markers and continue the rebase. On success,
/// pushes the result.
///
/// Fetches before rebasing to avoid using a stale ref when a concurrent push
/// landed between the prior abort and this retry.
async fn resolve_conflict_and_push<C: CorrectionRunner + ?Sized>(
    worktree_path: &Path,
    fix_branch: &str,
    pr_branch: &str,
    session_id: &str,
    fix_config: &ReviewStepConfig,
    correction_runner: &C,
) -> Result<()> {
    match rebase_onto(worktree_path, pr_branch).await {
        Ok(()) => { /* Rebase clean — fall through to push */ }
        Err(Error::RebaseConflict { .. }) => {
            // Conflicts in worktree — resume agent to resolve
            let prompt = "The rebase onto the PR branch has merge conflicts. \
                Please resolve ALL conflicts:\n\
                1. Edit each conflicted file to remove conflict markers (<<<<<<< / ======= / >>>>>>>)\n\
                2. `git add` each resolved file\n\
                3. `git rebase --continue`\n\
                Do not abort the rebase.";

            resume_agent(
                correction_runner,
                fix_config,
                session_id,
                prompt,
                worktree_path,
                None,
            )
            .await
            .inspect_err(|_| abort_rebase(worktree_path))?;
        }
        Err(e) => return Err(e),
    }

    // Push with retry. If this hits another RebaseConflict we intentionally
    // propagate the error rather than resuming the agent again to avoid an
    // infinite conflict-resolution loop.
    push_to_pr_branch_with_retry(worktree_path, fix_branch, pr_branch).await
}

/// Build the runner used for fix agent invocations.
fn build_fix_runner(config: &ReviewStepConfig, agent_timeout_retries: u32) -> AnyRunner {
    build_runner(
        config.runner,
        &config.agent_binary,
        config.agent_model.as_deref(),
        config.agent_effort.as_deref(),
        config.agent_variant.as_deref(),
        config.agent_timeout,
        agent_timeout_retries,
    )
    .with_stream_prefix("fix".to_string())
}

/// Inner function: spawn agent, parse output, rebase/push with retry, update reactions + reply.
async fn run_fix_agent_and_apply(
    ctx: &FixContext<'_>,
    worktree_path: &Path,
    submission: &(impl SubmissionBackend + ?Sized),
    correction_runner: &(impl CorrectionRunner + ?Sized),
) -> Result<()> {
    // Spawn fix agent
    info!(finding_id = %ctx.item.finding.id, "spawning fix agent");
    let runner = build_fix_runner(ctx.fix_config, ctx.agent_timeout_retries);

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

    // Apply result
    let fix_result = apply_fix_output(
        fix_output,
        &ctx.item.finding.id,
        worktree_path,
        ctx.fix_branch,
        ctx.pr_branch,
        &ConflictResolutionCtx {
            session_id: run_result.session_id.as_deref(),
            fix_config: ctx.fix_config,
            correction_runner,
        },
    )
    .await?;

    // Update reactions and post reply (best-effort — don't fail on already-pushed code)
    update_reactions_and_reply(ctx, submission, &fix_result);

    Ok(())
}

/// Remove 🚀 reactions from a single review comment (best-effort).
fn remove_rocket_reactions(
    finding_id: &str,
    comment_id: CommentId,
    rocket_reaction_ids: &[ReactionId],
    submission: &(impl SubmissionBackend + ?Sized),
) {
    for &reaction_id in rocket_reaction_ids {
        if let Err(e) = submission.delete_review_comment_reaction(comment_id, reaction_id) {
            warn!(
                finding_id, comment_id = %comment_id, reaction_id = %reaction_id,
                error = %e,
                "failed to remove 🚀 reaction"
            );
        }
    }
}

/// Remove stale 🚀 reactions from items that are already resolved (Fixed or WontFix).
///
/// When a finding has both 🚀 and 👍/😕, the state is correctly resolved as Fixed/WontFix,
/// but the 🚀 is never cleaned up since rockets are only removed during active fix processing.
fn cleanup_stale_rockets(items: &[FixItem], submission: &(impl SubmissionBackend + ?Sized)) {
    for item in items {
        if matches!(item.state, FindingState::Fixed | FindingState::WontFix)
            && !item.rocket_reaction_ids.is_empty()
        {
            info!(
                finding_id = %item.finding.id,
                comment_id = %item.comment_id,
                rockets = item.rocket_reaction_ids.len(),
                "cleaning up stale 🚀 reactions on resolved finding"
            );
            remove_rocket_reactions(
                &item.finding.id,
                item.comment_id,
                &item.rocket_reaction_ids,
                submission,
            );
        }
    }
}

/// Update reactions on the finding's review comment and post a reply.
///
/// - Add 👍 (fixed) or 😕 (won't fix)
/// - Post a reply with details
/// - Remove all 🚀 reactions
///
/// The outcome emoji and reply are added before removing 🚀 so that observers
/// always see the result before the queuing signal disappears.
fn update_reactions_and_reply(
    ctx: &FixContext<'_>,
    submission: &(impl SubmissionBackend + ?Sized),
    fix_result: &FixResultKind,
) {
    let comment_id = ctx.item.comment_id;
    let finding_id = &ctx.item.finding.id;

    // Add result reaction (best-effort)
    let (reaction, reply_body) = match fix_result {
        FixResultKind::Fixed { commit_message } => {
            (REACTION_THUMBS_UP, format!("Fixed: {commit_message}"))
        }
        FixResultKind::WontFix { reason } => (REACTION_CONFUSED, format!("Won't fix: {reason}")),
    };

    if let Err(e) = submission.add_review_comment_reaction(comment_id, reaction) {
        warn!(
            %finding_id, comment_id = %comment_id, reaction,
            error = %e,
            "failed to add result reaction"
        );
    }

    // Post reply (best-effort)
    info!(
        pr_number = %ctx.pr_number,
        %finding_id,
        comment_id = %comment_id,
        "posting fix reply to review comment"
    );
    if let Err(e) = submission.reply_to_review_comment(ctx.pr_number, comment_id, &reply_body) {
        warn!(
            %finding_id, comment_id = %comment_id,
            error = %e,
            "failed to post fix reply"
        );
    }

    // Remove all 🚀 reactions (best-effort) — done last so the outcome is visible
    // before the queuing signal disappears.
    remove_rocket_reactions(
        finding_id,
        comment_id,
        &ctx.item.rocket_reaction_ids,
        submission,
    );
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

/// Rebase current branch onto origin/<pr-branch>, fetching first.
///
/// On conflict the rebase is **not** aborted — the caller decides whether to
/// resume the agent for conflict resolution or abort itself.
async fn rebase_onto(worktree_path: &Path, pr_branch: &str) -> Result<()> {
    fetch_with_retry(worktree_path, pr_branch).await?;
    start_rebase(worktree_path, pr_branch)
}

/// Start a rebase onto origin/<pr-branch> (assumes refs are already fetched).
///
/// On conflict the rebase is **not** aborted — the caller decides whether to
/// resume the agent for conflict resolution or abort itself.
fn start_rebase(worktree_path: &Path, pr_branch: &str) -> Result<()> {
    let remote_ref = format!("origin/{pr_branch}");

    if let Err(stderr) = git_in_dir(worktree_path, &["rebase", &remote_ref]) {
        if stderr.contains("CONFLICT") || stderr.contains("could not apply") {
            warn!(remote_ref, stderr = %stderr, "rebase conflict");
            return Err(Error::RebaseConflict {
                target_ref: remote_ref,
            });
        }
        warn!(remote_ref, stderr = %stderr, "git rebase failed (non-conflict)");
        return Err(Error::Orchestrator(format!(
            "git rebase onto {remote_ref} failed: {stderr}"
        )));
    }

    info!(remote_ref, "rebased onto latest PR branch");
    Ok(())
}

/// Best-effort abort of a rebase in progress.
fn abort_rebase(worktree_path: &Path) {
    let _ = git_in_dir(worktree_path, &["rebase", "--abort"]);
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
        // First attempt pushes as-is; rebase only after a conflict reveals divergence
        if attempt > 1
            && let Err(e) = rebase_onto(worktree_path, pr_branch).await
        {
            abort_rebase(worktree_path);
            return Err(e);
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
    use crate::fix_comment::{FindingState, build_fix_items_from_review_comments};
    use crate::fix_deps::{FindingDeps, resolved_finding_ids};
    use crate::test_helpers::{
        make_finding, make_finding_with_deps, make_reactions, make_review_comment,
    };

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
    fn test_eligible_item_selection_with_reactions() {
        let findings = [make_finding("a"), make_finding("b"), make_finding("c")];
        let c1 = make_review_comment(100, &findings[0]);
        let c2 = make_review_comment(200, &findings[1]);
        let c3 = make_review_comment(300, &findings[2]);

        // Only "b" has a 🚀 reaction
        let reactions = vec![
            (CommentId::new(100), vec![]),
            (CommentId::new(200), make_reactions(&[("rocket", 1)])),
            (CommentId::new(300), vec![]),
        ];

        let items = build_fix_items_from_review_comments(&[c1, c2, c3], &reactions);
        let eligible: Vec<_> = items
            .iter()
            .filter(|item| item.state == FindingState::Queued)
            .collect();

        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].finding.id, "b");
    }

    #[test]
    fn test_multiple_eligible_items() {
        let findings = [make_finding("a"), make_finding("b"), make_finding("c")];
        let c1 = make_review_comment(100, &findings[0]);
        let c2 = make_review_comment(200, &findings[1]);
        let c3 = make_review_comment(300, &findings[2]);

        // "a" and "c" have 🚀 reactions
        let reactions = vec![
            (CommentId::new(100), make_reactions(&[("rocket", 1)])),
            (CommentId::new(200), vec![]),
            (CommentId::new(300), make_reactions(&[("rocket", 2)])),
        ];

        let items = build_fix_items_from_review_comments(&[c1, c2, c3], &reactions);
        let eligible: Vec<_> = items
            .iter()
            .filter(|item| item.state == FindingState::Queued)
            .collect();

        assert_eq!(eligible.len(), 2);
        assert!(eligible.iter().any(|i| i.finding.id == "a"));
        assert!(eligible.iter().any(|i| i.finding.id == "c"));
    }

    #[test]
    fn test_no_eligible_items() {
        let findings = [make_finding("a")];
        let c1 = make_review_comment(100, &findings[0]);

        // No reactions
        let items = build_fix_items_from_review_comments(&[c1], &[]);
        let eligible: Vec<_> = items
            .iter()
            .filter(|item| item.state == FindingState::Queued)
            .collect();

        assert!(eligible.is_empty());
    }

    #[test]
    fn test_already_fixed_items_not_eligible() {
        let findings = [make_finding("a")];
        let c1 = make_review_comment(100, &findings[0]);

        // Has 👍 (fixed) — not eligible
        let reactions = vec![(CommentId::new(100), make_reactions(&[("+1", 1)]))];

        let items = build_fix_items_from_review_comments(&[c1], &reactions);
        let eligible: Vec<_> = items
            .iter()
            .filter(|item| item.state == FindingState::Queued)
            .collect();

        assert!(eligible.is_empty());
        assert_eq!(items[0].state, FindingState::Fixed);
    }

    // --- Dependency-aware eligibility tests ---

    #[test]
    fn test_dependent_item_blocked_when_dep_queued() {
        let findings = [make_finding("a"), make_finding_with_deps("b", &["a"])];
        let c1 = make_review_comment(100, &findings[0]);
        let c2 = make_review_comment(200, &findings[1]);

        // Both have 🚀
        let reactions = vec![
            (CommentId::new(100), make_reactions(&[("rocket", 1)])),
            (CommentId::new(200), make_reactions(&[("rocket", 2)])),
        ];

        let items = build_fix_items_from_review_comments(&[c1, c2], &reactions);
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);

        // a has no deps → eligible
        assert!(deps.deps_met("a", &resolved));
        // b depends on a which is Queued (not Fixed) → blocked
        assert!(!deps.deps_met("b", &resolved));
    }

    #[test]
    fn test_dependent_item_unblocked_when_dep_fixed() {
        let findings = [make_finding("a"), make_finding_with_deps("b", &["a"])];
        let c1 = make_review_comment(100, &findings[0]);
        let c2 = make_review_comment(200, &findings[1]);

        // a has 👍 (fixed), b has 🚀
        let reactions = vec![
            (CommentId::new(100), make_reactions(&[("+1", 1)])),
            (CommentId::new(200), make_reactions(&[("rocket", 2)])),
        ];

        let items = build_fix_items_from_review_comments(&[c1, c2], &reactions);
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);

        assert!(deps.deps_met("b", &resolved));
    }

    #[test]
    fn test_dependent_item_unblocked_when_dep_wontfix() {
        let findings = [make_finding("a"), make_finding_with_deps("b", &["a"])];
        let c1 = make_review_comment(100, &findings[0]);
        let c2 = make_review_comment(200, &findings[1]);

        // a has 😕 (wontfix), b has 🚀
        let reactions = vec![
            (CommentId::new(100), make_reactions(&[("confused", 1)])),
            (CommentId::new(200), make_reactions(&[("rocket", 2)])),
        ];

        let items = build_fix_items_from_review_comments(&[c1, c2], &reactions);
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);

        assert!(deps.deps_met("b", &resolved));
    }

    #[test]
    fn test_circular_deps_detected() {
        let findings = [
            make_finding_with_deps("a", &["b"]),
            make_finding_with_deps("b", &["a"]),
        ];
        let c1 = make_review_comment(100, &findings[0]);
        let c2 = make_review_comment(200, &findings[1]);

        let reactions = vec![
            (CommentId::new(100), make_reactions(&[("rocket", 1)])),
            (CommentId::new(200), make_reactions(&[("rocket", 2)])),
        ];

        let items = build_fix_items_from_review_comments(&[c1, c2], &reactions);
        let deps = FindingDeps::build(&items);
        assert!(deps.in_cycle("a"));
        assert!(deps.in_cycle("b"));
    }

    #[test]
    fn test_dep_chain() {
        let findings = [
            make_finding("a"),
            make_finding_with_deps("b", &["a"]),
            make_finding_with_deps("c", &["b"]),
        ];
        let c1 = make_review_comment(100, &findings[0]);
        let c2 = make_review_comment(200, &findings[1]);
        let c3 = make_review_comment(300, &findings[2]);

        // All queued
        let reactions = vec![
            (CommentId::new(100), make_reactions(&[("rocket", 1)])),
            (CommentId::new(200), make_reactions(&[("rocket", 2)])),
            (CommentId::new(300), make_reactions(&[("rocket", 3)])),
        ];

        let items = build_fix_items_from_review_comments(&[c1, c2, c3], &reactions);
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);

        // Initially: a eligible, b and c blocked
        assert!(deps.deps_met("a", &resolved));
        assert!(!deps.deps_met("b", &resolved));
        assert!(!deps.deps_met("c", &resolved));

        // After a is fixed: b eligible, c still blocked
        let findings2 = [
            make_finding("a"),
            make_finding_with_deps("b", &["a"]),
            make_finding_with_deps("c", &["b"]),
        ];
        let d1 = make_review_comment(100, &findings2[0]);
        let d2 = make_review_comment(200, &findings2[1]);
        let d3 = make_review_comment(300, &findings2[2]);
        let reactions2 = vec![
            (CommentId::new(100), make_reactions(&[("+1", 10)])), // a fixed
            (CommentId::new(200), make_reactions(&[("rocket", 2)])), // b queued
            (CommentId::new(300), make_reactions(&[("rocket", 3)])), // c queued
        ];
        let items2 = build_fix_items_from_review_comments(&[d1, d2, d3], &reactions2);
        let resolved2 = resolved_finding_ids(&items2);

        assert!(deps.deps_met("b", &resolved2));
        assert!(!deps.deps_met("c", &resolved2));
    }

    #[test]
    fn test_unknown_dep_does_not_block() {
        let findings = [make_finding_with_deps("a", &["nonexistent"])];
        let c1 = make_review_comment(100, &findings[0]);

        let reactions = vec![(CommentId::new(100), make_reactions(&[("rocket", 1)]))];

        let items = build_fix_items_from_review_comments(&[c1], &reactions);
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);

        // Unknown deps are ignored → eligible
        assert!(deps.deps_met("a", &resolved));
    }

    // --- Batch preparation tests ---

    /// Helper to build a FixItem directly from a ReviewFinding.
    fn make_fix_item(finding: crate::review_schema::ReviewFinding, comment_id: u64) -> FixItem {
        FixItem {
            finding,
            state: FindingState::Queued,
            comment_id: CommentId::new(comment_id),
            rocket_reaction_ids: vec![ReactionId::new(1)],
        }
    }

    #[test]
    fn test_batch_prep_partial_failure() {
        // "bad finding" contains a space → invalid branch name
        // "good-a" and "good-b" are valid
        let items = vec![
            make_fix_item(make_finding("good-a"), 100),
            make_fix_item(make_finding("bad finding"), 200),
            make_fix_item(make_finding("good-b"), 300),
        ];

        let engine = PromptEngine::new(None);
        let fix_config = crate::config::default_review_step("fix");
        let mut reply_map: ReplyMap = HashMap::new();

        let mut prepared = Vec::new();
        let mut failed = HashSet::new();

        for item in items {
            let finding_id = item.finding.id.clone();
            match prepare_fix_item(
                item,
                PrNumber::new(42),
                &fix_config,
                &engine,
                &mut reply_map,
            ) {
                Some(p) => prepared.push(p),
                None => {
                    failed.insert(finding_id);
                }
            }
        }

        // Two items succeed, one fails
        assert_eq!(prepared.len(), 2);
        assert_eq!(prepared[0].item.finding.id, "good-a");
        assert_eq!(prepared[1].item.finding.id, "good-b");

        assert_eq!(failed.len(), 1);
        assert!(failed.contains("bad finding"));
    }
}
