# Rust Conventions

## Design Principles

- Keep public API surface minimal (`pub(crate)` by default).
- Separate pure domain logic from runtime/IO glue.
- Use additive feature flags and keep defaults lean.
- Prefer enums/newtypes over stringly-typed state.
- Avoid panics in non-test code; return structured errors.

## Error Handling

Central `Error` enum in `error.rs` with `thiserror`. No `anyhow`, no `.unwrap()` in library code.

- **Simple modules** (single failure mode): use `Error::VariantName(String)` directly (e.g. `Worktree(String)`).
- **Modules with multiple error kinds**: define a module-local error enum and nest it via `#[from]` + `#[error(transparent)]`. Example: `DiffPositionMapperError` has `Parse`, `FileNotFound`, `NoMappableLines` variants; the central enum wraps it as `DiffPositionMapper(#[from] DiffPositionMapperError)`.
- **Structured data**: `ProcessTimeout` carries stdout/stderr lines for resume logic.

## Async

- Tokio runtime (`#[tokio::main]`, `tokio::spawn`, `JoinSet` for parallel review phases).
- `AgentRunner::run` returns `impl Future` (not `async_trait` — uses RPITIT).
- Process spawning is async (`tokio::process::Command`), but GitHub/submission ops are sync (`std::process::Command`).
- Do not perform blocking work on Tokio worker threads. Use async APIs or `tokio::task::spawn_blocking` when blocking is unavoidable.
- For external commands/network boundaries, set explicit timeouts and surface stdout/stderr in errors for debugging.
- Design for cancellation safety and bounded resource usage.
- Prefer bounded queues/channels when producer rate can exceed consumer rate.
- Keep `unsafe` blocks minimal, documented, and invariants explicit.

## Config Merge Precedence

CLI flags > config file values > built-in defaults. This is enforced in `config.rs::merge()`. When adding new config fields, follow the same `cli.field.or(file.field).unwrap_or(default)` pattern.

## Observability

- Add `tracing` spans/events around key pipeline steps.

## API

- JSON error responses use all-lowercase strings, e.g. `{ "error": "invalid token claims" }`. No sentence case or punctuation.

## Compatibility And Versioning

- Target stable Rust in CI. If a change requires newer stable features, call it out in release notes and update CI/tooling docs in the same PR.
- Keep release tags and `Cargo.toml` version aligned (`vX.Y.Z` for `X.Y.Z`).
- Treat user-visible behavioral changes (CLI flags, config semantics, review/fix loop behavior) as release-notes required.
