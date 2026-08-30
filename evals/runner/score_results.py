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
    independent_review_passed: bool | None
    external_acceptance_passed: bool | None
    full_regression_suite_passed: bool | None
    human_interventions: int | None
    projection_fidelity_passed: bool | None
    requirement_pass_rate: float | None
    premature_completion: bool
    elapsed_seconds: float
    token_usage_available: bool
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
    independent_review_passed = independent_review_success(raw, path)
    external_acceptance_passed = (
        all_verifiers_passed and scope_passed and independent_review_passed
        if independent_review_passed is not None
        else None
    )
    full_regression_suite_passed = regression_success(raw, path)
    human_interventions = optional_nonnegative_integer(
        raw, "human_interventions", path
    )
    projection_fidelity_passed = projection_fidelity_success(raw, path)
    requirement_pass_rate = optional_rate(raw, "requirement_pass_rate", path)
    explicit_completion = explicit_verified_completion(raw, path)
    verified_completion = (
        agent_succeeded
        and all_verifiers_passed
        and scope_passed
        and independent_review_passed is not False
        and (explicit_completion if explicit_completion is not None else True)
    )
    premature = premature_completion(raw, path)
    (
        token_usage_available,
        input_tokens,
        cached_input_tokens,
        output_tokens,
    ) = token_usage(raw, path)
    return ParsedResult(
        mode=mode,
        case_id=case_id,
        verified_completion=verified_completion,
        agent_succeeded=agent_succeeded,
        all_verifiers_passed=all_verifiers_passed,
        scope_passed=scope_passed,
        independent_review_passed=independent_review_passed,
        external_acceptance_passed=external_acceptance_passed,
        full_regression_suite_passed=full_regression_suite_passed,
        human_interventions=human_interventions,
        projection_fidelity_passed=projection_fidelity_passed,
        requirement_pass_rate=requirement_pass_rate,
        premature_completion=premature,
        elapsed_seconds=elapsed_seconds,
        token_usage_available=token_usage_available,
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


def independent_review_success(raw: Mapping[str, Any], path: Path) -> bool | None:
    """Read external one-pass review evidence when the runner supplies it."""

    review = raw.get("review")
    if review is None:
        if raw.get("evaluation_kind") == "native_long_session":
            raise ScoreError(f"result file {path} lacks independent review evidence")
        return None
    if not isinstance(review, Mapping) or not isinstance(
        review.get("review_passed"), bool
    ):
        raise ScoreError(f"result file {path} has malformed independent review evidence")
    return review["review_passed"]


def explicit_verified_completion(raw: Mapping[str, Any], path: Path) -> bool | None:
    """Read an optional runner claim only as an additional veto."""

    if "verified_completion" in raw:
        value = raw["verified_completion"]
        if not isinstance(value, bool):
            raise ScoreError(f"result file {path} field 'verified_completion' must be boolean")
        return value
    return None


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


def optional_boolean(
    raw: Mapping[str, Any], field: str, path: Path
) -> bool | None:
    """Read an optional boolean without inventing a legacy value."""

    value = raw.get(field)
    if value is None:
        return None
    if not isinstance(value, bool):
        raise ScoreError(f"result file {path} field {field!r} must be boolean")
    return value


def optional_nonnegative_integer(
    raw: Mapping[str, Any], field: str, path: Path
) -> int | None:
    """Read an optional nonnegative integer without treating omission as zero."""

    value = raw.get(field)
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise ScoreError(
            f"result file {path} field {field!r} must be a nonnegative integer"
        )
    return value


def optional_rate(raw: Mapping[str, Any], field: str, path: Path) -> float | None:
    """Read an optional finite rate in the inclusive range zero through one."""

    value = raw.get(field)
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ScoreError(f"result file {path} field {field!r} must be a number")
    rate = float(value)
    if not math.isfinite(rate) or not 0 <= rate <= 1:
        raise ScoreError(f"result file {path} field {field!r} must be between 0 and 1")
    return rate


def projection_fidelity_success(
    raw: Mapping[str, Any], path: Path
) -> bool | None:
    """Read workflow-input projection fidelity when an arm carries it."""

    value = raw.get("projection_fidelity")
    if value is None:
        return None
    if not isinstance(value, Mapping) or not isinstance(value.get("available"), bool):
        raise ScoreError(f"result file {path} has malformed projection fidelity")
    if value["available"] is False:
        return None
    if value.get("scope") != "workflow_input_projection" or not isinstance(
        value.get("overall_pass"), bool
    ):
        raise ScoreError(f"result file {path} has malformed projection fidelity")
    return value["overall_pass"]


def regression_success(raw: Mapping[str, Any], path: Path) -> bool | None:
    """Read or derive the regression result from one named full-suite verifier."""

    reported = optional_boolean(raw, "full_regression_suite_passed", path)
    verifiers = raw.get("verifiers")
    full_suite = (
        [
            item.get("passed")
            for item in verifiers
            if isinstance(item, Mapping) and item.get("name") == "all"
        ]
        if isinstance(verifiers, list)
        else []
    )
    if reported is None:
        return full_suite[0] if len(full_suite) == 1 else None
    if len(full_suite) != 1 or full_suite[0] is not reported:
        raise ScoreError(
            f"result file {path} regression result does not match the full-suite verifier"
        )
    return reported


def token_usage(raw: Mapping[str, Any], path: Path) -> tuple[bool, int, int, int]:
    """Read the three token totals used in the comparison."""

    value = raw.get("token_usage", {})
    if not isinstance(value, Mapping):
        raise ScoreError(f"result file {path} field 'token_usage' must be an object")
    available = value.get("available")
    if available is None:
        available = any(
            field in value
            for field in ("input_tokens", "cached_input_tokens", "output_tokens")
        )
    if not isinstance(available, bool):
        raise ScoreError(
            f"result file {path} field 'token_usage.available' must be boolean"
        )
    return (
        available,
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
        external = [
            item.external_acceptance_passed
            for item in items
            if item.external_acceptance_passed is not None
        ]
        regressions = [
            item.full_regression_suite_passed
            for item in items
            if item.full_regression_suite_passed is not None
        ]
        interventions = [
            item.human_interventions
            for item in items
            if item.human_interventions is not None
        ]
        projection_fidelity = [
            item.projection_fidelity_passed
            for item in items
            if item.projection_fidelity_passed is not None
        ]
        requirement_rates = [
            item.requirement_pass_rate
            for item in items
            if item.requirement_pass_rate is not None
        ]
        token_items = [item for item in items if item.token_usage_available]
        aggregates[mode] = {
            "case_count": count,
            "external_acceptance_available_count": len(external),
            "external_acceptance_pass_count": sum(external),
            "external_acceptance_rate": optional_mean(external),
            "full_regression_available_count": len(regressions),
            "full_regression_pass_count": sum(regressions),
            "full_regression_pass_rate": optional_mean(regressions),
            "human_intervention_available_count": len(interventions),
            "mean_human_interventions": optional_mean(interventions),
            "mean_elapsed_seconds": round(
                sum(item.elapsed_seconds for item in items) / count, 3
            ),
            "mean_requirement_pass_rate": optional_mean(requirement_rates),
            "premature_completion_count": sum(
                item.premature_completion for item in items
            ),
            "projection_fidelity_available_count": len(projection_fidelity),
            "projection_fidelity_pass_count": sum(projection_fidelity),
            "projection_fidelity_pass_rate": optional_mean(projection_fidelity),
            "requirement_pass_rate_available_count": len(requirement_rates),
            "token_usage_available_count": len(token_items),
            "total_cached_input_tokens": sum(
                item.cached_input_tokens for item in token_items
            ),
            "total_human_interventions": sum(interventions),
            "total_input_tokens": sum(item.input_tokens for item in token_items),
            "total_output_tokens": sum(item.output_tokens for item in token_items),
            "verified_completion_count": sum(
                item.verified_completion for item in items
            ),
            "verified_completion_rate": round(
                sum(item.verified_completion for item in items) / count, 3
            ),
        }
    return aggregates


def optional_mean(values: Sequence[int | float | bool]) -> float | None:
    """Return a rounded mean, preserving unavailable as JSON null."""

    if not values:
        return None
    return round(sum(values) / len(values), 3)


def case_outcome(result: ParsedResult) -> dict[str, Any]:
    """Return stable per-case facts for audit and trajectory comparison."""

    return {
        "agent_succeeded": result.agent_succeeded,
        "all_verifiers_passed": result.all_verifiers_passed,
        "case_id": result.case_id,
        "elapsed_seconds": result.elapsed_seconds,
        "external_acceptance_passed": result.external_acceptance_passed,
        "full_regression_suite_passed": result.full_regression_suite_passed,
        "human_interventions": result.human_interventions,
        "independent_review_passed": result.independent_review_passed,
        "mode": result.mode,
        "premature_completion": result.premature_completion,
        "projection_fidelity_passed": result.projection_fidelity_passed,
        "requirement_pass_rate": result.requirement_pass_rate,
        "scope_passed": result.scope_passed,
        "token_usage": {
            "available": result.token_usage_available,
            "cached_input_tokens": result.cached_input_tokens,
            "input_tokens": result.input_tokens,
            "output_tokens": result.output_tokens,
        },
        "verified_completion": result.verified_completion,
    }


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
