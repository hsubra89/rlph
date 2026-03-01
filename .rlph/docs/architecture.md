# Architecture

## Orchestrator Pipeline

The core loop in `orchestrator.rs` runs this sequence per iteration:

```
Fetch tasks (TaskSource) → filter by dependency graph (deps.rs)
  → Choose phase: agent picks task, writes .rlph/task.toml
  → Create worktree (worktree.rs)
  → Implement phase: agent codes in worktree
  → Push branch, submit PR (SubmissionBackend)
  → Review pipeline (parallel phases → aggregate → fix loop)
  → Cleanup worktree
```

Review pipeline runs up to `max_review_rounds` (default 3). Each round:
1. Runs review phases in parallel (correctness, security, hygiene by default)
2. Aggregation agent combines findings, emits `REVIEW_APPROVED` or `REVIEW_NEEDS_FIX: <instructions>`
3. If needs fix: fix agent applies changes, pushes, next round

## Core Traits

All extensibility is through traits dispatched via enums (`AnySource`, `AnyRunner`):

- **`TaskSource`** (`sources/mod.rs`) — fetch eligible tasks, mark in-progress/in-review, get details. Implementations: `GitHubSource` (via `gh` CLI), `LinearSource` (via API).
- **`AgentRunner`** (`runner.rs`) — run an agent for a phase with a prompt in a working directory. Implementations: `ClaudeRunner`, `CodexRunner`, `CallbackRunner` (tests).
- **`SubmissionBackend`** (`submission.rs`) — submit PRs, find existing PRs, upsert review comments. Implementation: `GitHubSubmission` (via `gh` CLI).

## Module Responsibilities

| Module | Does | Does NOT |
|--------|------|----------|
| `orchestrator` | Sequences phases, manages iteration lifecycle | Know about CLI args or specific agent CLIs |
| `runner` | Builds agent CLI commands, handles timeout/resume | Know about tasks or git |
| `process` | Spawns child processes, signal forwarding, heartbeat | Know about agents or phases |
| `sources` | Fetches/filters tasks from issue trackers | Know about worktrees or PRs |
| `submission` | Creates PRs, manages review comments | Know about tasks or agents |
| `worktree` | Creates/removes git worktrees | Know about tasks |
| `state` | TOML persistence with flock-based locking | Know about git or agents |
| `prompts` | Loads templates (embedded defaults + overrides), `{{var}}` substitution | Execute agents |
| `deps` | Parses dependency references, Tarjan's SCC for cycle detection | Fetch tasks |
| `config` | Merges CLI flags → config file → defaults | Validate business logic beyond field values |

## Design Decisions

- **Agent output is trusted.** No verification of agent-reported task IDs, review signals, or PR numbers. The system is only as reliable as the underlying model.
- **`gh` CLI as GitHub API layer.** All GitHub operations shell out to `gh` rather than using a Rust HTTP client. This leverages the user's existing auth and avoids token management.
- **Worktree isolation.** Each task runs in a separate git worktree so main branch stays clean and multiple tasks could theoretically run in parallel.
- **Prompt template overrides.** Users can place custom templates in `.rlph/prompts/` to override embedded defaults without modifying the binary.
- **Review comment upserts.** Review comments are identified by `<!-- rlph-review -->` HTML marker for idempotent updates.
- **Untrusted PR comment wrapping.** External PR comments are wrapped in `<untrusted-content>` tags in prompts to mitigate prompt injection.
