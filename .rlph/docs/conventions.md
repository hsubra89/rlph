# Rust Conventions

## Error Handling

Single `Error` enum in `error.rs` with `thiserror`. Every module has a variant (`TaskSource(String)`, `Worktree(String)`, etc.). Use `Error::VariantName(format!(...))` — no `anyhow`, no `.unwrap()` in library code.

`ProcessTimeout` is the exception: it carries structured data (stdout/stderr lines) for resume logic.

## Edition 2024 Features Used

- `let-else` chains: `let Some(x) = foo else { return Err(...) };`
- `if-let` chains: `if let Some(x) = foo && let Some(y) = bar { ... }`
- These are used throughout — follow the existing style.

## Async

- Tokio runtime (`#[tokio::main]`, `tokio::spawn`, `JoinSet` for parallel review phases).
- `AgentRunner::run` returns `impl Future` (not `async_trait` — uses RPITIT).
- Process spawning is async (`tokio::process::Command`), but GitHub/submission ops are sync (`std::process::Command`).

## Config Merge Precedence

CLI flags > config file values > built-in defaults. This is enforced in `config.rs::merge()`. When adding new config fields, follow the same `cli.field.or(file.field).unwrap_or(default)` pattern.

## Dependencies

Minimal dependency policy. Current deps:
- `clap` (CLI), `tokio` (async), `serde`/`toml`/`serde_json` (serialization)
- `thiserror` (errors), `tracing` (logging), `regex` (dep parsing)
- `libc` (flock, signals), `ureq` (HTTP fallback)

Prefer standard library where reasonable. Add new deps only when they replace substantial hand-written code.
