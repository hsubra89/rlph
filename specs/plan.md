# Plan: Local Plan Source & Sync-First Build Loop

> Source PRD: specs/prd.md

## Architectural decisions

Durable decisions that apply across all phases:

- **Plan directory**: `<repo_root>/plans/<slug>/` — flat directory of files (markdown or any format). Slug is derived from issue title when available, falls back to issue number.
- **File naming (synced issues)**: `<issue_number>.md` (e.g., `42.md`, `45.md`). No metadata files or frontmatter.
- **Reference rewriting**: GitHub/Linear issue URLs rewritten to `[#N](./N.md)` when the referenced file exists locally. Unchanged otherwise.
- **Reference depth**: Fetch direct references + up to 4 levels deep. Deduplicate across levels.
- **Prompt file references**: `@plans/<folder>/file` syntax in both choose and implement prompts. Works across all runners (Claude, Codex, OpenCode).
- **Agent cwd**: Worktree root for both choose and implement phases.
- **Choose signal**: Agent outputs `NOTHING_LEFT` when no sub-tasks remain.
- **Draft PR flow**: Open draft immediately after plan commit. Mark ready after inner loop completes. Remote-sourced PRs include `Resolves #<issue_number>` in body.
- **TaskSource trait extension**: New `fetch_sub_issues(task_id) -> Vec<Task>` method for GitHub sub-issues and Linear child issues.

---

## Phase 1: Reference Rewriter

**User stories**: 4

### What to build

A pure-logic module that transforms issue tracker URLs into local markdown links. Given a markdown string and a set of locally-available issue IDs, rewrite matching URLs to relative markdown links.

GitHub URL patterns: `https://github.com/<owner>/<repo>/issues/<N>`, `https://github.com/<owner>/<repo>/pull/<N>`.
Linear URL patterns: `https://linear.app/<team>/issue/<ID>`.

For each URL where the referenced issue file exists locally: replace with `[#N](./N.md)`. Leave all other URLs unchanged.

This is a standalone pure function module with no I/O or dependencies on other new modules.

### Acceptance criteria

- [x] Rewrites `https://github.com/org/repo/issues/45` to `[#45](./45.md)` when `45.md` is in the local set
- [x] Rewrites `https://github.com/org/repo/pull/45` similarly
- [x] Rewrites Linear issue URLs to the equivalent local link
- [x] Leaves URLs unchanged when the referenced ID is not in the local set
- [x] Handles multiple references on the same line
- [x] Does not rewrite URLs already inside markdown link syntax that the user manually wrote (avoids double-wrapping)
- [x] Works with bare URLs and URLs inside markdown text

---

## Phase 2: Plan Sync Module

**User stories**: 2, 3, 14, 16, 17

### What to build

A new module that fetches a remote issue (plus sub-issues and referenced issues) and writes them as local markdown files into `plans/<slug>/`.

Add `fetch_sub_issues(task_id) -> Vec<Task>` to the `TaskSource` trait. Implement for `GitHubSource` using the GraphQL API / `gh` CLI to fetch sub-issues. Add the trait method to `LinearSource` (stub returning empty vec for now; trait method is required).

The sync flow:
1. Fetch the main issue via `get_task_details`.
2. Fetch sub-issues via `fetch_sub_issues`.
3. Parse all issue URL references from fetched issue bodies.
4. Recursively fetch referenced issues up to 4 levels deep, deduplicating by ID.
5. Write each issue as `<id>.md` into `plans/<slug>/`.
6. Run the reference rewriter (Phase 1) across all written files.
7. Return a `PlanDirectory` struct with the path and file list.

Slug generation reuses the existing `WorktreeManager::slugify` logic (or extracts it to a shared utility). Falls back to issue number prefix (e.g., `gh-42`) when title produces an empty slug.

### Acceptance criteria

- [x] Given a mock `TaskSource`, sync produces correct directory structure with one file per issue
- [x] Sub-issues are fetched and written as separate files
- [x] Referenced issues are fetched up to 4 levels deep
- [x] Circular references are deduplicated (no infinite loops)
- [x] Cross-references in written files are rewritten to local markdown links
- [x] Slug is derived from issue title, falling back to issue number
- [x] `fetch_sub_issues` added to `TaskSource` trait with GitHub implementation
- [x] `LinearSource` compiles with a stub `fetch_sub_issues`

---

## Phase 3: Local Plan Source

**User stories**: 1, 15, 18

### What to build

Enable `rlph build plans/my-feature` to work from a local plan directory without any remote issue tracker.

Add an optional positional argument `plan_path` to the `build` subcommand. When present, the system infers local mode — no `--source` flag required, no remote fetch or sync.

The orchestrator skips the "acquire + sync" step and uses the provided directory directly. Plan files are read from the directory and fed into the choose and implement prompts (prompt changes come in Phase 4, but the orchestrator plumbing to pass local plan paths through needs to exist here).

For local plans: the plan directory must already exist and contain at least one file. The "task" identity is derived from the directory name (used for branch naming, PR title placeholder, state tracking).

### Acceptance criteria

- [x] `rlph build plans/my-feature` parses successfully with `plan_path` set
- [x] `rlph build` without a path continues to work as before (remote source mode)
- [x] Local mode skips `TaskSource` fetch entirely
- [x] Error if the provided plan path doesn't exist or is empty
- [x] Task identity (for branch name, state) is derived from the directory name
- [x] The plan file list is correctly resolved and passed through the orchestrator

---

## Phase 4: Choose + Implement Prompt Changes

**User stories**: 7, 8, 11, 12, 19

### What to build

Rewrite the choose and implement prompt templates to work with local plan files instead of inline issue content.

**Choose prompt**: Instead of `{{issues_json}}`, receives a list of plan file paths. The agent studies the files and picks the next sub-task to implement. Must support outputting `NOTHING_LEFT` when all work is done. The output format for selecting a task should identify which file(s) / sub-task it chose.

**Implement prompt**: Instead of `{{issue_body}}` inlined, lists `@plans/<folder>/file1, @plans/<folder>/file2, ...` references. The agent reads the actual files. Remove `{{issue_title}}`, `{{issue_url}}` inline vars; replace with a reference to the plan directory and (when remote-sourced) the original issue URL as context.

Both prompts set the agent cwd to the worktree root so `@` references resolve correctly.

Template variable changes:
- New: `{{plan_files}}` (formatted `@` file reference list), `{{plan_dir}}` (relative path to plan directory)
- Retained: `{{worktree_path}}`, `{{branch_name}}`, `{{base_branch}}`, `{{repo_path}}`
- Removed from implement: inline `{{issue_body}}`, `{{issue_title}}` (content is now in the files)

### Acceptance criteria

- [x] Choose prompt template renders with plan file list instead of JSON
- [x] Choose prompt includes `NOTHING_LEFT` signal instruction
- [x] Implement prompt template renders with `@` file references
- [x] Agent cwd is worktree root for both phases
- [x] Template renders correctly with varying numbers of plan files (1 file, many files)
- [x] Prompt engine tests pass with new template variables

---

## Phase 5: Inner Loop + Draft PR Lifecycle

**User stories**: 5, 6, 9, 10, 13, 20

### What to build

Restructure the orchestrator to use a two-level loop with draft PR lifecycle.

**Plan commit**: After sync (Phase 2) or local plan resolution (Phase 3), commit the plan files to the worktree branch as the first commit. For local plans, copy files into the worktree's `plans/` directory first.

**Draft PR**: Immediately after the plan commit, push the branch and open a draft PR. For remote-sourced tasks, the PR body includes `Resolves #<issue_number>`. For local plans, the PR body references the plan folder.

**Inner loop**: Repeat { choose → implement → commit } until the choose agent outputs `NOTHING_LEFT`. Each cycle:
1. Run choose phase — agent picks next sub-task from plan files.
2. Parse choose output. If `NOTHING_LEFT`, exit loop.
3. Run implement phase — agent implements the chosen sub-task.
4. Commit changes with a message based on the work done.
5. Push branch.
6. Loop back to step 1.

**Finalize**: After the inner loop exits, mark the PR as ready for review (`gh pr ready` or equivalent). Then run the review pipeline as today.

The `SubmissionBackend` trait needs a way to submit draft PRs and mark them ready. Either add `submit_draft` + `mark_ready` methods, or add a `draft` parameter to `submit`.

### Acceptance criteria

- [x] Plan files are committed as the first commit on the worktree branch
- [x] Draft PR is opened immediately after the plan commit
- [ ] Remote-sourced draft PRs include `Resolves #<issue_number>` in body
- [x] Local-plan draft PRs reference the plan folder in body
- [x] Inner loop runs choose → implement → commit cycles
- [x] Inner loop exits when choose agent outputs `NOTHING_LEFT`
- [ ] Each cycle produces a separate commit
- [x] PR is marked ready after inner loop completes
- [x] Review pipeline runs after PR is marked ready
- [ ] Plan directory persists after completion (not cleaned up)
- [x] `SubmissionBackend` supports draft PR creation and ready marking
