# PRD: Local Plan Source & Sync-First Build Loop

## Problem Statement

The build workflow is tightly coupled to remote issue trackers (GitHub Issues, Linear). The orchestrator fetches task details at runtime, embeds them inline in prompts, and the agent never sees the original source material as files. This creates several problems:

- There is no way to work from a local plan or spec without an issue tracker.
- The agent cannot reference task files on disk, forcing large inline prompt payloads.
- Sub-issues and referenced issues are not fetched or surfaced to the agent.
- The system cannot easily support new task sources without modifying the core loop.

Users want to write a plan locally (or sync one from a remote source) and have the agent work through it item by item, with all context available as files on disk.

## Solution

Introduce a **sync-first architecture** where all task sources (GitHub, Linear, local filesystem) are normalized into a local `plans/<folder>/` directory before any agent work begins. The orchestrator always works from local plan files, never from inline issue content.

The build loop changes from "fetch → choose → implement → PR → review" to:

1. **Acquire task** — pick the first eligible issue from the remote source, OR accept a local plan path from the CLI.
2. **Sync to local** — fetch the issue, its sub-issues, and referenced issues (up to 4 levels deep) into `plans/<slug>/` as individual markdown files. Rewrite cross-references from URLs to local markdown links.
3. **Create branch & commit** — create a slugified worktree branch, commit the plan files as the first commit.
4. **Open draft PR** — submit a draft PR referencing the plan folder/issue.
5. **Inner loop** — repeat { choose (pick next sub-task from plan files) → implement → commit } until the choose agent reports "nothing left to do."
6. **Finalize** — mark the PR as ready for review, run the review pipeline as today.

For local-only plans (`rlph build plans/my-feature`), steps 1-2 are skipped — the user's files are used directly.

## User Stories

1. As a developer, I want to run `rlph build plans/my-feature` to have the agent implement a plan I wrote locally, so that I don't need an issue tracker to use rlph.
2. As a developer, I want rlph to sync a GitHub issue and all its sub-issues into local markdown files before implementation, so that the agent has full context as files on disk.
3. As a developer, I want referenced issues (up to 4 levels deep) to be fetched and stored alongside the main task, so that the agent understands the full dependency context.
4. As a developer, I want cross-reference URLs in synced issue files to be rewritten to local markdown links (e.g., `See [#45](./45.md)`), so that references are navigable locally.
5. As a developer, I want the plan files committed as the first commit on the worktree branch, so that the plan is version-controlled alongside the implementation.
6. As a developer, I want a draft PR opened immediately after the plan commit, so that I can track progress before implementation is complete.
7. As a developer, I want the choose agent to pick the next sub-task from the plan files each iteration, so that multi-item plans are worked through systematically.
8. As a developer, I want the inner loop to exit when the choose agent reports "nothing left to do," so that the system knows when the plan is fully implemented.
9. As a developer, I want each choose-implement cycle to produce a separate commit, so that sub-task work is individually reviewable.
10. As a developer, I want the PR automatically marked as ready for review when all sub-tasks are done, so that the review pipeline kicks in without manual intervention.
11. As a developer, I want the implement prompt to reference plan files using `@plans/<folder>/file1` syntax, so that the agent reads the actual files rather than receiving inlined content.
12. As a developer, I want the choose prompt to receive a list of plan files (not JSON issue data), so that it selects work based on the local plan structure.
13. As a developer, I want plan directories to persist after completion as a record, so that I can review what was planned vs. implemented.
14. As a developer, I want `plans/<slug>/` directories named with a slugified title when possible, falling back to the issue number, so that plan folders are human-readable.
15. As a developer, I want to use `rlph build plans/my-feature` without specifying `--source local` explicitly, so that the CLI infers local mode from the path argument.
16. As a developer, I want GitHub sub-issues (via GraphQL API / `gh` CLI) to be synced as individual files in the plan directory, so that the agent sees the full task breakdown.
17. As a developer, I want Linear child issues to be synced the same way as GitHub sub-issues, so that the plan sync is source-agnostic.
18. As a developer, I want files inside my local plan directory to be arbitrary (markdown, text, any format), so that I'm not constrained to a specific structure.
19. As a developer, I want the agent's cwd during choose and implement phases to be the worktree root, so that `@plans/<folder>/file` references resolve correctly.
20. As a developer, I want the draft PR body to include a `Resolves #<issue_number>` reference when the task originated from a remote source (GitHub/Linear), so that the PR is linked to the original issue and auto-closes it on merge.

## Implementation Decisions

### New module: `plan_sync`

A new module responsible for syncing remote issues into local plan directories. Interface:

- `sync_to_local(source, issue_id, plans_dir) -> PlanDirectory` — fetches the issue, sub-issues, and references (4 levels deep), writes them as markdown files into `plans/<slug>/`, rewrites cross-references, returns the path and file manifest.
- `list_plan_files(plan_dir) -> Vec<PathBuf>` — lists all files in a plan directory for prompt generation.

This module calls into `TaskSource` for fetching but owns the file I/O, reference crawling, and URL rewriting.

### New module: `reference_rewriter`

Handles rewriting URLs in synced markdown files:

- Detects GitHub issue/PR URLs (e.g., `https://github.com/org/repo/issues/45`) and Linear issue URLs.
- Rewrites them to local markdown links (e.g., `[#45](./45.md)`) when the referenced file exists locally.
- Leaves URLs unchanged when the referenced issue was not synced.

### TaskSource trait changes

Add a method for fetching sub-issues:

- `fetch_sub_issues(task_id) -> Vec<Task>` — returns child/sub-issues for a given parent task.

The existing `get_task_details` is used for fetching referenced issues by ID.

### Plan directory structure

All files are stored flat in `plans/<slug>/`:

```
plans/gh-42-fix-auth-bug/
  42.md        # main issue
  45.md        # sub-issue
  50.md        # referenced issue (from #42 or #45, up to 4 levels)
  63.md        # another reference
```

For local-only plans, the user creates the directory and files themselves. No required structure — all files in the folder are fed to the agent.

### CLI changes

Add an optional positional argument to `build`:

- `rlph build [plan_path]` — when `plan_path` is provided, use local plan mode. No remote source needed.
- When `plan_path` is absent, behavior is as today (use `--source` flag or config) but with the new sync-first flow.

### Orchestrator loop restructuring

The current single-pass loop becomes a two-level loop:

**Outer:** Acquire task → sync → branch → commit plan → open draft PR
**Inner:** Loop { choose next sub-task from plan files → implement → commit } until "nothing left to do"
**Finalize:** Mark PR ready → run review pipeline

The choose prompt changes from receiving a JSON array of issues to receiving a list of plan file paths. The implement prompt changes from inlining `{{issue_body}}` to listing `@plans/<folder>/file` references.

### Prompt template changes

**Choose prompt:** Receives file list instead of `{{issues_json}}`. The agent is asked to study the plan files and pick the next best sub-task to implement. Must support a "NOTHING_LEFT" signal.

**Implement prompt:** Instead of `{{issue_body}}`, includes `@` file references: `Study @plans/<folder>/42.md, @plans/<folder>/45.md, ...`. The `{{issue_title}}` and `{{issue_url}}` vars are replaced with a reference to the plan directory.

### Draft PR flow

After the plan commit, open a draft PR immediately via `gh pr create --draft`. After the inner loop completes, mark ready via `gh pr ready`. The `SubmissionBackend` trait may need a `submit_draft` and `mark_ready` method, or the existing `submit` method gains a `draft` flag.

When the task originated from a remote source (GitHub/Linear), the draft PR body must include a `Resolves #<issue_number>` reference to the main issue, so the PR is linked and auto-closes the issue on merge. For local-only plans, the PR body references the plan folder path instead.

### Reference crawling strategy

Starting from the main issue:
1. Fetch the issue body. Parse all issue/PR URL references.
2. For each reference, fetch the issue and add to the plan.
3. Repeat up to 4 levels deep.
4. Deduplicate — don't re-fetch issues already in the plan.
5. After all files are written, run the reference rewriter across all files.

### First eligible selection

When using a remote source without a path argument, the system picks the first eligible issue from the fetched list (no choose phase for top-level selection). The choose phase is reserved for picking sub-tasks within the plan.

## Testing Decisions

Good tests verify external behavior through the module's public interface, not implementation details. Tests should be deterministic and not depend on network access or filesystem ordering.

### Modules to test

**`plan_sync`** — Test that given a mock `TaskSource` returning known issues with references, the sync produces the correct directory structure and file contents. Test reference depth limiting (stops at 4 levels). Test deduplication of circular references. Test slugification with fallback to issue number. Prior art: `sources/github.rs` tests with `MockGhClient`.

**`reference_rewriter`** — Test URL-to-local-link rewriting for GitHub and Linear URL formats. Test that URLs for issues not in the local plan are left unchanged. Test edge cases: multiple references on one line, references in code blocks, references in markdown links already. This is a pure function module — straightforward unit tests.

**Choose prompt generation** — Test that the new choose prompt correctly lists plan files and includes the "NOTHING_LEFT" signal instruction. Test with varying numbers of files. Prior art: `prompts.rs` template tests.

**Orchestrator inner loop** — Test the choose→implement→commit loop exits when the choose agent reports "nothing left to do." Test that each cycle produces a commit. Test the draft→ready PR transition. Prior art: existing orchestrator integration tests using `CallbackRunner`.

## Out of Scope

- Changes to `mark_in_progress` / `mark_in_review` behavior (to be decided later).
- Linear source implementation of `fetch_sub_issues` (just the trait method for now; GitHub first).
- Continuous mode changes (the new loop structure applies to single-iteration mode; continuous mode wrapping is unchanged).
- Review pipeline changes (still runs as-is after the PR is marked ready).
- Plan file cleanup/deletion after completion (plans persist as records).
- Any UI or dashboard for viewing plan status.

## Further Notes

- The `@` file reference syntax (`@plans/folder/file`) works across Claude, Codex, and OpenCode runners. No runner-specific handling needed.
- The plan directory lives at `<worktree_root>/plans/<folder>/` and is committed to the branch. It will appear in the PR diff, which is intentional — reviewers can see the plan alongside the implementation.
- The inner choose→implement loop replaces the current single-pass implement phase. The review pipeline only runs once, after all sub-tasks are done.
- Sub-issue detection for GitHub uses the GraphQL API via `gh` CLI. The exact query will need to be determined during implementation, as GitHub's sub-issues feature is relatively new.
