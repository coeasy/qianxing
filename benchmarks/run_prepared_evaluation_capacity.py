#!/usr/bin/env python3
"""Measure PreparedEvaluation batch persistence and process-crash recovery."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
import time
import uuid

from hardware import hardware_fingerprint
from process_metrics import run_measured
from source_fingerprint import MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA, measurement_source_fingerprint

ROOT = Path(__file__).resolve().parent.parent
CRASH_POINTS = (
    ("temp_created", 1000),
    ("payload_written", 1000),
    ("temp_synced", 1000),
    ("stale_backup_removed", 1000),
    ("backup_renamed", 1000),
    ("new_published", 1001),
    ("backup_cleaned", 1001),
)


def find_binary(value: str | None) -> Path:
    candidates = [Path(value)] if value else [
        ROOT / "target" / "release" / "qx-cli.exe",
        ROOT / "target" / "release" / "qx-cli",
        ROOT / "target" / "debug" / "qx-cli.exe",
        ROOT / "target" / "debug" / "qx-cli",
    ]
    for candidate in candidates:
        if candidate.is_file():
            return candidate.resolve()
    raise SystemExit("benchmark-feature qx-cli not found; build with --features bench-prepared-evaluation")


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, max(0, math.ceil(len(ordered) * fraction) - 1))]


def command(binary: Path, *args: str) -> list[str]:
    return [str(binary), "bench-prepared-evaluation", *args]


def json_output(stdout: str) -> dict:
    for line in reversed(stdout.splitlines()):
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            return value
    raise RuntimeError(f"benchmark command produced no JSON object:\n{stdout[-2000:]}")


def run(binary: Path, args: list[str], cwd: Path, *, env: dict[str, str] | None = None):
    result = run_measured(command(binary, *args), cwd=cwd, timeout_seconds=120.0, env=env)
    if result.return_code != 0:
        raise RuntimeError(
            f"qx-cli benchmark command exited {result.return_code}: {args}\n"
            f"stdout:\n{result.stdout[-2000:]}\nstderr:\n{result.stderr[-2000:]}"
        )
    return result, json_output(result.stdout)


def crash_recovery(
    binary: Path, root: Path, point: str, expected_run_id: int, repeats: int
) -> dict:
    wall_samples: list[float] = []
    read_samples: list[float] = []
    recovery_rss: list[int | None] = []
    recovery_tree_rss: list[int | None] = []
    journal_bytes: list[int] = []
    recovered_from_backup: list[bool] = []
    for index in range(repeats):
        case = root / f"crash-{point}-{index}"
        case.mkdir()
        journal = case / "worker.prepared-evaluation.json"
        backup = journal.with_suffix(".prepared.bak")
        run(binary, ["prepare", str(journal), "8", "1000"], cwd=case)
        if point == "stale_backup_removed":
            backup.write_bytes(journal.read_bytes())
        env = os.environ.copy()
        env["QX_BENCH_PREPARED_CRASH_AFTER"] = point
        crashed = run_measured(
            command(binary, "prepare", str(journal), "16", "1001"),
            cwd=case,
            timeout_seconds=30.0,
            env=env,
        )
        if crashed.return_code != 86:
            raise RuntimeError(
                f"failpoint {point} did not terminate at the injected boundary: "
                f"exit={crashed.return_code}\n{crashed.stderr[-2000:]}"
            )
        recovered_process, recovery = run(binary, ["recover", str(journal)], cwd=case)
        if recovery["run_id"] != expected_run_id:
            raise RuntimeError(
                f"failpoint {point} recovered run_id={recovery['run_id']}, expected {expected_run_id}"
            )
        wall_samples.append(recovered_process.elapsed_ms)
        read_samples.append(float(recovery["recovery_read_ms"]))
        recovery_rss.append(recovered_process.peak_rss_bytes)
        recovery_tree_rss.append(recovered_process.peak_tree_rss_bytes)
        journal_bytes.append(int(recovery["journal_bytes"]))
        recovered_from_backup.append(bool(recovery["recovered_from_backup"]))
    if len(set(recovered_from_backup)) != 1 or len(set(journal_bytes)) != 1:
        raise RuntimeError(f"failpoint {point} recovery source changed across repetitions")
    return {
        "failpoint": point,
        "repeats": repeats,
        "crash_exit_code": 86,
        "recovered_run_id": expected_run_id,
        "expected_run_id": expected_run_id,
        "recovery_wall_ms_p50_including_process_start": percentile(wall_samples, 0.50),
        "recovery_wall_ms_p95_including_process_start": percentile(wall_samples, 0.95),
        "recovery_read_ms_p50": percentile(read_samples, 0.50),
        "recovery_read_ms_p95": percentile(read_samples, 0.95),
        "recovered_from_backup": recovered_from_backup[0],
        "journal_bytes": journal_bytes[0],
        "recovery_peak_rss_bytes": max((value for value in recovery_rss if value is not None), default=None),
        "recovery_tree_peak_rss_bytes": max((value for value in recovery_tree_rss if value is not None), default=None),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary")
    parser.add_argument("--batch-sizes", default="1,16,64,256,1024,4096,8192,16384")
    parser.add_argument("--iterations", type=int, default=25)
    parser.add_argument("--crash-repeats", type=int, default=5)
    parser.add_argument("--queue-capacity", type=int, default=4096)
    parser.add_argument("--queue-backend", choices=("file", "sqlite", "postgres"), default="file")
    parser.add_argument("--queue-only", action="store_true", help="measure the selected JobQueue backend only")
    parser.add_argument("--control-queue-only", action="store_true", help="measure the selected ControlCommandQueue backend only")
    parser.add_argument("--out", type=Path, default=ROOT / "maturity" / "evidence" / "prepared-evaluation-capacity.json")
    args = parser.parse_args()
    batch_sizes = [int(value) for value in args.batch_sizes.split(",") if value.strip()]
    if not batch_sizes or len(batch_sizes) != len(set(batch_sizes)) or any(not 1 <= n <= 16_384 for n in batch_sizes):
        parser.error("--batch-sizes must contain unique integers in 1..=16384")
    if not 5 <= args.iterations <= 10_000:
        parser.error("--iterations must be in 5..=10000")
    if not 3 <= args.crash_repeats <= 100:
        parser.error("--crash-repeats must be in 3..=100")
    if not 1 <= args.queue_capacity <= 4096:
        parser.error("--queue-capacity must be in 1..=4096")
    if args.queue_only and args.control_queue_only:
        parser.error("--queue-only and --control-queue-only cannot be combined")
    queue_only = args.queue_only or args.control_queue_only
    if args.control_queue_only and args.queue_capacity != 4096:
        parser.error("--control-queue-only requires --queue-capacity 4096")
    if args.queue_backend == "postgres" and not os.environ.get("QX_BENCH_POSTGRES_DSN", "").strip():
        parser.error("--queue-backend postgres requires QX_BENCH_POSTGRES_DSN for an isolated database")
    binary = find_binary(args.binary)
    identity = subprocess.run(
        [str(binary), "version"], capture_output=True, text=True, encoding="utf-8", errors="replace", check=True
    ).stdout.strip()
    result = {
        "schema_version": 1,
        "evidence_kind": (
            "control_command_queue_bounded_capacity" if args.control_queue_only
            else "job_queue_bounded_capacity" if args.queue_only
            else "prepared_evaluation_bounded_persistence_and_crash_recovery"
        ),
        "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "platform": {"system": platform.system(), "release": platform.release(), "machine": platform.machine()},
        "python": {"implementation": platform.python_implementation(), "version": platform.python_version()},
        "hardware": hardware_fingerprint(),
        "binary": str(binary),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "build_identity": identity,
        "measurement_source_fingerprint_schema": MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA,
        "measurement_source_fingerprint": measurement_source_fingerprint(ROOT),
        "batch_sizes": batch_sizes,
        "iterations_per_batch": args.iterations,
        "repeats_per_crash_point": args.crash_repeats,
        "queue_capacity": args.queue_capacity,
        "queue_backend": args.queue_backend,
        "queue_kind": "control-command" if args.control_queue_only else "job" if args.queue_only else "job-and-prepared-evaluation",
        "job_queue_capacity": args.queue_capacity,
        "job_queue_backend": args.queue_backend,
        "write_limit_bytes": 8 * 1024 * 1024,
        "crash_recovery_semantics": "synthetic local journal only; no venue or command side effects",
        "workloads": [],
        "crash_recovery": [],
    }
    with tempfile.TemporaryDirectory(prefix="prepared-evaluation-bench-", dir=ROOT / "target") as temp:
        work = Path(temp)
        if not queue_only:
            for batch in batch_sizes:
                journal = work / f"batch-{batch}" / "worker.prepared-evaluation.json"
                journal.parent.mkdir(parents=True)
                process, measured = run(
                    binary,
                    ["measure", str(journal), str(batch), str(args.iterations)],
                    cwd=journal.parent,
                )
                if measured["journal_bytes"] > result["write_limit_bytes"]:
                    raise RuntimeError(f"batch={batch} exceeded the bounded journal write cap")
                result["workloads"].append({
                    **measured,
                    "peak_rss_bytes": process.peak_rss_bytes,
                    "process_tree_peak_rss_bytes": process.peak_tree_rss_bytes,
                    "peak_process_count": process.peak_process_count,
                    "process_wall_ms_including_startup": process.elapsed_ms,
                })
            for point, expected in CRASH_POINTS:
                result["crash_recovery"].append(
                    crash_recovery(binary, work, point, expected, args.crash_repeats)
                )
            if os.name != "nt":
                result["crash_recovery"].append(
                    crash_recovery(binary, work, "directory_synced", 1001, args.crash_repeats)
                )
        queue_dir = work / ("control-command-queue-capacity" if args.control_queue_only else "job-queue-capacity")
        queue_dir.mkdir()
        queue_namespace = uuid.uuid4().hex[:16]
        queue_operation = "control-queue" if args.control_queue_only else "queue"
        queue_arguments = (
            [queue_operation, args.queue_backend, str(queue_dir), str(args.queue_capacity), queue_namespace]
            if args.control_queue_only
            else [queue_operation, args.queue_backend, str(queue_dir), str(args.queue_capacity),
                  str(args.queue_capacity), queue_namespace]
        )
        queue_key = "control_queue" if args.control_queue_only else "job_queue"
        queue_process, result[queue_key] = run(binary, queue_arguments, cwd=queue_dir)
        result[queue_key].update({
            "peak_rss_bytes": queue_process.peak_rss_bytes,
            "process_tree_peak_rss_bytes": queue_process.peak_tree_rss_bytes,
            "process_wall_ms_including_startup": queue_process.elapsed_ms,
        })
        queue_result = result[queue_key]
        if queue_result["pending_count"] != args.queue_capacity or not queue_result["overflow_rejected"]:
            raise RuntimeError(f"bounded {queue_key} capacity acceptance invariant failed")
        if args.control_queue_only:
            if not queue_result["oversize_rejected"] or queue_result["commands_acknowledged"] != args.queue_capacity:
                raise RuntimeError("bounded ControlCommandQueue acknowledgement invariant failed")
        elif (
            queue_result["worker_batch_count"] > queue_result["worker_batch_limit"]
            or queue_result["benchmark_jobs_acknowledged"] != args.queue_capacity
        ):
            raise RuntimeError("bounded JobQueue worker batch/acknowledgement invariant failed")
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({
        "out": str(args.out),
        "workload_count": len(result["workloads"]),
        "crash_recovery_count": len(result["crash_recovery"]),
        "crash_recovery_samples": sum(row["repeats"] for row in result["crash_recovery"]),
        "queue_backend": args.queue_backend,
        "queue_kind": result["queue_kind"],
        "queue_pending": result[queue_key]["pending_count"],
        "queue_overflow_rejected": result[queue_key]["overflow_rejected"],
        "largest_journal_bytes": max((row["journal_bytes"] for row in result["workloads"]), default=0),
        "result_sha256": hashlib.sha256(args.out.read_bytes()).hexdigest(),
    }, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
