"""Create a deterministic tar.gz archive from an installed C++ SDK tree."""

from __future__ import annotations

import argparse
import gzip
import os
import stat
import tarfile
from pathlib import Path


def _members(source: Path) -> list[Path]:
    if source.is_symlink() or not source.is_dir():
        raise ValueError(f"SDK source must be a real directory: {source}")
    paths = [source]
    for root, directories, files in os.walk(source, followlinks=False):
        root_path = Path(root)
        for name in directories:
            item = root_path / name
            if item.is_symlink():
                raise ValueError(f"SDK archive refuses symlink: {item}")
            paths.append(item)
        for name in files:
            item = root_path / name
            if item.is_symlink():
                raise ValueError(f"SDK archive refuses symlink: {item}")
            if not item.is_file():
                raise ValueError(f"SDK archive accepts only regular files: {item}")
            paths.append(item)
    return sorted(paths, key=lambda path: path.relative_to(source.parent).as_posix())


def package_sdk(source: Path, output: Path, *, mtime: int = 0) -> None:
    """Package *source* under the stable top-level ``sdk/`` path."""
    if source.is_symlink():
        raise ValueError(f"SDK source must not be a symlink: {source}")
    source = source.resolve()
    output = output.resolve()
    if output.is_relative_to(source):
        raise ValueError("SDK archive output must be outside the installed SDK tree")
    members = _members(source)
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as archive:
                for path in members:
                    arcname = path.relative_to(source.parent).as_posix()
                    info = archive.gettarinfo(str(path), arcname=arcname)
                    info.mtime = mtime
                    info.uid = 0
                    info.gid = 0
                    info.uname = ""
                    info.gname = ""
                    info.pax_headers = {}
                    if info.isfile():
                        info.mode = stat.S_IMODE(path.stat().st_mode)
                        with path.open("rb") as payload:
                            archive.addfile(info, payload)
                    else:
                        archive.addfile(info)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--mtime", type=int, default=0)
    args = parser.parse_args()
    if args.mtime < 0:
        parser.error("--mtime must be non-negative")
    try:
        package_sdk(args.source, args.output, mtime=args.mtime)
    except (OSError, ValueError, tarfile.TarError) as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
