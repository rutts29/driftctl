use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

fn temporary_directory(case: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("driftctl-{case}-{}-{nonce}", std::process::id()));
    fs::create_dir(&path).expect("create isolated test directory");
    path
}

fn run(root: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_driftctl"))
        .current_dir(root)
        .args(arguments)
        .output()
        .expect("run driftctl")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_resumes_steering_and_leaves_agent_instructions_unchanged() {
    let root = temporary_directory("cli-resume");
    let agents = root.join("AGENTS.md");
    let claude = root.join("CLAUDE.md");
    fs::write(&agents, "existing Codex instructions\n").expect("write AGENTS fixture");
    fs::write(&claude, "existing Claude instructions\n").expect("write CLAUDE fixture");

    assert_success(&run(
        &root,
        &[
            "start",
            "--goal",
            "Add retry support",
            "--requirement",
            "Retry once",
        ],
    ));
    let steer = run(
        &root,
        &[
            "steer",
            "--requirement",
            "Do not retry authentication failures",
        ],
    );
    assert_success(&steer);
    assert_eq!(String::from_utf8_lossy(&steer.stdout).trim(), "R2");

    assert_success(&run(
        &root,
        &[
            "satisfy",
            "--id",
            "R1",
            "--evidence",
            "retry unit test passes",
        ],
    ));

    let resumed = run(&root, &["resume", "--json"]);
    assert_success(&resumed);
    let resumed: Value = serde_json::from_slice(&resumed.stdout).expect("resume emits JSON");
    assert_eq!(resumed["goal"], "Add retry support");
    assert_eq!(resumed["unresolved"], serde_json::json!(["R2"]));
    assert_eq!(resumed["closed"], false);

    let blocked = run(&root, &["close"]);
    assert_eq!(blocked.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("R2"));

    assert_success(&run(
        &root,
        &[
            "satisfy",
            "--id",
            "R2",
            "--evidence",
            "integration test excludes authentication failures",
        ],
    ));
    let closed = run(&root, &["close"]);
    assert_success(&closed);
    assert_eq!(String::from_utf8_lossy(&closed.stdout).trim(), "verified");

    assert_eq!(
        fs::read_to_string(agents).expect("read AGENTS fixture"),
        "existing Codex instructions\n"
    );
    assert_eq!(
        fs::read_to_string(claude).expect("read CLAUDE fixture"),
        "existing Claude instructions\n"
    );

    fs::remove_dir_all(root).expect("remove isolated test directory");
}
