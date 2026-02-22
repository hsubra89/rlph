use std::path::Path;

use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Choose,
    Implement,
    Review,
}

#[derive(Debug)]
pub struct RunResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait AgentRunner {
    /// Run the agent for a given phase with a prompt in a working directory.
    /// Returns streamed output and a result.
    fn run(&self, phase: Phase, prompt: &str, working_dir: &Path) -> Result<RunResult>;
}
