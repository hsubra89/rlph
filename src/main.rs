use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use tokio::sync::watch;
use tracing::info;

use rlph::cli::{Cli, CliCommand};
use rlph::config::Config;
use rlph::orchestrator::Orchestrator;
use rlph::prompts::PromptEngine;
use rlph::runner::{AnyRunner, ClaudeRunner, CodexRunner};
use rlph::sources::AnySource;
use rlph::sources::github::GitHubSource;
use rlph::sources::linear::LinearSource;
use rlph::state::StateManager;
use rlph::submission::GitHubSubmission;
use rlph::worktree::WorktreeManager;

fn init_logging() {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_timer(tracing_subscriber::fmt::time::SystemTime)
        .init();
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    init_logging();

    info!("rlph starting");

    // Handle init before loading full config (init may need to create the config)
    if let Some(CliCommand::Init) = cli.command {
        let source = cli.source.as_deref().unwrap_or("github");
        let label = cli.label.as_deref().unwrap_or("rlph");
        if source == "linear" {
            if let Err(e) = rlph::sources::linear::init_interactive(label) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        } else {
            info!("init: nothing to do for source '{source}'");
        }
        return;
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
            info!("[rlph] SIGINT received; shutting down after current iteration");
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
