use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use tokio::sync::Semaphore;
use tracing::{info, warn};

/// Serializes comment re-fetch + update to prevent concurrent fix agents from racing.
static COMMENT_UPDATE_LOCK: Semaphore = Semaphore::const_new(1);

use crate::config::{Config, ReviewStepConfig};
use crate::error::{Error, Result};
use crate::fix_comment::{CheckboxState, FixItem, FixResultKind, parse_fix_items, update_comment};
use crate::orchestrator::{CorrectionRunner, retry_with_correction};
use crate::prompts::PromptEngine;
use crate::review_schema::{SchemaName, StandaloneFixOutput, parse_standalone_fix_output};
use crate::runner::{AgentRunner, Phase, RunResult, build_runner};
use crate::submission::{REVIEW_MARKER, SubmissionBackend};
use crate::worktree::{WorktreeManager, git_in_dir, validate_branch_name};

/// Run the standalone fix flow for a single checked finding on a PR.
///
/// Steps:
/// 1. Fetch review comment, parse checked items
/// 2. Take the first eligible checked item
/// 3. Create worktree off `origin/<pr-branch>`
/// 4. Spawn fix agent with finding context
/// 5. Parse FixOutput JSON (with retry)
/// 6. If fixed: rebase onto `origin/<pr-branch>`, push to PR branch
/// 7. Update review comment checkbox with result
/// 8. Clean up worktree
pub async fn run_fix(
    pr_number: u64,
    pr_branch: &str,
    config: &Config,
    submission: &impl SubmissionBackend,
    prompt_engine: &PromptEngine,
    repo_root: &Path,
    correction_runner: &(impl CorrectionRunner + ?Sized),
) -> Result<()> {
    // Validate pr_branch from GitHub API at the trust boundary
    validate_branch_name(pr_branch)?;

    // 1. Fetch review comment and parse checked items
    info!(pr_number, "polling GitHub for PR comments");
    let comments = submission.fetch_pr_comments(pr_number)?;
    let review_comment = comments
        .iter()
        .find(|c| c.body.contains(REVIEW_MARKER))
        .ok_or_else(|| {
            Error::Orchestrator(format!("no rlph review comment found on PR #{pr_number}"))
        })?;

    let items = parse_fix_items(&review_comment.body);
    info!(total = items.len(), "parsed fix items from review comment");

    // 2. Take the first eligible checked item
    let eligible = items
        .iter()
        .find(|item| item.state == CheckboxState::Checked);
    let Some(item) = eligible else {
        info!("no checked items found — nothing to fix");
        return Ok(());
    };

    info!(
        finding_id = %item.finding.id,
        file = %item.finding.file,
        line = item.finding.line,
        severity = %item.finding.severity.label(),
        "selected finding for fix"
    );

    // 3. Create worktree off origin/<pr-branch>
    let id = &item.finding.id;
    let fix_branch = format!("rlph-fix-{pr_number}-{id}");
    validate_branch_name(&fix_branch)?;

    let wm = WorktreeManager::new(
        repo_root.to_path_buf(),
        repo_root.join(&config.worktree_dir),
        pr_branch.to_string(),
    );
    let worktree_path = wm.create_fresh(&fix_branch, pr_branch)?.path;
    info!(path = %worktree_path.display(), branch = %fix_branch, "created fix worktree");

    // Run the fix agent and handle results, ensuring worktree cleanup
    let ctx = FixContext {
        item,
        pr_number,
        pr_branch,
        fix_branch: &fix_branch,
        worktree_path: &worktree_path,
    };
    let result =
        run_fix_agent_and_apply(&ctx, config, submission, prompt_engine, correction_runner).await;

    // 8. Clean up worktree (always, even on error)
    info!(path = %worktree_path.display(), "cleaning up fix worktree");
    if let Err(e) = wm.remove(&worktree_path) {
        warn!(error = %e, "failed to clean up fix worktree");
    }

    result
}

/// Context for a single fix agent invocation.
struct FixContext<'a> {
    item: &'a FixItem,
    pr_number: u64,
    pr_branch: &'a str,
    fix_branch: &'a str,
    worktree_path: &'a Path,
}

/// Build template variables from a fix item's finding.
fn build_finding_vars(item: &FixItem) -> HashMap<String, String> {
    let mut vars = HashMap::new();
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
        if item.finding.depends_on.is_empty() {
            String::new()
        } else {
            item.finding.depends_on.join(", ")
        },
    );
    vars
}

/// Inner function: spawn agent, parse output, rebase/push, update comment.
async fn run_fix_agent_and_apply(
    ctx: &FixContext<'_>,
    config: &Config,
    submission: &impl SubmissionBackend,
    prompt_engine: &PromptEngine,
    correction_runner: &(impl CorrectionRunner + ?Sized),
) -> Result<()> {
    let fix_config = &config.fix;

    // 4. Render prompt and spawn fix agent
    let vars = build_finding_vars(ctx.item);
    let prompt = prompt_engine.render_phase(&fix_config.prompt, &vars)?;

    info!(finding_id = %ctx.item.finding.id, "spawning fix agent");
    let runner = build_runner(
        fix_config.runner,
        &fix_config.agent_binary,
        fix_config.agent_model.as_deref(),
        fix_config.agent_effort.as_deref(),
        fix_config.agent_variant.as_deref(),
        fix_config.agent_timeout.map(Duration::from_secs),
        config.agent_timeout_retries,
    )
    .with_stream_prefix("fix".to_string());

    let run_result = runner.run(Phase::Fix, &prompt, ctx.worktree_path).await?;

    // 5. Parse FixOutput JSON (with retry on failure)
    let fix_output = parse_fix_with_retry(
        &run_result,
        fix_config,
        ctx.worktree_path,
        correction_runner,
    )
    .await?;

    info!(finding_id = %ctx.item.finding.id, ?fix_output, "fix agent completed");

    // 6 & 7. Apply result and update comment
    apply_fix_and_update_comment(ctx, &fix_output, submission).await
}

/// Apply fix result (rebase+push if fixed) and update the review comment.
async fn apply_fix_and_update_comment(
    ctx: &FixContext<'_>,
    fix_output: &StandaloneFixOutput,
    submission: &impl SubmissionBackend,
) -> Result<()> {
    let fix_result = match fix_output {
        StandaloneFixOutput::Fixed { commit_message } => {
            info!(commit_message, "fix applied — rebasing and pushing");
            rebase_onto(ctx.worktree_path, ctx.pr_branch)?;
            push_to_pr_branch(ctx.worktree_path, ctx.fix_branch, ctx.pr_branch)?;
            FixResultKind::Fixed {
                commit_message: commit_message.clone(),
            }
        }
        StandaloneFixOutput::WontFix { reason } => {
            info!(reason, "finding marked as won't fix");
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

    info!(ctx.pr_number, finding_id = %ctx.item.finding.id, "polling GitHub to re-fetch review comment");
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
    info!(ctx.pr_number, finding_id = %ctx.item.finding.id, "updating review comment");
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

/// Run a git command in the given directory, mapping failures to [`Error::Orchestrator`].
fn run_git(cwd: &Path, args: &[&str], label: &str) -> Result<String> {
    git_in_dir(cwd, args)
        .map_err(|stderr| Error::Orchestrator(format!("{label} failed: {stderr}")))
}

/// Rebase current branch onto origin/<pr-branch>.
fn rebase_onto(worktree_path: &Path, pr_branch: &str) -> Result<()> {
    run_git(worktree_path, &["fetch", "origin", pr_branch], &format!("git fetch origin {pr_branch}"))?;

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

/// Push fix branch to PR branch: `git push origin <fix-branch>:<pr-branch>`.
fn push_to_pr_branch(worktree_path: &Path, fix_branch: &str, pr_branch: &str) -> Result<()> {
    let refspec = format!("{fix_branch}:{pr_branch}");
    run_git(worktree_path, &["push", "origin", &refspec], &format!("git push origin {refspec}"))?;
    info!(refspec, "pushed fix to PR branch");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_schema::{ReviewFinding, Severity, render_findings_for_github};

    fn make_finding(id: &str) -> ReviewFinding {
        ReviewFinding {
            id: id.to_string(),
            file: "src/main.rs".to_string(),
            line: 42,
            severity: Severity::Critical,
            description: format!("{id} description"),
            category: Some("correctness".to_string()),
            depends_on: vec![],
        }
    }

    #[test]
    fn test_fix_branch_name_is_valid() {
        let branch = "rlph-fix-42-sql-injection";
        assert!(validate_branch_name(&branch).is_ok());
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
        let eligible = items
            .iter()
            .find(|item| item.state == CheckboxState::Checked);

        assert!(eligible.is_some());
        assert_eq!(eligible.unwrap().finding.id, "b");
    }

    #[test]
    fn test_no_eligible_items() {
        let findings = vec![make_finding("a")];
        let comment = render_findings_for_github(&findings, "Summary.");

        let items = parse_fix_items(&comment);
        let eligible = items
            .iter()
            .find(|item| item.state == CheckboxState::Checked);

        assert!(eligible.is_none());
    }

    #[test]
    fn test_already_fixed_items_not_eligible() {
        let findings = vec![make_finding("a")];
        let comment = render_findings_for_github(&findings, "Summary.");
        let comment = comment.replace("- [ ] ", "- ✅ ");

        let items = parse_fix_items(&comment);
        let eligible = items
            .iter()
            .find(|item| item.state == CheckboxState::Checked);

        assert!(eligible.is_none());
    }
}
