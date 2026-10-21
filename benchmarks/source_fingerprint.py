"""Fingerprint the Rust and fixture inputs used by release benchmarks."""

from __future__ import annotations

import hashlib
from pathlib import Path
import re


MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA = 2


def _workspace_version(root: Path) -> str | None:
    manifest = (root / "Cargo.toml").read_text(encoding="utf-8")
    section = re.search(r"^\[workspace\.package\]\s*$([\s\S]*?)(?=^\[|\Z)", manifest, re.MULTILINE)
    if section is None:
        return None
    match = re.search(r'^version\s*=\s*"([^"]+)"\s*$', section.group(1), re.MULTILINE)
    return match.group(1) if match else None


def _normalized_manifest(path: Path, workspace_version: str) -> bytes:
    content = path.read_text(encoding="utf-8")
    # Cargo package/dependency versions change on every release. They do not
    # change benchmarked source; dependency versions remain fingerprinted.
    if path.name == "Cargo.lock":
        local_names = set()
        for manifest in (path.parent / "crates").glob("*/Cargo.toml"):
            text = manifest.read_text(encoding="utf-8")
            package = re.search(r"^\[package\]\s*$([\s\S]*?)(?=^\[|\Z)", text, re.MULTILINE)
            if package:
                name = re.search(r'^name\s*=\s*"([^"]+)"', package.group(1), re.MULTILINE)
                if name:
                    local_names.add(name.group(1))
        blocks = re.split(r"(?=^\[\[package\]\])", content, flags=re.MULTILINE)
        for index, block in enumerate(blocks):
            name = re.search(r'^name\s*=\s*"([^"]+)"\s*$', block, re.MULTILINE)
            if name and name.group(1) in local_names:
                blocks[index] = re.sub(
                    r'^version\s*=\s*"[^"]+"\s*$',
                    'version = "<workspace-release>"',
                    block,
                    count=1,
                    flags=re.MULTILINE,
                )
        content = "".join(blocks)
    else:
        content = content.replace(
            f'version = "{workspace_version}"', 'version = "<workspace-release>"'
        )
    return content.encode("utf-8")


def measurement_source_fingerprint(root: Path) -> str:
    candidates = [root / "Cargo.toml", root / "Cargo.lock"]
    for manifest in (root / "crates").glob("*/Cargo.toml"):
        candidates.append(manifest)
    candidates.extend((root / "crates").rglob("*.rs"))
    candidates.extend(
        root / "benchmarks" / name
        for name in (
            "run_baseline.py",
            "run_rust_bench.py",
            "run_ashare_profile_backtest.py",
            "run_crypto_paper_e2e.py",
            "hardware.py",
        )
    )
    candidates.extend((root / "maturity" / "fixtures").rglob("*"))

    workspace_version = _workspace_version(root)
    digest = hashlib.sha256()
    for path in sorted({path for path in candidates if path.is_file()}):
        digest.update(path.relative_to(root).as_posix().encode("utf-8"))
        digest.update(b"\0")
        if workspace_version and path.name in {"Cargo.toml", "Cargo.lock"}:
            content = _normalized_manifest(path, workspace_version)
        else:
            content = path.read_bytes()
        digest.update(content)
        digest.update(b"\0")
    return digest.hexdigest()
