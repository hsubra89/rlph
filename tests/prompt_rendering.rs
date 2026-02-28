use std::collections::HashMap;

use rlph::prompts::PromptEngine;

/// Variables shared by all review phases (the "issue" block).
fn base_vars() -> HashMap<String, String> {
    HashMap::from([
        (
            "issue_title".into(),
            "Add category to ReviewFinding, rewrite style review as sub-agent coordinator".into(),
        ),
        ("issue_number".into(), "94".into()),
        (
            "issue_url".into(),
            "https://github.com/hsubra89/rlph/pull/94".into(),
        ),
        (
            "branch_name".into(),
            "style-review-subagents-and-category".into(),
        ),
        ("worktree_path".into(), "/tmp/wt-94".into()),
        ("repo_path".into(), "/home/user/rlph".into()),
        (
            "issue_body".into(),
            "Rewrite style review as sub-agent coordinator".into(),
        ),
    ])
}

/// Extends `base_vars` with fields common to correctness/security/style reviews.
fn review_phase_vars() -> HashMap<String, String> {
    let mut vars = base_vars();
    vars.insert("base_branch".into(), "main".into());
    vars.insert("pr_comments".into(), "No comments yet.".into());
    vars.insert("pr_number".into(), "94".into());
    vars.insert("has_pr_comments".into(), "true".into());
    vars
}

#[test]
fn test_render_correctness_review() {
    let engine = PromptEngine::new(None);
    let mut vars = review_phase_vars();
    vars.insert("review_phase_name".into(), "correctness".into());

    let result = engine.render_phase("correctness-review", &vars).unwrap();

    let expected = "\
# Correctness Review Agent

A previous engineer has completed work for the task below. Your job is to review the implementation for **logical correctness** only.

## Issue

- **Title:** Add category to ReviewFinding, rewrite style review as sub-agent coordinator
- **Number:** #94
- **URL:** https://github.com/hsubra89/rlph/pull/94
- **Branch:** style-review-subagents-and-category
- **Base Branch:** main
- **Worktree:** /tmp/wt-94
- **Repository:** /home/user/rlph
- **Review Phase:** correctness

### Description

Rewrite style review as sub-agent coordinator

## Instructions

1. Run `git diff main...HEAD` to identify changed files. Only review changed code.
2. Check for logical bugs, off-by-one errors, incorrect conditions, and missing edge cases.
3. Verify that error handling covers failure paths and does not silently swallow errors.
4. Check that tests exist for the changed code and cover important branches.
5. Verify the implementation actually satisfies the issue requirements.

**Do NOT make any code changes.** This is a read-only review.

## Output

Respond with a single JSON object (no markdown fences, no commentary outside the JSON). The schema:

```json
{
  \"findings\": [
    {
      \"id\": \"<short-slugified-id>\",
      \"file\": \"<path>\",
      \"line\": <number>,
      \"severity\": \"critical\" | \"warning\" | \"info\",
      \"description\": \"<description>\",
      \"category\": \"<category>\",
      \"depends_on\": [\"<other-finding-id>\"] | null
    }
  ]
}
```

- `id`: short slugified identifier (lowercase, hyphens, max 50 chars).
- `depends_on`: array of finding `id`s this finding is blocked by, or `null`.
- Return an empty `findings` array when there are no issues.

- `severity` must be one of: `\"critical\"`, `\"warning\"`, `\"info\"`.

## Existing PR Comments

No comments yet.

If any comment above is **factually inaccurate** or **missing important context** related to your review domain, reply concisely by running:
`gh pr comment 94 --body \"your reply\"`

Only reply when confident the comment is wrong or misleading. Do not reply to correct comments. Skip if pr_number is empty.

";

    assert_eq!(result, expected);
}

#[test]
fn test_render_security_review() {
    let engine = PromptEngine::new(None);
    let mut vars = review_phase_vars();
    vars.insert("review_phase_name".into(), "security".into());

    let result = engine.render_phase("security-review", &vars).unwrap();

    let expected = "\
# Security Review Agent

A previous engineer has completed work for the task below. Your job is to review the implementation for **security vulnerabilities** only.

## Issue

- **Title:** Add category to ReviewFinding, rewrite style review as sub-agent coordinator
- **Number:** #94
- **URL:** https://github.com/hsubra89/rlph/pull/94
- **Branch:** style-review-subagents-and-category
- **Base Branch:** main
- **Worktree:** /tmp/wt-94
- **Repository:** /home/user/rlph
- **Review Phase:** security

### Description

Rewrite style review as sub-agent coordinator

## Instructions

1. Run `git diff main...HEAD` to identify changed files. Only review changed code.
2. Check for injection vulnerabilities (command injection, SQL injection, XSS, etc.).
3. Verify authentication and authorization are correctly enforced.
4. Check for hardcoded secrets, credentials, or API keys.
5. Verify input validation and sanitization at trust boundaries.
6. Check for path traversal, SSRF, and insecure deserialization.
7. Verify that sensitive data is not logged or exposed in error messages.

**Do NOT make any code changes.** This is a read-only review.

## Output

Respond with a single JSON object (no markdown fences, no commentary outside the JSON). The schema:

```json
{
  \"findings\": [
    {
      \"id\": \"<short-slugified-id>\",
      \"file\": \"<path>\",
      \"line\": <number>,
      \"severity\": \"critical\" | \"warning\" | \"info\",
      \"description\": \"<description>\",
      \"category\": \"<category>\",
      \"depends_on\": [\"<other-finding-id>\"] | null
    }
  ]
}
```

- `id`: short slugified identifier (lowercase, hyphens, max 50 chars).
- `depends_on`: array of finding `id`s this finding is blocked by, or `null`.
- Return an empty `findings` array when there are no issues.

- `severity` must be one of: `\"critical\"`, `\"warning\"`, `\"info\"`.

## Existing PR Comments

No comments yet.

If any comment above is **factually inaccurate** or **missing important context** related to your review domain, reply concisely by running:
`gh pr comment 94 --body \"your reply\"`

Only reply when confident the comment is wrong or misleading. Do not reply to correct comments. Skip if pr_number is empty.

";

    assert_eq!(result, expected);
}

#[test]
fn test_render_style_review() {
    let engine = PromptEngine::new(None);
    let mut vars = review_phase_vars();
    vars.insert("review_phase_name".into(), "style".into());

    let result = engine.render_phase("style-review", &vars).unwrap();

    let expected = "\
# Style Review Coordinator

A previous engineer has completed work for the task below. You are a **coordinator** that runs 4 parallel sub-agent reviews covering different quality domains, then validates and aggregates their outputs.

## Issue

- **Title:** Add category to ReviewFinding, rewrite style review as sub-agent coordinator
- **Number:** #94
- **URL:** https://github.com/hsubra89/rlph/pull/94
- **Branch:** style-review-subagents-and-category
- **Base Branch:** main
- **Worktree:** /tmp/wt-94
- **Repository:** /home/user/rlph
- **Review Phase:** style

### Description

Rewrite style review as sub-agent coordinator

## Instructions

### Step 1: Get changed files

Run `git diff main...HEAD` to identify all changed files. Only review changed code — do not flag pre-existing issues in unchanged lines.

### Step 2: Spawn 4 sub-agent reviews

Launch 4 sub-agents in parallel. Each sub-agent receives the list of changed files and reviews them through a specific lens. Each sub-agent must return a JSON object with a `findings` array.

| Category | Focus |
|----------|-------|
| `style` | Naming conventions (functions, variables, types, modules), idiomatic patterns for the language, consistency with existing codebase style |
| `reuse` | Duplicated logic across changed files, missed opportunities to use shared utilities or existing helpers, copy-paste code |
| `quality` | Unnecessary complexity, dead code, commented-out code, readability issues, overly clever constructs |
| `efficiency` | Unnecessary allocations, redundant operations, wasteful iterations, algorithmic issues in hot paths |

Each sub-agent must output findings JSON (same schema as coordinator output):

Respond with a single JSON object (no markdown fences, no commentary outside the JSON). The schema:

```json
{
  \"findings\": [
    {
      \"id\": \"<short-slugified-id>\",
      \"file\": \"<path>\",
      \"line\": <number>,
      \"severity\": \"critical\" | \"warning\" | \"info\",
      \"description\": \"<description>\",
      \"category\": \"<category>\",
      \"depends_on\": [\"<other-finding-id>\"] | null
    }
  ]
}
```

- `id`: short slugified identifier (lowercase, hyphens, max 50 chars).
- `depends_on`: array of finding `id`s this finding is blocked by, or `null`.
- Return an empty `findings` array when there are no issues.

- `severity` must be `\"warning\"` or `\"info\"` only — no `\"critical\"`.
- `category` must be one of: `\"style\"`, `\"reuse\"`, `\"quality\"`, `\"efficiency\"`.

### Step 3: Validate sub-agent outputs

For each sub-agent's output:
- Parse the JSON. If it fails to parse, discard that sub-agent's results entirely.
- Verify each finding has all required fields (`id`, `file`, `line`, `severity`, `category`, `description`).
- Discard any individual finding missing required fields.

### Step 4: Aggregate

Combine all valid findings from all sub-agents into a single `findings` array. Ensure each finding's `category` is set to the sub-agent's domain if not already present.

### Step 5: Return result

Emit a single JSON object containing the aggregated `findings` array (see Output schema below). **Do NOT make any code changes.** This is a read-only review.

## Output

Respond with a single JSON object (no markdown fences, no commentary outside the JSON). The schema:

```json
{
  \"findings\": [
    {
      \"id\": \"<short-slugified-id>\",
      \"file\": \"<path>\",
      \"line\": <number>,
      \"severity\": \"critical\" | \"warning\" | \"info\",
      \"description\": \"<description>\",
      \"category\": \"<category>\",
      \"depends_on\": [\"<other-finding-id>\"] | null
    }
  ]
}
```

- `id`: short slugified identifier (lowercase, hyphens, max 50 chars).
- `depends_on`: array of finding `id`s this finding is blocked by, or `null`.
- Return an empty `findings` array when there are no issues.

- `severity` must be one of: `\"warning\"`, `\"info\"`.
- `category` must be one of: `\"style\"`, `\"reuse\"`, `\"quality\"`, `\"efficiency\"`.

## Existing PR Comments

No comments yet.

If any comment above is **factually inaccurate** or **missing important context** related to your review domain, reply concisely by running:
`gh pr comment 94 --body \"your reply\"`

Only reply when confident the comment is wrong or misleading. Do not reply to correct comments. Skip if pr_number is empty.

";

    assert_eq!(result, expected);
}

#[test]
fn test_render_review_aggregate() {
    let engine = PromptEngine::new(None);
    let mut vars = base_vars();
    vars.insert(
        "review_outputs".into(),
        "## Correctness\nNo issues found.\n\n## Security\nNo issues found.".into(),
    );

    let result = engine.render_phase("review-aggregate", &vars).unwrap();

    let expected = "\
# Review Aggregation Agent

Multiple review agents have independently analyzed an implementation. Your job is to aggregate their findings into a single coherent PR comment and decide whether the code is ready to merge.

## Issue

- **Title:** Add category to ReviewFinding, rewrite style review as sub-agent coordinator
- **Number:** #94
- **URL:** https://github.com/hsubra89/rlph/pull/94
- **Branch:** style-review-subagents-and-category
- **Worktree:** /tmp/wt-94
- **Repository:** /home/user/rlph

### Description

Rewrite style review as sub-agent coordinator

## Review Outputs

## Correctness
No issues found.

## Security
No issues found.

## Instructions

1. Read all review outputs above carefully.
2. De-duplicate findings that appear in multiple reviews.
3. Prioritize by severity: critical > warning > info.
4. Compose a clear, actionable PR comment summarizing all findings.
5. Decide: are there any critical or warning findings that require code changes?

## Output

The output extends the standard findings schema with aggregator-specific fields.

Respond with a single JSON object (no markdown fences, no commentary outside the JSON). The schema:

```json
{
  \"findings\": [
    {
      \"id\": \"<short-slugified-id>\",
      \"file\": \"<path>\",
      \"line\": <number>,
      \"severity\": \"critical\" | \"warning\" | \"info\",
      \"description\": \"<description>\",
      \"category\": \"<category>\",
      \"depends_on\": [\"<other-finding-id>\"] | null
    }
  ]
}
```

- `id`: short slugified identifier (lowercase, hyphens, max 50 chars).
- `depends_on`: array of finding `id`s this finding is blocked by, or `null`.
- Return an empty `findings` array when there are no issues.


Additionally, the top-level object must include these fields:

```json
{
  \"verdict\": \"approved\" | \"needs_fix\",
  \"comment\": \"<markdown PR comment body — list issues as a task list (`- [ ] ...`)>\",
  \"fix_instructions\": \"<concise instructions for the fix agent, or null if approved>\"
}
```

- Set `verdict` to `\"approved\"` if there are no actionable findings requiring code changes.
- Set `verdict` to `\"needs_fix\"` if code changes are needed, and populate `fix_instructions`.
- `findings` may be empty when the code is clean.
- `fix_instructions` must be `null` when `verdict` is `\"approved\"`.
";

    assert_eq!(result, expected);
}

#[test]
fn test_render_review_fix() {
    let engine = PromptEngine::new(None);
    let mut vars = base_vars();
    vars.insert(
        "fix_instructions".into(),
        "Fix the off-by-one error in src/orchestrator.rs line 42.".into(),
    );

    let result = engine.render_phase("review-fix", &vars).unwrap();

    let expected = "\
# Review Fix Agent

The review process has identified issues that need to be fixed. Your job is to apply the requested changes.

## Issue

- **Title:** Add category to ReviewFinding, rewrite style review as sub-agent coordinator
- **Number:** #94
- **URL:** https://github.com/hsubra89/rlph/pull/94
- **Branch:** style-review-subagents-and-category
- **Worktree:** /tmp/wt-94
- **Repository:** /home/user/rlph

### Description

Rewrite style review as sub-agent coordinator

## Fix Instructions

Fix the off-by-one error in src/orchestrator.rs line 42.

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
  \"status\": \"fixed\",
  \"summary\": \"Brief description of what was changed\",
  \"files_changed\": [\"src/main.rs\", \"src/lib.rs\"]
}
```

- `status` — one of `\"fixed\"` or `\"error\"`
- `summary` — a concise description of the changes made
- `files_changed` — list of file paths that were modified
";

    assert_eq!(result, expected);
}
