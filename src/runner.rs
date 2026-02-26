use std::fmt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tracing::warn;

use crate::error::{Error, Result};
use crate::process::{ProcessConfig, spawn_and_stream};

/// Which agent backend to dispatch to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunnerKind {
    Claude,
    Codex,
}

impl fmt::Display for RunnerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunnerKind::Claude => write!(f, "claude"),
            RunnerKind::Codex => write!(f, "codex"),
        }
    }
}

impl FromStr for RunnerKind {
    type Err = Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "claude" => Ok(RunnerKind::Claude),
            "codex" => Ok(RunnerKind::Codex),
            other => Err(Error::ConfigValidation(format!(
                "unknown runner: {other} (expected: claude, codex)"
            ))),
        }
    }
}

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
    runner: RunnerKind,
    agent_binary: &str,
    model: Option<&str>,
    effort: Option<&str>,
    timeout: Option<Duration>,
    timeout_retries: u32,
) -> AnyRunner {
    match runner {
        RunnerKind::Codex => AnyRunner::Codex(CodexRunner::new(
            agent_binary.to_string(),
            model.map(str::to_string),
            effort.map(str::to_string),
            timeout,
            timeout_retries,
        )),
        RunnerKind::Claude => AnyRunner::Claude(ClaudeRunner::new(
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
    pub session_id: Option<String>,
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

/// Build the base Claude CLI flags shared by all command builders.
///
/// Returns: `[--print, --verbose, --output-format, stream-json, --dangerously-skip-permissions]`
/// plus optional `--model` and `--effort`.
fn base_claude_args(model: Option<&str>, effort: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "--print".to_string(),
        "--verbose".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--dangerously-skip-permissions".to_string(),
    ];

    if let Some(model) = model {
        args.push("--model".to_string());
        args.push(model.to_string());
    }

    if let Some(effort) = effort {
        args.push("--effort".to_string());
        args.push(effort.to_string());
    }

    args
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
        let mut args = base_claude_args(self.model.as_deref(), self.effort.as_deref());
        args.push("-p".to_string());
        args.push(prompt.to_string());
        (self.agent_binary.clone(), args)
    }

    /// Build a resume command for a timed-out session.
    pub fn build_resume_command(&self, session_id: &str) -> (String, Vec<String>) {
        let mut args = base_claude_args(self.model.as_deref(), self.effort.as_deref());
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

/// Extract the final human-readable result from Claude stream-json output.
///
/// Claude emits many JSON events when using `--output-format stream-json`.
/// The useful summary is stored in `{"type":"result","result":"..."}`.
/// If found, returning this keeps downstream prompts/comments compact.
fn extract_claude_result(stdout_lines: &[String]) -> Option<String> {
    let mut last_result: Option<String> = None;
    for line in stdout_lines {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if val.get("type").and_then(|v| v.as_str()) == Some("result")
            && let Some(result) = val.get("result").and_then(|v| v.as_str())
        {
            last_result = Some(result.to_string());
        }
    }
    last_result
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
                        warn!(prefix = %log_prefix, attempt, "timeout retry: no session_id found, cannot resume");
                        return Err(Error::AgentRunner(
                            "agent timed out and no session_id found for resume".to_string(),
                        ));
                    }
                };
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
                stream_output: false,
                env: vec![],
                stdin_data: None,
                quiet: true,
            };

            match spawn_and_stream(config).await {
                Ok(output) => {
                    all_stdout.extend(output.stdout_lines);
                    all_stderr.extend(output.stderr_lines);

                    let stdout =
                        extract_claude_result(&all_stdout).unwrap_or_else(|| all_stdout.join("\n"));
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

                    let session_id = extract_session_id(&all_stdout);

                    return Ok(RunResult {
                        exit_code: output.exit_code,
                        stdout,
                        stderr,
                        session_id,
                    });
                }
                Err(Error::ProcessTimeout {
                    timeout,
                    stdout_lines,
                    stderr_lines,
                }) => {
                    all_stdout.extend(stdout_lines);
                    all_stderr.extend(stderr_lines);
                    warn!(prefix = %log_prefix, attempt = attempt + 1, ?timeout, buffered = all_stdout.len(), "attempt timed out");
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

/// Build a Claude resume-with-prompt command for an existing session.
///
/// Unlike `build_resume_command` (which just resumes), this sends a new user message
/// to the session — used to send correction prompts for malformed JSON recovery.
pub fn build_claude_resume_with_prompt_command(
    agent_binary: &str,
    model: Option<&str>,
    effort: Option<&str>,
    session_id: &str,
    prompt: &str,
) -> (String, Vec<String>) {
    let mut args = base_claude_args(model, effort);
    args.push("--resume".to_string());
    args.push(session_id.to_string());
    args.push("-p".to_string());
    args.push(prompt.to_string());
    (agent_binary.to_string(), args)
}

/// Build a Codex resume-with-prompt command for an existing thread.
///
/// Uses `codex exec resume <thread_id> -` with the correction prompt
/// delivered via stdin.
pub fn build_codex_resume_with_prompt_command(
    agent_binary: &str,
    model: Option<&str>,
    effort: Option<&str>,
    thread_id: &str,
) -> (String, Vec<String>) {
    let mut args = base_codex_args(model, effort);
    args.push("resume".to_string());
    args.push(thread_id.to_string());
    args.push("-".to_string());
    (agent_binary.to_string(), args)
}

/// Resume an existing agent session with a correction prompt.
///
/// Sends a new user message to the session (e.g., a JSON correction prompt)
/// and returns the agent's new output. Used by the orchestrator to recover
/// from malformed JSON without restarting the entire agent.
///
/// The `runner_type` parameter selects the appropriate command builder and
/// result extractor: `Claude` uses Claude CLI flags, `Codex` uses Codex
/// CLI flags with stdin delivery.
#[allow(clippy::too_many_arguments)]
pub async fn resume_with_correction(
    runner_type: RunnerKind,
    agent_binary: &str,
    model: Option<&str>,
    effort: Option<&str>,
    session_id: &str,
    correction_prompt: &str,
    working_dir: &Path,
    timeout: Option<Duration>,
) -> Result<RunResult> {
    let (command, args, stdin_data) = match runner_type {
        RunnerKind::Codex => {
            let (cmd, a) = build_codex_resume_with_prompt_command(
                agent_binary,
                model,
                effort,
                session_id,
            );
            (cmd, a, Some(correction_prompt.to_string()))
        }
        RunnerKind::Claude => {
            let (cmd, a) = build_claude_resume_with_prompt_command(
                agent_binary,
                model,
                effort,
                session_id,
                correction_prompt,
            );
            (cmd, a, None)
        }
    };

    let config = ProcessConfig {
        command,
        args,
        working_dir: working_dir.to_path_buf(),
        timeout,
        log_prefix: "agent:correction".to_string(),
        stream_output: false,
        env: vec![],
        stdin_data,
        quiet: true,
    };

    let output = spawn_and_stream(config).await?;

    let (stdout, session_id) = match runner_type {
        RunnerKind::Codex => (
            extract_codex_result(&output.stdout_lines)
                .unwrap_or_else(|| output.stdout_lines.join("\n")),
            extract_thread_id(&output.stdout_lines),
        ),
        RunnerKind::Claude => (
            extract_claude_result(&output.stdout_lines)
                .unwrap_or_else(|| output.stdout_lines.join("\n")),
            extract_session_id(&output.stdout_lines),
        ),
    };
    let stderr = output.stderr_lines.join("\n");

    if let Some(sig) = output.signal {
        return Err(Error::AgentRunner(format!(
            "correction agent killed by signal {sig}"
        )));
    }

    if output.exit_code != 0 {
        return Err(Error::AgentRunner(format!(
            "correction agent exited with code {}",
            output.exit_code
        )));
    }

    Ok(RunResult {
        exit_code: output.exit_code,
        stdout,
        stderr,
        session_id,
    })
}

/// Type alias for a callback function used in `CallbackRunner`.
pub type RunnerCallbackFn = dyn Fn(
        Phase,
        String,
        PathBuf,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<RunResult>> + Send>>
    + Send
    + Sync;

/// A runner backed by a callback function, primarily for testing.
pub struct CallbackRunner {
    callback: Arc<RunnerCallbackFn>,
}

impl CallbackRunner {
    pub fn new(callback: Arc<RunnerCallbackFn>) -> Self {
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

/// Build the base Codex CLI flags shared by all command builders.
///
/// Returns: `["exec", "--dangerously-bypass-approvals-and-sandbox", "--json"]`
/// plus optional `--model` and `--config model_reasoning_effort`.
fn base_codex_args(model: Option<&str>, effort: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "exec".to_string(),
        "--dangerously-bypass-approvals-and-sandbox".to_string(),
        "--json".to_string(),
    ];

    if let Some(model) = model {
        args.push("--model".to_string());
        args.push(model.to_string());
    }

    if let Some(effort) = effort {
        args.push("--config".to_string());
        args.push(format!("model_reasoning_effort=\"{effort}\""));
    }

    args
}

/// Extract thread_id from Codex JSON output lines.
///
/// Scans lines for JSON objects with a top-level `thread_id` field.
/// Returns the last one found (most recent).
pub fn extract_thread_id(stdout_lines: &[String]) -> Option<String> {
    let mut last_id = None;
    for line in stdout_lines {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line)
            && let Some(id) = val.get("thread_id").and_then(|v| v.as_str())
            && !id.is_empty()
        {
            last_id = Some(id.to_string());
        }
    }
    last_id
}

/// Extract the final human-readable result from Codex JSON output.
///
/// Codex emits JSON events when using `--json`. The useful output is in
/// `{"type":"item.completed","item":{"type":"agent_message","text":"..."}}`.
/// Concatenates all agent_message texts, returning the last one found.
fn extract_codex_result(stdout_lines: &[String]) -> Option<String> {
    let mut last_text: Option<String> = None;
    for line in stdout_lines {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(item) = val.get("item")
            && item.get("type").and_then(|v| v.as_str()) == Some("agent_message")
            && let Some(text) = item.get("text").and_then(|v| v.as_str())
        {
            last_text = Some(text.to_string());
        }
    }
    last_text
}

/// Codex runner — invokes the OpenAI Codex CLI.
pub struct CodexRunner {
    agent_binary: String,
    model: Option<String>,
    effort: Option<String>,
    timeout: Option<Duration>,
    max_timeout_retries: u32,
}

impl CodexRunner {
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

    /// Build the command and arguments for codex invocation.
    pub fn build_command(&self) -> (String, Vec<String>) {
        let mut args =
            base_codex_args(self.model.as_deref(), self.effort.as_deref());
        args.push("-".to_string());
        (self.agent_binary.clone(), args)
    }

    /// Build a resume command for a timed-out session.
    /// Uses `codex exec resume --last` which resumes the most recent session
    /// scoped to the current working directory.
    pub fn build_resume_command(&self) -> (String, Vec<String>) {
        let mut args =
            base_codex_args(self.model.as_deref(), self.effort.as_deref());
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
                stream_output: false,
                env: vec![],
                stdin_data,
                quiet: true,
            };

            match spawn_and_stream(config).await {
                Ok(output) => {
                    all_stdout.extend(output.stdout_lines);
                    all_stderr.extend(output.stderr_lines);

                    let stdout = extract_codex_result(&all_stdout)
                        .unwrap_or_else(|| all_stdout.join("\n"));
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

                    let session_id = extract_thread_id(&all_stdout);

                    return Ok(RunResult {
                        exit_code: output.exit_code,
                        stdout,
                        stderr,
                        session_id,
                    });
                }
                Err(Error::ProcessTimeout {
                    timeout,
                    stdout_lines,
                    stderr_lines,
                }) => {
                    all_stdout.extend(stdout_lines);
                    all_stderr.extend(stderr_lines);
                    warn!(prefix = %log_prefix, attempt = attempt + 1, ?timeout, buffered = all_stdout.len(), "attempt timed out");
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
    fn test_extract_claude_result_prefers_result_event() {
        let lines = vec![
            r#"{"type":"system","session_id":"abc"}"#.to_string(),
            r#"{"type":"assistant","message":"thinking"}"#.to_string(),
            r#"{"type":"result","result":"REVIEW_APPROVED"}"#.to_string(),
        ];
        assert_eq!(
            extract_claude_result(&lines).as_deref(),
            Some("REVIEW_APPROVED")
        );
    }

    #[test]
    fn test_extract_claude_result_returns_last_result_event() {
        let lines = vec![
            r#"{"type":"result","result":"first"}"#.to_string(),
            r#"{"type":"result","result":"second"}"#.to_string(),
        ];
        assert_eq!(extract_claude_result(&lines).as_deref(), Some("second"));
    }

    #[test]
    fn test_extract_claude_result_none_without_result_event() {
        let lines = vec![
            r#"{"type":"system","session_id":"abc"}"#.to_string(),
            r#"{"type":"assistant","message":"noop"}"#.to_string(),
        ];
        assert_eq!(extract_claude_result(&lines), None);
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
        let runner = build_runner(RunnerKind::Claude, "claude", Some("opus"), Some("high"), None, 2);
        assert!(matches!(runner, AnyRunner::Claude(_)));
    }

    #[test]
    fn test_build_runner_codex() {
        let runner = build_runner(RunnerKind::Codex, "codex", Some("gpt-5.3"), None, None, 2);
        assert!(matches!(runner, AnyRunner::Codex(_)));
    }

    #[test]
    fn test_codex_build_command_defaults() {
        let runner = CodexRunner::new("codex".to_string(), None, None, None, 2);
        let (cmd, args) = runner.build_command();
        assert_eq!(cmd, "codex");
        assert!(args.contains(&"exec".to_string()));
        assert!(args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        assert!(args.contains(&"--json".to_string()));
        assert!(args.contains(&"-".to_string()));
        assert!(!args.contains(&"--model".to_string()));
    }

    #[test]
    fn test_codex_build_command_with_model() {
        let runner = CodexRunner::new("codex".to_string(), Some("o3".to_string()), None, None, 2);
        let (_cmd, args) = runner.build_command();
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"o3".to_string()));
    }

    #[test]
    fn test_codex_build_command_custom_binary() {
        let runner = CodexRunner::new("/usr/local/bin/codex".to_string(), None, None, None, 2);
        let (cmd, _args) = runner.build_command();
        assert_eq!(cmd, "/usr/local/bin/codex");
    }

    #[test]
    fn test_codex_build_resume_command() {
        let runner = CodexRunner::new("codex".to_string(), None, None, None, 2);
        let (cmd, args) = runner.build_resume_command();
        assert_eq!(cmd, "codex");
        assert!(args.contains(&"exec".to_string()));
        assert!(args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        assert!(args.contains(&"--json".to_string()));
        assert!(args.contains(&"resume".to_string()));
        assert!(args.contains(&"--last".to_string()));
        // Must NOT have `-` stdin marker
        assert!(!args.contains(&"-".to_string()));
    }

    #[test]
    fn test_codex_build_resume_command_with_model() {
        let runner = CodexRunner::new("codex".to_string(), Some("gpt-5.3".to_string()), None, None, 2);
        let (_cmd, args) = runner.build_resume_command();
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"gpt-5.3".to_string()));
        assert!(args.contains(&"resume".to_string()));
        assert!(args.contains(&"--last".to_string()));
    }

    #[test]
    fn test_build_claude_resume_with_prompt_command_has_both_resume_and_prompt() {
        let (cmd, args) =
            build_claude_resume_with_prompt_command("claude", None, None, "sess-123", "fix your JSON");
        assert_eq!(cmd, "claude");
        assert!(args.contains(&"--resume".to_string()));
        assert!(args.contains(&"sess-123".to_string()));
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"fix your JSON".to_string()));
        // Common flags
        assert!(args.contains(&"--print".to_string()));
        assert!(args.contains(&"--verbose".to_string()));
        assert!(args.contains(&"--output-format".to_string()));
        assert!(args.contains(&"stream-json".to_string()));
        assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
    }

    #[test]
    fn test_build_claude_resume_with_prompt_command_with_model_and_effort() {
        let (_cmd, args) = build_claude_resume_with_prompt_command(
            "claude",
            Some("opus"),
            Some("high"),
            "sess-456",
            "correction",
        );
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"opus".to_string()));
        assert!(args.contains(&"--effort".to_string()));
        assert!(args.contains(&"high".to_string()));
        assert!(args.contains(&"--resume".to_string()));
        assert!(args.contains(&"sess-456".to_string()));
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"correction".to_string()));
    }

    #[test]
    fn test_extract_thread_id_from_json() {
        let lines = vec![
            r#"{"type":"thread.started","thread_id":"019c97dd-d6ce-7642-99c8-3717697fd004"}"#.to_string(),
            r#"{"type":"turn.started"}"#.to_string(),
        ];
        assert_eq!(
            extract_thread_id(&lines),
            Some("019c97dd-d6ce-7642-99c8-3717697fd004".to_string())
        );
    }

    #[test]
    fn test_extract_thread_id_returns_last() {
        let lines = vec![
            r#"{"thread_id":"first"}"#.to_string(),
            r#"{"thread_id":"second"}"#.to_string(),
        ];
        assert_eq!(extract_thread_id(&lines), Some("second".to_string()));
    }

    #[test]
    fn test_extract_thread_id_none_for_empty() {
        assert_eq!(extract_thread_id(&[]), None);
    }

    #[test]
    fn test_extract_thread_id_none_for_non_json() {
        let lines = vec!["not json".to_string()];
        assert_eq!(extract_thread_id(&lines), None);
    }

    #[test]
    fn test_extract_thread_id_none_for_empty_id() {
        let lines = vec![r#"{"thread_id":""}"#.to_string()];
        assert_eq!(extract_thread_id(&lines), None);
    }

    #[test]
    fn test_extract_codex_result_agent_message() {
        let lines = vec![
            r#"{"type":"thread.started","thread_id":"abc"}"#.to_string(),
            r#"{"type":"turn.started"}"#.to_string(),
            r#"{"type":"item.completed","item":{"id":"item_0","type":"reasoning","text":"thinking"}}"#.to_string(),
            r#"{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"hello"}}"#.to_string(),
            r#"{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":10}}"#.to_string(),
        ];
        assert_eq!(extract_codex_result(&lines).as_deref(), Some("hello"));
    }

    #[test]
    fn test_extract_codex_result_returns_last_agent_message() {
        let lines = vec![
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"first"}}"#.to_string(),
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"second"}}"#.to_string(),
        ];
        assert_eq!(extract_codex_result(&lines).as_deref(), Some("second"));
    }

    #[test]
    fn test_extract_codex_result_none_without_agent_message() {
        let lines = vec![
            r#"{"type":"thread.started","thread_id":"abc"}"#.to_string(),
            r#"{"type":"turn.started"}"#.to_string(),
        ];
        assert_eq!(extract_codex_result(&lines), None);
    }

    #[test]
    fn test_extract_codex_result_skips_reasoning() {
        let lines = vec![
            r#"{"type":"item.completed","item":{"type":"reasoning","text":"thinking hard"}}"#.to_string(),
        ];
        assert_eq!(extract_codex_result(&lines), None);
    }

    #[test]
    fn test_codex_build_resume_with_prompt_command() {
        let (cmd, args) = build_codex_resume_with_prompt_command("codex", None, None, "thread-abc");
        assert_eq!(cmd, "codex");
        assert!(args.contains(&"exec".to_string()));
        assert!(args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        assert!(args.contains(&"--json".to_string()));
        assert!(args.contains(&"resume".to_string()));
        assert!(args.contains(&"thread-abc".to_string()));
        assert!(args.contains(&"-".to_string()));
        // Must NOT have -p (prompt via stdin)
        assert!(!args.contains(&"-p".to_string()));
    }

    #[test]
    fn test_codex_effort_flag() {
        let runner = CodexRunner::new(
            "codex".to_string(),
            None,
            Some("low".to_string()),
            None,
            2,
        );
        let (_cmd, args) = runner.build_command();
        assert!(args.contains(&"--config".to_string()));
        assert!(args.contains(&"model_reasoning_effort=\"low\"".to_string()));
    }

    #[test]
    fn test_codex_effort_flag_not_present_when_none() {
        let runner = CodexRunner::new("codex".to_string(), None, None, None, 2);
        let (_cmd, args) = runner.build_command();
        assert!(!args.contains(&"--config".to_string()));
    }

    #[test]
    fn test_build_runner_codex_with_effort() {
        let runner = build_runner(RunnerKind::Codex, "codex", None, Some("high"), None, 2);
        assert!(matches!(runner, AnyRunner::Codex(_)));
        if let AnyRunner::Codex(r) = runner {
            let (_cmd, args) = r.build_command();
            assert!(args.contains(&"--config".to_string()));
            assert!(args.contains(&"model_reasoning_effort=\"high\"".to_string()));
        }
    }

    #[test]
    fn test_codex_resume_with_prompt_command_with_model_and_effort() {
        let (_cmd, args) = build_codex_resume_with_prompt_command(
            "codex",
            Some("gpt-5.3"),
            Some("medium"),
            "thread-xyz",
        );
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"gpt-5.3".to_string()));
        assert!(args.contains(&"--config".to_string()));
        assert!(args.contains(&"model_reasoning_effort=\"medium\"".to_string()));
        assert!(args.contains(&"resume".to_string()));
        assert!(args.contains(&"thread-xyz".to_string()));
        assert!(args.contains(&"-".to_string()));
    }
}
