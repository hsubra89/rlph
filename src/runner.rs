use std::fmt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use crate::error::{Error, Result};
use crate::process::{ProcessConfig, spawn_and_stream};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Choose,
    Implement,
    Review,
    ReviewAggregate,
    ReviewFix,
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Phase::Choose => write!(f, "choose"),
            Phase::Implement => write!(f, "implement"),
            Phase::Review => write!(f, "review"),
            Phase::ReviewAggregate => write!(f, "review-aggregate"),
            Phase::ReviewFix => write!(f, "review-fix"),
        }
    }
}

/// Build an `AnyRunner` from config values.
pub fn build_runner(
    runner: &str,
    agent_binary: &str,
    model: Option<&str>,
    effort: Option<&str>,
    timeout: Option<Duration>,
    timeout_retries: u32,
) -> AnyRunner {
    match runner {
        "codex" => AnyRunner::Codex(CodexRunner::new(
            agent_binary.to_string(),
            model.map(str::to_string),
            timeout,
            timeout_retries,
        )),
        _ => AnyRunner::Claude(ClaudeRunner::new(
            agent_binary.to_string(),
            model.map(str::to_string),
            effort.map(str::to_string),
            timeout,
            timeout_retries,
        )),
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

/// Claude runner — invokes the claude CLI directly.
pub struct ClaudeRunner {
    agent_binary: String,
    model: Option<String>,
    effort: Option<String>,
    timeout: Option<Duration>,
    max_timeout_retries: u32,
}

impl ClaudeRunner {
    pub fn new(
        agent_binary: String,
        model: Option<String>,
        effort: Option<String>,
        timeout: Option<Duration>,
        max_timeout_retries: u32,
    ) -> Self {
        Self {
            agent_binary,
            model,
            effort,
            timeout,
            max_timeout_retries,
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

        if let Some(ref effort) = self.effort {
            args.push("--effort".to_string());
            args.push(effort.clone());
        }

        args.push("-p".to_string());
        args.push(prompt.to_string());

        (self.agent_binary.clone(), args)
    }

    /// Build a resume command for a timed-out session.
    pub fn build_resume_command(&self, session_id: &str) -> (String, Vec<String>) {
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

        if let Some(ref effort) = self.effort {
            args.push("--effort".to_string());
            args.push(effort.clone());
        }

        args.push("--resume".to_string());
        args.push(session_id.to_string());

        (self.agent_binary.clone(), args)
    }
}

/// Extract session_id from stream-json stdout lines.
///
/// Scans lines for JSON objects with a top-level `session_id` field.
/// Returns the last one found (most recent).
pub fn extract_session_id(stdout_lines: &[String]) -> Option<String> {
    let mut last_id = None;
    for line in stdout_lines {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line)
            && let Some(id) = val.get("session_id").and_then(|v| v.as_str())
            && !id.is_empty()
        {
            last_id = Some(id.to_string());
        }
    }
    last_id
}

impl AgentRunner for ClaudeRunner {
    async fn run(&self, phase: Phase, prompt: &str, working_dir: &Path) -> Result<RunResult> {
        let log_prefix = format!("agent:{phase}");
        let max_attempts = 1 + self.max_timeout_retries;
        let mut all_stdout: Vec<String> = Vec::new();
        let mut all_stderr: Vec<String> = Vec::new();

        for attempt in 0..max_attempts {
            let (command, args) = if attempt == 0 {
                self.build_command(prompt)
            } else {
                // On retry, try to resume from session_id in previous output.
                let session_id = match extract_session_id(&all_stdout) {
                    Some(id) => id,
                    None => {
                        warn!(
                            "[{log_prefix}] timeout retry {attempt}: no session_id found in output, cannot resume"
                        );
                        return Err(Error::AgentRunner(
                            "agent timed out and no session_id found for resume".to_string(),
                        ));
                    }
                };
                info!(
                    "[{log_prefix}] timeout retry {attempt}/{max_attempts}: resuming session {session_id}"
                );
                eprintln!(
                    "[{log_prefix}] resuming timed-out session {session_id} (attempt {}/{})",
                    attempt + 1,
                    max_attempts
                );
                self.build_resume_command(&session_id)
            };

            let config = ProcessConfig {
                command,
                args,
                working_dir: working_dir.to_path_buf(),
                timeout: self.timeout,
                log_prefix: log_prefix.clone(),
                env: vec![],
                stdin_data: None,
            };

            match spawn_and_stream(config).await {
                Ok(output) => {
                    all_stdout.extend(output.stdout_lines);
                    all_stderr.extend(output.stderr_lines);

                    let stdout = all_stdout.join("\n");
                    let stderr = all_stderr.join("\n");

                    if let Some(sig) = output.signal {
                        return Err(Error::AgentRunner(format!("agent killed by signal {sig}")));
                    }

                    if output.exit_code != 0 {
                        return Err(Error::AgentRunner(format!(
                            "agent exited with code {}",
                            output.exit_code
                        )));
                    }

                    return Ok(RunResult {
                        exit_code: output.exit_code,
                        stdout,
                        stderr,
                    });
                }
                Err(Error::ProcessTimeout {
                    timeout,
                    stdout_lines,
                    stderr_lines,
                }) => {
                    all_stdout.extend(stdout_lines);
                    all_stderr.extend(stderr_lines);
                    warn!(
                        "[{log_prefix}] attempt {} timed out after {timeout:?} ({} stdout lines buffered)",
                        attempt + 1,
                        all_stdout.len()
                    );
                    // Continue to next attempt (or fall through if last).
                }
                Err(e) => return Err(e),
            }
        }

        // All attempts exhausted.
        Err(Error::AgentRunner(format!(
            "agent timed out after {max_attempts} attempts"
        )))
    }
}

/// Type alias for a callback function used in `CallbackRunner`.
pub type RunnerCallbackFn = dyn Fn(Phase, String, PathBuf) -> Pin<Box<dyn std::future::Future<Output = Result<RunResult>> + Send>>
    + Send
    + Sync;

/// A runner backed by a callback function, primarily for testing.
pub struct CallbackRunner {
    callback: Arc<RunnerCallbackFn>,
}

impl CallbackRunner {
    pub fn new(
        callback: Arc<RunnerCallbackFn>,
    ) -> Self {
        Self { callback }
    }
}

impl AgentRunner for CallbackRunner {
    async fn run(&self, phase: Phase, prompt: &str, working_dir: &Path) -> Result<RunResult> {
        let fut = (self.callback)(phase, prompt.to_string(), working_dir.to_path_buf());
        fut.await
    }
}

/// Enum dispatching to either Claude, Codex, or callback runner.
pub enum AnyRunner {
    Claude(ClaudeRunner),
    Codex(CodexRunner),
    Callback(CallbackRunner),
}

impl AgentRunner for AnyRunner {
    async fn run(&self, phase: Phase, prompt: &str, working_dir: &Path) -> Result<RunResult> {
        match self {
            AnyRunner::Claude(r) => r.run(phase, prompt, working_dir).await,
            AnyRunner::Codex(r) => r.run(phase, prompt, working_dir).await,
            AnyRunner::Callback(r) => r.run(phase, prompt, working_dir).await,
        }
    }
}

/// Codex runner — invokes the OpenAI Codex CLI.
pub struct CodexRunner {
    agent_binary: String,
    model: Option<String>,
    timeout: Option<Duration>,
    max_timeout_retries: u32,
}

impl CodexRunner {
    pub fn new(
        agent_binary: String,
        model: Option<String>,
        timeout: Option<Duration>,
        max_timeout_retries: u32,
    ) -> Self {
        Self {
            agent_binary,
            model,
            timeout,
            max_timeout_retries,
        }
    }

    /// Build the command and arguments for codex invocation.
    pub fn build_command(&self) -> (String, Vec<String>) {
        let mut args = vec![
            "exec".to_string(),
            "--dangerously-bypass-approvals-and-sandbox".to_string(),
        ];

        if let Some(ref model) = self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        args.push("-".to_string());

        (self.agent_binary.clone(), args)
    }

    /// Build a resume command for a timed-out session.
    /// Uses `codex exec resume --last` which resumes the most recent session
    /// scoped to the current working directory.
    pub fn build_resume_command(&self) -> (String, Vec<String>) {
        let mut args = vec![
            "exec".to_string(),
            "--dangerously-bypass-approvals-and-sandbox".to_string(),
        ];

        if let Some(ref model) = self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }

        args.push("resume".to_string());
        args.push("--last".to_string());

        (self.agent_binary.clone(), args)
    }
}

impl AgentRunner for CodexRunner {
    async fn run(&self, phase: Phase, prompt: &str, working_dir: &Path) -> Result<RunResult> {
        let log_prefix = format!("agent:{phase}");
        let max_attempts = 1 + self.max_timeout_retries;
        let mut all_stdout: Vec<String> = Vec::new();
        let mut all_stderr: Vec<String> = Vec::new();

        for attempt in 0..max_attempts {
            let (command, args, stdin_data) = if attempt == 0 {
                let (cmd, a) = self.build_command();
                (cmd, a, Some(prompt.to_string()))
            } else {
                info!(
                    "[{log_prefix}] timeout retry {attempt}/{max_attempts}: resuming last session"
                );
                eprintln!(
                    "[{log_prefix}] resuming timed-out session (attempt {}/{})",
                    attempt + 1,
                    max_attempts
                );
                let (cmd, a) = self.build_resume_command();
                (cmd, a, None)
            };

            let config = ProcessConfig {
                command,
                args,
                working_dir: working_dir.to_path_buf(),
                timeout: self.timeout,
                log_prefix: log_prefix.clone(),
                env: vec![],
                stdin_data,
            };

            match spawn_and_stream(config).await {
                Ok(output) => {
                    all_stdout.extend(output.stdout_lines);
                    all_stderr.extend(output.stderr_lines);

                    let stdout = all_stdout.join("\n");
                    let stderr = all_stderr.join("\n");

                    if let Some(sig) = output.signal {
                        return Err(Error::AgentRunner(format!("agent killed by signal {sig}")));
                    }

                    if output.exit_code != 0 {
                        return Err(Error::AgentRunner(format!(
                            "agent exited with code {}",
                            output.exit_code
                        )));
                    }

                    return Ok(RunResult {
                        exit_code: output.exit_code,
                        stdout,
                        stderr,
                    });
                }
                Err(Error::ProcessTimeout {
                    timeout,
                    stdout_lines,
                    stderr_lines,
                }) => {
                    all_stdout.extend(stdout_lines);
                    all_stderr.extend(stderr_lines);
                    warn!(
                        "[{log_prefix}] attempt {} timed out after {timeout:?} ({} stdout lines buffered)",
                        attempt + 1,
                        all_stdout.len()
                    );
                }
                Err(e) => return Err(e),
            }
        }

        Err(Error::AgentRunner(format!(
            "agent timed out after {max_attempts} attempts"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_command_defaults() {
        let runner = ClaudeRunner::new("claude".to_string(), None, None, None, 2);
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
        let runner = ClaudeRunner::new(
            "claude".to_string(),
            Some("opus".to_string()),
            None,
            None,
            2,
        );
        let (_cmd, args) = runner.build_command("pick a task");
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"opus".to_string()));
    }

    #[test]
    fn test_build_command_custom_binary() {
        let runner = ClaudeRunner::new("/usr/local/bin/my-agent".to_string(), None, None, None, 2);
        let (cmd, _args) = runner.build_command("review code");
        assert_eq!(cmd, "/usr/local/bin/my-agent");
    }

    #[test]
    fn test_build_resume_command_has_resume_flag() {
        let runner = ClaudeRunner::new("claude".to_string(), None, None, None, 2);
        let (_cmd, args) = runner.build_resume_command("sess-abc-123");
        assert!(args.contains(&"--resume".to_string()));
        assert!(args.contains(&"sess-abc-123".to_string()));
        // Must NOT have -p flag
        assert!(!args.contains(&"-p".to_string()));
        // Must still have common flags
        assert!(args.contains(&"--print".to_string()));
        assert!(args.contains(&"--verbose".to_string()));
        assert!(args.contains(&"--output-format".to_string()));
        assert!(args.contains(&"stream-json".to_string()));
        assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
    }

    #[test]
    fn test_build_resume_command_with_model() {
        let runner = ClaudeRunner::new(
            "claude".to_string(),
            Some("opus".to_string()),
            None,
            None,
            2,
        );
        let (_cmd, args) = runner.build_resume_command("sess-xyz");
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"opus".to_string()));
        assert!(args.contains(&"--resume".to_string()));
        assert!(args.contains(&"sess-xyz".to_string()));
    }

    #[test]
    fn test_extract_session_id_from_stream_json() {
        let lines = vec![
            r#"{"type":"user","message":{"role":"user"},"session_id":"abc-123"}"#.to_string(),
            r#"{"type":"assistant","message":{"role":"assistant"},"session_id":"abc-123"}"#
                .to_string(),
        ];
        assert_eq!(extract_session_id(&lines), Some("abc-123".to_string()));
    }

    #[test]
    fn test_extract_session_id_returns_last() {
        let lines = vec![
            r#"{"session_id":"first"}"#.to_string(),
            r#"{"session_id":"second"}"#.to_string(),
        ];
        assert_eq!(extract_session_id(&lines), Some("second".to_string()));
    }

    #[test]
    fn test_extract_session_id_none_for_empty() {
        assert_eq!(extract_session_id(&[]), None);
    }

    #[test]
    fn test_extract_session_id_none_for_non_json() {
        let lines = vec!["not json".to_string(), "also not json".to_string()];
        assert_eq!(extract_session_id(&lines), None);
    }

    #[test]
    fn test_extract_session_id_none_for_empty_id() {
        let lines = vec![r#"{"session_id":""}"#.to_string()];
        assert_eq!(extract_session_id(&lines), None);
    }

    #[test]
    fn test_extract_session_id_skips_missing_field() {
        let lines = vec![
            r#"{"type":"system","message":"hello"}"#.to_string(),
            r#"{"type":"user","session_id":"found-it"}"#.to_string(),
        ];
        assert_eq!(extract_session_id(&lines), Some("found-it".to_string()));
    }

    #[test]
    fn test_phase_display() {
        assert_eq!(Phase::Choose.to_string(), "choose");
        assert_eq!(Phase::Implement.to_string(), "implement");
        assert_eq!(Phase::Review.to_string(), "review");
        assert_eq!(Phase::ReviewAggregate.to_string(), "review-aggregate");
        assert_eq!(Phase::ReviewFix.to_string(), "review-fix");
    }

    #[test]
    fn test_build_runner_claude() {
        let runner = build_runner("claude", "claude", Some("opus"), Some("high"), None, 2);
        assert!(matches!(runner, AnyRunner::Claude(_)));
    }

    #[test]
    fn test_build_runner_codex() {
        let runner = build_runner("codex", "codex", Some("gpt-5.3"), None, None, 2);
        assert!(matches!(runner, AnyRunner::Codex(_)));
    }

    #[test]
    fn test_codex_build_command_defaults() {
        let runner = CodexRunner::new("codex".to_string(), None, None, 2);
        let (cmd, args) = runner.build_command();
        assert_eq!(cmd, "codex");
        assert!(args.contains(&"exec".to_string()));
        assert!(args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        assert!(args.contains(&"-".to_string()));
        assert!(!args.contains(&"--model".to_string()));
    }

    #[test]
    fn test_codex_build_command_with_model() {
        let runner = CodexRunner::new("codex".to_string(), Some("o3".to_string()), None, 2);
        let (_cmd, args) = runner.build_command();
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"o3".to_string()));
    }

    #[test]
    fn test_codex_build_command_custom_binary() {
        let runner = CodexRunner::new("/usr/local/bin/codex".to_string(), None, None, 2);
        let (cmd, _args) = runner.build_command();
        assert_eq!(cmd, "/usr/local/bin/codex");
    }

    #[test]
    fn test_codex_build_resume_command() {
        let runner = CodexRunner::new("codex".to_string(), None, None, 2);
        let (cmd, args) = runner.build_resume_command();
        assert_eq!(cmd, "codex");
        assert!(args.contains(&"exec".to_string()));
        assert!(args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        assert!(args.contains(&"resume".to_string()));
        assert!(args.contains(&"--last".to_string()));
        // Must NOT have `-` stdin marker
        assert!(!args.contains(&"-".to_string()));
    }

    #[test]
    fn test_codex_build_resume_command_with_model() {
        let runner = CodexRunner::new("codex".to_string(), Some("gpt-5.3".to_string()), None, 2);
        let (_cmd, args) = runner.build_resume_command();
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"gpt-5.3".to_string()));
        assert!(args.contains(&"resume".to_string()));
        assert!(args.contains(&"--last".to_string()));
    }
}
