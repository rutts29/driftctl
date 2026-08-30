"""Fake-process tests for the intact-native-session evaluation runner."""

from __future__ import annotations

import json
import os
from pathlib import Path
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
                self.assertTrue(arm["native_checkpoint"]["source_workspace_clean"])
                self.assertTrue(arm["native_checkpoint"]["injection"]["accepted"])
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
                    "thread/inject_items",
                    "turn/start",
                    "thread/settings/update",
                ],
            )
            self.assertTrue(requests[0]["params"]["capabilities"]["experimentalApi"])
            self.assertIn("Goal:", requests[3]["params"]["input"][0]["text"])
            self.assertNotIn(
                "Do not edit files", requests[3]["params"]["input"][0]["text"]
            )
            self.assertIn("Late steering", requests[5]["params"]["input"][0]["text"])
            self.assertEqual(requests[3]["params"]["collaborationMode"]["mode"], "plan")
            self.assertEqual(requests[5]["params"]["collaborationMode"]["mode"], "plan")
            self.assertEqual(
                requests[6]["params"]["collaborationMode"]["mode"], "default"
            )
            self.assertEqual(requests[2]["params"]["model"], "gpt-5.6-luna")
            self.assertEqual(requests[2]["params"]["effort"], "max")
            self.assertEqual(requests[2]["params"]["sandbox"], "workspace-write")
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

    def test_continues_when_context_injection_is_not_supported(self) -> None:
        with temporary_fixture() as fixture:
            result, outputs = run_fixture(fixture, {"FAKE_INJECTION_UNSUPPORTED": "1"})
            self.assertEqual(result["status"], "completed")
            baseline = json.loads(outputs["baseline"].read_text(encoding="utf-8"))
            injection = baseline["native_checkpoint"]["injection"]
            self.assertTrue(injection["attempted"])
            self.assertFalse(injection["accepted"])
            self.assertEqual(injection["reason"], "unsupported_or_rejected")


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
    root: Path, extra: dict[str, str] | None = None
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
    completed = subprocess.run(
        [
            sys.executable,
            str(RUNNER),
            "--case",
            str(CASE),
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
        ],
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
        result = {"turn": {"id": "planning-" + str(request["id"]), "status": "completed"}}
    elif method == "thread/inject_items" and os.environ.get("FAKE_INJECTION_UNSUPPORTED"):
        print(json.dumps({"id": request["id"], "error": {"message": "unsupported"}}), flush=True)
        continue
    elif method == "thread/inject_items":
        result = {"accepted": True}
    elif method == "thread/settings/update":
        result = {}
    else:
        print(json.dumps({"id": request["id"], "error": {"message": "unexpected"}}), flush=True)
        continue
    print(json.dumps({"id": request["id"], "result": result}), flush=True)
"""

FAKE_DRIFTCTL = """\
#!/usr/bin/env python3
import json
import os
import sys

arguments = sys.argv[1:]
with open(os.environ["FAKE_DRIFTCTL_REQUESTS"], "a", encoding="utf-8") as output:
    output.write(json.dumps({"arguments": arguments}) + "\\n")
if arguments[0] == "compare":
    result = {
        "baseline": {"child_thread_id": "baseline-child", "child_cwd": os.environ["FAKE_BASELINE_CANDIDATE"], "turn_status": "completed", "changed_paths": ["service_client.py"]},
        "workflow": {"child_thread_id": "workflow-child", "child_cwd": os.environ["FAKE_WORKFLOW_CANDIDATE"], "turn_status": "completed", "changed_paths": ["service_client.py"]},
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
