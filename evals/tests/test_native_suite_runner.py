"""Deterministic tests for the bounded all-case native suite runner."""

from __future__ import annotations

import json
from pathlib import Path
import os
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "evals" / "calibration" / "manifest.json"
RUNNER = ROOT / "evals" / "runner" / "run_native_suite.py"


class NativeSuiteRunnerTests(unittest.TestCase):
    """Exercise orchestration without invoking Codex or a paid provider."""

    def test_runs_frozen_cases_once_and_only_flagship_gets_plain_summary(self) -> None:
        with fake_runner() as fixture:
            completed = run_suite(fixture)

            self.assertEqual(completed.returncode, 0, completed.stderr)
            status = json.loads(completed.stdout)
            self.assertEqual(status["status"], "completed")
            self.assertEqual(
                [case["case_id"] for case in status["cases"]],
                [
                    "01-steering-retry",
                    "02-steering-pagination",
                    "03-steering-atomic-import",
                    "04-scope-csv-export",
                    "05-rename-resume",
                ],
            )
            self.assertEqual(
                [case["exit_code"] for case in status["cases"]], [0] * 5
            )
            self.assertEqual(
                status["result_files"]["02-steering-pagination"],
                {
                    "baseline": "02-steering-pagination-native-baseline.json",
                    "plain_summary": "02-steering-pagination-native-plain_summary.json",
                    "workflow": "02-steering-pagination-native-workflow.json",
                },
            )
            self.assertIn(
                "python3 evals/runner/run_native_suite.py",
                status["reproduction_command"],
            )
            self.assertNotIn(str(fixture), json.dumps(status))

            invocations = read_json_lines(fixture / "invocations.jsonl")
            self.assertEqual(
                [Path(item["case"]).name for item in invocations],
                [case["case_id"] for case in status["cases"]],
            )
            self.assertEqual(len(invocations), 5)
            for invocation in invocations:
                self.assertEqual(invocation["context_bytes"], "131072")
                self.assertEqual(invocation["worker_model"], "gpt-5.6-luna")
                self.assertEqual(invocation["worker_effort"], "max")
                if Path(invocation["case"]).name == "02-steering-pagination":
                    self.assertIsNotNone(invocation["plain_summary_file"])
                else:
                    self.assertIsNone(invocation["plain_summary_file"])

            status_file = fixture / "results" / "native-suite-status.json"
            self.assertEqual(json.loads(status_file.read_text()), status)

    def test_nonzero_case_is_retained_as_invalid_and_later_cases_still_run(self) -> None:
        with fake_runner(failing_case="03-steering-atomic-import") as fixture:
            completed = run_suite(fixture)

            self.assertEqual(completed.returncode, 1)
            status = json.loads(completed.stdout)
            self.assertEqual(status["status"], "invalid")
            self.assertEqual(len(status["cases"]), 5)
            failed = status["cases"][2]
            self.assertEqual(failed["case_id"], "03-steering-atomic-import")
            self.assertEqual(failed["exit_code"], 17)
            self.assertEqual(failed["status"], "infrastructure-invalid")
            self.assertIn("provider failure", failed["stdout"])
            self.assertIn("<local-path>", failed["stderr"])
            self.assertNotIn("/private/provider/session.json", json.dumps(status))
            self.assertEqual(
                [
                    Path(item["case"]).name
                    for item in read_json_lines(fixture / "invocations.jsonl")
                ],
                [case["case_id"] for case in status["cases"]],
            )

    def test_source_linked_safety_block_is_not_infrastructure_invalid(self) -> None:
        with fake_runner(safety_case="05-rename-resume") as fixture:
            completed = run_suite(fixture)

            self.assertEqual(completed.returncode, 0, completed.stderr)
            status = json.loads(completed.stdout)
            self.assertEqual(status["status"], "completed_with_safety_blocks")
            self.assertEqual(status["invalid_case_count"], 0)
            self.assertEqual(status["safety_blocked_case_count"], 1)
            blocked = status["cases"][-1]
            self.assertEqual(blocked["status"], "safety-blocked")
            self.assertEqual(blocked["result_files"], {})
            self.assertEqual(blocked["runner_status"], "safety_blocked")

    def test_fingerprint_drift_blocks_all_native_invocations(self) -> None:
        with fake_runner() as fixture:
            manifest = fixture / "manifest.json"
            manifest.write_text(MANIFEST.read_text(encoding="utf-8"), encoding="utf-8")
            value = json.loads(manifest.read_text(encoding="utf-8"))
            value["suite_fingerprint_sha256"] = "0" * 64
            manifest.write_text(json.dumps(value), encoding="utf-8")

            completed = run_suite(fixture, manifest=manifest)

            self.assertEqual(completed.returncode, 1)
            status = json.loads(completed.stdout)
            self.assertEqual(status["status"], "invalid")
            self.assertEqual(status["cases"], [])
            self.assertIn("drift", status["preflight"]["error"])
            self.assertFalse((fixture / "invocations.jsonl").exists())


def run_suite(
    fixture: Path,
    *,
    manifest: Path = MANIFEST,
) -> subprocess.CompletedProcess[str]:
    arguments = [
        sys.executable,
        str(RUNNER),
        "--manifest",
        str(manifest),
        "--results-dir",
        str(fixture / "results"),
        "--native-runner",
        str(fixture / "fake-native-runner.py"),
        "--driftctl-bin",
        "fake-driftctl",
        "--codex-bin",
        "fake-codex",
    ]
    return subprocess.run(
        arguments,
        cwd=ROOT,
        env=os.environ | {"FAKE_SUITE_INVOCATIONS": str(fixture / "invocations.jsonl")},
        capture_output=True,
        text=True,
        check=False,
    )


class fake_runner:
    def __init__(
        self, failing_case: str | None = None, safety_case: str | None = None
    ) -> None:
        self.failing_case = failing_case
        self.safety_case = safety_case

    def __enter__(self) -> Path:
        self.temporary = tempfile.TemporaryDirectory(prefix="driftctl-suite-test-")
        root = Path(self.temporary.name)
        code = FAKE_NATIVE_RUNNER.replace(
            "__FAILING_CASE__", self.failing_case or ""
        ).replace("__SAFETY_CASE__", self.safety_case or "")
        runner = root / "fake-native-runner.py"
        runner.write_text(textwrap.dedent(code), encoding="utf-8")
        runner.chmod(runner.stat().st_mode | stat.S_IXUSR)
        return root

    def __exit__(self, *_: object) -> None:
        self.temporary.cleanup()


def read_json_lines(path: Path) -> list[dict[str, object]]:
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


FAKE_NATIVE_RUNNER = r"""
#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

arguments = sys.argv[1:]
def value(name):
    return arguments[arguments.index(name) + 1]

case = Path(value("--case"))
invocation = {
    "case": str(case),
    "context_bytes": value("--context-bytes"),
    "worker_model": value("--worker-model"),
    "worker_effort": value("--worker-effort"),
    "plain_summary_file": value("--plain-summary-file") if "--plain-summary-file" in arguments else None,
}
with open(os.environ["FAKE_SUITE_INVOCATIONS"], "a", encoding="utf-8") as output:
    output.write(json.dumps(invocation) + "\n")

if case.name == "__FAILING_CASE__":
    print("provider failure: /private/provider/session.json")
    print("stderr /private/provider/session.json", file=sys.stderr)
    raise SystemExit(17)

if case.name == "__SAFETY_CASE__":
    print(json.dumps({
        "case_id": case.name,
        "result_files": {},
        "safety_block": {"kind": "unresolved_intent_conflict"},
        "status": "safety_blocked",
    }))
    raise SystemExit(0)

files = {
    "baseline": f"{case.name}-native-baseline.json",
    "workflow": f"{case.name}-native-workflow.json",
}
if case.name == "02-steering-pagination":
    files["plain_summary"] = f"{case.name}-native-plain_summary.json"
print(json.dumps({"case_id": case.name, "result_files": files, "status": "completed"}))
"""


if __name__ == "__main__":
    unittest.main()
