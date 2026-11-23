from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from select_compatible_baseline import REPORTS, is_compatible


class SelectCompatibleBaselineTests(unittest.TestCase):
    def make_reports(
        self,
        directory: Path,
        *,
        cpu_count: int = 8,
        total_ram_bytes: int = 16_000_000_000,
        source: str = "baseline-source",
    ) -> None:
        directory.mkdir(parents=True, exist_ok=True)
        report = {
            "schema_version": 1,
            "measurement_source_fingerprint_schema": 1,
            "measurement_source_fingerprint": source,
            "hardware": {
                "os": "Linux",
                "arch": "x86_64",
                "processor": "AMD EPYC 7763",
                "logical_cpu_count": cpu_count,
                "physical_core_count": 4,
                "total_ram_bytes": total_ram_bytes,
            },
            "rows": [{"bars": 1000, "p50_ms": 1.0, "p95_ms": 2.0}],
        }
        for filename in REPORTS:
            (directory / filename).write_text(json.dumps(report), encoding="utf-8")

    def test_selects_baseline_with_same_runner_fingerprint(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.make_reports(root / "current", source="candidate-source")
            self.make_reports(root / "baseline")

            compatible, reason = is_compatible(root / "current", root / "baseline")

        self.assertTrue(compatible)
        self.assertIn("4 report(s) have matching source schema, workload, and runner", reason)

    def test_allows_small_reported_ram_variation(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.make_reports(
                root / "current", total_ram_bytes=16_000_000_000, source="candidate-source"
            )
            self.make_reports(root / "baseline", total_ram_bytes=16_050_000_000)

            compatible, _ = is_compatible(root / "current", root / "baseline")

        self.assertTrue(compatible)

    def test_rejects_different_runner_even_when_latency_rows_match(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.make_reports(root / "current", source="candidate-source")
            self.make_reports(root / "baseline", cpu_count=16)

            compatible, reason = is_compatible(root / "current", root / "baseline")

        self.assertFalse(compatible)
        self.assertIn("hardware fingerprint differs", reason)

    def test_rejects_missing_hardware_fingerprint(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.make_reports(root / "current", source="candidate-source")
            self.make_reports(root / "baseline")
            (root / "baseline" / REPORTS[0]).write_text(
                json.dumps({"schema_version": 1, "rows": []}), encoding="utf-8"
            )

            compatible, reason = is_compatible(root / "current", root / "baseline")

        self.assertFalse(compatible)
        self.assertIn("missing or invalid baseline hardware", reason)

    def test_rejects_unsupported_report_schema_even_when_sources_are_unchanged(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.make_reports(root / "current")
            self.make_reports(root / "baseline")
            report_path = root / "baseline" / REPORTS[0]
            report = json.loads(report_path.read_text(encoding="utf-8"))
            report["schema_version"] = 99
            report_path.write_text(json.dumps(report), encoding="utf-8")

            compatible, reason = is_compatible(root / "current", root / "baseline")

        self.assertFalse(compatible)
        self.assertIn("missing or invalid report", reason)

    def test_rejects_malformed_report_rows_before_selecting_same_source(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.make_reports(root / "current")
            self.make_reports(root / "baseline")
            report_path = root / "baseline" / REPORTS[0]
            report = json.loads(report_path.read_text(encoding="utf-8"))
            report["rows"] = [None]
            report_path.write_text(json.dumps(report), encoding="utf-8")

            compatible, reason = is_compatible(root / "current", root / "baseline")

        self.assertFalse(compatible)
        self.assertIn("missing or invalid report", reason)

    def test_rejects_when_no_reports_are_shared(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.make_reports(root / "current", source="candidate-source")
            (root / "baseline").mkdir()

            compatible, reason = is_compatible(root / "current", root / "baseline")

        self.assertFalse(compatible)
        self.assertIn("no shared report", reason)

    def test_rejects_fingerprint_schema_mismatch_when_no_other_report_is_compatible(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.make_reports(root / "current", source="candidate-source")
            self.make_reports(root / "baseline")
            (root / "baseline" / REPORTS[0]).write_text(
                json.dumps(
                    {
                        **json.loads((root / "baseline" / REPORTS[0]).read_text(encoding="utf-8")),
                        "measurement_source_fingerprint_schema": 99,
                    }
                ),
                encoding="utf-8",
            )
            for filename in REPORTS[1:]:
                (root / "baseline" / filename).unlink()

            compatible, reason = is_compatible(root / "current", root / "baseline")

        self.assertFalse(compatible)
        self.assertIn("no shared report", reason)


if __name__ == "__main__":
    unittest.main()
