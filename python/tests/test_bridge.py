import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing_bridge import BarFrame, TransformManifest, load_account_snapshot

#: Rust 写侧（`AccountSnapshot::to_json`）原样产出的那一份文档；两侧各钉一次，见
#: `crates/qx-protocol/tests/snapshot_single_source.rs`。
ACCOUNT_SNAPSHOT_SAMPLE = Path(__file__).resolve().parent / "fixtures" / "account-snapshot-v1.sample.json"


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
        self.assertEqual(load_account_snapshot(json.dumps(payload))["schema_version"], 1)
        payload.pop("positions")
        with self.assertRaisesRegex(ValueError, "missing snapshot fields"):
            load_account_snapshot(json.dumps(payload))

    def test_reader_consumes_the_document_the_writer_emits(self):
        # 此前这里喂的是手抄字典、四张键表全填 {}：写侧把键印成裸数字（`"orders":{77:...}`，
        # 那份产物根本不是 JSON）、枚举印数字码的那几周，一条都不会红（V11 R18）。
        snapshot = load_account_snapshot(ACCOUNT_SNAPSHOT_SAMPLE.read_text(encoding="utf-8"))
        self.assertEqual(snapshot["protocol"], "QIANXING_ACCOUNT")
        self.assertEqual(
            sorted(snapshot["positions"]),
            ["BTCUSDT.BINANCE"],
            "持仓键必须是 `标的代码.交易所` 字符串——写侧与 serde 读侧共用 instrument_key",
        )
        self.assertEqual(snapshot["positions"]["BTCUSDT.BINANCE"]["instrument"],
                         {"symbol": "BTCUSDT", "venue": "BINANCE"})
        for table, key in (("orders", "77"), ("fills", "9"), ("transfers", "3")):
            self.assertEqual(
                sorted(snapshot[table]),
                [key],
                f"{table} 的键必须是带引号的十进制字符串：裸数字不是合法 JSON",
            )
        self.assertEqual(snapshot["orders"]["77"]["side"], "Sell")
        self.assertEqual(snapshot["orders"]["77"]["status"], "Accepted")
        self.assertEqual(snapshot["header"]["state_hash"], 17770453242411702682)
        # 定点数走整数；没算过的钱格是 null 而不是 0。
        self.assertEqual(snapshot["cash_raw"]["USDT"], 1000)
        self.assertIsInstance(snapshot["header"]["state_hash"], int)
        self.assertIsNone(snapshot["available_raw"])
        self.assertIsNone(snapshot["positions"]["BTCUSDT.BINANCE"]["unrealized_pnl_raw"])


if __name__ == "__main__":
    unittest.main()
