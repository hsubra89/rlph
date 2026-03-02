# Rust Conventions

## Error Handling

Single `Error` enum in `error.rs` with `thiserror`. Every module has a variant (`TaskSource(String)`, `Worktree(String)`, etc.). Use `Error::VariantName(format!(...))` — no `anyhow`, no `.unwrap()` in library code.

`ProcessTimeout` is the exception: it carries structured data (stdout/stderr lines) for resume logic.

## Async

- Tokio runtime (`#[tokio::main]`, `tokio::spawn`, `JoinSet` for parallel review phases).
- `AgentRunner::run` returns `impl Future` (not `async_trait` — uses RPITIT).
- Process spawning is async (`tokio::process::Command`), but GitHub/submission ops are sync (`std::process::Command`).

## Config Merge Precedence

CLI flags > config file values > built-in defaults. This is enforced in `config.rs::merge()`. When adding new config fields, follow the same `cli.field.or(file.field).unwrap_or(default)` pattern.

## Dependencies

Prefer well-established crates over hand-written code. Only roll your own when no popular crate fits.
