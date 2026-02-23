# rlph

Autonomous AI development loop CLI. Fetches tasks from issue trackers, spins up an AI agent to implement them, reviews the output, and submits pull requests — all in a single command.

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
poll_seconds = 30              # Poll interval in seconds (continuous mode)
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
      --max-iterations <N>         Maximum iterations before stopping
      --dry-run                    Go through the full loop without pushing changes or marking issues
      --runner <RUNNER>            Agent runner: claude, codex
      --source <SOURCE>            Task source: github, linear
      --submission <BACKEND>       Submission backend: github, graphite
      --label <LABEL>              Label to filter eligible tasks
      --poll-seconds <SECONDS>     Poll interval in seconds (continuous mode)
      --config <PATH>              Path to config file
      --worktree-dir <DIR>         Worktree base directory
      --agent-binary <NAME>        Agent binary name (default: codex)
      --agent-model <MODEL>        Model for the agent (default for codex: gpt-5.3-codex)
      --agent-timeout <SECONDS>    Agent timeout in seconds
      --max-review-rounds <N>      Max review rounds per task
  -h, --help                       Print help
  -V, --version                    Print version
```

Specify one of `--once`, `--continuous`, or `--max-iterations`. `--continuous` and `--max-iterations` can be combined.

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

## License

See [LICENSE](LICENSE) for details.
