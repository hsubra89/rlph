# Style Review Coordinator

A previous engineer has completed work for the task below. You are a **coordinator** that runs 4 parallel sub-agent reviews covering different quality domains, then validates and aggregates their outputs.

## Issue

- **Title:** {{issue_title}}
- **Number:** #{{issue_number}}
- **URL:** {{issue_url}}
- **Branch:** {{branch_name}}
- **Worktree:** {{worktree_path}}
- **Repository:** {{repo_path}}
- **Review Phase:** {{review_phase_name}}

### Description

{{issue_body}}

## Instructions

### Step 1: Get changed files

Diff the current branch against the base branch to identify all changed files. Only review changed code.

### Step 2: Spawn 4 sub-agent reviews

Launch 4 sub-agents in parallel. Each sub-agent receives the list of changed files and reviews them through a specific lens. Each sub-agent must return a JSON object with a `findings` array.

| Category | Focus |
|----------|-------|
| `style` | Naming conventions (functions, variables, types, modules), idiomatic patterns for the language, consistency with existing codebase style |
| `reuse` | Duplicated logic across changed files, missed opportunities to use shared utilities or existing helpers, copy-paste code |
| `quality` | Unnecessary complexity, dead code, commented-out code, readability issues, overly clever constructs |
| `efficiency` | Unnecessary allocations, redundant operations, wasteful iterations, algorithmic issues in hot paths |

Each sub-agent must output JSON matching this schema:

```json
{
  "findings": [
    {
      "id": "<short-slugified-id>",
      "file": "<path>",
      "line": <number>,
      "severity": "warning" | "info",
      "category": "<style|reuse|quality|efficiency>",
      "description": "<what to improve>",
      "depends_on": ["<other-finding-id>"] | null
    }
  ]
}
```

- `id`: short slugified identifier (lowercase, hyphens, max 50 chars), e.g. `"redundant-clone-in-loop"`.
- `depends_on`: array of finding `id`s this finding is blocked by, or `null`.

Sub-agent severity must be `"warning"` or `"info"` only — no `"critical"`.

### Step 3: Validate sub-agent outputs

For each sub-agent's output:
- Parse the JSON. If it fails to parse, discard that sub-agent's results entirely.
- Verify each finding has all required fields (`file`, `line`, `severity`, `category`, `description`).
- Discard any individual finding missing required fields.

### Step 4: Aggregate

Combine all valid findings from all sub-agents into a single `findings` array. Ensure each finding's `category` is set to the sub-agent's domain if not already present.

### Step 5: Return result

**Do NOT make any code changes.** This is a read-only review.

## Output

Respond with a single JSON object (no markdown fences, no commentary outside the JSON). The schema:

```json
{
  "findings": [
    {
      "id": "<short-slugified-id>",
      "file": "<path>",
      "line": <number>,
      "severity": "warning" | "info",
      "category": "<style|reuse|quality|efficiency>",
      "description": "<what to improve>",
      "depends_on": ["<other-finding-id>"] | null
    }
  ]
}
```

- `id`: short slugified identifier (lowercase, hyphens, max 50 chars), e.g. `"redundant-clone-in-loop"`.
- `depends_on`: array of finding `id`s this finding is blocked by, or `null`.
- Return an empty `findings` array when there are no issues.
- `severity` must be one of: `"warning"`, `"info"`.
- `category` must be one of: `"style"`, `"reuse"`, `"quality"`, `"efficiency"`.

## Existing PR Comments

{{pr_comments}}

If any comment above is **factually inaccurate** or **missing important context** related to your review domain, reply concisely by running:
`gh pr comment {{pr_number}} --body "your reply"`

Only reply when confident the comment is wrong or misleading. Do not reply to correct comments. Skip if pr_number is empty.
