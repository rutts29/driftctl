"""Deterministic checks for baseline/workflow result scoring."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
SCORER = REPOSITORY_ROOT / "evals" / "runner" / "score_results.py"


class ScoreResultsTests(unittest.TestCase):
    """Exercise external-outcome aggregation and input rejection."""

    def test_scores_modes_from_external_outcomes(self) -> None:
        with tempfile.TemporaryDirectory(prefix="driftctl-score-test-") as temporary:
            directory = Path(temporary)
            baseline = directory / "baseline.json"
            workflow = directory / "workflow.json"
            self._write(
                baseline,
                {
                    "case_id": "case-a",
                    "status": "completed",
                    "elapsed_seconds": 2.0,
                    "premature_completion": {"detected": True},
                    "token_usage": {
                        "input_tokens": 10,
                        "cached_input_tokens": 4,
                        "output_tokens": 3,
                    },
                    "scope": {"passed": True},
                    "verifiers": [{"name": "integration", "passed": False}],
                },
            )
            self._write(
                workflow,
                {
                    "mode": "workflow",
                    "case_id": "case-a",
                    "status": "verified",
                    "elapsed_seconds": 4.0,
                    "closure": {"verified_close_succeeded": True},
                    "token_usage": {
                        "input_tokens": 20,
                        "cached_input_tokens": 8,
                        "output_tokens": 5,
                    },
                    "scope": {"passed": True},
                    "verifiers": [{"name": "integration", "passed": True}],
                },
            )

            completed = subprocess.run(
                [sys.executable, str(SCORER), str(workflow), str(baseline)],
                cwd=REPOSITORY_ROOT,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(completed.returncode, 0, completed.stderr)
            result = json.loads(completed.stdout)
            self.assertEqual(result["primary_metric"], "verified_completion_rate")
            baseline_score = result["by_mode"]["baseline"]
            workflow_score = result["by_mode"]["workflow"]
            self.assertEqual(baseline_score["case_count"], 1)
            self.assertEqual(baseline_score["mean_elapsed_seconds"], 2.0)
            self.assertEqual(baseline_score["premature_completion_count"], 1)
            self.assertEqual(baseline_score["total_input_tokens"], 10)
            self.assertEqual(baseline_score["verified_completion_count"], 0)
            self.assertEqual(baseline_score["verified_completion_rate"], 0.0)
            self.assertEqual(workflow_score["case_count"], 1)
            self.assertEqual(workflow_score["mean_elapsed_seconds"], 4.0)
            self.assertEqual(workflow_score["premature_completion_count"], 0)
            self.assertEqual(workflow_score["total_input_tokens"], 20)
            self.assertEqual(workflow_score["verified_completion_count"], 1)
            self.assertEqual(workflow_score["verified_completion_rate"], 1.0)
            self.assertEqual(
                [(item["mode"], item["verified_completion"]) for item in result["cases"]],
                [("baseline", False), ("workflow", True)],
            )
            self.assertTrue(result["cases"][1]["agent_succeeded"])
            self.assertTrue(result["cases"][1]["scope_passed"])

    def test_aggregates_available_secondary_metrics_without_fabricating_missing_data(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(prefix="driftctl-score-test-") as temporary:
            directory = Path(temporary)
            rich = directory / "rich.json"
            legacy = directory / "legacy.json"
            self._write(
                rich,
                {
                    "mode": "workflow",
                    "case_id": "rich",
                    "evaluation_kind": "native_long_session",
                    "status": "verified",
                    "elapsed_seconds": 5,
                    "human_interventions": 0,
                    "projection_fidelity": {
                        "available": True,
                        "overall_pass": True,
                        "scope": "workflow_input_projection",
                    },
                    "requirement_pass_rate": 0.75,
                    "review": {"review_passed": True},
                    "scope": {"passed": True},
                    "token_usage": {
                        "available": False,
                        "input_tokens": 0,
                        "cached_input_tokens": 0,
                        "output_tokens": 0,
                    },
                    "verifiers": [
                        {"name": "unit", "passed": True},
                        {"name": "all", "passed": False},
                    ],
                },
            )
            self._write(
                legacy,
                {
                    "mode": "workflow",
                    "case_id": "legacy",
                    "status": "completed",
                    "elapsed_seconds": 1,
                    "scope": {"passed": True},
                    "token_usage": {"input_tokens": 9},
                    "verifiers": [{"name": "unit", "passed": True}],
                },
            )

            completed = subprocess.run(
                [sys.executable, str(SCORER), str(rich), str(legacy)],
                cwd=REPOSITORY_ROOT,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(completed.returncode, 0, completed.stderr)
            scored = json.loads(completed.stdout)
            aggregate = scored["by_mode"]["workflow"]
            self.assertEqual(aggregate["external_acceptance_available_count"], 1)
            self.assertEqual(aggregate["external_acceptance_pass_count"], 0)
            self.assertEqual(aggregate["full_regression_available_count"], 1)
            self.assertEqual(aggregate["full_regression_pass_count"], 0)
            self.assertEqual(aggregate["human_intervention_available_count"], 1)
            self.assertEqual(aggregate["total_human_interventions"], 0)
            self.assertEqual(aggregate["mean_requirement_pass_rate"], 0.75)
            self.assertEqual(aggregate["requirement_pass_rate_available_count"], 1)
            self.assertEqual(aggregate["token_usage_available_count"], 1)
            cases = {item["case_id"]: item for item in scored["cases"]}
            self.assertTrue(cases["rich"]["projection_fidelity_passed"])
            self.assertIsNone(cases["legacy"]["projection_fidelity_passed"])

    def test_scope_failure_overrides_explicit_verified_completion(self) -> None:
        with tempfile.TemporaryDirectory(prefix="driftctl-score-test-") as temporary:
            result_path = Path(temporary) / "scope-failure.json"
            self._write(
                result_path,
                {
                    "mode": "workflow",
                    "case_id": "scope-failure",
                    "status": "verified",
                    "elapsed_seconds": 1,
                    "scope": {"passed": False},
                    "token_usage": {},
                    "verified_completion": True,
                    "verifiers": [{"name": "integration", "passed": True}],
                },
            )

            completed = subprocess.run(
                [sys.executable, str(SCORER), str(result_path)],
                cwd=REPOSITORY_ROOT,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(completed.returncode, 0, completed.stderr)
            outcome = json.loads(completed.stdout)["cases"][0]
            self.assertFalse(outcome["scope_passed"])
            self.assertFalse(outcome["verified_completion"])

    def test_internal_closure_cannot_override_failed_independent_review(self) -> None:
        with tempfile.TemporaryDirectory(prefix="driftctl-score-test-") as temporary:
            result_path = Path(temporary) / "closure-is-not-proof.json"
            self._write(
                result_path,
                {
                    "mode": "workflow",
                    "case_id": "closure-is-not-proof",
                    "status": "verified",
                    "elapsed_seconds": 1,
                    "closure": {"verified_close_succeeded": True},
                    "review": {"review_passed": False},
                    "scope": {"passed": True},
                    "token_usage": {},
                    "verifiers": [{"name": "all", "passed": True}],
                },
            )

            completed = subprocess.run(
                [sys.executable, str(SCORER), str(result_path)],
                cwd=REPOSITORY_ROOT,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(completed.returncode, 0, completed.stderr)
            outcome = json.loads(completed.stdout)["cases"][0]
            self.assertFalse(outcome["independent_review_passed"])
            self.assertFalse(outcome["verified_completion"])

    def test_scores_the_plain_summary_control_as_a_distinct_arm(self) -> None:
        with tempfile.TemporaryDirectory(prefix="driftctl-score-test-") as temporary:
            result_path = Path(temporary) / "plain-summary.json"
            self._write(
                result_path,
                {
                    "mode": "plain_summary",
                    "case_id": "case-b",
                    "status": "verified",
                    "elapsed_seconds": 3,
                    "scope": {"passed": True},
                    "token_usage": {},
                    "verified_completion": True,
                    "verifiers": [{"name": "integration", "passed": True}],
                },
            )

            completed = subprocess.run(
                [sys.executable, str(SCORER), str(result_path)],
                cwd=REPOSITORY_ROOT,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(completed.returncode, 0, completed.stderr)
            scored = json.loads(completed.stdout)
            self.assertEqual(scored["by_mode"]["plain_summary"]["case_count"], 1)
            self.assertEqual(scored["cases"][0]["mode"], "plain_summary")

    def test_rejects_missing_or_malformed_scope(self) -> None:
        with tempfile.TemporaryDirectory(prefix="driftctl-score-test-") as temporary:
            directory = Path(temporary)
            missing = directory / "missing-scope.json"
            malformed = directory / "malformed-scope.json"
            base = {
                "mode": "baseline",
                "case_id": "scope-case",
                "status": "completed",
                "elapsed_seconds": 1,
                "token_usage": {},
                "verifiers": [{"name": "integration", "passed": True}],
            }
            self._write(missing, base)
            self._write(malformed, base | {"scope": {"passed": "yes"}})

            missing_run = subprocess.run(
                [sys.executable, str(SCORER), str(missing)],
                cwd=REPOSITORY_ROOT,
                capture_output=True,
                text=True,
                check=False,
            )
            malformed_run = subprocess.run(
                [sys.executable, str(SCORER), str(malformed)],
                cwd=REPOSITORY_ROOT,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(missing_run.returncode, 1)
            self.assertIn("scope", json.loads(missing_run.stdout)["error"])
            self.assertEqual(malformed_run.returncode, 1)
            self.assertIn("scope", json.loads(malformed_run.stdout)["error"])

    def test_rejects_duplicate_case_and_malformed_result(self) -> None:
        with tempfile.TemporaryDirectory(prefix="driftctl-score-test-") as temporary:
            directory = Path(temporary)
            first = directory / "first.json"
            duplicate = directory / "duplicate.json"
            malformed = directory / "malformed.json"
            content = {
                "mode": "workflow",
                "case_id": "same",
                "status": "completed",
                "elapsed_seconds": 1,
                "scope": {"passed": True},
                "verifiers": [{"passed": True}],
            }
            self._write(first, content)
            self._write(duplicate, content)
            self._write(malformed, {"mode": "workflow", "case_id": "missing-fields"})

            duplicate_run = subprocess.run(
                [sys.executable, str(SCORER), str(first), str(duplicate)],
                cwd=REPOSITORY_ROOT,
                capture_output=True,
                text=True,
                check=False,
            )
            malformed_run = subprocess.run(
                [sys.executable, str(SCORER), str(malformed)],
                cwd=REPOSITORY_ROOT,
                capture_output=True,
                text=True,
                check=False,
            )

            self.assertEqual(duplicate_run.returncode, 1)
            self.assertIn("duplicate result", json.loads(duplicate_run.stdout)["error"])
            self.assertEqual(malformed_run.returncode, 1)
            self.assertIn("elapsed_seconds", json.loads(malformed_run.stdout)["error"])

    @staticmethod
    def _write(path: Path, value: object) -> None:
        path.write_text(json.dumps(value), encoding="utf-8")


if __name__ == "__main__":
    unittest.main()
