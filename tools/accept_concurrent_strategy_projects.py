"""Run concurrent project isolation checks with the active installed native module."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

# Load the wheel/native module before the test adds the source tree to sys.path.
import qianxing_bridge._qianxing_native  # noqa: F401

from python.tests import test_strategy_project_concurrency


def main() -> int:
    suite = unittest.defaultTestLoader.loadTestsFromModule(
        test_strategy_project_concurrency
    )
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    raise SystemExit(main())
