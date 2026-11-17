import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from qianxing_ccxt.private_probe import _preflight_account_queries


class PrivateProbeCapabilityTest(unittest.TestCase):
    def _config(self, default_type: str, account_model: str = "unified") -> Path:
        temp = tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", suffix=".json", delete=False)
        with temp:
            json.dump(
                {
                    "exchange_id": "okx",
                    "account_model": account_model,
                    "default_type": default_type,
                    "sandbox": True,
                    "options": {},
                },
                temp,
            )
        self.addCleanup(Path(temp.name).unlink, missing_ok=True)
        return Path(temp.name)

    def test_derivative_probe_requests_position_and_account_settings_scopes(self):
        decision = {
            "allowed": True,
            "allowed_operations": [
                "balance.read",
                "positions.read",
                "account_settings.read",
            ],
        }
        with patch(
            "qianxing_ccxt.private_probe.app_account_query_capability_preflight_v1",
            return_value=json.dumps(decision),
        ) as preflight:
            value, product = _preflight_account_queries(self._config("swap"))

        spec = json.loads(preflight.call_args.args[0])
        self.assertEqual(product, "perpetual")
        self.assertEqual(value["account_model"], "unified")
        self.assertEqual(
            spec["required_operations"],
            ["balance.read", "positions.read", "account_settings.read"],
        )
        self.assertTrue(spec["sandbox_endpoint"])

    def test_probe_fails_closed_before_connection_for_undeclared_scope(self):
        with patch(
            "qianxing_ccxt.private_probe.app_account_query_capability_preflight_v1",
            return_value=json.dumps(
                {
                    "allowed": False,
                    "reason_code": "account_scope_not_declared",
                    "reason": "scope missing",
                }
            ),
        ):
            with self.assertRaisesRegex(ValueError, "capability preflight denied: scope missing"):
                _preflight_account_queries(self._config("swap", "unknown"))

    def test_spot_probe_requires_only_balance_read(self):
        with patch(
            "qianxing_ccxt.private_probe.app_account_query_capability_preflight_v1",
            return_value=json.dumps({"allowed": True}),
        ) as preflight:
            _, product = _preflight_account_queries(self._config("spot"))

        self.assertEqual(product, "spot")
        spec = json.loads(preflight.call_args.args[0])
        self.assertEqual(spec["required_operations"], ["balance.read"])


if __name__ == "__main__":
    unittest.main()
