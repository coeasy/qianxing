"""Build a deterministic, self-identifying static Web console release archive."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import html.parser
import io
import json
import re
import tarfile
from pathlib import Path
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_SOURCE = ROOT / "web" / "console"
SCHEMA_REGISTRY_FILE = ROOT / "maturity" / "schema-registry.json"
ASSET_FILES = ("index.html", "app.js", "styles.css")
VERSION_PATTERN = re.compile(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?")
IDENTITY_NAME = "release-identity.json"


class _AssetReferences(html.parser.HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.references: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        if tag.lower() == "script" and values.get("src"):
            self.references.append(values["src"] or "")
        elif tag.lower() == "link" and "stylesheet" in (values.get("rel") or "").lower().split():
            if values.get("href"):
                self.references.append(values["href"] or "")


def _asset_bytes(source_dir: Path) -> dict[str, bytes]:
    assets: dict[str, bytes] = {}
    for name in ASSET_FILES:
        path = source_dir / name
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"missing or symlinked console asset: {name}")
        assets[name] = path.read_bytes()
    parser = _AssetReferences()
    parser.feed(assets["index.html"].decode("utf-8"))
    references: list[str] = []
    for reference in parser.references:
        parsed = urlsplit(reference)
        if parsed.scheme or parsed.netloc or parsed.query or parsed.fragment or parsed.path != reference:
            raise ValueError(f"console assets must use local relative URLs: {reference!r}")
        if reference not in ASSET_FILES or reference == "index.html":
            raise ValueError(f"unpackaged console asset reference: {reference!r}")
        references.append(reference)
    expected = sorted(set(ASSET_FILES) - {"index.html"})
    if sorted(references) != expected:
        raise ValueError(f"index.html asset references {sorted(references)!r}; expected {expected!r}")
    if re.search(rb"(?i)@import\b|url\s*\(", assets["styles.css"]):
        raise ValueError("styles.css must not depend on unpackaged or external resources")
    return assets


def _validate_identity(version: str, commit: str) -> None:
    if VERSION_PATTERN.fullmatch(version) is None:
        raise ValueError(f"invalid release version: {version!r}")
    if re.fullmatch(r"[0-9a-fA-F]{40}", commit) is None:
        raise ValueError("git commit must be a full 40-character hexadecimal object id")


def _schema_registry_version() -> int:
    registry = json.loads(SCHEMA_REGISTRY_FILE.read_text(encoding="utf-8"))
    version = registry.get("registry_version") if isinstance(registry, dict) else None
    if not isinstance(version, int) or isinstance(version, bool) or version < 1:
        raise ValueError(f"invalid registry_version in {SCHEMA_REGISTRY_FILE}")
    return version


def _build_archive(entries: dict[str, bytes]) -> bytes:
    payload = io.BytesIO()
    with gzip.GzipFile(filename="", mode="wb", fileobj=payload, compresslevel=9, mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as archive:
            for name, data in sorted(entries.items()):
                info = tarfile.TarInfo(f"qianxing-web-console/{name}")
                info.size = len(data)
                info.mode = 0o644
                info.mtime = 0
                info.uid = 0
                info.gid = 0
                info.uname = ""
                info.gname = ""
                archive.addfile(info, io.BytesIO(data))
    return payload.getvalue()


def package_console(source_dir: Path, output: Path, version: str, commit: str) -> dict[str, object]:
    """Package the static assets and a self-identifying content manifest reproducibly."""
    _validate_identity(version, commit)
    assets = _asset_bytes(source_dir)
    identity: dict[str, object] = {
        "format": "qianxing-web-console-release-v1",
        "version": version,
        "git_commit": commit.lower(),
        "target_triple": "web-static",
        "profile": "release",
        "schema_registry_version": _schema_registry_version(),
        "distribution_boundary": "local-only",
        "requires_same_origin_bff": True,
        "csrf_supported": False,
        "session_permissions_supported": False,
        "desktop_host_supported": False,
        "sandbox_accepted": False,
        "production_accepted": False,
        "assets": [
            {"path": name, "sha256": hashlib.sha256(data).hexdigest()}
            for name, data in sorted(assets.items())
        ],
    }
    entries = dict(assets)
    entries[IDENTITY_NAME] = (
        json.dumps(identity, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n"
    ).encode("utf-8")
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(_build_archive(entries))
    return identity


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True, help="release version without the leading v")
    parser.add_argument("--commit", required=True, help="full 40-character git commit id")
    parser.add_argument("--output", required=True, type=Path, help="output .tar.gz path")
    parser.add_argument("--source", type=Path, default=DEFAULT_SOURCE, help="console asset directory")
    args = parser.parse_args()
    try:
        identity = package_console(args.source, args.output, args.version, args.commit)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    print(f"packaged {args.output} ({len(identity['assets'])} assets, commit {identity['git_commit']})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
