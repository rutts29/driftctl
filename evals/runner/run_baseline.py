#!/usr/bin/env python3
"""Run one evaluation case with a direct, continuity-free Codex workflow."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import sys
import tempfile
import time
from typing import Any, Mapping, Sequence


class RunnerError(RuntimeError):
    """Report an invalid evaluation fixture or unavailable local prerequisite."""


@dataclass(frozen=True)
class Injection:
    """Copy one steering artifact into the temporary candidate workspace."""

    source: PurePosixPath
    destination: PurePosixPath


@dataclass(frozen=True)
class SteeringPoint:
    """A requirement that arrives after the first agent turn."""

    requirement: str
    injections: tuple[Injection, ...]


@dataclass(frozen=True)
class Verifier:
    """One externally run command used to score a candidate workspace."""

    name: str
    command: str


@dataclass(frozen=True)
class CaseDefinition:
    """The normalized, validated input required for one baseline run."""

    case_id: str
    title: str
    workspace: PurePosixPath
    goal: str
    initial_requirements: tuple[str, ...]
    steering: tuple[SteeringPoint, ...]
    verifiers: tuple[Verifier, ...]


@dataclass(frozen=True)
class AgentTurn:
    """The observable output of one direct Codex invocation."""

    phase: str
    exit_code: int
    events: tuple[Mapping[str, Any], ...]

    def completed(self) -> bool:
        """Return whether Codex reported a completed turn."""

        return any(event.get("type") == "turn.completed" for event in self.events)


@dataclass
class TokenUsage:
    """Aggregate token fields that Codex supplied in JSONL events."""

    available: bool = False
    input_tokens: int = 0
    cached_input_tokens: int = 0
    output_tokens: int = 0
    reported_total_tokens: int = 0

    def add(self, events: Sequence[Mapping[str, Any]]) -> None:
        """Accumulate the known usage fields in one agent turn."""

        for event in events:
            usage = event.get("usage")
            if not isinstance(usage, Mapping):
                continue
            self.available = True
            self.input_tokens += integer_field(usage, "input_tokens")
            self.cached_input_tokens += integer_field(usage, "cached_input_tokens")
            self.output_tokens += integer_field(usage, "output_tokens")
            self.reported_total_tokens += integer_field(usage, "total_tokens")

    def as_dict(self) -> dict[str, int | bool]:
        """Return stable JSON suitable for comparison across evaluation runs."""

        observed_total = self.input_tokens + self.output_tokens
        total_tokens = self.reported_total_tokens or observed_total
        return {
            "available": self.available,
            "input_tokens": self.input_tokens,
            "cached_input_tokens": self.cached_input_tokens,
            "output_tokens": self.output_tokens,
            "total_tokens": total_tokens,
        }


def main(arguments: Sequence[str]) -> int:
    """Parse CLI arguments, run a single case, and print exactly one JSON value."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--case", required=True, type=Path, help="case directory")
    parser.add_argument(
        "--codex-bin",
        default="codex",
        help="Codex executable; use a local fake for deterministic tests",
    )
    parser.add_argument(
        "--artifacts",
        type=Path,
        help="optional directory for raw per-turn JSONL trajectories",
    )
    namespace = parser.parse_args(arguments)

    try:
        result = run_case(namespace.case, namespace.codex_bin, namespace.artifacts)
    except RunnerError as error:
        result = {"status": "runner_error", "error": str(error)}
        print(json.dumps(result, sort_keys=True))
        return 1

    print(json.dumps(result, sort_keys=True))
    return 0


def run_case(
    case_directory: Path,
    codex_bin: str,
    artifact_directory: Path | None = None,
) -> dict[str, Any]:
    """Run the direct baseline on one temporary copy of an evaluation case."""

    started_at = time.monotonic()
    case_directory = case_directory.resolve()
    definition = load_case(case_directory)
    source_workspace = contained_path(case_directory, definition.workspace, "workspace")
    if not source_workspace.is_dir():
        raise RunnerError(f"case workspace does not exist: {source_workspace}")

    with tempfile.TemporaryDirectory(prefix=f"driftctl-{definition.case_id}-") as temporary:
        workspace = Path(temporary) / "workspace"
        shutil.copytree(source_workspace, workspace, symlinks=True)
        initial_commit = initialize_git_repository(workspace)

        initial_turn = run_initial_turn(codex_bin, workspace, definition)
        thread_id = thread_id_from(initial_turn.events)
        turns = [initial_turn]
        injected_paths: list[str] = []

        for index, steering_point in enumerate(definition.steering, start=1):
            injected_paths.extend(inject_steering(case_directory, workspace, steering_point))
            turns.append(
                run_resumed_turn(
                    codex_bin,
                    workspace,
                    thread_id,
                    steering_point.requirement,
                    index,
                )
            )

        verifiers = run_verifiers(workspace, definition.verifiers)
        changed_paths = changed_paths_since(workspace, initial_commit, injected_paths)

    token_usage = TokenUsage()
    for turn in turns:
        token_usage.add(turn.events)
    trajectory_files = write_trajectories(
        artifact_directory, definition.case_id, turns
    )
    elapsed_seconds = round(time.monotonic() - started_at, 3)
    return {
        "case_id": definition.case_id,
        "changed_paths": changed_paths,
        "elapsed_seconds": elapsed_seconds,
        "injected_paths": sorted(set(injected_paths)),
        "premature_completion": premature_completion(turns, verifiers),
        "status": run_status(turns),
        "thread_id": thread_id,
        "title": definition.title,
        "token_usage": token_usage.as_dict(),
        "trajectory_files": trajectory_files,
        "turns": [turn_summary(turn) for turn in turns],
        "verifiers": verifiers,
    }


def write_trajectories(
    artifact_directory: Path | None,
    case_id: str,
    turns: Sequence[AgentTurn],
) -> list[str]:
    """Persist raw provider events only when the evaluator requests artifacts."""

    if artifact_directory is None:
        return []
    artifact_directory = artifact_directory.resolve()
    artifact_directory.mkdir(parents=True, exist_ok=True)

    filenames: list[str] = []
    for turn in turns:
        filename = f"{case_id}-baseline-{turn.phase}.jsonl"
        path = artifact_directory / filename
        content = "".join(
            f"{json.dumps(event, separators=(',', ':'))}\n" for event in turn.events
        )
        path.write_text(content, encoding="utf-8")
        filenames.append(filename)
    return filenames


def load_case(case_directory: Path) -> CaseDefinition:
    """Load the small JSON case contract and reject ambiguous path input."""

    case_path = case_directory / "case.json"
    try:
        raw = json.loads(case_path.read_text(encoding="utf-8"))
    except OSError as error:
        raise RunnerError(f"could not read case definition: {error}") from error
    except json.JSONDecodeError as error:
        raise RunnerError(f"invalid case JSON: {error}") from error
    if not isinstance(raw, Mapping):
        raise RunnerError("case definition must be a JSON object")

    return CaseDefinition(
        case_id=required_string(raw, "id"),
        title=required_string(raw, "title"),
        workspace=relative_path(required_string(raw, "workspace"), "workspace"),
        goal=required_string(raw, "goal"),
        initial_requirements=string_list(raw, "initial_requirements"),
        steering=steering_points(raw),
        verifiers=verifier_list(raw),
    )


def required_string(raw: Mapping[str, Any], field: str) -> str:
    """Read one nonempty string from an input object."""

    value = raw.get(field)
    if not isinstance(value, str) or not value.strip():
        raise RunnerError(f"case field {field!r} must be a nonempty string")
    return value


def string_list(raw: Mapping[str, Any], field: str) -> tuple[str, ...]:
    """Read a nonempty list of nonempty strings."""

    value = raw.get(field)
    if not isinstance(value, list) or not value:
        raise RunnerError(f"case field {field!r} must be a nonempty array")
    strings = tuple(item for item in value if isinstance(item, str) and item.strip())
    if len(strings) != len(value):
        raise RunnerError(f"case field {field!r} must contain only nonempty strings")
    return strings


def steering_points(raw: Mapping[str, Any]) -> tuple[SteeringPoint, ...]:
    """Validate the ordered post-start steering points."""

    value = raw.get("steering", [])
    if not isinstance(value, list):
        raise RunnerError("case field 'steering' must be an array")

    points: list[SteeringPoint] = []
    for index, item in enumerate(value, start=1):
        if not isinstance(item, Mapping):
            raise RunnerError(f"steering item {index} must be an object")
        injections = item.get("inject", [])
        if not isinstance(injections, list):
            raise RunnerError(f"steering item {index} inject field must be an array")
        points.append(
            SteeringPoint(
                requirement=required_string(item, "requirement"),
                injections=tuple(injection_from(raw_injection, index) for raw_injection in injections),
            )
        )
    return tuple(points)


def injection_from(raw: Any, steering_index: int) -> Injection:
    """Validate one fixture injection without permitting directory traversal."""

    if not isinstance(raw, Mapping):
        raise RunnerError(f"steering item {steering_index} injection must be an object")
    return Injection(
        source=relative_path(required_string(raw, "source"), "injection source"),
        destination=relative_path(
            required_string(raw, "destination"), "injection destination"
        ),
    )


def verifier_list(raw: Mapping[str, Any]) -> tuple[Verifier, ...]:
    """Validate the shared evaluation commands."""

    value = raw.get("verifiers")
    if not isinstance(value, list) or not value:
        raise RunnerError("case field 'verifiers' must be a nonempty array")
    verifiers: list[Verifier] = []
    for index, item in enumerate(value, start=1):
        if not isinstance(item, Mapping):
            raise RunnerError(f"verifier {index} must be an object")
        verifiers.append(
            Verifier(
                name=required_string(item, "name"),
                command=required_string(item, "command"),
            )
        )
    return tuple(verifiers)


def relative_path(raw: str, label: str) -> PurePosixPath:
    """Return a portable relative path that cannot escape a fixture root."""

    path = PurePosixPath(raw)
    if path.is_absolute() or not path.parts or any(part in {"", ".", ".."} for part in path.parts):
        raise RunnerError(f"{label} must be a nonempty relative path")
    return path


def contained_path(root: Path, relative: PurePosixPath, label: str) -> Path:
    """Resolve a validated path and ensure symlinks cannot escape its root."""

    root = root.resolve()
    candidate = root.joinpath(*relative.parts).resolve()
    if candidate != root and root not in candidate.parents:
        raise RunnerError(f"{label} escapes its allowed directory")
    return candidate


def initialize_git_repository(workspace: Path) -> str:
    """Create a local-only repository so final changes have a fixed base."""

    run_checked(["git", "init", "--quiet"], workspace, "initialize temporary repository")
    run_checked(["git", "add", "--all"], workspace, "stage initial fixture")
    run_checked(
        [
            "git",
            "-c",
            "user.name=driftctl evaluation",
            "-c",
            "user.email=eval@invalid",
            "commit",
            "--quiet",
            "-m",
            "Initial evaluation fixture",
        ],
        workspace,
        "commit initial fixture",
    )
    return run_checked(["git", "rev-parse", "HEAD"], workspace, "read initial commit").stdout.strip()


def run_initial_turn(codex_bin: str, workspace: Path, definition: CaseDefinition) -> AgentTurn:
    """Start the direct agent on only the requirements known at task start."""

    command = [
        codex_bin,
        "exec",
        "--json",
        "--sandbox",
        "workspace-write",
        initial_prompt(definition),
    ]
    return invoke_codex(command, workspace, "initial")


def run_resumed_turn(
    codex_bin: str,
    workspace: Path,
    thread_id: str,
    requirement: str,
    steering_index: int,
) -> AgentTurn:
    """Native-resume the exact initial thread with one later requirement."""

    command = [
        codex_bin,
        "exec",
        "resume",
        thread_id,
        "--json",
        steering_prompt(requirement),
    ]
    return invoke_codex(command, workspace, f"steering-{steering_index}")


def initial_prompt(definition: CaseDefinition) -> str:
    """Build the fair direct-agent prompt without continuity instructions."""

    requirements = "\n".join(f"- {item}" for item in definition.initial_requirements)
    return (
        "Work on the current repository and complete this task.\n\n"
        f"Goal: {definition.goal}\n\n"
        f"Requirements:\n{requirements}\n\n"
        "Make the smallest complete change and run the relevant tests. "
        "Follow the repository's existing instructions."
    )


def steering_prompt(requirement: str) -> str:
    """Build the one new requirement delivered at a steering point."""

    return (
        "A new requirement has arrived. Continue work in this repository and satisfy it:\n\n"
        f"- {requirement}\n\n"
        "Run the relevant tests before finishing."
    )


def invoke_codex(command: Sequence[str], workspace: Path, phase: str) -> AgentTurn:
    """Run Codex without forwarding its JSONL so stdout remains one result object."""

    try:
        completed = subprocess.run(
            command,
            cwd=workspace,
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as error:
        raise RunnerError(f"could not launch Codex during {phase}: {error}") from error
    return AgentTurn(
        phase=phase,
        exit_code=completed.returncode,
        events=parse_events(completed.stdout, phase),
    )


def parse_events(stdout: str, phase: str) -> tuple[Mapping[str, Any], ...]:
    """Decode Codex JSONL while making malformed provider output explicit."""

    events: list[Mapping[str, Any]] = []
    for line_number, line in enumerate(stdout.splitlines(), start=1):
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError as error:
            raise RunnerError(
                f"Codex emitted invalid JSON during {phase} at line {line_number}: {error}"
            ) from error
        if not isinstance(event, Mapping):
            raise RunnerError(
                f"Codex emitted a non-object JSON event during {phase} at line {line_number}"
            )
        events.append(event)
    if not events:
        raise RunnerError(f"Codex emitted no JSON events during {phase}")
    return tuple(events)


def thread_id_from(events: Sequence[Mapping[str, Any]]) -> str:
    """Extract the native resume identifier from the initial Codex trajectory."""

    for event in events:
        if event.get("type") != "thread.started":
            continue
        thread_id = event.get("thread_id")
        if isinstance(thread_id, str) and thread_id:
            return thread_id
    raise RunnerError("initial Codex turn did not emit a thread.started thread_id")


def inject_steering(
    case_directory: Path,
    workspace: Path,
    steering_point: SteeringPoint,
) -> list[str]:
    """Inject newly revealed test material immediately before its resume turn."""

    injected_paths: list[str] = []
    for injection in steering_point.injections:
        source = contained_path(case_directory, injection.source, "injection source")
        destination = contained_path(workspace, injection.destination, "injection destination")
        if not source.is_file():
            raise RunnerError(f"steering injection source does not exist: {source}")
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
        injected_paths.append(injection.destination.as_posix())
    return injected_paths


def run_verifiers(workspace: Path, verifiers: Sequence[Verifier]) -> list[dict[str, Any]]:
    """Run the case's shared verifier commands against the final candidate."""

    outcomes: list[dict[str, Any]] = []
    for verifier in verifiers:
        completed = subprocess.run(
            verifier.command,
            cwd=workspace,
            shell=True,
            capture_output=True,
            text=True,
            check=False,
        )
        outcomes.append(
            {
                "command": verifier.command,
                "exit_code": completed.returncode,
                "name": verifier.name,
                "passed": completed.returncode == 0,
                "stderr": completed.stderr,
                "stdout": completed.stdout,
            }
        )
    return outcomes


def changed_paths_since(
    workspace: Path,
    initial_commit: str,
    injected_paths: Sequence[str],
) -> list[str]:
    """Return candidate changes while excluding evaluator-supplied steering files."""

    committed_or_tracked = run_checked(
        ["git", "diff", "--name-only", "--no-renames", initial_commit],
        workspace,
        "inspect candidate changes",
    ).stdout.splitlines()
    untracked = run_checked(
        ["git", "ls-files", "--others", "--exclude-standard"],
        workspace,
        "inspect untracked candidate changes",
    ).stdout.splitlines()
    excluded = set(injected_paths)
    return sorted({path for path in committed_or_tracked + untracked if path not in excluded})


def run_checked(command: Sequence[str], workspace: Path, action: str) -> subprocess.CompletedProcess[str]:
    """Run one required local Git command and turn failures into useful errors."""

    try:
        completed = subprocess.run(
            command,
            cwd=workspace,
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as error:
        raise RunnerError(f"could not {action}: {error}") from error
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise RunnerError(f"could not {action}: {detail or 'command failed'}")
    return completed


def premature_completion(
    turns: Sequence[AgentTurn], verifiers: Sequence[Mapping[str, Any]]
) -> dict[str, Any]:
    """Report a completed final turn followed by failed external verification."""

    final_turn = turns[-1]
    available = final_turn.completed()
    return {
        "available": available,
        "detected": available and any(not outcome["passed"] for outcome in verifiers),
        "phase": final_turn.phase if available else None,
    }


def run_status(turns: Sequence[AgentTurn]) -> str:
    """Summarize agent process outcomes without overriding the verifier evidence."""

    return "completed" if all(turn.exit_code == 0 for turn in turns) else "agent_failed"


def turn_summary(turn: AgentTurn) -> dict[str, Any]:
    """Keep only the evaluation-relevant direct-agent event facts."""

    return {
        "completed": turn.completed(),
        "exit_code": turn.exit_code,
        "phase": turn.phase,
    }


def integer_field(raw: Mapping[str, Any], field: str) -> int:
    """Read a nonnegative integer usage field, treating absent values as zero."""

    value = raw.get(field, 0)
    return value if isinstance(value, int) and value >= 0 else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
