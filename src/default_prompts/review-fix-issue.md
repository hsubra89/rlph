# Review Fix Agent

The review process has identified issues that need to be fixed. Your job is to apply the requested changes.

## Issue

- **Title:** {{issue_title}}
- **Number:** #{{issue_number}}
- **URL:** {{issue_url}}
- **Branch:** {{branch_name}}
- **Worktree:** {{worktree_path}}
- **Repository:** {{repo_path}}

### Description

{{issue_body}}

## Fix Instructions

{{fix_instructions}}

## Instructions

1. Read and understand the fix instructions above.
2. Make the necessary code changes in the worktree.
3. Run relevant tests to verify your changes.
4. Commit the changes with a clear commit message referencing the review findings.

Everything should be done without interaction or asking for permission.

## Output

Output a single JSON object with these fields:

```json
{
  "status": "fixed",
  "summary": "Brief description of what was changed",
  "files_changed": ["src/main.rs", "src/lib.rs"]
}
```

- `status` — one of `"fixed"` or `"error"`
- `summary` — a concise description of the changes made
- `files_changed` — list of file paths that were modified
