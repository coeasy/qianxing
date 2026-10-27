import sys
import unittest
from pathlib import Path
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing import AppClient


class AccountReadClientTest(unittest.TestCase):
    def setUp(self):
        self.client = AppClient("http://127.0.0.1:18082")

    def test_positions_are_scoped_to_account_and_venue(self):
        with patch.object(
            self.client, "_request_json_value", return_value=[{"symbol": "BTC/USDT"}]
        ) as request:
            rows = self.client.get_account_positions(
                account_id="account 1", venue_id="binance"
            )

        self.assertEqual(rows, [{"symbol": "BTC/USDT"}])
        path = request.call_args.args[0]
        self.assertEqual(urlsplit(path).path, "/account/positions")
        self.assertEqual(
            parse_qs(urlsplit(path).query),
            {"account_id": ["account 1"], "venue_id": ["binance"]},
        )
        self.assertEqual(request.call_args.kwargs["method"], "GET")

    def test_partial_scope_is_rejected_before_http(self):
        with patch.object(self.client, "_request_json_value") as request:
            with self.assertRaisesRegex(ValueError, "supplied together"):
                self.client.get_account_orders(account_id="account-1")
        request.assert_not_called()

    def test_balances_keep_the_documented_summary_shape(self):
        summary = {"cash_raw": {"USDT": "1000"}, "equity_raw": None}
        with patch.object(self.client, "_request_json", return_value=summary) as request:
            result = self.client.get_account_balances(
                account_id="account-1", venue_id="okx"
            )
        self.assertEqual(result, summary)
        self.assertIn("/account/balances?account_id=account-1&venue_id=okx", request.call_args.args[0])

    def test_portfolio_query_preserves_requested_reporting_currency(self):
        with patch.object(
            self.client, "_request_json", return_value={"portfolio_id": "demo"}
        ) as request:
            result = self.client.get_account_portfolio(
                "demo portfolio", reporting_currency="USDT"
            )

        self.assertEqual(result["portfolio_id"], "demo")
        path = request.call_args.args[0]
        self.assertEqual(urlsplit(path).path, "/account/portfolio")
        self.assertEqual(
            parse_qs(urlsplit(path).query),
            {"portfolio_id": ["demo portfolio"], "reporting_currency": ["USDT"]},
        )

    def test_activity_report_requires_scope_and_encodes_one_cursor_page(self):
        report = {"schema_version": 1, "events": {"rows": []}}
        with patch.object(self.client, "_request_json", return_value=report) as request:
            result = self.client.get_account_activity(
                account_id="account 1",
                venue_id="binance/spot",
                after=42,
                limit=100,
            )

        self.assertEqual(result, report)
        path = request.call_args.args[0]
        self.assertEqual(urlsplit(path).path, "/account/activity")
        self.assertEqual(
            parse_qs(urlsplit(path).query),
            {
                "account_id": ["account 1"],
                "venue_id": ["binance/spot"],
                "after": ["42"],
                "limit": ["100"],
            },
        )
        self.assertEqual(request.call_args.kwargs["method"], "GET")

    def test_activity_report_rejects_invalid_scope_cursor_and_limit(self):
        invalid_queries = (
            {"account_id": "account-1", "venue_id": ""},
            {"account_id": "account-1", "venue_id": "okx", "after": -1},
            {"account_id": "account-1", "venue_id": "okx", "after": True},
            {"account_id": "account-1", "venue_id": "okx", "after": 1, "before": 2},
            {"account_id": "account-1", "venue_id": "okx", "limit": 301},
        )
        with patch.object(self.client, "_request_json") as request:
            for query in invalid_queries:
                with self.subTest(query=query), self.assertRaises(ValueError):
                    self.client.get_account_activity(**query)
        request.assert_not_called()

    def test_account_list_rejects_unexpected_response_shape(self):
        with patch.object(self.client, "_request_json_value", return_value={"rows": []}):
            with self.assertRaisesRegex(RuntimeError, "invalid positions read model"):
                self.client.get_account_positions()


if __name__ == "__main__":
    unittest.main()
