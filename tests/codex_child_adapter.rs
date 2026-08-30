#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use driftctl::codex_child::{ChildForkRequest, ChildTurnRequest, CodexChildAdapter};
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

fn write_fake_app_server(root: &Path) -> PathBuf {
    let program = root.join("fake-codex.py");
    let staging = root.join("fake-codex.py.tmp");
    fs::write(
        &staging,
        r#"#!/usr/bin/env python3
import json
import os
import sys

root = os.path.dirname(os.path.realpath(sys.argv[0]))
with open(os.path.join(root, "fake-config.json"), encoding="utf-8") as file:
    config = json.load(file)
capture = os.path.join(root, "requests.jsonl")
scenario = config["scenario"]
parent = "parent-thread"
child = "child-thread"
target_cwd = config["child_cwd"]
approved = config["objective"]

for raw in sys.stdin:
    request = json.loads(raw)
    with open(capture, "a", encoding="utf-8") as file:
        file.write(json.dumps(request, sort_keys=True) + "\n")
    method = request.get("method")
    if method == "initialized":
        continue
    if method == "thread/fork" and scenario == "capability-unavailable":
        print(json.dumps({"id": request["id"], "error": {"code": -32601, "message": "unsupported"}}), flush=True)
        continue
    if method == "initialize":
        result = {"userAgent":"fake", "codexHome":"/not-used", "platformFamily":"unix", "platformOs":"linux"}
    elif method == "thread/fork":
        result = {"thread": {"id": child if scenario != "wrong-child" else parent, "cwd": target_cwd if scenario != "wrong-cwd" else "/wrong", "ephemeral": scenario == "ephemeral"}}
    elif method == "thread/resume":
        observed_model = "gpt-5.6-sol" if scenario == "policy-mismatch" else request["params"]["model"]
        result = {
            "approvalPolicy":request["params"]["approvalPolicy"],
            "cwd":target_cwd,
            "model":observed_model,
            "reasoningEffort":request["params"].get("effort", "max"),
            "sandbox":{"type":"workspaceWrite"},
            "thread":{"id":child,"cwd":target_cwd,"ephemeral":False}
        }
        if scenario == "policy-missing":
            result.pop("reasoningEffort")
    elif method == "thread/settings/update":
        result = {}
    elif method == "thread/goal/get":
        thread_id = request["params"]["threadId"]
        if thread_id == parent:
            parent_gets = globals().get("parent_gets", 0) + 1
            globals()["parent_gets"] = parent_gets
            objective = "parent objective"
            if scenario == "parent-changed-after" and parent_gets > 1:
                objective = "unexpected changed parent objective"
            result = {"goal": {"threadId": parent, "objective": objective}}
        else:
            child_gets = globals().get("child_gets", 0) + 1
            globals()["child_gets"] = child_gets
            if scenario == "absent-child" and child_gets == 1:
                result = {"goal": None}
                print(json.dumps({"id": request["id"], "result": result}), flush=True)
                continue
            if scenario == "missing-readback" and child_gets > 1:
                result = {"wrong": None}
                print(json.dumps({"id": request["id"], "result": result}), flush=True)
                continue
            objective = approved
            if scenario == "mismatched-readback" and method == "thread/goal/get":
                if child_gets > 1:
                    objective = "different objective"
            result = {"goal": {"threadId": child if scenario != "goal-wrong-child" else parent, "objective": objective}}
    elif method == "thread/goal/clear":
        result = {"cleared": scenario != "partial-clear"}
    elif method == "thread/goal/set":
        if scenario == "partial-set":
            result = {"goal": {"threadId": child, "objective": ""}}
        else:
            result = {"goal": {"threadId": child, "objective": approved}}
    elif method == "turn/start":
        if scenario == "turn-notification":
            result = {"turn": {"id": "turn-1", "items": [], "status": "inProgress"}}
            print(json.dumps({"id": request["id"], "result": result}), flush=True)
            print(json.dumps({"method": "turn/completed", "params": {"threadId": "child-thread", "turn": {"id": "turn-1", "items": [], "status": "completed"}}}), flush=True)
            continue
        else:
            result = {"turn": {"id": "turn-1", "items": [], "status": "completed"}}
    else:
        print(json.dumps({"id": request["id"], "error": {"code": -32601, "message": "unsupported"}}), flush=True)
        continue
    print(json.dumps({"id": request["id"], "result": result}), flush=True)
"#,
    )
    .expect("write fake Codex App Server");
    fs::File::open(&staging)
        .expect("open fake Codex App Server")
        .sync_all()
        .expect("sync fake Codex App Server");
    let mut permissions = fs::metadata(&staging)
        .expect("read fake metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&staging, permissions).expect("make fake executable");
    fs::rename(&staging, &program).expect("publish fake Codex App Server");
    program
}

fn configure_fake(
    root: &Path,
    scenario: &str,
    child_cwd: &Path,
    objective: &str,
) -> CodexChildAdapter {
    let program = write_fake_app_server(root);
    fs::write(
        root.join("fake-config.json"),
        json!({"scenario":scenario, "child_cwd":child_cwd, "objective":objective}).to_string(),
    )
    .expect("write fake configuration");
    CodexChildAdapter::with_program(program)
}

fn captured_requests(root: &Path) -> Vec<Value> {
    fs::read_to_string(root.join("requests.jsonl"))
        .expect("read request capture")
        .lines()
        .map(|line| serde_json::from_str(line).expect("captured JSON request"))
        .collect()
}

fn request(target: &Path, objective: &str) -> ChildForkRequest {
    ChildForkRequest::new("parent-thread", target, objective).expect("valid child request")
}

#[test]
fn creates_a_persisted_isolated_child_and_migrates_only_its_goal_transactionally() {
    let root = temporary_directory("child-goal-success");
    let child_cwd = root.join("isolated-child");
    fs::create_dir(&child_cwd).expect("create child fixture");
    let objective = "Complete the approved full objective";
    let adapter = configure_fake(&root, "success", &child_cwd, objective);

    let outcome = adapter
        .fork_and_migrate(request(&child_cwd, objective))
        .expect("child migration succeeds");
    assert_eq!(outcome.parent_goal().objective(), Some("parent objective"));
    assert_eq!(outcome.child_id(), "child-thread");
    assert_eq!(
        outcome.child_cwd(),
        child_cwd.canonicalize().expect("canonical cwd")
    );
    assert_eq!(outcome.child_goal().objective(), Some(objective));

    let requests = captured_requests(&root);
    let methods: Vec<_> = requests
        .iter()
        .map(|request| request["method"].as_str())
        .collect();
    assert_eq!(
        methods,
        vec![
            Some("initialize"),
            Some("initialized"),
            Some("thread/goal/get"),
            Some("thread/fork"),
            Some("thread/settings/update"),
            Some("thread/resume"),
            Some("thread/goal/get"),
            Some("thread/goal/clear"),
            Some("thread/goal/set"),
            Some("thread/goal/get"),
            Some("thread/goal/get"),
        ]
    );
    assert_eq!(requests[2]["params"], json!({"threadId":"parent-thread"}));
    assert_eq!(
        requests[0]["params"]["capabilities"],
        json!({"experimentalApi":true,"requestAttestation":false})
    );
    assert_eq!(
        requests[3]["params"],
        json!({
            "threadId":"parent-thread",
            "cwd": child_cwd.canonicalize().expect("canonical cwd"),
            "ephemeral":false,
            "model":"gpt-5.6-luna",
            "sandbox":"workspace-write",
            "approvalPolicy":"never"
        })
    );
    assert_eq!(
        requests[4]["params"],
        json!({
            "threadId":"child-thread",
            "model":"gpt-5.6-luna",
            "effort":"max",
            "approvalPolicy":"never",
            "sandboxPolicy":{"type":"workspaceWrite"}
        })
    );
    for request in &requests[6..10] {
        if request["method"]
            .as_str()
            .is_some_and(|method| method.starts_with("thread/goal/"))
        {
            assert_eq!(request["params"]["threadId"], "child-thread");
        }
    }
    assert_eq!(requests[8]["params"]["objective"], objective);

    fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn skips_clear_when_the_persisted_child_goal_is_already_absent() {
    let root = temporary_directory("child-goal-already-absent");
    let child_cwd = root.join("isolated-child");
    fs::create_dir(&child_cwd).expect("create child fixture");
    let objective = "Install the approved objective on the empty child";
    let adapter = configure_fake(&root, "absent-child", &child_cwd, objective);

    let outcome = adapter
        .fork_and_migrate(request(&child_cwd, objective))
        .expect("absent child goal does not require clear");
    assert_eq!(outcome.child_goal().objective(), Some(objective));
    let requests = captured_requests(&root);
    assert!(
        !requests
            .iter()
            .any(|request| request["method"] == "thread/goal/clear")
    );
    assert!(
        requests
            .iter()
            .any(|request| request["method"] == "thread/goal/set")
    );
    fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn rejects_unverifiable_or_partial_child_goal_migrations_without_mutating_the_parent_goal() {
    for scenario in [
        "wrong-child",
        "wrong-cwd",
        "ephemeral",
        "goal-wrong-child",
        "partial-clear",
        "partial-set",
        "missing-readback",
        "mismatched-readback",
        "parent-changed-after",
        "policy-mismatch",
        "policy-missing",
    ] {
        let root = temporary_directory(&format!("child-goal-{scenario}"));
        let child_cwd = root.join("isolated-child");
        fs::create_dir(&child_cwd).expect("create child fixture");
        let adapter = configure_fake(&root, scenario, &child_cwd, "Approved objective");

        let error = adapter
            .fork_and_migrate(request(&child_cwd, "Approved objective"))
            .expect_err("invalid migration must fail");
        assert!(
            error.to_string().contains("child")
                || error.to_string().contains("goal")
                || error.to_string().contains("parent")
        );
        let requests = captured_requests(&root);
        for request in requests.iter().filter(|request| {
            matches!(
                request["method"].as_str(),
                Some("thread/goal/clear") | Some("thread/goal/set")
            )
        }) {
            assert_ne!(request["params"]["threadId"], "parent-thread", "{scenario}");
        }
        fs::remove_dir_all(root).expect("remove test directory");
    }
}

#[test]
fn starts_a_child_turn_with_one_explicit_verified_worker_policy() {
    let root = temporary_directory("child-turn");
    let child_cwd = root.join("isolated-child");
    fs::create_dir(&child_cwd).expect("create child fixture");
    let adapter = configure_fake(&root, "turn-notification", &child_cwd, "Approved objective");

    let started = adapter
        .start_child_turn(
            ChildTurnRequest::new(
                "child-thread",
                &child_cwd,
                "Continue from the supplied neutral projection.",
                "Requirement R1 remains unresolved.",
            )
            .expect("valid turn request"),
        )
        .expect("start child turn");
    assert_eq!(started.turn_id(), "turn-1");
    assert!(started.completed());

    let requests = captured_requests(&root);
    assert_eq!(requests[2]["method"], "thread/resume");
    assert_eq!(
        requests[2]["params"],
        json!({
            "threadId":"child-thread",
            "model":"gpt-5.6-luna",
            "sandbox":"workspace-write",
            "approvalPolicy":"never"
        })
    );
    assert_eq!(requests[3]["method"], "thread/settings/update");
    assert_eq!(requests[4]["method"], "thread/resume");
    assert_eq!(requests[5]["method"], "turn/start");
    assert_eq!(
        requests[5]["params"],
        json!({
            "threadId":"child-thread",
            "input":[{"type":"text", "text":"Continue from the supplied neutral projection.\n\nRequirement R1 remains unresolved."}],
            "model":"gpt-5.6-luna",
            "effort":"max",
            "approvalPolicy":"never",
            "sandboxPolicy":{"type":"workspaceWrite"}
        })
    );
    fs::remove_dir_all(root).expect("remove test directory");
}

#[test]
fn reports_missing_app_server_capabilities_without_interactive_fallback() {
    let root = temporary_directory("child-capability");
    let child_cwd = root.join("isolated-child");
    fs::create_dir(&child_cwd).expect("create child fixture");
    let adapter = configure_fake(
        &root,
        "capability-unavailable",
        &child_cwd,
        "Approved objective",
    );

    let error = adapter
        .fork_and_migrate(request(&child_cwd, "Approved objective"))
        .expect_err("unsupported fork must fail");
    assert!(error.to_string().contains("capability unavailable"));
    assert!(error.to_string().contains("unsupported"));
    assert!(error.to_string().contains("no interactive fallback"));
    let requests = captured_requests(&root);
    assert_eq!(
        requests
            .iter()
            .map(|request| request["method"].as_str())
            .collect::<Vec<_>>(),
        vec![
            Some("initialize"),
            Some("initialized"),
            Some("thread/goal/get"),
            Some("thread/fork"),
        ]
    );
    fs::remove_dir_all(root).expect("remove test directory");
}
