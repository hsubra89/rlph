use std::collections::HashMap;
use std::process::Stdio;

use tracing::info;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::prompts::PromptEngine;

/// Build the submission instructions string for the given task source.
pub fn submission_instructions(source: &str) -> &'static str {
    match source {
        "github" => concat!(
            "Submit the final PRD as a GitHub issue using the `gh` CLI:\n",
            "```\n",
            "gh issue create --title \"PRD: <title>\" --body \"<prd content>\"\n",
            "```\n",
            "Use a HEREDOC for the body if it contains special characters.\n",
            "Add the label `rlph` to the issue so the autonomous loop can pick it up.",
        ),
        "linear" => concat!(
            "Submit the final PRD as a Linear project/issue.\n",
            "Use the Linear CLI or API to create the issue with the PRD as its description.\n",
            "Ensure it is placed in the correct team and project.",
        ),
        _ => "Submit the final PRD to your configured task source.",
    }
}

/// Build the agent command for an interactive plan session.
///
/// Returns `(binary, args)` suitable for spawning with inherited stdio.
pub fn build_plan_command(
    config: &Config,
    rendered_prompt: &str,
    description: Option<&str>,
) -> (String, Vec<String>) {
    let mut args = Vec::new();

    args.push("--append-system-prompt".to_string());
    args.push(rendered_prompt.to_string());

    if let Some(ref model) = config.agent_model {
        args.push("--model".to_string());
        args.push(model.clone());
    }

    if let Some(desc) = description {
        args.push("-p".to_string());
        args.push(desc.to_string());
    }

    (config.agent_binary.clone(), args)
}

/// Run an interactive plan session.
///
/// Launches the configured agent with the plan prompt and inherited stdio.
/// Blocks until the agent exits, then propagates the exit code.
pub async fn run_plan(config: &Config, description: Option<&str>) -> Result<i32> {
    let engine = PromptEngine::new(None);

    let mut vars = HashMap::new();
    vars.insert(
        "submission_instructions".to_string(),
        submission_instructions(&config.source).to_string(),
    );
    vars.insert(
        "description".to_string(),
        description.unwrap_or("").to_string(),
    );

    let rendered = engine.render_phase("plan", &vars)?;

    let (binary, args) = build_plan_command(config, &rendered, description);

    info!(
        binary = %binary,
        args = ?args.iter().take(3).collect::<Vec<_>>(),
        "launching interactive plan session"
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
        let instr = submission_instructions("github");
        assert!(instr.contains("gh issue create"));
        assert!(instr.contains("rlph"));
    }

    #[test]
    fn test_submission_instructions_linear() {
        let instr = submission_instructions("linear");
        assert!(instr.contains("Linear"));
    }

    #[test]
    fn test_submission_instructions_unknown_source() {
        let instr = submission_instructions("jira");
        assert!(instr.contains("configured task source"));
    }

    #[test]
    fn test_build_plan_command_basic() {
        let config = test_config("claude", "github", None);
        let (cmd, args) = build_plan_command(&config, "rendered prompt", None);
        assert_eq!(cmd, "claude");
        assert!(args.contains(&"--append-system-prompt".to_string()));
        assert!(args.contains(&"rendered prompt".to_string()));
        assert!(!args.contains(&"-p".to_string()));
    }

    #[test]
    fn test_build_plan_command_with_description() {
        let config = test_config("claude", "github", None);
        let (_, args) = build_plan_command(&config, "prompt", Some("add auth"));
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"add auth".to_string()));
    }

    #[test]
    fn test_build_plan_command_without_description() {
        let config = test_config("claude", "github", None);
        let (_, args) = build_plan_command(&config, "prompt", None);
        assert!(!args.contains(&"-p".to_string()));
    }

    #[test]
    fn test_build_plan_command_with_model() {
        let config = test_config("claude", "github", Some("opus"));
        let (_, args) = build_plan_command(&config, "prompt", None);
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"opus".to_string()));
    }

    #[test]
    fn test_build_plan_command_without_model() {
        let config = test_config_no_model("claude", "github");
        let (_, args) = build_plan_command(&config, "prompt", None);
        assert!(!args.contains(&"--model".to_string()));
    }

    fn test_config(binary: &str, source: &str, model: Option<&str>) -> Config {
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
            agent_binary: binary.to_string(),
            agent_model: model.map(str::to_string),
            agent_timeout: Some(600),
            agent_effort: Some("high".to_string()),
            max_review_rounds: 3,
            agent_timeout_retries: 2,
        }
    }

    fn test_config_no_model(binary: &str, source: &str) -> Config {
        Config {
            agent_model: None,
            ..test_config(binary, source, None)
        }
    }
}
