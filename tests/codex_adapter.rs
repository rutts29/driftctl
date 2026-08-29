#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn temporary_directory(case: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("driftctl-{case}-{}-{nonce}", std::process::id()));
    fs::create_dir(&path).expect("create isolated test directory");
    path
}

fn run(root: &Path, arguments: &[&str], environment: &[(&str, &Path)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_driftctl"));
    command.current_dir(root).args(arguments);
    for (name, value) in environment {
        command.env(name, value);
    }
    command.output().expect("run driftctl")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_executable(path: &Path, script: &str) {
    fs::write(path, script).expect("write fake Codex executable");
    let mut permissions = fs::metadata(path)
        .expect("read fake executable metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(path, permissions).expect("make fake Codex executable");
}

#[test]
fn codex_run_receives_the_current_frontier_and_captures_jsonl() {
    let root = temporary_directory("codex-adapter");
    let agents = root.join("AGENTS.md");
    let claude = root.join("CLAUDE.md");
    fs::write(&agents, "existing Codex instructions\n").expect("write AGENTS fixture");
    fs::write(&claude, "existing Claude instructions\n").expect("write CLAUDE fixture");

    let fake_codex = root.join("fake-codex");
    write_executable(
        &fake_codex,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$DRIFTCTL_FAKE_ARGS\"\nprintf '%s\\n' '{\"type\":\"thread.started\",\"thread_id\":\"fixture-thread\"}'\nprintf '%s\\n' '{\"type\":\"turn.started\"}'\nprintf '%s\\n' '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":2}}'\n",
    );
    let captured_arguments = root.join("codex-arguments.txt");

    assert_success(&run(
        &root,
        &[
            "start",
            "--goal",
            "Add retry support",
            "--requirement",
            "Retry once",
        ],
        &[],
    ));
    assert_success(&run(
        &root,
        &[
            "steer",
            "--requirement",
            "Do not retry authentication failures",
        ],
        &[],
    ));

    let output = run(
        &root,
        &["run", "codex"],
        &[
            ("DRIFTCTL_CODEX_BIN", &fake_codex),
            ("DRIFTCTL_FAKE_ARGS", &captured_arguments),
        ],
    );
    assert_success(&output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "trajectory: .driftctl/trajectories/run-0001.jsonl"
    );

    let arguments = fs::read_to_string(captured_arguments).expect("read captured Codex arguments");
    let argument_lines: Vec<&str> = arguments.lines().collect();
    assert_eq!(
        &argument_lines[..5],
        &[
            "exec",
            "--json",
            "--ephemeral",
            "--sandbox",
            "workspace-write",
        ]
    );
    let prompt = argument_lines[5..].join("\n");
    assert!(prompt.contains("Goal: Add retry support"));
    assert!(prompt.contains("R1: Retry once"));
    assert!(prompt.contains("R2: Do not retry authentication failures"));
    assert!(prompt.contains("Do not edit .driftctl"));
    assert!(prompt.contains("external evidence"));

    let trajectory = fs::read_to_string(root.join(".driftctl/trajectories/run-0001.jsonl"))
        .expect("read captured trajectory");
    let events: Vec<serde_json::Value> = trajectory
        .lines()
        .map(|line| serde_json::from_str(line).expect("trajectory line is JSON"))
        .collect();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["type"], "thread.started");
    assert_eq!(events[1]["type"], "turn.started");
    assert_eq!(events[2]["type"], "turn.completed");

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

#[test]
fn codex_terminal_event_must_match_the_process_exit() {
    let root = temporary_directory("codex-terminal-mismatch");
    fs::write(root.join("AGENTS.md"), "existing instructions\n").expect("write AGENTS fixture");
    let fake_codex = root.join("fake-codex");
    write_executable(
        &fake_codex,
        "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"thread.started\",\"thread_id\":\"fixture-thread\"}'\nprintf '%s\\n' '{\"type\":\"turn.started\"}'\nprintf '%s\\n' '{\"type\":\"turn.failed\",\"error\":{\"message\":\"fixture failure\"}}'\nexit 0\n",
    );

    assert_success(&run(
        &root,
        &[
            "start",
            "--goal",
            "Fix the fixture",
            "--requirement",
            "Make the check pass",
        ],
        &[],
    ));
    let output = run(
        &root,
        &["run", "codex"],
        &[("DRIFTCTL_CODEX_BIN", &fake_codex)],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not match"));
    assert!(root.join(".driftctl/trajectories/run-0001.jsonl").is_file());

    fs::remove_dir_all(root).expect("remove isolated test directory");
}
