# rlph

Rust binary crate (edition 2024). Autonomous AI dev-loop CLI: fetches tasks, spins up worktrees, runs coding agents through implement/review phases, submits PRs.

## Commands

- **Lint:** `cargo clippy`
- **Test:** `cargo nextest run`
- **Integration:** `cargo nextest run --profile integration`
- **Single test:** `cargo nextest run -E 'test(test_name)'`

## Development Methodology

- TDD (red-green-refactor) for features. Use `/tdd` skill.
- `cargo fmt && cargo clippy` before finishing — zero warnings.

## Docs

Read when working in the relevant area:

- [Architecture](.rlph/docs/architecture.md) — key paths, module responsibilities, trait system, orchestrator pipeline
- [Testing](.rlph/docs/testing.md) — mocking strategy, integration tests, what to test
- [Conventions](.rlph/docs/conventions.md) — error handling, async patterns, dependency policy
