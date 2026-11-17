"""Behavioral contracts for the long-only SMA decision callback."""
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from qianxing_bridge.app import _PythonBacktestCallback
from qianxing_bridge.strategy import StrategyBars, StrategyInput
from qianxing_strategy.long_only_sma import LongOnlySma
from qianxing_strategy.class_adapter import BaseStrategyProcessAdapter


class LongOnlySmaTests(unittest.TestCase):
    @staticmethod
    def parity_fixture():
        path = Path(__file__).resolve().parent / "fixtures" / "long-only-sma-parity-v1.json"
        return json.loads(path.read_text(encoding="utf-8"))

    @staticmethod
    def normalized_intents(decision):
        normalized = []
        for intent in decision["intents"]:
            instrument = intent["instrument"]
            if isinstance(instrument, dict):
                instrument = f"{instrument['symbol']}.{instrument['venue']}"
            qty_exact = intent.get("qty_exact")
            if isinstance(qty_exact, dict):
                from decimal import Decimal

                qty_exact = str(
                    Decimal(qty_exact["coefficient"]).scaleb(-qty_exact["scale"])
                )
            normalized.append(
                {
                    "instrument": instrument,
                    "side": intent["side"].capitalize(),
                    "qty_raw": intent.get("qty_raw", intent.get("qty")),
                    "qty_exact": qty_exact,
                    "reduce_only": intent.get("reduce_only", False),
                }
            )
        return normalized

    def test_shared_event_fixture_matches_local_and_runtime_worker_host(self):
        fixture = self.parity_fixture()
        instrument = fixture["instrument"]
        symbol, venue = instrument.rsplit(".", 1)
        params = fixture["strategy"]
        spec = {"parameters": params}
        local = _PythonBacktestCallback(LongOnlySma(spec))
        runtime = BaseStrategyProcessAdapter(
            LongOnlySma(spec), account_id="main", venue_id=venue
        )
        position_raw = 0
        local_decisions = []
        runtime_decisions = []
        timestamps = [params["start_ts"] + index for index in range(len(fixture["close_raw"]))]
        for index, (timestamp, close_raw) in enumerate(
            zip(timestamps, fixture["close_raw"], strict=True)
        ):
            history = fixture["close_raw"][: index + 1]
            frame = StrategyBars(
                source="long-only-sma-parity-v1",
                ts=tuple(timestamps[: index + 1]),
                open_raw=tuple(history),
                high_raw=tuple(history),
                low_raw=tuple(history),
                close_raw=tuple(history),
                volume_raw=(10**9,) * len(history),
            )
            request = StrategyInput(
                request_id=f"long-only-sma-parity-{timestamp}",
                strategy_id=LongOnlySma.STRATEGY_ID,
                strategy_version=LongOnlySma.STRATEGY_VERSION,
                data_fingerprint="long-only-sma-parity-v1",
                as_of=timestamp,
                instrument=instrument,
                positions={instrument: position_raw},
                available_margin_raw=fixture["available_margin_raw"],
                risk_state="clear",
                bars=frame,
            )
            runtime_output = runtime(request)
            runtime.commit()
            bar = {
                "ts": timestamp,
                "instrument": {"symbol": symbol, "venue": venue},
                "open_raw": close_raw,
                "high_raw": close_raw,
                "low_raw": close_raw,
                "close_raw": close_raw,
                "volume_raw": 10**9,
            }
            context = {
                "strategy_id": LongOnlySma.STRATEGY_ID,
                "strategy_version": LongOnlySma.STRATEGY_VERSION,
                "request_id": request.request_id,
                "account_id": "main",
                "venue_id": venue,
                "instrument": {"symbol": symbol, "venue": venue},
                "data_fingerprint": request.data_fingerprint,
                "as_of": timestamp,
                "positions": {instrument: position_raw},
                "available_margin_raw": fixture["available_margin_raw"],
                "risk_state": "clear",
            }
            local_output = json.loads(
                local._qx_on_event(json.dumps(context), json.dumps({"Bar": bar}))
            )
            local_intents = self.normalized_intents(local_output)
            runtime_intents = self.normalized_intents(runtime_output)
            self.assertEqual(local_intents, runtime_intents, f"Bar {timestamp}")
            local_decisions.append(local_intents)
            runtime_decisions.append(runtime_intents)
            for intent in runtime_intents:
                if intent["side"] == "Buy":
                    position_raw = intent["qty_raw"]
                elif intent["side"] == "Sell":
                    position_raw = 0
        expected = []
        for item in fixture["expected_intents"]:
            expected.append(
                {
                    "bar_index": item["bar_index"],
                    "instrument": instrument,
                    "side": item["side"],
                    "qty_raw": item.get("qty_raw", 0),
                    "qty_exact": item.get("qty_exact"),
                    "reduce_only": item["reduce_only"],
                }
            )
        actual = [
            {
                "bar_index": index,
                **intent,
            }
            for index, intents in enumerate(runtime_decisions)
            for intent in intents
        ]
        self.assertEqual(actual, expected)
        self.assertEqual(local_decisions[: params["slow_window"]], [[], [], []])
        self.assertEqual([item["bar_index"] for item in actual], [3, 5])
        local._qx_on_stop()
        runtime.close()

    def test_shared_fixture_runs_rust_fills_and_closes_the_ledger_position(self):
        import csv
        import tempfile

        from qianxing_bridge.app import run_backtest_with_strategy, verify_run

        fixture = self.parity_fixture()
        scale = 10**9
        closes = fixture["close_raw"]
        # The seventh Bar gives the Rust execution loop a later event on which
        # to fill the reduce-only intent emitted by the sixth fixture Bar.
        execution_closes = [*closes, closes[-1]]
        with tempfile.TemporaryDirectory(prefix="qx-r16-sma-") as temporary:
            output = Path(temporary)
            bars_path = output / "bars.json"
            bars_path.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "instrument": fixture["instrument"],
                        "source": "long-only-sma-parity-v1",
                        "ts": [100 + index for index in range(len(execution_closes))],
                        "open_raw": execution_closes,
                        "high_raw": execution_closes,
                        "low_raw": execution_closes,
                        "close_raw": execution_closes,
                        "volume_raw": [scale] * len(execution_closes),
                    }
                ),
                encoding="utf-8",
            )
            spec = {
                "schema_version": 1,
                "run_id": "r16-long-only-sma-full-execution",
                "instrument": fixture["instrument"],
                "bars_path": str(bars_path),
                "settlement_currency": "USDT",
                "initial_cash_raw": fixture["available_margin_raw"],
                "seed": 20261029,
                "output_dir": str(output / "run"),
                # External callbacks require the default built-in knobs; the
                # Python strategy's own windows and sizing come from the fixture.
                "strategy": {
                    "kind": "sma_cross",
                    "strategy_id": LongOnlySma.STRATEGY_ID,
                    "quantity_raw": scale,
                    "fast_window": 5,
                    "slow_window": 20,
                },
                "market_spec": {
                    "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
                    "product": "spot",
                    "base_currency": "BTC",
                    "quote_currency": "USDT",
                    "settlement_currency": "USDT",
                    "contract_size": scale,
                    "linear": False,
                    "inverse": False,
                    "price_tick": 1,
                    "qty_step": 1,
                    "min_qty": 1,
                    "min_notional_raw": 0,
                    "max_leverage": 1,
                    "maintenance_margin_bps": 0,
                    "valid_from": 0,
                    "valid_to": None,
                    "exact_rules": {
                        "contract_size": {"coefficient": "1", "scale": 0},
                        "price_tick": {"coefficient": "1", "scale": 8},
                        "quantity_step": {"coefficient": "1", "scale": 8},
                        "minimum_quantity": {"coefficient": "1", "scale": 8},
                        "minimum_notional": {"coefficient": "0", "scale": 0},
                    },
                },
                # Match the Paper runtime's configured 5 bps taker fee so this
                # fixture can compare full Ledger effects, not only positions.
                "fee_schedule": {"maker_fee_bps": 0, "taker_fee_bps": 5},
            }
            result = run_backtest_with_strategy(
                spec,
                LongOnlySma({"parameters": fixture["strategy"]}),
                strategy_id=LongOnlySma.STRATEGY_ID,
                strategy_version=LongOnlySma.STRATEGY_VERSION,
                source_sha256="a" * 64,
            )
            verification = verify_run(result)
            self.assertTrue(verification["verified"], verification["mismatches"])
            with Path(result["artifacts"]["fills"]).open(
                encoding="utf-8", newline=""
            ) as stream:
                fills = list(csv.DictReader(stream))
            self.assertEqual(len(fills), 2)
            self.assertEqual(
                [row["qty_raw"] for row in fills],
                ["225000000000", "0"],
            )
            self.assertEqual([row["ts"] for row in fills], ["104", "106"])

            events = [
                json.loads(line)
                for line in Path(result["artifacts"]["event_trace"])
                .read_text(encoding="utf-8")
                .splitlines()
                if line.strip()
            ]
            position_entries = [
                event["kind"]["LedgerApplied"]["entry"]
                for event in events
                if "LedgerApplied" in event.get("kind", {})
                and event["kind"]["LedgerApplied"]["entry"].get("kind")
                == "TradePosition"
            ]
            self.assertEqual(len(position_entries), 2)
            quantities = []
            for entry in position_entries:
                exact_quantity = entry.get("exact_amounts", {}).get("quantity")
                if exact_quantity is not None:
                    quantities.append(int(exact_quantity["coefficient"]))
                else:
                    quantities.append(entry["quantity"] // scale)
            self.assertEqual(quantities, [225, -225])
            self.assertEqual(sum(quantities), 0)

    def decide(self, closes, position=0, available=1000 * 10**9, ts=100):
        strategy = LongOnlySma({"parameters": {"fast_window": 2, "slow_window": 3,
                                                "start_ts": 100, "exposure_bps": 9000}})
        rows = [{"close_raw": value * 10**9} for value in closes]
        instrument = {"symbol": "000300", "venue": "SSE"}
        with patch.object(strategy, "history", return_value=rows), patch.object(strategy, "position_qty", return_value=position), \
                patch.object(strategy, "decision", side_effect=lambda c, k, b, **kw: kw["intents"]), \
                patch.object(strategy, "close_position", return_value={"side": "sell", "reduce_only": True}):
            return strategy.on_bar({"available_margin_raw": available}, {"ts": ts, "instrument": instrument})

    def test_cross_up_buys_only_when_flat_and_sizes_whole_units(self):
        intents = self.decide([3, 2, 1, 4])
        self.assertEqual(len(intents), 1)
        self.assertEqual(intents[0]["side"], "Buy")
        self.assertEqual(intents[0]["qty"], 225 * 10**9)
        self.assertEqual(self.decide([3, 2, 1, 4], position=100), [])

    def test_cross_down_closes_existing_long_never_opens_short(self):
        self.assertEqual(self.decide([1, 3, 4, 1]), [])
        self.assertEqual(self.decide([1, 3, 4, 1], position=100), [{"side": "sell", "reduce_only": True}])

    def test_no_trade_before_start_or_without_history_or_cash(self):
        self.assertEqual(self.decide([3, 2, 1, 4], ts=99), [])
        self.assertEqual(self.decide([3, 2, 1]), [])
        self.assertEqual(self.decide([3, 2, 1, 4], available=0), [])

    def test_short_position_and_bad_parameters_rejected(self):
        with self.assertRaisesRegex(ValueError, "short position"):
            self.decide([3, 2, 1, 4], position=-1)
        for values in ({"fast_window": True}, {"fast_window": 60}, {"exposure_bps": 10001}):
            with self.assertRaises(ValueError):
                LongOnlySma({"parameters": values})


if __name__ == "__main__":
    unittest.main()
