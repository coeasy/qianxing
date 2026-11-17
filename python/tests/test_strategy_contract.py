import json
import hashlib
import io
import json
import os
import signal
import struct
import subprocess
import tempfile
import sys
import time
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing_strategy import StrategyBars, StrategyInput, StrategyIntent, StrategyOutput
from qianxing_bridge.strategy import account_position_key
from examples import strategy_exact_amount, strategy_exact_target, strategy_multi_venue_contract
from qianxing_strategy.frame import REQUEST, RESPONSE, encode_frame, read_frame
from qianxing_strategy.ring import RingEmpty, RingFull, SharedMemoryRing
from qianxing_strategy.columnar import decode_request
from qianxing_strategy.worker import (
    PARENT_LIVENESS_PROBE_SECONDS,
    _handle_line,
    _load_handler,
    _parent_process_alive,
)


class StrategyContractTest(unittest.TestCase):
    def setUp(self):
        self.request = StrategyInput(
            request_id="request-1",
            strategy_id="strategy-1",
            strategy_version="v1",
            data_fingerprint="bars-1",
            as_of=10,
            instrument="BTCUSDT.BINANCE",
            positions={"BTCUSDT.BINANCE": 2},
            cash={"USDT": 100},
            available_margin_raw=90,
            risk_state="verified",
            research_targets={"BTCUSDT.BINANCE": 3},
            bars=StrategyBars("snapshot-1", (9, 10), (1, 2), (2, 3), (1, 2), (2, 3), (10, 11)),
        )

    def test_round_trip_and_identity_binding(self):
        restored = StrategyInput.from_json(self.request.to_json())
        self.assertEqual(restored, self.request)
        output = StrategyOutput(
            request_id="request-1",
            strategy_id="strategy-1",
            signal_id=1,
            instrument="BTCUSDT.BINANCE",
            target_qty=3,
            confidence=900,
            priority=1,
            expires_at=10,
        )
        payload = output.to_json(self.request)
        self.assertEqual(StrategyOutput.from_json(payload, self.request), output)
        wrong = json.loads(payload)
        wrong["request_id"] = "other"
        with self.assertRaises(ValueError):
            StrategyOutput.from_dict(wrong, self.request)
        # 六种失败必须各说各话：折叠成一句 "identity or expiry" 之后，返回 dict 时漏写
        # schema_version（`from_dict` 缺省读成 0）的作者会去查 request_id，永远查不出问题。
        arms = {
            "schema_version": (0, "schema_version must be 1"),
            "strategy_id": ("other", "strategy_id does not match input"),
            "signal_id": (0, "signal_id must be positive"),
            "expires_at": (9, "expires_at must not precede input as_of"),
            "instrument": ("ETHUSDT.BINANCE", "instrument does not match input"),
        }
        for field, (value, expect) in arms.items():
            mutated = json.loads(payload)
            mutated[field] = value
            with self.assertRaisesRegex(ValueError, f"strategy output {expect}") as caught:
                StrategyOutput.from_dict(mutated, self.request)
            self.assertIn(expect, str(caught.exception))
        # 空标的走自己那一条，而不是被"与输入不一致"顺路念掉。
        blank = json.loads(payload)
        blank["instrument"] = "   "
        with self.assertRaisesRegex(ValueError, "strategy output instrument must be non-empty"):
            StrategyOutput.from_dict(blank, self.request)
        # 缺键走的是 `from_dict` 那一格，不是某一臂的值比对：把缺省版本采纳成自家
        # `SCHEMA_VERSION` 时，「漏写 schema_version」在 Python 侧变成合法产出，只有 Rust 拒它。
        omitted = json.loads(payload)
        del omitted["schema_version"]
        with self.assertRaisesRegex(ValueError, "strategy output schema_version must be 1"):
            StrategyOutput.from_dict(omitted, self.request)
        # 逐臂点名不能退化成第二种折叠：同一句身份话术必须只出现在真正不匹配的那一臂。
        messages = set()
        for field, (value, _) in arms.items():
            mutated = json.loads(payload)
            mutated[field] = value
            try:
                StrategyOutput.from_dict(mutated, self.request)
            except ValueError as error:
                messages.add(str(error))
        self.assertEqual(len(messages), len(arms))

    def test_v5_preserves_exact_ohlcv_and_rejects_old_versions_or_projection_drift(self):
        exact_row = {
            "open": {"coefficient": "5", "scale": 10},
            "high": {"coefficient": "5", "scale": 10},
            "low": {"coefficient": "5", "scale": 10},
            "close": {"coefficient": "5", "scale": 10},
            "volume": {"coefficient": "5", "scale": 13},
        }
        bars = StrategyBars(
            "subnano-source", (9,), (0,), (0,), (0,), (0,), (0,), (exact_row,)
        )
        request = StrategyInput(
            schema_version=5,
            request_id="request-v5",
            strategy_id="strategy-v5",
            strategy_version="1.0",
            data_fingerprint="subnano-bars",
            as_of=10,
            instrument="ALTUSDT.BINANCE",
            risk_state="verified",
            bars=bars,
        )
        encoded = request.to_dict()
        self.assertEqual(encoded["bars"]["exact_ohlcv"][0]["close"], exact_row["close"])
        self.assertEqual(StrategyInput.from_dict(encoded), request)
        with self.assertRaisesRegex(ValueError, "v1-v4"):
            StrategyInput.from_dict({**encoded, "schema_version": 1})
        bad_projection = {**encoded, "bars": {**encoded["bars"], "close_raw": [1]}}
        with self.assertRaisesRegex(ValueError, "projection mismatch"):
            StrategyInput.from_dict(bad_projection)
        malformed_exact = {
            **encoded,
            "bars": {**encoded["bars"], "exact_ohlcv": ["not-an-object"]},
        }
        with self.assertRaisesRegex(ValueError, "rows must be objects or null"):
            StrategyInput.from_dict(malformed_exact)
        with self.assertRaisesRegex(ValueError, "requires exact OHLCV"):
            StrategyInput(
                schema_version=5,
                request_id="request-v5-empty",
                strategy_id="strategy-v5",
                strategy_version="1.0",
                data_fingerprint="raw-bars",
                as_of=10,
                instrument="ALTUSDT.BINANCE",
                risk_state="verified",
                bars=StrategyBars("raw", (9,), (0,), (0,), (0,), (0,), (0,)),
            ).validate()

    def test_v8_preserves_exact_order_book_depth_and_rejects_invalid_ordering(self):
        book = {
            "stream_id": "market:binance",
            "event_seq": 9,
            "source_seq": 12,
            "ts": 10,
            "instrument": "ALTUSDT.BINANCE",
            "sequence": 88,
            "bids": [
                {
                    "price_raw": 0,
                    "qty_raw": 0,
                    "price_exact": "0.0000000007",
                    "qty_exact": "0.0000000002",
                }
            ],
            "asks": [
                {
                    "price_raw": 0,
                    "qty_raw": 0,
                    "price_exact": "0.0000000009",
                    "qty_exact": "0.0000000004",
                }
            ],
        }
        request = StrategyInput(
            schema_version=8,
            request_id="request-v8",
            strategy_id="strategy-v8",
            strategy_version="1.0",
            data_fingerprint="order-book-event-log",
            as_of=10,
            instrument="ALTUSDT.BINANCE",
            risk_state="verified",
            market_order_books=(book,),
        )
        encoded = request.to_dict()
        self.assertEqual(encoded["market_order_books"][0]["bids"][0]["price_exact"], "0.0000000007")
        self.assertEqual(StrategyInput.from_dict(encoded), request)
        with self.assertRaisesRegex(ValueError, "raw projection"):
            StrategyInput.from_dict(
                {
                    **encoded,
                    "market_order_books": [
                        {
                            **book,
                            "bids": [{**book["bids"][0], "price_raw": 1}],
                        }
                    ],
                }
            )
        with self.assertRaisesRegex(ValueError, "best bid"):
            StrategyInput.from_dict(
                {
                    **encoded,
                    "market_order_books": [
                        {**book, "asks": [{**book["asks"][0], "price_exact": "0.0000000006"}]}
                    ],
                }
            )
        with self.assertRaisesRegex(ValueError, "v1 cannot contain"):
            StrategyInput.from_dict({**encoded, "schema_version": 1})

    def test_v9_preserves_exact_cash_and_margin_without_order_books(self):
        request = StrategyInput(
            schema_version=9,
            request_id="request-v9",
            strategy_id="strategy-v9",
            strategy_version="1.0",
            data_fingerprint="exact-account-facts",
            as_of=10,
            instrument="ALTUSDT.BINANCE",
            cash={"USDT": 100},
            cash_exact={"USDT": "0.0000001007"},
            available_margin_raw=90,
            available_margin_exact="0.0000000909",
            risk_state="verified",
        )
        encoded = request.to_dict()
        self.assertEqual(encoded["cash_exact"]["USDT"], "0.0000001007")
        self.assertEqual(encoded["available_margin_exact"], "0.0000000909")
        self.assertEqual(encoded["market_order_books"], [])
        self.assertEqual(StrategyInput.from_dict(encoded), request)

        bad_cash = dict(encoded)
        bad_cash["cash_exact"] = {"USDT": "0.000000101"}
        with self.assertRaisesRegex(ValueError, "cash_exact.*conflicts with the raw cash projection"):
            StrategyInput.from_dict(bad_cash)
        bad_margin = dict(encoded)
        bad_margin["available_margin_exact"] = "-0.0000000001"
        with self.assertRaisesRegex(ValueError, "available_margin_exact cannot be negative"):
            StrategyInput.from_dict(bad_margin)

    def test_execution_update_input_v2_round_trip_and_fail_closed_validation(self):
        self.assertNotIn("execution_updates", self.request.to_dict())
        update = {
            "schema_version": 1,
            "event": "order_status",
            "event_seq": 0,
            "ts": 10,
            "status": "submitted",
            "order": {
                "order_id": 9,
                "account_id": "account-1",
                "instrument": "BTCUSDT.BINANCE",
                "side": "Buy",
                "qty_raw": 1_000_000_000,
                "filled_raw": 0,
                "strategy_id": "strategy-1",
            },
        }
        request = StrategyInput(
            request_id="execution-update-v2",
            strategy_id="strategy-1",
            strategy_version="v1",
            data_fingerprint="bars-1",
            as_of=10,
            instrument="BTCUSDT.BINANCE",
            risk_state="verified",
            schema_version=2,
            execution_updates=(update,),
        )
        payload = json.loads(request.to_json())
        self.assertEqual(payload["schema_version"], 2)
        self.assertEqual(payload["execution_updates"][0]["event_seq"], 0)
        self.assertEqual(StrategyInput.from_json(request.to_json()), request)
        output = StrategyOutput(
            request_id=request.request_id,
            strategy_id=request.strategy_id,
            signal_id=1,
            instrument=request.instrument,
            target_qty=0,
        )
        self.assertEqual(output.to_dict(request)["schema_version"], 1)

        for mutate, expected in (
            (lambda value: value["execution_updates"][0].update(event_seq=-1), "non-negative integer"),
            (lambda value: value["execution_updates"][0]["order"].update(strategy_id="other"), "does not match input"),
            (lambda value: value["execution_updates"][0].update(extra="no"), "unknown key"),
        ):
            invalid = json.loads(request.to_json())
            mutate(invalid)
            with self.assertRaisesRegex(ValueError, expected):
                StrategyInput.from_dict(invalid)

        invalid_v1 = self.request.to_dict()
        invalid_v1["execution_updates"] = [update]
        with self.assertRaisesRegex(ValueError, "v1 cannot contain"):
            StrategyInput.from_dict(invalid_v1)
        empty_v2 = self.request.to_dict()
        empty_v2["schema_version"] = 2
        with self.assertRaisesRegex(ValueError, "v2 requires non-empty execution_updates"):
            StrategyInput.from_dict(empty_v2)
        for invalid_version in (True, 1.0, "2"):
            invalid = json.loads(request.to_json())
            invalid["schema_version"] = invalid_version
            with self.assertRaisesRegex(ValueError, "unsupported strategy schema_version"):
                StrategyInput.from_dict(invalid)

    def test_fill_update_input_v3_round_trip_and_fail_closed_validation(self):
        root = Path(__file__).resolve().parents[2]
        fixture_path = root / "crates" / "qx-strategy" / "tests" / "fixtures" / "strategy-execution-sequence-v1.json"
        fixture = json.loads(fixture_path.read_text(encoding="utf-8"))
        status_updates = tuple(
            event["update"] for event in fixture["events"] if event["kind"] == "order_status"
        )
        fill_updates = tuple(
            event["update"] for event in fixture["events"] if event["kind"] == "fill"
        )
        request = StrategyInput(
            request_id="fill-update-v3-shared-golden",
            strategy_id=fixture["strategy_id"],
            strategy_version="v1",
            data_fingerprint="bars-1",
            as_of=12,
            instrument=fixture["instrument"],
            risk_state="verified",
            schema_version=3,
            execution_updates=status_updates,
            fill_updates=fill_updates,
        )
        payload = json.loads(request.to_json())
        self.assertEqual(payload["schema_version"], 3)
        self.assertEqual(payload["execution_updates"], list(status_updates))
        self.assertEqual(payload["fill_updates"], list(fill_updates))
        self.assertEqual([update["event_seq"] for update in status_updates], [24, 25, 24])
        self.assertEqual([update["event_seq"] for update in fill_updates], [24, 25, 24])
        self.assertEqual(StrategyInput.from_json(request.to_json()), request)

        quote_budget_update = json.loads(request.to_json())
        quote_budget_fill = quote_budget_update["fill_updates"][0]["fill"]
        quote_budget_fill["qty"] = 0
        quote_budget_fill["price"] = 0
        quote_budget_fill["exact_amounts"] = {
            "quantity": {"coefficient": "3333", "scale": 3},
            "price": {"coefficient": "3", "scale": 0},
            "quote_cost": {"coefficient": "9999", "scale": 3},
            "quote_budget_complete": True,
        }
        StrategyInput.from_dict(quote_budget_update)

        for value, expected in (
            ({"quantity": {"coefficient": "1", "scale": 0}, "price": {"coefficient": "1", "scale": 0}, "quote_budget_complete": True}, "requires quote_cost"),
            ({"quantity": {"coefficient": "1", "scale": 0}, "price": {"coefficient": "1", "scale": 0}, "quote_cost": {"coefficient": "1", "scale": 0}, "quote_budget_complete": 1}, "must be a boolean"),
        ):
            invalid = json.loads(request.to_json())
            invalid_fill = invalid["fill_updates"][0]["fill"]
            invalid_fill["qty"] = 0
            invalid_fill["price"] = 0
            invalid_fill["exact_amounts"] = value
            with self.assertRaisesRegex(ValueError, expected):
                StrategyInput.from_dict(invalid)

        received = []

        def process_strategy(strategy_input):
            received.extend(strategy_input.execution_updates)
            received.extend(strategy_input.fill_updates)
            return StrategyOutput(
                request_id=strategy_input.request_id,
                strategy_id=strategy_input.strategy_id,
                signal_id=1,
                instrument=strategy_input.instrument,
                target_qty=0,
            )

        worker_response = json.loads(_handle_line(process_strategy, request.to_json()))
        self.assertTrue(worker_response["ok"], worker_response)
        self.assertEqual(received, [*status_updates, *fill_updates])

        for mutate, expected in (
            (lambda value: value["fill_updates"][0].update(event_seq=-1), "non-negative integer"),
            (lambda value: value["fill_updates"][0]["order"].update(strategy_id="other"), "does not match input"),
            (lambda value: value["fill_updates"][0]["fill"].update(ts=9), "match the update timestamp"),
            (lambda value: value["fill_updates"][0].update(extra="no"), "unknown key"),
        ):
            invalid = json.loads(request.to_json())
            mutate(invalid)
            with self.assertRaisesRegex(ValueError, expected):
                StrategyInput.from_dict(invalid)

        empty_v3 = self.request.to_dict()
        empty_v3["schema_version"] = 3
        with self.assertRaisesRegex(ValueError, "v3 requires non-empty fill_updates"):
            StrategyInput.from_dict(empty_v3)

    def test_exact_position_and_signed_exact_target_round_trip(self):
        request = StrategyInput(
            request_id="exact-target-request",
            strategy_id="strategy-exact-target",
            strategy_version="v1",
            data_fingerprint="bars-1",
            as_of=10,
            instrument="PEPEUSDT.BINANCE",
            positions={"PEPEUSDT.BINANCE": 2},
            positions_exact={"PEPEUSDT.BINANCE": "0.0000000021"},
            risk_state="verified",
        )
        restored_request = StrategyInput.from_json(request.to_json())
        self.assertEqual(restored_request.positions_exact, request.positions_exact)

        output = StrategyOutput.for_target_position(
            request,
            signal_id=1,
            target_qty_exact="-0.0000000001",
        )
        restored_output = StrategyOutput.from_json(output.to_json(request), request)
        self.assertEqual(restored_output.target_qty_exact, "-0.0000000001")

    def test_output_builders_bind_identity_and_enforce_wire_integer_types(self):
        raw_target = StrategyOutput.for_target_position(
            self.request,
            signal_id=2,
            target_qty=-17,
            expires_at=self.request.as_of,
        )
        self.assertEqual(raw_target.target_qty, -17)
        self.assertEqual(raw_target.request_id, self.request.request_id)
        self.assertEqual(
            StrategyOutput.from_json(raw_target.to_json(self.request), self.request), raw_target
        )

        exact_target = StrategyOutput.for_target_position(
            self.request,
            signal_id=3,
            target_qty_exact="-0.0000000001",
        )
        self.assertEqual(exact_target.target_qty, 0)
        self.assertEqual(exact_target.target_qty_exact, "-0.0000000001")
        with self.assertRaisesRegex(ValueError, "exactly one"):
            StrategyOutput.for_target_position(self.request, signal_id=4)
        with self.assertRaisesRegex(ValueError, "exactly one"):
            StrategyOutput.for_target_position(
                self.request, signal_id=4, target_qty=1, target_qty_exact="1"
            )

        valid = raw_target.to_dict(self.request)
        invalid_fields = (
            ("schema_version", True, "schema_version must be 1"),
            ("signal_id", True, "signal_id must be an integer"),
            ("target_qty", 1.5, "target_qty must be an integer"),
            ("confidence", False, "confidence must be an integer"),
            ("priority", 1.5, "priority must be an integer"),
            ("expires_at", False, "expires_at must be an integer"),
        )
        for name, value, message in invalid_fields:
            with self.subTest(field=name):
                payload = dict(valid)
                payload[name] = value
                with self.assertRaisesRegex(ValueError, message):
                    StrategyOutput.from_dict(payload, self.request)

    def test_exact_position_projection_and_target_mutual_exclusion_fail_closed(self):
        with self.assertRaisesRegex(ValueError, "raw position projection"):
            StrategyInput(
                request_id="exact-position-mismatch",
                strategy_id="strategy-exact-target",
                strategy_version="v1",
                data_fingerprint="bars-1",
                as_of=10,
                instrument="PEPEUSDT.BINANCE",
                positions={"PEPEUSDT.BINANCE": 1},
                positions_exact={"PEPEUSDT.BINANCE": "0.0000000021"},
                risk_state="verified",
            ).validate()

        output = StrategyOutput(
            request_id=self.request.request_id,
            strategy_id=self.request.strategy_id,
            signal_id=1,
            instrument=self.request.instrument,
            target_qty=1,
            target_qty_exact="0.0000000001",
        )
        with self.assertRaisesRegex(ValueError, "target_qty must be zero"):
            output.validate_for(self.request)

    def test_contract_rejects_future_or_malformed_bars(self):
        value = json.loads(self.request.to_json())
        value["bars"]["ts"] = [10, 9]
        with self.assertRaises(ValueError):
            StrategyInput.from_dict(value)

    def test_market_bars_round_trip_and_reject_future_data(self):
        market = StrategyBars("secondary", (9, 10), (1, 2), (2, 3), (1, 2), (2, 3), (4, 5))
        request = StrategyInput(
            request_id="multi-market-input",
            strategy_id="strategy-1",
            strategy_version="v1",
            data_fingerprint="multi-market-fingerprint",
            as_of=10,
            instrument="BTCUSDT.BINANCE",
            market_bars={"ETHUSDT.OKX": market},
            risk_state="verified",
        )
        self.assertEqual(StrategyInput.from_json(request.to_json()), request)
        payload = json.loads(request.to_json())
        payload["market_bars"]["ETHUSDT.OKX"]["ts"] = [9, 11]
        with self.assertRaisesRegex(ValueError, "after as_of"):
            StrategyInput.from_dict(payload)

    def test_output_supports_multiple_order_intents(self):
        output = StrategyOutput.for_intents(
            self.request,
            signal_id=2,
            intents=(
                StrategyIntent(
                    intent_id=101,
                    instrument="BTCUSDT.BINANCE",
                    account_id="binance-main",
                    side="BUY",
                    qty_raw=3,
                    limit_price_raw=100,
                    post_only=True,
                ),
                StrategyIntent(
                    intent_id=102,
                    instrument="ETHUSDT.BINANCE",
                    side="sell",
                    qty_raw=2,
                    reduce_only=True,
                ),
            ),
        )
        restored = StrategyOutput.from_json(output.to_json(self.request), self.request)
        self.assertEqual(restored, output)
        self.assertEqual(restored.intents[1].side, "sell")
        self.assertEqual(restored.intents[0].side, "buy")
        self.assertEqual(restored.intents[0].account_id, "binance-main")

    def test_account_position_keys_and_multi_venue_contract_pair(self):
        binance = "BTC/USDT:USDT.BINANCE"
        okx = "BTC/USDT:USDT.OKX"
        account_binance = "binance-usdm-sandbox"
        account_okx = "okx-demo-swap"
        keys = {
            account_position_key(account_binance, binance): "0",
            account_position_key(account_okx, okx): "0",
        }
        bars = StrategyBars(
            "cross-venue-example",
            tuple(range(1, 21)),
            tuple(range(1, 21)),
            tuple(range(1, 21)),
            tuple(range(1, 21)),
            tuple([100_000_000_000] * 19 + [101_000_000_000]),
            tuple([1] * 20),
        )
        reference_bars = StrategyBars(
            "cross-venue-example",
            tuple(range(1, 21)),
            tuple([100_000_000_000] * 20),
            tuple([100_000_000_000] * 20),
            tuple([100_000_000_000] * 20),
            tuple([100_000_000_000] * 20),
            tuple([1] * 20),
        )
        request = StrategyInput(
            request_id="multi-venue-1",
            strategy_id="multi-venue-contract",
            strategy_version="v1",
            data_fingerprint="bars-1",
            as_of=20,
            instrument=okx,
            positions_exact=keys,
            risk_state="verified",
            bars=bars,
            market_bars={binance: reference_bars},
        )
        output = strategy_multi_venue_contract.on_event(request)
        output.validate_for(request)
        self.assertEqual([intent.instrument for intent in output.intents], [binance, okx])
        self.assertEqual([intent.side for intent in output.intents], ["buy", "sell"])
        self.assertEqual(
            [intent.account_id for intent in output.intents], [account_binance, account_okx]
        )

        falling = tuple(range(20, 0, -1))
        close_request = StrategyInput(
            request_id="multi-venue-2",
            strategy_id="multi-venue-contract",
            strategy_version="v1",
            data_fingerprint="bars-2",
            as_of=21,
            instrument=okx,
            positions_exact={
                account_position_key(account_binance, binance): "0.001",
                account_position_key(account_okx, okx): "-1",
            },
            risk_state="verified",
            bars=StrategyBars(
                "cross-venue-example", tuple(range(1, 21)), falling, falling, falling, falling, tuple([1] * 20)
            ),
            market_bars={binance: reference_bars},
        )
        close_output = strategy_multi_venue_contract.on_event(close_request)
        close_output.validate_for(close_request)
        self.assertEqual([intent.side for intent in close_output.intents], ["sell", "buy"])
        self.assertTrue(all(intent.reduce_only for intent in close_output.intents))

    def test_multi_venue_strategy_waits_for_both_account_position_snapshots(self):
        request = StrategyInput(
            request_id="multi-venue-no-snapshot",
            strategy_id="multi-venue-contract",
            strategy_version="v1",
            data_fingerprint="bars-1",
            as_of=20,
            instrument="BTC/USDT:USDT.OKX",
            risk_state="verified",
            bars=StrategyBars(
                "cross-venue-example",
                tuple(range(1, 21)),
                tuple(range(1, 21)),
                tuple(range(1, 21)),
                tuple(range(1, 21)),
                tuple(range(1, 21)),
                tuple([1] * 20),
            ),
            market_bars={
                "BTC/USDT:USDT.BINANCE": StrategyBars(
                    "cross-venue-example",
                    tuple(range(1, 21)),
                    tuple([100_000_000_000] * 20),
                    tuple([100_000_000_000] * 20),
                    tuple([100_000_000_000] * 20),
                    tuple([100_000_000_000] * 20),
                    tuple([1] * 20),
                )
            },
        )
        output = strategy_multi_venue_contract.on_event(request)
        output.validate_for(request)
        self.assertEqual(output.intents, ())

    def test_quote_budget_intent_round_trips_as_a_distinct_order_unit(self):
        output = StrategyOutput(
            request_id="request-1",
            strategy_id="strategy-1",
            signal_id=5,
            instrument=self.request.instrument,
            target_qty=0,
            intents=(
                StrategyIntent(
                    intent_id=501,
                    instrument="PEPE/USDT.BITGET",
                    side="buy",
                    qty_raw=0,
                    quote_budget_exact="12.34000001",
                ),
            ),
        )
        payload = json.loads(output.to_json(self.request))
        self.assertEqual(payload["intents"][0]["quote_budget_exact"], "12.34000001")
        restored = StrategyOutput.from_json(json.dumps(payload), self.request)
        self.assertEqual(restored, output)

    def test_quote_budget_intent_rejects_quantity_limit_or_sell_combinations(self):
        for values in (
            {"side": "sell", "qty_raw": 0},
            {"side": "buy", "qty_raw": 1},
            {"side": "buy", "qty_raw": 0, "limit_price_raw": 1},
        ):
            with self.subTest(values=values), self.assertRaises(ValueError):
                StrategyIntent(
                    intent_id=1,
                    instrument="PEPE/USDT.BITGET",
                    quote_budget_exact="1.25",
                    **values,
                ).validate()

    def test_intent_carries_the_three_derivative_fields_rust_emits(self):
        # Rust StrategyContractIntent 对未设置的 margin_mode/position_mode/leverage 写的是
        # JSON null，键始终在。Python 侧少这三个字段时 _reject_unknown_keys 会把 Rust 自己
        # 的产出判成坏载荷，多腿衍生品策略因此无法跨语言往返（V12 §16 第三遍）。
        output = StrategyOutput(
            request_id="request-1",
            strategy_id="strategy-1",
            signal_id=7,
            instrument=self.request.instrument,
            target_qty=0,
            intents=(
                StrategyIntent(
                    intent_id=701,
                    instrument="BTCUSDT.BINANCE",
                    side="BUY",
                    qty_raw=3,
                    position_side="LONG",
                    margin_mode="ISOLATED",
                    position_mode="HEDGE",
                    leverage=10,
                ),
                StrategyIntent(
                    intent_id=702,
                    instrument="ETHUSDT.BINANCE",
                    side="sell",
                    qty_raw=2,
                ),
            ),
        )
        payload = json.loads(output.to_json(self.request))
        for intent in payload["intents"]:
            for key in ("margin_mode", "position_mode", "leverage"):
                self.assertIn(key, intent, "Rust 侧始终写出该键，Python 产出也要写出去")
        self.assertIsNone(payload["intents"][1]["margin_mode"])
        restored = StrategyOutput.from_json(json.dumps(payload), self.request)
        self.assertEqual(restored, output)
        self.assertEqual(
            (
                restored.intents[0].side,
                restored.intents[0].position_side,
                restored.intents[0].margin_mode,
                restored.intents[0].position_mode,
                restored.intents[0].leverage,
            ),
            ("buy", "long", "isolated", "hedge", 10),
        )

    def test_exact_decimal_intent_round_trip_and_rejects_mixed_scales(self):
        output = StrategyOutput(
            request_id="request-1",
            strategy_id="strategy-1",
            signal_id=8,
            instrument=self.request.instrument,
            target_qty=0,
            intents=(
                StrategyIntent(
                    intent_id=801,
                    instrument="PEPEUSDT.BINANCE",
                    side="buy",
                    qty_raw=0,
                    qty_exact="1e-10",
                    limit_price_exact="0.00000005",
                ),
            ),
        )
        payload = json.loads(output.to_json(self.request))
        self.assertEqual(payload["intents"][0]["qty_exact"], "1e-10")
        restored = StrategyOutput.from_json(json.dumps(payload), self.request)
        self.assertEqual(restored, output)

        for kwargs, message in (
            ({"qty_raw": 1, "qty_exact": "1e-10"}, "qty_raw must be 0"),
            ({"qty_raw": 0, "qty_exact": None}, "qty_raw must be positive"),
            (
                {"qty_raw": 3, "limit_price_exact": "0.00000005"},
                "limit_price_exact requires qty_exact",
            ),
            (
                {"qty_raw": 0, "qty_exact": "1e-19"},
                "supported exact decimal range",
            ),
        ):
            with self.subTest(kwargs=kwargs), self.assertRaisesRegex(ValueError, message):
                StrategyIntent(
                    intent_id=802,
                    instrument="PEPEUSDT.BINANCE",
                    side="buy",
                    **kwargs,
                ).validate()

    def test_exact_decimal_strategy_example_emits_a_valid_versioned_intent(self):
        output = strategy_exact_amount.on_event(self.request)
        output.validate_for(self.request)
        self.assertEqual(output.intents[0].qty_exact, "0.0000000001")
        self.assertEqual(output.intents[0].limit_price_exact, "0.00000005")

    def test_exact_target_example_uses_the_identity_bound_constructor(self):
        output = strategy_exact_target.on_event(self.request)
        output.validate_for(self.request)
        self.assertEqual(output.target_qty_exact, "0.0000000001")
        self.assertEqual(output.request_id, self.request.request_id)

    def test_intent_rejects_derivative_fields_rust_would_reject(self):
        # 与 Rust StrategyContractIntent::validate 同口径：档位取值封闭、leverage 不得为 0。
        for kwargs, message in (
            ({"margin_mode": "portfolio"}, r"margin_mode must be cash, cross or isolated"),
            ({"position_mode": "both"}, r"position_mode must be one_way or hedge"),
            ({"leverage": 0}, r"leverage must be positive"),
        ):
            with self.subTest(**kwargs), self.assertRaisesRegex(ValueError, message):
                StrategyIntent(
                    intent_id=703,
                    instrument="BTCUSDT.BINANCE",
                    side="buy",
                    qty_raw=3,
                    **kwargs,
                ).validate()

    def test_intent_wire_parser_rejects_boolean_and_lossy_numeric_coercion(self):
        intent = StrategyIntent(
            intent_id=1,
            instrument="BTCUSDT.BINANCE",
            side="buy",
            qty_raw=3,
        )
        base = intent.to_dict()
        invalid_fields = (
            ("intent_id", True, "intent.intent_id must be an integer"),
            ("qty_raw", 3.5, "intent.qty_raw must be an integer"),
            ("leverage", True, "intent.leverage must be an integer"),
            ("post_only", "false", "reduce_only and post_only must be booleans"),
            ("reduce_only", 1, "reduce_only and post_only must be booleans"),
            ("instrument", 123, "intent.instrument must be a string"),
        )
        for name, value, message in invalid_fields:
            with self.subTest(field=name):
                payload = dict(base)
                payload[name] = value
                with self.assertRaisesRegex(ValueError, message):
                    StrategyIntent.from_dict(payload)

        with self.assertRaisesRegex(ValueError, "missing required key"):
            StrategyIntent.from_dict({"intent_id": 1, "instrument": "BTCUSDT.BINANCE"})

    def test_output_rejects_unknown_keys_rather_than_dropping_them(self):
        # 与 Rust `StrategyContractOutput/Intent` 的 deny_unknown_fields 同一口径：拼错的
        # 可选键静默丢掉，等于那一腿按运行时默认档位成交，而作者以为开了 post-only。
        output = StrategyOutput(
            request_id="request-1",
            strategy_id="strategy-1",
            signal_id=3,
            instrument=self.request.instrument,
            target_qty=1,
            intents=(
                StrategyIntent(
                    intent_id=201,
                    instrument="BTCUSDT.BINANCE",
                    side="buy",
                    qty_raw=3,
                    post_only=True,
                ),
            ),
        )
        payload = json.loads(output.to_json(self.request))
        payload["post_onli"] = True
        with self.assertRaisesRegex(ValueError, r"strategy output contains unknown key\(s\).*post_onli"):
            StrategyOutput.from_dict(payload, self.request)
        del payload["post_onli"]
        payload["intents"][0]["post_onli"] = True
        with self.assertRaisesRegex(ValueError, r"strategy intent contains unknown key\(s\).*post_onli"):
            StrategyOutput.from_dict(payload, self.request)

    def test_output_wire_parser_requires_schema_fields_and_array_intents(self):
        output = StrategyOutput.for_target_position(
            self.request, signal_id=1, target_qty=3
        )
        payload = output.to_dict(self.request)
        missing = dict(payload)
        del missing["confidence"]
        with self.assertRaisesRegex(ValueError, "missing required key.*confidence"):
            StrategyOutput.from_dict(missing, self.request)
        invalid_intents = dict(payload)
        invalid_intents["intents"] = {}
        with self.assertRaisesRegex(ValueError, "intents must be an array"):
            StrategyOutput.from_dict(invalid_intents, self.request)

    def test_framed_transport_round_trip_and_crc_guard(self):
        payload = b'{"request_id":"frame-1"}'
        encoded = encode_frame(REQUEST, 7, payload)
        self.assertEqual(read_frame(io.BytesIO(encoded)), (REQUEST, 7, payload))
        response = encode_frame(RESPONSE, 7, b'{"ok":true}')
        self.assertEqual(read_frame(io.BytesIO(response))[:2], (RESPONSE, 7))
        corrupted = bytearray(encoded)
        corrupted[-1] ^= 1
        with self.assertRaisesRegex(ValueError, "CRC32"):
            read_frame(io.BytesIO(corrupted))

    def test_shared_memory_ring_round_trip_and_backpressure(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "ring.bin"
            with SharedMemoryRing.create(path, 2, 128) as writer:
                with SharedMemoryRing(path, 2, 128) as reader:
                    with self.assertRaises(RingEmpty):
                        reader.try_pop()
                    writer.try_push(b"one")
                    writer.try_push(b"two")
                    with self.assertRaises(RingFull):
                        writer.try_push(b"three")
                    self.assertEqual(reader.try_pop(), b"one")
                    writer.try_push(b"three")
                    self.assertEqual(reader.try_pop(), b"two")
                    self.assertEqual(reader.try_pop(), b"three")

    def test_columnar_request_restores_bar_columns(self):
        metadata = json.loads(self.request.to_json())
        bars = metadata.pop("bars")
        metadata["bars"] = None
        metadata["__qx_bars_source"] = bars["source"]
        metadata_bytes = json.dumps(metadata, separators=(",", ":")).encode()
        payload = bytearray(struct.pack("<4sHHII", b"QXCB", 1, 0, len(metadata_bytes), len(bars["ts"])))
        payload.extend(metadata_bytes)
        payload.extend(b"".join(struct.pack("<Q", value) for value in bars["ts"]))
        for key in ("open_raw", "high_raw", "low_raw", "close_raw", "volume_raw"):
            payload.extend(b"".join(int(value).to_bytes(16, "little", signed=True) for value in bars[key]))
        restored = decode_request(bytes(payload))
        self.assertEqual(restored, self.request)

    def test_columnar_v2_restores_exact_ohlcv_sidecar(self):
        exact_row = {
            "open": {"coefficient": "5", "scale": 10},
            "high": {"coefficient": "5", "scale": 10},
            "low": {"coefficient": "5", "scale": 10},
            "close": {"coefficient": "5", "scale": 10},
            "volume": {"coefficient": "5", "scale": 13},
        }
        request = StrategyInput(
            schema_version=5,
            request_id="columnar-exact",
            strategy_id="strategy-v5",
            strategy_version="1.0",
            data_fingerprint="subnano-bars",
            as_of=10,
            instrument="ALTUSDT.BINANCE",
            risk_state="verified",
            bars=StrategyBars("subnano", (9,), (0,), (0,), (0,), (0,), (0,), (exact_row,)),
        )
        metadata = request.to_dict()
        bars = metadata.pop("bars")
        metadata["bars"] = None
        metadata["__qx_bars_source"] = bars["source"]
        metadata["__qx_bars_exact_ohlcv"] = bars["exact_ohlcv"]
        metadata_bytes = json.dumps(metadata, separators=(",", ":")).encode()
        payload = bytearray(
            struct.pack("<4sHHII", b"QXCB", 2, 1, len(metadata_bytes), len(bars["ts"]))
        )
        payload.extend(metadata_bytes)
        payload.extend(b"".join(struct.pack("<Q", value) for value in bars["ts"]))
        for key in ("open_raw", "high_raw", "low_raw", "close_raw", "volume_raw"):
            payload.extend(
                b"".join(int(value).to_bytes(16, "little", signed=True) for value in bars[key])
            )
        self.assertEqual(decode_request(bytes(payload)), request)

    @staticmethod
    def _spawned_dead_pid() -> int:
        process = subprocess.Popen([sys.executable, "-c", "import sys; sys.exit(0)"])
        process.wait()
        return process.pid

    _WORKER_STRATEGY_SOURCE = (
        "from qianxing_bridge.strategy import StrategyOutput\n"
        "\n"
        "def on_event(request):\n"
        "    return StrategyOutput(\n"
        "        request_id=request.request_id,\n"
        "        strategy_id=request.strategy_id,\n"
        "        signal_id=1,\n"
        "        instrument=request.instrument,\n"
        "        target_qty=1,\n"
        "    )\n"
    )

    def _start_shared_worker(self, directory: str, parent_pid: int) -> subprocess.Popen:
        module = Path(directory) / "strategy.py"
        module.write_text(self._WORKER_STRATEGY_SOURCE, encoding="utf-8")
        worker = subprocess.Popen(
            [
                sys.executable,
                "-m",
                "qianxing_strategy.worker",
                "--module",
                str(module),
                "--protocol",
                "shared_memory_json",
                "--input-ring",
                str(Path(directory) / "input.bin"),
                "--output-ring",
                str(Path(directory) / "output.bin"),
                "--ring-capacity",
                "8",
                "--ring-slot-bytes",
                "4096",
                "--parent-pid",
                str(parent_pid),
            ],
            cwd=str(Path(__file__).resolve().parents[1]),
        )
        return worker

    def test_shared_worker_exits_when_the_parent_process_is_gone(self):
        """#215：ring 传输没有 stdin 可关，父进程被强杀后这条循环只能自己认出口。"""
        with tempfile.TemporaryDirectory() as directory:
            for name in ("input.bin", "output.bin"):
                SharedMemoryRing.create(Path(directory) / name, 8, 4096).close()
            worker = self._start_shared_worker(directory, self._spawned_dead_pid())
            try:
                self.assertEqual(
                    worker.wait(timeout=PARENT_LIVENESS_PROBE_SECONDS * 30),
                    0,
                    "父进程早就不在了，worker 仍在 ring 上以 1 kHz 空转",
                )
            finally:
                if worker.poll() is None:
                    worker.kill()
                    worker.wait()

    def test_shared_worker_exits_after_its_real_parent_is_force_killed(self):
        """ring worker must notice OS parent death, not rely on Rust Drop/stdin EOF."""
        with tempfile.TemporaryDirectory() as directory:
            for name in ("input.bin", "output.bin"):
                SharedMemoryRing.create(Path(directory) / name, 8, 4096).close()
            module = Path(directory) / "strategy.py"
            module.write_text(self._WORKER_STRATEGY_SOURCE, encoding="utf-8")
            harness = (
                "import os, subprocess, sys, time; "
                "args=sys.argv[1:]; args[-1]=str(os.getpid()); "
                "child=subprocess.Popen(args, stdin=subprocess.DEVNULL, "
                "stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL); "
                "print(child.pid, flush=True); time.sleep(30)"
            )
            command = [
                sys.executable,
                "-m",
                "qianxing_strategy.worker",
                "--module",
                str(module),
                "--protocol",
                "shared_memory_json",
                "--input-ring",
                str(Path(directory) / "input.bin"),
                "--output-ring",
                str(Path(directory) / "output.bin"),
                "--ring-capacity",
                "8",
                "--ring-slot-bytes",
                "4096",
                "--parent-pid",
                "0",
            ]
            parent = subprocess.Popen(
                [sys.executable, "-c", harness, *command],
                cwd=str(Path(__file__).resolve().parents[1]),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            worker_pid = None
            try:
                line = parent.stdout.readline()
                self.assertTrue(line.strip(), "parent harness did not publish its worker pid")
                worker_pid = int(line.strip())
                parent.kill()
                parent.wait(timeout=5)

                deadline = time.monotonic() + PARENT_LIVENESS_PROBE_SECONDS * 5
                while _parent_process_alive(worker_pid) and time.monotonic() < deadline:
                    time.sleep(0.05)
                self.assertFalse(
                    _parent_process_alive(worker_pid),
                    "shared-memory worker survived after its real parent was force-killed",
                )
            finally:
                if parent.poll() is None:
                    parent.kill()
                    parent.wait(timeout=5)
                if worker_pid is not None and _parent_process_alive(worker_pid):
                    try:
                        os.kill(worker_pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                    deadline = time.monotonic() + 5
                    while _parent_process_alive(worker_pid) and time.monotonic() < deadline:
                        time.sleep(0.05)
                if parent.stdout is not None:
                    parent.stdout.close()
                if parent.stderr is not None:
                    parent.stderr.close()

    def test_shared_worker_keeps_serving_while_the_parent_is_alive(self):
        """同一条闸门不能把健康的 worker 一起误杀：父进程在时请求必须答得回来。"""
        with tempfile.TemporaryDirectory() as directory:
            input_path = Path(directory) / "input.bin"
            output_path = Path(directory) / "output.bin"
            SharedMemoryRing.create(input_path, 8, 4096).close()
            SharedMemoryRing.create(output_path, 8, 4096).close()
            worker = self._start_shared_worker(directory, os.getpid())
            try:
                deadline = time.monotonic() + 20.0
                with SharedMemoryRing(input_path, 8, 4096) as client_input, SharedMemoryRing(
                    output_path, 8, 4096
                ) as client_output:
                    client_input.try_push(
                        encode_frame(REQUEST, 11, self.request.to_json().encode("utf-8"))
                    )
                    payload = None
                    while payload is None:
                        try:
                            payload = client_output.try_pop()
                        except RingEmpty:
                            if time.monotonic() >= deadline:
                                self.fail("父进程还活着时 worker 没有回话")
                    kind, sequence, response = read_frame(io.BytesIO(payload))
                    self.assertEqual((kind, sequence), (RESPONSE, 11))
                    self.assertTrue(json.loads(response)["ok"])
            finally:
                worker.kill()
                worker.wait()

    def test_worker_verifies_file_backed_strategy_artifact(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "strategy.py"
            path.write_text(
                "def on_event(request):\n    return {'target_qty': 1}\n",
                encoding="utf-8",
            )
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            with patch.dict(os.environ, {"QX_STRATEGY_ARTIFACT_SHA256": digest}):
                self.assertTrue(callable(_load_handler(str(path))))
                path.write_text("def on_event(request):\n    return {'target_qty': 2}\n", encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "SHA-256 mismatch"):
                    _load_handler(str(path))


if __name__ == "__main__":
    unittest.main()
