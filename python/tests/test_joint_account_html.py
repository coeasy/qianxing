from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing.reports import (
    render_multi_account_joint_report_html,
    write_multi_account_joint_report_html,
)


def verified_report() -> dict[str, object]:
    return {
        "schema_version": 1,
        "run_id": "joint-<script>alert(1)</script>",
        "settlement_currency": "USDT",
        "spec_fingerprint": "spec-hash",
        "verification": {
            "verified": True,
            "run_id": "joint-run",
            "result_hash": "result-hash",
            "data_fingerprint": "data-hash",
            "checks": ["parent identity", "child event logs"],
            "mismatches": [],
        },
        "summary": {
            "accounts_total": 2,
            "markets_total": 3,
            "initial_cash_raw": "100000000000000000000",
            "final_equity_raw": "100500000000000000000",
            "return_bps": 50,
            "max_drawdown_bps": 25,
            "successful_fills": 2,
            "complete": True,
        },
        "equity": {
            "total": 3001,
            "truncated": True,
            "rows": [
                {"ts": 1, "equity_raw": "900719925474099312345"},
                {"ts": 2, "equity_raw": "900719925474099312346"},
            ],
        },
        "fills": {
            "total": 2,
            "truncated": False,
            "rows": [{"exchange_id": "okx", "fill_id": "fill-1"}],
        },
        "accounts": [
            {
                "exchange_id": "okx",
                "account_id": "primary",
                "run_id": "okx-run",
                "initial_cash_raw": "50000000000000000000",
                "final_equity_raw": "50250000000000000000",
                "return_bps": 50,
                "max_drawdown_bps": 25,
                "markets_total": 2,
                "strategies_total": 1,
                "successful_fills": 1,
                "fill_rows_total": 1,
                "fill_rows_truncated": False,
                "fill_rows": [{"fill_id": "fill-1", "note": "<img src=x onerror=alert(1)>"}],
                "event_rows_total": 2,
                "event_rows_offset": 0,
                "event_rows_limit": 50,
                "event_rows_truncated": False,
                "event_rows_previous_offset": None,
                "event_rows_next_offset": None,
                "event_rows": [{"event_id": "event-1", "kind": "FillApplied"}],
            },
            {
                "exchange_id": "bitget",
                "account_id": "primary",
                "run_id": "bitget-run",
                "initial_cash_raw": "50000000000000000000",
                "final_equity_raw": "50250000000000000000",
                "return_bps": 50,
                "max_drawdown_bps": 20,
                "markets_total": 1,
                "strategies_total": 1,
                "successful_fills": 1,
                "fill_rows_total": 1,
                "fill_rows_truncated": False,
                "fill_rows": [{"fill_id": "fill-2"}],
                "event_rows_total": 1,
                "event_rows_offset": 0,
                "event_rows_limit": 50,
                "event_rows_truncated": False,
                "event_rows_previous_offset": None,
                "event_rows_next_offset": None,
                "event_rows": [{"event_id": "event-2", "kind": "FillApplied"}],
            },
        ],
    }


class JointAccountHtmlTest(unittest.TestCase):
    def test_render_uses_verified_reader_and_emits_safe_interactive_page(self) -> None:
        outcome = {"run_id": "joint-run"}
        pages = {"okx": {"primary": {"offset": 1, "limit": 10}}}
        with patch(
            "qianxing.reports.joint_account_html.read_multi_account_joint_report_v1",
            return_value=verified_report(),
        ) as reader:
            page = render_multi_account_joint_report_html(
                outcome,
                joint_account_event_pages=pages,
            )

        reader.assert_called_once_with(outcome, joint_account_event_pages=pages)
        self.assertIn("equityChart", page)
        self.assertIn("accountFilter", page)
        self.assertIn("exportAccounts", page)
        self.assertIn("exportFills", page)
        self.assertIn("BigInt(p.equity_raw)", page)
        self.assertIn("900719925474099312345", page)
        self.assertIn("EventLog", page)
        self.assertIn("各交易所账户拥有独立事件序列", page)
        self.assertIn("\u003cscript\u003e", page)
        self.assertNotIn("<script>alert(1)</script>", page)
        self.assertNotIn("<img src=x onerror=alert(1)>", page)

    def test_rejects_unverified_or_oversized_facts(self) -> None:
        report = verified_report()
        report["verification"]["verified"] = False  # type: ignore[index]
        with patch(
            "qianxing.reports.joint_account_html.read_multi_account_joint_report_v1",
            return_value=report,
        ):
            with self.assertRaisesRegex(ValueError, "Rust-verified"):
                render_multi_account_joint_report_html({"run_id": "joint-run"})

        report = verified_report()
        report["fills"]["rows"] = [{}] * 501  # type: ignore[index]
        with patch(
            "qianxing.reports.joint_account_html.read_multi_account_joint_report_v1",
            return_value=report,
        ):
            with self.assertRaisesRegex(ValueError, "fill rows"):
                render_multi_account_joint_report_html({"run_id": "joint-run"})

        with (
            patch(
                "qianxing.reports.joint_account_html.read_multi_account_joint_report_v1",
                return_value=verified_report(),
            ),
            patch("qianxing.reports.joint_account_html.MAX_INLINE_REPORT_BYTES", 128),
        ):
            with self.assertRaisesRegex(ValueError, "HTML payload bound"):
                render_multi_account_joint_report_html({"run_id": "joint-run"})

    def test_writer_uses_default_path_and_protects_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            artifact = root / "joint-outcome.json"
            outcome = {
                "artifact_path": str(artifact),
                "artifact_locations": {"equity": str(root / "equity.json")},
            }
            with patch(
                "qianxing.reports.joint_account_html.read_multi_account_joint_report_v1",
                return_value=verified_report(),
            ):
                result = write_multi_account_joint_report_html(outcome)
            self.assertEqual(result, root / "joint-outcome.report.html")
            self.assertIn("jointReportData", result.read_text(encoding="utf-8"))
            self.assertEqual(list(root.glob("*.tmp")), [])

            with patch(
                "qianxing.reports.joint_account_html.read_multi_account_joint_report_v1"
            ) as reader:
                with self.assertRaisesRegex(ValueError, "overwrite"):
                    write_multi_account_joint_report_html(outcome, artifact)
            reader.assert_not_called()


if __name__ == "__main__":
    unittest.main()
