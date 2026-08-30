"""Behavior checks for the durable driftctl workflow evaluator."""

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


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
RUNNER = REPOSITORY_ROOT / "evals" / "runner" / "run_workflow.py"
CASE = REPOSITORY_ROOT / "evals" / "cases" / "01-steering-retry"
DRIFTCTL = REPOSITORY_ROOT / "target" / "debug" / "driftctl"


class WorkflowRunnerTests(unittest.TestCase):
    """Exercise interruption, steering, and ledger-gated closure with a fake Codex."""

    @classmethod
    def setUpClass(cls) -> None:
        subprocess.run(
            ["cargo", "build", "--quiet", "--locked"],
            cwd=REPOSITORY_ROOT,
            check=True,
        )

    def test_resumes_from_ledger_and_closes_only_after_external_evidence(self) -> None:
        with tempfile.TemporaryDirectory(prefix="driftctl-workflow-test-") as temporary:
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
                    "--driftctl-bin",
                    str(DRIFTCTL),
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
            self.assertEqual(result["mode"], "workflow")
            self.assertEqual(result["changed_paths"], ["service_client.py"])
            self.assertTrue(result["scope"]["passed"])
            self.assertEqual(result["scope"]["unexpected_changed_paths"], [])
            self.assertEqual(result["recovered_steering_count"], 1)
            self.assertEqual(result["status"], "verified")
            self.assertTrue(result["verified_completion"])
            self.assertTrue(result["closure_is_evidence_gated"])
            self.assertEqual(result["closure_gate"]["exit_code"], 2)
            self.assertEqual(result["closure"]["exit_code"], 0)
            self.assertTrue(all(verifier["passed"] for verifier in result["verifiers"]))
            self.assertEqual(
                result["trajectory_files"],
                [
                    "01-steering-retry-workflow-initial.jsonl",
                    "01-steering-retry-workflow-steering-1.jsonl",
                ],
            )
            self.assertEqual(
                result["token_usage"],
                {
                    "available": True,
                    "cached_input_tokens": 0,
                    "input_tokens": 5,
                    "output_tokens": 3,
                    "total_tokens": 8,
                },
            )

            captured = arguments.read_text(encoding="utf-8")
            self.assertEqual(calls.read_text(encoding="utf-8").splitlines(), ["1", "2"])
            self.assertIn("Retry a failed service request once", captured)
            self.assertIn("Do not retry 401 or 403 authentication failures", captured)
            self.assertEqual(
                [turn["phase"] for turn in result["turns"]],
                ["initial", "steering-1"],
            )

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
            printf '%s\\n' "$@" >> "$FAKE_CODEX_ARGUMENTS"
            printf '%s\\n' '---' >> "$FAKE_CODEX_ARGUMENTS"
            test ! -e tests/test_integration_checkout.py

            if [ "$count" -eq 1 ]; then
              input_tokens=2
              output_tokens=1
            else
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
                        and response.status_code not in (401, 403)
                        and self._max_retries > 0
                    ):
                        return self._transport.send(operation, token)
                    return response
            PYTHON
              input_tokens=3
              output_tokens=2
            fi

            printf '%s\\n' "{\\"type\\":\\"thread.started\\",\\"thread_id\\":\\"workflow-$count\\"}"
            printf '%s\\n' '{"type":"turn.started"}'
            printf '%s\\n' "{\\"type\\":\\"turn.completed\\",\\"usage\\":{\\"input_tokens\\":$input_tokens,\\"output_tokens\\":$output_tokens}}"
            '''
        )
        path.write_text(script, encoding="utf-8")
        path.chmod(path.stat().st_mode | stat.S_IXUSR)


if __name__ == "__main__":
    unittest.main()
