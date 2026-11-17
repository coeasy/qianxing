"""Concurrent local projects keep helper imports and input paths isolated."""
from __future__ import annotations

import json
import sys
import tempfile
import threading
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from qianxing import strategy_project


class ConcurrentStrategyProjectTests(unittest.TestCase):
    def _make_project(self, root: Path, project_id: str, marker: str) -> Path:
        project = root / project_id
        strategies = project / "strategies"
        strategies.mkdir(parents=True)
        (strategies / "helper.py").write_text(
            f"VALUE = {marker!r}\n", encoding="utf-8"
        )
        (strategies / "main.py").write_text(
            "import time\n"
            "import helper\n"
            "time.sleep(0.05)\n"
            "def create_strategy(spec):\n"
            "    from pathlib import Path\n"
            "    return {'helper': helper.VALUE, 'data': Path(spec['data_path']).read_text(encoding='utf-8')}\n",
            encoding="utf-8",
        )
        data = project / "inputs" / "data.txt"
        data.parent.mkdir()
        data.write_text(f"data-{marker}", encoding="utf-8")
        spec_path = project / "input.json"
        spec_path.write_text(json.dumps({"data_path": "inputs/data.txt"}), encoding="utf-8")
        (project / strategy_project.PROJECT_FILE).write_text(
            json.dumps(
                {
                    "schema_version": strategy_project.PROJECT_SCHEMA_VERSION,
                    "kind": strategy_project.PROJECT_KIND,
                    "project_id": project_id,
                    "strategy_file": "strategies/main.py",
                    "input_spec": "input.json",
                }
            ),
            encoding="utf-8",
        )
        return project

    def test_simultaneous_projects_do_not_cross_import_or_resolve_each_others_data(self):
        with tempfile.TemporaryDirectory(prefix="qx-concurrent-projects-") as directory:
            base = Path(directory)
            roots = [
                self._make_project(base, "project-alpha", "ALPHA"),
                self._make_project(base, "project-beta", "BETA"),
            ]
            barrier = threading.Barrier(len(roots))
            original_cwd = Path.cwd()
            original_path = tuple(sys.path)

            def load_and_run(root: Path) -> tuple[str, dict[str, str]]:
                barrier.wait(timeout=5)
                project_root, manifest = strategy_project.load_strategy_project(root)
                self.assertEqual(project_root, root.resolve())
                input_path = project_root / manifest["input_spec"]
                input_spec = json.loads(input_path.read_text(encoding="utf-8"))
                resolved, failures = strategy_project._resolve_input_paths(project_root, input_spec)
                self.assertEqual(failures, [])
                strategy_path = project_root / manifest["strategy_file"]
                _module, factory, _bundle_digest = strategy_project.load_strategy_file(strategy_path)
                return manifest["project_id"], factory(resolved)

            with patch.dict(sys.modules):
                sys.modules.pop("helper", None)
                with ThreadPoolExecutor(max_workers=2) as executor:
                    futures = [executor.submit(load_and_run, root) for root in roots]
                    results = [future.result(timeout=10) for future in futures]

            self.assertEqual(
                dict(results),
                {
                    "project-alpha": {"helper": "ALPHA", "data": "data-ALPHA"},
                    "project-beta": {"helper": "BETA", "data": "data-BETA"},
                },
            )
            self.assertEqual(Path.cwd(), original_cwd)
            self.assertEqual(tuple(sys.path), original_path)
            resolved_roots = {root.resolve() for root in roots}
            leaked = []
            for name, module in tuple(sys.modules.items()):
                module_path = getattr(module, "__file__", None)
                if not isinstance(module_path, str):
                    continue
                if any(
                    _is_within(Path(module_path), project_root)
                    for project_root in resolved_roots
                ):
                    leaked.append(name)
            self.assertEqual(leaked, [])
            self.assertNotIn("helper", sys.modules)


def _is_within(path: Path, root: Path) -> bool:
    try:
        path.resolve().relative_to(root)
    except (ValueError, OSError):
        return False
    return True


if __name__ == "__main__":
    unittest.main()
