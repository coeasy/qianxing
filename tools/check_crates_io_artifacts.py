#!/usr/bin/env python3
"""Compare packaged Rust crate archives with their exact crates.io versions.

Run tools/check_rust_sdk_packages.py first. This checker is read-only: it never
uploads a crate and does not infer name ownership or publish permission.
"""
import argparse
import hashlib
import json
import subprocess
import sys
import time
from pathlib import Path

try:
    from .cargo_publish_plan import build_plan, inspect_crates_io
except ImportError:
    from cargo_publish_plan import build_plan, inspect_crates_io


def sha256_file(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def compare_artifacts(plan, package_dir, *, inspect=inspect_crates_io, sleep=time.sleep, progress=None):
    """Compare each local archive against registry metadata, stopping on query failure."""
    package_dir = Path(package_dir)
    results = []
    verified = True
    for index, package in enumerate(plan["packages"]):
        if index:
            sleep(1)
        name, version = package["name"], package["version"]
        if progress is not None:
            progress(index + 1, len(plan["packages"]), name)
        archive = package_dir / f"{name}-{version}.crate"
        if not archive.is_file():
            results.append({"name": name, "version": version, "status": "local_archive_missing",
                            "archive": str(archive)})
            verified = False
            break
        local_checksum = sha256_file(archive)
        try:
            remote = inspect(name, version)
        except ValueError as error:
            results.append({"name": name, "version": version, "status": "query_failed",
                            "local_checksum": local_checksum, "error": str(error)})
            verified = False
            break

        status = remote["status"]
        item = {"name": name, "version": version, "status": status,
                "local_checksum": local_checksum, "registry_checksum": remote.get("checksum")}
        if status == "version_exists":
            if remote.get("yanked"):
                item["status"] = "published_version_yanked"
                verified = False
            elif remote.get("checksum") != local_checksum:
                item["status"] = "published_artifact_mismatch"
                verified = False
            else:
                item["status"] = "published_artifact_matches"
        elif status == "name_exists":
            item["status"] = "version_not_registered"
        elif status == "no_registry_record":
            item["status"] = "crate_name_not_found_at_query_time"
        else:
            item["status"] = "invalid_registry_status"
            verified = False
        results.append(item)
        if not verified:
            break
    return {"root": plan["root"], "registry_checked": verified, "packages": results,
            "note": "A matching checksum proves archive identity only; it does not prove crate ownership or upload permission."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", nargs="?", default="qx-sdk")
    parser.add_argument("--package-dir", type=Path, default=None,
                        help="directory containing Cargo-generated .crate files (default: target/package)")
    parser.add_argument("--json", action="store_true", help="emit machine-readable JSON")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    try:
        result = subprocess.run(["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"],
                                cwd=root, check=True, capture_output=True, text=True, encoding="utf-8")
        metadata = json.loads(result.stdout)
        plan = build_plan(metadata, args.package)
        package_dir = args.package_dir or Path(metadata["target_directory"]) / "package"
        comparison = compare_artifacts(
            plan, package_dir,
            progress=lambda index, total, name: print(
                f"Checking crates.io artifact {index}/{total}: {name}", file=sys.stderr, flush=True
            ),
        )
    except (OSError, ValueError, KeyError, json.JSONDecodeError, subprocess.CalledProcessError) as error:
        print(f"crates.io artifact check failed: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr.strip(), file=sys.stderr)
        return 2
    if args.json:
        print(json.dumps(comparison, indent=2, ensure_ascii=False))
    else:
        print(f"{comparison['root']}: {len(comparison['packages'])} package artifact checks")
        for package in comparison["packages"]:
            print(f"  {package['name']} {package['version']}: {package['status']}")
            if package.get("error"):
                print(f"    {package['error']}")
        print(comparison["note"])
    return 0 if comparison["registry_checked"] else 2


if __name__ == "__main__":
    sys.exit(main())
