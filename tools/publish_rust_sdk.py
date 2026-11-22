#!/usr/bin/env python3
"""Preflight or explicitly publish the qx-sdk crates.io dependency closure.

Default mode is read-only except for building local Cargo package archives. An
upload requires --publish, an exact confirmation string, a clean vX.Y.Z tag,
and CARGO_REGISTRY_TOKEN in the environment. A failed partial release is safe
to resume: already-published packages are skipped only after Cargo regenerates
the canonical registry archive and its checksum matches crates.io byte-for-byte.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

try:
    from .cargo_publish_plan import build_plan, inspect_crates_io
    from .check_package_distribution import validate_distribution
    from .check_crates_io_artifacts import sha256_file
except ImportError:
    from cargo_publish_plan import build_plan, inspect_crates_io
    from check_package_distribution import validate_distribution
    from check_crates_io_artifacts import sha256_file


def publication_decision(status):
    if status == "version_exists":
        return "verify_existing"
    if status in {"name_exists", "no_registry_record", "version_not_registered",
                  "crate_name_not_found_at_query_time"}:
        return "publish"
    raise ValueError(f"refusing to publish after registry status {status!r}")


def validate_publish_request(*, package, version, publish, confirmation, token, clean, tag):
    expected = f"{package}@{version}"
    if not publish:
        return
    if package != "qx-sdk":
        raise ValueError("the release publisher only supports the qx-sdk dependency closure")
    if confirmation != expected:
        raise ValueError(f"type --confirm {expected} to enable upload")
    if not token:
        raise ValueError("CARGO_REGISTRY_TOKEN is not set")
    if not clean:
        raise ValueError("publishing requires a clean Git worktree")
    if tag != f"v{version}":
        raise ValueError(f"publishing requires HEAD tagged exactly v{version}")


def load_publish_plan(root, package):
    result = subprocess.run(["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"],
                            cwd=root, check=True, capture_output=True, text=True, encoding="utf-8")
    metadata = json.loads(result.stdout)
    catalog = json.loads((root / "maturity" / "package-distribution.json").read_text(encoding="utf-8"))
    distribution_errors = validate_distribution(metadata, catalog, package)
    if distribution_errors:
        raise ValueError("workspace package distribution boundary failed: " + "; ".join(distribution_errors))
    plan = build_plan(metadata, package)
    packages = plan["packages"]
    versions = {item["version"] for item in packages}
    if len(versions) != 1:
        raise ValueError("lockstep release required: SDK closure contains multiple package versions")
    for item in packages:
        registries = item["publish"]
        if ((registries is not None and "crates-io" not in registries)
                or item["outside_workspace_path_dependencies"] or item["missing_metadata"]):
            raise ValueError(f"{item['name']} is not ready for crates.io packaging")
    return plan, next(iter(versions)), Path(metadata["target_directory"])


def git_release_identity(root):
    status = subprocess.run(["git", "status", "--porcelain"], cwd=root, check=True,
                            capture_output=True, text=True, encoding="utf-8").stdout
    tag_result = subprocess.run(["git", "describe", "--exact-match", "--tags", "HEAD"], cwd=root,
                                capture_output=True, text=True, encoding="utf-8")
    return not status.strip(), tag_result.stdout.strip() if tag_result.returncode == 0 else None


def package_closure(root, package, allow_dirty, offline_package_check, *, json_mode=False):
    command = [sys.executable, str(root / "tools" / "check_rust_sdk_packages.py"), package]
    if allow_dirty:
        command.append("--allow-dirty")
    if offline_package_check:
        command.append("--offline")
    environment = os.environ.copy()
    # The Windows MSVC `ring` build can exceed compiler command-line debug
    # record limits under Cargo's default dev symbols; this is only the isolated
    # package-consumer verification profile, not the user's build profile.
    environment["CARGO_PROFILE_DEV_DEBUG"] = "0"
    subprocess.run(command, cwd=root, env=environment, check=True,
                   stdout=sys.stderr if json_mode else None)


def package_registry_archive(root, name, version, *, target, cargo_home, environment):
    """Regenerate the canonical registry package after its dependencies are published.

    The closure preflight uses local [patch.crates-io] overrides so it can build
    unpublished dependencies. Those overrides must not supply the checksum used
    to verify a registry release: Cargo may normalize a dependent crate archive
    differently when publishing it against the real registry.
    """
    command = ["cargo", "package", "-p", name, "--registry", "crates-io",
               "--locked", "--no-verify"]
    child_env = environment.copy()
    child_env["CARGO_HOME"] = str(cargo_home)
    subprocess.run(command, cwd=root, env=child_env, check=True)
    archive = Path(target) / "package" / f"{name}-{version}.crate"
    if not archive.is_file():
        raise ValueError(f"Cargo did not create the canonical registry archive for {name} {version}")
    return sha256_file(archive)


def publish_one(root, name, version, *, target, cargo_home, environment):
    command = ["cargo", "publish", "-p", name, "--registry", "crates-io", "--locked"]
    child_env = environment.copy()
    child_env["CARGO_HOME"] = str(cargo_home)
    subprocess.run(command, cwd=root, env=child_env, check=True)
    archive = Path(target) / "package" / f"{name}-{version}.crate"
    if not archive.is_file():
        raise ValueError(f"Cargo did not leave the uploaded registry archive for {name} {version}")
    return sha256_file(archive)


def inspect_registry_decisions(plan, *, inspect=inspect_crates_io, sleep=time.sleep, progress=None):
    """Read every registry version before upload; verify archive identity per package later."""
    decisions = []
    for index, item in enumerate(plan["packages"]):
        if index:
            sleep(1)
        name, version = item["name"], item["version"]
        if progress is not None:
            progress(index + 1, len(plan["packages"]), name)
        remote = inspect(name, version)
        if remote.get("status") == "version_exists" and remote.get("yanked"):
            raise ValueError(f"refusing to publish after registry status 'published_version_yanked' for {name} {version}")
        action = publication_decision(remote.get("status"))
        decisions.append({"name": name, "version": version, "action": action,
                          "status": remote.get("status"), "registry_checksum": remote.get("checksum")})
    return decisions


def wait_for_published_checksum(name, version, expected, *, inspect=inspect_crates_io,
                                sleep=time.sleep, attempts=30):
    last = None
    for attempt in range(attempts):
        if attempt:
            sleep(10)
        try:
            last = inspect(name, version)
        except ValueError as error:
            last = {"status": "query_failed", "error": str(error)}
            continue
        if last.get("status") == "version_exists":
            if last.get("yanked"):
                raise ValueError(f"published {name} {version} is marked yanked")
            if last.get("checksum") != expected:
                raise ValueError(f"published {name} {version} checksum differs from the verified local archive")
            return
    raise ValueError(f"registry did not confirm {name} {version} with the expected checksum: {last}")


def validate_registry_consumer_output(output):
    if "Verified: true" not in output or "Bars: 70; fills: 1;" not in output:
        raise ValueError("clean registry consumer did not verify the 70-bar quickstart result")


def run_registry_consumer(root, version, *, cargo_home):
    """Install the published SDK from crates.io in an isolated consumer project."""
    consumer = cargo_home.parent / "registry-consumer"
    source = consumer / "src"
    source.mkdir(parents=True)
    quickstart = root / "examples" / "rust-sdk-quickstart"
    shutil.copy2(quickstart / "src" / "main.rs", source / "main.rs")
    shutil.copy2(quickstart / "bars.json", consumer / "bars.json")
    (consumer / "Cargo.toml").write_text(
        '[package]\nname = "qianxing-registry-consumer"\nversion = "0.1.0"\n'
        'edition = "2021"\npublish = false\n\n[workspace]\n\n[dependencies]\n'
        f'qx-sdk = "={version}"\n', encoding="utf-8"
    )
    environment = os.environ.copy()
    environment.pop("CARGO_REGISTRY_TOKEN", None)
    environment["CARGO_HOME"] = str(cargo_home)
    environment["CARGO_TARGET_DIR"] = str(cargo_home.parent / "registry-consumer-target")
    command = ["cargo", "run", "--manifest-path", str(consumer / "Cargo.toml")]
    result = subprocess.run(command, cwd=consumer, env=environment, capture_output=True, text=True)
    output = result.stdout + result.stderr
    if result.returncode:
        raise subprocess.CalledProcessError(result.returncode, command, output=result.stdout, stderr=result.stderr)
    validate_registry_consumer_output(output)
    print("Clean crates.io consumer passed:\n" + output.strip(), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", nargs="?", default="qx-sdk")
    parser.add_argument("--publish", action="store_true", help="upload the complete dependency closure")
    parser.add_argument("--confirm", help="exactly PACKAGE@VERSION; required with --publish")
    parser.add_argument("--root", type=Path, default=None,
                        help="release source checkout (must be clean and exactly tagged when publishing)")
    parser.add_argument("--allow-dirty", action="store_true", help="preflight only: package uncommitted changes")
    parser.add_argument("--offline-package-check", action="store_true",
                        help="build the local package closure from cached dependencies; registry checks remain online")
    parser.add_argument("--json", action="store_true", help="emit registry decisions as JSON")
    args = parser.parse_args()
    root = (args.root or Path(__file__).resolve().parents[1]).resolve()
    decisions = []
    try:
        plan, version, target = load_publish_plan(root, args.package)
        clean, tag = git_release_identity(root)
        token = os.environ.get("CARGO_REGISTRY_TOKEN")
        validate_publish_request(package=args.package, version=version, publish=args.publish,
                                 confirmation=args.confirm, token=token, clean=clean, tag=tag)
        if args.publish and args.allow_dirty:
            raise ValueError("--allow-dirty cannot be combined with --publish")
        print("Building and verifying the complete local package closure.", file=sys.stderr if args.json else sys.stdout,
              flush=True)
        package_closure(root, args.package, args.allow_dirty, args.offline_package_check, json_mode=args.json)
        decisions = inspect_registry_decisions(
            plan,
            progress=lambda index, total, name: print(
                f"Checking crates.io version {index}/{total}: {name}", file=sys.stderr, flush=True
            ),
        )
    except (OSError, ValueError, KeyError, json.JSONDecodeError, subprocess.CalledProcessError) as error:
        print(f"Rust SDK release preflight failed: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr.strip(), file=sys.stderr)
        return 2

    if args.json:
        print(json.dumps({"package": args.package, "version": version, "publish_order": decisions,
                          "upload_enabled": args.publish}, indent=2, ensure_ascii=False))
    else:
        mode = "UPLOAD ENABLED" if args.publish else "preflight only; no upload"
        print(f"{args.package} {version}: {len(decisions)} packages ({mode})")
        for decision in decisions:
            print(f"  {decision['name']} {decision['version']}: {decision['action']} ({decision['status']})")
        print("Registry name ownership and upload permission are not inferred from public metadata.")

    if not args.publish:
        return 0

    environment = os.environ.copy()
    try:
        # An isolated Cargo home bypasses machine-wide source replacement such as
        # crates.io -> a regional mirror. The upload token stays in the environment,
        # never on the process command line or in output.
        with tempfile.TemporaryDirectory(prefix="qx-crates-io-publish-") as temporary:
            cargo_home = Path(temporary) / "cargo-home"
            cargo_home.mkdir()
            for decision in decisions:
                if decision["action"] == "verify_existing":
                    checksum = package_registry_archive(
                        root, decision["name"], version, target=target,
                        cargo_home=cargo_home, environment=environment,
                    )
                    wait_for_published_checksum(decision["name"], version, checksum)
                    print(f"Already published with matching checksum: {decision['name']} {version}", flush=True)
                    continue
                print(f"Publishing {decision['name']} {version}", flush=True)
                checksum = publish_one(
                    root, decision["name"], version, target=target,
                    cargo_home=cargo_home, environment=environment,
                )
                wait_for_published_checksum(decision["name"], version, checksum)
            run_registry_consumer(root, version, cargo_home=cargo_home)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"Rust SDK release stopped; rerun after resolving the reported package: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr.strip(), file=sys.stderr)
        return 3
    print(f"Published and checksum-verified the {len(decisions)}-package {args.package} closure.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
