from __future__ import annotations

import io
import json
import os
import subprocess
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from tools.accept_clean_install import (
    BUILD_COMMAND_TIMEOUT_SECONDS,
    QUICKSTART_COMPLETION_STEPS,
    configure_utf8_output,
    ensure_cli_install,
    run,
    verify_quickstart_acceptance,
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


class QuickstartAcceptanceTests(unittest.TestCase):
    def fixture_output(self, project: Path) -> str:
        completed = "\n".join(
            f"[完成] {label}：qx-cli {index}"
            for index, label in enumerate(QUICKSTART_COMPLETION_STEPS, start=1)
        )
        assets = "\n".join(
            f"[Report] {name}={project / (name + suffix)}"
            for name, suffix in (
                ("html", ".html"),
                ("equity_svg", ".equity.svg"),
                ("fills_svg", ".fills.svg"),
                ("monthly_svg", ".monthly.svg"),
                ("evidence", ".evidence.json"),
            )
        )
        verification = json.dumps(
            {
                "operation": "run-evidence-input-and-artifact-verified",
                "ok": True,
                "input_identity_recomputed": True,
                "result_hash": "a" * 16,
                "result_recomputed": False,
                "verified_artifact_count": 5,
            },
            separators=(",", ":"),
        )
        return (
            f"{completed}\n{verification}\n"
            f"EvidenceBundle 已离线核验：{project / 'evidence.evidence.json'}\n"
            f"{assets}\nresult_hash={'a' * 16}\n"
        )

    def test_requires_one_command_to_complete_verified_visual_report(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            project = Path(temporary) / "project"
            project.mkdir()
            for name in (
                "html.html",
                "equity_svg.equity.svg",
                "fills_svg.fills.svg",
                "monthly_svg.monthly.svg",
                "evidence.evidence.json",
            ):
                content = (
                    "<!doctype html><html><body></body></html>"
                    if name == "html.html"
                    else "<svg xmlns='http://www.w3.org/2000/svg'></svg>"
                    if name.endswith(".svg")
                    else "{}"
                )
                (project / name).write_text(content, encoding="utf-8")

            result = verify_quickstart_acceptance(
                self.fixture_output(project), project=project, elapsed_ms=1234.5
            )

            self.assertEqual(result["command_count_to_verified_report"], 1)
            self.assertEqual(result["completed_steps"], list(QUICKSTART_COMPLETION_STEPS))
            self.assertEqual(result["quickstart_elapsed_ms"], 1234.5)
            self.assertEqual(set(result["report_artifacts"]), {
                "html", "equity_svg", "fills_svg", "monthly_svg", "evidence"
            })
            self.assertEqual(len(result["result_hash"]), 16)
            self.assertFalse(result["result_recomputed"])
            self.assertEqual(result["evidence_verified_artifact_count"], 5)

    def test_rejects_missing_or_out_of_order_steps(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            project = Path(temporary)
            output = self.fixture_output(project).replace("[完成] 静态检查", "[遗漏] 静态检查")
            with self.assertRaisesRegex(RuntimeError, "six documented steps"):
                verify_quickstart_acceptance(output, project=project, elapsed_ms=1)

    def test_rejects_missing_evidence_verification(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            project = Path(temporary)
            output = "\n".join(
                line
                for line in self.fixture_output(project).splitlines()
                if '"operation":"run-evidence-input-and-artifact-verified"' not in line
            )
            with self.assertRaisesRegex(RuntimeError, "offline EvidenceBundle verification"):
                verify_quickstart_acceptance(output, project=project, elapsed_ms=1)

    def test_rejects_artifacts_outside_the_project(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            project = Path(temporary) / "project"
            project.mkdir()
            outside = Path(temporary) / "outside.html"
            outside.write_text("outside", encoding="utf-8")
            output = self.fixture_output(project).replace(
                f"{project / 'html.html'}", str(outside)
            )
            with self.assertRaisesRegex(RuntimeError, "escaped its project directory"):
                verify_quickstart_acceptance(output, project=project, elapsed_ms=1)


if __name__ == "__main__":
    unittest.main()
