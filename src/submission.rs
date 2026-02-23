use std::process::Command;

use tracing::info;

use crate::error::{Error, Result};

#[derive(Debug)]
pub struct SubmitResult {
    pub url: String,
    pub number: Option<u64>,
}

pub trait SubmissionBackend {
    /// Submit a branch as a PR or diff. Returns the URL of the created PR/diff.
    fn submit(&self, branch: &str, base: &str, title: &str, body: &str) -> Result<SubmitResult>;

    /// Find an open PR that references the given issue number.
    fn find_existing_pr_for_issue(&self, issue_number: u64) -> Result<Option<u64>>;

    /// Post a comment on an existing PR.
    fn comment_on_pr(&self, pr_number: u64, body: &str) -> Result<()>;
}

/// GitHub PR submission via `gh` CLI.
#[derive(Default)]
pub struct GitHubSubmission;

impl GitHubSubmission {
    pub fn new() -> Self {
        Self
    }

    /// Check if a PR already exists for the given branch.
    fn find_existing_pr(&self, branch: &str) -> Result<Option<(String, Option<u64>)>> {
        let output = Command::new("gh")
            .args([
                "pr", "list", "--head", branch, "--json", "url,number", "--limit", "1",
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
            let number = pr.get("number").and_then(|v| v.as_u64());
            return Ok(Some((url.to_string(), number)));
        }

        Ok(None)
    }

    fn find_existing_pr_for_issue_impl(&self, issue_number: u64) -> Result<Option<u64>> {
        let output = Command::new("gh")
            .args([
                "pr",
                "list",
                "--state",
                "open",
                "--json",
                "number,body",
                "--limit",
                "100",
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

        for pr in prs {
            let Some(number) = pr.get("number").and_then(|v| v.as_u64()) else {
                continue;
            };
            let body = pr
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if pr_body_references_issue(&body, issue_number) {
                return Ok(Some(number));
            }
        }

        Ok(None)
    }
}

impl SubmissionBackend for GitHubSubmission {
    fn submit(&self, branch: &str, base: &str, title: &str, body: &str) -> Result<SubmitResult> {
        // Check for existing PR first
        if let Some((url, number)) = self.find_existing_pr(branch)? {
            info!(url = %url, "found existing PR for branch");
            return Ok(SubmitResult { url, number });
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
        let number = parse_pr_number_from_url(&url);
        info!(url = %url, "created PR");
        Ok(SubmitResult { url, number })
    }

    fn find_existing_pr_for_issue(&self, issue_number: u64) -> Result<Option<u64>> {
        self.find_existing_pr_for_issue_impl(issue_number)
    }

    fn comment_on_pr(&self, pr_number: u64, body: &str) -> Result<()> {
        let number_str = pr_number.to_string();
        let output = Command::new("gh")
            .args(["pr", "comment", &number_str, "--body", body])
            .output()
            .map_err(|e| Error::Submission(format!("failed to run gh: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Submission(format!(
                "gh pr comment failed: {stderr}"
            )));
        }

        info!(pr_number = pr_number, "commented on PR");
        Ok(())
    }
}

/// Parse PR number from a URL like `https://github.com/owner/repo/pull/123`.
fn parse_pr_number_from_url(url: &str) -> Option<u64> {
    url.rsplit('/').next().and_then(|s| s.parse().ok())
}

fn pr_body_references_issue(body: &str, issue_number: u64) -> bool {
    let needle = format!("#{issue_number}");
    body.split_whitespace().any(|token| {
        token == needle || token.trim_matches(|c: char| ",.;:()[]{}".contains(c)) == needle
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_pr_number_from_url, pr_body_references_issue};

    #[test]
    fn test_pr_body_references_issue_exact_match() {
        assert!(pr_body_references_issue("Resolves #42", 42));
    }

    #[test]
    fn test_pr_body_references_issue_with_punctuation() {
        assert!(pr_body_references_issue("Fixes (#42).", 42));
    }

    #[test]
    fn test_pr_body_references_issue_not_partial() {
        assert!(!pr_body_references_issue("Resolves #142", 42));
    }

    #[test]
    fn test_parse_pr_number_from_url() {
        assert_eq!(
            parse_pr_number_from_url("https://github.com/owner/repo/pull/123"),
            Some(123)
        );
        assert_eq!(
            parse_pr_number_from_url("https://github.com/owner/repo/pull/1"),
            Some(1)
        );
        assert_eq!(parse_pr_number_from_url("not-a-url"), None);
    }
}
