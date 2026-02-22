# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

- **Build:** `cargo build`
- **Run:** `cargo run`
- **Test:** `cargo test`
- **Lint:** `cargo clippy`
- **Format:** `cargo fmt`
- **Single test:** `cargo test test_name`

## Project Overview

Rust binary crate (`rlph`), edition 2024. Autonomous AI development loop CLI.

### Dependencies
clap (CLI parsing), serde + toml (config), thiserror (errors), tracing + tracing-subscriber (logging).

### Module Structure
- `cli` — Argument parsing via clap
- `config` — `.rlph/config.toml` parsing, validation, CLI merge
- `error` — Common error type used across all modules
- `sources` — `TaskSource` trait (fetch, mark_in_progress, mark_done, get_task_details)
- `runner` — `AgentRunner` trait (run agent for choose/implement/review phases)
- `submission` — `SubmissionBackend` trait (submit PR/diff)
- `orchestrator` — Core loop (stub)
- `worktree` — Git worktree management (stub)
- `deps` — Dependency graph parsing (stub)
- `prompts` — Template loading and rendering (stub)
- `state` — Local state management (stub)
- `process` — Child process lifecycle (stub)

### Design Assumptions
- **Agent output is trusted.** The orchestrator assumes zero hallucination in structured agent responses (task selection, `REVIEW_COMPLETE` signals, PR numbers). We do not verify agent-reported values against external state. This is a deliberate trade-off — the system is only as reliable as the underlying model.
