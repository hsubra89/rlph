use std::collections::HashMap;
use std::path::Path;

use crate::error::{Error, Result};

const DEFAULT_CHOOSE: &str = include_str!("default_prompts/choose-issue.md");
const DEFAULT_IMPLEMENT: &str = include_str!("default_prompts/implement-issue.md");
const DEFAULT_REVIEW: &str = include_str!("default_prompts/review-issue.md");

/// Known template variable names for validation.
const KNOWN_VARIABLES: &[&str] = &[
    "issue_title",
    "issue_body",
    "issue_number",
    "issue_url",
    "repo_path",
    "branch_name",
    "worktree_path",
    "issues_json",
];

fn default_template(phase: &str) -> Option<&'static str> {
    match phase {
        "choose" => Some(DEFAULT_CHOOSE),
        "implement" => Some(DEFAULT_IMPLEMENT),
        "review" => Some(DEFAULT_REVIEW),
        _ => None,
    }
}

fn template_filename(phase: &str) -> String {
    format!("{phase}-issue.md")
}

/// Prompt template engine with default templates and user overrides.
pub struct PromptEngine {
    override_dir: Option<String>,
}

impl PromptEngine {
    pub fn new(override_dir: Option<String>) -> Self {
        Self { override_dir }
    }

    /// Load a prompt template for the given phase.
    /// User overrides in `override_dir` take precedence over defaults.
    pub fn load_template(&self, phase: &str) -> Result<String> {
        // Check for user override first
        if let Some(ref dir) = self.override_dir {
            let path = Path::new(dir).join(template_filename(phase));
            if path.exists() {
                return std::fs::read_to_string(&path).map_err(|e| {
                    Error::Prompt(format!(
                        "failed to read override template {}: {e}",
                        path.display()
                    ))
                });
            }
        }

        // Fall back to embedded default
        default_template(phase)
            .map(|s| s.to_string())
            .ok_or_else(|| Error::Prompt(format!("unknown prompt phase: {phase}")))
    }

    /// Load a template and render it with the given variables.
    pub fn render_phase(&self, phase: &str, vars: &HashMap<String, String>) -> Result<String> {
        let template = self.load_template(phase)?;
        render_template(&template, vars)
    }
}

/// Render a template string by substituting `{{variable}}` placeholders.
/// Errors on unknown variables (strict mode).
pub fn render_template(template: &str, vars: &HashMap<String, String>) -> Result<String> {
    let mut result = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '{' && chars.peek() == Some(&'{') {
            chars.next(); // consume second {
            let mut var_name = String::new();
            let mut found_close = false;

            while let Some(c2) = chars.next() {
                if c2 == '}' && chars.peek() == Some(&'}') {
                    chars.next(); // consume second }
                    found_close = true;
                    break;
                }
                var_name.push(c2);
            }

            if !found_close {
                return Err(Error::Prompt(format!(
                    "unclosed template variable: {{{{{var_name}"
                )));
            }

            let var_name = var_name.trim();
            if !KNOWN_VARIABLES.contains(&var_name) {
                return Err(Error::Prompt(format!(
                    "unknown template variable: {var_name}"
                )));
            }

            match vars.get(var_name) {
                Some(value) => result.push_str(value),
                None => {
                    return Err(Error::Prompt(format!(
                        "missing value for template variable: {var_name}"
                    )));
                }
            }
        } else {
            result.push(c);
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_load_default_choose() {
        let engine = PromptEngine::new(None);
        let template = engine.load_template("choose").unwrap();
        assert!(template.contains("Task Selection Agent"));
        assert!(template.contains("{{repo_path}}"));
    }

    #[test]
    fn test_load_default_implement() {
        let engine = PromptEngine::new(None);
        let template = engine.load_template("implement").unwrap();
        assert!(template.contains("Task Implementation Agent"));
        assert!(template.contains("{{issue_title}}"));
    }

    #[test]
    fn test_load_default_review() {
        let engine = PromptEngine::new(None);
        let template = engine.load_template("review").unwrap();
        assert!(template.contains("Review Agent"));
        assert!(template.contains("{{issue_title}}"));
    }

    #[test]
    fn test_load_unknown_phase() {
        let engine = PromptEngine::new(None);
        let err = engine.load_template("deploy").unwrap_err();
        assert!(err.to_string().contains("unknown prompt phase"));
    }

    #[test]
    fn test_override_takes_precedence() {
        let dir = TempDir::new().unwrap();
        let override_path = dir.path().join("choose-issue.md");
        fs::write(&override_path, "Custom choose template for {{repo_path}}").unwrap();

        let engine = PromptEngine::new(Some(dir.path().to_string_lossy().to_string()));
        let template = engine.load_template("choose").unwrap();
        assert_eq!(template, "Custom choose template for {{repo_path}}");
    }

    #[test]
    fn test_override_fallback_to_default() {
        let dir = TempDir::new().unwrap();
        // No override file for "implement"
        let engine = PromptEngine::new(Some(dir.path().to_string_lossy().to_string()));
        let template = engine.load_template("implement").unwrap();
        assert!(template.contains("Task Implementation Agent"));
    }

    #[test]
    fn test_render_basic_substitution() {
        let mut vars = HashMap::new();
        vars.insert("issue_title".to_string(), "Fix bug".to_string());
        vars.insert("issue_number".to_string(), "42".to_string());

        let result =
            render_template("Title: {{issue_title}}, Number: {{issue_number}}", &vars).unwrap();
        assert_eq!(result, "Title: Fix bug, Number: 42");
    }

    #[test]
    fn test_render_with_whitespace_in_braces() {
        let mut vars = HashMap::new();
        vars.insert("issue_title".to_string(), "Fix bug".to_string());

        let result = render_template("Title: {{ issue_title }}", &vars).unwrap();
        assert_eq!(result, "Title: Fix bug");
    }

    #[test]
    fn test_render_unknown_variable_errors() {
        let vars = HashMap::new();
        let err = render_template("{{unknown_var}}", &vars).unwrap_err();
        assert!(err.to_string().contains("unknown template variable"));
    }

    #[test]
    fn test_render_missing_value_errors() {
        let vars = HashMap::new();
        let err = render_template("{{issue_title}}", &vars).unwrap_err();
        assert!(err.to_string().contains("missing value"));
    }

    #[test]
    fn test_render_unclosed_variable() {
        let vars = HashMap::new();
        let err = render_template("{{issue_title", &vars).unwrap_err();
        assert!(err.to_string().contains("unclosed template variable"));
    }

    #[test]
    fn test_render_no_variables() {
        let vars = HashMap::new();
        let result = render_template("No variables here", &vars).unwrap();
        assert_eq!(result, "No variables here");
    }

    #[test]
    fn test_render_single_brace_passthrough() {
        let vars = HashMap::new();
        let result = render_template("JSON: {\"key\": \"value\"}", &vars).unwrap();
        assert_eq!(result, "JSON: {\"key\": \"value\"}");
    }

    #[test]
    fn test_render_all_known_variables() {
        let mut vars = HashMap::new();
        vars.insert("issue_title".to_string(), "title".to_string());
        vars.insert("issue_body".to_string(), "body".to_string());
        vars.insert("issue_number".to_string(), "1".to_string());
        vars.insert("issue_url".to_string(), "https://example.com".to_string());
        vars.insert("repo_path".to_string(), "/repo".to_string());
        vars.insert("branch_name".to_string(), "main".to_string());
        vars.insert("worktree_path".to_string(), "/wt".to_string());

        let template = "{{issue_title}} {{issue_body}} {{issue_number}} {{issue_url}} {{repo_path}} {{branch_name}} {{worktree_path}}";
        let result = render_template(template, &vars).unwrap();
        assert_eq!(result, "title body 1 https://example.com /repo main /wt");
    }

    #[test]
    fn test_render_phase_end_to_end() {
        let engine = PromptEngine::new(None);
        let mut vars = HashMap::new();
        vars.insert("repo_path".to_string(), "/my/repo".to_string());
        vars.insert("issues_json".to_string(), "[{\"id\":\"1\"}]".to_string());

        let result = engine.render_phase("choose", &vars).unwrap();
        assert!(result.contains("/my/repo"));
        assert!(!result.contains("{{repo_path}}"));
        assert!(result.contains("[{\"id\":\"1\"}]"));
    }
}
