#!/usr/bin/env bash

set -euo pipefail

LABEL_FILTER="${LABEL_FILTER:-ralph}"
POLL_SECONDS="${POLL_SECONDS:-30}"
ISSUE_LIMIT="${ISSUE_LIMIT:-100}"
MAX_REVIEW_LOOPS="${MAX_REVIEW_LOOPS:-2}"
BASE_BRANCH="${BASE_BRANCH:-}"
AGENT_BIN="${AGENT_BIN:-claude}"
AGENT_ARGS="${AGENT_ARGS:---dangerously-skip-permissions --verbose --model opus}"
AGENT_CMD_PREFIX="${AGENT_CMD_PREFIX:-}"
ONCE_MODE=false
REVIEW_PR_NUMBER=""

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROMPTS_DIR="$SCRIPT_DIR/ralph-prompts"
CHOOSE_PROMPT_TEMPLATE="$PROMPTS_DIR/choose-issue.md"
IMPLEMENT_PROMPT_TEMPLATE="$PROMPTS_DIR/implement-issue.md"
REVIEW_PROMPT_TEMPLATE="$PROMPTS_DIR/review-issue.md"

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
STATE_DIR="$REPO_ROOT/.ralph-gh"
RUN_DIR="$STATE_DIR/run"
ISSUES_FILE="$RUN_DIR/issues.json"
PR_CONTEXT_FILE="$RUN_DIR/pr-context.json"
RALPH_DIR="$REPO_ROOT/.ralph"
TASK_FILE="$RALPH_DIR/task.json"
WORKTREES_DIR="$(cd "$REPO_ROOT/.." && pwd)/rlph-worktrees"

if [[ -z "${GITHUB_TOKEN:-}" ]]; then
  GITHUB_TOKEN="$(gh auth token 2>/dev/null || true)"
fi

read -r -a AGENT_PREFIX_ARR <<< "$AGENT_CMD_PREFIX"
read -r -a AGENT_ARGS_ARR <<< "$AGENT_ARGS"

PICKED_TASK_ID=""
PICKED_ISSUE_NUMBER=""
PICKED_PR_NUMBER=""
NO_PR_SENTINEL="NO_PR"
NO_PR_REASON_NO_COMMITS="NO_COMMITS"

log() {
  local level="$1"
  shift
  local ts
  ts="$(date '+%Y-%m-%d %H:%M:%S')"
  printf '[%s] [%s] %s\n' "$ts" "$level" "$*"
}

die() {
  log "ERROR" "$*"
  exit 1
}

cleanup() {
  log "INFO" "Stopping ralph GitHub loop."
}

trap cleanup EXIT
trap 'trap - INT TERM; cleanup; kill -- -$$ 2>/dev/null || true; exit 130' INT TERM

require_cmd() {
  local cmd="$1"
  command -v "$cmd" >/dev/null 2>&1 || die "Missing required command: $cmd"
}

gh_retry() {
  local attempts=3
  local delay=5
  local i
  for ((i=1; i<=attempts; i++)); do
    if "$@"; then
      return 0
    fi
    if [[ $i -lt $attempts ]]; then
      log "WARN" "Command failed (attempt $i/$attempts), retrying in ${delay}s: $*"
      sleep "$delay"
      delay=$((delay * 2))
    fi
  done
  log "ERROR" "Command failed after $attempts attempts: $*"
  return 1
}

usage() {
  cat <<EOF
Usage: scripts/ralph-github-loop.sh [--once] [--review-pr <N>] [--help]

Options:
  --once           Process at most one selected issue, then exit.
  --review-pr <N>  Skip issue selection; run review pass directly on PR #N, then exit.
  --help           Show this help text.
EOF
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --once)
        ONCE_MODE=true
        shift
        ;;
      --review-pr)
        REVIEW_PR_NUMBER="${2:-}"
        [[ -n "$REVIEW_PR_NUMBER" ]] || die "--review-pr requires a PR number"
        ONCE_MODE=true
        shift 2
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        die "Unknown argument: $1 (use --help)"
        ;;
    esac
  done
}

issue_number_from_task_id() {
  local task_id="$1"
  if [[ "$task_id" =~ ^gh-([0-9]+)$ ]]; then
    printf '%s' "${BASH_REMATCH[1]}"
    return 0
  fi
  return 1
}

# ── GitHub-direct issue selection ──────────────────────────────

# Shared jq definitions for issue filtering & selection.
# Operates directly on the fetched issues.json array.
_issues_jq_preamble='
  def issue_state:
    ([.labels[]?.name | ascii_downcase] // []) as $l
    | if ($l | index("in-progress")) then "in-progress"
      elif ($l | index("in-review")) then "in-review"
      elif ($l | index("done")) then "done"
      else "todo"
      end;

  def issue_priority:
    (
      [
        (.labels[]?.name | ascii_downcase
          | if test("^p[0-9]+$") then (sub("^p"; "") | tonumber)
            elif test("^priority[: _-]*[0-9]+$") then (capture("(?<n>[0-9]+)$").n | tonumber)
            elif . == "priority-high" or . == "high-priority" then 1
            elif . == "priority-medium" or . == "medium-priority" then 2
            elif . == "priority-low" or . == "low-priority" then 3
            else empty end
        ),
        (
          (try ((.body // "") | capture("(?im)^priority:[[:space:]]*(?<n>[0-9]+)").n | tonumber) catch null)
        )
      ] | map(select(. != null)) | min
    ) // 999;

  def blocked_by_numbers:
    (.body // "") as $body
    | (
        [($body | scan("(?i)blocked\\s+by\\s+#([0-9]+)") | .[0] | tonumber)] +
        [($body | scan("(?i)depends\\s+on\\s+#([0-9]+)") | .[0] | tonumber)] +
        [($body | scan("(?i)blockedBy:\\s*\\[([0-9, ]+)\\]") | .[0] | split(",")[] | gsub("\\s"; "") | select(length > 0) | tonumber)]
      ) | unique;

  def actionable_issues:
    [.[].number] as $open_numbers
    | [
        .[]
        | select(issue_state == "todo")
        | (blocked_by_numbers) as $deps
        | select([$deps[] | select(. as $n | $open_numbers | index($n))] | length == 0)
        | . + { _priority: issue_priority }
      ]
    | sort_by(._priority, .number);
'

select_next_issue() {
  if [[ ! -f "$ISSUES_FILE" ]]; then
    return 1
  fi
  jq -r "${_issues_jq_preamble}"'
    actionable_issues
    | .[0]
    | if . then {number, title, url, body, _priority} else empty end
  ' "$ISSUES_FILE"
}

count_actionable_issues() {
  if [[ ! -f "$ISSUES_FILE" ]]; then
    echo "0"
    return
  fi
  jq -r "${_issues_jq_preamble}"'
    actionable_issues | length
  ' "$ISSUES_FILE"
}

find_open_pr_for_issue() {
  local issue_number="$1"
  gh pr list --state open --search "#${issue_number} in:title" \
    --json number --jq '.[0].number // empty' 2>/dev/null || true
}

# ── GitHub state management ────────────────────────────────────

apply_issue_state_to_github() {
  local issue_number="$1"
  local state="$2"

  case "$state" in
    in-progress)
      gh issue reopen "$issue_number" >/dev/null 2>&1 || true
      gh issue edit "$issue_number" --add-label in-progress --remove-label in-review >/dev/null || true
      ;;
    in-review)
      gh issue reopen "$issue_number" >/dev/null 2>&1 || true
      gh issue edit "$issue_number" --add-label in-review --remove-label in-progress >/dev/null || true
      ;;
    done)
      gh issue close "$issue_number" >/dev/null || true
      ;;
    *)
      gh issue reopen "$issue_number" >/dev/null 2>&1 || true
      gh issue edit "$issue_number" --remove-label in-progress --remove-label in-review >/dev/null || true
      ;;
  esac
}

slugify() {
  local input="$1"
  local slug
  slug="$(echo "$input" | tr '[:upper:]' '[:lower:]' | tr -cs 'a-z0-9' '-' | sed 's/^-*//;s/-*$//')"
  printf '%s' "${slug:0:48}"
}

create_worktree() {
  local issue_number="$1"
  local issue_title="$2"
  local base_branch="$3"
  local existing_pr_number="${4:-}"

  local issue_slug
  issue_slug="$(slugify "$issue_title")"
  if [[ -z "$issue_slug" ]]; then
    issue_slug="task"
  fi
  local wt_name="ralph-${issue_number}-${issue_slug}"
  local wt_path="${WORKTREES_DIR}/${wt_name}"
  local branch="ralph/${issue_number}-${issue_slug}"

  # Resolve branch from existing PR if provided
  if [[ -n "$existing_pr_number" && "$existing_pr_number" != "null" ]]; then
    local pr_branch
    pr_branch="$(gh pr view "$existing_pr_number" --json headRefName --jq '.headRefName' 2>/dev/null || true)"
    if [[ -n "$pr_branch" ]]; then
      branch="$pr_branch"
    fi
  fi

  mkdir -p "$WORKTREES_DIR"

  if [[ -d "$wt_path" ]]; then
    log "INFO" "Worktree already exists at $wt_path" >&2
    printf '%s|%s' "$wt_path" "$branch"
    return
  fi

  git fetch origin "$base_branch" >/dev/null 2>&1 || true
  git fetch origin "$branch" >/dev/null 2>&1 || true

  local base_ref="origin/$base_branch"
  if ! git rev-parse --verify "$base_ref" >/dev/null 2>&1; then
    base_ref="$base_branch"
  fi

  if git show-ref --verify --quiet "refs/heads/$branch"; then
    git worktree add "$wt_path" "$branch" >&2
  elif git show-ref --verify --quiet "refs/remotes/origin/$branch"; then
    git worktree add --track -b "$branch" "$wt_path" "origin/$branch" >&2
  else
    git worktree add -b "$branch" "$wt_path" "$base_ref" >&2
  fi

  log "INFO" "Created worktree at $wt_path on branch $branch" >&2
  printf '%s|%s' "$wt_path" "$branch"
}

list_ralph_worktrees() {
  if [[ ! -d "$WORKTREES_DIR" ]]; then
    return
  fi
  git worktree list --porcelain | while IFS= read -r line; do
    if [[ "$line" == worktree\ * ]]; then
      local wt_path="${line#worktree }"
      if [[ "$wt_path" == "${WORKTREES_DIR}"/ralph-* ]]; then
        printf '%s\n' "$wt_path"
      fi
    fi
  done
}

cleanup_merged_worktrees() {
  if [[ ! -d "$WORKTREES_DIR" ]]; then
    return
  fi

  local worktrees
  worktrees="$(list_ralph_worktrees)"
  if [[ -z "$worktrees" ]]; then
    return
  fi

  while IFS= read -r wt_path; do
    [[ -n "$wt_path" ]] || continue
    local wt_name
    wt_name="$(basename "$wt_path")"

    # Extract issue number from worktree name (ralph-{N}-{slug})
    local issue_number
    if [[ "$wt_name" =~ ^ralph-([0-9]+)- ]]; then
      issue_number="${BASH_REMATCH[1]}"
    else
      log "WARN" "Cannot parse issue number from worktree name: $wt_name"
      continue
    fi

    # Extract branch name from worktree
    local wt_branch
    wt_branch="$(git worktree list --porcelain | awk -v path="$wt_path" '
      /^worktree / { p = substr($0, 10) }
      /^branch / && p == path { print substr($0, 8); exit }
    ')"

    # Check if PR for this branch is merged
    local short_branch=""
    local pr_state=""
    if [[ -n "$wt_branch" ]]; then
      short_branch="${wt_branch#refs/heads/}"
      pr_state="$(gh pr list --state merged --head "$short_branch" --json number --jq '.[0].number // empty' 2>/dev/null || true)"
    fi

    if [[ -n "$pr_state" ]]; then
      log "INFO" "Worktree $wt_name has merged PR #$pr_state — removing."
      git worktree remove --force "$wt_path" 2>/dev/null || true
      if [[ -d "$wt_path" ]]; then
        rm -rf "$wt_path"
      fi
      # Clean up the local branch
      if [[ -n "$short_branch" ]]; then
        git branch -D "$short_branch" 2>/dev/null || true
      fi
      log "INFO" "Removed worktree and branch for merged PR #$pr_state"
    fi
  done <<< "$worktrees"
}

stream_filter() {
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    local text=""
    text="$(printf '%s\n' "$line" | jq -r '
      def prefix_lines(p): split("\n") | map(select(. != "") | p + .) | join("\n");
      if type != "object" then "SANDBOX:" + (. | tostring)
      elif .type == "assistant" then
        [
          (.message.content[]? |
            if .type == "tool_use" then
              "TOOL:" + .name + (if .input.command then " $ " + (.input.command | tostring | split("\n")[0]) elif .input.file_path then " " + .input.file_path else "" end)
            elif .type == "text" then
              .text // empty | if . != "" then prefix_lines("AGENT:") else empty end
            else empty end
          )
        ] | map(select(. != null and . != "")) | join("\n")
      elif .type == "result" then
        .result // empty | if . != "" then prefix_lines("AGENT:") else empty end
      else
        empty
      end' 2>/dev/null || printf 'SANDBOX:%s' "$line")"
    if [[ -n "$text" ]]; then
      while IFS= read -r tline; do
        if [[ "$tline" == SANDBOX:* ]]; then
          printf '[SANDBOX] %s\n' "${tline#SANDBOX:}"
        elif [[ "$tline" == TOOL:* ]]; then
          printf '[TOOL]  %s\n' "${tline#TOOL:}"
        elif [[ "$tline" == AGENT:* ]]; then
          printf '[AGENT] %s\n' "${tline#AGENT:}"
        fi
      done <<< "$text"
    fi
  done
}

assistant_text_from_stream() {
  local output_file="$1"
  jq -r 'select(.type == "assistant") | .message.content[]? | select(.type == "text") | .text // empty' "$output_file" 2>/dev/null || true
}

run_agent() {
  local prompt_file="$1"
  local output_file="$2"
  local workspace="${3:-}"
  local exit_code=0
  local prompt_content
  prompt_content="$(cat "$prompt_file")"

  local workspace_args=()
  if [[ -n "$workspace" && ${#AGENT_PREFIX_ARR[@]} -gt 0 ]]; then
    workspace_args=(-w "$workspace")
  fi
  local stderr_file="${output_file%.jsonl}.stderr.log"
  set +eu
  ${AGENT_PREFIX_ARR[@]+"${AGENT_PREFIX_ARR[@]}"} ${workspace_args[@]+"${workspace_args[@]}"} "$AGENT_BIN" "${AGENT_ARGS_ARR[@]}" \
    --print --output-format stream-json \
    "${prompt_content}" \
    2> >(tee "$stderr_file" >&2) \
    | tee "$output_file" \
    | stream_filter
  exit_code=${PIPESTATUS[0]}
  set -eu

  return "$exit_code"
}

fetch_issues() {
  gh_retry gh issue list \
    --state open \
    --label "$LABEL_FILTER" \
    --limit "$ISSUE_LIMIT" \
    --json number,title,url,body,labels,createdAt,updatedAt,comments > "$ISSUES_FILE"
}

build_choose_prompt() {
  local prompt_file="$RUN_DIR/choose.prompt.md"
  cat "$CHOOSE_PROMPT_TEMPLATE" > "$prompt_file"
  {
    echo
    echo "## Runtime Files"
    echo "- Chosen-task output path: \`$TASK_FILE\`"
    echo
    echo "## Open Issues (GitHub JSON)"
    cat "$ISSUES_FILE"
  } >> "$prompt_file"
  printf '%s' "$prompt_file"
}

build_implement_prompt() {
  local issue_number="$1"
  local task_id="$2"
  local issue_title="$3"
  local issue_url="$4"
  local issue_body="$5"
  local branch="$6"
  local base_branch="$7"
  local prompt_file="$RUN_DIR/implement-${issue_number}.prompt.md"

  cat "$IMPLEMENT_PROMPT_TEMPLATE" > "$prompt_file"
  {
    echo
    echo "## Current Context"
    echo "Task ID: $task_id"
    echo "Repository branch: $branch"
    echo "Base branch: $base_branch"
    echo
    echo "## Issue"
    echo "Number: $issue_number"
    echo "Title: $issue_title"
    echo "URL: $issue_url"
    echo
    echo "Description:"
    printf '%s\n' "$issue_body"
  } >> "$prompt_file"

  printf '%s' "$prompt_file"
}

build_review_prompt() {
  local issue_number="$1"
  local issue_title="$2"
  local pr_number="$3"
  local pr_url="$4"
  local branch="$5"
  local previous_instructions_file="$6"
  local prompt_file="$RUN_DIR/review-${issue_number}-${pr_number}.prompt.md"

  cat "$REVIEW_PROMPT_TEMPLATE" > "$prompt_file"
  {
    echo
    echo "## Current Context"
    echo "Issue: #$issue_number - $issue_title"
    echo "PR: #$pr_number ($pr_url)"
    echo "Branch: $branch"
    echo
    echo "## Pull Request Context (JSON)"
    cat "$PR_CONTEXT_FILE"
    if [[ -n "$previous_instructions_file" && -f "$previous_instructions_file" ]]; then
      echo
      echo "## Previous instructions (context only)"
      cat "$previous_instructions_file"
    fi
  } >> "$prompt_file"

  printf '%s' "$prompt_file"
}

selected_task_id_from_file() {
  if [[ ! -f "$TASK_FILE" ]]; then
    return 1
  fi
  jq -r '.id // empty' "$TASK_FILE" 2>/dev/null
}

selected_task_pr_number_from_file() {
  if [[ ! -f "$TASK_FILE" ]]; then
    return 1
  fi
  jq -r '.githubPrNumber // empty' "$TASK_FILE" 2>/dev/null
}

review_complete_from_agent() {
  local output_file="$1"
  assistant_text_from_stream "$output_file" | rg -q "REVIEW_COMPLETE:"
}

resolve_base_branch() {
  if [[ -n "$BASE_BRANCH" ]]; then
    printf '%s' "$BASE_BRANCH"
    return
  fi
  gh_retry gh repo view --json defaultBranchRef --jq '.defaultBranchRef.name'
}

branch_has_commits_ahead_of_base() {
  local branch="$1"
  local base_branch="$2"
  local base_ref="origin/$base_branch"
  local ahead_count

  if ! git rev-parse --verify "$base_ref" >/dev/null 2>&1; then
    base_ref="$base_branch"
  fi
  if ! git rev-parse --verify "$base_ref" >/dev/null 2>&1; then
    log "WARN" "Could not verify base ref '$base_branch'; assuming branch '$branch' has changes."
    return 0
  fi

  ahead_count="$(git rev-list --count "$base_ref..$branch" 2>/dev/null || echo 0)"
  [[ "$ahead_count" =~ ^[0-9]+$ ]] || ahead_count=0
  [[ "$ahead_count" -gt 0 ]]
}

create_or_get_pr() {
  local issue_number="$1"
  local issue_title="$2"
  local issue_url="$3"
  local branch="$4"
  local base_branch="$5"
  local chosen_pr_number="${6:-}"
  local has_commits_ahead="${7:-}"
  local pr_number
  local pr_url

  if [[ -n "$chosen_pr_number" && "$chosen_pr_number" != "null" ]]; then
    pr_number="$chosen_pr_number"
    pr_url="$(gh_retry gh pr view "$pr_number" --json url --jq '.url')"
    printf '%s|%s' "$pr_number" "$pr_url"
    return
  fi

  pr_number="$(gh pr list --state open --head "$branch" --json number --jq '.[0].number // empty')"
  if [[ -n "$pr_number" ]]; then
    pr_url="$(gh pr view "$pr_number" --json url --jq '.url')"
    printf '%s|%s' "$pr_number" "$pr_url"
    return
  fi

  if [[ "$has_commits_ahead" != "true" ]]; then
    if branch_has_commits_ahead_of_base "$branch" "$base_branch"; then
      has_commits_ahead="true"
    else
      has_commits_ahead="false"
    fi
  fi

  if [[ "$has_commits_ahead" != "true" ]]; then
    printf '%s|%s' "$NO_PR_SENTINEL" "$NO_PR_REASON_NO_COMMITS"
    return
  fi

  if ! git rev-parse --verify "origin/$branch" >/dev/null 2>&1; then
    git push -u origin "$branch"
  else
    git push
  fi

  local pr_title
  pr_title="feat: ${issue_title} (#${issue_number})"
  local pr_body
  pr_body=$(cat <<EOF
Closes #$issue_number

Issue: $issue_url

Automated by scripts/ralph-github-loop.sh.
EOF
)

  gh pr create \
    --base "$base_branch" \
    --head "$branch" \
    --title "$pr_title" \
    --body "$pr_body" >/dev/null

  pr_number="$(gh pr list --state open --head "$branch" --json number --jq '.[0].number // empty')"
  [[ -n "$pr_number" ]] || die "Failed to create PR for issue #$issue_number"
  pr_url="$(gh pr view "$pr_number" --json url --jq '.url')"

  gh issue comment "$issue_number" --body "Opened PR #$pr_number for this issue: $pr_url" >/dev/null || true
  printf '%s|%s' "$pr_number" "$pr_url"
}


run_review_loops() {
  local issue_number="$1"
  local issue_title="$2"
  local pr_number="$3"
  local pr_url="$4"
  local branch="$5"
  local previous_instructions_file="$6"
  local workspace="${7:-}"

  for ((loop=1; loop<=MAX_REVIEW_LOOPS; loop++)); do
    log "INFO" "Review loop $loop/$MAX_REVIEW_LOOPS for issue #$issue_number (PR #$pr_number)"

    gh pr view "$pr_number" --json number,title,url,body,state,reviews,comments,files,headRefName,baseRefName > "$PR_CONTEXT_FILE"

    local review_prompt
    review_prompt="$(build_review_prompt "$issue_number" "$issue_title" "$pr_number" "$pr_url" "$branch" "$previous_instructions_file")"
    local review_output
    review_output="$RUN_DIR/review-${issue_number}-${loop}.stream.jsonl"

    if ! run_agent "$review_prompt" "$review_output" "$workspace"; then
      log "WARN" "Review agent exited non-zero for issue #$issue_number on loop $loop"
    fi

    local ahead_count=0
    if git rev-parse --verify "origin/$branch" >/dev/null 2>&1; then
      ahead_count="$(git rev-list --count "origin/$branch..HEAD" 2>/dev/null || echo 0)"
    fi
    if [[ "$ahead_count" -gt 0 ]]; then
      git push
    fi

    if review_complete_from_agent "$review_output"; then
      log "SUCCESS" "Review loop signaled completion for issue #$issue_number"
      break
    fi
  done
}

# ── Task selection ────────────────────────────────────────────

pick_task_selection() {
  local choose_prompt
  choose_prompt="$(build_choose_prompt)"
  local choose_output="$RUN_DIR/choose.stream.jsonl"
  local selected_task_id=""
  local selected_task_pr=""
  local selected_issue=""
  PICKED_TASK_ID=""
  PICKED_ISSUE_NUMBER=""
  PICKED_PR_NUMBER=""
  rm -f "$TASK_FILE"

  if run_agent "$choose_prompt" "$choose_output"; then
    selected_task_id="$(selected_task_id_from_file || true)"
    selected_task_pr="$(selected_task_pr_number_from_file || true)"
  else
    log "WARN" "Chooser agent exited non-zero, using deterministic fallback selection."
  fi

  # Agent selection: extract and verify issue number from task id
  if [[ -n "$selected_task_id" ]]; then
    if ! selected_issue="$(issue_number_from_task_id "$selected_task_id" 2>/dev/null)"; then
      log "WARN" "Cannot parse issue number from task id '$selected_task_id'; using fallback."
      selected_task_id=""
    elif ! jq -e --argjson n "$selected_issue" '.[] | select(.number == $n)' "$ISSUES_FILE" >/dev/null 2>&1; then
      log "WARN" "Issue #$selected_issue not found in fetched issues; using fallback."
      selected_task_id=""
      selected_issue=""
    fi
  fi

  # Fallback: deterministic selection from issues.json
  if [[ -z "$selected_task_id" ]]; then
    local fallback_json
    fallback_json="$(select_next_issue)"
    if [[ -z "$fallback_json" ]]; then
      return 1
    fi
    selected_issue="$(printf '%s' "$fallback_json" | jq -r '.number')"
    selected_task_id="gh-${selected_issue}"
    selected_task_pr="$(find_open_pr_for_issue "$selected_issue" || true)"
  fi

  [[ -n "$selected_issue" ]] || return 1

  PICKED_TASK_ID="$selected_task_id"
  PICKED_ISSUE_NUMBER="$selected_issue"
  PICKED_PR_NUMBER="${selected_task_pr:-}"
  return 0
}

process_issue() {
  local task_id="$1"
  local issue_number="$2"
  local chosen_pr_number="$3"
  local base_branch="$4"
  local issue_title
  local issue_url
  local issue_body
  local branch
  local impl_prompt
  local impl_output
  local pr_info
  local pr_number
  local pr_url
  local has_commits_ahead="false"

  issue_title="$(jq -r --argjson n "$issue_number" '.[] | select(.number == $n) | .title' "$ISSUES_FILE" | head -1)"
  issue_url="$(jq -r --argjson n "$issue_number" '.[] | select(.number == $n) | .url' "$ISSUES_FILE" | head -1)"
  issue_body="$(jq -r --argjson n "$issue_number" '.[] | select(.number == $n) | .body // ""' "$ISSUES_FILE")"
  [[ -n "$issue_title" ]] || die "Could not load issue #$issue_number details from issue list."

  log "INFO" "Selected task $task_id (issue #$issue_number): $issue_title"

  apply_issue_state_to_github "$issue_number" "in-progress"

  local wt_info wt_path
  wt_info="$(create_worktree "$issue_number" "$issue_title" "$base_branch" "${chosen_pr_number:-}")"
  wt_path="${wt_info%%|*}"
  branch="${wt_info##*|}"
  log "INFO" "Worktree ready at $wt_path (branch: $branch)"

  # Run all git operations inside the worktree
  pushd "$wt_path" >/dev/null

  impl_prompt="$(build_implement_prompt "$issue_number" "$task_id" "$issue_title" "$issue_url" "$issue_body" "$branch" "$base_branch")"
  impl_output="$RUN_DIR/implement-${issue_number}.stream.jsonl"

  log "INFO" "Running implementation agent for issue #$issue_number on branch $branch"
  if ! run_agent "$impl_prompt" "$impl_output" "$wt_path"; then
    log "WARN" "Implementation agent exited non-zero for issue #$issue_number"
  fi

  if branch_has_commits_ahead_of_base "$branch" "$base_branch"; then
    has_commits_ahead="true"
    log "INFO" "Detected commits ahead of '$base_branch' on branch '$branch'; PR flow will proceed."
  else
    log "INFO" "No commits ahead of '$base_branch' on branch '$branch' after implementation."
  fi

  pr_info="$(create_or_get_pr "$issue_number" "$issue_title" "$issue_url" "$branch" "$base_branch" "${chosen_pr_number:-}" "$has_commits_ahead")"
  pr_number="${pr_info%%|*}"
  pr_url="${pr_info##*|}"

  if [[ "$pr_number" == "$NO_PR_SENTINEL" && "$pr_url" == "$NO_PR_REASON_NO_COMMITS" ]]; then
    log "INFO" "Skipping PR/review for issue #$issue_number because no branch diff was produced; resetting issue to todo."
    apply_issue_state_to_github "$issue_number" "todo"
    popd >/dev/null
    return
  fi

  log "SUCCESS" "Issue #$issue_number linked to PR #$pr_number ($pr_url)"
  apply_issue_state_to_github "$issue_number" "in-review"

  run_review_loops "$issue_number" "$issue_title" "$pr_number" "$pr_url" "$branch" "$impl_prompt" "$wt_path"

  popd >/dev/null
}

run_direct_review() {
  local pr_number="$1"
  local base_branch="$2"

  local pr_json
  pr_json="$(gh_retry gh pr view "$pr_number" --json number,title,url,body,headRefName,baseRefName,state)" \
    || die "Failed to fetch PR #$pr_number"

  local pr_url branch pr_title pr_body
  pr_url="$(printf '%s' "$pr_json" | jq -r '.url')"
  branch="$(printf '%s' "$pr_json" | jq -r '.headRefName')"
  pr_title="$(printf '%s' "$pr_json" | jq -r '.title')"
  pr_body="$(printf '%s' "$pr_json" | jq -r '.body // ""')"

  # Extract issue number from PR body (Closes/Fixes/Resolves #N) or title (#N)
  local issue_number=""
  if [[ "$pr_body" =~ (Closes|Fixes|Resolves)[[:space:]]+#([0-9]+) ]]; then
    issue_number="${BASH_REMATCH[2]}"
  elif [[ "$pr_title" =~ \#([0-9]+) ]]; then
    issue_number="${BASH_REMATCH[1]}"
  fi

  local issue_title="$pr_title"
  if [[ -n "$issue_number" ]]; then
    issue_title="$(gh issue view "$issue_number" --json title --jq '.title' 2>/dev/null || echo "$pr_title")"
  else
    issue_number="0"
    log "WARN" "Could not extract issue number from PR #$pr_number; using 0"
  fi

  # Create/reuse worktree for the PR branch
  local wt_info wt_path
  wt_info="$(create_worktree "$issue_number" "$issue_title" "$base_branch" "$pr_number")"
  wt_path="${wt_info%%|*}"
  branch="${wt_info##*|}"

  pushd "$wt_path" >/dev/null

  log "INFO" "Running direct review for PR #$pr_number (issue #$issue_number) on branch $branch"
  run_review_loops "$issue_number" "$issue_title" "$pr_number" "$pr_url" "$branch" "" "$wt_path"

  popd >/dev/null
}

main() {
  parse_args "$@"

  require_cmd gh
  require_cmd jq
  require_cmd git
  require_cmd rg
  if [[ ${#AGENT_PREFIX_ARR[@]} -gt 0 ]]; then
    require_cmd "${AGENT_PREFIX_ARR[0]}"
  fi
  require_cmd "$AGENT_BIN"

  [[ -f "$CHOOSE_PROMPT_TEMPLATE" ]] || die "Missing prompt template: $CHOOSE_PROMPT_TEMPLATE"
  [[ -f "$IMPLEMENT_PROMPT_TEMPLATE" ]] || die "Missing prompt template: $IMPLEMENT_PROMPT_TEMPLATE"
  [[ -f "$REVIEW_PROMPT_TEMPLATE" ]] || die "Missing prompt template: $REVIEW_PROMPT_TEMPLATE"

  gh auth status >/dev/null 2>&1 || die "GitHub CLI is not authenticated. Run: gh auth login"
  git rev-parse --git-dir >/dev/null 2>&1 || die "Not inside a git repository."

  mkdir -p "$RUN_DIR" "$RALPH_DIR"
  local base_branch
  base_branch="$(resolve_base_branch)"

  if [[ -n "$REVIEW_PR_NUMBER" ]]; then
    log "INFO" "Direct review mode for PR #$REVIEW_PR_NUMBER"
    run_direct_review "$REVIEW_PR_NUMBER" "$base_branch"
    return
  fi

  log "INFO" "Starting ralph GitHub loop"
  log "INFO" "Label filter: $LABEL_FILTER"
  log "INFO" "Base branch: $base_branch"
  log "INFO" "Polling interval: ${POLL_SECONDS}s"
  log "INFO" "Max review loops: $MAX_REVIEW_LOOPS"
  log "INFO" "Once mode: $ONCE_MODE"
  log "INFO" "Agent prefix: ${AGENT_CMD_PREFIX:-<none>}"
  log "INFO" "Worktrees dir: $WORKTREES_DIR"

  while true; do
    cleanup_merged_worktrees
    fetch_issues

    local actionable_count
    actionable_count="$(count_actionable_issues)"

    if [[ "$actionable_count" -eq 0 ]]; then
      if [[ "$ONCE_MODE" == "true" ]]; then
        log "INFO" "No actionable tasks. Exiting."
        break
      fi
      log "INFO" "No actionable tasks. Sleeping ${POLL_SECONDS}s."
      sleep "$POLL_SECONDS"
      continue
    fi

    if ! pick_task_selection; then
      if [[ "$ONCE_MODE" == "true" ]]; then
        log "INFO" "No eligible task selected. Exiting due to --once."
        break
      fi
      log "WARN" "No eligible task selected. Retrying in ${POLL_SECONDS}s."
      sleep "$POLL_SECONDS"
      continue
    fi

    process_issue "$PICKED_TASK_ID" "$PICKED_ISSUE_NUMBER" "$PICKED_PR_NUMBER" "$base_branch"

    if [[ "$ONCE_MODE" == "true" ]]; then
      log "INFO" "Completed one issue iteration in --once mode. Exiting."
      break
    fi
  done
}

main "$@"
