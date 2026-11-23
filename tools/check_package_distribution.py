#!/usr/bin/env python3
"""Validate the workspace package boundary and emit its current consumer map."""
import argparse
import json
from pathlib import Path
import subprocess
import sys

try:
    from .cargo_publish_plan import build_plan
except ImportError:
    from cargo_publish_plan import build_plan


ROLE_CHANNEL = {
    "sdk-facade": "crates-io",
    "sdk-dependency": "crates-io",
    "cli": "github-release",
    "python-extension": "pypi-wheel",
    "internal": "workspace-only",
    "test-only": "ci-only",
}
PUBLISHABLE_ROLES = {"sdk-facade", "sdk-dependency"}


def direct_consumers(metadata):
    members = set(metadata["workspace_members"])
    packages = [package for package in metadata["packages"] if package["id"] in members]
    by_path = {str(Path(package["manifest_path"]).parent.resolve()): package["name"]
               for package in packages}
    consumers = {package["name"]: [] for package in packages}
    for package in packages:
        for dependency in package["dependencies"]:
            if not dependency.get("path"):
                continue
            target = by_path.get(str(Path(dependency["path"]).resolve()))
            if target is None:
                continue
            consumers[target].append({
                "package": package["name"],
                "kind": dependency.get("kind") or "normal",
                "optional": dependency["optional"],
                "target": dependency.get("target"),
            })
    for entries in consumers.values():
        entries.sort(key=lambda item: (item["package"], item["kind"], item["target"] or ""))
    return consumers


def validate_distribution(metadata, catalog, package="qx-sdk"):
    members = {item["name"]: item for item in metadata["packages"]
               if item["id"] in set(metadata["workspace_members"])}
    entries = catalog.get("packages")
    errors = []
    if catalog.get("schema_version") != 2:
        errors.append("package distribution schema_version must be 2")
    if catalog.get("sdk_root") != package:
        errors.append(f"package catalog sdk_root must match selected SDK root {package!r}")
    minimum_rust_version = catalog.get("minimum_rust_version")
    if not isinstance(minimum_rust_version, str) or not minimum_rust_version.strip():
        errors.append("package catalog must declare minimum_rust_version")
    if not isinstance(entries, list):
        return [*errors, "package distribution catalog must contain a packages array"]

    classified = {}
    for entry in entries:
        if not isinstance(entry, dict) or not isinstance(entry.get("name"), str):
            errors.append("each package catalog entry must have a string name")
            continue
        name = entry["name"]
        if name in classified:
            errors.append(f"package {name} is classified more than once")
        classified[name] = entry
        role = entry.get("role")
        if role not in ROLE_CHANNEL:
            errors.append(f"package {name} has unknown distribution role {role!r}")
        elif entry.get("channel") != ROLE_CHANNEL[role]:
            errors.append(f"package {name} role {role!r} must use channel {ROLE_CHANNEL[role]!r}")
        if not isinstance(entry.get("owner"), str) or not entry["owner"].strip():
            errors.append(f"package {name} must declare a technical owner")

    missing = sorted(set(members) - set(classified))
    stale = sorted(set(classified) - set(members))
    if missing:
        errors.append(f"workspace packages are unclassified: {', '.join(missing)}")
    if stale:
        errors.append(f"catalog packages are not workspace members: {', '.join(stale)}")

    try:
        plan = build_plan(metadata, package)
    except (ValueError, KeyError) as error:
        return [*errors, str(error)]
    closure = {item["name"] for item in plan["packages"]}
    declared_closure = {name for name, entry in classified.items()
                        if entry.get("role") in PUBLISHABLE_ROLES}
    roots = {name for name, entry in classified.items() if entry.get("role") == "sdk-facade"}
    if roots != {package}:
        errors.append(f"distribution catalog must identify exactly {package!r} as sdk-facade")
    if closure != declared_closure:
        errors.append(
            "SDK publish closure and distribution catalog differ; "
            f"unclassified-in-closure={sorted(closure - declared_closure)}, "
            f"outside-closure-marked-publishable={sorted(declared_closure - closure)}"
        )

    for name, entry in classified.items():
        manifest = members.get(name)
        if manifest is None:
            continue
        role = entry.get("role")
        cargo_publish = manifest.get("publish")
        if role in PUBLISHABLE_ROLES:
            if cargo_publish is not None and "crates-io" not in cargo_publish:
                errors.append(f"SDK package {name} must permit crates.io publishing")
        elif cargo_publish != []:
            errors.append(f"non-SDK package {name} must set publish = false")

    closure_items = {item["name"]: item for item in plan["packages"]}
    for name, item in closure_items.items():
        manifest = members.get(name)
        if manifest is not None and manifest.get("rust_version") != minimum_rust_version:
            errors.append(
                f"SDK package {name} must declare rust-version {minimum_rust_version!r}; "
                f"found {manifest.get('rust_version')!r}"
            )
        if item["missing_metadata"]:
            errors.append(f"SDK package {name} is missing metadata: {', '.join(item['missing_metadata'])}")
        if item["outside_workspace_path_dependencies"]:
            errors.append(f"SDK package {name} has a path dependency outside the workspace")
    return errors


def build_report(metadata, catalog):
    consumers = direct_consumers(metadata)
    plan = build_plan(metadata, catalog["sdk_root"])
    publish_order = {name: index for index, name in enumerate(plan["publish_order"], 1)}
    return {
        "schema_version": catalog["schema_version"],
        "sdk_root": catalog["sdk_root"],
        "minimum_rust_version": catalog["minimum_rust_version"],
        "publish_order": plan["publish_order"],
        "packages": [
            {
                **entry,
                "cargo_version": next(item["version"] for item in metadata["packages"]
                                       if item["id"] in set(metadata["workspace_members"])
                                       and item["name"] == entry["name"]),
                "publish_order_index": publish_order.get(entry["name"]),
                "direct_consumers": consumers[entry["name"]],
            }
            for entry in catalog["packages"]
        ],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true", help="emit the package and consumer map as JSON")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    catalog_path = root / "maturity" / "package-distribution.json"
    try:
        metadata = json.loads(subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"],
            cwd=root, check=True, capture_output=True, text=True, encoding="utf-8",
        ).stdout)
        catalog = json.loads(catalog_path.read_text(encoding="utf-8"))
        errors = validate_distribution(metadata, catalog)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError, json.JSONDecodeError) as error:
        print(f"package distribution check failed: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr.strip(), file=sys.stderr)
        return 2
    if errors:
        print("Package distribution boundary violations:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    report = build_report(metadata, catalog)
    if args.json:
        print(json.dumps(report, indent=2, ensure_ascii=False))
    else:
        print(f"Package distribution boundary verified for {len(report['packages'])} workspace packages.")
        print("SDK publish order: " + " -> ".join(report["publish_order"]))
        for item in report["packages"]:
            order = f"#{item['publish_order_index']}" if item["publish_order_index"] else "not-published"
            consumers = ", ".join(sorted({dep["package"] for dep in item["direct_consumers"]})) or "none"
            print(f"  {item['name']}: {item['role']} / {item['channel']} / owner={item['owner']} / {order}; consumers={consumers}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
