use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::fix_comment::{CheckboxState, FixItem, FixResultKind, parse_fix_items, update_comment};
use crate::prompts::PromptEngine;
use crate::review_schema::{
    SchemaName, StandaloneFixOutput, correction_prompt, parse_standalone_fix_output,
};
use crate::runner::{AgentRunner, Phase, RunResult, build_runner, resume_with_correction};
use crate::submission::{REVIEW_MARKER, SubmissionBackend};
use crate::worktree::validate_branch_name;

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
) -> Result<()> {
    // 1. Fetch review comment and parse checked items
    info!(pr_number, "fetching PR comments");
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
    let fix_branch = format!("rlph-fix-{pr_number}-{}", item.finding.id);
    validate_branch_name(&fix_branch)?;

    let worktree_path = create_fix_worktree(repo_root, pr_branch, &fix_branch)?;
    info!(path = %worktree_path.display(), branch = %fix_branch, "created fix worktree");

    // Run the fix agent and handle results, ensuring worktree cleanup
    let ctx = FixContext {
        item,
        pr_number,
        pr_branch,
        fix_branch: &fix_branch,
        worktree_path: &worktree_path,
        comment_body: &review_comment.body,
    };
    let result = run_fix_agent_and_apply(&ctx, config, submission, prompt_engine).await;

    // 8. Clean up worktree (always, even on error)
    info!(path = %worktree_path.display(), "cleaning up fix worktree");
    cleanup_worktree(repo_root, &worktree_path, &fix_branch);

    result
}

/// Context for a single fix agent invocation.
struct FixContext<'a> {
    item: &'a FixItem,
    pr_number: u64,
    pr_branch: &'a str,
    fix_branch: &'a str,
    worktree_path: &'a Path,
    comment_body: &'a str,
}

/// Inner function: spawn agent, parse output, rebase/push, update comment.
#[allow(clippy::too_many_lines)]
async fn run_fix_agent_and_apply(
    ctx: &FixContext<'_>,
    config: &Config,
    submission: &impl SubmissionBackend,
    prompt_engine: &PromptEngine,
) -> Result<()> {
    let fix_config = &config.fix;
    let item = ctx.item;

    // 4. Render prompt and spawn fix agent
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

    let prompt = prompt_engine.render_phase(&fix_config.prompt, &vars)?;

    info!(finding_id = %item.finding.id, "spawning fix agent");
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
    let fix_output = parse_with_retry(&run_result, fix_config, ctx.worktree_path).await?;

    info!(finding_id = %item.finding.id, ?fix_output, "fix agent completed");

    // 6 & 7. Apply result: rebase+push or update comment
    let fix_result = match &fix_output {
        StandaloneFixOutput::Fixed { commit_message } => {
            info!(commit_message, "fix applied — rebasing and pushing");

            // Rebase onto origin/<pr-branch>
            rebase_onto(ctx.worktree_path, ctx.pr_branch)?;

            // Push to PR branch: git push origin <fix-branch>:<pr-branch>
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

    // Update review comment
    let updated_body = update_comment(ctx.comment_body, &item.finding.id, &fix_result);
    info!(ctx.pr_number, finding_id = %item.finding.id, "updating review comment");
    submission.upsert_review_comment(ctx.pr_number, &updated_body)?;

    Ok(())
}

/// Parse fix output with up to 2 retries via session resume.
async fn parse_with_retry(
    run_result: &RunResult,
    fix_config: &crate::config::ReviewStepConfig,
    working_dir: &Path,
) -> Result<StandaloneFixOutput> {
    match parse_standalone_fix_output(&run_result.stdout) {
        Ok(output) => Ok(output),
        Err(initial_err) => {
            let session_id = run_result.session_id.as_deref().ok_or_else(|| {
                Error::Orchestrator(format!(
                    "fix agent JSON parse failed and no session_id for retry: {initial_err}"
                ))
            })?;

            let mut last_error = initial_err.to_string();
            const MAX_RETRIES: u32 = 2;

            for attempt in 1..=MAX_RETRIES {
                let prompt = correction_prompt(SchemaName::StandaloneFix, &last_error);
                info!(
                    session_id,
                    attempt, MAX_RETRIES, "resuming session with correction prompt"
                );

                match resume_with_correction(
                    fix_config.runner,
                    &fix_config.agent_binary,
                    fix_config.agent_model.as_deref(),
                    fix_config.agent_effort.as_deref(),
                    fix_config.agent_variant.as_deref(),
                    session_id,
                    &prompt,
                    working_dir,
                    fix_config.agent_timeout.map(Duration::from_secs),
                )
                .await
                {
                    Ok(corrected) => match parse_standalone_fix_output(&corrected.stdout) {
                        Ok(output) => return Ok(output),
                        Err(e) => {
                            last_error = e.to_string();
                            warn!(attempt, error = %last_error, "correction output still invalid");
                        }
                    },
                    Err(e) => {
                        return Err(Error::Orchestrator(format!(
                            "fix agent correction resume failed: {e}"
                        )));
                    }
                }
            }

            Err(Error::Orchestrator(format!(
                "fix agent JSON correction exhausted after {MAX_RETRIES} retries: {last_error}"
            )))
        }
    }
}

/// Create a worktree for the fix branch, branching off origin/<pr-branch>.
fn create_fix_worktree(repo_root: &Path, pr_branch: &str, fix_branch: &str) -> Result<PathBuf> {
    // Fetch latest PR branch
    info!(pr_branch, "fetching latest PR branch from origin");
    git(repo_root, &["fetch", "origin", pr_branch])?;

    let remote_ref = format!("origin/{pr_branch}");

    // Determine worktree path
    let worktree_dir = repo_root.join("..").join("rlph-worktrees");
    std::fs::create_dir_all(&worktree_dir).map_err(|e| {
        Error::Worktree(format!(
            "failed to create worktree dir {}: {e}",
            worktree_dir.display()
        ))
    })?;
    let worktree_path = worktree_dir.join(fix_branch);

    // Clean up if path already exists (stale worktree)
    if worktree_path.exists() {
        debug!(path = %worktree_path.display(), "removing stale fix worktree");
        let _ = git(
            repo_root,
            &[
                "worktree",
                "remove",
                "--force",
                &worktree_path.to_string_lossy(),
            ],
        );
    }

    // Delete stale local branch if it exists
    let local_ref = format!("refs/heads/{fix_branch}");
    if git(repo_root, &["show-ref", "--verify", "--quiet", &local_ref]).is_ok() {
        let _ = git(repo_root, &["branch", "-D", fix_branch]);
    }

    // Create worktree with new branch from remote ref
    let path_str = worktree_path.to_string_lossy();
    git(
        repo_root,
        &["worktree", "add", "-b", fix_branch, &path_str, &remote_ref],
    )?;

    let canonical = worktree_path.canonicalize().unwrap_or(worktree_path);
    Ok(canonical)
}

/// Rebase current branch onto origin/<pr-branch>.
fn rebase_onto(worktree_path: &Path, pr_branch: &str) -> Result<()> {
    let remote_ref = format!("origin/{pr_branch}");

    // Fetch latest
    git(worktree_path, &["fetch", "origin", pr_branch])?;

    let output = Command::new("git")
        .args(["rebase", &remote_ref])
        .current_dir(worktree_path)
        .output()
        .map_err(|e| Error::Orchestrator(format!("failed to run git rebase: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Abort the rebase on failure
        let _ = Command::new("git")
            .args(["rebase", "--abort"])
            .current_dir(worktree_path)
            .output();
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
    let output = Command::new("git")
        .args(["push", "origin", &refspec])
        .current_dir(worktree_path)
        .output()
        .map_err(|e| Error::Orchestrator(format!("failed to run git push: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Orchestrator(format!(
            "git push origin {refspec} failed: {stderr}"
        )));
    }

    info!(refspec, "pushed fix to PR branch");
    Ok(())
}

/// Clean up worktree and its branch.
fn cleanup_worktree(repo_root: &Path, worktree_path: &Path, fix_branch: &str) {
    let path_str = worktree_path.to_string_lossy();
    if let Err(e) = git(repo_root, &["worktree", "remove", "--force", &path_str]) {
        warn!(error = %e, "failed to remove fix worktree");
    }
    if let Err(e) = git(repo_root, &["branch", "-D", fix_branch]) {
        debug!(error = %e, "failed to delete fix branch (may already be gone)");
    }
}

/// Run a git command and return stdout on success, or an error message on failure.
fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| Error::Orchestrator(format!("failed to run git {}: {e}", args.join(" "))))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(Error::Orchestrator(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim()
        )))
    }
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
        let branch = format!("rlph-fix-{}-{}", 42, "sql-injection");
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

        assert!(updated.contains("\u{2010}"));
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
