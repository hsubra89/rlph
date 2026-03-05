//! Shared test utilities for constructing mock review comments and reactions.

use crate::ids::CommentId;
use crate::review_schema::{ReviewFinding, Severity, render_inline_finding_comment_for_github};
use crate::submission::{PrReviewComment, Reaction};

/// Create a `ReviewFinding` with sensible defaults for tests.
pub fn make_finding(id: &str) -> ReviewFinding {
    ReviewFinding {
        id: id.to_string(),
        file: "src/main.rs".to_string(),
        line: 42,
        severity: Severity::Warning,
        description: format!("{id} description"),
        category: Some("correctness".to_string()),
        depends_on: vec![],
    }
}

/// Create a CRITICAL `ReviewFinding` with sensible defaults for tests.
pub fn make_finding_critical(id: &str) -> ReviewFinding {
    ReviewFinding {
        severity: Severity::Critical,
        ..make_finding(id)
    }
}

/// Create an INFO `ReviewFinding` with sensible defaults for tests.
pub fn make_finding_info(id: &str) -> ReviewFinding {
    ReviewFinding {
        severity: Severity::Info,
        ..make_finding(id)
    }
}

/// Create a `ReviewFinding` with `depends_on` set.
pub fn make_finding_with_deps(id: &str, deps: &[&str]) -> ReviewFinding {
    ReviewFinding {
        depends_on: deps.iter().map(|s| s.to_string()).collect(),
        ..make_finding(id)
    }
}

/// Create a `PrReviewComment` from a `ReviewFinding` for tests.
pub fn make_review_comment(id: u64, finding: &ReviewFinding) -> PrReviewComment {
    PrReviewComment {
        id: CommentId::new(id),
        body: render_inline_finding_comment_for_github(finding, &[], None),
        in_reply_to_id: None,
    }
}

/// Create a `Vec<Reaction>` from `(content, id)` pairs for tests.
pub fn make_reactions(specs: &[(&str, u64)]) -> Vec<Reaction> {
    specs
        .iter()
        .map(|(content, id)| Reaction {
            id: CommentId::new(*id),
            content: content.to_string(),
        })
        .collect()
}
