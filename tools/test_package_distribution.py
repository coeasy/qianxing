import unittest

from tools.check_package_distribution import direct_consumers, validate_distribution


def package(name, dependencies=(), publish=None):
    return {
        "name": name,
        "id": name,
        "version": "0.4.0",
        "rust_version": "1.87",
        "publish": publish,
        "description": f"{name} package",
        "license": "MIT",
        "repository": "https://example.invalid/qianxing",
        "manifest_path": f"/workspace/{name}/Cargo.toml",
        "dependencies": list(dependencies),
    }


def dependency(name, kind=None, optional=False):
    return {
        "name": name,
        "req": "^0.4.0",
        "kind": kind,
        "optional": optional,
        "target": None,
        "path": f"/workspace/{name}",
    }


def catalog(*entries):
    return {
        "schema_version": 2,
        "sdk_root": "qx-sdk",
        "minimum_rust_version": "1.87",
        "packages": list(entries),
    }


def entry(name, role, channel, owner=None):
    return {"name": name, "role": role, "channel": channel, "owner": owner or name}


class PackageDistributionTest(unittest.TestCase):
    def test_sdk_closure_and_workspace_only_packages_are_separated(self):
        metadata = {
            "workspace_members": ["qx-sdk", "qx-core", "qx-cli", "contract-tests"],
            "packages": [
                package("qx-sdk", [dependency("qx-core")]),
                package("qx-core"),
                package("qx-cli", publish=[]),
                package("contract-tests", publish=[]),
            ],
        }
        policy = catalog(
            entry("qx-sdk", "sdk-facade", "crates-io"),
            entry("qx-core", "sdk-dependency", "crates-io"),
            entry("qx-cli", "cli", "github-release"),
            entry("contract-tests", "test-only", "ci-only"),
        )
        self.assertEqual(validate_distribution(metadata, policy), [])

    def test_workspace_consumer_map_includes_direct_consumers(self):
        metadata = {
            "workspace_members": ["qx-sdk", "qx-core", "qx-cli"],
            "packages": [
                package("qx-sdk", [dependency("qx-core")]),
                package("qx-core"),
                package("qx-cli", [dependency("qx-sdk")]),
            ],
        }
        consumers = direct_consumers(metadata)
        self.assertEqual(consumers["qx-sdk"][0]["package"], "qx-cli")
        self.assertEqual(consumers["qx-core"][0]["package"], "qx-sdk")

    def test_unclassified_or_workspace_publishable_crates_fail_closed(self):
        metadata = {
            "workspace_members": ["qx-sdk", "qx-core", "qx-cli", "qx-new"],
            "packages": [
                package("qx-sdk", [dependency("qx-core")]),
                package("qx-core"),
                package("qx-cli"),
                package("qx-new", publish=[]),
            ],
        }
        policy = catalog(
            entry("qx-sdk", "sdk-facade", "crates-io"),
            entry("qx-core", "sdk-dependency", "crates-io"),
            entry("qx-cli", "cli", "github-release"),
            entry("qx-new", "internal", "workspace-only"),
        )
        errors = validate_distribution(metadata, policy)
        self.assertTrue(any("non-SDK package qx-cli must set publish = false" in item for item in errors))

    def test_new_workspace_package_must_be_classified(self):
        metadata = {
            "workspace_members": ["qx-sdk", "qx-core", "qx-new"],
            "packages": [
                package("qx-sdk", [dependency("qx-core")]),
                package("qx-core"),
                package("qx-new", publish=[]),
            ],
        }
        policy = catalog(
            entry("qx-sdk", "sdk-facade", "crates-io"),
            entry("qx-core", "sdk-dependency", "crates-io"),
        )
        errors = validate_distribution(metadata, policy)
        self.assertTrue(any("workspace packages are unclassified: qx-new" in item for item in errors))

    def test_new_sdk_dependency_cannot_expand_release_scope_silently(self):
        metadata = {
            "workspace_members": ["qx-sdk", "qx-core", "qx-app"],
            "packages": [
                package("qx-sdk", [dependency("qx-core"), dependency("qx-app")]),
                package("qx-core"),
                package("qx-app"),
            ],
        }
        policy = catalog(
            entry("qx-sdk", "sdk-facade", "crates-io"),
            entry("qx-core", "sdk-dependency", "crates-io"),
            entry("qx-app", "internal", "workspace-only"),
        )
        errors = validate_distribution(metadata, policy)
        self.assertTrue(any("SDK publish closure and distribution catalog differ" in item for item in errors))

    def test_sdk_closure_packages_must_declare_catalog_msrv(self):
        metadata = {
            "workspace_members": ["qx-sdk", "qx-core"],
            "packages": [
                package("qx-sdk", [dependency("qx-core")]),
                package("qx-core"),
            ],
        }
        metadata["packages"][1]["rust_version"] = "1.86"
        policy = catalog(
            entry("qx-sdk", "sdk-facade", "crates-io"),
            entry("qx-core", "sdk-dependency", "crates-io"),
        )
        errors = validate_distribution(metadata, policy)
        self.assertTrue(any("SDK package qx-core must declare rust-version '1.87'" in item for item in errors))

    def test_package_catalog_must_declare_msrv(self):
        metadata = {
            "workspace_members": ["qx-sdk", "qx-core"],
            "packages": [
                package("qx-sdk", [dependency("qx-core")]),
                package("qx-core"),
            ],
        }
        policy = catalog(
            entry("qx-sdk", "sdk-facade", "crates-io"),
            entry("qx-core", "sdk-dependency", "crates-io"),
        )
        del policy["minimum_rust_version"]
        errors = validate_distribution(metadata, policy)
        self.assertTrue(any("package catalog must declare minimum_rust_version" in item for item in errors))


if __name__ == "__main__":
    unittest.main()
