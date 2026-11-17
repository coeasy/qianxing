"""Generate a deterministic SPDX 2.3 SBOM from locked Cargo metadata."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
from typing import Any


def _package_source(package: dict[str, Any], metadata: dict[str, Any], workspace_members: set[str]) -> str:
    source = package.get("source")
    if source:
        return str(source)
    if package["id"] in workspace_members:
        return "workspace"
    manifest = Path(package["manifest_path"])
    root = Path(metadata["workspace_root"])
    try:
        return "path:" + manifest.parent.relative_to(root).as_posix()
    except ValueError:
        return "path:" + manifest.parent.name


def _spdx_id(name: str, version: str, source: str) -> str:
    identity = json.dumps([name, version, source], ensure_ascii=True, separators=(",", ":"))
    return "SPDXRef-Package-" + hashlib.sha256(identity.encode("utf-8")).hexdigest()[:24]


def build_spdx(metadata: dict[str, Any], *, commit: str, created_epoch: int) -> dict[str, Any]:
    if not re.fullmatch(r"[0-9a-fA-F]{40,64}", commit):
        raise ValueError("commit must be a 40- or 64-character hexadecimal revision")
    packages = metadata.get("packages")
    resolve = metadata.get("resolve")
    if not isinstance(packages, list) or not isinstance(resolve, dict):
        raise ValueError("Cargo metadata must contain packages and a resolved dependency graph")

    workspace_members = set(metadata.get("workspace_members", []))
    package_by_id = {item["id"]: item for item in packages}
    if len(package_by_id) != len(packages):
        raise ValueError("Cargo metadata contains duplicate package identities")

    package_ids: dict[str, str] = {}
    spdx_packages: list[dict[str, Any]] = []
    for package in sorted(packages, key=lambda item: (item["name"], item["version"], item["id"])):
        source = _package_source(package, metadata, workspace_members)
        spdx_id = _spdx_id(package["name"], package["version"], source)
        if spdx_id in package_ids.values():
            raise ValueError(f"package identity collision for {package['name']} {package['version']}")
        package_ids[package["id"]] = spdx_id
        item: dict[str, Any] = {
            "name": package["name"],
            "SPDXID": spdx_id,
            "versionInfo": package["version"],
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": False,
            "licenseConcluded": "NOASSERTION",
            "licenseDeclared": package.get("license") or "NOASSERTION",
            "copyrightText": "NOASSERTION",
            "sourceInfo": source,
            "externalRefs": [{
                "referenceCategory": "PACKAGE-MANAGER",
                "referenceType": "purl",
                "referenceLocator": f"pkg:cargo/{package['name']}@{package['version']}",
            }],
        }
        spdx_packages.append(item)

    relationships: set[tuple[str, str, str]] = set()
    for spdx_id in package_ids.values():
        relationships.add(("SPDXRef-DOCUMENT", "DESCRIBES", spdx_id))
    for node in resolve.get("nodes", []):
        source_id = package_ids.get(node.get("id"))
        if source_id is None:
            raise ValueError(f"Cargo resolver references an unknown package: {node.get('id')}")
        for dependency in node.get("deps", []):
            target_id = package_ids.get(dependency.get("pkg"))
            if target_id is None:
                raise ValueError(f"Cargo dependency references an unknown package: {dependency.get('pkg')}")
            if source_id != target_id:
                relationships.add((source_id, "DEPENDS_ON", target_id))

    created = datetime.fromtimestamp(created_epoch, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    return {
        "spdxVersion": "SPDX-2.3",
        "dataLicense": "CC0-1.0",
        "SPDXID": "SPDXRef-DOCUMENT",
        "name": "Qianxing Cargo workspace dependencies",
        "documentNamespace": f"https://github.com/coeasy/qianxing/spdx/cargo/{commit.lower()}",
        "creationInfo": {
            "created": created,
            "creators": ["Tool: qianxing-cargo-sbom/1.0"],
        },
        "packages": spdx_packages,
        "documentDescribes": sorted(package_ids.values()),
        "relationships": [
            {"spdxElementId": source, "relationshipType": relationship, "relatedSpdxElement": target}
            for source, relationship, target in sorted(relationships)
        ],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--metadata", required=True, help="JSON from cargo metadata --format-version 1 --locked")
    parser.add_argument("--commit", required=True, help="Git commit associated with this source inventory")
    parser.add_argument("--created-epoch", required=True, type=int, help="Commit timestamp in Unix seconds")
    parser.add_argument("--output", required=True, help="Output SPDX JSON path")
    args = parser.parse_args()

    metadata = json.loads(Path(args.metadata).read_text(encoding="utf-8"))
    sbom = build_spdx(metadata, commit=args.commit, created_epoch=args.created_epoch)
    output = Path(args.output)
    output.write_text(json.dumps(sbom, ensure_ascii=False, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"SPDX packages={len(sbom['packages'])} relationships={len(sbom['relationships'])} output={output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
