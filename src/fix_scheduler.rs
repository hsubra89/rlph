use std::collections::HashSet;

use crate::fix_comment::FixItem;
use crate::fix_deps::FindingDeps;
use crate::review_schema::Severity;

/// Maximum number of WARNING/INFO findings in a single batch.
const MAX_BATCH_SIZE: usize = 3;

/// Action the fix orchestrator should take next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleAction {
    /// Run a single CRITICAL finding in its own session.
    RunCritical(String),
    /// Run a batch of WARNING/INFO findings (up to 3) in a single session.
    RunBatch(Vec<String>),
    /// Nothing eligible to schedule.
    Idle,
}

/// Determine the next scheduling action given current fix state.
///
/// The scheduler is pure and stateless — all state is passed in:
/// - `findings`: all findings for the PR
/// - `deps`: pre-built dependency graph
/// - `completed`: finding IDs already completed (fixed or won't-fix)
/// - `failed`: finding IDs that failed and should be skipped
///
/// Priority rules:
/// 1. Dependencies override severity (a WARNING depended on by a CRITICAL runs first)
/// 2. Eligible CRITICALs run solo via `RunCritical`
/// 3. Eligible WARNING/INFO findings are batched (up to 3), WARNING preferred over INFO
/// 4. Findings in cycles or in the failed set are skipped
pub fn next_action(
    findings: &[FixItem],
    deps: &FindingDeps,
    completed: &HashSet<&str>,
    failed: &HashSet<&str>,
) -> ScheduleAction {
    // Collect eligible findings: not completed, not failed, not in cycle, deps met.
    let mut criticals: Vec<&str> = Vec::new();
    let mut warnings: Vec<&str> = Vec::new();
    let mut infos: Vec<&str> = Vec::new();

    for item in findings {
        let id = item.finding.id.as_str();

        if completed.contains(id) || failed.contains(id) {
            continue;
        }
        if deps.in_cycle(id) {
            continue;
        }
        if !deps.deps_met(id, completed) {
            continue;
        }

        match item.finding.severity {
            Severity::Critical => criticals.push(id),
            Severity::Warning => warnings.push(id),
            Severity::Info => infos.push(id),
        }
    }

    // CRITICALs run solo, first one wins.
    if let Some(id) = criticals.first() {
        return ScheduleAction::RunCritical(id.to_string());
    }

    // Batch WARNING then INFO, up to MAX_BATCH_SIZE.
    let mut batch: Vec<String> = Vec::new();
    for id in warnings.iter().chain(infos.iter()) {
        if batch.len() >= MAX_BATCH_SIZE {
            break;
        }
        batch.push(id.to_string());
    }

    if batch.is_empty() {
        ScheduleAction::Idle
    } else {
        ScheduleAction::RunBatch(batch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix_comment::{FindingState, FixItem};
    use crate::fix_deps::FindingDeps;
    use crate::review_schema::{ReviewFinding, Severity};

    // --- helpers ---

    fn finding(id: &str, severity: Severity, deps: &[&str]) -> ReviewFinding {
        ReviewFinding {
            id: id.to_string(),
            file: "src/main.rs".to_string(),
            line: 1,
            severity,
            description: format!("{id} desc"),
            category: Some("test".to_string()),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn item(id: &str, severity: Severity, deps: &[&str]) -> FixItem {
        FixItem {
            finding: finding(id, severity, deps),
            state: FindingState::Queued,
            comment_id: 0,
            rocket_reaction_ids: vec![],
        }
    }

    fn schedule(items: &[FixItem], completed: &[&str], failed: &[&str]) -> ScheduleAction {
        let deps = FindingDeps::build(items);
        let completed: HashSet<&str> = completed.iter().copied().collect();
        let failed: HashSet<&str> = failed.iter().copied().collect();
        next_action(items, &deps, &completed, &failed)
    }

    // --- All-CRITICAL input → sequential RunCritical ---

    #[test]
    fn all_criticals_returns_first() {
        let items = vec![
            item("c1", Severity::Critical, &[]),
            item("c2", Severity::Critical, &[]),
        ];
        assert_eq!(
            schedule(&items, &[], &[]),
            ScheduleAction::RunCritical("c1".into())
        );
    }

    #[test]
    fn all_criticals_second_after_first_completed() {
        let items = vec![
            item("c1", Severity::Critical, &[]),
            item("c2", Severity::Critical, &[]),
        ];
        assert_eq!(
            schedule(&items, &["c1"], &[]),
            ScheduleAction::RunCritical("c2".into())
        );
    }

    // --- All-WARNING/INFO input → RunBatch ---

    #[test]
    fn all_warnings_batched() {
        let items = vec![
            item("w1", Severity::Warning, &[]),
            item("w2", Severity::Warning, &[]),
        ];
        assert_eq!(
            schedule(&items, &[], &[]),
            ScheduleAction::RunBatch(vec!["w1".into(), "w2".into()])
        );
    }

    #[test]
    fn all_info_batched() {
        let items = vec![
            item("i1", Severity::Info, &[]),
            item("i2", Severity::Info, &[]),
        ];
        assert_eq!(
            schedule(&items, &[], &[]),
            ScheduleAction::RunBatch(vec!["i1".into(), "i2".into()])
        );
    }

    #[test]
    fn warnings_preferred_over_info_in_batch() {
        let items = vec![
            item("i1", Severity::Info, &[]),
            item("w1", Severity::Warning, &[]),
            item("i2", Severity::Info, &[]),
            item("w2", Severity::Warning, &[]),
        ];
        let action = schedule(&items, &[], &[]);
        // Warnings first, then infos, capped at 3
        assert_eq!(
            action,
            ScheduleAction::RunBatch(vec!["w1".into(), "w2".into(), "i1".into()])
        );
    }

    // --- Mixed severities → CRITICALs first, then batches ---

    #[test]
    fn mixed_critical_first() {
        let items = vec![
            item("w1", Severity::Warning, &[]),
            item("c1", Severity::Critical, &[]),
            item("i1", Severity::Info, &[]),
        ];
        assert_eq!(
            schedule(&items, &[], &[]),
            ScheduleAction::RunCritical("c1".into())
        );
    }

    #[test]
    fn mixed_batch_after_criticals_done() {
        let items = vec![
            item("c1", Severity::Critical, &[]),
            item("w1", Severity::Warning, &[]),
            item("i1", Severity::Info, &[]),
        ];
        assert_eq!(
            schedule(&items, &["c1"], &[]),
            ScheduleAction::RunBatch(vec!["w1".into(), "i1".into()])
        );
    }

    // --- Dependency chains crossing severity → deps override severity ---

    #[test]
    fn critical_blocked_by_warning_dep() {
        // c1 depends on w1 — w1 must go first even though it's lower severity
        let items = vec![
            item("c1", Severity::Critical, &["w1"]),
            item("w1", Severity::Warning, &[]),
        ];
        assert_eq!(
            schedule(&items, &[], &[]),
            ScheduleAction::RunBatch(vec!["w1".into()])
        );
    }

    #[test]
    fn critical_unblocked_after_dep_completed() {
        let items = vec![
            item("c1", Severity::Critical, &["w1"]),
            item("w1", Severity::Warning, &[]),
        ];
        assert_eq!(
            schedule(&items, &["w1"], &[]),
            ScheduleAction::RunCritical("c1".into())
        );
    }

    #[test]
    fn info_dep_blocks_warning() {
        let items = vec![
            item("w1", Severity::Warning, &["i1"]),
            item("i1", Severity::Info, &[]),
        ];
        assert_eq!(
            schedule(&items, &[], &[]),
            ScheduleAction::RunBatch(vec!["i1".into()])
        );
    }

    // --- Failed CRITICAL → skipped ---

    #[test]
    fn failed_critical_skipped() {
        let items = vec![
            item("c1", Severity::Critical, &[]),
            item("c2", Severity::Critical, &[]),
        ];
        assert_eq!(
            schedule(&items, &[], &["c1"]),
            ScheduleAction::RunCritical("c2".into())
        );
    }

    #[test]
    fn all_failed_returns_idle() {
        let items = vec![item("c1", Severity::Critical, &[])];
        assert_eq!(schedule(&items, &[], &["c1"]), ScheduleAction::Idle);
    }

    #[test]
    fn failed_warning_skipped_in_batch() {
        let items = vec![
            item("w1", Severity::Warning, &[]),
            item("w2", Severity::Warning, &[]),
        ];
        assert_eq!(
            schedule(&items, &[], &["w1"]),
            ScheduleAction::RunBatch(vec!["w2".into()])
        );
    }

    // --- Cycle findings → skipped ---

    #[test]
    fn cycle_findings_skipped() {
        let items = vec![
            item("a", Severity::Critical, &["b"]),
            item("b", Severity::Critical, &["a"]),
            item("c", Severity::Critical, &[]),
        ];
        assert_eq!(
            schedule(&items, &[], &[]),
            ScheduleAction::RunCritical("c".into())
        );
    }

    #[test]
    fn all_in_cycle_returns_idle() {
        let items = vec![
            item("a", Severity::Warning, &["b"]),
            item("b", Severity::Warning, &["a"]),
        ];
        assert_eq!(schedule(&items, &[], &[]), ScheduleAction::Idle);
    }

    // --- Empty eligible set → Idle ---

    #[test]
    fn empty_findings_returns_idle() {
        assert_eq!(schedule(&[], &[], &[]), ScheduleAction::Idle);
    }

    #[test]
    fn all_completed_returns_idle() {
        let items = vec![
            item("c1", Severity::Critical, &[]),
            item("w1", Severity::Warning, &[]),
        ];
        assert_eq!(schedule(&items, &["c1", "w1"], &[]), ScheduleAction::Idle);
    }

    // --- Batch size capped at 3 ---

    #[test]
    fn batch_capped_at_three() {
        let items = vec![
            item("w1", Severity::Warning, &[]),
            item("w2", Severity::Warning, &[]),
            item("w3", Severity::Warning, &[]),
            item("w4", Severity::Warning, &[]),
        ];
        let action = schedule(&items, &[], &[]);
        match &action {
            ScheduleAction::RunBatch(ids) => {
                assert_eq!(ids.len(), 3);
                assert_eq!(ids, &["w1", "w2", "w3"]);
            }
            other => panic!("expected RunBatch, got {other:?}"),
        }
    }

    #[test]
    fn batch_cap_prefers_warnings_over_info() {
        let items = vec![
            item("i1", Severity::Info, &[]),
            item("w1", Severity::Warning, &[]),
            item("i2", Severity::Info, &[]),
            item("w2", Severity::Warning, &[]),
            item("w3", Severity::Warning, &[]),
        ];
        let action = schedule(&items, &[], &[]);
        assert_eq!(
            action,
            ScheduleAction::RunBatch(vec!["w1".into(), "w2".into(), "w3".into()])
        );
    }

    // --- Single WARNING/INFO → RunBatch with 1 element ---

    #[test]
    fn single_warning_returns_batch_of_one() {
        let items = vec![item("w1", Severity::Warning, &[])];
        assert_eq!(
            schedule(&items, &[], &[]),
            ScheduleAction::RunBatch(vec!["w1".into()])
        );
    }

    #[test]
    fn single_info_returns_batch_of_one() {
        let items = vec![item("i1", Severity::Info, &[])];
        assert_eq!(
            schedule(&items, &[], &[]),
            ScheduleAction::RunBatch(vec!["i1".into()])
        );
    }

    // --- Complex integration scenarios ---

    #[test]
    fn chain_critical_depends_on_warning_depends_on_info() {
        // c1 → w1 → i1: only i1 eligible first
        let items = vec![
            item("c1", Severity::Critical, &["w1"]),
            item("w1", Severity::Warning, &["i1"]),
            item("i1", Severity::Info, &[]),
        ];

        // Step 1: only i1 eligible
        assert_eq!(
            schedule(&items, &[], &[]),
            ScheduleAction::RunBatch(vec!["i1".into()])
        );

        // Step 2: after i1, w1 eligible
        assert_eq!(
            schedule(&items, &["i1"], &[]),
            ScheduleAction::RunBatch(vec!["w1".into()])
        );

        // Step 3: after w1, c1 eligible
        assert_eq!(
            schedule(&items, &["i1", "w1"], &[]),
            ScheduleAction::RunCritical("c1".into())
        );

        // Step 4: all done
        assert_eq!(
            schedule(&items, &["i1", "w1", "c1"], &[]),
            ScheduleAction::Idle
        );
    }

    #[test]
    fn independent_critical_and_blocked_critical() {
        // c1 has no deps, c2 depends on w1
        let items = vec![
            item("c1", Severity::Critical, &[]),
            item("c2", Severity::Critical, &["w1"]),
            item("w1", Severity::Warning, &[]),
        ];

        // c1 eligible (critical), w1 eligible (warning) — critical wins
        assert_eq!(
            schedule(&items, &[], &[]),
            ScheduleAction::RunCritical("c1".into())
        );

        // After c1, w1 still eligible as batch (c2 blocked)
        assert_eq!(
            schedule(&items, &["c1"], &[]),
            ScheduleAction::RunBatch(vec!["w1".into()])
        );

        // After w1, c2 unblocked
        assert_eq!(
            schedule(&items, &["c1", "w1"], &[]),
            ScheduleAction::RunCritical("c2".into())
        );
    }

    #[test]
    fn failed_dep_blocks_dependent_forever() {
        // c1 depends on w1, w1 failed → c1 never gets deps_met
        // (w1 is in failed but not in completed, so deps aren't met)
        let items = vec![
            item("c1", Severity::Critical, &["w1"]),
            item("w1", Severity::Warning, &[]),
        ];
        assert_eq!(schedule(&items, &[], &["w1"]), ScheduleAction::Idle);
    }

    #[test]
    fn mixed_cycle_and_non_cycle() {
        let items = vec![
            item("a", Severity::Warning, &["b"]),
            item("b", Severity::Warning, &["a"]), // cycle with a
            item("c", Severity::Info, &[]),       // independent
        ];
        assert_eq!(
            schedule(&items, &[], &[]),
            ScheduleAction::RunBatch(vec!["c".into()])
        );
    }
}
