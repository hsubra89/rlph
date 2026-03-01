use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::{debug, info, warn};

use crate::error::{Error, Result};

/// Validate that a branch name is safe: matches `^[a-zA-Z0-9/_.-]+$` and does not start with `refs/`.
pub fn validate_branch_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::Worktree("branch name must not be empty".to_string()));
    }
    if name.starts_with("refs/") {
        return Err(Error::Worktree(format!(
            "branch name must not start with 'refs/': {name}"
        )));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '_' || c == '.' || c == '-')
    {
        return Err(Error::Worktree(format!(
            "branch name contains invalid characters (allowed: a-zA-Z0-9/_.-): {name}"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: String,
}

/// Manages git worktrees for isolated task implementation.
pub struct WorktreeManager {
    repo_root: PathBuf,
    base_dir: PathBuf,
    base_branch: String,
}

impl WorktreeManager {
    pub fn new(repo_root: PathBuf, base_dir: PathBuf, base_branch: String) -> Self {
        Self {
            repo_root,
            base_dir,
            base_branch,
        }
    }

    /// Generate the worktree directory name: `rlph-{issue_number}-{slug}`.
    pub fn worktree_name(issue_number: u64, slug: &str) -> String {
        format!("rlph-{issue_number}-{slug}")
    }

    /// Create a URL/title-safe slug from a string.
    pub fn slugify(title: &str) -> String {
        let slug: String = title
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();

        // Collapse consecutive hyphens and trim
        let mut result = String::new();
        let mut prev_hyphen = false;
        for c in slug.chars() {
            if c == '-' {
                if !prev_hyphen && !result.is_empty() {
                    result.push('-');
                }
                prev_hyphen = true;
            } else {
                result.push(c);
                prev_hyphen = false;
            }
        }

        // Trim trailing hyphen
        if result.ends_with('-') {
            result.pop();
        }

        // Limit length
        if result.len() > 50 {
            result.truncate(50);
            if result.ends_with('-') {
                result.pop();
            }
        }

        result
    }

    /// Create a worktree for an issue. Reuses existing worktrees.
    pub fn create(&self, issue_number: u64, slug: &str) -> Result<WorktreeInfo> {
        // Check for existing worktree
        if let Some(existing) = self.find_existing(issue_number)? {
            info!(
                issue = issue_number,
                path = %existing.path.display(),
                "reusing existing worktree"
            );
            return Ok(existing);
        }

        let name = Self::worktree_name(issue_number, slug);
        let path = self.base_dir.join(&name);
        let branch = name.clone();

        // Ensure base directory exists
        std::fs::create_dir_all(&self.base_dir).map_err(|e| {
            Error::Worktree(format!(
                "failed to create base dir {}: {e}",
                self.base_dir.display()
            ))
        })?;

        // Fetch latest base branch from origin (mandatory, with retries)
        self.fetch_with_retry(&self.base_branch, 3)?;

        // Start point is always origin/<base> since fetch above succeeded
        let start_point = format!("origin/{}", self.base_branch);

        // Try creating with a new branch from main
        let create_result = match self.git_worktree_add(&path, &branch, true, Some(&start_point)) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Branch might already exist — try checking out existing branch
                if e.to_string().contains("already exists") {
                    self.git_worktree_add(&path, &branch, false, None)
                } else {
                    Err(e)
                }
            }
        };

        create_result?;

        // Canonicalize to resolve symlinks (e.g. /var -> /private/var on macOS)
        let canonical_path = path.canonicalize().unwrap_or(path);

        // Log resolved commit SHA (uses Command directly because self.git() runs in repo_root)
        let commit_sha = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&canonical_path)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        info!(
            issue = issue_number,
            path = %canonical_path.display(),
            branch = %branch,
            commit = %commit_sha,
            "created worktree from origin/{}",
            self.base_branch
        );
        Ok(WorktreeInfo {
            path: canonical_path,
            branch,
        })
    }

    /// Create a worktree for a PR review against an existing branch.
    /// Reuses an existing dedicated PR worktree when present.
    pub fn create_for_branch(&self, pr_number: u64, branch: &str) -> Result<WorktreeInfo> {
        validate_branch_name(branch)?;

        let slug = {
            let s = Self::slugify(branch);
            if s.is_empty() {
                "branch".to_string()
            } else {
                s
            }
        };
        let name = format!("rlph-pr-{pr_number}-{slug}");
        let local_branch = name.clone();

        if let Some(existing) = self.find_existing_by_name(&name)? {
            info!(
                pr = pr_number,
                branch,
                path = %existing.path.display(),
                "reusing existing PR review worktree, updating to latest"
            );

            // Fetch latest from origin so we don't review stale code
            self.fetch_with_retry(branch, 3)?;

            // Reset the worktree to the latest remote HEAD
            let remote_ref = format!("origin/{branch}");
            let reset_output = Command::new("git")
                .args(["reset", "--hard", &remote_ref])
                .current_dir(&existing.path)
                .output()
                .map_err(|e| {
                    Error::Worktree(format!("failed to reset worktree to {remote_ref}: {e}"))
                })?;
            if !reset_output.status.success() {
                let stderr = String::from_utf8_lossy(&reset_output.stderr);
                return Err(Error::Worktree(format!(
                    "failed to reset worktree to {remote_ref}: {stderr}"
                )));
            }

            return Ok(existing);
        }

        let path = self.base_dir.join(&name);

        std::fs::create_dir_all(&self.base_dir).map_err(|e| {
            Error::Worktree(format!(
                "failed to create base dir {}: {e}",
                self.base_dir.display()
            ))
        })?;

        // Fetch latest branch from origin (mandatory, with retries)
        self.fetch_with_retry(branch, 3)?;

        let remote_ref = format!("origin/{branch}");
        let local_ref = format!("refs/heads/{local_branch}");
        let local_branch_exists = self
            .git(&["show-ref", "--verify", "--quiet", &local_ref])
            .is_ok();
        if local_branch_exists {
            self.git(&["branch", "-f", &local_branch, &remote_ref])
                .map_err(|e| {
                    Error::Worktree(format!(
                        "failed to fast-forward local branch '{local_branch}' to {remote_ref}: {e}"
                    ))
                })?;
        }

        let create_result = if local_branch_exists {
            self.git_worktree_add(&path, &local_branch, false, None)
        } else {
            self.git_worktree_add(&path, &local_branch, true, Some(&remote_ref))
        };

        create_result?;

        let canonical_path = path.canonicalize().unwrap_or(path);
        let commit_sha = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&canonical_path)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        info!(
            pr = pr_number,
            path = %canonical_path.display(),
            branch,
            commit = %commit_sha,
            "created PR review worktree"
        );
        Ok(WorktreeInfo {
            path: canonical_path,
            branch: local_branch,
        })
    }

    /// Create a fresh worktree with a new branch from a remote branch.
    ///
    /// Removes any stale worktree or local branch with the same name first.
    pub fn create_fresh(&self, branch_name: &str, remote_branch: &str) -> Result<WorktreeInfo> {
        validate_branch_name(branch_name)?;

        // Fetch latest remote branch
        self.fetch_with_retry(remote_branch, 3)?;

        std::fs::create_dir_all(&self.base_dir).map_err(|e| {
            Error::Worktree(format!(
                "failed to create base dir {}: {e}",
                self.base_dir.display()
            ))
        })?;

        let path = self.base_dir.join(branch_name);

        // Clean up stale worktree at path if it exists
        if path.exists() {
            debug!(path = %path.display(), "removing stale worktree");
            let _ = self.git(&["worktree", "remove", "--force", &path.to_string_lossy()]);
        }

        // Delete stale local branch if it exists
        let local_ref = format!("refs/heads/{branch_name}");
        if self
            .git(&["show-ref", "--verify", "--quiet", &local_ref])
            .is_ok()
        {
            let _ = self.git(&["branch", "-D", branch_name]);
        }

        // Create worktree with new branch from remote ref
        let remote_ref = format!("origin/{remote_branch}");
        self.git_worktree_add(&path, branch_name, true, Some(&remote_ref))?;

        let canonical = path.canonicalize().unwrap_or(path);
        Ok(WorktreeInfo {
            path: canonical,
            branch: branch_name.to_string(),
        })
    }

    /// Remove a worktree and delete its branch.
    pub fn remove(&self, worktree_path: &Path) -> Result<()> {
        // Canonicalize to match git's output paths
        let worktree_path = &worktree_path
            .canonicalize()
            .unwrap_or(worktree_path.to_path_buf());

        // Extract branch name before removing
        let branch = self.branch_for_worktree(worktree_path);

        // Prune stale worktrees first
        let _ = self.git(&["worktree", "prune"]);

        // Remove the worktree
        let path_str = worktree_path.to_string_lossy();
        self.git(&["worktree", "remove", "--force", &path_str])
            .map_err(|e| {
                Error::Worktree(format!(
                    "failed to remove worktree {}: {e}",
                    worktree_path.display()
                ))
            })?;

        info!(path = %worktree_path.display(), "removed worktree");

        // Clean up the branch
        if let Some(branch) = branch {
            if !branch.starts_with("rlph-") {
                info!(
                    branch = %branch,
                    "skipping deletion for non-rlph branch after worktree removal"
                );
                return Ok(());
            }
            match self.git(&["branch", "-D", &branch]) {
                Ok(_) => info!(branch = %branch, "deleted branch"),
                Err(e) => warn!(branch = %branch, error = %e, "failed to delete branch"),
            }
        }

        Ok(())
    }

    /// Parse `git worktree list --porcelain` output, returning the first entry
    /// whose directory name satisfies `predicate`.
    fn find_worktree(&self, predicate: impl Fn(&str) -> bool) -> Result<Option<WorktreeInfo>> {
        let _ = self.git(&["worktree", "prune"]);
        let output = self
            .git(&["worktree", "list", "--porcelain"])
            .map_err(|e| Error::Worktree(format!("failed to list worktrees: {e}")))?;

        let mut current_path: Option<PathBuf> = None;
        let mut current_branch: Option<String> = None;

        for line in output.lines() {
            if let Some(path_str) = line.strip_prefix("worktree ") {
                if let Some(ref path) = current_path
                    && let Some(name) = path.file_name().and_then(|n| n.to_str())
                    && predicate(name)
                {
                    return Ok(Some(WorktreeInfo {
                        path: path.clone(),
                        branch: current_branch.unwrap_or_else(|| name.to_string()),
                    }));
                }
                current_path = Some(PathBuf::from(path_str));
                current_branch = None;
            } else if let Some(branch_ref) = line.strip_prefix("branch ") {
                current_branch = branch_ref
                    .strip_prefix("refs/heads/")
                    .map(|b| b.to_string());
            }
        }

        // Check last entry
        if let Some(ref path) = current_path
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
            && predicate(name)
        {
            return Ok(Some(WorktreeInfo {
                path: path.clone(),
                branch: current_branch.unwrap_or_else(|| name.to_string()),
            }));
        }

        Ok(None)
    }

    /// Find an existing worktree for an issue number.
    pub fn find_existing(&self, issue_number: u64) -> Result<Option<WorktreeInfo>> {
        let prefix = format!("rlph-{issue_number}-");
        self.find_worktree(|name| name.starts_with(&prefix))
    }

    fn find_existing_by_name(&self, name: &str) -> Result<Option<WorktreeInfo>> {
        self.find_worktree(|n| n == name)
    }

    /// Run `git worktree add`. If `new_branch` is true, uses `-b` to create the branch.
    /// `start_point` specifies the commit/ref to branch from (only used with new_branch).
    fn git_worktree_add(
        &self,
        path: &Path,
        branch: &str,
        new_branch: bool,
        start_point: Option<&str>,
    ) -> Result<()> {
        let path_str = path.to_string_lossy();
        let mut args = vec!["worktree", "add"];
        if new_branch {
            args.extend_from_slice(&["-b", branch, &path_str]);
            if let Some(sp) = start_point {
                args.push(sp);
            }
        } else {
            args.extend_from_slice(&[&path_str, branch]);
        }

        self.git(&args).map_err(|e| {
            Error::Worktree(format!(
                "git worktree add failed for {}: {e}",
                path.display()
            ))
        })?;

        Ok(())
    }

    /// Get the branch name for a worktree path by checking git worktree list.
    fn branch_for_worktree(&self, worktree_path: &Path) -> Option<String> {
        let output = self.git(&["worktree", "list", "--porcelain"]).ok()?;
        let target = worktree_path.to_string_lossy();

        let mut found = false;
        for line in output.lines() {
            if let Some(path_str) = line.strip_prefix("worktree ") {
                found = path_str == target.as_ref();
            } else if found && let Some(branch_ref) = line.strip_prefix("branch ") {
                return branch_ref
                    .strip_prefix("refs/heads/")
                    .map(|b| b.to_string());
            }
        }
        None
    }

    /// Fetch a ref from origin with retries. Returns an error if all attempts fail.
    fn fetch_with_retry(&self, refspec: &str, max_attempts: u32) -> Result<()> {
        let mut last_err = String::new();
        for attempt in 1..=max_attempts {
            match self.git(&["fetch", "origin", refspec]) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    warn!(
                        attempt,
                        max_attempts,
                        error = %e.trim(),
                        "git fetch origin {} failed",
                        refspec
                    );
                    last_err = e;
                    if attempt < max_attempts {
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    }
                }
            }
        }
        Err(Error::Worktree(format!(
            "failed to fetch origin/{} after {max_attempts} attempts: {}",
            refspec,
            last_err.trim()
        )))
    }

    /// Run a git command in the repo root.
    fn git(&self, args: &[&str]) -> std::result::Result<String, String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.repo_root)
            .output()
            .map_err(|e| format!("failed to run git: {e}"))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worktree_name() {
        assert_eq!(
            WorktreeManager::worktree_name(5, "worktree-management"),
            "rlph-5-worktree-management"
        );
        assert_eq!(
            WorktreeManager::worktree_name(42, "fix-bug"),
            "rlph-42-fix-bug"
        );
    }

    #[test]
    fn test_slugify_basic() {
        assert_eq!(WorktreeManager::slugify("Fix the bug"), "fix-the-bug");
    }

    #[test]
    fn test_slugify_special_chars() {
        assert_eq!(
            WorktreeManager::slugify("Add feature: OAuth 2.0!"),
            "add-feature-oauth-2-0"
        );
    }

    #[test]
    fn test_slugify_consecutive_special() {
        assert_eq!(WorktreeManager::slugify("foo---bar___baz"), "foo-bar-baz");
    }

    #[test]
    fn test_slugify_leading_trailing() {
        assert_eq!(WorktreeManager::slugify("---hello---"), "hello");
    }

    #[test]
    fn test_slugify_long_title() {
        let long_title = "a".repeat(100);
        let slug = WorktreeManager::slugify(&long_title);
        assert!(slug.len() <= 50);
    }

    #[test]
    fn test_slugify_empty() {
        assert_eq!(WorktreeManager::slugify(""), "");
    }

    #[test]
    fn test_slugify_numbers_only() {
        assert_eq!(WorktreeManager::slugify("123"), "123");
    }

    #[test]
    fn test_validate_branch_name_valid() {
        assert!(validate_branch_name("main").is_ok());
        assert!(validate_branch_name("feature/foo-bar").is_ok());
        assert!(validate_branch_name("rlph-pr-56-some.branch_name").is_ok());
        assert!(validate_branch_name("v1.2.3").is_ok());
    }

    #[test]
    fn test_validate_branch_name_empty() {
        assert!(validate_branch_name("").is_err());
    }

    #[test]
    fn test_validate_branch_name_refs_prefix() {
        assert!(validate_branch_name("refs/heads/main").is_err());
        assert!(validate_branch_name("refs/remotes/origin/main").is_err());
    }

    #[test]
    fn test_validate_branch_name_invalid_chars() {
        assert!(validate_branch_name("branch name").is_err());
        assert!(validate_branch_name("branch~1").is_err());
        assert!(validate_branch_name("branch:foo").is_err());
        assert!(validate_branch_name("branch*").is_err());
    }
}
