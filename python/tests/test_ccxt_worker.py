import asyncio
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


if __name__ == "__main__":
    unittest.main()
