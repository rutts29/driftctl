#!/usr/bin/env python3
"""Run the five frozen native evaluation cases exactly once each."""

from __future__ import annotations

import argparse
from collections.abc import Mapping, Sequence
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
from typing import Any


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
if str(REPOSITORY_ROOT) not in sys.path:
    sys.path.insert(0, str(REPOSITORY_ROOT))

from evals.calibration.calibrate import (  # noqa: E402
    CalibrationError,
    check_manifest,
    load_manifest,
)


DEFAULT_MANIFEST = REPOSITORY_ROOT / "evals" / "calibration" / "manifest.json"
DEFAULT_NATIVE_RUNNER = Path(__file__).resolve().with_name(
    "run_native_long_session.py"
)
DEFAULT_RESULTS_DIR = Path("evals/results/native-suite")
FLAGSHIP_CASE_ID = "02-steering-pagination"
STATUS_FILENAME = "native-suite-status.json"
REPRODUCTION_COMMAND = (
    "PYTHONDONTWRITEBYTECODE=1 python3 evals/runner/run_native_suite.py "
    "--manifest evals/calibration/manifest.json "
    "--results-dir evals/results/native-suite "
    "--driftctl-bin driftctl --codex-bin codex"
)
POSIX_PATH = re.compile(r"(?<![A-Za-z0-9_:/])/(?:[^\s\"'`<>{}\[\],;)]*)")
WINDOWS_PATH = re.compile(
    r"(?<![A-Za-z0-9_])(?:[A-Za-z]:[\\/]|\\\\)(?:[^\s\"'`<>{}\[\],;)]*)"
)


class SuiteError(RuntimeError):
    """Report an invalid suite input or unavailable local runner."""


def main(arguments: Sequence[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--results-dir", type=Path, default=DEFAULT_RESULTS_DIR)
    parser.add_argument(
        "--native-runner",
        "--runner",
        dest="native_runner",
        type=Path,
        default=DEFAULT_NATIVE_RUNNER,
    )
    parser.add_argument("--root", type=Path, default=REPOSITORY_ROOT)
    parser.add_argument("--driftctl-bin", default="driftctl")
    parser.add_argument("--codex-bin", default="codex")
    parser.add_argument("--artifacts", type=Path)
    namespace = parser.parse_args(arguments)

    try:
        status = run_suite(
            namespace.manifest,
            namespace.results_dir,
            root=namespace.root,
            native_runner=namespace.native_runner,
            driftctl_bin=namespace.driftctl_bin,
            codex_bin=namespace.codex_bin,
            artifacts=namespace.artifacts,
        )
    except (OSError, SuiteError) as error:
        status = {
            "schema_version": 1,
            "evaluation_kind": "native_suite",
            "status": "invalid",
            "error": sanitize_text(str(error), ()),
            "cases": [],
            "result_files": {},
            "reproduction_command": REPRODUCTION_COMMAND,
        }
    print(json.dumps(status, ensure_ascii=False, sort_keys=True, separators=(",", ":")))
    return (
        0
        if status.get("status") in {"completed", "completed_with_safety_blocks"}
        else 1
    )


def run_suite(
    manifest_path: Path = DEFAULT_MANIFEST,
    results_dir: Path = DEFAULT_RESULTS_DIR,
    *,
    root: Path = REPOSITORY_ROOT,
    native_runner: Path = DEFAULT_NATIVE_RUNNER,
    driftctl_bin: str = "driftctl",
    codex_bin: str = "codex",
    artifacts: Path | None = None,
) -> dict[str, Any]:
    """Run and retain a sanitized status for every frozen case."""

    root = Path(root).resolve()
    manifest_path = Path(manifest_path).resolve()
    results_dir = Path(results_dir).resolve()
    native_runner = Path(native_runner).resolve()
    artifacts = Path(artifacts).resolve() if artifacts is not None else None
    results_dir.mkdir(parents=True, exist_ok=True)
    public_paths = [root, manifest_path, results_dir, native_runner]
    if artifacts is not None:
        public_paths.append(artifacts)

    try:
        manifest = load_manifest(manifest_path)
        validation = check_manifest(root, manifest_path)
    except (CalibrationError, OSError, ValueError) as error:
        status = invalid_status(
            sanitize_text(str(error), public_paths),
            preflight={"status": "invalid", "error": sanitize_text(str(error), public_paths)},
        )
        write_status(results_dir, status)
        return status

    try:
        cases = manifest["cases"]
        if not isinstance(cases, list) or not all(isinstance(case, Mapping) for case in cases):
            raise SuiteError("calibration manifest cases are malformed")
        case_ids = [case.get("id") for case in cases]
        if not all(isinstance(case_id, str) and case_id for case_id in case_ids):
            raise SuiteError("calibration manifest case IDs are malformed")
    except (KeyError, TypeError, SuiteError) as error:
        status = invalid_status(
            sanitize_text(str(error), public_paths),
            preflight={
                "status": "invalid",
                "error": sanitize_text(str(error), public_paths),
            },
        )
        write_status(results_dir, status)
        return status

    case_statuses: list[dict[str, Any]] = []
    result_files: dict[str, dict[str, str]] = {}
    for case in cases:
        case_id = str(case["id"])
        case_directory = root / "evals" / "cases" / case_id
        record = run_case(
            case,
            case_directory,
            results_dir,
            native_runner,
            driftctl_bin,
            codex_bin,
            artifacts,
            public_paths + [case_directory],
        )
        case_statuses.append(record)
        if record["result_files"]:
            result_files[case_id] = dict(record["result_files"])

    invalid_count = sum(
        record["status"] == "infrastructure-invalid" for record in case_statuses
    )
    safety_blocked_count = sum(
        record["status"] == "safety-blocked" for record in case_statuses
    )
    suite_status = (
        "invalid"
        if invalid_count
        else "completed_with_safety_blocks"
        if safety_blocked_count
        else "completed"
    )
    status = {
        "schema_version": 1,
        "evaluation_kind": "native_suite",
        "status": suite_status,
        "case_count": len(case_statuses),
        "invalid_case_count": invalid_count,
        "safety_blocked_case_count": safety_blocked_count,
        "manifest_id": manifest.get("manifest_id"),
        "suite_fingerprint_sha256": manifest.get("suite_fingerprint_sha256"),
        "preflight": {
            "status": "passed",
            "manifest_fingerprint_sha256": validation.get(
                "manifest_fingerprint_sha256"
            ),
        },
        "cases": case_statuses,
        "result_files": result_files,
        "result_filenames": [
            filename
            for record in case_statuses
            for filename in record["result_files"].values()
        ],
        "status_file": STATUS_FILENAME,
        "reproduction_command": REPRODUCTION_COMMAND,
    }
    write_status(results_dir, status)
    return status


def run_case(
    case: Mapping[str, Any],
    case_directory: Path,
    results_dir: Path,
    native_runner: Path,
    driftctl_bin: str,
    codex_bin: str,
    artifacts: Path | None,
    public_paths: Sequence[Path],
) -> dict[str, Any]:
    case_id = str(case["id"])
    command = native_command(
        case,
        case_directory,
        results_dir,
        native_runner,
        driftctl_bin,
        codex_bin,
        artifacts,
    )
    try:
        completed = subprocess.run(
            command,
            cwd=case_directory,
            capture_output=True,
            text=True,
            check=False,
        )
        exit_code = completed.returncode
        stdout = completed.stdout
        stderr = completed.stderr
    except OSError as error:
        exit_code = -1
        stdout = ""
        stderr = str(error)

    result: dict[str, Any] = {
        "case_id": case_id,
        "exit_code": exit_code,
        "stdout": sanitize_text(stdout, public_paths),
        "stderr": sanitize_text(stderr, public_paths),
        "result_files": {},
        "status": "infrastructure-invalid",
    }
    parsed: Mapping[str, Any] | None = None
    error: str | None = None
    if stdout:
        try:
            value = json.loads(stdout)
            if not isinstance(value, Mapping):
                raise ValueError("native runner emitted a non-object result")
            parsed = value
        except (json.JSONDecodeError, ValueError) as exc:
            error = f"native runner output is invalid JSON: {exc}"
    else:
        error = "native runner emitted no stdout"

    if parsed is not None:
        reported_case_id = parsed.get("case_id")
        if reported_case_id != case_id:
            error = "native runner result has an unexpected case ID"
        runner_status = parsed.get("status")
        result["runner_status"] = runner_status
        if runner_status == "completed":
            try:
                files = normalize_result_files(parsed.get("result_files"))
            except SuiteError as exc:
                error = str(exc)
            else:
                result["result_files"] = files
        elif runner_status == "safety_blocked":
            if parsed.get("result_files") != {} or not isinstance(
                parsed.get("safety_block"), Mapping
            ):
                error = "native safety block is malformed"
        else:
            error = "native runner did not report a recognized terminal status"
    if exit_code != 0:
        error = f"native runner exited with status {exit_code}"
    if error is None:
        result["status"] = (
            "safety-blocked"
            if parsed is not None and parsed.get("status") == "safety_blocked"
            else "completed"
        )
    else:
        result["error"] = sanitize_text(error, public_paths)
    return result


def native_command(
    case: Mapping[str, Any],
    case_directory: Path,
    results_dir: Path,
    native_runner: Path,
    driftctl_bin: str,
    codex_bin: str,
    artifacts: Path | None,
) -> list[str]:
    policy = case.get("runner_model_policy")
    if not isinstance(policy, Mapping):
        raise SuiteError(f"case {case.get('id')!r} has no runner model policy")
    context_bytes = policy.get("context_bytes")
    model = policy.get("model")
    effort = policy.get("effort")
    if (
        isinstance(context_bytes, bool)
        or not isinstance(context_bytes, int)
        or context_bytes < 0
        or not isinstance(model, str)
        or not model.strip()
        or not isinstance(effort, str)
        or not effort.strip()
    ):
        raise SuiteError(f"case {case.get('id')!r} has malformed runner model policy")

    command = [
        sys.executable,
        str(native_runner),
        "--case",
        str(case_directory),
        "--results-dir",
        str(results_dir),
        "--driftctl-bin",
        driftctl_bin,
        "--codex-bin",
        codex_bin,
        "--context-bytes",
        str(context_bytes),
        "--worker-model",
        model,
        "--worker-effort",
        effort,
    ]
    if artifacts is not None:
        command.extend(("--artifacts", str(artifacts)))
    if case.get("id") == FLAGSHIP_CASE_ID:
        artifact_values = case.get("artifacts")
        summary_relative = (
            artifact_values.get("plain_summary")
            if isinstance(artifact_values, Mapping)
            else None
        )
        if not isinstance(summary_relative, str) or not safe_relative(summary_relative):
            raise SuiteError("flagship case has an invalid plain summary path")
        command.extend(
            ("--plain-summary-file", str(case_directory / PurePosixPath(summary_relative)))
        )
    return command


def normalize_result_files(value: Any) -> dict[str, str]:
    if not isinstance(value, Mapping) or not value:
        raise SuiteError("native runner result_files must be a nonempty object")
    files: dict[str, str] = {}
    for mode, filename in value.items():
        if (
            not isinstance(mode, str)
            or not mode
            or not isinstance(filename, str)
            or not filename
            or not safe_filename(filename)
        ):
            raise SuiteError("native runner result_files must contain filenames only")
        files[mode] = filename
    return files


def safe_relative(value: str) -> bool:
    path = PurePosixPath(value)
    return bool(path.parts) and not path.is_absolute() and all(
        part not in {"", ".", ".."} for part in path.parts
    )


def safe_filename(value: str) -> bool:
    return safe_relative(value) and len(PurePosixPath(value).parts) == 1


def invalid_status(error: str, *, preflight: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "evaluation_kind": "native_suite",
        "status": "invalid",
        "case_count": 0,
        "invalid_case_count": 0,
        "preflight": dict(preflight),
        "cases": [],
        "result_files": {},
        "result_filenames": [],
        "status_file": STATUS_FILENAME,
        "error": error,
        "reproduction_command": REPRODUCTION_COMMAND,
    }


def write_status(results_dir: Path, status: Mapping[str, Any]) -> None:
    path = results_dir / STATUS_FILENAME
    path.write_text(
        json.dumps(status, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
        + "\n",
        encoding="utf-8",
    )


def sanitize_text(value: str, paths: Sequence[Path]) -> str:
    sanitized = value
    for path in sorted({str(item) for item in paths if str(item)}, key=len, reverse=True):
        sanitized = sanitized.replace(path, "<local-path>")
    sanitized = POSIX_PATH.sub("<local-path>", sanitized)
    return WINDOWS_PATH.sub("<local-path>", sanitized)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
