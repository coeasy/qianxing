"""Detect whether a revision changes inputs that performance gates compare."""

from __future__ import annotations

import argparse
from pathlib import Path, PurePosixPath
import subprocess
import sys


def is_measured_source_input(path: str) -> bool:
    normalized = PurePosixPath(path.replace("\\", "/"))
    if normalized.as_posix() in {
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        ".cargo/config",
        ".cargo/config.toml",
        "maturity/targets.yaml",
    }:
        return True
    if normalized.parts[:2] == (".cargo", "config"):
        return True
    if normalized.parts[:2] == ("maturity", "fixtures"):
        return True
    return (
        len(normalized.parts) >= 2
        and normalized.parts[0] == "crates"
        and (normalized.suffix == ".rs" or normalized.name == "Cargo.toml")
    )


def changed_measured_inputs(root: Path, base: str, head: str) -> bool:
    result = subprocess.run(
        ["git", "diff", "--name-only", base, head],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    return any(is_measured_source_input(path) for path in result.stdout.splitlines())


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", help="parent revision")
    parser.add_argument("head", help="candidate revision")
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    try:
        changed = changed_measured_inputs(args.root.resolve(), args.base, args.head)
    except subprocess.CalledProcessError as error:
        sys.stderr.write(error.stderr)
        return error.returncode
    print("true" if changed else "false")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
