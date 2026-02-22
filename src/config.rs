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
    pub poll_interval: Option<u64>,
    pub worktree_dir: Option<String>,
    pub max_iterations: Option<u32>,
    pub dry_run: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub source: String,
    pub runner: String,
    pub submission: String,
    pub label: String,
    pub poll_interval: u64,
    pub worktree_dir: String,
    pub max_iterations: Option<u32>,
    pub dry_run: bool,
    pub once: bool,
    pub continuous: bool,
}

impl Config {
    pub fn load(cli: &Cli) -> Result<Self> {
        let config_path = Path::new(&cli.config);
        let file_config = if config_path.exists() {
            let content = std::fs::read_to_string(config_path)?;
            parse_config(&content)?
        } else {
            return Err(Error::ConfigNotFound(config_path.to_path_buf()));
        };

        Ok(merge(file_config, cli))
    }
}

pub fn parse_config(content: &str) -> Result<ConfigFile> {
    let config: ConfigFile = toml::from_str(content)?;
    validate(&config)?;
    Ok(config)
}

fn validate(config: &ConfigFile) -> Result<()> {
    if let Some(ref source) = config.source {
        match source.as_str() {
            "github" | "linear" => {}
            other => {
                return Err(Error::ConfigValidation(format!(
                    "unknown source: {other} (expected: github, linear)"
                )));
            }
        }
    }
    if let Some(ref runner) = config.runner {
        match runner.as_str() {
            "bare" | "docker" => {}
            other => {
                return Err(Error::ConfigValidation(format!(
                    "unknown runner: {other} (expected: bare, docker)"
                )));
            }
        }
    }
    if let Some(ref submission) = config.submission {
        match submission.as_str() {
            "github" | "graphite" => {}
            other => {
                return Err(Error::ConfigValidation(format!(
                    "unknown submission: {other} (expected: github, graphite)"
                )));
            }
        }
    }
    if let Some(interval) = config.poll_interval
        && interval == 0
    {
        return Err(Error::ConfigValidation(
            "poll_interval must be > 0".to_string(),
        ));
    }
    Ok(())
}

pub fn merge(file: ConfigFile, cli: &Cli) -> Config {
    Config {
        source: cli
            .source
            .clone()
            .or(file.source)
            .unwrap_or_else(|| "github".to_string()),
        runner: cli
            .runner
            .clone()
            .or(file.runner)
            .unwrap_or_else(|| "bare".to_string()),
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
        poll_interval: cli.poll_interval.or(file.poll_interval).unwrap_or(60),
        worktree_dir: cli
            .worktree_dir
            .clone()
            .or(file.worktree_dir)
            .unwrap_or_else(|| "../rlph-worktrees".to_string()),
        max_iterations: cli.max_iterations.or(file.max_iterations),
        dry_run: cli.dry_run || file.dry_run.unwrap_or(false),
        once: cli.once,
        continuous: cli.continuous,
    }
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
runner = "bare"
submission = "github"
label = "rlph"
poll_interval = 30
worktree_dir = "/tmp/wt"
"#;
        let config = parse_config(toml).unwrap();
        assert_eq!(config.source.as_deref(), Some("github"));
        assert_eq!(config.poll_interval, Some(30));
    }

    #[test]
    fn test_parse_empty_config() {
        let config = parse_config("").unwrap();
        assert_eq!(config, ConfigFile::default());
    }

    #[test]
    fn test_parse_invalid_source() {
        let toml = r#"source = "jira""#;
        let err = parse_config(toml).unwrap_err();
        assert!(err.to_string().contains("unknown source"));
    }

    #[test]
    fn test_parse_invalid_runner() {
        let toml = r#"runner = "podman""#;
        let err = parse_config(toml).unwrap_err();
        assert!(err.to_string().contains("unknown runner"));
    }

    #[test]
    fn test_parse_invalid_submission() {
        let toml = r#"submission = "gitlab""#;
        let err = parse_config(toml).unwrap_err();
        assert!(err.to_string().contains("unknown submission"));
    }

    #[test]
    fn test_parse_zero_poll_interval() {
        let toml = r#"poll_interval = 0"#;
        let err = parse_config(toml).unwrap_err();
        assert!(err.to_string().contains("poll_interval must be > 0"));
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
            runner: Some("bare".to_string()),
            label: Some("file-label".to_string()),
            poll_interval: Some(120),
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
        let config = merge(file, &cli);
        assert_eq!(config.source, "linear"); // CLI wins
        assert_eq!(config.label, "cli-label"); // CLI wins
        assert_eq!(config.runner, "bare"); // file value kept
        assert_eq!(config.poll_interval, 120); // file value kept
        assert!(config.once);
    }

    #[test]
    fn test_defaults_applied() {
        let file = ConfigFile::default();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let config = merge(file, &cli);
        assert_eq!(config.source, "github");
        assert_eq!(config.runner, "bare");
        assert_eq!(config.submission, "github");
        assert_eq!(config.label, "rlph");
        assert_eq!(config.poll_interval, 60);
    }
}
