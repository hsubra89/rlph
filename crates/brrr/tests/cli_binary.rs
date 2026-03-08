use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

#[allow(deprecated)] // cargo_bin deprecated in favor of cargo_bin_cmd! macro
fn cmd() -> Command {
    Command::cargo_bin("brrr").unwrap()
}

fn cmd_in_tmp() -> (Command, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = cmd();
    c.current_dir(&tmp);
    (c, tmp)
}

fn parse_json_stderr(stderr: &[u8]) -> Vec<serde_json::Value> {
    let s = String::from_utf8_lossy(stderr);
    s.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|_| panic!("expected valid JSON: {l}")))
        .collect()
}

// --- Help & version ---

#[test]
fn help_flag() {
    cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("autonomous"));
}

#[test]
fn version_flag() {
    cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("brrr"));
}

#[test]
fn review_help() {
    cmd()
        .args(["review", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("PR_REF"));
}

#[test]
fn prd_help() {
    cmd()
        .args(["prd", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("DESCRIPTION"));
}

// --- Mode flag validation ---

#[test]
fn bare_brrr_requires_mode() {
    let (mut cmd, _tmp) = cmd_in_tmp();
    cmd.arg("build")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "specify one of --once, --continuous, or --max-iterations",
        ));
}

// --- Clap conflicts ---

#[test]
fn once_and_continuous_conflict() {
    let (mut cmd, _tmp) = cmd_in_tmp();
    cmd.args(["build", "--once", "--continuous"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

// --- Fix subcommand ---

#[test]
fn fix_help() {
    cmd()
        .args(["fix", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("PR_REF"))
        .stdout(predicate::str::contains("--dry-run"));
}

#[test]
fn fix_missing_pr_ref() {
    cmd()
        .arg("fix")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("PR_REF"));
}

// --- Missing required args ---

#[test]
fn review_missing_pr_number() {
    cmd()
        .arg("review")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("PR_REF"));
}

// --- Config validation ---

#[test]
fn unknown_source_rejected() {
    let (mut cmd, _tmp) = cmd_in_tmp();
    cmd.args(["build", "--once", "--source", "jira"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("unknown source: jira"));
}

#[test]
fn unknown_runner_rejected() {
    let (mut cmd, _tmp) = cmd_in_tmp();
    cmd.args(["build", "--once", "--runner", "foo"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("unknown runner: foo"));
}

#[test]
fn unknown_submission_rejected() {
    let (mut cmd, _tmp) = cmd_in_tmp();
    cmd.args(["build", "--once", "--submission", "gitlab"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("unknown submission: gitlab"));
}

#[test]
fn zero_poll_seconds_rejected() {
    let (mut cmd, _tmp) = cmd_in_tmp();
    cmd.args(["build", "--once", "--poll-seconds", "0"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("poll_seconds must be > 0"));
}

// --- Config file errors ---

#[test]
fn config_file_not_found() {
    let (mut cmd, _tmp) = cmd_in_tmp();
    cmd.args(["build", "--once", "--config", "/nonexistent.toml"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("config file not found"));
}

#[test]
fn invalid_toml_config() {
    let (mut cmd, tmp) = cmd_in_tmp();
    let cfg_dir = tmp.path().join(".brrr");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(cfg_dir.join("config.toml"), "not valid {{{{ toml").unwrap();
    cmd.args(["build", "--once"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("config parse error"));
}

// --- Review subcommand validation ---

#[test]
fn review_rejects_non_github_source() {
    let (mut cmd, tmp) = cmd_in_tmp();
    let cfg_dir = tmp.path().join(".brrr");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(
        cfg_dir.join("config.toml"),
        "source = \"linear\"\n[linear]\nteam = \"ENG\"\n",
    )
    .unwrap();
    cmd.args(["review", "123"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "supports only source = \"github\"",
        ));
}

// --- Init subcommand ---

#[test]
fn init_github_noop() {
    let (mut cmd, _tmp) = cmd_in_tmp();
    cmd.arg("init").assert().success();
}

#[test]
fn init_unknown_source_rejected() {
    let (mut cmd, _tmp) = cmd_in_tmp();
    cmd.args(["init", "--source", "jira"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("unknown source: jira"));
}

// --- Verbosity / log-format flags ---

#[test]
fn default_verbosity_shows_info() {
    let (mut cmd, _tmp) = cmd_in_tmp();
    cmd.arg("init")
        .assert()
        .success()
        .stderr(predicate::str::contains("INFO"));
}

#[test]
fn single_v_enables_debug() {
    let (mut cmd, _tmp) = cmd_in_tmp();
    cmd.args(["-v", "init"])
        .assert()
        .success()
        .stderr(predicate::str::contains("DEBUG"));
}

#[test]
fn double_v_enables_trace() {
    let (mut cmd, _tmp) = cmd_in_tmp();
    cmd.args(["-vv", "init"])
        .assert()
        .success()
        .stderr(predicate::str::contains("TRACE"));
}

#[test]
fn rust_log_overrides_cli_verbosity() {
    let (mut cmd, _tmp) = cmd_in_tmp();
    cmd.env("RUST_LOG", "error")
        .args(["-vv", "build", "--once", "--source", "jira"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("error: unknown source: jira"))
        .stderr(predicate::str::contains("INFO").not())
        .stderr(predicate::str::contains("DEBUG").not())
        .stderr(predicate::str::contains("TRACE").not());
}

#[test]
fn json_format_produces_valid_json() {
    let (mut cmd, _tmp) = cmd_in_tmp();
    let output = cmd
        .args(["--log-format", "json", "init"])
        .output()
        .expect("failed to run");

    assert!(output.status.success());
    let lines = parse_json_stderr(&output.stderr);
    assert!(!lines.is_empty(), "expected at least one JSON log line");
}

#[test]
fn verbose_json_combination() {
    let (mut cmd, _tmp) = cmd_in_tmp();
    let output = cmd
        .args(["-v", "--log-format", "json", "init"])
        .output()
        .expect("failed to run");

    assert!(output.status.success());
    let lines = parse_json_stderr(&output.stderr);
    let found_debug = lines
        .iter()
        .any(|v| v.get("level").and_then(|l| l.as_str()) == Some("DEBUG"));
    assert!(found_debug, "expected at least one DEBUG-level JSON line");
}

#[test]
fn text_format_explicit() {
    let (mut cmd, _tmp) = cmd_in_tmp();
    cmd.args(["--log-format", "text", "init"])
        .assert()
        .success()
        .stderr(predicate::str::contains("INFO"));
}

#[test]
fn help_shows_logging_flags() {
    cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--verbose"))
        .stdout(predicate::str::contains("--log-format"));
}
