from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from measurement_source_changes import is_measured_source_input


class MeasuredSourceChangeTests(unittest.TestCase):
    def test_runtime_rust_manifests_fixtures_and_performance_policy_are_measured(self) -> None:
        for path in (
            "Cargo.toml",
            "Cargo.lock",
            "crates/qx-runtime/src/pipeline.rs",
            "crates/qx-runtime/Cargo.toml",
            "crates/qx-runtime/benches/criterion_performance.rs",
            "maturity/fixtures/market.json",
            "maturity/targets.yaml",
        ):
            with self.subTest(path=path):
                self.assertTrue(is_measured_source_input(path))

    def test_docs_ci_and_benchmark_harness_changes_do_not_claim_product_regression(self) -> None:
        for path in (
            ".github/workflows/ci.yml",
            "benchmarks/run_rust_bench.py",
            "benchmarks/README.md",
            "benchmarks/tests/test_run_rust_bench.py",
            "tools/test_performance_workflow.py",
            "docs/performance.md",
        ):
            with self.subTest(path=path):
                self.assertFalse(is_measured_source_input(path))


if __name__ == "__main__":
    unittest.main()
