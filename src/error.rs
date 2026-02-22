use std::path::PathBuf;

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

    #[error("state error: {0}")]
    State(String),
}

pub type Result<T> = std::result::Result<T, Error>;
