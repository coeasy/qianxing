from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from run_rust_bench import criterion_estimate_path


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


if __name__ == "__main__":
    unittest.main()
