use std::process::Command;

use tracing::info;

use crate::error::{Error, Result};

#[derive(Debug)]
pub struct SubmitResult {
    pub url: String,
}

pub trait SubmissionBackend {
    /// Submit a branch as a PR or diff. Returns the URL of the created PR/diff.
    fn submit(&self, branch: &str, base: &str, title: &str, body: &str) -> Result<SubmitResult>;
}

/// GitHub PR submission via `gh` CLI.
#[derive(Default)]
pub struct GitHubSubmission;

impl GitHubSubmission {
    pub fn new() -> Self {
        Self
    }

    /// Check if a PR already exists for the given branch.
    fn find_existing_pr(&self, branch: &str) -> Result<Option<String>> {
        let output = Command::new("gh")
            .args([
                "pr", "list", "--head", branch, "--json", "url", "--limit", "1",
            ])
            .output()
            .map_err(|e| Error::Submission(format!("failed to run gh: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Submission(format!("gh pr list failed: {stderr}")));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let prs: Vec<serde_json::Value> = serde_json::from_str(&stdout)
            .map_err(|e| Error::Submission(format!("failed to parse gh output: {e}")))?;

        if let Some(pr) = prs.first()
            && let Some(url) = pr.get("url").and_then(|v| v.as_str())
        {
            return Ok(Some(url.to_string()));
        }

        Ok(None)
    }
}

impl SubmissionBackend for GitHubSubmission {
    fn submit(&self, branch: &str, base: &str, title: &str, body: &str) -> Result<SubmitResult> {
        // Check for existing PR first
        if let Some(url) = self.find_existing_pr(branch)? {
            info!(url = %url, "found existing PR for branch");
            return Ok(SubmitResult { url });
        }

        // Create new PR
        let output = Command::new("gh")
            .args([
                "pr", "create", "--head", branch, "--base", base, "--title", title, "--body", body,
            ])
            .output()
            .map_err(|e| Error::Submission(format!("failed to run gh: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Submission(format!("gh pr create failed: {stderr}")));
        }

        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        info!(url = %url, "created PR");
        Ok(SubmitResult { url })
    }
}
