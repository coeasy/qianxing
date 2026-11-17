#!/usr/bin/env python3
"""Measure replay-verified L2 order-book backtests at several snapshot/depth sizes.

This is a local synthetic matching-engine profile. It records generation time, input and
artifact sizes, process-tree resources, deterministic result hashes, and a separate artifact
verification result. It does not claim a complete live or production capacity profile.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import statistics
import sys
import tempfile
import time
from pathlib import Path

from hardware import hardware_fingerprint
from process_metrics import run_measured
from source_fingerprint import MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA, measurement_source_fingerprint

ROOT = Path(__file__).resolve().parent.parent
SCALE = 1_000_000_000
START_TS = 1_000
BASE_PRICE_RAW = 50_000 * SCALE
PRICE_TICK_RAW = 1_000_000
QTY_RAW = 100 * SCALE
DEFAULT_DEPTH_LEVELS = 10


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def percentile(values: list[float], percent: float) -> float:
    ordered = sorted(values)
    return round(ordered[min(len(ordered) - 1, round(percent * (len(ordered) - 1)))], 3)


def find_binary(value: str | None) -> Path:
    candidates = [Path(value)] if value else [
        ROOT / "target" / "release" / "qx-cli.exe",
        ROOT / "target" / "release" / "qx-cli",
    ]
    for path in candidates:
        if path.is_file():
            return path.resolve()
    raise SystemExit("找不到 qx-cli release binary；请先 cargo build --release -p qx-cli")


def generate_depth_frame(snapshots: int, levels: int, path: Path) -> None:
    instrument = {"symbol": "BTCUSDT", "venue": "BINANCE"}
    rows = []
    for index in range(snapshots):
        phase = index % 40
        offset = (phase if phase < 20 else 40 - phase) - 10
        mid = BASE_PRICE_RAW + offset * 20 * PRICE_TICK_RAW
        rows.append({
            "instrument": instrument,
            "ts": START_TS + index * 1_000,
            "sequence": index + 1,
            "bids": [
                {"price": mid - (level + 1) * PRICE_TICK_RAW, "qty": QTY_RAW}
                for level in range(levels)
            ],
            "asks": [
                {"price": mid + (level + 1) * PRICE_TICK_RAW, "qty": QTY_RAW}
                for level in range(levels)
            ],
        })
    frame = {
        "schema_version": 1,
        "source": f"capacity-synthetic-l2-{snapshots}x{levels}-v1",
        "instrument": instrument,
        "snapshots": rows,
    }
    path.write_text(json.dumps(frame, separators=(",", ":")), encoding="utf-8")


def build_spec(depth_path: Path, output_dir: Path, run_id: str, snapshots: int) -> dict:
    return {
        "schema_version": 1,
        "run_id": run_id,
        "depth_path": str(depth_path),
        "tier": "l2",
        "settlement_currency": "USDT",
        "initial_cash_raw": 100_000 * SCALE,
        "fee_bps": 1,
        "latency_snapshots": 0,
        "queue_position_bps": 0,
        "market_impact_bps": 0,
        "output_dir": str(output_dir),
        "market_spec": {
            "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
            "product": "perpetual",
            "base_currency": "BTC",
            "quote_currency": "USDT",
            "settlement_currency": "USDT",
            "contract_size": SCALE,
            "linear": True,
            "inverse": False,
            "price_tick": PRICE_TICK_RAW,
            "qty_step": SCALE,
            "min_qty": SCALE,
            "max_leverage": 50,
            "maintenance_margin_bps": 500,
            "valid_from": START_TS,
            "valid_to": START_TS + snapshots * 1_000,
        },
        "order_policy": {
            "reduce_only": False,
            "position_side": "net",
            "margin_mode": "cross",
            "position_mode": "one_way",
            "leverage": 2,
            "post_only": False,
        },
        "strategy": {
            "kind": "sma_cross",
            "strategy_id": "capacity-l2-sma",
            "quantity_raw": SCALE,
            "fast_window": 5,
            "slow_window": 20,
            "period": 14,
            "threshold_bps": 0,
        },
    }


def invoke(binary: Path, args: list[str], cwd: Path) -> tuple[dict, dict]:
    measured = run_measured([str(binary), *args], cwd=cwd)
    if measured.return_code != 0:
        raise RuntimeError(f"qx-cli {' '.join(args[:2])} failed (exit={measured.return_code}): {measured.stderr[-2500:]}")
    try:
        result = json.loads(measured.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"qx-cli returned invalid JSON: {error}: {measured.stdout[-1500:]}") from error
    resources = {
        "wall_ms": round(measured.elapsed_ms, 3),
        "peak_rss_bytes": measured.peak_rss_bytes,
        "process_tree_peak_rss_bytes": measured.peak_tree_rss_bytes,
        "peak_process_count": measured.peak_process_count,
        "process_tree_cpu_seconds": measured.tree_cpu_seconds,
        "process_tree_io_read_bytes": measured.tree_io_read_bytes,
        "process_tree_io_write_bytes": measured.tree_io_write_bytes,
    }
    return result, resources


def directory_bytes(path: Path) -> int:
    if not path.exists():
        return 0
    return sum(item.stat().st_size for item in path.rglob("*") if item.is_file())


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary")
    parser.add_argument("--sizes", default="1000,8000,64000")
    parser.add_argument("--depth-levels", type=int, default=DEFAULT_DEPTH_LEVELS)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--out", type=Path, default=ROOT / "maturity/evidence/depth-backtest-capacity.json")
    args = parser.parse_args()
    sizes = [int(item) for item in args.sizes.split(",") if item.strip()]
    if not sizes or any(item < 32 for item in sizes) or len(set(sizes)) != len(sizes):
        parser.error("--sizes 必须是互不重复且不少于 32 的正整数")
    if not 1 <= args.depth_levels <= 100:
        parser.error("--depth-levels 必须在 1..=100 内")
    if args.repeats < 2:
        parser.error("--repeats 至少为 2")

    binary = find_binary(args.binary)
    evidence = {
        "schema_version": 1,
        "evidence_kind": "replay_verified_l2_depth_backtest_capacity",
        "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "hardware": hardware_fingerprint(),
        "binary": str(binary),
        "binary_sha256": sha256(binary),
        "measurement_source_fingerprint_schema": MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA,
        "measurement_source_fingerprint": measurement_source_fingerprint(ROOT),
        "benchmark_script_sha256": sha256(Path(__file__).resolve()),
        "scenario": "qx-cli app depth-backtest with synthetic BTCUSDT perpetual L2 snapshots and ten-level books",
        "profile_coverage_claim": False,
        "profile_target_statuses_updated": False,
        "resource_measurement": "OS process counters plus optional 10ms psutil process-tree RSS/CPU/I/O sampling",
        "limitations": [
            "synthetic deterministic depth snapshots; no venue feed capture or paper/live workflow",
            "L2 depth level count is a configurable synthetic workload dimension",
            "process-tree samples may miss brief peaks; this is not a hard memory bound",
            "does not measure allocator calls, long-run stability, internal parallelism, or all nine maturity profiles",
        ],
        "workloads": [],
    }
    with tempfile.TemporaryDirectory(prefix="qx-depth-cap-", dir=ROOT / "target") as temporary:
        workdir = Path(temporary)
        for snapshot_count in sizes:
            generation_started = time.perf_counter()
            frame = workdir / f"l2-{snapshot_count}.json"
            generate_depth_frame(snapshot_count, args.depth_levels, frame)
            generation_ms = round((time.perf_counter() - generation_started) * 1_000, 3)
            samples = []
            result_hashes = []
            fill_counts = []
            input_fingerprints = []
            artifact_sizes = []
            for repeat in range(args.repeats):
                output_dir = workdir / f"run-{snapshot_count}-{repeat}"
                spec_path = workdir / f"run-{snapshot_count}-{repeat}.spec.json"
                outcome_path = workdir / f"run-{snapshot_count}-{repeat}.outcome.json"
                # Keep run identity stable across isolated output roots so the result hash
                # comparison catches nondeterminism instead of fingerprinting the repetition.
                run_id = f"l2-capacity-{snapshot_count}"
                spec_path.write_text(
                    json.dumps(build_spec(frame, output_dir, run_id, snapshot_count), separators=(",", ":")),
                    encoding="utf-8",
                )
                result, resources = invoke(binary, ["app", "depth-backtest", str(spec_path)], ROOT)
                outcome_path.write_text(json.dumps(result, separators=(",", ":")), encoding="utf-8")
                verification, _ = invoke(binary, ["app", "verify-depth", str(outcome_path)], ROOT)
                if verification.get("verified") is not True:
                    raise RuntimeError(f"depth result failed replay verification: {verification}")
                result_hash = result.get("result_hash")
                if not result_hash:
                    raise RuntimeError("depth outcome omitted result_hash")
                result_hashes.append(result_hash)
                fill_counts.append(result.get("fills", 0))
                input_fingerprints.append(result.get("data_fingerprint"))
                artifact_sizes.append(directory_bytes(output_dir))
                samples.append({**resources, "verification": "passed", "result_hash": result_hash})
            if len(set(result_hashes)) != 1 or len(set(input_fingerprints)) != 1:
                raise RuntimeError(f"non-deterministic L2 replay for N={snapshot_count}")
            evidence["workloads"].append({
                "snapshots": snapshot_count,
                "depth_levels_per_side": args.depth_levels,
                "repeats": args.repeats,
                "input_generation_ms": generation_ms,
                "input_bytes": frame.stat().st_size,
                "input_sha256": sha256(frame),
                "data_fingerprint": input_fingerprints[0],
                "result_hash_unique_count": len(set(result_hashes)),
                "fills_per_repeat": fill_counts,
                "wall_p50_ms": round(statistics.median(item["wall_ms"] for item in samples), 3),
                "wall_p95_ms": percentile([item["wall_ms"] for item in samples], 0.95),
                "peak_rss_bytes_max": max((item["peak_rss_bytes"] for item in samples if item["peak_rss_bytes"] is not None), default=None),
                "process_tree_peak_rss_bytes_max": max((item["process_tree_peak_rss_bytes"] for item in samples if item["process_tree_peak_rss_bytes"] is not None), default=None),
                "peak_process_count": max(item["peak_process_count"] for item in samples),
                "process_tree_cpu_seconds_p50": round(statistics.median([item["process_tree_cpu_seconds"] for item in samples if item["process_tree_cpu_seconds"] is not None]), 3) if any(item["process_tree_cpu_seconds"] is not None for item in samples) else None,
                "process_tree_io_read_bytes_max": max((item["process_tree_io_read_bytes"] for item in samples if item["process_tree_io_read_bytes"] is not None), default=None),
                "process_tree_io_write_bytes_max": max((item["process_tree_io_write_bytes"] for item in samples if item["process_tree_io_write_bytes"] is not None), default=None),
                "artifact_directory_bytes_max": max(artifact_sizes),
                "replay_verification": "passed_each_repeat",
                "scope_claim": "synthetic_l2_matching_capacity_only",
            })
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"saved {len(evidence['workloads'])} replay-verified L2 capacity workloads: {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
