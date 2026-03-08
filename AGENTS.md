# rlph

Mixed-language monorepo: Rust CLI (`crates/`) + TypeScript packages (`packages/`).

## Layout

- `crates/rlph/` — Rust binary crate (edition 2024). Autonomous AI dev-loop CLI.
- `packages/` — TypeScript packages (pnpm workspace).
- `justfile` — Unified task runner.

## Commands

- **All checks:** `just check`
- **Format:** `cargo fmt --all -- --check`
- **Lint:** `cargo clippy --all-targets --all-features -- -D warnings`
- **Test:** `cargo nextest run`
- **Integration (CI gate):** `cargo nextest run --profile integration -E 'binary(cli_binary)'`
- **Integration (full local sweep, optional):** `cargo nextest run --profile integration`
- **Single test:** `cargo nextest run -E 'test(test_name)'`

## Development Methodology

- TDD (red-green-refactor) for features. Use `/tdd` skill.
- Mirror CI locally before finishing:
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo nextest run`
  - `cargo nextest run --profile integration -E 'binary(cli_binary)'`

## Docs

Read when working in the relevant area:

- [Architecture](docs/architecture.md) — key paths, module responsibilities, trait system, orchestrator pipeline
- [Testing](docs/testing.md) — mocking strategy, integration tests, what to test
- [Conventions](docs/conventions.md) — error handling, async patterns, dependency policy
- [Engineering Checklist](docs/engineering-checklist.md) — CI gates, async/concurrency patterns, and release hygiene
