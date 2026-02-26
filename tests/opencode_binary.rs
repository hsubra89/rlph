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

/// Extract `sessionID` from opencode JSON output lines.
///
/// Scans lines for JSON objects with a top-level `sessionID` field.
/// Returns the last one found.
fn extract_session_id(lines: &[String]) -> Option<String> {
    let mut last_id = None;
    for line in lines {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line)
            && let Some(id) = val.get("sessionID").and_then(|v| v.as_str())
            && !id.is_empty()
        {
            last_id = Some(id.to_string());
        }
    }
    last_id
}

fn base_args(prompt: &str) -> Vec<String> {
    vec![
        "run".to_string(),
        "--format".to_string(),
        "json".to_string(),
        prompt.to_string(),
    ]
}

fn config_with_args(args: Vec<String>) -> ProcessConfig {
    ProcessConfig {
        command: "opencode".to_string(),
        args,
        working_dir: working_dir(),
        timeout: Some(TIMEOUT),
        log_prefix: "test-opencode".to_string(),
        stream_output: false,
        env: vec![],
        stdin_data: None,
        quiet: true,
    }
}

#[tokio::test]
async fn test_opencode_emits_session_id() {
    if !integration_enabled() {
        return;
    }

    let args = base_args(PROMPT);

    let output = spawn_and_stream(config_with_args(args))
        .await
        .expect("opencode should complete successfully");

    assert_eq!(
        output.exit_code, 0,
        "opencode exited with {}",
        output.exit_code
    );

    let session_id = extract_session_id(&output.stdout_lines);
    assert!(
        session_id.is_some(),
        "expected sessionID in JSON output"
    );
    assert!(
        !session_id.unwrap().is_empty(),
        "sessionID should be non-empty"
    );
}

#[tokio::test]
async fn test_opencode_model_flag() {
    if !integration_enabled() {
        return;
    }

    let args = vec![
        "run".to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--model".to_string(),
        "anthropic/claude-haiku-4-5-20251001".to_string(),
        PROMPT.to_string(),
    ];

    let output = spawn_and_stream(config_with_args(args))
        .await
        .expect("opencode should complete successfully");

    assert_eq!(
        output.exit_code, 0,
        "opencode with --model flag exited with {}",
        output.exit_code
    );
}

#[tokio::test]
async fn test_opencode_variant_flag() {
    if !integration_enabled() {
        return;
    }

    let args = vec![
        "run".to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--variant".to_string(),
        "low".to_string(),
        PROMPT.to_string(),
    ];

    let output = spawn_and_stream(config_with_args(args))
        .await
        .expect("opencode should complete successfully");

    assert_eq!(
        output.exit_code, 0,
        "opencode with --variant flag exited with {}",
        output.exit_code
    );
}

#[tokio::test]
async fn test_opencode_resume_with_prompt() {
    if !integration_enabled() {
        return;
    }

    // First invocation: get a sessionID.
    let args1 = base_args("Say hello");

    let output1 = spawn_and_stream(config_with_args(args1))
        .await
        .expect("first opencode invocation should succeed");

    assert_eq!(output1.exit_code, 0);

    let session_id =
        extract_session_id(&output1.stdout_lines).expect("first invocation must emit sessionID");

    // Second invocation: resume with --session and a new prompt.
    let args2 = vec![
        "run".to_string(),
        "--format".to_string(),
        "json".to_string(),
        "--session".to_string(),
        session_id.clone(),
        "Now say goodbye".to_string(),
    ];

    let output2 = spawn_and_stream(config_with_args(args2))
        .await
        .expect("resumed opencode invocation should succeed");

    assert_eq!(
        output2.exit_code, 0,
        "resumed session exited with {}",
        output2.exit_code
    );

    let session_id2 = extract_session_id(&output2.stdout_lines);
    assert!(
        session_id2.is_some(),
        "resumed session should emit sessionID"
    );

    // Stdout should contain some response.
    assert!(
        !output2.stdout_lines.is_empty(),
        "resumed session should produce output"
    );
}
