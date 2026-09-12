import asyncio
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing_ccxt import (
    CcxtConfig,
    CcxtConnectorError,
    CcxtErrorClass,
    CcxtExchangeClient,
    normalize_stream_event,
    watch_stream,
    watch_ohlcv,
)


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
            }
        }
        self.calls = []

    def load_markets(self, reload=False):
        self.calls.append(("load_markets", reload))
        return self.markets

    def fetch_ohlcv(self, symbol, timeframe, since, limit, params):
        self.calls.append(("fetch_ohlcv", symbol, timeframe, since, limit, params))
        if since == 1000:
            return [[1000, "100", "101", "99", "100.5", "2"], [1100, 101, 102, 100, 101, 3]]
        return [[1200, 102, 103, 101, 102, 4]]

    def fetch_ticker(self, symbol, params):
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
            "bidVolume": "2",
            "askVolume": "3",
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
    def setUp(self):
        self.exchange = FakeExchange()
        self.client = CcxtExchangeClient(
            CcxtConfig(exchange_id="binance"), exchange=self.exchange
        )

    def test_spot_market_allows_explicit_null_contract_size(self):
        self.exchange.markets["BTC/USDT"]["contractSize"] = None
        market = self.client.resolve_market("BTC/USDT.BINANCE")
        self.assertEqual(market.contract_size_raw, 1_000_000_000)

    def test_symbol_mapping_and_paginated_ohlcv_preserve_fixed_point_columns(self):
        frame = self.client.fetch_ohlcv(
            "BTCUSDT.BINANCE", start_ms=1000, end_ms=1200, limit=2
        )
        self.assertEqual(frame.ts, (1000, 1100, 1200))
        self.assertEqual(frame.open_raw[0], 100_000_000_000)
        self.assertEqual(frame.close_raw[0], 100_500_000_000)
        self.assertEqual(frame.source, "ccxt:binance:BTC/USDT:1m")

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
        self.assertIn(("fetch_order_book", "BTC/USDT:USDT", 5, {}), exchange.calls)

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
        self.assertEqual(tiers[0].initial_margin_bps, 1_000)
        self.assertEqual(tiers[1].maintenance_margin_bps, 100)

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


if __name__ == "__main__":
    unittest.main()
