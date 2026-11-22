#!/usr/bin/env python3
"""Run the published qx-sdk quickstart as an isolated crates.io consumer."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tempfile
from pathlib import Path

from publish_rust_sdk import run_registry_consumer


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True, help="exact published X.Y.Z version")
    args = parser.parse_args()
    if re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", args.version) is None:
        parser.error("--version must be an exact X.Y.Z version")

    root = Path(__file__).resolve().parents[1]
    try:
        with tempfile.TemporaryDirectory(prefix="qx-sdk-registry-consumer-") as temporary:
            cargo_home = Path(temporary) / "cargo-home"
            cargo_home.mkdir()
            run_registry_consumer(root, args.version, cargo_home=cargo_home)
    except subprocess.CalledProcessError as error:
        print(f"crates.io consumer verification failed: {error}", file=sys.stderr)
        if error.stdout:
            print(error.stdout.strip(), file=sys.stderr)
        if error.stderr:
            print(error.stderr.strip(), file=sys.stderr)
        return 1
    except (OSError, ValueError) as error:
        print(f"crates.io consumer verification failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
