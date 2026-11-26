import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = REPOSITORY_ROOT / "tools" / "record_strategy_acceptance.py"
SPEC = importlib.util.spec_from_file_location("record_strategy_acceptance", MODULE_PATH)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class StrategyAcceptanceSourceManifestTests(unittest.TestCase):
    def test_manifest_excludes_local_tool_and_runtime_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(["git", "init", "--quiet"], cwd=root, check=True)

            tracked = root / "crates" / "qx-core" / "src" / "lib.rs"
            tracked.parent.mkdir(parents=True)
            tracked.write_text("pub fn stable_source() {}\n", encoding="utf-8")
            subprocess.run(["git", "add", "crates/qx-core/src/lib.rs"], cwd=root, check=True)

            generated = [
                ".codex-tmp-build/cache/build.log",
                "python/.codex-tmp-build/cache/build.log",
                "crates/qx-cli/.task-tmp/generated/config.json",
                ".tmp-release-prep/build.log",
                "python/python/.tmp/temp.log",
                "runs/sample/run.json",
                "python/runs/sample/run.json",
                "runtime.sqlite3",
                "qx-audit-debug-sample/runtime.zip",
                "maturity/acceptance-artifacts/old.log",
            ]
            for relative in generated:
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("ephemeral\n", encoding="utf-8")

            original_root = MODULE.ROOT
            try:
                MODULE.ROOT = root
                manifest = MODULE.tracked_source_manifest(root / "maturity/acceptance-artifacts/current.log")
            finally:
                MODULE.ROOT = original_root

            paths = {entry["path"] for entry in manifest["files"]}
            self.assertEqual(paths, {"crates/qx-core/src/lib.rs"})

    def test_ci_staging_includes_only_current_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "repo"
            runner_temp = Path(directory) / "runner-temp"
            current_log = root / "maturity" / "acceptance-artifacts" / "current.log"
            current_manifest = root / "maturity" / "acceptance-artifacts" / "current.source-tree.json"
            old_log = root / "maturity" / "acceptance-artifacts" / "old.log"
            for path, content in (
                (current_log, "current run\n"),
                (current_manifest, '{"source_tree_sha256":"abc"}\n'),
                (old_log, "historical run\n"),
            ):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content, encoding="utf-8")

            staged = MODULE.stage_ci_acceptance_artifacts(
                [current_log, current_manifest],
                repository_root=root,
                runner_temp=runner_temp,
            )

            self.assertEqual(staged, runner_temp / "qianxing-acceptance-evidence")
            staged_files = {
                path.relative_to(staged).as_posix()
                for path in staged.rglob("*")
                if path.is_file()
            }
            self.assertEqual(
                staged_files,
                {
                    "maturity/acceptance-artifacts/current.log",
                    "maturity/acceptance-artifacts/current.source-tree.json",
                },
            )

    def test_ci_staging_rejects_paths_outside_repository(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "repo"
            outside = Path(directory) / "outside.log"
            outside.write_text("not repository evidence\n", encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "inside the repository"):
                MODULE.stage_ci_acceptance_artifacts(
                    [outside],
                    repository_root=root,
                    runner_temp=Path(directory) / "runner-temp",
                )


if __name__ == "__main__":
    unittest.main()
