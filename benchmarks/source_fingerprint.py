"""Fingerprint the Rust and fixture inputs used by release benchmarks."""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
import re


MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA = 5


def _normalized_rust_source(source: str) -> bytes:
    """Return Rust tokens with comments and formatting whitespace removed.

    Documentation edits do not change benchmarked behavior. Treat comments as
    whitespace, while preserving quoted and raw string contents because those
    can be runtime inputs or macro tokens.
    """
    output: list[str] = []
    index = 0
    pending_space = False

    def emit(value: str) -> None:
        nonlocal pending_space
        if pending_space and output:
            output.append(" ")
        output.append(value)
        pending_space = False

    def raw_string_start(offset: int) -> tuple[int, int] | None:
        prefix_length = 0
        if source.startswith(("br", "cr"), offset):
            prefix_length = 2
        elif source.startswith("r", offset):
            prefix_length = 1
        else:
            return None
        cursor = offset + prefix_length
        while cursor < len(source) and source[cursor] == "#":
            cursor += 1
        if cursor < len(source) and source[cursor] == '"':
            return cursor, cursor - (offset + prefix_length)
        return None

    def char_literal_end(offset: int) -> int | None:
        cursor = offset + 1
        if cursor >= len(source) or source[cursor] in "\r\n":
            return None
        if source[cursor] == "\\":
            cursor += 1
            if cursor >= len(source):
                return None
            if source.startswith("u{", cursor):
                end = source.find("}", cursor + 2)
                if end < 0:
                    return None
                cursor = end + 1
            elif source[cursor] == "x":
                cursor += 3
            else:
                cursor += 1
        else:
            cursor += 1
        return cursor + 1 if cursor < len(source) and source[cursor] == "'" else None

    while index < len(source):
        char = source[index]

        if char.isspace():
            pending_space = True
            index += 1
            continue

        raw_start = raw_string_start(index)
        if raw_start is not None:
            quote, hash_count = raw_start
            closing = '"' + ("#" * hash_count)
            end = source.find(closing, quote + 1)
            end = len(source) if end < 0 else end + len(closing)
            emit(source[index:end])
            index = end
            continue

        if char == '"':
            end = index + 1
            while end < len(source):
                if source[end] == "\\":
                    end += 2
                elif source[end] == '"':
                    end += 1
                    break
                else:
                    end += 1
            emit(source[index:end])
            index = end
            continue

        if char == "'":
            end = char_literal_end(index)
            if end is None:
                emit(char)
                index += 1
            else:
                emit(source[index:end])
                index = end
            continue

        if source.startswith("//", index):
            newline = source.find("\n", index + 2)
            index = len(source) if newline < 0 else newline + 1
            pending_space = True
            continue

        if source.startswith("/*", index):
            depth = 1
            cursor = index + 2
            while cursor < len(source) and depth:
                if source.startswith("/*", cursor):
                    depth += 1
                    cursor += 2
                elif source.startswith("*/", cursor):
                    depth -= 1
                    cursor += 2
                else:
                    cursor += 1
            index = cursor
            pending_space = True
            continue

        emit(char)
        index += 1

    return "".join(output).encode("utf-8")


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
    # Walk only source/test trees. `Path.rglob` over the entire crates directory
    # also descends into `crates/target` and nested task scratch directories,
    # where integration tests can leave tens of thousands of generated files.
    for current, directories, filenames in os.walk(root / "crates"):
        directories[:] = sorted(
            name for name in directories if name not in {"target", ".task-tmp"}
        )
        candidates.extend(
            Path(current) / filename
            for filename in filenames
            if filename.endswith(".rs")
        )
    candidates.extend(
        root / "benchmarks" / name
        for name in (
            "run_baseline.py",
            "run_rust_bench.py",
            "run_ashare_profile_backtest.py",
            "run_profile_backtest.py",
            "run_concurrent_capacity.py",
            "run_crypto_paper_e2e.py",
            "run_multi_account_capacity.py",
            "run_multi_market_depth_capacity.py",
            "run_prepared_evaluation_capacity.py",
            "process_metrics.py",
            "hardware.py",
            "source_fingerprint.py",
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
        elif path.suffix == ".rs":
            content = _normalized_rust_source(path.read_text(encoding="utf-8"))
        else:
            content = path.read_bytes()
        digest.update(content)
        digest.update(b"\0")
    return digest.hexdigest()
