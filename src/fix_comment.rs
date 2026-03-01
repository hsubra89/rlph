use std::fmt;

use crate::review_schema::{
    FINDING_MARKER, ReviewFinding, capitalize_first, extract_finding_json, group_by_category,
};

/// State of a finding's checkbox in the review comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckboxState {
    /// `- [ ]` — not selected for fix
    Unchecked,
    /// `- [x]` — selected, ready to be fixed
    Checked,
    /// `- ✅` — already fixed
    Fixed,
    /// `- 😵` — won't fix
    WontFix,
}

impl fmt::Display for CheckboxState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckboxState::Unchecked => write!(f, "[ ]"),
            CheckboxState::Checked => write!(f, "[x]"),
            CheckboxState::Fixed => write!(f, "✅"),
            CheckboxState::WontFix => write!(f, "😵"),
        }
    }
}

/// A finding extracted from a review comment along with its checkbox state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixItem {
    pub finding: ReviewFinding,
    pub state: CheckboxState,
}

/// Result of applying a fix to a finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixResultKind {
    Fixed { commit_message: String },
    WontFix { reason: String },
}

/// Parse all `FixItem`s from a review comment body string.
///
/// Scans each line for the `<!-- rlph-finding:{...} -->` marker, extracts the
/// embedded JSON, and determines the checkbox state from the line prefix.
/// Lines with malformed/missing JSON are silently skipped.
pub fn parse_fix_items(body: &str) -> Vec<FixItem> {
    let mut items = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim_start();
        if !trimmed.contains(FINDING_MARKER) {
            continue;
        }

        let state = detect_checkbox_state(trimmed);
        let Some(state) = state else { continue };

        if let Some(finding) = extract_finding_from_line(trimmed) {
            items.push(FixItem { finding, state });
        }
    }
    items
}

/// Update the comment body after a fix result for the given finding id.
///
/// - **Fixed**: replaces the checkbox prefix with `- ✅` and appends
///   `\n  > Fixed: <commit_message>` on the next line.
/// - **WontFix**: replaces the prefix with `- 😵` and appends
///   `\n  > Won't fix: <reason>` on the next line.
///
/// All other lines are preserved unchanged.
pub fn update_comment(body: &str, finding_id: &str, result: &FixResultKind) -> String {
    let mut output_lines: Vec<String> = Vec::new();

    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.contains(FINDING_MARKER)
            && let Some(finding) = extract_finding_from_line(trimmed)
            && finding.id == finding_id
        {
            let (new_prefix, annotation) = match result {
                FixResultKind::Fixed { commit_message } => {
                    ("\u{2705}", format!("  > Fixed: {commit_message}"))
                }
                FixResultKind::WontFix { reason } => {
                    ("\u{1F635}", format!("  > Won't fix: {reason}"))
                }
            };
            let updated = replace_checkbox_prefix(line, new_prefix);
            output_lines.push(updated);
            output_lines.push(annotation);
            continue;
        }
        output_lines.push(line.to_string());
    }

    let mut result_str = output_lines.join("\n");
    if body.ends_with('\n') {
        result_str.push('\n');
    }
    result_str
}

/// Format parsed fix items for terminal display, grouped by category.
pub fn format_fix_items_for_display(items: &[FixItem]) -> String {
    if items.is_empty() {
        return "No findings in review comment.".to_string();
    }

    // Group by category
    let groups = group_by_category(items, |item| item.finding.category.as_deref());

    let mut out = String::new();
    for (category, group) in &groups {
        out.push_str(&format!("\n{}\n", capitalize_first(category)));
        for item in group {
            let state_icon = match item.state {
                CheckboxState::Unchecked => "[ ]",
                CheckboxState::Checked => "[x]",
                CheckboxState::Fixed => " ✅ ",
                CheckboxState::WontFix => " 😵 ",
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

/// Detect the checkbox state from a trimmed line prefix.
fn detect_checkbox_state(trimmed: &str) -> Option<CheckboxState> {
    if trimmed.starts_with("- [ ] ") {
        Some(CheckboxState::Unchecked)
    } else if trimmed.starts_with("- [x] ") || trimmed.starts_with("- [X] ") {
        Some(CheckboxState::Checked)
    } else if trimmed.starts_with("- \u{2705}") {
        Some(CheckboxState::Fixed)
    } else if trimmed.starts_with("- \u{1F635}") {
        Some(CheckboxState::WontFix)
    } else {
        None
    }
}

/// Extract a `ReviewFinding` from the embedded JSON in a line.
fn extract_finding_from_line(line: &str) -> Option<ReviewFinding> {
    let json = extract_finding_json(line)?;
    serde_json::from_str(json).ok()
}

/// Replace the checkbox prefix of a line with a new marker character.
fn replace_checkbox_prefix(line: &str, new_marker: &str) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];

    let prefixes = [
        "- [ ] ",
        "- [x] ",
        "- [X] ",
        "- \u{2705} ",
        "- \u{2705}",
        "- \u{1F635} ",
        "- \u{1F635}",
    ];

    for prefix in prefixes {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return format!("{indent}- {new_marker} {rest}");
        }
    }

    line.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_schema::{Severity, render_findings_for_github};

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

    // ---- Parser tests ----

    #[test]
    fn parse_unchecked_item() {
        let f = make_finding("bug-1", Severity::Critical, "correctness");
        let comment = render_findings_for_github(std::slice::from_ref(&f), "Summary.");
        let items = parse_fix_items(&comment);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].state, CheckboxState::Unchecked);
        assert_eq!(items[0].finding.id, "bug-1");
        assert_eq!(items[0].finding, f);
    }

    #[test]
    fn parse_checked_item() {
        let f = make_finding("bug-1", Severity::Critical, "correctness");
        let comment = render_findings_for_github(&[f], "Summary.");
        // Simulate user checking the box
        let comment = comment.replace("- [ ] ", "- [x] ");
        let items = parse_fix_items(&comment);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].state, CheckboxState::Checked);
    }

    #[test]
    fn parse_checked_uppercase_x() {
        let f = make_finding("bug-1", Severity::Critical, "correctness");
        let comment = render_findings_for_github(&[f], "Summary.");
        let comment = comment.replace("- [ ] ", "- [X] ");
        let items = parse_fix_items(&comment);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].state, CheckboxState::Checked);
    }

    #[test]
    fn parse_fixed_item() {
        let f = make_finding("bug-1", Severity::Critical, "correctness");
        let comment = render_findings_for_github(&[f], "Summary.");
        let comment = comment.replace("- [ ] ", "- ✅ ");
        let items = parse_fix_items(&comment);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].state, CheckboxState::Fixed);
    }

    #[test]
    fn parse_wontfix_item() {
        let f = make_finding("bug-1", Severity::Critical, "correctness");
        let comment = render_findings_for_github(&[f], "Summary.");
        let comment = comment.replace("- [ ] ", "- \u{1F635} ");
        let items = parse_fix_items(&comment);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].state, CheckboxState::WontFix);
    }

    #[test]
    fn parse_mixed_states() {
        let findings = vec![
            make_finding("a", Severity::Critical, "correctness"),
            make_finding("b", Severity::Warning, "correctness"),
            make_finding("c", Severity::Info, "style"),
        ];
        let mut comment = render_findings_for_github(&findings, "Summary.");

        // Make the second item checked, third fixed
        // The rendered lines contain the findings in severity order within category,
        // so "a" is first (critical), "b" is second (warning), "c" is under style
        let lines: Vec<&str> = comment.lines().collect();
        let mut result = Vec::new();
        for line in &lines {
            if line.contains("bug-1-a description") || line.contains("a description") {
                // keep unchecked
                result.push(line.to_string());
            } else if line.contains("b description") {
                result.push(line.replace("- [ ] ", "- [x] "));
            } else if line.contains("c description") {
                result.push(line.replace("- [ ] ", "- ✅ "));
            } else {
                result.push(line.to_string());
            }
        }
        comment = result.join("\n");

        let items = parse_fix_items(&comment);
        assert_eq!(items.len(), 3);

        let a = items.iter().find(|i| i.finding.id == "a").unwrap();
        let b = items.iter().find(|i| i.finding.id == "b").unwrap();
        let c = items.iter().find(|i| i.finding.id == "c").unwrap();
        assert_eq!(a.state, CheckboxState::Unchecked);
        assert_eq!(b.state, CheckboxState::Checked);
        assert_eq!(c.state, CheckboxState::Fixed);
    }

    #[test]
    fn parse_no_review_comment_returns_empty() {
        let items = parse_fix_items("Just a normal comment without findings.");
        assert!(items.is_empty());
    }

    #[test]
    fn parse_empty_body_returns_empty() {
        let items = parse_fix_items("");
        assert!(items.is_empty());
    }

    #[test]
    fn parse_malformed_json_skipped() {
        let body = "- [ ] **CRITICAL** `f.rs` L1: bug <!-- rlph-finding:{bad json} -->";
        let items = parse_fix_items(body);
        assert!(items.is_empty());
    }

    #[test]
    fn parse_missing_closing_comment_skipped() {
        let body = "- [ ] **CRITICAL** `f.rs` L1: bug <!-- rlph-finding:{\"id\":\"x\"}";
        let items = parse_fix_items(body);
        assert!(items.is_empty());
    }

    #[test]
    fn parse_line_without_checkbox_prefix_skipped() {
        let f = make_finding("x", Severity::Info, "style");
        let json = serde_json::to_string(&f).unwrap();
        let body = format!("Some text <!-- rlph-finding:{json} -->");
        let items = parse_fix_items(&body);
        assert!(items.is_empty());
    }

    #[test]
    fn parse_multiple_categories() {
        let findings = vec![
            make_finding("sec-1", Severity::Critical, "security"),
            make_finding("style-1", Severity::Info, "style"),
            make_finding("perf-1", Severity::Warning, "performance"),
        ];
        let comment = render_findings_for_github(&findings, "Review.");
        let items = parse_fix_items(&comment);
        assert_eq!(items.len(), 3);

        let categories: Vec<&str> = items
            .iter()
            .map(|i| i.finding.category.as_deref().unwrap_or("general"))
            .collect();
        assert!(categories.contains(&"security"));
        assert!(categories.contains(&"style"));
        assert!(categories.contains(&"performance"));
    }

    #[test]
    fn parse_finding_with_depends_on() {
        let f = ReviewFinding {
            id: "deref".to_string(),
            file: "src/main.rs".to_string(),
            line: 15,
            severity: Severity::Critical,
            description: "Null deref".to_string(),
            category: Some("correctness".to_string()),
            depends_on: vec!["null-check".to_string()],
        };
        let comment = render_findings_for_github(&[f], "S.");
        let items = parse_fix_items(&comment);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].finding.depends_on, vec!["null-check"]);
    }

    #[test]
    fn parse_finding_with_double_dashes_in_description() {
        let f = ReviewFinding {
            id: "html-esc".to_string(),
            file: "src/tmpl.rs".to_string(),
            line: 10,
            severity: Severity::Warning,
            description: "Outputs --> and -- unescaped".to_string(),
            category: Some("security".to_string()),
            depends_on: vec![],
        };
        let comment = render_findings_for_github(&[f], "S.");
        let items = parse_fix_items(&comment);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].finding.description, "Outputs --> and -- unescaped");
    }

    // ---- Updater tests ----

    #[test]
    fn update_fixed_replaces_checkbox_and_appends_annotation() {
        let f = make_finding("bug-1", Severity::Critical, "correctness");
        let comment = render_findings_for_github(&[f], "Summary.");
        let comment = comment.replace("- [ ] ", "- [x] ");

        let updated = update_comment(
            &comment,
            "bug-1",
            &FixResultKind::Fixed {
                commit_message: "Fixed the bug".to_string(),
            },
        );

        assert!(updated.contains("- ✅ "));
        assert!(!updated.contains("- [x] "));
        assert!(updated.contains("  > Fixed: Fixed the bug"));
    }

    #[test]
    fn update_wontfix_replaces_checkbox_and_appends_annotation() {
        let f = make_finding("nit-1", Severity::Info, "style");
        let comment = render_findings_for_github(&[f], "Summary.");
        let comment = comment.replace("- [ ] ", "- [x] ");

        let updated = update_comment(
            &comment,
            "nit-1",
            &FixResultKind::WontFix {
                reason: "Not worth the effort".to_string(),
            },
        );

        assert!(updated.contains("- \u{1F635} "));
        assert!(!updated.contains("- [x] "));
        assert!(updated.contains("  > Won't fix: Not worth the effort"));
    }

    #[test]
    fn update_preserves_other_lines() {
        let findings = vec![
            make_finding("a", Severity::Critical, "correctness"),
            make_finding("b", Severity::Warning, "correctness"),
        ];
        let comment = render_findings_for_github(&findings, "Summary.");
        // Check only "a"
        let comment = comment.replacen("- [ ] ", "- [x] ", 1);

        let updated = update_comment(
            &comment,
            "a",
            &FixResultKind::Fixed {
                commit_message: "done".to_string(),
            },
        );

        // "b" line should remain unchanged (still unchecked)
        assert!(updated.contains("- [ ] "));
        // Summary preserved
        assert!(updated.contains("Summary."));
        // Category heading preserved
        assert!(updated.contains("### Correctness"));
    }

    #[test]
    fn update_nonexistent_finding_returns_unchanged() {
        let f = make_finding("bug-1", Severity::Critical, "correctness");
        let comment = render_findings_for_github(&[f], "Summary.");

        let updated = update_comment(
            &comment,
            "nonexistent",
            &FixResultKind::Fixed {
                commit_message: "done".to_string(),
            },
        );

        assert_eq!(updated, comment);
    }

    #[test]
    fn update_annotation_appears_after_finding_line() {
        let f = make_finding("bug-1", Severity::Critical, "correctness");
        let comment = render_findings_for_github(&[f], "Summary.");
        let comment = comment.replace("- [ ] ", "- [x] ");

        let updated = update_comment(
            &comment,
            "bug-1",
            &FixResultKind::Fixed {
                commit_message: "commit abc".to_string(),
            },
        );

        let lines: Vec<&str> = updated.lines().collect();
        let finding_line_idx = lines
            .iter()
            .position(|l| l.contains("bug-1"))
            .expect("finding line");
        let annotation_idx = lines
            .iter()
            .position(|l| l.contains("> Fixed: commit abc"))
            .expect("annotation line");
        assert_eq!(annotation_idx, finding_line_idx + 1);
    }

    // ---- Display format tests ----

    #[test]
    fn display_empty_items() {
        let out = format_fix_items_for_display(&[]);
        assert_eq!(out, "No findings in review comment.");
    }

    #[test]
    fn display_groups_by_category() {
        let items = vec![
            FixItem {
                finding: make_finding("s1", Severity::Info, "style"),
                state: CheckboxState::Unchecked,
            },
            FixItem {
                finding: make_finding("c1", Severity::Critical, "correctness"),
                state: CheckboxState::Checked,
            },
        ];
        let out = format_fix_items_for_display(&items);
        // BTreeMap: correctness before style
        let corr_pos = out.find("Correctness").unwrap();
        let style_pos = out.find("Style").unwrap();
        assert!(corr_pos < style_pos);
    }

    #[test]
    fn display_shows_state_icons() {
        let items = vec![
            FixItem {
                finding: make_finding("a", Severity::Critical, "test"),
                state: CheckboxState::Unchecked,
            },
            FixItem {
                finding: make_finding("b", Severity::Warning, "test"),
                state: CheckboxState::Checked,
            },
            FixItem {
                finding: make_finding("c", Severity::Info, "test"),
                state: CheckboxState::Fixed,
            },
            FixItem {
                finding: make_finding("d", Severity::Info, "test"),
                state: CheckboxState::WontFix,
            },
        ];
        let out = format_fix_items_for_display(&items);
        assert!(out.contains("[ ]"));
        assert!(out.contains("[x]"));
        assert!(out.contains("✅"));
        assert!(out.contains("😵"));
    }

    // ---- CheckboxState Display ----

    #[test]
    fn checkbox_state_display() {
        assert_eq!(CheckboxState::Unchecked.to_string(), "[ ]");
        assert_eq!(CheckboxState::Checked.to_string(), "[x]");
        assert_eq!(CheckboxState::Fixed.to_string(), "✅");
        assert_eq!(CheckboxState::WontFix.to_string(), "😵");
    }
}
