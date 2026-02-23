use std::path::Path;

use serde::Deserialize;

use crate::cli::Cli;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub source: Option<String>,
    pub runner: Option<String>,
    pub submission: Option<String>,
    pub label: Option<String>,
    #[serde(alias = "poll_interval")]
    pub poll_seconds: Option<u64>,
    pub worktree_dir: Option<String>,
    pub max_iterations: Option<u32>,
    pub dry_run: Option<bool>,
    pub base_branch: Option<String>,
    pub agent_binary: Option<String>,
    pub agent_model: Option<String>,
    pub agent_timeout: Option<u64>,
    pub max_review_rounds: Option<u32>,
    pub agent_timeout_retries: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub source: String,
    pub runner: String,
    pub submission: String,
    pub label: String,
    pub poll_seconds: u64,
    pub worktree_dir: String,
    pub base_branch: String,
    pub max_iterations: Option<u32>,
    pub dry_run: bool,
    pub once: bool,
    pub continuous: bool,
    pub agent_binary: String,
    pub agent_model: Option<String>,
    pub agent_timeout: Option<u64>,
    pub max_review_rounds: u32,
    pub agent_timeout_retries: u32,
}

const DEFAULT_CONFIG_FILE: &str = ".rlph/config.toml";

impl Config {
    pub fn load(cli: &Cli) -> Result<Self> {
        Self::load_from(cli, Path::new("."))
    }

    pub fn load_from(cli: &Cli, project_dir: &Path) -> Result<Self> {
        let file_config = match &cli.config {
            Some(explicit_path) => {
                let path = Path::new(explicit_path);
                if !path.exists() {
                    return Err(Error::ConfigNotFound(path.to_path_buf()));
                }
                let content = std::fs::read_to_string(path)?;
                parse_config(&content)?
            }
            None => {
                let path = project_dir.join(DEFAULT_CONFIG_FILE);
                if path.exists() {
                    let content = std::fs::read_to_string(&path)?;
                    parse_config(&content)?
                } else {
                    ConfigFile::default()
                }
            }
        };

        merge(file_config, cli)
    }
}

pub fn parse_config(content: &str) -> Result<ConfigFile> {
    let config: ConfigFile = toml::from_str(content)?;
    Ok(config)
}

pub fn merge(file: ConfigFile, cli: &Cli) -> Result<Config> {
    let runner = cli
        .runner
        .clone()
        .or(file.runner)
        .unwrap_or_else(|| "claude".to_string());

    let default_binary = match runner.as_str() {
        "codex" => "codex",
        _ => "claude",
    };
    let default_model = match runner.as_str() {
        "codex" => Some("gpt-5.3-codex"),
        _ => None,
    };

    let config = Config {
        source: cli
            .source
            .clone()
            .or(file.source)
            .unwrap_or_else(|| "github".to_string()),
        runner,
        submission: cli
            .submission
            .clone()
            .or(file.submission)
            .unwrap_or_else(|| "github".to_string()),
        label: cli
            .label
            .clone()
            .or(file.label)
            .unwrap_or_else(|| "rlph".to_string()),
        poll_seconds: cli.poll_seconds.or(file.poll_seconds).unwrap_or(30),
        worktree_dir: cli
            .worktree_dir
            .clone()
            .or(file.worktree_dir)
            .unwrap_or_else(|| "../rlph-worktrees".to_string()),
        base_branch: cli
            .base_branch
            .clone()
            .or(file.base_branch)
            .unwrap_or_else(|| "main".to_string()),
        max_iterations: cli.max_iterations.or(file.max_iterations),
        dry_run: cli.dry_run || file.dry_run.unwrap_or(false),
        once: cli.once,
        continuous: cli.continuous,
        agent_binary: cli
            .agent_binary
            .clone()
            .or(file.agent_binary)
            .unwrap_or_else(|| default_binary.to_string()),
        agent_model: cli
            .agent_model
            .clone()
            .or(file.agent_model)
            .or_else(|| default_model.map(str::to_string)),
        agent_timeout: cli.agent_timeout.or(file.agent_timeout).or(Some(600)),
        max_review_rounds: cli
            .max_review_rounds
            .or(file.max_review_rounds)
            .unwrap_or(3),
        agent_timeout_retries: cli
            .agent_timeout_retries
            .or(file.agent_timeout_retries)
            .unwrap_or(2),
    };
    validate(&config)?;
    Ok(config)
}

fn validate(config: &Config) -> Result<()> {
    match config.source.as_str() {
        "github" | "linear" => {}
        other => {
            return Err(Error::ConfigValidation(format!(
                "unknown source: {other} (expected: github, linear)"
            )));
        }
    }
    match config.runner.as_str() {
        "claude" | "codex" => {}
        other => {
            return Err(Error::ConfigValidation(format!(
                "unknown runner: {other} (expected: claude, codex)"
            )));
        }
    }
    match config.submission.as_str() {
        "github" | "graphite" => {}
        other => {
            return Err(Error::ConfigValidation(format!(
                "unknown submission: {other} (expected: github, graphite)"
            )));
        }
    }
    if config.poll_seconds == 0 {
        return Err(Error::ConfigValidation(
            "poll_seconds must be > 0".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;

    #[test]
    fn test_parse_valid_config() {
        let toml = r#"
source = "github"
runner = "claude"
submission = "github"
label = "rlph"
poll_seconds = 30
worktree_dir = "/tmp/wt"
"#;
        let config = parse_config(toml).unwrap();
        assert_eq!(config.source.as_deref(), Some("github"));
        assert_eq!(config.poll_seconds, Some(30));
    }

    #[test]
    fn test_parse_legacy_poll_interval_key() {
        let config = parse_config(r#"poll_interval = 15"#).unwrap();
        assert_eq!(config.poll_seconds, Some(15));
    }

    #[test]
    fn test_parse_empty_config() {
        let config = parse_config("").unwrap();
        assert_eq!(config, ConfigFile::default());
    }

    #[test]
    fn test_file_invalid_source_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), r#"source = "jira""#).unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let err = Config::load_from(&cli, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("unknown source: jira"));
    }

    #[test]
    fn test_file_invalid_runner_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), r#"runner = "podman""#).unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let err = Config::load_from(&cli, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("unknown runner: podman"));
    }

    #[test]
    fn test_file_invalid_submission_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), r#"submission = "gitlab""#).unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let err = Config::load_from(&cli, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("unknown submission: gitlab"));
    }

    #[test]
    fn test_file_zero_poll_seconds_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), r#"poll_seconds = 0"#).unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let err = Config::load_from(&cli, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("poll_seconds must be > 0"));
    }

    #[test]
    fn test_parse_unknown_field() {
        let toml = r#"bogus = "value""#;
        let err = parse_config(toml).unwrap_err();
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn test_cli_overrides_config() {
        let file = ConfigFile {
            source: Some("github".to_string()),
            runner: Some("claude".to_string()),
            label: Some("file-label".to_string()),
            poll_seconds: Some(120),
            ..Default::default()
        };
        let cli = Cli::parse_from([
            "rlph",
            "--once",
            "--source",
            "linear",
            "--label",
            "cli-label",
        ]);
        let config = merge(file, &cli).unwrap();
        assert_eq!(config.source, "linear"); // CLI wins
        assert_eq!(config.label, "cli-label"); // CLI wins
        assert_eq!(config.runner, "claude"); // file value kept
        assert_eq!(config.poll_seconds, 120); // file value kept
        assert!(config.once);
    }

    #[test]
    fn test_defaults_applied() {
        let file = ConfigFile::default();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let config = merge(file, &cli).unwrap();
        assert_eq!(config.source, "github");
        assert_eq!(config.runner, "claude");
        assert_eq!(config.submission, "github");
        assert_eq!(config.label, "rlph");
        assert_eq!(config.poll_seconds, 30);
        assert_eq!(config.agent_binary, "claude");
        assert_eq!(config.agent_model, None);
        assert_eq!(config.agent_timeout, Some(600));
        assert_eq!(config.agent_timeout_retries, 2);
    }

    #[test]
    fn test_agent_timeout_overrides_default() {
        let file = ConfigFile {
            agent_timeout: Some(120),
            ..Default::default()
        };
        let cli = Cli::parse_from(["rlph", "--once", "--agent-timeout", "45"]);
        let config = merge(file, &cli).unwrap();
        assert_eq!(config.agent_timeout, Some(45));
    }

    #[test]
    fn test_load_missing_default_config_falls_back_to_defaults() {
        // When no --config is provided and .rlph/config.toml doesn't exist,
        // Config::load should succeed with built-in defaults.
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(config.source, "github");
        assert_eq!(config.runner, "claude");
        assert_eq!(config.submission, "github");
        assert_eq!(config.label, "rlph");
        assert_eq!(config.poll_seconds, 30);
        assert_eq!(config.agent_binary, "claude");
        assert_eq!(config.agent_model, None);
        assert!(config.once);
    }

    #[test]
    fn test_load_missing_default_config_with_cli_overrides() {
        // CLI overrides should still win when default config file is absent.
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["rlph", "--once", "--runner", "codex", "--label", "custom"]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(config.runner, "codex");
        assert_eq!(config.label, "custom");
        assert_eq!(config.source, "github"); // default
    }

    #[test]
    fn test_load_explicit_missing_config_errors() {
        // When --config points to a missing file, Config::load should fail.
        let cli = Cli::parse_from(["rlph", "--once", "--config", "/nonexistent/config.toml"]);
        let err = Config::load(&cli).unwrap_err();
        assert!(err.to_string().contains("config file not found"));
    }

    #[test]
    fn test_cli_invalid_source_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["rlph", "--once", "--source", "jira"]);
        let err = Config::load_from(&cli, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("unknown source: jira"));
    }

    #[test]
    fn test_cli_invalid_runner_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["rlph", "--once", "--runner", "bogus"]);
        let err = Config::load_from(&cli, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("unknown runner: bogus"));
    }

    #[test]
    fn test_cli_invalid_submission_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["rlph", "--once", "--submission", "gitlab"]);
        let err = Config::load_from(&cli, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("unknown submission: gitlab"));
    }

    #[test]
    fn test_cli_zero_poll_seconds_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["rlph", "--once", "--poll-seconds", "0"]);
        let err = Config::load_from(&cli, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("poll_seconds must be > 0"));
    }

    #[test]
    fn test_codex_runner_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["rlph", "--once", "--runner", "codex"]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(config.runner, "codex");
    }

    #[test]
    fn test_codex_runner_defaults_binary_to_codex() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["rlph", "--once", "--runner", "codex"]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(config.agent_binary, "codex");
        assert_eq!(config.agent_model.as_deref(), Some("gpt-5.3-codex"));
    }

    #[test]
    fn test_codex_runner_binary_override() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from([
            "rlph",
            "--once",
            "--runner",
            "codex",
            "--agent-binary",
            "/opt/codex",
        ]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(config.agent_binary, "/opt/codex");
    }

    #[test]
    fn test_old_runner_bare_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["rlph", "--once", "--runner", "bare"]);
        let err = Config::load_from(&cli, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("unknown runner: bare"));
    }

    #[test]
    fn test_old_runner_docker_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["rlph", "--once", "--runner", "docker"]);
        let err = Config::load_from(&cli, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("unknown runner: docker"));
    }
}
