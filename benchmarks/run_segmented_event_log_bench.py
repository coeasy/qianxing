#!/usr/bin/env python3
"""Measure warm and reopened-store append costs for the segmented EventLog backend."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

from hardware import hardware_fingerprint

ROOT = Path(__file__).resolve().parent.parent
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
        default=ROOT / "maturity" / "evidence" / "rust-segmented-event-log-append-2026-10-17.json",
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
            if isinstance(value, dict) and value.get("benchmark") == "segmented_event_log_append":
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
        "benchmark": "segmented_event_log_append",
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "command": COMMAND,
        "hardware": hardware_fingerprint(),
        "rustc": rustc.stdout.strip(),
        "profile": "release",
        "storage": {
            "events_per_segment": 1_024,
            "same_instance": "manifest has been validated and cached by the writing store",
            "reopened_store": "new store instance validates and loads the existing segmented log during timed append",
            "os_file_cache_controlled": False,
        },
        "results": records,
        "limitations": [
            "This microbenchmark measures one appended event and segmented file storage only; it does not measure full backtest throughput.",
            "The reopened-store path includes full log decoding and validation, while the operating-system file cache is uncontrolled.",
            "Synthetic EventKind::Settle histories do not represent production event mixes or the nine product profiles.",
        ],
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"saved segmented EventLog benchmark evidence to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
