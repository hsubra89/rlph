#![allow(dead_code)]

use std::path::Path;
use std::process::Command;

use rlph::config::{Config, default_review_phases, default_review_step};
use rlph::runner::RunnerKind;

pub fn run_git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} in {} failed: {}",
        args,
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Create a bare remote + working repo with an initial commit pushed to main.
pub fn setup_git_repo() -> (tempfile::TempDir, tempfile::TempDir) {
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

    (bare_dir, repo_dir)
}

/// Sensible default `Config` for tests. Callers can override fields via struct update syntax.
pub fn default_test_config() -> Config {
    Config {
        source: "github".to_string(),
        runner: RunnerKind::Claude,
        submission: "github".to_string(),
        label: "rlph".to_string(),
        poll_seconds: 30,
        worktree_dir: String::new(),
        base_branch: "main".to_string(),
        max_iterations: None,
        dry_run: false,
        once: true,
        continuous: false,
        agent_binary: "claude".to_string(),
        agent_model: None,
        agent_timeout: None,
        implement_timeout: None,
        agent_effort: None,
        agent_variant: None,
        max_review_rounds: 3,
        agent_timeout_retries: 2,
        review_phases: default_review_phases(),
        review_aggregate: default_review_step("review-aggregate"),
        review_fix: default_review_step("review-fix"),
        fix: default_review_step("fix"),
        linear: None,
    }
}
