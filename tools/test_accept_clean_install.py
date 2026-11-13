from __future__ import annotations

import io
import os
import subprocess
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from tools.accept_clean_install import (
    BUILD_COMMAND_TIMEOUT_SECONDS,
    configure_utf8_output,
    ensure_cli_install,
    run,
)


class RunTimeoutTests(unittest.TestCase):
    def test_passes_timeout_to_subprocess(self) -> None:
        expected = subprocess.CompletedProcess(["tool"], 0, "ok", "")
        with patch("tools.accept_clean_install.subprocess.run", return_value=expected) as run_process:
            result = run(["tool"], cwd=Path("."), env={}, timeout_seconds=12)

        self.assertIs(result, expected)
        self.assertEqual(run_process.call_args.kwargs["timeout"], 12)

    def test_reports_timed_out_command(self) -> None:
        failure = subprocess.TimeoutExpired(["slow-tool"], 7, output=b"partial output")
        with patch("tools.accept_clean_install.subprocess.run", side_effect=failure):
            with self.assertRaisesRegex(TimeoutError, r"timed out after 7s: slow-tool"):
                run(["slow-tool"], cwd=Path("."), env={}, timeout_seconds=7)

    def test_rejects_non_positive_timeout(self) -> None:
        with self.assertRaisesRegex(ValueError, "must be positive"):
            run(["tool"], cwd=Path("."), env={}, timeout_seconds=0)


class EnsureCliInstallTests(unittest.TestCase):
    def test_reuses_an_existing_cli_install_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            repo = Path(temporary) / "repo"
            cli_root = Path(temporary) / "shared-cli"
            cli_bin = cli_root / "bin"
            cli_bin.mkdir(parents=True)
            cli_name = "qx-cli.exe" if os.name == "nt" else "qx-cli"
            existing_cli = cli_bin / cli_name
            existing_cli.write_text("existing test binary", encoding="utf-8")

            with patch("tools.accept_clean_install.run") as run:
                self.assertEqual(ensure_cli_install(repo, cli_root, {}), existing_cli)

            run.assert_not_called()

    def test_installs_cli_when_shared_root_has_no_binary(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            repo = Path(temporary) / "repo"
            cli_root = Path(temporary) / "shared-cli"
            cli_bin = cli_root / "bin"
            cli_name = "qx-cli.exe" if os.name == "nt" else "qx-cli"
            expected_cli = cli_bin / cli_name

            def fake_install(
                command: list[str],
                *,
                cwd: Path,
                env: dict[str, str],
                timeout_seconds: int,
            ) -> None:
                self.assertEqual(command[:2], ["cargo", "install"])
                self.assertIn("--force", command)
                self.assertEqual(cwd, repo)
                self.assertEqual(timeout_seconds, BUILD_COMMAND_TIMEOUT_SECONDS)
                cli_bin.mkdir(parents=True)
                expected_cli.write_text("new test binary", encoding="utf-8")

            with patch("tools.accept_clean_install.run", side_effect=fake_install) as run:
                self.assertEqual(ensure_cli_install(repo, cli_root, {}), expected_cli)

            run.assert_called_once()


class Utf8OutputTests(unittest.TestCase):
    def test_reconfigures_legacy_windows_console_encoding(self) -> None:
        stream = io.TextIOWrapper(io.BytesIO(), encoding="cp1252")
        try:
            configure_utf8_output(stream)
            self.assertEqual(stream.encoding.lower().replace("_", "-"), "utf-8")
            self.assertEqual(stream.errors, "backslashreplace")
            stream.write("中文帮助输出")
            stream.flush()
            self.assertIn("中文帮助输出".encode("utf-8"), stream.buffer.getvalue())
        finally:
            stream.close()


if __name__ == "__main__":
    unittest.main()
