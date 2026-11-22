import io
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
import urllib.error

from tools.cargo_publish_plan import build_plan, check_crates_io, inspect_crates_io
from tools.check_crates_io_artifacts import compare_artifacts
from tools.publish_rust_sdk import (
    inspect_registry_decisions,
    package_registry_archive,
    publication_decision,
    publish_one,
    validate_publish_request,
    validate_registry_consumer_output,
    wait_for_published_checksum,
)


def package(name, dependencies=(), publish=None):
    return {"name": name, "id": name, "version": "1.0.0", "publish": publish,
            "manifest_path": f"/workspace/{name}/Cargo.toml", "dependencies": list(dependencies)}


def dependency(name, kind=None, optional=False, path=None):
    return {"name": name, "req": "^1", "kind": kind, "optional": optional,
            "target": None, "path": path or f"/workspace/{name}"}


class PublishPlanTest(unittest.TestCase):
    def test_transitive_order_optional_build_and_dev_exclusion(self):
        packages = [package("sdk", [dependency("app"), dependency("test", "dev")]),
                    package("app", [dependency("core", "build", True)]),
                    package("core", publish=[]), package("test")]
        plan = build_plan({"workspace_members": [p["id"] for p in packages], "packages": packages}, "sdk")
        self.assertEqual(plan["publish_order"], ["core", "app", "sdk"])
        self.assertEqual(plan["packages"][0]["publish"], [])
        self.assertTrue(plan["packages"][1]["dependencies"][0]["optional"])
        self.assertIn("test", plan["workspace_packages_outside_closure"])
        self.assertEqual(plan["packages"][-1]["dev_path_dependencies"][0]["workspace_package"], "test")
        self.assertFalse(plan["registry_checked"])

    def test_cycle_and_outside_workspace_path_are_visible(self):
        packages = [package("sdk", [dependency("external", path="/elsewhere/external")])]
        plan = build_plan({"workspace_members": ["sdk"], "packages": packages}, "sdk")
        self.assertEqual(plan["packages"][0]["outside_workspace_path_dependencies"][0]["name"], "external")
        packages = [package("a", [dependency("b")]), package("b", [dependency("a")])]
        with self.assertRaisesRegex(ValueError, "dependency cycle"):
            build_plan({"workspace_members": ["a", "b"], "packages": packages}, "a")

    def test_registry_distinguishes_missing_name_versions_and_yanked(self):
        def not_found(request, timeout):
            self.assertEqual(timeout, 20)
            self.assertIn("qianxing-release-check", request.get_header("User-agent"))
            raise urllib.error.HTTPError(request.full_url, 404, "not found", {}, None)
        self.assertEqual(inspect_crates_io("qx-core", "0.4.0", opener=not_found)["status"], "no_registry_record")
        checksum = "a" * 64
        metadata = {"crate": {"id": "qx_core", "repository": "https://example.invalid/repo"},
                    "versions": [{"num": "0.4.0+build1", "yanked": True, "checksum": checksum},
                                 {"num": "0.3.0", "yanked": False, "checksum": "b" * 64}]}
        def open_metadata(request, timeout):
            return io.BytesIO(json.dumps(metadata).encode("utf-8"))
        result = inspect_crates_io("qx-core", "0.4.0", opener=open_metadata)
        self.assertEqual(result["status"], "version_exists")
        self.assertTrue(result["yanked"])
        self.assertEqual(result["crate"], "qx_core")
        self.assertEqual(result["checksum"], checksum)
        self.assertEqual(inspect_crates_io("qx-core", "0.5.0", opener=open_metadata)["status"], "name_exists")

    def test_registry_rejects_missing_or_malformed_version_checksum(self):
        def open_metadata(checksum):
            document = {"crate": {"id": "qx-core"},
                        "versions": [{"num": "0.4.0", "yanked": False, "checksum": checksum}]}
            return io.BytesIO(json.dumps(document).encode("utf-8"))
        for checksum in (None, "not-a-sha256", "a" * 63):
            with self.subTest(checksum=checksum), self.assertRaisesRegex(ValueError, "invalid checksum"):
                inspect_crates_io("qx-core", "0.4.0", opener=lambda request, timeout: open_metadata(checksum))

    def test_registry_failures_never_become_available_or_complete(self):
        def rejected(request, timeout):
            raise urllib.error.HTTPError(request.full_url, 429, "limited", {}, None)
        with self.assertRaisesRegex(ValueError, "HTTP 429"):
            inspect_crates_io("qx-core", "0.4.0", opener=rejected, sleep=lambda _: None)
        def invalid(request, timeout):
            return io.BytesIO(b'{"crate":{},"versions":[]}')
        with self.assertRaisesRegex(ValueError, "mismatched crate identity"):
            inspect_crates_io("qx-core", "0.4.0", opener=invalid)

    def test_registry_retries_transient_server_errors_with_bounded_backoff(self):
        calls, delays = [], []
        metadata = {"crate": {"id": "qx-core"}, "versions": []}
        def sometimes_available(request, timeout):
            calls.append(timeout)
            if len(calls) < 3:
                raise urllib.error.HTTPError(request.full_url, 503, "temporarily unavailable", {}, None)
            return io.BytesIO(json.dumps(metadata).encode("utf-8"))
        result = inspect_crates_io("qx-core", "0.4.0", opener=sometimes_available, sleep=delays.append)
        self.assertEqual(result["status"], "name_exists")
        self.assertEqual(calls, [20, 20, 20])
        self.assertEqual(delays, [1, 2])
        packages = [package("a"), package("b"), package("c")]
        plan = build_plan({"workspace_members": [p["id"] for p in packages], "packages": packages}, "a")
        plan["packages"] = packages
        calls, delays = [], []
        def inspect(name, version):
            calls.append(name)
            if name == "b":
                raise ValueError("offline")
            return {"status": "no_registry_record"}
        self.assertFalse(check_crates_io(plan, inspect=inspect, sleep=delays.append))
        self.assertEqual(calls, ["a", "b"])
        self.assertEqual(delays, [1])
        self.assertEqual([p["crates_io"]["status"] for p in packages],
                         ["no_registry_record", "query_failed", "not_checked"])
        self.assertFalse(plan["registry_checked"])

    def test_registry_check_completes_all_entries_without_claiming_permission(self):
        packages = [package("a"), package("b")]
        plan = {"packages": packages, "registry_checked": False}
        delays = []
        self.assertTrue(check_crates_io(
            plan, inspect=lambda name, version: {"status": "name_exists", "crate": name},
            sleep=delays.append,
        ))
        self.assertEqual(delays, [1])
        self.assertTrue(plan["registry_checked"])
        self.assertEqual([item["crates_io"]["status"] for item in packages], ["name_exists", "name_exists"])


class CratesIoArtifactCheckTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.package_dir = Path(self.temp.name)
        self.archive = self.package_dir / "qx-core-0.4.0.crate"
        self.archive.write_bytes(b"reproducible crate archive")
        self.plan = {"root": "qx-sdk", "packages": [{"name": "qx-core", "version": "0.4.0"}]}
        self.checksum = hashlib.sha256(self.archive.read_bytes()).hexdigest()

    def test_existing_version_must_match_exact_archive_and_not_be_yanked(self):
        result = compare_artifacts(self.plan, self.package_dir,
                                   inspect=lambda name, version: {"status": "version_exists",
                                                                  "checksum": self.checksum, "yanked": False},
                                   sleep=lambda _: None)
        self.assertTrue(result["registry_checked"])
        self.assertEqual(result["packages"][0]["status"], "published_artifact_matches")
        mismatch = compare_artifacts(self.plan, self.package_dir,
                                     inspect=lambda name, version: {"status": "version_exists",
                                                                    "checksum": "0" * 64, "yanked": False},
                                     sleep=lambda _: None)
        self.assertFalse(mismatch["registry_checked"])
        self.assertEqual(mismatch["packages"][0]["status"], "published_artifact_mismatch")
        yanked = compare_artifacts(self.plan, self.package_dir,
                                   inspect=lambda name, version: {"status": "version_exists",
                                                                  "checksum": self.checksum, "yanked": True},
                                   sleep=lambda _: None)
        self.assertEqual(yanked["packages"][0]["status"], "published_version_yanked")

    def test_new_names_and_versions_do_not_claim_permission(self):
        for remote, expected in (({"status": "no_registry_record"}, "crate_name_not_found_at_query_time"),
                                 ({"status": "name_exists"}, "version_not_registered")):
            with self.subTest(status=remote["status"]):
                result = compare_artifacts(self.plan, self.package_dir,
                                           inspect=lambda name, version: remote, sleep=lambda _: None)
                self.assertTrue(result["registry_checked"])
                self.assertEqual(result["packages"][0]["status"], expected)
                self.assertIn("does not prove crate ownership", result["note"])

    def test_missing_archive_and_registry_failure_stop_check(self):
        self.archive.unlink()
        result = compare_artifacts(self.plan, self.package_dir,
                                   inspect=lambda name, version: self.fail("must not query registry"),
                                   sleep=lambda _: None)
        self.assertFalse(result["registry_checked"])
        self.assertEqual(result["packages"][0]["status"], "local_archive_missing")
        self.archive.write_bytes(b"reproducible crate archive")
        failed = compare_artifacts(self.plan, self.package_dir,
                                   inspect=lambda name, version: (_ for _ in ()).throw(ValueError("offline")),
                                   sleep=lambda _: None)
        self.assertEqual(failed["packages"][0]["status"], "query_failed")


class RustSdkPublisherGuardTest(unittest.TestCase):
    def test_publish_workflow_scopes_registry_token_to_required_steps(self):
        workflow = (Path(__file__).resolve().parents[1] / ".github" / "workflows"
                    / "publish-rust-crates.yml").read_text(encoding="utf-8")
        binding = "CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}"
        self.assertEqual(workflow.count(binding), 2)
        self.assertNotRegex(workflow, r"(?m)^    env:\s*$")
        self.assertRegex(
            workflow,
            r"(?ms)^      - name: Require a tagged release and configured publisher token\n"
            r"        env:\n          CARGO_REGISTRY_TOKEN: \$\{\{ secrets\.CARGO_REGISTRY_TOKEN \}\}",
        )
        self.assertRegex(
            workflow,
            r"(?ms)^      - name: Publish crates in dependency order and verify a clean registry consumer\n"
            r"        env:\n          CARGO_REGISTRY_TOKEN: \$\{\{ secrets\.CARGO_REGISTRY_TOKEN \}\}",
        )
        self.assertIn("path: publisher", workflow)
        self.assertIn("path: release-source", workflow)
        self.assertIn("ref: ${{ github.event.repository.default_branch }}", workflow)
        self.assertIn("ref: ${{ inputs.release_tag }}", workflow)
        self.assertIn("--root release-source", workflow)

    def test_registry_decision_is_resumable_but_conflicts_fail_closed(self):
        self.assertEqual(publication_decision("version_exists"), "verify_existing")
        self.assertEqual(publication_decision("version_not_registered"), "publish")
        self.assertEqual(publication_decision("crate_name_not_found_at_query_time"), "publish")
        for status in ("published_artifact_mismatch", "published_version_yanked", "query_failed"):
            with self.subTest(status=status), self.assertRaisesRegex(ValueError, "refusing to publish"):
                publication_decision(status)

    def test_registry_inventory_rejects_yanked_and_keeps_existing_versions_for_archive_verification(self):
        plan = {"packages": [package("a"), package("b")]}
        responses = iter(({"status": "version_exists", "checksum": "a" * 64, "yanked": False},
                          {"status": "no_registry_record"}))
        decisions = inspect_registry_decisions(plan, inspect=lambda *_: next(responses), sleep=lambda _: None)
        self.assertEqual([item["action"] for item in decisions], ["verify_existing", "publish"])
        self.assertEqual(decisions[0]["registry_checksum"], "a" * 64)
        with self.assertRaisesRegex(ValueError, "published_version_yanked"):
            inspect_registry_decisions(
                {"packages": [package("a")]},
                inspect=lambda *_: {"status": "version_exists", "checksum": "a" * 64, "yanked": True},
                sleep=lambda _: None,
            )

    def test_existing_registry_archive_is_rebuilt_without_preflight_patches(self):
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "source"
            target = root / "target"
            archive = target / "package" / "qx-core-0.4.1.crate"
            archive.parent.mkdir(parents=True)
            archive.write_bytes(b"canonical cargo package")
            with patch("tools.publish_rust_sdk.subprocess.run") as run:
                checksum = package_registry_archive(
                    root, "qx-core", "0.4.1", target=target,
                    cargo_home=Path(temporary) / "cargo-home", environment={},
                )
            command = run.call_args.args[0]
            self.assertIn("--registry", command)
            self.assertIn("crates-io", command)
            self.assertNotIn("--config", command)
            self.assertEqual(checksum, hashlib.sha256(b"canonical cargo package").hexdigest())

    def test_upload_checksum_comes_from_cargo_archive_created_by_publish(self):
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "source"
            target = root / "target"
            archive = target / "package" / "qx-core-0.4.1.crate"
            archive.parent.mkdir(parents=True)
            archive.write_bytes(b"stale local-patch archive")

            def cargo_publish(command, *, cwd, env, check):
                self.assertEqual(command[:3], ["cargo", "publish", "-p"])
                self.assertFalse(archive.exists(), "publish must not reuse a preflight archive")
                archive.write_bytes(b"fresh archive cargo publish uploads")

            with patch("tools.publish_rust_sdk.subprocess.run", side_effect=cargo_publish) as run:
                checksum = publish_one(
                    root, "qx-core", "0.4.1", target=target,
                    cargo_home=Path(temporary) / "cargo-home", environment={},
                )
            self.assertEqual(run.call_args.args[0][:3], ["cargo", "publish", "-p"])
            self.assertEqual(checksum, hashlib.sha256(b"fresh archive cargo publish uploads").hexdigest())

    def test_publish_requires_exact_package_version_clean_tag_and_token(self):
        valid = {"package": "qx-sdk", "version": "0.4.0", "publish": True,
                 "confirmation": "qx-sdk@0.4.0", "token": "secret-token",
                 "clean": True, "tag": "v0.4.0"}
        validate_publish_request(**valid)
        for key, value in (("confirmation", "qx-sdk@0.4.1"), ("token", None),
                           ("clean", False), ("tag", "main"), ("package", "qx-cli")):
            invalid = {**valid, key: value}
            with self.subTest(key=key), self.assertRaises(ValueError):
                validate_publish_request(**invalid)
        validate_publish_request(package="qx-sdk", version="0.4.0", publish=False,
                                 confirmation=None, token=None, clean=False, tag=None)

    def test_registry_confirmation_waits_for_expected_checksum(self):
        responses = iter(({"status": "name_exists"},
                          {"status": "version_exists", "checksum": "a" * 64, "yanked": False}))
        delays = []
        wait_for_published_checksum("qx-core", "0.4.0", "a" * 64,
                                    inspect=lambda name, version: next(responses),
                                    sleep=delays.append, attempts=3)
        self.assertEqual(delays, [10])
        with self.assertRaisesRegex(ValueError, "checksum differs"):
            wait_for_published_checksum("qx-core", "0.4.0", "a" * 64,
                                        inspect=lambda name, version: {"status": "version_exists",
                                                                       "checksum": "b" * 64, "yanked": False},
                                        sleep=lambda _: None, attempts=1)

    def test_clean_registry_consumer_must_run_and_verify_expected_quickstart(self):
        validate_registry_consumer_output("Bars: 70; fills: 1; result hash: abc\nVerified: true; output: runs/x")
        with self.assertRaisesRegex(ValueError, "did not verify"):
            validate_registry_consumer_output("Bars: 70; fills: 1; result hash: abc\nVerified: false")


if __name__ == "__main__":
    unittest.main()
