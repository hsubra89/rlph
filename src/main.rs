//! CLI entry point: parses arguments, loads config, dispatches to build/fix/init subcommands.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use tokio::sync::watch;
use tracing::{debug, info};
use tracing_subscriber::EnvFilter;

use rlph::cli::{Cli, CliCommand};
use rlph::config::{Config, resolve_init_config};
use rlph::fix;
use rlph::fix_comment::format_fix_items_for_display;
use rlph::ids::PrNumber;
use rlph::orchestrator::{
    DefaultCorrectionRunner, Orchestrator, ReviewInvocation, build_task_vars,
};
use rlph::prd;
use rlph::prompts::PromptEngine;
use rlph::runner::build_runner;
use rlph::sources::AnySource;
use rlph::sources::github::GitHubSource;
use rlph::sources::linear::LinearSource;
use rlph::sources::{Task, TaskSource};
use rlph::state::StateManager;
use rlph::submission::{GitHubSubmission, PrContext};
use rlph::worktree::{WorktreeManager, resolve_setup_script};

/// Parse a PR reference that is either a plain number or a GitHub PR URL.
fn parse_pr_ref(s: &str) -> Result<PrNumber, String> {
    s.parse::<u64>()
        .or_else(|_| s.trim_end_matches('/').rsplit('/').next().unwrap().parse())
        .map(PrNumber::new)
        .map_err(|_| format!("invalid PR reference '{s}' — expected a number or GitHub PR URL"))
}

/// Parse a PR ref or print an error and exit.
fn parse_pr_ref_or_exit(s: &str) -> PrNumber {
    parse_pr_ref(s).unwrap_or_else(|msg| {
        eprintln!("error: {msg}");
        std::process::exit(1);
    })
}

fn print_pr_banner(pr: &PrContext) {
    eprintln!(
        "[rlph] PR #{}: {} \u{2192} {}",
        pr.number, pr.head_branch, pr.base_branch
    );
}

fn build_worktree_manager(
    config: &Config,
    repo_root: &Path,
    base_branch: &str,
) -> rlph::error::Result<WorktreeManager> {
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
            eprintln!("{first_message}");
            let _ = tx.send(true);
        }
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("[rlph] Second SIGINT received; exiting immediately");
            std::process::exit(130);
        }
    });
    rx
}

fn init_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_target(true)
        .without_time()
        .init();
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    init_logging();

    debug!("rlph starting");

    match cli.command {
        CliCommand::Init => {
            let init_cfg = match resolve_init_config(&cli) {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };
            if init_cfg.source == "linear" {
                if let Err(e) = rlph::sources::linear::init_interactive(&init_cfg.label) {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            } else {
                info!("init: nothing to do for source '{}'", init_cfg.source);
            }
        }
        CliCommand::Review { ref pr_ref } => {
            let pr_number = parse_pr_ref_or_exit(pr_ref);
            let config = match Config::load(&cli, None) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };
            if config.source != "github" {
                eprintln!("error: 'rlph review' supports only source = \"github\"");
                std::process::exit(1);
            }

            let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let source: AnySource = AnySource::GitHub(GitHubSource::new(&config));

            let submission = GitHubSubmission::new();
            let pr_context = match submission.get_pr_context(pr_number) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };

            print_pr_banner(&pr_context);

            let worktree_mgr =
                match build_worktree_manager(&config, &repo_root, &pr_context.base_branch) {
                    Ok(wm) => wm,
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                };
            let worktree_info =
                match worktree_mgr.create_for_branch(pr_context.number, &pr_context.head_branch) {
                    Ok(w) => w,
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                };

            let mut issue_title = pr_context.title.clone();
            let mut issue_body = pr_context.body.clone();
            let mut issue_number = pr_context.number.to_string();
            let mut issue_url = pr_context.url.clone();
            let mut task_id_for_state = format!("pr-{}", pr_context.number);
            let mut mark_in_review_task_id: Option<String> = None;

            if let Some(linked_issue_number) = pr_context.linked_issue_number {
                let linked_issue_id = linked_issue_number.to_string();
                if let Ok(task) = source.get_task_details(&linked_issue_id) {
                    issue_title = task.title;
                    issue_body = task.body;
                    issue_number = task.id.clone();
                    issue_url = task.url;
                    task_id_for_state = format!("gh-{linked_issue_number}");
                    mark_in_review_task_id = Some(task.id);
                } else {
                    task_id_for_state = format!("gh-{linked_issue_number}");
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

            let state_mgr = StateManager::new(StateManager::default_dir(&repo_root));
            let prompt_engine = PromptEngine::new(None);
            let timeout = config.implement_timeout;
            let factory = rlph::orchestrator::DefaultReviewRunnerFactory { stream: true };
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
                state_mgr,
                prompt_engine,
                config,
                repo_root,
            )
            .with_review_factory(factory);

            let invocation = ReviewInvocation {
                task_id_for_state,
                mark_in_review_task_id,
                worktree_info,
                vars,
                comment_pr_number: Some(pr_context.number),
            };

            if let Err(e) = orchestrator.run_review_for_existing_pr(invocation).await {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        CliCommand::Fix {
            ref pr_ref,
            dry_run,
        } => {
            let pr_number = parse_pr_ref_or_exit(pr_ref);
            let submission = GitHubSubmission::new();

            if dry_run {
                let (items, _comments) = match fix::fetch_and_parse_items(pr_number, &submission) {
                    Ok(result) => result,
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                };
                print!("{}", format_fix_items_for_display(&items));
                return;
            }

            // Non-dry-run: run the fix polling loop
            let config = match Config::load(&cli, None) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };

            // Get PR context to determine the head branch
            let pr_context = match submission.get_pr_context(pr_number) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };

            print_pr_banner(&pr_context);

            let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let prompt_engine = PromptEngine::new(None);

            // Set up SIGINT handler for graceful shutdown
            let shutdown_rx = install_sigint_handler(
                "[rlph] SIGINT received; completing in-flight fixes then exiting",
            );

            if let Err(e) = fix::run_fix_loop(
                pr_number,
                &pr_context.head_branch,
                &config,
                Arc::new(submission),
                &prompt_engine,
                &repo_root,
                Arc::new(DefaultCorrectionRunner),
                shutdown_rx,
            )
            .await
            {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        CliCommand::Prd {
            ref description, ..
        } => {
            let cfg = match Config::load(&cli, None) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };

            info!(?cfg, "config loaded for prd");

            let exit_code = match prd::run_prd(&cfg, description.as_deref()).await {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };

            std::process::exit(exit_code);
        }
        CliCommand::Build { ref args } => {
            let config = match Config::load(&cli, Some(args)) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };

            info!(?config, "config loaded");

            if !config.once && !config.continuous && config.max_iterations.is_none() {
                eprintln!("error: specify one of --once, --continuous, or --max-iterations");
                std::process::exit(1);
            }

            let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

            let source: AnySource = match config.source.as_str() {
                "linear" => match LinearSource::new(&config) {
                    Ok(s) => AnySource::Linear(s),
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                },
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
            let worktree_mgr =
                match build_worktree_manager(&config, &repo_root, &config.base_branch) {
                    Ok(wm) => wm,
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                };
            let state_mgr = StateManager::new(StateManager::default_dir(&repo_root));
            let prompt_engine = PromptEngine::new(None);

            let orchestrator = Orchestrator::new(
                source,
                runner,
                submission,
                worktree_mgr,
                state_mgr,
                prompt_engine,
                config,
                repo_root,
            );

            let shutdown_rx = install_sigint_handler(
                "[rlph] SIGINT received; shutting down after current iteration",
            );

            if let Err(e) = orchestrator.run_loop(Some(shutdown_rx)).await {
                if matches!(&e, rlph::error::Error::Interrupted) {
                    std::process::exit(130);
                }
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
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
