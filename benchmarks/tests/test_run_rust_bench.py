from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from run_rust_bench import CRITERION_REQUIRED, criterion_estimate_path, criterion_plan


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
    def test_pr_profile_runs_representative_hot_paths_with_full_sample_settings(self) -> None:
        required, commands = criterion_plan("cargo", "pr")
        self.assertEqual(len(required), 12)
        self.assertLess(len(required), len(CRITERION_REQUIRED))
        self.assertEqual(len(commands), 4)
        self.assertEqual({command[3] for command in commands}, {"qx-core", "qx-zhenlu"})
        for command in commands:
            self.assertEqual(command[-1], "--noplot")
            self.assertIn("--bench", command)

    def test_full_profile_keeps_all_crates_and_all_registered_benchmarks(self) -> None:
        required, commands = criterion_plan("cargo", "full")
        self.assertEqual(required, CRITERION_REQUIRED)
        self.assertEqual(len(commands), 5)
        self.assertEqual(
            [command[3] for command in commands],
            ["qx-core", "qx-scheduler", "qx-xingban", "qx-zhenlu", "qx-cli"],
        )


if __name__ == "__main__":
    unittest.main()
