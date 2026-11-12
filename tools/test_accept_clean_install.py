from __future__ import annotations

import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from tools.accept_clean_install import ensure_cli_install


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

            def fake_install(command: list[str], *, cwd: Path, env: dict[str, str]) -> None:
                self.assertEqual(command[:2], ["cargo", "install"])
                self.assertIn("--force", command)
                self.assertEqual(cwd, repo)
                cli_bin.mkdir(parents=True)
                expected_cli.write_text("new test binary", encoding="utf-8")

            with patch("tools.accept_clean_install.run", side_effect=fake_install) as run:
                self.assertEqual(ensure_cli_install(repo, cli_root, {}), expected_cli)

            run.assert_called_once()


if __name__ == "__main__":
    unittest.main()
