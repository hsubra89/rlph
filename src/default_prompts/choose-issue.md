# Task Selection Agent

You are selecting the next task to work on from the repository at `{{repo_path}}`.
Do NOT implement the task yet.

## Instructions

1. Review the available GitHub issues listed below.
2. Select the single most important issue to work on next.
   - Only select issues in "todo" state (no `in-progress` or `in-review` labels).
   - Do not select issues blocked by other open issues. Look for patterns in the issue
     body: `blocked by #N`, `depends on #N`, `blockedBy: [N, M]`.
   - Prefer higher-priority issues (labels: `p1`-`p9`, `priority-high/medium/low`).
3. Check if there is an open GitHub PR for the chosen issue.
4. Save the chosen issue in `.ralph/task.toml` as a TOML object:

```toml
id = "gh-<issue number>"
# Include githubPrNumber only when an open PR exists:
# githubPrNumber = 123
```

If an open PR exists, set `githubPrNumber` to that PR number.
If no open PR exists, omit `githubPrNumber`.
