#!/usr/bin/env python3
"""Package and verify a workspace dependency closure without publishing it.

Each crate is built from its normalized .crate archive in an isolated directory,
using earlier archives as dependency patches. This tests package contents and
ordering; it does not prove registry availability, ownership, or installation.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

TRANSIENT_PACKAGE_PATHS = {".task-tmp", ".tmp-test", "target", "__pycache__"}

try:
    from .cargo_publish_plan import build_plan
except ImportError:
    from cargo_publish_plan import build_plan


def write_patches(config, entries):
    lines = ["[patch.crates-io]"]
    lines.extend(f"{json.dumps(name)} = {{ path = {json.dumps(str(path))} }}" for name, path in entries)
    config.write_text("\n".join(lines) + "\n", encoding="utf-8")


def build_environment(temporary):
    environment = os.environ.copy()
    environment["CARGO_TARGET_DIR"] = str(Path(temporary) / "cargo-target")
    return environment


def unpack_archive(archive, destination, expected_name):
    with tarfile.open(archive, "r:gz") as bundle:
        seen = set()
        for member in bundle.getmembers():
            parts = member.name.split("/")
            if (parts[0] != expected_name or any(part in (".", "..", "") for part in parts)
                    or "\\" in member.name or ":" in member.name or member.name in seen
                    or any(part in TRANSIENT_PACKAGE_PATHS for part in parts[1:])
                    or not (member.isfile() or member.isdir())):
                raise ValueError(f"unsafe entry in {archive.name} (including transient paths): {member.name}")
            seen.add(member.name)
        if sys.version_info >= (3, 12):
            bundle.extractall(destination, filter="data")
        else:
            bundle.extractall(destination)
    directory = destination / expected_name
    if not (directory / "Cargo.toml").is_file():
        raise ValueError(f"archive {archive.name} is missing its manifest")
    return directory.resolve()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", nargs="?", default="qx-sdk")
    parser.add_argument("--offline", action="store_true", help="use only cached external dependencies")
    parser.add_argument("--allow-dirty", action="store_true", help="allow Cargo to package uncommitted changes")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    metadata_command = ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"]
    if args.offline:
        metadata_command.append("--offline")
    try:
        result = subprocess.run(metadata_command, cwd=root, check=True, capture_output=True,
                                text=True, encoding="utf-8")
        metadata = json.loads(result.stdout)
        plan = build_plan(metadata, args.package)
        for package in plan["packages"]:
            registries = package["publish"]
            if ((registries is not None and "crates-io" not in registries)
                    or package["outside_workspace_path_dependencies"]):
                raise ValueError(f"{package['name']} is not a public workspace package or has external path dependencies")
        target = Path(metadata["target_directory"])
        target.mkdir(parents=True, exist_ok=True)
        sources = {package["name"]: Path(package["manifest_path"]).parent.resolve()
                   for package in metadata["packages"] if package["id"] in metadata["workspace_members"]}
        packages_by_name = {package["name"]: package for package in plan["packages"]}

        def required_predecessors(name):
            visited, pending = set(), [name]
            while pending:
                current = pending.pop()
                package = packages_by_name[current]
                for dependency in package["dependencies"] + package["dev_path_dependencies"]:
                    target_name = dependency.get("workspace_package")
                    if target_name in packages_by_name and target_name not in visited:
                        visited.add(target_name)
                        pending.append(target_name)
            return visited

        extracted = []
        with tempfile.TemporaryDirectory(prefix="qx-package-check-", dir=target) as temporary:
            temporary = Path(temporary)
            source_config = temporary / "source-patches.toml"
            archive_config = temporary / "archive-patches.toml"
            for index, package in enumerate(plan["packages"], 1):
                required = required_predecessors(package["name"])
                write_patches(source_config, [(name, sources[name]) for name, _ in extracted if name in required])
                command = ["cargo", "package", "-p", package["name"], "--locked", "--no-verify",
                           "--config", str(source_config)]
                if args.offline:
                    command.append("--offline")
                if args.allow_dirty:
                    command.append("--allow-dirty")
                print(f"[{index}/{len(plan['packages'])}] package and verify {package['name']} {package['version']}",
                      flush=True)
                subprocess.run(command, cwd=root, check=True)
                package_id = f"{package['name']}-{package['version']}"
                directory = unpack_archive(target / "package" / f"{package_id}.crate", temporary, package_id)
                members = [path.name for _, path in extracted] + [package_id]
                (temporary / "Cargo.toml").write_text(
                    "[workspace]\nresolver = \"2\"\nmembers = [" +
                    ", ".join(json.dumps(member) for member in members) + "]\n", encoding="utf-8"
                )
                write_patches(archive_config, extracted)
                build = ["cargo", "build", "--manifest-path", str(directory / "Cargo.toml"),
                         "--config", str(archive_config)]
                if args.offline:
                    build.append("--offline")
                # Every closure run starts with empty build artifacts. Reusing a workspace
                # target directory can make Cargo accept stale metadata from an earlier
                # version of an extracted crate and produce misleading compiler errors.
                environment = build_environment(temporary)
                subprocess.run(build, cwd=temporary, env=environment, check=True)
                extracted.append((package["name"], directory))
            if args.package == "qx-sdk":
                consumer = temporary / "consumer"
                (consumer / "src").mkdir(parents=True)
                quickstart = root / "examples" / "rust-sdk-quickstart"
                shutil.copy2(quickstart / "src" / "main.rs", consumer / "src" / "main.rs")
                shutil.copy2(quickstart / "bars.json", consumer / "bars.json")
                version = plan["packages"][-1]["version"]
                (consumer / "Cargo.toml").write_text(
                    '[package]\nname = "qx-package-consumer"\nversion = "0.1.0"\n'
                    'edition = "2021"\npublish = false\n\n[workspace]\n\n[dependencies]\n'
                    f'qx-sdk = {json.dumps(version)}\n', encoding="utf-8"
                )
                write_patches(archive_config, extracted)
                run = ["cargo", "run", "--manifest-path", str(consumer / "Cargo.toml"),
                       "--config", str(archive_config)]
                if args.offline:
                    run.append("--offline")
                print("[consumer] run independent Rust quickstart using packaged dependencies", flush=True)
                subprocess.run(run, cwd=consumer, env=environment, check=True)
    except (OSError, ValueError, KeyError, tarfile.TarError, subprocess.CalledProcessError) as error:
        print(f"package closure check failed: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr.strip(), file=sys.stderr)
        return 1
    print(f"Verified {len(extracted)} local packages; no registry publication or install was performed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
