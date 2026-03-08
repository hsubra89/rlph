//! CLI argument parsing via `clap`.

use std::fmt;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Log output format.
#[derive(ValueEnum, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogFormat {
    /// Human-readable ANSI-colored output.
    #[default]
    Text,
    /// Machine-readable JSON lines.
    Json,
}

impl fmt::Display for LogFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Text => "text",
            Self::Json => "json",
        };
        f.write_str(value)
    }
}

/// rlph — autonomous AI development loop
#[derive(Parser, Debug, Clone)]
#[command(name = "brrr", version, about, subcommand_required = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: CliCommand,

    /// Increase log verbosity (-v = debug, -vv = trace).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Log output format.
    #[arg(long, value_enum, default_value_t = LogFormat::Text, global = true)]
    pub log_format: LogFormat,

    /// Task source to use (github, linear)
    #[arg(long, global = true)]
    pub source: Option<String>,

    /// Label to filter eligible tasks
    #[arg(long, global = true)]
    pub label: Option<String>,

    /// Path to config file
    #[arg(long, global = true)]
    pub config: Option<String>,

    #[command(flatten)]
    pub agent: AgentArgs,
}

/// Agent identity args available on every subcommand.
#[derive(Args, Debug, Clone, Default)]
pub struct AgentArgs {
    /// Agent runner to use (claude, codex, opencode)
    #[arg(long, global = true)]
    pub runner: Option<String>,

    /// Agent binary to use (default: claude)
    #[arg(long, global = true)]
    pub agent_binary: Option<String>,

    /// Model for the agent to use (default for claude: claude-opus-4-6)
    #[arg(long, global = true)]
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

    pub fn agent_args(&self) -> &AgentArgs {
        &self.agent
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
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_build_once() {
        let cli = Cli::parse_from(["brrr", "build", "--once"]);
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
        let cli = Cli::parse_from(["brrr", "build", "--continuous", "--max-iterations", "5"]);
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
        let cli = Cli::parse_from(["brrr", "build", "--dry-run", "--once"]);
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
            "brrr",
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
        assert_eq!(cli.agent.runner.as_deref(), Some("codex"));
        match cli.command {
            CliCommand::Build { args } => {
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
        let cli = Cli::parse_from(["brrr", "build", "--once", "--poll-interval", "45"]);
        match cli.command {
            CliCommand::Build { args } => {
                assert_eq!(args.poll_seconds, Some(45));
            }
            _ => panic!("expected Build subcommand"),
        }
    }

    #[test]
    fn test_parse_init_allows_global_args_after_subcommand() {
        let cli = Cli::parse_from(["brrr", "init", "--source", "linear", "--label", "auto"]);
        assert!(matches!(cli.command, CliCommand::Init));
        assert_eq!(cli.source.as_deref(), Some("linear"));
        assert_eq!(cli.label.as_deref(), Some("auto"));
    }

    #[test]
    fn test_parse_review() {
        let cli = Cli::parse_from(["brrr", "review", "123"]);
        match cli.command {
            CliCommand::Review { pr_ref } => assert_eq!(pr_ref, "123"),
            _ => panic!("expected Review subcommand"),
        }
    }

    #[test]
    fn test_parse_review_url() {
        let cli = Cli::parse_from(["brrr", "review", "https://github.com/owner/repo/pull/456"]);
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
            "brrr", "review", "77", "--source", "github", "--label", "brrr",
        ]);
        match cli.command {
            CliCommand::Review { pr_ref } => assert_eq!(pr_ref, "77"),
            _ => panic!("expected Review subcommand"),
        }
        assert_eq!(cli.source.as_deref(), Some("github"));
        assert_eq!(cli.label.as_deref(), Some("brrr"));
    }

    #[test]
    fn test_parse_prd_no_description() {
        let cli = Cli::parse_from(["brrr", "prd"]);
        match cli.command {
            CliCommand::Prd { description, .. } => assert!(description.is_none()),
            _ => panic!("expected Prd subcommand"),
        }
    }

    #[test]
    fn test_parse_prd_with_description() {
        let cli = Cli::parse_from(["brrr", "prd", "add auth support"]);
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
            "brrr",
            "prd",
            "--runner",
            "codex",
            "--source",
            "linear",
            "my feature",
        ]);
        match cli.command {
            CliCommand::Prd { description } => {
                assert_eq!(description.as_deref(), Some("my feature"));
            }
            _ => panic!("expected Prd subcommand"),
        }
        assert_eq!(cli.agent.runner.as_deref(), Some("codex"));
        assert_eq!(cli.source.as_deref(), Some("linear"));
    }

    #[test]
    fn test_parse_review_with_runner() {
        let cli = Cli::parse_from(["brrr", "review", "99", "--runner", "codex"]);
        match cli.command {
            CliCommand::Review { pr_ref } => assert_eq!(pr_ref, "99"),
            _ => panic!("expected Review subcommand"),
        }
        assert_eq!(cli.agent.runner.as_deref(), Some("codex"));
    }

    #[test]
    fn test_parse_fix_with_runner() {
        let cli = Cli::parse_from(["brrr", "fix", "42", "--runner", "opencode"]);
        match cli.command {
            CliCommand::Fix { pr_ref, dry_run } => {
                assert_eq!(pr_ref, "42");
                assert!(!dry_run);
            }
            _ => panic!("expected Fix subcommand"),
        }
        assert_eq!(cli.agent.runner.as_deref(), Some("opencode"));
    }

    #[test]
    fn test_parse_fix_dry_run() {
        let cli = Cli::parse_from(["brrr", "fix", "123", "--dry-run"]);
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
        let cli = Cli::parse_from(["brrr", "fix", "456"]);
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
        let cli = Cli::parse_from(["brrr", "fix", "https://github.com/owner/repo/pull/789"]);
        match cli.command {
            CliCommand::Fix { pr_ref, .. } => {
                assert_eq!(pr_ref, "https://github.com/owner/repo/pull/789");
            }
            _ => panic!("expected Fix subcommand"),
        }
    }

    #[test]
    fn test_default_verbosity_and_format() {
        let cli = Cli::parse_from(["brrr", "build", "--once"]);
        assert_eq!(cli.verbose, 0);
        assert_eq!(cli.log_format, LogFormat::Text);
    }

    #[test]
    fn test_single_verbose() {
        let cli = Cli::parse_from(["brrr", "-v", "build", "--once"]);
        assert_eq!(cli.verbose, 1);
    }

    #[test]
    fn test_double_verbose() {
        let cli = Cli::parse_from(["brrr", "-vv", "build", "--once"]);
        assert_eq!(cli.verbose, 2);
    }

    #[test]
    fn test_verbose_after_subcommand() {
        let cli = Cli::parse_from(["brrr", "build", "--once", "-v"]);
        assert_eq!(cli.verbose, 1);
    }

    #[test]
    fn test_log_format_json() {
        let cli = Cli::parse_from(["brrr", "--log-format", "json", "build", "--once"]);
        assert_eq!(cli.log_format, LogFormat::Json);
    }

    #[test]
    fn test_log_format_text_explicit() {
        let cli = Cli::parse_from(["brrr", "--log-format", "text", "build", "--once"]);
        assert_eq!(cli.log_format, LogFormat::Text);
    }

    #[test]
    fn test_log_format_display_uses_cli_values() {
        assert_eq!(LogFormat::Text.to_string(), "text");
        assert_eq!(LogFormat::Json.to_string(), "json");
    }

    #[test]
    fn test_verbose_with_json_format() {
        let cli = Cli::parse_from(["brrr", "-vv", "--log-format", "json", "build", "--once"]);
        assert_eq!(cli.verbose, 2);
        assert_eq!(cli.log_format, LogFormat::Json);
    }
}
