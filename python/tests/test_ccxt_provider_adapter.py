"""CCXT history must flow through the Rust Provider and backtest contracts."""

from __future__ import annotations

import tempfile
import unittest
from dataclasses import replace
from pathlib import Path

from qianxing.app import BacktestSpec, BuiltinStrategySpec, verify_run
from qianxing.data import ProviderCapability, ProviderQuery, ProviderRegistry
from qianxing_bridge import BarFrame, native
from qianxing_ccxt import (
    CcxtConnectorError,
    CcxtErrorClass,
    CcxtProviderAdapter,
    register_ccxt_provider,
)


class _FakeCcxtClient:
    def __init__(self, frame: BarFrame):
        self.frame = frame
        self.calls: list[tuple[str, dict[str, int | str]]] = []

    def fetch_ohlcv(self, instrument: str, *, timeframe: str, start_ms: int, end_ms: int) -> BarFrame:
        self.calls.append((instrument, {"timeframe": timeframe, "start_ms": start_ms, "end_ms": end_ms}))
        return self.frame


def _fixture() -> tuple[ProviderCapability, ProviderQuery, BarFrame]:
    instrument = "BTCUSDT.BINANCE"
    start = 1_700_000_000_000
    frame = BarFrame(
        instrument=instrument,
        source="binance",
        ts=tuple(start + index * 60_000 for index in range(6)),
        open_raw=(100, 101, 99, 102, 104, 103),
        high_raw=(102, 103, 101, 104, 106, 105),
        low_raw=(98, 99, 97, 100, 102, 101),
        close_raw=(100, 101, 99, 102, 104, 103),
        volume_raw=(1_000,) * 6,
    )
    capability = ProviderCapability(
        provider_id="ccxt:binance",
        version="v1",
        data_kinds=("Bar",),
        asset_classes=("crypto",),
        frequencies=("1m",),
        auth_scope="public",
        rate_limit_per_second=10,
        freshness_seconds=60,
        historical_start=start,
        historical_end=start + 5 * 60_000,
        realtime=False,
        priority=0,
        quality_score=100,
        cost_score=1,
    )
    query = ProviderQuery(
        kind="Bar",
        asset_class="crypto",
        frequency="1m",
        start=start,
        end=start + 5 * 60_000,
        instrument_set=(instrument,),
    )
    return capability, query, frame


class CcxtProviderAdapterTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        if not native.available():
            raise unittest.SkipTest("build qx-python native extension to run CCXT Provider tests")

    def test_ccxt_history_enters_rust_registry_and_manifest_backtest(self) -> None:
        capability, query, frame = _fixture()
        client = _FakeCcxtClient(frame)
        registry = ProviderRegistry()
        adapter = register_ccxt_provider(
            registry, capability, client, clock_ms=lambda: 1_800_000_000_000
        )

        self.assertIsInstance(adapter, CcxtProviderAdapter)
        self.assertEqual(registry.candidates(query), ("ccxt:binance",))
        normalized = registry.fetch_bar_frame(query)
        self.assertEqual(normalized["instrument"], frame.instrument)
        self.assertEqual(len(normalized["ts"]), 6)
        self.assertEqual(client.calls[0][1]["timeframe"], "1m")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            spec = BacktestSpec(
                run_id="ccxt-provider-run",
                instrument=frame.instrument,
                bars_path=str(root / "bars.json"),
                settlement_currency="USDT",
                initial_cash_raw=100_000_000_000_000,
                output_dir=str(root / "run"),
                strategy=BuiltinStrategySpec(
                    kind="sma_cross", strategy_id="ccxt-provider", fast_window=2, slow_window=3
                ),
            )
            result = registry.run_backtest_with_manifest(
                spec, query, "binance-history", "v1", interval_ms=60_000
            )
            self.assertEqual(result.manifest["source_lineage"]["primary_source"], "ccxt:binance")
            self.assertTrue(Path(result.manifest_path).is_file())
            self.assertTrue(verify_run(result.outcome).verified)

    def test_ccxt_auth_stops_failover_and_network_errors_allow_it(self) -> None:
        capability, query, frame = _fixture()

        class BrokenClient:
            def __init__(self, error_class: CcxtErrorClass):
                self.error_class = error_class

            def fetch_ohlcv(self, *_args: object, **_kwargs: object) -> BarFrame:
                raise CcxtConnectorError(self.error_class, "fixture failure")

        registry = ProviderRegistry()
        primary = replace(capability, provider_id="ccxt:primary", priority=0)
        fallback = replace(capability, provider_id="ccxt:fallback", priority=1)
        registry.register(
            primary,
            CcxtProviderAdapter(
                primary.provider_id,
                primary.version,
                BrokenClient(CcxtErrorClass.AUTHENTICATION),
                clock_ms=lambda: 1_800_000_000_000,
            ),
        )
        registry.register(
            fallback,
            CcxtProviderAdapter(
                fallback.provider_id,
                fallback.version,
                _FakeCcxtClient(frame),
                clock_ms=lambda: 1_800_000_000_000,
            ),
        )
        with self.assertRaisesRegex(ValueError, "ManualIntervention"):
            registry.fetch(query)

        registry = ProviderRegistry()
        registry.register(
            primary,
            CcxtProviderAdapter(
                primary.provider_id,
                primary.version,
                BrokenClient(CcxtErrorClass.RETRYABLE),
                clock_ms=lambda: 1_800_000_000_000,
            ),
        )
        registry.register(
            fallback,
            CcxtProviderAdapter(
                fallback.provider_id,
                fallback.version,
                _FakeCcxtClient(frame),
                clock_ms=lambda: 1_800_000_000_000,
            ),
        )
        result = registry.fetch(query)
        self.assertEqual(result["provider_id"], "ccxt:fallback")


if __name__ == "__main__":
    unittest.main()
