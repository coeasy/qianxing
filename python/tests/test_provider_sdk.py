"""Python provider adapters must terminate in Rust validation and kernel execution."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from qianxing.app import BacktestSpec, BuiltinStrategySpec
from qianxing.data import (
    BAR_JSON_CONTENT_TYPE,
    ProviderCapability,
    ProviderManualInterventionError,
    ProviderPayloadRow,
    ProviderQuery,
    ProviderRegistry,
    ProviderResponse,
)
from qianxing_bridge import native


class _Bars:
    def __init__(self, capability: ProviderCapability, rows: tuple[ProviderPayloadRow, ...]):
        self.capability = capability
        self.rows = rows

    def fetch(self, query_json: str) -> str:
        query = json.loads(query_json)
        selected = tuple(
            row for row in self.rows if query["start"] <= row.event_time <= query["end"]
        )
        return ProviderResponse(
            self.capability.provider_id,
            self.capability.version,
            f"req-{self.capability.provider_id}",
            max((row.receive_time for row in selected), default=query["end"]),
            selected,
        ).to_json()


def _capability(provider_id: str, priority: int) -> ProviderCapability:
    return ProviderCapability(
        provider_id=provider_id,
        version="v1",
        data_kinds=("Bar",),
        asset_classes=("crypto",),
        frequencies=("1m",),
        auth_scope="public",
        rate_limit_per_second=10,
        freshness_seconds=60,
        historical_start=1,
        historical_end=10_000,
        realtime=False,
        priority=priority,
        quality_score=100,
        cost_score=1,
    )


def _rows() -> tuple[ProviderPayloadRow, ...]:
    prices = (100, 101, 99, 102, 104, 103)
    return tuple(
        ProviderPayloadRow(
            event_time=index,
            receive_time=index,
            instrument="BTCUSDT.BINANCE",
            content_type=BAR_JSON_CONTENT_TYPE,
            json=json.dumps(
                {
                    "ts": index,
                    "open": price,
                    "high": price + 2,
                    "low": price - 2,
                    "close": price,
                    "volume": 1_000,
                },
                separators=(",", ":"),
            ),
        )
        for index, price in enumerate(prices, 1)
    )


class ProviderSdkTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        if not native.available():
            raise unittest.SkipTest("build qx-python native extension to run provider SDK tests")

    def _query(self) -> ProviderQuery:
        return ProviderQuery(
            kind="Bar",
            asset_class="crypto",
            frequency="1m",
            start=1,
            end=6,
            instrument_set=("BTCUSDT.BINANCE",),
        )

    def test_rust_registry_failover_and_barframe_quality_gate(self) -> None:
        registry = ProviderRegistry()
        primary = _capability("primary", 0)
        secondary = _capability("secondary", 1)

        class Unavailable:
            def fetch(self, _query_json: str) -> str:
                raise RuntimeError("temporarily unavailable")

        registry.register(primary, Unavailable())
        registry.register(secondary, _Bars(secondary, _rows()))
        query = self._query()
        self.assertEqual(registry.candidates(query), ("primary", "secondary"))
        result = registry.fetch(query)
        self.assertEqual(result["provider_id"], "secondary")
        self.assertEqual(result["retry_chain"], ["primary", "secondary"])
        frame = registry.fetch_bar_frame(query)
        self.assertEqual(frame["instrument"], "BTCUSDT.BINANCE")
        self.assertIn("#query=", frame["source"])

    def test_manual_intervention_does_not_fail_over(self) -> None:
        registry = ProviderRegistry()
        primary = _capability("primary", 0)
        secondary = _capability("secondary", 1)

        class NeedsAuth:
            def fetch(self, _query_json: str) -> str:
                raise ProviderManualInterventionError("credentials need operator review")

        class MustNotRun:
            def fetch(self, _query_json: str) -> str:
                raise AssertionError("manual intervention must stop failover")

        registry.register(primary, NeedsAuth())
        registry.register(secondary, MustNotRun())
        with self.assertRaisesRegex(ValueError, "ManualIntervention"):
            registry.fetch(self._query())

    def test_invalid_ohlc_is_rejected_by_rust_quality_gate(self) -> None:
        capability = _capability("bad-bars", 0)
        row = ProviderPayloadRow(
            event_time=1,
            receive_time=1,
            instrument="BTCUSDT.BINANCE",
            content_type=BAR_JSON_CONTENT_TYPE,
            json='{"ts":1,"open":100,"high":99,"low":98,"close":100,"volume":5}',
        )
        registry = ProviderRegistry()
        registry.register(capability, _Bars(capability, (row,)))
        with self.assertRaisesRegex(ValueError, "质量门"):
            registry.fetch_bar_frame(self._query())

    def test_provider_result_to_shared_rust_backtest(self) -> None:
        capability = _capability("fixture", 0)
        registry = ProviderRegistry()
        registry.register(capability, _Bars(capability, _rows()))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            spec = BacktestSpec(
                run_id="provider-sdk-run",
                instrument="BTCUSDT.BINANCE",
                bars_path=str(root / "bars.json"),
                settlement_currency="USDT",
                initial_cash_raw=100_000_000_000_000,
                output_dir=str(root / "run"),
                strategy=BuiltinStrategySpec(
                    kind="sma_cross", strategy_id="provider-sdk", fast_window=2, slow_window=3
                ),
            )
            outcome = registry.run_backtest(spec, self._query())
            self.assertEqual(outcome.run_id, spec.run_id)
            frozen = Path(spec.bars_path).read_text(encoding="utf-8")
            self.assertIn("#query=", json.loads(frozen)["source"])
            with self.assertRaises(FileExistsError):
                registry.run_backtest(
                    spec,
                    ProviderQuery(
                        kind="Bar",
                        asset_class="crypto",
                        frequency="1m",
                        start=1,
                        end=5,
                        instrument_set=("BTCUSDT.BINANCE",),
                    ),
                )


if __name__ == "__main__":
    unittest.main()
