use clap::Parser;

/// rlph — autonomous AI development loop
#[derive(Parser, Debug)]
#[command(name = "rlph", version, about)]
pub struct Cli {
    /// Run a single iteration then exit
    #[arg(long)]
    pub once: bool,

    /// Run continuously, polling for new tasks
    #[arg(long, conflicts_with = "once")]
    pub continuous: bool,

    /// Maximum number of iterations before stopping
    #[arg(long, conflicts_with = "once")]
    pub max_iterations: Option<u32>,

    /// Go through the full loop without pushing changes or marking issues
    #[arg(long)]
    pub dry_run: bool,

    /// Agent runner to use (bare, docker)
    #[arg(long)]
    pub runner: Option<String>,

    /// Task source to use (github, linear)
    #[arg(long)]
    pub source: Option<String>,

    /// Submission backend to use (github, graphite)
    #[arg(long)]
    pub submission: Option<String>,

    /// Label to filter eligible tasks
    #[arg(long)]
    pub label: Option<String>,

    /// Poll interval in seconds (continuous mode)
    #[arg(long)]
    pub poll_interval: Option<u64>,

    /// Path to config file
    #[arg(long, default_value = ".rlph/config.toml")]
    pub config: String,

    /// Worktree base directory
    #[arg(long)]
    pub worktree_dir: Option<String>,

    /// Agent binary to use (default: claude)
    #[arg(long)]
    pub agent_binary: Option<String>,

    /// Model for the agent to use
    #[arg(long)]
    pub agent_model: Option<String>,

    /// Agent timeout in seconds
    #[arg(long)]
    pub agent_timeout: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_once() {
        let cli = Cli::parse_from(["rlph", "--once"]);
        assert!(cli.once);
        assert!(!cli.continuous);
        assert!(!cli.dry_run);
    }

    #[test]
    fn test_parse_continuous_with_max() {
        let cli = Cli::parse_from(["rlph", "--continuous", "--max-iterations", "5"]);
        assert!(cli.continuous);
        assert_eq!(cli.max_iterations, Some(5));
    }

    #[test]
    fn test_parse_dry_run() {
        let cli = Cli::parse_from(["rlph", "--dry-run", "--once"]);
        assert!(cli.dry_run);
        assert!(cli.once);
    }

    #[test]
    fn test_parse_all_overrides() {
        let cli = Cli::parse_from([
            "rlph",
            "--once",
            "--runner",
            "docker",
            "--source",
            "linear",
            "--submission",
            "graphite",
            "--label",
            "auto",
            "--poll-interval",
            "30",
            "--worktree-dir",
            "/tmp/wt",
        ]);
        assert_eq!(cli.runner.as_deref(), Some("docker"));
        assert_eq!(cli.source.as_deref(), Some("linear"));
        assert_eq!(cli.submission.as_deref(), Some("graphite"));
        assert_eq!(cli.label.as_deref(), Some("auto"));
        assert_eq!(cli.poll_interval, Some(30));
        assert_eq!(cli.worktree_dir.as_deref(), Some("/tmp/wt"));
    }
}
