#!/usr/bin/env python3
"""Measure external EventLog append plus full runtime pipeline refresh."""

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
    "benchmarks/run_event_log_pipeline_tail_bench.py",
    "crates/qx-runtime/examples/event_log_pipeline_tail_bench.rs",
    "crates/qx-runtime/src/pipeline.rs",
    "crates/qx-storage/src/lib.rs",
    "crates/qx-storage/src/segmented_event_log.rs",
    "crates/qx-core/src/sourcing/event_log.rs",
    "crates/qx-core/src/event.rs",
)
COMMAND = [
    "cargo",
    "run",
    "-p",
    "qx-runtime",
    "--example",
    "event_log_pipeline_tail_bench",
    "--release",
    "--offline",
    "-j1",
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out",
        type=Path,
        default=ROOT
        / "maturity"
        / "evidence"
        / "rust-event-log-pipeline-tail-comparison-2026-11-07.json",
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
            if isinstance(value, dict) and value.get("benchmark") == "event_log_pipeline_ingest_refresh":
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
        "benchmark": "event_log_pipeline_ingest_refresh",
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "command": COMMAND,
        "hardware": hardware_fingerprint(),
        "rustc": rustc.stdout.strip(),
        "profile": "release",
        "source_sha256": {
            path: hashlib.sha256((ROOT / path).read_bytes()).hexdigest()
            for path in SOURCE_FILES
        },
        "workload": {
            "write": "one MarketQuote through LiveEventPipeline::ingest",
            "refresh": "LiveEventPipeline::refresh on a separately opened reader after the writer commit",
            "events_per_segment": 1_024,
            "repeats_per_history_size": 7,
            "timer": "includes writer ingest, file persistence, Outbox append and separate reader refresh; fixture construction and both pipeline startups are excluded",
        },
        "results": records,
        "limitations": [
            "This measures MarketQuote ingest, persistence, Outbox append and another pipeline refresh; it does not include exchange I/O, venue adapters, order/risk processing, or a complete Paper worker tick.",
            "Synthetic EventKind::Settle histories do not represent production event mixes.",
            "Operating-system file cache is uncontrolled; results are host-specific and are not capacity guarantees.",
            "The flat-file backend preserves the legacy single-file format; segmented storage is explicitly configured and is not the default.",
        ],
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"saved EventLog pipeline comparison evidence to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
