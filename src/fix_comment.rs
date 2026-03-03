use std::collections::HashMap;
use std::fmt;

use crate::review_schema::{
    FINDING_MARKER, ReviewFinding, capitalize_first, extract_finding_json, group_by_category,
};
use crate::submission::{PrReviewComment, Reaction};

/// GitHub reaction content strings used for fix workflow signaling.
pub const REACTION_ROCKET: &str = "rocket";
pub const REACTION_THUMBS_UP: &str = "+1";
pub const REACTION_CONFUSED: &str = "confused";

/// State of a finding derived from reactions on its inline review comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingState {
    /// No 🚀 reaction — not selected for fix
    Pending,
    /// Has 🚀 reaction — selected, ready to be fixed
    Queued,
    /// Has 👍 reaction — already fixed
    Fixed,
    /// Has 😕 reaction — won't fix
    WontFix,
}

impl fmt::Display for FindingState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FindingState::Pending => write!(f, "pending"),
            FindingState::Queued => write!(f, "🚀"),
            FindingState::Fixed => write!(f, "👍"),
            FindingState::WontFix => write!(f, "😕"),
        }
    }
}

/// A finding extracted from an inline review comment along with its reaction-derived state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixItem {
    pub finding: ReviewFinding,
    pub state: FindingState,
    /// The GitHub comment ID of the inline review comment containing this finding.
    pub comment_id: u64,
    /// Reaction IDs for 🚀 reactions on this comment (needed for removal after fix).
    pub rocket_reaction_ids: Vec<u64>,
}

/// Result of applying a fix to a finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixResultKind {
    Fixed { commit_message: String },
    WontFix { reason: String },
}

/// Determine the `FindingState` from a set of reactions on a comment.
///
/// Priority: Fixed (👍) and WontFix (😕) take precedence over Queued (🚀).
/// If both 👍 and 😕 are present, Fixed wins.
pub fn determine_finding_state(reactions: &[Reaction]) -> FindingState {
    let has_thumbs_up = reactions.iter().any(|r| r.content == REACTION_THUMBS_UP);
    let has_confused = reactions.iter().any(|r| r.content == REACTION_CONFUSED);
    let has_rocket = reactions.iter().any(|r| r.content == REACTION_ROCKET);

    if has_thumbs_up {
        FindingState::Fixed
    } else if has_confused {
        FindingState::WontFix
    } else if has_rocket {
        FindingState::Queued
    } else {
        FindingState::Pending
    }
}

/// Collect 🚀 reaction IDs from a set of reactions.
pub fn rocket_reaction_ids(reactions: &[Reaction]) -> Vec<u64> {
    reactions
        .iter()
        .filter(|r| r.content == REACTION_ROCKET)
        .map(|r| r.id)
        .collect()
}

/// Build `FixItem`s from inline review comments and their reactions.
///
/// For each review comment that contains a `<!-- rlph-finding:{...} -->` marker,
/// parses the finding JSON and determines the state from reactions.
/// Reply comments (`in_reply_to_id` set) are skipped.
pub fn build_fix_items_from_review_comments(
    comments: &[PrReviewComment],
    reactions_by_comment: &[(u64, Vec<Reaction>)],
) -> Vec<FixItem> {
    let reactions_map: HashMap<u64, &[Reaction]> = reactions_by_comment
        .iter()
        .map(|(id, r)| (*id, r.as_slice()))
        .collect();

    let mut items = Vec::new();
    for comment in comments {
        // Skip reply comments — only process top-level finding comments
        if comment.in_reply_to_id.is_some() {
            continue;
        }

        if !comment.body.contains(FINDING_MARKER) {
            continue;
        }

        let Some(finding) = extract_finding_from_body(&comment.body) else {
            continue;
        };

        let reactions = reactions_map.get(&comment.id).copied().unwrap_or(&[]);

        let state = determine_finding_state(reactions);
        let rocket_ids = rocket_reaction_ids(reactions);

        items.push(FixItem {
            finding,
            state,
            comment_id: comment.id,
            rocket_reaction_ids: rocket_ids,
        });
    }
    items
}

/// Extract a `ReviewFinding` from the embedded JSON in a comment body.
///
/// Scans all lines for the `<!-- rlph-finding:{...} -->` marker and returns
/// the first successfully parsed finding.
fn extract_finding_from_body(body: &str) -> Option<ReviewFinding> {
    for line in body.lines() {
        if let Some(json) = extract_finding_json(line)
            && let Ok(finding) = serde_json::from_str(json)
        {
            return Some(finding);
        }
    }
    None
}

/// Format parsed fix items for terminal display, grouped by category.
pub fn format_fix_items_for_display(items: &[FixItem]) -> String {
    if items.is_empty() {
        return "No findings in review comments.".to_string();
    }

    // Group by category
    let groups = group_by_category(items, |item| item.finding.category.as_deref());

    let mut out = String::new();
    for (category, group) in &groups {
        out.push_str(&format!("\n{}\n", capitalize_first(category)));
        for item in group {
            let state_icon = match item.state {
                FindingState::Pending => "  ",
                FindingState::Queued => "🚀",
                FindingState::Fixed => "👍",
                FindingState::WontFix => "😕",
            };
            out.push_str(&format!(
                "  {} ({}) {} `{}` L{}: {}\n",
                state_icon,
                item.finding.id,
                item.finding.severity.label(),
                item.finding.file,
                item.finding.line,
                item.finding.description,
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_schema::{Severity, render_inline_finding_comment_for_github};

    fn make_finding(id: &str, severity: Severity, category: &str) -> ReviewFinding {
        ReviewFinding {
            id: id.to_string(),
            file: "src/main.rs".to_string(),
            line: 42,
            severity,
            description: format!("{id} description"),
            category: Some(category.to_string()),
            depends_on: vec![],
        }
    }

    fn make_review_comment(id: u64, finding: &ReviewFinding) -> PrReviewComment {
        let body = render_inline_finding_comment_for_github(finding, &[], None);
        PrReviewComment {
            id,
            body,
            in_reply_to_id: None,
        }
    }

    fn make_reactions(specs: &[(&str, u64)]) -> Vec<Reaction> {
        specs
            .iter()
            .map(|(content, id)| Reaction {
                id: *id,
                content: content.to_string(),
            })
            .collect()
    }

    // ---- determine_finding_state tests ----

    #[test]
    fn test_state_pending_when_no_reactions() {
        assert_eq!(determine_finding_state(&[]), FindingState::Pending);
    }

    #[test]
    fn test_state_queued_when_rocket() {
        let reactions = make_reactions(&[("rocket", 1)]);
        assert_eq!(determine_finding_state(&reactions), FindingState::Queued);
    }

    #[test]
    fn test_state_fixed_when_check() {
        let reactions = make_reactions(&[("+1", 1)]);
        assert_eq!(determine_finding_state(&reactions), FindingState::Fixed);
    }

    #[test]
    fn test_state_wontfix_when_confused() {
        let reactions = make_reactions(&[("confused", 1)]);
        assert_eq!(determine_finding_state(&reactions), FindingState::WontFix);
    }

    #[test]
    fn test_state_fixed_takes_precedence_over_rocket() {
        let reactions = make_reactions(&[("rocket", 1), ("+1", 2)]);
        assert_eq!(determine_finding_state(&reactions), FindingState::Fixed);
    }

    #[test]
    fn test_state_wontfix_takes_precedence_over_rocket() {
        let reactions = make_reactions(&[("rocket", 1), ("confused", 2)]);
        assert_eq!(determine_finding_state(&reactions), FindingState::WontFix);
    }

    #[test]
    fn test_state_fixed_takes_precedence_over_confused() {
        let reactions = make_reactions(&[("+1", 1), ("confused", 2)]);
        assert_eq!(determine_finding_state(&reactions), FindingState::Fixed);
    }

    #[test]
    fn test_state_ignores_irrelevant_reactions() {
        let reactions = make_reactions(&[("heart", 1), ("eyes", 2)]);
        assert_eq!(determine_finding_state(&reactions), FindingState::Pending);
    }

    // ---- rocket_reaction_ids tests ----

    #[test]
    fn test_rocket_ids_empty_when_no_rockets() {
        let reactions = make_reactions(&[("heart", 1), ("+1", 2)]);
        assert!(rocket_reaction_ids(&reactions).is_empty());
    }

    #[test]
    fn test_rocket_ids_collects_all_rocket_reactions() {
        let reactions = make_reactions(&[("rocket", 10), ("heart", 20), ("rocket", 30)]);
        let ids = rocket_reaction_ids(&reactions);
        assert_eq!(ids, vec![10, 30]);
    }

    // ---- build_fix_items_from_review_comments tests ----

    #[test]
    fn test_build_items_from_comments_with_finding() {
        let finding = make_finding("bug-1", Severity::Critical, "correctness");
        let comment = make_review_comment(100, &finding);
        let reactions = vec![(100u64, make_reactions(&[("rocket", 1)]))];

        let items = build_fix_items_from_review_comments(&[comment], &reactions);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].finding.id, "bug-1");
        assert_eq!(items[0].state, FindingState::Queued);
        assert_eq!(items[0].comment_id, 100);
        assert_eq!(items[0].rocket_reaction_ids, vec![1]);
    }

    #[test]
    fn test_build_items_skips_comments_without_marker() {
        let comment = PrReviewComment {
            id: 100,
            body: "Just a regular comment".to_string(),
            in_reply_to_id: None,
        };
        let items = build_fix_items_from_review_comments(&[comment], &[]);
        assert!(items.is_empty());
    }

    #[test]
    fn test_build_items_skips_reply_comments() {
        let finding = make_finding("bug-1", Severity::Critical, "correctness");
        let body = render_inline_finding_comment_for_github(&finding, &[], None);
        let comment = PrReviewComment {
            id: 100,
            body,
            in_reply_to_id: Some(50), // This is a reply
        };
        let reactions = vec![(100u64, make_reactions(&[("rocket", 1)]))];

        let items = build_fix_items_from_review_comments(&[comment], &reactions);
        assert!(items.is_empty());
    }

    #[test]
    fn test_build_items_mixed_states() {
        let f1 = make_finding("a", Severity::Critical, "correctness");
        let f2 = make_finding("b", Severity::Warning, "correctness");
        let f3 = make_finding("c", Severity::Info, "style");
        let c1 = make_review_comment(100, &f1);
        let c2 = make_review_comment(200, &f2);
        let c3 = make_review_comment(300, &f3);

        let reactions = vec![
            (100u64, make_reactions(&[("rocket", 1)])), // Queued
            (200u64, make_reactions(&[("+1", 2)])),     // Fixed
            (300u64, vec![]),                           // Pending (no reactions)
        ];

        let items = build_fix_items_from_review_comments(&[c1, c2, c3], &reactions);
        assert_eq!(items.len(), 3);

        let a = items.iter().find(|i| i.finding.id == "a").unwrap();
        let b = items.iter().find(|i| i.finding.id == "b").unwrap();
        let c = items.iter().find(|i| i.finding.id == "c").unwrap();
        assert_eq!(a.state, FindingState::Queued);
        assert_eq!(b.state, FindingState::Fixed);
        assert_eq!(c.state, FindingState::Pending);
    }

    #[test]
    fn test_build_items_no_reactions_for_comment() {
        let finding = make_finding("bug-1", Severity::Critical, "correctness");
        let comment = make_review_comment(100, &finding);

        // No reactions for comment 100
        let items = build_fix_items_from_review_comments(&[comment], &[]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].state, FindingState::Pending);
        assert!(items[0].rocket_reaction_ids.is_empty());
    }

    #[test]
    fn test_build_items_malformed_json_skipped() {
        let comment = PrReviewComment {
            id: 100,
            body: "**CRITICAL** `f.rs` L1: bug <!-- rlph-finding:{bad json} -->".to_string(),
            in_reply_to_id: None,
        };
        let items = build_fix_items_from_review_comments(&[comment], &[]);
        assert!(items.is_empty());
    }

    #[test]
    fn test_build_items_with_depends_on() {
        let f = ReviewFinding {
            id: "deref".to_string(),
            file: "src/main.rs".to_string(),
            line: 15,
            severity: Severity::Critical,
            description: "Null deref".to_string(),
            category: Some("correctness".to_string()),
            depends_on: vec!["null-check".to_string()],
        };
        let comment = make_review_comment(100, &f);
        let reactions = vec![(100u64, make_reactions(&[("rocket", 1)]))];

        let items = build_fix_items_from_review_comments(&[comment], &reactions);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].finding.depends_on, vec!["null-check"]);
    }

    // ---- Display format tests ----

    #[test]
    fn test_display_empty_items() {
        let out = format_fix_items_for_display(&[]);
        assert_eq!(out, "No findings in review comments.");
    }

    #[test]
    fn test_display_groups_by_category() {
        let items = vec![
            FixItem {
                finding: make_finding("s1", Severity::Info, "style"),
                state: FindingState::Pending,
                comment_id: 1,
                rocket_reaction_ids: vec![],
            },
            FixItem {
                finding: make_finding("c1", Severity::Critical, "correctness"),
                state: FindingState::Queued,
                comment_id: 2,
                rocket_reaction_ids: vec![],
            },
        ];
        let out = format_fix_items_for_display(&items);
        // BTreeMap: correctness before style
        let corr_pos = out.find("Correctness").unwrap();
        let style_pos = out.find("Style").unwrap();
        assert!(corr_pos < style_pos);
    }

    #[test]
    fn test_display_shows_state_icons() {
        let items = vec![
            FixItem {
                finding: make_finding("a", Severity::Critical, "test"),
                state: FindingState::Pending,
                comment_id: 1,
                rocket_reaction_ids: vec![],
            },
            FixItem {
                finding: make_finding("b", Severity::Warning, "test"),
                state: FindingState::Queued,
                comment_id: 2,
                rocket_reaction_ids: vec![],
            },
            FixItem {
                finding: make_finding("c", Severity::Info, "test"),
                state: FindingState::Fixed,
                comment_id: 3,
                rocket_reaction_ids: vec![],
            },
            FixItem {
                finding: make_finding("d", Severity::Info, "test"),
                state: FindingState::WontFix,
                comment_id: 4,
                rocket_reaction_ids: vec![],
            },
        ];
        let out = format_fix_items_for_display(&items);
        assert!(out.contains("🚀"));
        assert!(out.contains("👍"));
        assert!(out.contains("😕"));
    }

    // ---- FindingState Display ----

    #[test]
    fn test_finding_state_display() {
        assert_eq!(FindingState::Pending.to_string(), "pending");
        assert_eq!(FindingState::Queued.to_string(), "🚀");
        assert_eq!(FindingState::Fixed.to_string(), "👍");
        assert_eq!(FindingState::WontFix.to_string(), "😕");
    }
}
