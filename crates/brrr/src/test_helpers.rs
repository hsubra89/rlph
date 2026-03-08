//! Shared test utilities for constructing mock review comments and reactions.

#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
use crate::config::{Config, default_review_phases, default_review_step};
#[cfg(test)]
use crate::error::Result;
use crate::ids::{CommentId, ReactionId};
#[cfg(test)]
use crate::orchestrator::CorrectionRunner;
use crate::review_schema::{ReviewFinding, Severity, render_inline_finding_comment_for_github};
#[cfg(test)]
use crate::runner::{RunResult, RunnerKind};
use crate::submission::{PrReviewComment, Reaction};
#[cfg(test)]
use crate::submission::{SubmissionBackend, SubmitResult};

/// Create a `ReviewFinding` with sensible defaults for tests.
pub fn make_finding(id: &str) -> ReviewFinding {
    ReviewFinding {
        id: id.to_string(),
        file: "src/main.rs".to_string(),
        line: 42,
        severity: Severity::Warning,
        description: format!("{id} description"),
        suggested_fixes: vec![],
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
            id: ReactionId::new(*id),
            content: content.to_string(),
        })
        .collect()
}

/// No-op submission backend for tests that only need to satisfy type constraints.
#[cfg(test)]
pub struct NoopSubmission;

#[cfg(test)]
impl SubmissionBackend for NoopSubmission {
    fn submit(&self, _: &str, _: &str, _: &str, _: &str) -> Result<SubmitResult> {
        unreachable!("submit is not used in tests that use NoopSubmission")
    }
}

/// No-op correction runner for tests that must not spawn real agent processes.
#[cfg(test)]
pub struct NoopCorrectionRunner;

#[cfg(test)]
impl CorrectionRunner for NoopCorrectionRunner {
    async fn resume(
        &self,
        _runner_type: RunnerKind,
        _agent_binary: &str,
        _model: Option<&str>,
        _effort: Option<&str>,
        _variant: Option<&str>,
        _session_id: &str,
        _correction_prompt: &str,
        _working_dir: &Path,
        _timeout: Option<Duration>,
        _stream_prefix: Option<&str>,
    ) -> Result<RunResult> {
        unreachable!("resume is not used in tests that use NoopCorrectionRunner")
    }
}

/// Build a minimal config suitable for unit tests.
#[cfg(test)]
pub fn make_test_config() -> Config {
    Config {
        source: "github".to_string(),
        runner: RunnerKind::Claude,
        submission: "github".to_string(),
        label: "brrr".to_string(),
        poll_seconds: Duration::from_secs(30),
        worktree_dir: "worktrees".to_string(),
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
        agent_timeout_retries: 0,
        review_phases: default_review_phases(),
        review_aggregate: default_review_step("review-aggregate"),
        fix: default_review_step("fix"),
        worktree_setup_script: None,
        linear: None,
    }
}
