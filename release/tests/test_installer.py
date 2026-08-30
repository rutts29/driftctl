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
