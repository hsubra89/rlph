# rlph

Rust binary crate (edition 2024). Autonomous AI dev-loop CLI: fetches tasks, spins up worktrees, runs coding agents through implement/review phases, submits PRs.

## Commands

- **Lint:** `cargo clippy`
- **Test:** `cargo nextest run`
- **Integration:** `RLPH_INTEGRATION=1 cargo nextest run --test cli_binary`
- **Single test:** `cargo nextest run -E 'test(test_name)'`

## Workflow

- TDD (red-green-refactor) for features. Use `/tdd` skill.
- `cargo clippy` before finishing — zero warnings.

## Key Paths

| What | Where |
|------|-------|
| CLI entry + orchestrator setup | `src/main.rs` |
| Core loop (choose → implement → review) | `src/orchestrator.rs` |
| Agent process spawning | `src/runner.rs`, `src/process.rs` |
| GitHub/Linear task sources | `src/sources/` |
| PR submission | `src/submission.rs` |
| Prompt templates | `src/default_prompts/` |
| Config | `src/config.rs`, `.rlph/config.toml` |
| Local state | `src/state.rs`, `.rlph/state/` |

## Docs

Read when working in the relevant area:

- [Architecture](.rlph/docs/architecture.md) — module responsibilities, trait system, orchestrator pipeline
- [Testing](.rlph/docs/testing.md) — mocking strategy, integration tests, what to test
- [Conventions](.rlph/docs/conventions.md) — error handling, async patterns, edition 2024 features
