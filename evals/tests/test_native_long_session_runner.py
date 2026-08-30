"""Fake-process tests for the intact-native-session evaluation runner."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "evals" / "runner" / "run_native_long_session.py"
CASE = ROOT / "evals" / "cases" / "01-steering-retry"
SCORER = ROOT / "evals" / "runner" / "score_results.py"


class NativeLongSessionRunnerTests(unittest.TestCase):
    def test_rejects_context_above_the_bounded_stress_limit(self) -> None:
        with tempfile.TemporaryDirectory(prefix="driftctl-native-limit-test-") as temporary:
            completed = subprocess.run(
                [
                    sys.executable,
                    str(RUNNER),
                    "--case",
                    str(CASE),
                    "--results-dir",
                    str(Path(temporary) / "results"),
                    "--context-bytes",
                    str(1024 * 1024 + 1),
                ],
                cwd=ROOT,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(completed.returncode, 1)
            self.assertIn(
                "context bytes must be between 0 and 1048576",
                json.loads(completed.stdout)["error"],
            )

    def test_seeds_intact_session_compares_and_writes_scoreable_arms(self) -> None:
        with temporary_fixture() as fixture:
            result, outputs = run_fixture(fixture)
            self.assertEqual(result["evaluation_kind"], "native_long_session")
            self.assertEqual(
                result["statistical_claim"], "descriptive_only_no_significance"
            )
            baseline = json.loads(outputs["baseline"].read_text(encoding="utf-8"))
            workflow = json.loads(outputs["workflow"].read_text(encoding="utf-8"))
            for arm, mode in ((baseline, "baseline"), (workflow, "workflow")):
                self.assertEqual(arm["mode"], mode)
                self.assertTrue(arm["verified_completion"])
                self.assertTrue(arm["agent_succeeded"])
                self.assertTrue(arm["scope"]["passed"])
                self.assertEqual(arm["scope"]["unexpected_changed_paths"], [])
                self.assertTrue(arm["native_checkpoint"]["source_workspace_clean"])
                self.assertTrue(arm["native_checkpoint"]["injection"]["accepted"])
                self.assertEqual(arm["native_checkpoint"]["source_user_turn_count"], 2)
                self.assertEqual(
                    arm["native_checkpoint"]["source_turn_labels"],
                    ["initial", "steering-1"],
                )
                self.assertEqual(
                    arm["native_checkpoint"]["injection"]["position"],
                    "after_last_user_turn",
                )
                self.assertEqual(
                    arm["worker_policy"],
                    {
                        "approval_policy": "never",
                        "effort": "max",
                        "model": "gpt-5.6-luna",
                        "sandbox": "workspace-write",
                    },
                )
                self.assertEqual(len(arm["verifiers"]), 3)
                self.assertTrue(all(item["passed"] for item in arm["verifiers"]))
                self.assertTrue(
                    all(item["elapsed_ms"] == 1 for item in arm["verifiers"])
                )
                self.assertEqual(arm["elapsed_scope"], "paired_case_wall_time")
                rendered = json.dumps(arm)
                self.assertNotIn(str(fixture / "baseline-candidate"), rendered)
                self.assertNotIn("source-thread", rendered)
            self.assertEqual(
                baseline["source_session_sha256"], workflow["source_session_sha256"]
            )

            score = subprocess.run(
                [
                    sys.executable,
                    str(SCORER),
                    str(outputs["baseline"]),
                    str(outputs["workflow"]),
                ],
                cwd=ROOT,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(score.returncode, 0, score.stderr)
            self.assertEqual(
                json.loads(score.stdout)["by_mode"]["workflow"][
                    "verified_completion_count"
                ],
                1,
            )

            requests = read_json_lines(fixture / "codex-requests.jsonl")
            self.assertEqual(
                [request.get("method") for request in requests],
                [
                    "initialize",
                    "initialized",
                    "thread/start",
                    "turn/start",
                    "thread/read",
                    "turn/interrupt",
                    "thread/read",
                    "turn/start",
                    "thread/read",
                    "turn/interrupt",
                    "thread/read",
                    "thread/inject_items",
                ],
            )
            self.assertFalse(requests[0]["params"]["capabilities"]["experimentalApi"])
            self.assertIn("Goal:", requests[3]["params"]["input"][0]["text"])
            self.assertNotIn(
                "Do not edit files", requests[3]["params"]["input"][0]["text"]
            )
            self.assertIn("Late steering", requests[7]["params"]["input"][0]["text"])
            self.assertEqual(requests[-1]["method"], "thread/inject_items")
            self.assertEqual(len(requests[-1]["params"]["items"]), 4)
            self.assertEqual(
                sum(
                    len(item["content"][0]["text"])
                    for item in requests[-1]["params"]["items"]
                ),
                64,
            )
            self.assertNotIn("collaborationMode", requests[3]["params"])
            self.assertNotIn("collaborationMode", requests[8]["params"])
            self.assertEqual(requests[2]["params"]["model"], "gpt-5.6-luna")
            self.assertEqual(requests[2]["params"]["effort"], "max")
            self.assertEqual(requests[2]["params"]["sandbox"], "read-only")
            self.assertEqual(requests[2]["params"]["approvalPolicy"], "never")
            commands = read_json_lines(fixture / "driftctl-requests.jsonl")
            self.assertEqual(
                commands[0]["arguments"][:4],
                ["compare", "codex", "--session", "source-thread"],
            )
            verify_calls = [
                item for item in commands if item["arguments"][0] == "verify"
            ]
            self.assertEqual(len(verify_calls), 6)
            self.assertTrue(all("--" in item["arguments"] for item in verify_calls))
            private = next(
                (fixture / "artifacts").glob(
                    "01-steering-retry-native-run-*/artifacts/*.json"
                )
            )
            self.assertIn("source-thread", private.read_text(encoding="utf-8"))
            self.assertEqual(stat.S_IMODE(private.parent.stat().st_mode), 0o700)
            self.assertEqual(stat.S_IMODE(private.stat().st_mode), 0o600)
            private_run = private.parent.parent
            self.assertEqual(stat.S_IMODE(private_run.stat().st_mode), 0o700)
            self.assertTrue((private_run / "source-workspace").is_dir())
            self.assertTrue((private_run / "state").is_dir())
            self.assertEqual(stat.S_IMODE((private_run / "tmp").stat().st_mode), 0o700)
            self.assertTrue(
                all(item["tmpdir"] == str(private_run / "tmp") for item in commands)
            )

    def test_continues_when_context_injection_is_not_supported(self) -> None:
        with temporary_fixture() as fixture:
            result, outputs = run_fixture(fixture, {"FAKE_INJECTION_UNSUPPORTED": "1"})
            self.assertEqual(result["status"], "completed")
            baseline = json.loads(outputs["baseline"].read_text(encoding="utf-8"))
            injection = baseline["native_checkpoint"]["injection"]
            self.assertTrue(injection["attempted"])
            self.assertFalse(injection["accepted"])
            self.assertEqual(injection["reason"], "unsupported_or_rejected")

    def test_records_each_steering_update_as_an_ordered_native_turn(self) -> None:
        with temporary_fixture() as fixture:
            case = fixture / "case"
            shutil.copytree(CASE, case)
            case_path = case / "case.json"
            definition = json.loads(case_path.read_text(encoding="utf-8"))
            definition["steering"].append(
                {
                    "requirement": (
                        "Final correction: preserve the legacy token while applying "
                        "the authentication retry restriction."
                    )
                }
            )
            case_path.write_text(json.dumps(definition), encoding="utf-8")

            _, outputs = run_fixture(fixture, case=case)

            baseline = json.loads(outputs["baseline"].read_text(encoding="utf-8"))
            self.assertEqual(
                baseline["native_checkpoint"]["source_turn_labels"],
                ["initial", "steering-1", "steering-2"],
            )
            self.assertEqual(
                baseline["native_checkpoint"]["source_user_turn_count"], 3
            )
            requests = read_json_lines(fixture / "codex-requests.jsonl")
            turn_text = [
                request["params"]["input"][0]["text"]
                for request in requests
                if request.get("method") == "turn/start"
            ]
            self.assertEqual(len(turn_text), 3)
            self.assertIn("Late steering 1", turn_text[1])
            self.assertIn("Late steering 2", turn_text[2])
            self.assertEqual(requests[-1]["method"], "thread/inject_items")

    def test_scope_violation_blocks_verified_completion(self) -> None:
        with temporary_fixture() as fixture:
            _, outputs = run_fixture(fixture, {"FAKE_EXTRA_CHANGED_PATH": "tasks/plan.md"})
            for output in outputs.values():
                arm = json.loads(output.read_text(encoding="utf-8"))
                self.assertFalse(arm["scope"]["passed"])
                self.assertEqual(
                    arm["scope"]["unexpected_changed_paths"], ["tasks/plan.md"]
                )
                self.assertFalse(arm["verified_completion"])
                self.assertEqual(arm["status"], "completed")

    def test_rejects_comparison_that_does_not_prove_equal_starting_manifests(self) -> None:
        with temporary_fixture() as fixture:
            with self.assertRaisesRegex(
                AssertionError, "starting manifests are not equal"
            ):
                run_fixture(fixture, {"FAKE_UNEQUAL_MANIFEST": "1"})

    def test_runs_one_information_matched_plain_summary_control(self) -> None:
        with temporary_fixture() as fixture:
            case = fixture / "case"
            shutil.copytree(CASE, case)
            summary = case / "plain-summary.txt"
            summary.write_text(
                "Retry transient service failures once. Never retry 401 or 403.\n",
                encoding="utf-8",
            )

            result, outputs = run_fixture(
                fixture,
                case=case,
                plain_summary_file=summary,
            )

            self.assertEqual(
                set(result["result_files"]),
                {"baseline", "plain_summary", "workflow"},
            )
            control = json.loads(
                outputs["plain_summary"].read_text(encoding="utf-8")
            )
            self.assertEqual(control["mode"], "plain_summary")
            self.assertTrue(control["verified_completion"])
            self.assertEqual(control["control_context"]["kind"], "flat_plain_summary")
            self.assertEqual(
                control["control_context"]["sha256"],
                "sha256:13d27fea77ce8815376f260447b03592d3064e9b8b18c16939bd91bde10cf5e8",
            )
            self.assertNotIn(summary.read_text(encoding="utf-8"), json.dumps(control))

            requests = read_json_lines(fixture / "codex-requests.jsonl")
            control_turns = [
                request
                for request in requests
                if request.get("method") == "turn/start"
                and request.get("params", {}).get("threadId") == "summary-child"
            ]
            self.assertEqual(len(control_turns), 1)
            prompt = control_turns[0]["params"]["input"][0]["text"]
            self.assertIn("Continue the task from this checkpoint", prompt)
            self.assertIn("Never retry 401 or 403", prompt)

    def test_waits_for_plain_summary_control_terminal_notification(self) -> None:
        with temporary_fixture() as fixture:
            case = fixture / "case"
            shutil.copytree(CASE, case)
            summary = case / "plain-summary.txt"
            summary.write_text("Preserve all active constraints.\n", encoding="utf-8")

            _, outputs = run_fixture(
                fixture,
                {"FAKE_SUMMARY_IN_PROGRESS": "1"},
                case,
                summary,
            )

            control = json.loads(
                outputs["plain_summary"].read_text(encoding="utf-8")
            )
            self.assertEqual(control["turn_status"], "completed")

    def test_accepts_a_source_turn_that_finished_before_interrupt(self) -> None:
        with temporary_fixture() as fixture:
            result, outputs = run_fixture(fixture, {"FAKE_NO_ACTIVE_TURN": "1"})
            self.assertEqual(result["status"], "completed")
            baseline = json.loads(outputs["baseline"].read_text(encoding="utf-8"))
            self.assertEqual(baseline["native_checkpoint"]["source_user_turn_count"], 2)

    def test_retries_a_private_native_read_rejection_without_exposing_it(self) -> None:
        with temporary_fixture() as fixture:
            result, _ = run_fixture(fixture, {"FAKE_FIRST_READ_REJECTED": "1"})
            self.assertEqual(result["status"], "completed")

    def test_rejects_a_returned_turn_id_when_the_user_message_is_not_durable(
        self,
    ) -> None:
        with temporary_fixture() as fixture:
            with self.assertRaisesRegex(
                AssertionError, "did not durably retain exactly 2 user messages"
            ):
                run_fixture(fixture, {"FAKE_MISSING_SECOND_USER": "1"})


class temporary_fixture:
    def __enter__(self) -> Path:
        self.temporary = tempfile.TemporaryDirectory(
            prefix="driftctl-native-eval-test-"
        )
        root = Path(self.temporary.name)
        for name, contents in {
            "fake-codex.py": FAKE_CODEX,
            "fake-driftctl.py": FAKE_DRIFTCTL,
        }.items():
            path = root / name
            path.write_text(textwrap.dedent(contents), encoding="utf-8")
            path.chmod(path.stat().st_mode | stat.S_IXUSR)
        for name in ("baseline-candidate", "workflow-candidate"):
            (root / name).mkdir()
        return root

    def __exit__(self, *_: object) -> None:
        self.temporary.cleanup()


def run_fixture(
    root: Path,
    extra: dict[str, str] | None = None,
    case: Path = CASE,
    plain_summary_file: Path | None = None,
) -> tuple[dict[str, object], dict[str, Path]]:
    results = root / "results"
    environment = os.environ | {
        "FAKE_CODEX_REQUESTS": str(root / "codex-requests.jsonl"),
        "FAKE_DRIFTCTL_REQUESTS": str(root / "driftctl-requests.jsonl"),
        "FAKE_BASELINE_CANDIDATE": str(root / "baseline-candidate"),
        "FAKE_WORKFLOW_CANDIDATE": str(root / "workflow-candidate"),
    }
    if extra:
        environment.update(extra)
    arguments = [
            sys.executable,
            str(RUNNER),
            "--case",
            str(case),
            "--results-dir",
            str(results),
            "--driftctl-bin",
            str(root / "fake-driftctl.py"),
            "--codex-bin",
            str(root / "fake-codex.py"),
            "--context-bytes",
            "64",
            "--artifacts",
            str(root / "artifacts"),
        ]
    if plain_summary_file is not None:
        arguments.extend(["--plain-summary-file", str(plain_summary_file)])
    completed = subprocess.run(
        arguments,
        cwd=ROOT,
        env=environment,
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        raise AssertionError(
            f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
        )
    manifest = json.loads(completed.stdout)
    return manifest, {
        mode: results / filename for mode, filename in manifest["result_files"].items()
    }


def read_json_lines(path: Path) -> list[dict[str, object]]:
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


FAKE_CODEX = """\
#!/usr/bin/env python3
import json
import os
import sys

turns_path = os.environ["FAKE_CODEX_REQUESTS"] + ".turns"
try:
    with open(turns_path, encoding="utf-8") as source:
        turns = json.load(source)
except FileNotFoundError:
    turns = []
child_cwd = None
child_goal = None
read_count = 0
for raw in sys.stdin:
    request = json.loads(raw)
    with open(os.environ["FAKE_CODEX_REQUESTS"], "a", encoding="utf-8") as output:
        output.write(json.dumps(request) + "\\n")
    method = request["method"]
    if method == "initialized":
        continue
    if method == "initialize":
        result = {"userAgent": "fake"}
    elif method == "thread/start":
        result = {"thread": {"id": "source-thread", "cwd": request["params"]["cwd"], "ephemeral": False}}
    elif method == "turn/start":
        if request["params"]["threadId"] == "source-thread":
            turns.append({"items": [{"type": "userMessage", "content": request["params"]["input"]}]})
            with open(turns_path, "w", encoding="utf-8") as output:
                json.dump(turns, output)
            result = {"turn": {"id": "source-" + str(request["id"]), "status": "inProgress"}}
        else:
            result = {"turn": {"id": "summary-turn", "status": "inProgress" if os.environ.get("FAKE_SUMMARY_IN_PROGRESS") else "completed"}}
    elif method == "turn/interrupt":
        if os.environ.get("FAKE_NO_ACTIVE_TURN"):
            print(json.dumps({"id": request["id"], "error": {"message": "no active turn to interrupt"}}), flush=True)
            continue
        result = {}
    elif method == "thread/inject_items" and os.environ.get("FAKE_INJECTION_UNSUPPORTED"):
        print(json.dumps({"id": request["id"], "error": {"message": "unsupported"}}), flush=True)
        continue
    elif method == "thread/inject_items":
        result = {"accepted": True}
    elif method == "thread/read":
        read_count += 1
        if os.environ.get("FAKE_FIRST_READ_REJECTED") and read_count == 1:
            print(json.dumps({"id": request["id"], "error": {"message": "/private/session/path is empty"}}), flush=True)
            continue
        retained = turns[:1] if os.environ.get("FAKE_MISSING_SECOND_USER") and len(turns) == 2 else turns
        result = {"thread": {"id": "source-thread", "turns": retained}}
    elif method == "thread/fork":
        child_cwd = request["params"]["cwd"]
        result = {"thread": {"id": "summary-child", "cwd": request["params"]["cwd"], "ephemeral": False}}
    elif method == "thread/goal/get":
        result = {"goal": None if child_goal is None else {"threadId": "summary-child", "objective": child_goal}}
    elif method == "thread/goal/clear":
        child_goal = None
        result = {"cleared": True}
    elif method == "thread/goal/set":
        child_goal = request["params"]["objective"]
        result = {"goal": {"threadId": "summary-child", "objective": child_goal}}
    elif method == "thread/settings/update":
        result = {}
    elif method == "thread/resume":
        result = {
            "approvalPolicy": request["params"]["approvalPolicy"],
            "cwd": child_cwd,
            "model": request["params"]["model"],
            "reasoningEffort": "max",
            "sandbox": {"type": "workspaceWrite"},
            "thread": {"id": "summary-child", "cwd": child_cwd, "ephemeral": False},
        }
    else:
        print(json.dumps({"id": request["id"], "error": {"message": "unexpected"}}), flush=True)
        continue
    print(json.dumps({"id": request["id"], "result": result}), flush=True)
    if method == "turn/start" and request["params"]["threadId"] == "summary-child" and os.environ.get("FAKE_SUMMARY_IN_PROGRESS"):
        print(json.dumps({"method": "turn/completed", "params": {"threadId": "summary-child", "turn": {"id": "summary-turn", "status": "completed"}}}), flush=True)
"""

FAKE_DRIFTCTL = """\
#!/usr/bin/env python3
import json
import os
import sys

arguments = sys.argv[1:]
with open(os.environ["FAKE_DRIFTCTL_REQUESTS"], "a", encoding="utf-8") as output:
    output.write(json.dumps({"arguments": arguments, "tmpdir": os.environ.get("TMPDIR")}) + "\\n")
if arguments[0] == "compare":
    changed_paths = ["service_client.py"]
    if os.environ.get("FAKE_EXTRA_CHANGED_PATH"):
        changed_paths.append(os.environ["FAKE_EXTRA_CHANGED_PATH"])
    result = {
        "fairness": {
            "starting_manifest_equal": not bool(os.environ.get("FAKE_UNEQUAL_MANIFEST")),
            "neutral_prompt_equal": True,
            "worker_policy": {
                "model": "gpt-5.6-luna",
                "effort": "max",
                "sandbox": "workspace-write",
                "approval_policy": "never",
                "verified_readback": True,
            },
            "only_intended_input_difference": "workflow receives the bounded active-intent projection",
        },
        "parent_unchanged": True,
        "source_unchanged": True,
        "baseline": {"child_thread_id": "baseline-child", "child_cwd": os.environ["FAKE_BASELINE_CANDIDATE"], "turn_status": "completed", "changed_paths": changed_paths},
        "workflow": {"child_thread_id": "workflow-child", "child_cwd": os.environ["FAKE_WORKFLOW_CANDIDATE"], "turn_status": "completed", "changed_paths": changed_paths},
    }
    print(json.dumps(result))
    raise SystemExit(0)
if arguments[0] == "verify":
    print(json.dumps({"status": "passed", "artifact_id": "private", "command_digest": "sha256:command", "verifier_digest": "sha256:verifier", "candidate_before_digest": "sha256:before", "candidate_after_digest": "sha256:after", "elapsed_ms": 1}))
    raise SystemExit(0)
raise SystemExit(3)
"""


if __name__ == "__main__":
    unittest.main()
