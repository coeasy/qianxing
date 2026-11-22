#!/usr/bin/env python3
"""Generate a reproducible workspace dependency map from Cargo package metadata."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
CATALOG_PATH = ROOT / "maturity" / "package-distribution.json"
JSON_PATH = ROOT / "maturity" / "crate-dependency-map.json"
DOC_PATH = ROOT / "docs" / "crate-dependency-map.md"
KINDS = ("normal", "build", "dev")


def _dependency_kind(value: str | None) -> str:
    return value or "normal"


def _strongly_connected_components(nodes: list[str], edges: list[dict]) -> list[list[str]]:
    adjacency = {name: [] for name in nodes}
    for edge in edges:
        adjacency[edge["from"]].append(edge["to"])

    index = 0
    indices: dict[str, int] = {}
    lowlinks: dict[str, int] = {}
    stack: list[str] = []
    on_stack: set[str] = set()
    components: list[list[str]] = []

    def visit(node: str) -> None:
        nonlocal index
        indices[node] = lowlinks[node] = index
        index += 1
        stack.append(node)
        on_stack.add(node)
        for neighbor in adjacency[node]:
            if neighbor not in indices:
                visit(neighbor)
                lowlinks[node] = min(lowlinks[node], lowlinks[neighbor])
            elif neighbor in on_stack:
                lowlinks[node] = min(lowlinks[node], indices[neighbor])
        if lowlinks[node] == indices[node]:
            component: list[str] = []
            while True:
                member = stack.pop()
                on_stack.remove(member)
                component.append(member)
                if member == node:
                    break
            if len(component) > 1 or any(
                edge["from"] == node and edge["to"] == node for edge in edges
            ):
                components.append(sorted(component))

    for node in nodes:
        if node not in indices:
            visit(node)
    return sorted(components)


def build_map(metadata: dict, catalog: dict) -> dict:
    workspace_ids = set(metadata["workspace_members"])
    workspace_packages = [
        package for package in metadata["packages"] if package["id"] in workspace_ids
    ]
    package_by_name = {package["name"]: package for package in workspace_packages}
    workspace_paths = {
        str(Path(package["manifest_path"]).parent.resolve()): package
        for package in workspace_packages
    }
    owners = {item["name"]: item for item in catalog["packages"]}
    missing = sorted(set(package_by_name) - set(owners))
    stale = sorted(set(owners) - set(package_by_name))
    if missing or stale:
        raise ValueError(
            "package distribution catalog is out of sync: "
            f"unclassified={missing}; non_workspace={stale}"
        )

    edges_by_key: dict[tuple[str, str, str, str | None, str], dict] = {}
    external: dict[str, dict[str, set[str]]] = {
        package["name"]: {kind: set() for kind in KINDS}
        for package in workspace_packages
    }
    external_details: dict[str, dict[tuple[str, str, str, str, str], dict]] = {
        package["name"]: {} for package in workspace_packages
    }
    for source_package in workspace_packages:
        source_name = source_package["name"]
        for dependency in source_package.get("dependencies", []):
            kind = _dependency_kind(dependency.get("kind"))
            target = dependency.get("target")
            if kind not in KINDS:
                raise ValueError(f"unknown Cargo dependency kind {kind!r}")
            dependency_path = dependency.get("path")
            target_package = (
                workspace_paths.get(str(Path(dependency_path).resolve()))
                if dependency_path
                else None
            )
            if target_package is None:
                source = dependency.get("source") or ("path" if dependency_path else "unknown")
                external_key = (kind, dependency["name"], dependency["req"], target or "", source)
                external[source_name][kind].add("|".join(external_key[1:]))
                external_details[source_name][external_key] = {
                    "kind": kind,
                    "name": dependency["name"],
                    "requirement": dependency["req"],
                    "target": target,
                    "source": source,
                    "optional": dependency["optional"],
                    "features": sorted(dependency.get("features", [])),
                }
                continue
            target_name = target_package["name"]
            alias = dependency.get("rename")
            key = (source_name, target_name, kind, target, alias or "")
            edges_by_key[key] = {
                "from": source_name,
                "to": target_name,
                "kind": kind,
                "target": target,
                "alias": alias,
                "optional": dependency["optional"],
                "features": sorted(dependency.get("features", [])),
            }

    edges = sorted(
        edges_by_key.values(),
        key=lambda edge: (edge["from"], edge["kind"], edge["to"], edge["target"] or ""),
    )
    names = sorted(package_by_name)
    normal_build_edges = [edge for edge in edges if edge["kind"] in {"normal", "build"}]
    all_components = _strongly_connected_components(names, edges)
    normal_build_components = _strongly_connected_components(names, normal_build_edges)
    dev_components = [
        component
        for component in all_components
        if any(
            edge["kind"] == "dev" and edge["from"] in component and edge["to"] in component
            for edge in edges
        )
    ]

    consumers: dict[str, list[dict]] = {name: [] for name in names}
    direct: dict[str, list[dict]] = {name: [] for name in names}
    for edge in edges:
        direct[edge["from"]].append(edge)
        consumers[edge["to"]].append(
            {"package": edge["from"], "kind": edge["kind"], "target": edge["target"]}
        )

    packages = []
    for name in names:
        package = package_by_name[name]
        distribution = owners[name]
        packages.append({
            "name": name,
            "version": package["version"],
            "manifest": str(Path(package["manifest_path"]).relative_to(ROOT)).replace("\\", "/"),
            "owner": distribution["owner"],
            "role": distribution["role"],
            "channel": distribution["channel"],
            "cargo_publish": package.get("publish"),
            "external_dependency_count": {
                kind: len(external[name][kind]) for kind in KINDS
            },
            "external_dependencies": sorted(
                external_details[name].values(),
                key=lambda item: (item["kind"], item["name"], item["requirement"], item["target"] or ""),
            ),
            "direct_dependencies": direct[name],
            "direct_consumers": sorted(
                consumers[name],
                key=lambda item: (item["package"], item["kind"], item["target"] or ""),
            ),
        })

    return {
        "schema_version": 1,
        "source": {
            "cargo_metadata": "cargo metadata --no-deps --locked --format-version 1",
            "distribution_catalog": "maturity/package-distribution.json",
        },
        "workspace_package_count": len(packages),
        "internal_edge_count": len(edges),
        "packages": packages,
        "internal_edges": edges,
        "cycles": {
            "normal_build": normal_build_components,
            "dev_involving": dev_components,
        },
    }


def render_markdown(dependency_map: dict) -> str:
    package_count = dependency_map["workspace_package_count"]
    edge_count = dependency_map["internal_edge_count"]
    normal_cycles = dependency_map["cycles"]["normal_build"]
    dev_cycles = dependency_map["cycles"]["dev_involving"]
    lines = [
        "# Cargo workspace crate dependency map",
        "",
        "> Generated by `python tools/generate_crate_dependency_map.py` from locked Cargo package metadata and `maturity/package-distribution.json`. Do not edit this file by hand.",
        "> Machine-readable source: [`maturity/crate-dependency-map.json`](../maturity/crate-dependency-map.json).",
        "",
        f"Workspace packages: **{package_count}** · direct internal edges: **{edge_count}**.",
        "",
        "`normal` edges participate in runtime/library composition; `build` edges are build scripts; `dev` edges are test/benchmark-only. Target-specific edges retain the Cargo target expression. Optional dependencies and requested feature flags remain visible. External counts are distinct declared name/version/target/source combinations grouped by dependency kind.",
        "",
        "## Package index",
        "",
        "| Crate | Owner | Distribution | Normal deps | Build deps | Dev deps | External N/B/D | Normal/build consumers |",
        "|---|---|---|---|---|---|---|---|",
    ]
    for package in dependency_map["packages"]:
        deps = {kind: [] for kind in KINDS}
        consumers = set()
        for edge in package["direct_dependencies"]:
            target = edge["to"]
            if edge["target"]:
                target += f" (`{edge['target']}`)"
            deps[edge["kind"]].append(target)
        for consumer in package["direct_consumers"]:
            if consumer["kind"] in {"normal", "build"}:
                consumers.add(consumer["package"])
        def cell(values: list[str]) -> str:
            return ", ".join(f"`{value}`" for value in values) or "—"
        channel = f"{package['role']} / {package['channel']}"
        external_counts = package["external_dependency_count"]
        external = "/".join(str(external_counts[kind]) for kind in KINDS)
        lines.append(
            f"| [`{package['name']}`](../{package['manifest']}) | `{package['owner']}` | {channel} | "
            f"{cell(sorted(deps['normal']))} | {cell(sorted(deps['build']))} | "
            f"{cell(sorted(deps['dev']))} | {external} | {cell(sorted(consumers))} |"
        )

    lines.extend([
        "",
        "## Internal workspace edges",
        "",
        "Each row is one resolved direct edge. The Cargo target expression is shown when an edge is conditional.",
        "",
        "| From | To | Kind | Target | Optional | Features | Alias |",
        "|---|---|---|---|---|---|---|",
    ])
    for edge in dependency_map["internal_edges"]:
        target = f"`{edge['target']}`" if edge["target"] else "—"
        alias = f"`{edge['alias']}`" if edge["alias"] else "—"
        features = ", ".join(f"`{feature}`" for feature in edge["features"]) or "—"
        lines.append(
            f"| `{edge['from']}` | `{edge['to']}` | `{edge['kind']}` | "
            f"{target} | {str(edge['optional']).lower()} | {features} | {alias} |"
        )

    lines.extend(["", "## Dependency cycles", ""])
    if normal_cycles:
        lines.append("**Normal/build cycles need review:**")
        for component in normal_cycles:
            lines.append("- " + " ↔ ".join(f"`{name}`" for name in component))
    else:
        lines.append("No cycle was found among normal and build edges.")
    if dev_cycles:
        lines.append("")
        lines.append("**Dev-involving cycles (test-only edges):**")
        for component in dev_cycles:
            lines.append("- " + " ↔ ".join(f"`{name}`" for name in component))
    else:
        lines.append("No dev-involving cycle was found.")
    return "\n".join(lines) + "\n"


def cargo_metadata() -> dict:
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--locked", "--format-version", "1"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    return json.loads(result.stdout)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail if generated files are stale")
    args = parser.parse_args()
    try:
        catalog = json.loads(CATALOG_PATH.read_text(encoding="utf-8"))
        dependency_map = build_map(cargo_metadata(), catalog)
        generated = {
            JSON_PATH: json.dumps(dependency_map, indent=2, ensure_ascii=False) + "\n",
            DOC_PATH: render_markdown(dependency_map),
        }
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError, json.JSONDecodeError) as error:
        print(f"crate dependency map generation failed: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr.strip(), file=sys.stderr)
        return 2

    stale = [path for path, content in generated.items()
             if not path.exists() or path.read_text(encoding="utf-8") != content]
    if args.check:
        if stale:
            for path in stale:
                print(f"stale generated file: {path.relative_to(ROOT).as_posix()}", file=sys.stderr)
            print("Run `python tools/generate_crate_dependency_map.py` and commit the results.", file=sys.stderr)
            return 1
        cycles = dependency_map["cycles"]
        if cycles["normal_build"] or cycles["dev_involving"]:
            print("Cargo workspace dependency cycles are not allowed:", file=sys.stderr)
            for group_name, components in cycles.items():
                for component in components:
                    print(f"- {group_name}: {' -> '.join(component)}", file=sys.stderr)
            return 1
        print(
            f"Crate dependency map is current: {dependency_map['workspace_package_count']} packages, "
            f"{dependency_map['internal_edge_count']} internal edges."
        )
        return 0

    for path, content in generated.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8", newline="\n")
        print(f"wrote {path.relative_to(ROOT).as_posix()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
