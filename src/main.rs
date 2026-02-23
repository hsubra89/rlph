use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use tracing::info;

use rlph::cli::Cli;
use rlph::config::Config;
use rlph::orchestrator::Orchestrator;
use rlph::prompts::PromptEngine;
use rlph::runner::{AnyRunner, ClaudeRunner, CodexRunner};
use rlph::sources::github::GitHubSource;
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

    let config = match Config::load(&cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    info!(?config, "config loaded");

    if !cli.once && !cli.continuous {
        eprintln!("error: specify --once or --continuous");
        std::process::exit(1);
    }

    let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let source = GitHubSource::new(&config);
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

    if cli.once {
        if let Err(e) = orchestrator.run_once().await {
            if matches!(&e, rlph::error::Error::Interrupted) {
                std::process::exit(130);
            }
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    } else {
        eprintln!("error: continuous mode not yet implemented");
        std::process::exit(1);
    }
}
