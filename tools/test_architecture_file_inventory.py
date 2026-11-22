"""Tests for the source inventory used by the architecture gate."""
from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "qx_architecture_gate", ROOT / "tools" / "check_architecture.py"
)
assert SPEC is not None and SPEC.loader is not None
GATE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = GATE
SPEC.loader.exec_module(GATE)


class ArchitectureFileInventoryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="qx-architecture-glob-")
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        for relative in (
            "qx-app/Cargo.toml",
            "qx-app/src/lib.rs",
            "qx-app/src/cases/run.rs",
            "crates/qx-app/src/lib.rs",
            "crates/qx-cli/src/main.rs",
            "qx-cli/Cargo.toml",
            "qx-cli/src/cli.rs",
            "qx-cli/src/workers/strategy_worker.rs",
            "qx-cli/src/run_evidence_multi_market_pack.rs",
            "target/generated/src/ignored.rs",
        ):
            path = self.base / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("// fixture\n", encoding="utf-8")

    def test_source_globs_match_pathlib_and_prune_generated_targets(self) -> None:
        cases = (
            ("*/src/**/*.rs", False),
            ("qx-app/src/**/*.rs", False),
            ("qx-cli/src/workers/**/*.rs", False),
            ("qx-cli/src/run_evidence_multi_market*.rs", False),
            ("*/Cargo.toml", False),
            ("crates/**/*.rs", False),
            ("*.rs", True),
            ("src/**/*.rs", True),
        )
        for pattern, recursive in cases:
            with self.subTest(pattern=pattern, recursive=recursive):
                pathlib_matches = set(
                    self.base.rglob(pattern) if recursive else self.base.glob(pattern)
                )
                pathlib_matches = {
                    path
                    for path in pathlib_matches
                    if "target" not in path.relative_to(self.base).parts
                }
                actual = set(GATE._glob_paths(self.base, pattern, recursive))
                self.assertEqual(actual, pathlib_matches)

    def test_target_is_pruned_before_directory_descent(self) -> None:
        inventory = GATE._source_file_inventory(self.base)
        self.assertFalse(any("target" in path.relative_to(self.base).parts for path in inventory))


if __name__ == "__main__":
    unittest.main()
