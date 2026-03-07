use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

#[allow(deprecated)]
fn cmd() -> Command {
    Command::cargo_bin("brrr").unwrap()
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
    let tmp = tempfile::tempdir().unwrap();
    cmd()
        .current_dir(&tmp)
        .arg("build")
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
    let tmp = tempfile::tempdir().unwrap();
    cmd()
        .current_dir(&tmp)
        .args(["build", "--once", "--continuous"])
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
    let tmp = tempfile::tempdir().unwrap();
    cmd()
        .current_dir(&tmp)
        .args(["build", "--once", "--source", "jira"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("unknown source: jira"));
}

#[test]
fn unknown_runner_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    cmd()
        .current_dir(&tmp)
        .args(["build", "--once", "--runner", "foo"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("unknown runner: foo"));
}

#[test]
fn unknown_submission_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    cmd()
        .current_dir(&tmp)
        .args(["build", "--once", "--submission", "gitlab"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("unknown submission: gitlab"));
}

#[test]
fn zero_poll_seconds_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    cmd()
        .current_dir(&tmp)
        .args(["build", "--once", "--poll-seconds", "0"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("poll_seconds must be > 0"));
}

// --- Config file errors ---

#[test]
fn config_file_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    cmd()
        .current_dir(&tmp)
        .args(["build", "--once", "--config", "/nonexistent.toml"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("config file not found"));
}

#[test]
fn invalid_toml_config() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_dir = tmp.path().join(".brrr");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(cfg_dir.join("config.toml"), "not valid {{{{ toml").unwrap();
    cmd()
        .current_dir(&tmp)
        .args(["build", "--once"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("config parse error"));
}

// --- Review subcommand validation ---

#[test]
fn review_rejects_non_github_source() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_dir = tmp.path().join(".brrr");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(
        cfg_dir.join("config.toml"),
        "source = \"linear\"\n[linear]\nteam = \"ENG\"\n",
    )
    .unwrap();
    cmd()
        .current_dir(&tmp)
        .args(["review", "123"])
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
    let tmp = tempfile::tempdir().unwrap();
    cmd().current_dir(&tmp).arg("init").assert().success();
}

#[test]
fn init_unknown_source_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    cmd()
        .current_dir(&tmp)
        .args(["init", "--source", "jira"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("unknown source: jira"));
}
