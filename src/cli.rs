//! CLI argument parsing via `clap`.

use clap::{Args, Parser, Subcommand};

/// rlph — autonomous AI development loop
#[derive(Parser, Debug, Clone)]
#[command(name = "rlph", version, about, subcommand_required = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: CliCommand,

    /// Task source to use (github, linear)
    #[arg(long, global = true)]
    pub source: Option<String>,

    /// Label to filter eligible tasks
    #[arg(long, global = true)]
    pub label: Option<String>,

    /// Path to config file
    #[arg(long, global = true)]
    pub config: Option<String>,
}

/// Agent identity args shared by Build and Prd subcommands.
#[derive(Args, Debug, Clone, Default)]
pub struct AgentArgs {
    /// Agent runner to use (claude, codex, opencode)
    #[arg(long)]
    pub runner: Option<String>,

    /// Agent binary to use (default: claude)
    #[arg(long)]
    pub agent_binary: Option<String>,

    /// Model for the agent to use (default for claude: claude-opus-4-6)
    #[arg(long)]
    pub agent_model: Option<String>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct BuildArgs {
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

    #[command(flatten)]
    pub agent: AgentArgs,

    /// Submission backend to use (github, graphite)
    #[arg(long)]
    pub submission: Option<String>,

    /// Poll interval in seconds (continuous mode)
    #[arg(long = "poll-seconds", alias = "poll-interval")]
    pub poll_seconds: Option<u64>,

    /// Worktree base directory
    #[arg(long)]
    pub worktree_dir: Option<String>,

    /// Base branch for worktrees and PRs (default: main)
    #[arg(long)]
    pub base_branch: Option<String>,

    /// Agent timeout in seconds
    #[arg(long)]
    pub agent_timeout: Option<u64>,

    /// Implement phase timeout in seconds (default: 1800)
    #[arg(long)]
    pub implement_timeout: Option<u64>,

    /// Effort level for the agent (low, medium, high) — Claude/Codex runner only
    #[arg(long)]
    pub agent_effort: Option<String>,

    /// Variant for the agent (low, high) — OpenCode runner only
    #[arg(long)]
    pub agent_variant: Option<String>,

    /// Maximum retries when agent times out (session resume)
    #[arg(long)]
    pub agent_timeout_retries: Option<u32>,
}

impl Cli {
    pub fn build_args(&self) -> Option<&BuildArgs> {
        match &self.command {
            CliCommand::Build { args } => Some(args),
            _ => None,
        }
    }

    pub fn agent_args(&self) -> Option<&AgentArgs> {
        match &self.command {
            CliCommand::Build { args } => Some(&args.agent),
            CliCommand::Prd { agent, .. } => Some(agent),
            _ => None,
        }
    }
}

#[derive(Subcommand, Debug, Clone)]
pub enum CliCommand {
    /// Run the build loop: fetch tasks, spin up worktrees, run agents
    Build {
        #[command(flatten)]
        args: BuildArgs,
    },

    /// Initialize the project for the configured task source (e.g., create labels)
    Init,

    /// Run review phases directly for an existing GitHub PR
    Review {
        /// GitHub pull request number or URL
        pr_ref: String,
    },

    /// Fix review findings for an existing GitHub PR
    Fix {
        /// GitHub pull request number or URL
        pr_ref: String,

        /// Show parsed findings without applying fixes
        #[arg(long)]
        dry_run: bool,
    },

    /// Launch an interactive PRD-writing session
    Prd {
        /// Seed description for the PRD (optional)
        description: Option<String>,

        #[command(flatten)]
        agent: AgentArgs,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_build_once() {
        let cli = Cli::parse_from(["rlph", "build", "--once"]);
        match cli.command {
            CliCommand::Build { args } => {
                assert!(args.once);
                assert!(!args.continuous);
                assert!(!args.dry_run);
            }
            _ => panic!("expected Build subcommand"),
        }
    }

    #[test]
    fn test_parse_build_continuous_with_max() {
        let cli = Cli::parse_from(["rlph", "build", "--continuous", "--max-iterations", "5"]);
        match cli.command {
            CliCommand::Build { args } => {
                assert!(args.continuous);
                assert_eq!(args.max_iterations, Some(5));
            }
            _ => panic!("expected Build subcommand"),
        }
    }

    #[test]
    fn test_parse_build_dry_run() {
        let cli = Cli::parse_from(["rlph", "build", "--dry-run", "--once"]);
        match cli.command {
            CliCommand::Build { args } => {
                assert!(args.dry_run);
                assert!(args.once);
            }
            _ => panic!("expected Build subcommand"),
        }
    }

    #[test]
    fn test_parse_build_all_overrides() {
        let cli = Cli::parse_from([
            "rlph",
            "build",
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
        match cli.command {
            CliCommand::Build { args } => {
                assert_eq!(args.agent.runner.as_deref(), Some("codex"));
                assert_eq!(args.submission.as_deref(), Some("graphite"));
                assert_eq!(args.poll_seconds, Some(30));
                assert_eq!(args.worktree_dir.as_deref(), Some("/tmp/wt"));
            }
            _ => panic!("expected Build subcommand"),
        }
        assert_eq!(cli.source.as_deref(), Some("linear"));
        assert_eq!(cli.label.as_deref(), Some("auto"));
    }

    #[test]
    fn test_parse_build_poll_interval_alias() {
        let cli = Cli::parse_from(["rlph", "build", "--once", "--poll-interval", "45"]);
        match cli.command {
            CliCommand::Build { args } => {
                assert_eq!(args.poll_seconds, Some(45));
            }
            _ => panic!("expected Build subcommand"),
        }
    }

    #[test]
    fn test_parse_init_allows_global_args_after_subcommand() {
        let cli = Cli::parse_from(["rlph", "init", "--source", "linear", "--label", "auto"]);
        assert!(matches!(cli.command, CliCommand::Init));
        assert_eq!(cli.source.as_deref(), Some("linear"));
        assert_eq!(cli.label.as_deref(), Some("auto"));
    }

    #[test]
    fn test_parse_review() {
        let cli = Cli::parse_from(["rlph", "review", "123"]);
        match cli.command {
            CliCommand::Review { pr_ref } => assert_eq!(pr_ref, "123"),
            _ => panic!("expected Review subcommand"),
        }
    }

    #[test]
    fn test_parse_review_url() {
        let cli = Cli::parse_from(["rlph", "review", "https://github.com/owner/repo/pull/456"]);
        match cli.command {
            CliCommand::Review { pr_ref } => {
                assert_eq!(pr_ref, "https://github.com/owner/repo/pull/456");
            }
            _ => panic!("expected Review subcommand"),
        }
    }

    #[test]
    fn test_parse_review_with_global_args_after_subcommand() {
        let cli = Cli::parse_from([
            "rlph", "review", "77", "--source", "github", "--label", "rlph",
        ]);
        match cli.command {
            CliCommand::Review { pr_ref } => assert_eq!(pr_ref, "77"),
            _ => panic!("expected Review subcommand"),
        }
        assert_eq!(cli.source.as_deref(), Some("github"));
        assert_eq!(cli.label.as_deref(), Some("rlph"));
    }

    #[test]
    fn test_parse_prd_no_description() {
        let cli = Cli::parse_from(["rlph", "prd"]);
        match cli.command {
            CliCommand::Prd { description, .. } => assert!(description.is_none()),
            _ => panic!("expected Prd subcommand"),
        }
    }

    #[test]
    fn test_parse_prd_with_description() {
        let cli = Cli::parse_from(["rlph", "prd", "add auth support"]);
        match cli.command {
            CliCommand::Prd { description, .. } => {
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
            CliCommand::Prd {
                description,
                ref agent,
            } => {
                assert_eq!(description.as_deref(), Some("my feature"));
                assert_eq!(agent.runner.as_deref(), Some("codex"));
            }
            _ => panic!("expected Prd subcommand"),
        }
        assert_eq!(cli.source.as_deref(), Some("linear"));
    }

    #[test]
    fn test_parse_fix_dry_run() {
        let cli = Cli::parse_from(["rlph", "fix", "123", "--dry-run"]);
        match cli.command {
            CliCommand::Fix { pr_ref, dry_run } => {
                assert_eq!(pr_ref, "123");
                assert!(dry_run);
            }
            _ => panic!("expected Fix subcommand"),
        }
    }

    #[test]
    fn test_parse_fix_without_dry_run() {
        let cli = Cli::parse_from(["rlph", "fix", "456"]);
        match cli.command {
            CliCommand::Fix { pr_ref, dry_run } => {
                assert_eq!(pr_ref, "456");
                assert!(!dry_run);
            }
            _ => panic!("expected Fix subcommand"),
        }
    }

    #[test]
    fn test_parse_fix_url() {
        let cli = Cli::parse_from(["rlph", "fix", "https://github.com/owner/repo/pull/789"]);
        match cli.command {
            CliCommand::Fix { pr_ref, .. } => {
                assert_eq!(pr_ref, "https://github.com/owner/repo/pull/789");
            }
            _ => panic!("expected Fix subcommand"),
        }
    }
}
