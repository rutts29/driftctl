#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

fn temporary_directory(case: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "driftctl-verification-{case}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&path).expect("create fixture root");
    path
}

fn git(root: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git failed: {output:?}");
}

fn candidate(root: &Path) -> PathBuf {
    let candidate = root.join("candidate");
    fs::create_dir(&candidate).expect("create candidate");
    git(&candidate, &["init", "--quiet"]);
    git(
        &candidate,
        &["config", "user.email", "verification@example.invalid"],
    );
    git(&candidate, &["config", "user.name", "Verification"]);
    fs::write(candidate.join("tracked.txt"), "expected\n").expect("write candidate");
    git(&candidate, &["add", "tracked.txt"]);
    git(&candidate, &["commit", "--quiet", "-m", "checkpoint"]);
    candidate
}

fn script(root: &Path, name: &str, body: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).expect("write verifier");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
        .expect("make verifier executable");
    path
}

fn run(root: &Path, candidate: &Path, requirement: &str, verifier: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_driftctl"))
        .args([
            "verify",
            "--candidate",
            candidate.to_str().expect("candidate UTF-8"),
            "--requirement",
            requirement,
            "--json",
            "--",
            verifier.to_str().expect("verifier UTF-8"),
        ])
        .env("XDG_STATE_HOME", root.join("state"))
        .output()
        .expect("run verifier entrypoint")
}

#[test]
fn runs_one_external_requirement_check_and_keeps_raw_output_private() {
    let root = temporary_directory("pass");
    let candidate = candidate(&root);
    let verifier = script(
        &root,
        "pass.sh",
        "test \"$(cat tracked.txt)\" = expected\nprintf public-but-private-output\nprintf diagnostic >&2",
    );
    let output = run(&root, &candidate, "intent-validation-1", &verifier);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let document: Value = serde_json::from_slice(&output.stdout).expect("verification JSON");
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["requirement_id"], "intent-validation-1");
    assert_eq!(document["status"], "passed");
    assert_eq!(document["exit_code"], 0);
    assert_eq!(
        document["candidate_before_digest"],
        document["candidate_after_digest"]
    );
    let public = document.to_string();
    assert!(!public.contains("public-but-private-output"));
    assert!(!public.contains(&candidate.display().to_string()));
    assert!(!public.contains(&verifier.display().to_string()));

    let artifact_id = document["artifact_id"].as_str().expect("artifact ID");
    let artifact = root
        .join("state/driftctl/verification-artifacts")
        .join(artifact_id);
    assert_eq!(
        fs::read(artifact.join("stdout.bin")).unwrap(),
        b"public-but-private-output"
    );
    assert_eq!(
        fs::read(artifact.join("stderr.bin")).unwrap(),
        b"diagnostic"
    );
    for path in [
        artifact.join("command.json"),
        artifact.join("stdout.bin"),
        artifact.join("stderr.bin"),
    ] {
        assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
    }
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn distinguishes_command_failure_from_candidate_mutation_and_rejects_aggregate_ids() {
    let root = temporary_directory("failures");
    let candidate = candidate(&root);
    let failing = script(&root, "fail.sh", "exit 7");
    let failed = run(&root, &candidate, "intent-validation-2", &failing);
    assert_eq!(failed.status.code(), Some(2), "{failed:?}");
    let failed_document: Value = serde_json::from_slice(&failed.stdout).expect("failed JSON");
    assert_eq!(failed_document["status"], "failed");
    assert_eq!(failed_document["exit_code"], 7);

    let self_mutating = script(&root, "self-mutate.sh", "printf '# changed' >> \"$0\"");
    let protected = run(
        &root,
        &candidate,
        "intent-validation-protected",
        &self_mutating,
    );
    assert_eq!(protected.status.code(), Some(2), "{protected:?}");
    let protected_document: Value =
        serde_json::from_slice(&protected.stdout).expect("protected-input JSON");
    assert_eq!(protected_document["status"], "protected_input_changed");

    let mutating = script(&root, "mutate.sh", "printf changed > tracked.txt");
    let mutated = run(&root, &candidate, "intent-validation-3", &mutating);
    assert_eq!(mutated.status.code(), Some(2), "{mutated:?}");
    let mutated_document: Value = serde_json::from_slice(&mutated.stdout).expect("mutated JSON");
    assert_eq!(mutated_document["status"], "candidate_changed");
    assert_ne!(
        mutated_document["candidate_before_digest"],
        mutated_document["candidate_after_digest"]
    );

    let aggregate = Command::new(env!("CARGO_BIN_EXE_driftctl"))
        .args([
            "verify",
            "--candidate",
            candidate.to_str().unwrap(),
            "--requirement",
            "intent-a",
            "--requirement",
            "intent-b",
            "--json",
            "--",
            failing.to_str().unwrap(),
        ])
        .env("XDG_STATE_HOME", root.join("state"))
        .output()
        .expect("run aggregate rejection");
    assert_eq!(aggregate.status.code(), Some(1), "{aggregate:?}");
    assert!(String::from_utf8_lossy(&aggregate.stderr).contains("unsupported verify option"));

    let inside_candidate = script(&candidate, "inside-verifier.sh", "exit 0");
    let rejected = run(
        &root,
        &candidate,
        "intent-validation-inside",
        &inside_candidate,
    );
    assert_eq!(rejected.status.code(), Some(1), "{rejected:?}");
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("verifier inputs must remain outside the candidate")
    );
    assert_eq!(
        json!(failed_document["requirement_id"]),
        json!("intent-validation-2")
    );
    fs::remove_dir_all(root).expect("remove fixture");
}
