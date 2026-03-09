# crates/brrr

CLI binary crate. Orchestrates the autonomous AI dev loop: fetch tasks, run coding agents, manage worktrees, submit PRs, run reviews.

## Orchestrator Pipeline

```
Fetch tasks (TaskSource) → filter by dependency graph
  → Choose phase: agent picks task
  → Create worktree
  → Implement phase: agent codes in worktree
  → Push + submit PR (SubmissionBackend)
  → Review pipeline (parallel phases → aggregate → post findings)
  → Cleanup worktree
```

## Core Traits

All extensibility via traits dispatched through enums (`AnySource`, `AnyRunner`):

- **`TaskSource`** (`sources/mod.rs`) — fetch/filter tasks. Impls: `GitHubSource` (gh CLI), `LinearSource` (API).
- **`AgentRunner`** (`runner.rs`) — run agent for a phase. Impls: `ClaudeRunner`, `CodexRunner`, `OpencodeRunner`, `CallbackRunner` (tests).
- **`SubmissionBackend`** (`submission.rs`) — submit PRs, upsert review comments. Impl: `GitHubSubmission` (gh CLI).
- **`ReviewRunnerFactory`** (`orchestrator.rs`) — inject mock runners for review phases in tests.

## Module Boundaries

| Module | Does | Does NOT |
|--------|------|----------|
| `orchestrator` | Sequences phases, manages iteration lifecycle | Know about CLI args or agent CLIs |
| `runner` | Builds agent CLI commands, handles timeout/resume | Know about tasks or git |
| `process` | Spawns child processes, signal forwarding, heartbeat | Know about agents or phases |
| `sources/` | Fetches/filters tasks from issue trackers | Know about worktrees or PRs |
| `submission` | Creates PRs, manages review comments | Know about tasks or agents |
| `worktree` | Creates/removes git worktrees | Know about tasks |
| `prompts` | Loads templates + user overrides, `{{var}}` substitution | Execute agents |
| `config` | Merges CLI > file > defaults | Validate business logic |

## Key Patterns

- **`gh` CLI as GitHub layer.** All GitHub ops shell out to `gh` — no Rust HTTP client.
- **Prompt overrides.** Users place custom templates in `.brrr/prompts/` to override embedded defaults in `src/default_prompts/`.
- **Review comment upserts.** `<!-- brrr-review -->` HTML marker for idempotent updates.
- **Untrusted content wrapping.** External PR comments wrapped in `<untrusted-content>` tags.
- **Process spawning.** Async via `tokio::process::Command` with heartbeat timeout. GitHub/submission ops are sync (`std::process::Command`).

## Error Handling

Central `Error` enum in `error.rs` with `thiserror`. Module-local error enums nested via `#[from]` + `#[error(transparent)]`. `ProcessTimeout` carries stdout/stderr for resume logic. No `.unwrap()` in library code.

## Testing

- Unit tests: in-module `#[cfg(test)]` blocks
- Integration tests: `tests/` directory (cli_binary, orchestrator, worktree, process, prd, runner-specific)
- Mocks: hand-rolled (`MockGhClient`, `CallbackRunner`, `ReviewRunnerFactory`)
- `#[serial_test::serial]` for tests sharing global state
- Run: `cargo nextest run` (unit) + `cargo nextest run --profile integration -E 'binary(cli_binary)'` (CI gate)

## Commands

- **Lint:** `cargo clippy --all-targets --all-features -- -D warnings`
- **Format:** `cargo fmt --all`
- **Test:** `cargo nextest run`
- **Integration:** `cargo nextest run --profile integration -E 'binary(cli_binary)'`
