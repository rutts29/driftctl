use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use driftctl::intent_history::{SourceProvider, SourceRole};
use driftctl::session_bundle::{BundleRecord, NativeGoal, NeutralSessionBundle};

fn has_argument_pair(arguments: &Value, first: &str, second: &str) -> bool {
    arguments
        .as_array()
        .expect("argument array")
        .windows(2)
        .any(|pair| pair[0] == first && pair[1] == second)
}

fn temporary_directory(case: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "driftctl-resolver-{case}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&path).expect("create isolated test directory");
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

fn driftctl_bin() -> OsString {
    std::env::var_os("DRIFTCTL_E2E_BIN")
        .unwrap_or_else(|| OsString::from(env!("CARGO_BIN_EXE_driftctl")))
}

fn clean_repository(root: &Path) {
    git(root, &["init", "--quiet"]);
    git(
        root,
        &["config", "user.email", "driftctl-test@example.invalid"],
    );
    git(root, &["config", "user.name", "driftctl test"]);
    fs::write(root.join("README.md"), "source repository\n").expect("write fixture");
    git(root, &["add", "README.md"]);
    git(root, &["commit", "--quiet", "-m", "source fixture"]);
}

fn fake_codex(root: &Path) -> PathBuf {
    let program = root.join("fake-codex.py");
    fs::write(
        &program,
        r#"#!/usr/bin/env python3
import json
import os
import sys

if "app-server" in sys.argv and "--stdio" in sys.argv:
    rpc_capture = os.environ["DRIFTCTL_FAKE_RPC_CAPTURE"]
    goals_path = rpc_capture + ".goals"
    try:
        with open(goals_path, encoding="utf-8") as goals_file:
            child_goals = json.load(goals_file)
    except FileNotFoundError:
        child_goals = {}
    def save_goals():
        with open(goals_path, "w", encoding="utf-8") as goals_file:
            json.dump(child_goals, goals_file, sort_keys=True)
    for raw in sys.stdin:
        request = json.loads(raw)
        method = request.get("method")
        if rpc_capture:
            with open(rpc_capture, "a", encoding="utf-8") as capture:
                capture.write(json.dumps(request, sort_keys=True) + "\n")
        if method == "initialized":
            continue
        if method == "initialize":
            result = {"userAgent":"fake","codexHome":"/private","platformFamily":"unix","platformOs":"linux"}
        elif method == "thread/list":
            result = {"data":[],"nextCursor":None,"backwardsCursor":None}
        elif method == "thread/read":
            thread_id = request["params"]["threadId"]
            if thread_id.startswith("continued-child"):
                with open(rpc_capture + ".children", encoding="utf-8") as child_file:
                    children = json.load(child_file)
                result = {"thread": children[thread_id] | {"turns": []}}
            else:
                sequence = json.loads(os.environ.get("DRIFTCTL_FAKE_READ_SEQUENCE", "[]"))
                if sequence:
                    count_path = rpc_capture + ".read-count"
                    try:
                        read_index = int(open(count_path, encoding="utf-8").read())
                    except FileNotFoundError:
                        read_index = 0
                    with open(count_path, "w", encoding="utf-8") as count_file:
                        count_file.write(str(read_index + 1))
                    result = sequence[min(read_index, len(sequence) - 1)]
                else:
                    result = json.loads(os.environ["DRIFTCTL_FAKE_READ"])
        elif method == "thread/goal/get":
            thread_id = request["params"]["threadId"]
            if thread_id.startswith("continued-child"):
                child_goal = os.environ.get(
                    "DRIFTCTL_FAKE_CHILD_GOAL_OVERRIDE", child_goals.get(thread_id)
                )
                result = {"goal": None if child_goal is None else {"threadId":thread_id,"objective":child_goal}}
            else:
                result = json.loads(os.environ.get("DRIFTCTL_FAKE_GOAL", '{"goal":null}'))
        elif method == "thread/fork":
            children_path = rpc_capture + ".children"
            try:
                with open(children_path, encoding="utf-8") as child_file:
                    children = json.load(child_file)
            except FileNotFoundError:
                children = {}
            child_id = "continued-child" if not children else "continued-child-" + str(len(children) + 1)
            result = {"thread":{"id":child_id,"cwd":request["params"]["cwd"],"ephemeral":False}}
            children[child_id] = result["thread"]
            with open(children_path, "w", encoding="utf-8") as child_file:
                json.dump(children, child_file)
        elif method == "thread/resume":
            with open(rpc_capture + ".children", encoding="utf-8") as child_file:
                children = json.load(child_file)
            child = children[request["params"]["threadId"]]
            result = {
                "approvalPolicy":request["params"]["approvalPolicy"],
                "cwd":child["cwd"],
                "model":request["params"]["model"],
                "reasoningEffort":"max",
                "sandbox":{"type":"workspaceWrite"},
                "thread":child
            }
        elif method == "thread/settings/update":
            with open(rpc_capture + ".children", encoding="utf-8") as child_file:
                children = json.load(child_file)
            thread_id = request["params"]["threadId"]
            result = {}
            print(json.dumps({"method":"thread/settings/updated","params":{
                "threadId":thread_id,
                "threadSettings":{
                    "approvalPolicy":request["params"]["approvalPolicy"],
                    "approvalsReviewer":"user",
                    "collaborationMode":{"mode":"default","settings":{"model":request["params"]["model"]}},
                    "cwd":children[thread_id]["cwd"],
                    "effort":request["params"]["effort"],
                    "model":request["params"]["model"],
                    "modelProvider":"openai",
                    "sandboxPolicy":request["params"]["sandboxPolicy"]
                }
            }}), flush=True)
        elif method == "thread/goal/clear":
            child_goals.pop(request["params"]["threadId"], None)
            save_goals()
            result = {"cleared":True}
        elif method == "thread/goal/set":
            if os.environ.get("DRIFTCTL_FAKE_GOAL_SET_UNAVAILABLE") == "1":
                print(json.dumps({"id":request["id"],"error":{"code":-32601,"message":"unsupported goal set"}}), flush=True)
                continue
            child_goal = request["params"]["objective"]
            child_goals[request["params"]["threadId"]] = child_goal
            save_goals()
            result = {"goal":{"threadId":request["params"]["threadId"],"objective":child_goal}}
        elif method == "turn/start":
            source_path = os.environ.get("DRIFTCTL_FAKE_MUTATE_SOURCE_PATH")
            if source_path:
                with open(source_path, "w", encoding="utf-8") as source_file:
                    source_file.write("mutated by child turn\n")
            post_turn_goal = os.environ.get("DRIFTCTL_FAKE_POST_TURN_GOAL")
            if post_turn_goal:
                child_goals[request["params"]["threadId"]] = post_turn_goal
                save_goals()
            result = {"turn":{"id":"continued-turn-" + request["params"]["threadId"],"items":[],"status":os.environ.get("DRIFTCTL_FAKE_TURN_STATUS", "completed")}}
        else:
            print(json.dumps({"id":request["id"],"error":{"code":-32601,"message":"unexpected"}}), flush=True)
            continue
        print(json.dumps({"id":request["id"],"result":result}), flush=True)
    sys.exit(0)

args = sys.argv[1:]
prompt = sys.stdin.read()
prompt_document = json.loads(prompt)
schema_path = args[args.index("--output-schema") + 1]
capture_path = os.environ["DRIFTCTL_FAKE_EXEC_CAPTURE"]
with open(capture_path, "a", encoding="utf-8") as capture:
    capture.write(json.dumps({
        "args": args,
        "prompt": prompt_document,
        "schema": json.load(open(schema_path, encoding="utf-8")),
        "api_key_environment_present": "OPENAI_API_KEY" in os.environ or "CODEX_API_KEY" in os.environ,
    }, sort_keys=True) + "\n")

count_path = capture_path + ".count"
try:
    call_index = int(open(count_path, encoding="utf-8").read())
except FileNotFoundError:
    call_index = 0
with open(count_path, "w", encoding="utf-8") as count:
    count.write(str(call_index + 1))
break_source = os.environ.get("DRIFTCTL_FAKE_BREAK_SOURCE_CURSOR")
if break_source:
    os.remove(break_source)
    os.mkdir(break_source)
if os.environ.get("DRIFTCTL_FAKE_DYNAMIC_CHUNKS") == "1":
    if prompt_document["protocol"] == "driftctl.semantic-proposal.v1":
        records = prompt_document["records"]
        response = {
            "schema_version": 1,
            "goal": {"text": "Preserve every bounded chunk", "source_record_ids": [records[0]["id"]]},
            "accounted_source_record_ids": [record["id"] for record in records],
            "operations": [{
                "operation": "add", "key": "chunk-" + record["id"], "kind": "invariant",
                "text": "Preserve chunk " + record["id"], "target_key": "", "intent_keys": [],
                "source_record_ids": [record["id"]], "alternatives": []
            } for record in records]
        }
    else:
        records = prompt_document["delta_records"]
        active = prompt_document["active_projection"]
        active_ids = [item["id"] for section in ["preserve", "frontier", "validation"] for item in active[section]]
        response = {
            "schema_version": 1,
            "base_projection_revision": prompt_document["base_projection_revision"],
            "base_event_sequence": prompt_document["base_event_sequence"],
            "classification": "additive",
            "accounted_active_intent_ids": active_ids,
            "accounted_source_record_ids": [record["id"] for record in records],
            "operations": [{
                "operation": "add", "key": "chunk-" + record["id"], "kind": "invariant",
                "text": "Preserve chunk " + record["id"], "target_intent_id": "", "intent_ids": [],
                "evidence_id": "", "reason": "", "source_record_ids": [record["id"]], "alternatives": []
            } for record in records]
        }
else:
    responses = json.loads(os.environ["DRIFTCTL_FAKE_PROPOSALS"])
    response = responses[min(call_index, len(responses) - 1)]
text = response if isinstance(response, str) else json.dumps(response)
last_path = args[args.index("--output-last-message") + 1]
with open(last_path, "w", encoding="utf-8") as last:
    last.write(text)
print(json.dumps({"type":"thread.started","thread_id":"private-compactor-thread"}))
print(json.dumps({"type":"item.completed","item":{"id":"private-item","type":"agent_message","text":text}}))
print(json.dumps({"type":"turn.completed","usage":{"input_tokens":101,"cached_input_tokens":11,"output_tokens":23,"reasoning_output_tokens":7}}))
"#,
    )
    .expect("write fake Codex");
    let mut permissions = fs::metadata(&program).expect("fake metadata").permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o755);
    }
    fs::set_permissions(&program, permissions).expect("make fake executable");
    program
}

fn session(id: &str, root: &Path, messages: &[(&str, &str)]) -> Value {
    let items: Vec<Value> = messages
        .iter()
        .map(|(record, text)| {
            json!({"type":"userMessage","id":record,"content":[{"type":"text","text":text}]})
        })
        .collect();
    json!({"thread":{"id":id,"cwd":root,"turns":[{"items":items}]}})
}

fn non_user_change_sequence(fixture: &Fixture, stable_reads: usize) -> String {
    let canonical = fixture.root.canonicalize().expect("canonical source");
    let mut before = session(
        &fixture.session_id,
        &canonical,
        &[
            ("u1", "private raw goal that public output must not echo"),
            ("u2", "private additive steering"),
            ("u3", "private explicit supersession"),
        ],
    );
    before["thread"]["turns"][0]["items"]
        .as_array_mut()
        .expect("thread items")
        .push(json!({"type":"agentMessage","id":"a1","text":"first assistant state"}));
    let mut after = before.clone();
    after["thread"]["turns"][0]["items"]
        .as_array_mut()
        .expect("thread items")[3]["text"] = json!("changed assistant state");
    let mut sequence = vec![before; stable_reads];
    sequence.push(after);
    Value::Array(sequence).to_string()
}

fn base_proposal() -> Value {
    json!({
        "schema_version": 1,
        "goal": {"text":"Ship a usable inspector","source_record_ids":["u1:0"]},
        "accounted_source_record_ids": ["u1:0","u2:0","u3:0"],
        "operations": [
            {"operation":"add","key":"format-json","kind":"constraint","text":"Emit JSON","target_key":"","intent_keys":[],"source_record_ids":["u2:0"],"alternatives":[]},
            {"operation":"supersede","key":"format-yaml","kind":"constraint","text":"Emit YAML","target_key":"format-json","intent_keys":[],"source_record_ids":["u3:0"],"alternatives":[]}
        ]
    })
}

struct Fixture {
    root: PathBuf,
    environment: BTreeMap<&'static str, String>,
    capture: PathBuf,
    rpc_capture: PathBuf,
    artifacts: PathBuf,
    state_home: PathBuf,
    session_id: String,
}

impl Fixture {
    fn new(proposals: Vec<Value>) -> Self {
        let root = temporary_directory("source");
        clean_repository(&root);
        let canonical = root.canonicalize().expect("canonical source");
        let fake_root = temporary_directory("fake");
        let program = fake_codex(&fake_root);
        let capture = fake_root.join("exec.jsonl");
        let rpc_capture = fake_root.join("rpc.jsonl");
        let artifacts = fake_root.join("private-artifacts");
        let state_home = fake_root.join("state");
        let session_id = "private-source-session".to_owned();
        let source = session(
            &session_id,
            &canonical,
            &[
                ("u1", "private raw goal that public output must not echo"),
                ("u2", "private additive steering"),
                ("u3", "private explicit supersession"),
            ],
        );
        let mut environment = BTreeMap::new();
        environment.insert("DRIFTCTL_CODEX_BIN", program.display().to_string());
        environment.insert("DRIFTCTL_FAKE_EXEC_CAPTURE", capture.display().to_string());
        environment.insert(
            "DRIFTCTL_FAKE_RPC_CAPTURE",
            rpc_capture.display().to_string(),
        );
        environment.insert("DRIFTCTL_ARTIFACT_DIR", artifacts.display().to_string());
        environment.insert("XDG_STATE_HOME", state_home.display().to_string());
        environment.insert("DRIFTCTL_FAKE_READ", source.to_string());
        environment.insert(
            "DRIFTCTL_FAKE_PROPOSALS",
            Value::Array(proposals).to_string(),
        );
        Self {
            root,
            environment,
            capture,
            rpc_capture,
            artifacts,
            state_home,
            session_id,
        }
    }

    fn run(&self, options: &[&str]) -> Output {
        let mut arguments = vec!["inspect", "codex", "--session", &self.session_id];
        arguments.extend_from_slice(options);
        let mut command = Command::new(driftctl_bin());
        command.current_dir(&self.root).args(arguments);
        command.env(
            "OPENAI_API_KEY",
            "private-test-key-must-not-reach-compactor",
        );
        command.env("CODEX_API_KEY", "private-test-key-must-not-reach-compactor");
        for (name, value) in &self.environment {
            command.env(name, value);
        }
        command.output().expect("run driftctl")
    }

    fn run_bundle(&self, run_id: &str) -> Output {
        let mut command = Command::new(driftctl_bin());
        command
            .current_dir(&self.root)
            .args(["bundle", "--run", run_id, "--json"]);
        for (name, value) in &self.environment {
            command.env(name, value);
        }
        command.output().expect("run driftctl bundle")
    }

    fn run_bundle_inspect(&self, path: &Path, options: &[&str]) -> Output {
        let mut arguments = vec!["inspect", "bundle", "--file", path.to_str().unwrap()];
        arguments.extend_from_slice(options);
        let mut command = Command::new(driftctl_bin());
        command.current_dir(&self.root).args(arguments);
        for (name, value) in &self.environment {
            command.env(name, value);
        }
        command.output().expect("run neutral bundle inspect")
    }

    fn run_bundle_inspect_stdin(&self, bytes: &[u8]) -> Output {
        let mut command = Command::new(driftctl_bin());
        command
            .current_dir(&self.root)
            .args(["inspect", "bundle", "--stdin", "--json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in &self.environment {
            command.env(name, value);
        }
        let mut child = command.spawn().expect("spawn neutral bundle stdin inspect");
        child
            .stdin
            .take()
            .expect("bundle stdin")
            .write_all(bytes)
            .expect("write bundle stdin");
        child
            .wait_with_output()
            .expect("wait for bundle stdin inspect")
    }

    fn run_continue(&self, options: &[&str]) -> Output {
        let mut arguments = vec!["continue", "codex", "--session", &self.session_id];
        arguments.extend_from_slice(options);
        let mut command = Command::new(driftctl_bin());
        command.current_dir(&self.root).args(arguments);
        for (name, value) in &self.environment {
            command.env(name, value);
        }
        command.output().expect("run driftctl continue")
    }

    fn run_compare(&self, options: &[&str]) -> Output {
        let mut arguments = vec!["compare", "codex", "--session", &self.session_id];
        arguments.extend_from_slice(options);
        let mut command = Command::new(driftctl_bin());
        command.current_dir(&self.root).args(arguments);
        for (name, value) in &self.environment {
            command.env(name, value);
        }
        command.output().expect("run driftctl compare")
    }

    fn run_bound_verify(&self, run_id: &str, requirement: &str, verifier: &str) -> Output {
        let mut command = Command::new(driftctl_bin());
        command.current_dir(&self.root).args([
            "verify",
            "--run",
            run_id,
            "--requirement",
            requirement,
            "--json",
            "--",
            verifier,
        ]);
        for (name, value) in &self.environment {
            command.env(name, value);
        }
        command.output().expect("run bound driftctl verifier")
    }

    fn run_bound_gate(&self, run_id: &str, gate: &str, verifier: &str) -> Output {
        let mut command = Command::new(driftctl_bin());
        command.current_dir(&self.root).args([
            "verify", "--run", run_id, "--gate", gate, "--json", "--", verifier,
        ]);
        for (name, value) in &self.environment {
            command.env(name, value);
        }
        command.output().expect("run bound driftctl gate")
    }

    fn calls(&self) -> Vec<Value> {
        fs::read_to_string(&self.capture)
            .expect("read exec capture")
            .lines()
            .map(|line| serde_json::from_str(line).expect("capture JSON"))
            .collect()
    }

    fn rpc_calls(&self) -> Vec<Value> {
        fs::read_to_string(&self.rpc_capture)
            .expect("read RPC capture")
            .lines()
            .map(|line| serde_json::from_str(line).expect("captured RPC request"))
            .collect()
    }

    fn assert_unchanged(&self) {
        let output = Command::new("git")
            .current_dir(&self.root)
            .args(["status", "--porcelain"])
            .output()
            .expect("git status");
        assert!(output.status.success());
        assert!(output.stdout.is_empty(), "source repository was mutated");
    }

    fn retained_artifacts(&self) -> Vec<PathBuf> {
        let mut paths: Vec<_> = fs::read_dir(&self.artifacts)
            .expect("read private artifact directory")
            .map(|entry| entry.expect("artifact entry").path())
            .collect();
        paths.sort();
        paths
    }

    fn state_file(&self, name: &str) -> PathBuf {
        let mut pending = vec![self.state_home.clone()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).expect("read state directory") {
                let path = entry.expect("state entry").path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.file_name().is_some_and(|candidate| candidate == name) {
                    return path;
                }
            }
        }
        panic!("state file {name} was not found")
    }
}

fn prepare_pending_goal_change(fixture: &mut Fixture, proposed_goal: &str) -> Value {
    let first = fixture.run(&["--json"]);
    assert_eq!(first.status.code(), Some(0), "{first:?}");
    let first_document: Value = serde_json::from_slice(&first.stdout).expect("initial JSON");
    let revision = first_document["projection"]["revision"]
        .as_u64()
        .expect("projection revision");
    let active_ids = first_document["projection"]["preserve"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            first_document["projection"]["frontier"]
                .as_array()
                .into_iter()
                .flatten(),
        )
        .chain(
            first_document["projection"]["validation"]
                .as_array()
                .into_iter()
                .flatten(),
        )
        .map(|item| item["id"].clone())
        .collect::<Vec<_>>();
    let proposal = json!({
        "schema_version":1,
        "base_projection_revision":revision,
        "base_event_sequence":revision,
        "classification":"goal_change",
        "accounted_active_intent_ids":active_ids,
        "accounted_source_record_ids":["u4:0"],
        "operations":[],
        "proposed_goal":{
            "text":proposed_goal,
            "source_record_ids":["u4:0"]
        }
    });
    let canonical = fixture.root.canonicalize().expect("canonical source");
    fixture.environment.insert(
        "DRIFTCTL_FAKE_READ",
        session(
            &fixture.session_id,
            &canonical,
            &[
                ("u1", "private raw goal that public output must not echo"),
                ("u2", "private additive steering"),
                ("u3", "private explicit supersession"),
                ("u4", "replace the overall goal with the proposed objective"),
            ],
        )
        .to_string(),
    );
    fixture.environment.insert(
        "DRIFTCTL_FAKE_PROPOSALS",
        json!([base_proposal(), proposal]).to_string(),
    );
    let pending = fixture.run(&["--json"]);
    assert_eq!(pending.status.code(), Some(2), "{pending:?}");
    first_document
}

#[test]
fn inspect_persists_and_reuses_a_private_run_that_bundle_can_export() {
    let fixture = Fixture::new(vec![base_proposal()]);

    let first = fixture.run(&["--json"]);
    assert_eq!(first.status.code(), Some(0), "{first:?}");
    let first_document: Value = serde_json::from_slice(&first.stdout).expect("first inspect JSON");
    let run_id = first_document["run_id"]
        .as_str()
        .expect("inspect returns an opaque run ID");
    assert!(
        !run_id.is_empty()
            && run_id.chars().all(
                |character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            )
    );

    let second = fixture.run(&["--json"]);
    assert_eq!(second.status.code(), Some(0), "{second:?}");
    let second_document: Value =
        serde_json::from_slice(&second.stdout).expect("second inspect JSON");
    assert_eq!(second_document["run_id"], run_id);
    assert_eq!(second_document["projection"], first_document["projection"]);
    assert_eq!(
        fixture.calls().len(),
        1,
        "an unchanged source must not consume another compactor call"
    );

    let bundle = fixture.run_bundle(run_id);
    assert_eq!(bundle.status.code(), Some(0), "{bundle:?}");
    let bundle_document: Value = serde_json::from_slice(&bundle.stdout).expect("bundle JSON");
    assert_eq!(bundle_document["schema_version"], 1);
    assert_eq!(bundle_document["run_id"], run_id);
    assert_eq!(bundle_document["projection"], first_document["projection"]);
    let public = bundle_document.to_string();
    assert!(!public.contains(&fixture.session_id));
    assert!(!public.contains(&fixture.root.display().to_string()));
    assert!(!public.contains("private raw goal"));

    let driftctl_state = fixture.state_home.join("driftctl");
    assert!(driftctl_state.is_dir(), "inspect creates XDG-local state");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&driftctl_state)
                .expect("state metadata")
                .permissions()
                .mode()
                & 0o077,
            0,
            "Driftctl state must not be group/world accessible"
        );
    }
    fixture.assert_unchanged();
}

#[test]
fn non_authoritative_append_advances_source_without_model_or_intent_change() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    let first = fixture.run(&["--json"]);
    assert_eq!(first.status.code(), Some(0), "{first:?}");
    let first_document: Value = serde_json::from_slice(&first.stdout).expect("first inspect JSON");

    let mut source: Value = serde_json::from_str(&fixture.environment["DRIFTCTL_FAKE_READ"])
        .expect("mutable source fixture");
    source["thread"]["turns"][0]["items"]
        .as_array_mut()
        .expect("source items")
        .push(json!({
            "type":"agentMessage",
            "id":"assistant-after-user-steering",
            "text":"private assistant-only append"
        }));
    fixture
        .environment
        .insert("DRIFTCTL_FAKE_READ", source.to_string());

    let second = fixture.run(&["--json"]);
    assert_eq!(second.status.code(), Some(0), "{second:?}");
    let second_document: Value =
        serde_json::from_slice(&second.stdout).expect("second inspect JSON");
    assert_eq!(second_document["projection"], first_document["projection"]);
    assert_eq!(second_document["source"]["imported_user_records"], 3);
    assert_ne!(
        second_document["source"]["digest"],
        first_document["source"]["digest"]
    );
    assert_eq!(
        fixture.calls().len(),
        1,
        "assistant-only evidence append must not spend a semantic model call"
    );
    let cursor: Value = serde_json::from_str(
        &fs::read_to_string(fixture.state_file("source.json")).expect("source cursor"),
    )
    .expect("source cursor JSON");
    assert_eq!(
        cursor["accepted_records"]
            .as_array()
            .expect("accepted records")
            .last()
            .expect("last source record")["role"],
        "assistant"
    );
    fixture.assert_unchanged();
}

#[test]
fn inspect_accepts_a_strict_neutral_bundle_without_a_native_harness_adapter() {
    let fixture = Fixture::new(vec![base_proposal()]);
    let canonical = fixture.root.canonicalize().expect("canonical source");
    let repository_digest = format!(
        "sha256:{:x}",
        Sha256::digest(canonical.to_str().expect("UTF-8 source path").as_bytes())
    );
    let bundle = NeutralSessionBundle::from_records_with_native_goal(
        SourceProvider::Bundle,
        "synthetic-local-session",
        repository_digest,
        NativeGoal::Unknown,
        vec![
            BundleRecord::new("u1:0", SourceRole::User, "Ship a usable inspector")
                .expect("goal record"),
            BundleRecord::new("u2:0", SourceRole::User, "Emit JSON").expect("steering record"),
            BundleRecord::new(
                "a1:0",
                SourceRole::Assistant,
                "private assistant context is not intent authority",
            )
            .expect("assistant context record"),
            BundleRecord::new("u3:0", SourceRole::User, "Emit YAML instead")
                .expect("supersession record"),
        ],
    )
    .expect("neutral bundle");
    let path = fixture.state_home.with_file_name("neutral-session.json");
    fs::write(&path, bundle.to_json().expect("bundle JSON")).expect("write neutral bundle");
    let before = fs::read(&path).expect("bundle before");

    let inspected = fixture.run_bundle_inspect(&path, &["--json"]);
    assert_eq!(inspected.status.code(), Some(0), "{inspected:?}");
    let document: Value = serde_json::from_slice(&inspected.stdout).expect("bundle inspect JSON");
    assert_eq!(document["status"], "usable");
    assert_eq!(document["provider"], "bundle");
    assert_eq!(document["source"]["imported_user_records"], 3);
    assert_eq!(
        document["projection"]["goal"]["text"],
        "Ship a usable inspector"
    );
    assert_eq!(document["projection"]["frontier"][0]["text"], "Emit YAML");
    assert!(
        !document
            .to_string()
            .contains("private assistant context is not intent authority")
    );
    assert_eq!(fs::read(&path).expect("bundle after"), before);

    let cached = fixture.run_bundle_inspect(&path, &["--json"]);
    assert_eq!(cached.status.code(), Some(0), "{cached:?}");
    assert_eq!(
        fixture.calls().len(),
        1,
        "cached intake must not rerun the model"
    );
    let piped = fixture.run_bundle_inspect_stdin(&before);
    assert_eq!(piped.status.code(), Some(0), "{piped:?}");
    assert_eq!(
        fixture.calls().len(),
        1,
        "stdin cached intake reran the model"
    );

    let mut wrong_repository: Value =
        serde_json::from_slice(&before).expect("mutable neutral bundle JSON");
    wrong_repository["source"]["repository_digest"] = json!(format!("sha256:{}", "0".repeat(64)));
    fs::write(&path, wrong_repository.to_string()).expect("write wrong-repository bundle");
    let rejected = fixture.run_bundle_inspect(&path, &["--json"]);
    assert_eq!(rejected.status.code(), Some(1), "{rejected:?}");
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("different repository"),
        "{rejected:?}"
    );
    assert_eq!(fixture.calls().len(), 1, "rejection must precede model use");

    let mut future_schema: Value =
        serde_json::from_slice(&before).expect("future-schema neutral bundle JSON");
    future_schema["schema_version"] = json!(2);
    fs::write(&path, future_schema.to_string()).expect("write future-schema bundle");
    let future = fixture.run_bundle_inspect(&path, &["--json"]);
    assert_eq!(future.status.code(), Some(1), "{future:?}");
    assert!(
        String::from_utf8_lossy(&future.stderr).contains("unsupported neutral session bundle"),
        "{future:?}"
    );
    assert_eq!(
        fixture.calls().len(),
        1,
        "schema rejection precedes model use"
    );
    fixture.assert_unchanged();
}

#[test]
fn inspect_rejects_a_non_user_parent_transcript_change_during_resolution() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    let sequence = non_user_change_sequence(&fixture, 1);
    fixture
        .environment
        .insert("DRIFTCTL_FAKE_READ_SEQUENCE", sequence);

    let inspected = fixture.run(&["--json"]);
    assert_eq!(inspected.status.code(), Some(1), "{inspected:?}");
    assert!(
        String::from_utf8_lossy(&inspected.stderr)
            .contains("Codex source session or native goal changed during inspect"),
        "{inspected:?}"
    );
}

#[test]
fn compare_blocks_when_non_user_parent_history_changes_during_children() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    let sequence = non_user_change_sequence(&fixture, 3);
    fixture
        .environment
        .insert("DRIFTCTL_FAKE_READ_SEQUENCE", sequence);

    let compared = fixture.run_compare(&["--json"]);
    assert_eq!(compared.status.code(), Some(2), "{compared:?}");
    assert!(
        String::from_utf8_lossy(&compared.stderr)
            .contains("Codex source session or native goal changed during inspect"),
        "{compared:?}"
    );
}

#[test]
fn continue_blocks_when_non_user_parent_history_changes_during_child() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    let sequence = non_user_change_sequence(&fixture, 3);
    fixture
        .environment
        .insert("DRIFTCTL_FAKE_READ_SEQUENCE", sequence);

    let continued = fixture.run_continue(&["--json"]);
    assert_eq!(continued.status.code(), Some(2), "{continued:?}");
    assert!(
        String::from_utf8_lossy(&continued.stderr)
            .contains("Codex source session or native goal changed during inspect"),
        "{continued:?}"
    );
}

#[test]
fn compare_runs_equal_isolated_children_with_only_the_projection_added() {
    let fixture = Fixture::new(vec![base_proposal()]);
    let compared = fixture.run_compare(&["--json"]);
    assert_eq!(compared.status.code(), Some(0), "{compared:?}");
    let document: Value = serde_json::from_slice(&compared.stdout).expect("comparison JSON");
    assert_eq!(document["status"], "completed");
    assert_eq!(document["fairness"]["starting_manifest_equal"], true);
    assert_eq!(document["fairness"]["neutral_prompt_equal"], true);
    assert_eq!(document["fairness"]["tool_policy_equal"], true);
    assert_eq!(document["fairness"]["turn_timeout_equal"], true);
    assert_eq!(
        document["fairness"]["turn_timeout_policy"],
        "provider_terminal_event"
    );
    assert_eq!(
        document["fairness"]["worker_policy"],
        json!({
            "model":"gpt-5.6-luna",
            "effort":"max",
            "sandbox":"workspace-write",
            "approval_policy":"never",
            "verified_readback":true
        })
    );
    assert_eq!(document["baseline"]["turn_status"], "completed");
    assert_eq!(document["workflow"]["turn_status"], "completed");
    assert_eq!(document["baseline"]["changed_paths"], json!([]));
    assert_eq!(document["workflow"]["changed_paths"], json!([]));
    assert_ne!(
        document["baseline"]["child_thread_id"],
        document["workflow"]["child_thread_id"]
    );
    assert_eq!(document["source_unchanged"], true);
    assert_eq!(document["parent_unchanged"], true);
    assert_eq!(document["adoption"], "none");
    assert!(
        document["containment"]
            .as_str()
            .expect("containment notice")
            .contains("host-wide")
    );

    let rpc = fixture.rpc_calls();
    let turns = rpc
        .iter()
        .filter(|request| request["method"] == "turn/start")
        .collect::<Vec<_>>();
    assert_eq!(turns.len(), 2);
    let baseline_text = turns[0]["params"]["input"][0]["text"]
        .as_str()
        .expect("baseline prompt");
    let workflow_text = turns[1]["params"]["input"][0]["text"]
        .as_str()
        .expect("workflow prompt");
    assert!(workflow_text.starts_with(baseline_text));
    assert!(workflow_text.contains("\"goal\""));
    assert!(workflow_text.contains("\"frontier\""));
    assert!(!workflow_text.contains("private raw goal"));
    for request in rpc.iter().filter(|request| {
        matches!(
            request["method"].as_str(),
            Some("thread/fork" | "thread/resume")
        )
    }) {
        assert_eq!(request["params"]["model"], "gpt-5.6-luna");
        assert_eq!(request["params"]["sandbox"], "workspace-write");
        assert_eq!(request["params"]["approvalPolicy"], "never");
    }
    for request in &turns {
        assert_eq!(request["params"]["model"], "gpt-5.6-luna");
        assert_eq!(request["params"]["effort"], "max");
        assert_eq!(request["params"]["sandboxPolicy"]["type"], "workspaceWrite");
        assert_eq!(request["params"]["approvalPolicy"], "never");
    }
    let settings = rpc
        .iter()
        .filter(|request| request["method"] == "thread/settings/update")
        .collect::<Vec<_>>();
    assert_eq!(settings.len(), 4);
    assert!(settings.iter().all(|request| {
        request["params"]["model"] == "gpt-5.6-luna"
            && request["params"]["effort"] == "max"
            && request["params"]["sandboxPolicy"]["type"] == "workspaceWrite"
            && request["params"]["approvalPolicy"] == "never"
    }));
    assert_eq!(fixture.calls().len(), 1);
    fixture.assert_unchanged();
}

#[test]
fn run_bound_verification_attaches_evidence_and_candidate_change_reopens_it() {
    let fixture = Fixture::new(vec![base_proposal()]);
    let inspected = fixture.run(&["--json"]);
    assert_eq!(inspected.status.code(), Some(0), "{inspected:?}");
    let inspected_document: Value =
        serde_json::from_slice(&inspected.stdout).expect("inspect JSON");
    let run_id = inspected_document["run_id"].as_str().expect("run ID");
    let requirement_id = inspected_document["projection"]["frontier"][0]["id"]
        .as_str()
        .expect("active requirement ID");

    let continued = fixture.run_continue(&["--json"]);
    assert_eq!(continued.status.code(), Some(0), "{continued:?}");
    let continued_document: Value =
        serde_json::from_slice(&continued.stdout).expect("continue JSON");
    let candidate = PathBuf::from(
        continued_document["child_cwd"]
            .as_str()
            .expect("bound candidate path"),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(fixture.state_file("candidate.json"))
                .expect("candidate binding metadata")
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }

    let passed = fixture.run_bound_verify(run_id, requirement_id, "/bin/true");
    assert_eq!(passed.status.code(), Some(0), "{passed:?}");
    let passed_document: Value =
        serde_json::from_slice(&passed.stdout).expect("bound verification JSON");
    assert_eq!(passed_document["run_id"], run_id);
    assert_eq!(passed_document["status"], "passed");
    assert_eq!(passed_document["evidence_attached"], true);
    assert_eq!(passed_document["requirement_evidence_complete"], true);
    assert_eq!(passed_document["verified_completion"], false);
    assert_eq!(
        passed_document["completion_blockers"]
            .as_array()
            .expect("completion blockers")
            .len(),
        4
    );

    for gate in ["regression", "integration", "protected_scope", "review"] {
        let gated = fixture.run_bound_gate(run_id, gate, "/bin/true");
        assert_eq!(gated.status.code(), Some(0), "{gate}: {gated:?}");
        let gated_document: Value =
            serde_json::from_slice(&gated.stdout).expect("gate verification JSON");
        assert_eq!(gated_document["gate"], gate);
        assert_eq!(gated_document["gate_evidence_recorded"], true);
        assert_eq!(gated_document["verified_completion"], gate == "review");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(fixture.state_file("completion-gates.json"))
                .expect("completion gate metadata")
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }

    let satisfied = fixture.run(&["--json"]);
    assert_eq!(satisfied.status.code(), Some(0), "{satisfied:?}");
    let satisfied_document: Value =
        serde_json::from_slice(&satisfied.stdout).expect("satisfied inspect JSON");
    assert!(
        satisfied_document["projection"]["frontier"]
            .as_array()
            .expect("frontier")
            .is_empty()
    );
    assert_eq!(
        satisfied_document["projection"]["preserve"][0]["evidence_state"],
        "satisfied"
    );

    fs::write(
        candidate.join("README.md"),
        "candidate changed after evidence\n",
    )
    .expect("mutate only the isolated candidate");
    let rejected_review = fixture.run_bound_gate(run_id, "review", "/bin/false");
    assert_eq!(
        rejected_review.status.code(),
        Some(2),
        "{rejected_review:?}"
    );
    let rejected_review_document: Value =
        serde_json::from_slice(&rejected_review.stdout).expect("failed review JSON");
    assert_eq!(rejected_review_document["gate_evidence_recorded"], false);
    assert_eq!(rejected_review_document["verified_completion"], false);
    assert_eq!(rejected_review_document["invalidated_evidence_count"], 1);
    assert!(
        rejected_review_document["completion_blockers"]
            .as_array()
            .expect("failed-review blockers")
            .iter()
            .any(|blocker| {
                blocker["kind"] == "failed_completion_gate" && blocker["gate"] == "review"
            })
    );
    let rejected_review_retry = fixture.run_bound_gate(run_id, "review", "/bin/true");
    assert_eq!(
        rejected_review_retry.status.code(),
        Some(1),
        "{rejected_review_retry:?}"
    );
    assert!(
        String::from_utf8_lossy(&rejected_review_retry.stderr)
            .contains("review is already recorded for this candidate checkpoint")
    );
    let failed = fixture.run_bound_verify(run_id, requirement_id, "/bin/false");
    assert_eq!(failed.status.code(), Some(2), "{failed:?}");
    let failed_document: Value =
        serde_json::from_slice(&failed.stdout).expect("failed bound verification JSON");
    assert_eq!(failed_document["status"], "failed");
    assert_eq!(failed_document["evidence_attached"], false);
    assert_eq!(failed_document["invalidated_evidence_count"], 0);
    assert_eq!(failed_document["requirement_evidence_complete"], false);
    assert_eq!(failed_document["verified_completion"], false);
    assert!(
        failed_document["completion_blockers"]
            .as_array()
            .expect("post-mutation completion blockers")
            .iter()
            .any(|blocker| blocker["kind"] == "stale_completion_gate")
    );

    let reopened = fixture.run(&["--json"]);
    assert_eq!(reopened.status.code(), Some(0), "{reopened:?}");
    let reopened_document: Value =
        serde_json::from_slice(&reopened.stdout).expect("reopened inspect JSON");
    assert_eq!(
        reopened_document["projection"]["frontier"][0]["id"],
        requirement_id
    );
    assert_eq!(
        reopened_document["projection"]["frontier"][0]["evidence_state"],
        "reopened"
    );
    fixture.assert_unchanged();
}

#[test]
fn continue_blocks_when_the_child_goal_changes_during_the_turn() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    fixture.environment.insert(
        "DRIFTCTL_FAKE_POST_TURN_GOAL",
        "unapproved replacement goal".to_owned(),
    );
    let inspected = fixture.run(&["--json"]);
    assert_eq!(inspected.status.code(), Some(0), "{inspected:?}");

    let continued = fixture.run_continue(&["--json"]);
    assert_eq!(continued.status.code(), Some(2), "{continued:?}");
    assert!(
        String::from_utf8_lossy(&continued.stderr)
            .contains("child native goal changed after continuation"),
        "{continued:?}"
    );
    fixture.assert_unchanged();
}

#[test]
fn external_child_goal_change_prevents_verified_completion() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    let inspected = fixture.run(&["--json"]);
    assert_eq!(inspected.status.code(), Some(0), "{inspected:?}");
    let inspected_document: Value =
        serde_json::from_slice(&inspected.stdout).expect("inspect JSON");
    let run_id = inspected_document["run_id"].as_str().expect("run ID");
    let requirement_id = inspected_document["projection"]["frontier"][0]["id"]
        .as_str()
        .expect("requirement ID");
    let continued = fixture.run_continue(&["--json"]);
    assert_eq!(continued.status.code(), Some(0), "{continued:?}");
    assert_eq!(
        fixture
            .run_bound_verify(run_id, requirement_id, "/bin/true")
            .status
            .code(),
        Some(0)
    );
    for gate in ["regression", "integration", "protected_scope"] {
        let gated = fixture.run_bound_gate(run_id, gate, "/bin/true");
        assert_eq!(gated.status.code(), Some(0), "{gate}: {gated:?}");
    }
    fixture.environment.insert(
        "DRIFTCTL_FAKE_CHILD_GOAL_OVERRIDE",
        "externally changed goal".to_owned(),
    );

    let review = fixture.run_bound_gate(run_id, "review", "/bin/true");
    assert_eq!(review.status.code(), Some(0), "{review:?}");
    let review_document: Value = serde_json::from_slice(&review.stdout).expect("review JSON");
    assert_eq!(review_document["verified_completion"], false);
    assert!(
        review_document["completion_blockers"]
            .as_array()
            .expect("completion blockers")
            .iter()
            .any(|blocker| blocker["kind"] == "native_goal_alignment"),
        "{review_document}"
    );
    fixture.assert_unchanged();
}

#[test]
fn continue_returns_a_child_only_manual_goal_handoff_when_migration_is_unavailable() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    fixture
        .environment
        .insert("DRIFTCTL_FAKE_GOAL_SET_UNAVAILABLE", "1".to_owned());

    let continued = fixture.run_continue(&["--json"]);
    assert_eq!(continued.status.code(), Some(2), "{continued:?}");
    let document: Value = serde_json::from_slice(&continued.stdout).expect("manual handoff JSON");
    assert_eq!(document["status"], "manual_goal_handoff_required");
    assert_eq!(document["child_thread_id"], "continued-child");
    assert_eq!(document["observed_goal"]["state"], "unknown");
    assert_eq!(document["intended_goal"], "Ship a usable inspector");
    assert_eq!(document["requires_new_approval"], true);
    assert_eq!(
        document["resume"]["argv"],
        json!(["codex", "resume", "continued-child"])
    );
    assert_eq!(
        document["slash_commands"],
        json!([
            {"command":"/goal clear"},
            {"command":"/goal", "argument":"Ship a usable inspector"}
        ])
    );
    assert_eq!(document["turn_started"], false);
    assert_eq!(document["parent_unchanged"], true);
    assert_eq!(document["source_unchanged"], true);
    let rpc = fixture.rpc_calls();
    assert!(rpc.iter().any(|request| {
        request["method"] == "thread/goal/set" && request["params"]["threadId"] == "continued-child"
    }));
    assert!(!rpc.iter().any(|request| request["method"] == "turn/start"));
    fixture.assert_unchanged();
}

#[test]
fn compare_blocks_when_a_child_mutates_the_source_after_isolation() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    fixture.environment.insert(
        "DRIFTCTL_FAKE_MUTATE_SOURCE_PATH",
        fixture.root.join("README.md").display().to_string(),
    );

    let compared = fixture.run_compare(&["--json"]);
    assert_eq!(compared.status.code(), Some(2), "{compared:?}");
    assert!(
        String::from_utf8_lossy(&compared.stderr).contains("source workspace changed"),
        "{compared:?}"
    );
}

#[test]
fn continue_blocks_when_the_child_mutates_the_source_after_isolation() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    fixture.environment.insert(
        "DRIFTCTL_FAKE_MUTATE_SOURCE_PATH",
        fixture.root.join("README.md").display().to_string(),
    );

    let continued = fixture.run_continue(&["--json"]);
    assert_eq!(continued.status.code(), Some(2), "{continued:?}");
    assert!(
        String::from_utf8_lossy(&continued.stderr).contains("source workspace changed"),
        "{continued:?}"
    );
}

#[test]
fn compare_blocks_when_a_child_mutates_excluded_harness_configuration() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    fs::create_dir(fixture.root.join(".codex")).expect("create excluded harness directory");
    let configuration = fixture.root.join(".codex/config.toml");
    fs::write(&configuration, "sandbox = 'workspace-write'\n")
        .expect("write excluded harness configuration");
    fixture.environment.insert(
        "DRIFTCTL_FAKE_MUTATE_SOURCE_PATH",
        configuration.display().to_string(),
    );

    let compared = fixture.run_compare(&["--json"]);
    assert_eq!(compared.status.code(), Some(2), "{compared:?}");
    assert!(
        String::from_utf8_lossy(&compared.stderr).contains("source workspace changed"),
        "{compared:?}"
    );
}

#[test]
fn continue_blocks_when_the_child_mutates_excluded_harness_configuration() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    fs::create_dir(fixture.root.join(".codex")).expect("create excluded harness directory");
    let configuration = fixture.root.join(".codex/config.toml");
    fs::write(&configuration, "sandbox = 'workspace-write'\n")
        .expect("write excluded harness configuration");
    fixture.environment.insert(
        "DRIFTCTL_FAKE_MUTATE_SOURCE_PATH",
        configuration.display().to_string(),
    );

    let continued = fixture.run_continue(&["--json"]);
    assert_eq!(continued.status.code(), Some(2), "{continued:?}");
    assert!(
        String::from_utf8_lossy(&continued.stderr).contains("source workspace changed"),
        "{continued:?}"
    );
}

#[test]
fn failed_child_turn_never_reports_a_successful_continuation() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    fixture
        .environment
        .insert("DRIFTCTL_FAKE_TURN_STATUS", "failed".to_owned());

    let continued = fixture.run_continue(&["--json"]);
    assert_eq!(continued.status.code(), Some(2), "{continued:?}");
    let document: Value =
        serde_json::from_slice(&continued.stdout).expect("failed continuation JSON");
    assert_eq!(document["status"], "invalid_continuation");
    assert_eq!(document["turn_status"], "failed");
    assert_eq!(document["changed_paths"], json!([]));
    assert_eq!(document["evidence"], json!([]));
    assert_eq!(document["blockers"][0]["kind"], "child_turn_failed");
    assert_eq!(
        document["blockers"][1]["kind"],
        "external_verification_required"
    );
    assert_eq!(document["parent_unchanged"], true);
    assert_eq!(document["source_unchanged"], true);
    assert_eq!(document["adoption"], "none");
    fixture.assert_unchanged();
}

#[test]
fn inspect_compacts_only_the_new_source_delta_and_reuses_the_updated_projection() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    let first = fixture.run(&["--json"]);
    assert_eq!(first.status.code(), Some(0), "{first:?}");
    let first_document: Value = serde_json::from_slice(&first.stdout).expect("first inspect JSON");
    let active_ids = first_document["projection"]["preserve"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            first_document["projection"]["frontier"]
                .as_array()
                .into_iter()
                .flatten(),
        )
        .chain(
            first_document["projection"]["validation"]
                .as_array()
                .into_iter()
                .flatten(),
        )
        .map(|item| item["id"].clone())
        .collect::<Vec<_>>();
    let revision = first_document["projection"]["revision"]
        .as_u64()
        .expect("projection revision");
    let incremental = json!({
        "schema_version": 1,
        "base_projection_revision": revision,
        "base_event_sequence": revision,
        "classification": "additive",
        "accounted_active_intent_ids": active_ids,
        "accounted_source_record_ids": ["u4:0"],
        "operations": [{
            "operation": "add",
            "key": "preserve-install-boundary",
            "kind": "invariant",
            "text": "Preserve the installation boundary",
            "target_intent_id": "",
            "intent_ids": [],
            "evidence_id": "",
            "reason": "",
            "source_record_ids": ["u4:0"],
            "alternatives": []
        }]
    });
    let canonical = fixture.root.canonicalize().expect("canonical source");
    fixture.environment.insert(
        "DRIFTCTL_FAKE_READ",
        session(
            &fixture.session_id,
            &canonical,
            &[
                ("u1", "private raw goal that public output must not echo"),
                ("u2", "private additive steering"),
                ("u3", "private explicit supersession"),
                ("u4", "RAW_ONLY_NEW_DELTA preserve the install boundary"),
            ],
        )
        .to_string(),
    );
    fixture.environment.insert(
        "DRIFTCTL_FAKE_PROPOSALS",
        json!([base_proposal(), incremental]).to_string(),
    );

    let second = fixture.run(&["--json"]);
    assert_eq!(second.status.code(), Some(0), "{second:?}");
    let second_document: Value =
        serde_json::from_slice(&second.stdout).expect("second inspect JSON");
    assert!(
        second_document["projection"]["frontier"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["text"] == "Preserve the installation boundary")
    );
    assert!(second_document["projection"]["revision"].as_u64().unwrap() > revision);

    let calls = fixture.calls();
    assert_eq!(calls.len(), 2);
    let incremental_prompt = &calls[1]["prompt"];
    assert_eq!(
        incremental_prompt["protocol"],
        "driftctl.semantic-incremental-proposal.v1"
    );
    assert_eq!(
        incremental_prompt["delta_records"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(incremental_prompt["delta_records"][0]["id"], "u4:0");
    assert!(
        incremental_prompt
            .to_string()
            .contains("RAW_ONLY_NEW_DELTA")
    );
    assert!(!incremental_prompt.to_string().contains("private raw goal"));
    assert!(incremental_prompt.get("history").is_none());

    let third = fixture.run(&["--json"]);
    assert_eq!(third.status.code(), Some(0), "{third:?}");
    let third_document: Value = serde_json::from_slice(&third.stdout).expect("third inspect JSON");
    assert_eq!(third_document["projection"], second_document["projection"]);
    assert_eq!(fixture.calls().len(), 2);
    fixture.assert_unchanged();
}

#[test]
fn pending_goal_change_survives_cached_inspect_without_mutating_the_accepted_goal() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    let first = fixture.run(&["--json"]);
    assert_eq!(first.status.code(), Some(0), "{first:?}");
    let first_document: Value = serde_json::from_slice(&first.stdout).expect("first inspect JSON");
    let accepted_goal = first_document["projection"]["goal"]["text"].clone();
    let active_ids = first_document["projection"]["preserve"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            first_document["projection"]["frontier"]
                .as_array()
                .into_iter()
                .flatten(),
        )
        .chain(
            first_document["projection"]["validation"]
                .as_array()
                .into_iter()
                .flatten(),
        )
        .map(|item| item["id"].clone())
        .collect::<Vec<_>>();
    let revision = first_document["projection"]["revision"].as_u64().unwrap();
    let proposal = json!({
        "schema_version":1,
        "base_projection_revision":revision,
        "base_event_sequence":revision,
        "classification":"goal_change",
        "accounted_active_intent_ids":active_ids,
        "accounted_source_record_ids":["u4:0"],
        "operations":[],
        "proposed_goal":{
            "text":"Ship the replacement objective",
            "source_record_ids":["u4:0"]
        }
    });
    let canonical = fixture.root.canonicalize().expect("canonical source");
    fixture.environment.insert(
        "DRIFTCTL_FAKE_READ",
        session(
            &fixture.session_id,
            &canonical,
            &[
                ("u1", "private raw goal that public output must not echo"),
                ("u2", "private additive steering"),
                ("u3", "private explicit supersession"),
                ("u4", "replace the overall goal with the new objective"),
            ],
        )
        .to_string(),
    );
    fixture.environment.insert(
        "DRIFTCTL_FAKE_PROPOSALS",
        json!([base_proposal(), proposal]).to_string(),
    );

    let second = fixture.run(&["--json"]);
    assert_eq!(second.status.code(), Some(2), "{second:?}");
    let second_document: Value = serde_json::from_slice(&second.stdout).expect("proposal JSON");
    assert_eq!(second_document["status"], "blocked");
    assert_eq!(
        second_document["goal_change"]["proposed_goal"],
        "Ship the replacement objective"
    );
    assert_eq!(second_document["projection"]["goal"]["text"], accepted_goal);

    let third = fixture.run(&["--json"]);
    assert_eq!(third.status.code(), Some(2), "{third:?}");
    let third_document: Value =
        serde_json::from_slice(&third.stdout).expect("cached proposal JSON");
    assert_eq!(
        third_document["goal_change"],
        second_document["goal_change"]
    );
    assert_eq!(third_document["projection"]["goal"]["text"], accepted_goal);
    assert_eq!(fixture.calls().len(), 2);

    let run_id = second_document["run_id"].as_str().expect("run id");
    let bundle = fixture.run_bundle(run_id);
    assert_eq!(bundle.status.code(), Some(0), "{bundle:?}");
    let bundle_document: Value = serde_json::from_slice(&bundle.stdout).expect("bundle JSON");
    assert_eq!(bundle_document["status"], "blocked");
    assert!(
        bundle_document["blockers"]
            .as_array()
            .is_some_and(|blockers| {
                blockers
                    .iter()
                    .any(|blocker| blocker["kind"] == "goal_change_pending")
            })
    );
    assert!(!bundle_document.to_string().contains(&fixture.session_id));
    fixture.assert_unchanged();
}

#[test]
fn continue_requires_operator_authority_then_migrates_only_an_isolated_child() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    let first = fixture.run(&["--json"]);
    assert_eq!(first.status.code(), Some(0), "{first:?}");
    let first_document: Value = serde_json::from_slice(&first.stdout).expect("initial JSON");
    let old_goal = first_document["projection"]["goal"]["text"].clone();
    let revision = first_document["projection"]["revision"]
        .as_u64()
        .expect("projection revision");
    let active_ids = first_document["projection"]["preserve"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            first_document["projection"]["frontier"]
                .as_array()
                .into_iter()
                .flatten(),
        )
        .chain(
            first_document["projection"]["validation"]
                .as_array()
                .into_iter()
                .flatten(),
        )
        .map(|item| item["id"].clone())
        .collect::<Vec<_>>();
    let proposal = json!({
        "schema_version":1,
        "base_projection_revision":revision,
        "base_event_sequence":revision,
        "classification":"goal_change",
        "accounted_active_intent_ids":active_ids,
        "accounted_source_record_ids":["u4:0"],
        "operations":[],
        "proposed_goal":{
            "text":"Ship the operator-approved replacement objective",
            "source_record_ids":["u4:0"]
        }
    });
    let canonical = fixture.root.canonicalize().expect("canonical source");
    fixture.environment.insert(
        "DRIFTCTL_FAKE_READ",
        session(
            &fixture.session_id,
            &canonical,
            &[
                ("u1", "private raw goal that public output must not echo"),
                ("u2", "private additive steering"),
                ("u3", "private explicit supersession"),
                ("u4", "replace the overall goal with the approved objective"),
            ],
        )
        .to_string(),
    );
    fixture.environment.insert(
        "DRIFTCTL_FAKE_PROPOSALS",
        json!([base_proposal(), proposal]).to_string(),
    );
    let pending = fixture.run(&["--json"]);
    assert_eq!(pending.status.code(), Some(2), "{pending:?}");

    let edited = fixture.run_continue(&[
        "--edit-goal",
        "Ship the operator-edited replacement objective",
        "--json",
    ]);
    assert_eq!(edited.status.code(), Some(2), "{edited:?}");
    let edited_document: Value =
        serde_json::from_slice(&edited.stdout).expect("edited proposal JSON");
    assert_eq!(
        edited_document["goal_change"]["proposed_goal"],
        "Ship the operator-edited replacement objective"
    );
    let cancelled = fixture.run_continue(&["--cancel", "--json"]);
    assert_eq!(cancelled.status.code(), Some(2), "{cancelled:?}");
    let cancelled_document: Value =
        serde_json::from_slice(&cancelled.stdout).expect("cancelled continuation JSON");
    assert_eq!(
        cancelled_document["goal_change"],
        edited_document["goal_change"]
    );

    let unattended = fixture.run_continue(&["--json"]);
    assert_eq!(unattended.status.code(), Some(2), "{unattended:?}");
    assert!(String::from_utf8_lossy(&unattended.stderr).contains("operator decision required"));
    assert!(!fixture.rpc_calls().iter().any(|request| {
        request["method"] == "thread/fork"
            || request["method"] == "thread/goal/clear"
            || request["method"] == "thread/goal/set"
    }));

    let approved = fixture.run_continue(&["--approve-goal", "--json"]);
    assert_eq!(approved.status.code(), Some(0), "{approved:?}");
    let approved_document: Value =
        serde_json::from_slice(&approved.stdout).expect("approved continuation JSON");
    assert_eq!(approved_document["status"], "started");
    assert_eq!(approved_document["child_thread_id"], "continued-child");
    assert_eq!(approved_document["turn_status"], "completed");
    assert_eq!(approved_document["changed_paths"], json!([]));
    assert_eq!(approved_document["evidence"], json!([]));
    assert_eq!(
        approved_document["blockers"][0]["kind"],
        "external_verification_required"
    );
    assert_eq!(approved_document["parent_unchanged"], true);
    assert_eq!(approved_document["source_unchanged"], true);
    assert_eq!(approved_document["adoption"], "manual");
    assert!(
        approved_document["containment"]
            .as_str()
            .expect("containment notice")
            .contains("host-wide")
    );
    assert_ne!(approved_document["goal"], old_goal);

    let rpc = fixture.rpc_calls();
    assert!(rpc.iter().any(|request| request["method"] == "thread/fork"));
    assert!(rpc.iter().any(|request| {
        request["method"] == "thread/goal/set"
            && request["params"]["threadId"] == "continued-child"
            && request["params"]["objective"] == "Ship the operator-edited replacement objective"
    }));
    assert!(!rpc.iter().any(|request| {
        matches!(
            request["method"].as_str(),
            Some("thread/goal/clear" | "thread/goal/set")
        ) && request["params"]["threadId"] == fixture.session_id
    }));

    let accepted = fixture.run(&["--json"]);
    assert_eq!(accepted.status.code(), Some(0), "{accepted:?}");
    let accepted_document: Value =
        serde_json::from_slice(&accepted.stdout).expect("accepted inspect JSON");
    assert_eq!(accepted_document["goal_change"], Value::Null);
    assert_eq!(
        accepted_document["projection"]["goal"]["text"],
        "Ship the operator-edited replacement objective"
    );
    assert_eq!(fixture.calls().len(), 2);
    fixture.assert_unchanged();
}

#[test]
fn retaining_the_current_goal_rejects_new_steering_and_continues_the_child() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    let first =
        prepare_pending_goal_change(&mut fixture, "This replacement must be explicitly rejected");
    let accepted_goal = first["projection"]["goal"]["text"]
        .as_str()
        .expect("accepted goal")
        .to_owned();

    let retained = fixture.run_continue(&["--retain-goal", "--json"]);
    assert_eq!(retained.status.code(), Some(0), "{retained:?}");
    let retained_document: Value =
        serde_json::from_slice(&retained.stdout).expect("retained continuation JSON");
    assert_eq!(retained_document["status"], "started");
    assert_eq!(retained_document["goal"], accepted_goal);
    assert!(fixture.rpc_calls().iter().any(|request| {
        request["method"] == "thread/goal/set"
            && request["params"]["threadId"] == "continued-child"
            && request["params"]["objective"] == accepted_goal
    }));

    let inspected = fixture.run(&["--json"]);
    assert_eq!(inspected.status.code(), Some(0), "{inspected:?}");
    let inspected_document: Value =
        serde_json::from_slice(&inspected.stdout).expect("retained inspect JSON");
    assert_eq!(inspected_document["goal_change"], Value::Null);
    assert_eq!(
        inspected_document["projection"]["goal"]["text"],
        accepted_goal
    );
    assert_eq!(fixture.calls().len(), 2);
    fixture.assert_unchanged();
}

#[test]
fn interrupted_goal_proposal_commit_recovers_without_another_model_call() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    let first = fixture.run(&["--json"]);
    assert_eq!(first.status.code(), Some(0), "{first:?}");
    let first_document: Value = serde_json::from_slice(&first.stdout).expect("initial JSON");
    let revision = first_document["projection"]["revision"]
        .as_u64()
        .expect("projection revision");
    let active_ids = first_document["projection"]["preserve"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            first_document["projection"]["frontier"]
                .as_array()
                .into_iter()
                .flatten(),
        )
        .chain(
            first_document["projection"]["validation"]
                .as_array()
                .into_iter()
                .flatten(),
        )
        .map(|item| item["id"].clone())
        .collect::<Vec<_>>();
    let proposal = json!({
        "schema_version":1,
        "base_projection_revision":revision,
        "base_event_sequence":revision,
        "classification":"goal_change",
        "accounted_active_intent_ids":active_ids,
        "accounted_source_record_ids":["u4:0"],
        "operations":[],
        "proposed_goal":{
            "text":"Recover the interrupted replacement objective",
            "source_record_ids":["u4:0"]
        }
    });
    let canonical = fixture.root.canonicalize().expect("canonical source");
    fixture.environment.insert(
        "DRIFTCTL_FAKE_READ",
        session(
            &fixture.session_id,
            &canonical,
            &[
                ("u1", "private raw goal that public output must not echo"),
                ("u2", "private additive steering"),
                ("u3", "private explicit supersession"),
                ("u4", "replace the overall goal after this interruption"),
            ],
        )
        .to_string(),
    );
    fixture.environment.insert(
        "DRIFTCTL_FAKE_PROPOSALS",
        json!([base_proposal(), proposal]).to_string(),
    );

    let source_path = fixture.state_file("source.json");
    let accepted_source = fs::read(&source_path).expect("read accepted source cursor");
    fixture.environment.insert(
        "DRIFTCTL_FAKE_BREAK_SOURCE_CURSOR",
        source_path.display().to_string(),
    );
    let interrupted = fixture.run(&["--json"]);
    assert_eq!(interrupted.status.code(), Some(1), "{interrupted:?}");
    assert!(
        source_path.is_dir(),
        "fake boundary interrupted cursor commit"
    );

    fs::remove_dir(&source_path).expect("remove interrupted cursor directory");
    fs::write(&source_path, accepted_source).expect("restore last durable cursor");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&source_path, fs::Permissions::from_mode(0o600))
            .expect("restore private cursor mode");
    }
    fixture
        .environment
        .remove("DRIFTCTL_FAKE_BREAK_SOURCE_CURSOR");

    let recovered = fixture.run(&["--json"]);
    assert_eq!(recovered.status.code(), Some(2), "{recovered:?}");
    let recovered_document: Value =
        serde_json::from_slice(&recovered.stdout).expect("recovered JSON");
    assert_eq!(
        recovered_document["goal_change"]["proposed_goal"],
        "Recover the interrupted replacement objective"
    );
    assert_eq!(fixture.calls().len(), 2, "recovery must not call the model");

    let cached = fixture.run(&["--json"]);
    assert_eq!(cached.status.code(), Some(2), "{cached:?}");
    assert_eq!(fixture.calls().len(), 2);
    fixture.assert_unchanged();
}

#[test]
fn initial_inspect_chunks_a_large_session_before_any_provider_call() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    let canonical = fixture.root.canonicalize().expect("canonical source");
    let first = format!("FIRST_CHUNK_ONLY {}", "a".repeat(35_000));
    let second = format!("SECOND_CHUNK_ONLY {}", "b".repeat(35_000));
    let third = format!("THIRD_CHUNK_ONLY {}", "c".repeat(35_000));
    fixture.environment.insert(
        "DRIFTCTL_FAKE_READ",
        session(
            &fixture.session_id,
            &canonical,
            &[
                ("large-1", &first),
                ("large-2", &second),
                ("large-3", &third),
            ],
        )
        .to_string(),
    );
    fixture
        .environment
        .insert("DRIFTCTL_FAKE_DYNAMIC_CHUNKS", "1".to_owned());

    let output = fixture.run(&["--json"]);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let disclosure = String::from_utf8_lossy(&output.stderr);
    assert!(disclosure.contains("expected calls: 3; maximum 6 with repair"));
    let document: Value = serde_json::from_slice(&output.stdout).expect("inspect JSON");
    assert_eq!(document["resolver"]["calls"], 3);
    assert_eq!(
        document["projection"]["frontier"].as_array().map(Vec::len),
        Some(3)
    );

    let calls = fixture.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls[0]["prompt"]["protocol"],
        "driftctl.semantic-proposal.v1"
    );
    assert_eq!(
        calls[1]["prompt"]["protocol"],
        "driftctl.semantic-incremental-proposal.v1"
    );
    assert_eq!(
        calls[2]["prompt"]["protocol"],
        "driftctl.semantic-incremental-proposal.v1"
    );
    assert!(calls[0]["prompt"].to_string().contains("FIRST_CHUNK_ONLY"));
    assert!(!calls[0]["prompt"].to_string().contains("SECOND_CHUNK_ONLY"));
    assert!(calls[1]["prompt"].to_string().contains("SECOND_CHUNK_ONLY"));
    assert!(!calls[1]["prompt"].to_string().contains("FIRST_CHUNK_ONLY"));
    assert!(calls[2]["prompt"].to_string().contains("THIRD_CHUNK_ONLY"));
    assert!(!calls[2]["prompt"].to_string().contains("SECOND_CHUNK_ONLY"));
    fixture.assert_unchanged();
}

#[test]
fn inspect_invokes_luna_max_read_only_ephemeral_and_returns_a_sanitized_projection() {
    let fixture = Fixture::new(vec![base_proposal()]);

    let output = fixture.run(&["--json"]);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let disclosure = String::from_utf8_lossy(&output.stderr);
    assert!(disclosure.contains("gpt-5.6-luna"));
    assert!(disclosure.contains("reasoning: max"));
    assert!(disclosure.contains("expected calls: 1; maximum 2 with repair"));
    assert!(disclosure.contains("local Codex authentication and usage allowance"));
    let document: Value = serde_json::from_slice(&output.stdout).expect("one JSON document");
    assert_eq!(document["status"], "usable");
    assert_eq!(
        document["projection"]["goal"]["text"],
        "Ship a usable inspector"
    );
    assert_eq!(
        document["projection"]["frontier"].as_array().unwrap().len(),
        1
    );
    assert_eq!(document["projection"]["frontier"][0]["text"], "Emit YAML");
    assert_eq!(document["resolver"]["calls"], 1);
    assert_eq!(document["resolver"]["usage"]["input_tokens"], 101);
    let artifact_ids = document["resolver"]["artifact_ids"]
        .as_array()
        .expect("sanitized artifact IDs");
    assert_eq!(artifact_ids.len(), 1);
    assert!(
        artifact_ids[0]
            .as_str()
            .unwrap()
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    );
    let public = document.to_string();
    assert!(!public.contains(&fixture.session_id));
    assert!(!public.contains(&fixture.root.display().to_string()));
    assert!(!public.contains("private raw goal"));
    assert!(!public.contains("private additive steering"));
    assert!(!public.contains(&fixture.artifacts.display().to_string()));

    let calls = fixture.calls();
    assert_eq!(calls.len(), 1);
    let arguments = calls[0]["args"].as_array().expect("arguments");
    assert_eq!(arguments.len(), 16);
    let expected_prefix = json!([
        "exec",
        "--model",
        "gpt-5.6-luna",
        "--config",
        "model_reasoning_effort=\"max\"",
        "--sandbox",
        "read-only",
        "--ephemeral",
        "--json",
        "--output-schema"
    ]);
    assert_eq!(&arguments[..10], expected_prefix.as_array().unwrap());
    assert!(
        arguments
            .iter()
            .any(|argument| argument == "--output-last-message")
    );
    assert!(arguments.iter().any(|argument| argument == "-C"));
    assert_eq!(
        arguments[14],
        fixture.root.canonicalize().unwrap().display().to_string()
    );
    assert_eq!(arguments.last(), Some(&Value::String("-".to_owned())));
    assert_eq!(calls[0]["schema"]["additionalProperties"], false);
    assert_eq!(calls[0]["api_key_environment_present"], false);
    assert_eq!(calls[0]["prompt"]["records"].as_array().unwrap().len(), 3);
    assert_eq!(
        calls[0]["prompt"]["records"][0]["content"],
        "private raw goal that public output must not echo"
    );
    let artifacts = fixture.retained_artifacts();
    assert_eq!(
        artifacts.len(),
        2,
        "one JSONL and one final message per call"
    );
    assert!(
        artifacts
            .iter()
            .any(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"))
    );
    assert!(artifacts.iter().any(|path| {
        path.file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.ends_with(".final.json"))
    }));
    let retained = artifacts
        .iter()
        .map(|path| fs::read_to_string(path).expect("read retained private artifact"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(retained.contains("turn.completed"));
    assert!(retained.contains("Ship a usable inspector"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&fixture.artifacts)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for path in artifacts {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    fixture.assert_unchanged();
}

#[test]
fn inspect_blocks_on_a_native_goal_mismatch_without_exposing_the_parent_goal() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    fixture.environment.insert(
        "DRIFTCTL_FAKE_GOAL",
        json!({
            "goal":{
                "threadId":fixture.session_id,
                "objective":"PRIVATE_PARENT_NATIVE_GOAL",
                "status":"active",
                "tokenBudget":null,
                "tokensUsed":0,
                "timeUsedSeconds":0,
                "createdAt":1,
                "updatedAt":1
            }
        })
        .to_string(),
    );

    let output = fixture.run(&["--json"]);

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let document: Value = serde_json::from_slice(&output.stdout).expect("inspect JSON");
    assert_eq!(document["status"], "blocked");
    assert_eq!(document["native_goal"]["state"], "known");
    assert_eq!(document["native_goal"]["conflicts_with_projection"], true);
    assert!(
        document["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| { blocker["kind"] == "native_goal_conflict" })
    );
    assert!(!document.to_string().contains("PRIVATE_PARENT_NATIVE_GOAL"));
    fixture.assert_unchanged();
}

#[test]
fn inspect_supports_terra_high_medium_and_explicit_sol_without_arbitrary_models() {
    let terra = Fixture::new(vec![base_proposal()]);
    assert_eq!(
        terra.run(&["--compactor", "terra", "--json"]).status.code(),
        Some(0)
    );
    let terra_args = terra.calls()[0]["args"].clone();
    assert!(has_argument_pair(&terra_args, "--model", "gpt-5.6-terra"));
    assert!(has_argument_pair(
        &terra_args,
        "--config",
        "model_reasoning_effort=\"high\""
    ));

    let medium = Fixture::new(vec![base_proposal()]);
    assert_eq!(
        medium
            .run(&["--compactor", "terra", "--reasoning", "medium", "--json"])
            .status
            .code(),
        Some(0)
    );
    assert!(has_argument_pair(
        &medium.calls()[0]["args"],
        "--config",
        "model_reasoning_effort=\"medium\""
    ));

    let sol = Fixture::new(vec![base_proposal()]);
    assert_eq!(
        sol.run(&["--compactor", "sol", "--json"]).status.code(),
        Some(0)
    );
    assert!(has_argument_pair(
        &sol.calls()[0]["args"],
        "--model",
        "gpt-5.6-sol"
    ));

    let arbitrary = Fixture::new(vec![base_proposal()]);
    let rejected = arbitrary.run(&["--compactor", "gpt-unknown", "--json"]);
    assert_eq!(rejected.status.code(), Some(1));
    assert!(!arbitrary.capture.exists());
}

#[test]
fn ambiguous_proposal_returns_an_unresolved_conflict_and_exit_two() {
    let mut proposal = base_proposal();
    proposal["operations"] = json!([
        {"operation":"add","key":"format","kind":"constraint","text":"Choose an output format","target_key":"","intent_keys":[],"source_record_ids":["u2:0"],"alternatives":[]},
        {"operation":"conflict","key":"format-choice","kind":"constraint","text":"Output format is ambiguous","target_key":"","intent_keys":["format"],"source_record_ids":["u3:0"],"alternatives":[
            {"key":"json","text":"Emit JSON","source_record_ids":["u2:0"]},
            {"key":"yaml","text":"Emit YAML","source_record_ids":["u3:0"]}
        ]}
    ]);
    let fixture = Fixture::new(vec![proposal]);

    let output = fixture.run(&["--json"]);

    assert_eq!(output.status.code(), Some(2));
    let document: Value = serde_json::from_slice(&output.stdout).expect("blocked JSON");
    assert_eq!(document["status"], "blocked");
    assert_eq!(
        document["projection"]["conflicts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(document["blockers"][0]["kind"], "conflict");

    let conflict = &document["projection"]["conflicts"][0];
    let conflict_id = conflict["id"].as_str().expect("conflict ID");
    let alternative_id = conflict["alternatives"][0]["id"]
        .as_str()
        .expect("alternative ID");

    let unattended = fixture.run_continue(&["--json"]);
    assert_eq!(unattended.status.code(), Some(2), "{unattended:?}");
    assert!(
        String::from_utf8_lossy(&unattended.stderr).contains("--resolve-conflict"),
        "{unattended:?}"
    );
    assert!(!fixture.rpc_calls().iter().any(|request| {
        matches!(
            request["method"].as_str(),
            Some("thread/fork" | "turn/start")
        )
    }));

    let rejected = fixture.run_continue(&[
        "--resolve-conflict",
        conflict_id,
        "not-an-alternative",
        "--json",
    ]);
    assert_eq!(rejected.status.code(), Some(1), "{rejected:?}");
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("does not belong"),
        "{rejected:?}"
    );
    assert!(!fixture.rpc_calls().iter().any(|request| {
        matches!(
            request["method"].as_str(),
            Some("thread/fork" | "turn/start")
        )
    }));

    let continued =
        fixture.run_continue(&["--resolve-conflict", conflict_id, alternative_id, "--json"]);
    assert_eq!(continued.status.code(), Some(0), "{continued:?}");
    let continued_document: Value =
        serde_json::from_slice(&continued.stdout).expect("continued JSON");
    assert_eq!(continued_document["status"], "started");
    assert_eq!(continued_document["turn_status"], "completed");

    let accepted = fixture.run(&["--json"]);
    assert_eq!(accepted.status.code(), Some(0), "{accepted:?}");
    let accepted_document: Value = serde_json::from_slice(&accepted.stdout).expect("accepted JSON");
    assert_eq!(accepted_document["projection"]["conflicts"], json!([]));
    assert!(
        accepted_document["projection"]["frontier"]
            .as_array()
            .expect("frontier")
            .iter()
            .any(|item| item["text"] == "Emit JSON")
    );
    let rpc = fixture.rpc_calls();
    let turn = rpc
        .iter()
        .find(|request| request["method"] == "turn/start")
        .expect("child turn");
    let prompt = turn["params"]["input"][0]["text"]
        .as_str()
        .expect("child prompt");
    assert!(prompt.contains("Emit JSON"));
    assert!(!prompt.contains("Choose an output format"));
    fixture.assert_unchanged();
}

#[test]
fn invalid_reference_is_repaired_once_and_two_invalid_results_stop() {
    let mut invented = base_proposal();
    invented["goal"]["source_record_ids"] = json!(["invented-record"]);
    let repaired = Fixture::new(vec![invented.clone(), base_proposal()]);

    let output = repaired.run(&["--json"]);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(repaired.calls().len(), 2);
    let document: Value = serde_json::from_slice(&output.stdout).expect("repaired JSON");
    assert_eq!(document["resolver"]["calls"], 2);

    let failed = Fixture::new(vec![invented.clone(), invented, base_proposal()]);
    let output = failed.run(&["--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(failed.calls().len(), 2);
    let document: Value = serde_json::from_slice(&output.stdout).expect("failure JSON");
    assert_eq!(document["status"], "error");
    assert_eq!(document["error"], "invalid_compactor_proposal");
    assert_eq!(
        document["resolver"]["artifact_ids"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(!document.to_string().contains("private raw goal"));
    failed.assert_unchanged();
}

#[test]
fn structural_failures_are_precise_without_exposing_retained_content() {
    let mut irrelevant = base_proposal();
    irrelevant["operations"][0]["alternatives"] = json!([{
        "key":"not-valid-for-add",
        "text":"not valid for add",
        "source_record_ids":["u2:0"]
    }]);
    let fixture = Fixture::new(vec![irrelevant.clone(), irrelevant]);

    let output = fixture.run(&["--json"]);

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let document: Value = serde_json::from_slice(&output.stdout).expect("failure JSON");
    assert_eq!(
        document["resolver"]["last_validation_failure"],
        "add_irrelevant_fields"
    );
    assert_eq!(
        document["resolver"]["artifact_ids"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let public = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!public.contains("not-valid-for-add"));
    assert!(!public.contains(&fixture.artifacts.display().to_string()));
    assert_eq!(fixture.retained_artifacts().len(), 4);
}

#[test]
fn malformed_output_gets_one_repair_and_never_leaks_raw_model_or_source_text() {
    let fixture = Fixture::new(vec![
        Value::String("not json; private raw goal that public output must not echo".to_owned()),
        base_proposal(),
    ]);

    let output = fixture.run(&["--json"]);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(fixture.calls().len(), 2);
    let public = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!public.contains("private raw goal"));
    assert!(!public.contains("not json"));
    fixture.assert_unchanged();
}

#[test]
fn projection_overflow_is_observable_and_blocks_with_exit_two() {
    let mut fixture = Fixture::new(vec![base_proposal()]);
    fixture
        .environment
        .insert("DRIFTCTL_PROJECTION_BYTE_BUDGET", "64".to_owned());

    let output = fixture.run(&["--json"]);

    assert_eq!(output.status.code(), Some(2));
    let document: Value = serde_json::from_slice(&output.stdout).expect("overflow JSON");
    assert_eq!(document["status"], "blocked");
    assert_eq!(document["projection"]["overflow"]["budget"], 64);
    assert!(document["projection"]["overflow"]["reason"].is_string());
    assert!(
        document["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker["kind"] == "projection_overflow")
    );
    fixture.assert_unchanged();
}

#[test]
fn withdrawal_stays_in_history_and_validation_scope_and_stop_conditions_project_correctly() {
    let proposal = json!({
        "schema_version":1,
        "goal":{"text":"Ship the inspector","source_record_ids":["u1:0"]},
        "accounted_source_record_ids":["u1:0","u2:0","u3:0"],
        "operations":[
            {"operation":"add","key":"temporary","kind":"constraint","text":"Use a temporary format","target_key":"","intent_keys":[],"source_record_ids":["u2:0"],"alternatives":[]},
            {"operation":"withdraw","key":"","kind":"constraint","text":"","target_key":"temporary","intent_keys":[],"source_record_ids":["u3:0"],"alternatives":[]},
            {"operation":"add","key":"scope","kind":"scope","text":"Inspect only","target_key":"","intent_keys":[],"source_record_ids":["u2:0"],"alternatives":[]},
            {"operation":"add","key":"validate","kind":"validation","text":"Run the shipped CLI","target_key":"","intent_keys":[],"source_record_ids":["u3:0"],"alternatives":[]},
            {"operation":"add","key":"stop","kind":"stop_condition","text":"Stop after observable success","target_key":"","intent_keys":[],"source_record_ids":["u3:0"],"alternatives":[]}
        ]
    });
    let fixture = Fixture::new(vec![proposal]);

    let output = fixture.run(&["--json"]);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let document: Value = serde_json::from_slice(&output.stdout).expect("inspect JSON");
    assert!(document["history"].as_array().unwrap().iter().any(|item| {
        item["text"] == "Use a temporary format" && item["lifecycle"] == "withdrawn"
    }));
    assert!(
        !document["projection"]["frontier"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["text"] == "Use a temporary format")
    );
    assert!(
        document["projection"]["frontier"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["kind"] == "scope")
    );
    assert_eq!(
        document["projection"]["validation"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn duplicate_unaccounted_unsupported_empty_and_illegal_transitions_never_commit() {
    let mut duplicate = base_proposal();
    duplicate["accounted_source_record_ids"] = json!(["u1:0", "u2:0", "u2:0"]);
    let mut unaccounted = base_proposal();
    unaccounted["accounted_source_record_ids"] = json!(["u1:0", "u2:0"]);
    let mut unsupported = base_proposal();
    unsupported["operations"][0]["operation"] = json!("rewrite_everything");
    let mut empty = base_proposal();
    empty["operations"][0]["text"] = json!("");
    let mut illegal = base_proposal();
    illegal["operations"][1]["target_key"] = json!("missing-target");

    for invalid in [duplicate, unaccounted, unsupported, empty, illegal] {
        let fixture = Fixture::new(vec![invalid.clone(), invalid]);
        let output = fixture.run(&["--json"]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(fixture.calls().len(), 2);
        let document: Value = serde_json::from_slice(&output.stdout).expect("failure JSON");
        assert_eq!(document["error"], "invalid_compactor_proposal");
        fixture.assert_unchanged();
    }
}
