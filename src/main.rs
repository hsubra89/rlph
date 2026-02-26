use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use tokio::sync::watch;
use tracing::{debug, info};
use tracing_subscriber::EnvFilter;

use rlph::cli::{Cli, CliCommand};
use rlph::config::{Config, resolve_init_config};
use rlph::orchestrator::{Orchestrator, ReviewInvocation};
use rlph::prd;
use rlph::prompts::PromptEngine;
use rlph::runner::{AnyRunner, ClaudeRunner, CodexRunner, build_runner};
use rlph::sources::AnySource;
use rlph::sources::TaskSource;
use rlph::sources::github::GitHubSource;
use rlph::sources::linear::LinearSource;
use rlph::state::StateManager;
use rlph::submission::GitHubSubmission;
use rlph::worktree::WorktreeManager;

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
        Some(CliCommand::Init) => {
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
            return;
        }
        Some(CliCommand::Review { pr_number }) => {
            let config = match Config::load(&cli) {
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

            let worktree_base = PathBuf::from(&config.worktree_dir);
            let worktree_mgr =
                WorktreeManager::new(repo_root.clone(), worktree_base, config.base_branch.clone());
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

            let mut vars = HashMap::new();
            vars.insert("issue_title".to_string(), issue_title);
            vars.insert("issue_body".to_string(), issue_body);
            vars.insert("issue_number".to_string(), issue_number);
            vars.insert("issue_url".to_string(), issue_url);
            vars.insert("pr_number".to_string(), pr_context.number.to_string());
            vars.insert("pr_branch".to_string(), pr_context.head_branch.clone());
            vars.insert("pr_url".to_string(), pr_context.url.clone());
            vars.insert("repo_path".to_string(), repo_root.display().to_string());
            vars.insert("branch_name".to_string(), worktree_info.branch.clone());
            vars.insert(
                "worktree_path".to_string(),
                worktree_info.path.display().to_string(),
            );

            let state_mgr = StateManager::new(StateManager::default_dir(&repo_root));
            let prompt_engine = PromptEngine::new(None);
            let timeout = config.agent_timeout.map(Duration::from_secs);
            let orchestrator = Orchestrator::new(
                source,
                build_runner(
                    &config.runner,
                    &config.agent_binary,
                    config.agent_model.as_deref(),
                    config.agent_effort.as_deref(),
                    timeout,
                    config.agent_timeout_retries,
                ),
                submission,
                worktree_mgr,
                state_mgr,
                prompt_engine,
                config,
                repo_root,
            );

            let invocation = ReviewInvocation {
                task_id_for_state,
                mark_in_review_task_id,
                worktree_info,
                vars,
                comment_pr_number: Some(pr_context.number),
                push_remote_branch: Some(pr_context.head_branch),
            };

            if let Err(e) = orchestrator.run_review_for_existing_pr(invocation).await {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
            return;
        }
        Some(CliCommand::Prd {
            ref description,
            ref runner,
            ref source,
            ref config,
            ref agent_binary,
            ref agent_model,
        }) => {
            // Clone top-level CLI and merge prd-specific overrides.
            // Using clone ensures new Cli fields are automatically preserved.
            let mut prd_cli = cli.clone();
            prd_cli.command = None;
            prd_cli.once = false;
            prd_cli.continuous = false;
            prd_cli.max_iterations = None;
            prd_cli.dry_run = false;
            if let Some(r) = runner {
                prd_cli.runner = Some(r.clone());
            }
            if let Some(s) = source {
                prd_cli.source = Some(s.clone());
            }
            if let Some(c) = config {
                prd_cli.config = Some(c.clone());
            }
            if let Some(b) = agent_binary {
                prd_cli.agent_binary = Some(b.clone());
            }
            if let Some(m) = agent_model {
                prd_cli.agent_model = Some(m.clone());
            }

            let cfg = match Config::load(&prd_cli) {
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
        None => {}
    }

    let config = match Config::load(&cli) {
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
    let timeout = config.agent_timeout.map(Duration::from_secs);
    let runner = match config.runner.as_str() {
        "codex" => AnyRunner::Codex(CodexRunner::new(
            config.agent_binary.clone(),
            config.agent_model.clone(),
            config.agent_effort.clone(),
            timeout,
            config.agent_timeout_retries,
        )),
        _ => AnyRunner::Claude(ClaudeRunner::new(
            config.agent_binary.clone(),
            config.agent_model.clone(),
            config.agent_effort.clone(),
            timeout,
            config.agent_timeout_retries,
        )),
    };
    let submission = GitHubSubmission::new();
    let worktree_base = PathBuf::from(&config.worktree_dir);
    let worktree_mgr =
        WorktreeManager::new(repo_root.clone(), worktree_base, config.base_branch.clone());
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

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        // First SIGINT: graceful shutdown after current iteration
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("[rlph] SIGINT received; shutting down after current iteration");
            let _ = shutdown_tx.send(true);
        }
        // Second SIGINT: force exit
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("[rlph] Second SIGINT received; exiting immediately");
            std::process::exit(130);
        }
    });

    if let Err(e) = orchestrator.run_loop(Some(shutdown_rx)).await {
        if matches!(&e, rlph::error::Error::Interrupted) {
            std::process::exit(130);
        }
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
