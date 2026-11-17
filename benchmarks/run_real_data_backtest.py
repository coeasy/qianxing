#!/usr/bin/env python3
"""Measure qx-cli end-to-end backtest cost on a frozen real-market BarFrame."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shutil
import statistics
import sys
import tempfile
from pathlib import Path

from hardware import hardware_fingerprint
from process_metrics import run_measured
from run_baseline import find_binary, percentile, resource_summary
from runtime_limits import runtime_limits
from source_fingerprint import MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA, measurement_source_fingerprint

ROOT = Path(__file__).resolve().parent.parent


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default=None)
    parser.add_argument("--bars", required=True, type=Path)
    parser.add_argument("--spec-template", required=True, type=Path)
    parser.add_argument("--repeats", type=int, default=7)
    parser.add_argument(
        "--cpu-affinity-count",
        type=int,
        default=0,
        help="pin every measured CLI process to this many allowed Linux CPUs (0 leaves affinity unchanged)",
    )
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    if args.repeats < 2:
        parser.error("--repeats must be at least 2")
    benchmark_affinity: list[int] | None = None
    command_prefix: list[str] = []
    if args.cpu_affinity_count:
        if args.cpu_affinity_count < 1 or platform.system() != "Linux":
            parser.error("--cpu-affinity-count requires a positive count on Linux")
        try:
            allowed_cpus = sorted(os.sched_getaffinity(0))
        except (AttributeError, OSError):
            parser.error("the current Linux process does not expose CPU affinity")
        if args.cpu_affinity_count > len(allowed_cpus):
            parser.error(
                f"requested {args.cpu_affinity_count} CPUs, but only {len(allowed_cpus)} are allowed"
            )
        taskset = shutil.which("taskset")
        if taskset is None:
            parser.error("taskset is required to enforce --cpu-affinity-count")
        benchmark_affinity = allowed_cpus[: args.cpu_affinity_count]
        command_prefix = [taskset, "--cpu-list", ",".join(map(str, benchmark_affinity))]
    binary = find_binary(args.binary)
    frame = args.bars.resolve()
    bars_count = len(json.loads(frame.read_text(encoding="utf-8"))["ts"])
    template = json.loads(args.spec_template.read_text(encoding="utf-8-sig"))
    template["bars_path"] = str(frame)
    base_run_id = str(template["run_id"])
    samples = []
    hashes = []
    warm_hash = None

    with tempfile.TemporaryDirectory(prefix="qx-real-backtest-", dir=ROOT / "target") as tmp:
        workdir = Path(tmp)
        for index in range(args.repeats + 1):
            spec = dict(template)
            spec["run_id"] = base_run_id
            spec["output_dir"] = str(workdir / f"run-{index}")
            spec_path = workdir / f"spec-{index}.json"
            spec_path.write_text(json.dumps(spec, separators=(",", ":")), encoding="utf-8")
            measured = run_measured(
                [*command_prefix, str(binary), "app", "backtest", str(spec_path)], cwd=ROOT
            )
            if measured.return_code != 0:
                raise RuntimeError(measured.stderr.strip() or measured.stdout.strip())
            payload = json.loads(measured.stdout)
            result_hash = payload.get("result_hash")
            if not isinstance(result_hash, str) or not result_hash:
                raise RuntimeError("backtest response omitted result_hash")
            if index == 0:
                warm_hash = result_hash
            else:
                samples.append(measured)
                hashes.append(result_hash)

    deterministic = len(set(hashes)) == 1 and hashes[0] == warm_hash
    if not deterministic:
        raise RuntimeError(f"real-data result hash changed across repeats: {hashes}")
    p50 = percentile([sample.elapsed_ms for sample in samples], 0.50)
    observed_limits = runtime_limits()
    limitations = [
        "This measures qx-cli with this declared BacktestSpec and frozen data only; it is not a same-semantics competitor comparison.",
        "Fill model, fee schedule, and inline market rules come from the spec template and are not claimed to be a current Binance exchangeInfo snapshot.",
        "The result hash is deterministic execution evidence, not evidence of strategy profitability.",
    ]
    if platform.system() == "Linux":
        if observed_limits["cgroup_cpu_limit_count"] is None:
            limitations.append(
                "No effective cgroup CPU quota was observable; CPU affinity constrains eligible CPUs but does not establish a CPU-time quota."
            )
        if observed_limits["cgroup_memory_limit_bytes"] is None:
            limitations.append(
                "No effective cgroup memory limit was observable; host/container RAM totals must not be treated as the benchmark memory budget."
            )
    if all(sample.peak_tree_rss_bytes is None for sample in samples):
        limitations.append(
            "Process-tree RSS was unavailable in this environment; peak RSS covers the measured CLI process only."
        )
    report = {
        "schema_version": 1,
        "scenario": "qx-cli app backtest with frozen public 180d BTCUSDT 15m data",
        "hardware": hardware_fingerprint(),
        "binary": str(binary),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "measurement_source_fingerprint": measurement_source_fingerprint(ROOT),
        "measurement_source_fingerprint_schema": MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA,
        "bars_path": str(frame.relative_to(ROOT)),
        "bars_sha256": hashlib.sha256(frame.read_bytes()).hexdigest(),
        "spec_template_path": str(args.spec_template.resolve().relative_to(ROOT)),
        "spec_template_sha256": hashlib.sha256(args.spec_template.read_bytes()).hexdigest(),
        "repeats": args.repeats,
        "bars": bars_count,
        "warmup_result_hash": warm_hash,
        "result_hash": hashes[0],
        "deterministic_across_repeats": deterministic,
        "bars_per_second_at_p50": round(bars_count / (p50 / 1000), 2),
        "p50_ms": round(p50, 3),
        "p95_ms": round(percentile([sample.elapsed_ms for sample in samples], 0.95), 3),
        "mean_ms": round(statistics.fmean(sample.elapsed_ms for sample in samples), 3),
        "resources": resource_summary(samples),
        "runtime_context": {
            "container_runtime": os.environ.get("CONTAINER_RUNTIME"),
            "container_image": os.environ.get("CONTAINER_IMAGE"),
            "benchmark_cpu_affinity": benchmark_affinity,
            "benchmark_cpu_affinity_count": len(benchmark_affinity)
            if benchmark_affinity is not None
            else None,
            **observed_limits,
        },
        "samples": [
            {
                "elapsed_ms": round(sample.elapsed_ms, 3),
                "peak_process_rss_bytes": sample.peak_rss_bytes,
                "peak_process_tree_rss_bytes": sample.peak_tree_rss_bytes,
                "peak_process_count": sample.peak_process_count,
                "tree_cpu_seconds": sample.tree_cpu_seconds,
                "tree_io_read_bytes": sample.tree_io_read_bytes,
                "tree_io_write_bytes": sample.tree_io_write_bytes,
            }
            for sample in samples
        ],
        "limitations": limitations,
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
