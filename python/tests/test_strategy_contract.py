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

    def test_written_keys_stay_inside_the_declared_contract(self):
        # 契约文件住在仓库里，读侧按 serde 字段名解码：这里把写侧真正交出去的键集与契约声明的
        # 键集/必填集各比一次（V11 R4-4）。拼错可选键那一半今天两侧各自拒绝——Rust 的
        # deny_unknown_fields 与 Python 的 _reject_unknown_keys（V12 R4-k）。
        schema_path = Path(__file__).resolve().parents[2] / "schemas" / "strategy_api_v1.schema.json"
        schema = json.loads(schema_path.read_text(encoding="utf-8"))
        output = StrategyOutput(
            request_id="request-1",
            strategy_id="strategy-1",
            signal_id=3,
            instrument=self.request.instrument,
            target_qty=0,
            intents=(StrategyIntent(intent_id=11, instrument="BTCUSDT.BINANCE", side="buy", qty_raw=1),),
        ).to_dict(self.request)
        self.assertLessEqual(set(schema["required"]), set(output))
        self.assertLessEqual(set(output), set(schema["properties"]))
        intent = output["intents"][0]
        # intent 的形状住在 $defs/intent（V12 R4-k 把它抽成了 $ref），比对先指到那一格。
        declared = schema["$defs"]["intent"]
        self.assertLessEqual(set(declared["required"]), set(intent))
        self.assertLessEqual(set(intent), set(declared["properties"]))

    def test_required_keys_are_refused_when_absent_by_either_reader(self):
        # 上一颗只问写侧交不交出那些键，这一颗问读侧缺了会不会拒：读侧一旦 `.get(key, 默认)`，
        # 同一份契约就有了第二个更宽的读法——作者本地过得去的载荷会在 Rust 运行时被拒（V12 W10）。
        schema = json.loads(
            (Path(__file__).resolve().parents[2] / "schemas" / "strategy_api_v1.schema.json").read_text(
                encoding="utf-8"
            )
        )
        output = StrategyOutput(
            request_id="request-1",
            strategy_id="strategy-1",
            signal_id=4,
            instrument=self.request.instrument,
            target_qty=1,
            intents=(StrategyIntent(intent_id=12, instrument="BTCUSDT.BINANCE", side="buy", qty_raw=1),),
        ).to_dict(self.request)
        for key in schema["required"]:
            payload = dict(output)
            del payload[key]
            with self.subTest(output_key=key), self.assertRaises(KeyError):
                StrategyOutput.from_dict(payload, self.request)
        for key in schema["$defs"]["intent"]["required"]:
            payload = dict(output)
            payload["intents"] = [{name: value for name, value in output["intents"][0].items() if name != key}]
            with self.subTest(intent_key=key), self.assertRaises(KeyError):
                StrategyOutput.from_dict(payload, self.request)

    def test_optional_keys_may_be_absent_for_either_reader(self):
        # 上一颗问的是"缺了必填会不会拒"，这一颗问反方向："缺了可省的那一组会不会也拒"。
        # 读侧对可省键写成 value["key"] 就是给契约留了个更窄的读法：只填必填的第三方载荷、
        # 或 Rust 侧省略 null 字段发出来的意图都会在 Python 这里当场 KeyError（V12 W10 反向那一半）。
        schema = json.loads(
            (Path(__file__).resolve().parents[2] / "schemas" / "strategy_api_v1.schema.json").read_text(
                encoding="utf-8"
            )
        )
        output_required = set(schema["required"])
        intent_required = set(schema["$defs"]["intent"]["required"])
        output = StrategyOutput(
            request_id="request-1",
            strategy_id="strategy-1",
            signal_id=5,
            instrument=self.request.instrument,
            target_qty=1,
            intents=(StrategyIntent(intent_id=13, instrument="BTCUSDT.BINANCE", side="buy", qty_raw=1),),
        ).to_dict(self.request)
        minimal = {key: value for key, value in output.items() if key in output_required}
        # intents 本身是契约里的可省键，但意图行的必填要留着，否则这一颗量不到读侧的窄读法。
        minimal["intents"] = [
            {key: value for key, value in item.items() if key in intent_required} for item in output["intents"]
        ]
        # 输出层可省的那一组若不止 intents，这一颗就没把新增的可省键故意落下——先把形状钉住再往下走。
        self.assertEqual(
            set(output) - output_required,
            {"intents"},
            "输出层可省的键变了，这一颗的覆盖面要跟着改",
        )
        for key in set(output["intents"][0]) - intent_required:
            self.assertNotIn(key, minimal["intents"][0], f"{key} 是意图行的可省键，这一份载荷故意不带它")

        restored = StrategyOutput.from_dict(minimal, self.request)
        self.assertEqual(restored.strategy_id, "strategy-1")
        self.assertEqual(len(restored.intents), 1)
        self.assertEqual(restored.intents[0].intent_id, 13)
        self.assertIsNone(restored.intents[0].margin_mode)

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
        # 与 Rust StrategyContractOutput::validate_for 的意图那一半同口径（Rust 侧没有单独的
        # intent 级 validate，那三条词的判据就住在这一个函数里）：档位取值封闭、
        # leverage 不得为 0。
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
