"""Remote discovery must expose the same metadata shape as local Rust discovery."""

from __future__ import annotations

import asyncio
import unittest
from unittest.mock import patch

from qianxing.app import (
    APP_ROUTE_TEMPLATES,
    AppClient,
    BuiltinStrategyInfo,
    QianxingError,
)


class RemoteStrategyCatalogTest(unittest.TestCase):
    def test_application_capabilities_validate_the_versioned_server_contract(self) -> None:
        payload = {
            "schema_version": 1,
            "execution_backend": "shared-rust",
            "runtime_preflight_required": True,
            "capabilities": [{"id": "backtest.bar", "routes": ["/app/backtest"]}],
            "limitations": ["runtime checks apply"],
        }
        client = AppClient("https://localhost")
        with patch.object(client, "_request_json_value", return_value=payload) as request:
            self.assertEqual(client.application_capabilities(), payload)
        self.assertEqual(
            request.call_args.args[0], APP_ROUTE_TEMPLATES["application_capabilities"]
        )
        self.assertEqual(request.call_args.kwargs["method"], "GET")

    def test_application_capabilities_reject_malformed_contract(self) -> None:
        client = AppClient("https://localhost")
        with (
            patch.object(client, "_request_json_value", return_value={"capabilities": []}),
            self.assertRaisesRegex(QianxingError, "invalid application capability catalog"),
        ):
            client.application_capabilities()

    def test_async_application_capabilities_delegates_to_sync_contract(self) -> None:
        payload = {
            "schema_version": 1,
            "execution_backend": "shared-rust",
            "runtime_preflight_required": True,
            "capabilities": [],
            "limitations": [],
        }
        client = AppClient("https://localhost")
        with patch.object(client, "application_capabilities", return_value=payload) as call:
            self.assertEqual(asyncio.run(client.async_application_capabilities()), payload)
        call.assert_called_once_with()

    def test_product_capability_query_uses_the_shared_stable_id_route(self) -> None:
        result = {"schema_version": 1, "capability": {"id": "qx.venue.binance_spot"}}
        client = AppClient("https://localhost")
        with patch.object(client, "_request_json", return_value=result) as request:
            self.assertEqual(client.query_product_capability("qx.venue.binance_spot"), result)
        self.assertEqual(
            request.call_args.args[0], APP_ROUTE_TEMPLATES["product_capability_query"]
        )
        self.assertEqual(request.call_args.kwargs["method"], "POST")
        self.assertEqual(
            request.call_args.kwargs["payload"],
            {"capability_id": "qx.venue.binance_spot"},
        )

    def test_product_capability_query_accepts_exact_execution_scope_filters(self) -> None:
        result = {"schema_version": 2, "match_count": 1, "matches": []}
        client = AppClient("https://localhost")
        with patch.object(client, "_request_json", return_value=result) as request:
            self.assertEqual(
                client.query_product_capability(
                    venue="BINANCE",
                    product="perpetual",
                    account_mode="hedge",
                    account_model="usd_m_futures",
                ),
                result,
            )
        self.assertEqual(
            request.call_args.kwargs["payload"],
            {
                "venue": "BINANCE",
                "product": "perpetual",
                "account_mode": "hedge",
                "account_model": "usd_m_futures",
            },
        )
        with self.assertRaisesRegex(ValueError, "scope filter"):
            client.query_product_capability()

    def test_product_capability_query_rejects_empty_id_and_async_delegates(self) -> None:
        client = AppClient("https://localhost")
        with self.assertRaisesRegex(ValueError, "capability_id"):
            client.query_product_capability("  ")
        result = {"schema_version": 1, "capability": {"id": "qx.venue.paper"}}
        with patch.object(client, "query_product_capability", return_value=result) as query:
            self.assertEqual(
                asyncio.run(client.async_query_product_capability("qx.venue.paper")),
                result,
            )
        query.assert_called_once_with(
            "qx.venue.paper",
            venue=None,
            product=None,
            account_mode=None,
            account_model=None,
            margin_mode=None,
        )

    def test_remote_catalog_is_typed_and_uses_the_canonical_route(self) -> None:
        payload = [
            {
                "name": "ema_cross",
                "description": "指数移动平均线交叉趋势",
                "signal_knobs": ["fast_window", "slow_window"],
                "requires_reference_leg": False,
            },
            {
                "name": "pairs_arbitrage",
                "description": "双腿配对价差套利",
                "signal_knobs": ["period", "threshold_bps"],
                "requires_reference_leg": True,
            },
        ]
        client = AppClient("https://localhost")
        with patch.object(client, "_request_json_value", return_value=payload) as request:
            catalog = client.list_builtin_strategies()

        self.assertEqual(request.call_args.args[0], "/app/strategies")
        self.assertEqual(request.call_args.kwargs["method"], "GET")
        self.assertEqual(APP_ROUTE_TEMPLATES["builtin_strategy_catalog"], "/app/strategies")
        self.assertEqual(
            catalog,
            (
                BuiltinStrategyInfo(
                    "ema_cross",
                    "指数移动平均线交叉趋势",
                    ("fast_window", "slow_window"),
                    False,
                ),
                BuiltinStrategyInfo(
                    "pairs_arbitrage",
                    "双腿配对价差套利",
                    ("period", "threshold_bps"),
                    True,
                ),
            ),
        )

    def test_remote_catalog_rejects_non_array_response(self) -> None:
        client = AppClient("https://localhost")
        with (
            patch.object(client, "_request_json_value", return_value={"items": []}),
            self.assertRaisesRegex(QianxingError, "invalid built-in strategy catalog"),
        ):
            client.list_builtin_strategies()

    def test_remote_catalog_rejects_malformed_entries_without_leaking_python_errors(self) -> None:
        valid = {
            "name": "ema_cross",
            "description": "EMA",
            "signal_knobs": ["fast_window", "slow_window"],
            "requires_reference_leg": False,
        }
        malformed = (
            [],
            {key: value for key, value in valid.items() if key != "name"},
            {**valid, "name": 42},
            {**valid, "signal_knobs": "fast_window"},
            {**valid, "signal_knobs": ["fast_window", 1]},
            {**valid, "requires_reference_leg": "false"},
            {**valid, "unexpected": True},
            [valid, valid],
        )
        client = AppClient("https://localhost")
        for value in malformed:
            with (
                self.subTest(value=value),
                patch.object(
                    client,
                    "_request_json_value",
                    return_value=[value] if isinstance(value, dict) else value,
                ),
                self.assertRaisesRegex(QianxingError, "invalid built-in strategy catalog"),
            ):
                client.list_builtin_strategies()

    def test_async_remote_catalog_delegates_to_the_same_typed_entrypoint(self) -> None:
        expected = (BuiltinStrategyInfo("ema_cross", "EMA", ("fast_window", "slow_window"), False),)
        client = AppClient("https://localhost")
        with patch.object(client, "list_builtin_strategies", return_value=expected) as listing:
            self.assertEqual(asyncio.run(client.async_list_builtin_strategies()), expected)
        listing.assert_called_once_with()


if __name__ == "__main__":
    unittest.main()
