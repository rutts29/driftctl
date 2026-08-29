#!/usr/bin/env python3
"""Run one evaluation case through driftctl's durable continuity workflow."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
from typing import Any, Mapping, Sequence

try:
    from .run_baseline import (
        AgentTurn,
        CaseDefinition,
        RunnerError,
        TokenUsage,
        changed_paths_since,
        inject_steering,
        initialize_git_repository,
        load_case,
        parse_events,
        premature_completion,
        run_status,
        run_verifiers,
        turn_summary,
    )
except ImportError:  # Direct script execution has no package context.
    from run_baseline import (
        AgentTurn,
        CaseDefinition,
        RunnerError,
        TokenUsage,
        changed_paths_since,
        inject_steering,
        initialize_git_repository,
        load_case,
        parse_events,
        premature_completion,
        run_status,
        run_verifiers,
        turn_summary,
    )


def main(arguments: Sequence[str]) -> int:
    """Parse CLI arguments, run one case, and print exactly one JSON value."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--case", required=True, type=Path, help="case directory")
    parser.add_argument(
        "--driftctl-bin",
        default="driftctl",
        help="driftctl executable; use a local build for deterministic tests",
    )
    parser.add_argument(
        "--codex-bin",
        default="codex",
        help="Codex executable supplied to driftctl through DRIFTCTL_CODEX_BIN",
    )
    parser.add_argument(
        "--artifacts",
        type=Path,
        help="optional directory for raw per-turn JSONL trajectories",
    )
    namespace = parser.parse_args(arguments)

    try:
        result = run_case(
            namespace.case,
            namespace.driftctl_bin,
            namespace.codex_bin,
            namespace.artifacts,
        )
    except RunnerError as error:
        result = {"status": "runner_error", "error": str(error)}
        print(json.dumps(result, sort_keys=True))
        return 1

    print(json.dumps(result, sort_keys=True))
    return 0


def run_case(
    case_directory: Path,
    driftctl_bin: str,
    codex_bin: str,
    artifact_directory: Path | None = None,
) -> dict[str, Any]:
    """Run the durable workflow on a disposable copy of one evaluation case."""

    started_at = time.monotonic()
    case_directory = case_directory.resolve()
    definition = load_case(case_directory)
    source_workspace = case_directory.joinpath(*definition.workspace.parts).resolve()
    if not source_workspace.is_dir():
        raise RunnerError(f"case workspace does not exist: {source_workspace}")

    with tempfile.TemporaryDirectory(prefix=f"driftctl-{definition.case_id}-") as temporary:
        workspace = Path(temporary) / "workspace"
        shutil.copytree(source_workspace, workspace, symlinks=True)
        initial_commit = initialize_git_repository(workspace)
        start_workflow(driftctl_bin, workspace, definition)

        turns = [run_workflow_turn(driftctl_bin, codex_bin, workspace, "initial")]
        initial_verifiers = run_initial_verifiers(workspace, definition)
        initial_evidence = evidence_for(initial_verifiers)
        initial_requirements_satisfied = initial_evidence is not None
        if initial_evidence is not None:
            satisfy_requirements(
                driftctl_bin,
                workspace,
                range(1, len(definition.initial_requirements) + 1),
                initial_evidence,
            )

        injected_paths: list[str] = []
        steering_ids: list[str] = []
        closure_gate: dict[str, Any] | None = None
        for index, steering_point in enumerate(definition.steering, start=1):
            injected_paths.extend(inject_steering(case_directory, workspace, steering_point))
            steering_id = steer_workflow(driftctl_bin, workspace, steering_point.requirement)
            steering_ids.append(steering_id)
            if closure_gate is None:
                closure_gate = attempted_closure(driftctl_bin, workspace)
            turns.append(
                run_workflow_turn(
                    driftctl_bin,
                    codex_bin,
                    workspace,
                    f"steering-{index}",
                )
            )

        verifiers = run_verifiers(workspace, definition.verifiers)
        final_evidence = evidence_for(verifiers)
        if final_evidence is not None:
            if not initial_requirements_satisfied:
                satisfy_requirements(
                    driftctl_bin,
                    workspace,
                    range(1, len(definition.initial_requirements) + 1),
                    final_evidence,
                )
            satisfy_requirement_ids(driftctl_bin, workspace, steering_ids, final_evidence)
        closure = attempted_closure(driftctl_bin, workspace)
        changed_paths = workflow_changed_paths(workspace, initial_commit, injected_paths)

    token_usage = TokenUsage()
    for turn in turns:
        token_usage.add(turn.events)
    trajectory_files = write_trajectories(artifact_directory, definition.case_id, turns)
    elapsed_seconds = round(time.monotonic() - started_at, 3)
    closure_blocked = bool(closure_gate and closure_gate["exit_code"] == 2)
    verified_completion = (
        closure["exit_code"] == 0
        and all(outcome["passed"] for outcome in verifiers)
        and all(turn.exit_code == 0 for turn in turns)
    )
    return {
        "case_id": definition.case_id,
        "changed_paths": changed_paths,
        "closure": closure,
        "closure_gate": closure_gate,
        "closure_is_evidence_gated": closure_blocked,
        "elapsed_seconds": elapsed_seconds,
        "injected_paths": sorted(set(injected_paths)),
        "mode": "workflow",
        "premature_completion": premature_completion(turns, verifiers),
        "status": "verified" if verified_completion else run_status(turns),
        "title": definition.title,
        "token_usage": token_usage.as_dict(),
        "trajectory_files": trajectory_files,
        "turns": [turn_summary(turn) for turn in turns],
        "verified_completion": verified_completion,
        "verifiers": verifiers,
    }


def start_workflow(driftctl_bin: str, workspace: Path, definition: CaseDefinition) -> None:
    """Initialize the ledger with only requirements known at task start."""

    arguments = ["start", "--goal", definition.goal]
    for requirement in definition.initial_requirements:
        arguments.extend(["--requirement", requirement])
    require_success(invoke_driftctl(driftctl_bin, workspace, arguments), "start workflow")


def run_initial_verifiers(
    workspace: Path, definition: CaseDefinition
) -> list[dict[str, Any]]:
    """Run the known unit boundary before attaching initial evidence to the ledger."""

    unit_verifiers = tuple(
        verifier for verifier in definition.verifiers if verifier.name == "unit"
    )
    if not unit_verifiers:
        raise RunnerError("case must include a verifier named 'unit'")
    return run_verifiers(workspace, unit_verifiers)


def steer_workflow(driftctl_bin: str, workspace: Path, requirement: str) -> str:
    """Append one new requirement and return its ledger-generated identifier."""

    completed = invoke_driftctl(
        driftctl_bin,
        workspace,
        ["steer", "--requirement", requirement],
    )
    require_success(completed, "append steering requirement")
    requirement_id = completed.stdout.strip()
    if not requirement_id:
        raise RunnerError("driftctl steer did not report a requirement identifier")
    return requirement_id


def satisfy_requirements(
    driftctl_bin: str,
    workspace: Path,
    identifiers: Sequence[int],
    evidence: str,
) -> None:
    """Attach the same passed verifier evidence to initial requirement identifiers."""

    satisfy_requirement_ids(
        driftctl_bin,
        workspace,
        (f"R{identifier}" for identifier in identifiers),
        evidence,
    )


def satisfy_requirement_ids(
    driftctl_bin: str,
    workspace: Path,
    identifiers: Sequence[str],
    evidence: str,
) -> None:
    """Satisfy requirements only after the supplied external evidence passes."""

    for identifier in identifiers:
        completed = invoke_driftctl(
            driftctl_bin,
            workspace,
            ["satisfy", "--id", identifier, "--evidence", evidence],
        )
        require_success(completed, f"attach evidence to {identifier}")


def evidence_for(verifiers: Sequence[Mapping[str, Any]]) -> str | None:
    """Return compact causal evidence only when every supplied verifier passed."""

    if not verifiers or any(not verifier["passed"] for verifier in verifiers):
        return None
    names = ", ".join(str(verifier["name"]) for verifier in verifiers)
    return f"external verifier passed: {names}"


def attempted_closure(driftctl_bin: str, workspace: Path) -> dict[str, Any]:
    """Attempt closure and retain its observable evidence-gate result."""

    completed = invoke_driftctl(driftctl_bin, workspace, ["close"])
    return {
        "exit_code": completed.returncode,
        "stderr": completed.stderr,
        "stdout": completed.stdout,
    }


def run_workflow_turn(
    driftctl_bin: str,
    codex_bin: str,
    workspace: Path,
    phase: str,
) -> AgentTurn:
    """Run one fresh driftctl Codex turn and recover its persisted raw JSONL."""

    before = trajectory_paths(workspace)
    completed = invoke_driftctl(
        driftctl_bin,
        workspace,
        ["run", "codex"],
        {"DRIFTCTL_CODEX_BIN": codex_bin},
    )
    created = sorted(trajectory_paths(workspace) - before)
    if not created:
        return AgentTurn(phase=phase, exit_code=completed.returncode, events=())
    if len(created) != 1:
        raise RunnerError(f"driftctl produced {len(created)} trajectories during {phase}")
    try:
        events = parse_events(created[0].read_text(encoding="utf-8"), phase)
    except OSError as error:
        raise RunnerError(f"could not read trajectory during {phase}: {error}") from error
    return AgentTurn(phase=phase, exit_code=completed.returncode, events=events)


def trajectory_paths(workspace: Path) -> set[Path]:
    """List driftctl trajectories without treating its private state as candidate output."""

    directory = workspace / ".driftctl" / "trajectories"
    if not directory.is_dir():
        return set()
    return {path for path in directory.glob("*.jsonl") if path.is_file()}


def invoke_driftctl(
    driftctl_bin: str,
    workspace: Path,
    arguments: Sequence[str],
    environment: Mapping[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run driftctl while keeping its stdout available for runner-owned JSON output."""

    process_environment = None
    if environment is not None:
        process_environment = dict(os.environ)
        process_environment.update(environment)
    try:
        return subprocess.run(
            [driftctl_bin, *arguments],
            cwd=workspace,
            env=process_environment,
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as error:
        raise RunnerError(f"could not launch driftctl: {error}") from error


def require_success(completed: subprocess.CompletedProcess[str], action: str) -> None:
    """Turn a driftctl setup failure into a useful runner error."""

    if completed.returncode == 0:
        return
    detail = completed.stderr.strip() or completed.stdout.strip()
    raise RunnerError(f"could not {action}: {detail or 'command failed'}")


def workflow_changed_paths(
    workspace: Path,
    initial_commit: str,
    injected_paths: Sequence[str],
) -> list[str]:
    """Exclude evaluator injection and workflow state from candidate change evidence."""

    changed = changed_paths_since(workspace, initial_commit, injected_paths)
    return [
        path
        for path in changed
        if path != ".driftctl" and not path.startswith(".driftctl/")
    ]


def write_trajectories(
    artifact_directory: Path | None,
    case_id: str,
    turns: Sequence[AgentTurn],
) -> list[str]:
    """Persist the exact workflow-provider JSONL when artifacts are requested."""

    if artifact_directory is None:
        return []
    artifact_directory = artifact_directory.resolve()
    artifact_directory.mkdir(parents=True, exist_ok=True)

    filenames: list[str] = []
    for turn in turns:
        filename = f"{case_id}-workflow-{turn.phase}.jsonl"
        path = artifact_directory / filename
        path.write_text(
            "".join(
                f"{json.dumps(event, separators=(',', ':'))}\n" for event in turn.events
            ),
            encoding="utf-8",
        )
        filenames.append(filename)
    return filenames


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
