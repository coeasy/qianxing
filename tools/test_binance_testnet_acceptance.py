"""Unit tests for the fail-closed sandbox evidence gate."""

import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

import tools.binance_testnet_acceptance as acceptance
from tools.binance_testnet_acceptance import (
    redact_credentials,
    sandbox_tested_flip_allowed,
    write_result_package,
)


class SandboxFlipGateTests(unittest.TestCase):
    def test_credentials_are_redacted_from_diagnostics(self):
        message = "request failed key=public-key secret=private-secret passphrase=demo-pass"
        redacted = redact_credentials(
            message,
            ["public-key", "private-secret", "demo-pass"],
        )
        self.assertEqual(
            redacted,
            "request failed key=[REDACTED] secret=[REDACTED] passphrase=[REDACTED]",
        )
        for credential in ("public-key", "private-secret", "demo-pass"):
            self.assertNotIn(credential, redacted)

    def test_stage_evidence_redacts_credentials_echoed_by_supplier(self):
        credential = "private-secret-value"
        completed = acceptance.subprocess.CompletedProcess(
            args=["qx", "probe"], returncode=1, stdout="", stderr=f"supplier echoed {credential}"
        )
        acceptance.STAGE_RECORDS.clear()
        with patch.dict("os.environ", {acceptance.SECRET_ENV: credential}), patch(
            "tools.binance_testnet_acceptance.subprocess.run", return_value=completed
        ):
            acceptance.run(Path("qx"), ["private-probe"])
        evidence_text = str(acceptance.STAGE_RECORDS)
        self.assertIn("[REDACTED]", evidence_text)
        self.assertNotIn(credential, evidence_text)

    def test_requires_pass_credentials_and_order_submission(self):
        cases = (
            ("pass", True, True, True),
            ("pass", True, False, False),
            ("pass", False, True, False),
            ("skipped", True, True, False),
            ("fail", True, True, False),
        )
        for outcome, credentials, send_orders, expected in cases:
            with self.subTest(
                outcome=outcome,
                credentials_present=credentials,
                send_orders=send_orders,
            ):
                self.assertEqual(
                    sandbox_tested_flip_allowed(outcome, credentials, send_orders),
                    expected,
                )

    def test_result_package_uses_same_gate(self):
        with tempfile.TemporaryDirectory() as directory:
            package = write_result_package(
                workdir=Path(directory),
                binary=Path("qx"),
                instrument="BTCUSDT.BINANCE",
                send_orders=False,
                credentials_present=True,
                outcome="pass",
                started_at=0,
            )
            import json

            self.assertFalse(json.loads(package.read_text(encoding="utf-8"))["sandbox_tested_flip"]["allowed"])

    def test_okx_acceptance_copies_sandbox_config_next_to_runtime_config(self):
        import json

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = acceptance.build_acceptance_config(
                root,
                acceptance.OKX_BASE_CONFIG,
                ccxt_config=acceptance.OKX_CCXT_CONFIG,
            )
            runtime = json.loads(config.read_text(encoding="utf-8"))
            ccxt = json.loads((root / acceptance.OKX_CCXT_CONFIG.name).read_text(encoding="utf-8"))
            self.assertEqual(runtime["workers"][1]["endpoint"], acceptance.OKX_CCXT_CONFIG.name)
            self.assertEqual(ccxt["exchange_id"], "okx")
            self.assertIs(ccxt["sandbox"], True)

    def test_okx_acceptance_rejects_non_sandbox_ccxt_config(self):
        import json

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = root / "unsafe.json"
            payload = json.loads(acceptance.OKX_CCXT_CONFIG.read_text(encoding="utf-8"))
            payload["sandbox"] = False
            config.write_text(json.dumps(payload), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "sandbox=true"):
                acceptance.build_acceptance_config(
                    root,
                    acceptance.OKX_BASE_CONFIG,
                    ccxt_config=config,
                )


if __name__ == "__main__":
    unittest.main()
