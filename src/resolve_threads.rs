use std::process::Command;

use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::review_schema::FINDING_MARKER;

/// GraphQL reaction content values indicating a completed finding (Fixed or WontFix).
const COMPLETED_REACTIONS: &[&str] = &["THUMBS_UP", "CONFUSED"];

// ---------------------------------------------------------------------------
// GraphQL response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GraphQlResponse<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Option<Vec<GraphQlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQlError {
    message: String,
}

// -- Query: reviewThreads --

#[derive(Debug, Deserialize)]
struct ReviewThreadsData {
    repository: Option<RepositoryNode>,
}

#[derive(Debug, Deserialize)]
struct RepositoryNode {
    #[serde(rename = "pullRequest")]
    pull_request: Option<PullRequestNode>,
}

#[derive(Debug, Deserialize)]
struct PullRequestNode {
    #[serde(rename = "reviewThreads")]
    review_threads: ReviewThreadConnection,
}

#[derive(Debug, Deserialize)]
struct ReviewThreadConnection {
    nodes: Vec<ReviewThreadNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ReviewThreadNode {
    pub id: String,
    #[serde(rename = "isResolved")]
    pub is_resolved: bool,
    pub comments: CommentConnection,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CommentConnection {
    pub nodes: Vec<CommentNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CommentNode {
    pub body: String,
    pub reactions: ReactionConnection,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ReactionConnection {
    pub nodes: Vec<ReactionNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ReactionNode {
    pub content: String,
}

// ---------------------------------------------------------------------------
// Pure logic
// ---------------------------------------------------------------------------

/// Filter review thread nodes to find unresolved rlph-finding threads with
/// completed reactions (THUMBS_UP or CONFUSED).
///
/// A thread qualifies when:
/// 1. It is not already resolved
/// 2. Its first comment body contains the `<!-- rlph-finding:` marker
/// 3. Its first comment has a THUMBS_UP or CONFUSED reaction
pub(crate) fn find_completed_rlph_thread_ids(threads: &[ReviewThreadNode]) -> Vec<&str> {
    threads
        .iter()
        .filter(|thread| {
            if thread.is_resolved {
                return false;
            }
            let Some(first_comment) = thread.comments.nodes.first() else {
                return false;
            };
            if !first_comment.body.contains(FINDING_MARKER) {
                return false;
            }
            first_comment
                .reactions
                .nodes
                .iter()
                .any(|r| COMPLETED_REACTIONS.contains(&r.content.as_str()))
        })
        .map(|thread| thread.id.as_str())
        .collect()
}

// ---------------------------------------------------------------------------
// GraphQL queries
// ---------------------------------------------------------------------------

// NOTE: reviewThreads(first: 100) is not paginated. PRs with >100 review
// threads may miss completed rlph threads beyond the first page. In practice
// this limit is unlikely to be hit; if it is, switch to cursor-based pagination
// (see run_gh_api_paginated for a reference pattern).
//
// NOTE: reactions(first: 20) is not paginated. A comment with >20 reactions
// could have its completion reaction missed, silently preventing thread
// resolution. In practice this is unlikely for review comments.
const REVIEW_THREADS_QUERY: &str = r#"
query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 100) {
        nodes {
          id
          isResolved
          comments(first: 1) {
            nodes {
              body
              reactions(first: 20) {
                nodes {
                  content
                }
              }
            }
          }
        }
      }
    }
  }
}
"#;

const RESOLVE_THREAD_MUTATION: &str = r#"
mutation($threadId: ID!) {
  resolveReviewThread(input: {threadId: $threadId}) {
    thread {
      isResolved
    }
  }
}
"#;

fn check_graphql_errors<T>(response: &GraphQlResponse<T>) -> Result<()> {
    if let Some(errors) = &response.errors {
        let msgs: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        return Err(Error::Submission(format!(
            "GraphQL errors: {}",
            msgs.join(", ")
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// GraphQL I/O
// ---------------------------------------------------------------------------

/// Resolve all completed rlph-finding review threads on a PR.
///
/// Returns the number of threads resolved.
pub fn resolve_completed_threads(owner: &str, repo: &str, pr_number: u64) -> Result<u32> {
    let threads = fetch_review_threads(owner, repo, pr_number)?;
    let thread_ids = find_completed_rlph_thread_ids(&threads);

    if thread_ids.is_empty() {
        debug!(pr_number, "no completed rlph review threads to resolve");
        return Ok(0);
    }

    info!(
        pr_number,
        count = thread_ids.len(),
        "resolving completed rlph review threads"
    );

    let mut resolved = 0u32;
    for thread_id in &thread_ids {
        match resolve_thread(thread_id) {
            Ok(()) => resolved += 1,
            Err(e) => warn!(thread_id, error = %e, "failed to resolve review thread"),
        }
    }

    info!(pr_number, resolved, "resolved rlph review threads");
    Ok(resolved)
}

fn fetch_review_threads(owner: &str, repo: &str, pr_number: u64) -> Result<Vec<ReviewThreadNode>> {
    let number_arg = format!("number={pr_number}");
    let output = Command::new("gh")
        .args([
            "api",
            "graphql",
            "-f",
            &format!("query={REVIEW_THREADS_QUERY}"),
            "-f",
            &format!("owner={owner}"),
            "-f",
            &format!("name={repo}"),
            "-F",
            &number_arg,
        ])
        .output()
        .map_err(|e| Error::Submission(format!("failed to run gh api graphql: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Submission(format!(
            "gh api graphql failed: {stderr}"
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let response: GraphQlResponse<ReviewThreadsData> = serde_json::from_str(&stdout)
        .map_err(|e| Error::Submission(format!("failed to parse GraphQL response: {e}")))?;

    check_graphql_errors(&response)?;

    let threads = response
        .data
        .and_then(|d| d.repository)
        .and_then(|r| r.pull_request)
        .map(|pr| pr.review_threads.nodes)
        .unwrap_or_default();

    Ok(threads)
}

fn resolve_thread(thread_id: &str) -> Result<()> {
    let output = Command::new("gh")
        .args([
            "api",
            "graphql",
            "-f",
            &format!("query={RESOLVE_THREAD_MUTATION}"),
            "-f",
            &format!("threadId={thread_id}"),
        ])
        .output()
        .map_err(|e| Error::Submission(format!("failed to run gh api graphql: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Submission(format!(
            "gh api graphql resolve thread failed: {stderr}"
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let response: GraphQlResponse<serde_json::Value> = serde_json::from_str(&stdout)
        .map_err(|e| Error::Submission(format!("failed to parse resolve response: {e}")))?;

    check_graphql_errors(&response)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_thread(
        id: &str,
        is_resolved: bool,
        body: &str,
        reactions: &[&str],
    ) -> ReviewThreadNode {
        ReviewThreadNode {
            id: id.to_string(),
            is_resolved,
            comments: CommentConnection {
                nodes: vec![CommentNode {
                    body: body.to_string(),
                    reactions: ReactionConnection {
                        nodes: reactions
                            .iter()
                            .map(|r| ReactionNode {
                                content: r.to_string(),
                            })
                            .collect(),
                    },
                }],
            },
        }
    }

    fn finding_body() -> String {
        format!("{FINDING_MARKER}{{\"id\":\"bug-1\"}} -->")
    }

    #[test]
    fn test_empty_threads_returns_empty() {
        assert!(find_completed_rlph_thread_ids(&[]).is_empty());
    }

    #[test]
    fn test_already_resolved_thread_skipped() {
        let thread = make_thread("T1", true, &finding_body(), &["THUMBS_UP"]);
        assert!(find_completed_rlph_thread_ids(&[thread]).is_empty());
    }

    #[test]
    fn test_non_finding_thread_skipped() {
        let thread = make_thread("T1", false, "Just a regular comment", &["THUMBS_UP"]);
        assert!(find_completed_rlph_thread_ids(&[thread]).is_empty());
    }

    #[test]
    fn test_finding_without_completed_reaction_skipped() {
        let thread = make_thread("T1", false, &finding_body(), &["ROCKET"]);
        assert!(find_completed_rlph_thread_ids(&[thread]).is_empty());
    }

    #[test]
    fn test_finding_with_no_reactions_skipped() {
        let thread = make_thread("T1", false, &finding_body(), &[]);
        assert!(find_completed_rlph_thread_ids(&[thread]).is_empty());
    }

    #[test]
    fn test_finding_with_thumbs_up_included() {
        let threads = [make_thread("T1", false, &finding_body(), &["THUMBS_UP"])];
        let ids = find_completed_rlph_thread_ids(&threads);
        assert_eq!(ids, vec!["T1"]);
    }

    #[test]
    fn test_finding_with_confused_included() {
        let threads = [make_thread("T1", false, &finding_body(), &["CONFUSED"])];
        let ids = find_completed_rlph_thread_ids(&threads);
        assert_eq!(ids, vec!["T1"]);
    }

    #[test]
    fn test_finding_with_mixed_reactions_included() {
        let threads = [make_thread(
            "T1",
            false,
            &finding_body(),
            &["ROCKET", "THUMBS_UP"],
        )];
        let ids = find_completed_rlph_thread_ids(&threads);
        assert_eq!(ids, vec!["T1"]);
    }

    #[test]
    fn test_thread_with_empty_comments_skipped() {
        let thread = ReviewThreadNode {
            id: "T1".to_string(),
            is_resolved: false,
            comments: CommentConnection { nodes: vec![] },
        };
        assert!(find_completed_rlph_thread_ids(&[thread]).is_empty());
    }

    #[test]
    fn test_mixed_threads_filters_correctly() {
        let threads = vec![
            make_thread("T1", false, &finding_body(), &["THUMBS_UP"]), // ✓ completed
            make_thread("T2", true, &finding_body(), &["THUMBS_UP"]),  // ✗ already resolved
            make_thread("T3", false, "regular comment", &["THUMBS_UP"]), // ✗ not a finding
            make_thread("T4", false, &finding_body(), &["ROCKET"]),    // ✗ not completed
            make_thread("T5", false, &finding_body(), &["CONFUSED"]),  // ✓ won't fix
            make_thread("T6", false, &finding_body(), &[]),            // ✗ no reactions
        ];
        let ids = find_completed_rlph_thread_ids(&threads);
        assert_eq!(ids, vec!["T1", "T5"]);
    }

    #[test]
    fn test_finding_with_irrelevant_reactions_only_skipped() {
        let thread = make_thread("T1", false, &finding_body(), &["HEART", "EYES", "LAUGH"]);
        assert!(find_completed_rlph_thread_ids(&[thread]).is_empty());
    }

    #[test]
    fn test_graphql_response_parsing() {
        let json = r#"{
            "data": {
                "repository": {
                    "pullRequest": {
                        "reviewThreads": {
                            "nodes": [
                                {
                                    "id": "PRRT_abc123",
                                    "isResolved": false,
                                    "comments": {
                                        "nodes": [
                                            {
                                                "body": "**CRITICAL** <!-- rlph-finding:{} -->",
                                                "reactions": {
                                                    "nodes": [
                                                        {"content": "THUMBS_UP"}
                                                    ]
                                                }
                                            }
                                        ]
                                    }
                                }
                            ]
                        }
                    }
                }
            }
        }"#;

        let response: GraphQlResponse<ReviewThreadsData> =
            serde_json::from_str(json).expect("should parse");
        let threads = response
            .data
            .unwrap()
            .repository
            .unwrap()
            .pull_request
            .unwrap()
            .review_threads
            .nodes;
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, "PRRT_abc123");

        let ids = find_completed_rlph_thread_ids(&threads);
        assert_eq!(ids, vec!["PRRT_abc123"]);
    }

    #[test]
    fn test_graphql_response_with_errors() {
        let json = r#"{
            "data": null,
            "errors": [{"message": "Not found"}]
        }"#;
        let response: GraphQlResponse<ReviewThreadsData> =
            serde_json::from_str(json).expect("should parse");
        assert!(response.errors.is_some());
        assert!(response.data.is_none());
    }

    #[test]
    fn test_graphql_response_empty_pr() {
        let json = r#"{
            "data": {
                "repository": {
                    "pullRequest": {
                        "reviewThreads": {
                            "nodes": []
                        }
                    }
                }
            }
        }"#;
        let response: GraphQlResponse<ReviewThreadsData> =
            serde_json::from_str(json).expect("should parse");
        let threads = response
            .data
            .unwrap()
            .repository
            .unwrap()
            .pull_request
            .unwrap()
            .review_threads
            .nodes;
        assert!(threads.is_empty());
    }
}
