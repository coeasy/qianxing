from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from benchmarks.source_fingerprint import measurement_source_fingerprint


class MeasurementSourceFingerprintTests(unittest.TestCase):
    def test_workspace_release_version_does_not_change_source_fingerprint(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            crate = root / "crates" / "qx-demo"
            crate.mkdir(parents=True)
            (root / "Cargo.toml").write_text(
                '[workspace.package]\nversion = "0.2.0"\n', encoding="utf-8"
            )
            (crate / "Cargo.toml").write_text(
                '[package]\nname = "qx-demo"\nversion = "0.2.0"\n', encoding="utf-8"
            )
            (crate / "src").mkdir()
            (crate / "src" / "lib.rs").write_text("pub fn value() -> u8 { 1 }\n", encoding="utf-8")
            (root / "Cargo.lock").write_text(
                '[[package]]\nname = "qx-demo"\nversion = "0.2.0"\n\n'
                '[[package]]\nname = "external"\nversion = "0.2.0"\n',
                encoding="utf-8",
            )
            before = measurement_source_fingerprint(root)
            (root / "Cargo.toml").write_text(
                '[workspace.package]\nversion = "0.2.1"\n', encoding="utf-8"
            )
            (crate / "Cargo.toml").write_text(
                '[package]\nname = "qx-demo"\nversion = "0.2.1"\n', encoding="utf-8"
            )
            (root / "Cargo.lock").write_text(
                '[[package]]\nname = "qx-demo"\nversion = "0.2.1"\n\n'
                '[[package]]\nname = "external"\nversion = "0.2.0"\n',
                encoding="utf-8",
            )
            self.assertEqual(before, measurement_source_fingerprint(root))

    def test_rust_source_change_changes_fingerprint(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            crate = root / "crates" / "qx-demo"
            crate.mkdir(parents=True)
            (root / "Cargo.toml").write_text(
                '[workspace.package]\nversion = "0.2.1"\n', encoding="utf-8"
            )
            (crate / "src").mkdir()
            source = crate / "src" / "lib.rs"
            source.write_text("pub fn value() -> u8 { 1 }\n", encoding="utf-8")
            before = measurement_source_fingerprint(root)
            source.write_text("pub fn value() -> u8 { 2 }\n", encoding="utf-8")
            self.assertNotEqual(before, measurement_source_fingerprint(root))


if __name__ == "__main__":
    unittest.main()
