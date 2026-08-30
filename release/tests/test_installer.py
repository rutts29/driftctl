"""Real archive and installer boundary tests."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
PACKAGE = ROOT / "scripts" / "package-release.sh"
INSTALLER = ROOT / "scripts" / "install.sh"


class InstallerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        scratch = ROOT / "target" / "test-tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        cls.temporary = tempfile.TemporaryDirectory(
            prefix="driftctl-release-test-", dir=scratch
        )
        cls.root = Path(cls.temporary.name)
        metadata = json.loads(
            subprocess.run(
                ["cargo", "metadata", "--no-deps", "--format-version", "1"],
                cwd=ROOT,
                capture_output=True,
                text=True,
                check=True,
            ).stdout
        )
        package = next(item for item in metadata["packages"] if item["name"] == "driftctl")
        cls.version = "v" + package["version"]
        verbose = subprocess.run(
            ["rustc", "-vV"], capture_output=True, text=True, check=True
        ).stdout
        cls.target = next(
            line.removeprefix("host: ")
            for line in verbose.splitlines()
            if line.startswith("host: ")
        )
        cls.release = cls.root / "releases" / cls.version
        cls.release.mkdir(parents=True)
        subprocess.run(
            [
                "sh",
                str(PACKAGE),
                "--out",
                str(cls.release),
                "--target",
                cls.target,
            ],
            cwd=ROOT,
            env=os.environ | {"TMPDIR": str(cls.root)},
            capture_output=True,
            text=True,
            check=True,
        )

    @classmethod
    def tearDownClass(cls) -> None:
        cls.temporary.cleanup()

    def test_installs_a_checksummed_real_binary(self) -> None:
        binary = self.root / "installed" / "bin" / "driftctl"
        completed = self.install(binary.parent)

        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertTrue(binary.is_file())
        help_result = subprocess.run(
            [str(binary), "--help"], capture_output=True, text=True, check=False
        )
        self.assertEqual(help_result.returncode, 0)
        self.assertIn("driftctl inspect codex", help_result.stdout)

    def test_packaged_binary_runs_the_isolated_explicit_plugin_flow(self) -> None:
        flow = self.root / "plugin-flow"
        binary = flow / "bin" / "driftctl"
        completed = self.install(binary.parent)
        self.assertEqual(completed.returncode, 0, completed.stderr)

        workspace = flow / "workspace"
        workspace.mkdir(parents=True)
        fake_codex = self.write_fake_codex(flow / "provider")
        session_id = "thread-packaged-plugin"
        state_home = flow / "state"
        data_home = flow / "data"
        provider_root = flow / "provider"
        environment = os.environ | {
            "CODEX_HOME": str(flow / "codex-home"),
            "DRIFTCTL_ARTIFACT_DIR": str(provider_root / "artifacts"),
            "DRIFTCTL_CODEX_BIN": str(fake_codex),
            "DRIFTCTL_FAKE_PLUGIN_STATE": str(provider_root / "plugin-state"),
            "DRIFTCTL_FAKE_PROMPTS": str(provider_root / "prompts.jsonl"),
            "DRIFTCTL_FAKE_PROPOSAL": json.dumps(
                {
                    "schema_version": 1,
                    "goal": {
                        "text": "Preserve packaged plugin behavior.",
                        "source_record_ids": ["user-1:0"],
                    },
                    "accounted_source_record_ids": ["user-1:0"],
                    "operations": [
                        {
                            "operation": "add",
                            "key": "packaged-plugin-behavior",
                            "kind": "constraint",
                            "text": "Preserve packaged plugin behavior.",
                            "target_key": "",
                            "intent_keys": [],
                            "source_record_ids": ["user-1:0"],
                            "alternatives": [],
                        }
                    ],
                },
                separators=(",", ":"),
            ),
            "DRIFTCTL_FAKE_READ": json.dumps(
                {
                    "thread": {
                        "id": session_id,
                        "cwd": str(workspace.resolve()),
                        "turns": [
                            {
                                "items": [
                                    {
                                        "type": "userMessage",
                                        "id": "user-1",
                                        "content": [
                                            {
                                                "type": "text",
                                                "text": "Preserve packaged plugin behavior.",
                                            }
                                        ],
                                    }
                                ]
                            }
                        ],
                    }
                },
                separators=(",", ":"),
            ),
            "PATH": str(binary.parent) + os.pathsep + os.environ["PATH"],
            "TMPDIR": str(flow),
            "XDG_DATA_HOME": str(data_home),
            "XDG_STATE_HOME": str(state_home),
        }
        integrated = subprocess.run(
            [str(binary), "integrate", "codex", "install", "--json"],
            cwd=workspace,
            env=environment,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(integrated.returncode, 0, integrated.stderr)
        plugin = data_home / "driftctl/codex-marketplace/plugins/driftctl-codex"
        self.assertTrue((plugin / "hooks/hooks.json").is_file())
        self.assertTrue((plugin / "skills/driftctl/SKILL.md").is_file())

        ordinary = self.run_hook(
            binary,
            workspace,
            environment,
            {
                "session_id": "thread-control",
                "transcript_path": None,
                "cwd": str(workspace),
                "hook_event_name": "UserPromptSubmit",
                "model": "gpt-5.6-luna",
                "permission_mode": "default",
                "turn_id": "turn-ordinary",
                "prompt": "Continue ordinary work.",
            },
        )
        self.assertEqual(ordinary.returncode, 0, ordinary.stderr)
        self.assertEqual(ordinary.stdout, "")
        self.assertFalse((state_home / "driftctl").exists())

        activated = self.run_hook(
            binary,
            workspace,
            environment,
            {
                "session_id": session_id,
                "transcript_path": None,
                "cwd": str(workspace),
                "hook_event_name": "UserPromptSubmit",
                "model": "gpt-5.6-luna",
                "permission_mode": "default",
                "turn_id": "turn-on",
                "prompt": "$driftctl on",
            },
        )
        self.assertEqual(activated.returncode, 0, activated.stderr)
        activation = json.loads(activated.stdout)
        context = activation["hookSpecificOutput"]["additionalContext"]
        self.assertIn("Preserve packaged plugin behavior.", context)
        self.assertNotIn(session_id, activated.stdout)

        status = self.run_hook(
            binary,
            workspace,
            environment,
            {
                "session_id": session_id,
                "transcript_path": None,
                "cwd": str(workspace),
                "hook_event_name": "UserPromptSubmit",
                "model": "gpt-5.6-luna",
                "permission_mode": "default",
                "turn_id": "turn-status",
                "prompt": "$driftctl status",
            },
        )
        self.assertIn("State: on", status.stdout)

        disabled = self.run_hook(
            binary,
            workspace,
            environment,
            {
                "session_id": session_id,
                "transcript_path": None,
                "cwd": str(workspace),
                "hook_event_name": "UserPromptSubmit",
                "model": "gpt-5.6-luna",
                "permission_mode": "default",
                "turn_id": "turn-off",
                "prompt": "$driftctl off",
            },
        )
        self.assertIn("State: off", disabled.stdout)

        resumed = self.run_hook(
            binary,
            workspace,
            environment,
            {
                "session_id": session_id,
                "transcript_path": None,
                "cwd": str(workspace),
                "hook_event_name": "SessionStart",
                "model": "gpt-5.6-luna",
                "source": "resume",
            },
        )
        self.assertEqual(resumed.returncode, 0, resumed.stderr)
        self.assertEqual(resumed.stdout, "")

    @staticmethod
    def run_hook(
        binary: Path,
        workspace: Path,
        environment: dict[str, str],
        event: dict[str, object],
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(binary), "hook", "codex"],
            cwd=workspace,
            env=environment,
            input=json.dumps(event, separators=(",", ":")),
            capture_output=True,
            text=True,
            check=False,
        )

    @staticmethod
    def write_fake_codex(root: Path) -> Path:
        root.mkdir(parents=True)
        program = root / "codex"
        program.write_text(
            """#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

args = sys.argv[1:]
state = Path(os.environ["DRIFTCTL_FAKE_PLUGIN_STATE"])
state.mkdir(parents=True, exist_ok=True)

if args[:2] == ["features", "list"]:
    print("hooks stable true")
    raise SystemExit(0)
if args[:3] == ["plugin", "marketplace", "add"]:
    (state / "marketplace").write_text(args[3], encoding="utf-8")
    print("{}")
    raise SystemExit(0)
if args[:3] == ["plugin", "marketplace", "list"]:
    marketplace = state / "marketplace"
    entries = [] if not marketplace.exists() else [{
        "name": "driftctl-local",
        "root": marketplace.read_text(encoding="utf-8"),
    }]
    print(json.dumps({"marketplaces": entries}, separators=(",", ":")))
    raise SystemExit(0)
if args[:2] == ["plugin", "add"]:
    (state / "installed").write_text("yes", encoding="utf-8")
    print("{}")
    raise SystemExit(0)
if args[:2] == ["plugin", "list"]:
    installed = (state / "installed").exists()
    plugins = [] if not installed else [{
        "pluginId": "driftctl-codex@driftctl-local",
        "installed": True,
        "enabled": True,
    }]
    print(json.dumps({"installed": plugins}, separators=(",", ":")))
    raise SystemExit(0)
if args and args[0] == "exec":
    prompt = sys.stdin.read()
    with open(os.environ["DRIFTCTL_FAKE_PROMPTS"], "a", encoding="utf-8") as output:
        output.write(prompt + "\\n")
    proposal = os.environ["DRIFTCTL_FAKE_PROPOSAL"]
    destination = Path(args[args.index("--output-last-message") + 1])
    destination.write_text(proposal, encoding="utf-8")
    print(json.dumps({"type":"thread.started","thread_id":"private-compactor"}))
    print(json.dumps({"type":"item.completed","item":{"id":"item","type":"agent_message","text":proposal}}))
    print(json.dumps({"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_output_tokens":1}}))
    raise SystemExit(0)
if args[:2] != ["app-server", "--stdio"]:
    raise SystemExit(1)

for raw in sys.stdin:
    request = json.loads(raw)
    method = request.get("method")
    if method == "initialized":
        continue
    if method == "initialize":
        result = {"userAgent":"fake","codexHome":"/unused","platformFamily":"unix","platformOs":"linux"}
    elif method == "thread/read":
        result = json.loads(os.environ["DRIFTCTL_FAKE_READ"])
        if request["params"]["threadId"] != result["thread"]["id"]:
            print(json.dumps({"id":request["id"],"error":{"code":-32000,"message":"thread not found"}}), flush=True)
            continue
    elif method == "thread/goal/get":
        result = {"goal": None}
    else:
        print(json.dumps({"id":request["id"],"error":{"code":-32601,"message":"unexpected method"}}), flush=True)
        continue
    print(json.dumps({"id":request["id"],"result":result}, separators=(",", ":")), flush=True)
""",
            encoding="utf-8",
        )
        program.chmod(0o755)
        return program

    def test_rejects_a_corrupted_archive_before_install(self) -> None:
        archive = next(self.release.glob("*.tar.gz"))
        corrupted = self.root / "corrupted" / self.version
        corrupted.mkdir(parents=True)
        shutil.copy2(archive, corrupted / archive.name)
        checksum = self.release / f"{archive.name}.sha256"
        shutil.copy2(checksum, corrupted / checksum.name)
        with (corrupted / archive.name).open("ab") as output:
            output.write(b"corruption")
        binary_directory = self.root / "rejected" / "bin"

        completed = self.install(
            binary_directory,
            base_url=(self.root / "corrupted").as_uri(),
        )

        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("checksum", completed.stderr.lower())
        self.assertFalse((binary_directory / "driftctl").exists())

    def test_rejects_an_unshipped_target_before_network_access(self) -> None:
        binary_directory = self.root / "unsupported" / "bin"
        completed = subprocess.run(
            [
                "sh",
                str(INSTALLER),
                "--version",
                self.version,
                "--target",
                "aarch64-apple-darwin",
                "--bin-dir",
                str(binary_directory),
            ],
            cwd=ROOT,
            env=os.environ
            | {
                "DRIFTCTL_BASE_URL": "file:///path-that-must-not-be-read",
                "TMPDIR": str(self.root),
            },
            capture_output=True,
            text=True,
            check=False,
        )

        self.assertEqual(completed.returncode, 1)
        self.assertIn("unsupported release target", completed.stderr)
        self.assertNotIn("curl", completed.stderr.lower())
        self.assertFalse((binary_directory / "driftctl").exists())

    def test_packager_rejects_an_unshipped_target_before_build(self) -> None:
        completed = subprocess.run(
            [
                "sh",
                str(PACKAGE),
                "--out",
                str(self.root / "unsupported-package"),
                "--target",
                "aarch64-apple-darwin",
            ],
            cwd=ROOT,
            env=os.environ | {"TMPDIR": str(self.root)},
            capture_output=True,
            text=True,
            check=False,
        )

        self.assertEqual(completed.returncode, 1)
        self.assertIn("unsupported release target", completed.stderr)
        self.assertNotIn("Compiling", completed.stderr)

    def install(
        self, binary_directory: Path, base_url: str | None = None
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                "sh",
                str(INSTALLER),
                "--version",
                self.version,
                "--target",
                self.target,
                "--bin-dir",
                str(binary_directory),
            ],
            cwd=ROOT,
            env=os.environ
            | {
                "DRIFTCTL_BASE_URL": base_url
                or (self.root / "releases").as_uri(),
                "TMPDIR": str(self.root),
            },
            capture_output=True,
            text=True,
            check=False,
        )


if __name__ == "__main__":
    unittest.main()
