import json
import hashlib
import io
import json
import os
import struct
import tempfile
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing_strategy import StrategyBars, StrategyInput, StrategyIntent, StrategyOutput
from qianxing_strategy.frame import REQUEST, RESPONSE, encode_frame, read_frame
from qianxing_strategy.ring import RingEmpty, RingFull, SharedMemoryRing
from qianxing_strategy.columnar import decode_request
from qianxing_strategy.worker import _load_handler


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

    def test_contract_rejects_future_or_malformed_bars(self):
        value = json.loads(self.request.to_json())
        value["bars"]["ts"] = [10, 9]
        with self.assertRaises(ValueError):
            StrategyInput.from_dict(value)

    def test_output_supports_multiple_order_intents(self):
        output = StrategyOutput(
            request_id="request-1",
            strategy_id="strategy-1",
            signal_id=2,
            instrument=self.request.instrument,
            target_qty=0,
            intents=(
                StrategyIntent(
                    intent_id=101,
                    instrument="BTCUSDT.BINANCE",
                    side="buy",
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
                    side="buy",
                    qty_raw=3,
                    margin_mode="isolated",
                    position_mode="hedge",
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
            (restored.intents[0].margin_mode, restored.intents[0].leverage),
            ("isolated", 10),
        )

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
