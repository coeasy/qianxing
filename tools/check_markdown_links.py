#!/usr/bin/env python3
"""Check repository-local Markdown links, including case-sensitive path spelling."""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path, PurePosixPath
from urllib.parse import unquote, urlsplit


LINK = re.compile(r"(?<!!)\[[^\]]*\]\(([^)]+)\)")


def tracked_markdown() -> list[Path]:
    output = subprocess.check_output(["git", "ls-files", "-z", "--", "*.md"])
    return [Path(raw.decode("utf-8")) for raw in output.split(b"\0") if raw]


def local_target_exists(source: PurePosixPath, target: str) -> bool:
    parsed = urlsplit(target)
    if parsed.scheme or parsed.netloc:
        return True
    path = unquote(parsed.path)
    if not path:
        return True

    parts = list(source.parent.parts)
    for part in PurePosixPath(path).parts:
        if part in ("", ".", "/"):
            continue
        if part == "..":
            if not parts:
                return False
            parts.pop()
            continue
        current = Path(*parts)
        try:
            names = {entry.name for entry in current.iterdir()}
        except OSError:
            return False
        if part not in names:
            return False
        parts.append(part)
    return Path(*parts).exists()


def main() -> int:
    failures: list[str] = []
    files = tracked_markdown()
    for source in files:
        if not source.exists():
            continue
        try:
            content = source.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as exc:
            failures.append(f"{source}: cannot read UTF-8 Markdown: {exc}")
            continue
        for match in LINK.finditer(content):
            target = match.group(1).strip().strip("<>")
            if not local_target_exists(source, target):
                line = content.count("\n", 0, match.start()) + 1
                failures.append(f"{source}:{line}: broken or case-mismatched link: {target}")

    if failures:
        print("\n".join(failures), file=sys.stderr)
        print(f"Markdown link check failed: {len(failures)} broken link(s)", file=sys.stderr)
        return 1
    print(f"Markdown links are valid: {len(files)} tracked files checked")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
