# crates/brrr

CLI binary crate. Orchestrates the autonomous AI dev loop: fetch tasks, run coding agents, manage worktrees, submit PRs, run reviews.

## Orchestrator Pipeline

See [docs/architecture.md](../../docs/architecture.md#orchestrator-pipeline) for the full pipeline. In brief: fetch → choose (skipped if one eligible) → implement → PR → review → cleanup.

## Core Traits

See [docs/architecture.md](../../docs/architecture.md#core-traits). Key file locations in this crate:

- **`TaskSource`** — `sources/mod.rs`
- **`AgentRunner`** — `runner.rs`
- **`SubmissionBackend`** — `submission.rs`
- **`ReviewRunnerFactory`**, **`CorrectionRunner`**, **`ProgressReporter`** — `orchestrator.rs`

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

## Error Handling

See [docs/conventions.md](../../docs/conventions.md#error-handling). Crate-specific: `ProcessTimeout` carries stdout/stderr for agent session resume logic.

## Testing

See [docs/testing.md](../../docs/testing.md). Key mock types in this crate:

- **`MockGhClient`** — `sources/github.rs`
- **`CallbackRunner`** — `runner.rs`
- **`ReviewRunnerFactory`** — `orchestrator.rs`

## Key Patterns

See [docs/architecture.md](../../docs/architecture.md#design-decisions) for shared patterns. Crate-specific:

- **Process spawning.** Async via `tokio::process::Command` with heartbeat timeout. GitHub/submission ops are sync (`std::process::Command`).

## Commands

See root `CLAUDE.md` for canonical commands (`just test`, `just integration`, etc.).
