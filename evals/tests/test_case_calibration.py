"""Deterministic checks for the frozen T14 case calibration lane."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from evals.calibration.calibrate import (
    CalibrationError,
    check_manifest,
    load_manifest,
)


ROOT = Path(__file__).resolve().parents[2]
CALIBRATION = ROOT / "evals" / "calibration"
MANIFEST = CALIBRATION / "manifest.json"
COMMAND = CALIBRATION / "calibrate.py"
CASE_IDS = [
    "01-steering-retry",
    "02-steering-pagination",
    "03-steering-atomic-import",
    "04-scope-csv-export",
    "05-rename-resume",
]


class CaseCalibrationTests(unittest.TestCase):
    """Exercise manifest freezing, drift checks, and both calibration arms."""

    def test_manifest_freezes_all_case_inputs_and_plain_summary(self) -> None:
        manifest = load_manifest(MANIFEST)

        self.assertEqual(manifest["schema_version"], 1)
        self.assertEqual(
            [case["id"] for case in manifest["cases"]],
            CASE_IDS,
        )
        for case in manifest["cases"]:
            with self.subTest(case=case["id"]):
                for field in (
                    "workspace",
                    "case_contract",
                    "external_grader",
                    "neutral_continuation_prompt",
                    "gold_projection",
                    "plain_summary",
                    "information_match",
                    "allowed_paths",
                    "runner_model_policy",
                    "artifacts",
                    "fingerprints",
                ):
                    self.assertIn(field, case)
                self.assertIsInstance(case["plain_summary"], str)
                self.assertGreater(case["plain_summary_utf8_bytes"], 0)
                self.assertEqual(
                    case["plain_summary_utf8_bytes"],
                    len(case["plain_summary"].encode("utf-8")),
                )
                self.assertGreater(case["neutral_continuation_prompt_utf8_bytes"], 0)
                self.assertEqual(
                    case["neutral_continuation_prompt_utf8_bytes"],
                    len(case["neutral_continuation_prompt"].encode("utf-8")),
                )
                self.assertEqual(
                    case["information_match"]["status"],
                    "exact",
                )
                projection = case["gold_projection"]
                self.assertFalse(projection["allow_additional_active"])
                self.assertEqual(
                    projection["source_namespace"],
                    {
                        "comparison": "non_identity",
                        "name": "fixture_logical_v1",
                    },
                )
                self.assertTrue(
                    all(requirement.get("id") for requirement in projection["requirements"])
                )
                self.assertTrue(
                    all(requirement.get("evidence") for requirement in projection["requirements"])
                )
                self.assertNotIn(
                    "all",
                    [
                        requirement["evidence"].get("verifier_name")
                        for requirement in projection["requirements"]
                    ],
                )
                self.assertIn("inactive_requirements", projection)
                source_bundle_relative = case["artifacts"]["source_bundle"]
                source_bundle = json.loads(
                    (
                        ROOT
                        / "evals/cases"
                        / case["id"]
                        / source_bundle_relative
                    ).read_text(encoding="utf-8")
                )
                self.assertEqual(source_bundle["schema_version"], 1)
                self.assertEqual(source_bundle["source"]["provider"], "codex")
                self.assertEqual(source_bundle["native_goal"], {"state": "absent"})
                self.assertTrue(
                    all(record["role"] == "user" for record in source_bundle["records"])
                )
                self.assertTrue(
                    all(
                        record["content_digest"].startswith("sha256:")
                        for record in source_bundle["records"]
                    )
                )
                self.assertEqual(
                    source_bundle["source"]["head"],
                    source_bundle["records"][-1]["id"],
                )
                self.assertEqual(
                    case["neutral_continuation_prompt"],
                    "Continue the task from this checkpoint. Preserve existing behavior and complete the remaining work. Do not claim completion without running relevant validation.",
                )
                self.assertEqual(
                    case["runner_model_policy"]["provider"],
                    "Codex",
                )
                self.assertEqual(case["runner_model_policy"]["model"], "gpt-5.6-luna")
                self.assertEqual(case["runner_model_policy"]["effort"], "max")
                self.assertEqual(case["runner_model_policy"]["approval_policy"], "never")
                self.assertEqual(case["runner_model_policy"]["sandbox"], "workspace-write")
                self.assertEqual(case["runner_model_policy"]["context_bytes"], 131072)
                self.assertFalse(case["runner_model_policy"]["calibration"]["codex_calls"])
                self.assertFalse(case["runner_model_policy"]["calibration"]["network"])

        pagination = manifest["cases"][1]["gold_projection"]
        self.assertEqual(
            pagination["inactive_requirements"],
            [
                {
                    "id": "02.superseded-last-occurrence",
                    "lifecycle": "superseded",
                    "source_record_ids": ["steering.1"],
                    "superseded_by": "02.final-correction",
                    "text": (
                        "When pages overlap, keep the last occurrence of each duplicate "
                        "item ID because a later page should be treated as newer data. "
                        "Preserve the order of the retained occurrences."
                    ),
                }
            ],
        )

    def test_manifest_fingerprints_are_current(self) -> None:
        result = check_manifest(ROOT, MANIFEST)

        self.assertTrue(result["passed"])
        self.assertEqual(result["case_count"], 5)
        self.assertEqual(result["drift"], [])

    def test_check_command_runs_reference_three_times_and_negative_once_per_case(self) -> None:
        completed = subprocess.run(
            [sys.executable, str(COMMAND), "check", "--root", str(ROOT)],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )

        self.assertEqual(completed.returncode, 0, completed.stderr)
        result = json.loads(completed.stdout)
        self.assertEqual(result["status"], "verified")
        self.assertEqual(result["manifest"]["case_count"], 5)
        self.assertEqual(
            [case["case_id"] for case in result["cases"]],
            CASE_IDS,
        )
        for case in result["cases"]:
            with self.subTest(case=case["case_id"]):
                self.assertEqual(case["reference"]["repetitions"], 3)
                self.assertEqual(case["reference"]["passed_runs"], 3)
                self.assertTrue(case["reference"]["passed"])
                self.assertTrue(case["known_bad"]["visible_plausible"])
                self.assertTrue(case["known_bad"]["hidden_failed"])
                self.assertTrue(case["known_bad"]["passed"])

    def test_check_rejects_fingerprint_drift_without_rewriting_manifest(self) -> None:
        raw = json.loads(MANIFEST.read_text(encoding="utf-8"))
        raw["cases"][0]["plain_summary"] += " drift"
        with tempfile.TemporaryDirectory(prefix="driftctl-calibration-manifest-") as temporary:
            manifest = Path(temporary) / "manifest.json"
            manifest.write_text(json.dumps(raw), encoding="utf-8")

            with self.assertRaises(CalibrationError):
                check_manifest(ROOT, manifest)


if __name__ == "__main__":
    unittest.main()
