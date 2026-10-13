"""Python provider adapters must terminate in Rust validation and kernel execution."""

from __future__ import annotations

import json
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path

from qianxing.app import (
    BacktestSpec,
    BuiltinStrategySpec,
    DepthBacktestSpec,
    InstrumentId,
    MultiLegBacktestSpec,
    TradingInstrumentSpec,
    verify_depth_run,
    verify_run,
)
from qianxing.data import (
    BAR_JSON_CONTENT_TYPE,
    FUNDING_RATE_JSON_CONTENT_TYPE,
    ORDER_BOOK_SNAPSHOT_JSON_CONTENT_TYPE,
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
        from python.tests.test_app_use_cases import _load_native

        _load_native()
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

    def test_auth_scope_realtime_and_freshness_flow_through_rust_selection(self) -> None:
        registry = ProviderRegistry()
        public = _capability("public", 0)
        private_history = replace(
            _capability("private-history", 1), auth_scope="account:read"
        )
        primary = replace(
            _capability("private-primary", 2),
            auth_scope="account:read",
            realtime=True,
        )
        backup = replace(
            _capability("private-backup", 3),
            auth_scope="account:read",
            realtime=True,
        )

        stale_rows = tuple(row for row in _rows() if row.event_time in (1, 6))
        fresh_rows = tuple(row for row in _rows() if row.event_time in (5, 6))
        registry.register(public, _Bars(public, _rows()))
        registry.register(private_history, _Bars(private_history, _rows()))
        registry.register(primary, _Bars(primary, stale_rows))
        registry.register(backup, _Bars(backup, fresh_rows))
        query = replace(
            self._query(),
            as_of=6,
            required_auth_scope="account:read",
            require_realtime=True,
            max_staleness_ms=2,
        )

        self.assertEqual(registry.candidates(query), ("private-primary", "private-backup"))
        result = registry.fetch(query)
        self.assertEqual(result["provider_id"], "private-backup")
        self.assertEqual(result["retry_chain"], ["private-primary", "private-backup"])

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

    def test_financial_provider_funding_flows_into_multi_leg_rust_backtest(self) -> None:
        primary = "BTCUSDT.BINANCE"
        rows = (
            ProviderPayloadRow(
                event_time=144_000_000,
                receive_time=172_800_000,
                instrument=primary,
                content_type=FUNDING_RATE_JSON_CONTENT_TYPE,
                json='{"rate_bps":100}',
            ),
            ProviderPayloadRow(
                event_time=172_800_000,
                receive_time=172_800_000,
                instrument=primary,
                content_type=FUNDING_RATE_JSON_CONTENT_TYPE,
                json='{"rate_bps":-50}',
            ),
        )
        capability = ProviderCapability(
            provider_id="funding-history",
            version="v1",
            data_kinds=("Financial",),
            asset_classes=("crypto",),
            frequencies=("funding_8h",),
            auth_scope="public",
            rate_limit_per_second=10,
            freshness_seconds=60,
            historical_start=1,
            historical_end=200_000_000,
            realtime=False,
            priority=1,
            quality_score=100,
            cost_score=1,
        )
        registry = ProviderRegistry()
        primary_capability = replace(capability, provider_id="funding-primary", priority=0)

        class UnavailableFunding:
            def fetch(self, _query_json: str) -> str:
                raise RuntimeError("funding source temporarily unavailable")

        registry.register(primary_capability, UnavailableFunding())
        registry.register(capability, _Bars(capability, rows))
        query = ProviderQuery(
            kind="Financial",
            asset_class="crypto",
            frequency="funding_8h",
            start=144_000_000,
            end=172_800_000,
            instrument_set=(primary,),
            field_set=("funding_rate_bps",),
        )

        def derivative(instrument: str) -> TradingInstrumentSpec:
            symbol, venue = instrument.split(".")
            return TradingInstrumentSpec(
                instrument=InstrumentId(symbol=symbol, venue=venue),
                product="perpetual",
                base_currency=symbol[:3],
                quote_currency=symbol[3:],
                settlement_currency="USDT",
                contract_size=1_000_000_000,
                linear=True,
                inverse=False,
                price_tick=1_000,
                qty_step=1,
                min_qty=1,
                max_leverage=20,
                maintenance_margin_bps=500,
                valid_from=0,
            )

        fixture = Path(__file__).resolve().parents[2] / "deploy"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            spec = MultiLegBacktestSpec(
                run_id="provider-funding-python",
                primary_bars_path=str(fixture / "qianxing.bar-frame.pairs-primary.example.json"),
                reference_bars_path=str(fixture / "qianxing.bar-frame.pairs-reference.example.json"),
                settlement_currency="USDT",
                primary_initial_cash_raw=1_000_000_000_000_000,
                reference_initial_cash_raw=1_000_000_000_000_000,
                output_dir=str(root / "run"),
                seed=7,
                strategy=BuiltinStrategySpec(
                    kind="pairs_arbitrage",
                    strategy_id="provider-funding-python",
                    quantity_raw=1_000_000_000,
                    fast_window=5,
                    slow_window=20,
                    period=14,
                    threshold_bps=100,
                ),
                primary_market_spec=derivative(primary),
                reference_market_spec=derivative("ETHUSDT.BINANCE"),
            )
            outcome = registry.run_multi_leg_backtest_with_funding(spec, query)
            artifact = json.loads(
                Path(outcome.attribution_artifact).read_text(encoding="utf-8")
            )
            self.assertEqual(outcome.run_id, spec.run_id)
            self.assertEqual(artifact["funding_rate_input"], "caller_supplied_historical_events")
            self.assertEqual(
                [event["rate_bps"] for event in artifact["funding_rate_events"]],
                [100, -50],
            )
            self.assertEqual(
                artifact["funding_provider_provenance"]["provider_id"], "funding-history"
            )
            self.assertEqual(
                artifact["funding_provider_provenance"]["retry_chain"],
                ["funding-primary", "funding-history"],
            )
            self.assertEqual(artifact["funding_provider_provenance"]["record_count"], 2)
            self.assertEqual(len(artifact["execution_funding_events"]), 2)

    def test_provider_builds_and_validates_dataset_manifest_before_rust_backtest(self) -> None:
        capability = _capability("manifest-provider", 0)
        registry = ProviderRegistry()
        registry.register(capability, _Bars(capability, _rows()))
        query = self._query()
        frame, manifest = registry.fetch_bar_dataset(
            query,
            dataset_id="provider-bars",
            dataset_version="source-v1",
            interval_ms=1,
            timezone="UTC",
        )
        self.assertEqual(manifest["manifest_version"], 1)
        self.assertRegex(manifest["dataset"]["fingerprint"], r"^[0-9a-f]{16}$")
        self.assertEqual(frame["instrument"], "BTCUSDT.BINANCE")
        self.assertEqual(manifest["quality_report"]["row_count"], len(_rows()))
        self.assertEqual(manifest["quality_report"]["missing_intervals"], 0)
        self.assertEqual(manifest["source_lineage"]["primary_source"], "manifest-provider")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            spec = BacktestSpec(
                run_id="provider-manifest-run",
                instrument="BTCUSDT.BINANCE",
                bars_path=str(root / "bars.json"),
                settlement_currency="USDT",
                initial_cash_raw=100_000_000_000_000,
                output_dir=str(root / "run"),
                strategy=BuiltinStrategySpec(
                    kind="sma_cross", strategy_id="manifest-sdk", fast_window=2, slow_window=3
                ),
            )
            result = registry.run_backtest_with_manifest(
                spec, query, "provider-bars", "source-v1", interval_ms=1
            )
            self.assertEqual(result.dataset_verdict.dataset_id, "provider-bars")
            self.assertEqual(result.manifest["dataset"]["fingerprint"], manifest["dataset"]["fingerprint"])
            self.assertTrue(Path(result.manifest_path).is_file())
            self.assertTrue(verify_run(result.outcome).verified)

    def test_quote_provider_to_depth_frame_and_shared_rust_tick_backtest(self) -> None:
        fixture = json.loads(
            (Path(__file__).resolve().parents[2] / "deploy" / "qianxing.depth-frame.l1.example.json")
            .read_text(encoding="utf-8")
        )
        instrument = f"{fixture['instrument']['symbol']}.{fixture['instrument']['venue']}"
        rows = tuple(
            ProviderPayloadRow(
                event_time=snapshot["ts"],
                receive_time=snapshot["ts"],
                instrument=instrument,
                content_type=ORDER_BOOK_SNAPSHOT_JSON_CONTENT_TYPE,
                json=json.dumps(snapshot, sort_keys=True, separators=(",", ":")),
            )
            for snapshot in fixture["snapshots"]
        )
        capability = ProviderCapability(
            provider_id="quote-fixture",
            version="v1",
            data_kinds=("Quote",),
            asset_classes=("crypto",),
            frequencies=("tick",),
            auth_scope="public",
            rate_limit_per_second=10,
            freshness_seconds=60,
            historical_start=rows[0].event_time,
            historical_end=rows[-1].event_time,
            realtime=False,
            priority=0,
            quality_score=100,
            cost_score=1,
        )
        registry = ProviderRegistry()
        registry.register(capability, _Bars(capability, rows))
        query = ProviderQuery(
            kind="Quote",
            asset_class="crypto",
            frequency="tick",
            start=rows[0].event_time,
            end=rows[-1].event_time,
            instrument_set=(instrument,),
        )
        frame = registry.fetch_depth_frame(query, "l1")
        self.assertRegex(frame["source"], r"^quote-fixture#query=[0-9a-f]{16}$")
        self.assertEqual(len(frame["snapshots"]), len(rows))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            spec = DepthBacktestSpec(
                run_id="provider-sdk-depth-run",
                depth_path=str(root / "depth.json"),
                tier="l1",
                settlement_currency="USDT",
                initial_cash_raw=1_000_000_000_000_000,
                output_dir=str(root / "run"),
                strategy=BuiltinStrategySpec(
                    kind="sma_cross",
                    strategy_id="provider-depth-sdk",
                    fast_window=2,
                    slow_window=3,
                ),
            )
            outcome = registry.run_depth_backtest_from_frame(spec, query, "l1")
            self.assertTrue(verify_depth_run(outcome).verified)

            provider_spec = replace(
                spec,
                run_id="provider-sdk-depth-provider-run",
                depth_path=str(root / "provider-depth.json"),
                output_dir=str(root / "provider-run"),
            )
            provider_outcome = registry.run_depth_backtest(provider_spec, query)
            self.assertEqual(provider_outcome.provider_id, "quote-fixture")
            self.assertEqual(provider_outcome.retry_chain, ("quote-fixture",))
            self.assertTrue(verify_depth_run(provider_outcome.outcome).verified)

    def test_depth_provider_rejects_event_time_mismatch(self) -> None:
        fixture = json.loads(
            (Path(__file__).resolve().parents[2] / "deploy" / "qianxing.depth-frame.l1.example.json")
            .read_text(encoding="utf-8")
        )
        snapshot = fixture["snapshots"][0]
        instrument = f"{fixture['instrument']['symbol']}.{fixture['instrument']['venue']}"
        row = ProviderPayloadRow(
            event_time=snapshot["ts"] + 1,
            receive_time=snapshot["ts"] + 1,
            instrument=instrument,
            content_type=ORDER_BOOK_SNAPSHOT_JSON_CONTENT_TYPE,
            json=json.dumps(snapshot, sort_keys=True, separators=(",", ":")),
        )
        capability = ProviderCapability(
            provider_id="bad-quote",
            version="v1",
            data_kinds=("Quote",),
            asset_classes=("crypto",),
            frequencies=("tick",),
            auth_scope="public",
            rate_limit_per_second=10,
            freshness_seconds=60,
            historical_start=row.event_time,
            historical_end=row.event_time,
            realtime=False,
            priority=0,
            quality_score=100,
            cost_score=1,
        )
        registry = ProviderRegistry()
        registry.register(capability, _Bars(capability, (row,)))
        query = ProviderQuery(
            kind="Quote",
            asset_class="crypto",
            frequency="tick",
            start=row.event_time,
            end=row.event_time,
            instrument_set=(instrument,),
        )
        with self.assertRaisesRegex(ValueError, "event timestamp"):
            registry.fetch_depth_frame(query, "l1")


if __name__ == "__main__":
    unittest.main()
