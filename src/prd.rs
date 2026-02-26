use std::collections::HashMap;
use std::process::Stdio;

use tracing::info;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::prompts::PromptEngine;

const PROMPT_OVERRIDE_DIR: &str = ".rlph/prompts";

/// Build the submission instructions string for the given task source.
pub fn submission_instructions(source: &str, label: &str) -> String {
    match source {
        "github" => format!(
            "Submit the final PRD as a GitHub issue using the `gh` CLI:\n\
             ```\n\
             gh issue create --label \"{label}\" --title \"PRD: <title>\" --body \"<prd content>\"\n\
             ```\n\
             Use a HEREDOC for the body if it contains special characters.\n\
             Add the label `{label}` to the issue so the autonomous loop can pick it up.",
        ),
        "linear" => format!(
            "Submit the final PRD as a Linear project/issue.\n\
             Use the Linear CLI or API to create the issue with the PRD as its description.\n\
             Ensure it is placed in the correct team and project.\n\
             Tag it with the label `{label}`.",
        ),
        _ => "Submit the final PRD to your configured task source.".to_string(),
    }
}

/// Build the agent command for an interactive PRD session.
///
/// Returns `(binary, args)` suitable for spawning with inherited stdio.
/// Dispatches on `config.runner` to produce the correct CLI flags.
pub fn build_prd_command(
    config: &Config,
    rendered_prompt: &str,
    description: Option<&str>,
) -> (String, Vec<String>) {
    let mut args = Vec::new();

    // Build the initial user prompt: template + optional description.
    // Passed as a positional argument so the session stays interactive.
    let prompt = match description {
        Some(desc) if !desc.is_empty() => {
            format!("{rendered_prompt}\n\n## Desired Objective\n\n{desc}")
        }
        _ => rendered_prompt.to_string(),
    };

    if let Some(ref model) = config.agent_model {
        args.push("--model".to_string());
        args.push(model.clone());
    }

    args.push(prompt);

    (config.agent_binary.clone(), args)
}

/// Run an interactive PRD session.
///
/// Launches the configured agent with the PRD prompt and inherited stdio.
/// Blocks until the agent exits, then propagates the exit code.
pub async fn run_prd(config: &Config, description: Option<&str>) -> Result<i32> {
    let override_dir = std::path::Path::new(PROMPT_OVERRIDE_DIR);
    let engine = PromptEngine::new(
        override_dir
            .is_dir()
            .then(|| override_dir.to_string_lossy().to_string()),
    );

    let mut vars = HashMap::new();
    vars.insert(
        "submission_instructions".to_string(),
        submission_instructions(&config.source, &config.label),
    );

    let rendered = engine.render_phase("prd", &vars)?;

    let (binary, args) = build_prd_command(config, &rendered, description);

    info!(
        binary = %binary,
        args = ?args.iter().take(3).collect::<Vec<_>>(),
        "launching interactive PRD session"
    );

    let mut cmd = tokio::process::Command::new(&binary);
    cmd.args(&args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    // Remove CLAUDECODE env var to allow nested CLI invocation.
    cmd.env_remove("CLAUDECODE");

    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Process(format!("failed to spawn '{}': {e}", binary)))?;

    let status = child
        .wait()
        .await
        .map_err(|e| Error::Process(format!("failed to wait for agent: {e}")))?;

    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_submission_instructions_github() {
        let instr = submission_instructions("github", "rlph");
        assert!(instr.contains("gh issue create"));
        assert!(instr.contains("rlph"));
    }

    #[test]
    fn test_submission_instructions_github_custom_label() {
        let instr = submission_instructions("github", "ai-tasks");
        assert!(instr.contains("--label \"ai-tasks\""));
        assert!(instr.contains("ai-tasks"));
        assert!(!instr.contains("rlph"));
    }

    #[test]
    fn test_submission_instructions_linear() {
        let instr = submission_instructions("linear", "rlph");
        assert!(instr.contains("Linear"));
        assert!(instr.contains("rlph"));
    }

    #[test]
    fn test_submission_instructions_unknown_source() {
        let instr = submission_instructions("jira", "rlph");
        assert!(instr.contains("configured task source"));
    }

    // --- Common behavior tests (runner-agnostic) ---

    #[test]
    fn test_build_prd_command_basic() {
        let config = test_config("claude", "github", None);
        let (cmd, args) = build_prd_command(&config, "rendered prompt", None);
        assert_eq!(cmd, "claude");
        // Template passed as positional arg (last element)
        assert_eq!(args.last().unwrap(), "rendered prompt");
        // No system prompt or -p flags
        assert!(!args.contains(&"--append-system-prompt".to_string()));
        assert!(!args.contains(&"-p".to_string()));
    }

    #[test]
    fn test_build_prd_command_with_description() {
        let config = test_config("claude", "github", None);
        let (_, args) = build_prd_command(&config, "prompt", Some("add auth"));
        let positional = args.last().unwrap();
        assert!(positional.contains("prompt"));
        assert!(positional.contains("## Desired Objective"));
        assert!(positional.contains("add auth"));
        assert!(!args.contains(&"-p".to_string()));
    }

    #[test]
    fn test_build_prd_command_without_description() {
        let config = test_config("claude", "github", None);
        let (_, args) = build_prd_command(&config, "prompt", None);
        assert_eq!(args.last().unwrap(), "prompt");
        assert!(!args.last().unwrap().contains("Desired Objective"));
    }

    #[test]
    fn test_build_prd_command_with_model() {
        let config = test_config("claude", "github", Some("opus"));
        let (_, args) = build_prd_command(&config, "prompt", None);
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"opus".to_string()));
    }

    #[test]
    fn test_build_prd_command_without_model() {
        let config = test_config_no_model("claude", "github");
        let (_, args) = build_prd_command(&config, "prompt", None);
        assert!(!args.contains(&"--model".to_string()));
    }

    #[test]
    fn test_build_prd_command_codex_same_behavior() {
        let config = test_config_codex("codex", "github", Some("gpt-5.3-codex"));
        let (cmd, args) = build_prd_command(&config, "rendered prompt", Some("add auth"));
        assert_eq!(cmd, "codex");
        let positional = args.last().unwrap();
        assert!(positional.contains("rendered prompt"));
        assert!(positional.contains("## Desired Objective"));
        assert!(positional.contains("add auth"));
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"gpt-5.3-codex".to_string()));
    }

    fn test_config(binary: &str, source: &str, model: Option<&str>) -> Config {
        use crate::config::{default_review_phases, default_review_step};
        use crate::runner::RunnerKind;
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
            agent_binary: binary.to_string(),
            agent_model: model.map(str::to_string),
            agent_timeout: Some(600),
            agent_effort: Some("high".to_string()),
            agent_variant: None,
            max_review_rounds: 3,
            agent_timeout_retries: 2,
            review_phases: default_review_phases(),
            review_aggregate: default_review_step("review-aggregate"),
            review_fix: default_review_step("review-fix"),
            linear: None,
        }
    }

    fn test_config_no_model(binary: &str, source: &str) -> Config {
        Config {
            agent_model: None,
            ..test_config(binary, source, None)
        }
    }

    fn test_config_codex(binary: &str, source: &str, model: Option<&str>) -> Config {
        use crate::runner::RunnerKind;
        Config {
            runner: RunnerKind::Codex,
            agent_binary: binary.to_string(),
            agent_model: model.map(str::to_string),
            ..test_config("codex", source, model)
        }
    }
}
