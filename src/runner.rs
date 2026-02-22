use std::fmt;
use std::path::Path;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::process::{ProcessConfig, spawn_and_stream};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Choose,
    Implement,
    Review,
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Phase::Choose => write!(f, "choose"),
            Phase::Implement => write!(f, "implement"),
            Phase::Review => write!(f, "review"),
        }
    }
}

#[derive(Debug)]
pub struct RunResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait AgentRunner {
    /// Run the agent for a given phase with a prompt in a working directory.
    fn run(
        &self,
        phase: Phase,
        prompt: &str,
        working_dir: &Path,
    ) -> impl std::future::Future<Output = Result<RunResult>> + Send;
}

/// Bare Claude runner — invokes the claude CLI directly.
pub struct BareClaudeRunner {
    agent_binary: String,
    model: Option<String>,
    timeout: Option<Duration>,
}

impl BareClaudeRunner {
    pub fn new(agent_binary: String, model: Option<String>, timeout: Option<Duration>) -> Self {
        Self {
            agent_binary,
            model,
            timeout,
        }
    }

    /// Build the command and arguments for a given phase and prompt.
    pub fn build_command(&self, prompt: &str) -> (String, Vec<String>) {
        let mut args = vec![
            "--print".to_string(),
            "--verbose".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--dangerously-skip-permissions".to_string(),
        ];

        if let Some(ref model) = self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }

        args.push("-p".to_string());
        args.push(prompt.to_string());

        (self.agent_binary.clone(), args)
    }
}

impl AgentRunner for BareClaudeRunner {
    async fn run(&self, phase: Phase, prompt: &str, working_dir: &Path) -> Result<RunResult> {
        let (command, args) = self.build_command(prompt);

        let config = ProcessConfig {
            command,
            args,
            working_dir: working_dir.to_path_buf(),
            timeout: self.timeout,
            log_prefix: format!("agent:{phase}"),
            env: vec![],
            stdin_data: None,
        };

        let output = spawn_and_stream(config).await?;

        let stdout = output.stdout_lines.join("\n");
        let stderr = output.stderr_lines.join("\n");

        if let Some(sig) = output.signal {
            return Err(Error::AgentRunner(format!("agent killed by signal {sig}")));
        }

        if output.exit_code != 0 {
            return Err(Error::AgentRunner(format!(
                "agent exited with code {}",
                output.exit_code
            )));
        }

        Ok(RunResult {
            exit_code: output.exit_code,
            stdout,
            stderr,
        })
    }
}

/// Enum dispatching to either Claude or Codex runner.
pub enum AnyRunner {
    Claude(BareClaudeRunner),
    Codex(CodexRunner),
}

impl AgentRunner for AnyRunner {
    async fn run(&self, phase: Phase, prompt: &str, working_dir: &Path) -> Result<RunResult> {
        match self {
            AnyRunner::Claude(r) => r.run(phase, prompt, working_dir).await,
            AnyRunner::Codex(r) => r.run(phase, prompt, working_dir).await,
        }
    }
}

/// Codex runner — invokes the OpenAI Codex CLI.
pub struct CodexRunner {
    agent_binary: String,
    model: Option<String>,
    timeout: Option<Duration>,
}

impl CodexRunner {
    pub fn new(agent_binary: String, model: Option<String>, timeout: Option<Duration>) -> Self {
        Self {
            agent_binary,
            model,
            timeout,
        }
    }

    /// Build the command and arguments for codex invocation.
    pub fn build_command(&self) -> (String, Vec<String>) {
        let mut args = vec!["--quiet".to_string(), "--full-auto".to_string()];

        if let Some(ref model) = self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }

        (self.agent_binary.clone(), args)
    }
}

impl AgentRunner for CodexRunner {
    async fn run(&self, phase: Phase, prompt: &str, working_dir: &Path) -> Result<RunResult> {
        let (command, args) = self.build_command();

        let config = ProcessConfig {
            command,
            args,
            working_dir: working_dir.to_path_buf(),
            timeout: self.timeout,
            log_prefix: format!("agent:{phase}"),
            env: vec![],
            stdin_data: Some(prompt.to_string()),
        };

        let output = spawn_and_stream(config).await?;

        let stdout = output.stdout_lines.join("\n");
        let stderr = output.stderr_lines.join("\n");

        if let Some(sig) = output.signal {
            return Err(Error::AgentRunner(format!("agent killed by signal {sig}")));
        }

        if output.exit_code != 0 {
            return Err(Error::AgentRunner(format!(
                "agent exited with code {}",
                output.exit_code
            )));
        }

        Ok(RunResult {
            exit_code: output.exit_code,
            stdout,
            stderr,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_command_defaults() {
        let runner = BareClaudeRunner::new("claude".to_string(), None, None);
        let (cmd, args) = runner.build_command("do something");
        assert_eq!(cmd, "claude");
        assert!(args.contains(&"--print".to_string()));
        assert!(args.contains(&"--output-format".to_string()));
        assert!(args.contains(&"stream-json".to_string()));
        assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"do something".to_string()));
        // No --model when not configured
        assert!(!args.contains(&"--model".to_string()));
    }

    #[test]
    fn test_build_command_with_model() {
        let runner = BareClaudeRunner::new("claude".to_string(), Some("opus".to_string()), None);
        let (_cmd, args) = runner.build_command("pick a task");
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"opus".to_string()));
    }

    #[test]
    fn test_build_command_custom_binary() {
        let runner = BareClaudeRunner::new("/usr/local/bin/my-agent".to_string(), None, None);
        let (cmd, _args) = runner.build_command("review code");
        assert_eq!(cmd, "/usr/local/bin/my-agent");
    }

    #[test]
    fn test_phase_display() {
        assert_eq!(Phase::Choose.to_string(), "choose");
        assert_eq!(Phase::Implement.to_string(), "implement");
        assert_eq!(Phase::Review.to_string(), "review");
    }

    #[test]
    fn test_codex_build_command_defaults() {
        let runner = CodexRunner::new("codex".to_string(), None, None);
        let (cmd, args) = runner.build_command();
        assert_eq!(cmd, "codex");
        assert!(args.contains(&"--quiet".to_string()));
        assert!(args.contains(&"--full-auto".to_string()));
        assert!(!args.contains(&"--model".to_string()));
    }

    #[test]
    fn test_codex_build_command_with_model() {
        let runner = CodexRunner::new("codex".to_string(), Some("o3".to_string()), None);
        let (_cmd, args) = runner.build_command();
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"o3".to_string()));
    }

    #[test]
    fn test_codex_build_command_custom_binary() {
        let runner = CodexRunner::new("/usr/local/bin/codex".to_string(), None, None);
        let (cmd, _args) = runner.build_command();
        assert_eq!(cmd, "/usr/local/bin/codex");
    }
}
