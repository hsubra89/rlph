use std::path::PathBuf;
use std::time::Duration;

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

#[tokio::test]
async fn test_claude_stream_json_event_types() {
    if !integration_enabled() {
        return;
    }

    let mut args = base_args();
    args.extend(["-p".to_string(), PROMPT.to_string()]);

    let Some(output) = run_claude_or_skip(args).await else {
        return;
    };

    assert_eq!(output.exit_code, 0, "claude exited with {}", output.exit_code);

    // Parse every stdout line as JSON; collect events by type.
    let events: Vec<Value> = output
        .stdout_lines
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect();

    assert!(!events.is_empty(), "expected at least one JSON event on stdout");

    let types: Vec<&str> = events
        .iter()
        .filter_map(|e| e.get("type").and_then(Value::as_str))
        .collect();

    // --- system init event ---
    let system_init = events.iter().find(|e| {
        e.get("type").and_then(Value::as_str) == Some("system")
            && e.get("subtype").and_then(Value::as_str) == Some("init")
    });
    assert!(system_init.is_some(), "expected system/init event; got types: {types:?}");
    let init = system_init.unwrap();
    assert!(
        init.get("session_id").and_then(Value::as_str).is_some_and(|s| !s.is_empty()),
        "system init must have non-empty session_id"
    );
    assert!(init.get("tools").and_then(Value::as_array).is_some(), "system init must have tools array");
    assert!(init.get("model").and_then(Value::as_str).is_some(), "system init must have model string");
    assert!(init.get("cwd").and_then(Value::as_str).is_some(), "system init must have cwd string");

    // --- assistant event ---
    let assistant = events.iter().find(|e| {
        e.get("type").and_then(Value::as_str) == Some("assistant")
    });
    assert!(assistant.is_some(), "expected assistant event; got types: {types:?}");
    let asst = assistant.unwrap();
    let msg = asst.get("message").expect("assistant event must have message");
    assert_eq!(
        msg.get("role").and_then(Value::as_str),
        Some("assistant"),
        "assistant message role must be 'assistant'"
    );
    assert!(
        msg.get("content").and_then(Value::as_array).is_some(),
        "assistant message must have content array"
    );

    // --- result event ---
    let result_event = events.iter().find(|e| {
        e.get("type").and_then(Value::as_str) == Some("result")
    });
    assert!(result_event.is_some(), "expected result event; got types: {types:?}");
    let res = result_event.unwrap();
    assert!(res.get("result").and_then(Value::as_str).is_some(), "result event must have result string");
    assert!(
        res.get("session_id").and_then(Value::as_str).is_some_and(|s| !s.is_empty()),
        "result event must have non-empty session_id"
    );
    assert!(res.get("is_error").and_then(Value::as_bool).is_some(), "result event must have is_error bool");
    assert!(
        res.get("duration_ms").and_then(Value::as_f64).is_some(),
        "result event must have duration_ms number"
    );
}
