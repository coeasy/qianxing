from __future__ import annotations

import importlib.util
import tarfile
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).resolve().parents[2] / "tools" / "package_cpp_sdk.py"
SPEC = importlib.util.spec_from_file_location("package_cpp_sdk", MODULE_PATH)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC is not None and SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class CppSdkPackageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.source = self.root / "sdk"
        (self.source / "include" / "nested").mkdir(parents=True)
        (self.source / "lib").mkdir()
        (self.source / "include" / "nested" / "strategy.hpp").write_bytes(b"header\n")
        (self.source / "lib" / "libqianxing.a").write_bytes(b"archive bytes")

    def tearDown(self):
        self.temporary.cleanup()

    def test_archive_is_reproducible_and_normalizes_metadata(self):
        first = self.root / "first.tar.gz"
        second = self.root / "second.tar.gz"
        MODULE.package_sdk(self.source, first)
        for path in self.source.rglob("*"):
            if path.is_file():
                path.touch()
        MODULE.package_sdk(self.source, second)

        self.assertEqual(first.read_bytes(), second.read_bytes())
        with tarfile.open(first, "r:gz") as archive:
            self.assertEqual(
                archive.getnames(),
                [
                    "sdk",
                    "sdk/include",
                    "sdk/include/nested",
                    "sdk/include/nested/strategy.hpp",
                    "sdk/lib",
                    "sdk/lib/libqianxing.a",
                ],
            )
            for member in archive.getmembers():
                self.assertEqual(member.mtime, 0)
                self.assertEqual((member.uid, member.gid), (0, 0))
                self.assertEqual((member.uname, member.gname), ("", ""))
            self.assertEqual(
                archive.extractfile("sdk/include/nested/strategy.hpp").read(), b"header\n"
            )

    def test_archive_refuses_output_inside_source_tree(self):
        with self.assertRaisesRegex(ValueError, "outside the installed SDK tree"):
            MODULE.package_sdk(self.source, self.source / "sdk.tar.gz")

    def test_archive_refuses_symlinks(self):
        link = self.source / "include" / "linked.hpp"
        try:
            link.symlink_to(self.source / "include" / "nested" / "strategy.hpp")
        except (OSError, NotImplementedError) as error:
            self.skipTest(f"symlink creation unavailable: {error}")
        with self.assertRaisesRegex(ValueError, "symlink"):
            MODULE.package_sdk(self.source, self.root / "symlink.tar.gz")


if __name__ == "__main__":
    unittest.main()
