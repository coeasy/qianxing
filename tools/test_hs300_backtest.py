"""Data admission tests: gaps, stale data and damaged prices never publish returns."""
import csv
from datetime import date, timedelta
import json
from pathlib import Path
import sys
import tempfile
import unittest
import zipfile
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import hs300_backtest as runner


class HistoryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.days = [date(2009, 12, 28) + timedelta(days=i) for i in range(12)]
        self.days = [day for day in self.days if day.weekday() < 5]
        self.csv = self.root / "bars.csv"
        self.calendar = self.root / "calendar.json"
        self.write_csv(self.days)
        runner.save(self.calendar, {"schema_version": 1, "source": "independent-test-calendar",
                                   "coverage": {"start": "2009-12-28", "end": "2010-01-08"},
                                   "trading_days": list(map(str, self.days))})

    def write_csv(self, days):
        with self.csv.open("w", encoding="utf-8", newline="") as stream:
            writer = csv.writer(stream)
            writer.writerow(["date", "open", "high", "low", "close", "volume"])
            writer.writerows([[str(day), "3000.12", "3100", "2900", "3050.4567", "0"] for day in days])

    def load(self):
        return runner.load_history(self.csv, self.calendar, date(2010, 1, 1), date(2010, 1, 8), 3, "test-only")

    def test_complete_calendar_includes_prior_warmup_and_zero_volume(self):
        frame, quality = self.load()
        self.assertEqual(quality["warmup_sessions"], 3)
        self.assertEqual(quality["actual_last_session"], "2010-01-08")
        self.assertEqual(frame["close_raw"][0], 3050456700000)
        self.assertEqual(frame["ts"][0] % 86400000, 7 * 3600000)

    def test_missing_middle_and_stale_tail_are_rejected(self):
        for omitted in (self.days[5], self.days[-1]):
            with self.subTest(omitted=omitted):
                self.write_csv([day for day in self.days if day != omitted])
                with self.assertRaisesRegex(ValueError, "missing="):
                    self.load()

    def test_duplicate_reverse_and_nontrading_day_are_rejected(self):
        for days in (self.days + [self.days[-1]], list(reversed(self.days)),
                     sorted(self.days + [date(2010, 1, 2)])):
            self.write_csv(days)
            with self.assertRaises(ValueError):
                self.load()

    def test_independent_calendar_coverage_and_warmup_are_required(self):
        value = json.loads(self.calendar.read_text())
        value["coverage"]["end"] = "2010-01-07"
        runner.save(self.calendar, value)
        with self.assertRaisesRegex(ValueError, "coverage"):
            self.load()

    def test_price_decimal_is_exact_or_rejected(self):
        self.assertEqual(runner.raw("123456789012345678901.123456789", "close"), 123456789012345678901123456789)
        for value in ("NaN", "Infinity", "0", "-1", "1.0000000001", "1e100"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                runner.raw(value, "close")

    def test_bad_ohlc_is_rejected(self):
        self.csv.write_text(self.csv.read_text().replace("3100", "3000"), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "OHLC"):
            self.load()

    def test_missing_data_writes_blocked_report_without_spawning_engine(self):
        import argparse
        args = argparse.Namespace(csv=self.root / "missing.csv", calendar=self.calendar,
                                  start=date(2010, 1, 1), end=date(2010, 1, 8), source="test",
                                  fast=2, slow=3, cash="1000000", exposure_bps=9500, fee_bps=3,
                                  work_dir=self.root / "work", report_dir=self.root / "reports", binary=Path("qx-cli"))
        with patch.object(runner.subprocess, "run") as process:
            result = runner.run(args)
        process.assert_not_called()
        self.assertEqual(result["status"], "blocked")
        self.assertIsNone(result["performance_metrics"])
        self.assertTrue((args.report_dir / "report.html").is_file())
        with self.assertRaisesRegex(ValueError, "previous evidence"):
            runner.run(args)

    def test_evidence_archive_binds_run_inputs_and_reports_deterministically(self):
        work = self.root / "work"
        reports = self.root / "reports"
        work.mkdir()
        reports.mkdir()
        (work / "source.csv").write_bytes(b"date,close\n")
        (work / "summary.json").write_text('{"run_id":"run-1"}', encoding="utf-8")
        (reports / "engine-report.html").write_text("<html>verified</html>", encoding="utf-8")
        archive_path = reports / "inputs-and-artifacts.zip"
        run_identity = {"run_id": "run-1", "result_hash": "abc123"}

        first_archive_sha, first_manifest_sha = runner._create_evidence_archive(
            work, reports, archive_path, run_identity,
        )
        with zipfile.ZipFile(archive_path) as archive:
            manifest = json.loads(archive.read("reports/archive-manifest.json"))
            self.assertEqual(manifest["run_identity"], run_identity)
            self.assertEqual(set(manifest["members"]), {
                "inputs/source.csv", "inputs/summary.json", "reports/engine-report.html",
            })
            self.assertIn("reports/archive-manifest.json", archive.namelist())
        second_archive_sha, second_manifest_sha = runner._create_evidence_archive(
            work, reports, archive_path, run_identity,
        )
        self.assertEqual(first_archive_sha, second_archive_sha)
        self.assertEqual(first_manifest_sha, second_manifest_sha)


if __name__ == "__main__":
    unittest.main()
