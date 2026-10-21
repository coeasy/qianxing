#!/usr/bin/env python3
"""Compare same-machine benchmark results; block regressions for registered groups."""

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


def same_benchmark_machine(baseline: dict, current: dict) -> bool:
    """Allow small container memory-reporting noise, but require the same runner shape."""
    old = baseline.get("hardware")
    new = current.get("hardware")
    if not isinstance(old, dict) or not isinstance(new, dict):
        return old == new

    volatile = {"total_ram_bytes"}
    if {key: value for key, value in old.items() if key not in volatile} != {
        key: value for key, value in new.items() if key not in volatile
    }:
        return False

    old_ram = old.get("total_ram_bytes")
    new_ram = new.get("total_ram_bytes")
    if old_ram is None or new_ram is None:
        return old_ram == new_ram
    if not isinstance(old_ram, (int, float)) or not isinstance(new_ram, (int, float)):
        return old_ram == new_ram
    if old_ram <= 0 or new_ram <= 0:
        return old_ram == new_ram
    return abs(old_ram - new_ram) / max(old_ram, new_ram) <= 0.01


def series(report: dict) -> dict[tuple[str, str], dict[str, float]]:
    result = {}
    for row in report.get("rows", []):
        key = ("end_to_end", str(row["bars"]))
        result[key] = {name: float(row[name]) for name in ("p50_ms", "p95_ms") if name in row}
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
            result[key] = {name: float(row[name]) for name in ("p50_ms", "p95_ms") if name in row}
    return result


def load_thresholds(path: Path) -> dict[str, float]:
    """Read the simple benchmark_regression_thresholds mapping from targets.yaml."""
    lines = path.read_text(encoding="utf-8").splitlines()
    try:
        start = lines.index("benchmark_regression_thresholds:") + 1
    except ValueError as error:
        raise SystemExit(f"{path} has no benchmark_regression_thresholds mapping") from error
    thresholds: dict[str, float] = {}
    for line in lines[start:]:
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if not line.startswith("  "):
            break
        key, separator, value = line.strip().partition(":")
        if not separator or not key or key in thresholds:
            raise SystemExit(f"invalid benchmark regression threshold entry: {line!r}")
        try:
            threshold = float(value.strip())
        except ValueError as error:
            raise SystemExit(f"invalid benchmark regression threshold entry: {line!r}") from error
        if not 0 < threshold < 1:
            raise SystemExit(f"threshold for {key!r} must be between 0 and 1")
        thresholds[key] = threshold
    if not thresholds:
        raise SystemExit(f"{path} has an empty benchmark_regression_thresholds mapping")
    return thresholds


def benchmark_group(key: tuple[str, str]) -> str:
    name = key[0]
    if name == "end_to_end":
        return "end_to_end"
    if name.startswith("criterion:"):
        return "criterion"
    if name == "ashare_rules_on_cli_backtest":
        return "ashare_rules_on"
    return "rust_micro"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("current", type=Path)
    parser.add_argument("--targets", type=Path, default=Path("maturity/targets.yaml"))
    parser.add_argument("--threshold", type=float, help="override all registered thresholds")
    args = parser.parse_args()
    thresholds = load_thresholds(args.targets)
    if args.threshold is not None:
        if not 0 < args.threshold < 1:
            parser.error("--threshold must be between 0 and 1")
        thresholds = {group: args.threshold for group in thresholds}

    baseline, current = load(args.baseline), load(args.current)
    if not same_benchmark_machine(baseline, current):
        print("[SKIP] Hardware fingerprint differs; cross-machine trend is not comparable.")
        return 0
    old_rows, new_rows = series(baseline), series(current)
    common = sorted(old_rows.keys() & new_rows.keys())
    if not common:
        print("[SKIP] No matching workload sizes; no trend comparison was made.")
        return 0

    failed = False
    for key in common:
        group = benchmark_group(key)
        threshold = thresholds.get(group)
        if threshold is None:
            print(f"[SKIP] {key[0]} is not a registered benchmark group.")
            continue
        shared_metrics = old_rows[key].keys() & new_rows[key].keys()
        for metric in sorted(shared_metrics):
            if metric == "p95_ms":
                # With 5–7 independent process runs, nearest-rank p95 is effectively
                # the maximum and is too noisy to block a release candidate.
                continue
            before = old_rows[key][metric]
            after = new_rows[key][metric]
            if before <= 0:
                continue
            ratio = after / before
            higher_is_better = metric == "operations_per_second"
            regressed = ratio < 1 - threshold if higher_is_better else ratio > 1 + threshold
            if regressed:
                failed = True
                change_pct = (1 - ratio) * 100 if higher_is_better else (ratio - 1) * 100
                note = (
                    f"[FAIL] {key[0]}[{key[1]}] {metric}: {before:.3f} -> {after:.3f} "
                    f"({change_pct:+.1f}% regression, registered limit +{threshold * 100:.0f}%)"
                )
                print(note)
                print(f"::error title=Qianxing benchmark regression::{note}")
    if not failed:
        print("[OK] No registered benchmark group exceeded its regression threshold.")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
