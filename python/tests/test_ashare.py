import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing_ashare import (  # noqa: E402
    AkShareProvider,
    AshareActionManifest,
    AshareActionQuery,
    AshareManifest,
    AsharePITRecord,
    AshareQuery,
    AshareTradingCalendar,
    AshareCorporateAction,
    BarFrame,
    build_dataset_bundle_manifest,
    normalize_bar_rows,
    normalize_corporate_action_rows,
    normalize_instrument,
    reconcile_corporate_actions,
    screen_bar_frames,
    SCALE,
)


class _FakeAkShare:
    __version__ = "test"

    @staticmethod
    def stock_zh_a_hist(**kwargs):
        assert kwargs["symbol"] == "000001"
        return [
            {"日期": "2024-01-03", "开盘": "10", "最高": "11", "最低": "9.5", "收盘": "10.5", "成交量": "100"},
            {"日期": "2024-01-02", "开盘": "9", "最高": "10", "最低": "8.5", "收盘": "9.5", "成交量": "90"},
            # 同一时间重复返回时，保留最后一条，避免回测出现重复 bar。
            {"日期": "2024-01-03", "开盘": "10.1", "最高": "11.1", "最低": "9.6", "收盘": "10.6", "成交量": "101"},
        ]

    @staticmethod
    def stock_dividend_cninfo(**kwargs):
        assert kwargs["symbol"] == "000001"
        return [
            {
                "除权除息日": "2024-06-03",
                "公告日期": "2024-05-20",
                "股权登记日": "2024-05-31",
                "派息": "0.10",
                "送股": "0",
            }
        ]


class AshareTest(unittest.TestCase):
    def test_instrument_normalization(self):
        self.assertEqual(normalize_instrument("sh.600000"), "600000.SSE")
        self.assertEqual(normalize_instrument("000001"), "000001.SZSE")
        self.assertEqual(normalize_instrument("430047.BJSE"), "430047.BJSE")

    def test_normalize_rows_sorts_deduplicates_and_scales(self):
        frame = normalize_bar_rows(
            [
                {"date": "2024-01-02", "open": 1, "high": 2, "low": 0.9, "close": 1.5, "volume": 10},
                {"date": "2024-01-01", "open": 1, "high": 2, "low": 0.9, "close": 1.2, "volume": 8},
                {"date": "2024-01-02", "open": 1, "high": 2, "low": 0.9, "close": 1.6, "volume": 11},
            ],
            code="sz.000001",
            source="test",
        )
        self.assertEqual(frame.instrument, "000001.SZSE")
        self.assertEqual(len(frame.ts), 2)
        self.assertEqual(frame.close_raw[-1], 1_600_000_000)
        self.assertLess(frame.ts[0], frame.ts[1])

    def test_intraday_date_and_time_are_not_collapsed(self):
        frame = normalize_bar_rows(
            [
                {"date": "2024-01-02", "time": "09:30:00", "open": 1, "high": 1, "low": 1, "close": 1, "volume": 1},
                {"date": "2024-01-02", "time": "09:35:00", "open": 1, "high": 1, "low": 1, "close": 1, "volume": 1},
            ],
            code="600000", source="test", frequency="5m",
        )
        self.assertEqual(len(frame.ts), 2)
        self.assertGreater(frame.ts[1] - frame.ts[0], 0)

    def test_provider_and_manifest_are_offline_testable(self):
        query = AshareQuery("000001.SZSE", "20240101", "20240131", adjustment="qfq")
        frame, manifest = AkShareProvider(_FakeAkShare()).fetch(query)
        self.assertEqual(frame.instrument, "000001.SZSE")
        self.assertEqual(manifest.provider, "akshare")
        restored = AshareManifest.from_json(manifest.to_json())
        self.assertEqual(restored.source_hash, manifest.source_hash)

    def test_corporate_action_normalization_and_akshare_provider(self):
        actions, manifest = AkShareProvider(_FakeAkShare()).fetch_corporate_actions(
            AshareActionQuery("000001", "20240101", "20241231", "2024-05-31T23:59:59+08:00")
        )
        self.assertEqual(len(actions), 1)
        self.assertEqual(actions[0].action_type, "cash_dividend")
        self.assertEqual(actions[0].cash_dividend_raw, 100_000_000)
        self.assertEqual(actions[0].record_date, "2024-05-31")
        self.assertIsInstance(AshareActionManifest.from_json(manifest.to_json()), AshareActionManifest)

    def test_corporate_action_pit_filter_does_not_leak_future_event(self):
        actions, _ = AkShareProvider(_FakeAkShare()).fetch_corporate_actions(
            AshareActionQuery("000001", "20240101", "20241231", "2024-05-19T23:59:59+08:00")
        )
        self.assertEqual(actions, ())

    def test_corporate_action_reconciliation_reports_conflicts(self):
        first = normalize_corporate_action_rows(
            [{"除权除息日": "2024-06-03", "公告日期": "2024-05-20", "派息": "0.10"}],
            code="000001", source="baostock",
        )
        second = normalize_corporate_action_rows(
            [{"除权除息日": "2024-06-03", "公告日期": "2024-05-21", "派息": "0.12"}],
            code="000001", source="akshare",
        )
        actions, conflicts = reconcile_corporate_actions(first, second)
        self.assertEqual(len(actions), 1)
        self.assertEqual(actions[0].source, "baostock+akshare")
        self.assertEqual(len(conflicts), 1)
        self.assertEqual(conflicts[0].field, "cash_dividend_raw")

    def test_complex_corporate_action_fields_are_normalized_for_rust_ledger(self):
        actions = normalize_corporate_action_rows(
            [
                {
                    "除权除息日": "2024-06-03",
                    "公告日期": "2024-05-20",
                    "action_type": "rights_issue",
                    "配股权代码": "000001.SZSE",
                    "配股价": "5",
                    "配股比例": "0.1",
                    "认购数量": "10",
                }
            ],
            code="000001",
            source="test",
        )
        action = actions[0]
        self.assertEqual(action.rights_instrument, "000001.SZSE")
        self.assertEqual(action.subscription_qty_raw, 10 * 1_000_000_000)
        restored = AshareCorporateAction.from_json(action.to_json())
        self.assertEqual(restored, action)

    def test_capital_change_requires_and_preserves_absolute_issuer_supply(self):
        action = normalize_corporate_action_rows(
            [
                {
                    "除权除息日": "2024-06-03",
                    "公告日期": "2024-05-20",
                    "action_type": "capital_change",
                    "issuer_total_shares_raw": "1000000000000",
                    "issuer_free_float_shares_raw": "700000000000",
                }
            ],
            code="000001",
            source="test",
        )[0]
        self.assertEqual(action.issuer_total_shares_raw, 1_000_000_000_000)
        self.assertEqual(action.issuer_free_float_shares_raw, 700_000_000_000)
        self.assertEqual(AshareCorporateAction.from_json(action.to_json()), action)

        with self.assertRaises(ValueError):
            normalize_corporate_action_rows(
                [
                    {
                        "除权除息日": "2024-06-03",
                        "公告日期": "2024-05-20",
                        "action_type": "capital_change",
                    }
                ],
                code="000001",
                source="test",
            )

        human_units = normalize_corporate_action_rows(
            [
                {
                    "除权除息日": "2024-06-03",
                    "公告日期": "2024-05-20",
                    "action_type": "除权除息",
                    "总股本": "1000",
                    "流通股本": "700",
                }
            ],
            code="000001",
            source="test",
        )[0]
        self.assertEqual(human_units.issuer_total_shares_raw, 1000 * SCALE)
        self.assertEqual(human_units.issuer_free_float_shares_raw, 700 * SCALE)

    def test_rights_expiry_is_a_separate_explicit_corporate_action(self):
        actions = normalize_corporate_action_rows(
            [
                {
                    "除权除息日": "2024-06-10",
                    "公告日期": "2024-05-20",
                    "action_type": "rights_issue_expiry",
                    "配股权代码": "700001.SZSE",
                    "配股失效数量": "10",
                }
            ],
            code="000001",
            source="test",
        )
        self.assertEqual(actions[0].action_type, "rights_issue_expiry")
        self.assertEqual(actions[0].rights_expiry_qty_raw, 10 * 1_000_000_000)

    def test_convertible_bond_lifecycle_fields_are_normalized_and_round_trip(self):
        actions = normalize_corporate_action_rows(
            [
                {
                    "除权除息日": "2024-06-10",
                    "公告日期": "2024-05-20",
                    "action_type": "convertible_bond_interest",
                    "可转债代码": "123001.SZSE",
                    "每张利息": "5",
                    "股权登记日": "2024-06-10",
                    "派息日": "2024-06-20",
                }
            ],
            code="000001",
            source="test",
        )
        interest = actions[0]
        self.assertEqual(interest.action_type, "convertible_bond_interest")
        self.assertEqual(interest.convertible_bond_instrument, "123001.SZSE")
        self.assertEqual(interest.interest_per_bond_raw, 5 * 1_000_000_000)
        self.assertEqual(AshareCorporateAction.from_json(interest.to_json()), interest)

        redemption = normalize_corporate_action_rows(
            [
                {
                    "除权除息日": "2024-06-30",
                    "公告日期": "2024-06-01",
                    "action_type": "可转债赎回",
                    "可转债代码": "123001.SZSE",
                    "赎回数量": "100",
                    "赎回价": "105",
                }
            ],
            code="000001",
            source="test",
        )[0]
        self.assertEqual(redemption.action_type, "convertible_bond_redemption")
        self.assertEqual(redemption.settlement_qty_raw, 100 * 1_000_000_000)
        self.assertEqual(redemption.settlement_price_raw, 105 * 1_000_000_000)

        call = normalize_corporate_action_rows(
            [
                {
                    "除权除息日": "2024-07-31",
                    "公告日期": "2024-07-01",
                    "action_type": "可转债强赎",
                    "可转债代码": "123001.SZSE",
                    "赎回价": "106",
                }
            ],
            code="000001",
            source="test",
        )[0]
        self.assertEqual(call.action_type, "convertible_bond_call")
        self.assertEqual(call.settlement_qty_raw, 0)
        self.assertEqual(call.settlement_price_raw, 106 * 1_000_000_000)
        self.assertEqual(AshareCorporateAction.from_json(call.to_json()), call)

    def test_screening_returns_ranked_candidates(self):
        first = normalize_bar_rows(
            [{"date": "2024-01-01", "open": 1, "high": 1, "low": 1, "close": 1, "volume": 10},
             {"date": "2024-01-02", "open": 1, "high": 1.2, "low": 1, "close": 1.2, "volume": 20}],
            code="000001", source="test",
        )
        second = normalize_bar_rows(
            [{"date": "2024-01-01", "open": 1, "high": 1, "low": 1, "close": 1, "volume": 10},
             {"date": "2024-01-02", "open": 1, "high": 1.1, "low": 1, "close": 1.1, "volume": 20}],
            code="600000", source="test",
        )
        rows = screen_bar_frames([first, second], min_return_bps=500, limit=1)
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["instrument"], "000001.SZSE")
        self.assertEqual(json.loads(first.to_json())["instrument"], "000001.SZSE")

    def test_calendar_corporate_action_and_pit_are_as_of_bounded(self):
        calendar = AshareTradingCalendar("sse-2024", ("2024-01-02", "2024-01-03"), (("09:30", "11:30"),))
        self.assertTrue(AshareTradingCalendar.from_json(calendar.to_json()).contains("2024-01-02"))
        action = AshareCorporateAction("000001", "2024-01-03", "2024-01-04T09:00:00+08:00", source="test")
        self.assertFalse(action.visible_at("2024-01-03T15:00:00+08:00"))
        self.assertTrue(action.visible_at("2024-01-04T09:00:00+08:00"))
        record = AsharePITRecord("000001", "2023-12-31", "2024-01-05T09:00:00+08:00", {"pe": 10})
        self.assertFalse(record.visible_at("2024-01-04T15:00:00+08:00"))

    def test_python_bundle_manifest_matches_rust_component_schema(self):
        bars = AshareManifest(
            schema_version=1,
            provider="akshare",
            provider_version="test",
            instrument="000001.SZSE",
            frequency="daily",
            adjustment="none",
            start="2024-01-02",
            end="2024-01-03",
            row_count=2,
            source_hash="0123456789abcdef",
            received_at="2024-01-04T09:00:00+08:00",
        )
        actions = AshareActionManifest(
            schema_version=1,
            provider="akshare",
            provider_version="test",
            instrument="000001.SZSE",
            start="2024-01-01",
            end="2024-12-31",
            row_count=1,
            source_hash="fedcba9876543210",
            received_at="2024-01-04T09:00:00+08:00",
        )
        calendar = AshareTradingCalendar(
            "cn-2024", ("2024-01-02", "2024-01-03"), (("09:30", "11:30"),)
        )
        frame = BarFrame(
            "000001.SZSE",
            "akshare",
            (1704159000000, 1704245400000),
            (1_000_000_000, 1_100_000_000),
            (1_200_000_000, 1_300_000_000),
            (900_000_000, 1_000_000_000),
            (1_100_000_000, 1_200_000_000),
            (100, 120),
        )
        bundle = build_dataset_bundle_manifest(
            "ashare.000001",
            "snapshot-v1",
            "akshare+calendar",
            bars,
            actions,
            calendar,
            frame,
        )
        self.assertEqual(bundle["components"]["bars"]["kind"], "bars")
        self.assertEqual(len(bundle["components"]["bars"]["fingerprint"]), 16)
        self.assertEqual(bundle["components"]["corporate_actions"]["row_count"], 1)
        self.assertEqual(bundle["components"]["calendar"]["version"], "cn-2024")
        self.assertLess(
            bundle["components"]["bars"]["start_timestamp"],
            bundle["components"]["bars"]["end_timestamp"],
        )


if __name__ == "__main__":
    unittest.main()
