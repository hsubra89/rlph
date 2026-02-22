# Task Selection Agent

Your job is to choose the next GitHub issue to work on and save it in
`.ralph/task.json`.
Do NOT implement the task yet.

The following instructions should be done without interaction or asking for permission.

- Decide which single issue to work on next from the GitHub issues listed below.
  This should be the issue YOU decide is most important to work on next, not just the
  first issue in the list.
  - Only select issues in "todo" state (no `in-progress` or `in-review` labels).
  - Do not select issues blocked by other open issues. Look for patterns in the issue
    body: `blocked by #N`, `depends on #N`, `blockedBy: [N, M]`.
  - Prefer higher-priority issues (labels: `p1`-`p9`, `priority-high/medium/low`).
- Check if there is an open GitHub PR for the chosen issue. If there is, include the PR
  number in `.ralph/task.json`.
  - Only include open PRs that are not merged.
  - The PR should reference the issue number in title/body.
- Save the chosen issue in `.ralph/task.json` using exactly this format:

```json
{
  "id": "gh-<issue number>",
  "githubPrNumber": null
}
```

Set `githubPrNumber` to the PR number if one exists, otherwise use `null`.
