#!/usr/bin/env python3
"""Compare native Codex continuations from one intact, long-context checkpoint."""

from __future__ import annotations

import argparse
from contextlib import ExitStack
import hashlib
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
        CaseDefinition,
        RunnerError,
        contained_path,
        evaluation_fingerprint,
        initialize_git_repository,
        load_case,
        mutation_scope,
        require_evaluation_fingerprint,
    )
except ImportError:
    from run_baseline import (
        CaseDefinition,
        RunnerError,
        contained_path,
        evaluation_fingerprint,
        initialize_git_repository,
        load_case,
        mutation_scope,
        require_evaluation_fingerprint,
    )

LONG_SESSION_LABEL = "native_long_session"
NO_SIGNIFICANCE_LABEL = "descriptive_only_no_significance"
DEFAULT_WORKER_MODEL = "gpt-5.6-luna"
DEFAULT_WORKER_EFFORT = "max"
DURABLE_READ_ATTEMPTS = 100
DURABLE_READ_INTERVAL_SECONDS = 0.05
CONTEXT_CHUNK_COUNT = 4


class AppServerRequestError(RunnerError):
    def __init__(self, method: str, reason: str = "rejected") -> None:
        self.method = method
        self.reason = reason
        detail = (
            ": no active turn to interrupt"
            if reason == "no_active_turn"
            else ": provider rejected request"
        )
        super().__init__(f"App Server {method} failed{detail}")


class AppServer:
    def __init__(self, codex_bin: str) -> None:
        try:
            self.process = subprocess.Popen(
                [codex_bin, "app-server", "--stdio"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
        except OSError as error:
            raise RunnerError(f"could not launch Codex App Server: {error}") from error
        if self.process.stdin is None or self.process.stdout is None:
            raise RunnerError("could not open Codex App Server pipes")
        self.input = self.process.stdin
        self.output = self.process.stdout
        self.next_id = 1

    def close(self) -> None:
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait()

    def initialize(self) -> None:
        self.request(
            "initialize",
            {
                "clientInfo": {"name": "driftctl-native-eval", "version": "1"},
                "capabilities": {"experimentalApi": False, "requestAttestation": False},
            },
        )
        self.notify("initialized", {})

    def request(self, method: str, params: Mapping[str, Any]) -> Mapping[str, Any]:
        request_id = self.next_id
        self.next_id += 1
        self._write({"id": request_id, "method": method, "params": params})
        while True:
            response = self._read()
            if "method" in response and "id" not in response:
                continue
            if response.get("id") != request_id:
                raise RunnerError(
                    f"App Server returned an unexpected response ID for {method}"
                )
            error = response.get("error")
            if isinstance(error, Mapping):
                message = error.get("message")
                reason = (
                    "no_active_turn"
                    if message == "no active turn to interrupt"
                    else "rejected"
                )
                raise AppServerRequestError(method, reason)
            result = response.get("result")
            if not isinstance(result, Mapping):
                raise RunnerError(f"App Server {method} response has no object result")
            return result

    def notify(self, method: str, params: Mapping[str, Any]) -> None:
        self._write({"method": method, "params": params})

    def record_user_turn(
        self, thread_id: str, text: str, phase: str, expected_count: int
    ) -> str:
        result = self.request(
            "turn/start",
            {
                "input": [{"type": "text", "text": text}],
                "threadId": thread_id,
            },
        )
        turn = result.get("turn")
        if not isinstance(turn, Mapping):
            raise RunnerError(f"App Server {phase} turn has no turn object")
        turn_id = turn.get("id")
        status = turn.get("status")
        if not isinstance(turn_id, str) or not turn_id or not isinstance(status, str):
            raise RunnerError(f"App Server {phase} turn is malformed")
        if status == "inProgress":
            self.require_user_message_count(thread_id, expected_count)
            try:
                self.request(
                    "turn/interrupt", {"threadId": thread_id, "turnId": turn_id}
                )
            except AppServerRequestError as error:
                if error.reason != "no_active_turn":
                    raise
        elif status not in {"completed", "interrupted"}:
            raise RunnerError(f"App Server {phase} turn ended with {status!r}")
        self.require_user_message_count(thread_id, expected_count)
        return turn_id

    def require_user_message_count(self, thread_id: str, expected: int) -> None:
        for _ in range(DURABLE_READ_ATTEMPTS):
            try:
                result = self.request(
                    "thread/read", {"threadId": thread_id, "includeTurns": True}
                )
            except AppServerRequestError:
                time.sleep(DURABLE_READ_INTERVAL_SECONDS)
                continue
            thread = result.get("thread")
            turns = thread.get("turns") if isinstance(thread, Mapping) else None
            if isinstance(turns, list):
                count = sum(
                    1
                    for turn in turns
                    if isinstance(turn, Mapping)
                    for item in turn.get("items", [])
                    if isinstance(item, Mapping) and item.get("type") == "userMessage"
                )
                if count == expected:
                    return
                if count > expected:
                    break
            time.sleep(DURABLE_READ_INTERVAL_SECONDS)
        raise RunnerError(
            f"native source did not durably retain exactly {expected} user messages"
        )

    def _write(self, value: Mapping[str, Any]) -> None:
        self.input.write(json.dumps(value, separators=(",", ":")) + "\n")
        self.input.flush()

    def _read(self) -> Mapping[str, Any]:
        line = self.output.readline()
        if not line:
            raise RunnerError("Codex App Server closed before a response")
        try:
            value = json.loads(line)
        except json.JSONDecodeError as error:
            raise RunnerError(
                f"Codex App Server emitted malformed JSON: {error}"
            ) from error
        if not isinstance(value, Mapping):
            raise RunnerError("Codex App Server emitted a non-object response")
        return value


def main(arguments: Sequence[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--case", required=True, type=Path)
    parser.add_argument("--results-dir", required=True, type=Path)
    parser.add_argument("--driftctl-bin", default="driftctl")
    parser.add_argument("--codex-bin", default="codex")
    parser.add_argument("--context-bytes", type=int, default=32768)
    parser.add_argument("--worker-model", default=DEFAULT_WORKER_MODEL)
    parser.add_argument("--worker-effort", default=DEFAULT_WORKER_EFFORT)
    parser.add_argument("--artifacts", type=Path)
    namespace = parser.parse_args(arguments)
    try:
        manifest = run_case(
            namespace.case,
            namespace.results_dir,
            namespace.driftctl_bin,
            namespace.codex_bin,
            namespace.context_bytes,
            namespace.artifacts,
            namespace.worker_model,
            namespace.worker_effort,
        )
    except RunnerError as error:
        print(
            json.dumps({"status": "runner_error", "error": str(error)}, sort_keys=True)
        )
        return 1
    print(json.dumps(manifest, sort_keys=True))
    return 0


def run_case(
    case_directory: Path,
    results_directory: Path,
    driftctl_bin: str,
    codex_bin: str,
    context_bytes: int = 32768,
    artifact_directory: Path | None = None,
    worker_model: str = DEFAULT_WORKER_MODEL,
    worker_effort: str = DEFAULT_WORKER_EFFORT,
) -> dict[str, Any]:
    if context_bytes < 0 or context_bytes > 512 * 1024:
        raise RunnerError("context bytes must be between 0 and 524288")
    if not worker_model.strip() or not worker_effort.strip():
        raise RunnerError("worker model and effort must be nonempty")
    worker_policy = {
        "approval_policy": "never",
        "effort": worker_effort,
        "model": worker_model,
        "sandbox": "workspace-write",
    }
    started = time.monotonic()
    case_directory = case_directory.resolve()
    definition = load_case(case_directory)
    fingerprint = evaluation_fingerprint(case_directory)
    source_workspace = contained_path(case_directory, definition.workspace, "workspace")
    if not source_workspace.is_dir():
        raise RunnerError(f"case workspace does not exist: {source_workspace}")
    results_directory = results_directory.resolve()
    results_directory.mkdir(parents=True, exist_ok=True)
    with ExitStack() as resources:
        if artifact_directory is None:
            temporary = resources.enter_context(
                tempfile.TemporaryDirectory(
                    prefix=f"driftctl-native-{definition.case_id}-"
                )
            )
            root = Path(temporary)
            private_artifact_root = None
        else:
            root = create_private_run_directory(artifact_directory, definition.case_id)
            private_artifact_root = root / "artifacts"
        workspace = root / "source-workspace"
        shutil.copytree(source_workspace, workspace, symlinks=True)
        initialize_git_repository(workspace)
        state_directory = root / "state"
        state_directory.mkdir(mode=0o700)
        temporary_directory = root / "tmp"
        temporary_directory.mkdir(mode=0o700)
        environment = os.environ | {
            "DRIFTCTL_CODEX_BIN": codex_bin,
            "TMPDIR": str(temporary_directory),
            "XDG_STATE_HOME": str(state_directory),
        }
        session_id, source_turns, injection = seed_native_session(
            codex_bin, workspace, definition, context_bytes, worker_policy
        )
        source_clean = git_clean(workspace)
        if not source_clean:
            raise RunnerError("planning-only source turns changed the source workspace")
        comparison = invoke_compare(driftctl_bin, workspace, session_id, environment)
        require_evaluation_fingerprint(case_directory, fingerprint)
        private_artifact = retain_private_artifact(
            private_artifact_root, definition.case_id, session_id, comparison
        )
        if private_artifact is not None:
            private_artifact = f"{root.name}/artifacts/{private_artifact}"
        arms = {mode: comparison.get(mode) for mode in ("baseline", "workflow")}
        if any(not isinstance(arm, Mapping) for arm in arms.values()):
            raise RunnerError("native comparison has an incomplete arm")
        results = {
            mode: arm_result(
                definition,
                mode,
                arms[mode],
                driftctl_bin,
                case_directory,
                fingerprint,
                environment,
                source_turns,
                injection,
                source_clean,
                session_id,
                private_artifact,
                worker_policy,
            )
            for mode in ("baseline", "workflow")
        }
        pair_elapsed = round(time.monotonic() - started, 3)
        for result in results.values():
            result["elapsed_seconds"] = pair_elapsed
            result["elapsed_scope"] = "paired_case_wall_time"
        require_evaluation_fingerprint(case_directory, fingerprint)
    paths = write_results(results_directory, definition.case_id, results)
    return {
        "case_id": definition.case_id,
        "evaluation_kind": LONG_SESSION_LABEL,
        "result_files": paths,
        "statistical_claim": NO_SIGNIFICANCE_LABEL,
        "status": "completed",
    }


def seed_native_session(
    codex_bin: str,
    workspace: Path,
    definition: CaseDefinition,
    context_bytes: int,
    worker_policy: Mapping[str, str],
) -> tuple[str, list[str], dict[str, Any]]:
    server = AppServer(codex_bin)
    try:
        server.initialize()
        started = server.request(
            "thread/start",
            {
                "approvalPolicy": worker_policy["approval_policy"],
                "cwd": str(workspace),
                "effort": worker_policy["effort"],
                "ephemeral": False,
                "model": worker_policy["model"],
                "sandbox": "read-only",
            },
        )
        thread = started.get("thread")
        thread_id = thread.get("id") if isinstance(thread, Mapping) else None
        if not isinstance(thread_id, str) or not thread_id:
            raise RunnerError(
                "App Server thread/start response has no persisted thread ID"
            )
        initial = server.record_user_turn(
            thread_id, initial_source_prompt(definition), "initial", 1
        )
        source_turns = [initial]
        for index, steering in enumerate(definition.steering, start=1):
            source_turns.append(
                server.record_user_turn(
                    thread_id,
                    steering_source_prompt(steering.requirement, index),
                    f"steering-{index}",
                    index + 1,
                )
            )
        injection = inject_non_authoritative_context(server, thread_id, context_bytes)
        return thread_id, source_turns, injection
    finally:
        server.close()


def initial_source_prompt(definition: CaseDefinition) -> str:
    """Render the initial user turn without any later steering."""

    requirements = "\n".join(f"- {item}" for item in definition.initial_requirements)
    return f"Goal:\n{definition.goal}\n\nKnown requirements:\n{requirements}"


def steering_source_prompt(requirement: str, index: int) -> str:
    """Render one chronological user steering update as its own native turn."""

    return f"Late steering {index}:\n- {requirement}"


def inject_non_authoritative_context(
    server: AppServer, thread_id: str, context_bytes: int
) -> dict[str, Any]:
    if context_bytes == 0:
        return {
            "attempted": False,
            "accepted": False,
            "bytes": 0,
            "chunks": 0,
            "position": "after_last_user_turn",
        }
    payloads = synthetic_context_payloads(context_bytes)
    items = [
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": payload}],
        }
        for payload in payloads
    ]
    try:
        server.request("thread/inject_items", {"threadId": thread_id, "items": items})
    except RunnerError:
        return {
            "attempted": True,
            "accepted": False,
            "bytes": context_bytes,
            "chunks": len(payloads),
            "position": "after_last_user_turn",
            "reason": "unsupported_or_rejected",
        }
    return {
        "attempted": True,
        "accepted": True,
        "bytes": context_bytes,
        "chunks": len(payloads),
        "position": "after_last_user_turn",
    }


def synthetic_context_payloads(context_bytes: int) -> list[str]:
    """Create deterministic varied assistant activity totaling the requested bytes."""

    chunk_count = min(CONTEXT_CHUNK_COUNT, context_bytes)
    base_size, remainder = divmod(context_bytes, chunk_count)
    payloads: list[str] = []
    for chunk_index in range(chunk_count):
        size = base_size + (1 if chunk_index < remainder else 0)
        lines: list[str] = []
        line_index = 0
        payload_size = 0
        while payload_size < size:
            line = (
                "Synthetic activity "
                f"{chunk_index + 1:02d}.{line_index:05d}: "
                f"module_{line_index % 97:02d} check_{line_index % 53:02d} "
                f"status_{line_index % 11:02d}; no user requirement.\n"
            )
            lines.append(line)
            payload_size += len(line)
            line_index += 1
        payloads.append("".join(lines)[:size])
    return payloads


def invoke_compare(
    driftctl_bin: str, workspace: Path, session_id: str, environment: Mapping[str, str]
) -> Mapping[str, Any]:
    completed = invoke(
        [driftctl_bin, "compare", "codex", "--session", session_id, "--json"],
        workspace,
        environment,
        "run native comparison",
    )
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise RunnerError(f"native comparison failed: {detail or 'command failed'}")
    try:
        value = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise RunnerError(f"native comparison emitted invalid JSON: {error}") from error
    if not isinstance(value, Mapping):
        raise RunnerError("native comparison emitted a non-object result")
    return value


def arm_result(
    definition: CaseDefinition,
    mode: str,
    arm: Mapping[str, Any],
    driftctl_bin: str,
    case_directory: Path,
    fingerprint: str,
    base_environment: Mapping[str, str],
    source_turns: Sequence[str],
    injection: Mapping[str, Any],
    source_clean: bool,
    source_session_id: str,
    private_artifact: str | None,
    worker_policy: Mapping[str, str],
) -> dict[str, Any]:
    candidate = Path(str(arm["child_cwd"]))
    if not candidate.is_dir():
        raise RunnerError(f"native comparison {mode} arm has no accessible candidate")
    verifiers = verify_arm(
        driftctl_bin, candidate, definition, case_directory, base_environment
    )
    require_evaluation_fingerprint(case_directory, fingerprint)
    agent_succeeded = arm.get("turn_status") == "completed"
    changed_paths = arm.get("changed_paths", [])
    scope = mutation_scope(changed_paths, definition.allowed_changed_paths)
    verified = (
        agent_succeeded
        and all(item["passed"] for item in verifiers)
        and scope["passed"]
    )
    result: dict[str, Any] = {
        "agent_succeeded": agent_succeeded,
        "case_id": definition.case_id,
        "changed_paths": changed_paths,
        "evaluation_kind": LONG_SESSION_LABEL,
        "mode": mode,
        "native_checkpoint": {
            "injection": dict(injection),
            "source_user_turn_count": len(source_turns),
            "source_turn_labels": [
                "initial",
                *(f"steering-{index}" for index in range(1, len(source_turns))),
            ],
            "source_workspace_clean": source_clean,
        },
        "premature_completion": {
            "available": agent_succeeded,
            "detected": agent_succeeded
            and not all(item["passed"] for item in verifiers),
            "phase": "continuation" if agent_succeeded else None,
        },
        "recovery_context": "intact_native_session",
        "source_session_sha256": digest_text(source_session_id),
        "scope": scope,
        "statistical_claim": NO_SIGNIFICANCE_LABEL,
        "status": (
            "verified"
            if verified
            else "completed" if agent_succeeded else "agent_failed"
        ),
        "title": definition.title,
        "token_usage": {
            "available": False,
            "cached_input_tokens": 0,
            "input_tokens": 0,
            "output_tokens": 0,
            "total_tokens": 0,
        },
        "turn_status": arm.get("turn_status"),
        "verifier_fingerprint_sha256": fingerprint,
        "verified_completion": verified,
        "verifiers": verifiers,
        "worker_policy": dict(worker_policy),
    }
    if private_artifact is not None:
        result["private_artifact"] = private_artifact
    return result


def verify_arm(
    driftctl_bin: str,
    candidate: Path,
    definition: CaseDefinition,
    case_directory: Path,
    base_environment: Mapping[str, str],
) -> list[dict[str, Any]]:
    environment = dict(base_environment)
    environment["DRIFTCTL_EVAL_CASE_DIR"] = str(case_directory)
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    environment["PYTHONPATH"] = str(candidate)
    outcomes: list[dict[str, Any]] = []
    for verifier in definition.verifiers:
        requirement_id = f"{definition.case_id}:{verifier.name}"
        completed = invoke(
            [
                driftctl_bin,
                "verify",
                "--candidate",
                str(candidate),
                "--requirement",
                requirement_id,
                "--json",
                "--",
                "/bin/sh",
                "-lc",
                verifier.command,
            ],
            candidate,
            environment,
            f"verify {requirement_id}",
        )
        try:
            raw = json.loads(completed.stdout)
        except json.JSONDecodeError as error:
            raise RunnerError(
                f"verifier {requirement_id} emitted invalid JSON: {error}"
            ) from error
        if not isinstance(raw, Mapping):
            raise RunnerError(f"verifier {requirement_id} emitted a non-object result")
        outcomes.append(
            {
                "artifact_id": raw.get("artifact_id"),
                "candidate_after_digest": raw.get("candidate_after_digest"),
                "candidate_before_digest": raw.get("candidate_before_digest"),
                "command_digest": raw.get("command_digest"),
                "elapsed_ms": raw.get("elapsed_ms"),
                "name": verifier.name,
                "passed": completed.returncode == 0 and raw.get("status") == "passed",
                "requirement_id": requirement_id,
                "status": raw.get("status"),
                "verifier_digest": raw.get("verifier_digest"),
            }
        )
    return outcomes


def invoke(
    command: Sequence[str], cwd: Path, environment: Mapping[str, str], action: str
) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            command,
            cwd=cwd,
            env=environment,
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as error:
        raise RunnerError(f"could not {action}: {error}") from error


def git_clean(workspace: Path) -> bool:
    completed = invoke(
        ["git", "status", "--porcelain"], workspace, os.environ, "inspect source"
    )
    if completed.returncode != 0:
        raise RunnerError("could not inspect planning-only source workspace")
    return not completed.stdout.strip()


def create_private_run_directory(directory: Path, case_id: str) -> Path:
    directory = directory.absolute()
    try:
        if directory.is_symlink():
            raise RunnerError("private artifact directory must not be a symlink")
        directory.mkdir(parents=True, mode=0o700, exist_ok=True)
        if not directory.is_dir():
            raise RunnerError("private artifact path is not a directory")
        directory.chmod(0o700)
        run = Path(tempfile.mkdtemp(prefix=f"{case_id}-native-run-", dir=directory))
        run.chmod(0o700)
        return run
    except OSError as error:
        raise RunnerError(f"could not create private run directory: {error}") from error


def retain_private_artifact(
    directory: Path | None, case_id: str, session_id: str, comparison: Mapping[str, Any]
) -> str | None:
    if directory is None:
        return None
    directory = directory.absolute()
    try:
        if directory.is_symlink():
            raise RunnerError("private artifact directory must not be a symlink")
        directory.mkdir(parents=True, mode=0o700, exist_ok=True)
        if not directory.is_dir():
            raise RunnerError("private artifact path is not a directory")
        directory.chmod(0o700)
    except OSError as error:
        raise RunnerError(
            f"could not protect private artifact directory: {error}"
        ) from error
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            dir=directory,
            prefix=f"{case_id}-native-",
            suffix=".json",
            delete=False,
        ) as output:
            json.dump(
                {"session_id": session_id, "comparison": comparison},
                output,
                sort_keys=True,
            )
    except OSError as error:
        raise RunnerError(f"could not write private artifact: {error}") from error
    return Path(output.name).name


def write_results(
    directory: Path, case_id: str, results: Mapping[str, Mapping[str, Any]]
) -> dict[str, str]:
    paths: dict[str, str] = {}
    for mode, result in results.items():
        path = directory / f"{case_id}-native-{mode}.json"
        path.write_text(json.dumps(result, sort_keys=True) + "\n", encoding="utf-8")
        paths[mode] = path.name
    return paths


def digest_text(value: str) -> str:
    return "sha256:" + hashlib.sha256(value.encode("utf-8")).hexdigest()


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
