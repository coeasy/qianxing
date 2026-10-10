"""应用层用例的 Python 门面（T2-2 / 退出门 G1）。

与 `python/tests/test_native_extension.py` 同一约定：扩展是构建产物，没构建就 skip
（`cargo build -p qx-python` 或 `tools/build_python_wheel.*`），而不是把"没构建"当成失败。

这里守的是 Python 这一侧**自己**的契约：四条入口存在、进出一份 JSON 文档、失败时抛的异常
文本就是 `AppError` 文档（而不是某个 Python 侧自造的形状）。三入口之间的**逐字节等价**
由 `crates/qx-cli/tests/app_three_entrypoints.rs` 那一条守——两边分工，不重复。
"""

from __future__ import annotations

import json
import os
import tempfile
import sys
import unittest
from pathlib import Path


def _load_native():
    """找到**带应用层入口**的那一份扩展；找不到就返回 None（调用方 skip）。

    搜索顺序里 `CARGO_TARGET_DIR` 排在前面：本仓的构建产物可能落在仓库外的目标目录里，而
    `<root>/target/<profile>` 里躺着的是上一次构建的旧 `.pyd`——旧世代能 import，但没有
    `app_*` 三条入口，所以候选必须**按名字探一次**再采用（`available()` 只回答"能 import"，
    按它判断会把"扩展是旧世代"读成"扩展可用"）。
    """
    root = Path(__file__).resolve().parents[2]
    directories = []
    for profile in ("release", "debug"):
        for base in (os.environ.get("CARGO_TARGET_DIR"), str(root / "target")):
            if base:
                directories.append(Path(base) / profile)
    directories.append(root / "python" / "qianxing_bridge")
    for directory in directories:
        for pattern in ("_qianxing_native*.pyd", "_qianxing_native*.so"):
            for path in sorted(directory.glob(pattern)):
                sys.path.insert(0, str(path.parent))
                sys.modules.pop("_qianxing_native", None)
                try:
                    import _qianxing_native
                except ImportError:
                    continue
                if all(
                    hasattr(_qianxing_native, name)
                    for name in (
                        "app_validate_dataset",
                        "app_run_backtest",
                        "app_verify_run",
                        "app_compare_runs",
                    )
                ):
                    return _qianxing_native
    return None


class AppUseCaseTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = Path(__file__).resolve().parents[2]
        cls.native = _load_native()
        if cls.native is None:
            cls.bridge = None
            return
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing_bridge import app

        cls.bridge = app

    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory(
            prefix="qx-app-use-cases-", dir=self.root / "target"
        )
        self.output_dir = Path(self.temp_dir.name)

    def tearDown(self):
        self.temp_dir.cleanup()

    def _require(self):
        if self.bridge is None:
            self.skipTest("build qx-python to run the app use-case test")

    def test_python_cli_routes_compare_runs_into_the_sdk(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing.cli import _parser

        args = _parser().parse_args(["compare-runs", "comparison.json"])
        self.assertEqual(args.command, "compare-runs")
        self.assertEqual(args.input, "comparison.json")

    def _spec(self, run_id: str, output_dir: Path, bars: str | None = None) -> dict:
        return {
            "schema_version": 1,
            "run_id": run_id,
            "instrument": "BTCUSDT.BINANCE",
            "bars_path": bars or str(self.root / "deploy" / "qianxing.bar-frame.example.json"),
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_000,
            "seed": 20261010,
            "output_dir": str(output_dir),
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": "py-sma",
                "quantity_raw": 1_000_000_000,
                "fast_window": 2,
                "slow_window": 3,
            },
        }

    def test_the_shared_use_cases_run_end_to_end_from_python(self):
        self._require()
        out = self.output_dir
        verdict = self.bridge.validate_dataset(
            {
                "schema_version": 1,
                "dataset_id": "py-dataset",
                "bars_path": str(
                    self.root / "deploy" / "qianxing.bar-frame.example.json"
                ),
            }
        )
        self.assertTrue(verdict["usable"], verdict["gaps"])
        self.assertEqual(verdict["rows"], 70)

        outcome = self.bridge.run_backtest(self._spec("py-run", out))
        self.assertGreater(outcome["fills"], 0)
        self.assertEqual(outcome["equity_points"], 70)
        self.assertEqual(len(outcome["result_hash"]), 16)

        verification = self.bridge.verify_run(outcome)
        self.assertTrue(verification["verified"], verification["mismatches"])
        self.assertEqual(verification["result_hash"], outcome["result_hash"])

        second_spec = self._spec("py-run-alt", out / "alt")
        second_spec["strategy"]["strategy_id"] = "py-sma-alt"
        second_spec["strategy"]["fast_window"] = 3
        second_spec["strategy"]["slow_window"] = 4
        second = self.bridge.run_backtest(second_spec)
        comparison = self.bridge.compare_runs({
            "schema_version": 1,
            "runs": [
                {key: outcome[key] for key in ("run_id", "instrument", "data_fingerprint", "result_hash", "return_bps", "max_drawdown_bps")},
                {key: second[key] for key in ("run_id", "instrument", "data_fingerprint", "result_hash", "return_bps", "max_drawdown_bps")},
            ],
        })
        self.assertEqual([item["rank"] for item in comparison["runs"]], [1, 2])
        self.assertEqual(comparison["data_fingerprint"], outcome["data_fingerprint"])

        from qianxing import CompareRunsSpec, ComparedRun, compare_runs

        typed = compare_runs(
            CompareRunsSpec(
                runs=tuple(
                    ComparedRun(
                        run_id=item["run_id"],
                        instrument=item["instrument"],
                        data_fingerprint=item["data_fingerprint"],
                        result_hash=item["result_hash"],
                        return_bps=item["return_bps"],
                        max_drawdown_bps=item["max_drawdown_bps"],
                    )
                    for item in (outcome, second)
                )
            )
        )
        self.assertEqual([item.rank for item in typed.runs], [1, 2])

    def test_a_failure_raises_the_same_document_the_other_entrypoints_print(self):
        self._require()
        spec = self._spec("py-missing", self.output_dir, "nope.json")
        with self.assertRaises(Exception) as caught:
            self.bridge.run_backtest(spec)
        payload = self.bridge.app_error_payload(caught.exception)
        self.assertIsNotNone(payload, f"异常文本必须是 AppError 文档：{caught.exception}")
        self.assertEqual(payload["category"], "DATA_UNAVAILABLE")
        self.assertEqual(payload["source_code"], "io:NotFound")
        self.assertEqual(payload["correlation_id"], "py-missing")
        self.assertFalse(payload["safe_to_retry"])

    def test_a_short_dataset_is_a_verdict_not_an_exception(self):
        self._require()
        frame = {
            "schema_version": 1,
            "instrument": "BTCUSDT.BINANCE",
            "source": "py-test",
            "ts": [1000],
            "open_raw": [100_000_000_000],
            "high_raw": [101_000_000_000],
            "low_raw": [99_000_000_000],
            "close_raw": [100_500_000_000],
            "volume_raw": [1_000_000_000],
        }
        path = self.root / "target" / "py-app-test" / "short-frame.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(frame), encoding="utf-8")
        verdict = self.bridge.validate_dataset(
            {"schema_version": 1, "dataset_id": "py-short", "bars_path": str(path)}
        )
        self.assertFalse(verdict["usable"])
        self.assertTrue(verdict["gaps"], "判了不可用就必须给出理由")


if __name__ == "__main__":
    unittest.main()
