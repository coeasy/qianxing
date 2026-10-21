from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
COMPARATOR = ROOT / "benchmarks" / "compare_baseline.py"


class CompareBaselineTests(unittest.TestCase):
    def compare_reports(
        self, old_report: dict, new_report: dict, targets: str
    ) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            baseline = root / "baseline.json"
            current = root / "current.json"
            target_file = root / "targets.yaml"
            baseline.write_text(
                json.dumps(old_report),
                encoding="utf-8",
            )
            current.write_text(
                json.dumps(new_report),
                encoding="utf-8",
            )
            target_file.write_text(targets, encoding="utf-8")
            return subprocess.run(
                [
                    sys.executable,
                    str(COMPARATOR),
                    str(baseline),
                    str(current),
                    "--targets",
                    str(target_file),
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=False,
            )

    def compare(self, old_rows: list[dict], new_rows: list[dict], targets: str) -> subprocess.CompletedProcess[str]:
        base = {"schema_version": 1, "hardware": {"cpu": "same"}, "records": []}
        return self.compare_reports(
            {**base, "rows": old_rows},
            {**base, "rows": new_rows},
            targets,
        )

    def test_registered_same_machine_regression_over_threshold_fails(self) -> None:
        result = self.compare(
            [{"bars": 1000, "p50_ms": 10.0, "p95_ms": 20.0}],
            [{"bars": 1000, "p50_ms": 11.1, "p95_ms": 20.0}],
            "benchmark_regression_thresholds:\n  end_to_end: 0.10\n",
        )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("registered limit +10%", result.stdout)

    def test_improvement_does_not_fail(self) -> None:
        result = self.compare(
            [{"bars": 1000, "p50_ms": 10.0, "p95_ms": 20.0}],
            [{"bars": 1000, "p50_ms": 8.0, "p95_ms": 15.0}],
            "benchmark_regression_thresholds:\n  end_to_end: 0.10\n",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_unmatched_workload_is_a_first_sample(self) -> None:
        result = self.compare(
            [{"bars": 1000, "p50_ms": 10.0, "p95_ms": 20.0}],
            [{"bars": 2000, "p50_ms": 11.1, "p95_ms": 21.0}],
            "benchmark_regression_thresholds:\n  end_to_end: 0.10\n",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("No matching workload sizes", result.stdout)

    def test_unregistered_metric_group_is_not_blocked(self) -> None:
        result = self.compare(
            [{"bars": 1000, "p50_ms": 10.0, "p95_ms": 20.0}],
            [{"bars": 1000, "p50_ms": 15.0, "p95_ms": 30.0}],
            "benchmark_regression_thresholds:\n  rust_micro: 0.10\n",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("is not a registered benchmark group", result.stdout)

    def test_new_paper_lifecycle_semantics_do_not_compare_with_old_workload_id(self) -> None:
        base = {"schema_version": 1, "hardware": {"cpu": "same"}, "records": []}
        result = self.compare_reports(
            {
                **base,
                "criterion_records": [
                    {"benchmark": "qx_zhenlu_paper_venue_order_lifecycle/16", "median_ns": 1000}
                ],
            },
            {
                **base,
                "criterion_records": [
                    {"benchmark": "qx_zhenlu_crypto_paper_lifecycle_v2/16", "median_ns": 3_000}
                ],
            },
            "benchmark_regression_thresholds:\n  criterion: 0.10\n",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("No matching workload sizes", result.stdout)

    def test_registered_paper_lifecycle_v2_regression_over_ten_percent_fails(self) -> None:
        base = {"schema_version": 1, "hardware": {"cpu": "same"}, "records": []}
        result = self.compare_reports(
            {
                **base,
                "criterion_records": [
                    {"benchmark": "qx_zhenlu_crypto_paper_lifecycle_v2/16", "median_ns": 1000}
                ],
            },
            {
                **base,
                "criterion_records": [
                    {"benchmark": "qx_zhenlu_crypto_paper_lifecycle_v2/16", "median_ns": 1110}
                ],
            },
            "benchmark_regression_thresholds:\n  criterion: 0.10\n",
        )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("registered limit +10%", result.stdout)

    def test_crypto_paper_worker_e2e_record_uses_blocking_end_to_end_threshold(self) -> None:
        base = {"schema_version": 1, "hardware": {"cpu": "same"}}
        result = self.compare_reports(
            {
                **base,
                "records": [
                    {
                        "benchmark": "end_to_end",
                        "operations": "crypto-paper-worker-acceptance-v1",
                        "median_ms": 100.0,
                        "p95_ms": 110.0,
                    }
                ],
            },
            {
                **base,
                "records": [
                    {
                        "benchmark": "end_to_end",
                        "operations": "crypto-paper-worker-acceptance-v1",
                        "median_ms": 111.0,
                        "p95_ms": 110.0,
                    }
                ],
            },
            "benchmark_regression_thresholds:\n  end_to_end: 0.10\n",
        )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("registered limit +10%", result.stdout)


if __name__ == "__main__":
    unittest.main()
