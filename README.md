# rlph

Autonomous AI development loop CLI. Fetches tasks from issue trackers, spins up an AI agent to implement them, reviews the output, and submits pull requests — all in a single command.

## What is a Ralph Loop?

A ralph loop is a fully autonomous cycle where an AI agent picks up a task, implements it, reviews its own work, and submits the result — without human intervention. The loop looks like this:

```
fetch task → choose → implement → self-review → submit PR → repeat
```

Each iteration is self-contained: the agent works in an isolated git worktree, so the main branch stays clean regardless of what the agent produces. If the self-review fails, the agent iterates on its own changes up to a configurable number of rounds before giving up.

In practice, you label issues in your tracker (GitHub, Linear), point `rlph` at the repo, and walk away. The tool handles task selection (including dependency ordering), worktree lifecycle, agent orchestration across choose/implement/review phases, branch pushing, and PR creation.

`rlph` is agent-agnostic — it shells out to any CLI-based coding agent (Claude Code, OpenAI Codex) via a simple trait interface, so you can swap models without changing your workflow.

## Installation

```bash
cargo install --path .
```

## Quick Start

```bash
# Run a single iteration: pick one task, implement it, submit a PR
rlph --once

# Run continuously, polling for new tasks
rlph --continuous
```

## Configuration

Configuration is read from `.rlph/config.toml` in the project root. CLI flags override file values, which override built-in defaults.

```toml
source = "github"              # Task source: github, linear
runner = "codex"               # Agent runner: claude, codex
submission = "github"          # Submission backend: github, graphite
label = "rlph"                 # Label to filter eligible tasks
poll_interval = 60             # Poll interval in seconds (continuous mode)
worktree_dir = "../rlph-worktrees"  # Base directory for git worktrees
max_iterations = 10            # Max iterations before stopping (continuous mode)
dry_run = false                # Full loop without pushing or marking issues
agent_binary = "codex"         # Agent binary name
agent_model = "gpt-5.3-codex"  # Model for the agent (GPT 5.3)
agent_timeout = 300            # Agent timeout in seconds
max_review_rounds = 3          # Max review rounds per task
```

## CLI Reference

```
rlph — autonomous AI development loop

Usage: rlph [OPTIONS]

Options:
      --once                       Run a single iteration then exit
      --continuous                 Run continuously, polling for new tasks
      --max-iterations <N>         Max iterations before stopping (continuous mode)
      --dry-run                    Go through the full loop without pushing changes or marking issues
      --runner <RUNNER>            Agent runner: claude, codex
      --source <SOURCE>            Task source: github, linear
      --submission <BACKEND>       Submission backend: github, graphite
      --label <LABEL>              Label to filter eligible tasks
      --poll-interval <SECONDS>    Poll interval in seconds (continuous mode)
      --config <PATH>              Path to config file
      --worktree-dir <DIR>         Worktree base directory
      --agent-binary <NAME>        Agent binary name (default: codex)
      --agent-model <MODEL>        Model for the agent (default for codex: gpt-5.3-codex)
      --agent-timeout <SECONDS>    Agent timeout in seconds
      --max-review-rounds <N>      Max review rounds per task
  -h, --help                       Print help
  -V, --version                    Print version
```

Either `--once` or `--continuous` is required. `--max-iterations` and `--poll-interval` are only valid in continuous mode.

## How It Works

1. **Fetch** — Pulls eligible tasks from the configured source (GitHub issues, Linear tickets) filtered by label.
2. **Worktree** — Creates an isolated git worktree for the task.
3. **Implement** — Runs an AI agent (e.g., Codex) to implement the task.
4. **Review** — The agent reviews its own work, iterating up to `max_review_rounds`.
5. **Submit** — Opens a pull request via the configured submission backend.

## Development

```bash
cargo build          # Build
cargo test           # Run tests
cargo clippy         # Lint
cargo fmt            # Format
```

## Release Process

Releases are automated through GitHub Actions in two ways:

1. Tag pushes matching `v*.*.*`.
2. Manual dispatch (UI or CLI) with an explicit `tag` input.

Release steps:

1. Update `Cargo.toml` `version`.
2. Trigger release using one of:
   - Push a matching tag (for example, `v0.2.0` for `version = "0.2.0"`).
   - Run `gh workflow run release.yml --ref main -f tag=v0.2.0`.
3. The release workflow validates the tag format (`vX.Y.Z`) and enforces tag/version match.
4. It builds and uploads archives for:
   - `x86_64-unknown-linux-gnu`
   - `aarch64-unknown-linux-gnu`
   - `x86_64-apple-darwin`
   - `aarch64-apple-darwin`

Each release archive includes the `rlph` binary, `README.md`, and `LICENSE`.

## Inspired by

- [lalph](https://github.com/tim-smart/lalph) by [Tim Smart](https://x.com/tim_smart)
- [accountability](https://github.com/mikearnaldi/accountability) by [Michael Arnaldi](https://x.com/MichaelArnaldi)
- [skills](https://github.com/mattpocock/skills) by [Matt Pocock](https://x.com/mattpocockuk)
- [The Loop](https://ghuntley.com/loop/) by [Geoff Huntley](https://x.com/GeoffreyHuntley)

## License

See [LICENSE](LICENSE) for details.
