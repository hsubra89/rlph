# rlph

Autonomous AI development loop CLI. `rlph` picks up tasks from a source (GitHub Issues, Linear), spins up an AI agent to implement them in isolated git worktrees, runs a review cycle, and submits the result as a pull request.

## Installation

```bash
cargo install --path .
```

## Quick start

```bash
# Run a single iteration: pick one task, implement it, submit a PR
rlph --once

# Run continuously, polling every 60 seconds
rlph --continuous
```

## Usage

```
rlph [OPTIONS]
```

Either `--once` or `--continuous` is required.

### Execution mode

| Flag | Description |
|---|---|
| `--once` | Run a single iteration then exit |
| `--continuous` | Run continuously, polling for new tasks |
| `--max-iterations <N>` | Stop after N iterations (continuous mode only) |

`--once` conflicts with `--continuous` and `--max-iterations`.

### Behavior

| Flag | Description |
|---|---|
| `--dry-run` | Go through the full loop without pushing changes or marking issues |

### Component selection

| Flag | Values | Default |
|---|---|---|
| `--source <TYPE>` | `github`, `linear` | `github` |
| `--runner <TYPE>` | `bare`, `docker` | `bare` |
| `--submission <TYPE>` | `github`, `graphite` | `github` |

### Task filtering

| Flag | Description | Default |
|---|---|---|
| `--label <LABEL>` | Only pick tasks with this label | `rlph` |

### Polling

| Flag | Description | Default |
|---|---|---|
| `--poll-interval <SECS>` | Seconds between polls (continuous mode) | `60` |

### Agent configuration

| Flag | Description | Default |
|---|---|---|
| `--agent-binary <BIN>` | Agent binary to invoke | `claude` |
| `--agent-model <MODEL>` | Model for the agent to use | — |
| `--agent-timeout <SECS>` | Agent timeout in seconds | — |
| `--max-review-rounds <N>` | Maximum review rounds per task | `3` |

### Worktree

| Flag | Description | Default |
|---|---|---|
| `--worktree-dir <DIR>` | Base directory for git worktrees | `../rlph-worktrees` |

### Configuration file

| Flag | Description |
|---|---|
| `--config <PATH>` | Path to config file |

By default, `rlph` looks for `.rlph/config.toml` in the current directory. If the file doesn't exist, built-in defaults are used. If `--config` is provided and the file is missing, `rlph` exits with an error.

## Configuration file

All fields are optional. CLI flags take precedence over config file values.

```toml
# .rlph/config.toml

source = "github"           # github | linear
runner = "bare"             # bare | docker
submission = "github"       # github | graphite
label = "rlph"              # label to filter tasks
poll_interval = 60          # seconds between polls (must be > 0)
worktree_dir = "../rlph-worktrees"
max_iterations = 10         # optional iteration cap
dry_run = false

# Agent settings
agent_binary = "claude"
agent_model = "opus"        # optional
agent_timeout = 300         # optional, in seconds
max_review_rounds = 3
```

### Precedence

CLI flags > config file > built-in defaults.

## How it works

1. **Fetch** — queries the task source for eligible issues matching the configured label.
2. **Choose** — the agent selects a task, respecting dependency order.
3. **Worktree** — creates an isolated git worktree for the task.
4. **Implement** — the agent implements the task in the worktree.
5. **Review** — the agent reviews its own work (up to `max_review_rounds`).
6. **Submit** — pushes a branch and creates a PR via the submission backend.

## Development

```bash
cargo build          # build
cargo test           # run tests
cargo clippy         # lint
cargo fmt            # format
```

## License

See [LICENSE](LICENSE) for details.
