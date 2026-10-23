from __future__ import annotations

import sys
import subprocess
import unittest
from pathlib import Path

from benchmarks.process_metrics import _decode_process_output, run_measured


class ProcessMetricsTests(unittest.TestCase):
    def test_utf8_output_decodes_without_loss(self) -> None:
        text = "Paper 轨验收通过"
        self.assertEqual(_decode_process_output(text.encode("utf-8")), text)

    def test_process_tree_capture_preserves_unicode_output(self) -> None:
        result = run_measured(
            [sys.executable, "-c", "print('Paper 轨验收通过')"],
            cwd=Path.cwd(),
        )
        self.assertEqual(result.return_code, 0)
        self.assertIn("Paper 轨验收通过", result.stdout)
        self.assertGreater(result.elapsed_ms, 0)
        self.assertGreaterEqual(result.peak_process_count, 1)

    def test_process_timeout_is_enforced(self) -> None:
        with self.assertRaises(subprocess.TimeoutExpired):
            run_measured(
                [sys.executable, "-c", "import time; time.sleep(5)"],
                cwd=Path.cwd(),
                timeout_seconds=0.05,
            )


if __name__ == "__main__":
    unittest.main()
