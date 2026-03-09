# brrr

Mixed-language monorepo: Rust CLI (`crates/`) + TypeScript packages (`packages/`).

## Layout

| Directory | Language | What |
|-----------|----------|------|
| `crates/brrr/` | Rust 2024 | CLI binary — orchestrator, agent runners, task sources, PR submission |
| `crates/brrr-core/` | Rust 2024 | Pure domain types — IDs, tasks, dependency graphs, SCC |
| `packages/server/` | TypeScript (Effect) | Auth API server — SSH login, JWT, rate limiting |

## Commands

- **All checks:** `just check` (fmt-check + lint + test + ts-build)
- **Format:** `just fmt` / `just fmt-check`
- **Lint:** `just lint` (clippy + oxlint)
- **Test (unit):** `just test`
- **Integration (CI gate):** `just integration`
- **Integration (full):** `just integration-all`
- **Single Rust test:** `cargo nextest run -E 'test(test_name)'`
- **TS dev server:** `just ts-dev`

## Development Methodology

- TDD (red-green-refactor). Use `/tdd` skill.
- Mirror CI before finishing: `just check && just integration`

## Docs

Read when working in the relevant area:

- [Architecture](docs/architecture.md) — crate structure, orchestrator pipeline, core traits, module responsibilities
- [Testing](docs/testing.md) — mocking strategy, integration tests, execution policy, coverage expectations
- [Conventions](docs/conventions.md) — design principles, error handling, async patterns, config merge, observability
- [Specifications/](specs/) — specification files for systems built and being built
