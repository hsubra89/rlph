use serde::{Deserialize, Deserializer};

use crate::error::{Error, Result};

/// Deserialize a `Vec<String>` that tolerates both absent keys and explicit `null`.
///
/// `#[serde(default)]` handles a missing key, but an explicit `"depends_on": null` from
/// an LLM would fail deserialization. This function accepts `null` and returns an empty vec.
fn deserialize_null_as_empty_vec<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt: Option<Vec<String>> = Option::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

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
    pub id: String,
    pub file: String,
    pub line: u32,
    pub severity: Severity,
    pub description: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub depends_on: Vec<String>,
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
///
/// If a finding has a `category` set, it is used. Otherwise `default_category` is used.
pub fn render_findings_for_prompt(
    findings: &[ReviewFinding],
    default_category: Option<&str>,
) -> String {
    if findings.is_empty() {
        return "No issues found.".to_string();
    }

    let mut result = String::new();
    for (i, f) in findings.iter().enumerate() {
        if i > 0 {
            result.push('\n');
        }
        let severity = match f.severity {
            Severity::Critical => "CRITICAL",
            Severity::Warning => "WARNING",
            Severity::Info => "INFO",
        };
        let category = f
            .category
            .as_deref()
            .or(default_category)
            .unwrap_or("general");
        use std::fmt::Write;
        let _ = write!(
            result,
            "- ({}) **{}** [{}] `{}` L{}: {}",
            f.id, severity, category, f.file, f.line, f.description
        );
        if !f.depends_on.is_empty() {
            let _ = write!(result, " (depends on: {})", f.depends_on.join(", "));
        }
    }
    result
}

/// Strip markdown code fences (` ```json ... ``` `) that Claude sometimes wraps output in,
/// then parse as `AggregatorOutput`.
pub fn parse_aggregator_output(raw: &str) -> Result<AggregatorOutput> {
    let json = strip_markdown_fences(raw);
    serde_json::from_str(&json)
        .map_err(|e| Error::Orchestrator(format!("failed to parse aggregator JSON: {e}")))
}

/// Status returned by the review-fix agent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FixStatus {
    Fixed,
    Error,
}

/// Structured output from the review-fix agent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FixOutput {
    pub status: FixStatus,
    pub summary: String,
    pub files_changed: Vec<String>,
}

/// Parse the fix agent's JSON output into `FixOutput`.
pub fn parse_fix_output(raw: &str) -> Result<FixOutput> {
    let json = strip_markdown_fences(raw);
    serde_json::from_str(&json)
        .map_err(|e| Error::Orchestrator(format!("failed to parse fix JSON: {e}")))
}

/// Schema names for the correction prompt generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaName {
    Phase,
    Aggregator,
    Fix,
}

impl SchemaName {
    /// Return a JSON example illustrating the expected schema.
    pub fn example_json(&self) -> &'static str {
        match self {
            SchemaName::Phase => {
                r#"{"findings": [{"id": "example-issue", "file": "src/main.rs", "line": 42, "severity": "critical", "description": "issue description", "category": "style", "depends_on": []}]}"#
            }
            SchemaName::Aggregator => {
                r#"{"verdict": "approved", "comment": "summary", "findings": [{"id": "example-issue", "file": "src/main.rs", "line": 1, "severity": "warning", "description": "issue", "category": "style", "depends_on": []}], "fix_instructions": null}"#
            }
            SchemaName::Fix => {
                r#"{"status": "fixed", "summary": "what was done", "files_changed": ["src/main.rs"]}"#
            }
        }
    }
}

/// Generate a correction prompt for an agent that returned malformed JSON.
///
/// The prompt tells the agent what went wrong and shows the expected schema.
pub fn correction_prompt(schema: SchemaName, parse_error: &str) -> String {
    format!(
        "Your previous output could not be parsed as valid JSON.\n\
         Error: {parse_error}\n\n\
         Return ONLY a JSON object matching this schema (no markdown fences, no extra text):\n\
         {example}",
        example = schema.example_json(),
    )
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
                    "id": "sql-injection",
                    "file": "src/main.rs",
                    "line": 42,
                    "severity": "critical",
                    "description": "SQL injection vulnerability"
                },
                {
                    "id": "unused-import",
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
        let fenced = "```json\n{\n  \"verdict\": \"needs_fix\",\n  \"comment\": \"Fix it.\",\n  \"findings\": [{\"id\": \"nit-issue\", \"file\": \"a.rs\", \"line\": 1, \"severity\": \"info\", \"description\": \"nit\"}],\n  \"fix_instructions\": \"do the thing\"\n}\n```";
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
                    "id": "null-ptr-deref",
                    "file": "src/main.rs",
                    "line": 10,
                    "severity": "critical",
                    "description": "Null pointer dereference"
                },
                {
                    "id": "use-constant",
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
        let input = "```json\n{\"findings\": [{\"id\": \"nit-issue\", \"file\": \"a.rs\", \"line\": 1, \"severity\": \"warning\", \"description\": \"nit\"}]}\n```";
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
        assert_eq!(render_findings_for_prompt(&[], None), "No issues found.");
    }

    #[test]
    fn test_render_findings_single() {
        let findings = vec![ReviewFinding {
            id: "sql-injection".to_string(),
            file: "src/main.rs".to_string(),
            line: 42,
            severity: Severity::Critical,
            description: "SQL injection vulnerability".to_string(),
            category: None,
            depends_on: vec![],
        }];
        let rendered = render_findings_for_prompt(&findings, Some("security"));
        assert_eq!(
            rendered,
            "- (sql-injection) **CRITICAL** [security] `src/main.rs` L42: SQL injection vulnerability"
        );
    }

    #[test]
    fn test_render_findings_multiple() {
        let findings = vec![
            ReviewFinding {
                id: "bug-main".to_string(),
                file: "src/main.rs".to_string(),
                line: 42,
                severity: Severity::Critical,
                description: "Bug".to_string(),
                category: Some("correctness".to_string()),
                depends_on: vec![],
            },
            ReviewFinding {
                id: "unused-import".to_string(),
                file: "src/lib.rs".to_string(),
                line: 10,
                severity: Severity::Warning,
                description: "Unused import".to_string(),
                category: None,
                depends_on: vec![],
            },
            ReviewFinding {
                id: "nit-util".to_string(),
                file: "src/util.rs".to_string(),
                line: 5,
                severity: Severity::Info,
                description: "Nit".to_string(),
                category: None,
                depends_on: vec![],
            },
        ];
        let rendered = render_findings_for_prompt(&findings, Some("style"));
        let expected = "\
- (bug-main) **CRITICAL** [correctness] `src/main.rs` L42: Bug
- (unused-import) **WARNING** [style] `src/lib.rs` L10: Unused import
- (nit-util) **INFO** [style] `src/util.rs` L5: Nit";
        assert_eq!(rendered, expected);
    }

    #[test]
    fn test_render_findings_no_default_category() {
        let findings = vec![ReviewFinding {
            id: "nit-main".to_string(),
            file: "src/main.rs".to_string(),
            line: 1,
            severity: Severity::Info,
            description: "nit".to_string(),
            category: None,
            depends_on: vec![],
        }];
        let rendered = render_findings_for_prompt(&findings, None);
        assert_eq!(
            rendered,
            "- (nit-main) **INFO** [general] `src/main.rs` L1: nit"
        );
    }

    // ---- FixOutput tests ----

    #[test]
    fn test_parse_fix_output_valid() {
        let json = r#"{
            "status": "fixed",
            "summary": "Applied SQL injection fix",
            "files_changed": ["src/main.rs", "src/db.rs"]
        }"#;
        let output = parse_fix_output(json).unwrap();
        assert_eq!(output.status, FixStatus::Fixed);
        assert_eq!(output.summary, "Applied SQL injection fix");
        assert_eq!(output.files_changed, vec!["src/main.rs", "src/db.rs"]);
    }

    #[test]
    fn test_parse_fix_output_empty_files_changed() {
        let json = r#"{
            "status": "fixed",
            "summary": "No code changes needed",
            "files_changed": []
        }"#;
        let output = parse_fix_output(json).unwrap();
        assert!(output.files_changed.is_empty());
    }

    #[test]
    fn test_parse_fix_output_missing_fields_errors() {
        // missing summary
        let json = r#"{"status": "fixed", "files_changed": []}"#;
        assert!(parse_fix_output(json).is_err());

        // missing status
        let json = r#"{"summary": "done", "files_changed": []}"#;
        assert!(parse_fix_output(json).is_err());

        // missing files_changed
        let json = r#"{"status": "fixed", "summary": "done"}"#;
        assert!(parse_fix_output(json).is_err());
    }

    #[test]
    fn test_parse_fix_output_fenced_json() {
        let input = "```json\n{\"status\": \"fixed\", \"summary\": \"done\", \"files_changed\": [\"a.rs\"]}\n```";
        let output = parse_fix_output(input).unwrap();
        assert_eq!(output.status, FixStatus::Fixed);
        assert_eq!(output.files_changed, vec!["a.rs"]);
    }

    #[test]
    fn test_parse_fix_output_error_status() {
        let json = r#"{
            "status": "error",
            "summary": "Could not apply fix",
            "files_changed": []
        }"#;
        let output = parse_fix_output(json).unwrap();
        assert_eq!(output.status, FixStatus::Error);
        assert_eq!(output.summary, "Could not apply fix");
        assert!(output.files_changed.is_empty());
    }

    #[test]
    fn test_parse_fix_output_invalid_status_errors() {
        let json = r#"{"status": "unknown", "summary": "done", "files_changed": []}"#;
        assert!(parse_fix_output(json).is_err());
    }

    #[test]
    fn test_parse_fix_output_invalid_json_errors() {
        assert!(parse_fix_output("not json").is_err());
    }

    // ---- id and depends_on tests ----

    #[test]
    fn test_parse_depends_on_null_deserializes_as_empty() {
        let json = r#"{
            "findings": [
                {
                    "id": "null-depends",
                    "file": "src/main.rs",
                    "line": 1,
                    "severity": "info",
                    "description": "test",
                    "depends_on": null
                }
            ]
        }"#;
        let output = parse_phase_output(json).unwrap();
        assert!(output.findings[0].depends_on.is_empty());
    }

    #[test]
    fn test_parse_phase_output_with_depends_on() {
        let json = r#"{
            "findings": [
                {
                    "id": "null-check-missing",
                    "file": "src/main.rs",
                    "line": 10,
                    "severity": "critical",
                    "description": "Missing null check"
                },
                {
                    "id": "null-ptr-deref",
                    "file": "src/main.rs",
                    "line": 15,
                    "severity": "critical",
                    "description": "Null pointer dereference",
                    "depends_on": ["null-check-missing"]
                }
            ]
        }"#;
        let output = parse_phase_output(json).unwrap();
        assert_eq!(output.findings[0].id, "null-check-missing");
        assert!(output.findings[0].depends_on.is_empty());
        assert_eq!(output.findings[1].id, "null-ptr-deref");
        assert_eq!(output.findings[1].depends_on, vec!["null-check-missing"]);
    }

    #[test]
    fn test_render_findings_shows_id() {
        let findings = vec![ReviewFinding {
            id: "redundant-clone-in-loop".to_string(),
            file: "src/lib.rs".to_string(),
            line: 99,
            severity: Severity::Warning,
            description: "Redundant clone inside loop".to_string(),
            category: Some("efficiency".to_string()),
            depends_on: vec![],
        }];
        let rendered = render_findings_for_prompt(&findings, None);
        assert_eq!(
            rendered,
            "- (redundant-clone-in-loop) **WARNING** [efficiency] `src/lib.rs` L99: Redundant clone inside loop"
        );
    }

    #[test]
    fn test_render_findings_with_depends_on() {
        let findings = vec![ReviewFinding {
            id: "null-ptr-deref".to_string(),
            file: "src/main.rs".to_string(),
            line: 15,
            severity: Severity::Critical,
            description: "Null pointer dereference".to_string(),
            category: Some("correctness".to_string()),
            depends_on: vec!["null-check-missing".to_string()],
        }];
        let rendered = render_findings_for_prompt(&findings, None);
        assert_eq!(
            rendered,
            "- (null-ptr-deref) **CRITICAL** [correctness] `src/main.rs` L15: Null pointer dereference (depends on: null-check-missing)"
        );
    }

    // ---- correction_prompt tests ----

    #[test]
    fn test_correction_prompt_contains_schema_example_phase() {
        let prompt = correction_prompt(SchemaName::Phase, "expected value at line 1");
        assert!(prompt.contains("could not be parsed"));
        assert!(prompt.contains("expected value at line 1"));
        assert!(prompt.contains("findings"));
        assert!(prompt.contains("severity"));
        // Verify the example is valid JSON
        let example = SchemaName::Phase.example_json();
        assert!(serde_json::from_str::<PhaseOutput>(example).is_ok());
    }

    #[test]
    fn test_correction_prompt_contains_schema_example_aggregator() {
        let prompt = correction_prompt(SchemaName::Aggregator, "EOF while parsing");
        assert!(prompt.contains("could not be parsed"));
        assert!(prompt.contains("EOF while parsing"));
        assert!(prompt.contains("verdict"));
        assert!(prompt.contains("fix_instructions"));
        let example = SchemaName::Aggregator.example_json();
        assert!(serde_json::from_str::<AggregatorOutput>(example).is_ok());
    }

    #[test]
    fn test_correction_prompt_contains_schema_example_fix() {
        let prompt = correction_prompt(SchemaName::Fix, "trailing comma");
        assert!(prompt.contains("could not be parsed"));
        assert!(prompt.contains("trailing comma"));
        assert!(prompt.contains("status"));
        assert!(prompt.contains("files_changed"));
        let example = SchemaName::Fix.example_json();
        assert!(serde_json::from_str::<FixOutput>(example).is_ok());
    }
}
