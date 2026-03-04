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
const MAX_CRITICAL_ATTEMPTS: u8 = 2;

use crate::config::{Config, ReviewStepConfig};
use crate::error::{Error, Result};
use crate::fix_comment::{
    FindingState, FixItem, FixResultKind, REACTION_CONFUSED, REACTION_THUMBS_UP, ReplyMap,
    build_fix_items_from_review_comments, collect_reply_bodies, format_review_context,
};
use crate::fix_deps::{FindingDeps, resolved_finding_ids};
use crate::fix_scheduler::{self, ScheduleAction};
use crate::orchestrator::{CorrectionRunner, retry_with_correction};
use crate::prompts::PromptEngine;
use crate::review_schema::{
    FINDING_MARKER, SchemaName, StandaloneFixOutput, parse_standalone_fix_output,
};
use crate::runner::{AgentRunner, Phase, RunResult, build_runner};
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

    // 1. Fetch review comments and reactions, build fix items
    info!(pr_number, "polling GitHub for PR review comments");
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
            ScheduleAction::RunCritical(id) => vec![id],
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
        info!(pr_number, "all fixes completed successfully");
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
/// Polls for newly 🚀-reacted comments every `poll_seconds`, runs the
/// scheduler each cycle to determine which findings to process next.
/// CRITICAL findings run one at a time in their own agent session.
/// Failed CRITICALs are retried once before being added to the failed set.
///
/// On shutdown signal: completes the current fix (if any), then exits cleanly.
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
    let poll_duration = Duration::from_secs(config.poll_seconds);

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
        info!(
            pr_number,
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
    pr_number: u64,
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
            ScheduleAction::RunCritical(finding_id) => {
                info!(%finding_id, "Critical processing mode: scheduling single-finding fix session");

                let Some(item) = queued_items
                    .iter()
                    .find(|i| i.finding.id == finding_id)
                    .cloned()
                else {
                    warn!(%finding_id, "scheduler returned unknown finding ID, marking as failed");
                    failed.insert(finding_id);
                    continue;
                };

                let Some(prepared) = prepare_fix_item(
                    item,
                    pr_number,
                    &shared.fix_config,
                    prompt_engine,
                    reply_map,
                ) else {
                    failed.insert(finding_id);
                    continue;
                };

                match run_prepared_fix(shared, prepared, pr_number).await {
                    Ok(()) => {
                        info!(%finding_id, "Critical fix completed successfully");
                        completed.insert(finding_id);
                    }
                    Err(e) => {
                        let attempts = retries.entry(finding_id.clone()).or_insert(0);
                        *attempts += 1;
                        if *attempts >= MAX_CRITICAL_ATTEMPTS {
                            warn!(
                                %finding_id,
                                error = %e,
                                "CRITICAL fix failed after retry, adding to failed set"
                            );
                            failed.insert(finding_id);
                        } else {
                            warn!(
                                %finding_id,
                                error = %e,
                                attempt = *attempts,
                                "CRITICAL fix failed, will retry"
                            );
                        }
                    }
                }
            }
            ScheduleAction::RunBatch(_) => {
                warn!("RunBatch not yet implemented, skipping batch findings for now");
                break;
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
    pr_number: u64,
    submission: &(impl SubmissionBackend + ?Sized),
) -> Result<(Vec<FixItem>, Vec<PrReviewComment>)> {
    let comments = submission.fetch_pr_review_comments(pr_number)?;

    // Only fetch reactions for comments that contain the finding marker
    let finding_comments: Vec<_> = comments
        .iter()
        .filter(|c| c.in_reply_to_id.is_none() && c.body.contains(FINDING_MARKER))
        .collect();

    // Fetch reactions in parallel across threads
    let reactions_by_comment: Vec<Result<(u64, Vec<Reaction>)>> = std::thread::scope(|s| {
        let handles: Vec<_> = finding_comments
            .iter()
            .map(|comment| {
                let id = comment.id;
                s.spawn(move || {
                    submission
                        .list_review_comment_reactions(id)
                        .map(|reactions| (id, reactions))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .map_err(|_| Error::Submission("reaction-fetch thread panicked".into()))?
            })
            .collect()
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
    worktree_dir: &str,
    repo_root: &Path,
    submission: &(impl SubmissionBackend + ?Sized),
    correction_runner: &(impl CorrectionRunner + ?Sized),
    setup_script: Option<&Path>,
) -> Result<()> {
    let wm = WorktreeManager::new(
        repo_root.to_path_buf(),
        repo_root.join(worktree_dir),
        ctx.pr_branch.to_string(),
    )
    .with_setup_script(setup_script.map(Path::to_path_buf));
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
    comment_id: u64,
    /// Reply bodies collected from the review thread.
    replies: Vec<String>,
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
    let replies = reply_map.remove(&comment_id).unwrap_or_default();

    Some(PreparedFixItem {
        item,
        fix_branch,
        prompt,
        comment_id,
        replies,
    })
}

/// Build [`FixContext`] and run a single fix.
///
/// Shared by both [`run_fix`] (one-shot) and [`run_fix_loop`] (polling).
async fn run_prepared_fix<S: SubmissionBackend, C: CorrectionRunner>(
    shared: &SharedFixState<S, C>,
    prepared: PreparedFixItem,
    pr_number: u64,
) -> Result<()> {
    let PreparedFixItem {
        item,
        fix_branch,
        mut prompt,
        comment_id,
        replies,
    } = prepared;

    // Re-fetch the comment body at execution time for the freshest content
    match shared.submission.fetch_review_comment_by_id(comment_id) {
        Ok(comment) => {
            prompt.push_str(&format_review_context(&comment.body, &replies));
        }
        Err(e) => {
            warn!(
                comment_id,
                error = %e,
                "failed to re-fetch review comment body, proceeding without review context"
            );
        }
    }

    let ctx = FixContext {
        item,
        pr_number,
        pr_branch: &shared.pr_branch,
        fix_branch: &fix_branch,
        fix_config: &shared.fix_config,
        agent_timeout_retries: shared.agent_timeout_retries,
        prompt: &prompt,
    };
    run_single_fix(
        ctx,
        &shared.worktree_dir,
        &shared.repo_root,
        &*shared.submission,
        &*shared.correction_runner,
        shared.setup_script.as_deref(),
    )
    .await
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

    // Apply result
    let fix_result = match fix_output {
        StandaloneFixOutput::Fixed { commit_message } => {
            info!(finding_id = %ctx.item.finding.id, commit_message, "fix applied — rebasing and pushing");
            push_to_pr_branch_with_retry(worktree_path, ctx.fix_branch, ctx.pr_branch).await?;
            FixResultKind::Fixed { commit_message }
        }
        StandaloneFixOutput::WontFix { reason } => {
            info!(finding_id = %ctx.item.finding.id, reason, "finding marked as won't fix");
            FixResultKind::WontFix { reason }
        }
    };

    // Update reactions and post reply (best-effort — don't fail on already-pushed code)
    update_reactions_and_reply(ctx, submission, &fix_result);

    Ok(())
}

/// Remove 🚀 reactions from a single review comment (best-effort).
fn remove_rocket_reactions(
    finding_id: &str,
    comment_id: u64,
    rocket_reaction_ids: &[u64],
    submission: &(impl SubmissionBackend + ?Sized),
) {
    for reaction_id in rocket_reaction_ids {
        if let Err(e) = submission.delete_review_comment_reaction(comment_id, *reaction_id) {
            warn!(
                finding_id, comment_id, reaction_id,
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
                comment_id = item.comment_id,
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
            %finding_id, comment_id, reaction,
            error = %e,
            "failed to add result reaction"
        );
    }

    // Post reply (best-effort)
    info!(
        pr_number = ctx.pr_number,
        %finding_id,
        comment_id,
        "posting fix reply to review comment"
    );
    if let Err(e) = submission.reply_to_review_comment(ctx.pr_number, comment_id, &reply_body) {
        warn!(
            %finding_id, comment_id,
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
            (100u64, vec![]),
            (200u64, make_reactions(&[("rocket", 1)])),
            (300u64, vec![]),
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
            (100u64, make_reactions(&[("rocket", 1)])),
            (200u64, vec![]),
            (300u64, make_reactions(&[("rocket", 2)])),
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
        let reactions = vec![(100u64, make_reactions(&[("+1", 1)]))];

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
            (100u64, make_reactions(&[("rocket", 1)])),
            (200u64, make_reactions(&[("rocket", 2)])),
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
            (100u64, make_reactions(&[("+1", 1)])),
            (200u64, make_reactions(&[("rocket", 2)])),
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
            (100u64, make_reactions(&[("confused", 1)])),
            (200u64, make_reactions(&[("rocket", 2)])),
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
            (100u64, make_reactions(&[("rocket", 1)])),
            (200u64, make_reactions(&[("rocket", 2)])),
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
            (100u64, make_reactions(&[("rocket", 1)])),
            (200u64, make_reactions(&[("rocket", 2)])),
            (300u64, make_reactions(&[("rocket", 3)])),
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
            (100u64, make_reactions(&[("+1", 10)])),    // a fixed
            (200u64, make_reactions(&[("rocket", 2)])), // b queued
            (300u64, make_reactions(&[("rocket", 3)])), // c queued
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

        let reactions = vec![(100u64, make_reactions(&[("rocket", 1)]))];

        let items = build_fix_items_from_review_comments(&[c1], &reactions);
        let deps = FindingDeps::build(&items);
        let resolved = resolved_finding_ids(&items);

        // Unknown deps are ignored → eligible
        assert!(deps.deps_met("a", &resolved));
    }
}
