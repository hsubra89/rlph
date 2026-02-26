use std::path::PathBuf;
use std::time::Duration;

use rlph::process::{ProcessConfig, spawn_and_stream};

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
        stderr_line_handler: None,
    }
}

#[tokio::test]
async fn test_codex_emits_thread_id() {
    if !integration_enabled() {
        return;
    }

    let mut args = base_args();
    args.push("-".to_string());

    let output = spawn_and_stream(config_with_args(args, Some(PROMPT.to_string())))
        .await
        .expect("codex should complete successfully");

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

    let output = spawn_and_stream(config_with_args(args, Some(PROMPT.to_string())))
        .await
        .expect("codex should complete successfully");

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

    let output = spawn_and_stream(config_with_args(args, Some(PROMPT.to_string())))
        .await
        .expect("codex should complete successfully");

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

    let output1 = spawn_and_stream(config_with_args(args1, Some("Say hello".to_string())))
        .await
        .expect("first codex invocation should succeed");

    assert_eq!(output1.exit_code, 0);

    let thread_id =
        extract_thread_id(&output1.stdout_lines).expect("first invocation must emit thread_id");

    // Second invocation: resume with a new prompt.
    let mut args2 = base_args();
    args2.extend(["resume".to_string(), thread_id.clone(), "-".to_string()]);

    let output2 = spawn_and_stream(config_with_args(args2, Some("Now say goodbye".to_string())))
        .await
        .expect("resumed codex invocation should succeed");

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
