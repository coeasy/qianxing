"""The Python strategy chooser must mirror and execute the Rust catalog."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from typing import get_args, get_type_hints

from qianxing.app import (
    BacktestSpec,
    BuiltinStrategySpec,
    ExperimentParameterRange,
    ExperimentParameterSpace,
    list_builtin_strategies,
    run_backtest,
    verify_run,
)
from qianxing_bridge import native


class ParameterTypingContractTest(unittest.TestCase):

    def test_typed_parameter_names_cover_rust_sizing_and_depth_maker_dimensions(self) -> None:
        names = set(get_args(get_type_hints(ExperimentParameterSpace)["name"]))
        self.assertEqual(
            names,
            {
                "fast_window",
                "slow_window",
                "period",
                "threshold_bps",
                "quantity_raw",
                "target_notional_raw",
                "target_available_margin_bps",
                "target_volatility_bps",
                "target_kelly_fraction_bps",
                "market_making_spread_bps",
                "market_making_quote_qty_raw",
                "market_making_max_inventory_raw",
                "market_making_tick_size_raw",
                "market_making_inventory_skew_bps",
            },
        )

    def test_typed_continuous_parameter_names_cover_fixed_point_sizing_dimensions(self) -> None:
        names = set(get_args(get_type_hints(ExperimentParameterRange)["name"]))
        self.assertEqual(
            names,
            {
                "fast_window",
                "slow_window",
                "period",
                "threshold_bps",
                "quantity_raw",
                "target_notional_raw",
                "target_available_margin_bps",
                "target_volatility_bps",
                "target_kelly_fraction_bps",
            },
        )


class StrategyCatalogTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        if not native.available():
            raise unittest.SkipTest("build qx-python native extension to query Rust strategy catalog")

    def test_python_catalog_matches_typed_choices_and_rust_backtest_partitions(self) -> None:
        catalog = list_builtin_strategies()
        names = {item.name for item in catalog}
        typed_choices = set(get_args(get_type_hints(BuiltinStrategySpec)["kind"]))
        self.assertEqual(names, typed_choices)
        self.assertEqual(len(catalog), 17)
        self.assertEqual(
            {item.name for item in catalog if item.requires_reference_leg},
            {
                "pairs_arbitrage",
                "basis_arbitrage",
                "cross_venue_arbitrage",
                "spot_futures_arbitrage",
            },
        )
        self.assertEqual(next(item for item in catalog if item.name == "macd").signal_knobs, ())
        self.assertTrue(all(item.description.strip() for item in catalog))

    def test_selecting_catalog_entry_runs_and_verifies_shared_rust_backtest(self) -> None:
        selected = next(
            item for item in list_builtin_strategies() if item.name == "ema_cross"
        )
        repository_root = Path(__file__).resolve().parents[2]
        bars_path = repository_root / "deploy" / "qianxing.bar-frame.example.json"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            spec = BacktestSpec(
                run_id="catalog-selected-ema",
                instrument="BTCUSDT.BINANCE",
                bars_path=str(bars_path),
                settlement_currency="USDT",
                initial_cash_raw=100_000_000_000_000,
                output_dir=str(root / "run"),
                strategy=BuiltinStrategySpec(
                    kind=selected.name,
                    strategy_id=f"catalog-{selected.name}",
                    fast_window=2,
                    slow_window=3,
                ),
            )
            outcome = run_backtest(spec)
            self.assertEqual(outcome.run_id, spec.run_id)
            self.assertTrue(verify_run(outcome).verified)


if __name__ == "__main__":
    unittest.main()
