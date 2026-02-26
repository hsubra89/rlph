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
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line)
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
    let combined = stdout_lines
        .iter()
        .chain(stderr_lines.iter())
        .map(|line| line.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("\n");

    if combined.contains("command not found") || combined.contains("no such file or directory") {
        return Some("codex binary not found".to_string());
    }

    if combined.contains("api key") || combined.contains("authentication") {
        return Some("codex API key / auth not configured".to_string());
    }

    None
}

async fn run_codex_or_skip(args: Vec<String>, stdin_data: Option<String>) -> Option<ProcessOutput> {
    match spawn_and_stream(config_with_args(args, stdin_data)).await {
        Ok(output) => {
            if let Some(reason) =
                classify_codex_skip(&output.stdout_lines, &output.stderr_lines)
            {
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
// JSON event schema validation
// ---------------------------------------------------------------------------

fn parse_events(output: &ProcessOutput) -> Vec<Value> {
    output
        .stdout_lines
        .iter()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

#[tokio::test]
async fn test_codex_json_event_types() {
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
    assert!(!events.is_empty(), "expected JSON events on stdout");

    // thread_id must appear in at least one event
    let has_thread_id = events
        .iter()
        .any(|e| {
            e.get("thread_id")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
        });
    assert!(
        has_thread_id,
        "expected at least one event with non-empty thread_id"
    );

    // At least one item event with type "agent_message" and non-empty text
    let has_agent_message = events.iter().any(|e| {
        e.get("item").is_some_and(|item| {
            item.get("type").and_then(Value::as_str) == Some("agent_message")
                && item
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|t| !t.is_empty())
        })
    });
    assert!(
        has_agent_message,
        "expected at least one item event with type 'agent_message' and non-empty text; \
         event keys: {:?}",
        events
            .iter()
            .map(|e| e.as_object().map(|o| o.keys().collect::<Vec<_>>()))
            .collect::<Vec<_>>()
    );
}
