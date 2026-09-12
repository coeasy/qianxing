from __future__ import annotations

import importlib.util
import json
import sys
import unittest
from pathlib import Path


def _load_native():
    root = Path(__file__).resolve().parents[2]
    target = root / "target" / "release"
    candidates = list(target.glob("_qianxing_native*.pyd")) + list(
        target.glob("_qianxing_native*.so")
    )
    if not candidates:
        return None
    sys.path.insert(0, str(target))
    import _qianxing_native

    return _qianxing_native


class NativeExtensionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.native = _load_native()

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
