#!/usr/bin/env python3
"""Measure deterministic shared-account multi-market L2 backtests.

This is a local synthetic matching-engine capacity profile. The product workflow
replays the shared ledger before persisting each result; this driver checks that
the replay marker and manifest agree with the reported result hash, then checks
determinism across isolated repetitions. It does not claim a live or production
capacity profile.
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


def parse_sizes(raw: str, label: str, minimum: int, maximum: int) -> list[int]:
    try:
        values = [int(item.strip()) for item in raw.split(",") if item.strip()]
    except ValueError as error:
        raise argparse.ArgumentTypeError(f"{label} 必须是逗号分隔的整数") from error
    if not values or len(set(values)) != len(values) or any(not minimum <= value <= maximum for value in values):
        raise argparse.ArgumentTypeError(f"{label} 必须是互不重复且位于 {minimum}..={maximum} 的整数")
    return values


def instrument_parts(index: int) -> tuple[str, str]:
    base = f"QX{index:02d}"
    return f"{base}USDT.BINANCE", base


def generate_depth_frame(index: int, snapshots: int, levels: int, path: Path) -> None:
    instrument, _ = instrument_parts(index)
    symbol = instrument.split(".", 1)[0]
    rows = []
    market_mid = BASE_PRICE_RAW + index * 10_000 * SCALE
    for frame_index in range(snapshots):
        phase = frame_index % 40
        offset = (phase if phase < 20 else 40 - phase) - 10
        mid = market_mid + offset * 20 * PRICE_TICK_RAW
        rows.append({
            "instrument": {"symbol": symbol, "venue": "BINANCE"},
            "ts": START_TS + frame_index * 1_000,
            "sequence": frame_index + 1,
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
        "source": f"capacity-synthetic-multi-l2-market-{index}-{snapshots}x{levels}-v1",
        "instrument": {"symbol": symbol, "venue": "BINANCE"},
        "snapshots": rows,
    }
    path.write_text(json.dumps(frame, separators=(",", ":")), encoding="utf-8")


def build_spec(
    frame_paths: list[Path], output_dir: Path, run_id: str, snapshots: int,
) -> dict:
    markets = []
    for index, depth_path in enumerate(frame_paths):
        instrument, base = instrument_parts(index)
        markets.append({
            "instrument": instrument,
            "depth_path": str(depth_path),
            "data_tier": "l2",
            "priority": index,
            "market_spec": {
                "instrument": {"symbol": instrument.split(".", 1)[0], "venue": "BINANCE"},
                "product": "spot",
                "base_currency": base,
                "quote_currency": "USDT",
                "settlement_currency": "USDT",
                "contract_size": SCALE,
                "linear": False,
                "inverse": False,
                "price_tick": PRICE_TICK_RAW,
                "qty_step": 1,
                "min_qty": 1,
                "max_leverage": 1,
                "maintenance_margin_bps": 0,
                "valid_from": START_TS,
                "valid_to": START_TS + snapshots * 1_000 + 1,
            },
            "fee_bps": 1,
            "latency_snapshots": 0,
            "queue_position_bps": 0,
            "market_impact_bps": 0,
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": f"capacity-multi-depth-sma-{index}",
                "quantity_raw": SCALE,
                "fast_window": 5,
                "slow_window": 20,
                "period": 14,
                "threshold_bps": 0,
            },
            "order_policy": {
                "reduce_only": False,
                "position_side": "net",
                "margin_mode": "cash",
                "position_mode": "one_way",
                "leverage": 1,
                "post_only": False,
            },
        })
    return {
        "schema_version": 1,
        "run_id": run_id,
        "account_id": "capacity-shared-account",
        "settlement_currency": "USDT",
        "initial_cash_raw": 1_000_000 * SCALE,
        "output_dir": str(output_dir),
        "markets": markets,
    }


def invoke(binary: Path, spec_path: Path, cwd: Path) -> tuple[dict, dict]:
    measured = run_measured(
        [str(binary), "app", "multi-market-depth-backtest", str(spec_path)], cwd=cwd
    )
    if measured.return_code != 0:
        raise RuntimeError(
            f"multi-market depth backtest failed (exit={measured.return_code}): "
            f"{measured.stderr[-2500:]}"
        )
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


def artifact_bytes(path: Path) -> int:
    return sum(item.stat().st_size for item in path.rglob("*") if item.is_file()) if path.exists() else 0


def validate_artifact(result: dict) -> dict:
    path = Path(result["artifact_path"])
    artifact = json.loads(path.read_text(encoding="utf-8"))
    outcome = artifact.get("outcome", {})
    manifest_path = Path(artifact.get("run_manifest_path", ""))
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if artifact.get("replay_verified") is not True:
        raise RuntimeError("multi-market depth artifact is not marked replay_verified")
    result_hash = result.get("shared_result_hash")
    if not result_hash or outcome.get("shared_result_hash") != result_hash:
        raise RuntimeError("outcome and persisted artifact result hashes differ")
    if manifest.get("result_hash") != result_hash:
        raise RuntimeError("RunManifest result hash differs from the shared outcome")
    events = artifact.get("event_log")
    ledger = artifact.get("ledger_entries")
    if not isinstance(events, list) or not isinstance(ledger, list):
        raise RuntimeError("artifact omitted the shared event log or replayed ledger")
    return {
        "artifact_bytes": artifact_bytes(path.parent),
        "event_count": len(events),
        "ledger_entry_count": len(ledger),
        "run_manifest_sha256": sha256(manifest_path),
        "equity_sidecar_sha256": sha256(Path(artifact["equity_path"])),
        "fills_sidecar_sha256": sha256(Path(artifact["fills_path"])),
        "replay_verification": "product_workflow_replay_verified_and_manifest_hash_matched",
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary")
    parser.add_argument("--snapshots", default="1000,8000")
    parser.add_argument("--markets", default="2,4,8")
    parser.add_argument("--depth-levels", type=int, default=10)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument(
        "--out", type=Path,
        default=ROOT / "maturity/evidence/multi-market-depth-capacity-2026-10-23.json",
    )
    args = parser.parse_args()
    snapshots = parse_sizes(args.snapshots, "--snapshots", 32, 10_000_000)
    market_counts = parse_sizes(args.markets, "--markets", 2, 32)
    if not 1 <= args.depth_levels <= 100:
        parser.error("--depth-levels 必须在 1..=100 内")
    if args.repeats < 2:
        parser.error("--repeats 至少为 2")

    binary = find_binary(args.binary)
    evidence = {
        "schema_version": 1,
        "evidence_kind": "shared_account_multi_market_l2_backtest_capacity",
        "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "hardware": hardware_fingerprint(),
        "binary": str(binary),
        "binary_sha256": sha256(binary),
        "measurement_source_fingerprint_schema": MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA,
        "measurement_source_fingerprint": measurement_source_fingerprint(ROOT),
        "benchmark_script_sha256": sha256(Path(__file__).resolve()),
        "scenario": "qx-cli app multi-market-depth-backtest with synthetic shared-account spot L2 markets",
        "profile_coverage_claim": False,
        "profile_target_statuses_updated": False,
        "resource_measurement": "OS process counters plus optional 10ms psutil process-tree RSS/CPU/I/O sampling",
        "limitations": [
            "synthetic deterministic L2 snapshots and spot instruments; no venue feed or paper/live workflow",
            "market timestamps are aligned and each market has one built-in SMA strategy",
            "process-tree samples may miss brief peaks; this is not a hard memory bound",
            "does not measure long-run stability, allocator calls, internal parallelism, or the nine full product profiles",
        ],
        "workloads": [],
    }
    with tempfile.TemporaryDirectory(prefix="qx-multi-depth-cap-", dir=ROOT / "target") as temporary:
        workdir = Path(temporary)
        for market_count in market_counts:
            if market_count > 32:
                parser.error("--markets 最大值不能超过 32")
            for snapshot_count in snapshots:
                input_started = time.perf_counter()
                frame_paths = []
                for market_index in range(market_count):
                    frame = workdir / f"l2-market-{market_index}-{snapshot_count}.json"
                    generate_depth_frame(market_index, snapshot_count, args.depth_levels, frame)
                    frame_paths.append(frame)
                generation_ms = round((time.perf_counter() - input_started) * 1_000, 3)
                samples = []
                result_hashes = []
                data_fingerprints = []
                input_hashes = [sha256(path) for path in frame_paths]
                for repeat in range(args.repeats):
                    output_dir = workdir / f"run-{market_count}-{snapshot_count}-{repeat}"
                    spec_path = workdir / f"run-{market_count}-{snapshot_count}-{repeat}.spec.json"
                    run_id = f"multi-l2-capacity-{market_count}-{snapshot_count}"
                    spec_path.write_text(
                        json.dumps(build_spec(frame_paths, output_dir, run_id, snapshot_count), separators=(",", ":")),
                        encoding="utf-8",
                    )
                    result, resources = invoke(binary, spec_path, ROOT)
                    artifact_validation = validate_artifact(result)
                    if result.get("markets_total") != market_count:
                        raise RuntimeError("outcome market count does not match the generated input")
                    result_hashes.append(result.get("shared_result_hash"))
                    data_fingerprints.append(result.get("data_fingerprint"))
                    samples.append({**resources, **artifact_validation})
                if len(set(result_hashes)) != 1 or not result_hashes[0]:
                    raise RuntimeError(
                        f"non-deterministic shared replay for markets={market_count}, snapshots={snapshot_count}"
                    )
                if len(set(data_fingerprints)) != 1 or not data_fingerprints[0]:
                    raise RuntimeError("repeated multi-market data fingerprints changed")
                evidence["workloads"].append({
                    "markets": market_count,
                    "snapshots_per_market": snapshot_count,
                    "total_snapshots": market_count * snapshot_count,
                    "depth_levels_per_side": args.depth_levels,
                    "repeats": args.repeats,
                    "input_generation_ms": generation_ms,
                    "input_bytes_total": sum(path.stat().st_size for path in frame_paths),
                    "input_sha256_by_market": input_hashes,
                    "data_fingerprint": data_fingerprints[0],
                    "shared_result_hash": result_hashes[0],
                    "result_hash_unique_count": len(set(result_hashes)),
                    "wall_p50_ms": round(statistics.median(item["wall_ms"] for item in samples), 3),
                    "wall_p95_ms": percentile([item["wall_ms"] for item in samples], 0.95),
                    "peak_rss_bytes_max": max((item["peak_rss_bytes"] for item in samples if item["peak_rss_bytes"] is not None), default=None),
                    "process_tree_peak_rss_bytes_max": max((item["process_tree_peak_rss_bytes"] for item in samples if item["process_tree_peak_rss_bytes"] is not None), default=None),
                    "peak_process_count": max(item["peak_process_count"] for item in samples),
                    "process_tree_cpu_seconds_p50": round(statistics.median([item["process_tree_cpu_seconds"] for item in samples if item["process_tree_cpu_seconds"] is not None]), 3) if any(item["process_tree_cpu_seconds"] is not None for item in samples) else None,
                    "process_tree_io_read_bytes_max": max((item["process_tree_io_read_bytes"] for item in samples if item["process_tree_io_read_bytes"] is not None), default=None),
                    "process_tree_io_write_bytes_max": max((item["process_tree_io_write_bytes"] for item in samples if item["process_tree_io_write_bytes"] is not None), default=None),
                    "artifact_directory_bytes_max": max(item["artifact_bytes"] for item in samples),
                    "event_count_per_repeat": [item["event_count"] for item in samples],
                    "ledger_entries_per_repeat": [item["ledger_entry_count"] for item in samples],
                    "replay_verification": "passed_each_repeat",
                    "scope_claim": "synthetic_shared_account_multi_market_l2_capacity_only",
                })
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"saved {len(evidence['workloads'])} replay-verified multi-market L2 workloads: {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
