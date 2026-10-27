import asyncio
import json
import os
import subprocess
import sys
import tempfile
import unittest
from io import StringIO
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing_ccxt import (
    LIVE_MARKET_METADATA_MAX_AGE_SECONDS,
    SUPPORTED_CCXT_VERSION,
    CcxtConfig,
    CcxtConnectorError,
    CcxtErrorClass,
    CcxtExchangeClient,
)
from qianxing_ccxt.worker import CcxtJsonWorker, _config_from_file, serve, serve_startup_error
from test_ccxt import FakeExchange, FakeProExchange


class CcxtWorkerTest(unittest.TestCase):
    def test_jsonl_balance_response_carries_scale_aware_decimal_sidecar(self):
        class BalanceExchange(FakeExchange):
            def fetch_balance(self, params):
                self.calls.append(("fetch_balance", params))
                return {
                    "free": {"PEPE": 0.000000000123},
                    "used": {"PEPE": 0.0},
                    "total": {"PEPE": 0.000000000123},
                    "info": [],
                }

        exchange = BalanceExchange()
        worker = CcxtJsonWorker(
            CcxtExchangeClient(CcxtConfig(exchange_id="binance"), exchange=exchange)
        )
        row = json.loads(worker.handle_line(json.dumps({"op": "fetch_balance"})))

        self.assertTrue(row["ok"], row)
        self.assertEqual(row["result"]["exact"]["free"]["PEPE"], "0.000000000123")
        self.assertEqual(row["result"]["exact"]["used"]["PEPE"], "0")
        self.assertFalse(any(call[0] == "create_order" for call in exchange.calls))

    def test_live_market_spec_check_accepts_match_and_reports_drift_without_write(self):
        exchange = FakeExchange()
        client = CcxtExchangeClient(CcxtConfig(exchange_id="binance"), exchange=exchange)
        market = client.resolve_market("BTC/USDT:USDT.BINANCE")
        expected_spec = {
            "product": "perpetual",
            "base_currency": "BTC",
            "quote_currency": "USDT",
            "settlement_currency": "USDT",
            "contract_size_raw": str(market.contract_size_raw),
            "linear": True,
            "inverse": False,
            "price_tick_raw": str(market.price_tick_raw),
            "qty_step_raw": str(market.qty_step_raw),
            "min_qty_raw": str(market.min_qty_raw),
            "min_notional_raw": "0",
            "max_leverage": 50,
            "maintenance_margin_bps": 50,
            # Rust serializes ExactDecimal as normalized coefficient + scale.
            "exact_rules": {
                "contract_size": {"coefficient": "1", "scale": 3},
                "price_tick": {"coefficient": "1", "scale": 1},
                "quantity_step": {"coefficient": "1", "scale": 0},
                "minimum_quantity": {"coefficient": "1", "scale": 0},
                "minimum_notional": {"coefficient": "0", "scale": 0},
            },
        }
        worker = CcxtJsonWorker(client)

        accepted = json.loads(
            worker.handle_line(
                json.dumps(
                    {
                        "op": "validate_market_spec",
                        "instrument": "BTC/USDT:USDT.BINANCE",
                        "expected_spec": expected_spec,
                    }
                )
            )
        )
        self.assertTrue(accepted["ok"], accepted)
        self.assertTrue(accepted["result"]["market_spec_verified"])

        refreshed_markets = {
            symbol: dict(row) for symbol, row in exchange.markets.items()
        }
        refreshed_markets["BTC/USDT:USDT"]["precision"] = dict(
            refreshed_markets["BTC/USDT:USDT"]["precision"]
        )
        refreshed_markets["BTC/USDT:USDT"]["precision"]["amount"] = "2"
        exchange.markets = refreshed_markets
        client._markets_loaded_at = 0.0
        with patch(
            "qianxing_ccxt.time.monotonic",
            return_value=float(LIVE_MARKET_METADATA_MAX_AGE_SECONDS + 1),
        ):
            rejected = json.loads(
                worker.handle_line(
                    json.dumps(
                        {
                            "op": "validate_market_spec",
                            "instrument": "BTC/USDT:USDT.BINANCE",
                            "expected_spec": expected_spec,
                        }
                    )
                )
            )
        self.assertTrue(rejected["ok"], rejected)
        self.assertEqual(
            rejected["result"]["market_spec_rejected"]["class"], "invalid"
        )
        self.assertIn("qty_step_raw", rejected["result"]["market_spec_rejected"]["message"])
        self.assertTrue(
            any(call[0] == "load_markets" and call[1] is True for call in exchange.calls)
        )
        self.assertFalse(any(call[0] == "create_order" for call in exchange.calls))

    def test_jsonl_worker_requires_risk_leverage_for_contract_order(self):
        exchange = FakeExchange()
        worker = CcxtJsonWorker(
            CcxtExchangeClient(CcxtConfig(exchange_id="binance"), exchange=exchange)
        )
        row = json.loads(
            worker.handle_line(
                json.dumps(
                    {
                        "op": "create_order",
                        "instrument": "BTC/USDT:USDT.BINANCE",
                        "side": "buy",
                        "order_type": "limit",
                        "amount_raw": 1_000_000_000,
                        "amount_unit": "contracts",
                        "price_raw": 100_000_000_000,
                    }
                )
            )
        )
        self.assertFalse(row["ok"])
        self.assertIn("risk_leverage", row["error"]["message"])
        self.assertFalse(any(call[0] == "create_order" for call in exchange.calls))

    def test_binance_spot_max_position_counts_cross_pair_orders_before_submit(self):
        class MaxPositionExchange(FakeExchange):
            def __init__(self):
                super().__init__()
                self.markets["PEPE/USDT"]["info"]["filters"] = [
                    {"filterType": "MAX_POSITION", "maxPosition": "0.000000006"}
                ]
                self.markets["PEPE/BTC"] = {
                    "symbol": "PEPE/BTC",
                    "id": "PEPEBTC",
                    "base": "PEPE",
                    "quote": "BTC",
                    "type": "spot",
                    "active": True,
                    "info": {"filters": []},
                    "precision": {"price": "0.00000001", "amount": "0.000000001"},
                    "limits": {"amount": {"min": "0.000000001"}},
                }
                self.open_order_rows = [
                    {
                        "id": "cross-pair-order",
                        "symbol": "PEPE/BTC",
                        "side": "buy",
                        "info": {
                            "symbol": "PEPEBTC",
                            "orderId": "42",
                            "side": "BUY",
                            "origQty": "0.000000004",
                            "executedQty": "0.000000001",
                        },
                    }
                ]

            def fetch_balance(self, params):
                self.calls.append(("fetch_balance", params))
                return {
                    "info": {
                        "balances": [
                            {"asset": "PEPE", "free": "0.000000001", "locked": "0.000000001"}
                        ]
                    }
                }

        exchange = MaxPositionExchange()
        worker = CcxtJsonWorker(
            CcxtExchangeClient(CcxtConfig(exchange_id="binance"), exchange=exchange)
        )
        row = json.loads(
            worker.handle_line(
                json.dumps(
                    {
                        "op": "create_order",
                        "instrument": "PEPE/USDT.BINANCE",
                        "side": "buy",
                        "order_type": "market",
                        "amount_exact": "0.000000002",
                        "amount_unit": "base",
                    }
                )
            )
        )

        self.assertTrue(row["ok"], row)
        self.assertEqual(row["result"]["preflight_rejected"]["class"], "invalid")
        self.assertIn("MAX_POSITION 超限", row["result"]["preflight_rejected"]["message"])
        operation_names = [call[0] for call in exchange.calls]
        self.assertLess(operation_names.index("fetch_open_orders"), operation_names.index("fetch_balance"))
        self.assertNotIn("create_order", operation_names)

        with self.assertRaisesRegex(CcxtConnectorError, "MAX_POSITION 超限"):
            worker.client.create_order(
                "PEPE/USDT.BINANCE",
                side="buy",
                order_type="market",
                amount_exact="0.000000002",
                amount_unit="base",
            )
        self.assertNotIn("create_order", [call[0] for call in exchange.calls])

        malformed_open_order = dict(exchange.open_order_rows[0])
        malformed_open_order.pop("side")
        malformed_info = dict(malformed_open_order["info"])
        malformed_info.pop("side")
        malformed_open_order["info"] = malformed_info
        exchange.open_order_rows = [malformed_open_order]
        with self.assertRaisesRegex(CcxtConnectorError, "缺少可识别的 side"):
            worker.client.create_order(
                "PEPE/USDT.BINANCE",
                side="buy",
                order_type="market",
                amount_exact="0.000000002",
                amount_unit="base",
            )
        self.assertNotIn("create_order", [call[0] for call in exchange.calls])

    def test_jsonl_tier_rejection_is_reported_as_preflight_without_exchange_write(self):
        exchange = FakeExchange()
        exchange.leverage_tier_rows = [
            {
                "tier": 1,
                "minNotional": "0",
                "maxNotional": "0.3",
                "maxLeverage": 20,
            },
            {
                "tier": 2,
                "minNotional": "0.3",
                "maxNotional": "1000",
                "maxLeverage": 5,
            },
        ]
        worker = CcxtJsonWorker(
            CcxtExchangeClient(CcxtConfig(exchange_id="binance"), exchange=exchange)
        )
        row = json.loads(
            worker.handle_line(
                json.dumps(
                    {
                        "op": "create_order",
                        "instrument": "BTC/USDT:USDT.BINANCE",
                        "side": "buy",
                        "order_type": "limit",
                        "amount_raw": 2_000_000_000,
                        "amount_unit": "contracts",
                        "price_raw": 100_000_000_000,
                        "risk_leverage": 10,
                        "risk_reduce_only": False,
                        "risk_margin_mode": "cross",
                    }
                )
            )
        )
        self.assertTrue(row["ok"], row)
        self.assertEqual(
            row["result"]["preflight_rejected"]["class"], "invalid"
        )
        self.assertFalse(any(call[0] == "create_order" for call in exchange.calls))

    def test_jsonl_bitget_hedge_preflight_uses_directional_position_sizes(self):
        class HedgedBitgetExchange(FakeExchange):
            def fetch_positions(self, symbols, params):
                self.calls.append(("fetch_positions", symbols, params))
                return [
                    {
                        "symbol": "BTC/USDT:USDT",
                        "side": "long",
                        "contracts": "10",
                        "contractSize": "0.001",
                        "markPrice": "100",
                        "hedged": True,
                    },
                    {
                        "symbol": "BTC/USDT:USDT",
                        "side": "short",
                        "contracts": "10",
                        "contractSize": "0.001",
                        "markPrice": "100",
                        "hedged": True,
                    },
                ]

        exchange = HedgedBitgetExchange()
        exchange.leverage_tier_rows = [
            {"tier": 1, "minNotional": "0", "maxNotional": "2", "maxLeverage": 20},
            {"tier": 2, "minNotional": "2", "maxNotional": "1000", "maxLeverage": 5},
        ]
        exchange.open_order_rows = [
            {
                "id": "long-open",
                "symbol": "BTC/USDT:USDT",
                "status": "open",
                "side": "buy",
                "type": "limit",
                "amount": "3",
                "filled": "0",
                "remaining": "3",
                "price": "100",
                "info": {"tradeSide": "open", "posSide": "long"},
            },
            {
                "id": "short-open",
                "symbol": "BTC/USDT:USDT",
                "status": "open",
                "side": "sell",
                "type": "limit",
                "amount": "2",
                "filled": "0",
                "remaining": "2",
                "price": "100",
                "info": {"tradeSide": "open", "posSide": "short"},
            },
        ]
        worker = CcxtJsonWorker(
            CcxtExchangeClient(CcxtConfig(exchange_id="bitget"), exchange=exchange)
        )

        def submit(params):
            return json.loads(
                worker.handle_line(
                    json.dumps(
                        {
                            "op": "create_order",
                            "instrument": "BTC/USDT:USDT.BITGET",
                            "side": "buy",
                            "order_type": "limit",
                            "amount_raw": 5 * 1_000_000_000,
                            "amount_unit": "contracts",
                            "price_raw": 100 * 1_000_000_000,
                            "risk_leverage": 10,
                            "risk_reduce_only": False,
                            "risk_margin_mode": "cross",
                            "params": params,
                        }
                    )
                )
            )

        hedged = submit({"hedged": True})
        self.assertTrue(hedged["ok"], hedged)
        self.assertEqual(hedged["result"]["order"]["order_id"], "order-1")

        exchange.calls.clear()
        one_way = submit({})
        self.assertTrue(one_way["ok"], one_way)
        self.assertEqual(one_way["result"]["preflight_rejected"]["class"], "invalid")
        self.assertFalse(any(call[0] == "create_order" for call in exchange.calls))

    def test_startup_failures_keep_jsonl_response_contract_for_each_request(self):
        output_stream = StringIO()
        serve_startup_error(
            CcxtErrorClass.UNSUPPORTED,
            "未安装公共 ccxt；请安装 qianxing[ccxt] 或 ccxt 包",
            StringIO('{"op":"fetch_ohlcv"}\n\n{"op":"fetch_ticker"}\n'),
            output_stream,
        )
        rows = [json.loads(line) for line in output_stream.getvalue().splitlines()]
        self.assertEqual(len(rows), 2)
        self.assertTrue(all(not row["ok"] for row in rows))
        self.assertTrue(all(row["error"]["class"] == "unsupported" for row in rows))
        self.assertTrue(all("qianxing[ccxt]" in row["error"]["message"] for row in rows))

    def test_jsonl_protocol_stays_utf8_under_legacy_windows_console_encoding(self):
        with tempfile.TemporaryDirectory() as directory:
            config_path = Path(directory) / "public.json"
            config_path.write_text(
                json.dumps({"exchange_id": "binance", "sandbox": True, "credential_env": None}),
                encoding="utf-8",
            )
            ccxt_package = Path(directory) / "ccxt"
            ccxt_package.mkdir()
            (ccxt_package / "__init__.py").write_text(
                f"__version__ = {SUPPORTED_CCXT_VERSION!r}\n"
                "class binance:\n"
                "    def __init__(self, params): pass\n"
                "    def set_sandbox_mode(self, enabled): pass\n",
                encoding="utf-8",
            )
            env = os.environ.copy()
            env["PYTHONIOENCODING"] = "cp1252"
            python_root = str(Path(__file__).resolve().parents[1])
            env["PYTHONPATH"] = (
                str(Path(directory))
                + os.pathsep
                + python_root
                + os.pathsep
                + env.get("PYTHONPATH", "")
            )
            result = subprocess.run(
                [sys.executable, "-m", "qianxing_ccxt.worker", "--config", str(config_path)],
                input=b'{"op":"not-supported"}\n',
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                env=env,
                check=False,
                timeout=10,
            )
        self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", errors="replace"))
        response = json.loads(result.stdout.decode("utf-8"))
        self.assertFalse(response["ok"])
        self.assertIn("不支持的 CCXT Worker 操作", response["error"]["message"])

    def test_config_file_allows_public_exchange_without_credentials(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "public.json"
            path.write_text(
                json.dumps(
                    {
                        "exchange_id": "binance",
                        "account_model": "spot",
                        "sandbox": True,
                        "credential_env": None,
                    }
                ),
                encoding="utf-8",
            )
            config = _config_from_file(path)
            self.assertEqual(config.exchange_id, "binance")
            self.assertEqual(config.account_model, "spot")
            self.assertEqual(config.api_key, "")
            self.assertEqual(config.secret, "")

    def test_jsonl_worker_keeps_success_and_error_envelopes_stable(self):
        worker = CcxtJsonWorker(
            CcxtExchangeClient(CcxtConfig(exchange_id="binance"), exchange=FakeExchange())
        )
        input_stream = StringIO(
            json.dumps(
                {
                    "op": "resolve_market",
                    "instrument": "BTCUSDT.BINANCE",
                }
            )
            + "\n"
            + json.dumps(
                {
                    "op": "fetch_ohlcv",
                    "instrument": "BTCUSDT.BINANCE",
                    "start_ms": 1000,
                    "end_ms": 1200,
                    "limit": 2,
                }
            )
            + "\n"
            + json.dumps({"op": "not-supported"})
            + "\n"
        )
        output_stream = StringIO()
        serve(worker, input_stream, output_stream)
        rows = [json.loads(line) for line in output_stream.getvalue().splitlines()]
        self.assertTrue(rows[0]["ok"])
        self.assertEqual(rows[0]["result"]["market"]["symbol"], "BTC/USDT")
        self.assertTrue(rows[1]["ok"])
        self.assertEqual(rows[1]["result"]["frame"]["ts"], [1000, 1100, 1200])
        self.assertFalse(rows[2]["ok"])
        self.assertEqual(rows[2]["error"]["class"], "unsupported")

    def test_jsonl_create_order_requires_amount_unit_and_rejects_spot_contracts(self):
        exchange = FakeExchange()
        worker = CcxtJsonWorker(
            CcxtExchangeClient(CcxtConfig(exchange_id="binance"), exchange=exchange)
        )
        request = {
            "op": "create_order",
            "instrument": "BTCUSDT.BINANCE",
            "side": "buy",
            "order_type": "limit",
            "amount_raw": 1_000_000_000,
            "price_raw": 100_000_000_000,
        }
        missing = json.loads(worker.handle_line(json.dumps(request)))
        self.assertFalse(missing["ok"])
        self.assertEqual(missing["error"]["class"], "invalid")
        self.assertFalse(any(call[0] == "create_order" for call in exchange.calls))

        request["amount_unit"] = "contracts"
        mismatched = json.loads(worker.handle_line(json.dumps(request)))
        self.assertFalse(mismatched["ok"])
        self.assertEqual(mismatched["error"]["class"], "invalid")
        self.assertFalse(any(call[0] == "create_order" for call in exchange.calls))

        request["amount_unit"] = "base"
        accepted = json.loads(worker.handle_line(json.dumps(request)))
        self.assertTrue(accepted["ok"], accepted)
        self.assertEqual(sum(1 for call in exchange.calls if call[0] == "create_order"), 1)

    def test_jsonl_exact_spot_amounts_round_trip_as_decimal_text(self):
        exchange = FakeExchange()
        market = exchange.markets["PEPE/USDT"]
        market["precision"] = {"price": "0.0000000001", "amount": "0.0000000001"}
        market["limits"] = {
            "price": {"min": "0.0000000001"},
            "amount": {"min": "0.0000000001"},
        }
        worker = CcxtJsonWorker(
            CcxtExchangeClient(CcxtConfig(exchange_id="binance"), exchange=exchange)
        )
        response = json.loads(
            worker.handle_line(
                json.dumps(
                    {
                        "op": "create_order",
                        "instrument": "PEPE/USDT.BINANCE",
                        "side": "buy",
                        "order_type": "limit",
                        "amount_unit": "base",
                        "amount_raw": None,
                        "amount_exact": {"coefficient": "2", "scale": 10},
                        "price_raw": None,
                        "price_exact": {"coefficient": "1", "scale": 0},
                    }
                )
            )
        )
        self.assertTrue(response["ok"], response)
        order = response["result"]["order"]
        self.assertEqual(order["amount_exact"], "0.0000000002")
        self.assertEqual(order["price_exact"], "1")
        self.assertEqual(order["amount_raw"], 0)
        submitted = next(call for call in exchange.calls if call[0] == "create_order")
        self.assertEqual(submitted[4], "0.0000000002")
        self.assertEqual(submitted[5], "1")

    def test_jsonl_bitget_uta_quote_budget_preserves_tiny_quote_amount(self):
        exchange = FakeExchange()
        worker = CcxtJsonWorker(
            CcxtExchangeClient(
                CcxtConfig(exchange_id="bitget", options={"uta": True}),
                exchange=exchange,
            )
        )
        response = json.loads(
            worker.handle_line(
                json.dumps(
                    {
                        "op": "create_order",
                        "instrument": "PEPE/USDT.BITGET",
                        "side": "buy",
                        "order_type": "market",
                        "amount_unit": "quote",
                        "amount_exact": "0.00000001",
                        "price_raw": None,
                        "price_exact": None,
                        "client_order_id": "qx-uta-budget-worker",
                    }
                )
            )
        )
        self.assertTrue(response["ok"], response)
        self.assertEqual(response["result"]["order"]["amount_exact"], "0.00000001")
        submitted = next(
            call for call in exchange.calls if call[0] == "private_uta_post_v3_trade_place_order"
        )[1]
        self.assertEqual(submitted["qty"], "0.00000001")
        self.assertEqual(submitted["category"], "SPOT")

    def test_jsonl_exact_derivative_order_preflights_and_preserves_decimal_text(self):
        exchange = FakeExchange()
        market = exchange.markets["BTC/USDT:USDT"]
        market["precision"] = {"price": "0.1", "amount": "0.0000000001"}
        market["limits"]["amount"] = {"min": "0.0000000001"}
        worker = CcxtJsonWorker(
            CcxtExchangeClient(CcxtConfig(exchange_id="binance"), exchange=exchange)
        )
        response = json.loads(
            worker.handle_line(
                json.dumps(
                    {
                        "op": "create_order",
                        "instrument": "BTC/USDT:USDT.BINANCE",
                        "side": "buy",
                        "order_type": "limit",
                        "amount_unit": "contracts",
                        "amount_exact": {"coefficient": "1", "scale": 10},
                        "price_exact": {"coefficient": "100", "scale": 0},
                        "risk_leverage": 2,
                        "risk_margin_mode": "cross",
                    }
                )
            )
        )
        self.assertTrue(response["ok"], response)
        self.assertEqual(response["result"]["order"]["amount_exact"], "0.0000000001")
        submitted = next(call for call in exchange.calls if call[0] == "create_order")
        self.assertEqual(submitted[4], "0.0000000001")
        self.assertTrue(any(call[0] == "fetch_leverage_tiers" for call in exchange.calls))
        self.assertTrue(any(call[0] == "fetch_positions" for call in exchange.calls))

    def test_jsonl_worker_exposes_public_ccxt_pro_stream_events(self):
        worker = CcxtJsonWorker(
            CcxtExchangeClient(CcxtConfig(exchange_id="binance"), exchange=FakeExchange()),
            pro_exchange=FakeProExchange(),
        )
        input_stream = StringIO(
            json.dumps(
                {
                    "op": "watch_orders",
                    "instrument": "BTCUSDT.BINANCE",
                    "received_ts": 7,
                }
            )
            + "\n"
        )
        output_stream = StringIO()
        serve(worker, input_stream, output_stream)
        row = json.loads(output_stream.getvalue())
        self.assertTrue(row["ok"])
        event = row["result"]["event"]
        self.assertEqual(event["stream"], "orders")
        self.assertEqual(event["received_ts"], 7)
        self.assertEqual(event["events"][0]["order_id"], "remote-1")

    def test_watch_with_wait_ms_replies_idle_instead_of_letting_the_read_window_expire(self):
        class SilentPro(FakeProExchange):
            async def watch_orders(self, symbol, since, limit, params):
                await asyncio.sleep(30)
                return []

        worker = CcxtJsonWorker(
            CcxtExchangeClient(CcxtConfig(exchange_id="binance"), exchange=FakeExchange()),
            pro_exchange=SilentPro(),
        )
        row = json.loads(
            worker.handle_line(
                json.dumps(
                    {
                        "op": "watch_orders",
                        "instrument": "BTCUSDT.BINANCE",
                        "received_ts": 11,
                        "wait_ms": 5,
                    }
                )
            )
        )
        self.assertTrue(row["ok"], row)
        event = row["result"]["event"]
        self.assertEqual(event["stream"], "orders")
        self.assertEqual(event["events"], [])
        self.assertTrue(event.get("idle"), "静默窗要答成 idle 事件，而不是让调用方的读窗到期杀进程")

    def test_watch_with_generous_wait_ms_still_delivers_the_event(self):
        worker = CcxtJsonWorker(
            CcxtExchangeClient(CcxtConfig(exchange_id="binance"), exchange=FakeExchange()),
            pro_exchange=FakeProExchange(),
        )
        row = json.loads(
            worker.handle_line(
                json.dumps(
                    {
                        "op": "watch_orders",
                        "instrument": "BTCUSDT.BINANCE",
                        "wait_ms": 5_000,
                    }
                )
            )
        )
        self.assertTrue(row["ok"], row)
        event = row["result"]["event"]
        self.assertEqual(event["events"][0]["order_id"], "remote-1")
        self.assertNotIn("idle", event, "答得出来的事件不能被记成空闲窗")

    def test_watch_without_a_window_still_bounds_its_own_waits(self):
        # Rust 驱动方永远会带 wait_ms，这条不带的形状只有直接喂 JSONL 的调用方走得到。
        # 缺省成"永远等"就再没有东西能叫醒子进程：父进程读窗到期只会把它杀掉重开，
        # 而日志里留下的长相是子进程无故消失（V13 R17 C5）。
        class SilentPro(FakeProExchange):
            def __init__(self):
                self.asks = 0

            async def watch_orders(self, symbol, since, limit, params):
                self.asks += 1
                await asyncio.sleep(30)
                return []

        silent = SilentPro()
        worker = CcxtJsonWorker(
            CcxtExchangeClient(
                CcxtConfig(exchange_id="binance", timeout_ms=1_000), exchange=FakeExchange()
            ),
            pro_exchange=silent,
        )
        row = json.loads(
            worker.handle_line(
                json.dumps({"op": "watch_orders", "instrument": "BTCUSDT.BINANCE"})
            )
        )
        self.assertFalse(row["ok"], row)
        self.assertEqual(row["error"]["class"], "retryable")
        self.assertIn(
            "800",
            row["error"]["message"],
            "缺省窗口不再等于读窗的 4/5，子进程就会和父进程的读窗同时到期",
        )
        self.assertEqual(silent.asks, 1, "无名窗口到期是可重试故障，重连由驱动方记账")

        # 子进程内重连退避的封顶走同一条口径：基准 60000 毫秒 × 2**attempts 不设上限时
        # 三次就是 60+120+240 秒，父进程最长读窗才 300 秒——睡满之前进程早被杀掉重开。
        # sleep 换成记账，只问交出去的时长，用例因此不真等。
        class NeverRecovers:
            async def watch_orders(self, symbol, since, limit, params):
                raise CcxtConnectorError(CcxtErrorClass.RETRYABLE, "socket closed")

            async def close(self):
                return None

        sleeps: list[float] = []

        async def record(delay_seconds: float) -> None:
            sleeps.append(delay_seconds)

        capped = CcxtJsonWorker(
            CcxtExchangeClient(
                CcxtConfig(
                    exchange_id="binance",
                    timeout_ms=1_000,
                    ws_max_retries=3,
                    ws_retry_backoff_ms=60_000,
                ),
                exchange=FakeExchange(),
            ),
            pro_exchange=NeverRecovers(),
        )
        with (
            patch("asyncio.sleep", new=record),
            patch(
                "qianxing_ccxt.worker.create_ccxt_pro_exchange",
                return_value=NeverRecovers(),
            ),
        ):
            row = json.loads(
                capped.handle_line(
                    json.dumps(
                        {
                            "op": "watch_orders",
                            "instrument": "BTCUSDT.BINANCE",
                            "wait_ms": 60_000,
                        }
                    )
                )
            )
        self.assertFalse(row["ok"], row)
        self.assertEqual(row["error"]["class"], "retryable")
        self.assertEqual(
            sleeps,
            [8.0, 8.0, 8.0],
            "退避没有封顶或封顶口径漂移：Rust 侧 MAX_DELAY 是 8 秒，这里必须同值",
        )

    def test_jsonl_worker_recreates_ccxt_pro_after_retryable_disconnect(self):
        class FlakyPro(FakeProExchange):
            def __init__(self):
                self.failed = False

            async def watch_orders(self, symbol, since, limit, params):
                if not self.failed:
                    self.failed = True
                    raise ConnectionError("socket closed")
                return await super().watch_orders(symbol, since, limit, params)

        first = FlakyPro()
        replacement = FakeProExchange()
        config = CcxtConfig(
            exchange_id="binance", ws_max_retries=1, ws_retry_backoff_ms=0
        )
        worker = CcxtJsonWorker(
            CcxtExchangeClient(config, exchange=FakeExchange()), pro_exchange=first
        )
        with patch("qianxing_ccxt.worker.create_ccxt_pro_exchange", return_value=replacement) as factory:
            row = json.loads(
                worker.handle_line(
                    json.dumps({"op": "watch_orders", "instrument": "BTCUSDT.BINANCE"})
                )
            )
        self.assertTrue(row["ok"])
        self.assertEqual(row["result"]["event"]["events"][0]["order_id"], "remote-1")
        factory.assert_called_once()

    def test_jsonl_worker_does_not_retry_authentication_error(self):
        class AuthFailure:
            async def watch_orders(self, symbol, since, limit, params):
                raise CcxtConnectorError(CcxtErrorClass.AUTHENTICATION, "invalid key")

            async def close(self):
                return None

        config = CcxtConfig(exchange_id="binance", ws_max_retries=3, ws_retry_backoff_ms=0)
        worker = CcxtJsonWorker(
            CcxtExchangeClient(config, exchange=FakeExchange()), pro_exchange=AuthFailure()
        )
        row = json.loads(
            worker.handle_line(
                json.dumps({"op": "watch_orders", "instrument": "BTCUSDT.BINANCE"})
            )
        )
        self.assertFalse(row["ok"])
        self.assertEqual(row["error"]["class"], "authentication")

    def test_jsonl_worker_applies_exact_native_retry_budget_to_ccxt_timeouts(self):
        class RequestTimeout(Exception):
            pass

        class AlwaysTimedOut:
            def __init__(self):
                self.calls = 0
                self.closed = 0

            async def watch_orders(self, symbol, since, limit, params):
                self.calls += 1
                raise RequestTimeout("socket timed out")

            async def close(self):
                self.closed += 1

        initial = AlwaysTimedOut()
        replacements: list[AlwaysTimedOut] = []
        sleeps: list[float] = []

        def make_exchange(config):
            replacement = AlwaysTimedOut()
            replacements.append(replacement)
            return replacement

        async def record_sleep(delay_seconds: float) -> None:
            sleeps.append(delay_seconds)

        config = CcxtConfig(
            exchange_id="binance", ws_max_retries=2, ws_retry_backoff_ms=1
        )
        worker = CcxtJsonWorker(
            CcxtExchangeClient(config, exchange=FakeExchange()), pro_exchange=initial
        )
        with (
            patch("asyncio.sleep", new=record_sleep),
            patch("qianxing_ccxt.worker.create_ccxt_pro_exchange", side_effect=make_exchange),
        ):
            row = json.loads(
                worker.handle_line(
                    json.dumps(
                        {
                            "op": "watch_orders",
                            "instrument": "BTCUSDT.BINANCE",
                            "wait_ms": 60_000,
                        }
                    )
                )
            )

        self.assertFalse(row["ok"], row)
        self.assertEqual(row["error"]["class"], "retryable")
        self.assertEqual(initial.calls, 1)
        self.assertEqual([exchange.calls for exchange in replacements], [1, 1])
        self.assertEqual([exchange.closed for exchange in [initial, *replacements]], [1, 1, 0])
        self.assertEqual(len(sleeps), 2)
        self.assertEqual(sleeps, [0.001, 0.002])

    def test_jsonl_worker_retry_decisions_cover_every_native_ccxt_category(self):
        category_cases = (
            ("RequestTimeout", "retryable", True),
            ("DDoSProtection", "rate_limited", True),
            ("AuthenticationError", "authentication", False),
            ("InvalidOrder", "invalid", False),
            ("NotSupported", "unsupported", False),
            ("ExchangeError", "exchange", False),
            ("ValueError", "unknown", False),
        )

        for exception_name, expected_category, should_retry in category_cases:
            with self.subTest(exception=exception_name):
                exception_type = type(exception_name, (Exception,), {})
                exchanges = []
                sleeps = []

                class FailingPro:
                    def __init__(self):
                        self.calls = 0

                    async def watch_orders(self, symbol, since, limit, params):
                        self.calls += 1
                        raise exception_type("classified by Rust")

                    async def close(self):
                        return None

                initial = FailingPro()

                def recreate(config):
                    replacement = FailingPro()
                    exchanges.append(replacement)
                    return replacement

                async def record_sleep(delay_seconds: float) -> None:
                    sleeps.append(delay_seconds)

                worker = CcxtJsonWorker(
                    CcxtExchangeClient(
                        CcxtConfig(
                            exchange_id="binance",
                            ws_max_retries=1,
                            ws_retry_backoff_ms=1,
                        ),
                        exchange=FakeExchange(),
                    ),
                    pro_exchange=initial,
                )
                with (
                    patch("asyncio.sleep", new=record_sleep),
                    patch("qianxing_ccxt.worker.create_ccxt_pro_exchange", side_effect=recreate),
                ):
                    row = json.loads(
                        worker.handle_line(
                            json.dumps(
                                {
                                    "op": "watch_orders",
                                    "instrument": "BTCUSDT.BINANCE",
                                }
                            )
                        )
                    )

                self.assertFalse(row["ok"], row)
                self.assertEqual(row["error"]["class"], expected_category)
                self.assertEqual(initial.calls + sum(item.calls for item in exchanges), 2 if should_retry else 1)
                self.assertEqual(len(exchanges), 1 if should_retry else 0)
                self.assertEqual(len(sleeps), 1 if should_retry else 0)


if __name__ == "__main__":
    unittest.main()
