#!/usr/bin/env python3
"""Reject mislabeled or pure-Python wheels that contain the Rust extension."""

from __future__ import annotations

import sys
import zipfile
from pathlib import Path


def main() -> int:
    directory = Path(sys.argv[1] if len(sys.argv) > 1 else "dist")
    wheels = sorted(directory.glob("qianxing-*.whl"))
    if len(wheels) != 1:
        raise SystemExit(f"expected exactly one qianxing wheel in {directory}, found {len(wheels)}")

    wheel = wheels[0]
    if "-cp310-abi3-" not in wheel.name:
        raise SystemExit(f"wheel filename does not carry the cp310-abi3 tag: {wheel.name}")

    with zipfile.ZipFile(wheel) as archive:
        metadata = [
            name for name in archive.namelist()
            if name.endswith(".dist-info/WHEEL")
        ]
        if len(metadata) != 1:
            raise SystemExit(f"expected one WHEEL metadata file, found {len(metadata)}")
        content = archive.read(metadata[0]).decode("utf-8")
        if "Root-Is-Purelib: false" not in content:
            raise SystemExit("native extension wheel is incorrectly marked pure Python")
        tags = [line.removeprefix("Tag: ") for line in content.splitlines() if line.startswith("Tag: ")]
        if len(tags) != 1 or not tags[0].startswith("cp310-abi3-"):
            raise SystemExit(f"WHEEL metadata does not declare one cp310-abi3 platform tag: {tags}")
        native = [
            name for name in archive.namelist()
            if Path(name).name.startswith("_qianxing_native.")
            and name.endswith((".pyd", ".so", ".dylib", ".dll"))
        ]
        if len(native) != 1:
            raise SystemExit(f"expected one bundled native extension, found {native}")

    print(f"stable-ABI wheel valid: {wheel.name} ({native[0]})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
