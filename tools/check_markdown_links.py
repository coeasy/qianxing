#!/usr/bin/env python3
"""Check repository-local Markdown links, including case-sensitive path spelling.

Every tracked Markdown file is checked, archives included: a link that 404s in an archived snapshot is
still dead for the reader, and the earlier archive/one-file exemptions silently left 20 of 78 tracked
documents unchecked. A deliberate exemption must be added back with its measured reason.

Backticked repo paths are also checked. These documents cite files as prose rather than as links, and only
the link form reached this checker: a citation spelled "docs/archive/…-V1.md" against a file tracked as
"…-v1.md" opens on a case-insensitive working copy and 404s on the Linux CI checkout, with nothing anywhere
in the repo able to see it.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path, PurePosixPath
from urllib.parse import unquote, urlsplit


LINK = re.compile(r"(?<!!)\[[^\]]*\]\(([^)]+)\)")
CITATION = re.compile(r"`([^`\n]{2,200})`")
PATH_CITATION = re.compile(r"[\w\-.]+(?:/[\w\-.]+)+\.[A-Za-z0-9]{1,6}\Z")


def tracked_markdown() -> list[Path]:
    output = subprocess.check_output(["git", "ls-files", "-z", "--", "*.md"])
    return [Path(raw.decode("utf-8")) for raw in output.split(b"\0") if raw]


def tracked_paths() -> set[str]:
    output = subprocess.check_output(["git", "ls-files", "-z"])
    return {raw.decode("utf-8") for raw in output.split(b"\0") if raw}


def citation_path(cited: str) -> str:
    """Strip the shell-navigation prefix from a cited repo path before it is judged.

    Shell-facing prose writes ./build.sh where the index spells it build.sh, and every membership test here
    compares against the index. Left unnormalised, that prefix both manufactures unreachable paths and hides
    a mis-cased spelling underneath it: ./docs/archive/X-V1.md would look absent for a different reason.
    """
    return cited[2:] if cited.startswith("./") else cited


def path_citations(source: Path, content: str, tracked: set[str],
                   by_lowercase: dict[str, list[str]]) -> tuple[int, int, list[str]]:
    """Return (citations examined, ambiguous, failures) for backticked repo paths in one document.

    Only a citation whose exact spelling is absent but whose lowercase form is tracked *exactly once* is
    flagged: that is the one judgement a case-insensitive filesystem cannot make for itself. Anything else
    (a planned path, a bare file name, a glob) is counted as examined and left unjudged, so the check can
    never become the noise that gets it disabled.
    """
    examined = 0
    ambiguous = 0
    failures: list[str] = []
    for match in CITATION.finditer(content):
        cited = match.group(1).strip().strip("<>")
        if "://" in cited or not PATH_CITATION.match(cited):
            continue
        examined += 1
        path = citation_path(cited)
        if path in tracked:
            continue
        variants = by_lowercase.get(path.lower(), [])
        if len(variants) == 1:
            line = content.count("\n", 0, match.start()) + 1
            failures.append(f"{source.as_posix()}:{line}: case-mismatched path citation: "
                            f"{cited} (tracked as {variants[0]})")
        elif variants:
            ambiguous += 1
    return examined, ambiguous, failures


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
    link_failures: list[str] = []
    citation_failures: list[str] = []
    files = tracked_markdown()
    tracked = tracked_paths()
    by_lowercase: dict[str, list[str]] = {}
    for path in tracked:
        by_lowercase.setdefault(path.lower(), []).append(path)
    examined = 0
    ambiguous = 0
    for source in files:
        if not source.exists():
            continue
        try:
            content = source.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as exc:
            link_failures.append(f"{source}: cannot read UTF-8 Markdown: {exc}")
            continue
        for match in LINK.finditer(content):
            target = match.group(1).strip().strip("<>")
            if not local_target_exists(source, target):
                line = content.count("\n", 0, match.start()) + 1
                link_failures.append(f"{source}:{line}: broken or case-mismatched link: {target}")
        seen, ambig, cited_bad = path_citations(source, content, tracked, by_lowercase)
        examined += seen
        ambiguous += ambig
        citation_failures.extend(cited_bad)

    failures = link_failures + citation_failures
    if failures:
        print("\n".join(failures), file=sys.stderr)
        print(f"Markdown check failed: {len(link_failures)} broken link(s), "
              f"{len(citation_failures)} case-mismatched citation(s)", file=sys.stderr)
        return 1
    print(f"Markdown links are valid: {len(files)} tracked files checked")
    print(f"Path citations examined: {examined}, case-mismatched: {len(citation_failures)}, "
          f"ambiguous: {ambiguous}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
