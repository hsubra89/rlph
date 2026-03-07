//! Git worktree lifecycle: creation, setup script execution, and cleanup.

use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::ids::{IssueNumber, PrNumber};

/// Convention path for the worktree setup script.
const CONVENTION_SETUP_SCRIPT: &str = ".rlph/worktree-setup.sh";
const FETCH_MAX_ATTEMPTS: u32 = 3;

/// Resolve the worktree setup script path.
///
/// Resolution order: config field (if set and non-empty) > convention file (if exists) > None.
/// An empty config string explicitly disables the script.
///
/// Returns an error if the user explicitly configured a script path that does not exist
/// (e.g. a typo). Convention-file absence is silently ignored.
pub fn resolve_setup_script(
    config_value: Option<&str>,
    repo_root: &Path,
) -> Result<Option<PathBuf>> {
    match config_value {
        Some("") => Ok(None),
        Some(s) => {
            let path = Path::new(s);
            if path.is_absolute()
                || path
                    .components()
                    .any(|c| c == std::path::Component::ParentDir)
            {
                warn!(
                    path = s,
                    "setup script path must be relative and within the repo, ignoring"
                );
                return Ok(None);
            }
            let resolved = repo_root.join(s);
            if !resolved.exists() {
                return Err(Error::ConfigValidation(format!(
                    "worktree_setup_script '{}' not found at {}",
                    s,
                    resolved.display()
                )));
            }
            Ok(Some(resolved))
        }
        None => {
            let convention = repo_root.join(CONVENTION_SETUP_SCRIPT);
            if convention.exists() {
                Ok(Some(convention))
            } else {
                Ok(None)
            }
        }
    }
}

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
    if name.contains("..") {
        return Err(Error::Worktree(format!(
            "branch name must not contain '..': {name}"
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

/// Run a git command in the given directory, returning stdout on success or stderr on failure.
pub(crate) fn git_in_dir(cwd: &Path, args: &[&str]) -> std::result::Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
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
    setup_script: Option<PathBuf>,
}

impl WorktreeManager {
    pub fn new(repo_root: PathBuf, base_dir: PathBuf, base_branch: String) -> Self {
        Self {
            repo_root,
            base_dir,
            base_branch,
            setup_script: None,
        }
    }

    /// Set an optional setup script to run after worktree creation.
    pub fn with_setup_script(mut self, script: Option<PathBuf>) -> Self {
        self.setup_script = script;
        self
    }

    /// Generate the worktree directory name: `rlph-{issue_number}-{slug}`.
    pub fn worktree_name(issue_number: IssueNumber, slug: &str) -> String {
        format!("rlph-{issue_number}-{slug}")
    }

    /// Generate the shared fix branch name for a PR branch.
    pub fn fix_branch_name(pr_branch: &str) -> String {
        format!("rlph-fix-{}", Self::slugify(pr_branch))
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
    pub fn create(&self, issue_number: IssueNumber, slug: &str) -> Result<WorktreeInfo> {
        // Check for existing worktree
        if let Some(existing) = self.find_existing(issue_number)? {
            info!(
                issue_number = %issue_number,
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
        self.fetch_with_retry(&self.base_branch, FETCH_MAX_ATTEMPTS)?;

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
            issue_number = %issue_number,
            path = %canonical_path.display(),
            branch = %branch,
            commit = %commit_sha,
            "created worktree from origin/{}",
            self.base_branch
        );

        self.run_setup_script(&canonical_path)?;

        Ok(WorktreeInfo {
            path: canonical_path,
            branch,
        })
    }

    /// Create a worktree for a PR review against an existing branch.
    /// Reuses an existing dedicated PR worktree when present.
    pub fn create_for_branch(&self, pr_number: PrNumber, branch: &str) -> Result<WorktreeInfo> {
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
                pr_number = %pr_number,
                branch,
                path = %existing.path.display(),
                "reusing existing PR review worktree, updating to latest"
            );

            // Fetch latest from origin so we don't review stale code
            self.fetch_with_retry(branch, FETCH_MAX_ATTEMPTS)?;

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
        self.fetch_with_retry(branch, FETCH_MAX_ATTEMPTS)?;

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
            pr_number = %pr_number,
            path = %canonical_path.display(),
            branch,
            commit = %commit_sha,
            "created PR review worktree"
        );

        self.run_setup_script(&canonical_path)?;

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
        self.fetch_with_retry(remote_branch, FETCH_MAX_ATTEMPTS)?;

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

        self.run_setup_script(&canonical)?;

        Ok(WorktreeInfo {
            path: canonical,
            branch: branch_name.to_string(),
        })
    }

    /// Reset an existing worktree to the latest remote branch state.
    ///
    /// Fetches the remote branch, hard-resets the worktree, and cleans untracked files
    /// so it's ready for the next fix session.
    pub fn reset_to_remote(&self, worktree_path: &Path, remote_branch: &str) -> Result<()> {
        self.fetch_with_retry(remote_branch, FETCH_MAX_ATTEMPTS)?;
        git_in_dir(
            worktree_path,
            &["reset", "--hard", &format!("origin/{remote_branch}")],
        )
        .map_err(|e| Error::Worktree(format!("failed to reset worktree: {e}")))?;
        git_in_dir(worktree_path, &["clean", "-ffd"])
            .map_err(|e| Error::Worktree(format!("failed to clean worktree: {e}")))?;
        Ok(())
    }

    /// Run the setup script in the given worktree directory, if configured.
    fn run_setup_script(&self, worktree_path: &Path) -> Result<()> {
        let script = match &self.setup_script {
            Some(p) => p,
            None => return Ok(()),
        };

        info!(
            script = %script.display(),
            worktree = %worktree_path.display(),
            "running worktree setup script"
        );

        let repo_root_str = self.repo_root.to_string_lossy();
        let output = Command::new("sh")
            .arg(script)
            .arg(repo_root_str.as_ref())
            .current_dir(worktree_path)
            .output()
            .map_err(|e| {
                Error::Worktree(format!(
                    "failed to run setup script {}: {e}",
                    script.display()
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut detail = stderr.trim().to_string();
            if !stdout.trim().is_empty() {
                if !detail.is_empty() {
                    detail.push('\n');
                }
                detail.push_str("stdout: ");
                detail.push_str(stdout.trim());
            }
            return Err(Error::Worktree(format!(
                "setup script {} exited with {}: {}",
                script.display(),
                output.status,
                detail
            )));
        }

        info!("worktree setup script completed successfully");
        Ok(())
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
    pub fn find_existing(&self, issue_number: IssueNumber) -> Result<Option<WorktreeInfo>> {
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
        git_in_dir(&self.repo_root, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(cwd: &Path, args: &[&str]) {
        git_in_dir(cwd, args).unwrap_or_else(|e| panic!("git {:?} failed: {e}", args));
    }

    fn init_temp_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();

        run_git(path, &["init"]);
        run_git(path, &["config", "user.email", "test@test.com"]);
        run_git(path, &["config", "user.name", "Test"]);

        std::fs::write(path.join("README.md"), "# initial\n").unwrap();
        run_git(path, &["add", "."]);
        run_git(path, &["commit", "-m", "init"]);
        run_git(path, &["branch", "-M", "main"]);

        let path_str = path.to_str().unwrap();
        run_git(path, &["remote", "add", "origin", path_str]);

        dir
    }

    #[test]
    fn test_worktree_name() {
        assert_eq!(
            WorktreeManager::worktree_name(IssueNumber::new(5), "worktree-management"),
            "rlph-5-worktree-management"
        );
        assert_eq!(
            WorktreeManager::worktree_name(IssueNumber::new(42), "fix-bug"),
            "rlph-42-fix-bug"
        );
    }

    #[test]
    fn test_fix_branch_name() {
        assert_eq!(
            WorktreeManager::fix_branch_name("feature/SQL Injection"),
            "rlph-fix-feature-sql-injection"
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
    fn test_validate_branch_name_dotdot() {
        assert!(validate_branch_name("feature/..").is_err());
        assert!(validate_branch_name("../escape").is_err());
        assert!(validate_branch_name("a..b").is_err());
    }

    #[test]
    fn test_validate_branch_name_invalid_chars() {
        assert!(validate_branch_name("branch name").is_err());
        assert!(validate_branch_name("branch~1").is_err());
        assert!(validate_branch_name("branch:foo").is_err());
        assert!(validate_branch_name("branch*").is_err());
    }

    #[test]
    fn test_run_setup_script_creates_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree_path = tmp.path().join("wt");
        std::fs::create_dir_all(&worktree_path).unwrap();

        let script_path = tmp.path().join("setup.sh");
        std::fs::write(&script_path, "touch \"$PWD/marker.txt\"\n").unwrap();

        let wm = WorktreeManager::new(
            tmp.path().to_path_buf(),
            tmp.path().join("worktrees"),
            "main".to_string(),
        )
        .with_setup_script(Some(script_path));

        wm.run_setup_script(&worktree_path).unwrap();
        assert!(worktree_path.join("marker.txt").exists());
    }

    #[test]
    fn test_run_setup_script_nonzero_exit_propagates_error() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree_path = tmp.path().join("wt");
        std::fs::create_dir_all(&worktree_path).unwrap();

        let script_path = tmp.path().join("bad.sh");
        std::fs::write(&script_path, "echo 'fail' >&2; exit 1\n").unwrap();

        let wm = WorktreeManager::new(
            tmp.path().to_path_buf(),
            tmp.path().join("worktrees"),
            "main".to_string(),
        )
        .with_setup_script(Some(script_path));

        let err = wm.run_setup_script(&worktree_path).unwrap_err();
        assert!(matches!(err, Error::Worktree(_)));
        assert!(err.to_string().contains("fail"));
    }

    #[test]
    fn test_run_setup_script_missing_script_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree_path = tmp.path().join("wt");
        std::fs::create_dir_all(&worktree_path).unwrap();

        let wm = WorktreeManager::new(
            tmp.path().to_path_buf(),
            tmp.path().join("worktrees"),
            "main".to_string(),
        )
        .with_setup_script(Some(tmp.path().join("nonexistent.sh")));

        // resolve_setup_script guarantees existence; a missing script here is a bug
        let err = wm.run_setup_script(&worktree_path).unwrap_err();
        assert!(err.to_string().contains("setup script"));
    }

    #[test]
    fn test_run_setup_script_none_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree_path = tmp.path().join("wt");
        std::fs::create_dir_all(&worktree_path).unwrap();

        let wm = WorktreeManager::new(
            tmp.path().to_path_buf(),
            tmp.path().join("worktrees"),
            "main".to_string(),
        );

        wm.run_setup_script(&worktree_path).unwrap();
    }

    #[test]
    fn test_run_setup_script_receives_repo_root_as_arg() {
        let tmp = tempfile::tempdir().unwrap();
        let worktree_path = tmp.path().join("wt");
        std::fs::create_dir_all(&worktree_path).unwrap();

        let script_path = tmp.path().join("check_arg.sh");
        std::fs::write(&script_path, "echo \"$1\" > \"$PWD/arg.txt\"\n").unwrap();

        let repo_root = tmp.path().to_path_buf();
        let wm = WorktreeManager::new(
            repo_root.clone(),
            tmp.path().join("worktrees"),
            "main".to_string(),
        )
        .with_setup_script(Some(script_path));

        wm.run_setup_script(&worktree_path).unwrap();
        let arg = std::fs::read_to_string(worktree_path.join("arg.txt")).unwrap();
        assert_eq!(arg.trim(), repo_root.to_string_lossy().as_ref());
    }

    #[test]
    fn test_resolve_setup_script_config_override() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("scripts/setup.sh");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "#!/bin/sh\n").unwrap();

        let result = resolve_setup_script(Some("scripts/setup.sh"), tmp.path()).unwrap();
        assert_eq!(result, Some(tmp.path().join("scripts/setup.sh")));
    }

    #[test]
    fn test_resolve_setup_script_errors_on_missing_configured_file() {
        let tmp = tempfile::tempdir().unwrap();
        let result = resolve_setup_script(Some("scripts/setup.sh"), tmp.path());
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("scripts/setup.sh"),
            "error should mention the configured path: {msg}"
        );
    }

    #[test]
    fn test_resolve_setup_script_empty_string_disables() {
        let tmp = tempfile::tempdir().unwrap();
        // Even if convention file exists, empty string disables
        let convention = tmp.path().join(".rlph/worktree-setup.sh");
        std::fs::create_dir_all(convention.parent().unwrap()).unwrap();
        std::fs::write(&convention, "#!/bin/sh\n").unwrap();

        let result = resolve_setup_script(Some(""), tmp.path()).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_setup_script_convention_file() {
        let tmp = tempfile::tempdir().unwrap();
        let convention = tmp.path().join(".rlph/worktree-setup.sh");
        std::fs::create_dir_all(convention.parent().unwrap()).unwrap();
        std::fs::write(&convention, "#!/bin/sh\n").unwrap();

        let result = resolve_setup_script(None, tmp.path()).unwrap();
        assert_eq!(result, Some(convention));
    }

    #[test]
    fn test_resolve_setup_script_no_convention_no_config() {
        let tmp = tempfile::tempdir().unwrap();
        let result = resolve_setup_script(None, tmp.path()).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_setup_script_rejects_absolute_path() {
        let tmp = tempfile::tempdir().unwrap();
        let result = resolve_setup_script(Some("/etc/evil.sh"), tmp.path()).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_setup_script_rejects_parent_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let result = resolve_setup_script(Some("../escape.sh"), tmp.path()).unwrap();
        assert_eq!(result, None);

        let result = resolve_setup_script(Some("scripts/../../escape.sh"), tmp.path()).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_reset_to_remote_restores_latest_remote_state() {
        let repo = init_temp_repo();
        let wt_base = tempfile::tempdir().unwrap();
        let mgr = WorktreeManager::new(
            repo.path().to_path_buf(),
            wt_base.path().to_path_buf(),
            "main".to_string(),
        );

        let info = mgr.create_fresh("rlph-fix-main", "main").unwrap();

        std::fs::write(repo.path().join("README.md"), "# remote\n").unwrap();
        std::fs::write(repo.path().join("remote-only.txt"), "from remote\n").unwrap();
        run_git(repo.path(), &["add", "."]);
        run_git(repo.path(), &["commit", "-m", "remote update"]);

        std::fs::write(info.path.join("README.md"), "# local dirty\n").unwrap();
        std::fs::write(info.path.join("staged.txt"), "staged change\n").unwrap();
        run_git(&info.path, &["add", "staged.txt"]);
        std::fs::write(info.path.join("scratch.txt"), "untracked\n").unwrap();

        let dirty_status = git_in_dir(&info.path, &["status", "--porcelain"]).unwrap();
        assert!(
            !dirty_status.trim().is_empty(),
            "expected dirty worktree before reset"
        );

        mgr.reset_to_remote(&info.path, "main").unwrap();

        assert_eq!(
            std::fs::read_to_string(info.path.join("README.md")).unwrap(),
            "# remote\n"
        );
        assert_eq!(
            std::fs::read_to_string(info.path.join("remote-only.txt")).unwrap(),
            "from remote\n"
        );
        assert!(!info.path.join("staged.txt").exists());
        assert!(!info.path.join("scratch.txt").exists());

        let status = git_in_dir(&info.path, &["status", "--porcelain"]).unwrap();
        assert!(
            status.trim().is_empty(),
            "expected clean worktree after reset, got: {status}"
        );
    }
}
