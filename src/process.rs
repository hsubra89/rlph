use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{info, warn};

use crate::error::{Error, Result};

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

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");

    let prefix_out = config.log_prefix.clone();
    let prefix_err = config.log_prefix;

    let stdout_task = tokio::spawn(async move {
        let mut lines = Vec::new();
        let mut reader = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            info!("[{prefix_out}] {line}");
            lines.push(line);
        }
        lines
    });

    let stderr_task = tokio::spawn(async move {
        let mut lines = Vec::new();
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            warn!("[{prefix_err}] {line}");
            lines.push(line);
        }
        lines
    });

    #[cfg(unix)]
    let signal_task = {
        let pgid = pid as i32;
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigint = signal(SignalKind::interrupt()).expect("SIGINT handler");
            let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
            loop {
                tokio::select! {
                    _ = sigint.recv() => {
                        unsafe { libc::killpg(pgid, libc::SIGINT); }
                    }
                    _ = sigterm.recv() => {
                        unsafe { libc::killpg(pgid, libc::SIGTERM); }
                    }
                }
            }
        })
    };

    let status = if let Some(dur) = config.timeout {
        match tokio::time::timeout(dur, child.wait()).await {
            Ok(r) => r.map_err(|e| Error::Process(format!("wait error: {e}")))?,
            Err(_) => {
                #[cfg(unix)]
                signal_task.abort();
                #[cfg(unix)]
                unsafe {
                    libc::killpg(pid as i32, libc::SIGTERM);
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                #[cfg(unix)]
                unsafe {
                    libc::killpg(pid as i32, libc::SIGKILL);
                }
                stdout_task.abort();
                stderr_task.abort();
                return Err(Error::Process(format!("process timed out after {dur:?}")));
            }
        }
    } else {
        child
            .wait()
            .await
            .map_err(|e| Error::Process(format!("wait error: {e}")))?
    };

    #[cfg(unix)]
    signal_task.abort();

    let stdout_lines = stdout_task
        .await
        .map_err(|e| Error::Process(format!("stdout reader failed: {e}")))?;
    let stderr_lines = stderr_task
        .await
        .map_err(|e| Error::Process(format!("stderr reader failed: {e}")))?;

    let (exit_code, signal) = extract_exit_info(&status);

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
