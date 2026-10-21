"""Fingerprint the Rust and fixture inputs used by release benchmarks."""

from __future__ import annotations

import hashlib
from pathlib import Path


def measurement_source_fingerprint(root: Path) -> str:
    candidates = [root / "Cargo.toml", root / "Cargo.lock"]
    for manifest in (root / "crates").glob("*/Cargo.toml"):
        candidates.append(manifest)
    candidates.extend((root / "crates").rglob("*.rs"))
    candidates.extend(
        root / "benchmarks" / name
        for name in (
            "run_baseline.py",
            "run_rust_bench.py",
            "run_ashare_profile_backtest.py",
            "run_crypto_paper_e2e.py",
            "hardware.py",
        )
    )
    candidates.extend((root / "maturity" / "fixtures").rglob("*"))

    digest = hashlib.sha256()
    for path in sorted({path for path in candidates if path.is_file()}):
        digest.update(path.relative_to(root).as_posix().encode("utf-8"))
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()
