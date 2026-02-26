use std::path::Path;

use serde::Deserialize;

use crate::cli::Cli;
use crate::error::{Error, Result};
use crate::runner::RunnerKind;

#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LinearConfigFile {
    pub team: Option<String>,
    pub project: Option<String>,
    pub api_key_env: Option<String>,
    pub in_progress_state: Option<String>,
    pub in_review_state: Option<String>,
    pub done_state: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LinearConfig {
    pub team: String,
    pub project: Option<String>,
    pub api_key_env: String,
    pub in_progress_state: String,
    pub in_review_state: String,
    pub done_state: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReviewPhaseConfigFile {
    pub name: String,
    pub prompt: String,
    pub runner: Option<String>,
    pub agent_binary: Option<String>,
    pub agent_model: Option<String>,
    pub agent_effort: Option<String>,
    pub agent_variant: Option<String>,
    pub agent_timeout: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReviewPhaseConfig {
    pub name: String,
    pub prompt: String,
    pub runner: RunnerKind,
    pub agent_binary: String,
    pub agent_model: Option<String>,
    pub agent_effort: Option<String>,
    pub agent_variant: Option<String>,
    pub agent_timeout: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReviewStepConfigFile {
    pub prompt: Option<String>,
    pub runner: Option<String>,
    pub agent_binary: Option<String>,
    pub agent_model: Option<String>,
    pub agent_effort: Option<String>,
    pub agent_variant: Option<String>,
    pub agent_timeout: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReviewStepConfig {
    pub prompt: String,
    pub runner: RunnerKind,
    pub agent_binary: String,
    pub agent_model: Option<String>,
    pub agent_effort: Option<String>,
    pub agent_variant: Option<String>,
    pub agent_timeout: Option<u64>,
}

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
    pub agent_effort: Option<String>,
    pub agent_variant: Option<String>,
    pub max_review_rounds: Option<u32>,
    pub agent_timeout_retries: Option<u32>,
    pub review_phases: Option<Vec<ReviewPhaseConfigFile>>,
    pub review_aggregate: Option<ReviewStepConfigFile>,
    pub review_fix: Option<ReviewStepConfigFile>,
    pub linear: Option<LinearConfigFile>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub source: String,
    pub runner: RunnerKind,
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
    pub agent_effort: Option<String>,
    pub agent_variant: Option<String>,
    pub max_review_rounds: u32,
    pub agent_timeout_retries: u32,
    pub review_phases: Vec<ReviewPhaseConfig>,
    pub review_aggregate: ReviewStepConfig,
    pub review_fix: ReviewStepConfig,
    pub linear: Option<LinearConfig>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InitConfig {
    pub source: String,
    pub label: String,
}

const DEFAULT_CONFIG_FILE: &str = ".rlph/config.toml";

/// Default review phases for use in tests and when no config is provided.
pub fn default_review_phases() -> Vec<ReviewPhaseConfig> {
    vec![
        ReviewPhaseConfig {
            name: "correctness".to_string(),
            prompt: "correctness-review".to_string(),
            runner: RunnerKind::Claude,
            agent_binary: "claude".to_string(),
            agent_model: None,
            agent_effort: None,
            agent_variant: None,
            agent_timeout: None,
        },
        ReviewPhaseConfig {
            name: "security".to_string(),
            prompt: "security-review".to_string(),
            runner: RunnerKind::Claude,
            agent_binary: "claude".to_string(),
            agent_model: None,
            agent_effort: None,
            agent_variant: None,
            agent_timeout: None,
        },
        ReviewPhaseConfig {
            name: "style".to_string(),
            prompt: "style-review".to_string(),
            runner: RunnerKind::Claude,
            agent_binary: "claude".to_string(),
            agent_model: None,
            agent_effort: None,
            agent_variant: None,
            agent_timeout: None,
        },
    ]
}

/// Default review step config for use in tests.
pub fn default_review_step(prompt: &str) -> ReviewStepConfig {
    ReviewStepConfig {
        prompt: prompt.to_string(),
        runner: RunnerKind::Claude,
        agent_binary: "claude".to_string(),
        agent_model: None,
        agent_effort: None,
        agent_variant: None,
        agent_timeout: None,
    }
}

impl Config {
    pub fn load(cli: &Cli) -> Result<Self> {
        Self::load_from(cli, Path::new("."))
    }

    pub fn load_from(cli: &Cli, project_dir: &Path) -> Result<Self> {
        let file_config = load_file_config(cli, project_dir)?;
        merge(file_config, cli)
    }
}

pub fn resolve_init_config(cli: &Cli) -> Result<InitConfig> {
    resolve_init_config_from(cli, Path::new("."))
}

pub fn resolve_init_config_from(cli: &Cli, project_dir: &Path) -> Result<InitConfig> {
    let file = load_file_config(cli, project_dir)?;
    let source = cli
        .source
        .clone()
        .or(file.source)
        .unwrap_or_else(|| "github".to_string());

    match source.as_str() {
        "github" | "linear" => {}
        other => {
            return Err(Error::ConfigValidation(format!(
                "unknown source: {other} (expected: github, linear)"
            )));
        }
    }

    Ok(InitConfig {
        source,
        label: cli
            .label
            .clone()
            .or(file.label)
            .unwrap_or_else(|| "rlph".to_string()),
    })
}

fn load_file_config(cli: &Cli, project_dir: &Path) -> Result<ConfigFile> {
    match &cli.config {
        Some(explicit_path) => {
            let path = Path::new(explicit_path);
            if !path.exists() {
                return Err(Error::ConfigNotFound(path.to_path_buf()));
            }
            let content = std::fs::read_to_string(path)?;
            parse_config(&content)
        }
        None => {
            let path = project_dir.join(DEFAULT_CONFIG_FILE);
            if path.exists() {
                let content = std::fs::read_to_string(&path)?;
                parse_config(&content)
            } else {
                Ok(ConfigFile::default())
            }
        }
    }
}

pub fn parse_config(content: &str) -> Result<ConfigFile> {
    let config: ConfigFile = toml::from_str(content)?;
    Ok(config)
}

fn runner_default_binary(runner: RunnerKind) -> &'static str {
    match runner {
        RunnerKind::Codex => "codex",
        RunnerKind::Claude => "claude",
        RunnerKind::OpenCode => "opencode",
    }
}

fn runner_default_model(runner: RunnerKind) -> Option<&'static str> {
    match runner {
        RunnerKind::Codex => Some("gpt-5.3-codex"),
        RunnerKind::Claude => Some("claude-opus-4-6"),
        RunnerKind::OpenCode => None,
    }
}

fn runner_default_effort(runner: RunnerKind) -> Option<&'static str> {
    match runner {
        RunnerKind::Codex => None,
        RunnerKind::Claude => Some("high"),
        RunnerKind::OpenCode => None,
    }
}

pub fn merge(file: ConfigFile, cli: &Cli) -> Result<Config> {
    let runner: RunnerKind = cli
        .runner
        .as_deref()
        .or(file.runner.as_deref())
        .unwrap_or("claude")
        .parse()?;

    let default_binary = runner_default_binary(runner);
    let default_model = runner_default_model(runner);
    let default_effort = runner_default_effort(runner);

    let linear = file.linear.map(|lc| LinearConfig {
        team: lc.team.unwrap_or_default(),
        project: lc.project,
        api_key_env: lc
            .api_key_env
            .unwrap_or_else(|| "LINEAR_API_KEY".to_string()),
        in_progress_state: lc
            .in_progress_state
            .unwrap_or_else(|| "In Progress".to_string()),
        in_review_state: lc
            .in_review_state
            .unwrap_or_else(|| "In Review".to_string()),
        done_state: lc.done_state.unwrap_or_else(|| "Done".to_string()),
    });

    let global_runner = runner;
    let global_binary_override = cli.agent_binary.clone().or(file.agent_binary.clone());
    let global_model_override = cli.agent_model.clone().or(file.agent_model.clone());
    let global_effort_override = cli.agent_effort.clone().or(file.agent_effort.clone());
    let global_variant_override = cli.agent_variant.clone().or(file.agent_variant.clone());

    let global_binary = global_binary_override
        .clone()
        .unwrap_or_else(|| default_binary.to_string());
    let global_model = global_model_override
        .clone()
        .or_else(|| default_model.map(str::to_string));
    let global_effort = global_effort_override
        .clone()
        .or_else(|| default_effort.map(str::to_string));
    let global_variant = global_variant_override.clone();
    let global_timeout = cli.agent_timeout.or(file.agent_timeout).or(Some(600));

    let review_phases: Vec<ReviewPhaseConfig> = file
        .review_phases
        .unwrap_or_else(|| {
            default_review_phases()
                .into_iter()
                .map(|p| ReviewPhaseConfigFile {
                    name: p.name,
                    prompt: p.prompt,
                    runner: None,
                    agent_binary: None,
                    agent_model: None,
                    agent_effort: None,
                    agent_variant: None,
                    agent_timeout: None,
                })
                .collect()
        })
        .into_iter()
        .map(|p| {
            let effective_runner: RunnerKind = match p.runner {
                Some(s) => s.parse()?,
                None => global_runner,
            };
            let runner_binary = runner_default_binary(effective_runner);
            let runner_model = runner_default_model(effective_runner);
            let runner_effort = runner_default_effort(effective_runner);
            Ok(ReviewPhaseConfig {
                name: p.name,
                prompt: p.prompt,
                agent_binary: p
                    .agent_binary
                    .or_else(|| global_binary_override.clone())
                    .unwrap_or_else(|| runner_binary.to_string()),
                agent_model: p
                    .agent_model
                    .or_else(|| global_model_override.clone())
                    .or_else(|| runner_model.map(str::to_string)),
                agent_effort: p
                    .agent_effort
                    .or_else(|| global_effort_override.clone())
                    .or_else(|| runner_effort.map(str::to_string)),
                agent_variant: p
                    .agent_variant
                    .or_else(|| global_variant_override.clone()),
                agent_timeout: p.agent_timeout.or(global_timeout),
                runner: effective_runner,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let resolve_step =
        |step: Option<ReviewStepConfigFile>, default_prompt: &str| -> Result<ReviewStepConfig> {
            let s = step.unwrap_or_default();
            let effective_runner: RunnerKind = match s.runner {
                Some(s) => s.parse()?,
                None => global_runner,
            };
            let runner_binary = runner_default_binary(effective_runner);
            let runner_model = runner_default_model(effective_runner);
            let runner_effort = runner_default_effort(effective_runner);
            Ok(ReviewStepConfig {
                prompt: s.prompt.unwrap_or_else(|| default_prompt.to_string()),
                agent_binary: s
                    .agent_binary
                    .or_else(|| global_binary_override.clone())
                    .unwrap_or_else(|| runner_binary.to_string()),
                agent_model: s
                    .agent_model
                    .or_else(|| global_model_override.clone())
                    .or_else(|| runner_model.map(str::to_string)),
                agent_effort: s
                    .agent_effort
                    .or_else(|| global_effort_override.clone())
                    .or_else(|| runner_effort.map(str::to_string)),
                agent_variant: s
                    .agent_variant
                    .or_else(|| global_variant_override.clone()),
                agent_timeout: s.agent_timeout.or(global_timeout),
                runner: effective_runner,
            })
        };

    let review_aggregate = resolve_step(file.review_aggregate, "review-aggregate")?;
    let review_fix = resolve_step(file.review_fix, "review-fix")?;

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
        agent_binary: global_binary,
        agent_model: global_model,
        agent_timeout: global_timeout,
        agent_effort: global_effort,
        agent_variant: global_variant,
        max_review_rounds: cli
            .max_review_rounds
            .or(file.max_review_rounds)
            .unwrap_or(3),
        agent_timeout_retries: cli
            .agent_timeout_retries
            .or(file.agent_timeout_retries)
            .unwrap_or(2),
        review_phases,
        review_aggregate,
        review_fix,
        linear,
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
    match config.submission.as_str() {
        "github" | "graphite" => {}
        other => {
            return Err(Error::ConfigValidation(format!(
                "unknown submission: {other} (expected: github, graphite)"
            )));
        }
    }
    if config.runner == RunnerKind::OpenCode && config.agent_effort.is_some() {
        return Err(Error::ConfigValidation(
            "opencode uses agent_variant, not agent_effort".to_string(),
        ));
    }
    if matches!(config.runner, RunnerKind::Claude | RunnerKind::Codex)
        && config.agent_variant.is_some()
    {
        return Err(Error::ConfigValidation(
            "agent_variant is only supported by opencode".to_string(),
        ));
    }
    if config.poll_seconds == 0 {
        return Err(Error::ConfigValidation(
            "poll_seconds must be > 0".to_string(),
        ));
    }
    if config.review_phases.is_empty() {
        return Err(Error::ConfigValidation(
            "at least one review phase is required".to_string(),
        ));
    }
    {
        let mut seen_names = std::collections::HashSet::new();
        for phase in &config.review_phases {
            if phase.name.is_empty() {
                return Err(Error::ConfigValidation(
                    "review phase name must not be empty".to_string(),
                ));
            }
            if phase.prompt.is_empty() {
                return Err(Error::ConfigValidation(format!(
                    "review phase '{}' prompt must not be empty",
                    phase.name
                )));
            }
            if !seen_names.insert(&phase.name) {
                return Err(Error::ConfigValidation(format!(
                    "duplicate review phase name: {}",
                    phase.name
                )));
            }
        }
    }
    if config.source == "linear" {
        match &config.linear {
            Some(lc) if lc.team.is_empty() => {
                return Err(Error::ConfigValidation(
                    "linear.team is required when source = \"linear\"".to_string(),
                ));
            }
            None => {
                return Err(Error::ConfigValidation(
                    "[linear] config section required when source = \"linear\"".to_string(),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use crate::runner::RunnerKind;
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
            linear: Some(LinearConfigFile {
                team: Some("ENG".to_string()),
                ..Default::default()
            }),
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
        assert_eq!(config.runner, RunnerKind::Claude); // file value kept
        assert_eq!(config.poll_seconds, 120); // file value kept
        assert!(config.once);
    }

    #[test]
    fn test_defaults_applied() {
        let file = ConfigFile::default();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let config = merge(file, &cli).unwrap();
        assert_eq!(config.source, "github");
        assert_eq!(config.runner, RunnerKind::Claude);
        assert_eq!(config.submission, "github");
        assert_eq!(config.label, "rlph");
        assert_eq!(config.poll_seconds, 30);
        assert_eq!(config.agent_binary, "claude");
        assert_eq!(config.agent_model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(config.agent_effort.as_deref(), Some("high"));
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
        assert_eq!(config.runner, RunnerKind::Claude);
        assert_eq!(config.submission, "github");
        assert_eq!(config.label, "rlph");
        assert_eq!(config.poll_seconds, 30);
        assert_eq!(config.agent_binary, "claude");
        assert_eq!(config.agent_model.as_deref(), Some("claude-opus-4-6"));
        assert_eq!(config.agent_effort.as_deref(), Some("high"));
        assert!(config.once);
    }

    #[test]
    fn test_load_missing_default_config_with_cli_overrides() {
        // CLI overrides should still win when default config file is absent.
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["rlph", "--once", "--runner", "codex", "--label", "custom"]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(config.runner, RunnerKind::Codex);
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
    fn test_resolve_init_config_uses_file_source_without_linear_section() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            "source = \"linear\"\nlabel = \"ops\"\n",
        )
        .unwrap();

        let cli = Cli::parse_from(["rlph", "init"]);
        let cfg = resolve_init_config_from(&cli, tmp.path()).unwrap();
        assert_eq!(cfg.source, "linear");
        assert_eq!(cfg.label, "ops");
    }

    #[test]
    fn test_resolve_init_config_cli_overrides_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            "source = \"github\"\nlabel = \"file\"\n",
        )
        .unwrap();

        let cli = Cli::parse_from(["rlph", "init", "--source", "linear", "--label", "cli"]);
        let cfg = resolve_init_config_from(&cli, tmp.path()).unwrap();
        assert_eq!(cfg.source, "linear");
        assert_eq!(cfg.label, "cli");
    }

    #[test]
    fn test_resolve_init_config_defaults_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["rlph", "init"]);
        let cfg = resolve_init_config_from(&cli, tmp.path()).unwrap();
        assert_eq!(cfg.source, "github");
        assert_eq!(cfg.label, "rlph");
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
        assert_eq!(config.runner, RunnerKind::Codex);
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

    #[test]
    fn test_review_phases_parsed_from_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"
[[review_phases]]
name = "correctness"
prompt = "correctness-review"

[[review_phases]]
name = "security"
prompt = "security-review"
agent_model = "claude-opus-4-6"
agent_effort = "high"
"#,
        )
        .unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(config.review_phases.len(), 2);
        assert_eq!(config.review_phases[0].name, "correctness");
        assert_eq!(config.review_phases[0].prompt, "correctness-review");
        assert_eq!(config.review_phases[0].runner, RunnerKind::Claude); // falls back to global
        assert_eq!(config.review_phases[1].name, "security");
        assert_eq!(
            config.review_phases[1].agent_model.as_deref(),
            Some("claude-opus-4-6")
        );
        assert_eq!(
            config.review_phases[1].agent_effort.as_deref(),
            Some("high")
        );
    }

    #[test]
    fn test_review_phases_duplicate_name_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"
[[review_phases]]
name = "check"
prompt = "check-review"

[[review_phases]]
name = "check"
prompt = "other-review"
"#,
        )
        .unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let err = Config::load_from(&cli, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("duplicate review phase name"));
    }

    #[test]
    fn test_review_aggregate_and_fix_parsed() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"
[[review_phases]]
name = "check"
prompt = "check-review"

[review_aggregate]
prompt = "my-aggregate"
agent_model = "claude-opus-4-6"

[review_fix]
prompt = "my-fix"
"#,
        )
        .unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(config.review_aggregate.prompt, "my-aggregate");
        assert_eq!(
            config.review_aggregate.agent_model.as_deref(),
            Some("claude-opus-4-6")
        );
        assert_eq!(config.review_fix.prompt, "my-fix");
    }

    #[test]
    fn test_review_aggregate_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(config.review_aggregate.prompt, "review-aggregate");
        assert_eq!(config.review_fix.prompt, "review-fix");
        assert_eq!(config.review_aggregate.runner, RunnerKind::Claude);
        assert_eq!(config.review_fix.runner, RunnerKind::Claude);
    }

    #[test]
    fn test_default_review_phases_provided() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(config.review_phases.len(), 3);
        assert_eq!(config.review_phases[0].name, "correctness");
        assert_eq!(config.review_phases[1].name, "security");
        assert_eq!(config.review_phases[2].name, "style");
    }

    #[test]
    fn test_review_phase_empty_name_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"
[[review_phases]]
name = ""
prompt = "check-review"
"#,
        )
        .unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let err = Config::load_from(&cli, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("name must not be empty"));
    }

    #[test]
    fn test_review_phase_empty_prompt_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"
[[review_phases]]
name = "check"
prompt = ""
"#,
        )
        .unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let err = Config::load_from(&cli, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("prompt must not be empty"));
    }

    #[test]
    fn test_review_phase_invalid_runner_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"
[[review_phases]]
name = "check"
prompt = "check-review"
runner = "podman"
"#,
        )
        .unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let err = Config::load_from(&cli, tmp.path()).unwrap_err();
        assert!(err.to_string().contains("unknown runner: podman"));
    }

    #[test]
    fn test_review_phase_inherits_global_runner() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"
runner = "codex"

[[review_phases]]
name = "check"
prompt = "check-review"
"#,
        )
        .unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(config.review_phases[0].runner, RunnerKind::Codex);
        assert_eq!(config.review_phases[0].agent_binary, "codex");
    }

    #[test]
    fn test_review_phase_overrides_runner_gets_correct_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"
runner = "codex"

[[review_phases]]
name = "check"
prompt = "check-review"
runner = "claude"
"#,
        )
        .unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(config.review_phases[0].runner, RunnerKind::Claude);
        assert_eq!(config.review_phases[0].agent_binary, "claude");
        assert_eq!(
            config.review_phases[0].agent_model.as_deref(),
            Some("claude-opus-4-6")
        );
        assert_eq!(
            config.review_phases[0].agent_effort.as_deref(),
            Some("high")
        );
    }

    #[test]
    fn test_review_step_overrides_runner_gets_correct_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"
runner = "codex"

[[review_phases]]
name = "check"
prompt = "check-review"

[review_aggregate]
runner = "claude"
"#,
        )
        .unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(config.review_aggregate.runner, RunnerKind::Claude);
        assert_eq!(config.review_aggregate.agent_binary, "claude");
        assert_eq!(
            config.review_aggregate.agent_model.as_deref(),
            Some("claude-opus-4-6")
        );
    }

    #[test]
    fn test_review_phase_inherits_global_agent_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"
runner = "codex"
agent_binary = "/opt/agent-proxy"
agent_model = "custom-model-v1"
agent_effort = "medium"

[[review_phases]]
name = "check"
prompt = "check-review"
"#,
        )
        .unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(config.review_phases[0].agent_binary, "/opt/agent-proxy");
        assert_eq!(
            config.review_phases[0].agent_model.as_deref(),
            Some("custom-model-v1")
        );
        assert_eq!(
            config.review_phases[0].agent_effort.as_deref(),
            Some("medium")
        );
    }

    #[test]
    fn test_review_steps_inherit_global_agent_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"
runner = "codex"
agent_binary = "/opt/agent-proxy"
agent_model = "custom-model-v1"
agent_effort = "medium"

[[review_phases]]
name = "check"
prompt = "check-review"

[review_aggregate]
prompt = "review-aggregate"

[review_fix]
prompt = "review-fix"
"#,
        )
        .unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();

        assert_eq!(config.review_aggregate.agent_binary, "/opt/agent-proxy");
        assert_eq!(
            config.review_aggregate.agent_model.as_deref(),
            Some("custom-model-v1")
        );
        assert_eq!(
            config.review_aggregate.agent_effort.as_deref(),
            Some("medium")
        );

        assert_eq!(config.review_fix.agent_binary, "/opt/agent-proxy");
        assert_eq!(
            config.review_fix.agent_model.as_deref(),
            Some("custom-model-v1")
        );
        assert_eq!(config.review_fix.agent_effort.as_deref(), Some("medium"));
    }

    #[test]
    fn test_opencode_runner_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["rlph", "--once", "--runner", "opencode"]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(config.runner, RunnerKind::OpenCode);
        assert_eq!(config.agent_binary, "opencode");
        assert_eq!(config.agent_model, None);
        assert_eq!(config.agent_effort, None);
    }

    #[test]
    fn test_opencode_with_variant() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from([
            "rlph",
            "--once",
            "--runner",
            "opencode",
            "--agent-variant",
            "high",
        ]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(config.runner, RunnerKind::OpenCode);
        assert_eq!(config.agent_variant.as_deref(), Some("high"));
    }

    #[test]
    fn test_opencode_with_effort_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from([
            "rlph",
            "--once",
            "--runner",
            "opencode",
            "--agent-effort",
            "high",
        ]);
        let err = Config::load_from(&cli, tmp.path()).unwrap_err();
        assert!(err
            .to_string()
            .contains("opencode uses agent_variant, not agent_effort"));
    }

    #[test]
    fn test_claude_with_variant_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from([
            "rlph",
            "--once",
            "--runner",
            "claude",
            "--agent-variant",
            "high",
        ]);
        let err = Config::load_from(&cli, tmp.path()).unwrap_err();
        assert!(err
            .to_string()
            .contains("agent_variant is only supported by opencode"));
    }

    #[test]
    fn test_codex_with_variant_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from([
            "rlph",
            "--once",
            "--runner",
            "codex",
            "--agent-variant",
            "low",
        ]);
        let err = Config::load_from(&cli, tmp.path()).unwrap_err();
        assert!(err
            .to_string()
            .contains("agent_variant is only supported by opencode"));
    }

    #[test]
    fn test_opencode_variant_plumbed_to_review_phases() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"
runner = "opencode"
agent_variant = "high"

[[review_phases]]
name = "check"
prompt = "check-review"

[review_aggregate]
prompt = "review-aggregate"

[review_fix]
prompt = "review-fix"
"#,
        )
        .unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(
            config.review_phases[0].agent_variant.as_deref(),
            Some("high")
        );
        assert_eq!(
            config.review_aggregate.agent_variant.as_deref(),
            Some("high")
        );
        assert_eq!(config.review_fix.agent_variant.as_deref(), Some("high"));
    }

    #[test]
    fn test_opencode_review_phase_variant_override() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".rlph");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            r#"
runner = "opencode"
agent_variant = "high"

[[review_phases]]
name = "check"
prompt = "check-review"
agent_variant = "low"
"#,
        )
        .unwrap();
        let cli = Cli::parse_from(["rlph", "--once"]);
        let config = Config::load_from(&cli, tmp.path()).unwrap();
        assert_eq!(
            config.review_phases[0].agent_variant.as_deref(),
            Some("low")
        );
    }
}
