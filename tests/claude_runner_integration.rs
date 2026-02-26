use std::time::Duration;

use rlph::runner::{AgentRunner, ClaudeRunner, Phase};

/// Helper: creates a ClaudeRunner that invokes a bash script instead of the real Claude CLI.
///
/// Returns the `TempDir` so the caller keeps it alive for the duration of the test.
fn mock_claude_runner(script: &str) -> (ClaudeRunner, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join("mock_claude");
    std::fs::write(&script_path, format!("#!/bin/bash\n{script}")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let runner = ClaudeRunner::new(
        script_path.to_string_lossy().to_string(),
        None,
        None,
        Some(Duration::from_secs(10)),
        0,
    );
    (runner, tmp)
}

#[tokio::test]
async fn test_claude_stream_handler_runs_without_error() {
    // Mock script emits stream-json events to stderr (matching real Claude CLI
    // behaviour) and the result/session_id to stdout.
    let script = r#"
cat >&2 <<'EVENTS'
{"type":"content_block_start","content_block":{"type":"text","text":""}}
{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hello "}}
{"type":"content_block_delta","delta":{"type":"text_delta","text":"world"}}
{"type":"content_block_stop"}
{"type":"content_block_start","content_block":{"type":"tool_use","name":"Read","id":"tu_1"}}
{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{}"}}
{"type":"content_block_stop"}
EVENTS
cat <<'STDOUT'
{"type":"system","session_id":"sess-abc","subtype":"init"}
{"type":"result","result":"TASK_DONE","session_id":"sess-abc"}
STDOUT
"#;
    let (runner, tmp) = mock_claude_runner(script);
    let result = runner
        .run(Phase::Implement, "do stuff", tmp.path())
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout, "TASK_DONE");
    assert_eq!(result.session_id.as_deref(), Some("sess-abc"));
}

#[tokio::test]
async fn test_claude_stream_non_json_lines_ignored() {
    // Mix of non-JSON garbage and valid events on stderr — runner should not crash.
    // Result and session_id go to stdout.
    let script = r#"
echo "not json at all" >&2
echo "{" >&2
echo '{"type":"content_block_delta","delta":{"type":"text_delta","text":"ok"}}' >&2
echo '{"type":"result","result":"DONE","session_id":"sess-xyz"}'
"#;
    let (runner, tmp) = mock_claude_runner(script);
    let result = runner
        .run(Phase::Review, "review code", tmp.path())
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout, "DONE");
}

#[test]
fn test_claude_command_construction() {
    let runner = ClaudeRunner::new(
        "claude".to_string(),
        Some("opus".to_string()),
        Some("high".to_string()),
        None,
        0,
    );
    let (cmd, args) = runner.build_command("implement feature X");
    assert_eq!(cmd, "claude");
    assert!(args.contains(&"--print".to_string()));
    assert!(args.contains(&"--verbose".to_string()));
    assert!(args.contains(&"--output-format".to_string()));
    assert!(args.contains(&"stream-json".to_string()));
    assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
    assert!(args.contains(&"--model".to_string()));
    assert!(args.contains(&"opus".to_string()));
    assert!(args.contains(&"--effort".to_string()));
    assert!(args.contains(&"high".to_string()));
    assert!(args.contains(&"-p".to_string()));
    assert!(args.contains(&"implement feature X".to_string()));
}
