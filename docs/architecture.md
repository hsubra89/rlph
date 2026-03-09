# Architecture

## Crate Structure

| Crate | Purpose |
|-------|---------|
| `brrr-core` | Pure domain types and algorithms (no IO). IDs, task/priority types, dependency graphs, Tarjan's SCC. |
| `brrr` | Binary crate. CLI, orchestrator, agent runners, process spawning, task sources, PR submission, fix loop. |

The `brrr` crate re-exports core modules (`ids`, `scc`, `deps`, `task`) so existing `crate::` paths continue to work.

## Orchestrator Pipeline

```
Fetch tasks (TaskSource) → filter by dependency graph
  → Choose phase (skipped if one eligible)
  → Create worktree → Implement phase → Push branch, submit PR
  → Review pipeline (parallel phases → aggregate → post findings)
  → Cleanup worktree
```

Review runs once — no full retry loop. JSON parse failures trigger session-resume correction (max 2 retries).

## Fix Loop

Separate command (`brrr fix`), polls a PR for 🚀-reacted findings:

```
Extract findings from HTML markers in PR comments
  → Schedule batch (respects deps, severity) → Agent fixes → Push
  → Update reactions (🚀 → 👍 or 😕) → Repeat until idle
```

## Core Traits

- **`TaskSource`** — fetch/filter tasks, mark state. Implementations: `GitHubSource` (`gh` CLI), `LinearSource` (GraphQL). Dispatched via `AnySource` enum.
- **`AgentRunner`** — run agent for a phase. Implementations: `ClaudeRunner`, `CodexRunner`, `OpencodeRunner`, `CallbackRunner` (tests). Dispatched via `AnyRunner` enum.
- **`SubmissionBackend`** — submit PRs, manage review comments, inline reviews. Single implementation: `GitHubSubmission` (`gh` CLI).
- **`ReviewRunnerFactory`**, **`CorrectionRunner`**, **`ProgressReporter`** — orchestrator-internal, injectable for tests.

## Design Decisions

- **Agent output is trusted.** No verification of agent-reported task IDs, review signals, or PR numbers.
- **`gh` CLI as GitHub API layer.** Leverages user's existing auth; avoids token management.
- **Worktree isolation.** Build tasks get separate worktrees; fix loop shares one.
- **Prompt template overrides.** `.brrr/prompts/` overrides embedded defaults.
- **Idempotent review comments.** `<!-- brrr-review -->` marker for summary upserts; `<!-- brrr-finding:{json} -->` for structured finding extraction.
- **Untrusted content wrapping.** External PR comments wrapped in `<untrusted-content>` tags to mitigate prompt injection.
- **JSON correction retry.** Malformed agent output → session resume with error description (max 2 retries).
- **Timeout and resume.** On agent timeout, session ID extracted from stdout and resumed (up to `max_timeout_retries`).
