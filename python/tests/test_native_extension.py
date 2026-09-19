from __future__ import annotations

import importlib.util
import json
import sys
import unittest
from pathlib import Path


def _load_native():
    root = Path(__file__).resolve().parents[2]
    # 与 tools/build_python_wheel.* 一致：先找 release，再退回 debug 产物。
    candidates = []
    for profile in ("release", "debug"):
        target = root / "target" / profile
        candidates += [
            (target, path)
            for pattern in ("_qianxing_native*.pyd", "_qianxing_native*.so")
            for path in target.glob(pattern)
        ]
    if not candidates:
        return None
    target, _ = candidates[0]
    sys.path.insert(0, str(target))
    import _qianxing_native

    return _qianxing_native


class NativeExtensionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.native = _load_native()

    def test_native_digest_matches_pure_python_bridge(self):
        """Rust 扩展与纯 Python 回退必须给出同一个指纹，否则两条链路会静默漂移。"""
        if self.native is None:
            self.skipTest("build qx-python to run native extension test")
        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing_bridge import BarFrame, native

        frame = BarFrame(
            "BTCUSDT.BINANCE",
            "native",
            (1, 2, 3),
            (100, 101, 102),
            (102, 103, 104),
            (99, 100, 101),
            (101, 102, 103),
            (10, 11, 12),
        )
        payload = json.dumps(
            {
                "instrument": frame.instrument,
                "source": frame.source,
                "ts": list(frame.ts),
                "open_raw": list(frame.open_raw),
                "high_raw": list(frame.high_raw),
                "low_raw": list(frame.low_raw),
                "close_raw": list(frame.close_raw),
                "volume_raw": list(frame.volume_raw),
            },
            separators=(",", ":"),
            ensure_ascii=False,
        )
        self.assertTrue(native.available())
        self.assertEqual(native.frame_digest(payload), frame.digest())
        self.assertEqual(json.loads(native.frame_to_json(payload))["close_raw"], [101, 102, 103])

    @unittest.skipUnless(
        importlib.util.find_spec("pyarrow") is not None,
        "pyarrow is optional",
    )
    def test_native_arrow_protocol_and_release_boundary(self):
        if self.native is None:
            self.skipTest("build qx-python to run native extension test")
        payload = json.dumps(
            {
                "instrument": "BTCUSDT.BINANCE",
                "source": "native",
                "ts": [1, 2],
                "open_raw": [100, 101],
                "high_raw": [102, 103],
                "low_raw": [99, 100],
                "close_raw": [101, 102],
                "volume_raw": [10, 11],
            }
        )
        import pyarrow as pa

        wrapper = self.native.owned_arrow_array(payload, 0)
        array = pa.array(wrapper)
        self.assertEqual(array.to_pylist(), [1, 2])
        self.assertEqual(str(array.type), "uint64")
        self.assertGreater(self.native.frame_digest(payload), 0)

        sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
        from qianxing_bridge import BarFrame

        frame = BarFrame(
            "BTCUSDT.BINANCE",
            "native",
            (1, 2),
            (100, 101),
            (102, 103),
            (99, 100),
            (101, 102),
            (10, 11),
        )
        table = frame.to_pyarrow_native()
        self.assertEqual(table.column("ts").to_pylist(), [1, 2])
        self.assertEqual(str(table.schema.field("close_raw").type), "decimal128(38, 0)")
