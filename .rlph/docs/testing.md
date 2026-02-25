# Testing Patterns

## Test Organization

- **Unit tests:** In-module `#[cfg(test)] mod tests` blocks. Run with `cargo test`.
- **Integration tests:** In `tests/` directory. Most require `RLPH_INTEGRATION=1` env var.
  - `cli_binary.rs` — Full CLI binary tests via `assert_cmd`
  - `orchestrator_integration.rs` — End-to-end with mock runners
  - `worktree_integration.rs` — Real git worktree operations
  - `process_integration.rs` — Signal handling, timeouts
  - `codex_runner_integration.rs` — Codex-specific behavior
  - `prd_integration.rs` — PRD session flow

## Mocking Strategy

No mocking framework. Mocks are hand-rolled per module:

- **`GhClient` trait** (`sources/github.rs`) — `MockGhClient` returns canned `Result<String>` responses. Wraps a `RefCell<Vec<Result<String>>>` for ordered response playback.
- **`CallbackRunner`** (`runner.rs`) — Takes an `Arc<dyn Fn(Phase, String, PathBuf) -> Future<Result<RunResult>>>`. Use for orchestrator tests.
- **`ReviewRunnerFactory` trait** (`orchestrator.rs`) — Override to inject mock runners for review phases specifically.

Pattern: define a trait for the external dependency, implement it for real + test, inject via constructor or generic.

## What to Test

- **Config parsing:** Every valid/invalid combination, CLI-overrides-file precedence, default fallbacks.
- **Task filtering:** Eligibility labels, dependency graph cycles, priority ordering.
- **Signal/output parsing:** `extract_session_id`, `extract_claude_result`, `REVIEW_APPROVED`/`REVIEW_NEEDS_FIX` parsing.
- **State management:** Concurrent modifications (flock correctness), roundtrip serialization, corruption recovery.
- **Worktree ops:** Branch name validation, create/remove lifecycle, path canonicalization.

## Concurrency in Tests

Use `#[serial_test::serial]` for tests that share global state (process signals, file locks). Most unit tests are safe to run in parallel.
