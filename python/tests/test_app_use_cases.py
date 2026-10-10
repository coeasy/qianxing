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
from unittest.mock import patch
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
                        "app_start_backtest",
                        "app_verify_run",
                        "app_compare_runs",
                        "app_run_experiment",
                        "app_start_experiment",
                        "app_run_depth_backtest",
                        "app_start_depth_backtest",
                        "app_run_multi_leg_backtest",
                        "app_start_multi_leg_backtest",
                        "app_verify_depth_run",
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
        experiment = _parser().parse_args(["run-experiment", "experiment.json"])
        self.assertEqual(experiment.command, "run-experiment")
        self.assertEqual(experiment.input, "experiment.json")
        depth = _parser().parse_args(["depth-backtest", "depth.json"])
        self.assertEqual(depth.command, "depth-backtest")
        self.assertEqual(depth.input, "depth.json")

    def test_doctor_reports_all_native_multi_leg_entrypoints(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import doctor
        from qianxing_bridge import native

        with patch.object(native, "available", return_value=True), patch.object(
            native, "app_available", return_value=True
        ):
            report = doctor()
        self.assertIn("backtest.multi-leg.bar.v1", report["implemented_use_cases"])
        self.assertIn(
            "backtest.multi-leg.bar.run-handle.v1", report["implemented_use_cases"]
        )
        self.assertFalse(any("multi-leg" in item for item in report["limitations"]))

    def test_paper_and_live_clients_submit_the_shared_rust_control_contract(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import LiveClient, PaperClient, TradeCommand

        class Response:
            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def read(self):
                return b'{"status":"Accepted","command_id":41}'

        command = TradeCommand(
            command_id=41,
            request_id="sdk-41",
            operator_id="untrusted-body-identity",
            reason="SDK contract test",
            kind="PauseStrategy",
            target="strategy-1",
            payload={},
        )
        with patch("qianxing.app.urlopen", return_value=Response()) as open_url:
            paper_receipt = PaperClient(
                "https://localhost:8443", headers={"Authorization": "Bearer paper-token"}
            ).submit(command)
            live_receipt = LiveClient(
                "https://localhost:8443", headers={"X-Authenticated-Session": "session-7"}
            ).submit(command)

        self.assertEqual(paper_receipt["status"], "Accepted")
        self.assertEqual(live_receipt["command_id"], 41)
        requests = [call.args[0] for call in open_url.call_args_list]
        self.assertEqual(requests[0].full_url, "https://localhost:8443/app/paper/commands")
        self.assertEqual(requests[1].full_url, "https://localhost:8443/app/live/commands")
        self.assertEqual(json.loads(requests[0].data)["workflow"], "paper")
        self.assertEqual(json.loads(requests[1].data)["command"]["request_id"], "sdk-41")
        self.assertEqual(requests[0].get_header("Authorization"), "Bearer paper-token")
        self.assertEqual(requests[1].get_header("X-authenticated-session"), "session-7")
        with self.assertRaisesRegex(ValueError, "requires HTTPS"):
            LiveClient("http://localhost:8443")
        with self.assertRaisesRegex(ValueError, "Content-Type"):
            PaperClient("http://localhost:8443", headers={"Content-Type": "text/plain"})
        with self.assertRaisesRegex(ValueError, "valid string"):
            PaperClient("http://localhost:8443", headers={"X-Auth": "bad\r\nvalue"})

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

    def test_run_handle_starts_waits_and_takes_result_from_rust(self):
        self._require()
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import start_backtest

        handle = start_backtest(self._spec("py-run-handle", self.output_dir))
        self.assertEqual(handle.run_id, "py-run-handle")
        self.assertEqual(handle.wait(timeout_ms=10_000), "succeeded")
        outcome = handle.result()
        self.assertIsNotNone(outcome)
        self.assertGreater(outcome.fills, 0)
        self.assertIsNone(handle.result(), "outcome is delivered only once")

    def test_parameter_grid_runs_and_compares_candidates_through_rust(self):
        self._require()
        spec = {
            "schema_version": 1,
            "experiment_id": "py-grid",
            "base": self._spec("unused", self.output_dir),
            "parameter_space": [{"name": "fast_window", "values": [1, 2]}],
        }
        result = self.bridge.run_experiment(spec)
        self.assertEqual(result["total_candidates"], 2)
        self.assertEqual(result["completed_candidates"], 2)
        self.assertEqual(result["succeeded_candidates"], 2)
        self.assertEqual(result["failed_candidates"], 0)
        self.assertEqual(len(result["comparison"]["runs"]), 2)
        self.assertTrue(Path(result["artifact_path"]).is_file())

        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import (
            BacktestSpec,
            BuiltinStrategySpec,
            ExperimentParameterSpace,
            RunExperimentSpec,
            run_experiment,
            start_experiment,
        )

        typed = run_experiment(
            RunExperimentSpec(
                experiment_id="py-typed-grid",
                base=BacktestSpec(
                    run_id="unused",
                    instrument="BTCUSDT.BINANCE",
                    bars_path=str(self.root / "deploy" / "qianxing.bar-frame.example.json"),
                    settlement_currency="USDT",
                    initial_cash_raw=100_000_000_000_000,
                    output_dir=str(self.output_dir / "typed"),
                    strategy=BuiltinStrategySpec(
                        kind="sma_cross",
                        strategy_id="py-typed-sma",
                        fast_window=2,
                        slow_window=4,
                    ),
                ),
                parameter_space=(ExperimentParameterSpace("fast_window", (2, 3)),),
            )
        )
        self.assertEqual(typed.completed_candidates, 2)
        self.assertEqual(typed.succeeded_candidates, 2)
        self.assertEqual(typed.failed_candidates, 0)
        self.assertEqual(len(typed.comparison.runs), 2)

        handle = start_experiment(
            RunExperimentSpec(
                experiment_id="py-typed-grid-handle",
                base=BacktestSpec(
                    run_id="unused-handle",
                    instrument="BTCUSDT.BINANCE",
                    bars_path=str(self.root / "deploy" / "qianxing.bar-frame.example.json"),
                    settlement_currency="USDT",
                    initial_cash_raw=100_000_000_000_000,
                    output_dir=str(self.output_dir / "typed-handle"),
                    strategy=BuiltinStrategySpec(
                        kind="sma_cross",
                        strategy_id="py-typed-sma-handle",
                        fast_window=2,
                        slow_window=4,
                    ),
                ),
                parameter_space=(ExperimentParameterSpace("fast_window", (2, 3)),),
            )
        )
        self.assertEqual(handle.wait(timeout_ms=10_000), "succeeded")
        async_result = handle.result()
        self.assertIsNotNone(async_result)
        self.assertEqual(async_result.completed_candidates, 2)
        self.assertIsNone(handle.result())

    def test_l1_tick_and_typed_sdk_use_the_shared_rust_application_kernel(self):
        self._require()
        snapshots = []
        for index in range(48):
            phase = index % 20
            mid = 100 + (phase * 2 if phase < 10 else (20 - phase) * 2)
            snapshots.append(
                {
                    "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                    "ts": 1000 + index * 1000,
                    "sequence": index + 1,
                    "bids": [{"price": (mid - 1) * 1_000_000_000, "qty": 100_000_000_000}],
                    "asks": [{"price": (mid + 1) * 1_000_000_000, "qty": 100_000_000_000}],
                }
            )
        depth_path = self.output_dir / "ticks.json"
        depth_path.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "source": "python-sdk-contract",
                    "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                    "snapshots": snapshots,
                }
            ),
            encoding="utf-8",
        )
        payload = {
            "schema_version": 1,
            "run_id": "python-tick-run",
            "depth_path": str(depth_path),
            "tier": "l1",
            "settlement_currency": "USDT",
            "initial_cash_raw": 1_000_000_000_000_000,
            "fee_bps": 0,
            "latency_snapshots": 0,
            "queue_position_bps": 0,
            "market_impact_bps": 0,
            "output_dir": str(self.output_dir / "depth-runs"),
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": "py-tick-sma",
                "quantity_raw": 1_000_000_000,
                "fast_window": 2,
                "slow_window": 3,
            },
        }
        outcome = self.bridge.run_depth_backtest(payload)
        self.assertEqual(outcome["tier"], "l1")
        self.assertEqual(outcome["equity_points"], 48)
        self.assertGreater(outcome["fills"], 0)
        self.assertTrue(Path(outcome["artifacts"]["run_manifest"]).is_file())

        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import (
            BuiltinStrategySpec,
            DepthBacktestSpec,
            InstrumentId,
            OrderPolicy,
            TradingInstrumentSpec,
            run_depth_backtest,
            start_depth_backtest,
            verify_depth_run,
        )

        typed = run_depth_backtest(
            DepthBacktestSpec(
                run_id="python-typed-tick-run",
                depth_path=str(depth_path),
                tier="l1",
                settlement_currency="USDT",
                initial_cash_raw=1_000_000_000_000_000,
                output_dir=str(self.output_dir / "typed-depth-runs"),
                strategy=BuiltinStrategySpec(
                    kind="sma_cross",
                    strategy_id="py-typed-tick-sma",
                    fast_window=2,
                    slow_window=3,
                ),
                market_spec=TradingInstrumentSpec(
                    instrument=InstrumentId(symbol="BTCUSDT", venue="BINANCE"),
                    product="perpetual",
                    base_currency="BTC",
                    quote_currency="USDT",
                    settlement_currency="USDT",
                    contract_size=1_000_000_000,
                    linear=True,
                    inverse=False,
                    price_tick=1,
                    qty_step=1,
                    min_qty=1,
                    max_leverage=100,
                    maintenance_margin_bps=500,
                    valid_from=1000,
                    valid_to=49000,
                ),
                order_policy=OrderPolicy(margin_mode="cross", leverage=2),
            )
        )
        self.assertEqual(typed.tier, "l1")
        self.assertEqual(typed.equity_points, 48)
        typed_summary = json.loads(Path(typed.artifacts["summary"]).read_text(encoding="utf-8"))
        self.assertEqual(typed_summary["market_spec"]["product"], "perpetual")
        self.assertEqual(typed_summary["order_policy"]["leverage"], 2)
        self.assertTrue(verify_depth_run(typed).verified)

        depth_handle_payload = {**payload, "run_id": "python-tick-handle-run"}
        depth_handle_payload["output_dir"] = str(self.output_dir / "depth-handle-runs")
        handle = start_depth_backtest(depth_handle_payload)
        self.assertEqual(handle.run_id, "python-tick-handle-run")
        self.assertEqual(handle.wait(timeout_ms=10_000), "succeeded")
        depth_result = handle.result()
        self.assertIsNotNone(depth_result)
        self.assertEqual(depth_result.tier, "l1")
        self.assertEqual(depth_result.equity_points, 48)
        self.assertIsNone(handle.result(), "depth outcome is delivered only once")

        from qianxing.cli import main as python_cli

        outcome_file = self.output_dir / "depth-outcome.json"
        outcome_file.write_text(json.dumps(typed.__dict__), encoding="utf-8")
        from contextlib import redirect_stdout
        from io import StringIO

        stdout = StringIO()
        with redirect_stdout(stdout):
            self.assertEqual(python_cli(["verify-depth", str(outcome_file)]), 0)
        self.assertTrue(json.loads(stdout.getvalue())["verified"])

    def test_multi_leg_backtest_uses_the_shared_rust_application_flow_and_handle(self):
        self._require()
        from qianxing_bridge.native import app_available

        self.assertTrue(app_available(), "multi-leg functions must pass the generation probe")
        fixture_root = self.root / "deploy"
        payload = {
            "schema_version": 1,
            "run_id": "python-multi-leg",
            "primary_bars_path": str(fixture_root / "qianxing.bar-frame.pairs-primary.example.json"),
            "reference_bars_path": str(fixture_root / "qianxing.bar-frame.pairs-reference.example.json"),
            "settlement_currency": "USDT",
            "primary_initial_cash_raw": 1_000_000_000_000_000,
            "reference_initial_cash_raw": 1_000_000_000_000_000,
            "seed": 7,
            "output_dir": str(self.output_dir / "multi-leg"),
            "strategy": {
                "kind": "pairs_arbitrage",
                "strategy_id": "py-pairs",
                "quantity_raw": 1_000_000_000,
                "fast_window": 5,
                "slow_window": 20,
                "period": 14,
                "threshold_bps": 100,
            },
        }
        first = self.bridge.run_multi_leg_backtest(payload)
        second = self.bridge.run_multi_leg_backtest(payload)
        self.assertEqual(first, second)
        self.assertGreater(first["primary_fills"], 0)
        self.assertGreater(first["reference_fills"], 0)
        self.assertTrue(Path(first["attribution_artifact"]).is_file())

        handle_payload = {**payload, "run_id": "python-multi-leg-handle"}
        handle_payload["output_dir"] = str(self.output_dir / "multi-leg-handle")
        handle = self.bridge.start_multi_leg_backtest(handle_payload)
        self.assertEqual(handle.run_id, "python-multi-leg-handle")
        self.assertEqual(handle.wait(10_000), "succeeded")
        self.assertIsNotNone(handle.result_json())
        self.assertIsNone(handle.result_json(), "multi-leg result is delivered once")

        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing import (
            BuiltinStrategySpec,
            InstrumentId,
            MultiLegBacktestSpec,
            TradingInstrumentSpec,
            run_multi_leg_backtest,
            start_multi_leg_backtest,
        )

        typed_spec = MultiLegBacktestSpec(
            run_id="python-typed-multi-leg",
            primary_bars_path=payload["primary_bars_path"],
            reference_bars_path=payload["reference_bars_path"],
            settlement_currency="USDT",
            primary_initial_cash_raw=1_000_000_000_000_000,
            reference_initial_cash_raw=1_000_000_000_000_000,
            output_dir=str(self.output_dir / "typed-multi-leg"),
            seed=7,
            strategy=BuiltinStrategySpec(
                kind="pairs_arbitrage",
                strategy_id="py-typed-pairs",
                quantity_raw=1_000_000_000,
                fast_window=5,
                slow_window=20,
                period=14,
                threshold_bps=100,
            ),
            primary_market_spec=TradingInstrumentSpec(
                instrument=InstrumentId(symbol="BTCUSDT", venue="BINANCE"),
                product="spot",
                base_currency="BTC",
                quote_currency="USDT",
                settlement_currency="USDT",
                contract_size=1_000_000_000,
                linear=True,
                inverse=False,
                price_tick=1_000,
                qty_step=1,
                min_qty=1,
                max_leverage=1,
                maintenance_margin_bps=0,
                valid_from=0,
            ),
            reference_market_spec=TradingInstrumentSpec(
                instrument=InstrumentId(symbol="ETHUSDT", venue="BINANCE"),
                product="spot",
                base_currency="ETH",
                quote_currency="USDT",
                settlement_currency="USDT",
                contract_size=1_000_000_000,
                linear=True,
                inverse=False,
                price_tick=1_000,
                qty_step=1,
                min_qty=1,
                max_leverage=1,
                maintenance_margin_bps=0,
                valid_from=0,
            ),
            fill_model="one_tick_slippage",
        )
        typed_outcome = run_multi_leg_backtest(typed_spec)
        self.assertGreater(typed_outcome.primary_fills, 0)
        typed_handle = start_multi_leg_backtest(
            MultiLegBacktestSpec(
                **{
                    **typed_spec.__dict__,
                    "run_id": "python-typed-multi-leg-handle",
                    "output_dir": str(self.output_dir / "typed-multi-leg-handle"),
                }
            )
        )
        self.assertEqual(typed_handle.wait(10_000), "succeeded")
        self.assertIsNotNone(typed_handle.result())
        self.assertIsNone(typed_handle.result(), "typed multi-leg result is delivered once")

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
