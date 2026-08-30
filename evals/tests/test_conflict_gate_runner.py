"""Production-shaped tests for the unattended conflict safety gate."""

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
RUNNER = ROOT / "evals" / "runner" / "run_conflict_gate.py"


class ConflictGateRunnerTests(unittest.TestCase):
    def test_blocks_without_a_tty_and_preserves_the_source(self) -> None:
        with tempfile.TemporaryDirectory(prefix="driftctl-conflict-test-") as temporary:
            root = Path(temporary)
            codex = executable(root / "fake-codex.py", FAKE_CODEX)
            driftctl = executable(root / "fake-driftctl.py", FAKE_DRIFTCTL)
            result_path = root / "result.json"
            environment = os.environ | {
                "FAKE_DRIFTCTL_CAPTURE": str(root / "driftctl-call.json"),
            }

            completed = subprocess.run(
                [
                    sys.executable,
                    str(RUNNER),
                    "--results-file",
                    str(result_path),
                    "--driftctl-bin",
                    str(driftctl),
                    "--codex-bin",
                    str(codex),
                    "--artifacts",
                    str(root / "artifacts"),
                ],
                cwd=ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(completed.returncode, 0, completed.stderr)
            result = json.loads(result_path.read_text(encoding="utf-8"))
            self.assertEqual(result["status"], "verified")
            self.assertTrue(result["unsafe_continuation_blocked"])
            self.assertEqual(result["entrypoint_exit_code"], 2)
            self.assertTrue(result["checks"]["source_session_unchanged"])
            self.assertTrue(result["checks"]["source_workspace_unchanged"])
            self.assertTrue(result["checks"]["no_child_created"])
            self.assertTrue(result["checks"]["source_linked_alternatives"])
            self.assertTrue(result["checks"]["private_state_permissions"])
            self.assertNotIn("source-thread", json.dumps(result))
            self.assertNotIn(str(root), json.dumps(result))

            call = json.loads(
                (root / "driftctl-call.json").read_text(encoding="utf-8")
            )
            self.assertEqual(call["arguments"][:3], ["continue", "codex", "--session"])
            self.assertEqual(call["arguments"][-1], "--json")
            self.assertFalse(call["stdin_is_tty"])
            private = next((root / "artifacts").glob("conflict-gate-run-*/artifacts/*.json"))
            self.assertEqual(stat.S_IMODE(private.parent.stat().st_mode), 0o700)
            self.assertEqual(stat.S_IMODE(private.stat().st_mode), 0o600)


def executable(path: Path, content: str) -> Path:
    path.write_text(textwrap.dedent(content), encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


FAKE_CODEX = """\
#!/usr/bin/env python3
import json
import sys

turns = []
for raw in sys.stdin:
    request = json.loads(raw)
    method = request["method"]
    if method == "initialized":
        continue
    if method == "initialize":
        result = {"userAgent": "fake"}
    elif method == "thread/start":
        result = {"thread": {"id": "source-thread", "cwd": request["params"]["cwd"], "ephemeral": False}}
    elif method == "turn/start":
        turns.append({"items": [{"type": "userMessage", "text": request["params"]["input"][0]["text"]}]})
        result = {"turn": {"id": "turn-" + str(len(turns)), "status": "inProgress"}}
    elif method == "turn/interrupt":
        result = {}
    elif method == "thread/read":
        result = {"thread": {"id": "source-thread", "turns": turns}}
    elif method == "thread/list":
        result = {"data": [{"id": "source-thread", "cwd": request["params"]["cwd"]}], "nextCursor": None}
    elif method == "thread/goal/get":
        result = {"goal": None}
    else:
        print(json.dumps({"id": request["id"], "error": {"message": "unexpected " + method}}), flush=True)
        continue
    print(json.dumps({"id": request["id"], "result": result}), flush=True)
"""


FAKE_DRIFTCTL = """\
#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

Path(os.environ["FAKE_DRIFTCTL_CAPTURE"]).write_text(json.dumps({
    "arguments": sys.argv[1:],
    "stdin_is_tty": sys.stdin.isatty(),
}), encoding="utf-8")
state = Path(os.environ["XDG_STATE_HOME"]) / "driftctl" / "repositories" / "opaque" / "runs" / "run-1"
state.mkdir(parents=True, mode=0o700)
for parent in [state, *state.parents]:
    if str(parent).startswith(os.environ["XDG_STATE_HOME"]):
        parent.chmod(0o700)
(state / "pending.jsonl").write_text("{}\\n", encoding="utf-8")
(state / "pending.jsonl").chmod(0o600)
print(json.dumps({
    "status": "blocked",
    "blockers": [{"kind": "conflict", "id": "format-choice", "source_record_ids": ["u2", "u3"]}],
    "projection": {"conflicts": [{
        "id": "format-choice",
        "source_record_ids": ["u2", "u3"],
        "alternatives": [
            {"id": "json", "text": "Emit JSON", "source_record_ids": ["u2"]},
            {"id": "yaml", "text": "Emit YAML", "source_record_ids": ["u3"]},
        ],
    }]},
}))
raise SystemExit(2)
"""


if __name__ == "__main__":
    unittest.main()
