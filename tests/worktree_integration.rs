use std::path::Path;
use std::process::Command;

use rlph::worktree::WorktreeManager;
use tempfile::TempDir;

/// Create a temporary git repo with an initial commit.
fn init_temp_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    let path = dir.path();

    let run = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };

    run(&["init"]);
    run(&["config", "user.email", "test@test.com"]);
    run(&["config", "user.name", "Test"]);

    // Create an initial commit so HEAD exists
    std::fs::write(path.join("README.md"), "# test").unwrap();
    run(&["add", "."]);
    run(&["commit", "-m", "init"]);
    run(&["branch", "-M", "main"]);

    // Add self as origin so `git fetch origin main` works in tests
    let path_str = path.to_str().unwrap();
    run(&["remote", "add", "origin", path_str]);

    dir
}

fn run_git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn create_remote_branch(repo: &TempDir, branch: &str) {
    run_git(repo.path(), &["checkout", "-b", branch]);
    std::fs::write(repo.path().join("branch.txt"), branch).unwrap();
    run_git(repo.path(), &["add", "."]);
    run_git(repo.path(), &["commit", "-m", "branch commit"]);
    run_git(repo.path(), &["push", "-u", "origin", branch]);
    run_git(repo.path(), &["checkout", "main"]);
}

#[test]
fn test_create_worktree() {
    let repo = init_temp_repo();
    let wt_base = TempDir::new().unwrap();

    let mgr = WorktreeManager::new(
        repo.path().to_path_buf(),
        wt_base.path().to_path_buf(),
        "main".to_string(),
    );

    let info = mgr.create(42, "fix-bug").unwrap();
    assert_eq!(info.branch, "rlph-42-fix-bug");
    assert!(info.path.exists());
    assert!(info.path.join("README.md").exists());
}

#[test]
fn test_create_worktree_correct_naming() {
    let repo = init_temp_repo();
    let wt_base = TempDir::new().unwrap();

    let mgr = WorktreeManager::new(
        repo.path().to_path_buf(),
        wt_base.path().to_path_buf(),
        "main".to_string(),
    );

    let info = mgr.create(7, "add-auth").unwrap();
    assert!(info.path.ends_with("rlph-7-add-auth"));
}

#[test]
fn test_find_existing_worktree() {
    let repo = init_temp_repo();
    let wt_base = TempDir::new().unwrap();

    let mgr = WorktreeManager::new(
        repo.path().to_path_buf(),
        wt_base.path().to_path_buf(),
        "main".to_string(),
    );

    // Create worktree
    let created = mgr.create(10, "feature").unwrap();

    // Should find it
    let found = mgr.find_existing(10).unwrap();
    assert!(found.is_some());
    let found = found.unwrap();
    assert_eq!(found.path, created.path);
    assert_eq!(found.branch, "rlph-10-feature");
}

#[test]
fn test_reuse_existing_worktree() {
    let repo = init_temp_repo();
    let wt_base = TempDir::new().unwrap();

    let mgr = WorktreeManager::new(
        repo.path().to_path_buf(),
        wt_base.path().to_path_buf(),
        "main".to_string(),
    );

    let first = mgr.create(10, "feature").unwrap();
    let second = mgr.create(10, "feature").unwrap();

    // Should return the same path (reuse, not duplicate)
    assert_eq!(first.path, second.path);
}

#[test]
fn test_find_nonexistent_worktree() {
    let repo = init_temp_repo();
    let wt_base = TempDir::new().unwrap();

    let mgr = WorktreeManager::new(
        repo.path().to_path_buf(),
        wt_base.path().to_path_buf(),
        "main".to_string(),
    );

    let found = mgr.find_existing(999).unwrap();
    assert!(found.is_none());
}

#[test]
fn test_remove_worktree() {
    let repo = init_temp_repo();
    let wt_base = TempDir::new().unwrap();

    let mgr = WorktreeManager::new(
        repo.path().to_path_buf(),
        wt_base.path().to_path_buf(),
        "main".to_string(),
    );

    let info = mgr.create(15, "cleanup-test").unwrap();
    assert!(info.path.exists());

    mgr.remove(&info.path).unwrap();

    // Worktree directory should be gone
    assert!(!info.path.exists());

    // Should not find it anymore
    let found = mgr.find_existing(15).unwrap();
    assert!(found.is_none());
}

#[test]
fn test_remove_worktree_cleans_branch() {
    let repo = init_temp_repo();
    let wt_base = TempDir::new().unwrap();

    let mgr = WorktreeManager::new(
        repo.path().to_path_buf(),
        wt_base.path().to_path_buf(),
        "main".to_string(),
    );

    let info = mgr.create(20, "branch-cleanup").unwrap();
    mgr.remove(&info.path).unwrap();

    // Branch should be deleted
    let output = Command::new("git")
        .args(["branch", "--list", "rlph-20-branch-cleanup"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    let branches = String::from_utf8_lossy(&output.stdout);
    assert!(
        branches.trim().is_empty(),
        "branch should be deleted, got: {branches}"
    );
}

#[test]
fn test_create_multiple_worktrees() {
    let repo = init_temp_repo();
    let wt_base = TempDir::new().unwrap();

    let mgr = WorktreeManager::new(
        repo.path().to_path_buf(),
        wt_base.path().to_path_buf(),
        "main".to_string(),
    );

    let wt1 = mgr.create(1, "first").unwrap();
    let wt2 = mgr.create(2, "second").unwrap();

    assert_ne!(wt1.path, wt2.path);
    assert!(wt1.path.exists());
    assert!(wt2.path.exists());

    // Finding each should work independently
    assert!(mgr.find_existing(1).unwrap().is_some());
    assert!(mgr.find_existing(2).unwrap().is_some());
}

#[test]
fn test_create_for_branch() {
    let repo = init_temp_repo();
    let wt_base = TempDir::new().unwrap();
    create_remote_branch(&repo, "feature/review-pr");

    let mgr = WorktreeManager::new(
        repo.path().to_path_buf(),
        wt_base.path().to_path_buf(),
        "main".to_string(),
    );

    let info = mgr.create_for_branch(77, "feature/review-pr").unwrap();
    assert_eq!(info.branch, "feature/review-pr");
    assert!(info.path.exists());
    assert!(info.path.ends_with("rlph-pr-77-feature-review-pr"));
    assert!(info.path.join("branch.txt").exists());
}

#[test]
fn test_create_for_branch_reuses_existing() {
    let repo = init_temp_repo();
    let wt_base = TempDir::new().unwrap();
    create_remote_branch(&repo, "feature/reuse-pr");

    let mgr = WorktreeManager::new(
        repo.path().to_path_buf(),
        wt_base.path().to_path_buf(),
        "main".to_string(),
    );

    let first = mgr.create_for_branch(88, "feature/reuse-pr").unwrap();
    let second = mgr.create_for_branch(88, "feature/reuse-pr").unwrap();

    assert_eq!(first.path, second.path);
    assert_eq!(first.branch, second.branch);
}
