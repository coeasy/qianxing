from __future__ import annotations

import contextlib
import io
import json
import tempfile
import types
import unittest
from unittest.mock import patch
from pathlib import Path

from qianxing_bridge.strategy import StrategyBars, StrategyInput
from qianxing_strategy import BaseStrategy, StrategyOutput
from qianxing_strategy.worker import (
    _handler_from_module,
    _handle_line,
    _load_strategy_module,
    serve,
)


class _LifecycleStrategy(BaseStrategy):
    STRATEGY_MANIFEST = {"warmup_bars": 2}

    def __init__(self, spec):
        self.spec = spec
        self.calls = []

    def history_requirements(self):
        return {"warmup_bars": 2}

    def on_init(self, context):
        self.calls.append(("init", context["account_id"]))

    def on_start(self, context):
        self.calls.append(("start", context["strategy_id"]))

    def on_warmup_complete(self, context, instrument, observed_bars):
        self.calls.append(
            ("warmup", instrument["symbol"], instrument["venue"], observed_bars)
        )

    def on_bar(self, context, bar):
        self.calls.append(("bar", bar["ts"]))
        intents = []
        if bar["ts"] == 2:
            intents.append(
                self.order_intent(
                    intent_id=10,
                    instrument=bar["instrument"],
                    side="buy",
                    qty_raw=1,
                )
            )
        return self.decision(context, "Bar", bar, intents=intents)

    def on_order(self, context, update):
        self.calls.append(("order", update["event_seq"], update["status"]))

    def on_trade(self, context, trade):
        self.calls.append(("fill", trade["order_id"]))

    def on_stop(self):
        self.calls.append(("stop",))


def _request(*, updates=False):
    execution_updates = ()
    fill_updates = ()
    schema_version = 1
    if updates:
        schema_version = 3
        order = {
            "order_id": 5,
            "account_id": "main",
            "instrument": "BTCUSDT.BINANCE",
            "side": "Buy",
            "qty_raw": 200,
            "filled_raw": 100,
            "strategy_id": "sma",
        }
        execution_updates = (
            {
                "schema_version": 1,
                "event": "order_status",
                "event_seq": 7,
                "ts": 2,
                "status": "partially_filled",
                "order": order,
                "reason": None,
            },
        )
        fill_updates = (
            {
                "schema_version": 2,
                "event": "fill",
                "event_seq": 7,
                "ts": 2,
                "order": order,
                "fill": {
                    "order_id": 5,
                    "qty": 100,
                    "price": 200,
                    "fee": 0,
                    "ts": 2,
                    "account_id": "main",
                    "strategy_id": "sma",
                },
            },
        )
    return StrategyInput(
        schema_version=schema_version,
        request_id="job-2",
        strategy_id="sma",
        strategy_version="1.0",
        data_fingerprint="bars-sha256",
        as_of=2,
        instrument="BTCUSDT.BINANCE",
        positions={"BTCUSDT.BINANCE": 0},
        positions_exact={"BTCUSDT.BINANCE": "0"},
        cash={"USDT": 100_000},
        risk_state="clear",
        bars=StrategyBars(
            source="fixture",
            ts=(1, 2),
            open_raw=(100, 110),
            high_raw=(101, 111),
            low_raw=(99, 109),
            close_raw=(100, 110),
            volume_raw=(10, 12),
        ),
        execution_updates=execution_updates,
        fill_updates=fill_updates,
    )


class BaseStrategyProcessAdapterTests(unittest.TestCase):
    def test_module_function_checkpoint_restores_state_across_worker_restart(self):
        first_state = {"count": 0}

        def first_on_event(request):
            first_state["count"] += 1
            return StrategyOutput(
                request_id=request.request_id,
                strategy_id=request.strategy_id,
                signal_id=1,
                instrument=request.instrument,
                target_qty=0,
                confidence=1000,
                priority=1,
                expires_at=request.as_of,
            )

        first_module = types.SimpleNamespace(
            on_event=first_on_event,
            snapshot_state=lambda: dict(first_state),
            restore_state=lambda state: first_state.update(state),
        )
        first_worker = _handler_from_module(first_module)
        request = _request()
        bootstrap = {
            "schema_version": 1,
            "strategy_id": request.strategy_id,
            "strategy_version": request.strategy_version,
            "adapter_state": {"schema_version": 1, "state_mode": "bootstrap"},
            "strategy_state": {},
        }
        first_payload = request.to_dict()
        first_payload["schema_version"] = 4
        first_payload["strategy_checkpoint"] = bootstrap
        first = json.loads(
            _handle_line(
                first_worker,
                json.dumps(first_payload),
                checkpoint_handler=first_worker.strategy_checkpoint,
            )
        )
        self.assertTrue(first["ok"], first)
        checkpoint = first["strategy_checkpoint"]
        self.assertEqual(checkpoint["strategy_state"], {"count": 1})
        self.assertEqual(
            checkpoint["adapter_state"],
            {"schema_version": 1, "state_mode": "module-function"},
        )

        restarted_state = {"count": -1}

        def restarted_on_event(next_request):
            restarted_state["count"] += 1
            return StrategyOutput(
                request_id=next_request.request_id,
                strategy_id=next_request.strategy_id,
                signal_id=2,
                instrument=next_request.instrument,
                target_qty=0,
                confidence=1000,
                priority=1,
                expires_at=next_request.as_of,
            )

        restarted_module = types.SimpleNamespace(
            on_event=restarted_on_event,
            snapshot_state=lambda: dict(restarted_state),
            restore_state=lambda state: restarted_state.update(state),
        )
        restarted_worker = _handler_from_module(restarted_module)
        resumed_payload = request.to_dict()
        resumed_payload["schema_version"] = 4
        resumed_payload["strategy_checkpoint"] = checkpoint
        resumed = json.loads(
            _handle_line(
                restarted_worker,
                json.dumps(resumed_payload),
                checkpoint_handler=restarted_worker.strategy_checkpoint,
            )
        )
        self.assertTrue(resumed["ok"], resumed)
        self.assertEqual(restarted_state["count"], 2)
        self.assertEqual(resumed["strategy_checkpoint"]["strategy_state"], {"count": 2})

    def test_module_function_checkpoint_rolls_back_failed_callback_mutations(self):
        state = {"count": 0}
        fail_once = {"enabled": True}

        def on_event(request):
            state["count"] += 1
            if fail_once["enabled"]:
                fail_once["enabled"] = False
                raise RuntimeError("module callback failed after state mutation")
            return StrategyOutput(
                request_id=request.request_id,
                strategy_id=request.strategy_id,
                signal_id=1,
                instrument=request.instrument,
                target_qty=0,
                confidence=1000,
                priority=1,
                expires_at=request.as_of,
            )

        worker = _handler_from_module(
            types.SimpleNamespace(
                on_event=on_event,
                snapshot_state=lambda: dict(state),
                restore_state=lambda saved: state.update(saved),
            )
        )
        request = _request()
        payload = request.to_dict()
        payload["schema_version"] = 4
        payload["strategy_checkpoint"] = {
            "schema_version": 1,
            "strategy_id": request.strategy_id,
            "strategy_version": request.strategy_version,
            "adapter_state": {"schema_version": 1, "state_mode": "bootstrap"},
            "strategy_state": {},
        }
        failed = json.loads(
            _handle_line(
                worker,
                json.dumps(payload),
                checkpoint_handler=worker.strategy_checkpoint,
            )
        )
        self.assertFalse(failed["ok"])
        self.assertNotIn("strategy_checkpoint", failed)
        self.assertEqual(state["count"], 0)
        retried = json.loads(
            _handle_line(
                worker,
                json.dumps(payload),
                checkpoint_handler=worker.strategy_checkpoint,
            )
        )
        self.assertTrue(retried["ok"], retried)
        self.assertEqual(state["count"], 1)
        self.assertEqual(retried["strategy_checkpoint"]["strategy_state"], {"count": 1})

    def test_module_function_requires_checkpoint_or_explicit_stateless_declaration(self):
        called = []

        def on_event(request):
            called.append(request.request_id)
            return StrategyOutput(
                request_id=request.request_id,
                strategy_id=request.strategy_id,
                signal_id=1,
                instrument=request.instrument,
                target_qty=0,
                confidence=1000,
                priority=1,
                expires_at=request.as_of,
            )

        request = _request()
        payload = request.to_dict()
        payload["schema_version"] = 4
        payload["strategy_checkpoint"] = {
            "schema_version": 1,
            "strategy_id": request.strategy_id,
            "strategy_version": request.strategy_version,
            "adapter_state": {"schema_version": 1, "state_mode": "bootstrap"},
            "strategy_state": {},
        }
        legacy = _handler_from_module(types.SimpleNamespace(on_event=on_event))
        refused = json.loads(_handle_line(legacy, json.dumps(payload)))
        self.assertFalse(refused["ok"])
        self.assertIn("STRATEGY_STATELESS = True", refused["error"])
        self.assertEqual(called, [])

        stateless = _handler_from_module(
            types.SimpleNamespace(on_event=on_event, STRATEGY_STATELESS=True)
        )
        accepted = json.loads(
            _handle_line(
                stateless,
                json.dumps(payload),
                checkpoint_handler=stateless.strategy_checkpoint,
            )
        )
        self.assertTrue(accepted["ok"], accepted)
        self.assertEqual(accepted["strategy_checkpoint"]["strategy_state"], {})
        self.assertEqual(
            accepted["strategy_checkpoint"]["adapter_state"]["state_mode"], "stateless"
        )

    def test_module_state_hooks_must_be_complete_and_consistent(self):
        module = types.SimpleNamespace(on_event=lambda _request: None, snapshot_state=lambda: {})
        with self.assertRaisesRegex(ValueError, r"both snapshot_state\(\) and restore_state"):
            _handler_from_module(module)
        with self.assertRaisesRegex(ValueError, "cannot be combined"):
            _handler_from_module(
                types.SimpleNamespace(
                    on_event=lambda _request: None,
                    snapshot_state=lambda: {},
                    restore_state=lambda _state: None,
                    STRATEGY_STATELESS=True,
                )
            )

    def test_contextual_stop_hook_receives_last_context_and_preserves_legacy_shape(self):
        stop_callbacks = []

        class ContextualStopStrategy(BaseStrategy):
            def __init__(self):
                self.stop_value = None

            def on_bar(self, context, bar):
                return self.decision(context, "Bar", bar)

            def on_stop(self, context, reason):
                self.stop_value = (context["strategy_id"], context["as_of"], reason)
                stop_callbacks.append(f"on_stop:{reason}")

        handler = _handler_from_module(
            types.SimpleNamespace(create_strategy=lambda _spec: ContextualStopStrategy()),
            {"data_level": "bar"},
        )
        response = json.loads(_handle_line(handler, json.dumps(_request().to_dict())))
        self.assertTrue(response["ok"], response)
        handler.close()
        self.assertEqual(handler.strategy.stop_value, ("sma", 2, "host_stop"))
        fixture_root = (
            Path(__file__).resolve().parents[2]
            / "crates"
            / "qx-strategy"
            / "tests"
            / "fixtures"
        )
        matrix = json.loads(
            (fixture_root / "strategy-lifecycle-matrix-v1.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(stop_callbacks, matrix["expected_stop_callbacks"])

    def test_stop_callback_failure_emits_versioned_error_context(self):
        observed_errors = []

        class StopFailureStrategy(BaseStrategy):
            def on_bar(self, context, bar):
                return self.decision(context, "Bar", bar)

            def on_stop(self, context, reason):
                raise RuntimeError("stop callback failed")

            def on_error_v1(self, error):
                observed_errors.append(error.to_dict())

        handler = _handler_from_module(
            types.SimpleNamespace(create_strategy=lambda _spec: StopFailureStrategy()),
            {"data_level": "bar"},
        )
        response = json.loads(_handle_line(handler, json.dumps(_request().to_dict())))
        self.assertTrue(response["ok"], response)
        with self.assertRaisesRegex(RuntimeError, "stop callback failed"):
            handler.close()
        self.assertEqual(
            observed_errors,
            [
                {
                    "schema_version": 1,
                    "stage": "stop",
                    "strategy_id": "sma",
                    "strategy_version": "1.0",
                    "timestamp": 2,
                    "instrument": "BTCUSDT.BINANCE",
                    "message": "stop callback failed",
                }
            ],
        )

    def test_runtime_worker_orderly_eof_reports_shutdown(self):
        class ContextualStopStrategy(BaseStrategy):
            def __init__(self):
                self.stop_value = None

            def on_bar(self, context, bar):
                return self.decision(context, "Bar", bar)

            def on_stop(self, context, reason):
                self.stop_value = (context["strategy_id"], context["as_of"], reason)

        handler = _handler_from_module(
            types.SimpleNamespace(create_strategy=lambda _spec: ContextualStopStrategy()),
            {"data_level": "bar"},
        )
        response = json.loads(_handle_line(handler, json.dumps(_request().to_dict())))
        self.assertTrue(response["ok"], response)

        with patch("qianxing_strategy.worker.sys.stdin", io.StringIO("")):
            serve(handler)

        self.assertEqual(handler.strategy.stop_value, ("sma", 2, "shutdown"))

    def test_runtime_job_requests_share_one_strategy_session_stop_lifecycle(self):
        class SessionStrategy(BaseStrategy):
            STRATEGY_MANIFEST = {"warmup_bars": 0}

            def __init__(self):
                self.starts = 0
                self.timers = []
                self.stop_reasons = []

            def on_start(self, _context):
                self.starts += 1

            def on_timer(self, context, timer):
                self.timers.append((context["request_id"], timer["ts"]))
                return self.decision(context, "Timer", timer)

            def on_stop(self, context, reason):
                self.stop_reasons.append((context["request_id"], reason))

        handler = _handler_from_module(
            types.SimpleNamespace(STRATEGY_CLASS=SessionStrategy),
            {"data_level": "timer"},
        )
        first = _request().to_dict()
        first["as_of"] = 2
        second = dict(first)
        second["request_id"] = "job-3"
        second["as_of"] = 3
        payload = "\n".join(json.dumps(item) for item in (first, second)) + "\n"

        with (
            patch("qianxing_strategy.worker.sys.stdin", io.StringIO(payload)),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            serve(handler)

        self.assertEqual(handler.strategy.starts, 1)
        self.assertEqual(handler.strategy.timers, [("job-2", 2), ("job-3", 3)])
        self.assertEqual(handler.strategy.stop_reasons, [("job-3", "shutdown")])

    def test_runtime_worker_input_failure_reports_failed(self):
        class ContextualStopStrategy(BaseStrategy):
            def __init__(self):
                self.stop_reason = None

            def on_bar(self, context, bar):
                return self.decision(context, "Bar", bar)

            def on_stop(self, context, reason):
                self.stop_reason = reason

        class BrokenInput:
            def __iter__(self):
                raise OSError("injected stdin failure")

        handler = _handler_from_module(
            types.SimpleNamespace(create_strategy=lambda _spec: ContextualStopStrategy()),
            {"data_level": "bar"},
        )
        response = json.loads(_handle_line(handler, json.dumps(_request().to_dict())))
        self.assertTrue(response["ok"], response)

        with patch("qianxing_strategy.worker.sys.stdin", BrokenInput()):
            with self.assertRaisesRegex(OSError, "injected stdin failure"):
                serve(handler)

        self.assertEqual(handler.strategy.stop_reason, "failed")

    def test_runtime_orderbook_data_level_dispatches_exact_book_to_on_order_book(self):
        class OrderBookStrategy(BaseStrategy):
            def __init__(self):
                self.books = []
                self.bars = []
                self.callbacks = []

            def on_order_book(self, context, book):
                self.books.append((book["sequence"], book["bids"][0]["price_exact"], context["as_of"]))
                self.callbacks.append(f"market:OrderBook:{context['as_of']}")
                return self.decision(context, "OrderBook", book)

            def on_bar(self, _context, bar):
                self.bars.append(bar["ts"])

        handler = _handler_from_module(
            types.SimpleNamespace(create_strategy=lambda _spec: OrderBookStrategy()),
            {"data_level": "orderbook"},
        )
        fixture_root = (
            Path(__file__).resolve().parents[2]
            / "crates"
            / "qx-strategy"
            / "tests"
            / "fixtures"
        )
        event = json.loads(
            (fixture_root / "strategy-market-orderbook-v1.json").read_text(
                encoding="utf-8"
            )
        )["OrderBook"]

        def runtime_level(level):
            exact_price = level["price_exact"]
            exact_quantity = level["qty_exact"]
            return {
                "price_raw": level["price_raw"],
                "qty_raw": level["qty_raw"],
                "price_exact": (
                    f"{exact_price['coefficient']}e-{exact_price['scale']}"
                ),
                "qty_exact": (
                    f"{exact_quantity['coefficient']}e-{exact_quantity['scale']}"
                ),
            }

        value = _request().to_dict()
        value.update(
            schema_version=8,
            as_of=event["ts"],
            market_order_books=[
                {
                    "stream_id": "market:binance",
                    "event_seq": 1,
                    "source_seq": event["sequence"],
                    "ts": event["ts"],
                    "instrument": (
                        f"{event['instrument']['symbol']}.{event['instrument']['venue']}"
                    ),
                    "sequence": event["sequence"],
                    "bids": [runtime_level(level) for level in event["bids"]],
                    "asks": [runtime_level(level) for level in event["asks"]],
                }
            ],
        )
        response = json.loads(_handle_line(handler, json.dumps(value)))

        self.assertTrue(response["ok"], response)
        self.assertEqual(handler.strategy.books, [(7, "12345e-12", 10)])
        self.assertEqual(handler.strategy.bars, [])
        matrix = json.loads(
            (fixture_root / "strategy-lifecycle-matrix-v1.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(
            handler.strategy.callbacks,
            matrix["expected_market_order_book_callbacks"],
        )

    def test_runtime_class_manifest_defaults_and_validator_are_applied(self):
        class ManifestStrategy(BaseStrategy):
            STRATEGY_ID = "manifest-strategy"
            STRATEGY_VERSION = "1.0"
            STRATEGY_MANIFEST = {
                "schema_version": 1,
                "strategy_id": "manifest-strategy",
                "strategy_version": "1.0",
                "parameters": {
                    "fast_window": {
                        "type": "integer",
                        "minimum": 2,
                        "maximum": 20,
                        "default": 5,
                    },
                    "slow_window": {
                        "type": "integer",
                        "minimum": 5,
                        "maximum": 50,
                        "default": 20,
                    },
                },
                "instrument_requirements": {
                    "asset_classes": [],
                    "products": [],
                    "venues": [],
                    "requires_explicit_spec": False,
                },
                "data_level": "bar",
                "warmup_bars": 2,
                "dependencies": [],
            }

            @staticmethod
            def validate_config(config):
                if config["fast_window"] >= config["slow_window"]:
                    raise ValueError("fast_window must be below slow_window")

            def __init__(self, spec):
                self.spec = spec

        module = types.SimpleNamespace(
            STRATEGY_CLASS=ManifestStrategy,
            create_strategy=ManifestStrategy,
        )
        handler = _handler_from_module(
            module,
            {"strategy": {"quantity_raw": 4}, "parameters": {"fast_window": 3}},
        )
        self.assertEqual(handler.strategy.spec["strategy"]["fast_window"], 3)
        self.assertEqual(handler.strategy.spec["strategy"]["slow_window"], 20)
        self.assertEqual(handler.strategy.spec["parameters"]["slow_window"], 20)
        response = json.loads(_handle_line(handler, _request().to_json()))
        self.assertTrue(response["ok"], response)

        with self.assertRaisesRegex(ValueError, "fast_window must be below slow_window"):
            _handler_from_module(
                module,
                {
                    "strategy": {"quantity_raw": 4},
                    "parameters": {"fast_window": 18, "slow_window": 15},
                },
            )

    def test_runtime_strategy_class_accepts_no_argument_constructor(self):
        class NoArgumentStrategy(BaseStrategy):
            STRATEGY_MANIFEST = {"warmup_bars": 0}

            def __init__(self):
                self.bar_count = 0

            def on_bar(self, context, bar):
                self.bar_count += 1
                return self.decision(context, "Bar", bar)

        handler = _handler_from_module(
            types.SimpleNamespace(STRATEGY_CLASS=NoArgumentStrategy),
            {"data_level": "bar"},
        )
        response = json.loads(_handle_line(handler, _request().to_json()))

        self.assertTrue(response["ok"], response)
        self.assertEqual(handler.strategy.bar_count, 2)

    def test_runtime_constructor_failure_emits_module_error_v1(self):
        observed_errors = []

        def create_strategy(_spec):
            raise RuntimeError("strategy constructor failed")

        def on_error_v1(error):
            observed_errors.append(error.to_dict())

        module = types.SimpleNamespace(
            STRATEGY_ID="constructor-strategy",
            STRATEGY_VERSION="2.1",
            STRATEGY_MANIFEST={"warmup_bars": 0},
            create_strategy=create_strategy,
            on_error_v1=on_error_v1,
        )

        with self.assertRaisesRegex(RuntimeError, "strategy constructor failed"):
            _handler_from_module(module, {"data_level": "bar"})

        self.assertEqual(
            observed_errors,
            [
                {
                    "schema_version": 1,
                    "stage": "initialization",
                    "strategy_id": "constructor-strategy",
                    "strategy_version": "2.1",
                    "timestamp": None,
                    "instrument": None,
                    "message": "strategy constructor failed",
                }
            ],
        )

    def test_runtime_constructor_error_hook_failure_preserves_original_error(self):
        def create_strategy(_spec):
            raise ValueError("original constructor failure")

        def on_error_v1(_error):
            raise RuntimeError("diagnostic hook failure")

        module = types.SimpleNamespace(
            STRATEGY_ID="constructor-strategy",
            STRATEGY_VERSION="2.1",
            STRATEGY_MANIFEST={"warmup_bars": 0},
            create_strategy=create_strategy,
            on_error_v1=on_error_v1,
        )
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            with self.assertRaisesRegex(ValueError, "original constructor failure"):
                _handler_from_module(module, {"data_level": "bar"})
        self.assertIn("original initialization error retained", stderr.getvalue())

    def test_strategy_file_worker_imports_sibling_helpers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "helper.py").write_text("VALUE = 42\n", encoding="utf-8")
            strategy_path = root / "strategy.py"
            strategy_path.write_text("import helper\nVALUE = helper.VALUE\n", encoding="utf-8")
            module = _load_strategy_module(str(strategy_path))
            self.assertEqual(module.VALUE, 42)

    def test_runtime_class_worker_rejects_manifest_data_level_conflict(self):
        class TickStrategy(BaseStrategy):
            STRATEGY_ID = "tick-strategy"
            STRATEGY_VERSION = "1.0"
            STRATEGY_MANIFEST = {
                "schema_version": 1,
                "strategy_id": "tick-strategy",
                "strategy_version": "1.0",
                "parameters": {},
                "instrument_requirements": {
                    "asset_classes": [],
                    "products": [],
                    "venues": [],
                    "requires_explicit_spec": False,
                },
                "data_level": "tick",
                "warmup_bars": 0,
                "dependencies": [],
            }

        with self.assertRaisesRegex(
            ValueError,
            "runtime python_strategy_spec.data_level conflicts with STRATEGY_MANIFEST.data_level",
        ):
            _handler_from_module(types.SimpleNamespace(STRATEGY_CLASS=TickStrategy), {})

    def test_runtime_timer_data_level_dispatches_one_scheduler_event_per_job(self):
        class TimerStrategy(BaseStrategy):
            STRATEGY_ID = "timer-strategy"
            STRATEGY_VERSION = "1.0"
            STRATEGY_MANIFEST = {
                "schema_version": 1,
                "strategy_id": "timer-strategy",
                "strategy_version": "1.0",
                "parameters": {},
                "instrument_requirements": {
                    "asset_classes": [],
                    "products": [],
                    "venues": [],
                    "requires_explicit_spec": False,
                },
                "data_level": "timer",
                "warmup_bars": 0,
                "dependencies": [],
            }

            def __init__(self, _spec):
                self.timers = []
                self.bars = []
                self.callbacks = []

            def on_timer(self, context, timer):
                self.timers.append((timer["name"], timer["ts"], context["as_of"]))
                self.callbacks.append(f"market:Timer:{timer['ts']}")
                return self.decision(context, "Timer", timer)

            def on_bar(self, context, bar):
                self.bars.append(bar["ts"])
                return self.decision(context, "Bar", bar)

        handler = _handler_from_module(
            types.SimpleNamespace(STRATEGY_CLASS=TimerStrategy),
            {"data_level": "timer"},
        )
        fixture_root = (
            Path(__file__).resolve().parents[2]
            / "crates"
            / "qx-strategy"
            / "tests"
            / "fixtures"
        )
        timer_event = json.loads(
            (fixture_root / "strategy-market-timer-v1.json").read_text(
                encoding="utf-8"
            )
        )["Timer"]
        first_request = _request().to_dict()
        first_request["as_of"] = timer_event["ts"]
        first = json.loads(_handle_line(handler, json.dumps(first_request)))
        second_value = _request().to_dict()
        second_value.update(request_id="job-3", as_of=timer_event["ts"] + 1)
        second = json.loads(
            _handle_line(handler, json.dumps(second_value, separators=(",", ":")))
        )

        self.assertTrue(first["ok"], first)
        self.assertTrue(second["ok"], second)
        self.assertEqual(
            handler.strategy.timers,
            [
                (timer_event["name"], timer_event["ts"], timer_event["ts"]),
                (timer_event["name"], timer_event["ts"] + 1, timer_event["ts"] + 1),
            ],
        )
        self.assertEqual(handler.strategy.bars, [])
        self.assertEqual(first["output"]["signal_id"], timer_event["ts"])
        self.assertEqual(second["output"]["signal_id"], timer_event["ts"] + 1)
        matrix = json.loads(
            (fixture_root / "strategy-lifecycle-matrix-v1.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(
            handler.strategy.callbacks[:1], matrix["expected_timer_callbacks"]
        )
        handler.close()

    def test_runtime_timer_data_level_rejects_bar_warmup(self):
        observed_errors = []

        class TimerWarmupStrategy(BaseStrategy):
            STRATEGY_MANIFEST = {"warmup_bars": 1}

            def __init__(self, _spec):
                pass

            def on_error_v1(self, error):
                observed_errors.append(error.to_dict())

        handler = _handler_from_module(
            types.SimpleNamespace(STRATEGY_CLASS=TimerWarmupStrategy),
            {"data_level": "timer"},
        )
        response = json.loads(_handle_line(handler, _request().to_json()))
        self.assertFalse(response["ok"])
        self.assertIn("Runtime Tick/Timer", response["error"])
        self.assertEqual(observed_errors[0]["stage"], "history_requirements")
        self.assertEqual(observed_errors[0]["timestamp"], 2)
        self.assertEqual(observed_errors[0]["instrument"], "BTCUSDT.BINANCE")

    def test_history_requirements_failure_reports_v1_and_legacy_hooks(self):
        observed_v1 = []
        observed_legacy = []

        class BrokenHistoryRequirements(BaseStrategy):
            def __init__(self, _spec):
                pass

            def history_requirements(self):
                raise RuntimeError("warmup declaration failed")

            def on_error_v1(self, error):
                observed_v1.append(error.to_dict())

            def on_error(self, context, error):
                observed_legacy.append((context["strategy_id"], str(error)))

        handler = _handler_from_module(
            types.SimpleNamespace(STRATEGY_CLASS=BrokenHistoryRequirements),
            {"data_level": "bar"},
        )
        response = json.loads(_handle_line(handler, _request().to_json()))
        self.assertFalse(response["ok"])
        self.assertEqual(response["error"], "warmup declaration failed")
        self.assertEqual(
            observed_v1,
            [
                {
                    "schema_version": 1,
                    "stage": "history_requirements",
                    "strategy_id": "sma",
                    "strategy_version": "1.0",
                    "timestamp": 2,
                    "instrument": "BTCUSDT.BINANCE",
                    "message": "warmup declaration failed",
                }
            ],
        )
        self.assertEqual(observed_legacy, [("sma", "warmup declaration failed")])

    def test_class_mode_runs_lifecycle_warmup_and_returns_contract_output(self):
        created = []

        def create_strategy(spec):
            created.append(spec)
            return _LifecycleStrategy(spec)

        handler = _handler_from_module(
            types.SimpleNamespace(create_strategy=create_strategy),
            {"strategy": {"kind": "sma"}, "parameters": {"fast": 2}},
            account_id="main",
            venue_id="binance",
        )
        response = json.loads(_handle_line(handler, _request().to_json()))
        self.assertTrue(response["ok"], response)
        self.assertEqual(created, [{"strategy": {"kind": "sma"}, "parameters": {"fast": 2}}])
        self.assertEqual(response["output"]["request_id"], "job-2")
        self.assertEqual(response["output"]["instrument"], "BTCUSDT.BINANCE")
        self.assertEqual(response["output"]["intents"][0]["intent_id"], 10)
        self.assertEqual(
            handler.strategy.calls,
            [
                ("init", "main"),
                ("start", "sma"),
                ("bar", 1),
                ("warmup", "BTCUSDT", "BINANCE", 2),
                ("bar", 2),
            ],
        )
        fixture = json.loads(
            (
                Path(__file__).resolve().parents[2]
                / "crates"
                / "qx-strategy"
                / "tests"
                / "fixtures"
                / "strategy-warmup-complete-v1.json"
            ).read_text(encoding="utf-8")
        )
        callback_order = []
        for call in handler.strategy.calls:
            if call[0] == "init":
                callback_order.append("on_init")
            elif call[0] == "start":
                callback_order.append("on_start")
            elif call[0] == "bar":
                callback_order.append(f"on_bar:{call[1]}")
            elif call[0] == "warmup":
                callback_order.append(
                    f"on_warmup_complete:{call[1]}.{call[2]}:{call[3]}"
                )
        self.assertEqual(callback_order, fixture["expected_callback_order"][:-1])
        handler.close()
        self.assertEqual(handler.strategy.calls[-1], ("stop",))
        callback_order.append("on_stop")
        self.assertEqual(callback_order, fixture["expected_callback_order"])

    def test_strategy_file_class_matches_shared_normalized_warmup_lifecycle_matrix(self):
        matrix = json.loads(
            (
                Path(__file__).resolve().parents[2]
                / "crates"
                / "qx-strategy"
                / "tests"
                / "fixtures"
                / "strategy-lifecycle-matrix-v1.json"
            ).read_text(encoding="utf-8")
        )

        base = _request()
        timestamps = tuple(matrix["bar_timestamps"])
        request = StrategyInput(
            **{
                **base.__dict__,
                "strategy_id": "lifecycle-golden",
                "as_of": timestamps[-1],
                "bars": StrategyBars(
                    source="strategy-lifecycle-matrix-v1",
                    ts=timestamps,
                    open_raw=(100, 100),
                    high_raw=(101, 101),
                    low_raw=(99, 99),
                    close_raw=(100, 100),
                    volume_raw=(1, 1),
                ),
            }
        )
        strategy_source = f'''from qianxing_strategy import BaseStrategy

class MatrixStrategy(BaseStrategy):
    STRATEGY_MANIFEST = {{"warmup_bars": {matrix["warmup_bars"]}}}

    def __init__(self, _spec):
        self.callbacks = []

    def on_init(self, _context):
        self.callbacks.append("on_init")

    def on_start(self, _context):
        self.callbacks.append("on_start")

    def on_warmup_complete(self, _context, instrument, observed_bars):
        self.callbacks.append(
            f"warmup_complete:{{instrument['symbol']}}.{{instrument['venue']}}:{{observed_bars}}"
        )

    def on_bar(self, context, bar):
        self.callbacks.append(f"market:Bar:{{bar['ts']}}")
        return self.decision(context, "Bar", bar)

    def on_stop(self, _context=None, reason="host_stop"):
        self.callbacks.append("on_stop" if reason == "host_stop" else f"on_stop:{{reason}}")

STRATEGY_CLASS = MatrixStrategy
'''
        with tempfile.TemporaryDirectory() as directory:
            strategy_path = Path(directory) / "strategy_matrix.py"
            strategy_path.write_text(strategy_source, encoding="utf-8")
            module = _load_strategy_module(str(strategy_path))
            handler = _handler_from_module(
                module,
                {"strategy": {}, "parameters": {}},
            )
            response = json.loads(_handle_line(handler, request.to_json()))
            self.assertTrue(response["ok"], response)
            handler.close()
            self.assertEqual(
                handler.strategy.callbacks,
                matrix["expected_normalized_callback_order"],
            )

    def test_class_mode_receives_source_precision_in_each_bar_event(self):
        observed = []

        class ExactStrategy(BaseStrategy):
            def __init__(self, _spec):
                pass

            def on_bar(self, context, bar):
                observed.append(bar.get("exact_ohlcv"))
                return None

        exact_rows = [
            {
                "open": {"coefficient": "1000000000005", "scale": 10},
                "high": {"coefficient": "1010000000005", "scale": 10},
                "low": {"coefficient": "990000000005", "scale": 10},
                "close": {"coefficient": "1000000000005", "scale": 10},
                "volume": {"coefficient": "100000000005", "scale": 10},
            },
            {
                "open": {"coefficient": "1100000000005", "scale": 10},
                "high": {"coefficient": "1110000000005", "scale": 10},
                "low": {"coefficient": "1090000000005", "scale": 10},
                "close": {"coefficient": "1100000000005", "scale": 10},
                "volume": {"coefficient": "120000000005", "scale": 10},
            },
        ]
        request_value = _request().to_dict()
        request_value["schema_version"] = 5
        request_value["bars"]["exact_ohlcv"] = exact_rows
        request_value["bars"].update(
            open_raw=[100_000_000_000, 110_000_000_000],
            high_raw=[101_000_000_000, 111_000_000_000],
            low_raw=[99_000_000_000, 109_000_000_000],
            close_raw=[100_000_000_000, 110_000_000_000],
            volume_raw=[10_000_000_000, 12_000_000_000],
        )
        request = StrategyInput.from_dict(request_value)
        handler = _handler_from_module(types.SimpleNamespace(STRATEGY_CLASS=ExactStrategy), {})
        response = json.loads(_handle_line(handler, request.to_json()))
        self.assertTrue(response["ok"], response)
        self.assertEqual(observed, exact_rows)
        handler.close()

    def test_class_checkpoint_restores_strategy_and_adapter_state_after_worker_restart(self):
        fixture_root = (
            Path(__file__).resolve().parents[2]
            / "crates"
            / "qx-strategy"
            / "tests"
            / "fixtures"
        )
        matrix = json.loads(
            (fixture_root / "strategy-lifecycle-matrix-v1.json").read_text(
                encoding="utf-8"
            )
        )
        fail_next_resumed_bar = [False]
        failed_state_after_mutation = []
        resumed_timestamp = matrix["bar_timestamps"][-1] + 1

        class StatefulStrategy(BaseStrategy):
            STRATEGY_MANIFEST = {"warmup_bars": matrix["warmup_bars"]}

            def __init__(self):
                self.bars_processed = 0
                self.restores = 0
                self.callbacks = []

            def on_warmup_complete(self, _context, instrument, observed_bars):
                self.callbacks.append(
                    f"warmup_complete:{instrument['symbol']}.{instrument['venue']}:{observed_bars}"
                )

            def on_bar(self, context, bar):
                self.bars_processed += 1
                self.callbacks.append(f"market:Bar:{bar['ts']}")
                if bar["ts"] == resumed_timestamp and fail_next_resumed_bar[0]:
                    failed_state_after_mutation.append(self.bars_processed)
                    fail_next_resumed_bar[0] = False
                    raise RuntimeError("injected callback failure after state mutation")
                return None

            def snapshot_state(self):
                return {"bars_processed": self.bars_processed}

            def restore_state(self, state):
                if set(state) != {"bars_processed"}:
                    raise ValueError("checkpoint strategy state fields are invalid")
                self.bars_processed = state["bars_processed"]

            def on_restore(self, context):
                self.restores += 1
                self.callbacks.append("on_restore")

        module = types.SimpleNamespace(create_strategy=lambda _spec: StatefulStrategy())
        first_worker = _handler_from_module(module, {})
        first_base = _request()
        timestamps = tuple(matrix["bar_timestamps"])
        first_request = StrategyInput(
            **{
                **first_base.__dict__,
                "schema_version": 1,
                "strategy_id": "checkpoint-golden",
                "as_of": timestamps[-1],
                "bars": StrategyBars(
                    source="strategy-lifecycle-matrix-v1",
                    ts=timestamps,
                    open_raw=(100, 100),
                    high_raw=(101, 101),
                    low_raw=(99, 99),
                    close_raw=(100, 100),
                    volume_raw=(1, 1),
                ),
            }
        )
        first = json.loads(
            _handle_line(
                first_worker,
                first_request.to_json(),
                checkpoint_handler=first_worker.strategy_checkpoint,
            )
        )
        self.assertTrue(first["ok"], first)
        checkpoint = first["strategy_checkpoint"]
        self.assertEqual(
            checkpoint,
            matrix["expected_checkpoint_after_first_batch"],
        )

        restarted_worker = _handler_from_module(module, {})
        resumed_request = StrategyInput(
            **{
                **first_base.__dict__,
                "schema_version": 4,
                "request_id": "checkpoint-resume",
                "strategy_id": "checkpoint-golden",
                "as_of": timestamps[-1] + 1,
                "strategy_checkpoint": checkpoint,
                "bars": StrategyBars(
                    source="strategy-lifecycle-matrix-v1",
                    ts=(*timestamps, timestamps[-1] + 1),
                    open_raw=(100, 100, 100),
                    high_raw=(101, 101, 101),
                    low_raw=(99, 99, 99),
                    close_raw=(100, 100, 100),
                    volume_raw=(1, 1, 1),
                ),
            }
        )
        fail_next_resumed_bar[0] = True
        failed = json.loads(
            _handle_line(
                restarted_worker,
                resumed_request.to_json(),
                checkpoint_handler=restarted_worker.strategy_checkpoint,
            )
        )
        self.assertEqual(
            failed,
            {
                "ok": False,
                "error": "injected callback failure after state mutation",
            },
        )
        self.assertEqual(failed_state_after_mutation, [3])
        self.assertNotIn("strategy_checkpoint", failed)
        self.assertEqual(restarted_worker.strategy.bars_processed, 0)
        self.assertEqual(restarted_worker.strategy.callbacks, [])

        # The failed callback mutated user state before raising. The adapter must
        # discard that in-memory transaction; retrying the same batch from the
        # last committed host checkpoint must produce the exact golden result.
        second = json.loads(
            _handle_line(
                restarted_worker,
                resumed_request.to_json(),
                checkpoint_handler=restarted_worker.strategy_checkpoint,
            )
        )
        self.assertTrue(second["ok"], second)
        self.assertEqual(restarted_worker.strategy.bars_processed, 3)
        self.assertEqual(restarted_worker.strategy.restores, 1)
        self.assertEqual(
            restarted_worker.strategy.callbacks,
            matrix["expected_checkpoint_resume_callbacks"],
        )
        self.assertEqual(
            second["strategy_checkpoint"],
            matrix["expected_checkpoint_after_restart"],
        )

    def test_updates_are_ordered_and_deduplicated_within_worker_lifetime(self):
        handler = _handler_from_module(
            types.SimpleNamespace(create_strategy=_LifecycleStrategy),
            {"strategy": {}, "parameters": {}},
            account_id="main",
            venue_id="binance",
        )
        request = _request(updates=True)
        first = json.loads(_handle_line(handler, request.to_json()))
        self.assertTrue(first["ok"], first)
        self.assertLess(
            handler.strategy.calls.index(("order", 7, "partially_filled")),
            handler.strategy.calls.index(("fill", 5)),
        )
        previous_calls = list(handler.strategy.calls)
        second = json.loads(_handle_line(handler, request.to_json()))
        self.assertTrue(second["ok"], second)
        self.assertEqual(handler.strategy.calls, previous_calls)

    def test_account_eventlog_cursor_checkpoint_skips_replay_after_worker_restart(self):
        fixture_root = (
            Path(__file__).resolve().parents[2]
            / "crates"
            / "qx-strategy"
            / "tests"
            / "fixtures"
        )
        matrix = json.loads(
            (fixture_root / "strategy-lifecycle-matrix-v1.json").read_text(
                encoding="utf-8"
            )
        )
        fixture = json.loads(
            (fixture_root / "strategy-account-update-sequence-v1.json").read_text(
                encoding="utf-8"
            )
        )

        class AccountCheckpointStrategy(BaseStrategy):
            def __init__(self):
                self.events = []
                self.resume_callbacks = []
                self.restored = False

            def _record(self, event):
                self.events.append(event)
                if self.restored:
                    self.resume_callbacks.append(event)

            def on_account(self, context, snapshot):
                self._record(
                    f"{context['account_id']}:{context['venue_id']}:{snapshot['event_seq']}:account_snapshot"
                )

            def on_positions_snapshot(self, context, snapshot):
                self._record(
                    f"{context['account_id']}:{context['venue_id']}:{snapshot['event_seq']}:position_snapshot"
                )

            def snapshot_state(self):
                return {"events": list(self.events)}

            def restore_state(self, state):
                if set(state) != {"events"} or not isinstance(state["events"], list):
                    raise ValueError("account checkpoint state is invalid")
                self.events = list(state["events"])

            def on_restore(self, _context):
                self.restored = True
                self.resume_callbacks.append("on_restore")

        module = types.SimpleNamespace(
            create_strategy=lambda _spec: AccountCheckpointStrategy()
        )
        base = _request()
        account_events = [dict(event) for event in fixture["events"]]
        first_request = StrategyInput(
            **{
                **base.__dict__,
                "schema_version": 9,
                "strategy_id": fixture["strategy_id"],
                "as_of": 10,
                "cash_exact": {"USDT": "0.0001"},
                "account_updates": tuple(account_events),
            }
        )
        first_worker = _handler_from_module(
            module,
            {},
            account_id=fixture["account_id"],
            venue_id=fixture["venue_id"],
        )
        first = json.loads(
            _handle_line(
                first_worker,
                first_request.to_json(),
                checkpoint_handler=first_worker.strategy_checkpoint,
            )
        )
        self.assertTrue(first["ok"], first)
        checkpoint = first["strategy_checkpoint"]
        self.assertEqual(
            checkpoint,
            matrix["expected_account_checkpoint_after_first_batch"],
        )
        self.assertEqual(
            first_worker.strategy.events,
            matrix["expected_account_update_callbacks"],
        )

        new_position_snapshot = dict(fixture["events"][-1])
        new_position_snapshot.update(event_seq=24, ts=11)
        replayed_plus_new_events = (*account_events, new_position_snapshot)
        resumed_request = StrategyInput(
            **{
                **base.__dict__,
                "schema_version": 9,
                "request_id": "account-checkpoint-resume",
                "strategy_id": fixture["strategy_id"],
                "as_of": 11,
                "cash_exact": {"USDT": "0.0001"},
                "strategy_checkpoint": checkpoint,
                "account_updates": replayed_plus_new_events,
            }
        )
        restarted_worker = _handler_from_module(
            module,
            {},
            account_id=fixture["account_id"],
            venue_id=fixture["venue_id"],
        )
        resumed = json.loads(
            _handle_line(
                restarted_worker,
                resumed_request.to_json(),
                checkpoint_handler=restarted_worker.strategy_checkpoint,
            )
        )
        self.assertTrue(resumed["ok"], resumed)
        self.assertEqual(
            restarted_worker.strategy.resume_callbacks,
            matrix["expected_account_checkpoint_resume_callbacks"],
        )
        self.assertEqual(
            restarted_worker.strategy.events,
            matrix["expected_account_checkpoint_after_restart"]["strategy_state"][
                "events"
            ],
        )
        self.assertEqual(
            resumed["strategy_checkpoint"],
            matrix["expected_account_checkpoint_after_restart"],
        )

    def test_multi_account_updates_use_scoped_cursors_and_shared_callback_order(self):
        class SequenceStrategy(BaseStrategy):
            def __init__(self):
                self.events = []

            def on_order_status_update(self, context, update):
                self.events.append(
                    (
                        "order_status",
                        update["order"]["account_id"],
                        update["order"]["instrument"],
                        update["event_seq"],
                        context["account_id"],
                        context["venue_id"],
                        context["instrument"],
                    )
                )

            def on_fill_update(self, context, update):
                self.events.append(
                    (
                        "fill",
                        update["order"]["account_id"],
                        update["order"]["instrument"],
                        update["event_seq"],
                        context["account_id"],
                        context["venue_id"],
                        context["instrument"],
                    )
                )

        repository = Path(__file__).resolve().parents[2]
        fixture_path = (
            repository
            / "crates"
            / "qx-strategy"
            / "tests"
            / "fixtures"
            / "strategy-execution-sequence-v1.json"
        )
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
        for event in fixture["events"]:
            update = event["update"]
            if update["order"]["account_id"] == "paper-b":
                update["order"]["account_id"] = "paper"
                if event["kind"] == "fill":
                    update["fill"]["account_id"] = "paper"
        expected_callback_order = [
            item.replace("paper-b:", "paper:")
            for item in fixture["expected_callback_order"]
        ]
        statuses = tuple(
            event["update"]
            for event in fixture["events"]
            if event["kind"] == "order_status"
        )
        fills = tuple(
            event["update"]
            for event in fixture["events"]
            if event["kind"] == "fill"
        )
        base_request = _request()
        request = StrategyInput(
            **{
                **base_request.__dict__,
                "schema_version": 3,
                "strategy_id": fixture["strategy_id"],
                "as_of": 12,
                "execution_updates": statuses,
                "fill_updates": fills,
            }
        )
        handler = _handler_from_module(
            types.SimpleNamespace(create_strategy=lambda _spec: SequenceStrategy()),
            {},
        )
        first = json.loads(_handle_line(handler, request.to_json()))
        self.assertTrue(first["ok"], first)
        callback_order = [
            f"{account}:{instrument}:{event_seq}:{kind}"
            for kind, account, instrument, event_seq, *_ in handler.strategy.events
        ]
        self.assertEqual(callback_order, expected_callback_order)
        self.assertEqual(handler.strategy.events[4][4:6], ("paper", "OKX"))
        self.assertEqual(
            handler.strategy.events[4][6],
            {"symbol": "ETHUSDT", "venue": "OKX"},
        )

        repeated = json.loads(_handle_line(handler, request.to_json()))
        self.assertTrue(repeated["ok"], repeated)
        self.assertEqual(len(handler.strategy.events), len(fixture["events"]))

    def test_account_and_empty_position_snapshots_match_shared_cross_host_fixture(self):
        class SnapshotStrategy(BaseStrategy):
            def __init__(self):
                self.events = []

            def on_account(self, context, snapshot):
                self.events.append(
                    f"{context['account_id']}:{context['venue_id']}:{snapshot['event_seq']}:account_snapshot"
                )

            def on_positions_snapshot(self, context, snapshot):
                self.events.append(
                    f"{context['account_id']}:{context['venue_id']}:{snapshot['event_seq']}:position_snapshot"
                )

        repository = Path(__file__).resolve().parents[2]
        fixture_path = (
            repository
            / "crates"
            / "qx-strategy"
            / "tests"
            / "fixtures"
            / "strategy-account-update-sequence-v1.json"
        )
        fixture = json.loads(fixture_path.read_text(encoding="utf-8"))
        lifecycle_matrix = json.loads(
            (fixture_path.parent / "strategy-lifecycle-matrix-v1.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(
            fixture["expected_callback_order"],
            lifecycle_matrix["expected_account_update_callbacks"],
        )
        base_request = _request()
        request = StrategyInput(
            **{
                **base_request.__dict__,
                "schema_version": 7,
                "strategy_id": fixture["strategy_id"],
                "as_of": 10,
                "account_updates": tuple(fixture["events"]),
            }
        )
        handler = _handler_from_module(
            types.SimpleNamespace(create_strategy=lambda _spec: SnapshotStrategy()),
            {},
            account_id=fixture["account_id"],
            venue_id=fixture["venue_id"],
        )

        response = json.loads(_handle_line(handler, request.to_json()))

        self.assertTrue(response["ok"], response)
        self.assertEqual(handler.strategy.events, fixture["expected_callback_order"])

    def test_class_mode_rejects_non_base_strategy_factory_result(self):
        with self.assertRaisesRegex(TypeError, "must return a BaseStrategy"):
            _handler_from_module(
                types.SimpleNamespace(create_strategy=lambda _spec: object()),
                {},
            )

    def test_class_mode_rejects_future_bars_before_callback(self):
        handler = _handler_from_module(
            types.SimpleNamespace(create_strategy=_LifecycleStrategy),
            {"strategy": {}, "parameters": {}},
        )
        request = _request()
        request = StrategyInput(
            **{
                **request.__dict__,
                "bars": StrategyBars(
                    source="fixture",
                    ts=(1, 3),
                    open_raw=(100, 110),
                    high_raw=(101, 111),
                    low_raw=(99, 109),
                    close_raw=(100, 110),
                    volume_raw=(10, 12),
                ),
            }
        )
        response = json.loads(_handle_line(handler, request.to_json()))
        self.assertFalse(response["ok"])
        self.assertIn("after as_of", response["error"])
        self.assertEqual(handler.strategy.calls, [])

    def test_empty_decision_preserves_exact_current_position(self):
        handler = _handler_from_module(
            types.SimpleNamespace(create_strategy=lambda _spec: BaseStrategy()),
            {},
        )
        request = _request()
        request = StrategyInput(
            **{
                **request.__dict__,
                "positions": {"BTCUSDT.BINANCE": 0},
                "positions_exact": {"BTCUSDT.BINANCE": "0.000000000123456789"},
                "bars": StrategyBars(
                    source="fixture",
                    ts=(2,),
                    open_raw=(100,),
                    high_raw=(101,),
                    low_raw=(99,),
                    close_raw=(100,),
                    volume_raw=(10,),
                ),
            }
        )
        response = json.loads(_handle_line(handler, request.to_json()))
        self.assertTrue(response["ok"], response)
        self.assertEqual(response["output"]["target_qty"], 0)
        self.assertEqual(response["output"]["target_qty_exact"], "0.000000000123456789")
        self.assertEqual(response["output"]["intents"], [])

    def test_class_intent_helper_exact_scales_and_policy_map_to_process_wire(self):
        class ExactStrategy(BaseStrategy):
            def on_bar(self, context, bar):
                intent = self.order_intent(
                    intent_id=22,
                    instrument=bar["instrument"],
                    side="buy",
                    qty_exact="0.000000000123456789",
                    limit_price_exact="0.00000005",
                    policy={
                        "position_side": "net",
                        "margin_mode": "cross",
                        "position_mode": "one_way",
                        "leverage": 3,
                    },
                )
                return self.decision(context, "Bar", bar, intents=[intent])

        handler = _handler_from_module(
            types.SimpleNamespace(create_strategy=lambda _spec: ExactStrategy()),
            {},
        )
        response = json.loads(_handle_line(handler, _request().to_json()))
        self.assertTrue(response["ok"], response)
        intent = response["output"]["intents"][0]
        self.assertEqual(intent["qty_raw"], 0)
        self.assertEqual(intent["qty_exact"], "0.000000000123456789")
        self.assertEqual(intent["limit_price_exact"], "0.00000005")
        self.assertEqual(intent["position_side"], "net")
        self.assertEqual(intent["margin_mode"], "cross")
        self.assertEqual(intent["leverage"], 3)

    def test_class_short_and_cover_helpers_map_hedge_policy_and_reduce_only(self):
        class HedgeStrategy(BaseStrategy):
            def on_bar(self, context, bar):
                policy = {
                    "position_side": "short",
                    "margin_mode": "cross",
                    "position_mode": "hedge",
                    "leverage": 3,
                }
                return self.decision(
                    context,
                    "Bar",
                    bar,
                    intents=[
                        self.short(
                            bar["instrument"],
                            qty_exact="0.125",
                            policy=policy,
                        ),
                        self.cover(
                            bar["instrument"],
                            qty_exact="0.05",
                            policy=policy,
                        ),
                    ],
                )

        handler = _handler_from_module(
            types.SimpleNamespace(create_strategy=lambda _spec: HedgeStrategy()),
            {},
        )
        response = json.loads(_handle_line(handler, _request().to_json()))
        self.assertTrue(response["ok"], response)
        short_intent, cover_intent = response["output"]["intents"]
        self.assertEqual((short_intent["side"], short_intent["position_side"]), ("sell", "short"))
        self.assertFalse(short_intent["reduce_only"])
        self.assertEqual(short_intent["position_mode"], "hedge")
        self.assertEqual(short_intent["leverage"], 3)
        self.assertEqual((cover_intent["side"], cover_intent["position_side"]), ("buy", "short"))
        self.assertTrue(cover_intent["reduce_only"])
        self.assertEqual(cover_intent["position_mode"], "hedge")
        self.assertEqual(cover_intent["qty_exact"], "0.05")

    def test_callback_failure_rolls_back_class_state_before_frozen_request_retry(self):
        failures_left = {"count": 1}
        observed_errors = []
        observed_errors_v1 = []

        class FailOnceStrategy(BaseStrategy):
            def __init__(self):
                self.seen = []

            def on_bar(self, context, bar):
                self.seen.append(bar["ts"])
                if bar["ts"] == 2 and failures_left["count"]:
                    failures_left["count"] -= 1
                    raise ValueError("injected callback failure")
                return self.decision(context, "Bar", bar)

            def on_error(self, context, error):
                observed_errors.append(
                    (context["strategy_id"], context["as_of"], type(error).__name__, str(error))
                )
                raise RuntimeError("diagnostic hook failure")

            def on_error_v1(self, error):
                observed_errors_v1.append(error.to_dict())

        factory_calls = []

        def create_strategy(_spec):
            instance = FailOnceStrategy()
            factory_calls.append(instance)
            return instance

        handler = _handler_from_module(
            types.SimpleNamespace(create_strategy=create_strategy),
            {},
        )
        request = _request()
        failed = json.loads(_handle_line(handler, request.to_json()))
        self.assertFalse(failed["ok"])
        self.assertEqual(failed["error"], "injected callback failure")
        self.assertEqual(
            observed_errors,
            [("sma", 2, "ValueError", "injected callback failure")],
        )
        self.assertEqual(
            observed_errors_v1,
            [
                {
                    "schema_version": 1,
                    "stage": "market_event",
                    "strategy_id": "sma",
                    "strategy_version": "1.0",
                    "timestamp": 2,
                    "instrument": "BTCUSDT.BINANCE",
                    "message": "injected callback failure",
                }
            ],
        )
        fixture_root = (
            Path(__file__).resolve().parents[2]
            / "crates"
            / "qx-strategy"
            / "tests"
            / "fixtures"
        )
        error_fixture = json.loads(
            (fixture_root / "strategy-error-v1.json").read_text(encoding="utf-8")
        )
        matrix = json.loads(
            (fixture_root / "strategy-lifecycle-matrix-v1.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(
            [f"on_error_v1:{error_fixture['stage']}"],
            matrix["expected_error_callbacks"],
        )
        self.assertEqual(
            [f"on_error_v1:{error['stage']}" for error in observed_errors_v1],
            matrix["expected_error_callbacks"],
        )
        self.assertEqual(factory_calls[0].seen, [1, 2])
        self.assertIs(handler.strategy, factory_calls[1])

        retried = json.loads(_handle_line(handler, request.to_json()))
        self.assertTrue(retried["ok"], retried)
        self.assertEqual(handler.strategy.seen, [1, 2])

    def test_module_function_on_error_receives_request_and_cannot_replace_failure(self):
        observed_errors = []
        observed_errors_v1 = []

        def on_event(request):
            raise ValueError(f"module callback failed at {request.as_of}")

        def on_error(request, error):
            observed_errors.append(
                (request.strategy_id, request.as_of, type(error).__name__, str(error))
            )
            raise RuntimeError("diagnostic hook failure")

        def on_error_v1(error):
            observed_errors_v1.append(error.to_dict())

        handler = _handler_from_module(
            types.SimpleNamespace(on_event=on_event, on_error=on_error, on_error_v1=on_error_v1)
        )
        request = _request()
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            response = json.loads(_handle_line(handler, request.to_json()))

        self.assertFalse(response["ok"])
        self.assertEqual(response["error"], "module callback failed at 2")
        self.assertEqual(
            observed_errors,
            [("sma", 2, "ValueError", "module callback failed at 2")],
        )
        self.assertEqual(
            observed_errors_v1,
            [
                {
                    "schema_version": 1,
                    "stage": "market_event",
                    "strategy_id": "sma",
                    "strategy_version": "1.0",
                    "timestamp": 2,
                    "instrument": "BTCUSDT.BINANCE",
                    "message": "module callback failed at 2",
                }
            ],
        )
        self.assertIn("strategy on_error hook failed", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
