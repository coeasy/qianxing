import unittest
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing_bridge import BarFrame, TransformManifest, load_account_snapshot


class BridgeTest(unittest.TestCase):
    def test_bar_frame_round_trip_preserves_raw_columns(self):
        frame = BarFrame("T.SIM", "bars-v1", (1, 2), (10, 11), (11, 12), (9, 10), (10, 11), (1, 2))
        restored = BarFrame.from_json(frame.to_json())
        self.assertEqual(restored, frame)
        self.assertEqual(frame.digest(), 0xF9D5_8B91_E72D_AE58)
        self.assertEqual(frame.select_time(2, 2).ts, (2,))
        self.assertEqual(frame.resample(2).volume_raw, (1, 2))
        selected, manifest = frame.select_time_with_manifest(2, 2)
        self.assertEqual(manifest.input_hash, frame.digest())
        self.assertEqual(manifest.output_hash, selected.digest())
        self.assertEqual(TransformManifest.from_json(manifest.to_json()), manifest)
        _, resample_manifest = frame.resample_with_manifest(2)
        self.assertEqual(resample_manifest.parameters["interval"], "2")
        with self.assertRaises(ValueError):
            frame.resample(0)

    def test_account_snapshot_schema_boundary(self):
        payload = {
            "protocol": "QIANXING_ACCOUNT",
            "schema_version": 1,
            "header": {},
            "cash_raw": {},
            "positions": {},
            "orders": {},
            "fills": {},
            "transfers": {},
            "reconcile": {},
        }
        self.assertEqual(load_account_snapshot(__import__("json").dumps(payload))["schema_version"], 1)


if __name__ == "__main__":
    unittest.main()
