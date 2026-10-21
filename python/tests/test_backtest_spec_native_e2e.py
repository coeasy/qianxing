"""Exercise the typed Python Bar spec through the Rust application boundary."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing.app import (  # noqa: E402
    AshareBacktestRules,
    BacktestExecutionCostRules,
    BacktestNoMargin,
    BacktestRiskRules,
    BacktestSpec,
    BuiltinStrategySpec,
    InvalidInputError,
    InstrumentId,
    MultiLegBacktestSpec,
    TradingInstrumentSpec,
    run_multi_leg_backtest,
    run_backtest,
)


ROOT = Path(__file__).resolve().parents[2]


class BacktestSpecNativeEndToEndTest(unittest.TestCase):
    def test_python_multileg_manifest_paths_reach_rust_pair_validation(self):
        spec = MultiLegBacktestSpec(
            run_id="python-native-multileg-manifest-pair",
            primary_bars_path="primary.json",
            reference_bars_path="reference.json",
            settlement_currency="USDT",
            primary_initial_cash_raw=100_000_000_000,
            reference_initial_cash_raw=100_000_000_000,
            output_dir="runs",
            strategy=BuiltinStrategySpec(kind="pairs_arbitrage", strategy_id="pair"),
            primary_dataset_manifest_path="primary-quality.json",
        )
        with self.assertRaisesRegex(InvalidInputError, "primary/reference"):
            run_multi_leg_backtest(spec)

    def test_python_spec_options_are_consumed_by_rust_ashare_backtest(self):
        rules_path = ROOT / "deploy" / "qianxing.ashare.rules.json"
        ashare_rules = json.loads(rules_path.read_text(encoding="utf-8"))

        with tempfile.TemporaryDirectory(prefix="qx-python-native-backtest-") as directory:
            scale = 1_000_000_000
            tick = 10_000_000
            closes = []
            for index in range(60):
                phase = index % 20
                cents = 1_000 + (phase if phase < 10 else 20 - phase)
                closes.append(cents * scale // 100)
            bars = {
                "schema_version": 1,
                "instrument": "000001.SZSE",
                "source": "python-native-ashare-contract-v1",
                "ts": [1_717_378_200_000 + index * 60_000 for index in range(len(closes))],
                "open_raw": closes,
                "high_raw": [price + tick for price in closes],
                "low_raw": [price - tick for price in closes],
                "close_raw": closes,
                "volume_raw": [4_000 * scale] * len(closes),
            }
            bars_path = Path(directory) / "ashare-bars.json"
            bars_path.write_text(json.dumps(bars), encoding="utf-8")
            output_dir = Path(directory) / "artifacts"
            spec = BacktestSpec(
                run_id="python-native-ashare-contract",
                instrument="000001.SZSE",
                bars_path=str(bars_path),
                settlement_currency="CNY",
                initial_cash_raw=100_000_000_000_000,
                output_dir=str(output_dir),
                strategy=BuiltinStrategySpec(
                    kind="sma_cross",
                    strategy_id="python-native-ashare-contract",
                    quantity_raw=1_000_000_000_000,
                    fast_window=2,
                    slow_window=3,
                ),
                market_spec=TradingInstrumentSpec(
                    instrument=InstrumentId(symbol="000001", venue="SZSE"),
                    product="spot",
                    base_currency="000001",
                    quote_currency="CNY",
                    settlement_currency="CNY",
                    contract_size=1_000_000_000,
                    linear=True,
                    inverse=False,
                    price_tick=10_000_000,
                    qty_step=100_000_000_000,
                    min_qty=100_000_000_000,
                    max_leverage=1,
                    maintenance_margin_bps=0,
                    valid_from=0,
                ),
                margin_model=BacktestNoMargin(),
                execution_cost_rules=BacktestExecutionCostRules(
                    name="ashare-python-e2e",
                    maker_bp=3,
                    taker_bp=12,
                    latency_base_ns=1_000_000,
                ),
                ashare_rules=AshareBacktestRules(**ashare_rules),
                fill_model="best_price",
                max_bar_volume_participation_bps=2_500,
                risk_rules=BacktestRiskRules(
                    version="python-sdk-risk-v1",
                    max_qty_raw=2_000_000_000_000,
                    no_short=True,
                ),
                allow_short=False,
            )

            outcome = run_backtest(spec)

            self.assertEqual(outcome.run_id, spec.run_id)
            self.assertEqual(outcome.instrument, spec.instrument)
            self.assertTrue(outcome.result_hash)
            self.assertGreater(outcome.equity_points, 0)
            self.assertIn("summary", outcome.artifacts)
            summary = json.loads(Path(outcome.artifacts["summary"]).read_text(encoding="utf-8"))
            self.assertGreater(outcome.fills, 0, f"summary={summary}")
            self.assertTrue(summary["fee_model"].startswith("AShareFee@v1["))
            self.assertEqual(summary["fill_model"], "BarVolumeParticipationLimit")
            self.assertIn("inner=BestPrice", summary["fill_model_details"]["parameters"])
            self.assertEqual(summary["fills"], outcome.fills)


if __name__ == "__main__":
    unittest.main()
