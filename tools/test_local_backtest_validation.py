"""Acceptance evidence must preserve paths and refuse stale workspace reuse."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import backtest_acceptance as acceptance


class LocalBacktestValidationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "project with spaces"
        self.root.mkdir()
        self.summary = self.root / "result.summary.json"
        self.summary.write_text('{"result_hash":"verified-result"}', encoding="utf-8")
        self.manifest = self.root / "result.run.json"
        self.manifest.write_text("{}", encoding="utf-8")
        (self.root / "result.v7.record.json").write_text(json.dumps({
            "input_digest": "input-1", "config_fingerprint": "config-1", "replay_verdict": "verified",
        }), encoding="utf-8")
        self.equity = self.root / "result.equity.csv"
        self.fills = self.root / "result.fills.csv"
        self.equity.write_text("equity\n1\n", encoding="utf-8")
        self.fills.write_text("fills\n1\n", encoding="utf-8")

    def output(self, newline):
        return newline.join([
            "[Strategy \u00b7 Backtest] result_hash=verified-result",
            f"[RunManifest] path={self.manifest}",
            f"[Artifacts] summary={self.summary} equity={self.equity} fills={self.fills}", "",
        ])

    def test_space_paths_and_both_line_endings_preserve_evidence_identity(self):
        for newline in ("\n", "\r\n"):
            with self.subTest(newline=newline):
                snapshot = acceptance.snapshot_from_output(self.output(newline), self.root)
                self.assertEqual(snapshot["result_hash"], "verified-result")
                self.assertEqual(snapshot["replay_verdict"], "verified")
                self.assertEqual(snapshot["contents"]["fills"], acceptance.sha256_file(self.fills))

    def test_missing_artifact_is_never_a_passing_snapshot(self):
        self.fills.unlink()
        with self.assertRaises(SystemExit):
            acceptance.snapshot_from_output(self.output("\n"), self.root)

    def test_existing_work_directory_is_preserved_before_any_cli_invocation(self):
        marker = self.root / "keep.txt"
        marker.write_text("existing evidence", encoding="utf-8")
        report = Path(self.temp.name) / "new-report"
        script = Path(__file__).with_name("local_backtest_validation.py")
        result = subprocess.run([sys.executable, str(script), "--binary", str(script),
                                 "--work-dir", str(self.root), "--report-dir", str(report)],
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(marker.read_text(encoding="utf-8"), "existing evidence")
        self.assertFalse(report.exists())

    def test_nested_output_directories_are_rejected_before_creation(self):
        script = Path(__file__).with_name("local_backtest_validation.py")
        parent = Path(self.temp.name) / "fresh"
        child = parent / "nested"
        for work, report in ((parent, child), (child, parent), (parent, parent)):
            with self.subTest(work=work, report=report):
                result = subprocess.run([sys.executable, str(script), "--binary", str(script),
                                         "--work-dir", str(work), "--report-dir", str(report)],
                                        capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 2)
                self.assertFalse(parent.exists())


if __name__ == "__main__":
    unittest.main()
