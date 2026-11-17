from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock


REPO_ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = REPO_ROOT / "benchmarks" / "run_strategy_worker_crash_recovery.py"
SPEC = importlib.util.spec_from_file_location("run_strategy_worker_crash_recovery", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
CRASH_RECOVERY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CRASH_RECOVERY)


class PsqlPathTests(unittest.TestCase):
    @unittest.skipUnless(os.name == "posix", "psql wrapper symlinks use POSIX executable semantics")
    def test_explicit_wrapper_symlink_is_preserved(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            wrapper = root / "pg_wrapper"
            wrapper.write_text("#!/bin/sh\nprintf '%s' \"$0\"\n", encoding="utf-8")
            wrapper.chmod(0o755)
            psql = root / "psql"
            psql.symlink_to(wrapper)

            resolved = CRASH_RECOVERY.locate_psql(psql)

            self.assertEqual(resolved, psql)
            assert resolved is not None
            completed = subprocess.run([str(resolved)], capture_output=True, text=True, check=True)
            self.assertEqual(completed.stdout, str(psql))

    @unittest.skipUnless(os.name == "posix", "psql wrapper symlinks use POSIX executable semantics")
    @mock.patch.object(CRASH_RECOVERY.shutil, "which")
    def test_discovered_wrapper_symlink_is_preserved(self, which: mock.MagicMock) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            wrapper = root / "pg_wrapper"
            wrapper.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            wrapper.chmod(0o755)
            psql = root / "psql"
            psql.symlink_to(wrapper)
            which.return_value = str(psql)

            resolved = CRASH_RECOVERY.locate_psql()

            self.assertEqual(resolved, psql)

    @mock.patch.object(CRASH_RECOVERY, "psql")
    def test_postgres_cursor_snapshot_reads_the_json_state_row(self, psql: mock.MagicMock) -> None:
        state = {
            "next_seq_by_log": {"paper": 7},
            "commit_generation": 3,
            "strategy_checkpoint": {"cursor": 7},
        }
        content = json.dumps(state, sort_keys=True, separators=(",", ":"))
        psql.side_effect = ["qx_json_state", content]

        snapshot = CRASH_RECOVERY.cursor_snapshot(
            Path("."),
            backend="postgres",
            psql_binary=Path("/usr/bin/psql"),
            postgres_dsn="postgres://user:pass@localhost/db",
            worker_id="strategy-paper",
        )

        self.assertIsNotNone(snapshot)
        assert snapshot is not None
        self.assertEqual(snapshot["path"], "postgres:strategy-execution-cursor/strategy-paper")
        self.assertEqual(snapshot["next_seq_by_log"], {"paper": 7})
        self.assertEqual(snapshot["commit_generation"], 3)
        self.assertEqual(
            psql.call_args.args[2],
            "SELECT content FROM qx_json_state WHERE state_key = 'strategy-execution-cursor/strategy-paper'",
        )


if __name__ == "__main__":
    unittest.main()
