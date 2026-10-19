#!/usr/bin/env python3
"""Compare legacy JSONL segment encoding with the buffered writer implementation."""

from __future__ import annotations

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
    "segmented_event_log_encoding_bench",
    "--release",
    "--offline",
    "-j1",
]


def main() -> int:
    output = ROOT / "maturity" / "evidence" / "rust-segmented-event-log-encoding-2026-10-18.json"
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

    results = []
    try:
        for line in process.stdout.splitlines():
            value = json.loads(line)
            if isinstance(value, dict) and value.get("benchmark") == "segmented_event_log_encoding":
                results.append(value)
    except json.JSONDecodeError as error:
        print(f"benchmark emitted invalid JSON: {error}", file=sys.stderr)
        return 1

    if [row.get("events_per_segment") for row in results] != [1, 64, 1_024]:
        print("benchmark output does not contain the required segment sizes", file=sys.stderr)
        return 1
    if not all(row.get("byte_equal") is True for row in results):
        print("legacy and buffered encodings differ", file=sys.stderr)
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
        "benchmark": "segmented_event_log_encoding",
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "command": COMMAND,
        "hardware": hardware_fingerprint(),
        "rustc": rustc.stdout.strip(),
        "profile": "release",
        "methods": {
            "legacy": "serde_json::to_string per event, format JSON plus LF, collect Strings",
            "buffered": "serde_json::to_writer into one byte buffer, append LF, convert once to UTF-8 String",
        },
        "results": results,
        "limitations": [
            "This microbenchmark isolates in-memory segment encoding; it excludes file I/O, manifest work, and EventLog prefix hashing.",
            "Synthetic EventKind::Settle events do not represent the production event mix or the nine product profiles.",
            "The legacy method is reconstructed from the prior compact JSONL format; matching bytes are verified before timing.",
        ],
    }
    output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"saved segmented EventLog encoding evidence to {output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
