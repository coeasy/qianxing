from __future__ import annotations

import unittest

from tools.generate_cargo_sbom import build_spdx


COMMIT = "0123456789abcdef0123456789abcdef01234567"


class CargoSbomTests(unittest.TestCase):
    def setUp(self) -> None:
        self.metadata = {
            "workspace_root": "C:/work/qianxing",
            "workspace_members": ["path+file:///C:/work/qianxing/crates/app#app@0.4.0"],
            "packages": [
                {
                    "id": "path+file:///C:/work/qianxing/crates/app#app@0.4.0",
                    "name": "app",
                    "version": "0.4.0",
                    "source": None,
                    "manifest_path": "C:/work/qianxing/crates/app/Cargo.toml",
                    "license": "Apache-2.0",
                },
                {
                    "id": "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0",
                    "name": "serde",
                    "version": "1.0.0",
                    "source": "registry+https://github.com/rust-lang/crates.io-index",
                    "manifest_path": "C:/cargo/registry/serde/Cargo.toml",
                    "license": "MIT OR Apache-2.0",
                },
            ],
            "resolve": {
                "nodes": [
                    {
                        "id": "path+file:///C:/work/qianxing/crates/app#app@0.4.0",
                        "deps": [{"pkg": "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0"}],
                    },
                    {
                        "id": "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0",
                        "deps": [],
                    },
                ],
            },
        }

    def test_emits_spdx_document_and_dependency_relationships(self) -> None:
        sbom = build_spdx(self.metadata, commit=COMMIT, created_epoch=0)

        self.assertEqual(sbom["spdxVersion"], "SPDX-2.3")
        self.assertEqual(sbom["dataLicense"], "CC0-1.0")
        self.assertEqual(sbom["creationInfo"]["created"], "1970-01-01T00:00:00Z")
        self.assertIn(COMMIT, sbom["documentNamespace"])
        packages = {package["name"]: package for package in sbom["packages"]}
        self.assertEqual(packages["app"]["sourceInfo"], "workspace")
        self.assertEqual(packages["serde"]["licenseDeclared"], "MIT OR Apache-2.0")
        self.assertEqual(packages["serde"]["externalRefs"][0]["referenceType"], "purl")
        self.assertFalse(any("C:/work/qianxing" in str(package) for package in sbom["packages"]))
        relationships = {
            (item["spdxElementId"], item["relationshipType"], item["relatedSpdxElement"])
            for item in sbom["relationships"]
        }
        self.assertIn(("SPDXRef-DOCUMENT", "DESCRIBES", packages["app"]["SPDXID"]), relationships)
        self.assertIn((packages["app"]["SPDXID"], "DEPENDS_ON", packages["serde"]["SPDXID"]), relationships)

    def test_output_is_deterministic_for_same_revision(self) -> None:
        first = build_spdx(self.metadata, commit=COMMIT, created_epoch=123)
        second = build_spdx(self.metadata, commit=COMMIT, created_epoch=123)

        self.assertEqual(first, second)

    def test_rejects_incomplete_dependency_graph(self) -> None:
        invalid = {**self.metadata, "resolve": None}
        with self.assertRaisesRegex(ValueError, "resolved dependency graph"):
            build_spdx(invalid, commit=COMMIT, created_epoch=0)


if __name__ == "__main__":
    unittest.main()
