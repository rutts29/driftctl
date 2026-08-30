use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

fn temporary_directory(case: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("driftctl-{case}-{}-{nonce}", std::process::id()));
    fs::create_dir(&path).expect("create isolated test directory");
    path
}

fn run(root: &Path, arguments: &[&str], environment: &BTreeMap<&str, String>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_driftctl"));
    command.current_dir(root).args(arguments);
    for (name, value) in environment {
        command.env(name, value);
    }
    command.output().expect("run driftctl")
}

fn git(root: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git command failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn clean_repository(root: &Path) {
    git(root, &["init", "--quiet"]);
    git(
        root,
        &["config", "user.email", "driftctl-test@example.invalid"],
    );
    git(root, &["config", "user.name", "driftctl test"]);
    fs::write(root.join("README.md"), "source repository\n").expect("write source fixture");
    git(root, &["add", "README.md"]);
    git(root, &["commit", "--quiet", "-m", "source fixture"]);
}

fn write_fake_app_server(root: &Path) -> PathBuf {
    let program = root.join("fake-codex.py");
    fs::write(
        &program,
        r#"#!/usr/bin/env python3
import json
import os
import sys

capture = os.environ["DRIFTCTL_FAKE_CAPTURE"]
for raw in sys.stdin:
    request = json.loads(raw)
    with open(capture, "a", encoding="utf-8") as file:
        file.write(json.dumps(request, sort_keys=True) + "\n")
    method = request.get("method")
    if method == "initialized":
        continue
    if method == "initialize":
        response = {"id": request["id"], "result": {"userAgent": "fake", "codexHome": "/not-used", "platformFamily": "unix", "platformOs": "linux"}}
    elif method == "thread/list":
        pages = json.loads(os.environ["DRIFTCTL_FAKE_LIST_PAGES"])
        page = 0 if request["params"].get("cursor") is None else 1
        response = {"id": request["id"], "result": pages[page]}
    elif method == "thread/read":
        if os.environ.get("DRIFTCTL_FAKE_TRUNCATE") == "1":
            sys.stdout.write('{"id":' + str(request["id"]))
            sys.stdout.flush()
            break
        response = {"id": request["id"], "result": json.loads(os.environ["DRIFTCTL_FAKE_READ"])}
    else:
        response = {"id": request["id"], "error": {"code": -32601, "message": "unexpected method"}}
    sys.stdout.write(json.dumps(response) + "\n")
    sys.stdout.flush()
"#,
    )
    .expect("write fake Codex");
    let mut permissions = fs::metadata(&program).expect("fake metadata").permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
    }
    fs::set_permissions(&program, permissions).expect("make fake Codex executable");
    program
}

fn thread(id: &str, cwd: &str, recency_at: i64) -> Value {
    json!({
        "id": id,
        "cwd": cwd,
        "recencyAt": recency_at,
        "updatedAt": recency_at,
    })
}

fn read_result(id: &str, cwd: &str) -> Value {
    json!({
        "thread": {
            "id": id,
            "cwd": cwd,
            "turns": [{
                "items": [
                    {"type": "userMessage", "id": "user-1", "content": [
                        {"type": "text", "text": "private user intent\r\nmust not be echoed"},
                        {"type": "image", "url": "https://example.invalid/private.png"}
                    ]},
                    {"type": "agentMessage", "id": "assistant-1", "text": "private assistant draft must not be authority"},
                    {"type": "userMessage", "id": "user-2", "content": [
                        {"type": "text", "text": "latest explicit steering"}
                    ]}
                ]
            }]
        }
    })
}

fn fake_environment(
    source: &Path,
    list_pages: Vec<Value>,
    read: Value,
    truncate: bool,
) -> (BTreeMap<&'static str, String>, PathBuf) {
    let fake_root = temporary_directory("fake-app-server");
    let fake_codex = write_fake_app_server(&fake_root);
    let capture = fake_root.join("requests.jsonl");
    let mut environment = BTreeMap::new();
    environment.insert("DRIFTCTL_CODEX_BIN", fake_codex.display().to_string());
    environment.insert("DRIFTCTL_FAKE_CAPTURE", capture.display().to_string());
    environment.insert(
        "DRIFTCTL_FAKE_LIST_PAGES",
        Value::Array(list_pages).to_string(),
    );
    environment.insert("DRIFTCTL_FAKE_READ", read.to_string());
    if truncate {
        environment.insert("DRIFTCTL_FAKE_TRUNCATE", "1".to_owned());
    }
    assert!(source.is_dir());
    (environment, capture)
}

fn captured_requests(capture: &Path) -> Vec<Value> {
    fs::read_to_string(capture)
        .expect("read request capture")
        .lines()
        .map(|line| serde_json::from_str(line).expect("captured request JSON"))
        .collect()
}

fn assert_repository_unchanged(root: &Path) {
    let output = Command::new("git")
        .current_dir(root)
        .args(["status", "--porcelain"])
        .output()
        .expect("inspect git status");
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "inspect mutated source repository: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn inspect_last_uses_the_canonical_cwd_imports_only_user_text_and_leaves_source_unchanged() {
    let root = temporary_directory("inspect-last");
    clean_repository(&root);
    let canonical_root = root.canonicalize().expect("canonical test root");
    let other_root = temporary_directory("other-source");
    let selected_id = "thread-private-selected-identifier";
    let (environment, capture) = fake_environment(
        &root,
        vec![json!({
            "data": [
                thread("thread-other-directory", &other_root.display().to_string(), 100),
                thread(selected_id, &canonical_root.display().to_string(), 99),
            ],
            "nextCursor": null,
            "backwardsCursor": null,
        })],
        read_result(selected_id, &canonical_root.display().to_string()),
        false,
    );

    let output = run(
        &root,
        &["inspect", "codex", "--last", "--json"],
        &environment,
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let result: Value = serde_json::from_slice(&output.stdout).expect("inspect JSON");
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["provider"], "codex");
    assert_eq!(result["imported_user_records"], 2);
    assert_eq!(result["blocker"], "projection_not_built");
    assert!(
        result["session"]
            .as_str()
            .expect("redacted session")
            .starts_with("codex-session:sha256:")
    );
    assert!(
        result["source_digest"]
            .as_str()
            .expect("source digest")
            .starts_with("sha256:")
    );
    let serialized = result.to_string();
    assert!(!serialized.contains(selected_id));
    assert!(!serialized.contains(&canonical_root.display().to_string()));
    assert!(!serialized.contains("private user intent"));
    assert!(!serialized.contains("assistant draft"));

    let requests = captured_requests(&capture);
    assert_eq!(requests[0]["method"], "initialize");
    assert_eq!(requests[0]["params"]["clientInfo"]["name"], "driftctl");
    assert_eq!(requests[1]["method"], "initialized");
    assert_eq!(requests[2]["method"], "thread/list");
    assert_eq!(
        requests[2]["params"]["cwd"],
        canonical_root.display().to_string()
    );
    assert_eq!(requests[2]["params"]["sortKey"], "recency_at");
    assert_eq!(requests[2]["params"]["sortDirection"], "desc");
    assert_eq!(requests[2]["params"]["useStateDbOnly"], true);
    assert_eq!(requests[3]["method"], "thread/read");
    assert_eq!(
        requests[3]["params"],
        json!({"threadId": selected_id, "includeTurns": true})
    );
    assert_repository_unchanged(&root);
}

#[test]
fn inspect_explicit_session_reads_only_that_session_and_keeps_text_out_of_human_output() {
    let root = temporary_directory("inspect-explicit");
    clean_repository(&root);
    let canonical_root = root.canonicalize().expect("canonical test root");
    let selected_id = "thread-private-explicit-identifier";
    let (environment, capture) = fake_environment(
        &root,
        vec![json!({"data": [], "nextCursor": null, "backwardsCursor": null})],
        read_result(selected_id, &canonical_root.display().to_string()),
        false,
    );

    let output = run(
        &root,
        &["inspect", "codex", "--session", selected_id],
        &environment,
    );
    assert_eq!(output.status.code(), Some(2));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("provider: codex"));
    assert!(text.contains("blocker: projection_not_built"));
    assert!(!text.contains(selected_id));
    assert!(!text.contains("private user intent"));
    let requests = captured_requests(&capture);
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2]["method"], "thread/read");
    assert_eq!(requests[2]["params"]["threadId"], selected_id);
    assert_repository_unchanged(&root);
}

#[test]
fn inspect_rejects_malformed_provider_content_and_truncated_json_without_mutating_source() {
    let root = temporary_directory("inspect-invalid");
    clean_repository(&root);
    let canonical_root = root.canonicalize().expect("canonical test root");
    let selected_id = "thread-private-invalid-identifier";
    let (environment, _) = fake_environment(
        &root,
        vec![json!({"data": [], "nextCursor": null, "backwardsCursor": null})],
        json!({"thread": {"id": selected_id, "cwd": canonical_root, "turns": "not-an-array"}}),
        false,
    );
    let malformed = run(
        &root,
        &["inspect", "codex", "--session", selected_id, "--json"],
        &environment,
    );
    assert_eq!(malformed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&malformed.stderr).contains("thread.turns"));
    assert!(!String::from_utf8_lossy(&malformed.stderr).contains(selected_id));
    assert_repository_unchanged(&root);

    let (environment, _) = fake_environment(
        &root,
        vec![json!({"data": [], "nextCursor": null, "backwardsCursor": null})],
        read_result(selected_id, &canonical_root.display().to_string()),
        true,
    );
    let truncated = run(
        &root,
        &["inspect", "codex", "--session", selected_id],
        &environment,
    );
    assert_eq!(truncated.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&truncated.stderr).contains("truncated"));
    assert_repository_unchanged(&root);
}

#[test]
fn inspect_rejects_duplicate_codex_record_ids_at_the_neutral_bundle_boundary() {
    let root = temporary_directory("inspect-duplicate-records");
    clean_repository(&root);
    let canonical_root = root.canonicalize().expect("canonical test root");
    let selected_id = "thread-private-duplicate-records";
    let duplicate_records = json!({
        "thread": {
            "id": selected_id,
            "cwd": canonical_root,
            "turns": [{
                "items": [
                    {"type": "userMessage", "id": "duplicate-message", "content": [{"type": "text", "text": "private first request"}]},
                    {"type": "userMessage", "id": "duplicate-message", "content": [{"type": "text", "text": "private second request"}]}
                ]
            }]
        }
    });
    let (environment, _) = fake_environment(
        &root,
        vec![json!({"data": [], "nextCursor": null, "backwardsCursor": null})],
        duplicate_records,
        false,
    );

    let output = run(
        &root,
        &["inspect", "codex", "--session", selected_id],
        &environment,
    );

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("duplicate neutral session bundle record ID"));
    assert!(!stderr.contains(selected_id));
    assert!(!stderr.contains("private first request"));
    assert_repository_unchanged(&root);
}

#[test]
fn inspect_last_fails_closed_when_no_exact_session_or_when_latest_is_ambiguous() {
    let root = temporary_directory("inspect-no-match");
    clean_repository(&root);
    let other_root = temporary_directory("inspect-other-root");
    let (environment, _) = fake_environment(
        &root,
        vec![json!({
            "data": [thread("thread-other", &other_root.display().to_string(), 100)],
            "nextCursor": null,
            "backwardsCursor": null,
        })],
        json!({"thread": {}}),
        false,
    );
    let no_match = run(&root, &["inspect", "codex", "--last"], &environment);
    assert_eq!(no_match.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&no_match.stderr).contains("no Codex session"));
    assert_repository_unchanged(&root);

    let canonical_root = root.canonicalize().expect("canonical test root");
    let (environment, _) = fake_environment(
        &root,
        vec![json!({
            "data": [
                thread("thread-one", &canonical_root.display().to_string(), 100),
                thread("thread-two", &canonical_root.display().to_string(), 100),
            ],
            "nextCursor": null,
            "backwardsCursor": null,
        })],
        json!({"thread": {}}),
        false,
    );
    let ambiguous = run(&root, &["inspect", "codex", "--last"], &environment);
    assert_eq!(ambiguous.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("ambiguous"));
    assert_repository_unchanged(&root);
}

#[test]
fn inspect_last_reads_every_page_before_selecting_the_latest_exact_cwd_match() {
    let root = temporary_directory("inspect-pagination");
    clean_repository(&root);
    let canonical_root = root.canonicalize().expect("canonical test root");
    let selected_id = "thread-private-second-page";
    let other_root = temporary_directory("inspect-pagination-other");
    let (environment, capture) = fake_environment(
        &root,
        vec![
            json!({
                "data": [thread("thread-other", &other_root.display().to_string(), 200)],
                "nextCursor": "cursor-page-two",
                "backwardsCursor": null,
            }),
            json!({
                "data": [thread(selected_id, &canonical_root.display().to_string(), 100)],
                "nextCursor": null,
                "backwardsCursor": null,
            }),
        ],
        read_result(selected_id, &canonical_root.display().to_string()),
        false,
    );

    let output = run(
        &root,
        &["inspect", "codex", "--last", "--json"],
        &environment,
    );
    assert_eq!(output.status.code(), Some(2));
    let requests = captured_requests(&capture);
    assert_eq!(requests[2]["params"]["cursor"], Value::Null);
    assert_eq!(requests[3]["method"], "thread/list");
    assert_eq!(requests[3]["params"]["cursor"], "cursor-page-two");
    assert_eq!(requests[4]["method"], "thread/read");
    assert_repository_unchanged(&root);
}
