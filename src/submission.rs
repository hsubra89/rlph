use std::process::Command;

use serde::Deserialize;
use tracing::info;

use crate::error::{Error, Result};

#[derive(Debug)]
pub struct SubmitResult {
    pub url: String,
    pub number: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrContext {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub head_branch: String,
    pub linked_issue_number: Option<u64>,
}

pub trait SubmissionBackend {
    /// Submit a branch as a PR or diff. Returns the URL of the created PR/diff.
    fn submit(&self, branch: &str, base: &str, title: &str, body: &str) -> Result<SubmitResult>;

    /// Find an open PR that references the given issue number.
    fn find_existing_pr_for_issue(&self, issue_number: u64) -> Result<Option<u64>>;

    /// Post or update a review comment on an existing PR.
    /// If a previous rlph review comment exists, updates it; otherwise creates a new one.
    fn upsert_review_comment(&self, pr_number: u64, body: &str) -> Result<()>;
}

/// HTML marker injected into review comments so we can find and update them.
pub const REVIEW_MARKER: &str = "<!-- rlph-review -->";

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
                "pr",
                "list",
                "--head",
                branch,
                "--json",
                "url,number",
                "--limit",
                "1",
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

    /// Find an existing rlph review comment on a PR, returning its ID if found.
    fn find_review_comment(&self, pr_number: u64) -> Result<Option<u64>> {
        let endpoint = format!("repos/{{owner}}/{{repo}}/issues/{pr_number}/comments");
        let output = Command::new("gh")
            .args(["api", &endpoint, "--jq", ".[] | select(.body | contains(\"<!-- rlph-review -->\")) | .id"])
            .output()
            .map_err(|e| Error::Submission(format!("failed to run gh: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Submission(format!(
                "gh api list comments failed: {stderr}"
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        // Take the first (most recent won't matter — there should be at most one)
        let comment_id = stdout.lines().next().and_then(|line| line.trim().parse::<u64>().ok());
        Ok(comment_id)
    }

    pub fn get_pr_context(&self, pr_number: u64) -> Result<PrContext> {
        let number_str = pr_number.to_string();
        let output = Command::new("gh")
            .args([
                "pr",
                "view",
                &number_str,
                "--json",
                "number,title,body,url,headRefName",
            ])
            .output()
            .map_err(|e| Error::Submission(format!("failed to run gh: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Submission(format!("gh pr view failed: {stderr}")));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_pr_context_json(&stdout)
            .map_err(|e| Error::Submission(format!("failed to parse gh pr view output: {e}")))
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

    fn upsert_review_comment(&self, pr_number: u64, body: &str) -> Result<()> {
        // Try to find an existing rlph review comment
        if let Some(comment_id) = self.find_review_comment(pr_number)? {
            let endpoint = format!("repos/{{owner}}/{{repo}}/issues/comments/{comment_id}");
            let output = Command::new("gh")
                .args(["api", &endpoint, "-X", "PATCH", "-f", &format!("body={body}")])
                .output()
                .map_err(|e| Error::Submission(format!("failed to run gh: {e}")))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(Error::Submission(format!(
                    "gh api PATCH comment failed: {stderr}"
                )));
            }

            info!(pr_number = pr_number, comment_id = comment_id, "updated review comment on PR");
        } else {
            let number_str = pr_number.to_string();
            let output = Command::new("gh")
                .args(["pr", "comment", &number_str, "--body", body])
                .output()
                .map_err(|e| Error::Submission(format!("failed to run gh: {e}")))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(Error::Submission(format!("gh pr comment failed: {stderr}")));
            }

            info!(pr_number = pr_number, "created review comment on PR");
        }
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

#[derive(Debug, Deserialize)]
struct GhPrView {
    number: u64,
    title: String,
    #[serde(default)]
    body: String,
    url: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
}

fn parse_pr_context_json(json: &str) -> std::result::Result<PrContext, String> {
    let pr: GhPrView =
        serde_json::from_str(json).map_err(|e| format!("invalid json payload: {e}"))?;
    if pr.head_ref_name.trim().is_empty() {
        return Err("missing headRefName".to_string());
    }

    Ok(PrContext {
        number: pr.number,
        title: pr.title,
        body: pr.body.clone(),
        url: pr.url,
        head_branch: pr.head_ref_name,
        linked_issue_number: extract_issue_number_reference(&pr.body),
    })
}

fn extract_issue_number_reference(body: &str) -> Option<u64> {
    body.split_whitespace().find_map(|token| {
        let trimmed = token.trim_matches(|c: char| ",.;:()[]{}".contains(c));
        if let Some(num) = trimmed.strip_prefix('#') {
            return num.parse::<u64>().ok();
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::{
        extract_issue_number_reference, parse_pr_context_json, parse_pr_number_from_url,
        pr_body_references_issue,
    };

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

    #[test]
    fn test_parse_pr_context_json_with_linked_issue() {
        let json = r#"{
            "number": 9,
            "title": "Fix race condition",
            "body": "Resolves #42",
            "url": "https://github.com/o/r/pull/9",
            "headRefName": "feature/fix-race"
        }"#;

        let ctx = parse_pr_context_json(json).unwrap();
        assert_eq!(ctx.number, 9);
        assert_eq!(ctx.title, "Fix race condition");
        assert_eq!(ctx.body, "Resolves #42");
        assert_eq!(ctx.url, "https://github.com/o/r/pull/9");
        assert_eq!(ctx.head_branch, "feature/fix-race");
        assert_eq!(ctx.linked_issue_number, Some(42));
    }

    #[test]
    fn test_parse_pr_context_json_without_linked_issue() {
        let json = r#"{
            "number": 11,
            "title": "Refactor worker",
            "body": "",
            "url": "https://github.com/o/r/pull/11",
            "headRefName": "refactor/worker"
        }"#;

        let ctx = parse_pr_context_json(json).unwrap();
        assert_eq!(ctx.number, 11);
        assert_eq!(ctx.linked_issue_number, None);
    }

    #[test]
    fn test_parse_pr_context_json_missing_head_ref_rejected() {
        let json = r#"{
            "number": 11,
            "title": "Refactor worker",
            "body": "",
            "url": "https://github.com/o/r/pull/11",
            "headRefName": ""
        }"#;

        let err = parse_pr_context_json(json).unwrap_err();
        assert!(err.contains("headRefName"));
    }

    #[test]
    fn test_extract_issue_number_reference() {
        assert_eq!(extract_issue_number_reference("Resolves #42"), Some(42));
        assert_eq!(extract_issue_number_reference("Fixes (#7)."), Some(7));
        assert_eq!(extract_issue_number_reference("No issue refs"), None);
    }
}
