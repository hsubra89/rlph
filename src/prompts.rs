use std::collections::HashMap;
use std::path::Path;

use crate::error::{Error, Result};

const DEFAULT_CHOOSE: &str = include_str!("default_prompts/choose-issue.md");
const DEFAULT_IMPLEMENT: &str = include_str!("default_prompts/implement-issue.md");
const DEFAULT_CORRECTNESS_REVIEW: &str =
    include_str!("default_prompts/correctness-review-issue.md");
const DEFAULT_SECURITY_REVIEW: &str = include_str!("default_prompts/security-review-issue.md");
const DEFAULT_HYGIENE_REVIEW: &str = include_str!("default_prompts/hygiene-review-issue.md");
const DEFAULT_REVIEW_AGGREGATE: &str = include_str!("default_prompts/review-aggregate-issue.md");
const DEFAULT_REVIEW_FIX: &str = include_str!("default_prompts/review-fix-issue.md");
const DEFAULT_PRD: &str = include_str!("default_prompts/prd.md");
const FINDINGS_SCHEMA: &str = include_str!("default_prompts/_findings-schema.md");

fn default_template(phase: &str) -> Option<&'static str> {
    match phase {
        "choose" => Some(DEFAULT_CHOOSE),
        "implement" => Some(DEFAULT_IMPLEMENT),
        "correctness-review" => Some(DEFAULT_CORRECTNESS_REVIEW),
        "security-review" => Some(DEFAULT_SECURITY_REVIEW),
        "hygiene-review" => Some(DEFAULT_HYGIENE_REVIEW),
        "review-aggregate" => Some(DEFAULT_REVIEW_AGGREGATE),
        "review-fix" => Some(DEFAULT_REVIEW_FIX),
        "prd" => Some(DEFAULT_PRD),
        _ => None,
    }
}

fn template_filename(phase: &str) -> String {
    match phase {
        "prd" => "prd.md".to_string(),
        _ => format!("{phase}-issue.md"),
    }
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
                let content = std::fs::read_to_string(&path).map_err(|e| {
                    Error::Prompt(format!(
                        "failed to read override template {}: {e}",
                        path.display()
                    ))
                })?;
                // No pre-render validation — upon's own render errors include
                // line/column and the offending snippet, which is clearer than
                // anything we could produce with regex-based checks.
                return Ok(content);
            }
        }

        // Fall back to embedded default
        default_template(phase)
            .map(|s| s.to_string())
            .ok_or_else(|| Error::Prompt(format!("unknown prompt phase: {phase}")))
    }

    /// Load a template and render it with the given variables.
    ///
    /// Built-in variables like `findings_schema` are auto-injected when not
    /// already present in `vars`, so templates can reference them without
    /// callers having to supply them.
    pub fn render_phase(&self, phase: &str, vars: &HashMap<String, String>) -> Result<String> {
        let template = self.load_template(phase)?;
        let mut all_vars = vars.clone();
        all_vars
            .entry("findings_schema".to_string())
            .or_insert_with(|| FINDINGS_SCHEMA.to_string());
        render_template(&template, &all_vars)
    }
}

/// Render a template string using the `upon` template engine.
/// Supports `{{ var }}`, `{% if %}`, and `{% for %}` syntax.
pub fn render_template(template: &str, vars: &HashMap<String, String>) -> Result<String> {
    let engine = upon::Engine::new();
    let compiled = engine
        .compile(template)
        .map_err(|e| Error::Prompt(format!("template compile error: {e}")))?;
    compiled
        .render(
            &engine,
            upon::to_value(vars).map_err(|e| Error::Prompt(e.to_string()))?,
        )
        .to_string()
        .map_err(|e| Error::Prompt(format!("template render error: {e}")))
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
    fn test_load_default_correctness_review() {
        let engine = PromptEngine::new(None);
        let template = engine.load_template("correctness-review").unwrap();
        assert!(template.contains("Correctness Review Agent"));
        assert!(template.contains("{{issue_title}}"));
        assert!(template.contains("{{review_phase_name}}"));
        assert!(template.contains("{{base_branch}}"));
    }

    #[test]
    fn test_load_default_security_review() {
        let engine = PromptEngine::new(None);
        let template = engine.load_template("security-review").unwrap();
        assert!(template.contains("Security Review Agent"));
        assert!(template.contains("{{base_branch}}"));
    }

    #[test]
    fn test_load_default_hygiene_review() {
        let engine = PromptEngine::new(None);
        let template = engine.load_template("hygiene-review").unwrap();
        assert!(template.contains("Hygiene Review Coordinator"));
        assert!(template.contains("{{base_branch}}"));
    }

    #[test]
    fn test_load_default_review_aggregate() {
        let engine = PromptEngine::new(None);
        let template = engine.load_template("review-aggregate").unwrap();
        assert!(template.contains("Review Aggregation Agent"));
        assert!(template.contains("{{review_outputs}}"));
    }

    #[test]
    fn test_load_default_review_fix() {
        let engine = PromptEngine::new(None);
        let template = engine.load_template("review-fix").unwrap();
        assert!(template.contains("Review Fix Agent"));
        assert!(template.contains("{{fix_instructions}}"));
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
        assert!(
            err.to_string().contains("render error"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_render_missing_value_errors() {
        let vars = HashMap::new();
        let err = render_template("{{issue_title}}", &vars).unwrap_err();
        assert!(
            err.to_string().contains("render error"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_render_unclosed_variable() {
        let vars = HashMap::new();
        let err = render_template("{{issue_title", &vars).unwrap_err();
        assert!(
            err.to_string().contains("compile error"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_render_if_conditional() {
        let mut vars = HashMap::new();
        vars.insert("pr_number".to_string(), "42".to_string());
        let template = "{% if pr_number %}PR #{{ pr_number }}{% endif %}";
        let result = render_template(template, &vars).unwrap();
        assert_eq!(result, "PR #42");
    }

    #[test]
    fn test_render_if_conditional_falsy_empty_string() {
        let mut vars = HashMap::new();
        vars.insert("pr_number".to_string(), String::new());
        let template = "{% if pr_number %}PR #{{ pr_number }}{% endif %}";
        let result = render_template(template, &vars).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_render_for_loop() {
        let mut vars = HashMap::new();
        vars.insert("items".to_string(), "unused".to_string());
        // upon for loops work on iterables; with HashMap<String,String> we can
        // only test that the engine compiles and renders for syntax correctly.
        // A simple for-in-value list isn't possible with flat string maps, so
        // we just verify the engine doesn't choke on the syntax.
        let engine = upon::Engine::new();
        let compiled = engine
            .compile("{% for x in items %}{{ x }}{% endfor %}")
            .unwrap();
        let val = upon::value! { items: ["a", "b", "c"] };
        let result = compiled.render(&engine, val).to_string().unwrap();
        assert_eq!(result, "abc");
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

        vars.insert("base_branch".to_string(), "main".to_string());

        let template = "{{issue_title}} {{issue_body}} {{issue_number}} {{issue_url}} {{repo_path}} {{branch_name}} {{worktree_path}} {{base_branch}}";
        let result = render_template(template, &vars).unwrap();
        assert_eq!(
            result,
            "title body 1 https://example.com /repo main /wt main"
        );
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

    #[test]
    fn test_load_default_prd() {
        let engine = PromptEngine::new(None);
        let template = engine.load_template("prd").unwrap();
        assert!(template.contains("PRD Writing Agent"));
        assert!(template.contains("{{submission_instructions}}"));
    }

    #[test]
    fn test_prd_override_takes_precedence() {
        let dir = TempDir::new().unwrap();
        let override_path = dir.path().join("prd.md");
        fs::write(&override_path, "Custom prd: {{submission_instructions}}").unwrap();

        let engine = PromptEngine::new(Some(dir.path().to_string_lossy().to_string()));
        let template = engine.load_template("prd").unwrap();
        assert_eq!(template, "Custom prd: {{submission_instructions}}");
    }

    #[test]
    fn test_render_prd_with_variables() {
        let engine = PromptEngine::new(None);
        let mut vars = HashMap::new();
        vars.insert(
            "submission_instructions".to_string(),
            "Create a GitHub issue using `gh issue create`".to_string(),
        );

        let result = engine.render_phase("prd", &vars).unwrap();
        assert!(result.contains("Create a GitHub issue using `gh issue create`"));
        assert!(!result.contains("{{submission_instructions}}"));
    }

    #[test]
    fn test_override_with_unknown_var_loads_but_fails_at_render() {
        let dir = TempDir::new().unwrap();
        let override_path = dir.path().join("choose-issue.md");
        fs::write(&override_path, "Custom: {{bad_var}}").unwrap();

        let engine = PromptEngine::new(Some(dir.path().to_string_lossy().to_string()));
        // load_template succeeds — validation deferred to render time
        let template = engine.load_template("choose").unwrap();
        assert_eq!(template, "Custom: {{bad_var}}");

        // render fails with upon's clear error
        let vars = HashMap::new();
        let err = render_template(&template, &vars).unwrap_err();
        assert!(err.to_string().contains("render error"), "got: {err}");
    }
}
