"""V13 R1-A2：Python 写侧与 Rust 读侧共用同一份 A 股公司行为对照夹具。

三份文件一条链：`*.rows.json`（数据源原始行）→ `*.payload.json`（Python 写出的 v1 信封）
→ `*.expectations.json`（两侧共读的期望值）。这里在进程内把后两份**重算并逐字节比对**，
Rust 侧在 `crates/qx-xingban/src/ashare/tests.rs` 里把同一份 payload 读回并比同一份期望值：
任何一侧改动口径或字段，就有一侧红 —— 与 V11 R17 那对日历指纹夹具同一条设计（谁都不抄它）。
两侧白名单与动作名册的静态对照归 `tools/check_architecture.py` 的
`ashare_cross_language_contract_check()`，这里只钉行为。
"""

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing_ashare import (  # noqa: E402
    ASHARE_SCHEMA_VERSION,
    SCALE,
    _CORPORATE_ACTION_TYPES,
    _canonical_action_type,
    normalize_corporate_action_rows,
    serialize_corporate_actions,
)  # noqa: E402

FIXTURES = Path(__file__).resolve().parent / "fixtures"
STEM = "ashare_actions_cross_check"


def _fixture_text(name: str) -> str:
    # 工作树可能是 CRLF，而比对的是写侧产出的那一串字节。
    return (FIXTURES / name).read_text(encoding="utf-8").replace("\r\n", "\n")


def _fixture_json(name: str) -> dict:
    return json.loads(_fixture_text(name))


def _trunc_div(numerator: int, denominator: int) -> int:
    """向零截断的整数除法：Rust 的 `/` 是这一种，Python 的 `//` 向负无穷。"""

    quotient = abs(numerator) // abs(denominator)
    return quotient if (numerator >= 0) == (denominator >= 0) else -quotient


class CrossLanguageActionContractTest(unittest.TestCase):
    def test_blessed_payload_is_what_the_python_writer_produces(self) -> None:
        """payload 夹具必须等于 Python 现在写出的字节，不是某人手抄的样本。"""

        source = _fixture_json(f"{STEM}.rows.json")
        actions = normalize_corporate_action_rows(
            source["rows"], code=source["code"], source=source["source"]
        )
        produced = serialize_corporate_actions(
            actions, source=source["source"], as_of=source["as_of"]
        )
        pinned = _fixture_text(f"{STEM}.payload.json")
        self.assertEqual(produced, pinned)
        self.assertIn('"schema_version":1', pinned)

    def test_expectations_are_derived_from_the_blessed_payload(self) -> None:
        """期望值必须能从 payload 复算：改写字段名册会立刻让这一格与 Rust 那侧分开红。"""

        payload = _fixture_json(f"{STEM}.payload.json")["actions"]
        expectations = _fixture_json(f"{STEM}.expectations.json")
        applied = [a for a in payload if a["action_type"] != "suspension"]
        halted = [a for a in payload if a["action_type"] == "suspension"]
        self.assertEqual(expectations["document"]["row_count"], len(payload))
        self.assertEqual(expectations["document"]["applied_actions"], len(applied))
        self.assertEqual(expectations["document"]["halted_days"], len(halted))
        self.assertEqual(expectations["document"]["schema_version"], ASHARE_SCHEMA_VERSION)
        self.assertEqual(expectations["halted_days"], [a["ex_date"] for a in halted])
        for row, event in zip(expectations["applied_actions"], applied):
            self.assertEqual(row, {key: event[key] for key in row})

    def test_anchor_reference_is_recomputed_from_the_payload_events(self) -> None:
        """锚定参考价是那条公式的第二份实现：Python 独立复算，改一格两侧同红。

        沪深口径 `(昨收 + 配股价×配股比例 − 每股现金红利) ÷ (1 + 送转比例 + 配股比例)`，
        再落到最小报价单位。红利那一项与 Rust 入账同源：`cash_dividend`/`bonus_share`/
        `capital_transfer`/`unknown` 四种动作按现金红利处理（Rust 的
        `is_cash_dividend_action` 是同一份名单）。
        """

        payload = _fixture_json(f"{STEM}.payload.json")["actions"]
        anchor = _fixture_json(f"{STEM}.expectations.json")["anchor"]
        events = [event for event in payload if event["ex_date"] == anchor["ex_date"]]
        self.assertTrue(events, "锚定日必须真有可折算的事件")
        cash_out = 0
        cash_in = 0
        share_factor = SCALE
        for event in events:
            if event["action_type"] in {"cash_dividend", "bonus_share", "capital_transfer", "unknown"}:
                cash_out += event["cash_dividend_raw"]
                num, den = event["share_ratio_num"], event["share_ratio_den"]
                if num != den:
                    share_factor += _trunc_div((num - den) * SCALE, den)
            elif event["action_type"] == "rights_issue" and event["rights_issue_ratio_num"] > 0:
                ratio = _trunc_div(event["rights_issue_ratio_num"] * SCALE, event["rights_issue_ratio_den"])
                share_factor += ratio
                cash_in += _trunc_div(event["rights_issue_price_raw"] * ratio, SCALE)
        adjusted = _trunc_div((anchor["previous_close_raw"] + cash_in - cash_out) * SCALE, share_factor)
        # 沪深 A 股最小报价单位 0.01 元，与 Rust 的 `default_price_tick` 同一格。
        tick = SCALE // 100
        self.assertEqual(
            _trunc_div(adjusted + tick // 2, tick) * tick,
            anchor["expected_reference_raw"],
        )

    def test_every_canonical_action_name_round_trips_through_the_row_reader(self) -> None:
        """线格式名必须能被写侧自己认回：认不回就退化 unknown，停牌/增发语义会丢。"""

        for name in sorted(_CORPORATE_ACTION_TYPES):
            with self.subTest(action_type=name):
                self.assertEqual(_canonical_action_type({"action_type": name}), name)

    def test_raw_named_input_columns_are_taken_as_already_scaled(self) -> None:
        """`*_raw` 输入名是已定点整数（Rust 读侧同一口径），不能再乘一次 SCALE。"""

        actions = normalize_corporate_action_rows(
            [
                {
                    "ex_date": "2024-06-03",
                    "published_at": "2024-05-28",
                    "action_type": "cash_dividend",
                    "cash_dividend_raw": 5 * 10**8,
                },
                {
                    "ex_date": "2024-06-05",
                    "published_at": "2024-05-30",
                    "action_type": "convertible_bond_interest",
                    "可转债代码": "123001.SZSE",
                    "interest_per_bond_raw": 8 * 10**8,
                },
                {
                    "ex_date": "2024-06-07",
                    "published_at": "2024-05-31",
                    "action_type": "convertible_bond_redemption",
                    "可转债代码": "123001.SZSE",
                    "settlement_qty_raw": 100 * 10**8,
                    "settlement_price_raw": 105 * 10**8,
                },
                {
                    "ex_date": "2024-06-09",
                    "published_at": "2024-06-01",
                    "action_type": "rights_issue",
                    "配股代码": "700001.SZSE",
                    "rights_issue_price_raw": 5 * 10**8,
                    "rights_issue_ratio_num": 2 * 10**8,
                    "rights_issue_ratio_den": 10**8,
                    "subscription_qty_raw": 10 * 10**8,
                },
            ],
            code="000001",
            source="test",
        )
        self.assertEqual([a.action_type for a in actions][:4], [
            "cash_dividend", "convertible_bond_interest", "convertible_bond_redemption", "rights_issue",
        ])
        self.assertEqual(actions[0].cash_dividend_raw, 5 * 10**8)
        self.assertEqual(actions[1].interest_per_bond_raw, 8 * 10**8)
        self.assertEqual(actions[2].settlement_qty_raw, 100 * 10**8)
        self.assertEqual(actions[2].settlement_price_raw, 105 * 10**8)
        # 此前 `rights_issue_price_raw` 不在输入别名里：已规范化的行读回时配股价丢成 0。
        self.assertEqual(actions[3].rights_issue_price_raw, 5 * 10**8)
        self.assertEqual(actions[3].subscription_qty_raw, 10 * 10**8)
        # 未带 `_raw` 的别名仍按元/股读数乘 SCALE，两种写法不能同时成立。
        plain = normalize_corporate_action_rows(
            [{"除权除息日": "2024-06-03", "公告日期": "2024-05-28", "派息": "0.5"}],
            code="000001",
            source="test",
        )[0]
        self.assertEqual(plain.cash_dividend_raw, 5 * 10**8)

    def test_iso_timestamp_announcement_date_is_not_redated_to_the_ex_date(self) -> None:
        """带时区的 ISO 时间戳要吃掉时间部分，否则公告日期回落成除权日。"""

        event = normalize_corporate_action_rows(
            [
                {
                    "除权除息日": "2024-06-03",
                    "公告日期": "2024-05-28T19:30:00+08:00",
                    "股权登记日": "2024-05-31T00:00:00+08:00",
                    "派息日": "2024-06-20T00:00:00+08:00",
                    "派息": "0.5",
                }
            ],
            code="000001",
            source="test",
        )[0]
        self.assertEqual(event.published_at, "2024-05-28T00:00:00+08:00")
        self.assertEqual(event.announcement_date, "2024-05-28")
        self.assertEqual(event.record_date, "2024-05-31")
        self.assertEqual(event.payment_date, "2024-06-20")


if __name__ == "__main__":
    unittest.main()
