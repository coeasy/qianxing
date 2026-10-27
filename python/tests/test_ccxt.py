import asyncio
import sys
import unittest
from fractions import Fraction
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing_ccxt import (
    CcxtConfig,
    CcxtConnectorError,
    CcxtErrorClass,
    CcxtExchangeClient,
    SCALE,
    classify_ccxt_error,
    normalize_stream_event,
    _decimal_from_raw,
    _exact_decimal_text,
    _raw_decimal,
    _signed_raw_decimal,
    watch_stream,
    watch_ohlcv,
)
from qianxing_bridge import native


class FakeExchange:
    def __init__(self):
        self.precisionMode = self.TICK_SIZE = 4
        self.markets = {
            "BTC/USDT": {
                "symbol": "BTC/USDT",
                "id": "BTCUSDT",
                "base": "BTC",
                "quote": "USDT",
                "type": "spot",
                "active": True,
                "info": {"filters": []},
                "precision": {"price": "0.01", "amount": "0.0001"},
                "limits": {"amount": {"min": "0.001"}},
            },
            "BTC/USDT:USDT": {
                "symbol": "BTC/USDT:USDT",
                "id": "BTCUSDT-PERP",
                "base": "BTC",
                "quote": "USDT",
                "settle": "USDT",
                "type": "swap",
                "contract": True,
                "linear": True,
                "inverse": False,
                "contractSize": "0.001",
                "active": True,
                "precision": {"price": "0.1", "amount": "1"},
                "limits": {"amount": {"min": "1"}, "leverage": {"max": 50}},
                "maintenanceMarginRate": "0.005",
            },
            "PEPE/USDT": {
                "symbol": "PEPE/USDT",
                "id": "PEPEUSDT",
                "base": "PEPE",
                "quote": "USDT",
                "type": "spot",
                "active": True,
                "info": {"filters": []},
                "precision": {"price": "0.000000001", "amount": "0.000000001"},
                "limits": {"amount": {"min": "0.000000001"}},
            },
        }
        self.calls = []
        self.open_order_rows = []
        self.leverage_tier_rows = None
        self.sandbox_enabled = False

    def set_sandbox_mode(self, enabled):
        self.sandbox_enabled = bool(enabled)

    def load_markets(self, reload=False):
        self.calls.append(("load_markets", reload))
        return self.markets

    def fetch_ohlcv(self, symbol, timeframe, since, limit, params):
        self.calls.append(("fetch_ohlcv", symbol, timeframe, since, limit, params))
        if since == 1000:
            return [[1000, "100", "101", "99", "100.5", "2"], [1100, 101, 102, 100, 101, 3]]
        return [[1200, 102, 103, 101, 102, 4]]

    def fetch_ticker(self, symbol, params):
        self.calls.append(("fetch_ticker", symbol, params))
        return {
            "timestamp": 1234,
            "bid": "100.1",
            "ask": "100.2",
            "bidVolume": "2",
            "askVolume": "3",
            "last": "100.15",
        }

    def fetch_order_book(self, symbol, limit, params):
        self.calls.append(("fetch_order_book", symbol, limit, params))
        return {
            "timestamp": 1235,
            "bids": [["100.1", "2"]],
            "asks": [["100.2", "3"]],
        }


    def create_order(self, symbol, order_type, side, amount, price, params):
        self.calls.append(("create_order", symbol, order_type, side, amount, price, params))
        return {
            "id": "order-1",
            "clientOrderId": "client-1",
            "symbol": symbol,
            "status": "open",
            "side": side,
            "type": order_type,
            "amount": amount,
            "filled": "0",
            "remaining": amount,
            "price": price,
            "timestamp": 1234,
        }

    def private_uta_post_v3_trade_place_order(self, request):
        self.calls.append(("private_uta_post_v3_trade_place_order", dict(request)))
        return {
            "data": {
                "orderId": "uta-order-1",
                "clientOid": request.get("clientOid", ""),
                "symbol": request["symbol"],
                "qty": request["qty"],
                "side": request["side"],
                "orderType": request["orderType"],
                "status": "new",
            }
        }

    def parse_order(self, data, market):
        self.calls.append(("parse_order", dict(data), market["symbol"]))
        return {
            "id": data["orderId"],
            "clientOrderId": data.get("clientOid", ""),
            "symbol": market["symbol"],
            "status": "open",
            "side": data["side"],
            "type": data["orderType"],
            "amount": data["qty"],
            "filled": "0",
            "remaining": data["qty"],
            "price": None,
            "timestamp": 1234,
            "info": dict(data),
        }

    def cost_to_precision(self, symbol, cost):
        self.calls.append(("cost_to_precision", symbol, str(cost)))
        return str(cost)

    def fetch_order(self, order_id, symbol, params):
        return {
            "id": order_id,
            "clientOrderId": "client-1",
            "symbol": symbol,
            "status": "closed",
            "side": "buy",
            "type": "limit",
            "amount": "2",
            "filled": "2",
            "remaining": "0",
            "price": "100",
            "average": "100",
            "fee": {"cost": "0.2", "currency": "USDT"},
            "timestamp": 1234,
        }

    def fetch_my_trades(self, symbol, since, limit, params):
        return [
            {
                "id": "trade-1",
                "order": "order-1",
                "symbol": symbol,
                "side": "buy",
                "amount": "2",
                "price": "100",
                "fee": {"cost": "0.2", "currency": "USDT"},
                "timestamp": 1234,
            }
        ]

    def set_margin_mode(self, margin_mode, symbol, params):
        return {"marginMode": margin_mode, "symbol": symbol, "params": params}

    def set_leverage(self, leverage, symbol, params):
        return {"leverage": leverage, "symbol": symbol, "params": params}

    def set_position_mode(self, hedged, symbol, params):
        return {"hedged": hedged, "symbol": symbol, "params": params}

    def fetch_positions(self, symbols, params):
        self.calls.append(("fetch_positions", symbols, params))
        return [
            {
                "symbol": "BTC/USDT:USDT",
                "side": "long",
                "contracts": "2",
                "contractSize": "0.001",
                "entryPrice": "100",
                "markPrice": "101",
                "liquidationPrice": "50",
                "unrealizedPnl": "0.002",
                "initialMargin": "0.02",
                "maintenanceMargin": "0.01",
                "leverage": 5,
                "marginMode": "isolated",
                "hedged": True,
            }
        ]

    def fetch_open_orders(self, symbol, since, limit, params):
        self.calls.append(("fetch_open_orders", symbol, since, limit, params))
        return self.open_order_rows

    def fetch_balance(self, params):
        self.calls.append(("fetch_balance", params))
        return {"info": {"balances": []}}

    def fetch_funding_rate(self, symbol, params):
        return {
            "symbol": symbol,
            "timestamp": 1234,
            "fundingRate": "0.0001",
            "nextFundingTimestamp": 4567,
        }

    def fetch_ledger(self, code, since, limit, params):
        return [
            {
                "id": "fund-1",
                "currency": "USDT",
                "type": "funding",
                "amount": "-0.12",
                "timestamp": 5000,
            },
            {
                "id": "interest-1",
                "currency": "USDT",
                "type": "borrowInterest",
                "amount": "0.03",
                "timestamp": 5100,
            },
        ]

    def fetch_funding_history(self, symbol, since, limit, params):
        return [
            {
                "id": "fund-history-1",
                "code": "USDT",
                "amount": "-0.2",
                "timestamp": 5200,
            }
        ]

    def fetch_leverage_tiers(self, symbols, params):
        self.calls.append(("fetch_leverage_tiers", symbols, params))
        if self.leverage_tier_rows is not None:
            return {"BTC/USDT:USDT": self.leverage_tier_rows}
        return {
            "BTC/USDT:USDT": [
                {
                    "tier": 1,
                    "minNotional": "0",
                    "maxNotional": "100000",
                    "initialMarginRate": "0.1",
                    "maintenanceMarginRate": "0.005",
                    "maxLeverage": 10,
                },
                {
                    "tier": 2,
                    "minNotional": "100000",
                    "maxNotional": "500000",
                    "initialMarginRate": "0.2",
                    "maintenanceMarginRate": "0.01",
                    "maxLeverage": 5,
                },
            ]
        }


class FakeProExchange:
    async def watch_ticker(self, symbol, params):
        return {
            "symbol": symbol,
            "timestamp": 2000,
            "bid": "100.1",
            "ask": "100.2",
            "bidVolume": "2.0000000001",
            "askVolume": "3.0000000002",
            "last": "100.15",
        }

    async def watch_orders(self, symbol, since, limit, params):
        return [
            {
                "id": "remote-1",
                "clientOrderId": "client-1",
                "symbol": symbol or "BTC/USDT",
                "status": "closed",
                "side": "buy",
                "type": "limit",
                "amount": "2",
                "filled": "2",
                "remaining": "0",
                "price": "100",
                "timestamp": 2000,
            }
        ]

    async def close(self):
        return None


class CcxtTest(unittest.TestCase):
    def test_scale_is_reexported_from_native_rust(self):
        self.assertEqual(SCALE, native.raw_scale())

    def test_decimal_to_raw_and_formatting_delegate_to_native_rust(self):
        self.assertEqual(_raw_decimal("1.25e-2", field="price"), 12_500_000)
        self.assertEqual(_signed_raw_decimal("-2.25", field="fee"), -2_250_000_000)
        self.assertEqual(_decimal_from_raw(1_000_000_001, field="price"), "1.000000001")
        with self.assertRaisesRegex(CcxtConnectorError, "定点精度 1e-9"):
            _raw_decimal("0.0000000001", field="price")
        with self.assertRaisesRegex(CcxtConnectorError, "非负有限数字"):
            _raw_decimal("-1", field="amount")
        with self.assertRaisesRegex(CcxtConnectorError, "不能为负"):
            _decimal_from_raw(-1, field="amount")

    def setUp(self):
        self.exchange = FakeExchange()
        self.client = CcxtExchangeClient(
            CcxtConfig(exchange_id="binance"), exchange=self.exchange
        )

    def test_error_classification_and_retryability_delegate_to_rust(self):
        examples = {
            "DDoSProtection": CcxtErrorClass.RATE_LIMITED,
            "RequestTimeout": CcxtErrorClass.RETRYABLE,
            "AuthenticationError": CcxtErrorClass.AUTHENTICATION,
            "BadSymbol": CcxtErrorClass.INVALID,
            "NotSupported": CcxtErrorClass.UNSUPPORTED,
            "ExchangeError": CcxtErrorClass.EXCHANGE,
            "ValueError": CcxtErrorClass.UNKNOWN,
        }
        for name, expected in examples.items():
            with self.subTest(name=name):
                error = type(name, (Exception,), {})()
                self.assertEqual(classify_ccxt_error(error), expected)

        self.assertTrue(native.ccxt_error_may_retry("retryable"))
        self.assertTrue(native.ccxt_error_may_retry("rate_limited"))
        self.assertFalse(native.ccxt_error_may_retry("exchange"))
        self.assertFalse(native.ccxt_error_may_retry("unknown"))

    def test_spot_market_allows_explicit_null_contract_size(self):
        self.exchange.markets["BTC/USDT"]["contractSize"] = None
        market = self.client.resolve_market("BTC/USDT.BINANCE")
        self.assertEqual(market.contract_size_raw, 1_000_000_000)

    def test_low_price_spot_market_preserves_nine_decimal_ticks_and_lot_steps(self):
        market = self.client.resolve_market("PEPEUSDT.BINANCE")
        self.assertEqual(market.price_tick_raw, 1)
        self.assertEqual(market.qty_step_raw, 1)
        self.assertEqual(market.min_qty_raw, 1)
        self.assertEqual(market.exact_rules["price_tick"], "0.000000001")
        self.assertEqual(market.exact_rules["quantity_step"], "0.000000001")
        order = self.client.create_order(
            "PEPE/USDT.BINANCE",
            side="buy",
            order_type="limit",
            amount_raw=1,
            price_raw=1,
        )
        self.assertEqual(order.amount_raw, 1)
        self.assertEqual(order.price_raw, 1)

    def test_exact_spot_order_and_fill_facts_keep_sub_nano_amounts_and_fees(self):
        market = self.exchange.markets["PEPE/USDT"]
        market["precision"] = {
            "price": "0.0000000001",
            "amount": "0.0000000001",
        }
        market["limits"] = {
            "price": {"min": "0.0000000001"},
            "amount": {"min": "0.0000000001"},
        }

        submitted = self.client.create_order(
            "PEPE/USDT.BINANCE",
            side="buy",
            order_type="limit",
            amount_exact="0.0000000002",
            amount_unit="base",
            price_exact="1",
        )
        request = next(call for call in self.exchange.calls if call[0] == "create_order")
        self.assertEqual(request[4], "0.0000000002")
        self.assertEqual(request[5], "1")
        self.assertEqual(submitted.amount_exact, "0.0000000002")
        self.assertEqual(submitted.amount_raw, 0)

        resolved = self.client.resolve_market("PEPE/USDT.BINANCE")
        order = self.client._normalize_order(
            {
                "id": "exact-order",
                "amount": "0.0000000002",
                "filled": "0.0000000002",
                "remaining": "0",
                "price": "1",
                "average": "1",
                "cost": "0.0000000002",
                "fees": [{"cost": "0.000000000000000001", "currency": "PEPE"}],
            },
            resolved,
        )
        self.assertEqual(order.filled_raw, 0)
        self.assertEqual(order.filled_exact, "0.0000000002")
        self.assertEqual(order.exact_fees, (("0.000000000000000001", "PEPE"),))
        self.assertEqual(order.fees, ((0, "PEPE"),))

        trade = self.client._normalize_trade(
            {
                "id": "exact-trade",
                "order": "exact-order",
                "amount": "0.0000000002",
                "price": "1",
                "fees": [{"cost": "0.000000000000000001", "currency": "PEPE"}],
            },
            resolved,
        )
        self.assertEqual(trade.amount_raw, 0)
        self.assertEqual(trade.amount_exact, "0.0000000002")
        self.assertEqual(trade.exact_fees, (("0.000000000000000001", "PEPE"),))

    def test_float_normalized_fee_recovers_raw_decimal_or_fails_closed(self):
        raw_amount = "0.123456789012345678"
        normalized = 0.12345678901234568
        exact_fees = self.client._normalize_fees_with_exact(
            {
                "fees": [{"cost": normalized, "currency": "bnb"}],
                "info": {
                    "commission": raw_amount,
                    "commissionAsset": "BNB",
                },
            }
        )
        self.assertEqual(exact_fees[3], ((raw_amount, "BNB"),))

        okx_fee = self.client._normalize_fees_with_exact(
            {
                "fee": {"cost": normalized, "currency": "usdt"},
                "info": {"fee": f"-{raw_amount}", "feeCcy": "USDT"},
            }
        )
        self.assertEqual(okx_fee[3], ((raw_amount, "USDT"),))

        bitget_fee = self.client._normalize_fees_with_exact(
            {
                "fees": [{"cost": -normalized, "currency": "usdt"}],
                "info": {
                    "feeDetail": [
                        {
                            "deduction": "no",
                            "feeCoin": "USDT",
                            "totalFee": raw_amount,
                        }
                    ]
                },
            }
        )
        self.assertEqual(bitget_fee[3], ((f"-{raw_amount}", "USDT"),))

        with self.assertRaisesRegex(CcxtConnectorError, "原始 info 中没有"):
            self.client._normalize_fees_with_exact(
                {"fee": {"cost": normalized, "currency": "BNB"}}
            )
        self.assertEqual(
            self.client._normalize_fees_with_exact(
                {"fee": {"cost": 0.0, "currency": "BNB"}}
            )[3],
            (),
        )

    def test_sub_nanounit_ticks_map_to_their_exact_representable_lattice(self):
        self.assertEqual(self.client._precision_step_raw("0.0000000001"), 1)
        self.assertEqual(self.client._precision_step_raw("0.0000000003"), 3)
        self.assertEqual(self.client._precision_step_raw("0.0000000015"), 3)

    def test_sub_nanounit_market_rules_allow_only_exact_internal_grid_values(self):
        market = self.exchange.markets["PEPE/USDT"]
        market["precision"] = {
            "price": "0.0000000003",
            "amount": "0.0000000003",
        }
        market["limits"] = {
            "price": {"min": "0.0000000001", "max": "0.0000000031"},
            "amount": {"min": "0.0000000001", "max": "0.0000000031"},
        }
        resolved = self.client.resolve_market("PEPE/USDT.BINANCE")
        self.assertEqual(resolved.price_tick_raw, 3)
        self.assertEqual(resolved.qty_step_raw, 3)
        self.assertEqual(resolved.exact_rules["price_tick"], "0.0000000003")
        self.assertEqual(resolved.exact_rules["quantity_step"], "0.0000000003")
        self.assertEqual(resolved.min_price_raw, 1)
        self.assertEqual(resolved.max_price_raw, 3)
        self.assertEqual(resolved.min_qty_raw, 1)
        self.assertEqual(resolved.max_qty_raw, 3)

        accepted = self.client.create_order(
            "PEPE/USDT.BINANCE",
            side="buy",
            order_type="limit",
            amount_raw=3,
            price_raw=3,
        )
        self.assertEqual(accepted.amount_raw, 3)
        self.assertEqual(accepted.price_raw, 3)

        with self.assertRaisesRegex(CcxtConnectorError, "qty step"):
            self.client.create_order(
                "PEPE/USDT.BINANCE",
                side="buy",
                order_type="limit",
                amount_raw=2,
                price_raw=3,
            )

    def test_exact_market_spec_drift_catches_changes_hidden_by_legacy_raw_grid(self):
        market = self.client.resolve_market("PEPE/USDT.BINANCE")
        expected = {
            "product": "spot",
            "base_currency": "PEPE",
            "quote_currency": "USDT",
            "settlement_currency": "USDT",
            "contract_size_raw": str(market.contract_size_raw),
            "price_tick_raw": str(market.price_tick_raw),
            "qty_step_raw": str(market.qty_step_raw),
            "min_qty_raw": str(market.min_qty_raw),
            "min_notional_raw": "0",
            "exact_rules": market.exact_rules,
        }
        self.client.validate_live_market_spec("PEPE/USDT.BINANCE", expected)

        # Both 1e-9 and 1e-10 map to legacy raw step 1. The exact sidecar must still
        # detect that the venue's accepted amount lattice has changed.
        self.exchange.markets["PEPE/USDT"]["precision"]["amount"] = "0.0000000001"
        self.client._markets_loaded_at = 0.0
        with self.assertRaisesRegex(CcxtConnectorError, "exact_rules.quantity_step"):
            self.client.validate_live_market_spec("PEPE/USDT.BINANCE", expected)

    def test_positive_subnanounit_maximum_with_no_internal_value_fails_closed(self):
        market = self.exchange.markets["PEPE/USDT"]
        market["precision"] = {"price": "0.0000000001", "amount": "0.0000000001"}
        market["limits"] = {
            "amount": {"min": "0.0000000001", "max": "0.0000000005"}
        }
        resolved = self.client.resolve_market("PEPE/USDT.BINANCE")
        self.assertEqual(resolved.max_qty_raw, 0)
        with self.assertRaisesRegex(CcxtConnectorError, "数量超过交易所最大数量"):
            self.client.create_order(
                "PEPE/USDT.BINANCE",
                side="buy",
                order_type="limit",
                amount_raw=1,
                price_raw=1,
            )

    def test_positive_subnanounit_market_lot_maximum_is_not_dropped(self):
        market = self.exchange.markets["PEPE/USDT"]
        market["precision"] = {"price": "0.0000000001", "amount": "0.0000000001"}
        market["limits"] = {"amount": {"max": "10"}}
        market["info"] = {
            "filters": [
                {
                    "filterType": "MARKET_LOT_SIZE",
                    "stepSize": "0.0000000001",
                    "minQty": "0",
                    "maxQty": "0.0000000005",
                }
            ]
        }
        resolved = self.client.resolve_market("PEPE/USDT.BINANCE")
        self.assertEqual(resolved.market_order_max_qty_raw, 0)

        with self.assertRaisesRegex(CcxtConnectorError, "数量超过交易所最大数量"):
            self.client.create_order(
                "PEPE/USDT.BINANCE",
                side="buy",
                order_type="market",
                amount_raw=1,
            )
        self.assertFalse(any(call[0] == "fetch_ticker" for call in self.exchange.calls))

    def test_order_amount_unit_must_match_spot_base_or_contract_market(self):
        spot_order = self.client.create_order(
            "BTC/USDT.BINANCE",
            side="buy",
            order_type="limit",
            amount_raw=SCALE,
            amount_unit="base",
            price_raw=100 * SCALE,
        )
        self.assertEqual(spot_order.amount_raw, SCALE)
        with self.assertRaisesRegex(CcxtConnectorError, "单位.*base.*不一致"):
            self.client.create_order(
                "BTC/USDT.BINANCE",
                side="buy",
                order_type="limit",
                amount_raw=SCALE,
                amount_unit="contracts",
                price_raw=100 * SCALE,
            )

        swap_order = self.client.create_order(
            "BTC/USDT:USDT.BINANCE",
            side="buy",
            order_type="limit",
            amount_raw=SCALE,
            amount_unit="contracts",
            price_raw=100 * SCALE,
        )
        self.assertEqual(swap_order.amount_raw, SCALE)
        with self.assertRaisesRegex(CcxtConnectorError, "单位.*contracts.*不一致"):
            self.client.create_order(
                "BTC/USDT:USDT.BINANCE",
                side="buy",
                order_type="limit",
                amount_raw=SCALE,
                amount_unit="base",
                price_raw=100 * SCALE,
            )
        self.assertEqual(
            sum(1 for call in self.exchange.calls if call[0] == "create_order"), 2
        )

    def test_bitget_uta_contract_settings_identify_account_path(self):
        class UtaExchange(FakeExchange):
            options = {"uta": True}

            def privateUtaGetV3AccountSettings(self):
                return {
                    "data": {
                        "holdMode": "one_way_mode",
                        "symbolConfigList": [
                            {
                                "symbol": "BTCUSDT-PERP",
                                "marginMode": "isolated",
                                "longLeverage": "3",
                                "shortLeverage": "3",
                            }
                        ],
                    }
                }

        client = CcxtExchangeClient(
            CcxtConfig(exchange_id="bitget", options={"uta": True}),
            exchange=UtaExchange(),
        )
        settings = client.fetch_contract_settings("BTC/USDT:USDT.BITGET")
        self.assertTrue(settings["uta"])
        self.assertEqual(settings["margin_mode"], "isolated")

    def test_create_order_enforces_cost_minimum_with_exact_raw_arithmetic(self):
        self.exchange.markets["BTC/USDT"]["limits"]["cost"] = {"min": "11"}
        market = self.client.resolve_market("BTCUSDT.BINANCE")
        self.assertEqual(market.min_notional_raw, 11 * SCALE)
        with self.assertRaises(CcxtConnectorError) as raised:
            self.client.create_order(
                "BTCUSDT.BINANCE",
                side="buy",
                order_type="limit",
                amount_raw=100_000_000,
                price_raw=100 * SCALE,
            )
        self.assertEqual(raised.exception.error_class, CcxtErrorClass.INVALID)
        accepted = self.client.create_order(
            "BTCUSDT.BINANCE",
            side="buy",
            order_type="limit",
            amount_raw=100_000_000,
            price_raw=110 * SCALE,
        )
        self.assertEqual(accepted.amount_raw, 100_000_000)

    def test_market_order_uses_side_aware_ticker_price_for_cost_minimum(self):
        self.exchange.markets["BTC/USDT"]["limits"]["cost"] = {"min": "11"}
        accepted = self.client.create_order(
            "BTCUSDT.BINANCE",
            side="buy",
            order_type="market",
            amount_raw=110_000_000,
        )
        self.assertEqual(accepted.amount_raw, 110_000_000)

    def test_okx_spot_market_buy_explicitly_uses_base_currency_sizing(self):
        exchange = FakeExchange()
        client = CcxtExchangeClient(CcxtConfig(exchange_id="okx"), exchange=exchange)
        client.create_order(
            "BTC/USDT.OKX",
            side="buy",
            order_type="market",
            amount_raw=SCALE,
            amount_unit="base",
        )
        submitted = next(call for call in exchange.calls if call[0] == "create_order")
        self.assertEqual(submitted[-1]["tgtCcy"], "base_ccy")

    def test_bitget_spot_market_buy_fails_before_ticker_or_write(self):
        exchange = FakeExchange()
        exchange.markets["BTC/USDT"]["limits"]["cost"] = {"min": "10"}
        client = CcxtExchangeClient(CcxtConfig(exchange_id="bitget"), exchange=exchange)
        with self.assertRaises(CcxtConnectorError) as raised:
            client.create_order(
                "BTC/USDT.BITGET",
                side="buy",
                order_type="market",
                amount_raw=SCALE,
                amount_unit="base",
            )
        self.assertEqual(raised.exception.error_class, CcxtErrorClass.UNSUPPORTED)
        self.assertFalse(any(call[0] in {"fetch_ticker", "create_order"} for call in exchange.calls))

    def test_bitget_spot_market_buy_accepts_exact_quote_budget_only(self):
        exchange = FakeExchange()
        client = CcxtExchangeClient(CcxtConfig(exchange_id="bitget"), exchange=exchange)
        order = client.create_order(
            "PEPE/USDT.BITGET",
            side="buy",
            order_type="market",
            amount_exact="12.34000001",
            amount_unit="quote",
        )
        self.assertEqual(order.order_id, "order-1")
        submitted = next(call for call in exchange.calls if call[0] == "create_order")
        self.assertEqual(submitted[1:6], ("PEPE/USDT", "market", "buy", "12.34000001", None))
        self.assertFalse(submitted[6]["createMarketBuyOrderRequiresPrice"])
        self.assertTrue(any(call[0] == "cost_to_precision" for call in exchange.calls))

    def test_bitget_quote_budget_rejects_non_buy_and_non_market_sizing(self):
        exchange = FakeExchange()
        client = CcxtExchangeClient(CcxtConfig(exchange_id="bitget"), exchange=exchange)
        with self.assertRaises(CcxtConnectorError):
            client.create_order(
                "PEPE/USDT.BITGET",
                side="sell",
                order_type="market",
                amount_exact="12.34",
                amount_unit="quote",
            )
        self.assertFalse(any(call[0] == "create_order" for call in exchange.calls))

    def test_bitget_uta_quote_budget_uses_quote_precision_and_uta_wire_shape(self):
        exchange = FakeExchange()
        client = CcxtExchangeClient(
            CcxtConfig(exchange_id="bitget", options={"uta": True}),
            exchange=exchange,
        )
        order = client.create_order(
            "PEPE/USDT.BITGET",
            side="buy",
            order_type="market",
            amount_exact="0.00000001",
            amount_unit="quote",
            client_order_id="qx-uta-budget",
        )
        self.assertEqual(order.order_id, "uta-order-1")
        submitted = next(
            call for call in exchange.calls if call[0] == "private_uta_post_v3_trade_place_order"
        )[1]
        self.assertEqual(
            submitted,
            {
                "category": "SPOT",
                "symbol": "PEPEUSDT",
                "qty": "0.00000001",
                "side": "buy",
                "orderType": "market",
                "clientOid": "qx-uta-budget",
            },
        )
        self.assertFalse(any(call[0] == "create_order" for call in exchange.calls))

    def test_symbol_mapping_and_paginated_ohlcv_preserve_fixed_point_columns(self):
        frame = self.client.fetch_ohlcv(
            "BTCUSDT.BINANCE", start_ms=1000, end_ms=1200, limit=2
        )
        self.assertEqual(frame.ts, (1000, 1100, 1200))
        self.assertEqual(frame.open_raw[0], 100_000_000_000)
        self.assertEqual(frame.close_raw[0], 100_500_000_000)
        self.assertEqual(frame.source, "ccxt:binance:BTC/USDT:1m")

    def test_ohlcv_keeps_sub_nano_source_precision_for_small_tokens(self):
        class TinyTokenExchange(FakeExchange):
            def fetch_ohlcv(self, symbol, timeframe, since, limit, params):
                self.calls.append(("fetch_ohlcv", symbol, timeframe, since, limit, params))
                return [[since, "0.0000000001", "0.0000000003", "0.0000000001", "0.0000000002", "0.00000000005"]]

        exchange = TinyTokenExchange()
        client = CcxtExchangeClient(CcxtConfig(exchange_id="binance"), exchange=exchange)
        frame = client.fetch_ohlcv("BTCUSDT.BINANCE", start_ms=1000, end_ms=1000)

        self.assertEqual(frame.open_raw, (0,))
        self.assertEqual(frame.exact_ohlcv[0]["close"], {"coefficient": "2", "scale": 10})
        self.assertEqual(frame.exact_ohlcv[0]["volume"], {"coefficient": "5", "scale": 11})

    def test_ohlcv_page_limit_exhaustion_fails_closed_instead_of_returning_partial_history(self):
        class FullPageExchange(FakeExchange):
            def fetch_ohlcv(self, symbol, timeframe, since, limit, params):
                self.calls.append(("fetch_ohlcv", symbol, timeframe, since, limit, params))
                return [
                    [since, 100, 101, 99, 100, 1],
                    [since + 1, 100, 101, 99, 100, 1],
                ]

        exchange = FullPageExchange()
        client = CcxtExchangeClient(CcxtConfig(exchange_id="binance"), exchange=exchange)
        with self.assertRaises(CcxtConnectorError) as raised:
            client.fetch_ohlcv(
                "BTCUSDT.BINANCE",
                start_ms=1000,
                end_ms=5000,
                limit=2,
                max_pages=2,
            )

        self.assertEqual(raised.exception.error_class, CcxtErrorClass.EXCHANGE)
        self.assertIn("refusing partial history", str(raised.exception))
        fetch_calls = [call for call in exchange.calls if call[0] == "fetch_ohlcv"]
        self.assertEqual(len(fetch_calls), 2)

    def test_ohlcv_full_page_that_repeats_the_cursor_fails_closed(self):
        class RepeatingExchange(FakeExchange):
            def fetch_ohlcv(self, symbol, timeframe, since, limit, params):
                self.calls.append(("fetch_ohlcv", symbol, timeframe, since, limit, params))
                return [[since, 100, 101, 99, 100, 1]] * limit

        exchange = RepeatingExchange()
        client = CcxtExchangeClient(CcxtConfig(exchange_id="binance"), exchange=exchange)
        with self.assertRaisesRegex(CcxtConnectorError, "pagination made no progress"):
            client.fetch_ohlcv(
                "BTCUSDT.BINANCE",
                start_ms=1000,
                end_ms=5000,
                limit=2,
                max_pages=3,
            )

    def test_ohlcv_query_has_a_hard_row_limit(self):
        class OversizedExchange(FakeExchange):
            def fetch_ohlcv(self, symbol, timeframe, since, limit, params):
                self.calls.append(("fetch_ohlcv", symbol, timeframe, since, limit, params))
                return [
                    [since + index, 100, 101, 99, 100, 1]
                    for index in range(limit)
                ]

        exchange = OversizedExchange()
        client = CcxtExchangeClient(CcxtConfig(exchange_id="binance"), exchange=exchange)
        with self.assertRaisesRegex(CcxtConnectorError, "100000-row safety limit") as raised:
            client.fetch_ohlcv(
                "BTCUSDT.BINANCE",
                start_ms=1000,
                end_ms=200_000,
                limit=100_001,
                max_pages=1,
            )
        self.assertEqual(raised.exception.error_class, CcxtErrorClass.INVALID)

    def test_ohlcv_rejects_an_exchange_page_larger_than_requested(self):
        class OversizedPageExchange(FakeExchange):
            def fetch_ohlcv(self, symbol, timeframe, since, limit, params):
                self.calls.append(("fetch_ohlcv", symbol, timeframe, since, limit, params))
                return [[since, 100, 101, 99, 100, 1]] * (limit + 1)

        exchange = OversizedPageExchange()
        client = CcxtExchangeClient(CcxtConfig(exchange_id="binance"), exchange=exchange)
        with self.assertRaisesRegex(CcxtConnectorError, "more rows than the requested"):
            client.fetch_ohlcv(
                "BTCUSDT.BINANCE",
                start_ms=1000,
                end_ms=5000,
                limit=2,
                max_pages=1,
            )

    def test_ticker_and_order_use_normalized_values(self):
        ticker = self.client.fetch_ticker("BTCUSDT.BINANCE")
        self.assertEqual(ticker.bid_raw, 100_100_000_000)
        order = self.client.create_order(
            "BTCUSDT.BINANCE",
            side="buy",
            order_type="limit",
            amount_raw=2_000_000_000,
            price_raw=100_000_000_000,
        )
        self.assertEqual(order.order_id, "order-1")
        self.assertEqual(order.amount_raw, 2_000_000_000)

    def test_ticker_without_last_keeps_valid_bid_ask_for_risk_reference(self):
        class MissingLastExchange(FakeExchange):
            def fetch_ticker(self, symbol, params):
                return {
                    "timestamp": 1234,
                    "bid": "100.1",
                    "ask": "100.2",
                    "bidVolume": "2",
                    "askVolume": "3",
                    "last": None,
                }

        ticker = CcxtExchangeClient(
            CcxtConfig(exchange_id="binance"), exchange=MissingLastExchange()
        ).fetch_ticker("BTC/USDT:USDT.BINANCE")
        self.assertEqual(ticker.last_raw, 0)
        self.assertEqual(ticker.ask_raw, 100_200_000_000)

    def test_ticker_preserves_sub_nano_reference_prices_as_exact_strings(self):
        class SubNanoExchange(FakeExchange):
            def fetch_ticker(self, symbol, params):
                return {
                    "timestamp": 1234,
                    "bid": "0.0000000007",
                    "ask": "0.0000000009",
                    "bidVolume": "1000000.0000000001",
                    "askVolume": "2000000.0000000002",
                    "last": "0.0000000008",
                }

        ticker = CcxtExchangeClient(
            CcxtConfig(exchange_id="binance"), exchange=SubNanoExchange()
        ).fetch_ticker("BTC/USDT:USDT.BINANCE")
        self.assertEqual(ticker.bid_raw, 0)
        self.assertEqual(ticker.ask_raw, 0)
        self.assertEqual(ticker.bid_exact, "0.0000000007")
        self.assertEqual(ticker.ask_exact, "0.0000000009")
        self.assertEqual(ticker.bid_qty_raw, 1_000_000_000_000_000)
        self.assertEqual(ticker.bid_qty_exact, "1000000.0000000001")
        self.assertEqual(ticker.ask_qty_exact, "2000000.0000000002")

    def test_derivative_ticker_falls_back_to_order_book_when_quote_is_missing(self):
        class MissingQuoteExchange(FakeExchange):
            def fetch_ticker(self, symbol, params):
                return {
                    "timestamp": 1234,
                    "bid": None,
                    "ask": None,
                    "bidVolume": None,
                    "askVolume": None,
                    "last": "100.15",
                }

        exchange = MissingQuoteExchange()
        client = CcxtExchangeClient(
            CcxtConfig(exchange_id="binance"), exchange=exchange
        )
        ticker = client.fetch_ticker("BTC/USDT:USDT.BINANCE")
        self.assertEqual(ticker.timestamp_ms, 1234)
        self.assertEqual(ticker.bid_raw, 100_100_000_000)
        self.assertEqual(ticker.ask_raw, 100_200_000_000)
        self.assertEqual(ticker.bid_qty_raw, 2_000_000_000)
        self.assertEqual(ticker.ask_qty_raw, 3_000_000_000)
        self.assertEqual(ticker.bid_qty_exact, "2")
        self.assertEqual(ticker.ask_qty_exact, "3")
        self.assertIn(("fetch_order_book", "BTC/USDT:USDT", 5, {}), exchange.calls)

    def test_fetch_order_book_keeps_exact_small_coin_levels_and_sequence(self):
        class TinyBookExchange(FakeExchange):
            def fetch_order_book(self, symbol, limit, params):
                self.calls.append(("fetch_order_book", symbol, limit, params))
                return {
                    "timestamp": 1235,
                    "nonce": 91,
                    "bids": [["0.0000000012", "0.0000000003"]],
                    "asks": [["0.0000000013", "0.0000000004"]],
                }

        client = CcxtExchangeClient(
            CcxtConfig(exchange_id="binance"), exchange=TinyBookExchange()
        )
        book = client.fetch_order_book("PEPE/USDT.BINANCE", limit=10)

        self.assertEqual(book.timestamp_ms, 1235)
        self.assertEqual(book.sequence, 91)
        self.assertEqual(book.bids[0].price_exact, "0.0000000012")
        self.assertEqual(book.bids[0].price_raw, 1)
        self.assertEqual(book.bids[0].qty_exact, "0.0000000003")
        self.assertEqual(book.bids[0].qty_raw, 0)
        self.assertEqual(book.asks[0].price_exact, "0.0000000013")

    def test_fetch_order_book_rejects_crossed_books_and_unsafe_depth_limits(self):
        class CrossedBookExchange(FakeExchange):
            def fetch_order_book(self, symbol, limit, params):
                return {
                    "timestamp": 1235,
                    "bids": [["100.2", "2"]],
                    "asks": [["100.1", "3"]],
                }

        client = CcxtExchangeClient(
            CcxtConfig(exchange_id="binance"), exchange=CrossedBookExchange()
        )
        with self.assertRaisesRegex(CcxtConnectorError, "买卖盘交叉"):
            client.fetch_order_book("BTC/USDT.BINANCE")
        with self.assertRaisesRegex(CcxtConnectorError, "1..=100"):
            client.fetch_order_book("BTC/USDT.BINANCE", limit=101)

    def test_ticker_without_timestamp_uses_order_book_timestamp(self):
        class MissingTimestampExchange(FakeExchange):
            def fetch_ticker(self, symbol, params):
                return {
                    "timestamp": None,
                    "bid": "100.1",
                    "ask": "100.2",
                    "bidVolume": "2",
                    "askVolume": "3",
                    "last": "100.15",
                }

        exchange = MissingTimestampExchange()
        client = CcxtExchangeClient(
            CcxtConfig(exchange_id="binance"), exchange=exchange
        )
        ticker = client.fetch_ticker("BTC/USDT:USDT.BINANCE")
        self.assertEqual(ticker.timestamp_ms, 1235)


    def test_invalid_exchange_binding_and_pro_stream_are_explicit(self):
        with self.assertRaises(CcxtConnectorError) as context:
            self.client.resolve_market("BTCUSDT.OKX")
        self.assertEqual(context.exception.error_class, CcxtErrorClass.INVALID)

        with self.assertRaises(CcxtConnectorError) as context:
            asyncio.run(watch_ohlcv(self.exchange, "BTC/USDT"))
        self.assertEqual(context.exception.error_class, CcxtErrorClass.UNSUPPORTED)

    def test_contract_margin_leverage_position_and_funding_are_normalized(self):
        instrument = "BTC/USDT:USDT.BINANCE"
        market = self.client.resolve_market(instrument)
        self.assertTrue(market.contract)
        self.assertEqual(market.contract_size_raw, 1_000_000)
        self.assertTrue(market.linear)
        self.assertEqual(market.price_tick_raw, 100_000_000)
        self.assertEqual(market.qty_step_raw, 1_000_000_000)
        self.assertEqual(market.min_qty_raw, 1_000_000_000)
        self.assertEqual(market.max_leverage, 50)
        self.assertEqual(market.maintenance_margin_bps, 50)
        self.client.set_margin_mode(instrument, "isolated")
        self.client.set_leverage(instrument, 5, margin_mode="isolated")
        self.client.set_position_mode(True, instrument=instrument)
        position = self.client.fetch_positions([instrument])[0]
        self.assertEqual(position.side, "long")
        self.assertEqual(position.contracts_raw, 2_000_000_000)
        self.assertEqual(position.leverage, 5)
        funding = self.client.fetch_funding_rate(instrument)
        self.assertEqual(funding.funding_rate_bps, 1)

    def test_position_and_balance_facts_keep_sub_nano_decimal_strings(self):
        instrument = "BTC/USDT:USDT.BINANCE"
        market = self.client.resolve_market(instrument)
        position = self.client._normalize_position(
            {
                "symbol": market.symbol,
                "side": "short",
                "contracts": "0.0000000001",
                "contractSize": "0.001",
                "entryPrice": "0.0000000002",
                "markPrice": "0.0000000003",
                "unrealizedPnl": "-0.0000000004",
                "initialMargin": "0.0000000005",
            },
            market,
        )
        self.assertEqual(position.contracts_raw, 0)
        self.assertEqual(position.contracts_exact, "0.0000000001")
        self.assertEqual(position.entry_price_raw, 0)
        self.assertEqual(position.entry_price_exact, "0.0000000002")
        self.assertEqual(position.unrealized_pnl_raw, 0)
        self.assertEqual(position.unrealized_pnl_exact, "-0.0000000004")

        exact = self.client.exact_balance_fields(
            {"free": {"PEPE": "0.000000000123"}, "used": {"PEPE": 0.0}}
        )
        self.assertEqual(exact["free"]["PEPE"], "0.000000000123")
        self.assertEqual(exact["used"]["PEPE"], "0")

    def test_float_normalized_positions_recover_exchange_raw_precision(self):
        precise_amount = "0.000000123456789123"
        precise_entry = "0.000000000234567891"
        precise_mark = "0.000000000345678912"
        precise_pnl = "-0.000000000000000123"

        for exchange_id, raw_info, normalized_contracts, extra in (
            (
                "binance",
                {
                    "positionAmt": "-" + precise_amount,
                    "entryPrice": precise_entry,
                    "markPrice": precise_mark,
                    "liquidationPrice": "0",
                    "unRealizedProfit": precise_pnl,
                    "positionInitialMargin": "0.000000000000000456",
                    "maintMargin": "0.000000000000000789",
                },
                float(precise_amount),
                {
                    "initialMargin": float("0.000000000000000456"),
                    "maintenanceMargin": float("0.000000000000000789"),
                },
            ),
            (
                "okx",
                {
                    "pos": "-" + precise_amount,
                    "avgPx": precise_entry,
                    "markPx": precise_mark,
                    "liqPx": "0",
                    "upl": precise_pnl,
                    "imr": "0.000000000000000456",
                    "mmr": "0.000000000000000789",
                },
                float(precise_amount),
                {
                    "initialMargin": float("0.000000000000000456"),
                    "maintenanceMargin": float("0.000000000000000789"),
                },
            ),
            (
                "bitget",
                {
                    "total": "0.00000123456",
                    "holdSide": "short",
                    "openPriceAvg": "123.456",
                    "markPrice": "123.456",
                    "liquidationPrice": "0",
                    "unrealizedPL": precise_pnl,
                    "marginSize": "0.000000000000000456",
                    "marginMode": "crossed",
                    "keepMarginRate": "0.004",
                    "leverage": "20",
                },
                float(Fraction("0.00000123456") / Fraction("0.001")),
                {
                    "initialMargin": float("0.000000000000000456"),
                    "maintenanceMargin": float(
                        Fraction("0.00000123456")
                        * Fraction("123.456")
                        * (Fraction("0.004") + Fraction("0.0006"))
                    ),
                },
            ),
        ):
            with self.subTest(exchange_id=exchange_id):
                position_entry = (
                    "123.456" if exchange_id == "bitget" else precise_entry
                )
                position_mark = (
                    "123.456" if exchange_id == "bitget" else precise_mark
                )
                client = CcxtExchangeClient(
                    CcxtConfig(exchange_id=exchange_id), exchange=FakeExchange()
                )
                market = client.resolve_market(f"BTC/USDT:USDT.{exchange_id.upper()}")
                normalized = {
                    "symbol": market.symbol,
                    "side": "short",
                    "contracts": normalized_contracts,
                    "contractSize": market.contract_size_raw / SCALE,
                    "entryPrice": float(position_entry),
                    "markPrice": float(position_mark),
                    "liquidationPrice": None,
                    "unrealizedPnl": float(precise_pnl),
                    "leverage": 5,
                    "marginMode": "cross",
                    "hedged": True,
                    "info": raw_info,
                    **extra,
                }
                position = client._normalize_position(normalized, market)
                expected_contracts = (
                    "0.000000123456789123"
                    if exchange_id != "bitget"
                    else "0.00123456"
                )
                self.assertEqual(position.contracts_exact, expected_contracts)
                self.assertEqual(position.entry_price_exact, position_entry)
                self.assertEqual(position.mark_price_exact, position_mark)
                self.assertEqual(position.unrealized_pnl_exact, precise_pnl)
                self.assertEqual(position.initial_margin_exact, "0.000000000000000456")
                if exchange_id == "bitget":
                    self.assertEqual(
                        position.maintenance_margin_exact,
                        _exact_decimal_text(
                            Fraction("0.00000123456")
                            * Fraction("123.456")
                            * (Fraction("0.004") + Fraction("0.0006")),
                            field="expected maintenance margin",
                        ),
                    )
                else:
                    self.assertEqual(
                        position.maintenance_margin_exact,
                        "0.000000000000000789",
                    )

    def test_float_normalized_position_without_raw_precision_fails_closed(self):
        market = self.client.resolve_market("BTC/USDT:USDT.BINANCE")
        with self.assertRaisesRegex(CcxtConnectorError, "无法从交易所原始字段精确恢复"):
            self.client._normalize_position(
                {
                    "symbol": market.symbol,
                    "side": "long",
                    "contracts": 1.234567890123,
                    "entryPrice": "100",
                },
                market,
            )
        with self.assertRaisesRegex(CcxtConnectorError, "无法从交易所原始字段精确恢复"):
            self.client._normalize_position(
                {
                    "symbol": market.symbol,
                    "side": "long",
                    "contracts": 1.234567890123,
                    "entryPrice": "100",
                    "info": {"positionAmt": 1.234567890123},
                },
                market,
            )

    def test_margin_and_funding_rates_use_exact_native_basis_point_conversion(self):
        self.assertEqual(CcxtExchangeClient._maintenance_margin_bps({"maintenanceMarginRate": "0.005"}), 50)
        self.assertEqual(CcxtExchangeClient._maintenance_margin_bps({"maintenanceMarginPercent": "0.5"}), 50)
        self.assertIsNone(CcxtExchangeClient._maintenance_margin_bps({"maintenanceMarginRate": "0.00001"}))

    def test_cashflow_ledger_and_funding_history_preserve_signed_amounts(self):
        ledger = self.client.fetch_ledger(code="USDT", since_ms=1, limit=10)
        self.assertEqual([item.kind for item in ledger], ["funding", "interest"])
        self.assertEqual(ledger[0].amount_raw, -120_000_000)
        self.assertEqual(ledger[1].amount_raw, 30_000_000)
        history = self.client.fetch_funding_history("BTC/USDT:USDT.BINANCE")
        self.assertEqual(history[0].external_id, "fund-history-1")
        self.assertEqual(history[0].amount_raw, -200_000_000)

    def test_order_and_trade_fees_are_normalized(self):
        order = self.client.fetch_order("order-1", "BTCUSDT.BINANCE")
        self.assertEqual(order.fee_raw, 200_000_000)
        self.assertEqual(order.fee_currency, "USDT")
        trades = self.client.fetch_my_trades("BTCUSDT.BINANCE")
        self.assertEqual(trades[0].trade_id, "trade-1")
        self.assertEqual(trades[0].fee_raw, 200_000_000)

    def test_leverage_tiers_are_normalized_to_fixed_point_rates(self):
        tiers = self.client.fetch_leverage_tiers("BTC/USDT:USDT.BINANCE")
        self.assertEqual(len(tiers), 2)
        self.assertEqual(tiers[0].max_notional_raw, 100_000 * 1_000_000_000)
        self.assertEqual(tiers[0].exposure_unit, "quote_notional")
        self.assertEqual(tiers[0].initial_margin_bps, 1_000)
        self.assertEqual(tiers[1].maintenance_margin_bps, 100)

    def test_market_leverage_tier_fallback_preserves_exchange_exposure_unit(self):
        class MarketTierExchange(FakeExchange):
            def __init__(self):
                super().__init__()
                self.has = {
                    "fetchLeverageTiers": False,
                    "fetchMarketLeverageTiers": True,
                }
                self.market_tier_calls = []

            def fetch_market_leverage_tiers(self, symbol, params):
                self.market_tier_calls.append((symbol, params))
                return [
                    {
                        "tier": 1,
                        "minNotional": "0",
                        "maxNotional": "3",
                        "maxLeverage": 20,
                    },
                    {
                        "tier": 2,
                        "minNotional": "3",
                        "maxNotional": "100",
                        "maxLeverage": 5,
                    },
                ]

        exchange = MarketTierExchange()
        client = CcxtExchangeClient(CcxtConfig(exchange_id="okx"), exchange=exchange)
        tiers = client.fetch_leverage_tiers(
            "BTC/USDT:USDT.OKX", margin_mode="isolated"
        )
        self.assertEqual(tiers[0].exposure_unit, "contracts")
        self.assertEqual(tiers[0].max_notional_raw, 3 * SCALE)
        self.assertEqual(
            exchange.market_tier_calls,
            [("BTC/USDT:USDT", {"marginMode": "isolated"})],
        )
        bitget_exchange = MarketTierExchange()
        bitget_client = CcxtExchangeClient(
            CcxtConfig(exchange_id="bitget"), exchange=bitget_exchange
        )
        bitget_tiers = bitget_client.fetch_leverage_tiers(
            "BTC/USDT:USDT.BITGET", margin_mode="cross"
        )
        self.assertEqual(bitget_tiers[0].exposure_unit, "quote_notional")
        self.assertEqual(
            bitget_exchange.market_tier_calls,
            [("BTC/USDT:USDT", {})],
        )
        with self.assertRaisesRegex(CcxtConnectorError, "最高杠杆 5x"):
            client.create_order(
                "BTC/USDT:USDT.OKX",
                side="buy",
                order_type="limit",
                amount_raw=2 * SCALE,
                amount_unit="contracts",
                price_raw=100 * SCALE,
                risk_leverage=10,
                risk_margin_mode="isolated",
            )

    def test_bitget_inverse_leverage_tiers_use_face_value_without_mark_price(self):
        class InverseExchange(FakeExchange):
            def fetch_positions(self, symbols, params):
                self.calls.append(("fetch_positions", symbols, params))
                return [
                    {
                        "symbol": "BTC/USDT:USDT",
                        "side": "long",
                        "contracts": "2",
                        "contractSize": "1",
                        "entryPrice": "100",
                        "markPrice": None,
                        "hedged": True,
                    }
                ]

        exchange = InverseExchange()
        exchange.markets["BTC/USDT:USDT"]["inverse"] = True
        exchange.markets["BTC/USDT:USDT"]["contractSize"] = "1"
        exchange.leverage_tier_rows = [
            {
                "tier": 1,
                "minNotional": "0",
                "maxNotional": "3",
                "maintenanceMarginRate": "0.005",
                "maxLeverage": 20,
            },
            {
                "tier": 2,
                "minNotional": "3",
                "maxNotional": "1000",
                "maintenanceMarginRate": "0.01",
                "maxLeverage": 5,
            },
        ]
        client = CcxtExchangeClient(CcxtConfig(exchange_id="bitget"), exchange=exchange)
        tiers = client.fetch_leverage_tiers("BTC/USDT:USDT.BITGET")
        self.assertEqual(tiers[0].exposure_unit, "quote_notional")
        self.assertEqual(tiers[0].max_notional_raw, 3 * SCALE)
        with self.assertRaisesRegex(CcxtConnectorError, "最高杠杆 5x"):
            client.create_order(
                "BTC/USDT:USDT.BITGET",
                side="buy",
                order_type="limit",
                amount_raw=2 * SCALE,
                amount_unit="contracts",
                price_raw=100 * SCALE,
                risk_leverage=10,
                risk_margin_mode="isolated",
            )
        self.assertFalse(any(call[0] == "create_order" for call in exchange.calls))
        self.assertFalse(any(call[0] == "fetch_ticker" for call in exchange.calls))
        self.assertTrue(any(call[0] == "fetch_positions" for call in exchange.calls))

    def test_bitget_hedge_tier_uses_larger_direction_including_open_orders(self):
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
        client = CcxtExchangeClient(CcxtConfig(exchange_id="bitget"), exchange=exchange)
        order = client.create_order(
            "BTC/USDT:USDT.BITGET",
            side="buy",
            order_type="limit",
            amount_raw=5 * SCALE,
            amount_unit="contracts",
            price_raw=100 * SCALE,
            risk_leverage=10,
            risk_margin_mode="cross",
            params={"hedged": True},
        )
        self.assertEqual(order.order_id, "order-1")
        self.assertTrue(any(call[0] == "fetch_positions" for call in exchange.calls))
        self.assertTrue(any(call[0] == "fetch_open_orders" for call in exchange.calls))
        self.assertTrue(any(call[0] == "create_order" for call in exchange.calls))

    def test_contract_open_order_checks_tier_against_positions_and_open_orders(self):
        self.exchange.leverage_tier_rows = [
            {
                "tier": 1,
                "minNotional": "0",
                "maxNotional": "0.3",
                "initialMarginRate": "0.05",
                "maintenanceMarginRate": "0.005",
                "maxLeverage": 20,
            },
            {
                "tier": 2,
                "minNotional": "0.3",
                "maxNotional": "1000",
                "initialMarginRate": "0.2",
                "maintenanceMarginRate": "0.01",
                "maxLeverage": 5,
            },
        ]
        self.exchange.open_order_rows = [
            {
                "id": "open-1",
                "clientOrderId": "open-1",
                "symbol": "BTC/USDT:USDT",
                "status": "open",
                "side": "buy",
                "type": "limit",
                "amount": "2",
                "filled": "0",
                "remaining": "2",
                "price": "100",
            }
        ]
        with self.assertRaisesRegex(CcxtConnectorError, "最高杠杆 5x"):
            self.client.create_order(
                "BTC/USDT:USDT.BINANCE",
                side="buy",
                order_type="limit",
                amount_raw=2 * SCALE,
                price_raw=100 * SCALE,
                amount_unit="contracts",
                risk_leverage=10,
            )
        self.assertFalse(any(call[0] == "create_order" for call in self.exchange.calls))
        self.assertTrue(any(call[0] == "fetch_positions" for call in self.exchange.calls))
        self.assertTrue(any(call[0] == "fetch_open_orders" for call in self.exchange.calls))
        self.assertTrue(any(call[0] == "fetch_leverage_tiers" for call in self.exchange.calls))

    def test_reduce_only_contract_order_skips_opening_tier_preflight(self):
        order = self.client.create_order(
            "BTC/USDT:USDT.BINANCE",
            side="sell",
            order_type="market",
            amount_raw=SCALE,
            amount_unit="contracts",
            risk_leverage=10,
            risk_reduce_only=True,
        )
        self.assertEqual(order.order_id, "order-1")
        self.assertFalse(any(call[0] in {"fetch_positions", "fetch_open_orders", "fetch_leverage_tiers"} for call in self.exchange.calls))

    def test_order_precision_and_leverage_limits_fail_before_exchange_call(self):
        with self.assertRaises(CcxtConnectorError) as context:
            self.client.create_order(
                "BTCUSDT.BINANCE",
                side="buy",
                order_type="limit",
                amount_raw=1_000_050,
                price_raw=100_000_000_000,
            )
        self.assertEqual(context.exception.error_class, CcxtErrorClass.INVALID)
        with self.assertRaises(CcxtConnectorError):
            self.client.set_leverage("BTC/USDT:USDT.BINANCE", 51)

    def test_public_ccxt_pro_streams_have_stable_fixed_point_envelopes(self):
        ticker = asyncio.run(
            watch_stream(
                FakeProExchange(),
                "ticker",
                instrument="BTCUSDT.BINANCE",
            )
        )
        envelope = normalize_stream_event("ticker", ticker, exchange_id="binance", received_ts=9)
        self.assertEqual(envelope["stream"], "ticker")
        self.assertEqual(envelope["events"][0]["bid_raw"], 100_100_000_000)
        self.assertEqual(envelope["events"][0]["bid_qty_raw"], 2_000_000_000)
        self.assertEqual(envelope["events"][0]["ask_qty_raw"], 3_000_000_000)
        self.assertEqual(envelope["events"][0]["bid_qty_exact"], "2.0000000001")
        self.assertEqual(envelope["events"][0]["ask_qty_exact"], "3.0000000002")
        orders = asyncio.run(
            watch_stream(
                FakeProExchange(),
                "orders",
                instrument="BTCUSDT.BINANCE",
            )
        )
        order_envelope = normalize_stream_event("orders", orders, exchange_id="binance", received_ts=9)
        self.assertEqual(order_envelope["events"][0]["order_id"], "remote-1")
        self.assertEqual(order_envelope["events"][0]["filled_raw"], 2_000_000_000)


class CcxtMultiExchangeSandboxContractTest(unittest.TestCase):
    def test_same_public_ccxt_contract_covers_binance_okx_and_bybit(self):
        for exchange_id in ("binance", "okx", "bybit"):
            with self.subTest(exchange_id=exchange_id):
                client = CcxtExchangeClient(
                    CcxtConfig(exchange_id=exchange_id, sandbox=True),
                    exchange=FakeExchange(),
                )
                instrument = f"BTC/USDT.{exchange_id.upper()}"
                market = client.resolve_market(instrument)
                self.assertEqual(market.instrument, instrument)
                ticker = client.fetch_ticker(instrument)
                self.assertEqual(ticker.bid_raw, 100_100_000_000)
                self.assertTrue(client.exchange.sandbox_enabled)

    def test_sandbox_configuration_fails_if_exchange_cannot_enable_it(self):
        class ExchangeWithoutSandbox:
            set_sandbox_mode = None

        with self.assertRaises(CcxtConnectorError) as caught:
            CcxtExchangeClient(
                CcxtConfig(exchange_id="unknown", sandbox=True),
                exchange=ExchangeWithoutSandbox(),
            )
        self.assertEqual(caught.exception.error_class, CcxtErrorClass.UNSUPPORTED)


if __name__ == "__main__":
    unittest.main()
