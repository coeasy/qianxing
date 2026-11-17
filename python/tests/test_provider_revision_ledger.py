"""Provider snapshot revision import stays conservative and deterministic."""

from __future__ import annotations

import json
import tempfile
import unittest
from contextlib import redirect_stdout
from io import StringIO
from pathlib import Path

from qianxing_strategy.diagnostics import _adjustment_chain_check, _validate_pit_ledger
from tools.import_provider_revision_ledger import (
    LedgerImportError,
    _write_output,
    build_provider_revision_ledger,
    main as importer_main,
)


def _bar_frame(timestamps: list[int], closes: list[int], volumes: list[int]) -> dict:
    return {
        "schema_version": 1,
        "instrument": "BTC/USDT.BINANCE",
        "source": "ccxt:binance:BTC/USDT:1m",
        "ts": timestamps,
        "open_raw": [value - 1 for value in closes],
        "high_raw": [value + 2 for value in closes],
        "low_raw": [value - 2 for value in closes],
        "close_raw": closes,
        "volume_raw": volumes,
    }


def _a_share_bar_frame(timestamp: int, close: int) -> dict:
    return {
        "schema_version": 1,
        "instrument": "000001.SZSE",
        "source": "test:a-share-bars",
        "ts": [timestamp],
        "open_raw": [close],
        "high_raw": [close + 1],
        "low_raw": [close - 1],
        "close_raw": [close],
        "volume_raw": [1_000],
    }


class ProviderRevisionLedgerTests(unittest.TestCase):
    def test_imports_realized_changes_as_bounded_capture_versions(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first = root / "first.json"
            refreshed = root / "refreshed.json"
            first.write_text(
                json.dumps(_bar_frame([1_000, 2_000], [100, 200], [5, 6])),
                encoding="utf-8",
            )
            refreshed.write_text(
                json.dumps(_bar_frame([1_000, 2_000, 3_000], [100, 201, 300], [5, 7, 8])),
                encoding="utf-8",
            )

            ledger = build_provider_revision_ledger([first, refreshed], [10_000, 20_000])
            declared_basis = build_provider_revision_ledger(
                [first, refreshed], [10_000, 20_000], "raw"
            )
            facts, fingerprint = _validate_pit_ledger(ledger)
            self.assertEqual(len(facts), 15)
            self.assertEqual(len(fingerprint), 64)
            self.assertIn("first-captured-by-qianxing", ledger["source"])
            self.assertIn("not the exchange publication timestamp", ledger["source"])
            self.assertEqual(ledger["price_adjustment_basis"], "unknown")
            self.assertEqual(declared_basis["price_adjustment_basis"], "raw")
            _validate_pit_ledger(declared_basis)

            revised_close = next(
                fact for fact in facts
                if fact["effective_at"] == 2_000 and fact["field"] == "bar.close_raw"
            )
            self.assertEqual(
                [version["available_at"] for version in revised_close["versions"]],
                [10_000, 20_000],
            )
            self.assertNotEqual(
                revised_close["versions"][0]["value_fingerprint"],
                revised_close["versions"][1]["value_fingerprint"],
            )
            unchanged_close = next(
                fact for fact in facts
                if fact["effective_at"] == 1_000 and fact["field"] == "bar.close_raw"
            )
            self.assertEqual(len(unchanged_close["versions"]), 1)

    def test_rejects_identity_drift_bad_observation_order_and_misaligned_fields(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first = root / "first.json"
            other = root / "other.json"
            first.write_text(json.dumps(_bar_frame([1_000], [100], [5])), encoding="utf-8")
            changed_identity = _bar_frame([1_000], [101], [5])
            changed_identity["instrument"] = "ETH/USDT.BINANCE"
            other.write_text(json.dumps(changed_identity), encoding="utf-8")
            with self.assertRaisesRegex(LedgerImportError, "identical instrument and source"):
                build_provider_revision_ledger([first, other], [10_000, 20_000])
            with self.assertRaisesRegex(LedgerImportError, "strictly increasing"):
                build_provider_revision_ledger([first, first], [20_000, 10_000])
            with self.assertRaisesRegex(LedgerImportError, "price adjustment basis"):
                build_provider_revision_ledger([first], [10_000], "guess")

            malformed = _bar_frame([1_000, 2_000], [100], [5, 6])
            other.write_text(json.dumps(malformed), encoding="utf-8")
            with self.assertRaisesRegex(LedgerImportError, "length must match ts"):
                build_provider_revision_ledger([other], [20_000])
            with self.assertRaisesRegex(LedgerImportError, "capture time precedes"):
                build_provider_revision_ledger([first], [999])

    def test_raw_integer_fingerprints_preserve_i128_precision_and_output_is_no_clobber(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            snapshot = root / "large.json"
            large = 170_141_183_460_469_231_731_687_303_715_884_105_717
            snapshot.write_text(
                json.dumps(_bar_frame([1_000], [large], [large])),
                encoding="utf-8",
            )
            ledger = build_provider_revision_ledger([snapshot], [10_000])
            _validate_pit_ledger(ledger)
            output = root / "ledger.json"
            output.write_text("preserve existing file", encoding="utf-8")
            with self.assertRaisesRegex(LedgerImportError, "output already exists"):
                _write_output(output, ledger, force=False)
            self.assertEqual(output.read_text(encoding="utf-8"), "preserve existing file")
            _write_output(output, ledger, force=True)
            imported = json.loads(output.read_text(encoding="utf-8"))
            _validate_pit_ledger(imported)
            outside_i128 = _bar_frame([1_000], [large + 10], [5])
            snapshot.write_text(json.dumps(outside_i128), encoding="utf-8")
            with self.assertRaisesRegex(LedgerImportError, "signed i128 range"):
                build_provider_revision_ledger([snapshot], [10_000])

    def test_imports_a_share_corporate_action_capture_versions_for_adjustment_diagnostics(self):
        from datetime import datetime, timezone

        def utc_ms(value: str) -> int:
            return int(datetime.fromisoformat(value).replace(tzinfo=timezone.utc).timestamp() * 1_000)

        bar_at = utc_ms("2024-05-29T01:30:00")
        first_capture = utc_ms("2024-06-01T00:00:00")
        second_capture = utc_ms("2024-06-04T00:00:00")
        action_at = utc_ms("2024-06-02T16:00:00")  # 2024-06-03 00:00 Asia/Shanghai
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first_bar = root / "bar-before.json"
            revised_bar = root / "bar-after.json"
            first_actions = root / "actions-before.json"
            action_revision = root / "actions-after.json"
            first_bar.write_text(json.dumps(_a_share_bar_frame(bar_at, 100)), encoding="utf-8")
            revised_bar.write_text(json.dumps(_a_share_bar_frame(bar_at, 90)), encoding="utf-8")
            action_document = {
                "schema_version": 1,
                "source": "test:a-share-actions",
                "instrument": "000001.SZSE",
                "as_of": None,
                "actions": [{
                    "action_id": "issuer-action-2024-001",
                    "instrument": "000001.SZSE",
                    "ex_date": "2024-06-03",
                    "published_at": "2024-05-28T00:00:00+08:00",
                    "source": "test:a-share-actions",
                    "action_type": "cash_dividend",
                    "cash_dividend_raw": "50000000",
                }],
            }
            first_actions.write_text(json.dumps(action_document), encoding="utf-8")
            action_document["actions"][0]["cash_dividend_raw"] = "100000000"
            action_revision.write_text(json.dumps(action_document), encoding="utf-8")

            ledger = build_provider_revision_ledger(
                [first_bar, revised_bar],
                [first_capture, second_capture],
                "backward_adjusted",
                [first_actions, action_revision],
                [first_capture, second_capture],
            )
            facts, _ = _validate_pit_ledger(ledger)
            price = next(fact for fact in facts if fact["field"] == "bar.close_raw")
            action = next(fact for fact in facts if fact["field"] == "corporate_action.cash_dividend")
            self.assertEqual(action["effective_at"], action_at)
            self.assertEqual(
                [version["available_at"] for version in action["versions"]],
                [first_capture, second_capture],
            )
            self.assertNotEqual(
                action["versions"][0]["value_fingerprint"],
                action["versions"][1]["value_fingerprint"],
            )
            self.assertEqual(price["versions"][-1]["available_at"], second_capture)
            self.assertIn("published_at is retained", ledger["source"])

            chain = _adjustment_chain_check(
                facts,
                [bar_at + 1, action_at - 1],
                ledger["price_adjustment_basis"],
            )
        self.assertEqual(chain["status"], "potential_back_adjustment_lookahead")
        # All four OHLC price fields moved after the same action entered the
        # captured action ledger, so each field contributes one temporal candidate.
        self.assertEqual(chain["candidate_count"], 4)
        self.assertEqual(chain["examples"][0]["causal_attribution"], "temporal_correlation_only")

    def test_corporate_action_import_requires_paired_capture_times_and_stable_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bars = root / "bars.json"
            actions = root / "actions.json"
            bars.write_text(json.dumps(_a_share_bar_frame(1_700_000_000_000, 100)), encoding="utf-8")
            actions.write_text(json.dumps({
                "schema_version": 1,
                "source": "test:actions",
                "instrument": "000001.SZSE",
                "as_of": None,
                "actions": [],
            }), encoding="utf-8")
            with self.assertRaisesRegex(LedgerImportError, "each --actions-snapshot"):
                build_provider_revision_ledger([bars], [1_700_000_001_000], action_snapshots=[actions])

            duplicate_action = {
                "instrument": "000001.SZSE", "ex_date": "2024-06-03",
                "published_at": "2024-05-28T00:00:00+08:00",
                "source": "test:actions", "action_type": "cash_dividend",
                "cash_dividend_raw": "100000000",
            }
            actions.write_text(json.dumps({
                "schema_version": 1, "source": "test:actions", "instrument": "000001.SZSE",
                "as_of": None, "actions": [duplicate_action, duplicate_action],
            }), encoding="utf-8")
            with self.assertRaisesRegex(LedgerImportError, "duplicates an action identity"):
                build_provider_revision_ledger(
                    [bars], [1_700_000_001_000], "unknown", [actions], [1_700_000_001_500]
                )

            first_action = dict(duplicate_action, action_id="stable-provider-id")
            revised_action = dict(first_action, ex_date="2024-06-04")
            before = root / "actions-before.json"
            after = root / "actions-after.json"
            for path, row in ((before, first_action), (after, revised_action)):
                path.write_text(json.dumps({
                    "schema_version": 1, "source": "test:actions", "instrument": "000001.SZSE",
                    "as_of": None, "actions": [row],
                }), encoding="utf-8")
            with self.assertRaisesRegex(LedgerImportError, "changed ex_date or action_type"):
                build_provider_revision_ledger(
                    [bars], [1_700_000_001_000], "unknown", [before, after],
                    [1_700_000_001_500, 1_700_000_002_000],
                )

    def test_imports_the_normalized_a_share_serializer_fixture(self):
        fixture = Path(__file__).parent / "fixtures" / "ashare_actions_cross_check.payload.json"
        with tempfile.TemporaryDirectory() as directory:
            bars = Path(directory) / "bars.json"
            bars.write_text(
                json.dumps(_a_share_bar_frame(1_716_946_200_000, 100)),
                encoding="utf-8",
            )
            ledger = build_provider_revision_ledger(
                [bars], [1_717_747_200_000], "raw", [fixture], [1_717_747_200_000]
            )
        facts, _ = _validate_pit_ledger(ledger)
        action_facts = [fact for fact in facts if fact["field"].startswith("corporate_action.")]
        self.assertEqual(len(action_facts), 5)
        self.assertEqual({fact["entity_id"] for fact in action_facts}, {"000001.SZSE"})

    def test_cli_imports_a_share_actions_with_capture_times(self):
        fixture = Path(__file__).parent / "fixtures" / "ashare_actions_cross_check.payload.json"
        with tempfile.TemporaryDirectory() as directory:
            bars = Path(directory) / "bars.json"
            output = Path(directory) / "ledger.json"
            bars.write_text(
                json.dumps(_a_share_bar_frame(1_716_946_200_000, 100)),
                encoding="utf-8",
            )
            with redirect_stdout(StringIO()):
                result = importer_main([
                    "--snapshot", str(bars),
                    "--observed-at-ms", "1717747200000",
                    "--actions-snapshot", str(fixture),
                    "--actions-observed-at-ms", "1717747200000",
                    "--price-adjustment-basis", "raw",
                    "--output", str(output),
                ])
            self.assertEqual(result, 0)
            ledger = json.loads(output.read_text(encoding="utf-8"))
        facts, _ = _validate_pit_ledger(ledger)
        self.assertEqual(len([fact for fact in facts if fact["field"].startswith("corporate_action.")]), 5)


if __name__ == "__main__":
    unittest.main()
