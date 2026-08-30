use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

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

if sys.argv[1:3] == ["app-server", "--stdio"]:
    for raw in sys.stdin:
        request = json.loads(raw)
        method = request.get("method")
        if method == "initialized":
            continue
        if method == "initialize":
            result = {"userAgent":"fake","codexHome":"/private","platformFamily":"unix","platformOs":"linux"}
        elif method == "thread/list":
            result = {"data":[],"nextCursor":None,"backwardsCursor":None}
        elif method == "thread/read":
            result = json.loads(os.environ["DRIFTCTL_FAKE_READ"])
        elif method == "thread/goal/get":
            result = json.loads(os.environ.get("DRIFTCTL_FAKE_GOAL", '{"goal":null}'))
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
            artifacts,
            state_home,
            session_id,
        }
    }

    fn run(&self, options: &[&str]) -> Output {
        let mut arguments = vec!["inspect", "codex", "--session", &self.session_id];
        arguments.extend_from_slice(options);
        let mut command = Command::new(env!("CARGO_BIN_EXE_driftctl"));
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_driftctl"));
        command
            .current_dir(&self.root)
            .args(["bundle", "--run", run_id, "--json"]);
        for (name, value) in &self.environment {
            command.env(name, value);
        }
        command.output().expect("run driftctl bundle")
    }

    fn calls(&self) -> Vec<Value> {
        fs::read_to_string(&self.capture)
            .expect("read exec capture")
            .lines()
            .map(|line| serde_json::from_str(line).expect("capture JSON"))
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
    irrelevant["operations"][0]["target_key"] = json!("not-valid-for-add");
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
