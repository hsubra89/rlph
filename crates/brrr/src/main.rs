//! CLI entry point: parses arguments, loads config, dispatches to build/fix/init subcommands.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use tokio::sync::watch;
use tracing::{debug, info, trace, warn};

use brrr::cli::{Cli, CliCommand};
use brrr::config::{Config, resolve_init_config};
use brrr::error::Error;
use brrr::fix;
use brrr::fix_comment::format_fix_items_for_display;
use brrr::ids::PrNumber;
use brrr::orchestrator::{
    DefaultCorrectionRunner, Orchestrator, ReviewInvocation, build_task_vars,
};
use brrr::prd;
use brrr::prompts::PromptEngine;
use brrr::runner::build_runner;
use brrr::sources::AnySource;
use brrr::sources::TaskSource;
use brrr::sources::github::GitHubSource;
use brrr::sources::linear::LinearSource;
use brrr::submission::{GitHubSubmission, PrContext, parse_pr_number_from_url};
use brrr::task::Task;
use brrr::worktree::{WorktreeManager, resolve_setup_script};

/// Parse a PR reference that is either a plain number or a GitHub PR URL.
fn parse_pr_ref(s: &str) -> Result<PrNumber, String> {
    if let Ok(number) = s.parse::<PrNumber>() {
        return Ok(number);
    }
    parse_pr_number_from_url(s)
        .ok_or_else(|| format!("invalid PR reference '{s}' — expected a number or GitHub PR URL"))
}

fn log_pr_context(pr: &PrContext) {
    info!(
        number = %pr.number,
        head = pr.head_branch,
        base = pr.base_branch,
        "reviewing PR"
    );
}

fn current_repo_root() -> Result<PathBuf, Error> {
    std::env::current_dir()
        .map_err(|e| Error::ConfigValidation(format!("cannot determine cwd: {e}")))
}

fn build_worktree_manager(
    config: &Config,
    repo_root: &Path,
    base_branch: &str,
) -> brrr::error::Result<WorktreeManager> {
    let worktree_base = PathBuf::from(&config.worktree_dir);
    let setup_script = resolve_setup_script(config.worktree_setup_script.as_deref(), repo_root)?;
    Ok(WorktreeManager::new(
        repo_root.to_path_buf(),
        worktree_base,
        base_branch.to_string(),
    )
    .with_setup_script(setup_script))
}

/// Install a double-SIGINT handler: first signal sends `true` on the channel
/// (graceful shutdown), second signal exits immediately with code 130.
fn install_sigint_handler(first_message: &'static str) -> watch::Receiver<bool> {
    let (tx, rx) = watch::channel(false);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            warn!("{first_message}");
            let _ = tx.send(true);
        }
        if tokio::signal::ctrl_c().await.is_ok() {
            warn!("Second SIGINT received; exiting immediately");
            std::process::exit(130);
        }
    });
    rx
}

async fn run(cli: Cli) -> Result<i32, Error> {
    trace!(verbose = cli.verbose, format = %cli.log_format, "logging initialized");
    debug!("brrr starting");

    let mut exit_code = 0;

    match cli.command {
        CliCommand::Init => {
            let init_cfg = resolve_init_config(&cli)?;
            if init_cfg.source == "linear" {
                brrr::sources::linear::init_interactive(&init_cfg.label)?;
            } else {
                info!("init: nothing to do for source '{}'", init_cfg.source);
            }
        }
        CliCommand::Review { ref pr_ref } => {
            let pr_number = parse_pr_ref(pr_ref).map_err(Error::ConfigValidation)?;
            let config = Config::load(&cli, None)?;
            if config.source != "github" {
                return Err(Error::ConfigValidation(
                    "'brrr review' supports only source = \"github\"".into(),
                ));
            }

            let repo_root = current_repo_root()?;
            let source: AnySource = AnySource::GitHub(GitHubSource::new(&config));

            let submission = GitHubSubmission::new();
            let pr_context = submission.get_pr_context(pr_number)?;

            log_pr_context(&pr_context);

            let worktree_mgr =
                build_worktree_manager(&config, &repo_root, &pr_context.base_branch)?;
            let worktree_info =
                worktree_mgr.create_for_branch(pr_context.number, &pr_context.head_branch)?;

            let mut issue_title = pr_context.title.clone();
            let mut issue_body = pr_context.body.clone();
            let mut issue_number = pr_context.number.to_string();
            let mut issue_url = pr_context.url.clone();
            let mut mark_in_review_task_id: Option<String> = None;

            if let Some(linked_issue_number) = pr_context.linked_issue_number {
                let linked_issue_id = linked_issue_number.to_string();
                if let Ok(task) = source.get_task_details(&linked_issue_id) {
                    issue_title = task.title;
                    issue_body = task.body;
                    issue_number = task.id.clone();
                    issue_url = task.url;
                    mark_in_review_task_id = Some(task.id);
                } else {
                    mark_in_review_task_id = Some(linked_issue_id);
                }
            }

            let task = Task {
                id: issue_number,
                title: issue_title,
                body: issue_body,
                url: issue_url,
                labels: vec![],
                priority: None,
            };
            let mut vars = build_task_vars(
                &task,
                &repo_root,
                &worktree_info.branch,
                &worktree_info.path,
                &pr_context.base_branch,
            );
            vars.insert("pr_number".to_string(), pr_context.number.to_string());
            vars.insert("pr_branch".to_string(), pr_context.head_branch.clone());
            vars.insert("pr_url".to_string(), pr_context.url.clone());

            let prompt_engine = PromptEngine::new(None);
            let timeout = config.implement_timeout;
            let factory = brrr::orchestrator::DefaultReviewRunnerFactory { stream: true };
            let orchestrator = Orchestrator::new(
                source,
                build_runner(
                    config.runner,
                    &config.agent_binary,
                    config.agent_model.as_deref(),
                    config.agent_effort.as_deref(),
                    config.agent_variant.as_deref(),
                    timeout,
                    config.agent_timeout_retries,
                ),
                submission,
                worktree_mgr,
                prompt_engine,
                config,
                repo_root,
            )
            .with_review_factory(factory);

            let invocation = ReviewInvocation {
                mark_in_review_task_id,
                worktree_info,
                vars,
                comment_pr_number: Some(pr_context.number),
            };

            orchestrator.run_review_for_existing_pr(invocation).await?;
        }
        CliCommand::Fix {
            ref pr_ref,
            dry_run,
        } => {
            let pr_number = parse_pr_ref(pr_ref).map_err(Error::ConfigValidation)?;
            let submission = GitHubSubmission::new();

            if dry_run {
                let (items, _comments) = fix::fetch_and_parse_items(pr_number, &submission)?;
                print!("{}", format_fix_items_for_display(&items));
                return Ok(0);
            }

            // Non-dry-run: run the fix polling loop
            let config = Config::load(&cli, None)?;

            // Get PR context to determine the head branch
            let pr_context = submission.get_pr_context(pr_number)?;

            log_pr_context(&pr_context);

            let repo_root = current_repo_root()?;
            let prompt_engine = PromptEngine::new(None);

            // Set up SIGINT handler for graceful shutdown
            let shutdown_rx =
                install_sigint_handler("SIGINT received; completing in-flight fixes then exiting");

            fix::run_fix_loop(
                pr_number,
                &pr_context.head_branch,
                &config,
                Arc::new(submission),
                &prompt_engine,
                &repo_root,
                Arc::new(DefaultCorrectionRunner),
                shutdown_rx,
            )
            .await?;
        }
        CliCommand::Prd {
            ref description, ..
        } => {
            let config = Config::load(&cli, None)?;

            info!("config loaded for prd");
            debug!(?config, "config loaded for prd");

            exit_code = prd::run_prd(&config, description.as_deref()).await?;
        }
        CliCommand::Build { ref args } => {
            let config = Config::load(&cli, Some(args))?;

            info!("config loaded");
            debug!(?config, "config loaded");

            if !config.once && !config.continuous && config.max_iterations.is_none() {
                return Err(Error::ConfigValidation(
                    "specify one of --once, --continuous, or --max-iterations".into(),
                ));
            }

            let repo_root = current_repo_root()?;

            let source: AnySource = match config.source.as_str() {
                "linear" => AnySource::Linear(LinearSource::new(&config)?),
                _ => AnySource::GitHub(GitHubSource::new(&config)),
            };
            let timeout = config.implement_timeout;
            let runner = build_runner(
                config.runner,
                &config.agent_binary,
                config.agent_model.as_deref(),
                config.agent_effort.as_deref(),
                config.agent_variant.as_deref(),
                timeout,
                config.agent_timeout_retries,
            )
            .with_stream_prefix("implement".to_string());
            let submission = GitHubSubmission::new();
            let worktree_mgr = build_worktree_manager(&config, &repo_root, &config.base_branch)?;
            let prompt_engine = PromptEngine::new(None);

            let orchestrator = Orchestrator::new(
                source,
                runner,
                submission,
                worktree_mgr,
                prompt_engine,
                config,
                repo_root,
            );

            let shutdown_rx =
                install_sigint_handler("SIGINT received; shutting down after current iteration");

            orchestrator.run_loop(Some(shutdown_rx)).await?;
        }
    }

    Ok(exit_code)
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    brrr::logging::init_logging(cli.verbose, cli.log_format);

    let code = match run(cli).await {
        Ok(0) => return,
        Ok(code) => code,
        Err(Error::Interrupted) => 130,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pr_ref_plain_number() {
        assert_eq!(parse_pr_ref("42").unwrap(), PrNumber::new(42));
    }

    #[test]
    fn parse_pr_ref_github_url() {
        assert_eq!(
            parse_pr_ref("https://github.com/owner/repo/pull/123").unwrap(),
            PrNumber::new(123)
        );
    }

    #[test]
    fn parse_pr_ref_trailing_slash() {
        assert_eq!(
            parse_pr_ref("https://github.com/owner/repo/pull/456/").unwrap(),
            PrNumber::new(456)
        );
    }

    #[test]
    fn parse_pr_ref_invalid() {
        assert!(parse_pr_ref("not-a-number").is_err());
    }
}
