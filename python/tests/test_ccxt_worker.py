import json
import sys
import tempfile
import unittest
from io import StringIO
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing_ccxt import CcxtConfig, CcxtConnectorError, CcxtErrorClass, CcxtExchangeClient
from qianxing_ccxt.worker import CcxtJsonWorker, _config_from_file, serve
from test_ccxt import FakeExchange, FakeProExchange


class CcxtWorkerTest(unittest.TestCase):
    def test_config_file_allows_public_exchange_without_credentials(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "public.json"
            path.write_text(
                json.dumps(
                    {
                        "exchange_id": "binance",
                        "sandbox": True,
                        "credential_env": None,
                    }
                ),
                encoding="utf-8",
            )
            config = _config_from_file(path)
            self.assertEqual(config.exchange_id, "binance")
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


if __name__ == "__main__":
    unittest.main()
