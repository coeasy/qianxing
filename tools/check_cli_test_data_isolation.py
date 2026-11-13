"""Fail when CLI tests leave runtime artifacts in repository data directories."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

DATA_PATHS = ("deploy/data", "crates/qx-cli/data")


def git_data_paths(root: Path, *arguments: str) -> list[str]:
    result = subprocess.run(
        ["git", *arguments, "--", *DATA_PATHS],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    return result.stdout.splitlines()


def changed_runtime_data(root: Path) -> list[str]:
    tracked = git_data_paths(root, "diff", "--name-only", "HEAD")
    untracked = git_data_paths(root, "ls-files", "--others", "--exclude-standard")
    ignored = git_data_paths(root, "ls-files", "--others", "--ignored", "--exclude-standard")
    return sorted(set(tracked + untracked + ignored))


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    changed = changed_runtime_data(root)
    if changed:
        print("CLI tests changed or generated repository runtime data:", file=sys.stderr)
        for path in changed:
            print(f"  {path}", file=sys.stderr)
        print("Set each test storage.data_dir to an isolated temporary directory.", file=sys.stderr)
        return 1
    print("CLI tests left deploy/data and crates/qx-cli/data unchanged.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
