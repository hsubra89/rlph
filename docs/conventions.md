# Rust Conventions

## Error Handling

Central `Error` enum in `error.rs` with `thiserror`. No `anyhow`, no `.unwrap()` in library code.

- **Simple modules** (single failure mode): use `Error::VariantName(String)` directly (e.g. `Worktree(String)`).
- **Modules with multiple error kinds**: define a module-local error enum and nest it via `#[from]` + `#[error(transparent)]`. Example: `DiffPositionMapperError` has `Parse`, `FileNotFound`, `NoMappableLines` variants; the central enum wraps it as `DiffPositionMapper(#[from] DiffPositionMapperError)`.
- **Structured data**: `ProcessTimeout` carries stdout/stderr lines for resume logic.

## Async

- Tokio runtime (`#[tokio::main]`, `tokio::spawn`, `JoinSet` for parallel review phases).
- `AgentRunner::run` returns `impl Future` (not `async_trait` — uses RPITIT).
- Process spawning is async (`tokio::process::Command`), but GitHub/submission ops are sync (`std::process::Command`).

## Config Merge Precedence

CLI flags > config file values > built-in defaults. This is enforced in `config.rs::merge()`. When adding new config fields, follow the same `cli.field.or(file.field).unwrap_or(default)` pattern.

## Dependencies

Prefer well-established crates over hand-written code. Only roll your own when no popular crate fits.
