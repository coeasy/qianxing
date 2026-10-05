import json
import hashlib
import io
import json
import os
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
from qianxing_strategy.frame import REQUEST, RESPONSE, encode_frame, read_frame
from qianxing_strategy.ring import RingEmpty, RingFull, SharedMemoryRing
from qianxing_strategy.columnar import decode_request
from qianxing_strategy.worker import PARENT_LIVENESS_PROBE_SECONDS, _load_handler


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
