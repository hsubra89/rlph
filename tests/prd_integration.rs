use std::collections::HashMap;

use rlph::config::{Config, default_review_phases, default_review_step};
use rlph::prd::{build_prd_command, submission_instructions};
use rlph::prompts::PromptEngine;
use rlph::runner::RunnerKind;

fn test_config(source: &str) -> Config {
    Config {
        source: source.to_string(),
        runner: RunnerKind::Claude,
        submission: "github".to_string(),
        label: "rlph".to_string(),
        poll_seconds: 30,
        worktree_dir: "../wt".to_string(),
        base_branch: "main".to_string(),
        max_iterations: None,
        dry_run: false,
        once: false,
        continuous: false,
        agent_binary: "claude".to_string(),
        agent_model: Some("claude-opus-4-6".to_string()),
        agent_timeout: Some(600),
        implement_timeout: Some(1800),
        agent_effort: Some("high".to_string()),
        agent_variant: None,
        max_review_rounds: 3,
        agent_timeout_retries: 2,
        review_phases: default_review_phases(),
        review_aggregate: default_review_step("review-aggregate"),
        review_fix: default_review_step("review-fix"),
        fix: default_review_step("fix"),
        linear: None,
    }
}

fn test_config_codex(source: &str) -> Config {
    Config {
        runner: RunnerKind::Codex,
        agent_binary: "codex".to_string(),
        agent_model: Some("gpt-5.3-codex".to_string()),
        ..test_config(source)
    }
}

#[test]
fn test_prd_command_passes_template_as_positional_arg() {
    let config = test_config("github");
    let (cmd, args) = build_prd_command(&config, "the rendered prompt", None);
    assert_eq!(cmd, "claude");
    // Template is a positional arg, not --append-system-prompt
    assert!(!args.contains(&"--append-system-prompt".to_string()));
    assert_eq!(args.last().unwrap(), "the rendered prompt");
}

#[test]
fn test_prd_command_with_description_appended_to_prompt() {
    let config = test_config("github");
    let (_, args) = build_prd_command(&config, "prompt", Some("add auth"));
    assert!(!args.contains(&"-p".to_string()));
    let positional = args.last().unwrap();
    assert!(positional.contains("prompt"));
    assert!(positional.contains("## Desired Objective"));
    assert!(positional.contains("add auth"));
}

#[test]
fn test_prd_command_without_description_no_p_flag() {
    let config = test_config("github");
    let (_, args) = build_prd_command(&config, "prompt", None);
    assert!(!args.contains(&"-p".to_string()));
}

#[test]
fn test_prd_command_includes_model() {
    let config = test_config("github");
    let (_, args) = build_prd_command(&config, "prompt", None);
    assert!(args.contains(&"--model".to_string()));
    assert!(args.contains(&"claude-opus-4-6".to_string()));
}

#[test]
fn test_prd_template_renders_with_github_source() {
    let engine = PromptEngine::new(None);
    let mut vars = HashMap::new();
    vars.insert(
        "submission_instructions".to_string(),
        submission_instructions("github", "rlph"),
    );

    let rendered = engine.render_phase("prd", &vars).unwrap();
    assert!(rendered.contains("gh issue create"));
    assert!(!rendered.contains("{{"));
}

#[test]
fn test_prd_template_renders_with_linear_source() {
    let engine = PromptEngine::new(None);
    let mut vars = HashMap::new();
    vars.insert(
        "submission_instructions".to_string(),
        submission_instructions("linear", "rlph"),
    );

    let rendered = engine.render_phase("prd", &vars).unwrap();
    assert!(rendered.contains("Linear"));
    assert!(!rendered.contains("{{"));
}

// --- Codex runner command tests ---

#[test]
fn test_prd_command_codex_positional_prompt() {
    let config = test_config_codex("github");
    let (cmd, args) = build_prd_command(&config, "the rendered prompt", None);
    assert_eq!(cmd, "codex");
    assert!(!args.contains(&"--append-system-prompt".to_string()));
    assert!(!args.contains(&"-p".to_string()));
    assert_eq!(args.last().unwrap(), "the rendered prompt");
}

#[test]
fn test_prd_command_codex_combines_prompt_and_description() {
    let config = test_config_codex("github");
    let (_, args) = build_prd_command(&config, "sys prompt", Some("add auth"));
    let positional = args.last().unwrap();
    assert!(positional.contains("sys prompt"));
    assert!(positional.contains("## Desired Objective"));
    assert!(positional.contains("add auth"));
}

#[test]
fn test_prd_command_codex_includes_model() {
    let config = test_config_codex("github");
    let (_, args) = build_prd_command(&config, "prompt", None);
    assert!(args.contains(&"--model".to_string()));
    assert!(args.contains(&"gpt-5.3-codex".to_string()));
}

/// End-to-end: mock agent binary verifies template is passed as positional arg.
#[tokio::test]
#[cfg(unix)]
async fn test_prd_end_to_end_with_mock_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join("mock_claude");
    // Script verifies the last arg contains the PRD template content
    // (no --append-system-prompt or -p flags)
    std::fs::write(
        &script_path,
        r#"#!/bin/bash
# Should NOT have --append-system-prompt or -p
for arg in "$@"; do
    if [ "$arg" = "--append-system-prompt" ] || [ "$arg" = "-p" ]; then
        echo "ERROR: unexpected flag $arg" >&2
        exit 1
    fi
done
# Last arg should be the rendered template (contains submission instructions)
last_arg="${@: -1}"
if echo "$last_arg" | grep -q "gh issue create\|Linear\|configured task source"; then
    exit 0
fi
echo "ERROR: last arg doesn't look like the PRD template" >&2
exit 1
"#,
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let config = Config {
        agent_binary: script_path.to_string_lossy().to_string(),
        agent_model: None,
        ..test_config("github")
    };

    let exit_code = rlph::prd::run_prd(&config, None).await.unwrap();
    assert_eq!(exit_code, 0);
}

/// Verify exit code propagation when the agent exits non-zero.
#[tokio::test]
#[cfg(unix)]
async fn test_prd_exit_code_propagation() {
    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join("mock_claude");
    std::fs::write(&script_path, "#!/bin/bash\nexit 42\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let config = Config {
        agent_binary: script_path.to_string_lossy().to_string(),
        agent_model: None,
        ..test_config("github")
    };

    let exit_code = rlph::prd::run_prd(&config, None).await.unwrap();
    assert_eq!(exit_code, 42);
}

/// Verify description is appended to the positional prompt arg.
#[tokio::test]
#[cfg(unix)]
async fn test_prd_passes_description_in_positional_arg() {
    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join("mock_claude");
    // Script checks last arg contains both the template and the description
    std::fs::write(
        &script_path,
        r#"#!/bin/bash
last_arg="${@: -1}"
if echo "$last_arg" | grep -q "Desired Objective" && echo "$last_arg" | grep -q "add auth"; then
    exit 0
fi
echo "ERROR: last arg missing description" >&2
exit 1
"#,
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let config = Config {
        agent_binary: script_path.to_string_lossy().to_string(),
        agent_model: None,
        ..test_config("github")
    };

    let exit_code = rlph::prd::run_prd(&config, Some("add auth")).await.unwrap();
    assert_eq!(exit_code, 0);
}
