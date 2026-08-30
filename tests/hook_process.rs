use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

fn temporary_directory(case: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "driftctl-hook-{case}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&path).expect("create isolated test directory");
    path
}

fn temporary_home_directory(case: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    let path = Path::new("/home/rutts").join(format!(
        ".driftctl-hook-{case}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&path).expect("create isolated home test directory");
    path
}

fn run_hook(root: &Path, state_home: &Path, input: &Value) -> Output {
    let mut environment = BTreeMap::new();
    environment.insert("XDG_STATE_HOME", state_home.display().to_string());
    run_hook_with_environment(root, &environment, input)
}

fn run_hook_with_environment(
    root: &Path,
    environment: &BTreeMap<&str, String>,
    input: &Value,
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_driftctl"));
    command
        .current_dir(root)
        .args(["hook", "codex"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in environment {
        command.env(name, value);
    }
    let mut child = command.spawn().expect("start driftctl hook");
    child
        .stdin
        .take()
        .expect("hook stdin")
        .write_all(input.to_string().as_bytes())
        .expect("write hook input");
    child.wait_with_output().expect("finish driftctl hook")
}

fn projection_from_hook(output: &Output) -> Value {
    let hook: Value = serde_json::from_slice(&output.stdout).expect("hook output JSON");
    let context = hook["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("hook additional context");
    serde_json::from_str(context.lines().last().expect("projection line"))
        .expect("injected projection JSON")
}

fn additional_context(output: &Output) -> String {
    let hook: Value = serde_json::from_slice(&output.stdout).expect("hook output JSON");
    hook["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("hook additional context")
        .to_owned()
}

fn fake_keeper_transcript(environment: &BTreeMap<&str, String>) -> Vec<u8> {
    fs::read(&environment["DRIFTCTL_FAKE_PROMPTS"]).unwrap_or_default()
}

fn private_state_document(environment: &BTreeMap<&str, String>, name: &str) -> Value {
    let mut pending = vec![PathBuf::from(&environment["XDG_STATE_HOME"]).join("driftctl")];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read private state directory") {
            let path = entry.expect("private state entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|candidate| candidate == name) {
                return serde_json::from_slice(&fs::read(path).expect("read private state file"))
                    .expect("private state JSON");
            }
        }
    }
    panic!("private state file {name} not found")
}

fn run_cli(root: &Path, arguments: &[&str], environment: &BTreeMap<&str, String>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_driftctl"));
    command.current_dir(root).args(arguments);
    for (name, value) in environment {
        command.env(name, value);
    }
    command.output().expect("run driftctl CLI")
}

fn write_fake_codex(root: &Path) -> PathBuf {
    let program = root.join("fake-codex.py");
    fs::write(
        &program,
        r#"#!/usr/bin/env python3
import json
import os
import sys

if sys.argv[1] == "exec":
    prompt = sys.stdin.read()
    with open(os.environ["DRIFTCTL_FAKE_PROMPTS"], "a", encoding="utf-8") as file:
        file.write(prompt + "\n")
    proposal = os.environ["DRIFTCTL_FAKE_PROPOSAL"]
    last_path = sys.argv[sys.argv.index("--output-last-message") + 1]
    with open(last_path, "w", encoding="utf-8") as file:
        file.write(proposal)
    print(json.dumps({"type":"thread.started","thread_id":"private-compactor"}))
    print(json.dumps({"type":"item.completed","item":{"id":"item","type":"agent_message","text":proposal}}))
    print(json.dumps({"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_output_tokens":1}}))
    sys.exit(0)

for raw in sys.stdin:
    request = json.loads(raw)
    capture = os.environ.get("DRIFTCTL_FAKE_RPC_CAPTURE")
    if capture:
        with open(capture, "a", encoding="utf-8") as file:
            file.write(json.dumps(request, separators=(",", ":")) + "\n")
    method = request.get("method")
    if method == "initialized":
        continue
    if method == "initialize":
        result = {"userAgent":"fake","codexHome":"/not-used","platformFamily":"unix","platformOs":"linux"}
    elif method == "thread/read":
        result = json.loads(os.environ["DRIFTCTL_FAKE_READ"])
        if request["params"]["threadId"] != result["thread"]["id"]:
            sys.stdout.write(json.dumps({"id":request["id"],"error":{"code":-32000,"message":"thread not found"}}) + "\n")
            sys.stdout.flush()
            continue
    elif method == "thread/goal/get":
        state_path = os.environ.get("DRIFTCTL_FAKE_GOAL_STATE")
        objective = None
        if state_path and os.path.exists(state_path):
            with open(state_path, "r", encoding="utf-8") as file:
                objective = json.load(file)
        goal = None if objective is None else {
            "threadId":request["params"]["threadId"],
            "objective":objective,
        }
        drift_after_read = os.environ.get("DRIFTCTL_FAKE_DRIFT_AFTER_GOAL_GET")
        if state_path and drift_after_read and objective is not None:
            with open(state_path, "w", encoding="utf-8") as file:
                json.dump(drift_after_read, file)
        result = {"goal":goal}
    elif method == "thread/goal/clear":
        state_path = os.environ["DRIFTCTL_FAKE_GOAL_STATE"]
        objective = None
        if os.path.exists(state_path):
            with open(state_path, "r", encoding="utf-8") as file:
                objective = json.load(file)
        with open(state_path, "w", encoding="utf-8") as file:
            json.dump(None, file)
        result = {"cleared":objective is not None}
    elif method == "thread/goal/set":
        state_path = os.environ["DRIFTCTL_FAKE_GOAL_STATE"]
        objective = request["params"]["objective"]
        with open(state_path, "w", encoding="utf-8") as file:
            json.dump(objective, file)
        result = {"goal":{
            "threadId":request["params"]["threadId"],
            "objective":objective,
        }}
    else:
        sys.stdout.write(json.dumps({"id":request["id"],"error":{"code":-32601,"message":"unexpected method"}}) + "\n")
        sys.stdout.flush()
        continue
    sys.stdout.write(json.dumps({"id":request["id"],"result":result}) + "\n")
    sys.stdout.flush()
"#,
    )
    .expect("write fake Codex");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755))
            .expect("make fake Codex executable");
    }
    program
}

fn attached_environment(root: &Path, session_id: &str) -> BTreeMap<&'static str, String> {
    let fake_root = temporary_directory("fake-provider");
    let program = write_fake_codex(&fake_root);
    let canonical_root = root.canonicalize().expect("canonical test root");
    let mut environment = BTreeMap::new();
    environment.insert("DRIFTCTL_CODEX_BIN", program.display().to_string());
    environment.insert(
        "DRIFTCTL_ARTIFACT_DIR",
        fake_root.join("artifacts").display().to_string(),
    );
    environment.insert(
        "DRIFTCTL_FAKE_PROMPTS",
        fake_root.join("prompts.jsonl").display().to_string(),
    );
    environment.insert(
        "DRIFTCTL_FAKE_READ",
        json!({
            "thread": {
                "id": session_id,
                "cwd": canonical_root,
                "turns": [{"items": [{
                    "type": "userMessage",
                    "id": "user-1",
                    "content": [{"type":"text","text":"Preserve the existing CLI behavior."}]
                }]}]
            }
        })
        .to_string(),
    );
    let goal_state = fake_root.join("goal-state.json");
    fs::write(&goal_state, "null").expect("initialize fake native goal");
    environment.insert("DRIFTCTL_FAKE_GOAL_STATE", goal_state.display().to_string());
    environment.insert(
        "DRIFTCTL_FAKE_RPC_CAPTURE",
        fake_root.join("rpc.jsonl").display().to_string(),
    );
    environment.insert(
        "DRIFTCTL_FAKE_PROPOSAL",
        json!({
            "schema_version":1,
            "goal":{"text":"Preserve the existing CLI behavior.","source_record_ids":["user-1:0"]},
            "accounted_source_record_ids":["user-1:0"],
            "operations":[{
                "operation":"add",
                "key":"existing-cli-behavior",
                "kind":"constraint",
                "text":"Preserve the existing CLI behavior.",
                "target_key":"",
                "intent_keys":[],
                "source_record_ids":["user-1:0"],
                "alternatives":[]
            }]
        })
        .to_string(),
    );
    environment.insert(
        "XDG_STATE_HOME",
        fake_root.join("state").display().to_string(),
    );
    environment
}

#[test]
fn k01_plugin_declares_the_required_codex_lifecycle_hooks() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_path = root.join("plugins/driftctl-codex/.codex-plugin/plugin.json");
    let hooks_path = root.join("plugins/driftctl-codex/hooks/hooks.json");
    let skill_path = root.join("plugins/driftctl-codex/skills/driftctl/SKILL.md");

    let manifest: Value = serde_json::from_slice(
        &fs::read(&manifest_path).expect("packaged Driftctl plugin manifest"),
    )
    .expect("plugin manifest JSON");
    assert_eq!(manifest["name"], "driftctl-codex");
    assert_eq!(manifest["skills"], "./skills/");
    let skill = fs::read_to_string(skill_path).expect("packaged Driftctl control skill");
    assert!(skill.contains("$driftctl on"));
    assert!(skill.contains("$driftctl off"));
    assert!(skill.contains("$driftctl status"));
    assert!(skill.contains("Never activate Driftctl implicitly"));

    let hooks: Value =
        serde_json::from_slice(&fs::read(hooks_path).expect("packaged Driftctl hooks"))
            .expect("hooks JSON");
    let configured = hooks["hooks"].as_object().expect("hook event map");
    for event in ["SessionStart", "UserPromptSubmit", "PreCompact", "Stop"] {
        let handlers = configured[event].as_array().expect("configured hook event");
        assert_eq!(handlers.len(), 1, "{event} has one matcher group");
        let command = handlers[0]["hooks"][0]["command"]
            .as_str()
            .expect("hook command");
        assert_eq!(command, "driftctl hook codex");
    }
}

#[test]
fn u01_explicit_on_activates_only_the_invoking_session_and_injects_intent() {
    let root = temporary_directory("in-session-on");
    let session_id = "thread-explicit-on";
    let control_id = "thread-not-activated";
    let environment = attached_environment(&root, session_id);

    let activated = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id": session_id,
            "transcript_path": null,
            "cwd": root,
            "hook_event_name": "UserPromptSubmit",
            "model": "gpt-5.6-luna",
            "permission_mode": "default",
            "turn_id": "turn-activate",
            "prompt": "$driftctl on"
        }),
    );
    assert_eq!(
        activated.status.code(),
        Some(0),
        "activation failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&activated.stdout),
        String::from_utf8_lossy(&activated.stderr)
    );
    let projection = projection_from_hook(&activated);
    assert_eq!(
        projection["goal"]["text"],
        "Preserve the existing CLI behavior."
    );
    assert!(!String::from_utf8_lossy(&activated.stdout).contains(session_id));

    let control = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id": control_id,
            "transcript_path": null,
            "cwd": root,
            "hook_event_name": "UserPromptSubmit",
            "model": "gpt-5.6-luna",
            "permission_mode": "default",
            "turn_id": "turn-control",
            "prompt": "Continue ordinary work."
        }),
    );
    assert_eq!(control.status.code(), Some(0));
    assert!(control.stdout.is_empty());
    assert!(control.stderr.is_empty());

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn u02_u03_lifecycle_and_near_match_prompts_never_activate_a_session() {
    let root = temporary_directory("no-implicit-activation");
    let state_home = root.join("state");
    let events = [
        json!({
            "session_id": "thread-inert",
            "transcript_path": null,
            "cwd": root,
            "hook_event_name": "SessionStart",
            "model": "gpt-5.6-luna",
            "source": "startup"
        }),
        json!({
            "session_id": "thread-inert",
            "transcript_path": null,
            "cwd": root,
            "hook_event_name": "SessionStart",
            "model": "gpt-5.6-luna",
            "source": "compact"
        }),
        json!({
            "session_id": "thread-inert",
            "transcript_path": null,
            "cwd": root,
            "hook_event_name": "PreCompact",
            "model": "gpt-5.6-luna",
            "turn_id": "turn-compact",
            "trigger": "auto"
        }),
    ];
    for event in events {
        let output = run_hook(&root, &state_home, &event);
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
    for (index, prompt) in [
        "$driftctl",
        "$driftctl ON",
        "$driftctl on now",
        "please use $driftctl on",
        "`$driftctl on`",
    ]
    .into_iter()
    .enumerate()
    {
        let output = run_hook(
            &root,
            &state_home,
            &json!({
                "session_id": "thread-inert",
                "transcript_path": null,
                "cwd": root,
                "hook_event_name": "UserPromptSubmit",
                "model": "gpt-5.6-luna",
                "permission_mode": "default",
                "turn_id": format!("turn-near-{index}"),
                "prompt": prompt
            }),
        );
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stdout.is_empty(), "near match activated: {prompt}");
        assert!(output.stderr.is_empty());
    }
    assert!(!state_home.exists(), "inert hooks created private state");

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn u04_status_reports_exact_session_state_without_creating_or_reconciling() {
    let root = temporary_directory("in-session-status");
    let state_home = root.join("state");
    let off = run_hook(
        &root,
        &state_home,
        &json!({
            "session_id": "thread-status-off",
            "transcript_path": null,
            "cwd": root,
            "hook_event_name": "UserPromptSubmit",
            "model": "gpt-5.6-luna",
            "permission_mode": "default",
            "turn_id": "turn-status-off",
            "prompt": "$driftctl status"
        }),
    );
    assert_eq!(off.status.code(), Some(0));
    assert!(additional_context(&off).contains("off"));
    assert!(!state_home.exists(), "status created an enrollment");

    let session_id = "thread-status-on";
    let environment = attached_environment(&root, session_id);
    let activated = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id": session_id,
            "transcript_path": null,
            "cwd": root,
            "hook_event_name": "UserPromptSubmit",
            "model": "gpt-5.6-luna",
            "permission_mode": "default",
            "turn_id": "turn-activate",
            "prompt": "$driftctl on"
        }),
    );
    assert_eq!(activated.status.code(), Some(0));
    let keeper_before = fake_keeper_transcript(&environment);
    let on = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id": session_id,
            "transcript_path": null,
            "cwd": root,
            "hook_event_name": "UserPromptSubmit",
            "model": "gpt-5.6-luna",
            "permission_mode": "default",
            "turn_id": "turn-status-on",
            "prompt": "$driftctl status"
        }),
    );
    assert_eq!(on.status.code(), Some(0));
    assert!(additional_context(&on).contains("on"));
    assert_eq!(fake_keeper_transcript(&environment), keeper_before);

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn u05_off_detaches_only_the_invoking_session() {
    let root = temporary_directory("in-session-off");
    let session_a = "thread-off-target";
    let session_b = "thread-off-control";
    let environment_a = attached_environment(&root, session_a);
    let mut environment_b = attached_environment(&root, session_b);
    environment_b.insert("XDG_STATE_HOME", environment_a["XDG_STATE_HOME"].clone());
    for (session, environment) in [(session_a, &environment_a), (session_b, &environment_b)] {
        let attached = run_cli(
            &root,
            &["attach", "codex", "--session", session, "--json"],
            environment,
        );
        assert_eq!(
            attached.status.code(),
            Some(0),
            "attach failed: {}",
            String::from_utf8_lossy(&attached.stderr)
        );
    }

    let disabled = run_hook_with_environment(
        &root,
        &environment_a,
        &json!({
            "session_id": session_a,
            "transcript_path": null,
            "cwd": root,
            "hook_event_name": "UserPromptSubmit",
            "model": "gpt-5.6-luna",
            "permission_mode": "default",
            "turn_id": "turn-off",
            "prompt": "$driftctl off"
        }),
    );
    assert_eq!(disabled.status.code(), Some(0));
    assert!(additional_context(&disabled).contains("off"));

    let target_after = run_hook_with_environment(
        &root,
        &environment_a,
        &json!({
            "session_id": session_a,
            "transcript_path": null,
            "cwd": root,
            "hook_event_name": "SessionStart",
            "model": "gpt-5.6-luna",
            "source": "resume"
        }),
    );
    assert!(target_after.stdout.is_empty());
    assert!(target_after.stderr.is_empty());

    let control_after = run_hook_with_environment(
        &root,
        &environment_b,
        &json!({
            "session_id": session_b,
            "transcript_path": null,
            "cwd": root,
            "hook_event_name": "SessionStart",
            "model": "gpt-5.6-luna",
            "source": "resume"
        }),
    );
    assert_eq!(control_after.status.code(), Some(0));
    assert_eq!(
        projection_from_hook(&control_after)["goal"]["text"],
        "Preserve the existing CLI behavior."
    );

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn u06_control_prompts_are_not_semantic_intent_after_reactivation() {
    let root = temporary_directory("control-source-accounting");
    let session_id = "thread-control-source";
    let mut environment = attached_environment(&root, session_id);
    let on_event = json!({
        "session_id": session_id,
        "transcript_path": null,
        "cwd": root,
        "hook_event_name": "UserPromptSubmit",
        "model": "gpt-5.6-luna",
        "permission_mode": "default",
        "turn_id": "turn-on",
        "prompt": "$driftctl on"
    });
    assert_eq!(
        run_hook_with_environment(&root, &environment, &on_event)
            .status
            .code(),
        Some(0)
    );
    let off_event = json!({
        "session_id": session_id,
        "transcript_path": null,
        "cwd": root,
        "hook_event_name": "UserPromptSubmit",
        "model": "gpt-5.6-luna",
        "permission_mode": "default",
        "turn_id": "turn-off",
        "prompt": "$driftctl off"
    });
    assert_eq!(
        run_hook_with_environment(&root, &environment, &off_event)
            .status
            .code(),
        Some(0)
    );
    let keeper_before = fake_keeper_transcript(&environment);
    environment.insert(
        "DRIFTCTL_FAKE_READ",
        json!({
            "thread": {
                "id": session_id,
                "cwd": root.canonicalize().expect("canonical test root"),
                "turns": [{"items": [
                    {
                        "type": "userMessage",
                        "id": "user-1",
                        "content": [{"type":"text","text":"Preserve the existing CLI behavior."}]
                    },
                    {
                        "type": "userMessage",
                        "id": "control-on",
                        "content": [{"type":"text","text":"$driftctl on"}]
                    },
                    {
                        "type": "userMessage",
                        "id": "control-off",
                        "content": [{"type":"text","text":"$driftctl off"}]
                    }
                ]}]
            }
        })
        .to_string(),
    );
    let reactivated = run_hook_with_environment(&root, &environment, &on_event);
    assert_eq!(
        reactivated.status.code(),
        Some(0),
        "reactivation failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&reactivated.stdout),
        String::from_utf8_lossy(&reactivated.stderr)
    );
    assert_eq!(
        projection_from_hook(&reactivated)["goal"]["text"],
        "Preserve the existing CLI behavior."
    );
    assert_eq!(fake_keeper_transcript(&environment), keeper_before);

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn k01_integrate_install_and_remove_preserve_existing_codex_configuration() {
    let root = temporary_directory("integration-repository");
    let isolated = temporary_home_directory("integration-home");
    let codex_home = isolated.join("codex-home");
    let bin = isolated.join("bin");
    fs::create_dir(&codex_home).expect("create isolated Codex home");
    fs::create_dir(&bin).expect("create isolated binary directory");
    fs::write(
        codex_home.join("config.toml"),
        "model = \"sentinel-model\"\n[features]\nhooks = true\n",
    )
    .expect("write existing Codex config");
    let existing_hooks = "{\"hooks\":{\"PreToolUse\":[]}}\n";
    fs::write(codex_home.join("hooks.json"), existing_hooks).expect("write existing hooks");
    #[cfg(unix)]
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_driftctl"), bin.join("driftctl"))
        .expect("link installed driftctl entrypoint");

    let mut environment = BTreeMap::new();
    environment.insert("CODEX_HOME", codex_home.display().to_string());
    environment.insert(
        "DRIFTCTL_CODEX_BIN",
        std::env::var("DRIFTCTL_REAL_CODEX_BIN").unwrap_or_else(|_| "codex".to_owned()),
    );
    environment.insert(
        "PATH",
        format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").expect("test PATH")
        ),
    );
    environment.insert("XDG_DATA_HOME", isolated.join("data").display().to_string());

    let installed = run_cli(
        &root,
        &["integrate", "codex", "install", "--json"],
        &environment,
    );
    assert_eq!(
        installed.status.code(),
        Some(0),
        "integration install failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&installed.stdout),
        String::from_utf8_lossy(&installed.stderr)
    );
    let installed_json: Value =
        serde_json::from_slice(&installed.stdout).expect("integration install JSON");
    assert_eq!(installed_json["status"], "installed");
    let installed_plugin = isolated.join("data/driftctl/codex-marketplace/plugins/driftctl-codex");
    let installed_skill = fs::read_to_string(installed_plugin.join("skills/driftctl/SKILL.md"))
        .expect("installed Driftctl skill");
    assert!(installed_skill.contains("$driftctl on"));
    let invocation_policy =
        fs::read_to_string(installed_plugin.join("skills/driftctl/agents/openai.yaml"))
            .expect("installed explicit invocation policy");
    assert!(invocation_policy.contains("allow_implicit_invocation: false"));
    let config = fs::read_to_string(codex_home.join("config.toml")).expect("preserved config");
    assert!(config.contains("sentinel-model"));
    assert_eq!(
        fs::read_to_string(codex_home.join("hooks.json")).expect("preserved hooks"),
        existing_hooks
    );

    let status = run_cli(
        &root,
        &["integrate", "codex", "status", "--json"],
        &environment,
    );
    assert_eq!(status.status.code(), Some(0));
    let status_json: Value =
        serde_json::from_slice(&status.stdout).expect("integration status JSON");
    assert_eq!(status_json["plugin"], "installed");
    assert_eq!(status_json["hooks_feature"], "enabled");

    let removed = run_cli(
        &root,
        &["integrate", "codex", "remove", "--json"],
        &environment,
    );
    assert_eq!(
        removed.status.code(),
        Some(0),
        "integration remove failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&removed.stdout),
        String::from_utf8_lossy(&removed.stderr)
    );
    let removed_json: Value =
        serde_json::from_slice(&removed.stdout).expect("integration remove JSON");
    assert_eq!(removed_json["status"], "removed");
    let config = fs::read_to_string(codex_home.join("config.toml")).expect("preserved config");
    assert!(config.contains("sentinel-model"));
    assert_eq!(
        fs::read_to_string(codex_home.join("hooks.json")).expect("preserved hooks"),
        existing_hooks
    );

    fs::remove_dir_all(root).expect("remove isolated repository");
    fs::remove_dir_all(isolated).expect("remove isolated integration home");
}

#[test]
fn k03_unenrolled_user_prompt_hook_is_a_strict_noop() {
    let root = temporary_directory("unenrolled");
    let state_home = root.join("state");
    let output = run_hook(
        &root,
        &state_home,
        &json!({
            "session_id": "thread-unenrolled",
            "transcript_path": null,
            "cwd": root,
            "hook_event_name": "UserPromptSubmit",
            "model": "gpt-5.6-luna",
            "permission_mode": "default",
            "turn_id": "turn-1",
            "prompt": "keep the existing behavior"
        }),
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "unenrolled hook failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "unenrolled hook emitted stdout");
    assert!(output.stderr.is_empty(), "unenrolled hook emitted stderr");
    assert!(!state_home.exists(), "unenrolled hook created state");

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn k04_k05_k06_exact_session_attach_inject_isolate_and_detach() {
    let root = temporary_directory("attached-lifecycle");
    let session_id = "thread-exact-attached";
    let control_id = "thread-same-repository-control";
    let environment = attached_environment(&root, session_id);

    let attached = run_cli(
        &root,
        &["attach", "codex", "--session", session_id, "--json"],
        &environment,
    );
    assert_eq!(
        attached.status.code(),
        Some(0),
        "attach failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&attached.stdout),
        String::from_utf8_lossy(&attached.stderr)
    );
    let attached_json: Value = serde_json::from_slice(&attached.stdout).expect("attach JSON");
    assert_eq!(attached_json["status"], "attached");
    assert_eq!(attached_json["created"], true);
    assert!(!String::from_utf8_lossy(&attached.stdout).contains(session_id));
    let repeated = run_cli(
        &root,
        &["attach", "codex", "--session", session_id, "--json"],
        &environment,
    );
    assert_eq!(repeated.status.code(), Some(0));
    let repeated_json: Value =
        serde_json::from_slice(&repeated.stdout).expect("repeat attach JSON");
    assert_eq!(repeated_json["created"], false);
    let enrollment_directory =
        Path::new(&environment["XDG_STATE_HOME"]).join("driftctl/enrollments");
    let enrollment_count = fs::read_dir(&enrollment_directory)
        .expect("enrollments")
        .count();
    let unknown = run_cli(
        &root,
        &[
            "attach",
            "codex",
            "--session",
            "thread-does-not-exist",
            "--json",
        ],
        &environment,
    );
    assert_eq!(unknown.status.code(), Some(1));
    assert_eq!(
        fs::read_dir(&enrollment_directory)
            .expect("enrollments after unknown session")
            .count(),
        enrollment_count,
        "unknown session created an enrollment"
    );

    let hook = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id": session_id,
            "transcript_path": null,
            "cwd": root,
            "hook_event_name": "SessionStart",
            "model": "gpt-5.6-luna",
            "source": "resume"
        }),
    );
    assert_eq!(
        hook.status.code(),
        Some(0),
        "hook stderr: {}",
        String::from_utf8_lossy(&hook.stderr)
    );
    let hook_json: Value = serde_json::from_slice(&hook.stdout).expect("hook output JSON");
    assert_eq!(
        hook_json["hookSpecificOutput"]["hookEventName"],
        "SessionStart"
    );
    let context = hook_json["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("injected context");
    assert!(context.contains("Preserve the existing CLI behavior."));
    assert!(!context.contains(session_id));

    let control = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id": control_id,
            "transcript_path": null,
            "cwd": root,
            "hook_event_name": "SessionStart",
            "model": "gpt-5.6-luna",
            "source": "resume"
        }),
    );
    assert_eq!(control.status.code(), Some(0));
    assert!(
        control.stdout.is_empty(),
        "control session received context"
    );
    assert!(
        control.stderr.is_empty(),
        "control session received warning"
    );

    let detached = run_cli(
        &root,
        &["detach", "codex", "--session", session_id, "--json"],
        &environment,
    );
    assert_eq!(detached.status.code(), Some(0));
    let detached_json: Value = serde_json::from_slice(&detached.stdout).expect("detach JSON");
    assert_eq!(detached_json["status"], "detached");

    let after_detach = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id": session_id,
            "transcript_path": null,
            "cwd": root,
            "hook_event_name": "SessionStart",
            "model": "gpt-5.6-luna",
            "source": "resume"
        }),
    );
    assert_eq!(after_detach.status.code(), Some(0));
    assert!(after_detach.stdout.is_empty());
    assert!(after_detach.stderr.is_empty());

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn k08_k12_user_prompt_is_folded_before_injection_and_duplicate_is_idempotent() {
    let root = temporary_directory("prompt-fold");
    let session_id = "thread-prompt-fold";
    let mut environment = attached_environment(&root, session_id);
    let attached = run_cli(
        &root,
        &["attach", "codex", "--session", session_id, "--json"],
        &environment,
    );
    assert_eq!(attached.status.code(), Some(0), "{attached:?}");

    let session_start = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id": session_id,
            "transcript_path": null,
            "cwd": root,
            "hook_event_name": "SessionStart",
            "model": "gpt-5.6-luna",
            "source": "resume"
        }),
    );
    let injected = projection_from_hook(&session_start);
    assert_eq!(
        injected["goal"]["text"],
        "Preserve the existing CLI behavior."
    );
    let projection = private_state_document(&environment, "projection.json");
    let active_ids = ["preserve", "frontier", "validation"]
        .into_iter()
        .flat_map(|section| {
            projection[section]
                .as_array()
                .expect("active projection section")
        })
        .map(|item| item["id"].clone())
        .collect::<Vec<_>>();
    let revision = projection["source_head"]["pending_sequence"]
        .as_u64()
        .expect("base event sequence");
    environment.insert(
        "DRIFTCTL_FAKE_PROPOSAL",
        json!({
            "schema_version":1,
            "base_projection_revision":projection["revision"],
            "base_event_sequence":revision,
            "classification":"additive",
            "accounted_active_intent_ids":active_ids,
            "accounted_source_record_ids":["user-2:0"],
            "operations":[{
                "operation":"add",
                "key":"keep-help-output",
                "kind":"constraint",
                "text":"Keep the existing help output unchanged.",
                "target_intent_id":"",
                "intent_ids":[],
                "evidence_id":"",
                "reason":"",
                "source_record_ids":["user-2:0"],
                "alternatives":[]
            }]
        })
        .to_string(),
    );
    let event = json!({
        "session_id": session_id,
        "transcript_path": null,
        "cwd": root,
        "hook_event_name": "UserPromptSubmit",
        "model": "gpt-5.6-luna",
        "permission_mode": "default",
        "turn_id": "user-2",
        "prompt": "Keep the existing help output unchanged."
    });
    let folded = run_hook_with_environment(&root, &environment, &event);
    assert_eq!(
        folded.status.code(),
        Some(0),
        "prompt hook failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&folded.stdout),
        String::from_utf8_lossy(&folded.stderr)
    );
    let folded_projection = projection_from_hook(&folded);
    assert!(
        folded_projection["frontier"]
            .as_array()
            .expect("frontier")
            .iter()
            .any(|item| item["text"] == "Keep the existing help output unchanged.")
    );
    let prompts = PathBuf::from(&environment["DRIFTCTL_FAKE_PROMPTS"]);
    let calls_after_fold = fs::read_to_string(&prompts)
        .expect("captured semantic calls")
        .lines()
        .count();
    assert_eq!(calls_after_fold, 2, "attach plus one prompt fold");

    let duplicate = run_hook_with_environment(&root, &environment, &event);
    assert_eq!(duplicate.status.code(), Some(0));
    assert_eq!(projection_from_hook(&duplicate), folded_projection);
    assert_eq!(
        fs::read_to_string(&prompts)
            .expect("captured semantic calls")
            .lines()
            .count(),
        calls_after_fold,
        "duplicate hook delivery spent another model call"
    );

    let canonical_root = root.canonicalize().expect("canonical test root");
    environment.insert(
        "DRIFTCTL_FAKE_READ",
        json!({
            "thread": {
                "id": session_id,
                "cwd": canonical_root,
                "turns": [{"items": [
                    {
                        "type":"userMessage",
                        "id":"user-1",
                        "content":[{"type":"text","text":"Preserve the existing CLI behavior."}]
                    },
                    {
                        "type":"userMessage",
                        "id":"provider-user-2",
                        "content":[{"type":"text","text":"Keep the existing help output unchanged."}]
                    }
                ]}]
            }
        })
        .to_string(),
    );
    let current = private_state_document(&environment, "projection.json");
    let current_active_ids = ["preserve", "frontier", "validation"]
        .into_iter()
        .flat_map(|section| {
            current[section]
                .as_array()
                .expect("active projection section")
        })
        .map(|item| item["id"].clone())
        .collect::<Vec<_>>();
    environment.insert(
        "DRIFTCTL_FAKE_PROPOSAL",
        json!({
            "schema_version":1,
            "base_projection_revision":current["revision"],
            "base_event_sequence":current["source_head"]["pending_sequence"],
            "classification":"no_change",
            "accounted_active_intent_ids":current_active_ids,
            "accounted_source_record_ids":["user-3:0"],
            "operations":[]
        })
        .to_string(),
    );
    let non_steering = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id": session_id,
            "transcript_path": null,
            "cwd": root,
            "hook_event_name": "UserPromptSubmit",
            "model": "gpt-5.6-luna",
            "permission_mode": "default",
            "turn_id": "user-3",
            "prompt": "What is the current status?"
        }),
    );
    assert_eq!(non_steering.status.code(), Some(0));
    assert_eq!(projection_from_hook(&non_steering), folded_projection);
    assert_eq!(
        fs::read_to_string(&prompts)
            .expect("captured semantic calls")
            .lines()
            .count(),
        calls_after_fold + 1,
        "one non-steering prompt should be source-accounted once"
    );

    environment.insert(
        "DRIFTCTL_FAKE_READ",
        json!({
            "thread": {
                "id": session_id,
                "cwd": canonical_root,
                "turns": [{"items": [
                    {"type":"userMessage","id":"user-1","content":[{"type":"text","text":"Preserve the existing CLI behavior."}]},
                    {"type":"userMessage","id":"provider-user-2","content":[{"type":"text","text":"Keep the existing help output unchanged."}]},
                    {"type":"userMessage","id":"provider-user-3","content":[{"type":"text","text":"What is the current status?"}]},
                    {"type":"agentMessage","id":"assistant-3","text":"Current status reported."}
                ]}]
            }
        })
        .to_string(),
    );
    let stop = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id":session_id,
            "transcript_path":null,
            "cwd":root,
            "hook_event_name":"Stop",
            "model":"gpt-5.6-luna",
            "stop_hook_active":false,
            "last_assistant_message":"Current status reported."
        }),
    );
    assert_eq!(stop.status.code(), Some(0));
    assert!(stop.stdout.is_empty());
    let pre_compact = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id":session_id,
            "transcript_path":null,
            "cwd":root,
            "hook_event_name":"PreCompact",
            "model":"gpt-5.6-luna",
            "trigger":"auto"
        }),
    );
    assert_eq!(pre_compact.status.code(), Some(0));
    assert!(pre_compact.stdout.is_empty());
    let after_compact = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id":session_id,
            "transcript_path":null,
            "cwd":root,
            "hook_event_name":"SessionStart",
            "model":"gpt-5.6-luna",
            "source":"compact"
        }),
    );
    assert_eq!(after_compact.status.code(), Some(0));
    assert_eq!(projection_from_hook(&after_compact), folded_projection);
    assert_eq!(
        fs::read_to_string(&prompts)
            .expect("captured semantic calls")
            .lines()
            .count(),
        calls_after_fold + 1,
        "observation and compaction lifecycle spent semantic model calls"
    );

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn k10_ambiguous_prompt_persists_conflict_and_blocks_before_model() {
    let root = temporary_directory("prompt-conflict");
    let session_id = "thread-prompt-conflict";
    let mut environment = attached_environment(&root, session_id);
    let attached = run_cli(
        &root,
        &["attach", "codex", "--session", session_id, "--json"],
        &environment,
    );
    assert_eq!(attached.status.code(), Some(0), "{attached:?}");
    let projection = private_state_document(&environment, "projection.json");
    let active_ids = ["preserve", "frontier", "validation"]
        .into_iter()
        .flat_map(|section| projection[section].as_array().expect("active section"))
        .map(|item| item["id"].clone())
        .collect::<Vec<_>>();
    let affected = active_ids.first().expect("one active intent").clone();
    environment.insert(
        "DRIFTCTL_FAKE_PROPOSAL",
        json!({
            "schema_version":1,
            "base_projection_revision":projection["revision"],
            "base_event_sequence":projection["source_head"]["pending_sequence"],
            "classification":"conflict",
            "accounted_active_intent_ids":active_ids,
            "accounted_source_record_ids":["user-2:0"],
            "operations":[{
                "operation":"conflict",
                "key":"output-choice",
                "kind":"constraint",
                "text":"The requested output format is ambiguous.",
                "target_intent_id":"",
                "intent_ids":[affected],
                "evidence_id":"",
                "reason":"",
                "source_record_ids":["user-2:0"],
                "alternatives":[
                    {"key":"json","text":"Use JSON.","source_record_ids":["user-2:0"]},
                    {"key":"yaml","text":"Use YAML.","source_record_ids":["user-2:0"]}
                ]
            }]
        })
        .to_string(),
    );
    let blocked = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id":session_id,
            "transcript_path":null,
            "cwd":root,
            "hook_event_name":"UserPromptSubmit",
            "model":"gpt-5.6-luna",
            "permission_mode":"default",
            "turn_id":"user-2",
            "prompt":"Use JSON. Actually, use YAML."
        }),
    );
    assert_eq!(blocked.status.code(), Some(0));
    let output: Value = serde_json::from_slice(&blocked.stdout).expect("blocking hook JSON");
    assert_eq!(output["decision"], "block");
    assert!(
        output["reason"]
            .as_str()
            .expect("blocking reason")
            .contains("unresolved conflict")
    );
    let persisted = private_state_document(&environment, "projection.json");
    assert_eq!(persisted["conflicts"].as_array().map(Vec::len), Some(1));
    assert!(
        persisted["conflicts"][0]["alternatives"]
            .as_array()
            .is_some_and(|alternatives| alternatives.len() == 2)
    );
    let conflict_id = persisted["conflicts"][0]["id"]
        .as_str()
        .expect("conflict ID");
    let alternative_id = persisted["conflicts"][0]["alternatives"][0]["id"]
        .as_str()
        .expect("alternative ID");
    let selected_text = persisted["conflicts"][0]["alternatives"][0]["text"]
        .as_str()
        .expect("alternative text")
        .to_owned();
    let resolved = run_cli(
        &root,
        &[
            "resolve",
            "codex",
            "--session",
            session_id,
            "--conflict",
            conflict_id,
            "--alternative",
            alternative_id,
            "--json",
        ],
        &environment,
    );
    assert_eq!(
        resolved.status.code(),
        Some(0),
        "resolve failed: {}",
        String::from_utf8_lossy(&resolved.stderr)
    );
    environment.insert(
        "DRIFTCTL_FAKE_READ",
        json!({
            "thread": {
                "id":session_id,
                "cwd":root.canonicalize().expect("canonical root"),
                "turns":[{"items":[
                    {"type":"userMessage","id":"user-1","content":[{"type":"text","text":"Preserve the existing CLI behavior."}]},
                    {"type":"userMessage","id":"user-2","content":[{"type":"text","text":"Use JSON. Actually, use YAML."}]}
                ]}]
            }
        })
        .to_string(),
    );
    let resumed = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id":session_id,
            "transcript_path":null,
            "cwd":root,
            "hook_event_name":"SessionStart",
            "model":"gpt-5.6-luna",
            "source":"resume"
        }),
    );
    assert_eq!(resumed.status.code(), Some(0));
    let projected = projection_from_hook(&resumed);
    assert_eq!(projected["conflicts"].as_array().map(Vec::len), Some(0));
    assert!(
        projected["frontier"]
            .as_array()
            .expect("resolved frontier")
            .iter()
            .any(|item| item["text"] == selected_text)
    );

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn k11_invalid_keeper_output_blocks_without_changing_accepted_state() {
    let root = temporary_directory("invalid-keeper");
    let session_id = "thread-invalid-keeper";
    let mut environment = attached_environment(&root, session_id);
    let attached = run_cli(
        &root,
        &["attach", "codex", "--session", session_id, "--json"],
        &environment,
    );
    assert_eq!(attached.status.code(), Some(0), "{attached:?}");
    let projection_before = private_state_document(&environment, "projection.json");
    let source_before = private_state_document(&environment, "source.json");
    environment.insert("DRIFTCTL_FAKE_PROPOSAL", "{}".to_owned());

    let blocked = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id":session_id,
            "transcript_path":null,
            "cwd":root,
            "hook_event_name":"UserPromptSubmit",
            "model":"gpt-5.6-luna",
            "permission_mode":"default",
            "turn_id":"user-2",
            "prompt":"Change all existing behavior without constraints."
        }),
    );
    assert_eq!(blocked.status.code(), Some(0));
    let output: Value = serde_json::from_slice(&blocked.stdout).expect("blocking hook JSON");
    assert_eq!(output["decision"], "block");
    assert!(
        output["reason"]
            .as_str()
            .expect("blocking reason")
            .contains("invalid proposal")
    );
    assert_eq!(
        private_state_document(&environment, "projection.json"),
        projection_before
    );
    assert_eq!(
        private_state_document(&environment, "source.json"),
        source_before
    );

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn k09_goal_change_requires_exact_operator_approval_and_native_readback() {
    let root = temporary_directory("goal-approval");
    let session_id = "thread-goal-approval";
    let mut environment = attached_environment(&root, session_id);
    let attached = run_cli(
        &root,
        &["attach", "codex", "--session", session_id, "--json"],
        &environment,
    );
    assert_eq!(attached.status.code(), Some(0), "{attached:?}");

    let projection = private_state_document(&environment, "projection.json");
    let active_ids = ["preserve", "frontier", "validation"]
        .into_iter()
        .flat_map(|section| projection[section].as_array().expect("active section"))
        .map(|item| item["id"].clone())
        .collect::<Vec<_>>();
    environment.insert(
        "DRIFTCTL_FAKE_PROPOSAL",
        json!({
            "schema_version":1,
            "base_projection_revision":projection["revision"],
            "base_event_sequence":projection["source_head"]["pending_sequence"],
            "classification":"goal_change",
            "accounted_active_intent_ids":active_ids,
            "accounted_source_record_ids":["user-2:0"],
            "operations":[],
            "proposed_goal":{
                "text":"Replace the overall objective.",
                "source_record_ids":["user-2:0"]
            }
        })
        .to_string(),
    );
    let blocked = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id":session_id,
            "transcript_path":null,
            "cwd":root,
            "hook_event_name":"UserPromptSubmit",
            "model":"gpt-5.6-luna",
            "permission_mode":"default",
            "turn_id":"user-2",
            "prompt":"Replace the overall objective."
        }),
    );
    assert_eq!(blocked.status.code(), Some(0));
    let blocked_json: Value = serde_json::from_slice(&blocked.stdout).expect("blocked hook JSON");
    assert_eq!(blocked_json["decision"], "block");
    assert_eq!(
        fs::read_to_string(&environment["DRIFTCTL_FAKE_GOAL_STATE"])
            .expect("native goal before decision"),
        "null"
    );

    let edited_goal = "Ship the explicitly edited objective.";
    let edited = run_cli(
        &root,
        &[
            "resolve",
            "codex",
            "--session",
            session_id,
            "--edit-goal",
            edited_goal,
            "--json",
        ],
        &environment,
    );
    assert_eq!(edited.status.code(), Some(0), "{edited:?}");
    let edited_json: Value = serde_json::from_slice(&edited.stdout).expect("edit JSON");
    assert_eq!(edited_json["status"], "blocked");
    assert_eq!(edited_json["proposed_goal"], edited_goal);
    assert_eq!(
        fs::read_to_string(&environment["DRIFTCTL_FAKE_GOAL_STATE"])
            .expect("native goal after edit"),
        "null"
    );

    let wrong_session = run_cli(
        &root,
        &[
            "resolve",
            "codex",
            "--session",
            "thread-not-attached",
            "--approve-goal",
            "--json",
        ],
        &environment,
    );
    assert_ne!(wrong_session.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(&environment["DRIFTCTL_FAKE_GOAL_STATE"])
            .expect("native goal after wrong-session decision"),
        "null"
    );

    let canonical_root = root.canonicalize().expect("canonical test root");
    let accepted_read = json!({
        "thread": {
            "id":session_id,
            "cwd":canonical_root,
            "turns":[{"items":[
                {"type":"userMessage","id":"user-1","content":[{"type":"text","text":"Preserve the existing CLI behavior."}]}
            ]}]
        }
    });
    environment.insert(
        "DRIFTCTL_FAKE_READ",
        json!({
            "thread": {
                "id":session_id,
                "cwd":canonical_root,
                "turns":[{"items":[
                    {"type":"userMessage","id":"user-1","content":[{"type":"text","text":"Preserve the existing CLI behavior."}]},
                    {"type":"userMessage","id":"user-unaccepted","content":[{"type":"text","text":"A concurrent source change."}]}
                ]}]
            }
        })
        .to_string(),
    );
    let stale = run_cli(
        &root,
        &[
            "resolve",
            "codex",
            "--session",
            session_id,
            "--approve-goal",
            "--json",
        ],
        &environment,
    );
    assert_ne!(stale.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(&environment["DRIFTCTL_FAKE_GOAL_STATE"])
            .expect("native goal after stale decision"),
        "null"
    );
    environment.insert("DRIFTCTL_FAKE_READ", accepted_read.to_string());

    let capture = PathBuf::from(&environment["DRIFTCTL_FAKE_RPC_CAPTURE"]);
    let projection_before_approval = private_state_document(&environment, "projection.json");
    let calls_before = fs::read_to_string(&capture)
        .unwrap_or_default()
        .lines()
        .count();
    let handoff = run_cli(
        &root,
        &[
            "resolve",
            "codex",
            "--session",
            session_id,
            "--approve-goal",
            "--json",
        ],
        &environment,
    );
    assert_eq!(handoff.status.code(), Some(2), "{handoff:?}");
    let handoff_text = String::from_utf8_lossy(&handoff.stderr);
    assert!(handoff_text.contains("/goal clear"), "{handoff_text}");
    assert!(
        handoff_text.contains(&format!("/goal {edited_goal}")),
        "{handoff_text}"
    );
    assert!(
        handoff_text.contains("rerun"),
        "handoff omitted confirmation step: {handoff_text}"
    );
    assert_eq!(
        fs::read_to_string(&environment["DRIFTCTL_FAKE_GOAL_STATE"])
            .expect("native goal after handoff"),
        "null"
    );
    assert_eq!(
        private_state_document(&environment, "projection.json"),
        projection_before_approval
    );
    let handoff_methods = fs::read_to_string(&capture)
        .expect("captured handoff RPC")
        .lines()
        .skip(calls_before)
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|request| request["method"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert!(
        !handoff_methods
            .iter()
            .any(|method| matches!(method.as_str(), "thread/goal/clear" | "thread/goal/set")),
        "handoff mutated the native goal: {handoff_methods:?}"
    );

    fs::write(
        &environment["DRIFTCTL_FAKE_GOAL_STATE"],
        serde_json::to_string(edited_goal).expect("encode operator-set goal"),
    )
    .expect("simulate operator setting the native goal");
    environment.insert(
        "DRIFTCTL_FAKE_DRIFT_AFTER_GOAL_GET",
        "Concurrent operator goal".to_owned(),
    );
    let calls_before_race = fs::read_to_string(&capture)
        .expect("captured pre-race RPC")
        .lines()
        .count();
    let raced = run_cli(
        &root,
        &[
            "resolve",
            "codex",
            "--session",
            session_id,
            "--approve-goal",
            "--json",
        ],
        &environment,
    );
    assert_eq!(raced.status.code(), Some(2), "{raced:?}");
    assert_eq!(
        serde_json::from_str::<Value>(
            &fs::read_to_string(&environment["DRIFTCTL_FAKE_GOAL_STATE"])
                .expect("native goal after concurrent change")
        )
        .expect("concurrent native goal JSON"),
        json!("Concurrent operator goal")
    );
    assert_eq!(
        private_state_document(&environment, "projection.json"),
        projection_before_approval
    );
    let race_methods = fs::read_to_string(&capture)
        .expect("captured race RPC")
        .lines()
        .skip(calls_before_race)
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|request| request["method"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert!(
        !race_methods
            .iter()
            .any(|method| matches!(method.as_str(), "thread/goal/clear" | "thread/goal/set")),
        "concurrent goal was overwritten: {race_methods:?}"
    );

    environment.remove("DRIFTCTL_FAKE_DRIFT_AFTER_GOAL_GET");
    fs::write(
        &environment["DRIFTCTL_FAKE_GOAL_STATE"],
        serde_json::to_string(edited_goal).expect("encode confirmed operator goal"),
    )
    .expect("restore exact operator-set goal");
    let calls_before_confirmation = fs::read_to_string(&capture)
        .expect("captured pre-confirmation RPC")
        .lines()
        .count();
    let approved = run_cli(
        &root,
        &[
            "resolve",
            "codex",
            "--session",
            session_id,
            "--approve-goal",
            "--json",
        ],
        &environment,
    );
    assert_eq!(
        approved.status.code(),
        Some(0),
        "approval failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&approved.stdout),
        String::from_utf8_lossy(&approved.stderr)
    );
    let approved_json: Value = serde_json::from_slice(&approved.stdout).expect("approval JSON");
    assert_eq!(approved_json["status"], "attached");
    assert_eq!(approved_json["applied_goal"], edited_goal);
    assert_eq!(
        serde_json::from_str::<Value>(
            &fs::read_to_string(&environment["DRIFTCTL_FAKE_GOAL_STATE"])
                .expect("native goal after approval")
        )
        .expect("native goal JSON"),
        json!(edited_goal)
    );
    let methods = fs::read_to_string(&capture)
        .expect("captured approval RPC")
        .lines()
        .skip(calls_before_confirmation)
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|request| request["method"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let goal_methods = methods
        .into_iter()
        .filter(|method| method.starts_with("thread/goal/"))
        .collect::<Vec<_>>();
    assert!(
        goal_methods
            .iter()
            .all(|method| method == "thread/goal/get"),
        "confirmation used a mutating native-goal RPC: {goal_methods:?}"
    );
    assert!(
        goal_methods.len() >= 2,
        "confirmation omitted exact read-back: {goal_methods:?}"
    );

    environment.insert(
        "DRIFTCTL_FAKE_READ",
        json!({
            "thread": {
                "id":session_id,
                "cwd":canonical_root,
                "turns":[{"items":[
                    {"type":"userMessage","id":"user-1","content":[{"type":"text","text":"Preserve the existing CLI behavior."}]},
                    {"type":"userMessage","id":"user-2","content":[{"type":"text","text":"Replace the overall objective."}]}
                ]}]
            }
        })
        .to_string(),
    );
    let restarted = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id":session_id,
            "transcript_path":null,
            "cwd":root,
            "hook_event_name":"SessionStart",
            "model":"gpt-5.6-luna",
            "source":"resume"
        }),
    );
    assert_eq!(restarted.status.code(), Some(0));
    assert_eq!(
        projection_from_hook(&restarted)["goal"]["text"],
        edited_goal
    );

    let goal_before_replay =
        fs::read_to_string(&environment["DRIFTCTL_FAKE_GOAL_STATE"]).expect("goal before replay");
    let replay = run_cli(
        &root,
        &[
            "resolve",
            "codex",
            "--session",
            session_id,
            "--approve-goal",
            "--json",
        ],
        &environment,
    );
    assert_ne!(replay.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(&environment["DRIFTCTL_FAKE_GOAL_STATE"]).expect("goal after replay"),
        goal_before_replay
    );

    fs::remove_dir_all(root).expect("remove isolated test directory");
}

#[test]
fn k09_rejecting_goal_change_keeps_native_and_accepted_goal_unchanged() {
    let root = temporary_directory("goal-rejection");
    let session_id = "thread-goal-rejection";
    let mut environment = attached_environment(&root, session_id);
    assert_eq!(
        run_cli(
            &root,
            &["attach", "codex", "--session", session_id, "--json"],
            &environment,
        )
        .status
        .code(),
        Some(0)
    );
    let projection = private_state_document(&environment, "projection.json");
    let active_ids = ["preserve", "frontier", "validation"]
        .into_iter()
        .flat_map(|section| projection[section].as_array().expect("active section"))
        .map(|item| item["id"].clone())
        .collect::<Vec<_>>();
    environment.insert(
        "DRIFTCTL_FAKE_PROPOSAL",
        json!({
            "schema_version":1,
            "base_projection_revision":projection["revision"],
            "base_event_sequence":projection["source_head"]["pending_sequence"],
            "classification":"goal_change",
            "accounted_active_intent_ids":active_ids,
            "accounted_source_record_ids":["user-2:0"],
            "operations":[],
            "proposed_goal":{"text":"Wrong inferred objective.","source_record_ids":["user-2:0"]}
        })
        .to_string(),
    );
    let blocked = run_hook_with_environment(
        &root,
        &environment,
        &json!({
            "session_id":session_id,
            "transcript_path":null,
            "cwd":root,
            "hook_event_name":"UserPromptSubmit",
            "model":"gpt-5.6-luna",
            "permission_mode":"default",
            "turn_id":"user-2",
            "prompt":"A diagnostic prompt misread as a goal change."
        }),
    );
    assert_eq!(blocked.status.code(), Some(0));
    let rejected = run_cli(
        &root,
        &[
            "resolve",
            "codex",
            "--session",
            session_id,
            "--reject-goal",
            "--json",
        ],
        &environment,
    );
    assert_eq!(rejected.status.code(), Some(0), "{rejected:?}");
    assert_eq!(
        fs::read_to_string(&environment["DRIFTCTL_FAKE_GOAL_STATE"])
            .expect("native goal after rejection"),
        "null"
    );
    let after = private_state_document(&environment, "projection.json");
    assert_eq!(after["goal"]["text"], "Preserve the existing CLI behavior.");

    fs::remove_dir_all(root).expect("remove isolated test directory");
}
