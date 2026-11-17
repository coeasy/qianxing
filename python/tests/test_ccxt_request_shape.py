"""Pinned CCXT request-shape checks; all exchange methods are pure local builders here."""

from __future__ import annotations

import unittest

try:
    import ccxt
except ImportError:  # optional extra
    ccxt = None

from qianxing_ccxt import CcxtExchangeClient, SUPPORTED_CCXT_VERSION


class CcxtRequestShapeContractTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if ccxt is None:
            raise unittest.SkipTest("install qianxing[ccxt] to run pinned CCXT request-shape checks")
        if getattr(ccxt, "__version__", None) != SUPPORTED_CCXT_VERSION:
            raise unittest.SkipTest(
                f"request-shape checks require CCXT {SUPPORTED_CCXT_VERSION}"
            )
        if CcxtExchangeClient._load_ccxt().__version__ != SUPPORTED_CCXT_VERSION:
            raise AssertionError("runtime version gate rejected the pinned CCXT package")

    def make_exchange(self, exchange_type, *, spot=False, uta=False, tick_size=0.00000001):
        symbol = "MEME/USDT" if spot else "MEME/USDT:USDT"
        market_id = "MEMEUSDT" if spot or exchange_type is not ccxt.okx else "MEME-USDT-SWAP"
        exchange = exchange_type(
            {
                "apiKey": "",
                "secret": "",
                "password": "",
                "options": {"defaultType": "spot" if spot else "swap", "uta": uta},
            }
        )
        market = {
            "id": market_id,
            "symbol": symbol,
            "base": "MEME",
            "quote": "USDT",
            "settle": None if spot else "USDT",
            "baseId": "MEME",
            "quoteId": "USDT",
            "settleId": None if spot else "USDT",
            "type": "spot" if spot else "swap",
            "spot": spot,
            "margin": False,
            "swap": not spot,
            "future": False,
            "option": False,
            "active": True,
            "contract": not spot,
            "linear": not spot,
            "inverse": False,
            "contractSize": 1.0,
            "precision": {"amount": tick_size, "price": tick_size, "cost": tick_size},
            "limits": {
                "amount": {"min": tick_size, "max": 1_000_000_000},
                "price": {"min": tick_size, "max": 1_000_000_000},
                "cost": {"min": 0, "max": 1_000_000_000},
            },
            "info": {"orderTypes": ["LIMIT", "MARKET"]},
        }
        exchange.markets = {symbol: market}
        exchange.markets_by_id = {market_id: [market]}
        exchange.markets_by_id_by_type = {}
        return exchange, symbol

    def test_eight_decimal_small_coin_values_remain_exact_in_ccxt_formatters(self):
        for exchange_type in (ccxt.binance, ccxt.binanceusdm, ccxt.okx, ccxt.bitget):
            spot = exchange_type is ccxt.binance
            exchange, symbol = self.make_exchange(exchange_type, spot=spot)
            with self.subTest(exchange=exchange.id, market=spot):
                self.assertEqual(
                    exchange.amount_to_precision(symbol, "123456.78901234"),
                    "123456.78901234",
                )
                self.assertEqual(
                    exchange.price_to_precision(symbol, "0.00001234"),
                    "0.00001234",
                )

    def test_nine_decimal_small_coin_values_remain_exact_across_venues(self):
        for exchange_type in (ccxt.binance, ccxt.binanceusdm, ccxt.okx, ccxt.bitget):
            spot = exchange_type is ccxt.binance
            exchange, symbol = self.make_exchange(
                exchange_type, spot=spot, tick_size=0.000000001
            )
            with self.subTest(exchange=exchange.id, market=spot):
                self.assertEqual(
                    exchange.amount_to_precision(symbol, "12.345678901"),
                    "12.345678901",
                )
                self.assertEqual(
                    exchange.price_to_precision(symbol, "0.000000001"),
                    "0.000000001",
                )

    def test_okx_spot_market_buy_request_uses_base_currency_amount(self):
        exchange, symbol = self.make_exchange(
            ccxt.okx, spot=True, tick_size=0.000000001
        )
        request = exchange.create_order_request(
            symbol,
            "market",
            "buy",
            "1.234567891",
            None,
            {"tgtCcy": "base_ccy", "clientOrderId": "spot-buy-probe"},
        )
        self.assertEqual(request["tdMode"], "cash")
        self.assertEqual(request["tgtCcy"], "base_ccy")
        self.assertEqual(request["sz"], "1.234567891")

    def test_bitget_spot_market_buy_request_uses_exact_quote_budget(self):
        exchange, symbol = self.make_exchange(
            ccxt.bitget, spot=True, tick_size=0.00000001
        )
        request = exchange.create_order_request(
            symbol,
            "market",
            "buy",
            "12.34000001",
            None,
            {
                "createMarketBuyOrderRequiresPrice": False,
                "clientOrderId": "qx-quote-budget",
            },
        )
        self.assertEqual(request["side"], "buy")
        self.assertEqual(request["size"], "12.34000001")
        self.assertEqual(request["clientOid"], "qx-quote-budget")

    def test_binance_usdm_hedge_close_long_uses_long_position_side_without_reduce_only(self):
        exchange, symbol = self.make_exchange(ccxt.binanceusdm)
        request = exchange.create_order_request(
            symbol,
            "limit",
            "sell",
            "12.34567891",
            "0.00001234",
            {"hedged": True, "reduceOnly": True, "clientOrderId": "qx-contract"},
        )
        self.assertEqual(request["positionSide"], "LONG")
        self.assertEqual(request["side"], "SELL")
        self.assertEqual(request["quantity"], "12.34567891")
        self.assertEqual(request["price"], "0.00001234")
        self.assertNotIn("reduceOnly", request)

    def test_okx_hedge_close_long_uses_posside_and_explicit_margin_mode(self):
        exchange, symbol = self.make_exchange(ccxt.okx)
        request = exchange.create_order_request(
            symbol,
            "limit",
            "sell",
            "12.34567891",
            "0.00001234",
            {
                "hedged": True,
                "reduceOnly": True,
                "marginMode": "isolated",
                "clientOrderId": "qx-contract",
            },
        )
        self.assertEqual(request["posSide"], "long")
        self.assertEqual(request["side"], "sell")
        self.assertEqual(request["tdMode"], "isolated")
        self.assertEqual(request["sz"], "12.34567891")
        self.assertNotIn("reduceOnly", request)

    def test_bitget_classic_hedge_close_long_uses_close_leg_shape(self):
        exchange, symbol = self.make_exchange(ccxt.bitget)
        request = exchange.create_order_request(
            symbol,
            "limit",
            "sell",
            "12.34567891",
            "0.00001234",
            {"hedged": True, "reduceOnly": True, "marginMode": "isolated"},
        )
        self.assertEqual(request["side"], "buy")
        self.assertEqual(request["tradeSide"], "Close")
        self.assertEqual(request["marginMode"], "isolated")
        self.assertEqual(request["size"], "12.34567891")
        self.assertNotIn("reduceOnly", request)

    def test_bitget_uta_hedge_close_long_uses_uta_posside_shape(self):
        exchange, symbol = self.make_exchange(ccxt.bitget, uta=True)
        request = exchange.create_uta_order_request(
            symbol,
            "limit",
            "sell",
            "12.34567891",
            "0.00001234",
            {"uta": True, "hedged": True, "reduceOnly": True},
        )
        self.assertEqual(request["category"], "USDT-FUTURES")
        self.assertEqual(request["posSide"], "long")
        self.assertEqual(request["side"], "sell")
        self.assertEqual(request["qty"], "12.34567891")
        self.assertNotIn("reduceOnly", request)
        self.assertNotIn("marginMode", request)


if __name__ == "__main__":
    unittest.main()
