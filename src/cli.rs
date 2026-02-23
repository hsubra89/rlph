use clap::{Parser, Subcommand};

/// rlph — autonomous AI development loop
#[derive(Parser, Debug, Clone)]
#[command(name = "rlph", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<CliCommand>,

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

    /// Agent runner to use (claude, codex)
    #[arg(long)]
    pub runner: Option<String>,

    /// Task source to use (github, linear)
    #[arg(long, global = true)]
    pub source: Option<String>,

    /// Submission backend to use (github, graphite)
    #[arg(long)]
    pub submission: Option<String>,

    /// Label to filter eligible tasks
    #[arg(long, global = true)]
    pub label: Option<String>,

    /// Poll interval in seconds (continuous mode)
    #[arg(long = "poll-seconds", alias = "poll-interval")]
    pub poll_seconds: Option<u64>,

    /// Path to config file
    #[arg(long, global = true)]
    pub config: Option<String>,

    /// Worktree base directory
    #[arg(long)]
    pub worktree_dir: Option<String>,

    /// Base branch for worktrees and PRs (default: main)
    #[arg(long)]
    pub base_branch: Option<String>,

    /// Agent binary to use (default: claude)
    #[arg(long)]
    pub agent_binary: Option<String>,

    /// Model for the agent to use (default for claude: claude-opus-4-6)
    #[arg(long)]
    pub agent_model: Option<String>,

    /// Agent timeout in seconds
    #[arg(long)]
    pub agent_timeout: Option<u64>,

    /// Effort level for the agent (low, medium, high) — Claude runner only
    #[arg(long)]
    pub agent_effort: Option<String>,

    /// Maximum review rounds per task
    #[arg(long)]
    pub max_review_rounds: Option<u32>,

    /// Maximum retries when agent times out (session resume)
    #[arg(long)]
    pub agent_timeout_retries: Option<u32>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum CliCommand {
    /// Initialize the project for the configured task source (e.g., create labels)
    Init,

    /// Launch an interactive PRD-writing session
    Prd {
        /// Seed description for the PRD (optional)
        description: Option<String>,

        /// Agent runner to use (claude, codex)
        #[arg(long)]
        runner: Option<String>,

        /// Task source to use (github, linear)
        #[arg(long)]
        source: Option<String>,

        /// Path to config file
        #[arg(long)]
        config: Option<String>,

        /// Agent binary to use
        #[arg(long)]
        agent_binary: Option<String>,

        /// Model for the agent to use
        #[arg(long)]
        agent_model: Option<String>,
    },
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
        assert!(cli.command.is_none());
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
            "codex",
            "--source",
            "linear",
            "--submission",
            "graphite",
            "--label",
            "auto",
            "--poll-seconds",
            "30",
            "--worktree-dir",
            "/tmp/wt",
        ]);
        assert_eq!(cli.runner.as_deref(), Some("codex"));
        assert_eq!(cli.source.as_deref(), Some("linear"));
        assert_eq!(cli.submission.as_deref(), Some("graphite"));
        assert_eq!(cli.label.as_deref(), Some("auto"));
        assert_eq!(cli.poll_seconds, Some(30));
        assert_eq!(cli.worktree_dir.as_deref(), Some("/tmp/wt"));
    }

    #[test]
    fn test_parse_poll_interval_alias() {
        let cli = Cli::parse_from(["rlph", "--once", "--poll-interval", "45"]);
        assert_eq!(cli.poll_seconds, Some(45));
    }

    #[test]
    fn test_parse_init_allows_global_args_after_subcommand() {
        let cli = Cli::parse_from(["rlph", "init", "--source", "linear", "--label", "auto"]);
        assert!(matches!(cli.command, Some(CliCommand::Init)));
        assert_eq!(cli.source.as_deref(), Some("linear"));
        assert_eq!(cli.label.as_deref(), Some("auto"));
    }

    #[test]
    fn test_parse_prd_no_description() {
        let cli = Cli::parse_from(["rlph", "prd"]);
        match cli.command {
            Some(CliCommand::Prd { description, .. }) => assert!(description.is_none()),
            _ => panic!("expected Prd subcommand"),
        }
    }

    #[test]
    fn test_parse_prd_with_description() {
        let cli = Cli::parse_from(["rlph", "prd", "add auth support"]);
        match cli.command {
            Some(CliCommand::Prd { description, .. }) => {
                assert_eq!(description.as_deref(), Some("add auth support"));
            }
            _ => panic!("expected Prd subcommand"),
        }
    }

    #[test]
    fn test_parse_prd_with_overrides() {
        let cli = Cli::parse_from([
            "rlph",
            "prd",
            "--runner",
            "codex",
            "--source",
            "linear",
            "my feature",
        ]);
        match cli.command {
            Some(CliCommand::Prd {
                description,
                runner,
                source,
                ..
            }) => {
                assert_eq!(description.as_deref(), Some("my feature"));
                assert_eq!(runner.as_deref(), Some("codex"));
                assert_eq!(source.as_deref(), Some("linear"));
            }
            _ => panic!("expected Prd subcommand"),
        }
    }

    #[test]
    fn test_bare_rlph_once_still_works() {
        let cli = Cli::parse_from(["rlph", "--once"]);
        assert!(cli.command.is_none());
        assert!(cli.once);
    }
}
