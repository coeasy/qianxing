"""Release archive contract for the static Web console (M4'/M5')."""

import importlib.util
import json
import tarfile
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / "tools" / "package_web_console.py"
SPEC = importlib.util.spec_from_file_location("package_web_console", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class WebConsolePackageTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.source.mkdir()
        (self.source / "index.html").write_text(
            '<link rel="stylesheet" href="styles.css"><script src="app.js"></script>',
            encoding="utf-8",
        )
        (self.source / "app.js").write_text("const ready = true;\n", encoding="utf-8")
        (self.source / "styles.css").write_text("body { color: black; }\n", encoding="utf-8")
        self.version = "0.1.0"
        self.commit = "0123456789abcdef0123456789abcdef01234567"

    def tearDown(self):
        self.temporary.cleanup()

    def test_archive_is_reproducible_and_contains_verified_identity(self):
        first = self.root / "first.tar.gz"
        second = self.root / "second.tar.gz"
        identity = MODULE.package_console(self.source, first, self.version, self.commit)
        MODULE.package_console(self.source, second, self.version, self.commit)

        self.assertEqual(first.read_bytes(), second.read_bytes())
        self.assertEqual(identity["git_commit"], self.commit)
        self.assertEqual(identity["schema_registry_version"], MODULE._schema_registry_version())
        with tarfile.open(first, "r:gz") as archive:
            names = sorted(archive.getnames())
            self.assertEqual(
                names,
                [
                    "qianxing-web-console/app.js",
                    "qianxing-web-console/index.html",
                    "qianxing-web-console/release-identity.json",
                    "qianxing-web-console/styles.css",
                ],
            )
            packed_identity = json.load(
                archive.extractfile("qianxing-web-console/release-identity.json")
            )
            self.assertEqual(packed_identity, identity)
            for item in packed_identity["assets"]:
                asset = archive.extractfile(f"qianxing-web-console/{item['path']}").read()
                self.assertEqual(MODULE.hashlib.sha256(asset).hexdigest(), item["sha256"])

    def test_unavailable_or_external_resources_are_rejected(self):
        (self.source / "index.html").write_text(
            '<link rel="stylesheet" href="https://cdn.example/style.css">'
            '<script src="app.js"></script>',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "local relative URLs"):
            MODULE.package_console(self.source, self.root / "bad.tar.gz", self.version, self.commit)

        (self.source / "index.html").write_text(
            '<link rel="stylesheet" href="styles.css"><script src="missing.js"></script>',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "unpackaged console asset reference"):
            MODULE.package_console(self.source, self.root / "missing.tar.gz", self.version, self.commit)

    def test_release_identity_rejects_invalid_version_and_short_commit(self):
        with self.assertRaisesRegex(ValueError, "invalid release version"):
            MODULE.package_console(self.source, self.root / "bad-version.tar.gz", "latest", self.commit)
        with self.assertRaisesRegex(ValueError, "40-character"):
            MODULE.package_console(self.source, self.root / "bad-commit.tar.gz", self.version, "0123")


if __name__ == "__main__":
    unittest.main()
