# rlph

Autonomous AI development loop CLI. Fetches tasks from issue trackers, spins up an AI agent to implement them, reviews the output, and submits pull requests — all in a single command.

## What is a Ralph Loop?

A ralph loop is a fully autonomous cycle where an AI agent picks up a task, implements it, reviews its own work, and submits the result — without human intervention. The loop looks like this:

```
fetch task → choose → implement → self-review → submit PR → repeat
```

Each iteration is self-contained: the agent works in an isolated git worktree, so the main branch stays clean regardless of what the agent produces.

In practice, you label issues in your tracker (GitHub, Linear), point `rlph` at the repo, and walk away. The tool handles task selection (including dependency ordering), worktree lifecycle, agent orchestration across choose/implement/review phases, branch pushing, and PR creation.

`rlph` is agent-agnostic — it shells out to any CLI-based coding agent (Claude Code, OpenAI Codex, OpenCode) via a simple trait interface, so you can swap models without changing your workflow.

## Installation

### Quick install (macOS / Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/hsubra89/rlph/main/install.sh | sh
```

To install to a custom directory (e.g. `/usr/local/bin`):

```bash
RLPH_INSTALL_DIR=/usr/local/bin curl -fsSL https://raw.githubusercontent.com/hsubra89/rlph/main/install.sh | sh
```

### From source

```bash
cargo install --path .
```

## Quick Start

```bash
# Run a single iteration: pick one task, implement it, submit a PR
rlph --once

# Run continuously, polling for new tasks
rlph --continuous

# Review an existing PR directly (accepts PR number or URL)
rlph review 123
rlph review https://github.com/owner/repo/pull/123

# Fix checked review findings on a PR
rlph fix 123
```

## Configuration

Configuration is read from `.rlph/config.toml` in the project root. CLI flags override file values, which override built-in defaults.

```toml
source = "github"              # Task source: github, linear
runner = "claude"              # Agent runner: claude, codex, opencode
submission = "github"          # Submission backend: github
label = "rlph"                 # Label to filter eligible tasks
poll_seconds = 30              # Poll interval in seconds (continuous mode)
worktree_dir = "../rlph-worktrees"  # Base directory for git worktrees
base_branch = "main"           # Base branch for worktrees and PRs (auto-detected)
max_iterations = 10            # Max iterations before stopping (continuous mode)
dry_run = false                # Full loop without pushing or marking issues
agent_binary = "claude"        # Agent binary name
agent_model = "claude-opus-4-6"  # Model for the agent
agent_timeout = 600            # Agent timeout in seconds
implement_timeout = 1800       # Implement phase timeout in seconds (30 min)
agent_effort = "high"          # Effort level: low, medium, high (Claude/Codex only)
agent_variant = "high"         # Variant: low, high (OpenCode only)
agent_timeout_retries = 1      # Retries on agent timeout (resumes session)
worktree_setup_script = ".rlph/worktree-setup.sh"  # Script to run after worktree creation (see below)
```

### Worktree Setup Script

After creating a worktree, `rlph` can run a shell script for project-specific setup (installing dependencies, copying `.env` files, etc.).

**Convention:** If `.rlph/worktree-setup.sh` exists in the repo root, it is **automatically executed** after every worktree creation — no configuration required. This follows the same trust model as `Makefile`s and `.github/workflows`: anyone with commit access to the repo can introduce or modify the script.

**Overrides:**

- Set `worktree_setup_script` in `.rlph/config.toml` to use a different path.
- Set `worktree_setup_script = ""` (empty string) to explicitly disable auto-execution.

The script receives the repo root as its first argument and runs with the new worktree as its working directory. A non-zero exit code aborts worktree creation.

## CLI Reference

```
rlph — autonomous AI development loop

Usage: rlph [OPTIONS] [COMMAND]

Options:
      --once                             Run a single iteration then exit
      --continuous                       Run continuously, polling for new tasks
      --max-iterations <N>               Maximum iterations before stopping
      --dry-run                          Full loop without pushing changes or marking issues
      --runner <RUNNER>                  Agent runner: claude, codex, opencode
      --source <SOURCE>                  Task source: github, linear
      --submission <BACKEND>             Submission backend: github
      --label <LABEL>                    Label to filter eligible tasks
      --poll-seconds <SECONDS>           Poll interval in seconds (continuous mode)
      --config <PATH>                    Path to config file
      --worktree-dir <DIR>               Worktree base directory
      --base-branch <BRANCH>             Base branch for worktrees and PRs (default: main)
      --agent-binary <NAME>              Agent binary name (default: claude)
      --agent-model <MODEL>              Model for the agent (default: claude-opus-4-6)
      --agent-timeout <SECONDS>          Agent timeout in seconds
      --implement-timeout <SECONDS>      Implement phase timeout in seconds (default: 1800)
      --agent-effort <LEVEL>             Effort level: low, medium, high (Claude/Codex only)
      --agent-variant <VARIANT>          Variant: low, high (OpenCode only)
      --agent-timeout-retries <N>        Retries on agent timeout (session resume)
  -h, --help                             Print help
  -V, --version                          Print version

Commands:
  init                                   Initialize project source integration
  review <PR_NUMBER|URL>                 Run review phases directly for an existing GitHub PR
  fix <PR_NUMBER>                        Fix review findings for an existing GitHub PR
  prd [DESCRIPTION]                      Launch an interactive PRD-writing session
```

Specify one of `--once`, `--continuous`, or `--max-iterations`. `--continuous` and `--max-iterations` can be combined.

## How It Works

### Main loop (`--once` / `--continuous`)

1. **Fetch** — Pulls eligible tasks from the configured source (GitHub issues, Linear tickets) filtered by label. Respects dependency ordering so blocked tasks are skipped. Auto-selects when only one task is eligible.
2. **Worktree** — Creates an isolated git worktree for the task. Runs the worktree setup script if present.
3. **Implement** — Runs an AI agent to implement the task. Agent output streams to stderr in real time. If the agent times out, `rlph` resumes the session automatically (up to `agent_timeout_retries` times).
4. **Review** — Parallel multi-phase review: independent agents evaluate correctness, security, and hygiene concurrently, then an aggregator consolidates findings into a verdict. Findings are posted as a PR comment.
5. **Submit** — Opens a pull request and posts structured review findings as a PR comment with per-finding checkboxes.

### `rlph review <PR>`

Runs the review pipeline directly on an existing PR (accepts PR number or full GitHub URL). Creates a worktree from the PR's head branch, runs all review phases, and posts findings.

### `rlph fix <PR>`

Polls a PR's review comment for checked finding checkboxes. Each checked finding spawns a fix agent in its own worktree. Findings support dependency ordering — a finding won't be fixed until its dependencies are resolved. Results are reflected back into the PR comment. Use `--dry-run` to parse and display findings without applying fixes.

### `rlph init`

Initializes the configured task source. For Linear: interactive team selection and label creation. For GitHub: no-op (labels are created on first use).

### `rlph prd [DESCRIPTION]`

Launches an interactive PRD-writing session with an AI agent. The agent guides you through requirements gathering and outputs a structured PRD. Optionally provide a seed description.

## Development

```bash
cargo build          # Build
cargo test           # Run tests
cargo clippy         # Lint
cargo fmt            # Format
```

### Git hooks

The repo includes a pre-commit hook at `.hooks/pre-commit`. To activate it, symlink it into your local `.git/hooks/`:

```bash
ln -sf ../../.hooks/pre-commit .git/hooks/pre-commit
```

This approach works in both the main checkout and git worktrees (unlike `core.hooksPath`, which breaks on relative paths in worktrees).

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
