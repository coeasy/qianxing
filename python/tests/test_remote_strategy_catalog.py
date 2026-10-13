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
