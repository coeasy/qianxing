#!/usr/bin/env python3
"""Measure the real Paper worker acceptance chain on isolated local roots."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

from hardware import hardware_fingerprint
from source_fingerprint import MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA, measurement_source_fingerprint

ROOT = Path(__file__).resolve().parent.parent
ACCEPTANCE = ROOT / "tools" / "paper_acceptance.py"
MIN_REPEATS = 3


def find_binary(value: str | None) -> Path:
    candidates = [Path(value)] if value else [
        ROOT / "target" / "release" / "qx-cli.exe",
        ROOT / "target" / "release" / "qx-cli",
        ROOT / "target" / "debug" / "qx-cli.exe",
        ROOT / "target" / "debug" / "qx-cli",
    ]
    for candidate in candidates:
        if candidate.is_file():
            return candidate.resolve()
    raise SystemExit("找不到 qx-cli；请先构建或使用 --binary 指定")


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, math.ceil(fraction * len(ordered)) - 1))
    return ordered[index]


def run_acceptance(binary: Path, root: Path) -> float:
    started = time.perf_counter()
    process = subprocess.run(
        [
            sys.executable,
            str(ACCEPTANCE),
            "--binary",
            str(binary),
            "--root",
            str(root),
            "--dry-run",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=300,
    )
    elapsed_ms = (time.perf_counter() - started) * 1_000.0
    if process.returncode != 0:
        raise SystemExit(
            f"Paper acceptance 失败（exit={process.returncode}）：\n"
            f"{(process.stdout or '')[-4000:]}\n{(process.stderr or '')[-4000:]}"
        )
    return elapsed_ms


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary")
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument(
        "--out",
        type=Path,
        default=ROOT / "benchmarks" / "results" / "crypto-paper-e2e.json",
    )
    args = parser.parse_args()
    if args.repeats < MIN_REPEATS:
        parser.error(f"--repeats 至少为 {MIN_REPEATS}")

    binary = find_binary(args.binary)
    samples: list[float] = []
    with tempfile.TemporaryDirectory(prefix="qx-crypto-paper-e2e-", dir=ROOT / "target") as temporary:
        work = Path(temporary)
        run_acceptance(binary, work / "warmup")
        for index in range(args.repeats):
            samples.append(run_acceptance(binary, work / f"sample-{index}"))

    report = {
        "schema_version": 1,
        "evidence_kind": "crypto_paper_worker_acceptance_e2e",
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "hardware": hardware_fingerprint(),
        "measurement_source_fingerprint": measurement_source_fingerprint(ROOT),
        "measurement_source_fingerprint_schema": MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA,
        "binary": binary.name,
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "scenario": "tools/paper_acceptance.py: scheduler -> strategy -> Paper execution -> Ledger/EventLog/audit, same-root idempotent rerun, and independent-root parity",
        "profile_coverage_claim": False,
        "records": [
            {
                "benchmark": "end_to_end",
                "operations": "crypto-paper-worker-acceptance-v1",
                "samples": args.repeats,
                "median_ms": round(percentile(samples, 0.50), 3),
                "p95_ms": round(percentile(samples, 0.95), 3),
                "raw_ms": [round(sample, 3) for sample in samples],
            }
        ],
        "limitations": [
            "The worker acceptance fixture uses one Paper order and does not model funding provider retrieval, eight-hour funding cadence, or an externally deployed database.",
            "Maker/taker, precision, weighted rate-limit, partial-fill and funding-ledger performance is measured separately by qx_zhenlu_crypto_paper_lifecycle_v2; this E2E row is not a complete crypto-paper profile claim.",
        ],
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
