use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::error::{Error, Result};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const INTERRUPT_GRACE: Duration = Duration::from_secs(2);
const TIMEOUT_GRACE: Duration = Duration::from_millis(500);

/// Configuration for spawning a child process.
#[derive(Debug, Clone)]
pub struct ProcessConfig {
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    pub timeout: Option<Duration>,
    pub log_prefix: String,
    pub env: Vec<(String, String)>,
}

/// Output from a completed child process.
#[derive(Debug)]
pub struct ProcessOutput {
    pub exit_code: i32,
    pub signal: Option<i32>,
    pub stdout_lines: Vec<String>,
    pub stderr_lines: Vec<String>,
}

impl ProcessOutput {
    pub fn success(&self) -> bool {
        self.exit_code == 0 && self.signal.is_none()
    }
}

/// Spawn a child process, stream its output line-by-line, and handle signals.
///
/// The child is placed in its own process group on Unix. SIGINT and SIGTERM
/// received by the parent are forwarded to the child's process group.
pub async fn spawn_and_stream(config: ProcessConfig) -> Result<ProcessOutput> {
    let started_at = Instant::now();
    let mut cmd = Command::new(&config.command);
    cmd.args(&config.args)
        .current_dir(&config.working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    for (key, value) in &config.env {
        cmd.env(key, value);
    }

    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Process(format!("failed to spawn '{}': {e}", config.command)))?;

    let pid = child
        .id()
        .ok_or_else(|| Error::Process("child has no pid".into()))?;
    let command_preview = format_command_preview(&config.command, &config.args);
    let log_prefix = config.log_prefix.clone();
    info!(
        "[{}] started pid={} command={command_preview}",
        log_prefix, pid
    );
    eprintln!("[{}] started (pid {pid}): {command_preview}", log_prefix);

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");

    let prefix_out = log_prefix.clone();
    let prefix_err = log_prefix.clone();

    let stdout_task = tokio::spawn(async move {
        let mut lines = Vec::new();
        let mut reader = BufReader::new(stdout).lines();
        loop {
            match reader.next_line().await {
                Ok(Some(line)) => {
                    println!("[{prefix_out}] {line}");
                    lines.push(line);
                }
                Ok(None) => break,
                Err(e) => {
                    warn!("[{prefix_out}] stdout read failed: {e}");
                    break;
                }
            }
        }
        lines
    });

    let stderr_task = tokio::spawn(async move {
        let mut lines = Vec::new();
        let mut reader = BufReader::new(stderr).lines();
        loop {
            match reader.next_line().await {
                Ok(Some(line)) => {
                    eprintln!("[{prefix_err}] {line}");
                    lines.push(line);
                }
                Ok(None) => break,
                Err(e) => {
                    warn!("[{prefix_err}] stderr read failed: {e}");
                    break;
                }
            }
        }
        lines
    });

    let heartbeat_prefix = log_prefix.clone();
    let heartbeat_started = started_at;
    let heartbeat_task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(HEARTBEAT_INTERVAL).await;
            let elapsed = heartbeat_started.elapsed().as_secs();
            eprintln!("[{heartbeat_prefix}] still running ({elapsed}s elapsed)");
        }
    });

    let mut wait_task = tokio::spawn(async move { child.wait().await });

    #[cfg(unix)]
    let status_result =
        wait_for_exit_unix(config.timeout, pid as i32, &log_prefix, &mut wait_task).await;
    #[cfg(not(unix))]
    let status_result = wait_for_exit_non_unix(config.timeout, &mut wait_task).await;

    heartbeat_task.abort();

    let status = match status_result {
        Ok(status) => status,
        Err(e) => {
            stdout_task.abort();
            stderr_task.abort();
            return Err(e);
        }
    };

    let stdout_lines = stdout_task
        .await
        .map_err(|e| Error::Process(format!("stdout reader failed: {e}")))?;
    let stderr_lines = stderr_task
        .await
        .map_err(|e| Error::Process(format!("stderr reader failed: {e}")))?;

    let (exit_code, signal) = extract_exit_info(&status);
    info!(
        "[{}] completed in {}s (exit_code={exit_code}, signal={signal:?})",
        log_prefix,
        started_at.elapsed().as_secs()
    );

    Ok(ProcessOutput {
        exit_code,
        signal,
        stdout_lines,
        stderr_lines,
    })
}

fn extract_exit_info(status: &std::process::ExitStatus) -> (i32, Option<i32>) {
    if let Some(code) = status.code() {
        return (code, None);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return (128 + sig, Some(sig));
        }
    }
    (-1, None)
}

fn format_command_preview(command: &str, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(shellish_quote(command));
    parts.extend(args.iter().map(|arg| shellish_quote(arg)));
    parts.join(" ")
}

fn shellish_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-:=+".contains(c))
    {
        value.to_string()
    } else {
        format!("{value:?}")
    }
}

fn wait_join_result(
    result: std::result::Result<std::io::Result<ExitStatus>, tokio::task::JoinError>,
) -> Result<ExitStatus> {
    let status = result.map_err(|e| Error::Process(format!("wait task failed: {e}")))?;
    status.map_err(|e| Error::Process(format!("wait error: {e}")))
}

#[cfg(unix)]
async fn wait_for_exit_unix(
    timeout: Option<Duration>,
    process_group_id: i32,
    log_prefix: &str,
    wait_task: &mut JoinHandle<std::io::Result<ExitStatus>>,
) -> Result<ExitStatus> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigint = signal(SignalKind::interrupt())
        .map_err(|e| Error::Process(format!("failed to install SIGINT handler: {e}")))?;
    let mut sigterm = signal(SignalKind::terminate())
        .map_err(|e| Error::Process(format!("failed to install SIGTERM handler: {e}")))?;

    if let Some(dur) = timeout {
        let timer = tokio::time::sleep(dur);
        tokio::pin!(timer);

        tokio::select! {
            result = &mut *wait_task => wait_join_result(result),
            _ = &mut timer => handle_timeout_unix(process_group_id, log_prefix, dur, wait_task).await,
            signal = sigint.recv() => {
                if signal.is_some() {
                    handle_interrupt_unix(process_group_id, log_prefix, libc::SIGINT, "SIGINT", wait_task).await
                } else {
                    wait_join_result((&mut *wait_task).await)
                }
            }
            signal = sigterm.recv() => {
                if signal.is_some() {
                    handle_interrupt_unix(process_group_id, log_prefix, libc::SIGTERM, "SIGTERM", wait_task).await
                } else {
                    wait_join_result((&mut *wait_task).await)
                }
            }
        }
    } else {
        tokio::select! {
            result = &mut *wait_task => wait_join_result(result),
            signal = sigint.recv() => {
                if signal.is_some() {
                    handle_interrupt_unix(process_group_id, log_prefix, libc::SIGINT, "SIGINT", wait_task).await
                } else {
                    wait_join_result((&mut *wait_task).await)
                }
            }
            signal = sigterm.recv() => {
                if signal.is_some() {
                    handle_interrupt_unix(process_group_id, log_prefix, libc::SIGTERM, "SIGTERM", wait_task).await
                } else {
                    wait_join_result((&mut *wait_task).await)
                }
            }
        }
    }
}

#[cfg(not(unix))]
async fn wait_for_exit_non_unix(
    timeout: Option<Duration>,
    wait_task: &mut JoinHandle<std::io::Result<ExitStatus>>,
) -> Result<ExitStatus> {
    if let Some(dur) = timeout {
        match tokio::time::timeout(dur, &mut *wait_task).await {
            Ok(result) => wait_join_result(result),
            Err(_) => {
                wait_task.abort();
                Err(Error::Process(format!("process timed out after {dur:?}")))
            }
        }
    } else {
        wait_join_result((&mut *wait_task).await)
    }
}

#[cfg(unix)]
async fn handle_interrupt_unix(
    process_group_id: i32,
    log_prefix: &str,
    signal: i32,
    signal_name: &str,
    wait_task: &mut JoinHandle<std::io::Result<ExitStatus>>,
) -> Result<ExitStatus> {
    warn!("[{log_prefix}] received {signal_name}; forwarding to process group {process_group_id}");
    eprintln!("[{log_prefix}] received {signal_name}; stopping child process...");
    send_signal_to_process_group(process_group_id, signal, log_prefix, signal_name);

    match tokio::time::timeout(INTERRUPT_GRACE, &mut *wait_task).await {
        Ok(result) => wait_join_result(result),
        Err(_) => {
            warn!(
                "[{log_prefix}] process group {process_group_id} ignored {signal_name}; sending SIGKILL"
            );
            send_signal_to_process_group(process_group_id, libc::SIGKILL, log_prefix, "SIGKILL");
            wait_join_result((&mut *wait_task).await)
        }
    }
}

#[cfg(unix)]
async fn handle_timeout_unix(
    process_group_id: i32,
    log_prefix: &str,
    timeout: Duration,
    wait_task: &mut JoinHandle<std::io::Result<ExitStatus>>,
) -> Result<ExitStatus> {
    warn!("[{log_prefix}] process timed out after {timeout:?}; sending SIGTERM");
    send_signal_to_process_group(process_group_id, libc::SIGTERM, log_prefix, "SIGTERM");

    match tokio::time::timeout(TIMEOUT_GRACE, &mut *wait_task).await {
        Ok(result) => {
            let _ = wait_join_result(result)?;
        }
        Err(_) => {
            warn!(
                "[{log_prefix}] process group {process_group_id} ignored SIGTERM; sending SIGKILL"
            );
            send_signal_to_process_group(process_group_id, libc::SIGKILL, log_prefix, "SIGKILL");
            let _ = wait_join_result((&mut *wait_task).await)?;
        }
    }

    Err(Error::Process(format!(
        "process timed out after {timeout:?}"
    )))
}

#[cfg(unix)]
fn send_signal_to_process_group(
    process_group_id: i32,
    signal: i32,
    log_prefix: &str,
    signal_name: &str,
) {
    // SAFETY: libc::killpg is an FFI call that does not dereference pointers.
    let rc = unsafe { libc::killpg(process_group_id, signal) };
    if rc == 0 {
        return;
    }

    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        return;
    }

    warn!("[{log_prefix}] failed to send {signal_name} to process group {process_group_id}: {err}");
}
