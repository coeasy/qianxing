import io
import tarfile
import tempfile
import unittest
from pathlib import Path

from tools.check_rust_sdk_packages import build_environment, unpack_archive, write_patches


class PackageBuildEnvironmentTest(unittest.TestCase):
    def test_cargo_target_is_scoped_to_the_temporary_closure(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            environment = build_environment(root)
            self.assertEqual(Path(environment["CARGO_TARGET_DIR"]), root / "cargo-target")
            self.assertNotEqual(environment["CARGO_TARGET_DIR"], str(Path("target/qx-package-closure-build")))


class PackageArchiveTest(unittest.TestCase):
    def make_archive(self, root, entries):
        archive = root / "test.crate"
        with tarfile.open(archive, "w:gz") as bundle:
            for name, contents, kind in entries:
                item = tarfile.TarInfo(name)
                if kind == "symlink":
                    item.type = tarfile.SYMTYPE
                    item.linkname = "../../outside"
                    bundle.addfile(item)
                else:
                    item.size = len(contents)
                    bundle.addfile(item, io.BytesIO(contents))
        return archive

    def test_unpacks_only_expected_regular_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            archive = self.make_archive(root, [("example-1.0.0/Cargo.toml", b"[package]", "file")])
            result = unpack_archive(archive, root / "unpacked", "example-1.0.0")
            self.assertEqual((result / "Cargo.toml").read_bytes(), b"[package]")
            config = root / "patches.toml"
            write_patches(config, [("example", result)])
            self.assertIn('"example" = { path = ', config.read_text(encoding="utf-8"))

    def test_rejects_traversal_symlinks_and_windows_aliases(self):
        unsafe = [
            ("example-1.0.0/../../outside", "file"),
            ("example-1.0.0/link", "symlink"),
            ("example-1.0.0/..\\outside", "file"),
            ("example-1.0.0/file:stream", "file"),
            ("example-1.0.0/.task-tmp/test-output.csv", "file"),
            ("example-1.0.0/__pycache__/module.pyc", "file"),
        ]
        for name, kind in unsafe:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                archive = self.make_archive(root, [(name, b"bad", kind)])
                with self.assertRaisesRegex(ValueError, "unsafe entry"):
                    unpack_archive(archive, root / "unpacked", "example-1.0.0")
                self.assertFalse((root / "outside").exists())


if __name__ == "__main__":
    unittest.main()
