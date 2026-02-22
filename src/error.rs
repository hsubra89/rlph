use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum Error {
    #[error("config file not found: {0}")]
    ConfigNotFound(PathBuf),

    #[error("config parse error: {0}")]
    ConfigParse(#[from] toml::de::Error),

    #[error("config validation error: {0}")]
    ConfigValidation(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("task source error: {0}")]
    TaskSource(String),

    #[error("agent runner error: {0}")]
    AgentRunner(String),

    #[error("submission error: {0}")]
    Submission(String),

    #[error("worktree error: {0}")]
    Worktree(String),

    #[error("process error: {0}")]
    Process(String),

    #[error("process timed out after {timeout:?}")]
    ProcessTimeout {
        timeout: Duration,
        stdout_lines: Vec<String>,
        stderr_lines: Vec<String>,
    },

    #[error("state error: {0}")]
    State(String),

    #[error("prompt error: {0}")]
    Prompt(String),

    #[error("orchestrator error: {0}")]
    Orchestrator(String),

    #[error("interrupted by signal")]
    Interrupted,
}

pub type Result<T> = std::result::Result<T, Error>;
