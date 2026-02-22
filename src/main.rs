use clap::Parser;
use tracing::info;

use rlph::cli::Cli;
use rlph::config::Config;

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

    info!("no source configured — exiting");
}
