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
runner = "claude"              # Agent runner: claude, codex
submission = "github"          # Submission backend: github, graphite
label = "rlph"                 # Label to filter eligible tasks
poll_interval = 60             # Poll interval in seconds (continuous mode)
worktree_dir = "../rlph-worktrees"  # Base directory for git worktrees
max_iterations = 10            # Max iterations before stopping (continuous mode)
dry_run = false                # Full loop without pushing or marking issues
agent_binary = "claude"        # Agent binary name
agent_model = "claude-opus-4-6"  # Model for the agent
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
      --agent-binary <NAME>        Agent binary name (default: claude)
      --agent-model <MODEL>        Model for the agent (e.g., claude-opus-4-6)
      --agent-timeout <SECONDS>    Agent timeout in seconds
      --max-review-rounds <N>      Max review rounds per task
  -h, --help                       Print help
  -V, --version                    Print version
```

Either `--once` or `--continuous` is required. `--max-iterations` and `--poll-interval` are only valid in continuous mode.

## How It Works

1. **Fetch** — Pulls eligible tasks from the configured source (GitHub issues, Linear tickets) filtered by label.
2. **Worktree** — Creates an isolated git worktree for the task.
3. **Implement** — Runs an AI agent (e.g., Claude) to implement the task.
4. **Review** — The agent reviews its own work, iterating up to `max_review_rounds`.
5. **Submit** — Opens a pull request via the configured submission backend.

## Development

```bash
cargo build          # Build
cargo test           # Run tests
cargo clippy         # Lint
cargo fmt            # Format
```

## License

See [LICENSE](LICENSE) for details.
