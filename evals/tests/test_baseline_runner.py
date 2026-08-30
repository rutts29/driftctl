"""Behavior checks for the direct-Codex baseline runner."""

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

from evals.runner.run_baseline import RunnerError, load_case


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
RUNNER = REPOSITORY_ROOT / "evals" / "runner" / "run_baseline.py"
CASE = REPOSITORY_ROOT / "evals" / "cases" / "01-steering-retry"


class BaselineRunnerTests(unittest.TestCase):
    """Exercise fresh interrupted turns against a deterministic Codex substitute."""

    def test_rejects_ambiguous_or_non_file_scope_paths(self) -> None:
        invalid_values = (
            [],
            ["service_client.py", "service_client.py"],
            ["../service_client.py"],
            ["*.py"],
            ["tests"],
            ["missing.py"],
        )
        for value in invalid_values:
            with self.subTest(value=value), tempfile.TemporaryDirectory(
                prefix="driftctl-case-scope-test-"
            ) as temporary:
                case = Path(temporary) / "case"
                shutil.copytree(CASE, case)
                case_path = case / "case.json"
                definition = json.loads(case_path.read_text(encoding="utf-8"))
                definition["allowed_changed_paths"] = value
                case_path.write_text(json.dumps(definition), encoding="utf-8")

                with self.assertRaises(RunnerError):
                    load_case(case)

    def test_runs_fresh_turn_after_interruption_then_verifies_workspace(self) -> None:
        with tempfile.TemporaryDirectory(prefix="driftctl-baseline-test-") as temporary:
            temporary_path = Path(temporary)
            fake_codex = temporary_path / "fake-codex"
            arguments = temporary_path / "arguments.txt"
            calls = temporary_path / "calls.txt"
            artifacts = temporary_path / "artifacts"
            self._write_fake_codex(fake_codex)

            environment = os.environ | {
                "FAKE_CODEX_ARGUMENTS": str(arguments),
                "FAKE_CODEX_CALLS": str(calls),
            }
            completed = subprocess.run(
                [
                    sys.executable,
                    str(RUNNER),
                    "--case",
                    str(CASE),
                    "--codex-bin",
                    str(fake_codex),
                    "--artifacts",
                    str(artifacts),
                ],
                cwd=REPOSITORY_ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(
                completed.returncode,
                0,
                msg=f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}",
            )
            result = json.loads(completed.stdout)
            self.assertEqual(result["case_id"], "01-steering-retry")
            self.assertEqual(result["mode"], "baseline")
            self.assertEqual(result["interruption"], "fresh_agent_session")
            self.assertEqual(result["recovery_context"], "worktree_only")
            self.assertEqual(result["lost_steering_count"], 1)
            self.assertEqual(result["thread_id"], "fixture-thread")
            self.assertEqual(result["changed_paths"], ["service_client.py"])
            self.assertEqual(
                result["scope"],
                {
                    "allowed_changed_paths": [
                        "service_client.py",
                        "tests/test_unit_client.py",
                    ],
                    "passed": True,
                    "unexpected_changed_paths": [],
                },
            )
            self.assertTrue(result["verified_completion"])
            self.assertEqual(
                result["trajectory_files"],
                [
                    "01-steering-retry-baseline-initial.jsonl",
                    "01-steering-retry-baseline-recovery-1.jsonl",
                ],
            )
            self.assertTrue(all(verifier["passed"] for verifier in result["verifiers"]))
            self.assertEqual(
                result["token_usage"],
                {
                    "available": True,
                    "cached_input_tokens": 7,
                    "input_tokens": 18,
                    "output_tokens": 8,
                    "total_tokens": 26,
                },
            )
            self.assertEqual(
                result["premature_completion"],
                {
                    "available": True,
                    "detected": False,
                    "phase": "recovery-1",
                },
            )

            captured = arguments.read_text(encoding="utf-8")
            self.assertIn(
                "exec\n--json\n--ephemeral\n--sandbox\nworkspace-write\n",
                captured,
            )
            self.assertNotIn("exec\nresume\n", captured)
            self.assertEqual(captured.splitlines().count("--ephemeral"), 2)
            self.assertIn("No durable task record is available", captured)
            self.assertIn(
                "Change only service_client.py and tests/test_unit_client.py",
                captured,
            )
            self.assertNotIn("Do not retry 401 or 403 authentication failures", captured)
            self.assertEqual(calls.read_text(encoding="utf-8").splitlines(), ["1", "2"])
            initial_events = [
                json.loads(line)
                for line in (
                    artifacts / "01-steering-retry-baseline-initial.jsonl"
                ).read_text(encoding="utf-8").splitlines()
            ]
            self.assertEqual(initial_events[0]["type"], "thread.started")
            self.assertEqual(initial_events[-1]["type"], "turn.completed")

    def _write_fake_codex(self, path: Path) -> None:
        script = textwrap.dedent(
            '''\
            #!/bin/sh
            set -eu

            count=0
            if [ -f "$FAKE_CODEX_CALLS" ]; then
              count=$(wc -l < "$FAKE_CODEX_CALLS")
            fi
            count=$((count + 1))
            printf '%s\\n' "$count" >> "$FAKE_CODEX_CALLS"
            for argument in "$@"; do
              printf '%s\\n' "$argument" >> "$FAKE_CODEX_ARGUMENTS"
            done
            printf '%s\\n' '---' >> "$FAKE_CODEX_ARGUMENTS"
            test ! -e tests/test_integration_checkout.py

            if [ "$count" -eq 2 ]; then
              printf '%s\\n' '{"type":"thread.started","thread_id":"fixture-thread-2"}'
              printf '%s\\n' '{"type":"turn.started"}'
              printf '%s\\n' '{"type":"turn.completed","usage":{"input_tokens":7,"cached_input_tokens":3,"output_tokens":5}}'
              exit 0
            fi

            test "$1" = "exec"
            cat > service_client.py <<'PYTHON'
            """Small client used by the checkout service."""

            from dataclasses import dataclass
            from typing import Protocol


            @dataclass(frozen=True)
            class ServiceResponse:
                status_code: int
                body: str


            class Transport(Protocol):
                def send(self, operation: str, token: str) -> ServiceResponse:
                    """Send one operation to the remote service."""


            class ServiceClient:
                def __init__(self, transport: Transport, max_retries: int = 0) -> None:
                    self._transport = transport
                    self._max_retries = max_retries

                def send(self, operation: str, token: str) -> ServiceResponse:
                    response = self._transport.send(operation, token)
                    if (
                        response.status_code >= 500
                        and self._max_retries > 0
                        and response.status_code not in (401, 403)
                    ):
                        return self._transport.send(operation, token)
                    return response
            PYTHON
            printf '%s\\n' '{"type":"thread.started","thread_id":"fixture-thread"}'
            printf '%s\\n' '{"type":"turn.started"}'
            printf '%s\\n' '{"type":"turn.completed","usage":{"input_tokens":11,"cached_input_tokens":4,"output_tokens":3}}'
            '''
        )
        path.write_text(script, encoding="utf-8")
        path.chmod(path.stat().st_mode | stat.S_IXUSR)


if __name__ == "__main__":
    unittest.main()
