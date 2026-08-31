#!/usr/bin/env python3
"""Functional verifier for the Click 8.2.1 stdin regression."""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
from pathlib import Path


candidate = Path.cwd().resolve()
source = candidate / "src"

if not (source / "click").is_dir():
    raise SystemExit(f"expected a Click candidate checkout, got {candidate}")

sys.path.insert(0, str(source))

import click  # noqa: E402
from click.testing import CliRunner  # noqa: E402


expected_module = (source / "click" / "__init__.py").resolve()
if Path(click.__file__).resolve() != expected_module:
    raise SystemExit(f"did not import the candidate source: {click.__file__}")


@click.command()
@click.argument("input_file", type=click.File("r"))
def cli(input_file: object) -> None:
    for line in input_file.readlines():
        print(line.rstrip())


result = CliRunner().invoke(cli, ["-"], input="DRIFTCTL_STDIN_FIXTURE\n")

if result.exit_code != 0:
    raise SystemExit(
        "stdin File argument still fails "
        f"(exit_code={result.exit_code}, exception={result.exception!r}, "
        f"output={result.output!r})"
    )

if result.output != "DRIFTCTL_STDIN_FIXTURE\n":
    raise SystemExit(f"unexpected output: {result.output!r}")

changed_files = subprocess.run(
    ["git", "diff", "--name-only", "HEAD"],
    cwd=candidate,
    check=True,
    capture_output=True,
    text=True,
).stdout.splitlines()
allowed_files = {
    "CHANGES.rst",
    "src/click/testing.py",
    "tests/test_chain.py",
    "tests/test_testing.py",
}

if not changed_files:
    raise SystemExit("candidate passed without a source change")

unexpected_files = sorted(set(changed_files) - allowed_files)
if unexpected_files:
    raise SystemExit(f"unexpected changed files: {', '.join(unexpected_files)}")

environment = os.environ | {
    "PYTHONDONTWRITEBYTECODE": "1",
    "PYTHONPATH": str(source),
}

with tempfile.TemporaryDirectory(prefix="driftctl-click-") as temp_dir:
    test_result = subprocess.run(
        [
            sys.executable,
            "-m",
            "pytest",
            "-q",
            "tests/test_testing.py",
            f"--basetemp={Path(temp_dir) / 'pytest'}",
            "-p",
            "no:cacheprovider",
        ],
        cwd=candidate,
        env=environment,
        text=True,
        capture_output=True,
    )

if test_result.returncode:
    raise SystemExit(
        "focused upstream test failed:\n"
        f"stdout:\n{test_result.stdout}\nstderr:\n{test_result.stderr}"
    )

print("PASS: stdin File argument accepts CliRunner input")
