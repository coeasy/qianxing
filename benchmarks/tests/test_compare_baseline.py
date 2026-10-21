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
        old_report.setdefault("measurement_source_fingerprint", "baseline-source")
        new_report.setdefault("measurement_source_fingerprint", "current-source")
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

    def test_small_container_memory_variation_still_blocks_regression(self) -> None:
        old = {
            "schema_version": 1,
            "hardware": {
                "os": "Linux",
                "arch": "x86_64",
                "processor": "x86_64",
                "logical_cpu_count": 4,
                "physical_core_count": None,
                "total_ram_bytes": 16_760_344_576,
            },
            "rows": [{"bars": 1000, "p50_ms": 10.0, "p95_ms": 20.0}],
        }
        new = {
            **old,
            "hardware": {**old["hardware"], "total_ram_bytes": 16_766_414_848},
            "rows": [{"bars": 1000, "p50_ms": 11.1, "p95_ms": 20.0}],
        }
        result = self.compare_reports(
            old,
            new,
            "benchmark_regression_thresholds:\n  end_to_end: 0.10\n",
        )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("registered limit +10%", result.stdout)

    def test_different_runner_shape_skips_trend_comparison(self) -> None:
        old = {
            "schema_version": 1,
            "hardware": {"os": "Linux", "arch": "x86_64", "logical_cpu_count": 4},
            "rows": [{"bars": 1000, "p50_ms": 10.0, "p95_ms": 20.0}],
        }
        new = {
            **old,
            "hardware": {**old["hardware"], "logical_cpu_count": 8},
            "rows": [{"bars": 1000, "p50_ms": 20.0, "p95_ms": 30.0}],
        }
        result = self.compare_reports(
            old,
            new,
            "benchmark_regression_thresholds:\n  end_to_end: 0.10\n",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Hardware fingerprint differs", result.stdout)

    def test_throughput_improvement_does_not_fail(self) -> None:
        base = {"schema_version": 1, "hardware": {"cpu": "same"}}
        result = self.compare_reports(
            {
                **base,
                "records": [
                    {"benchmark": "append_batch", "operations": 1000, "operations_per_second": 100.0}
                ],
            },
            {
                **base,
                "records": [
                    {"benchmark": "append_batch", "operations": 1000, "operations_per_second": 150.0}
                ],
            },
            "benchmark_regression_thresholds:\n  rust_micro: 0.10\n",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_throughput_regression_over_threshold_fails(self) -> None:
        base = {"schema_version": 1, "hardware": {"cpu": "same"}}
        result = self.compare_reports(
            {
                **base,
                "records": [
                    {"benchmark": "append_batch", "operations": 1000, "operations_per_second": 100.0}
                ],
            },
            {
                **base,
                "records": [
                    {"benchmark": "append_batch", "operations": 1000, "operations_per_second": 89.0}
                ],
            },
            "benchmark_regression_thresholds:\n  rust_micro: 0.10\n",
        )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("11.0% regression", result.stdout)

    def test_small_sample_p95_spike_is_reported_but_not_blocking(self) -> None:
        result = self.compare(
            [{"bars": 1000, "p50_ms": 10.0, "p95_ms": 20.0}],
            [{"bars": 1000, "p50_ms": 10.0, "p95_ms": 200.0}],
            "benchmark_regression_thresholds:\n  end_to_end: 0.10\n",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_unchanged_measured_source_skips_environment_noise(self) -> None:
        base = {
            "schema_version": 1,
            "hardware": {"cpu": "same"},
            "measurement_source_fingerprint": "same-source",
        }
        result = self.compare_reports(
            {**base, "rows": [{"bars": 1000, "p50_ms": 10.0, "p95_ms": 20.0}]},
            {**base, "rows": [{"bars": 1000, "p50_ms": 20.0, "p95_ms": 40.0}]},
            "benchmark_regression_thresholds:\n  end_to_end: 0.10\n",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Measured source is unchanged", result.stdout)

    def test_legacy_baseline_without_source_fingerprint_seeds_current_sample(self) -> None:
        result = self.compare_reports(
            {
                "schema_version": 1,
                "hardware": {"cpu": "same"},
                "measurement_source_fingerprint": None,
                "rows": [{"bars": 1000, "p50_ms": 10.0, "p95_ms": 20.0}],
            },
            {
                "schema_version": 1,
                "hardware": {"cpu": "same"},
                "rows": [{"bars": 1000, "p50_ms": 20.0, "p95_ms": 40.0}],
            },
            "benchmark_regression_thresholds:\n  end_to_end: 0.10\n",
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("first source baseline", result.stdout)

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
