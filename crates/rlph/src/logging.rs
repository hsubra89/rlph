//! Logging initialization: maps CLI verbosity / format flags to `tracing-subscriber` layers.

use tracing_subscriber::EnvFilter;

use crate::cli::LogFormat;

/// Initialize the global tracing subscriber.
///
/// * `verbosity` — 0 = info (default), 1 = debug, 2+ = trace.
/// * `format` — [`LogFormat::Text`] (ANSI colors) or [`LogFormat::Json`].
///
/// When the `RUST_LOG` environment variable is set it takes precedence over the
/// CLI verbosity flag.
pub fn init_logging(verbosity: u8, format: LogFormat) {
    let default_level = match verbosity {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));

    match format {
        LogFormat::Text => {
            // Text omits timestamps for readability — the terminal context is enough.
            tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_env_filter(env_filter)
                .with_target(true)
                .without_time()
                .init();
        }
        LogFormat::Json => {
            // JSON retains timestamps for machine consumers.
            tracing_subscriber::fmt()
                .json()
                .with_writer(std::io::stderr)
                .with_env_filter(env_filter)
                .with_target(true)
                .init();
        }
    }
}
