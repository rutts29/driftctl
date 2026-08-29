"""Behavior checks for sanitizing provider trajectories for publication."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
RUNNER = REPOSITORY_ROOT / "evals" / "runner" / "sanitize_trajectory.py"


class SanitizeTrajectoryTests(unittest.TestCase):
    """Exercise redaction, bounded output, and strict JSONL handling."""

    def test_redacts_paths_and_thread_ids_while_preserving_events_and_usage(self) -> None:
        home = Path.home().as_posix()
        supplied_home = "/custom/agent-home"
        temporary_workspace = "/tmp/driftctl-case-123/workspace/run/check"
        long_output = "0123456789" * 20
        source_events = [
            {
                "type": "thread.started",
                "thread_id": "private-thread-a",
                "nested": [
                    home + "/.agents/skills",
                    supplied_home + "/private.txt",
                    temporary_workspace,
                ],
            },
            {
                "type": "item.completed",
                "item": {
                    "type": "command_execution",
                    "command": f"cd {home}/project && {temporary_workspace}",
                    "aggregated_output": long_output,
                    "thread_id": "private-thread-a",
                },
            },
            {
                "type": "item.completed",
                "item": {
                    "type": "mcp_tool_call",
                    "result": {"content": [{"text": long_output}]},
                    "thread_id": "private-thread-b",
                },
            },
            {
                "type": "turn.completed",
                "usage": {
                    "input_tokens": 101,
                    "cached_input_tokens": 97,
                    "output_tokens": 13,
                    "reasoning_output_tokens": 8,
                },
            },
        ]

        with tempfile.TemporaryDirectory(prefix="driftctl-sanitize-test-") as temporary:
            directory = Path(temporary)
            source = directory / "raw.jsonl"
            destination = directory / "sanitized.jsonl"
            source.write_text(
                "".join(json.dumps(event) + "\n" for event in source_events),
                encoding="utf-8",
            )

            completed = subprocess.run(
                [
                    sys.executable,
                    str(RUNNER),
                    str(source),
                    str(destination),
                    "--max-output-chars",
                    "32",
                    "--home",
                    supplied_home,
                ],
                cwd=REPOSITORY_ROOT,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(completed.returncode, 0, completed.stderr)
            records = [json.loads(line) for line in destination.read_text().splitlines()]
            self.assertEqual(records[0]["record_type"], "metadata")
            self.assertEqual(records[0]["event_count"], len(source_events))
            self.assertEqual(len(records) - 1, len(source_events))
            self.assertEqual(records[1]["thread_id"], "thread-1")
            self.assertEqual(records[2]["item"]["thread_id"], "thread-1")
            self.assertEqual(records[3]["item"]["thread_id"], "thread-2")
            self.assertEqual(
                records[4]["usage"],
                source_events[3]["usage"],
            )

            serialized = destination.read_text(encoding="utf-8")
            self.assertNotIn(home, serialized)
            self.assertNotIn(supplied_home, serialized)
            self.assertNotIn(temporary_workspace, serialized)
            self.assertIn("$HOME/.agents/skills", serialized)
            self.assertIn("$WORKSPACE/run/check", serialized)
            self.assertIn("[... omitted", serialized)
            self.assertLessEqual(len(records[2]["item"]["aggregated_output"]), 32)
            self.assertLessEqual(
                len(records[3]["item"]["result"]["content"][0]["text"]),
                32,
            )

    def test_rejects_malformed_jsonl_without_overwriting_destination(self) -> None:
        with tempfile.TemporaryDirectory(prefix="driftctl-sanitize-test-") as temporary:
            directory = Path(temporary)
            source = directory / "raw.jsonl"
            destination = directory / "sanitized.jsonl"
            source.write_text('{"type":"thread.started"}\nnot-json\n', encoding="utf-8")
            destination.write_text("keep this file", encoding="utf-8")

            completed = subprocess.run(
                [sys.executable, str(RUNNER), str(source), str(destination)],
                cwd=REPOSITORY_ROOT,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(completed.returncode, 1)
            self.assertIn("line 2", completed.stderr)
            self.assertEqual(destination.read_text(encoding="utf-8"), "keep this file")

    def test_rejects_non_object_jsonl_records(self) -> None:
        with tempfile.TemporaryDirectory(prefix="driftctl-sanitize-test-") as temporary:
            directory = Path(temporary)
            source = directory / "raw.jsonl"
            destination = directory / "sanitized.jsonl"
            source.write_text("[]\n", encoding="utf-8")

            completed = subprocess.run(
                [sys.executable, str(RUNNER), str(source), str(destination)],
                cwd=REPOSITORY_ROOT,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(completed.returncode, 1)
            self.assertIn("JSON object", completed.stderr)
            self.assertFalse(destination.exists())


if __name__ == "__main__":
    unittest.main()
