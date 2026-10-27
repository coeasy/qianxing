from __future__ import annotations

import json
from decimal import Decimal
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing_bridge.app import _PythonBacktestCallback
from qianxing_strategy import BaseStrategy
from qianxing_strategy.base import bind_strategy_warmup
from qianxing_strategy.diagnostic_callback_worker import _read_diagnostic_state


class BaseStrategyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.context = {
            "strategy_id": "base-test",
            "strategy_version": "1.0",
            "account_id": "paper",
            "venue_id": "BINANCE",
            "data_fingerprint": "bars:test",
            "as_of": 100,
            "positions": {},
            "cash": {"USDT": 1_000_000},
            "available_margin_raw": None,
            "risk_state": "backtest",
        }

    def test_cash_and_available_margin_helpers_prefer_exact_account_values(self) -> None:
        context = {
            "cash": {"USDT": 100},
            "cash_exact": {"USDT": "0.0000001007"},
            "available_margin_raw": 90,
            "available_margin_exact": "0.0000000909",
        }
        self.assertEqual(BaseStrategy.cash_balance(context, "USDT"), Decimal("0.0000001007"))
        self.assertEqual(BaseStrategy.available_margin(context), Decimal("0.0000000909"))

        legacy = {"cash": {"USDT": 100}, "available_margin_raw": 90}
        self.assertEqual(BaseStrategy.cash_balance(legacy, "USDT"), Decimal("0.0000001"))
        self.assertEqual(BaseStrategy.available_margin(legacy), Decimal("0.00000009"))

    def test_dispatches_tagged_market_events_to_specialized_hooks(self) -> None:
        class Hooks(BaseStrategy):
            def __init__(self) -> None:
                self.events: list[tuple[str, int]] = []

            def on_bar(self, context, bar):
                self.events.append(("bar", bar["ts"]))
                return self.decision(context, "Bar", bar)

            def on_tick(self, context, tick):
                self.events.append(("tick", tick["ts"]))
                return self.decision(context, "Tick", tick)

            def on_order_book(self, context, book):
                self.events.append(("book", book["ts"]))
                return None

            def on_timer(self, context, timer):
                self.events.append(("timer", timer["ts"]))
                return None

        strategy = Hooks()
        events = [
            {"Bar": {"ts": 100, "instrument": "BTCUSDT.BINANCE"}},
            {"Tick": {"ts": 101, "instrument": "BTCUSDT.BINANCE"}},
            {"OrderBook": {"ts": 102, "instrument": "BTCUSDT.BINANCE", "sequence": 1}},
            {"Timer": {"ts": 103, "name": "heartbeat"}},
        ]

        decisions = [strategy.on_event(self.context, event) for event in events]

        self.assertEqual(
            strategy.events,
            [("bar", 100), ("tick", 101), ("book", 102), ("timer", 103)],
        )
        self.assertEqual(decisions[0]["signal_id"], 100)
        self.assertEqual(decisions[1]["signal_id"], 101)
        self.assertEqual(decisions[2]["intents"], [])
        self.assertEqual(decisions[3]["intents"], [])
        self.assertNotEqual(decisions[2]["request_id"], decisions[3]["request_id"])

    def test_timer_hook_matches_the_shared_rust_and_c_abi_fixture(self) -> None:
        fixture_path = (
            Path(__file__).resolve().parents[2]
            / "crates/qx-strategy/tests/fixtures/strategy-market-timer-v1.json"
        )
        event = json.loads(fixture_path.read_text(encoding="utf-8"))

        class TimerStrategy(BaseStrategy):
            received: dict[str, int | str] | None = None

            def on_timer(self, _context, timer):
                self.received = dict(timer)
                return None

        strategy = TimerStrategy()
        decision = strategy.on_event(self.context, event)

        self.assertEqual(strategy.received, {"name": "scheduler", "ts": 1000})
        self.assertEqual(decision["signal_id"], 1000)

    def test_exact_orderbook_hook_matches_the_shared_rust_and_c_abi_fixture(self) -> None:
        fixture_path = (
            Path(__file__).resolve().parents[2]
            / "crates/qx-strategy/tests/fixtures/strategy-market-orderbook-v1.json"
        )
        event = json.loads(fixture_path.read_text(encoding="utf-8"))

        class DepthStrategy(BaseStrategy):
            received: dict | None = None

            def on_order_book(self, _context, book):
                self.received = dict(book)
                return None

        strategy = DepthStrategy()
        decision = strategy.on_event(self.context, event)

        self.assertEqual(strategy.received["sequence"], 7)
        self.assertEqual(strategy.received["bids"][0]["price_exact"], {
            "coefficient": "12345", "scale": 12
        })
        self.assertEqual(strategy.received["bids"][0]["qty_exact"], {
            "coefficient": "5", "scale": 18
        })
        self.assertEqual(decision["intents"], [])

    def test_timer_hook_rejects_invalid_name_and_timestamp(self) -> None:
        strategy = BaseStrategy()
        with self.assertRaisesRegex(ValueError, "Timer MarketEvent name"):
            strategy.on_event(self.context, {"Timer": {"name": "  ", "ts": 1000}})
        with self.assertRaisesRegex(ValueError, "MarketEvent.ts"):
            strategy.on_event(self.context, {"Timer": {"name": "scheduler", "ts": True}})
        with self.assertRaisesRegex(ValueError, "u64"):
            strategy.on_event(
                self.context,
                {"Timer": {"name": "scheduler", "ts": 1 << 64}},
            )

    def test_diagnostic_state_is_optional_json_with_a_fixed_size_limit(self) -> None:
        class WithState(BaseStrategy):
            def diagnostic_state(self):
                return {"bars_seen": 3, "window": [100, 101, 102]}

        self.assertIsNone(_read_diagnostic_state(BaseStrategy()))
        self.assertEqual(
            _read_diagnostic_state(WithState()),
            {"bars_seen": 3, "window": [100, 101, 102]},
        )

        class NonJsonState(BaseStrategy):
            def diagnostic_state(self):
                return {"value": float("nan")}

        with self.assertRaisesRegex(ValueError, "finite JSON"):
            _read_diagnostic_state(NonJsonState())

        class OversizedState(BaseStrategy):
            def diagnostic_state(self):
                return {"value": "x" * (64 * 1024)}

        with self.assertRaisesRegex(ValueError, "64 KiB"):
            _read_diagnostic_state(OversizedState())

    def test_rejects_malformed_or_unknown_market_events(self) -> None:
        strategy = BaseStrategy()
        with self.assertRaisesRegex(ValueError, "single tagged"):
            strategy.on_event(self.context, {"Bar": {}, "Tick": {}})
        root = Path(__file__).resolve().parents[2]
        matrix = json.loads(
            (
                root
                / "crates/qx-strategy/tests/fixtures/strategy-lifecycle-matrix-v1.json"
            ).read_text(encoding="utf-8")
        )
        with self.assertRaisesRegex(
            ValueError, matrix["unsupported_event_error_category"]
        ):
            strategy.on_event(
                self.context,
                {matrix["unsupported_event_kind"]: {"ts": 100}},
            )

    def test_callback_bridge_forwards_lifecycle_and_order_update_hooks(self) -> None:
        class Hooks(BaseStrategy):
            def __init__(self) -> None:
                self.calls: list[str] = []

            def on_init(self, context):
                self.calls.append(f"init:{context['strategy_id']}")

            def on_start(self, context):
                self.calls.append(f"start:{context['as_of']}")

            def on_bar(self, context, bar):
                self.calls.append(f"bar:{bar['ts']}")
                return None

            def on_order_update(self, context, update):
                self.calls.append(f"update:{update['event']}")
                return None

            def on_stop(self, context, reason):
                self.calls.append(f"stop:{context['strategy_id']}:{reason}")

        strategy = Hooks()
        callback = _PythonBacktestCallback(strategy)
        context_json = json.dumps(self.context)
        callback._qx_on_init(context_json)
        callback._qx_on_start(context_json)
        decision = json.loads(
            callback._qx_on_event(
                context_json,
                json.dumps({"Bar": {"ts": 104, "instrument": "BTCUSDT.BINANCE"}}),
            )
        )
        update_result = callback._qx_on_order_update(
            context_json, json.dumps({"event": "fill"})
        )
        callback._qx_on_stop()

        self.assertEqual(
            strategy.calls,
            ["init:base-test", "start:100", "bar:104", "update:fill", "stop:base-test:host_stop"],
        )
        self.assertEqual(decision["strategy_id"], "base-test")
        self.assertEqual(decision["intents"], [])
        self.assertEqual(update_result, "null")

    def test_local_callback_failure_invokes_on_error_and_preserves_primary_error(self) -> None:
        observed_errors = []
        observed_errors_v1 = []

        class FailingStrategy(BaseStrategy):
            def on_bar(self, _context, _bar):
                raise ValueError("original event failure")

            def on_error(self, context, error):
                observed_errors.append(
                    (context["strategy_id"], context["as_of"], type(error).__name__, str(error))
                )
                raise RuntimeError("secondary error-hook failure")

            def on_error_v1(self, error):
                observed_errors_v1.append(error.to_dict())

        callback = _PythonBacktestCallback(FailingStrategy())
        context = {**self.context, "as_of": 105}
        event = {"Bar": {"ts": 105, "instrument": "BTCUSDT.BINANCE"}}
        with self.assertRaisesRegex(ValueError, "original event failure"):
            callback._qx_on_event(json.dumps(context), json.dumps(event))
        self.assertEqual(
            observed_errors,
            [("base-test", 105, "ValueError", "original event failure")],
        )
        self.assertEqual(
            observed_errors_v1,
            [
                {
                    "schema_version": 1,
                    "stage": "market_event",
                    "strategy_id": "base-test",
                    "strategy_version": "1.0",
                    "timestamp": 105,
                    "instrument": "BTCUSDT.BINANCE",
                    "message": "original event failure",
                }
            ],
        )

    def test_contextual_stop_context_forwards_terminal_reason_and_final_context(self) -> None:
        class Hooks(BaseStrategy):
            def __init__(self) -> None:
                self.stop_value = None

            def on_stop(self, context, reason):
                self.stop_value = (context["strategy_id"], context["as_of"], reason)

        strategy = Hooks()
        callback = _PythonBacktestCallback(strategy)
        final_context = {**self.context, "as_of": 105}
        callback._qx_on_stop_with_context(
            json.dumps(
                {
                    "schema_version": 1,
                    "reason": "cancelled",
                    "context": final_context,
                }
            )
        )
        self.assertEqual(strategy.stop_value, ("base-test", 105, "cancelled"))

    def test_rust_shared_stop_context_fixture_is_consumed_by_python_callback(self) -> None:
        class Hooks(BaseStrategy):
            def __init__(self) -> None:
                self.stop_value = None

            def on_stop(self, context, reason):
                self.stop_value = (context["strategy_id"], context["as_of"], reason)

        root = Path(__file__).resolve().parents[2]
        fixture = root / "crates" / "qx-strategy" / "tests" / "fixtures" / "strategy-stop-context-v1.json"
        stop = json.loads(fixture.read_text(encoding="utf-8"))
        strategy = Hooks()
        callback = _PythonBacktestCallback(strategy)

        callback._qx_on_stop_with_context(json.dumps(stop))

        self.assertEqual(strategy.stop_value, ("golden-stop", 100, "host_stop"))

    def test_contextual_stop_context_rejects_unknown_version_and_reason(self) -> None:
        callback = _PythonBacktestCallback(BaseStrategy())
        for stop in (
            {"schema_version": 2, "reason": "completed", "context": None},
            {"schema_version": 1, "reason": "mystery", "context": None},
        ):
            with self.subTest(stop=stop), self.assertRaisesRegex(
                ValueError, "unsupported version or reason"
            ):
                callback._qx_on_stop_with_context(json.dumps(stop))

    def test_depth_callback_does_not_emit_bar_warmup_and_rejects_bar_count_requirement(self) -> None:
        class Hooks(BaseStrategy):
            def __init__(self) -> None:
                self.warmup_events = []

            def on_warmup_complete(self, context, instrument, observed_bars):
                self.warmup_events.append((instrument, observed_bars))

        strategy = Hooks()
        callback = _PythonBacktestCallback(strategy, bar_warmup=False)
        callback._qx_on_start(json.dumps(self.context))
        self.assertEqual(strategy.warmup_events, [])

        with self.assertRaisesRegex(ValueError, "supported only for Bar events"):
            _PythonBacktestCallback(bind_strategy_warmup(Hooks(), 1), bar_warmup=False)

    def test_base_order_update_dispatches_committed_fill_to_on_trade(self) -> None:
        class TradeHook(BaseStrategy):
            def __init__(self) -> None:
                self.fill_timestamps: list[int] = []

            def on_trade(self, context, trade):
                self.fill_timestamps.append(trade["ts"])
                return self.decision(context, "Bar", trade)

        root = Path(__file__).resolve().parents[2]
        fixture = root / "crates" / "qx-strategy" / "tests" / "fixtures" / "strategy-update-fill-v1.json"
        update = json.loads(fixture.read_text(encoding="utf-8"))
        strategy = TradeHook()
        callback = _PythonBacktestCallback(strategy)

        result = json.loads(
            callback._qx_on_order_update(json.dumps(self.context), json.dumps(update))
        )

        self.assertEqual(strategy.fill_timestamps, [update["fill"]["ts"]])
        self.assertEqual(result["strategy_id"], self.context["strategy_id"])
        self.assertEqual(result["signal_id"], update["fill"]["ts"])

    def test_fill_update_v2_exposes_event_sequence_and_projects_for_legacy_hooks(self) -> None:
        class SequenceHook(BaseStrategy):
            def __init__(self) -> None:
                self.sequence: int | None = None

            def on_fill_update(self, context, update):
                self.sequence = update["event_seq"]
                return None

        class LegacyHook(BaseStrategy):
            def __init__(self) -> None:
                self.update = None

            def on_order_update(self, context, update):
                self.update = update
                return None

        root = Path(__file__).resolve().parents[2]
        fixture = root / "crates" / "qx-strategy" / "tests" / "fixtures" / "strategy-fill-update-v2.json"
        update = json.loads(fixture.read_text(encoding="utf-8"))
        sequence_strategy = SequenceHook()
        legacy_strategy = LegacyHook()

        self.assertEqual(
            _PythonBacktestCallback(sequence_strategy)._qx_on_fill_update_v2(
                json.dumps(self.context), json.dumps(update)
            ),
            "null",
        )
        self.assertEqual(
            _PythonBacktestCallback(legacy_strategy)._qx_on_fill_update_v2(
                json.dumps(self.context), json.dumps(update)
            ),
            "null",
        )
        self.assertEqual(sequence_strategy.sequence, update["event_seq"])
        self.assertEqual(legacy_strategy.update["schema_version"], 1)
        self.assertEqual(legacy_strategy.update["event"], "fill")
        self.assertNotIn("event_seq", legacy_strategy.update)

    def test_shared_partial_fill_sequence_matches_callback_dispatch_order(self) -> None:
        class SequenceHook(BaseStrategy):
            def __init__(self) -> None:
                self.events = []

            def on_order(self, context, update):
                self.events.append((
                    "order_status",
                    update["order"]["account_id"],
                    update["order"]["instrument"],
                    update["event_seq"],
                    update["status"],
                ))
                return None

            def on_fill_update(self, context, update):
                self.events.append((
                    "fill",
                    update["order"]["account_id"],
                    update["order"]["instrument"],
                    update["event_seq"],
                    update["order"]["filled_raw"],
                ))
                return None

        root = Path(__file__).resolve().parents[2]
        fixture_path = root / "crates" / "qx-strategy" / "tests" / "fixtures" / "strategy-execution-sequence-v1.json"
        fixture = json.loads(fixture_path.read_text(encoding="utf-8"))
        lifecycle_matrix = json.loads(
            (fixture_path.parent / "strategy-lifecycle-matrix-v1.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(
            fixture["expected_callback_order"],
            lifecycle_matrix["expected_execution_update_callbacks"],
        )
        strategy = SequenceHook()
        callback = _PythonBacktestCallback(strategy)

        for event in fixture["events"]:
            update = event["update"]
            order = update["order"]
            context = {
                **self.context,
                "strategy_id": fixture["strategy_id"],
                "account_id": order["account_id"],
                "venue_id": order["instrument"].rsplit(".", 1)[1],
                "as_of": 12,
            }
            if event["kind"] == "order_status":
                result = callback._qx_on_order_status_update(
                    json.dumps(context), json.dumps(update)
                )
            else:
                result = callback._qx_on_fill_update_v2(
                    json.dumps(context), json.dumps(update)
                )
            self.assertEqual(result, "null")
        self.assertEqual(
            [
                f"{account}:{instrument}:{event_seq}:{kind}"
                for kind, account, instrument, event_seq, _ in strategy.events
            ],
            fixture["expected_callback_order"],
        )
        self.assertEqual(
            strategy.events,
            [
                ("order_status", "paper", "BTCUSDT.BINANCE", 24, "partially_filled"),
                ("fill", "paper", "BTCUSDT.BINANCE", 24, 0),
                ("order_status", "paper", "BTCUSDT.BINANCE", 25, "filled"),
                ("fill", "paper", "BTCUSDT.BINANCE", 25, 1_000_000_000),
                ("order_status", "paper-b", "ETHUSDT.OKX", 24, "partially_filled"),
                ("fill", "paper-b", "ETHUSDT.OKX", 24, 0),
            ],
        )

    def test_strategy_fill_update_v2_golden_fixture_matches_published_schema(self) -> None:
        root = Path(__file__).resolve().parents[2]
        fixture = root / "crates" / "qx-strategy" / "tests" / "fixtures" / "strategy-fill-update-v2.json"
        schema_path = root / "schemas" / "strategy-fill-update-v2.json"
        update = json.loads(fixture.read_text(encoding="utf-8"))
        schema = json.loads(schema_path.read_text(encoding="utf-8"))

        try:
            from jsonschema import Draft202012Validator
        except ImportError:
            self.skipTest("jsonschema is optional in the Python test environment")
        Draft202012Validator(schema).validate(update)
        self.assertEqual(update["schema_version"], 2)
        self.assertGreaterEqual(update["event_seq"], 0)
        self.assertEqual(update["ts"], update["fill"]["ts"])

    def test_base_order_update_rejects_unknown_or_malformed_execution_facts(self) -> None:
        strategy = BaseStrategy()
        with self.assertRaisesRegex(ValueError, "unsupported StrategyUpdate event"):
            strategy.on_order_update(self.context, {"event": "order"})
        with self.assertRaisesRegex(ValueError, "must contain a fill mapping"):
            strategy.on_order_update(self.context, {"event": "fill"})

    def test_base_order_update_dispatches_committed_order_status(self) -> None:
        class OrderHook(BaseStrategy):
            def __init__(self) -> None:
                self.updates = []

            def on_order(self, context, update):
                self.updates.append((context["strategy_id"], update["event_seq"], update["status"]))
                return None

        root = Path(__file__).resolve().parents[2]
        fixture = root / "crates" / "qx-strategy" / "tests" / "fixtures" / "strategy-order-status-accepted-v1.json"
        update = json.loads(fixture.read_text(encoding="utf-8"))
        strategy = OrderHook()
        callback = _PythonBacktestCallback(strategy)

        result = callback._qx_on_order_status_update(
            json.dumps(self.context), json.dumps(update)
        )

        self.assertEqual(result, "null")
        self.assertEqual(strategy.updates, [(self.context["strategy_id"], 23, "accepted")])

    def test_order_status_update_rejects_stale_or_malformed_identity(self) -> None:
        root = Path(__file__).resolve().parents[2]
        fixture = root / "crates" / "qx-strategy" / "tests" / "fixtures" / "strategy-order-status-accepted-v1.json"
        update = json.loads(fixture.read_text(encoding="utf-8"))
        strategy = BaseStrategy()
        for field, value in (("event_seq", -1), ("ts", 0), ("status", "executed")):
            invalid = dict(update)
            invalid[field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                strategy.on_order_status_update(self.context, invalid)

        zero_based = dict(update)
        zero_based["event_seq"] = 0
        self.assertIsNone(strategy.on_order_status_update(self.context, zero_based))

    def test_order_intent_helper_builds_raw_exact_and_quote_budget_shapes(self) -> None:
        strategy = BaseStrategy()
        raw = strategy.order_intent(
            intent_id=1,
            instrument="BTCUSDT.BINANCE",
            side="sell",
            qty_raw=2_000_000_000,
            limit_price_raw=100_000_000_000,
            reduce_only=True,
        )
        exact = strategy.order_intent(
            intent_id=2,
            instrument={"symbol": "SHIBUSDT", "venue": "OKX"},
            side="Buy",
            qty_exact="0.0000000001000",
            limit_price_exact="0.00000005",
        )
        budget = strategy.order_intent(
            intent_id=3,
            instrument="BTCUSDT.BITGET",
            side="buy",
            quote_budget="12.50",
        )

        self.assertEqual(raw["instrument"], {"symbol": "BTCUSDT", "venue": "BINANCE"})
        self.assertEqual(raw["side"], "Sell")
        self.assertEqual(raw["qty"], 2_000_000_000)
        self.assertTrue(raw["reduce_only"])
        self.assertEqual(exact["qty"], 0)
        self.assertEqual(exact["qty_exact"], {"coefficient": "1", "scale": 10})
        self.assertEqual(exact["limit_price_exact"], {"coefficient": "5", "scale": 8})
        self.assertEqual(budget["quote_budget"], {"coefficient": "125", "scale": 1})
        self.assertEqual(budget["qty"], 0)

    def test_order_helpers_create_unique_intents_and_close_signed_positions(self) -> None:
        strategy = BaseStrategy()
        bought = strategy.buy("BTCUSDT.BINANCE", qty_raw=1_000_000_000)
        sold = strategy.sell(
            {"symbol": "ETHUSDT", "venue": "OKX"}, qty_exact="0.125"
        )
        self.assertEqual((bought["intent_id"], bought["side"]), (1, "Buy"))
        self.assertEqual((sold["intent_id"], sold["side"]), (2, "Sell"))
        self.assertEqual(sold["qty_exact"], {"coefficient": "125", "scale": 3})

        long_context = {
            **self.context,
            "positions_exact": {"paper|BTCUSDT.BINANCE": "1.234567890123"},
        }
        close_long = strategy.close_position(long_context, "BTCUSDT.BINANCE")
        self.assertEqual(close_long["side"], "Sell")
        self.assertEqual(close_long["qty_exact"], {"coefficient": "1234567890123", "scale": 12})
        self.assertTrue(close_long["reduce_only"])

        short_context = {
            **self.context,
            "positions": {"paper|ETHUSDT.OKX": -2_500_000_000},
        }
        close_short = strategy.close_position(short_context, "ETHUSDT.OKX")
        self.assertEqual(close_short["side"], "Buy")
        self.assertEqual(close_short["qty_exact"], {"coefficient": "25", "scale": 1})
        self.assertTrue(close_short["reduce_only"])
        self.assertIsNone(strategy.close_position(self.context, "BTCUSDT.BINANCE"))

    def test_short_and_cover_helpers_preserve_explicit_hedge_policy(self) -> None:
        strategy = BaseStrategy()
        hedge_short_policy = {
            "margin_mode": "cross",
            "position_mode": "hedge",
            "position_side": "short",
            "leverage": 3,
        }

        open_short = strategy.short(
            "BTCUSDT.OKX",
            qty_exact="0.125",
            policy=hedge_short_policy,
        )
        cover_short = strategy.cover(
            "BTCUSDT.OKX",
            qty_exact="0.05",
            policy=hedge_short_policy,
        )

        self.assertEqual(open_short["side"], "Sell")
        self.assertFalse(open_short["reduce_only"])
        self.assertEqual(open_short["policy"], hedge_short_policy)
        self.assertEqual(cover_short["side"], "Buy")
        self.assertTrue(cover_short["reduce_only"])
        self.assertEqual(cover_short["policy"], hedge_short_policy)
        self.assertEqual(open_short["qty_exact"], {"coefficient": "125", "scale": 3})
        self.assertEqual(cover_short["qty_exact"], {"coefficient": "5", "scale": 2})
        self.assertNotEqual(open_short["intent_id"], cover_short["intent_id"])

    def test_position_helper_prefers_route_scoped_exact_quantity(self) -> None:
        context = {
            **self.context,
            "positions": {
                "BTCUSDT.BINANCE": 1_000_000_000,
                "paper|BTCUSDT.BINANCE": 2_000_000_000,
            },
            "positions_exact": {
                "BTCUSDT.BINANCE": "1",
                "paper|BTCUSDT.BINANCE": "2.000000000123",
            },
        }
        self.assertEqual(
            BaseStrategy.position_qty(context, {"symbol": "BTCUSDT", "venue": "BINANCE"}),
            Decimal("2.000000000123"),
        )

    def test_position_helper_accepts_rust_exact_decimal_object(self) -> None:
        context = {
            **self.context,
            "positions": {"paper|BTCUSDT.BINANCE": 1_234_567_890},
            "positions_exact": {
                "paper|BTCUSDT.BINANCE": {"coefficient": "1234567890123", "scale": 12}
            },
        }
        self.assertEqual(
            BaseStrategy.position_qty(context, "BTCUSDT.BINANCE"),
            Decimal("1.234567890123"),
        )
        close_intent = BaseStrategy().close_position(context, "BTCUSDT.BINANCE")
        self.assertEqual(close_intent["qty_exact"], {"coefficient": "1234567890123", "scale": 12})

    def test_order_intent_helper_rejects_float_and_ambiguous_or_invalid_shapes(self) -> None:
        strategy = BaseStrategy()
        with self.assertRaisesRegex(TypeError, "decimal string"):
            strategy.order_intent(
                intent_id=1,
                instrument="BTCUSDT.BINANCE",
                side="buy",
                qty_exact=0.1,
            )
        with self.assertRaisesRegex(ValueError, "exactly one"):
            strategy.order_intent(
                intent_id=1,
                instrument="BTCUSDT.BINANCE",
                side="buy",
                qty_raw=1,
                qty_exact="1",
            )
        with self.assertRaisesRegex(ValueError, "precision exceeds"):
            strategy.order_intent(
                intent_id=1,
                instrument="BTCUSDT.BINANCE",
                side="buy",
                qty_exact="0.0000000000000000001",
            )
        with self.assertRaisesRegex(ValueError, "standalone non-post-only market buy"):
            strategy.order_intent(
                intent_id=1,
                instrument="BTCUSDT.BINANCE",
                side="buy",
                quote_budget="10",
                post_only=True,
            )

    def test_warmup_suppresses_orders_until_each_instrument_is_ready(self) -> None:
        class WarmupStrategy(BaseStrategy):
            def __init__(self) -> None:
                self.ready: list[tuple[str | None, int]] = []
                self.calls: list[str] = []
                self.bars_seen: dict[str, int] = {}

            def history_requirements(self):
                return {"warmup_bars": 2}

            def on_warmup_complete(self, context, instrument, observed_bars):
                self.assert_bar_count(instrument, observed_bars - 1)
                self.ready.append((instrument, observed_bars))
                self.calls.append(f"ready:{instrument}:{observed_bars}")

            def assert_bar_count(self, instrument, observed_bars):
                self_calls = self.bars_seen.get(instrument, 0)
                if self_calls != observed_bars:
                    raise AssertionError(
                        f"warmup callback saw {self_calls} bars, expected {observed_bars}"
                    )

            def on_bar(self, context, bar):
                instrument = bar["instrument"]
                count = self.bars_seen.get(instrument, 0) + 1
                self.bars_seen[instrument] = count
                self.calls.append(f"bar:{instrument}:{count}")
                return self.decision(
                    context,
                    "Bar",
                    bar,
                    intents=[{"intent_id": 1, "instrument": bar["instrument"]}],
                )

        strategy = WarmupStrategy()
        callback = _PythonBacktestCallback(strategy)
        callback._qx_on_start(json.dumps(self.context))

        def event(ts: int, instrument: str) -> dict:
            return {"Bar": {"ts": ts, "instrument": instrument}}

        first = json.loads(callback._qx_on_event(
            json.dumps(self.context), json.dumps(event(100, "BTCUSDT.BINANCE"))
        ))
        second = json.loads(callback._qx_on_event(
            json.dumps(self.context), json.dumps(event(101, "BTCUSDT.BINANCE"))
        ))
        other_first = json.loads(callback._qx_on_event(
            json.dumps(self.context), json.dumps(event(102, "ETHUSDT.BINANCE"))
        ))
        third = json.loads(callback._qx_on_event(
            json.dumps(self.context), json.dumps(event(103, "BTCUSDT.BINANCE"))
        ))

        self.assertEqual(first["intents"], [])
        self.assertEqual(len(second["intents"]), 1)
        self.assertEqual(other_first["intents"], [])
        self.assertEqual(len(third["intents"]), 1)
        self.assertEqual(
            strategy.ready,
            [("BTCUSDT.BINANCE", 2)],
        )
        self.assertEqual(
            strategy.calls,
            [
                "bar:BTCUSDT.BINANCE:1",
                "ready:BTCUSDT.BINANCE:2",
                "bar:BTCUSDT.BINANCE:2",
                "bar:ETHUSDT.BINANCE:1",
                "bar:BTCUSDT.BINANCE:3",
            ],
        )
        with self.assertRaisesRegex(ValueError, "warmup completed"):
            callback._qx_on_stop()

    def test_manifest_warmup_is_bound_and_conflicts_fail_closed(self) -> None:
        class Declared(BaseStrategy):
            def history_requirements(self):
                return {"warmup_bars": 3}

        callback = _PythonBacktestCallback(bind_strategy_warmup(Declared(), 3))
        self.assertEqual(callback._warmup_bars, 3)
        with self.assertRaisesRegex(ValueError, "conflicts"):
            bind_strategy_warmup(Declared(), 4)

    def test_history_requirements_failure_emits_versioned_error_during_local_init(self) -> None:
        observed = []

        class BrokenRequirements(BaseStrategy):
            def history_requirements(self):
                raise RuntimeError("history requirements unavailable")

            def on_error_v1(self, error):
                observed.append(error.to_dict())

        callback = _PythonBacktestCallback(BrokenRequirements())
        with self.assertRaisesRegex(RuntimeError, "history requirements unavailable"):
            callback._qx_on_init(json.dumps(self.context))
        self.assertEqual(
            observed,
            [
                {
                    "schema_version": 1,
                    "stage": "history_requirements",
                    "strategy_id": "base-test",
                    "strategy_version": "1.0",
                    "timestamp": 100,
                    "instrument": None,
                    "message": "history requirements unavailable",
                }
            ],
        )

    def test_bar_warmup_rejects_non_bar_events_and_zero_warmup_notifies_on_start(self) -> None:
        class Hooks(BaseStrategy):
            def __init__(self) -> None:
                self.ready: list[tuple[object, int]] = []

            def history_requirements(self):
                return {"warmup_bars": 1}

            def on_warmup_complete(self, context, instrument, observed_bars):
                self.ready.append((instrument, observed_bars))

        callback = _PythonBacktestCallback(Hooks())
        with self.assertRaisesRegex(ValueError, "only for Bar"):
            callback._qx_on_event(
                json.dumps(self.context),
                json.dumps({"Tick": {"ts": 100, "instrument": "BTCUSDT.BINANCE"}}),
            )

        class ZeroWarmup(Hooks):
            def history_requirements(self):
                return {"warmup_bars": 0}

        zero = ZeroWarmup()
        zero_callback = _PythonBacktestCallback(bind_strategy_warmup(zero, 0))
        zero_callback._qx_on_start(json.dumps(self.context))
        self.assertEqual(zero.ready, [(None, 0)])

    def test_existing_callable_strategy_callback_remains_supported(self) -> None:
        callback = _PythonBacktestCallback(
            lambda context, event: {
                "schema_version": 1,
                "request_id": f"{context['strategy_id']}:legacy",
                "strategy_id": context["strategy_id"],
                "signal_id": event["Bar"]["ts"],
                "confidence": 0,
                "priority": 0,
                "expires_at": 0,
                "intents": [],
            }
        )
        result = json.loads(
            callback._qx_on_event(
                json.dumps(self.context),
                json.dumps({"Bar": {"ts": 105, "instrument": "BTCUSDT.BINANCE"}}),
            )
        )
        self.assertEqual(result["request_id"], "base-test:legacy")

    def test_strategy_update_golden_fixture_matches_published_schema(self) -> None:
        root = Path(__file__).resolve().parents[2]
        fixture = root / "crates" / "qx-strategy" / "tests" / "fixtures" / "strategy-update-fill-v1.json"
        schema_path = root / "schemas" / "strategy-update-v1.json"
        update = json.loads(fixture.read_text(encoding="utf-8"))
        schema = json.loads(schema_path.read_text(encoding="utf-8"))

        try:
            from jsonschema import Draft202012Validator
        except ImportError:
            self.skipTest("jsonschema is optional in the Python test environment")
        Draft202012Validator(schema).validate(update)
        self.assertEqual(update["event"], "fill")
        self.assertEqual(update["order"]["strategy_id"], "update-test")

        quote_budget_v1 = json.loads(json.dumps(update))
        quote_budget_v1["fill"]["qty"] = 0
        quote_budget_v1["fill"]["price"] = 0
        quote_budget_v1["fill"]["exact_amounts"] = {
            "quantity": {"coefficient": "3333", "scale": 3},
            "price": {"coefficient": "3", "scale": 0},
            "quote_cost": {"coefficient": "9999", "scale": 3},
            "quote_budget_complete": True,
        }
        Draft202012Validator(schema).validate(quote_budget_v1)

        quote_budget_v2 = json.loads(
            (
                root
                / "crates"
                / "qx-strategy"
                / "tests"
                / "fixtures"
                / "strategy-fill-update-v2.json"
            ).read_text(encoding="utf-8")
        )
        quote_budget_v2["fill"]["qty"] = 0
        quote_budget_v2["fill"]["price"] = 0
        quote_budget_v2["fill"]["exact_amounts"] = quote_budget_v1["fill"]["exact_amounts"]
        v2_schema = json.loads(
            (root / "schemas" / "strategy-fill-update-v2.json").read_text(encoding="utf-8")
        )
        Draft202012Validator(v2_schema).validate(quote_budget_v2)

    def test_order_status_update_golden_fixture_matches_published_schema(self) -> None:
        root = Path(__file__).resolve().parents[2]
        fixture = root / "crates" / "qx-strategy" / "tests" / "fixtures" / "strategy-order-status-accepted-v1.json"
        schema_path = root / "schemas" / "strategy-order-status-update-v1.json"
        update = json.loads(fixture.read_text(encoding="utf-8"))
        schema = json.loads(schema_path.read_text(encoding="utf-8"))

        try:
            from jsonschema import Draft202012Validator
        except ImportError:
            self.skipTest("jsonschema is optional in the Python test environment")
        Draft202012Validator(schema).validate(update)
        self.assertEqual(update["event"], "order_status")
        self.assertEqual(update["order"]["strategy_id"], "update-test")

    def test_c_strategy_order_policy_fixture_matches_published_schema(self) -> None:
        root = Path(__file__).resolve().parents[2]
        fixture = root / "crates" / "qx-strategy" / "tests" / "fixtures" / "c-strategy-order-policy-v1.json"
        schema_path = root / "schemas" / "c-strategy-order-policy-v1.json"
        policy = json.loads(fixture.read_text(encoding="utf-8"))
        schema = json.loads(schema_path.read_text(encoding="utf-8"))

        try:
            from jsonschema import Draft202012Validator
        except ImportError:
            self.skipTest("jsonschema is optional in the Python test environment")
        validator = Draft202012Validator(schema)
        validator.validate(policy)
        self.assertEqual(policy["margin_mode"], "cross")
        self.assertEqual(policy["position_mode"], "hedge")
        self.assertEqual(policy["leverage"], 7)
        self.assertFalse(validator.is_valid({"schema_version": 1, "leverage": 7}))


if __name__ == "__main__":
    unittest.main()
