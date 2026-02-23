use std::path::PathBuf;
use std::time::Duration;

use rlph::runner::{AgentRunner, CodexRunner, Phase};

/// Helper: creates a CodexRunner that invokes a bash script echoing its args and stdin.
fn mock_codex_runner(script: &str) -> (CodexRunner, PathBuf) {
    mock_codex_runner_with_retries(script, 0)
}

fn mock_codex_runner_with_retries(script: &str, retries: u32) -> (CodexRunner, PathBuf) {
    mock_codex_runner_with_timeout_and_retries(script, Duration::from_secs(10), retries)
}

fn mock_codex_runner_with_timeout_and_retries(
    script: &str,
    timeout: Duration,
    retries: u32,
) -> (CodexRunner, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join("mock_codex");
    std::fs::write(&script_path, format!("#!/bin/bash\n{script}")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let runner = CodexRunner::new(
        script_path.to_string_lossy().to_string(),
        None,
        Some(timeout),
        retries,
    );
    let path = tmp.path().to_path_buf();
    // Keep the tempdir alive so the mock script stays on disk.
    std::mem::forget(tmp);
    (runner, path)
}

#[tokio::test]
async fn test_codex_command_construction() {
    let runner = CodexRunner::new("codex".to_string(), Some("o3".to_string()), None, 0);
    let (cmd, args) = runner.build_command();
    assert_eq!(cmd, "codex");
    assert_eq!(args[0], "exec");
    assert_eq!(args[1], "--dangerously-bypass-approvals-and-sandbox");
    assert_eq!(args[2], "--model");
    assert_eq!(args[3], "o3");
    assert_eq!(args[4], "-");
}

#[tokio::test]
async fn test_codex_command_no_model() {
    let runner = CodexRunner::new("codex".to_string(), None, None, 0);
    let (cmd, args) = runner.build_command();
    assert_eq!(cmd, "codex");
    assert_eq!(
        args,
        vec!["exec", "--dangerously-bypass-approvals-and-sandbox", "-"]
    );
}

#[tokio::test]
async fn test_codex_prompt_via_stdin() {
    // Mock script reads from stdin and echoes it
    let (runner, tmp) = mock_codex_runner("cat");
    let result = runner
        .run(Phase::Implement, "hello prompt", tmp.as_ref())
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout, "hello prompt");
}

#[tokio::test]
async fn test_codex_exec_bypass_flags() {
    // Consume stdin first so prompt write does not race with early process exit.
    let (runner, tmp) = mock_codex_runner(r#"cat >/dev/null; echo "$@""#);
    let result = runner
        .run(Phase::Choose, "pick a task", tmp.as_ref())
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("exec"));
    assert!(
        result
            .stdout
            .contains("--dangerously-bypass-approvals-and-sandbox")
    );
    assert!(result.stdout.contains("-"));
}

#[tokio::test]
async fn test_codex_model_flag_passed() {
    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join("mock_codex");
    std::fs::write(&script_path, "#!/bin/bash\ncat >/dev/null\necho \"$@\"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let runner = CodexRunner::new(
        script_path.to_string_lossy().to_string(),
        Some("gpt-4o".to_string()),
        Some(Duration::from_secs(10)),
        0,
    );
    let result = runner
        .run(Phase::Implement, "do stuff", tmp.path())
        .await
        .unwrap();
    assert!(result.stdout.contains("--model"));
    assert!(result.stdout.contains("gpt-4o"));
}

#[tokio::test]
async fn test_codex_nonzero_exit_detected() {
    let (runner, tmp) = mock_codex_runner("exit 1");
    let err = runner
        .run(Phase::Implement, "fail", tmp.as_ref())
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("agent exited with code 1"),
        "expected 'agent exited with code 1', got: {msg}"
    );
}

#[tokio::test]
async fn test_codex_binary_not_found() {
    let runner = CodexRunner::new(
        "/nonexistent/codex_xyz".to_string(),
        None,
        Some(Duration::from_secs(5)),
        0,
    );
    let err = runner
        .run(Phase::Implement, "test", std::path::Path::new("."))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("failed to spawn"));
}

#[tokio::test]
#[cfg(unix)]
async fn test_codex_signal_propagation() {
    // Script sends SIGKILL to itself
    let (runner, tmp) = mock_codex_runner("kill -9 $$");
    let err = runner
        .run(Phase::Implement, "die", tmp.as_ref())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("agent killed by signal"));
}

#[tokio::test]
#[cfg(unix)]
async fn test_codex_timeout_retries_with_resume() {
    // Script: first invocation (has `-` in args) sleeps forever to trigger timeout.
    // Resume invocation (has `resume` in args) succeeds immediately.
    let script = r#"
if echo "$@" | grep -q "resume"; then
    echo "resumed ok"
    exit 0
fi
# First attempt: sleep to trigger timeout
sleep 60
"#;
    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join("mock_codex");
    std::fs::write(&script_path, format!("#!/bin/bash\n{script}")).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let runner = CodexRunner::new(
        script_path.to_string_lossy().to_string(),
        None,
        Some(Duration::from_secs(2)),
        2,
    );
    let work_dir = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    let result = runner
        .run(Phase::Implement, "do work", work_dir.as_ref())
        .await
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.contains("resumed ok"));
}

#[tokio::test]
#[cfg(unix)]
async fn test_codex_timeout_exhausts_retries() {
    // Script always sleeps — all attempts will timeout.
    let (runner, tmp) =
        mock_codex_runner_with_timeout_and_retries("sleep 60", Duration::from_millis(200), 2);
    let err = runner
        .run(Phase::Implement, "never finish", tmp.as_ref())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("agent timed out after 3 attempts"));
}
