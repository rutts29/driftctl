#!/usr/bin/env python3
"""Freeze and deterministically calibrate the five T14 evaluation cases."""

from __future__ import annotations

import argparse
from collections.abc import Iterable, Mapping, Sequence
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import sys
import tempfile
from typing import Any


class CalibrationError(RuntimeError):
    """Report an invalid frozen fixture or a calibration failure."""


CASE_IDS = (
    "01-steering-retry",
    "02-steering-pagination",
    "03-steering-atomic-import",
    "04-scope-csv-export",
    "05-rename-resume",
)

NEUTRAL_CONTINUATION_PROMPT = (
    "Continue the task from this checkpoint. Preserve existing behavior and complete "
    "the remaining work. Do not claim completion without running relevant validation."
)

EVALUATION_POLICY = {
    "approval_policy": "never",
    "context_bytes": 32768,
    "effort": "max",
    "model": "gpt-5.6-luna",
    "provider": "Codex",
    "sandbox": "workspace-write",
}

CALIBRATION_POLICY = {
    "codex_calls": False,
    "model": "none",
    "network": False,
    "paid_calls": False,
    "provider": "none",
    "repetitions": 3,
    "runner": "deterministic_calibration",
    "sandbox": "temporary-local-copy",
}

EVALUATION_INPUT_EXCLUDES = frozenset({"__pycache__"})


def main(arguments: Sequence[str]) -> int:
    """Run manifest generation or a complete deterministic calibration."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "action",
        nargs="?",
        choices=("check", "generate", "run"),
        default="check",
        help="generate the frozen manifest or check and calibrate it",
    )
    parser.add_argument("--root", type=Path, help="repository root")
    parser.add_argument("--manifest", type=Path, help="frozen manifest path")
    parser.add_argument(
        "--repetitions",
        type=positive_integer,
        default=3,
        help="reference repetitions (the frozen policy requires three)",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="compatibility alias for the default check action",
    )
    parser.add_argument(
        "--generate",
        action="store_true",
        help="compatibility alias for the generate action",
    )
    namespace = parser.parse_args(arguments)
    if namespace.check and namespace.generate:
        parser.error("--check and --generate are mutually exclusive")
    if namespace.check:
        namespace.action = "check"
    elif namespace.generate:
        namespace.action = "generate"

    root = (namespace.root or repository_root()).resolve()
    manifest = (namespace.manifest or root / "evals" / "calibration" / "manifest.json").resolve()
    try:
        if namespace.action == "generate":
            generated = generate_manifest(root, manifest)
            result = {
                "manifest": str(generated.relative_to(root)),
                "status": "generated",
            }
        else:
            result = run_calibration(
                root,
                manifest,
                repetitions=namespace.repetitions,
            )
    except (CalibrationError, OSError, ValueError) as error:
        result = {"error": str(error), "status": "calibration_error"}
        print(json.dumps(result, sort_keys=True, separators=(",", ":")))
        return 1

    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0


def repository_root() -> Path:
    """Return the repository root relative to this module."""

    return Path(__file__).resolve().parents[2]


def positive_integer(value: str) -> int:
    """Parse a positive integer CLI option."""

    try:
        parsed = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be a positive integer") from error
    if parsed < 1:
        raise argparse.ArgumentTypeError("must be a positive integer")
    return parsed


def load_manifest(path: Path) -> dict[str, Any]:
    """Read one JSON manifest and require an object root."""

    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        raise CalibrationError(f"could not read manifest {path}: {error}") from error
    except json.JSONDecodeError as error:
        raise CalibrationError(f"invalid manifest JSON: {error}") from error
    if not isinstance(value, dict):
        raise CalibrationError("manifest must be a JSON object")
    return value


def generate_manifest(root: Path, manifest_path: Path) -> Path:
    """Build a manifest from checked-in cases and calibration artifacts."""

    root = root.resolve()
    manifest_path = manifest_path.resolve()
    case_root = root / "evals" / "cases"
    cases = []
    for case_id in CASE_IDS:
        case_directory = case_root / case_id
        write_source_bundle(case_directory)
        case = build_case_manifest(root, case_directory)
        cases.append(case)
    result: dict[str, Any] = {
        "cases": cases,
        "manifest_id": "t14-case-calibration",
        "schema_version": 1,
    }
    result["suite_fingerprint_sha256"] = suite_fingerprint(cases)
    manifest_path.parent.mkdir(parents=True, exist_ok=True)
    manifest_path.write_text(
        json.dumps(result, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    return manifest_path


def build_case_manifest(root: Path, case_directory: Path) -> dict[str, Any]:
    """Construct one case entry and its immutable input fingerprints."""

    case_directory = case_directory.resolve()
    case_path = case_directory / "case.json"
    raw_case = read_object(case_path, "case contract")
    case_id = required_string(raw_case, "id", case_path)
    if case_id not in CASE_IDS:
        raise CalibrationError(f"unexpected T14 case id: {case_id}")
    workspace_path = required_relative(raw_case, "workspace", case_path)
    allowed_paths = required_string_list(raw_case, "allowed_changed_paths", case_path)
    verifier_values = raw_case.get("verifiers")
    if not isinstance(verifier_values, list) or not verifier_values:
        raise CalibrationError(f"case {case_id} has no verifiers")
    verifiers = []
    for index, value in enumerate(verifier_values, start=1):
        if not isinstance(value, Mapping):
            raise CalibrationError(f"case {case_id} verifier {index} is not an object")
        verifiers.append(
            {
                "command": required_string(value, "command", case_path),
                "name": required_string(value, "name", case_path),
            }
        )

    workspace = case_directory / workspace_path
    grader = case_directory / "steering"
    workspace_files = relative_files(workspace)
    workspace_sha256 = fingerprint_tree(workspace, workspace_files)
    grader_files = relative_files(grader)
    gold_path = case_directory / "calibration" / "gold" / "active_projection.json"
    gold_projection = read_object(gold_path, "gold projection")
    validate_gold_projection(gold_projection)
    expected_summary = plain_summary_for_projection(gold_projection)
    plain_summary_path = case_directory / "calibration" / "gold" / "plain_summary.txt"
    plain_summary = read_text(plain_summary_path, "plain summary")
    if plain_summary != expected_summary + "\n":
        raise CalibrationError(
            f"case {case_id} plain summary does not match the approved gold facts"
        )
    source_bundle_path = case_directory / "calibration" / "gold" / "source_bundle.json"
    source_bundle = read_object(source_bundle_path, "neutral source bundle")
    expected_source_bundle = source_bundle_for_case(raw_case, workspace_sha256)
    if source_bundle != expected_source_bundle:
        raise CalibrationError(
            f"case {case_id} neutral source bundle does not match its frozen contract"
        )
    canonical_facts = canonical_facts_for(gold_projection)
    reference_path = case_directory / "calibration" / "reference"
    known_bad_path = case_directory / "calibration" / "known-bad"
    reference_files = relative_files(reference_path)
    known_bad_files = relative_files(known_bad_path)
    ensure_artifact_paths(case_id, reference_files, allowed_paths, reference_path)
    ensure_artifact_paths(case_id, known_bad_files, allowed_paths, known_bad_path)

    fingerprints = {
        "case_contract_sha256": fingerprint_paths(case_directory, ("case.json",)),
        "external_grader_sha256": fingerprint_tree(grader, grader_files),
        "gold_projection_sha256": fingerprint_paths(
            case_directory / "calibration" / "gold", ("active_projection.json",)
        ),
        "known_bad_artifact_sha256": fingerprint_tree(known_bad_path, known_bad_files),
        "neutral_continuation_prompt_sha256": digest_text(NEUTRAL_CONTINUATION_PROMPT),
        "plain_summary_artifact_sha256": fingerprint_paths(
            plain_summary_path.parent, (plain_summary_path.name,)
        ),
        "plain_summary_sha256": digest_text(plain_summary),
        "reference_artifact_sha256": fingerprint_tree(reference_path, reference_files),
        "source_bundle_sha256": fingerprint_paths(
            source_bundle_path.parent, (source_bundle_path.name,)
        ),
        "workspace_sha256": workspace_sha256,
    }
    case = {
        "allowed_paths": allowed_paths,
        "artifacts": {
            "gold_projection": "calibration/gold/active_projection.json",
            "known_bad": "calibration/known-bad",
            "plain_summary": "calibration/gold/plain_summary.txt",
            "reference": "calibration/reference",
            "source_bundle": "calibration/gold/source_bundle.json",
        },
        "case_contract": {
            "fields": {
                "goal": required_string(raw_case, "goal", case_path),
                "id": case_id,
                "initial_requirements": required_string_list(
                    raw_case, "initial_requirements", case_path
                ),
                "title": required_string(raw_case, "title", case_path),
            },
            "path": "case.json",
            "sha256": fingerprints["case_contract_sha256"],
        },
        "external_grader": {
            "files": grader_files,
            "hidden_verifiers": ["integration"],
            "path": "steering",
            "sha256": fingerprints["external_grader_sha256"],
            "verifiers": verifiers,
            "visible_verifiers": ["unit"],
        },
        "fingerprints": fingerprints,
        "gold_projection": gold_projection,
        "id": case_id,
        "information_match": {
            "active_fact_sha256": digest_facts(canonical_facts),
            "canonical_facts": canonical_facts,
            "fact_count": len(canonical_facts),
            "method": "ordered-active-fact-sha256",
            "status": "exact",
        },
        "neutral_continuation_prompt": NEUTRAL_CONTINUATION_PROMPT,
        "neutral_continuation_prompt_sha256": digest_text(NEUTRAL_CONTINUATION_PROMPT),
        "neutral_continuation_prompt_utf8_bytes": utf8_bytes(NEUTRAL_CONTINUATION_PROMPT),
        "plain_summary": plain_summary,
        "plain_summary_sha256": digest_text(plain_summary),
        "plain_summary_utf8_bytes": utf8_bytes(plain_summary),
        "runner_model_policy": {
            **EVALUATION_POLICY,
            "calibration": dict(CALIBRATION_POLICY),
        },
        "workspace": {
            "files": workspace_files,
            "path": workspace_path,
            "sha256": fingerprints["workspace_sha256"],
        },
    }
    return case


def write_source_bundle(case_directory: Path) -> Path:
    """Write the deterministic synthetic native-session input for one case."""

    case_path = case_directory / "case.json"
    raw_case = read_object(case_path, "case contract")
    workspace_path = required_relative(raw_case, "workspace", case_path)
    workspace = case_directory / workspace_path
    workspace_sha256 = fingerprint_tree(workspace, relative_files(workspace))
    bundle = source_bundle_for_case(raw_case, workspace_sha256)
    output = case_directory / "calibration" / "gold" / "source_bundle.json"
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        json.dumps(bundle, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    return output


def source_bundle_for_case(
    raw_case: Mapping[str, Any], workspace_sha256: str
) -> dict[str, Any]:
    """Derive a schema-v1 neutral bundle from a frozen synthetic case contract."""

    case_id = required_manifest_string(raw_case, "id")
    goal = required_manifest_string(raw_case, "goal")
    initial_requirements = required_manifest_string_list(
        raw_case, "initial_requirements"
    )
    steering = raw_case.get("steering")
    if not isinstance(steering, list):
        raise CalibrationError(f"case {case_id} steering must be an array")
    initial_requirements_text = "\n".join(
        f"- {requirement}" for requirement in initial_requirements
    )
    records = [
        source_record(
            "initial",
            f"Goal:\n{goal}\n\nKnown requirements:\n{initial_requirements_text}",
        )
    ]
    for index, item in enumerate(steering, start=1):
        if not isinstance(item, Mapping):
            raise CalibrationError(f"case {case_id} steering {index} is not an object")
        requirement = required_manifest_string(item, "requirement")
        records.append(
            source_record(
                f"steering-{index}",
                f"Late steering {index}:\n- {requirement}",
            )
        )
    source_hasher = hashlib.sha256()
    for record in records:
        source_hasher.update(record["id"].encode("utf-8"))
        source_hasher.update(b"\0")
        source_hasher.update(record["content_digest"].encode("utf-8"))
        source_hasher.update(b"\0")
    return {
        "schema_version": 1,
        "source": {
            "provider": "codex",
            "session_ref": f"synthetic-{case_id}",
            "repository_digest": f"sha256:{workspace_sha256}",
            "head": records[-1]["id"],
            "digest": f"sha256:{source_hasher.hexdigest()}",
        },
        "native_goal": {"state": "absent"},
        "records": records,
    }


def source_record(record_id: str, content: str) -> dict[str, str]:
    """Build one normalized user record with the shipped Rust digest algorithm."""

    content = content.replace("\r\n", "\n").replace("\r", "\n")
    hasher = hashlib.sha256()
    hasher.update(record_id.encode("utf-8"))
    hasher.update(b"\0user\0")
    hasher.update(content.encode("utf-8"))
    return {
        "id": record_id,
        "role": "user",
        "content": content,
        "content_digest": f"sha256:{hasher.hexdigest()}",
    }


def check_manifest(root: Path, manifest_path: Path) -> dict[str, Any]:
    """Validate every frozen input and return a compact drift report."""

    root = root.resolve()
    manifest = load_manifest(manifest_path)
    validate_manifest_shape(manifest)
    drift: list[str] = []
    cases = manifest["cases"]
    for case in cases:
        try:
            validate_case_fingerprints(root, case)
        except CalibrationError as error:
            drift.append(str(error))
    expected_suite = suite_fingerprint(cases)
    if manifest.get("suite_fingerprint_sha256") != expected_suite:
        drift.append("manifest suite fingerprint differs from its case entries")
    if drift:
        raise CalibrationError("manifest drift detected: " + "; ".join(drift))
    return {
        "case_count": len(cases),
        "drift": [],
        "manifest_fingerprint_sha256": digest_bytes(
            manifest_path.read_bytes()
        ),
        "passed": True,
    }


def run_calibration(
    root: Path,
    manifest_path: Path,
    *,
    repetitions: int = 3,
) -> dict[str, Any]:
    """Run reference and known-bad artifacts against every frozen case."""

    if repetitions != CALIBRATION_POLICY["repetitions"]:
        raise CalibrationError("the frozen calibration policy requires three repetitions")
    manifest_check = check_manifest(root, manifest_path)
    manifest = load_manifest(manifest_path)
    outcomes = []
    with tempfile.TemporaryDirectory(prefix="driftctl-calibration-") as temporary:
        temporary_root = Path(temporary)
        for case in manifest["cases"]:
            outcomes.append(
                calibrate_case(root.resolve(), temporary_root, case, repetitions)
            )
    passed = all(
        case["reference"]["passed"] and case["known_bad"]["passed"]
        for case in outcomes
    )
    return {
        "cases": outcomes,
        "manifest": {
            "case_count": manifest_check["case_count"],
            "fingerprint_sha256": manifest_check["manifest_fingerprint_sha256"],
        },
        "status": "verified" if passed else "failed",
    }


def calibrate_case(
    root: Path,
    temporary_root: Path,
    case: Mapping[str, Any],
    repetitions: int,
) -> dict[str, Any]:
    """Apply both artifacts for one case and evaluate the expected outcomes."""

    case_id = required_manifest_string(case, "id")
    case_directory = root / "evals" / "cases" / case_id
    workspace_relative = required_manifest_mapping(case, "workspace")["path"]
    source_workspace = safe_join(case_directory, workspace_relative, "workspace")
    artifact_values = required_manifest_mapping(case, "artifacts")
    reference_relative = required_manifest_string(artifact_values, "reference")
    known_bad_relative = required_manifest_string(artifact_values, "known_bad")
    reference_path = safe_join(case_directory, reference_relative, "reference artifact")
    known_bad_path = safe_join(case_directory, known_bad_relative, "known-bad artifact")
    verifiers = required_manifest_mapping(case, "external_grader")["verifiers"]
    reference_runs = []
    for index in range(1, repetitions + 1):
        candidate = temporary_root / f"{case_id}-reference-{index}"
        copy_workspace(source_workspace, candidate)
        apply_overlay(candidate, reference_path)
        reference_runs.append(run_verifiers(candidate, case_directory, verifiers))
    reference_passed = all(
        all(verifier["passed"] for verifier in run) for run in reference_runs
    )

    known_bad_candidate = temporary_root / f"{case_id}-known-bad"
    copy_workspace(source_workspace, known_bad_candidate)
    apply_overlay(known_bad_candidate, known_bad_path)
    known_bad_verifiers = run_verifiers(
        known_bad_candidate, case_directory, verifiers
    )
    external_grader = required_manifest_mapping(case, "external_grader")
    visible_names = required_manifest_string_list(external_grader, "visible_verifiers")
    hidden_names = required_manifest_string_list(external_grader, "hidden_verifiers")
    visible_plausible = all(
        outcome["passed"]
        for outcome in known_bad_verifiers
        if outcome["name"] in visible_names
    )
    hidden_failed = any(
        not outcome["passed"]
        for outcome in known_bad_verifiers
        if outcome["name"] in hidden_names
    )
    return {
        "case_id": case_id,
        "known_bad": {
            "hidden_failed": hidden_failed,
            "passed": visible_plausible and hidden_failed,
            "verifiers": known_bad_verifiers,
            "visible_plausible": visible_plausible,
        },
        "reference": {
            "passed": reference_passed,
            "passed_runs": sum(
                all(verifier["passed"] for verifier in run)
                for run in reference_runs
            ),
            "repetitions": repetitions,
            "runs": reference_runs,
        },
    }


def run_verifiers(
    workspace: Path,
    case_directory: Path,
    verifiers: Any,
) -> list[dict[str, Any]]:
    """Run case-owned verifier commands with no provider or network call."""

    if not isinstance(verifiers, list):
        raise CalibrationError("external_grader.verifiers must be an array")
    environment = dict(os.environ)
    environment["DRIFTCTL_EVAL_CASE_DIR"] = str(case_directory.resolve())
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    existing_python_path = environment.get("PYTHONPATH")
    environment["PYTHONPATH"] = (
        f"{workspace}{os.pathsep}{existing_python_path}"
        if existing_python_path
        else str(workspace)
    )
    outcomes = []
    for index, verifier in enumerate(verifiers, start=1):
        if not isinstance(verifier, Mapping):
            raise CalibrationError(f"verifier {index} is not an object")
        name = required_manifest_string(verifier, "name")
        command = required_manifest_string(verifier, "command")
        try:
            completed = subprocess.run(
                command,
                cwd=workspace,
                env=environment,
                shell=True,
                capture_output=True,
                text=True,
                timeout=60,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            outcomes.append(
                {"exit_code": -1, "name": name, "passed": False, "error": str(error)}
            )
            continue
        outcomes.append(
            {
                "exit_code": completed.returncode,
                "name": name,
                "passed": completed.returncode == 0,
            }
        )
    return outcomes


def copy_workspace(source: Path, destination: Path) -> None:
    """Copy one source workspace into a fresh candidate directory."""

    if destination.exists():
        raise CalibrationError(f"candidate already exists: {destination}")
    if not source.is_dir():
        raise CalibrationError(f"workspace does not exist: {source}")
    shutil.copytree(source, destination, symlinks=False)


def apply_overlay(workspace: Path, artifact_directory: Path) -> None:
    """Apply regular files from an artifact directory to a candidate."""

    if not artifact_directory.is_dir():
        raise CalibrationError(f"artifact directory does not exist: {artifact_directory}")
    for relative in relative_files(artifact_directory):
        source = safe_join(artifact_directory, relative, "artifact file")
        destination = safe_join(workspace, relative, "artifact target")
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)


def validate_manifest_shape(manifest: Mapping[str, Any]) -> None:
    """Validate the stable top-level and case identity contract."""

    if manifest.get("schema_version") != 1:
        raise CalibrationError("manifest schema_version must be 1")
    if manifest.get("manifest_id") != "t14-case-calibration":
        raise CalibrationError("manifest manifest_id is not the T14 calibration manifest")
    cases = manifest.get("cases")
    if not isinstance(cases, list) or [case.get("id") for case in cases if isinstance(case, Mapping)] != list(CASE_IDS):
        raise CalibrationError("manifest must contain the five ordered T14 cases")
    for case in cases:
        if not isinstance(case, Mapping):
            raise CalibrationError("manifest case must be an object")
        for field in (
            "allowed_paths",
            "artifacts",
            "case_contract",
            "external_grader",
            "fingerprints",
            "gold_projection",
            "id",
            "information_match",
            "neutral_continuation_prompt",
            "plain_summary",
            "runner_model_policy",
            "workspace",
        ):
            if field not in case:
                raise CalibrationError(f"manifest case {case.get('id')!r} lacks {field}")


def validate_case_fingerprints(root: Path, case: Mapping[str, Any]) -> None:
    """Compare all manifest fingerprints and text declarations to current files."""

    case_id = required_manifest_string(case, "id")
    case_directory = root / "evals" / "cases" / case_id
    workspace = required_manifest_mapping(case, "workspace")
    workspace_path = required_manifest_string(workspace, "path")
    workspace_files = required_manifest_string_list(workspace, "files")
    grader = required_manifest_mapping(case, "external_grader")
    grader_path = required_manifest_string(grader, "path")
    grader_files = required_manifest_string_list(grader, "files")
    artifacts = required_manifest_mapping(case, "artifacts")
    fingerprint_values = required_manifest_mapping(case, "fingerprints")
    case_contract = read_object(case_directory / "case.json", "case contract")
    if required_string(case_contract, "id", case_directory / "case.json") != case_id:
        raise CalibrationError(f"case {case_id} contract id drifted")
    contract_fields = required_manifest_mapping(case, "case_contract").get("fields")
    if not isinstance(contract_fields, Mapping):
        raise CalibrationError(f"case {case_id} contract fields are malformed")
    for field in ("id", "title", "goal", "initial_requirements"):
        if contract_fields.get(field) != case_contract.get(field):
            raise CalibrationError(f"case {case_id} contract field {field} drifted")
    contract_allowed = required_string_list(
        case_contract, "allowed_changed_paths", case_directory / "case.json"
    )
    if list(case.get("allowed_paths", [])) != contract_allowed:
        raise CalibrationError(f"case {case_id} allowed paths drifted")
    if case_contract.get("workspace") != workspace_path:
        raise CalibrationError(f"case {case_id} workspace path drifted")
    contract_verifiers = case_contract.get("verifiers")
    if not isinstance(contract_verifiers, list):
        raise CalibrationError(f"case {case_id} contract verifiers drifted")
    expected_verifiers = [
        {
            "command": required_string(item, "command", case_directory / "case.json"),
            "name": required_string(item, "name", case_directory / "case.json"),
        }
        for item in contract_verifiers
        if isinstance(item, Mapping)
    ]
    if expected_verifiers != list(grader.get("verifiers", [])):
        raise CalibrationError(f"case {case_id} verifier declarations drifted")
    if list(grader.get("files", [])) != relative_files(
        safe_join(case_directory, grader_path, "external grader")
    ):
        raise CalibrationError(f"case {case_id} external grader file list drifted")
    if grader.get("hidden_verifiers") != ["integration"]:
        raise CalibrationError(f"case {case_id} hidden verifier declaration drifted")
    if grader.get("visible_verifiers") != ["unit"]:
        raise CalibrationError(f"case {case_id} visible verifier declaration drifted")
    policy = required_manifest_mapping(case, "runner_model_policy")
    for key, value in EVALUATION_POLICY.items():
        if policy.get(key) != value:
            raise CalibrationError(f"case {case_id} evaluation policy drifted")
    calibration_policy = required_manifest_mapping(policy, "calibration")
    if dict(calibration_policy) != CALIBRATION_POLICY:
        raise CalibrationError(f"case {case_id} calibration policy drifted")
    actual = {
        "case_contract_sha256": fingerprint_paths(case_directory, ("case.json",)),
        "external_grader_sha256": fingerprint_tree(
            safe_join(case_directory, grader_path, "external grader"), grader_files
        ),
        "gold_projection_sha256": fingerprint_paths(
            safe_join(case_directory, artifacts["gold_projection"], "gold projection").parent,
            ("active_projection.json",),
        ),
        "known_bad_artifact_sha256": artifact_fingerprint(
            case_directory, artifacts, "known_bad"
        ),
        "neutral_continuation_prompt_sha256": digest_text(
            required_manifest_string(case, "neutral_continuation_prompt")
        ),
        "plain_summary_artifact_sha256": plain_summary_artifact_fingerprint(
            case_directory, artifacts
        ),
        "plain_summary_sha256": digest_text(required_manifest_string(case, "plain_summary")),
        "reference_artifact_sha256": artifact_fingerprint(
            case_directory, artifacts, "reference"
        ),
        "source_bundle_sha256": file_artifact_fingerprint(
            case_directory, artifacts, "source_bundle"
        ),
        "workspace_sha256": fingerprint_tree(
            safe_join(case_directory, workspace_path, "workspace"), workspace_files
        ),
    }
    for key, value in actual.items():
        expected = fingerprint_values.get(key)
        if expected != value:
            raise CalibrationError(f"case {case_id} {key} drifted")
    prompt = required_manifest_string(case, "neutral_continuation_prompt")
    summary = required_manifest_string(case, "plain_summary")
    if prompt != NEUTRAL_CONTINUATION_PROMPT:
        raise CalibrationError(f"case {case_id} neutral prompt drifted")
    summary_artifact = read_text(
        safe_join(case_directory, artifacts["plain_summary"], "plain summary"),
        "plain summary artifact",
    )
    if summary_artifact != summary:
        raise CalibrationError(f"case {case_id} plain summary artifact drifted")
    if case.get("neutral_continuation_prompt_utf8_bytes") != utf8_bytes(prompt):
        raise CalibrationError(f"case {case_id} neutral prompt byte count drifted")
    if case.get("plain_summary_utf8_bytes") != utf8_bytes(summary):
        raise CalibrationError(f"case {case_id} plain summary byte count drifted")
    if case.get("neutral_continuation_prompt_sha256") != digest_text(prompt):
        raise CalibrationError(f"case {case_id} neutral prompt hash drifted")
    if case.get("plain_summary_sha256") != digest_text(summary):
        raise CalibrationError(f"case {case_id} plain summary hash drifted")
    source_bundle_path = safe_join(
        case_directory, artifacts["source_bundle"], "neutral source bundle"
    )
    source_bundle = read_object(source_bundle_path, "neutral source bundle")
    expected_source_bundle = source_bundle_for_case(
        case_contract, actual["workspace_sha256"]
    )
    if source_bundle != expected_source_bundle:
        raise CalibrationError(f"case {case_id} neutral source bundle drifted")
    projection = required_manifest_mapping(case, "gold_projection")
    validate_gold_projection(projection)
    gold_projection_path = safe_join(
        case_directory, artifacts["gold_projection"], "gold projection"
    )
    if read_object(gold_projection_path, "gold projection") != dict(projection):
        raise CalibrationError(f"case {case_id} gold projection drifted")
    reference_files = relative_files(
        safe_join(case_directory, artifacts["reference"], "reference artifact")
    )
    known_bad_files = relative_files(
        safe_join(case_directory, artifacts["known_bad"], "known-bad artifact")
    )
    allowed_paths = required_manifest_string_list(case, "allowed_paths")
    ensure_artifact_paths(
        case_id,
        reference_files,
        allowed_paths,
        safe_join(case_directory, artifacts["reference"], "reference artifact"),
    )
    ensure_artifact_paths(
        case_id,
        known_bad_files,
        allowed_paths,
        safe_join(case_directory, artifacts["known_bad"], "known-bad artifact"),
    )
    facts = canonical_facts_for(projection)
    information = required_manifest_mapping(case, "information_match")
    if information.get("status") != "exact":
        raise CalibrationError(f"case {case_id} information match is not exact")
    if information.get("canonical_facts") != facts:
        raise CalibrationError(f"case {case_id} canonical facts drifted")
    if information.get("active_fact_sha256") != digest_facts(facts):
        raise CalibrationError(f"case {case_id} active fact fingerprint drifted")
    expected_summary = plain_summary_for_case(case, projection) + "\n"
    if summary != expected_summary:
        raise CalibrationError(f"case {case_id} plain summary does not match gold facts")


def artifact_fingerprint(
    case_directory: Path, artifacts: Mapping[str, Any], key: str
) -> str:
    """Fingerprint one artifact directory referenced by a manifest."""

    relative = required_manifest_string(artifacts, key)
    directory = safe_join(case_directory, relative, f"{key} artifact")
    return fingerprint_tree(directory, relative_files(directory))


def plain_summary_artifact_fingerprint(
    case_directory: Path, artifacts: Mapping[str, Any]
) -> str:
    """Fingerprint the exact UTF-8 bytes supplied to the plain-summary arm."""

    relative = required_manifest_string(artifacts, "plain_summary")
    path = safe_join(case_directory, relative, "plain summary artifact")
    if path.is_dir():
        raise CalibrationError("plain summary artifact must be a regular file")
    return fingerprint_paths(path.parent, (path.name,))


def file_artifact_fingerprint(
    case_directory: Path, artifacts: Mapping[str, Any], key: str
) -> str:
    """Fingerprint one regular-file artifact referenced by a manifest."""

    relative = required_manifest_string(artifacts, key)
    path = safe_join(case_directory, relative, f"{key} artifact")
    if path.is_dir():
        raise CalibrationError(f"{key} artifact must be a regular file")
    return fingerprint_paths(path.parent, (path.name,))


def ensure_artifact_paths(
    case_id: str,
    artifact_files: Sequence[str],
    allowed_paths: Sequence[str],
    artifact_directory: Path,
) -> None:
    """Require artifact overlays to stay within the case allowlist."""

    allowed = set(allowed_paths)
    for relative in artifact_files:
        if relative not in allowed:
            raise CalibrationError(
                f"case {case_id} artifact file escapes allowed paths: "
                f"{artifact_directory / relative}"
            )


def plain_summary_for_case(case: Mapping[str, Any], projection: Mapping[str, Any]) -> str:
    """Render the summary from one already-frozen case entry."""

    del case
    return plain_summary_for_projection(projection)


def validate_gold_projection(projection: Mapping[str, Any]) -> None:
    """Require stable IDs, closed-world actives, and logical provenance labels."""

    if projection.get("schema_version") != 1:
        raise CalibrationError("gold projection schema_version must be 1")
    if projection.get("allow_additional_active") is not False:
        raise CalibrationError("gold projection must reject additional active intent")
    if projection.get("source_namespace") != {
        "comparison": "non_identity",
        "name": "fixture_logical_v1",
    }:
        raise CalibrationError("gold projection source namespace is invalid")
    goal = required_manifest_mapping(projection, "goal")
    required_manifest_string(goal, "text")
    required_manifest_string_list(goal, "source_record_ids")
    requirements = projection.get("requirements")
    inactive = projection.get("inactive_requirements")
    if not isinstance(requirements, list) or not requirements:
        raise CalibrationError("gold projection requirements must be a nonempty array")
    if not isinstance(inactive, list):
        raise CalibrationError("gold projection inactive_requirements must be an array")
    active_ids: set[str] = set()
    for requirement in requirements:
        if not isinstance(requirement, Mapping):
            raise CalibrationError("gold projection requirement must be an object")
        requirement_id = required_manifest_string(requirement, "id")
        if requirement_id in active_ids:
            raise CalibrationError("gold projection requirement IDs must be unique")
        active_ids.add(requirement_id)
        required_manifest_string(requirement, "text")
        required_manifest_string_list(requirement, "source_record_ids")
        evidence = required_manifest_mapping(requirement, "evidence")
        evidence_kind = required_manifest_string(evidence, "kind")
        if evidence_kind == "external_verifier":
            if required_manifest_string(evidence, "verifier_name") not in {
                "unit",
                "integration",
            }:
                raise CalibrationError("gold verifier evidence mapping is invalid")
        elif evidence_kind == "mutation_scope":
            if "verifier_name" in evidence:
                raise CalibrationError("mutation-scope evidence cannot name a verifier")
        else:
            raise CalibrationError("gold requirement evidence kind is invalid")
    inactive_ids: set[str] = set()
    for requirement in inactive:
        if not isinstance(requirement, Mapping):
            raise CalibrationError("gold inactive requirement must be an object")
        requirement_id = required_manifest_string(requirement, "id")
        if requirement_id in active_ids or requirement_id in inactive_ids:
            raise CalibrationError("gold projection requirement IDs must be unique")
        inactive_ids.add(requirement_id)
        required_manifest_string(requirement, "text")
        required_manifest_string_list(requirement, "source_record_ids")
        if requirement.get("lifecycle") != "superseded":
            raise CalibrationError("gold inactive requirement lifecycle must be superseded")
        superseded_by = required_manifest_string(requirement, "superseded_by")
        if superseded_by not in active_ids:
            raise CalibrationError("gold inactive requirement superseded_by is not active")


def plain_summary_for_projection(projection: Mapping[str, Any]) -> str:
    """Render the stable prose form of a structured projection."""

    goal = required_manifest_mapping(projection, "goal")["text"]
    requirements = projection.get("requirements")
    if not isinstance(requirements, list) or not requirements:
        raise CalibrationError("gold projection requirements must be a nonempty array")
    requirement_text = []
    for index, requirement in enumerate(requirements, start=1):
        if not isinstance(requirement, Mapping):
            raise CalibrationError(f"gold projection requirement {index} is not an object")
        requirement_text.append(required_manifest_string(requirement, "text"))
    allowed = projection.get("allowed_paths")
    if not isinstance(allowed, list) or not allowed:
        raise CalibrationError("gold projection allowed_paths must be a nonempty array")
    paths = [required_manifest_string(item, "path") if isinstance(item, Mapping) else str(item) for item in allowed]
    return (
        f"Goal: {required_manifest_string({'text': goal}, 'text')} "
        f"Requirements: {' '.join(f'{index}) {text}' for index, text in enumerate(requirement_text, start=1))} "
        f"Allowed paths: {', '.join(paths)}."
    )


def canonical_facts_for(projection: Mapping[str, Any]) -> list[str]:
    """Return ordered fact values shared by projection and plain summary."""

    goal = required_manifest_mapping(projection, "goal")
    facts = [f"goal={required_manifest_string(goal, 'text')}"]
    requirements = projection.get("requirements")
    if not isinstance(requirements, list) or not requirements:
        raise CalibrationError("gold projection requirements must be a nonempty array")
    for requirement in requirements:
        if not isinstance(requirement, Mapping):
            raise CalibrationError("gold projection requirement must be an object")
        facts.append(f"requirement={required_manifest_string(requirement, 'text')}")
    allowed_paths = projection.get("allowed_paths")
    if not isinstance(allowed_paths, list) or not allowed_paths:
        raise CalibrationError("gold projection allowed_paths must be a nonempty array")
    for item in allowed_paths:
        if isinstance(item, Mapping):
            path = required_manifest_string(item, "path")
        else:
            path = item
        if not isinstance(path, str) or not path:
            raise CalibrationError("gold projection allowed path must be a string")
        facts.append(f"allowed_path={path}")
    return facts


def suite_fingerprint(cases: Sequence[Mapping[str, Any]]) -> str:
    """Hash the ordered case IDs and their immutable input fingerprints."""

    payload = [
        {
            "fingerprints": case.get("fingerprints"),
            "id": case.get("id"),
        }
        for case in cases
    ]
    return digest_text(json.dumps(payload, sort_keys=True, separators=(",", ":")))


def fingerprint_tree(root: Path, relative_paths: Sequence[str]) -> str:
    """Hash an exact ordered set of relative regular files and their bytes."""

    root = root.resolve()
    actual = relative_files(root)
    expected = sorted(relative_paths)
    if actual != expected:
        raise CalibrationError(
            f"file set drift under {root}: expected {expected}, found {actual}"
        )
    return fingerprint_paths(root, expected)


def fingerprint_paths(root: Path, relative_paths: Sequence[str]) -> str:
    """Hash named files with lengths to avoid path/content ambiguity."""

    digest = hashlib.sha256()
    for relative in sorted(relative_paths):
        path = safe_join(root, relative, "fingerprinted file")
        if path.is_symlink() or not path.is_file():
            raise CalibrationError(f"fingerprinted input is not a regular file: {path}")
        try:
            content = path.read_bytes()
        except OSError as error:
            raise CalibrationError(f"could not fingerprint {path}: {error}") from error
        encoded = relative.encode("utf-8")
        digest.update(len(encoded).to_bytes(8, "big"))
        digest.update(encoded)
        digest.update(len(content).to_bytes(8, "big"))
        digest.update(content)
    return digest.hexdigest()


def relative_files(root: Path) -> list[str]:
    """List regular fixture files while excluding interpreter cache files."""

    if not root.is_dir():
        raise CalibrationError(f"fixture directory does not exist: {root}")
    files = []
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root)
        if any(part in EVALUATION_INPUT_EXCLUDES for part in relative.parts):
            continue
        if path.is_symlink():
            raise CalibrationError(f"fixture input must not be a symlink: {path}")
        if path.is_file() and path.suffix != ".pyc":
            files.append(relative.as_posix())
    return files


def safe_join(root: Path, relative: Any, label: str) -> Path:
    """Join a portable relative path without allowing escape from its root."""

    if not isinstance(relative, str) or not relative:
        raise CalibrationError(f"{label} path must be a nonempty string")
    path = PurePosixPath(relative)
    if path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
        raise CalibrationError(f"{label} path must be portable and relative")
    root = root.resolve()
    candidate = root.joinpath(*path.parts).resolve()
    if candidate != root and root not in candidate.parents:
        raise CalibrationError(f"{label} path escapes its root")
    return candidate


def required_relative(raw: Mapping[str, Any], field: str, path: Path) -> str:
    """Read a safe relative path from a case contract."""

    value = raw.get(field)
    if not isinstance(value, str):
        raise CalibrationError(f"{path} field {field!r} must be a string")
    return safe_join(path.parent, value, field).relative_to(path.parent.resolve()).as_posix()


def required_string(raw: Mapping[str, Any], field: str, path: Path) -> str:
    """Read a nonempty string from a JSON object."""

    value = raw.get(field)
    if not isinstance(value, str) or not value.strip():
        raise CalibrationError(f"{path} field {field!r} must be a nonempty string")
    return value


def required_string_list(raw: Mapping[str, Any], field: str, path: Path) -> list[str]:
    """Read a nonempty list of nonempty strings."""

    value = raw.get(field)
    if not isinstance(value, list) or not value:
        raise CalibrationError(f"{path} field {field!r} must be a nonempty array")
    if any(not isinstance(item, str) or not item.strip() for item in value):
        raise CalibrationError(f"{path} field {field!r} must contain strings")
    return list(value)


def read_object(path: Path, label: str) -> dict[str, Any]:
    """Read one object-valued JSON fixture."""

    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise CalibrationError(f"could not read {label} {path}: {error}") from error
    if not isinstance(value, dict):
        raise CalibrationError(f"{label} {path} must contain an object")
    return value


def read_text(path: Path, label: str) -> str:
    """Read one nonempty UTF-8 text fixture."""

    try:
        value = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as error:
        raise CalibrationError(f"could not read {label} {path}: {error}") from error
    if not value.strip():
        raise CalibrationError(f"{label} {path} must be nonempty")
    return value


def required_manifest_string(raw: Mapping[str, Any], field: str) -> str:
    """Read a nonempty string from a manifest object."""

    value = raw.get(field)
    if not isinstance(value, str) or not value.strip():
        raise CalibrationError(f"manifest field {field!r} must be a nonempty string")
    return value


def required_manifest_string_list(raw: Mapping[str, Any], field: str) -> list[str]:
    """Read a nonempty string list from a manifest object."""

    value = raw.get(field)
    if not isinstance(value, list) or not value or any(
        not isinstance(item, str) or not item.strip() for item in value
    ):
        raise CalibrationError(f"manifest field {field!r} must be a nonempty string array")
    return list(value)


def required_manifest_mapping(raw: Mapping[str, Any], field: str) -> Mapping[str, Any]:
    """Read an object from a manifest object."""

    value = raw.get(field)
    if not isinstance(value, Mapping):
        raise CalibrationError(f"manifest field {field!r} must be an object")
    return value


def utf8_bytes(value: str) -> int:
    """Return the exact UTF-8 byte count for one frozen text value."""

    return len(value.encode("utf-8"))


def digest_bytes(value: bytes) -> str:
    """Hash bytes with SHA-256."""

    return hashlib.sha256(value).hexdigest()


def digest_text(value: str) -> str:
    """Hash UTF-8 text with SHA-256."""

    return digest_bytes(value.encode("utf-8"))


def digest_facts(facts: Iterable[str]) -> str:
    """Hash canonical active facts in order."""

    return digest_text(json.dumps(list(facts), ensure_ascii=False, separators=(",", ":")))


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
