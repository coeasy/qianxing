#!/usr/bin/env python3
"""Inspect workspace path dependencies needed by a Cargo package.

This is a conservative manifest graph, including optional and target-specific
normal/build dependencies. Optional crates.io checks are read-only.
"""
import argparse
import json
import re
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


CRATES_IO_API = "https://crates.io/api/v1/crates/"
USER_AGENT = "qianxing-release-check/0.4 (https://github.com/coeasy/qianxing)"


def inspect_crates_io(name, version, *, opener=urllib.request.urlopen, sleep=time.sleep, attempts=3):
    """Inspect public registry metadata; never infer ownership or publication permission."""
    url = CRATES_IO_API + urllib.parse.quote(name, safe="")
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT, "Accept": "application/json"})
    if attempts < 1:
        raise ValueError("crates.io query attempts must be positive")
    response = None
    for attempt in range(attempts):
        try:
            response = opener(request, timeout=20)
            break
        except urllib.error.HTTPError as error:
            if error.code == 404:
                return {"status": "no_registry_record", "crate": name, "version": version}
            retryable = error.code == 429 or 500 <= error.code < 600
            if retryable and attempt + 1 < attempts:
                sleep(2 ** attempt)
                continue
            raise ValueError(f"crates.io returned HTTP {error.code} for {name}") from error
        except (OSError, urllib.error.URLError) as error:
            if attempt + 1 < attempts:
                sleep(2 ** attempt)
                continue
            raise ValueError(f"crates.io query failed for {name}: {error}") from error
    try:
        with response:
            document = json.load(response)
    except (OSError, ValueError) as error:
        raise ValueError(f"crates.io returned invalid JSON metadata for {name}: {error}") from error
    if not isinstance(document, dict) or not isinstance(document.get("crate"), dict) \
            or not isinstance(document.get("versions"), list):
        raise ValueError(f"crates.io returned invalid metadata for {name}")
    crate = document["crate"]
    canonical = crate.get("id")
    if not isinstance(canonical, str) or canonical.lower().replace("_", "-") != name.lower().replace("_", "-"):
        raise ValueError(f"crates.io returned a mismatched crate identity for {name}")
    versions = document["versions"]
    if any(not isinstance(item, dict) or not isinstance(item.get("num"), str)
           or not isinstance(item.get("yanked"), bool) for item in versions):
        raise ValueError(f"crates.io returned invalid version metadata for {name}")
    matches = [item for item in versions if item["num"].split("+", 1)[0] == version.split("+", 1)[0]]
    if len(matches) > 1:
        raise ValueError(f"crates.io returned ambiguous version metadata for {name} {version}")
    checksum = matches[0].get("checksum") if matches else None
    if matches and (not isinstance(checksum, str) or re.fullmatch(r"[0-9a-fA-F]{64}", checksum) is None):
        raise ValueError(f"crates.io returned invalid checksum metadata for {name} {version}")
    return {
        "status": "version_exists" if matches else "name_exists",
        "crate": canonical,
        "version": version,
        "published_version": matches[0]["num"] if matches else None,
        "yanked": matches[0]["yanked"] if matches else None,
        "checksum": checksum.lower() if checksum else None,
        "repository": crate.get("repository"),
    }


def check_crates_io(plan, *, inspect=inspect_crates_io, sleep=time.sleep, progress=None):
    """Query sequentially at most once per second, then stop on unresolved failure."""
    packages = plan["packages"]
    for index, package in enumerate(packages):
        if index:
            sleep(1)
        if progress is not None:
            progress(index + 1, len(packages), package["name"])
        try:
            package["crates_io"] = inspect(package["name"], package["version"])
        except ValueError as error:
            package["crates_io"] = {"status": "query_failed", "error": str(error)}
            for remaining in packages[index + 1:]:
                remaining["crates_io"] = {"status": "not_checked"}
            plan["registry_checked"] = False
            return False
    plan["registry_checked"] = True
    return True


def build_plan(metadata, root):
    members = set(metadata["workspace_members"])
    packages = {p["name"]: p for p in metadata["packages"] if p["id"] in members}
    if root not in packages:
        raise ValueError(f"workspace package not found: {root}")
    paths = {str(Path(p["manifest_path"]).parent.resolve()): name for name, p in packages.items()}
    edges = {}
    dev_paths = {}
    outside = []
    for name, package in packages.items():
        edges[name] = []
        dev_paths[name] = []
        for dep in package["dependencies"]:
            if not dep.get("path"):
                continue
            entry = {"name": dep["name"], "requirement": dep["req"],
                     "kind": dep.get("kind") or "normal", "optional": dep["optional"],
                     "target": dep.get("target")}
            target = paths.get(str(Path(dep["path"]).resolve()))
            if dep.get("kind") == "dev":
                dev_paths[name].append({**entry, "workspace_package": target})
                continue
            if target is None:
                outside.append((name, entry))
            else:
                edges[name].append((target, entry))

    state, order = {}, []

    def visit(name, stack):
        if state.get(name) == 1:
            raise ValueError("dependency cycle: " + " -> ".join(stack + [name]))
        if state.get(name) == 2:
            return
        state[name] = 1
        for target, _ in sorted(edges[name], key=lambda item: item[0]):
            visit(target, stack + [name])
        state[name] = 2
        order.append(name)

    visit(root, [])
    closure = set(order)
    result = []
    for name in order:
        package = packages[name]
        deps = [{**entry, "workspace_package": target}
                for target, entry in sorted(edges[name], key=lambda item: (item[0], item[1]["kind"], item[1]["target"] or ""))]
        external_paths = [entry for owner, entry in outside if owner == name]
        missing_metadata = []
        if not package.get("description"):
            missing_metadata.append("description")
        if not (package.get("license") or package.get("license_file")):
            missing_metadata.append("license or license_file")
        if not package.get("repository"):
            missing_metadata.append("repository")
        result.append({"name": name, "version": package["version"],
                       "publish": package.get("publish"), "dependencies": deps,
                       "dev_path_dependencies": dev_paths[name],
                       "missing_metadata": missing_metadata,
                       "outside_workspace_path_dependencies": external_paths})
    return {"root": root, "scope": "normal and build path dependencies, including optional and all targets",
            "registry_checked": False, "publish_order": order, "packages": result,
            "workspace_packages_outside_closure": sorted(set(packages) - closure)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", nargs="?", default="qx-sdk")
    parser.add_argument("--json", action="store_true", help="emit machine-readable JSON")
    parser.add_argument("--check-crates-io", action="store_true", help="query public crates.io metadata (read-only)")
    args = parser.parse_args()
    try:
        proc = subprocess.run(["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"],
                              check=True, capture_output=True, text=True, encoding="utf-8")
        plan = build_plan(json.loads(proc.stdout), args.package)
    except (OSError, subprocess.CalledProcessError, ValueError, KeyError) as exc:
        print(f"cannot build publish plan: {exc}", file=sys.stderr)
        if isinstance(exc, subprocess.CalledProcessError) and exc.stderr:
            print(exc.stderr.strip(), file=sys.stderr)
        return 1
    checked = check_crates_io(
        plan,
        progress=lambda index, total, name: print(
            f"Checking crates.io {index}/{total}: {name}", file=sys.stderr, flush=True
        ),
    ) if args.check_crates_io else True
    if args.json:
        print(json.dumps(plan, indent=2, ensure_ascii=False))
    else:
        print(f"{plan['root']}: {len(plan['packages'])} workspace packages in conservative publish order")
        print("Includes optional and target-specific normal/build dependencies.")
        print("Dev path dependencies are listed separately and can affect packaging order.")
        print("Registry records do not prove name availability, ownership, upload permission, or artifact identity." if args.check_crates_io
              else "Registry availability is NOT checked.")
        for p in plan["packages"]:
            restrictions = p["publish"]
            label = "publish=false" if restrictions == [] else (f"registries={restrictions}" if restrictions else "registry permitted")
            print(f"  {p['name']} {p['version']} ({label}; {len(p['dependencies'])} workspace edges)")
            if p["missing_metadata"]:
                print(f"    missing package metadata: {', '.join(p['missing_metadata'])}")
            for dep in p["dev_path_dependencies"]:
                print(f"    dev path dependency requires separate packaging review: {dep['name']} {dep['requirement']}")
            if args.check_crates_io:
                print(f"    crates.io: {json.dumps(p['crates_io'], ensure_ascii=False)}")
            for dep in p["outside_workspace_path_dependencies"]:
                print(f"    outside workspace path dependency: {dep['name']} {dep['requirement']}")
    return 0 if checked else 2


if __name__ == "__main__":
    sys.exit(main())
