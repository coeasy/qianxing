#!/usr/bin/env python3
"""Measure allocator requests inside the shared qx-app Bar backtest use case.

The Rust example counts requested allocation bytes/calls in an isolated process. This driver
adds wall-clock, RSS, process-tree CPU/I/O, workload and source identity without treating any
single synthetic market proxy as a completed product capacity profile.
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
from run_profile_backtest import BASE_PRICE, SCALE, directory_bytes, generate_frame, load_fixture
from source_fingerprint import MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA, measurement_source_fingerprint

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_EVIDENCE = ROOT / "maturity" / "evidence" / "bar-allocator-capacity-2026-10-23.json"


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def percentile(values: list[float], percent: float) -> float:
    ordered = sorted(values)
    index = min(len(ordered) - 1, round(percent * (len(ordered) - 1)))
    return round(ordered[index], 3)


def find_binary(value: str | None) -> Path:
    candidates = [Path(value)] if value else [
        ROOT / "target" / "release" / "examples" / "measure_allocations.exe",
        ROOT / "target" / "release" / "examples" / "measure_allocations",
        ROOT / "target" / "debug" / "examples" / "measure_allocations.exe",
        ROOT / "target" / "debug" / "examples" / "measure_allocations",
    ]
    for path in candidates:
        if path.is_file():
            return path.resolve()
    raise SystemExit(
        "找不到 measure_allocations 示例；请先运行 "
        "cargo build --release -p qx-cli --example measure_allocations"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", help="compiled measure_allocations example")
    parser.add_argument("--sizes", default="1000,8000,64000")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--out", type=Path, default=DEFAULT_EVIDENCE)
    args = parser.parse_args()
    sizes = [int(value) for value in args.sizes.split(",") if value.strip()]
    if not sizes or any(value < 3 for value in sizes) or len(sizes) != len(set(sizes)):
        parser.error("--sizes 必须是互不重复且至少为 3 的正整数")
    if args.repeats < 2:
        parser.error("--repeats 至少为 2")

    binary = find_binary(args.binary)
    binary_sha256 = sha256_file(binary)
    fixture = "crypto-spot"
    market_spec, identity = load_fixture(fixture)
    quantity_raw = max(1, int(market_spec["min_qty"]))
    report = {
        "schema_version": 1,
        "evidence_kind": "application_backtest_allocator_capacity",
        "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "hardware": hardware_fingerprint(),
        "measurement_binary": str(binary),
        "measurement_binary_sha256": binary_sha256,
        "measurement_source_fingerprint_schema": MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA,
        "measurement_source_fingerprint": measurement_source_fingerprint(ROOT),
        "benchmark_script_sha256": sha256_file(Path(__file__).resolve()),
        "scenario": "qx_app::run_backtest on deterministic synthetic crypto-spot BarFrames",
        "allocator": "std::alloc::System wrapped by an isolated counting GlobalAlloc",
        "allocation_scope": "application use case; spec parsing and result serialization excluded",
        "profile_coverage_claim": False,
        "profile_target_statuses_updated": False,
        "limitations": [
            "synthetic crypto-spot inputs and one built-in SMA strategy",
            "allocation counts cover qx_app::run_backtest, not CLI/API startup or provider workers",
            "requested live bytes are allocator-level and are not process RSS or a hard memory limit",
            "this evidence does not measure parallel search, long-term stability, or full product profiles",
        ],
        "workloads": [],
    }

    with tempfile.TemporaryDirectory(prefix="qx-alloc-cap-", dir=ROOT / "target") as temp:
        workdir = Path(temp)
        for size in sizes:
            frame = workdir / f"{fixture}-{size}.bars.json"
            generate_frame(size, fixture, market_spec, identity, frame)
            spec_path = workdir / f"{fixture}-{size}.spec.json"
            spec_document = {
                "schema_version": 1,
                "run_id": f"allocation-profile-{fixture}-{size}",
                "instrument": identity["instrument"],
                "bars_path": str(frame.resolve()),
                "settlement_currency": market_spec["quote_currency"],
                "initial_cash_raw": 100_000_000_000_000,
                "seed": 20261023,
                "output_dir": str((workdir / "runs").resolve()),
                "market_spec": market_spec,
                "fee_schedule": {"maker_fee_bps": 2, "taker_fee_bps": 5},
                "strategy": {
                    "kind": "sma_cross",
                    "strategy_id": "allocation-profile-sma-cross-v1",
                    "quantity_raw": quantity_raw,
                    "fast_window": 5,
                    "slow_window": 20,
                },
            }
            spec_path.write_text(json.dumps(spec_document, ensure_ascii=False, separators=(",", ":")), encoding="utf-8")
            samples = []
            for repeat in range(args.repeats):
                run_id = f"allocation-{size}-{repeat + 1}"
                output_dir = workdir / "runs" / f"{size}-{repeat + 1}"
                measured = run_measured(
                    [str(binary), str(spec_path), run_id, str(output_dir)], cwd=ROOT
                )
                if measured.return_code != 0:
                    raise RuntimeError(
                        f"allocator workload failed (N={size}, repeat={repeat + 1}, "
                        f"exit={measured.return_code}): {measured.stderr[-2000:]}"
                    )
                try:
                    allocation = json.loads(measured.stdout)
                except json.JSONDecodeError as error:
                    raise RuntimeError(
                        f"allocator workload emitted invalid JSON (N={size}): {measured.stdout[-1000:]}"
                    ) from error
                if allocation.get("schema_version") != 1 or allocation.get("measurement_kind") != "qx_app_bar_backtest_allocator_requests":
                    raise RuntimeError(f"allocator workload returned an unexpected contract: {allocation}")
                allocation["process_wall_ms"] = round(measured.elapsed_ms, 3)
                allocation["peak_rss_bytes"] = measured.peak_rss_bytes
                allocation["process_tree_peak_rss_bytes"] = measured.peak_tree_rss_bytes
                allocation["peak_process_count"] = measured.peak_process_count
                allocation["process_tree_cpu_seconds"] = measured.tree_cpu_seconds
                allocation["process_tree_io_read_bytes"] = measured.tree_io_read_bytes
                allocation["process_tree_io_write_bytes"] = measured.tree_io_write_bytes
                samples.append(allocation)

            hashes = [sample["result_hash"] for sample in samples]
            if len(set(hashes)) != 1 or any(sample["fills"] == 0 for sample in samples):
                raise RuntimeError(f"allocation workload did not produce repeatable filled runs (N={size})")
            requested = [float(sample["requested_bytes_total"]) for sample in samples]
            calls = [float(sample["allocation_calls"]) for sample in samples]
            peak_requested = [float(sample["peak_live_requested_bytes_delta"]) for sample in samples]
            report["workloads"].append({
                "fixture": fixture,
                "bars": size,
                "repeats": args.repeats,
                "input_bytes": frame.stat().st_size + spec_path.stat().st_size,
                "bar_frame_sha256": sha256_file(frame),
                "run_spec_sha256": sha256_file(spec_path),
                "output_bytes_max": max(directory_bytes(workdir / "runs" / f"{size}-{repeat + 1}") for repeat in range(args.repeats)),
                "result_hash": hashes[0],
                "fills": samples[0]["fills"],
                "allocation_calls_p50": round(statistics.median(calls)),
                "allocation_calls_p95": percentile(calls, 0.95),
                "requested_bytes_total_p50": round(statistics.median(requested)),
                "requested_bytes_total_p95": percentile(requested, 0.95),
                "peak_live_requested_bytes_delta_p50": round(statistics.median(peak_requested)),
                "peak_live_requested_bytes_delta_p95": percentile(peak_requested, 0.95),
                "application_wall_p50_ms": round(statistics.median(float(sample["wall_ms"]) for sample in samples), 3),
                "application_wall_p95_ms": percentile([float(sample["wall_ms"]) for sample in samples], 0.95),
                "process_wall_p50_ms": round(statistics.median(float(sample["process_wall_ms"]) for sample in samples), 3),
                "process_wall_p95_ms": percentile([float(sample["process_wall_ms"]) for sample in samples], 0.95),
                "peak_rss_bytes_max": max((sample["peak_rss_bytes"] for sample in samples if sample["peak_rss_bytes"] is not None), default=None),
                "process_tree_peak_rss_bytes_max": max((sample["process_tree_peak_rss_bytes"] for sample in samples if sample["process_tree_peak_rss_bytes"] is not None), default=None),
                "process_tree_cpu_seconds_p50": round(statistics.median(float(sample["process_tree_cpu_seconds"]) for sample in samples if sample["process_tree_cpu_seconds"] is not None), 3) if any(sample["process_tree_cpu_seconds"] is not None for sample in samples) else None,
                "process_tree_cpu_seconds_p95": percentile([float(sample["process_tree_cpu_seconds"]) for sample in samples if sample["process_tree_cpu_seconds"] is not None], 0.95) if any(sample["process_tree_cpu_seconds"] is not None for sample in samples) else None,
                "process_tree_io_read_bytes_max": max((sample["process_tree_io_read_bytes"] for sample in samples if sample["process_tree_io_read_bytes"] is not None), default=None),
                "process_tree_io_write_bytes_max": max((sample["process_tree_io_write_bytes"] for sample in samples if sample["process_tree_io_write_bytes"] is not None), default=None),
                "result_hash_unique_count": len(set(hashes)),
                "scope_claim": "qx_app_bar_backtest_allocator_only",
                "samples": samples,
            })

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"saved {len(report['workloads'])} allocator workloads: {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
