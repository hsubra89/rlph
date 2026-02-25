use serde::Deserialize;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Approved,
    #[serde(rename = "needs_fix")]
    NeedsFix,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReviewFinding {
    pub file: String,
    pub line: u32,
    pub severity: Severity,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AggregatorOutput {
    pub verdict: Verdict,
    pub comment: String,
    pub findings: Vec<ReviewFinding>,
    pub fix_instructions: Option<String>,
}

/// Per-phase structured output: a list of findings returned by each review agent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PhaseOutput {
    pub findings: Vec<ReviewFinding>,
}

/// Parse a review phase agent's JSON output into `PhaseOutput`.
pub fn parse_phase_output(raw: &str) -> Result<PhaseOutput> {
    let json = strip_markdown_fences(raw);
    serde_json::from_str(&json)
        .map_err(|e| Error::Orchestrator(format!("failed to parse phase JSON: {e}")))
}

/// Render findings as human-readable markdown for injection into the aggregator prompt.
pub fn render_findings_for_prompt(findings: &[ReviewFinding]) -> String {
    if findings.is_empty() {
        return "No issues found.".to_string();
    }

    findings
        .iter()
        .map(|f| {
            let severity = match f.severity {
                Severity::Critical => "CRITICAL",
                Severity::Warning => "WARNING",
                Severity::Info => "INFO",
            };
            format!(
                "- **{}** `{}` L{}: {}",
                severity, f.file, f.line, f.description
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Strip markdown code fences (` ```json ... ``` `) that Claude sometimes wraps output in,
/// then parse as `AggregatorOutput`.
pub fn parse_aggregator_output(raw: &str) -> Result<AggregatorOutput> {
    let json = strip_markdown_fences(raw);
    serde_json::from_str(&json)
        .map_err(|e| Error::Orchestrator(format!("failed to parse aggregator JSON: {e}")))
}

/// Remove markdown code fences from a string, returning the inner content.
/// Handles ` ```json `, ` ``` `, and bare JSON.
fn strip_markdown_fences(input: &str) -> String {
    let trimmed = input.trim();

    // Look for opening fence: ```json or ```
    if let Some(rest) = trimmed.strip_prefix("```") {
        // Skip the optional language tag (e.g. "json") on the opening fence line
        let after_tag = if let Some(pos) = rest.find('\n') {
            &rest[pos + 1..]
        } else {
            return String::new();
        };

        // Strip closing fence
        if let Some(pos) = after_tag.rfind("```") {
            return after_tag[..pos].trim().to_string();
        }
        // No closing fence — return everything after opening
        return after_tag.trim().to_string();
    }

    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_approved() {
        let json = r#"{
            "verdict": "approved",
            "comment": "All looks good.",
            "findings": [],
            "fix_instructions": null
        }"#;
        let output = parse_aggregator_output(json).unwrap();
        assert_eq!(output.verdict, Verdict::Approved);
        assert_eq!(output.comment, "All looks good.");
        assert!(output.findings.is_empty());
        assert!(output.fix_instructions.is_none());
    }

    #[test]
    fn test_parse_valid_needs_fix() {
        let json = r#"{
            "verdict": "needs_fix",
            "comment": "Issues found.",
            "findings": [
                {
                    "file": "src/main.rs",
                    "line": 42,
                    "severity": "critical",
                    "description": "SQL injection vulnerability"
                },
                {
                    "file": "src/lib.rs",
                    "line": 10,
                    "severity": "warning",
                    "description": "Unused import"
                }
            ],
            "fix_instructions": "Fix the SQL injection in main.rs line 42."
        }"#;
        let output = parse_aggregator_output(json).unwrap();
        assert_eq!(output.verdict, Verdict::NeedsFix);
        assert_eq!(output.comment, "Issues found.");
        assert_eq!(output.findings.len(), 2);
        assert_eq!(output.findings[0].file, "src/main.rs");
        assert_eq!(output.findings[0].line, 42);
        assert_eq!(output.findings[0].severity, Severity::Critical);
        assert_eq!(
            output.findings[0].description,
            "SQL injection vulnerability"
        );
        assert_eq!(output.findings[1].severity, Severity::Warning);
        assert_eq!(
            output.fix_instructions.as_deref(),
            Some("Fix the SQL injection in main.rs line 42.")
        );
    }

    #[test]
    fn test_parse_empty_findings_array() {
        let json = r#"{
            "verdict": "approved",
            "comment": "Clean.",
            "findings": [],
            "fix_instructions": null
        }"#;
        let output = parse_aggregator_output(json).unwrap();
        assert!(output.findings.is_empty());
    }

    #[test]
    fn test_parse_missing_required_field_errors() {
        let json = r#"{ "verdict": "approved", "comment": "ok" }"#;
        assert!(parse_aggregator_output(json).is_err());
    }

    #[test]
    fn test_parse_invalid_verdict_errors() {
        let json = r#"{
            "verdict": "maybe",
            "comment": "hmm",
            "findings": [],
            "fix_instructions": null
        }"#;
        assert!(parse_aggregator_output(json).is_err());
    }

    #[test]
    fn test_strip_markdown_json_fence() {
        let input = "```json\n{\"verdict\": \"approved\"}\n```";
        assert_eq!(strip_markdown_fences(input), r#"{"verdict": "approved"}"#);
    }

    #[test]
    fn test_strip_markdown_bare_fence() {
        let input = "```\n{\"verdict\": \"approved\"}\n```";
        assert_eq!(strip_markdown_fences(input), r#"{"verdict": "approved"}"#);
    }

    #[test]
    fn test_strip_no_fence_passthrough() {
        let input = r#"{"verdict": "approved"}"#;
        assert_eq!(strip_markdown_fences(input), r#"{"verdict": "approved"}"#);
    }

    #[test]
    fn test_strip_fence_with_surrounding_whitespace() {
        let input = "\n  ```json\n{\"verdict\": \"approved\"}\n```  \n";
        assert_eq!(strip_markdown_fences(input), r#"{"verdict": "approved"}"#);
    }

    #[test]
    fn test_roundtrip_fenced_json() {
        let fenced = "```json\n{\n  \"verdict\": \"needs_fix\",\n  \"comment\": \"Fix it.\",\n  \"findings\": [{\"file\": \"a.rs\", \"line\": 1, \"severity\": \"info\", \"description\": \"nit\"}],\n  \"fix_instructions\": \"do the thing\"\n}\n```";
        let output = parse_aggregator_output(fenced).unwrap();
        assert_eq!(output.verdict, Verdict::NeedsFix);
        assert_eq!(output.findings.len(), 1);
        assert_eq!(output.findings[0].severity, Severity::Info);
    }

    #[test]
    fn test_both_verdict_variants_deserialize() {
        for (variant, expected) in [
            ("approved", Verdict::Approved),
            ("needs_fix", Verdict::NeedsFix),
        ] {
            let json = format!(
                r#"{{"verdict": "{variant}", "comment": "x", "findings": [], "fix_instructions": null}}"#
            );
            let output = parse_aggregator_output(&json).unwrap();
            assert_eq!(output.verdict, expected);
        }
    }

    #[test]
    fn test_fix_instructions_null_is_none() {
        let json =
            r#"{"verdict": "approved", "comment": "ok", "findings": [], "fix_instructions": null}"#;
        assert!(
            parse_aggregator_output(json)
                .unwrap()
                .fix_instructions
                .is_none()
        );
    }

    #[test]
    fn test_fix_instructions_absent_is_none() {
        // serde_json treats missing Option fields as None
        let json = r#"{"verdict": "approved", "comment": "ok", "findings": []}"#;
        assert!(
            parse_aggregator_output(json)
                .unwrap()
                .fix_instructions
                .is_none()
        );
    }

    // ---- PhaseOutput tests ----

    #[test]
    fn test_parse_phase_output_with_findings() {
        let json = r#"{
            "findings": [
                {
                    "file": "src/main.rs",
                    "line": 10,
                    "severity": "critical",
                    "description": "Null pointer dereference"
                },
                {
                    "file": "src/lib.rs",
                    "line": 25,
                    "severity": "info",
                    "description": "Consider using a constant"
                }
            ]
        }"#;
        let output = parse_phase_output(json).unwrap();
        assert_eq!(output.findings.len(), 2);
        assert_eq!(output.findings[0].file, "src/main.rs");
        assert_eq!(output.findings[0].line, 10);
        assert_eq!(output.findings[0].severity, Severity::Critical);
        assert_eq!(output.findings[0].description, "Null pointer dereference");
        assert_eq!(output.findings[1].severity, Severity::Info);
    }

    #[test]
    fn test_parse_phase_output_empty_findings() {
        let json = r#"{"findings": []}"#;
        let output = parse_phase_output(json).unwrap();
        assert!(output.findings.is_empty());
    }

    #[test]
    fn test_parse_phase_output_fenced_json() {
        let input = "```json\n{\"findings\": [{\"file\": \"a.rs\", \"line\": 1, \"severity\": \"warning\", \"description\": \"nit\"}]}\n```";
        let output = parse_phase_output(input).unwrap();
        assert_eq!(output.findings.len(), 1);
        assert_eq!(output.findings[0].severity, Severity::Warning);
    }

    #[test]
    fn test_parse_phase_output_invalid_json_errors() {
        assert!(parse_phase_output("not json").is_err());
    }

    // ---- render_findings_for_prompt tests ----

    #[test]
    fn test_render_findings_empty() {
        assert_eq!(render_findings_for_prompt(&[]), "No issues found.");
    }

    #[test]
    fn test_render_findings_single() {
        let findings = vec![ReviewFinding {
            file: "src/main.rs".to_string(),
            line: 42,
            severity: Severity::Critical,
            description: "SQL injection vulnerability".to_string(),
        }];
        let rendered = render_findings_for_prompt(&findings);
        assert_eq!(
            rendered,
            "- **CRITICAL** `src/main.rs` L42: SQL injection vulnerability"
        );
    }

    #[test]
    fn test_render_findings_multiple() {
        let findings = vec![
            ReviewFinding {
                file: "src/main.rs".to_string(),
                line: 42,
                severity: Severity::Critical,
                description: "Bug".to_string(),
            },
            ReviewFinding {
                file: "src/lib.rs".to_string(),
                line: 10,
                severity: Severity::Warning,
                description: "Unused import".to_string(),
            },
            ReviewFinding {
                file: "src/util.rs".to_string(),
                line: 5,
                severity: Severity::Info,
                description: "Nit".to_string(),
            },
        ];
        let rendered = render_findings_for_prompt(&findings);
        let expected = "\
- **CRITICAL** `src/main.rs` L42: Bug
- **WARNING** `src/lib.rs` L10: Unused import
- **INFO** `src/util.rs` L5: Nit";
        assert_eq!(rendered, expected);
    }
}
