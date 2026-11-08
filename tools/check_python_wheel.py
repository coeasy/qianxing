#!/usr/bin/env python3
"""Audit stable ABI wheels, including the platform Rust report renderer payload."""
from __future__ import annotations

import re
import struct
import sys
import zipfile
from email.parser import Parser
from pathlib import Path


def binary_platform(header: bytes) -> tuple[str, str]:
    """Read native object headers without executing foreign-platform payloads."""
    try:
        if header[:2] == b"MZ":
            offset = struct.unpack_from("<I", header, 0x3C)[0]
            if header[offset:offset + 4] != b"PE\0\0":
                raise ValueError("invalid PE signature")
            machine = struct.unpack_from("<H", header, offset + 4)[0]
            return "windows", {0x8664: "x86_64", 0x14C: "i686", 0xAA64: "aarch64"}[machine]
        if header[:4] == b"\x7fELF":
            endian = "<" if header[5] == 1 else ">"
            machine = struct.unpack_from(endian + "H", header, 18)[0]
            return "linux", {62: "x86_64", 3: "i686", 183: "aarch64", 40: "armv7l",
                             21: "ppc64le" if endian == "<" else "ppc64", 22: "s390x",
                             243: "riscv64"}[machine]
        if header[:4] in (b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf"):
            endian = "<" if header[:4] == b"\xcf\xfa\xed\xfe" else ">"
            cpu = struct.unpack_from(endian + "I", header, 4)[0]
            return "macos", {0x1000007: "x86_64", 0x100000C: "aarch64"}[cpu]
    except (IndexError, KeyError, struct.error) as error:
        raise ValueError("unrecognized native executable architecture") from error
    raise ValueError("unrecognized native executable format (universal binaries are not supported)")


def tag_platform(tag: str) -> tuple[str, str]:
    if tag == "win32":
        return "windows", "i686"
    if tag.startswith("win_"):
        return "windows", {"amd64": "x86_64", "arm64": "aarch64"}.get(tag[4:], tag[4:])
    system = "macos" if tag.startswith("macosx_") else "linux" if "linux" in tag else "unknown"
    for arch in ("x86_64", "aarch64", "arm64", "armv7l", "ppc64le", "ppc64", "s390x", "riscv64", "i686"):
        if tag.endswith("_" + arch):
            return system, "aarch64" if arch == "arm64" else arch
    raise ValueError(f"unsupported wheel platform tag: {tag}")


def audit_wheel(wheel: Path) -> dict[str, str]:
    if "-cp310-abi3-" not in wheel.name:
        raise ValueError(f"wheel filename does not carry the cp310-abi3 tag: {wheel.name}")
    with zipfile.ZipFile(wheel) as archive:
        names = archive.namelist()
        if len(names) != len(set(names)):
            raise ValueError("wheel contains duplicate entries")
        metadata = [name for name in names if name.endswith(".dist-info/WHEEL")]
        if len(metadata) != 1:
            raise ValueError(f"expected one WHEEL metadata file, found {len(metadata)}")
        content = archive.read(metadata[0]).decode("utf-8")
        if "Root-Is-Purelib: false" not in content:
            raise ValueError("native extension wheel is incorrectly marked pure Python")
        tags = [line.removeprefix("Tag: ") for line in content.splitlines() if line.startswith("Tag: ")]
        if len(tags) != 1 or not tags[0].startswith("cp310-abi3-"):
            raise ValueError(f"WHEEL metadata does not declare one cp310-abi3 platform tag: {tags}")
        platform_tag = tags[0].removeprefix("cp310-abi3-")
        if wheel.name.rsplit("-", 1)[-1] != platform_tag + ".whl":
            raise ValueError("wheel filename and WHEEL platform tags differ")
        platforms = {tag_platform(tag) for tag in platform_tag.split(".")}
        if len(platforms) != 1:
            raise ValueError("wheel has conflicting platform tags")
        expected = platforms.pop()
        native = [name for name in names if Path(name).name.startswith("_qianxing_native.")
                  and name.endswith((".pyd", ".so", ".dylib", ".dll"))]
        if len(native) != 1:
            raise ValueError(f"expected one bundled native extension, found {native}")
        renderer = "qianxing/_bin/" + ("qx-cli.exe" if expected[0] == "windows" else "qx-cli")
        payload = [name for name in names if name.startswith("qianxing/_bin/") and not name.endswith("/")]
        if payload != [renderer]:
            raise ValueError(f"expected only the platform report renderer {renderer}, found {payload}")
        for name in (native[0], renderer):
            with archive.open(name) as stream:
                actual = binary_platform(stream.read(65536))
            if actual != expected:
                raise ValueError(f"payload platform mismatch for {name}: {actual} != {expected}")
        if expected[0] != "windows" and not (archive.getinfo(renderer).external_attr >> 16) & 0o111:
            raise ValueError("bundled report renderer is not executable")
        caches = [name for name in names if "__pycache__" in name.split("/") or name.endswith(".pyc")]
        if caches:
            raise ValueError(f"wheel contains Python cache artifacts: {caches}")
        package_metadata = [name for name in names if name.endswith(".dist-info/METADATA")]
        if len(package_metadata) != 1:
            raise ValueError("expected one package METADATA file")
        package = Parser().parsestr(archive.read(package_metadata[0]).decode("utf-8"))
        for dependency in package.get_all("Requires-Dist", []):
            if not re.search(r";.*\bextra\s*==\s*['\"]", dependency):
                raise ValueError(f"base wheel must have no mandatory dependencies: {dependency}")
    return {"wheel": wheel.name, "native": native[0], "report_renderer": renderer}


def main() -> int:
    directory = Path(sys.argv[1] if len(sys.argv) > 1 else "dist").resolve()
    wheels = sorted(directory.glob("qianxing-*.whl"))
    if len(wheels) != 1:
        raise SystemExit(f"expected exactly one qianxing wheel in {directory}, found {len(wheels)}")
    try:
        result = audit_wheel(wheels[0])
    except ValueError as error:
        raise SystemExit(str(error)) from error
    print(f"stable-ABI wheel valid: {result['wheel']} ({result['native']}, {result['report_renderer']})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
