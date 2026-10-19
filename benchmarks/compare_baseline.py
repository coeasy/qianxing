#!/usr/bin/env python3
"""Compare same-machine benchmark results; deviations warn but never fail CI."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def load(path: Path) -> dict:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"cannot read benchmark result {path}: {error}")
    if value.get("schema_version") != 1 or not (
        isinstance(value.get("rows"), list)
        or isinstance(value.get("records"), list)
        or isinstance(value.get("workloads"), list)
    ):
        raise SystemExit(f"unsupported benchmark result shape: {path}")
    return value


def series(report: dict) -> dict[tuple[str, str], dict[str, float]]:
    result = {}
    for row in report.get("rows", []):
        key = ("end_to_end", str(row["bars"]))
        result[key] = {name: float(row[name]) for name in ("p50_ms", "p95_ms")}
    for row in report.get("records", []):
        size = row.get("operations", row.get("events", "all"))
        key = (str(row["benchmark"]), str(size))
        metrics = {
            name: float(row[name])
            for name in (
                "median_ms",
                "p95_ms",
                "operations_per_second",
                "incremental_refresh_p50_ms",
                "append_batch_p50_ms",
                "full_replay_median_ms",
            )
            if name in row
        }
        result[key] = metrics
    for row in report.get("criterion_records", []):
        key = (f"criterion:{row['benchmark']}", "criterion")
        result[key] = {"median_ms": float(row["median_ns"]) / 1_000_000}
    if report.get("evidence_kind") == "ashare_rules_on_cli_backtest_throughput":
        for row in report.get("workloads", []):
            key = ("ashare_rules_on_cli_backtest", str(row["bars"]))
            result[key] = {name: float(row[name]) for name in ("p50_ms", "p95_ms")}
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("current", type=Path)
    parser.add_argument("--threshold", type=float, default=0.20)
    args = parser.parse_args()
    if not 0 < args.threshold < 1:
        parser.error("--threshold must be between 0 and 1")

    baseline, current = load(args.baseline), load(args.current)
    if baseline.get("hardware") != current.get("hardware"):
        print("[SKIP] Hardware fingerprint differs; cross-machine trend is not comparable.")
        return 0
    old_rows, new_rows = series(baseline), series(current)
    common = sorted(old_rows.keys() & new_rows.keys())
    if not common:
        print("[SKIP] No matching workload sizes; no trend comparison was made.")
        return 0

    warned = False
    for key in common:
        shared_metrics = old_rows[key].keys() & new_rows[key].keys()
        for metric in sorted(shared_metrics):
            before = old_rows[key][metric]
            after = new_rows[key][metric]
            if before <= 0:
                continue
            ratio = after / before
            if ratio > 1 + args.threshold or ratio < 1 - args.threshold:
                warned = True
                note = (
                    f"[WARNING] {key[0]}[{key[1]}] {metric}: {before:.3f} -> {after:.3f} "
                    f"({(ratio - 1) * 100:+.1f}%, threshold +/-{args.threshold * 100:.0f}%)"
                )
                print(note)
                print(f"::warning title=Qianxing benchmark trend::{note}")
    if not warned:
        print(f"[OK] Matching workloads stayed within +/-{args.threshold * 100:.0f}%.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
