from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path

from tools.check_cli_test_data_isolation import changed_runtime_data


class CliTestDataIsolationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="qx-cli-data-isolation-")
        self.root = Path(self.temporary.name)
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)
        subprocess.run(
            ["git", "config", "user.email", "test@example.invalid"],
            cwd=self.root,
            check=True,
        )
        subprocess.run(
            ["git", "config", "user.name", "Test"], cwd=self.root, check=True
        )
        self._write(".gitignore", "crates/qx-cli/data/*/runs/\n")
        self._write("deploy/data/baseline.json", "{}\n")
        subprocess.run(["git", "add", "."], cwd=self.root, check=True)
        subprocess.run(
            ["git", "commit", "-qm", "baseline"], cwd=self.root, check=True
        )

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _write(self, relative: str, content: str) -> None:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def test_clean_repository_data_roots_pass(self) -> None:
        self.assertEqual(changed_runtime_data(self.root), [])

    def test_tracked_untracked_and_ignored_artifacts_are_reported(self) -> None:
        self._write("deploy/data/baseline.json", "changed\n")
        self._write("deploy/data/new-output.json", "{}\n")
        self._write("crates/qx-cli/data/qianxing/runs/report.json", "{}\n")
        self.assertEqual(
            changed_runtime_data(self.root),
            [
                "crates/qx-cli/data/qianxing/runs/report.json",
                "deploy/data/baseline.json",
                "deploy/data/new-output.json",
            ],
        )


if __name__ == "__main__":
    unittest.main()
