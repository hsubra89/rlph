use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::{info, warn};

use crate::error::{Error, Result};

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

        // Fetch latest base branch from origin (best-effort)
        let _ = self.git(&["fetch", "origin", &self.base_branch]);

        // Determine start point: prefer origin/<base>, fall back to local <base>
        let origin_ref = format!("origin/{}", self.base_branch);
        let start_point = if self.git(&["rev-parse", "--verify", &origin_ref]).is_ok() {
            origin_ref.as_str()
        } else if self
            .git(&["rev-parse", "--verify", &self.base_branch])
            .is_ok()
        {
            self.base_branch.as_str()
        } else {
            return Err(Error::Worktree(format!(
                "base branch '{}' not found locally or on origin",
                self.base_branch
            )));
        };

        // Try creating with a new branch from main
        let create_result = match self.git_worktree_add(&path, &branch, true, Some(start_point)) {
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
        info!(
            issue = issue_number,
            path = %canonical_path.display(),
            branch = %branch,
            "created worktree"
        );
        Ok(WorktreeInfo {
            path: canonical_path,
            branch,
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
            match self.git(&["branch", "-D", &branch]) {
                Ok(_) => info!(branch = %branch, "deleted branch"),
                Err(e) => warn!(branch = %branch, error = %e, "failed to delete branch"),
            }
        }

        Ok(())
    }

    /// Find an existing worktree for an issue number.
    pub fn find_existing(&self, issue_number: u64) -> Result<Option<WorktreeInfo>> {
        // Prune stale entries
        let _ = self.git(&["worktree", "prune"]);

        let output = self
            .git(&["worktree", "list", "--porcelain"])
            .map_err(|e| Error::Worktree(format!("failed to list worktrees: {e}")))?;

        let prefix = format!("rlph-{issue_number}-");

        let mut current_path: Option<PathBuf> = None;
        let mut current_branch: Option<String> = None;

        for line in output.lines() {
            if let Some(path_str) = line.strip_prefix("worktree ") {
                // Save any previous match
                if let Some(ref path) = current_path
                    && let Some(name) = path.file_name().and_then(|n| n.to_str())
                    && name.starts_with(&prefix)
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
            && name.starts_with(&prefix)
        {
            return Ok(Some(WorktreeInfo {
                path: path.clone(),
                branch: current_branch.unwrap_or_else(|| name.to_string()),
            }));
        }

        Ok(None)
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
}
