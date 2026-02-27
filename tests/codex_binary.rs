use std::path::PathBuf;
use std::time::Duration;

use rlph::error::Error;
use rlph::process::{ProcessConfig, ProcessOutput, spawn_and_stream};
use serde_json::Value;

fn integration_enabled() -> bool {
    std::env::var("RLPH_INTEGRATION").is_ok()
}

fn working_dir() -> PathBuf {
    std::env::current_dir().unwrap()
}

const TIMEOUT: Duration = Duration::from_secs(60);
const PROMPT: &str = "Respond with only the word hello";

/// Extract `thread_id` from codex JSON output lines.
///
/// Scans lines for JSON objects with a top-level `thread_id` field.
/// Returns the last one found.
fn extract_thread_id(stdout_lines: &[String]) -> Option<String> {
    let mut last_id = None;
    for line in stdout_lines {
        if let Ok(val) = serde_json::from_str::<Value>(line)
            && let Some(id) = val.get("thread_id").and_then(|v| v.as_str())
            && !id.is_empty()
        {
            last_id = Some(id.to_string());
        }
    }
    last_id
}

fn base_args() -> Vec<String> {
    vec![
        "exec".to_string(),
        "--dangerously-bypass-approvals-and-sandbox".to_string(),
        "--json".to_string(),
    ]
}

fn config_with_args(args: Vec<String>, stdin_data: Option<String>) -> ProcessConfig {
    ProcessConfig {
        command: "codex".to_string(),
        args,
        working_dir: working_dir(),
        timeout: Some(TIMEOUT),
        log_prefix: "test-codex".to_string(),
        stream_output: false,
        env: vec![],
        stdin_data,
        quiet: true,
    }
}

// ---------------------------------------------------------------------------
// Skip / error resilience helpers
// ---------------------------------------------------------------------------

fn classify_codex_skip(stdout_lines: &[String], stderr_lines: &[String]) -> Option<String> {
    // Only check stderr and non-JSON stdout lines to avoid false-positives
    // from error text embedded inside JSON event values (e.g. aggregated_output).
    let lines = stderr_lines
        .iter()
        .chain(
            stdout_lines
                .iter()
                .filter(|l| serde_json::from_str::<Value>(l).is_err()),
        )
        .map(|line| line.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("\n");

    if lines.contains("command not found") || lines.contains("no such file or directory") {
        return Some("codex binary not found".to_string());
    }

    if lines.contains("api key") || lines.contains("authentication") {
        return Some("codex API key / auth not configured".to_string());
    }

    None
}

async fn run_config_or_skip(config: ProcessConfig) -> Option<ProcessOutput> {
    match spawn_and_stream(config).await {
        Ok(output) => {
            if let Some(reason) = classify_codex_skip(&output.stdout_lines, &output.stderr_lines) {
                eprintln!("skipping codex integration test: {reason}");
                return None;
            }
            Some(output)
        }
        Err(Error::ProcessTimeout {
            stdout_lines,
            stderr_lines,
            ..
        }) => {
            if let Some(reason) = classify_codex_skip(&stdout_lines, &stderr_lines) {
                eprintln!("skipping codex integration test: {reason}");
                return None;
            }
            panic!("codex timed out unexpectedly; stdout={stdout_lines:?} stderr={stderr_lines:?}");
        }
        Err(Error::Io(ref e)) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping codex integration test: codex binary not found");
            None
        }
        Err(err) => panic!("codex should complete successfully: {err:?}"),
    }
}

async fn run_codex_or_skip(args: Vec<String>, stdin_data: Option<String>) -> Option<ProcessOutput> {
    run_config_or_skip(config_with_args(args, stdin_data)).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_codex_emits_thread_id() {
    if !integration_enabled() {
        return;
    }

    let mut args = base_args();
    args.push("-".to_string());

    let Some(output) = run_codex_or_skip(args, Some(PROMPT.to_string())).await else {
        return;
    };

    assert_eq!(
        output.exit_code, 0,
        "codex exited with {}",
        output.exit_code
    );

    let thread_id = extract_thread_id(&output.stdout_lines);
    assert!(
        thread_id.is_some(),
        "expected thread_id in codex JSON output"
    );
    assert!(
        !thread_id.unwrap().is_empty(),
        "thread_id should be non-empty"
    );
}

#[tokio::test]
async fn test_codex_model_flag() {
    if !integration_enabled() {
        return;
    }

    let mut args = base_args();
    args.extend([
        "--model".to_string(),
        "gpt-5.3-codex".to_string(),
        "-".to_string(),
    ]);

    let Some(output) = run_codex_or_skip(args, Some(PROMPT.to_string())).await else {
        return;
    };

    assert_eq!(
        output.exit_code, 0,
        "codex with --model flag exited with {}",
        output.exit_code
    );
}

#[tokio::test]
async fn test_codex_effort_via_config() {
    if !integration_enabled() {
        return;
    }

    let mut args = base_args();
    args.extend([
        "--config".to_string(),
        "model_reasoning_effort=\"low\"".to_string(),
        "-".to_string(),
    ]);

    let Some(output) = run_codex_or_skip(args, Some(PROMPT.to_string())).await else {
        return;
    };

    assert_eq!(
        output.exit_code, 0,
        "codex with --config model_reasoning_effort exited with {}",
        output.exit_code
    );
}

#[tokio::test]
async fn test_codex_resume_with_prompt() {
    if !integration_enabled() {
        return;
    }

    // First invocation: get a thread_id.
    let mut args1 = base_args();
    args1.push("-".to_string());

    let Some(output1) = run_codex_or_skip(args1, Some("Say hello".to_string())).await else {
        return;
    };

    assert_eq!(output1.exit_code, 0);

    let thread_id =
        extract_thread_id(&output1.stdout_lines).expect("first invocation must emit thread_id");

    // Second invocation: resume with a new prompt.
    let mut args2 = base_args();
    args2.extend(["resume".to_string(), thread_id.clone(), "-".to_string()]);

    let Some(output2) = run_codex_or_skip(args2, Some("Now say goodbye".to_string())).await else {
        return;
    };

    assert_eq!(
        output2.exit_code, 0,
        "resumed session exited with {}",
        output2.exit_code
    );

    // Stdout should contain some response.
    assert!(
        !output2.stdout_lines.is_empty(),
        "resumed session should produce output"
    );
}

// ---------------------------------------------------------------------------
// JSON event schema validation helpers
// ---------------------------------------------------------------------------
//
// Codex `--json` emits JSONL with these top-level event types:
//
//   thread.started  — { type, thread_id }
//   turn.started    — { type }
//   item.started    — { type, item: { id, type, ... } }      (command_execution only)
//   item.completed  — { type, item: { id, type, ... } }
//   turn.completed  — { type, usage: { input_tokens, cached_input_tokens, output_tokens } }
//
// Item subtypes (item.type):
//   reasoning          — { id, type, text }
//   agent_message      — { id, type, text }
//   command_execution  — { id, type, command, aggregated_output, exit_code, status }
//

fn parse_events(output: &ProcessOutput) -> Vec<Value> {
    output
        .stdout_lines
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

fn event_types(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| e.get("type").and_then(Value::as_str))
        .collect()
}

fn find_events_by_type<'a>(events: &'a [Value], ty: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|e| e.get("type").and_then(Value::as_str) == Some(ty))
        .collect()
}

fn find_items<'a>(events: &'a [Value], event_type: &str, item_type: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|e| {
            e.get("type").and_then(Value::as_str) == Some(event_type)
                && e.get("item")
                    .and_then(|i| i.get("type"))
                    .and_then(Value::as_str)
                    == Some(item_type)
        })
        .map(|e| e.get("item").unwrap())
        .collect()
}

/// Assert a field exists and is a non-empty string.
fn assert_str(val: &Value, field: &str, ctx: &str) {
    assert!(
        val.get(field)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty()),
        "{ctx}: must have non-empty string field '{field}'"
    );
}

/// Assert a field exists and is a number.
fn assert_number(val: &Value, field: &str, ctx: &str) {
    assert!(
        val.get(field).and_then(Value::as_f64).is_some(),
        "{ctx}: must have number field '{field}'"
    );
}

// ---------------------------------------------------------------------------
// Validators for each event type
// ---------------------------------------------------------------------------

fn validate_thread_started(events: &[Value]) -> &Value {
    let types = event_types(events);
    let evt = find_events_by_type(events, "thread.started")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("expected thread.started event; got types: {types:?}"));

    assert_str(evt, "thread_id", "thread.started");
    evt
}

fn validate_turn_started(events: &[Value]) {
    let types = event_types(events);
    assert!(
        !find_events_by_type(events, "turn.started").is_empty(),
        "expected turn.started event; got types: {types:?}"
    );
}

fn validate_turn_completed(events: &[Value]) -> &Value {
    let types = event_types(events);
    let evt = find_events_by_type(events, "turn.completed")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("expected turn.completed event; got types: {types:?}"));

    let usage = evt
        .get("usage")
        .unwrap_or_else(|| panic!("turn.completed: must have usage object"));
    assert_number(usage, "input_tokens", "turn.completed usage");
    assert_number(usage, "output_tokens", "turn.completed usage");

    evt
}

fn validate_item_agent_message(item: &Value) {
    assert_str(item, "id", "agent_message item");
    assert_eq!(
        item.get("type").and_then(Value::as_str),
        Some("agent_message"),
    );
    assert_str(item, "text", "agent_message item");
}

fn validate_item_reasoning(item: &Value) {
    assert_str(item, "id", "reasoning item");
    assert_eq!(item.get("type").and_then(Value::as_str), Some("reasoning"));
    // text may be empty for minimal reasoning, but field must exist
    assert!(
        item.get("text").and_then(Value::as_str).is_some(),
        "reasoning item: must have text field"
    );
}

fn validate_item_command_execution_started(event: &Value) {
    let item = event
        .get("item")
        .unwrap_or_else(|| panic!("item.started: must have item object"));
    assert_str(item, "id", "command_execution started item");
    assert_eq!(
        item.get("type").and_then(Value::as_str),
        Some("command_execution"),
    );
    assert_str(item, "command", "command_execution started item");
    assert_eq!(
        item.get("status").and_then(Value::as_str),
        Some("in_progress"),
        "command_execution started item: status must be 'in_progress'"
    );
    assert!(
        item.get("exit_code").is_some_and(Value::is_null),
        "command_execution started item: exit_code must be null"
    );
}

fn validate_item_command_execution_completed(item: &Value) {
    assert_str(item, "id", "command_execution completed item");
    assert_eq!(
        item.get("type").and_then(Value::as_str),
        Some("command_execution"),
    );
    assert_str(item, "command", "command_execution completed item");
    assert!(
        item.get("aggregated_output")
            .and_then(Value::as_str)
            .is_some(),
        "command_execution completed item: must have aggregated_output string"
    );
    assert!(
        item.get("exit_code").and_then(Value::as_i64).is_some(),
        "command_execution completed item: must have integer exit_code"
    );
    let status = item.get("status").and_then(Value::as_str).unwrap_or("");
    assert!(
        status == "completed" || status == "failed",
        "command_execution completed item: status must be 'completed' or 'failed', got '{status}'"
    );
}

// ---------------------------------------------------------------------------
// Test: simple text response schema
// thread.started → turn.started → item.completed(reasoning) →
// item.completed(agent_message) → turn.completed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_codex_json_text_response_schema() {
    if !integration_enabled() {
        return;
    }

    let mut args = base_args();
    args.push("-".to_string());

    let Some(output) = run_codex_or_skip(args, Some(PROMPT.to_string())).await else {
        return;
    };

    assert_eq!(
        output.exit_code, 0,
        "codex exited with {}",
        output.exit_code
    );

    let events = parse_events(&output);
    let types = event_types(&events);
    assert!(!events.is_empty(), "expected JSON events on stdout");

    // Every line must parse as JSON
    assert_eq!(
        events.len(),
        output.stdout_lines.len(),
        "all stdout lines should be valid JSON"
    );

    // Validate lifecycle events
    validate_thread_started(&events);
    validate_turn_started(&events);
    validate_turn_completed(&events);

    // Validate agent_message item(s)
    let agent_messages = find_items(&events, "item.completed", "agent_message");
    assert!(
        !agent_messages.is_empty(),
        "expected at least one agent_message item; got types: {types:?}"
    );
    for msg in &agent_messages {
        validate_item_agent_message(msg);
    }

    // Reasoning items are optional (model may skip for trivial prompts) but
    // if present they must have the right shape.
    let reasoning_items = find_items(&events, "item.completed", "reasoning");
    for r in &reasoning_items {
        validate_item_reasoning(r);
    }

    // Event ordering: thread.started first, turn.completed last
    assert_eq!(
        types.first().copied(),
        Some("thread.started"),
        "first event must be thread.started"
    );
    assert_eq!(
        types.last().copied(),
        Some("turn.completed"),
        "last event must be turn.completed"
    );

    // turn.started must come before any item events
    let turn_started_idx = types.iter().position(|t| *t == "turn.started").unwrap();
    let first_item_idx = types
        .iter()
        .position(|t| t.starts_with("item."))
        .unwrap_or(usize::MAX);
    assert!(
        turn_started_idx < first_item_idx,
        "turn.started must precede item events"
    );
}

// ---------------------------------------------------------------------------
// Test: command execution (tool use) schema
// thread.started → turn.started → item.completed(reasoning) →
// item.completed(agent_message) → item.started(command_execution) →
// item.completed(command_execution) → item.completed(agent_message) →
// turn.completed
// ---------------------------------------------------------------------------

const TOOL_TIMEOUT: Duration = Duration::from_secs(120);

#[tokio::test]
async fn test_codex_json_command_execution_schema() {
    if !integration_enabled() {
        return;
    }

    let mut args = base_args();
    args.push("-".to_string());

    let config = ProcessConfig {
        command: "codex".to_string(),
        args,
        working_dir: working_dir(),
        timeout: Some(TOOL_TIMEOUT),
        log_prefix: "test-codex-tool".to_string(),
        stream_output: false,
        env: vec![],
        stdin_data: Some(
            "Read the file Cargo.toml and tell me the package name. Be very concise.".to_string(),
        ),
        quiet: true,
    };

    let Some(output) = run_config_or_skip(config).await else {
        return;
    };

    assert_eq!(
        output.exit_code, 0,
        "codex exited with {}",
        output.exit_code
    );

    let events = parse_events(&output);
    let types = event_types(&events);

    // Lifecycle events
    validate_thread_started(&events);
    validate_turn_started(&events);
    validate_turn_completed(&events);

    // Must have at least one command_execution via item.started
    let cmd_started = find_events_by_type(&events, "item.started")
        .into_iter()
        .filter(|e| {
            e.get("item")
                .and_then(|i| i.get("type"))
                .and_then(Value::as_str)
                == Some("command_execution")
        })
        .collect::<Vec<_>>();
    assert!(
        !cmd_started.is_empty(),
        "expected item.started with command_execution; got types: {types:?}"
    );
    for evt in &cmd_started {
        validate_item_command_execution_started(evt);
    }

    // Must have at least one command_execution via item.completed
    let cmd_completed = find_items(&events, "item.completed", "command_execution");
    assert!(
        !cmd_completed.is_empty(),
        "expected item.completed with command_execution; got types: {types:?}"
    );
    for item in &cmd_completed {
        validate_item_command_execution_completed(item);
    }

    // IDs must match between started and completed for each command
    let started_ids: Vec<&str> = cmd_started
        .iter()
        .filter_map(|e| e["item"]["id"].as_str())
        .collect();
    let completed_ids: Vec<&str> = cmd_completed
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    for sid in &started_ids {
        assert!(
            completed_ids.contains(sid),
            "item.started id '{sid}' has no matching item.completed"
        );
    }

    // item.started must come before its matching item.completed
    for sid in &started_ids {
        let started_pos = events
            .iter()
            .position(|e| {
                e.get("type").and_then(Value::as_str) == Some("item.started")
                    && e["item"]["id"].as_str() == Some(sid)
            })
            .unwrap();
        let completed_pos = events
            .iter()
            .position(|e| {
                e.get("type").and_then(Value::as_str) == Some("item.completed")
                    && e["item"]["id"].as_str() == Some(sid)
            })
            .unwrap();
        assert!(
            started_pos < completed_pos,
            "item.started for '{sid}' must precede its item.completed"
        );
    }

    // Should also have agent_message(s) — the model's textual response
    let agent_messages = find_items(&events, "item.completed", "agent_message");
    assert!(
        !agent_messages.is_empty(),
        "expected agent_message items alongside command_execution"
    );
    for msg in &agent_messages {
        validate_item_agent_message(msg);
    }

    // The completed command should have a real command string and output
    let first_cmd = cmd_completed[0];
    let command_str = first_cmd["command"].as_str().unwrap();
    assert!(
        !command_str.is_empty(),
        "command_execution must have non-empty command"
    );
    let output_str = first_cmd["aggregated_output"].as_str().unwrap();
    assert!(
        !output_str.is_empty(),
        "successful command should produce non-empty aggregated_output"
    );
    assert_eq!(
        first_cmd["exit_code"].as_i64(),
        Some(0),
        "successful command should have exit_code 0"
    );
    assert_eq!(
        first_cmd["status"].as_str(),
        Some("completed"),
        "successful command should have status 'completed'"
    );
}

// ---------------------------------------------------------------------------
// Test: failed command execution schema
// Validates that command_execution items with non-zero exit codes have
// status "failed" and still carry the expected fields.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_codex_json_failed_command_schema() {
    if !integration_enabled() {
        return;
    }

    let mut args = base_args();
    args.push("-".to_string());

    let config = ProcessConfig {
        command: "codex".to_string(),
        args,
        working_dir: working_dir(),
        timeout: Some(TOOL_TIMEOUT),
        log_prefix: "test-codex-fail".to_string(),
        stream_output: false,
        env: vec![],
        stdin_data: Some(
            "Run the command: cat /nonexistent_file_xyz_abc_123. Report the exact error."
                .to_string(),
        ),
        quiet: true,
    };

    let Some(output) = run_config_or_skip(config).await else {
        return;
    };

    assert_eq!(
        output.exit_code, 0,
        "codex exited with {}",
        output.exit_code
    );

    let events = parse_events(&output);

    // Lifecycle
    validate_thread_started(&events);
    validate_turn_completed(&events);

    // Must have a command_execution that failed.
    // The LLM may refuse or run a different command, so skip gracefully
    // rather than failing — this test is inherently non-deterministic.
    let cmd_completed = find_items(&events, "item.completed", "command_execution");
    if cmd_completed.is_empty() {
        eprintln!("skipping: LLM did not produce any command_execution items");
        return;
    }

    let Some(failed_item) = cmd_completed
        .iter()
        .find(|item| item.get("status").and_then(Value::as_str) == Some("failed"))
    else {
        eprintln!(
            "skipping: no command_execution with status 'failed'; statuses: {:?}",
            cmd_completed
                .iter()
                .map(|i| i.get("status"))
                .collect::<Vec<_>>()
        );
        return;
    };
    validate_item_command_execution_completed(failed_item);

    // Non-zero exit code
    let exit_code = failed_item["exit_code"].as_i64().unwrap();
    assert_ne!(
        exit_code, 0,
        "failed command should have non-zero exit_code"
    );

    // aggregated_output should contain error text
    let agg_output = failed_item["aggregated_output"].as_str().unwrap();
    assert!(
        !agg_output.is_empty(),
        "failed command should have non-empty aggregated_output with error text"
    );
}
