#!/usr/bin/env python3
"""Compare one-event append costs for flat and segmented EventLog file backends."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

from hardware import hardware_fingerprint

ROOT = Path(__file__).resolve().parent.parent
SOURCE_FILES = (
    "benchmarks/run_segmented_event_log_bench.py",
    "crates/qx-storage/examples/segmented_event_log_append.rs",
    "crates/qx-storage/src/lib.rs",
    "crates/qx-storage/src/segmented_event_log.rs",
)
COMMAND = [
    "cargo",
    "run",
    "-p",
    "qx-storage",
    "--example",
    "segmented_event_log_append",
    "--release",
    "--offline",
    "-j1",
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out",
        type=Path,
        default=ROOT / "maturity" / "evidence" / "rust-event-log-file-backend-append-comparison-2026-11-07.json",
    )
    args = parser.parse_args()
    process = subprocess.run(
        COMMAND,
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if process.returncode:
        sys.stderr.write(process.stdout)
        sys.stderr.write(process.stderr)
        return process.returncode

    records = []
    try:
        for line in process.stdout.splitlines():
            value = json.loads(line)
            if isinstance(value, dict) and value.get("benchmark") == "event_log_file_backend_append_comparison":
                records.append(value)
    except json.JSONDecodeError as error:
        print(f"benchmark emitted invalid JSON: {error}", file=sys.stderr)
        return 1
    if [row.get("history_events") for row in records] != [1_000, 8_000, 64_000]:
        print("benchmark output does not contain the required history sizes", file=sys.stderr)
        return 1

    rustc = subprocess.run(
        ["rustc", "--version"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=True,
    )
    report = {
        "schema_version": 1,
        "benchmark": "event_log_file_backend_append_comparison",
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "command": COMMAND,
        "hardware": hardware_fingerprint(),
        "rustc": rustc.stdout.strip(),
        "profile": "release",
        "source_sha256": {
            path: hashlib.sha256((ROOT / path).read_bytes()).hexdigest()
            for path in SOURCE_FILES
        },
        "storage": {
            "events_per_segment": 1_024,
            "flat_file": "each timed append uses EventLogFileStore::write, including existing-log read, prefix validation, full JSON serialization, atomic replacement and sync",
            "segmented_same_instance": "manifest has been validated and cached by the writing store; timed append writes only the active segment tail and manifest",
            "segmented_reopened_store": "new store instance validates and loads the existing segmented log during timed append",
            "os_file_cache_controlled": False,
        },
        "results": records,
        "limitations": [
            "This microbenchmark measures one appended event after constructing the in-memory history; it does not measure full backtest throughput.",
            "The flat-file path includes full existing-log decoding, prefix validation, serialization and atomic write; segmented reopened-store includes full history verification.",
            "Operating-system file cache is uncontrolled, and results are host-specific rather than product capacity guarantees.",
            "Synthetic EventKind::Settle histories do not represent production event mixes or the nine product profiles.",
        ],
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"saved EventLog file backend comparison evidence to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
