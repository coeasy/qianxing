import json
import importlib.util
import tempfile
import unittest
from pathlib import Path

import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing.app import AppError, FidelityInsufficientError, validate_strategy_execution_preflight_v1


class StrategyExecutionPreflightTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.bars_path = self.root / "bars.json"
        self.bars_path.write_text(json.dumps(self._bar_frame()), encoding="utf-8")

    def tearDown(self):
        self.temp.cleanup()

    @staticmethod
    def _bar_frame():
        rows = 12
        return {
            "schema_version": 1,
            "instrument": "BTCUSDT.BINANCE",
            "source": "python-strategy-preflight-test",
            "ts": [1000 + 1000 * index for index in range(rows)],
            "open_raw": [100_000_000_000] * rows,
            "high_raw": [101_000_000_000] * rows,
            "low_raw": [99_000_000_000] * rows,
            "close_raw": [100_000_000_000] * rows,
            "volume_raw": [1_000_000_000] * rows,
        }

    def _manifest(self, *, completeness="complete", warmup_bars=4):
        return {
            "schema_version": 1,
            "kind": "qianxing-python-strategy-manifest",
            "strategy_id": "sample-sma",
            "strategy_version": "1.0.0",
            "entrypoint": "create_strategy(spec)",
            "parameters": {
                "lookback": {
                    "type": "integer",
                    "required": True,
                    "minimum": 2,
                    "maximum": 100,
                }
            },
            "instrument_requirements": {
                "asset_classes": ["crypto"],
                "products": ["spot"],
                "venues": ["binance"],
                "requires_explicit_spec": True,
            },
            "data_level": "bar" if completeness == "complete" else None,
            "warmup_bars": warmup_bars if completeness == "complete" else None,
            "dependencies": [],
            "completeness": completeness,
        }

    def _spec(self):
        return {
            "instrument": "BTCUSDT.BINANCE",
            "bars_path": str(self.bars_path),
            "market_spec": {"product": "spot", "exchange_id": "BINANCE"},
            "asset_class": "Crypto",
            "parameters": {"lookback": 12},
        }

    def test_complete_manifest_reports_rust_checked_bar_fingerprint(self):
        result = validate_strategy_execution_preflight_v1(
            self._manifest(), self._spec(), purpose="run"
        )
        self.assertEqual(result["schema_version"], 1)
        self.assertEqual(result["completeness"], "complete")
        self.assertEqual(result["checked_markets"][0]["rows"], 12)
        self.assertRegex(
            result["checked_markets"][0]["content_fingerprint"],
            r"^barframe:[0-9a-f]{16}$",
        )

    def test_partial_manifest_is_explicitly_unchecked(self):
        result = validate_strategy_execution_preflight_v1(
            self._manifest(completeness="partial"), {}, purpose="run"
        )
        self.assertEqual(result["completeness"], "partial")
        self.assertEqual(result["checked_markets"], [])

    def test_warmup_requires_a_decision_bar(self):
        with self.assertRaises(FidelityInsufficientError):
            validate_strategy_execution_preflight_v1(
                self._manifest(warmup_bars=12), self._spec(), purpose="run"
            )

    def test_market_mismatch_is_rejected_before_execution(self):
        spec = self._spec()
        spec["asset_class"] = "equity"
        with self.assertRaises(AppError) as captured:
            validate_strategy_execution_preflight_v1(
                self._manifest(), spec, purpose="run"
            )
        self.assertEqual(captured.exception.category, "INVALID_INPUT")

    def test_registered_schema_accepts_both_request_and_result_documents(self):
        if importlib.util.find_spec("jsonschema") is None:
            self.skipTest("jsonschema is optional in the base package")
        import jsonschema

        root = Path(__file__).resolve().parents[2]
        schema = json.loads(
            (root / "schemas" / "strategy-execution-preflight-v1.json").read_text(
                encoding="utf-8"
            )
        )
        request = {
            "schema_version": 1,
            "strategy_manifest": self._manifest(),
            "execution_spec": self._spec(),
            "purpose": "run",
            "target_strategy_id": None,
            "split_plan": None,
        }
        result = {
            "schema_version": 1,
            "strategy_id": "sample-sma",
            "strategy_version": "1.0.0",
            "completeness": "complete",
            "data_level": "bar",
            "warmup_bars": 4,
            "checked_markets": [
                {
                    "label": "backtest",
                    "instrument": "BTCUSDT.BINANCE",
                    "rows": 12,
                    "content_fingerprint": "barframe:0123456789abcdef",
                }
            ],
        }
        validator = jsonschema.Draft202012Validator(schema)
        validator.check_schema(schema)
        validator.validate(request)
        validator.validate(result)


if __name__ == "__main__":
    unittest.main()
