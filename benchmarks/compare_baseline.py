#!/usr/bin/env python3
"""Compare same-machine benchmark results; block regressions for registered groups."""

from __future__ import annotations

import argparse
import json
import math
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


def series(report: dict) -> dict[tuple[str, str], dict[str, float | list[float]]]:
    result = {}
    for row in report.get("rows", []):
        key = ("end_to_end", str(row["bars"]))
        metrics: dict[str, float | list[float]] = {
            name: float(row[name])
            for name in ("p50_ms", "p95_ms", "min_ms", "max_ms")
            if name in row
        }
        if isinstance(row.get("raw_ms"), list):
            metrics["raw_ms"] = [float(value) for value in row["raw_ms"]]
        if isinstance(row.get("repeats"), (int, float)):
            metrics["sample_count"] = float(row["repeats"])
        result[key] = metrics
    for row in report.get("records", []):
        size = row.get("operations", row.get("events", "all"))
        key = (str(row["benchmark"]), str(size))
        metrics = {
            name: float(row[name])
            for name in (
                "median_ms",
                "p95_ms",
                "min_ms",
                "max_ms",
                "operations_per_second",
                "incremental_refresh_p50_ms",
                "append_batch_p50_ms",
                "full_replay_median_ms",
            )
            if name in row
        }
        if isinstance(row.get("raw_ms"), list):
            metrics["raw_ms"] = [float(value) for value in row["raw_ms"]]
        if isinstance(row.get("samples"), (int, float)):
            metrics["sample_count"] = float(row["samples"])
        result[key] = metrics
    for row in report.get("criterion_records", []):
        key = (f"criterion:{row['benchmark']}", "criterion")
        metrics: dict[str, float | list[float]] = {
            "point_ms": float(row.get("mean_ns", row.get("median_ns"))) / 1_000_000
        }
        interval = row.get("mean_ci95_ns")
        if isinstance(interval, list) and len(interval) == 2:
            metrics["mean_ci95_ms"] = [float(value) / 1_000_000 for value in interval]
        result[key] = metrics
    if report.get("evidence_kind") == "ashare_rules_on_cli_backtest_throughput":
        for row in report.get("workloads", []):
            key = ("ashare_rules_on_cli_backtest", str(row["bars"]))
            metrics = {
                name: float(row[name])
                for name in ("p50_ms", "p95_ms", "min_ms", "max_ms")
                if name in row
            }
            if isinstance(row.get("raw_ms"), list):
                metrics["raw_ms"] = [float(value) for value in row["raw_ms"]]
            if isinstance(row.get("repeats"), (int, float)):
                metrics["sample_count"] = float(row["repeats"])
            result[key] = metrics
    return result


def repeat_bound(metrics: dict[str, float | list[float]], *, upper: bool) -> float | None:
    """Return an observed repeat bound; p95 is a max for the CI's 5–7 samples."""
    raw = metrics.get("raw_ms")
    if isinstance(raw, list) and len(raw) >= 3:
        values = [value for value in raw if math.isfinite(value) and value > 0]
        if len(values) >= 3:
            return max(values) if upper else min(values)

    sample_count = metrics.get("sample_count")
    if not isinstance(sample_count, (int, float)) or sample_count < 3:
        return None
    if upper:
        value = metrics.get("max_ms", metrics.get("p95_ms"))
    else:
        value = metrics.get("min_ms")
    if isinstance(value, (int, float)) and math.isfinite(value) and value > 0:
        return float(value)
    return None


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


# Cross-run confidence intervals for sub-microsecond operations are dominated by
# clock and vCPU placement noise. Keep these estimates visible without blocking.
MIN_CRITERION_BLOCKING_MS = 0.001


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
    old_schema = baseline.get("measurement_source_fingerprint_schema", 1)
    new_schema = current.get("measurement_source_fingerprint_schema", 1)
    if old_schema != new_schema:
        print("[SKIP] Measurement source fingerprint format changed; recording a compatible first sample.")
        return 0
    old_source = baseline.get("measurement_source_fingerprint")
    new_source = current.get("measurement_source_fingerprint")
    if old_source is None or new_source is None:
        print("[SKIP] A compatible source fingerprint is unavailable; recording the first source baseline.")
        return 0
    if old_source == new_source:
        print("[SKIP] Measured source is unchanged; no implementation performance comparison is needed.")
        return 0
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
            if (
                group == "rust_micro"
                and metric == "operations_per_second"
                and "median_ms" in shared_metrics
            ):
                # This is derived from the same median_ms estimate, not independent evidence.
                continue
            if metric in {"p95_ms", "min_ms", "max_ms", "raw_ms", "sample_count"} or (
                metric.endswith("_ci95_ms")
            ):
                # With 5–7 independent process runs, nearest-rank p95 is effectively
                # the maximum; repeat bounds are evidence for the median comparison, not
                # standalone performance metrics.
                continue
            before = old_rows[key][metric]
            after = new_rows[key][metric]
            repeat_comparable = (
                group in {"end_to_end", "ashare_rules_on"} and metric in {"p50_ms", "median_ms"}
            ) or (
                group == "rust_micro"
                and metric in {
                    "median_ms",
                    "incremental_refresh_p50_ms",
                    "append_batch_p50_ms",
                    "full_replay_median_ms",
                }
                and ("raw_ms" in old_rows[key] or "raw_ms" in new_rows[key])
            )
            if repeat_comparable:
                baseline_upper = repeat_bound(old_rows[key], upper=True)
                current_lower = repeat_bound(new_rows[key], upper=False)
                if baseline_upper is not None and current_lower is not None and baseline_upper > 0:
                    envelope_ratio = current_lower / baseline_upper
                    if envelope_ratio > 1 + threshold:
                        failed = True
                        change_pct = (envelope_ratio - 1) * 100
                        note = (
                            f"[FAIL] {key[0]}[{key[1]}] {metric}: baseline upper sample "
                            f"{baseline_upper:.3f} ms -> current lower sample {current_lower:.3f} ms "
                            f"({change_pct:+.1f}% beyond observed repeat envelope, registered limit "
                            f"+{threshold * 100:.0f}%)"
                        )
                        print(note)
                        print(f"::error title=Qianxing benchmark regression::{note}")
                    elif before > 0 and after / before > 1 + threshold:
                        print(
                            f"[INCONCLUSIVE] {key[0]}[{key[1]}] {metric}: point estimate increased by "
                            f"{(after / before - 1) * 100:+.1f}%, but the repeated timing envelopes "
                            "do not establish a regression beyond the limit."
                        )
                elif before > 0 and after / before > 1 + threshold:
                    print(
                        f"[INCONCLUSIVE] {key[0]}[{key[1]}] {metric}: point estimate increased by "
                        "more than the registered limit, but repeat bounds are unavailable."
                    )
                continue
            if group == "rust_micro" and metric in {
                "median_ms",
                "incremental_refresh_p50_ms",
                "append_batch_p50_ms",
                "full_replay_median_ms",
            }:
                print(
                    f"[INCONCLUSIVE] {key[0]}[{key[1]}] {metric}: single-run point estimate "
                    "has no independent repeat envelope and is diagnostic only."
                )
                continue
            if group == "criterion" and metric == "point_ms":
                old_interval = old_rows[key].get("mean_ci95_ms")
                new_interval = new_rows[key].get("mean_ci95_ms")
                if (
                    not isinstance(old_interval, list)
                    or not isinstance(new_interval, list)
                    or len(old_interval) != 2
                    or len(new_interval) != 2
                    or any(not math.isfinite(value) or value < 0 for value in old_interval + new_interval)
                    or old_interval[0] > old_interval[1]
                    or new_interval[0] > new_interval[1]
                    or old_interval[1] <= 0
                ):
                    note = (
                        f"[INCONCLUSIVE] {key[0]}: Criterion confidence interval is missing; "
                        "point estimate is diagnostic only."
                    )
                    print(note)
                    continue
                if (
                    old_interval[1] < MIN_CRITERION_BLOCKING_MS
                    and new_interval[1] < MIN_CRITERION_BLOCKING_MS
                ):
                    print(
                        f"[INCONCLUSIVE] {key[0]}: both 95% intervals are below "
                        f"{MIN_CRITERION_BLOCKING_MS * 1000:.0f} us; batch measurements are "
                        "needed for a blocking signal."
                    )
                    continue
                threshold_bound = old_interval[1] * (1 + threshold)
                if new_interval[0] > threshold_bound:
                    failed = True
                    change_pct = (new_interval[0] / old_interval[1] - 1) * 100
                    note = (
                        f"[FAIL] {key[0]}: baseline 95% CI upper "
                        f"{old_interval[1]:.6f} ms, current 95% CI lower "
                        f"{new_interval[0]:.6f} ms ({change_pct:+.1f}% beyond baseline "
                        f"interval, registered limit +{threshold * 100:.0f}%)"
                    )
                    print(note)
                    print(f"::error title=Qianxing benchmark regression::{note}")
                elif before > 0 and after / before > 1 + threshold:
                    print(
                        f"[INCONCLUSIVE] {key[0]}: point estimate increased by "
                        f"{(after / before - 1) * 100:+.1f}%, but the 95% confidence "
                        "intervals do not establish a regression beyond the limit."
                    )
                continue
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
