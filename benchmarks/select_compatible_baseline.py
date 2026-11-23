#!/usr/bin/env python3
"""Check whether a downloaded benchmark run can be compared on this runner."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from compare_baseline import same_benchmark_machine, series


REPORTS = (
    "end-to-end.json",
    "rust-micro.json",
    "ashare-rules-on.json",
    "crypto-paper-e2e.json",
)


def read_report(path: Path) -> dict | None:
    try:
        report = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    return report if isinstance(report, dict) else None


def is_compatible(current_dir: Path, baseline_dir: Path) -> tuple[bool, str]:
    """Require every shared report to match and at least one usable report."""
    comparable = 0
    unchanged = 0
    for filename in REPORTS:
        current_path = current_dir / filename
        baseline_path = baseline_dir / filename
        if not current_path.is_file() or not baseline_path.is_file():
            continue
        current = read_report(current_path)
        baseline = read_report(baseline_path)
        if current is None or baseline is None:
            return False, f"{filename}: missing or invalid report"
        current_hardware = current.get("hardware")
        baseline_hardware = baseline.get("hardware")
        if not isinstance(current_hardware, dict) or not current_hardware:
            return False, f"{filename}: missing or invalid current hardware fingerprint"
        if not isinstance(baseline_hardware, dict) or not baseline_hardware:
            return False, f"{filename}: missing or invalid baseline hardware fingerprint"
        if not same_benchmark_machine(
            {"hardware": baseline_hardware}, {"hardware": current_hardware}
        ):
            return False, f"{filename}: runner hardware fingerprint differs"
        if baseline.get("measurement_source_fingerprint_schema", 1) != current.get(
            "measurement_source_fingerprint_schema", 1
        ):
            continue
        baseline_source = baseline.get("measurement_source_fingerprint")
        current_source = current.get("measurement_source_fingerprint")
        if not isinstance(baseline_source, str) or not isinstance(current_source, str):
            continue
        if baseline_source == current_source:
            unchanged += 1
            continue
        try:
            has_common_workload = bool(series(baseline).keys() & series(current).keys())
        except (KeyError, TypeError, ValueError):
            return False, f"{filename}: malformed workload rows"
        if has_common_workload:
            comparable += 1

    if comparable:
        return True, f"{comparable} report(s) have matching source schema, workload, and runner"
    if unchanged:
        return True, f"{unchanged} report(s) have unchanged measured sources on this runner"
    return False, "no shared report has a comparable source schema and workload on this runner"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--current-dir", type=Path, required=True)
    parser.add_argument("--baseline-dir", type=Path, required=True)
    args = parser.parse_args()

    compatible, reason = is_compatible(args.current_dir, args.baseline_dir)
    print(("[MATCH] " if compatible else "[NO MATCH] ") + reason)
    return 0 if compatible else 1


if __name__ == "__main__":
    raise SystemExit(main())
