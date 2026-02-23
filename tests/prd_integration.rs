use std::collections::HashMap;

use rlph::config::Config;
use rlph::prd::{build_prd_command, submission_instructions};
use rlph::prompts::PromptEngine;

fn test_config(source: &str) -> Config {
    Config {
        source: source.to_string(),
        runner: "claude".to_string(),
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
        agent_effort: Some("high".to_string()),
        max_review_rounds: 3,
        agent_timeout_retries: 2,
        linear: None,
    }
}

fn test_config_codex(source: &str) -> Config {
    Config {
        runner: "codex".to_string(),
        agent_binary: "codex".to_string(),
        agent_model: Some("gpt-5.3-codex".to_string()),
        ..test_config(source)
    }
}

#[test]
fn test_prd_command_includes_append_system_prompt() {
    let config = test_config("github");
    let (cmd, args) = build_prd_command(&config, "the rendered prompt", None);
    assert_eq!(cmd, "claude");
    assert!(args.contains(&"--append-system-prompt".to_string()));
    assert!(args.contains(&"the rendered prompt".to_string()));
}

#[test]
fn test_prd_command_with_description_includes_p_flag() {
    let config = test_config("github");
    let (_, args) = build_prd_command(&config, "prompt", Some("add auth"));
    assert!(args.contains(&"-p".to_string()));
    assert!(args.contains(&"add auth".to_string()));
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
fn test_prd_command_codex_no_append_system_prompt() {
    let config = test_config_codex("github");
    let (cmd, args) = build_prd_command(&config, "the rendered prompt", None);
    assert_eq!(cmd, "codex");
    assert!(!args.contains(&"--append-system-prompt".to_string()));
    // System prompt passed via -p instead
    assert!(args.contains(&"-p".to_string()));
}

#[test]
fn test_prd_command_codex_combines_prompt_and_description() {
    let config = test_config_codex("github");
    let (_, args) = build_prd_command(&config, "sys prompt", Some("add auth"));
    let combined = args
        .iter()
        .find(|a| a.contains("sys prompt") && a.contains("add auth"));
    assert!(combined.is_some(), "codex should combine prompt+description into one -p arg");
}

#[test]
fn test_prd_command_codex_includes_model() {
    let config = test_config_codex("github");
    let (_, args) = build_prd_command(&config, "prompt", None);
    assert!(args.contains(&"--model".to_string()));
    assert!(args.contains(&"gpt-5.3-codex".to_string()));
}

/// End-to-end: mock agent binary verifies it receives the expected flags.
#[tokio::test]
#[cfg(unix)]
async fn test_prd_end_to_end_with_mock_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join("mock_claude");
    // Script verifies --append-system-prompt is present in args
    std::fs::write(
        &script_path,
        r#"#!/bin/bash
found_asp=false
for arg in "$@"; do
    if [ "$arg" = "--append-system-prompt" ]; then
        found_asp=true
    fi
done
if [ "$found_asp" = "false" ]; then
    echo "ERROR: --append-system-prompt not found" >&2
    exit 1
fi
exit 0
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

/// Verify -p flag is passed when description is provided.
#[tokio::test]
#[cfg(unix)]
async fn test_prd_passes_description_as_p_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join("mock_claude");
    // Script checks for -p flag followed by the description
    std::fs::write(
        &script_path,
        r#"#!/bin/bash
found_p=false
next_is_desc=false
for arg in "$@"; do
    if [ "$next_is_desc" = "true" ]; then
        if [ "$arg" = "add auth" ]; then
            found_p=true
        fi
        next_is_desc=false
    fi
    if [ "$arg" = "-p" ]; then
        next_is_desc=true
    fi
done
if [ "$found_p" = "false" ]; then
    echo "ERROR: -p 'add auth' not found" >&2
    exit 1
fi
exit 0
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
