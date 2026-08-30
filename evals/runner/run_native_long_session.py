#!/usr/bin/env python3
"""Compare native Codex continuations from one intact, long-context checkpoint."""

from __future__ import annotations

import argparse
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
from pathlib import PurePosixPath
import re
import shutil
import subprocess
import sys
import tempfile
import time
from typing import Any, Mapping, Sequence
import unicodedata

try:
    from .run_baseline import (
        CaseDefinition,
        RunnerError,
        changed_paths_since,
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
        changed_paths_since,
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
MAX_CONTEXT_BYTES = 1024 * 1024
MAX_PLAIN_SUMMARY_BYTES = 64 * 1024
REVIEWER_TIMEOUT_SECONDS = 600
NEUTRAL_CONTINUATION_PROMPT = (
    "Continue the task from this checkpoint. Preserve existing behavior and complete "
    "the remaining work. Do not claim completion without running relevant validation."
)
REVIEWER_SCHEMA_VERSION = 1
REVIEWER_SEVERITIES = frozenset({"Critical", "Required", "Advisory"})
REVIEWER_CATEGORIES = frozenset(
    {"correctness", "regression", "scope", "security", "test_gap"}
)


class ReviewValidationError(RuntimeError):
    """Report a strict reviewer-output failure without exposing raw output."""

    def __init__(self, kind: str) -> None:
        self.kind = kind
        super().__init__(kind)


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
    def __init__(self, codex_bin: str, worker_effort: str | None = None) -> None:
        command = [codex_bin, "app-server", "--stdio"]
        if worker_effort is not None:
            command = [
                codex_bin,
                "-c",
                f"model_reasoning_effort={json.dumps(worker_effort)}",
                "app-server",
                "--stdio",
            ]
        try:
            self.process = subprocess.Popen(
                command,
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
        self.notifications: list[Mapping[str, Any]] = []

    def close(self) -> None:
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait()

    def initialize(self, experimental: bool = False) -> None:
        self.request(
            "initialize",
            {
                "clientInfo": {"name": "driftctl-native-eval", "version": "1"},
                "capabilities": {
                    "experimentalApi": experimental,
                    "requestAttestation": False,
                },
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
                self.notifications.append(response)
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

    def run_control_turn(
        self,
        parent_thread_id: str,
        candidate: Path,
        goal: str,
        summary: str,
        worker_policy: Mapping[str, str],
        expected_source_messages: int,
    ) -> dict[str, Any]:
        fork = self.request(
            "thread/fork",
            {
                "threadId": parent_thread_id,
                "cwd": str(candidate),
                "ephemeral": False,
                "model": worker_policy["model"],
                "sandbox": "workspace-write",
                "approvalPolicy": "never",
            },
        )
        thread = fork.get("thread")
        if not isinstance(thread, Mapping):
            raise RunnerError("plain-summary thread/fork has no child thread")
        child_id = thread.get("id")
        if (
            not isinstance(child_id, str)
            or not child_id
            or child_id == parent_thread_id
            or thread.get("cwd") != str(candidate)
            or thread.get("ephemeral") is not False
        ):
            raise RunnerError("plain-summary thread/fork returned an invalid child")
        self.migrate_child_goal(child_id, goal)
        self.request(
            "thread/settings/update",
            {
                "threadId": child_id,
                "model": worker_policy["model"],
                "effort": worker_policy["effort"],
                "approvalPolicy": "never",
                "sandboxPolicy": {"type": "workspaceWrite"},
            },
        )
        resumed = self.request(
            "thread/resume",
            {
                "threadId": child_id,
                "model": worker_policy["model"],
                "sandbox": "workspace-write",
                "approvalPolicy": "never",
            },
        )
        resumed_thread = resumed.get("thread")
        if (
            not isinstance(resumed_thread, Mapping)
            or resumed_thread.get("id") != child_id
            or resumed_thread.get("cwd") != str(candidate)
            or resumed_thread.get("ephemeral") is not False
            or resumed.get("model") != worker_policy["model"]
            or resumed.get("reasoningEffort") != worker_policy["effort"]
            or resumed.get("approvalPolicy") != "never"
            or not isinstance(resumed.get("sandbox"), Mapping)
            or resumed["sandbox"].get("type") != "workspaceWrite"
        ):
            raise RunnerError("plain-summary child worker policy read-back failed")
        prompt = f"{NEUTRAL_CONTINUATION_PROMPT}\n\nPlain summary:\n{summary}"
        started = self.request(
            "turn/start",
            {
                "threadId": child_id,
                "input": [{"type": "text", "text": prompt}],
                "model": worker_policy["model"],
                "effort": worker_policy["effort"],
                "approvalPolicy": "never",
                "sandboxPolicy": {"type": "workspaceWrite"},
            },
        )
        turn = started.get("turn")
        if not isinstance(turn, Mapping):
            raise RunnerError("plain-summary turn/start has no turn")
        turn_id = turn.get("id")
        status = turn.get("status")
        if not isinstance(turn_id, str) or not turn_id:
            raise RunnerError("plain-summary turn/start has no turn ID")
        if status == "inProgress":
            status = self.wait_for_terminal_turn(child_id, turn_id)
        elif status not in {"completed", "failed", "interrupted"}:
            raise RunnerError("plain-summary turn/start returned an invalid status")
        self.require_user_record_ids(parent_thread_id, expected_source_messages)
        return {
            "child_cwd": str(candidate),
            "child_thread_id": child_id,
            "turn_id": turn_id,
            "turn_status": status,
        }

    def migrate_child_goal(self, child_id: str, goal: str) -> None:
        before = self.request("thread/goal/get", {"threadId": child_id}).get("goal")
        current = before.get("objective") if isinstance(before, Mapping) else None
        if current != goal:
            if current is not None:
                cleared = self.request("thread/goal/clear", {"threadId": child_id})
                if cleared.get("cleared") is not True:
                    raise RunnerError("plain-summary child goal clear was not confirmed")
            changed = self.request(
                "thread/goal/set", {"threadId": child_id, "objective": goal}
            ).get("goal")
            if (
                not isinstance(changed, Mapping)
                or changed.get("threadId") != child_id
                or changed.get("objective") != goal
            ):
                raise RunnerError("plain-summary child goal set was not confirmed")
        after = self.request("thread/goal/get", {"threadId": child_id}).get("goal")
        if (
            not isinstance(after, Mapping)
            or after.get("threadId") != child_id
            or after.get("objective") != goal
        ):
            raise RunnerError("plain-summary child goal read-back failed")

    def wait_for_terminal_turn(self, child_id: str, turn_id: str) -> str:
        for notification in reversed(self.notifications):
            status = terminal_turn_status(notification, child_id, turn_id)
            if status is not None:
                return status
        while True:
            message = self._read()
            status = terminal_turn_status(message, child_id, turn_id)
            if status is not None:
                return status
            if "method" in message and "id" not in message:
                continue
            raise RunnerError("App Server emitted an unexpected message during control turn")

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
            self.require_user_record_ids(thread_id, expected_count)
            try:
                self.request(
                    "turn/interrupt", {"threadId": thread_id, "turnId": turn_id}
                )
            except AppServerRequestError as error:
                if error.reason != "no_active_turn":
                    raise
        elif status not in {"completed", "interrupted"}:
            raise RunnerError(f"App Server {phase} turn ended with {status!r}")
        record_ids = self.require_user_record_ids(thread_id, expected_count)
        return record_ids[-1]

    def require_user_record_ids(self, thread_id: str, expected: int) -> list[str]:
        """Return the exact durable text-record IDs used by the Codex importer."""

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
                message_count = 0
                record_ids: list[str] = []
                malformed = False
                for turn in turns:
                    if not isinstance(turn, Mapping):
                        continue
                    for item in turn.get("items", []):
                        if not isinstance(item, Mapping) or item.get("type") != "userMessage":
                            continue
                        message_count += 1
                        item_id = item.get("id")
                        content = item.get("content")
                        if not isinstance(item_id, str) or not isinstance(content, list):
                            malformed = True
                            continue
                        text_parts = [
                            f"{item_id}:{index}"
                            for index, part in enumerate(content)
                            if isinstance(part, Mapping) and part.get("type") == "text"
                        ]
                        if len(text_parts) != 1:
                            malformed = True
                        record_ids.extend(text_parts)
                if message_count == expected and len(record_ids) == expected and not malformed:
                    return record_ids
                if message_count > expected or len(record_ids) > expected:
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
    parser.add_argument("--plain-summary-file", type=Path)
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
            namespace.plain_summary_file,
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
    plain_summary_file: Path | None = None,
) -> dict[str, Any]:
    if context_bytes < 0 or context_bytes > MAX_CONTEXT_BYTES:
        raise RunnerError(
            f"context bytes must be between 0 and {MAX_CONTEXT_BYTES}"
        )
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
    plain_summary = load_plain_summary(case_directory, plain_summary_file)
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
        review_artifact_root = root / "reviews"
        review_artifact_root.mkdir(mode=0o700)
        environment = os.environ | {
            "DRIFTCTL_CODEX_BIN": codex_bin,
            "TMPDIR": str(temporary_directory),
            "XDG_STATE_HOME": str(state_directory),
        }
        session_id, source_records, injection = seed_native_session(
            codex_bin, workspace, definition, context_bytes, worker_policy
        )
        source_clean = git_clean(workspace)
        if not source_clean:
            raise RunnerError("planning-only source turns changed the source workspace")
        projection_generation, observed_projection = invoke_inspect(
            driftctl_bin, workspace, session_id, environment
        )
        gold_projection, gold_projection_digest = load_gold_projection(case_directory)
        projection_fidelity = score_projection_fidelity(
            observed_projection,
            gold_projection,
            gold_projection_digest,
            source_records,
        )
        require_evaluation_fingerprint(case_directory, fingerprint)
        comparison = invoke_compare(driftctl_bin, workspace, session_id, environment)
        require_comparison_fairness(comparison, worker_policy)
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
                source_records,
                injection,
                source_clean,
                session_id,
                private_artifact,
                worker_policy,
                projection_generation,
                gold_projection,
                codex_bin,
                review_artifact_root,
            )
            for mode in ("baseline", "workflow")
        }
        if plain_summary is not None:
            control_summary, control_digest = plain_summary
            candidate = root / "plain-summary-workspace"
            shutil.copytree(workspace, candidate, symlinks=True)
            initial_commit = git_head(candidate)
            control = run_plain_summary_control(
                codex_bin,
                candidate,
                session_id,
                definition,
                control_summary,
                worker_policy,
                len(source_records),
            )
            control["changed_paths"] = changed_paths_since(candidate, initial_commit)
            results["plain_summary"] = arm_result(
                definition,
                "plain_summary",
                control,
                driftctl_bin,
                case_directory,
                fingerprint,
                environment,
                source_records,
                injection,
                source_clean,
                session_id,
                private_artifact,
                worker_policy,
                projection_generation,
                gold_projection,
                codex_bin,
                review_artifact_root,
            )
            results["plain_summary"]["control_context"] = {
                "kind": "flat_plain_summary",
                "sha256": control_digest,
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
        "projection_fidelity": projection_fidelity,
        "projection_generation": projection_generation,
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
        source_records = [initial]
        for index, steering in enumerate(definition.steering, start=1):
            source_records.append(
                server.record_user_turn(
                    thread_id,
                    steering_source_prompt(steering.requirement, index),
                    f"steering-{index}",
                    index + 1,
                )
            )
        injection = inject_non_authoritative_context(server, thread_id, context_bytes)
        return thread_id, source_records, injection
    finally:
        server.close()


def load_plain_summary(
    case_directory: Path, summary_file: Path | None
) -> tuple[str, str] | None:
    if summary_file is None:
        return None
    resolved = summary_file.resolve()
    try:
        resolved.relative_to(case_directory)
    except ValueError as error:
        raise RunnerError("plain summary must be inside the frozen case directory") from error
    try:
        data = resolved.read_bytes()
    except OSError as error:
        raise RunnerError(f"could not read plain summary: {error}") from error
    if not data or len(data) > MAX_PLAIN_SUMMARY_BYTES or b"\0" in data:
        raise RunnerError("plain summary must be nonempty, bounded UTF-8 text")
    try:
        summary = data.decode("utf-8")
    except UnicodeDecodeError as error:
        raise RunnerError("plain summary must be nonempty, bounded UTF-8 text") from error
    if not summary.strip():
        raise RunnerError("plain summary must be nonempty, bounded UTF-8 text")
    return summary, "sha256:" + hashlib.sha256(data).hexdigest()


def run_plain_summary_control(
    codex_bin: str,
    candidate: Path,
    parent_thread_id: str,
    definition: CaseDefinition,
    summary: str,
    worker_policy: Mapping[str, str],
    expected_source_messages: int,
) -> dict[str, Any]:
    server = AppServer(codex_bin, worker_policy["effort"])
    try:
        server.initialize(experimental=True)
        return server.run_control_turn(
            parent_thread_id,
            candidate,
            definition.goal,
            summary,
            worker_policy,
            expected_source_messages,
        )
    finally:
        server.close()


def git_head(workspace: Path) -> str:
    completed = invoke(
        ["git", "rev-parse", "HEAD"], workspace, os.environ, "read candidate checkpoint"
    )
    if completed.returncode != 0 or not completed.stdout.strip():
        raise RunnerError("plain-summary candidate has no frozen Git checkpoint")
    return completed.stdout.strip()


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


def terminal_turn_status(
    notification: Mapping[str, Any], child_id: str, turn_id: str
) -> str | None:
    if notification.get("method") != "turn/completed":
        return None
    params = notification.get("params")
    if not isinstance(params, Mapping) or params.get("threadId") != child_id:
        return None
    turn = params.get("turn")
    if not isinstance(turn, Mapping) or turn.get("id") != turn_id:
        return None
    status = turn.get("status")
    return status if status in {"completed", "failed", "interrupted"} else None


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


def invoke_inspect(
    driftctl_bin: str,
    workspace: Path,
    session_id: str,
    environment: Mapping[str, str],
) -> tuple[dict[str, Any], Mapping[str, Any]]:
    completed = invoke(
        [driftctl_bin, "inspect", "codex", "--session", session_id, "--json"],
        workspace,
        environment,
        "generate native projection",
    )
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise RunnerError(f"native inspect failed: {detail or 'command failed'}")
    try:
        document = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise RunnerError(f"native inspect emitted invalid JSON: {error}") from error
    resolver = document.get("resolver") if isinstance(document, Mapping) else None
    projection = document.get("projection") if isinstance(document, Mapping) else None
    usage = resolver.get("usage") if isinstance(resolver, Mapping) else None
    if (
        not isinstance(resolver, Mapping)
        or not isinstance(usage, Mapping)
        or not isinstance(projection, Mapping)
        or document.get("status") != "usable"
    ):
        raise RunnerError("native inspect did not return usable resolver evidence")
    fields = {
        "calls": resolver.get("calls"),
        "elapsed_ms": resolver.get("elapsed_ms"),
        "effort": resolver.get("reasoning"),
        "input_tokens": usage.get("input_tokens"),
        "cached_input_tokens": usage.get("cached_input_tokens", 0),
        "model": resolver.get("model"),
        "output_tokens": usage.get("output_tokens"),
        "reasoning_output_tokens": usage.get("reasoning_output_tokens"),
    }
    numeric = (
        "calls",
        "elapsed_ms",
        "input_tokens",
        "cached_input_tokens",
        "output_tokens",
        "reasoning_output_tokens",
    )
    if any(
        isinstance(fields[name], bool)
        or not isinstance(fields[name], int)
        or fields[name] < 0
        for name in numeric
    ) or any(
        not isinstance(fields[name], str) or not fields[name]
        for name in ("effort", "model")
    ):
        raise RunnerError("native inspect returned malformed resolver evidence")
    return fields, projection


def load_gold_projection(case_directory: Path) -> tuple[Mapping[str, Any], str]:
    path = case_directory / "calibration" / "gold" / "active_projection.json"
    try:
        data = path.read_bytes()
        projection = json.loads(data)
    except (OSError, json.JSONDecodeError) as error:
        raise RunnerError(f"could not read frozen gold projection: {error}") from error
    if not isinstance(projection, Mapping):
        raise RunnerError("frozen gold projection must contain an object")
    return projection, "sha256:" + hashlib.sha256(data).hexdigest()


def normalize_fidelity_text(value: Any) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError("fidelity text must be nonempty")
    return unicodedata.normalize("NFC", value.replace("\r\n", "\n")).strip()


def score_projection_fidelity(
    observed: Mapping[str, Any],
    gold: Mapping[str, Any],
    gold_digest: str,
    native_source_records: Sequence[str],
) -> dict[str, Any]:
    """Compare lifecycle and native provenance without requiring verbatim clauses."""

    base = {
        "schema_version": 1,
        "method": "structural_provenance_fidelity_v2",
        "gold_projection_sha256": gold_digest,
        "inspect_projection_schema_version": observed.get("schema_version"),
    }
    try:
        if observed.get("schema_version") != 1 or gold.get("schema_version") != 1:
            raise ValueError("unsupported projection schema")
        if gold.get("allow_additional_active") is not False:
            raise ValueError("gold projection is not closed-world")
        if gold.get("source_namespace") != {
            "comparison": "non_identity",
            "name": "fixture_logical_v1",
        }:
            raise ValueError("gold source namespace is invalid")
        if (
            not native_source_records
            or any(not isinstance(record, str) or not record for record in native_source_records)
            or len(set(native_source_records)) != len(native_source_records)
        ):
            raise ValueError("native source record sequence is malformed")
        gold_goal = gold.get("goal")
        observed_goal = observed.get("goal")
        if not isinstance(gold_goal, Mapping) or not isinstance(observed_goal, Mapping):
            raise ValueError("projection goal is malformed")
        gold_goal_sources = gold_goal.get("source_record_ids")
        native_goal_sources = observed_goal.get("source_record_ids")
        if not isinstance(gold_goal_sources, list) or not gold_goal_sources:
            raise ValueError("gold goal provenance is missing")
        if not isinstance(native_goal_sources, list) or any(
            not isinstance(source, str) or not source for source in native_goal_sources
        ):
            raise ValueError("native goal provenance is malformed")
        expected_goal_sources = logical_source_records(
            gold_goal_sources, native_source_records
        )
        goal_exact = normalize_fidelity_text(observed_goal.get("text")) == (
            normalize_fidelity_text(gold_goal.get("text"))
        )
        active_items: list[tuple[str, Mapping[str, Any]]] = []
        for bucket in ("preserve", "frontier", "validation"):
            items = observed.get(bucket)
            if not isinstance(items, list):
                raise ValueError(f"native projection {bucket} is malformed")
            for item in items:
                if not isinstance(item, Mapping):
                    raise ValueError(f"native projection {bucket} item is malformed")
                normalize_fidelity_text(item.get("text"))
                sources = item.get("source_record_ids")
                if (
                    not isinstance(sources, list)
                    or not sources
                    or any(not isinstance(source, str) or not source for source in sources)
                    or len(set(sources)) != len(sources)
                ):
                    raise ValueError("native item provenance is malformed")
                if not isinstance(item.get("kind"), str) or not item.get("kind"):
                    raise ValueError("native item kind is malformed")
                active_items.append((bucket, item))
        gold_requirements = gold.get("requirements")
        inactive_requirements = gold.get("inactive_requirements")
        if not isinstance(gold_requirements, list) or not gold_requirements:
            raise ValueError("gold active requirements are malformed")
        if not isinstance(inactive_requirements, list):
            raise ValueError("gold inactive requirements are malformed")
        expected: list[dict[str, Any]] = []
        for requirement in gold_requirements:
            if not isinstance(requirement, Mapping):
                raise ValueError("gold requirement is malformed")
            requirement_id = requirement.get("id")
            sources = requirement.get("source_record_ids")
            if (
                not isinstance(requirement_id, str)
                or not requirement_id
                or not isinstance(sources, list)
                or not sources
            ):
                raise ValueError("gold requirement identity or provenance is malformed")
            evidence = requirement.get("evidence")
            if not isinstance(evidence, Mapping):
                raise ValueError("gold requirement evidence is malformed")
            expected.append(
                {
                    "id": requirement_id,
                    "text": normalize_fidelity_text(requirement.get("text")),
                    "sources": logical_source_records(sources, native_source_records),
                    "scope": evidence.get("kind") == "mutation_scope",
                }
            )
        matches = []
        missing = []
        duplicates = []
        matched_indices: set[int] = set()
        text_exact_count = 0
        for requirement in expected:
            source_peers_include_scope = any(
                peer["scope"] and peer["sources"] == requirement["sources"]
                for peer in expected
            )
            candidates = []
            for index, (bucket, item) in enumerate(active_items):
                if index in matched_indices:
                    continue
                if tuple(item["source_record_ids"]) != requirement["sources"]:
                    continue
                if requirement["scope"] and item["kind"] != "scope":
                    continue
                if (
                    not requirement["scope"]
                    and source_peers_include_scope
                    and item["kind"] == "scope"
                ):
                    continue
                candidates.append((index, bucket, item))
            requirement_id = requirement["id"]
            if not candidates:
                missing.append(requirement_id)
                continue
            if len(candidates) != 1:
                duplicates.append(requirement_id)
                continue
            index, bucket, item = candidates[0]
            matched_indices.add(index)
            sources = item["source_record_ids"]
            text_exact = normalize_fidelity_text(item.get("text")) == requirement["text"]
            text_exact_count += int(text_exact)
            matches.append(
                {
                    "gold_requirement_id": requirement_id,
                    "native_item_id": item.get("id"),
                    "bucket": bucket,
                    "native_source_record_count": len(sources),
                    "text_exact": text_exact,
                    "native_provenance_nonempty": True,
                }
            )
        unexpected = [
            item.get("id")
            for index, (_, item) in enumerate(active_items)
            if index not in matched_indices
        ]
        inactive_sources = {
            logical_source_records(
                requirement.get("source_record_ids"), native_source_records
            )
            for requirement in inactive_requirements
            if isinstance(requirement, Mapping)
        }
        if len(inactive_sources) != len(inactive_requirements):
            raise ValueError("gold inactive requirements are malformed")
        leaked = [
            item.get("id")
            for _, item in active_items
            if tuple(item["source_record_ids"]) in inactive_sources
        ]
        native_goal_provenance = tuple(native_goal_sources) == expected_goal_sources
        matched_count = len(matches)
        overall = (
            goal_exact
            and native_goal_provenance
            and matched_count == len(gold_requirements)
            and not missing
            and not duplicates
            and not unexpected
            and not leaked
        )
        return base | {
            "status": "passed" if overall else "failed",
            "goal": {
                "text_exact": goal_exact,
                "gold_source_label_count": len(gold_goal_sources),
                "native_source_record_count": len(native_goal_sources),
                "native_provenance_exact": native_goal_provenance,
            },
            "requirements": {
                "expected_count": len(gold_requirements),
                "matched_count": matched_count,
                "provenance_complete_count": matched_count,
                "text_exact_count": text_exact_count,
                "missing_gold_requirement_ids": missing,
                "unexpected_active_items": unexpected,
                "duplicate_matches": duplicates,
                "matches": matches,
            },
            "inactive_requirements": {
                "expected_count": len(inactive_requirements),
                "leaked_active_items": leaked,
                "passed": not leaked,
            },
            "overall_pass": overall,
        }
    except (KeyError, TypeError, ValueError) as error:
        return base | {
            "status": "invalid",
            "reason": str(error),
            "overall_pass": False,
        }


def logical_source_records(
    labels: Any, native_source_records: Sequence[str]
) -> tuple[str, ...]:
    """Map fixture-only provenance labels onto ordered native record IDs."""

    if not isinstance(labels, list) or not labels:
        raise ValueError("gold source labels are malformed")
    mapped = []
    for label in labels:
        if not isinstance(label, str):
            raise ValueError("gold source label is malformed")
        if label.startswith("initial."):
            index = 0
        else:
            match = re.fullmatch(r"steering\.([1-9][0-9]*)", label)
            if match is None:
                raise ValueError("gold source label is unsupported")
            index = int(match.group(1))
        if index >= len(native_source_records):
            raise ValueError("gold source label has no native record")
        record = native_source_records[index]
        if record not in mapped:
            mapped.append(record)
    return tuple(mapped)


def require_comparison_fairness(
    comparison: Mapping[str, Any], worker_policy: Mapping[str, str]
) -> None:
    fairness = comparison.get("fairness")
    if not isinstance(fairness, Mapping):
        raise RunnerError("native comparison did not report fairness evidence")
    if fairness.get("starting_manifest_equal") is not True:
        raise RunnerError("native comparison starting manifests are not equal")
    if fairness.get("neutral_prompt_equal") is not True:
        raise RunnerError("native comparison did not use an equal neutral prompt")
    if comparison.get("parent_unchanged") is not True:
        raise RunnerError("native comparison did not preserve the parent session")
    if comparison.get("source_unchanged") is not True:
        raise RunnerError("native comparison did not preserve the source workspace")
    observed_policy = fairness.get("worker_policy")
    expected_policy = dict(worker_policy) | {"verified_readback": True}
    if observed_policy != expected_policy:
        raise RunnerError("native comparison worker policy does not match the frozen policy")
    if fairness.get("only_intended_input_difference") != (
        "workflow receives the bounded active-intent projection"
    ):
        raise RunnerError("native comparison reported an unexpected arm difference")


def arm_result(
    definition: CaseDefinition,
    mode: str,
    arm: Mapping[str, Any],
    driftctl_bin: str,
    case_directory: Path,
    fingerprint: str,
    base_environment: Mapping[str, str],
    source_records: Sequence[str],
    injection: Mapping[str, Any],
    source_clean: bool,
    source_session_id: str,
    private_artifact: str | None,
    worker_policy: Mapping[str, str],
    projection_generation: Mapping[str, Any],
    gold_projection: Mapping[str, Any],
    codex_bin: str,
    review_artifact_root: Path,
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
    requirement_evidence = map_requirement_evidence(
        gold_projection, verifiers, scope
    )
    passed_requirements = sum(item["passed"] is True for item in requirement_evidence)
    requirement_pass_rate = passed_requirements / len(requirement_evidence)
    review = review_candidate(
        codex_bin,
        candidate,
        definition,
        gold_projection,
        changed_paths,
        worker_policy,
        base_environment,
        review_artifact_root,
    )
    verified = (
        agent_succeeded
        and all(item["passed"] for item in verifiers)
        and scope["passed"]
        and review["review_passed"] is True
    )
    result: dict[str, Any] = {
        "agent_succeeded": agent_succeeded,
        "case_id": definition.case_id,
        "changed_paths": changed_paths,
        "evaluation_kind": LONG_SESSION_LABEL,
        "mode": mode,
        "native_checkpoint": {
            "injection": dict(injection),
            "source_user_turn_count": len(source_records),
            "source_turn_labels": [
                "initial",
                *(f"steering-{index}" for index in range(1, len(source_records))),
            ],
            "source_workspace_clean": source_clean,
        },
        "premature_completion": {
            "available": agent_succeeded,
            "detected": agent_succeeded
            and not all(item["passed"] for item in verifiers),
            "phase": "continuation" if agent_succeeded else None,
        },
        "projection_generation": dict(projection_generation),
        "requirement_evidence": requirement_evidence,
        "requirement_pass_rate": requirement_pass_rate,
        "review": review,
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


def review_candidate(
    codex_bin: str,
    candidate: Path,
    definition: CaseDefinition,
    gold_projection: Mapping[str, Any],
    changed_paths: Any,
    worker_policy: Mapping[str, str],
    base_environment: Mapping[str, str],
    artifact_root: Path,
) -> dict[str, Any]:
    """Run one blind read-only acceptance review and retain only private raw output."""

    try:
        requirements = review_requirements(gold_projection)
        prompt = review_prompt(definition, requirements, changed_paths)
        before = candidate_tree_digest(candidate)
    except (OSError, ReviewValidationError):
        return invalid_review("schema", worker_policy, None, None, False)

    environment = dict(base_environment)
    environment.pop("DRIFTCTL_EVAL_CASE_DIR", None)
    environment.pop("PYTHONPATH", None)
    command = [
        codex_bin,
        "-c",
        f"model_reasoning_effort={json.dumps(worker_policy['effort'])}",
        "-c",
        'approval_policy="never"',
        "exec",
        "--json",
        "--model",
        worker_policy["model"],
        "--sandbox",
        "read-only",
        "--ephemeral",
        prompt,
    ]
    try:
        completed = subprocess.run(
            command,
            cwd=candidate,
            env=environment,
            capture_output=True,
            text=True,
            check=False,
            timeout=REVIEWER_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as error:
        raw_retained = retain_private_review_artifact(
            artifact_root,
            timeout_stream(error.stdout),
            timeout_stream(error.stderr),
        )
        try:
            after = candidate_tree_digest(candidate)
        except OSError:
            return invalid_review(
                "candidate_digest", worker_policy, before, None, raw_retained
            )
        if before != after:
            return invalid_review(
                "candidate_mutated", worker_policy, before, after, raw_retained
            )
        return invalid_review("timeout", worker_policy, before, after, raw_retained)
    except OSError:
        return invalid_review("launch", worker_policy, before, before, False)

    raw_retained = retain_private_review_artifact(
        artifact_root, completed.stdout, completed.stderr
    )
    try:
        after = candidate_tree_digest(candidate)
    except OSError:
        return invalid_review("candidate_digest", worker_policy, before, None, raw_retained)
    if before != after:
        return invalid_review("candidate_mutated", worker_policy, before, after, raw_retained)
    if not raw_retained:
        return invalid_review("private_artifact", worker_policy, before, after, False)
    if completed.returncode != 0:
        return invalid_review("provider", worker_policy, before, after, raw_retained)
    try:
        review = validate_review_response(
            review_response_from_jsonl(completed.stdout), requirements, candidate
        )
    except ReviewValidationError as error:
        return invalid_review(error.kind, worker_policy, before, after, raw_retained)
    return {
        "schema_version": REVIEWER_SCHEMA_VERSION,
        "status": "completed",
        "review_passed": review["verdict"] == "pass",
        "attempt": 1,
        "reviewer_policy": {
            "model": worker_policy["model"],
            "effort": worker_policy["effort"],
            "sandbox": "read-only",
            "approval_policy": "never",
        },
        "prompt_sha256": digest_text(prompt),
        "requirements_sha256": digest_text(json.dumps(requirements, sort_keys=True)),
        "candidate_before_digest": before,
        "candidate_after_digest": after,
        "candidate_unchanged": True,
        "coverage_complete": True,
        "critical_count": review["critical_count"],
        "required_count": review["required_count"],
        "advisory_count": review["advisory_count"],
        "findings": review["findings"],
        "raw_private_retained": raw_retained,
    }


def review_requirements(gold_projection: Mapping[str, Any]) -> list[dict[str, str]]:
    requirements = gold_projection.get("requirements")
    if not isinstance(requirements, list) or not requirements:
        raise ReviewValidationError("schema")
    normalized: list[dict[str, str]] = []
    identifiers: set[str] = set()
    for requirement in requirements:
        if not isinstance(requirement, Mapping):
            raise ReviewValidationError("schema")
        identifier, text = requirement.get("id"), requirement.get("text")
        if not isinstance(identifier, str) or not identifier or not isinstance(text, str) or not text:
            raise ReviewValidationError("schema")
        if identifier in identifiers:
            raise ReviewValidationError("schema")
        identifiers.add(identifier)
        normalized.append({"id": identifier, "text": text})
    return normalized


def review_prompt(
    definition: CaseDefinition, requirements: Sequence[Mapping[str, str]], changed_paths: Any
) -> str:
    if not isinstance(changed_paths, list) or any(not isinstance(path, str) for path in changed_paths):
        raise ReviewValidationError("schema")
    declared = "\n".join(
        f"- [{requirement['id']}] {requirement['text']}" for requirement in requirements
    )
    allowed = "\n".join(f"- {path}" for path in definition.allowed_changed_paths)
    changed = "\n".join(f"- {path}" for path in changed_paths) or "- (none)"
    return (
        "Independently review the current candidate in this working directory. Do not edit "
        "files, run no commands that change state, and assess only the declared requirements. "
        "Do not infer unstated tests or requirements. Return exactly one JSON object, without "
        "Markdown, with schema_version=1, verdict=pass|fail, complete coverage entries, and "
        "findings. Each finding must have id F-NNN, severity Critical|Required|Advisory, "
        "requirement_ids, category correctness|regression|scope|security|test_gap, a portable "
        "candidate-relative path, line, and concise rationale.\n\n"
        "Coverage must be an array with exactly one object per declared requirement: "
        '{"requirement_id":"declared-id","assessed":true}. '
        "The root object and every coverage/finding object must contain only the stated "
        "fields.\n\n"
        f"Goal:\n{definition.goal}\n\nDeclared requirements:\n{declared}\n\n"
        f"Allowed changed paths:\n{allowed}\n\nObserved changed paths:\n{changed}\n"
    )


def review_response_from_jsonl(stdout: str) -> Mapping[str, Any]:
    final_text: str | None = None
    for line in stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError as error:
            raise ReviewValidationError("schema") from error
        item = event.get("item") if isinstance(event, Mapping) else None
        if (
            isinstance(item, Mapping)
            and item.get("type") == "agent_message"
            and isinstance(item.get("text"), str)
        ):
            final_text = item["text"]
    if final_text is None:
        raise ReviewValidationError("schema")
    try:
        value = json.loads(final_text)
    except json.JSONDecodeError as error:
        raise ReviewValidationError("schema") from error
    if not isinstance(value, Mapping):
        raise ReviewValidationError("schema")
    return value


def validate_review_response(
    review: Mapping[str, Any], requirements: Sequence[Mapping[str, str]], candidate: Path
) -> dict[str, Any]:
    if set(review) != {"schema_version", "verdict", "coverage", "findings"}:
        raise ReviewValidationError("schema")
    if review.get("schema_version") != REVIEWER_SCHEMA_VERSION:
        raise ReviewValidationError("schema")
    known = {requirement["id"] for requirement in requirements}
    coverage = review.get("coverage")
    if not isinstance(coverage, list):
        raise ReviewValidationError("coverage")
    covered = [
        item.get("requirement_id")
        for item in coverage
        if isinstance(item, Mapping)
        and set(item) == {"requirement_id", "assessed"}
        and item.get("assessed") is True
    ]
    if len(covered) != len(coverage) or set(covered) != known or len(set(covered)) != len(covered):
        raise ReviewValidationError("coverage")
    findings = review.get("findings")
    if not isinstance(findings, list):
        raise ReviewValidationError("schema")
    sanitized = []
    finding_ids: set[str] = set()
    for finding in findings:
        if not isinstance(finding, Mapping) or set(finding) != {
            "id", "severity", "requirement_ids", "category", "path", "line", "rationale"
        }:
            raise ReviewValidationError("schema")
        identifier = finding["id"]
        severity = finding["severity"]
        category = finding["category"]
        requirement_ids = finding["requirement_ids"]
        path = finding["path"]
        line = finding["line"]
        rationale = finding["rationale"]
        if (
            not isinstance(identifier, str)
            or re.fullmatch(r"F-[0-9]{3}", identifier) is None
            or identifier in finding_ids
            or severity not in REVIEWER_SEVERITIES
            or category not in REVIEWER_CATEGORIES
            or not isinstance(requirement_ids, list)
            or not requirement_ids
            or any(not isinstance(value, str) or value not in known for value in requirement_ids)
            or len(set(requirement_ids)) != len(requirement_ids)
            or not isinstance(line, int)
            or isinstance(line, bool)
            or line < 1
            or not isinstance(rationale, str)
            or not rationale.strip()
            or len(rationale) > 2000
            or not portable_review_path(path, candidate)
        ):
            raise ReviewValidationError("schema")
        finding_ids.add(identifier)
        sanitized.append({
            "id": identifier,
            "severity": severity,
            "requirement_ids": requirement_ids,
            "category": category,
            "path": path,
            "line": line,
            "finding_digest": digest_text(rationale),
        })
    critical = sum(item["severity"] == "Critical" for item in sanitized)
    required = sum(item["severity"] == "Required" for item in sanitized)
    verdict = "pass" if critical == 0 and required == 0 else "fail"
    if review.get("verdict") != verdict:
        raise ReviewValidationError("schema")
    return {
        "verdict": verdict,
        "critical_count": critical,
        "required_count": required,
        "advisory_count": sum(item["severity"] == "Advisory" for item in sanitized),
        "findings": sanitized,
    }


def portable_review_path(value: Any, candidate: Path) -> bool:
    if not isinstance(value, str) or not value or "\\" in value:
        return False
    path = PurePosixPath(value)
    if path.is_absolute() or not path.parts or any(part in {"", ".", ".."} for part in path.parts):
        return False
    return (candidate.joinpath(*path.parts)).is_file()


def candidate_tree_digest(candidate: Path) -> str:
    digest = hashlib.sha256()
    for path in sorted(candidate.rglob("*")):
        relative = path.relative_to(candidate)
        if ".git" in relative.parts:
            continue
        name = relative.as_posix().encode("utf-8")
        if path.is_symlink():
            payload = os.readlink(path).encode("utf-8")
            marker = b"L"
        elif path.is_file():
            payload = path.read_bytes()
            marker = b"F"
        elif path.is_dir():
            payload = b""
            marker = b"D"
        else:
            raise OSError("unsupported candidate entry")
        digest.update(marker)
        digest.update(len(name).to_bytes(8, "big"))
        digest.update(name)
        digest.update(len(payload).to_bytes(8, "big"))
        digest.update(payload)
    return "sha256:" + digest.hexdigest()


def retain_private_review_artifact(root: Path, stdout: str, stderr: str) -> bool:
    try:
        root.mkdir(mode=0o700, exist_ok=True)
        root.chmod(0o700)
        with tempfile.NamedTemporaryFile(
            mode="w", encoding="utf-8", dir=root, prefix="review-", suffix=".jsonl", delete=False
        ) as output:
            output.write(stdout)
            if stderr:
                output.write("\n")
                output.write(stderr)
        Path(output.name).chmod(0o600)
        return True
    except OSError:
        return False


def timeout_stream(value: str | bytes | None) -> str:
    """Normalize partial subprocess output for private retention only."""

    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return value


def invalid_review(
    kind: str,
    worker_policy: Mapping[str, str],
    before: str | None,
    after: str | None,
    raw_retained: bool,
) -> dict[str, Any]:
    return {
        "schema_version": REVIEWER_SCHEMA_VERSION,
        "status": "invalid",
        "review_passed": False,
        "attempt": 1,
        "reviewer_policy": {
            "model": worker_policy["model"],
            "effort": worker_policy["effort"],
            "sandbox": "read-only",
            "approval_policy": "never",
        },
        "failure_kind": kind,
        "candidate_before_digest": before,
        "candidate_after_digest": after,
        "candidate_unchanged": before is not None and before == after,
        "coverage_complete": False,
        "critical_count": None,
        "required_count": None,
        "advisory_count": None,
        "findings": [],
        "raw_private_retained": raw_retained,
    }


def map_requirement_evidence(
    gold_projection: Mapping[str, Any],
    verifiers: Sequence[Mapping[str, Any]],
    scope: Mapping[str, Any],
) -> list[dict[str, Any]]:
    requirements = gold_projection.get("requirements")
    if not isinstance(requirements, list) or not requirements:
        raise RunnerError("gold projection has no requirement evidence map")
    verifiers_by_name = {
        outcome.get("name"): outcome
        for outcome in verifiers
        if isinstance(outcome.get("name"), str)
    }
    records = []
    for requirement in requirements:
        if not isinstance(requirement, Mapping):
            raise RunnerError("gold requirement evidence map is malformed")
        requirement_id = requirement.get("id")
        evidence = requirement.get("evidence")
        if not isinstance(requirement_id, str) or not isinstance(evidence, Mapping):
            raise RunnerError("gold requirement evidence map is malformed")
        kind = evidence.get("kind")
        if kind == "external_verifier":
            verifier_name = evidence.get("verifier_name")
            outcome = verifiers_by_name.get(verifier_name)
            if not isinstance(verifier_name, str) or outcome is None:
                raise RunnerError(
                    f"gold requirement {requirement_id} names a missing verifier"
                )
            records.append(
                {
                    "requirement_id": requirement_id,
                    "gold_requirement_id": requirement_id,
                    "evidence_kind": kind,
                    "verifier_name": verifier_name,
                    "passed": outcome.get("passed") is True,
                    "artifact_id": outcome.get("artifact_id"),
                    "command_digest": outcome.get("command_digest"),
                    "verifier_digest": outcome.get("verifier_digest"),
                    "candidate_before_digest": outcome.get(
                        "candidate_before_digest"
                    ),
                    "candidate_after_digest": outcome.get("candidate_after_digest"),
                }
            )
        elif kind == "mutation_scope":
            records.append(
                {
                    "requirement_id": requirement_id,
                    "gold_requirement_id": requirement_id,
                    "evidence_kind": kind,
                    "verifier_name": None,
                    "passed": scope.get("passed") is True,
                    "artifact_id": None,
                    "allowed_changed_paths": scope.get("allowed_changed_paths"),
                    "unexpected_changed_paths": scope.get(
                        "unexpected_changed_paths"
                    ),
                }
            )
        else:
            raise RunnerError(
                f"gold requirement {requirement_id} has an invalid evidence kind"
            )
    return records


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
