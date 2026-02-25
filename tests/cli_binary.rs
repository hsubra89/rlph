use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

fn integration_enabled() -> bool {
    std::env::var("RLPH_INTEGRATION").is_ok()
}

#[allow(deprecated)]
fn cmd() -> Command {
    Command::cargo_bin("rlph").unwrap()
}

// --- Help & version ---

#[test]
fn help_flag() {
    if !integration_enabled() {
        return;
    }
    cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("autonomous"));
}

#[test]
fn version_flag() {
    if !integration_enabled() {
        return;
    }
    cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("rlph"));
}

#[test]
fn review_help() {
    if !integration_enabled() {
        return;
    }
    cmd()
        .args(["review", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("PR_NUMBER"));
}

#[test]
fn prd_help() {
    if !integration_enabled() {
        return;
    }
    cmd()
        .args(["prd", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("DESCRIPTION"));
}

// --- Mode flag validation ---

#[test]
fn bare_rlph_requires_mode() {
    if !integration_enabled() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    cmd()
        .current_dir(&tmp)
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
    if !integration_enabled() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    cmd()
        .current_dir(&tmp)
        .args(["--once", "--continuous"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

// --- Missing required args ---

#[test]
fn review_missing_pr_number() {
    if !integration_enabled() {
        return;
    }
    cmd()
        .arg("review")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("PR_NUMBER"));
}

// --- Config validation ---

#[test]
fn unknown_source_rejected() {
    if !integration_enabled() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    cmd()
        .current_dir(&tmp)
        .args(["--once", "--source", "jira"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("unknown source: jira"));
}

#[test]
fn unknown_runner_rejected() {
    if !integration_enabled() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    cmd()
        .current_dir(&tmp)
        .args(["--once", "--runner", "foo"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("unknown runner: foo"));
}

#[test]
fn unknown_submission_rejected() {
    if !integration_enabled() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    cmd()
        .current_dir(&tmp)
        .args(["--once", "--submission", "gitlab"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("unknown submission: gitlab"));
}

#[test]
fn zero_poll_seconds_rejected() {
    if !integration_enabled() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    cmd()
        .current_dir(&tmp)
        .args(["--once", "--poll-seconds", "0"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("poll_seconds must be > 0"));
}

// --- Config file errors ---

#[test]
fn config_file_not_found() {
    if !integration_enabled() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    cmd()
        .current_dir(&tmp)
        .args(["--once", "--config", "/nonexistent.toml"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("config file not found"));
}

#[test]
fn invalid_toml_config() {
    if !integration_enabled() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let cfg_dir = tmp.path().join(".rlph");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(cfg_dir.join("config.toml"), "not valid {{{{ toml").unwrap();
    cmd()
        .current_dir(&tmp)
        .arg("--once")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("config parse error"));
}

// --- Review subcommand validation ---

#[test]
fn review_rejects_non_github_source() {
    if !integration_enabled() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let cfg_dir = tmp.path().join(".rlph");
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
    if !integration_enabled() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    cmd().current_dir(&tmp).arg("init").assert().success();
}

#[test]
fn init_unknown_source_rejected() {
    if !integration_enabled() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    cmd()
        .current_dir(&tmp)
        .args(["init", "--source", "jira"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("unknown source: jira"));
}
