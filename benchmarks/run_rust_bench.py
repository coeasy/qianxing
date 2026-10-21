#!/usr/bin/env python3
"""Run Criterion and deterministic Rust microbenchmarks into one evidence file."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

from hardware import hardware_fingerprint

ROOT = Path(__file__).resolve().parent.parent
REQUIRED = {
    "append_batch",
    "event_log_digest",
    "full_replay",
    "l2_matching",
    "full_vs_incremental_replay",
    "segmented_event_log_append",
    "steady_state_pipeline_ingest",
}
CRITERION_REQUIRED = {
    *(f"qx_core_append_batch/{size}" for size in (1_000, 8_000, 64_000)),
    *(f"qx_zhenlu_crypto_paper_lifecycle_v2/{size}" for size in (1, 16, 256)),
    *(f"qx_core_event_log_digest/{size}" for size in (1_000, 8_000, 64_000)),
    *(f"qx_core_event_log_validate/{mode}/{size}" for mode in ("cached", "duplicate_tree_baseline") for size in (1_000, 8_000, 64_000)),
    *(f"qx_core_event_log_manifest_digest/{mode}/{size}" for mode in ("legacy_full_rehash", "rolling_v2") for size in (1_000, 8_000, 64_000)),
    *(f"qx_core_event_log_prefix_digest/{mode}/{size}" for mode in ("cached_after_append", "prior_two_scans") for size in (1_000, 8_000, 64_000)),
    *(f"qx_core_ledger_entry_lookup/{mode}/{size}" for mode in ("indexed", "linear") for size in (1_000, 8_000, 64_000)),
    *(f"qx_core_ledger_entry_commit/clone_full_ledger_each_entry/{size}" for size in (64, 256, 1_024)),
    *(f"qx_core_ledger_entry_commit/commit_affected_state/{size}" for size in (64, 256, 1_024, 8_000, 64_000)),
    *(f"qx_core_ledger_cash_lookup/{mode}/{size}" for mode in ("indexed", "composite_key_prior", "entry_scan") for size in (1_000, 8_000, 64_000)),
    *(f"qx_core_ledger_cash_balances_snapshot/{mode}/{size}" for mode in ("owned_allocating_snapshot", "borrowed_snapshot") for size in (1, 8, 64)),
    *(f"{name}/{size}" for name in ("full_replay", "l2_matching") for size in (1_000, 8_000, 64_000)),
    *(f"l2_queue_position_depth_lookup/{depth}" for depth in (16, 128, 512)),
    *(f"l2_queue_price_lookup_algorithm/{side}/{book_side}/{depth}"
      for depth in (16, 128, 512, 2_048)
      for side in ("linear", "binary")
      for book_side in ("Buy", "Sell")),
    *(f"event_log_validate/{mode}/{size}" for mode in ("cached", "duplicate_tree_baseline") for size in (1_000, 8_000, 64_000)),
    *(f"event_log_manifest_digest/{mode}/{size}" for mode in ("legacy_full_rehash", "rolling_v2") for size in (1_000, 8_000, 64_000)),
    *(f"event_log_prefix_digest/{mode}/{size}" for mode in ("cached_after_append", "prior_two_scans") for size in (1_000, 8_000, 64_000)),
    *(f"ledger_replay/{mode}/{size}" for mode in ("borrowed_events", "owned_events") for size in (1_000, 8_000, 64_000)),
    "full_vs_incremental_replay/incremental_refresh",
    "full_vs_incremental_replay/full_replay",
    *(f"ashare_previous_close/{mode}/{size}" for mode in ("pointwise", "precomputed") for size in (1_000, 8_000, 64_000)),
    *(f"ashare_ex_rights_daily_sweep/{mode}/{size}" for mode in ("legacy_daily_scan", "production_adaptive") for size in (1_000, 8_000, 64_000)),
    *(f"backtest_risk_metrics/{mode}/{size}" for mode in ("optimized_single_vector", "legacy_allocating") for size in (1_000, 8_000, 64_000)),
    *(f"portfolio_bar_timeline_64k_bars/{mode}/{markets}"
      for mode in ("timeline_constructor", "flatten_then_sort", "preallocated_flatten_then_sort", "preallocated_flatten_sort_with_indexes")
      for markets in (2, 8, 32, 64, 128)),
    *(f"portfolio_bar_timeline_aligned_64k_bars/{mode}/{markets}"
      for mode in ("timeline_constructor", "preallocated_flatten_sort_with_indexes")
      for markets in (2, 8, 32)),
    *(f"protective_exit_position_sides_per_64k_bars/{mode}/{sides}"
      for mode in ("allocate_vec_per_bar", "borrow_keys_per_bar")
      for sides in (1, 2)),
    *(f"portfolio_visible_marks_aligned/{mode}/{markets}"
      for mode in ("rebuild_each_admission_batch", "reuse_same_visible_timestamp")
      for markets in (2, 8, 32, 64)),
    *(f"portfolio_funding_reservation_decision_reads/{mode}/{markets}x{pending}"
      for mode in ("scan_markets_and_orders_per_decision", "read_cached_global_and_market_totals")
      for markets in (2, 8, 32, 64)
      for pending in (16, 256)),
    *(f"portfolio_margin_admission_snapshot/{mode}/{markets}"
      for mode in ("recalculate_positions_for_each_admission", "reuse_margin_snapshot_for_admissions")
      for markets in (2, 8, 32, 64)),
    "scheduler_active_run_selection/clone_and_scan_4097_runs",
    "scheduler_active_run_selection/indexed_active_ids",
}
MARGINAL_KNEE_THRESHOLD_PCT = 20.0


def criterion_estimate_path(benchmark: str, criterion_root: Path | None = None) -> Path:
    """Resolve nested ids and Windows-flattened sub-identifiers within a group."""
    root = criterion_root or ROOT / "target" / "criterion"
    parts = benchmark.split("/")
    nested = root.joinpath(*parts, "new", "estimates.json")
    if nested.is_file():
        return nested
    if len(parts) > 1:
        flattened_id = root / parts[0] / ("_".join(parts[1:]).lower()) / "new" / "estimates.json"
        if flattened_id.is_file():
            return flattened_id
    return nested


def criterion_estimates() -> list[dict]:
    records = []
    for benchmark in sorted(CRITERION_REQUIRED):
        # Workspace Criterion groups share target/criterion; crate-owned groups use
        # distinct benchmark ids so one package cannot overwrite another package's sample.
        path = criterion_estimate_path(benchmark)
        payload = json.loads(path.read_text(encoding="utf-8"))
        records.append(
            {
                "benchmark": benchmark,
                "mean_ns": payload["mean"]["point_estimate"],
                "mean_ci95_ns": [
                    payload["mean"]["confidence_interval"]["lower_bound"],
                    payload["mean"]["confidence_interval"]["upper_bound"],
                ],
                "median_ns": payload["median"]["point_estimate"],
                "std_dev_ns": payload["std_dev"]["point_estimate"],
            }
        )
    return records


def steady_state_tick_curve(records: list[dict]) -> dict:
    rows = sorted(
        (
            row
            for row in records
            if row.get("benchmark") == "steady_state_pipeline_ingest"
        ),
        key=lambda row: int(row["operations"]),
    )
    if len(rows) < 2 or len({row["operations"] for row in rows}) != len(rows):
        raise ValueError("steady-state tick evidence needs at least two unique history sizes")
    points = [
        {
            "history_events": int(row["operations"]),
            "ticks": int(row["ticks"]),
            "p50_ms_per_tick": float(row["median_ms"]),
            "p95_ms_per_tick": float(row["p95_ms"]),
        }
        for row in rows
    ]
    segments = []
    positive_knee = None
    previous_slope = None
    for previous, current in zip(rows, rows[1:]):
        events_delta = int(current["operations"]) - int(previous["operations"])
        if events_delta <= 0:
            raise ValueError("steady-state tick history sizes must increase")
        slope = (float(current["median_ms"]) - float(previous["median_ms"])) / events_delta
        change_pct = (
            None
            if previous_slope is None or previous_slope == 0
            else (slope - previous_slope) / abs(previous_slope) * 100.0
        )
        segments.append(
            {
                "from_history_events": int(previous["operations"]),
                "to_history_events": int(current["operations"]),
                "delta_p50_ms_per_tick": round(
                    float(current["median_ms"]) - float(previous["median_ms"]), 6
                ),
                "marginal_us_per_history_event": round(slope * 1_000.0, 6),
                "marginal_slope_change_pct": None if change_pct is None else round(change_pct, 3),
            }
        )
        if (
            positive_knee is None
            and previous_slope is not None
            and slope > previous_slope
            and slope > 0
            and change_pct is not None
            and change_pct > MARGINAL_KNEE_THRESHOLD_PCT
        ):
            positive_knee = int(current["operations"])
        previous_slope = slope
    return {
        "method": "adjacent p50 of single LiveEventPipeline::ingest calls; SQLite append + projection",
        "candidate_positive_knee_threshold_pct": MARGINAL_KNEE_THRESHOLD_PCT,
        "candidate_positive_knee_at_history_events": positive_knee,
        "not_a_statistical_claim": True,
        "points": points,
        "segments": segments,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--out", type=Path, default=ROOT / "benchmarks" / "results" / "rust-micro.json"
    )
    parser.add_argument("--cargo", default="cargo")
    parser.add_argument(
        "--skip-criterion",
        action="store_true",
        help="collect existing Criterion estimates without rerunning timed workloads",
    )
    args = parser.parse_args()
    criterion_commands = [
        [
            args.cargo,
            "bench",
            "-p",
            package,
            "--bench",
            "criterion_performance",
            "--",
            "--noplot",
        ]
        for package in ("qx-core", "qx-scheduler", "qx-xingban", "qx-zhenlu", "qx-cli")
    ]
    for criterion_command in (() if args.skip_criterion else criterion_commands):
        criterion_process = subprocess.run(
            criterion_command,
            cwd=ROOT,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
        if criterion_process.returncode:
            sys.stderr.write(criterion_process.stdout)
            sys.stderr.write(criterion_process.stderr)
            return criterion_process.returncode
    try:
        criterion_records = criterion_estimates()
    except (OSError, KeyError, json.JSONDecodeError) as error:
        print(f"Criterion estimate output is incomplete: {error}", file=sys.stderr)
        return 1

    command = [
        args.cargo,
        "bench",
        "-p",
        "qx-cli",
        "--bench",
        "performance",
        "--offline",
    ]
    process = subprocess.run(
        command,
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
    for line in process.stdout.splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict) and isinstance(value.get("benchmark"), str):
            records.append(value)
    found = {record["benchmark"] for record in records}
    if not REQUIRED <= found:
        print(f"microbenchmark output missing: {sorted(REQUIRED - found)}", file=sys.stderr)
        return 1
    replay = next(item for item in records if item["benchmark"] == "full_vs_incremental_replay")
    if replay.get("digests_equal") is not True:
        print("full and incremental replay diverged", file=sys.stderr)
        return 1

    report = {
        "schema_version": 1,
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "hardware": hardware_fingerprint(),
        "command": command,
        "criterion_commands": criterion_commands,
        "criterion_records": criterion_records,
        "records": records,
        "steady_state_tick_curve": steady_state_tick_curve(records),
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"saved {len(records)} Rust benchmark records to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
