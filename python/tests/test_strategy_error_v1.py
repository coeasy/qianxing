from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "python"))

from qianxing_strategy import StrategyErrorV1
from qianxing_strategy.base import _dispatch_on_error


class StrategyErrorV1Test(unittest.TestCase):
    def test_python_error_dto_matches_the_shared_rust_golden_fixture(self) -> None:
        fixture_path = ROOT / "crates" / "qx-strategy" / "tests" / "fixtures" / "strategy-error-v1.json"
        fixture = json.loads(fixture_path.read_text(encoding="utf-8"))
        error = StrategyErrorV1.from_exception(
            stage="market_event",
            strategy_id="error-test",
            strategy_version="1.0.0",
            timestamp=101,
            instrument="BTCUSDT.BINANCE",
            error=RuntimeError("test callback failed"),
        )
        self.assertEqual(error.to_dict(), fixture)

    def test_message_limit_is_measured_in_utf8_bytes_without_invalid_text(self) -> None:
        error = StrategyErrorV1.from_exception(
            stage="market_event",
            strategy_id="error-test",
            strategy_version="1.0.0",
            timestamp=101,
            instrument=None,
            error=RuntimeError("交易失败" * 10_000),
        )
        self.assertLessEqual(len(error.message.encode("utf-8")), 16 * 1024)
        self.assertEqual(error.to_dict()["schema_version"], 1)

    def test_v1_and_legacy_hooks_both_run_when_one_hook_fails(self) -> None:
        observed: list[str] = []

        class Strategy:
            def on_error_v1(self, error: StrategyErrorV1) -> None:
                observed.append(f"v1:{error.stage}")
                raise RuntimeError("secondary v1 observer failure")

            def on_error(self, context, error) -> None:
                observed.append(f"legacy:{context['strategy_id']}:{type(error).__name__}")

        error_v1 = StrategyErrorV1.from_exception(
            stage="market_event",
            strategy_id="error-test",
            strategy_version="1.0.0",
            timestamp=101,
            instrument="BTCUSDT.BINANCE",
            error=RuntimeError("original callback failure"),
        )
        with self.assertRaisesRegex(RuntimeError, "secondary v1 observer failure"):
            _dispatch_on_error(
                Strategy(),
                {"strategy_id": "error-test"},
                ValueError("original callback failure"),
                error_v1,
            )
        self.assertEqual(observed, ["v1:market_event", "legacy:error-test:ValueError"])


if __name__ == "__main__":
    unittest.main()
