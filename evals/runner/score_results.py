#!/usr/bin/env python3
"""Score baseline and workflow result files with one deterministic rubric."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
import math
from pathlib import Path
import sys
from typing import Any, Mapping, Sequence


MODES = frozenset({"baseline", "plain_summary", "workflow"})
SUCCESS_STATUSES = frozenset({"completed", "success", "succeeded", "ok", "verified"})


class ScoreError(RuntimeError):
    """Report malformed or contradictory evaluation result input."""


@dataclass(frozen=True)
class ParsedResult:
    """Evaluation facts normalized from one runner result."""

    mode: str
    case_id: str
    verified_completion: bool
    agent_succeeded: bool
    all_verifiers_passed: bool
    scope_passed: bool
    premature_completion: bool
    elapsed_seconds: float
    input_tokens: int
    cached_input_tokens: int
    output_tokens: int


def main(arguments: Sequence[str]) -> int:
    """Parse result paths, score them, and print exactly one JSON value."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", nargs="+", type=Path, help="runner result JSON files")
    namespace = parser.parse_args(arguments)

    try:
        result = score_files(namespace.results)
    except ScoreError as error:
        print(json.dumps({"error": str(error), "status": "score_error"}, sort_keys=True))
        return 1

    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0


def score_files(paths: Sequence[Path]) -> dict[str, Any]:
    """Load and aggregate one result object per path."""

    if not paths:
        raise ScoreError("at least one result file is required")

    parsed: list[ParsedResult] = []
    seen: set[tuple[str, str]] = set()
    for path in paths:
        result = parse_result(path)
        identity = (result.mode, result.case_id)
        if identity in seen:
            raise ScoreError(
                f"duplicate result for mode {result.mode!r}, case {result.case_id!r}"
            )
        seen.add(identity)
        parsed.append(result)

    parsed.sort(key=lambda item: (item.mode, item.case_id))
    return {
        "by_mode": aggregate_by_mode(parsed),
        "cases": [case_outcome(item) for item in parsed],
        "primary_metric": "verified_completion_rate",
    }


def parse_result(path: Path) -> ParsedResult:
    """Read and validate one runner result JSON object."""

    try:
        raw_value = json.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        raise ScoreError(f"could not read result file {path}: {error}") from error
    except json.JSONDecodeError as error:
        raise ScoreError(f"invalid JSON in result file {path}: {error}") from error
    if not isinstance(raw_value, Mapping):
        raise ScoreError(f"result file {path} must contain a JSON object")
    raw: Mapping[str, Any] = raw_value

    mode = result_mode(raw)
    case_id = required_string(raw, "case_id", path)
    elapsed_seconds = nonnegative_number(raw, "elapsed_seconds", path)
    agent_succeeded = agent_success(raw, path)
    all_verifiers_passed = verifier_success(raw, path)
    scope_passed = scope_success(raw, path)
    explicit_completion = explicit_verified_completion(raw, mode, path)
    verified_completion = (
        agent_succeeded
        and all_verifiers_passed
        and scope_passed
        and (explicit_completion if explicit_completion is not None else True)
    )
    premature = premature_completion(raw, path)
    input_tokens, cached_input_tokens, output_tokens = token_usage(raw, path)
    return ParsedResult(
        mode=mode,
        case_id=case_id,
        verified_completion=verified_completion,
        agent_succeeded=agent_succeeded,
        all_verifiers_passed=all_verifiers_passed,
        scope_passed=scope_passed,
        premature_completion=premature,
        elapsed_seconds=elapsed_seconds,
        input_tokens=input_tokens,
        cached_input_tokens=cached_input_tokens,
        output_tokens=output_tokens,
    )


def result_mode(raw: Mapping[str, Any]) -> str:
    """Read a mode, defaulting legacy runner output to the baseline mode."""

    value = raw.get("mode", raw.get("runner", "baseline"))
    if not isinstance(value, str) or value not in MODES:
        raise ScoreError(
            "result field 'mode' must be 'baseline', 'plain_summary', or 'workflow'"
        )
    return value


def required_string(raw: Mapping[str, Any], field: str, path: Path) -> str:
    """Read a nonempty string field."""

    value = raw.get(field)
    if not isinstance(value, str) or not value.strip():
        raise ScoreError(f"result file {path} field {field!r} must be a nonempty string")
    return value


def nonnegative_number(raw: Mapping[str, Any], field: str, path: Path) -> float:
    """Read a finite nonnegative numeric field."""

    value = raw.get(field)
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ScoreError(f"result file {path} field {field!r} must be a number")
    number = float(value)
    if not math.isfinite(number) or number < 0:
        raise ScoreError(f"result file {path} field {field!r} must be finite and nonnegative")
    return number


def agent_success(raw: Mapping[str, Any], path: Path) -> bool:
    """Determine whether every agent process used by the runner succeeded."""

    for field in ("agent_succeeded", "agent_success"):
        if field in raw:
            value = raw[field]
            if not isinstance(value, bool):
                raise ScoreError(f"result file {path} field {field!r} must be boolean")
            return value

    status = raw.get("status")
    if isinstance(status, str):
        return status in SUCCESS_STATUSES
    if status is not None:
        raise ScoreError(f"result file {path} field 'status' must be a string")

    turns = raw.get("turns")
    if isinstance(turns, list) and turns:
        for turn in turns:
            if not isinstance(turn, Mapping) or not isinstance(turn.get("exit_code"), int):
                raise ScoreError(f"result file {path} has malformed turn results")
        return all(turn["exit_code"] == 0 for turn in turns)
    raise ScoreError(f"result file {path} lacks an agent process outcome")


def verifier_success(raw: Mapping[str, Any], path: Path) -> bool:
    """Require a nonempty final verifier list and return its aggregate result."""

    verifiers = raw.get("verifiers")
    if not isinstance(verifiers, list) or not verifiers:
        raise ScoreError(f"result file {path} field 'verifiers' must be a nonempty array")
    passed: list[bool] = []
    for verifier in verifiers:
        if not isinstance(verifier, Mapping) or not isinstance(verifier.get("passed"), bool):
            raise ScoreError(f"result file {path} has malformed verifier results")
        passed.append(verifier["passed"])
    return all(passed)


def scope_success(raw: Mapping[str, Any], path: Path) -> bool:
    """Require the runner's declared mutation-scope result."""

    scope = raw.get("scope")
    if not isinstance(scope, Mapping):
        raise ScoreError(f"result file {path} field 'scope' must be an object")
    passed = scope.get("passed")
    if not isinstance(passed, bool):
        raise ScoreError(f"result file {path} field 'scope.passed' must be boolean")
    return passed


def explicit_verified_completion(
    raw: Mapping[str, Any], mode: str, path: Path
) -> bool | None:
    """Read optional explicit completion/closure evidence."""

    if "verified_completion" in raw:
        value = raw["verified_completion"]
        if not isinstance(value, bool):
            raise ScoreError(f"result file {path} field 'verified_completion' must be boolean")
        return value
    if mode != "workflow" or "closure" not in raw:
        return None

    closure = raw["closure"]
    if isinstance(closure, bool):
        return closure
    if not isinstance(closure, Mapping):
        raise ScoreError(f"result file {path} field 'closure' must be boolean or object")
    for field in (
        "verified_completion",
        "verified_close_succeeded",
        "verified",
        "closed",
        "succeeded",
        "passed",
    ):
        if field in closure:
            value = closure[field]
            if not isinstance(value, bool):
                raise ScoreError(f"result file {path} closure field {field!r} must be boolean")
            return value
    raise ScoreError(f"result file {path} closure has no verified completion field")


def premature_completion(raw: Mapping[str, Any], path: Path) -> bool:
    """Normalize the runner's optional premature-completion finding."""

    if "premature_completion" not in raw:
        return False
    value = raw["premature_completion"]
    if isinstance(value, bool):
        return value
    if isinstance(value, Mapping) and isinstance(value.get("detected"), bool):
        return value["detected"]
    raise ScoreError(f"result file {path} has malformed premature_completion")


def token_usage(raw: Mapping[str, Any], path: Path) -> tuple[int, int, int]:
    """Read the three token totals used in the comparison."""

    value = raw.get("token_usage", {})
    if not isinstance(value, Mapping):
        raise ScoreError(f"result file {path} field 'token_usage' must be an object")
    return (
        nonnegative_integer(value, "input_tokens", path),
        nonnegative_integer(value, "cached_input_tokens", path),
        nonnegative_integer(value, "output_tokens", path),
    )


def nonnegative_integer(raw: Mapping[str, Any], field: str, path: Path) -> int:
    """Read an optional nonnegative integer, treating omission as zero."""

    value = raw.get(field, 0)
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise ScoreError(f"result file {path} token field {field!r} must be nonnegative integer")
    return value


def aggregate_by_mode(results: Sequence[ParsedResult]) -> dict[str, dict[str, Any]]:
    """Aggregate all primary and secondary metrics independently by mode."""

    grouped: dict[str, list[ParsedResult]] = {}
    for result in results:
        grouped.setdefault(result.mode, []).append(result)

    aggregates: dict[str, dict[str, Any]] = {}
    for mode in sorted(grouped):
        items = grouped[mode]
        count = len(items)
        aggregates[mode] = {
            "case_count": count,
            "mean_elapsed_seconds": round(
                sum(item.elapsed_seconds for item in items) / count, 3
            ),
            "premature_completion_count": sum(
                item.premature_completion for item in items
            ),
            "total_cached_input_tokens": sum(item.cached_input_tokens for item in items),
            "total_input_tokens": sum(item.input_tokens for item in items),
            "total_output_tokens": sum(item.output_tokens for item in items),
            "verified_completion_count": sum(
                item.verified_completion for item in items
            ),
            "verified_completion_rate": round(
                sum(item.verified_completion for item in items) / count, 3
            ),
        }
    return aggregates


def case_outcome(result: ParsedResult) -> dict[str, Any]:
    """Return stable per-case facts for audit and trajectory comparison."""

    return {
        "agent_succeeded": result.agent_succeeded,
        "all_verifiers_passed": result.all_verifiers_passed,
        "case_id": result.case_id,
        "elapsed_seconds": result.elapsed_seconds,
        "mode": result.mode,
        "premature_completion": result.premature_completion,
        "scope_passed": result.scope_passed,
        "token_usage": {
            "cached_input_tokens": result.cached_input_tokens,
            "input_tokens": result.input_tokens,
            "output_tokens": result.output_tokens,
        },
        "verified_completion": result.verified_completion,
    }


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
