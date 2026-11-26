from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from run_rust_bench import (
    CRITERION_REQUIRED,
    criterion_profile_for_changed_files,
    criterion_estimate_path,
    criterion_plan,
    steady_state_tick_curve,
)


class CriterionEstimatePathTests(unittest.TestCase):
    def test_resolves_nested_criterion_ids(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            expected = root / "group" / "case" / "new" / "estimates.json"
            expected.parent.mkdir(parents=True)
            expected.touch()

            self.assertEqual(criterion_estimate_path("group/case", root), expected)

    def test_resolves_flattened_case_insensitive_ids(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            expected = root / "Group" / "linear_buy_16" / "new" / "estimates.json"
            expected.parent.mkdir(parents=True)
            expected.touch()

            self.assertEqual(criterion_estimate_path("Group/Linear/Buy/16", root), expected)


class CriterionPlanTests(unittest.TestCase):
    def test_single_package_change_only_runs_its_own_benchmarks(self) -> None:
        cases = (
            ("qx-runtime", "pr-runtime", "qx_runtime_"),
            ("qx-xingban", "pr-xingban", "portfolio_bar_timeline_aligned_64k_bars/"),
            ("qx-zhenlu", "pr-zhenlu", "qx_zhenlu_crypto_paper_lifecycle_v2/"),
        )
        for package, profile, benchmark_prefix in cases:
            with self.subTest(package=package):
                selected = criterion_profile_for_changed_files(
                    [f"crates/{package}/src/lib.rs", "benchmarks/run_rust_bench.py"]
                )
                self.assertEqual(selected, profile)
                required, commands = criterion_plan("cargo", selected)
                self.assertTrue(required)
                self.assertTrue(all(name.startswith(benchmark_prefix) for name in required))
                self.assertEqual({command[3] for command in commands}, {package})

    def test_shared_inputs_or_core_changes_include_the_shared_dependency_suite(self) -> None:
        for changed in (
            ["Cargo.lock", "crates/qx-runtime/src/pipeline.rs"],
            ["crates/qx-core/src/event_log.rs"],
        ):
            with self.subTest(changed=changed):
                self.assertEqual(criterion_profile_for_changed_files(changed), "pr-core")
                required, commands = criterion_plan("cargo", "pr-core")
                self.assertEqual(len(required), 74)
                self.assertEqual(len([command for command in commands if command[3] == "qx-core"]), 1)

    def test_multiple_or_unbenchmarked_package_changes_use_the_representative_profile(self) -> None:
        for changed in (
            ["crates/qx-runtime/src/pipeline.rs", "crates/qx-xingban/src/backtest.rs"],
            ["crates/qx-runtime/src/pipeline.rs", "crates/qx-service/src/orders.rs"],
        ):
            with self.subTest(changed=changed):
                self.assertEqual(criterion_profile_for_changed_files(changed), "pr")

    def test_pr_profile_covers_registered_performance_regression_families(self) -> None:
        required, commands = criterion_plan("cargo", "pr")
        expected_regressions = {
            *(
                f"qx_runtime_balance_snapshot_cursor/indexed/{size}"
                for size in (1_000, 8_000, 64_000)
            ),
            *(
                f"qx_runtime_latest_quote_lookup/indexed/{size}"
                for size in (1_000, 8_000, 64_000)
            ),
            *(
                f"portfolio_bar_timeline_aligned_64k_bars/{mode}/{markets}"
                for mode in ("timeline_constructor", "preallocated_flatten_sort_with_indexes")
                for markets in (2, 8, 32)
            ),
            *(f"qx_zhenlu_crypto_paper_lifecycle_v2/{size}" for size in (1, 16, 256)),
        }
        core_required = {name for name in CRITERION_REQUIRED if name.startswith("qx_core_")}
        self.assertEqual(len(required), 21)
        self.assertTrue(expected_regressions <= required)
        self.assertFalse(core_required & required)
        self.assertLess(len(required), len(CRITERION_REQUIRED))
        self.assertEqual(len(commands), 4)
        self.assertEqual(
            {command[3] for command in commands},
            {"qx-runtime", "qx-xingban", "qx-zhenlu"},
        )
        for command in commands:
            self.assertEqual(command[-1], "--noplot")
            self.assertIn("--bench", command)

    def test_pr_core_profile_runs_complete_core_suite_in_one_process(self) -> None:
        required, commands = criterion_plan("cargo", "pr-core")
        core_required = {name for name in CRITERION_REQUIRED if name.startswith("qx_core_")}
        self.assertEqual(len(required), 74)
        self.assertTrue(core_required <= required)
        core_commands = [command for command in commands if command[3] == "qx-core"]
        self.assertEqual(len(core_commands), 1)
        self.assertEqual(core_commands[0][-2], "qx_core_")
        self.assertTrue(
            {"qx-runtime", "qx-xingban", "qx-zhenlu"}
            == {command[3] for command in commands if command[3] != "qx-core"}
        )

    def test_full_profile_keeps_all_crates_and_all_registered_benchmarks(self) -> None:
        required, commands = criterion_plan("cargo", "full")
        self.assertEqual(required, CRITERION_REQUIRED)
        self.assertEqual(len(commands), 6)
        self.assertEqual(
            [command[3] for command in commands],
            ["qx-core", "qx-runtime", "qx-scheduler", "qx-xingban", "qx-zhenlu", "qx-cli"],
        )


class SteadyStateTickCurveTests(unittest.TestCase):
    def test_curve_retains_independent_window_medians(self) -> None:
        records = [
            {
                "benchmark": "steady_state_pipeline_ingest",
                "operations": size,
                "ticks": 105,
                "median_ms": median,
                "p95_ms": p95,
                "raw_ms": samples,
            }
            for size, median, p95, samples in (
                (1000, 1.8, 2.1, [1.7, 1.8, 1.8, 1.9, 1.8]),
                (8000, 3.5, 4.1, [3.4, 3.5, 3.6, 3.5, 3.5]),
            )
        ]

        curve = steady_state_tick_curve(records)

        self.assertEqual(curve["points"][0]["window_p50_ms"], records[0]["raw_ms"])
        self.assertIn("five independent window p50s", curve["method"])


if __name__ == "__main__":
    unittest.main()
