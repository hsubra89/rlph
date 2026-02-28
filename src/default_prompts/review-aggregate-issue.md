# Review Aggregation Agent

Multiple review agents have independently analyzed an implementation. Your job is to aggregate their findings into a single coherent PR comment and decide whether the code is ready to merge.

## Issue

- **Title:** {{issue_title}}
- **Number:** #{{issue_number}}
- **URL:** {{issue_url}}
- **Branch:** {{branch_name}}
- **Worktree:** {{worktree_path}}
- **Repository:** {{repo_path}}

### Description

{{issue_body}}

## Review Outputs

{{review_outputs}}

## Instructions

1. Read all review outputs above carefully.
2. De-duplicate findings that appear in multiple reviews.
3. Prioritize by severity: critical > warning > info.
4. Compose a clear, actionable PR comment summarizing all findings.
5. Decide: are there any critical or warning findings that require code changes?

## Output

Respond with a single JSON object (no markdown fences, no commentary outside the JSON). The schema:

```json
{
  "verdict": "approved" | "needs_fix",
  "comment": "<markdown PR comment body — list issues as a task list (`- [ ] ...`)>",
  "findings": [
    {
      "id": "<short-slugified-id>",
      "file": "<path>",
      "line": <number>,
      "severity": "critical" | "warning" | "info",
      "description": "<what is wrong>",
      "category": "<optional category tag, e.g. correctness, security, style>",
      "depends_on": ["<other-finding-id>"] | null
    }
  ],
  "fix_instructions": "<concise instructions for the fix agent, or null if approved>"
}
```

- Set `verdict` to `"approved"` if there are no actionable findings requiring code changes.
- Set `verdict` to `"needs_fix"` if code changes are needed, and populate `fix_instructions`.
- `findings` may be empty when the code is clean.
- `fix_instructions` must be `null` when `verdict` is `"approved"`.
