"""Disk-full writes must never turn a managed project run into a success."""
from __future__ import annotations

import errno
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from qianxing import project_runs, strategy_project


class ProjectRunDiskFullTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="qx-project-disk-full-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.loader = patch.object(
            strategy_project,
            "load_strategy_project",
            return_value=(self.root, {"project_id": "disk-full-fixture"}),
        )
        self.loader.start()
        self.addCleanup(self.loader.stop)

    def _receipt(self) -> dict:
        paths = list((self.root / ".qianxing" / "runs").glob("*/run.json"))
        self.assertEqual(len(paths), 1)
        return json.loads(paths[0].read_text(encoding="utf-8"))

    def test_result_write_enospc_is_persisted_as_failed(self):
        original_write = project_runs._write

        def write(path, value):
            if Path(path).name == "result.json":
                raise OSError(errno.ENOSPC, "No space left on device")
            return original_write(path, value)

        def execute(_root, _manifest):
            current = project_runs._ACTIVE.get()
            current.result({"artifacts": {"summary": "summary.json"}})
            return 0

        with (
            patch.object(project_runs, "_write", side_effect=write),
            self.assertRaises(OSError) as raised,
        ):
            project_runs.run_project(self.root, execute)

        self.assertEqual(raised.exception.errno, errno.ENOSPC)
        receipt = self._receipt()
        self.assertEqual(receipt["status"], "failed")
        self.assertEqual(receipt["error"]["type"], "OSError")
        run_directory = next((self.root / ".qianxing" / "runs").iterdir())
        self.assertFalse((run_directory / "result.json").exists())

    def test_receipt_write_enospc_is_returned_and_never_persisted_as_success(self):
        original_write = project_runs._write
        receipt_writes = 0

        def write(path, value):
            nonlocal receipt_writes
            if Path(path).name == "run.json":
                receipt_writes += 1
                if receipt_writes == 2:
                    raise OSError(errno.ENOSPC, "No space left on device")
            return original_write(path, value)

        with (
            patch.object(project_runs, "_write", side_effect=write),
            self.assertRaises(OSError) as raised,
        ):
            project_runs.run_project(self.root, lambda _root, _manifest: 0)

        self.assertEqual(raised.exception.errno, errno.ENOSPC)
        self.assertEqual(receipt_writes, 2)
        self.assertEqual(self._receipt()["status"], "preparing")


if __name__ == "__main__":
    unittest.main()
