use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rlph::error::Error;
use rlph::process::{ProcessConfig, spawn_and_stream};
use rlph::runner::extract_session_id;
use serde_json::Value;

fn integration_enabled() -> bool {
    std::env::var("RLPH_INTEGRATION").is_ok()
}

fn working_dir() -> PathBuf {
    std::env::current_dir().unwrap()
}

const TIMEOUT: Duration = Duration::from_secs(60);
const PROMPT: &str = "Respond with only the word hello";

fn base_args() -> Vec<String> {
    vec![
        "--print".to_string(),
        "--verbose".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--dangerously-skip-permissions".to_string(),
    ]
}

fn config_with_args(args: Vec<String>, stdin_data: Option<String>) -> ProcessConfig {
    ProcessConfig {
        command: "claude".to_string(),
        args,
        working_dir: working_dir(),
        timeout: Some(TIMEOUT),
        log_prefix: "test-claude".to_string(),
        stream_output: false,
        env: vec![],
        stdin_data,
        quiet: true,
    }
}

fn classify_claude_skip(stdout_lines: &[String], stderr_lines: &[String]) -> Option<String> {
    let combined = stdout_lines
        .iter()
        .chain(stderr_lines.iter())
        .map(|line| line.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("\n");

    if combined.contains("eperm: operation not permitted") && combined.contains("/.claude/debug/") {
        return Some("claude cannot write ~/.claude/debug in this sandbox".to_string());
    }

    None
}

async fn run_claude_or_skip(args: Vec<String>) -> Option<rlph::process::ProcessOutput> {
    match spawn_and_stream(config_with_args(args, None)).await {
        Ok(output) => {
            if let Some(reason) = classify_claude_skip(&output.stdout_lines, &output.stderr_lines) {
                eprintln!("skipping claude integration test: {reason}");
                return None;
            }
            Some(output)
        }
        Err(Error::ProcessTimeout {
            stdout_lines,
            stderr_lines,
            ..
        }) => {
            if let Some(reason) = classify_claude_skip(&stdout_lines, &stderr_lines) {
                eprintln!("skipping claude integration test: {reason}");
                return None;
            }
            panic!(
                "claude timed out unexpectedly; stdout={stdout_lines:?} stderr={stderr_lines:?}"
            );
        }
        Err(err) => panic!("claude should complete successfully: {err:?}"),
    }
}

fn extract_session_id_from_output(output: &rlph::process::ProcessOutput) -> Option<String> {
    let mut lines = output.stdout_lines.clone();
    lines.extend(output.stderr_lines.clone());
    extract_session_id(&lines)
}

#[tokio::test]
async fn test_claude_emits_session_id() {
    if !integration_enabled() {
        return;
    }

    let mut args = base_args();
    args.extend(["-p".to_string(), PROMPT.to_string()]);

    let Some(output) = run_claude_or_skip(args).await else {
        return;
    };

    assert_eq!(
        output.exit_code, 0,
        "claude exited with {}",
        output.exit_code
    );

    let session_id = extract_session_id_from_output(&output);
    assert!(
        session_id.is_some(),
        "expected session_id in stream-json output"
    );
    assert!(
        !session_id.unwrap().is_empty(),
        "session_id should be non-empty"
    );
}

#[tokio::test]
async fn test_claude_model_flag() {
    if !integration_enabled() {
        return;
    }

    let mut args = base_args();
    args.extend([
        "--model".to_string(),
        "claude-haiku-4-5-20251001".to_string(),
        "-p".to_string(),
        PROMPT.to_string(),
    ]);

    let Some(output) = run_claude_or_skip(args).await else {
        return;
    };

    assert_eq!(
        output.exit_code, 0,
        "claude with --model flag exited with {}",
        output.exit_code
    );
}

#[tokio::test]
async fn test_claude_effort_flag() {
    if !integration_enabled() {
        return;
    }

    let mut args = base_args();
    args.extend([
        "--effort".to_string(),
        "low".to_string(),
        "-p".to_string(),
        PROMPT.to_string(),
    ]);

    let Some(output) = run_claude_or_skip(args).await else {
        return;
    };

    assert_eq!(
        output.exit_code, 0,
        "claude with --effort flag exited with {}",
        output.exit_code
    );
}

#[tokio::test]
async fn test_claude_resume_with_prompt() {
    if !integration_enabled() {
        return;
    }

    // First invocation: get a session_id.
    let mut args1 = base_args();
    args1.extend(["-p".to_string(), "Say hello".to_string()]);

    let Some(output1) = run_claude_or_skip(args1).await else {
        return;
    };

    assert_eq!(output1.exit_code, 0);

    let session_id =
        extract_session_id_from_output(&output1).expect("first invocation must emit session_id");

    // Second invocation: resume with a new prompt.
    let mut args2 = base_args();
    args2.extend([
        "--resume".to_string(),
        session_id.clone(),
        "-p".to_string(),
        "Now say goodbye".to_string(),
    ]);

    let Some(output2) = run_claude_or_skip(args2).await else {
        return;
    };

    assert_eq!(
        output2.exit_code, 0,
        "resumed session exited with {}",
        output2.exit_code
    );

    let session_id2 = extract_session_id_from_output(&output2);
    assert!(
        session_id2.is_some(),
        "resumed session should emit session_id"
    );

    // Stdout should contain some response text.
    assert!(
        !output2.stdout_lines.is_empty(),
        "resumed session should produce output"
    );
}

// ---------------------------------------------------------------------------
// Helpers for schema validation
// ---------------------------------------------------------------------------

fn parse_events(output: &rlph::process::ProcessOutput) -> Vec<Value> {
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

fn find_event<'a>(events: &'a [Value], ty: &str, subtype: Option<&str>) -> Option<&'a Value> {
    events.iter().find(|e| {
        e.get("type").and_then(Value::as_str) == Some(ty)
            && match subtype {
                Some(st) => e.get("subtype").and_then(Value::as_str) == Some(st),
                None => true,
            }
    })
}

fn find_events<'a>(events: &'a [Value], ty: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|e| e.get("type").and_then(Value::as_str) == Some(ty))
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

/// Assert a field exists and is an array.
fn assert_array(val: &Value, field: &str, ctx: &str) {
    assert!(
        val.get(field).and_then(Value::as_array).is_some(),
        "{ctx}: must have array field '{field}'"
    );
}

/// Assert a field exists and is a number.
fn assert_number(val: &Value, field: &str, ctx: &str) {
    assert!(
        val.get(field).and_then(Value::as_f64).is_some(),
        "{ctx}: must have number field '{field}'"
    );
}

/// Assert a field exists and is a bool.
fn assert_bool(val: &Value, field: &str, ctx: &str) {
    assert!(
        val.get(field).and_then(Value::as_bool).is_some(),
        "{ctx}: must have bool field '{field}'"
    );
}

// ---------------------------------------------------------------------------
// Schema validation: system/init
// ---------------------------------------------------------------------------

fn validate_system_init(events: &[Value]) -> &Value {
    let types = event_types(events);
    let init = find_event(events, "system", Some("init"))
        .unwrap_or_else(|| panic!("expected system/init event; got types: {types:?}"));

    assert_str(init, "session_id", "system/init");
    assert_array(init, "tools", "system/init");
    assert_str(init, "model", "system/init");
    assert_str(init, "cwd", "system/init");
    assert_str(init, "permissionMode", "system/init");
    assert_str(init, "claude_code_version", "system/init");
    assert_str(init, "uuid", "system/init");

    // tools should be an array of strings
    let tools = init["tools"].as_array().unwrap();
    assert!(!tools.is_empty(), "system/init: tools should not be empty");
    for tool in tools {
        assert!(
            tool.as_str().is_some(),
            "system/init: each tool must be a string"
        );
    }

    // mcp_servers is an array of objects with name + status
    if let Some(servers) = init.get("mcp_servers").and_then(Value::as_array) {
        for srv in servers {
            assert_str(srv, "name", "system/init mcp_server");
            assert_str(srv, "status", "system/init mcp_server");
        }
    }

    init
}

// ---------------------------------------------------------------------------
// Schema validation: assistant message
// ---------------------------------------------------------------------------

fn validate_assistant_event(event: &Value, ctx: &str) {
    assert_str(event, "session_id", ctx);
    assert_str(event, "uuid", ctx);
    // parent_tool_use_id is either null (top-level) or a string (sub-agent)
    assert!(
        event.get("parent_tool_use_id").is_some(),
        "{ctx}: must have parent_tool_use_id"
    );

    let msg = event
        .get("message")
        .unwrap_or_else(|| panic!("{ctx}: must have message object"));
    assert_eq!(
        msg.get("role").and_then(Value::as_str),
        Some("assistant"),
        "{ctx}: message.role must be 'assistant'"
    );
    assert_str(msg, "model", ctx);
    assert_str(msg, "id", ctx);
    assert_array(msg, "content", ctx);

    // Validate each content block
    let content = msg["content"].as_array().unwrap();
    for block in content {
        let block_type = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{ctx}: content block must have type"));

        match block_type {
            "text" => {
                assert!(
                    block.get("text").and_then(Value::as_str).is_some(),
                    "{ctx}: text block must have text string"
                );
            }
            "tool_use" => {
                assert_str(block, "id", &format!("{ctx} tool_use"));
                assert_str(block, "name", &format!("{ctx} tool_use"));
                assert!(
                    block.get("input").is_some(),
                    "{ctx}: tool_use block must have input"
                );
            }
            _ => {
                // Other content block types (e.g. thinking) — don't fail, just note
                eprintln!("{ctx}: unknown content block type '{block_type}'");
            }
        }
    }

    // usage must be present on the message
    assert!(msg.get("usage").is_some(), "{ctx}: message must have usage");
}

// ---------------------------------------------------------------------------
// Schema validation: user (tool_result) message
// ---------------------------------------------------------------------------

fn validate_user_event(event: &Value, ctx: &str) {
    assert_str(event, "session_id", ctx);
    assert_str(event, "uuid", ctx);
    assert!(
        event.get("parent_tool_use_id").is_some(),
        "{ctx}: must have parent_tool_use_id"
    );

    let msg = event
        .get("message")
        .unwrap_or_else(|| panic!("{ctx}: must have message object"));
    assert_eq!(
        msg.get("role").and_then(Value::as_str),
        Some("user"),
        "{ctx}: message.role must be 'user'"
    );
    assert_array(msg, "content", ctx);
}

// ---------------------------------------------------------------------------
// Schema validation: result event
// ---------------------------------------------------------------------------

fn validate_result_event(events: &[Value]) -> &Value {
    let types = event_types(events);
    let res = find_event(events, "result", None)
        .unwrap_or_else(|| panic!("expected result event; got types: {types:?}"));

    assert_str(res, "session_id", "result");
    assert_str(res, "uuid", "result");
    assert_bool(res, "is_error", "result");
    assert_number(res, "duration_ms", "result");
    assert_number(res, "duration_api_ms", "result");
    assert_number(res, "num_turns", "result");
    assert_number(res, "total_cost_usd", "result");

    // result field is a string (the final text output)
    assert!(
        res.get("result").and_then(Value::as_str).is_some(),
        "result: must have result string"
    );
    // subtype should be present (e.g. "success")
    assert_str(res, "subtype", "result");

    // usage and modelUsage are objects
    assert!(res.get("usage").is_some(), "result: must have usage object");
    assert!(
        res.get("modelUsage").is_some(),
        "result: must have modelUsage object"
    );

    res
}

// ---------------------------------------------------------------------------
// Schema validation: rate_limit_event
// ---------------------------------------------------------------------------

fn validate_rate_limit_event(event: &Value) {
    assert_str(event, "session_id", "rate_limit_event");
    assert_str(event, "uuid", "rate_limit_event");

    let info = event
        .get("rate_limit_info")
        .unwrap_or_else(|| panic!("rate_limit_event: must have rate_limit_info"));
    assert_str(info, "status", "rate_limit_info");
    assert_str(info, "rateLimitType", "rate_limit_info");
}

// ---------------------------------------------------------------------------
// Schema validation: system/task_started (sub-agent)
// ---------------------------------------------------------------------------

fn validate_task_started_event(event: &Value) {
    assert_str(event, "session_id", "system/task_started");
    assert_str(event, "uuid", "system/task_started");
    assert_str(event, "task_id", "system/task_started");
    assert_str(event, "tool_use_id", "system/task_started");
    assert_str(event, "description", "system/task_started");
    assert_str(event, "task_type", "system/task_started");
}

// ---------------------------------------------------------------------------
// Test: simple text response (system/init → assistant(text) → result)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_claude_stream_json_text_response_schema() {
    if !integration_enabled() {
        return;
    }

    let mut args = base_args();
    args.extend(["-p".to_string(), PROMPT.to_string()]);

    let Some(output) = run_claude_or_skip(args).await else {
        return;
    };
    assert_eq!(
        output.exit_code, 0,
        "claude exited with {}",
        output.exit_code
    );

    let events = parse_events(&output);
    assert!(!events.is_empty(), "expected JSON events on stdout");

    // Validate system/init
    validate_system_init(&events);

    // Validate assistant with text content
    let types = event_types(&events);
    let asst = find_event(&events, "assistant", None)
        .unwrap_or_else(|| panic!("expected assistant event; got types: {types:?}"));
    validate_assistant_event(asst, "assistant(text)");

    // For a simple prompt, the top-level assistant should have text content
    let content = asst["message"]["content"].as_array().unwrap();
    let has_text = content
        .iter()
        .any(|b| b.get("type").and_then(Value::as_str) == Some("text"));
    assert!(has_text, "simple response should have a text content block");

    // Top-level assistant should have null parent_tool_use_id
    assert!(
        asst["parent_tool_use_id"].is_null(),
        "top-level assistant should have null parent_tool_use_id"
    );

    // Validate result
    validate_result_event(&events);

    // Validate rate_limit_event if present (non-deterministic)
    if let Some(rl) = find_event(&events, "rate_limit_event", None) {
        validate_rate_limit_event(rl);
    }

    // Verify event ordering: system/init comes first, result comes last
    let first_type = events[0].get("type").and_then(Value::as_str);
    assert_eq!(
        first_type,
        Some("system"),
        "first event must be system/init"
    );
    let last_type = events.last().unwrap().get("type").and_then(Value::as_str);
    assert_eq!(last_type, Some("result"), "last event must be result");
}

// ---------------------------------------------------------------------------
// Test: tool use response (assistant(tool_use) → user(tool_result) → assistant(text))
// ---------------------------------------------------------------------------

const TOOL_TIMEOUT: Duration = Duration::from_secs(120);

#[tokio::test]
async fn test_claude_stream_json_tool_use_schema() {
    if !integration_enabled() {
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let token = format!("tool-test-token-{nonce}");
    let file_name = "must_read_for_tool_test.txt";
    fs::write(tmp.path().join(file_name), format!("{token}\n")).unwrap();

    let mut args = base_args();
    args.extend([
        "-p".to_string(),
        format!(
            "Read the file {file_name} and reply with exactly the token in that file. Do not guess."
        ),
    ]);

    let config = ProcessConfig {
        command: "claude".to_string(),
        args,
        working_dir: tmp.path().to_path_buf(),
        timeout: Some(TOOL_TIMEOUT),
        log_prefix: "test-claude-tool".to_string(),
        stream_output: false,
        env: vec![],
        stdin_data: None,
        quiet: true,
    };

    let output = match spawn_and_stream(config).await {
        Ok(output) => {
            if let Some(reason) = classify_claude_skip(&output.stdout_lines, &output.stderr_lines) {
                eprintln!("skipping: {reason}");
                return;
            }
            output
        }
        Err(Error::ProcessTimeout {
            stdout_lines,
            stderr_lines,
            ..
        }) => {
            if let Some(reason) = classify_claude_skip(&stdout_lines, &stderr_lines) {
                eprintln!("skipping: {reason}");
                return;
            }
            panic!("timed out; stdout={stdout_lines:?} stderr={stderr_lines:?}");
        }
        Err(err) => panic!("failed: {err:?}"),
    };

    assert_eq!(
        output.exit_code, 0,
        "claude exited with {}",
        output.exit_code
    );

    let events = parse_events(&output);
    let types = event_types(&events);

    // Must have system/init and result
    validate_system_init(&events);
    validate_result_event(&events);

    // Should have at least one assistant event with tool_use content
    let assistant_events = find_events(&events, "assistant");
    assert!(
        !assistant_events.is_empty(),
        "expected assistant events; got types: {types:?}"
    );

    let tool_use_assistant = assistant_events.iter().find(|e| {
        e["message"]["content"].as_array().is_some_and(|c| {
            c.iter()
                .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
        })
    });
    assert!(
        tool_use_assistant.is_some(),
        "expected an assistant event with tool_use content block; got types: {types:?}"
    );
    let tua = tool_use_assistant.unwrap();
    validate_assistant_event(tua, "assistant(tool_use)");

    // Extract the tool_use block and validate its shape
    let tool_use_block = tua["message"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
        .unwrap();
    let tool_use_id = tool_use_block["id"].as_str().unwrap();
    let tool_name = tool_use_block["name"].as_str().unwrap();
    assert!(
        !tool_use_id.is_empty(),
        "tool_use block must have non-empty id"
    );
    assert!(
        !tool_name.is_empty(),
        "tool_use block must have non-empty name"
    );

    // Should have a corresponding user event with tool_result
    let user_events = find_events(&events, "user");
    assert!(
        !user_events.is_empty(),
        "expected user (tool_result) events; got types: {types:?}"
    );

    let tool_result_user = user_events.iter().find(|e| {
        e["message"]["content"].as_array().is_some_and(|c| {
            c.iter().any(|b| {
                b.get("type").and_then(Value::as_str) == Some("tool_result")
                    && b.get("tool_use_id").and_then(Value::as_str) == Some(tool_use_id)
            })
        })
    });
    assert!(
        tool_result_user.is_some(),
        "expected user event with tool_result for tool_use_id '{tool_use_id}'"
    );
    let tru = tool_result_user.unwrap();
    validate_user_event(tru, "user(tool_result)");

    // The tool_result content block should reference the correct tool_use_id
    let result_block = tru["message"]["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        .unwrap();
    assert_eq!(
        result_block["tool_use_id"].as_str(),
        Some(tool_use_id),
        "tool_result must reference the correct tool_use_id"
    );

    // Should have a final assistant event with text content (the answer)
    let text_assistant = assistant_events.iter().find(|e| {
        e["message"]["content"].as_array().is_some_and(|c| {
            c.iter()
                .any(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        })
    });
    assert!(
        text_assistant.is_some(),
        "expected a final assistant event with text content"
    );
}

// ---------------------------------------------------------------------------
// Test: sub-agent response (system/task_started, nested parent_tool_use_id)
// ---------------------------------------------------------------------------

const SUBAGENT_TIMEOUT: Duration = Duration::from_secs(180);

#[tokio::test]
async fn test_claude_stream_json_subagent_schema() {
    if !integration_enabled() {
        return;
    }

    let mut args = base_args();
    args.extend([
        "-p".to_string(),
        "Use the Task tool with subagent_type Explore to find which file contains the main function. Be concise.".to_string(),
    ]);

    let config = ProcessConfig {
        command: "claude".to_string(),
        args,
        working_dir: working_dir(),
        timeout: Some(SUBAGENT_TIMEOUT),
        log_prefix: "test-claude-subagent".to_string(),
        stream_output: false,
        env: vec![],
        stdin_data: None,
        quiet: true,
    };

    let output = match spawn_and_stream(config).await {
        Ok(output) => {
            if let Some(reason) = classify_claude_skip(&output.stdout_lines, &output.stderr_lines) {
                eprintln!("skipping: {reason}");
                return;
            }
            output
        }
        Err(Error::ProcessTimeout {
            stdout_lines,
            stderr_lines,
            ..
        }) => {
            if let Some(reason) = classify_claude_skip(&stdout_lines, &stderr_lines) {
                eprintln!("skipping: {reason}");
                return;
            }
            panic!("timed out; stdout={stdout_lines:?} stderr={stderr_lines:?}");
        }
        Err(err) => panic!("failed: {err:?}"),
    };

    assert_eq!(
        output.exit_code, 0,
        "claude exited with {}",
        output.exit_code
    );

    let events = parse_events(&output);
    let types = event_types(&events);

    // Basic structure
    validate_system_init(&events);
    validate_result_event(&events);

    // Should have a system/task_started event
    let task_started = find_event(&events, "system", Some("task_started"));
    assert!(
        task_started.is_some(),
        "expected system/task_started event; got types: {types:?}"
    );
    validate_task_started_event(task_started.unwrap());

    let parent_tool_use_id = task_started.unwrap()["tool_use_id"].as_str().unwrap();

    // There should be assistant events with non-null parent_tool_use_id (sub-agent messages)
    let subagent_assistants: Vec<&Value> = find_events(&events, "assistant")
        .into_iter()
        .filter(|e| {
            e.get("parent_tool_use_id")
                .and_then(Value::as_str)
                .is_some()
        })
        .collect();
    assert!(
        !subagent_assistants.is_empty(),
        "expected sub-agent assistant events with non-null parent_tool_use_id"
    );

    // Validate each sub-agent assistant event
    for (i, sa) in subagent_assistants.iter().enumerate() {
        validate_assistant_event(sa, &format!("sub-agent assistant[{i}]"));
        assert_eq!(
            sa["parent_tool_use_id"].as_str(),
            Some(parent_tool_use_id),
            "sub-agent assistant must reference the Task tool_use_id"
        );
    }

    // Sub-agent may use a different model (e.g. haiku for Explore)
    let subagent_model = subagent_assistants[0]["message"]["model"].as_str().unwrap();
    let top_level_model = events
        .iter()
        .find(|e| {
            e.get("type").and_then(Value::as_str) == Some("assistant")
                && e["parent_tool_use_id"].is_null()
        })
        .and_then(|e| e["message"]["model"].as_str())
        .unwrap();
    eprintln!("top-level model: {top_level_model}, sub-agent model: {subagent_model}");

    // There should be user events with the same parent_tool_use_id (sub-agent tool results)
    let subagent_users: Vec<&Value> = find_events(&events, "user")
        .into_iter()
        .filter(|e| e.get("parent_tool_use_id").and_then(Value::as_str) == Some(parent_tool_use_id))
        .collect();
    assert!(
        !subagent_users.is_empty(),
        "expected sub-agent user events with parent_tool_use_id"
    );
    for (i, su) in subagent_users.iter().enumerate() {
        validate_user_event(su, &format!("sub-agent user[{i}]"));
    }

    // The final user event returning the Task result should have tool_use_result metadata
    let task_return = find_events(&events, "user")
        .into_iter()
        .filter(|e| e["parent_tool_use_id"].is_null())
        .find(|e| {
            e["message"]["content"].as_array().is_some_and(|c| {
                c.iter().any(|b| {
                    b.get("tool_use_id").and_then(Value::as_str) == Some(parent_tool_use_id)
                })
            })
        });
    assert!(
        task_return.is_some(),
        "expected a top-level user event returning the Task tool result"
    );
    let tr = task_return.unwrap();
    assert!(
        tr.get("tool_use_result").is_some(),
        "Task return user event should have tool_use_result metadata"
    );
}
