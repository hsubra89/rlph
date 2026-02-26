use std::path::PathBuf;
use std::time::Duration;

use rlph::process::{ProcessConfig, spawn_and_stream};
use rlph::runner::extract_session_id;

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

#[tokio::test]
async fn test_claude_emits_session_id() {
    if !integration_enabled() {
        return;
    }

    let mut args = base_args();
    args.extend(["-p".to_string(), PROMPT.to_string()]);

    let output = spawn_and_stream(config_with_args(args, None))
        .await
        .expect("claude should complete successfully");

    assert_eq!(output.exit_code, 0, "claude exited with {}", output.exit_code);

    let session_id = extract_session_id(&output.stdout_lines);
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

    let output = spawn_and_stream(config_with_args(args, None))
        .await
        .expect("claude should complete successfully");

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

    let output = spawn_and_stream(config_with_args(args, None))
        .await
        .expect("claude should complete successfully");

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

    let output1 = spawn_and_stream(config_with_args(args1, None))
        .await
        .expect("first claude invocation should succeed");

    assert_eq!(output1.exit_code, 0);

    let session_id = extract_session_id(&output1.stdout_lines)
        .expect("first invocation must emit session_id");

    // Second invocation: resume with a new prompt.
    let mut args2 = base_args();
    args2.extend([
        "--resume".to_string(),
        session_id.clone(),
        "-p".to_string(),
        "Now say goodbye".to_string(),
    ]);

    let output2 = spawn_and_stream(config_with_args(args2, None))
        .await
        .expect("resumed claude invocation should succeed");

    assert_eq!(
        output2.exit_code, 0,
        "resumed session exited with {}",
        output2.exit_code
    );

    let session_id2 = extract_session_id(&output2.stdout_lines);
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
