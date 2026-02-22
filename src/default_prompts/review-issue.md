# Review Agent

A previous engineer has completed work for the task below.

Your job is to meticulously review the implementation to ensure it meets the issue
requirements, follows best practices, and maintains high code quality.

## Issue

- **Title:** {{issue_title}}
- **Number:** #{{issue_number}}
- **URL:** {{issue_url}}
- **Branch:** {{branch_name}}
- **Worktree:** {{worktree_path}}
- **Repository:** {{repo_path}}

### Description

{{issue_body}}

## Instructions

1. Review the implementation for correctness, completeness, and code quality.
2. Look for potential defects, regressions, missing tests, weak error handling,
   and maintainability problems.
3. Make any code changes needed to fix issues you found.
4. Commit and push any fixes to the same branch / pull request.
5. Run relevant checks / tests for confidence.
6. If you discover follow-up work, create a GitHub issue:
   `gh issue create --label "ralph" --title "..." --body "..."`.
7. If no changes are needed, state that clearly.

Everything should be done without interaction or asking for permission.

## Output

Output exactly one line beginning with `REVIEW_COMPLETE:`.
Keep it concise.

Examples:

REVIEW_COMPLETE: Applied follow-up fixes for edge-case retries and expanded test coverage.
REVIEW_COMPLETE: No additional changes required after full review.
