"""Managed project-run import audit covers archived, removed, and opaque origins."""
from __future__ import annotations

import builtins
import contextlib
import gc
import hashlib
import importlib
import importlib.abc
import io
import json
import sys
import tempfile
import unittest
import zipfile
import zipimport
from importlib import metadata as importlib_metadata
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from qianxing import project_runs


class StrategyProjectImportAuditTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        project_runs._ensure_runtime_import_hook()

    def _managed_inventory(self, root: Path, action):
        root.mkdir(parents=True)
        strategy = root / "main.py"
        strategy.write_text("def create_strategy(spec):\n    return None\n", encoding="utf-8")
        (root / "qianxing.strategy.json").write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "kind": "qianxing-strategy-project",
                    "project_id": root.name,
                    "strategy_file": strategy.name,
                    "input_spec": None,
                }
            ),
            encoding="utf-8",
        )

        def execute(project_root, _manifest):
            project_runs.snapshot_project_strategy(project_root, strategy)
            action()
            return 1  # Preserve the failed run receipt without needing result artifacts.

        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(project_runs.run_project(root, execute), 1)
        record, = project_runs.project_runs(root)
        self.assertEqual(record["status"], "failed")
        return record["runtime_import_files_completed"]

    def test_zip_import_remains_fingerprinted_after_module_is_removed(self):
        with tempfile.TemporaryDirectory(prefix="qx-import-audit-zip-") as directory:
            base = Path(directory)
            root = base / "project"
            archive = base / "runtime-fixture.zip"
            module_name = "qx_zip_audit_fixture"
            with zipfile.ZipFile(archive, "w") as output:
                output.writestr(f"{module_name}.py", "VALUE = 'zip-ok'\n")

            original_path = list(sys.path)
            sys.path.insert(0, str(archive))
            try:
                def import_then_remove():
                    module = importlib.import_module(module_name)
                    self.assertEqual(module.VALUE, "zip-ok")
                    sys.modules.pop(module_name, None)

                inventory = self._managed_inventory(root, import_then_remove)
            finally:
                sys.path[:] = original_path
                sys.modules.pop(module_name, None)
                sys.path_importer_cache.pop(str(archive), None)
                zipimport._zip_directory_cache.pop(str(archive), None)
                # The audit's distribution scan makes importlib.metadata cache a
                # zipp handle for every sys.path entry, which keeps the archive
                # locked on Windows until the cache is dropped and collected.
                importlib_metadata.MetadataPathFinder.invalidate_caches()
                gc.collect()

            record = next(
                entry
                for entry in inventory["files"]
                if Path(entry["path"]).resolve() == archive.resolve()
            )
            self.assertEqual(record["sha256"], hashlib.sha256(archive.read_bytes()).hexdigest())
            observation = next(
                entry for entry in inventory["imports"] if entry["module_name"] == module_name
            )
            self.assertEqual(observation["coverage"], "container")
            self.assertEqual(
                project_runs._verify_runtime_import_files(inventory), "matched"
            )

    def test_removed_file_module_origin_is_hashed_and_remains_in_import_manifest(self):
        with tempfile.TemporaryDirectory(prefix="qx-import-audit-file-") as directory:
            base = Path(directory)
            root = base / "project"
            module_name = "qx_removed_file_audit_fixture"
            source = base / f"{module_name}.py"
            source.write_text("VALUE = 'file-ok'\n", encoding="utf-8")
            original_path = list(sys.path)
            sys.path.insert(0, str(base))

            def import_then_remove():
                module = importlib.import_module(module_name)
                self.assertEqual(module.VALUE, "file-ok")
                sys.modules.pop(module_name, None)

            try:
                inventory = self._managed_inventory(root, import_then_remove)
            finally:
                sys.path[:] = original_path
                sys.modules.pop(module_name, None)
            record = next(
                entry
                for entry in inventory["files"]
                if Path(entry["path"]).resolve() == source.resolve()
            )
            self.assertEqual(record["sha256"], hashlib.sha256(source.read_bytes()).hexdigest())
            observation = next(
                entry for entry in inventory["imports"] if entry["module_name"] == module_name
            )
            self.assertEqual(observation["origin"], str(source))
            self.assertEqual(observation["coverage"], "file")
            self.assertEqual(
                project_runs._verify_runtime_import_files(inventory), "matched"
            )

    def test_opaque_importer_is_explicitly_unverifiable(self):
        with tempfile.TemporaryDirectory(prefix="qx-import-audit-opaque-") as directory:
            root = Path(directory) / "project"
            module_name = "qx_opaque_audit_fixture"
            class OpaqueLoader(importlib.abc.Loader):
                def create_module(self, spec):
                    return None

                def exec_module(self, module):
                    module.VALUE = "opaque-ok"

            class OpaqueFinder(importlib.abc.MetaPathFinder):
                def find_spec(self, fullname, path=None, target=None):
                    if fullname == module_name:
                        return importlib.util.spec_from_loader(
                            fullname, OpaqueLoader(), origin="<opaque importer>"
                        )
                    return None

            finder = OpaqueFinder()
            sys.meta_path.insert(0, finder)
            try:
                def import_opaque_module():
                    module = builtins.__import__(module_name)
                    self.assertEqual(module.VALUE, "opaque-ok")

                inventory = self._managed_inventory(root, import_opaque_module)
            finally:
                sys.modules.pop(module_name, None)
                sys.meta_path.remove(finder)

            observation = next(
                entry for entry in inventory["imports"] if entry["module_name"] == module_name
            )
            self.assertEqual(observation["coverage"], "opaque")
            self.assertEqual(
                project_runs._verify_runtime_import_files(inventory),
                "unresolved_imports",
            )


if __name__ == "__main__":
    unittest.main()
