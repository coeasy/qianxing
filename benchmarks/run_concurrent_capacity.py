#!/usr/bin/env python3
"""Measure aggregate qx-cli capacity under concurrent synthetic Bar backtests."""

from __future__ import annotations

import argparse
import json
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from hardware import hardware_fingerprint
from process_metrics import run_measured
from run_profile_backtest import (
    generate_frame,
    load_fixture,
    runtime_config,
    sha256,
)
from source_fingerprint import MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA, measurement_source_fingerprint

ROOT = Path(__file__).resolve().parent.parent
BAR_COUNT = 256_000
CONCURRENCY = (1, 2, 4)
REPEATS = 3
FIXTURE = "crypto-spot"


def command_for(binary: Path, frame: Path, market: Path, config: Path, quantity: int) -> list[str]:
    return [
        str(binary), "backtest", "builtin", "sma_cross", str(frame), str(market),
        str(quantity), "--config", str(config),
    ]


def execute_workers(job_path: Path) -> int:
    job = json.loads(job_path.read_text(encoding="utf-8"))
    processes = [
        subprocess.Popen(
            command,
            cwd=job["cwd"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
        for command in job["commands"]
    ]
    results = []
    for process in processes:
        stdout, stderr = process.communicate()
        digest = next(
            (line.split("result_hash=", 1)[1].split()[0]
             for line in stdout.splitlines() if "result_hash=" in line),
            "",
        )
        results.append({"exit_code": process.returncode, "result_hash": digest, "stderr": stderr[-1000:]})
    print(json.dumps(results, separators=(",", ":")))
    return 1 if any(row["exit_code"] != 0 or not row["result_hash"] for row in results) else 0


def parse_worker_output(stdout: str, expected: int) -> list[dict]:
    try:
        rows = json.loads(stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"concurrent runner returned invalid JSON: {error}: {stdout[-500:]}") from error
    if not isinstance(rows, list) or len(rows) != expected:
        raise RuntimeError(f"expected {expected} worker results, got {len(rows) if isinstance(rows, list) else type(rows)}")
    failures = [row for row in rows if row.get("exit_code") != 0 or not row.get("result_hash")]
    if failures:
        raise RuntimeError(f"one or more concurrent backtests failed: {failures}")
    return rows


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--concurrency", default=",".join(map(str, CONCURRENCY)))
    parser.add_argument("--bars", type=int, default=BAR_COUNT)
    parser.add_argument("--repeats", type=int, default=REPEATS)
    parser.add_argument("--out", type=Path, default=ROOT / "maturity" / "evidence" / "concurrent-capacity-2026-10-22.json")
    parser.add_argument("--execute-workers", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.execute_workers:
        return execute_workers(args.execute_workers)

    concurrency = [int(value) for value in args.concurrency.split(",") if value.strip()]
    if not concurrency or any(value < 1 or value > 32 for value in concurrency) or len(set(concurrency)) != len(concurrency):
        parser.error("--concurrency values must be unique integers in 1..=32")
    if args.bars < 3 or args.repeats < 2:
        parser.error("--bars must be >= 3 and --repeats must be >= 2")
    binary = args.binary or next(
        (candidate for candidate in (ROOT / "target/release/qx-cli.exe", ROOT / "target/release/qx-cli") if candidate.is_file()),
        None,
    )
    if binary is None or not binary.is_file():
        parser.error("qx-cli Release binary not found; build it first or pass --binary")
    binary = binary.resolve()
    spec, identity = load_fixture(FIXTURE)
    quantity = max(1, (int(spec["min_qty"]) + 1_000_000_000 - 1) // 1_000_000_000)
    report = {
        "schema_version": 1,
        "evidence_kind": "concurrent_synthetic_bar_backtest_capacity_proxy",
        "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "hardware": hardware_fingerprint(),
        "binary": str(binary),
        "binary_sha256": sha256(binary),
        "measurement_source_fingerprint_schema": MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA,
        "measurement_source_fingerprint": measurement_source_fingerprint(ROOT),
        "fixture": FIXTURE,
        "bars_per_worker": args.bars,
        "repeats": args.repeats,
        "method": "one Python orchestrator launches isolated qx-cli backtest processes; process_metrics samples orchestrator plus descendants",
        "profile_coverage_claim": False,
        "profile_target_statuses_updated": False,
        "workloads": [],
    }
    with tempfile.TemporaryDirectory(prefix="cap-", dir=ROOT / "target") as temp:
        workdir = Path(temp)
        market = workdir / "market.json"
        market.write_text(json.dumps(spec), encoding="utf-8")
        frame = workdir / f"{FIXTURE}-{args.bars}.json"
        generate_frame(args.bars, FIXTURE, spec, identity, frame)
        config_paths = []
        for worker_index in range(max(concurrency)):
            config = runtime_config(FIXTURE, args.bars, identity, workdir)
            payload = json.loads(config.read_text(encoding="utf-8"))
            payload["storage"]["data_dir"] = str(workdir / f"storage-worker-{worker_index}")
            config = workdir / f"worker-{worker_index}.runtime.json"
            config.write_text(json.dumps(payload, ensure_ascii=False), encoding="utf-8")
            config_paths.append(config)
        for count in concurrency:
            commands = [command_for(binary, frame, market, config_paths[index], quantity) for index in range(count)]
            job_path = workdir / f"job-{count}.json"
            job_path.write_text(json.dumps({"cwd": str(workdir), "commands": commands}), encoding="utf-8")
            warm_elapsed, warm_stdout, warm_stderr, warm_code, _, _, _ = run_measured(
                [sys.executable, str(Path(__file__).resolve()), "--execute-workers", str(job_path)], cwd=ROOT
            )
            if warm_code != 0:
                raise RuntimeError(f"concurrency={count} warmup failed (exit={warm_code}): {warm_stderr[-2000:]}")
            warm_rows = parse_worker_output(warm_stdout, count)
            warm_hashes = [row["result_hash"] for row in warm_rows]
            samples, tree_rss, process_counts, hash_runs = [], [], [], []
            cpu_seconds, io_read_bytes, io_write_bytes = [], [], []
            for _ in range(args.repeats):
                measured = run_measured(
                    [sys.executable, str(Path(__file__).resolve()), "--execute-workers", str(job_path)], cwd=ROOT
                )
                elapsed, stdout, stderr, code, _, tree_bytes, process_count = measured
                if code != 0:
                    raise RuntimeError(f"concurrency={count} failed (exit={code}): {stderr[-2000:]}")
                rows = parse_worker_output(stdout, count)
                hashes = [row["result_hash"] for row in rows]
                if hashes != warm_hashes:
                    raise RuntimeError(f"concurrency={count} result hashes changed between warmup and sample")
                samples.append(elapsed)
                hash_runs.append(hashes)
                if tree_bytes is not None:
                    tree_rss.append(tree_bytes)
                process_counts.append(process_count)
                if measured.tree_cpu_seconds is not None:
                    cpu_seconds.append(measured.tree_cpu_seconds)
                if measured.tree_io_read_bytes is not None:
                    io_read_bytes.append(measured.tree_io_read_bytes)
                if measured.tree_io_write_bytes is not None:
                    io_write_bytes.append(measured.tree_io_write_bytes)
            ordered = sorted(samples)
            report["workloads"].append({
                "concurrent_backtests": count,
                "bars_total": count * args.bars,
                "warmup_wall_ms": round(warm_elapsed, 3),
                "p50_wall_ms": round(statistics.median(samples), 3),
                "p95_wall_ms": round(ordered[min(len(ordered) - 1, round(0.95 * (len(ordered) - 1)))], 3),
                "aggregate_peak_process_tree_rss_bytes_max": max(tree_rss) if tree_rss else None,
                "peak_process_count": max(process_counts, default=count + 1),
                "process_tree_cpu_seconds_p50": round(statistics.median(cpu_seconds), 3) if cpu_seconds else None,
                "process_tree_io_read_bytes_max": max(io_read_bytes) if io_read_bytes else None,
                "process_tree_io_write_bytes_max": max(io_write_bytes) if io_write_bytes else None,
                "throughput_bars_per_second_p50": round(count * args.bars / (statistics.median(samples) / 1000), 2),
                "result_hashes_by_worker": warm_hashes,
                "deterministic_across_repeats": all(rows == warm_hashes for rows in hash_runs),
                "scope_limitations": ["synthetic crypto spot Bars", "separate qx-cli processes and storage roots", "not an internal multi-job scheduler or full crypto-paper profile"],
            })
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"saved concurrent capacity proxy: {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
