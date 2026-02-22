use std::path::PathBuf;
use std::time::Duration;

use rlph::process::{ProcessConfig, spawn_and_stream};

fn make_config(command: &str, args: &[&str]) -> ProcessConfig {
    ProcessConfig {
        command: command.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        working_dir: PathBuf::from("."),
        timeout: None,
        log_prefix: "test".to_string(),
        env: vec![],
        stdin_data: None,
    }
}

#[tokio::test]
async fn test_stdout_streaming() {
    let config = make_config("bash", &["-c", "echo line1; echo line2; echo line3"]);
    let output = spawn_and_stream(config).await.unwrap();
    assert!(output.success());
    assert_eq!(output.exit_code, 0);
    assert_eq!(output.signal, None);
    assert_eq!(output.stdout_lines, vec!["line1", "line2", "line3"]);
}

#[tokio::test]
async fn test_stderr_streaming() {
    let config = make_config("bash", &["-c", "echo err1 >&2; echo err2 >&2"]);
    let output = spawn_and_stream(config).await.unwrap();
    assert!(output.success());
    assert_eq!(output.stderr_lines, vec!["err1", "err2"]);
}

#[tokio::test]
async fn test_mixed_stdout_stderr() {
    let config = make_config(
        "bash",
        &["-c", "echo out1; echo err1 >&2; echo out2; echo err2 >&2"],
    );
    let output = spawn_and_stream(config).await.unwrap();
    assert!(output.success());
    assert_eq!(output.stdout_lines, vec!["out1", "out2"]);
    assert_eq!(output.stderr_lines, vec!["err1", "err2"]);
}

#[tokio::test]
async fn test_nonzero_exit_code() {
    let config = make_config("bash", &["-c", "exit 42"]);
    let output = spawn_and_stream(config).await.unwrap();
    assert!(!output.success());
    assert_eq!(output.exit_code, 42);
    assert_eq!(output.signal, None);
}

#[tokio::test]
#[cfg(unix)]
async fn test_signal_killed() {
    // Process kills itself with SIGKILL
    let config = make_config("bash", &["-c", "kill -9 $$"]);
    let output = spawn_and_stream(config).await.unwrap();
    assert!(!output.success());
    assert_eq!(output.signal, Some(9));
}

#[tokio::test]
async fn test_timeout() {
    let mut config = make_config("sleep", &["30"]);
    config.timeout = Some(Duration::from_millis(200));
    let result = spawn_and_stream(config).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("timed out"));
}

#[tokio::test]
async fn test_spawn_failure() {
    let config = make_config("nonexistent_binary_xyz_123", &[]);
    let result = spawn_and_stream(config).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("failed to spawn"));
}

#[tokio::test]
async fn test_env_vars() {
    let mut config = make_config("bash", &["-c", "echo $RLPH_TEST_VAR"]);
    config.env = vec![("RLPH_TEST_VAR".to_string(), "hello_world".to_string())];
    let output = spawn_and_stream(config).await.unwrap();
    assert!(output.success());
    assert_eq!(output.stdout_lines, vec!["hello_world"]);
}

#[tokio::test]
#[cfg(unix)]
async fn test_sigint_to_child() {
    let pid_file = format!("/tmp/rlph_test_sigint_{}", std::process::id());
    let pid_file_clone = pid_file.clone();

    let config = ProcessConfig {
        command: "bash".to_string(),
        args: vec![
            "-c".to_string(),
            format!("echo $$ > {pid_file_clone}; exec sleep 30"),
        ],
        working_dir: PathBuf::from("."),
        timeout: Some(Duration::from_secs(10)),
        log_prefix: "test:sigint".to_string(),
        env: vec![],
        stdin_data: None,
    };

    let handle = tokio::spawn(spawn_and_stream(config));

    // Wait for child to start and write PID file
    let child_pid = {
        let mut pid = None;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Ok(content) = std::fs::read_to_string(&pid_file)
                && let Ok(p) = content.trim().parse::<i32>()
            {
                pid = Some(p);
                break;
            }
        }
        pid.expect("child should write PID file")
    };

    // Send SIGINT to child process
    unsafe {
        libc::kill(child_pid, libc::SIGINT);
    }

    let output = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("should complete within 5s")
        .expect("task should not panic")
        .expect("spawn should succeed");

    assert!(!output.success());
    assert_eq!(output.signal, Some(libc::SIGINT));

    let _ = std::fs::remove_file(&pid_file);
}

#[tokio::test]
#[cfg(unix)]
async fn test_double_sigint_force_exit() {
    // Spawn a process that traps SIGINT and refuses to die
    let config = ProcessConfig {
        command: "bash".to_string(),
        args: vec!["-c".to_string(), "trap '' INT TERM; sleep 60".to_string()],
        working_dir: PathBuf::from("."),
        timeout: None,
        log_prefix: "test:double-sigint".to_string(),
        env: vec![],
        stdin_data: None,
    };

    let handle = tokio::spawn(spawn_and_stream(config));

    // Give child time to start and install trap
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Send SIGINT to ourselves (triggers first Ctrl-C path)
    unsafe {
        libc::kill(libc::getpid(), libc::SIGINT);
    }

    // Brief pause then second SIGINT (force exit path)
    tokio::time::sleep(Duration::from_millis(100)).await;
    unsafe {
        libc::kill(libc::getpid(), libc::SIGINT);
    }

    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("should complete within 5s")
        .expect("task should not panic");

    assert!(result.is_err(), "double SIGINT should return Err");
}

#[tokio::test]
#[cfg(unix)]
async fn test_timeout_kills_descendants() {
    let pid_file = format!("/tmp/rlph_timeout_descendant_{}.pid", std::process::id());
    let pid_file_clone = pid_file.clone();

    // Child shell ignores TERM and waits; its background child should not survive timeout cleanup.
    let config = ProcessConfig {
        command: "bash".to_string(),
        args: vec![
            "-c".to_string(),
            format!("sleep 30 & echo $! > {pid_file_clone}; trap '' TERM; wait"),
        ],
        working_dir: PathBuf::from("."),
        timeout: Some(Duration::from_millis(200)),
        log_prefix: "test:timeout-descendants".to_string(),
        env: vec![],
        stdin_data: None,
    };

    let result = spawn_and_stream(config).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("timed out"), "unexpected error: {err}");

    let mut descendant_pid = None;
    for _ in 0..50 {
        if let Ok(content) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = content.trim().parse::<i32>()
        {
            descendant_pid = Some(pid);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let descendant_pid = descendant_pid.expect("child should write descendant pid file");

    // SAFETY: kill(pid, 0) only checks for process existence.
    let still_alive = unsafe { libc::kill(descendant_pid, 0) == 0 };
    if still_alive {
        // SAFETY: best-effort cleanup for leaked process from the test.
        unsafe {
            libc::kill(descendant_pid, libc::SIGKILL);
        }
    }
    let _ = std::fs::remove_file(&pid_file);

    assert!(
        !still_alive,
        "descendant process {descendant_pid} survived timeout cleanup"
    );
}

#[tokio::test]
async fn test_stdout_with_output_before_failure() {
    let config = make_config("bash", &["-c", "echo before_fail; exit 1"]);
    let output = spawn_and_stream(config).await.unwrap();
    assert!(!output.success());
    assert_eq!(output.exit_code, 1);
    assert_eq!(output.stdout_lines, vec!["before_fail"]);
}

#[tokio::test]
async fn test_stdin_data() {
    let config = ProcessConfig {
        command: "bash".to_string(),
        args: vec!["-c".to_string(), "cat".to_string()],
        working_dir: PathBuf::from("."),
        timeout: None,
        log_prefix: "test:stdin".to_string(),
        env: vec![],
        stdin_data: Some("hello from stdin".to_string()),
    };
    let output = spawn_and_stream(config).await.unwrap();
    assert!(output.success());
    assert_eq!(output.stdout_lines, vec!["hello from stdin"]);
}

#[tokio::test]
async fn test_stdin_data_multiline() {
    let config = ProcessConfig {
        command: "bash".to_string(),
        args: vec!["-c".to_string(), "cat".to_string()],
        working_dir: PathBuf::from("."),
        timeout: None,
        log_prefix: "test:stdin-multi".to_string(),
        env: vec![],
        stdin_data: Some("line1\nline2\nline3".to_string()),
    };
    let output = spawn_and_stream(config).await.unwrap();
    assert!(output.success());
    assert_eq!(output.stdout_lines, vec!["line1", "line2", "line3"]);
}
