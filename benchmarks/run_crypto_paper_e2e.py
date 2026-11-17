#!/usr/bin/env python3
"""Measure the real Paper worker acceptance chain on isolated local roots."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import re
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

from hardware import hardware_fingerprint
from process_metrics import run_measured
from source_fingerprint import MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA, measurement_source_fingerprint

ROOT = Path(__file__).resolve().parent.parent
ACCEPTANCE = ROOT / "tools" / "paper_acceptance.py"
MIN_REPEATS = 3
FLOW_COUNTS = re.compile(
    r"Paper 轨验收通过：OrderSubmitted=(\d+) Filled=(\d+) LedgerApplied=(\d+)"
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
    raise SystemExit("找不到 qx-cli；请先构建或使用 --binary 指定")


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, math.ceil(fraction * len(ordered)) - 1))
    return ordered[index]


def run_acceptance(binary: Path, root: Path) -> dict[str, int | float | None]:
    process = run_measured(
        [
            sys.executable,
            str(ACCEPTANCE),
            "--binary",
            str(binary),
            "--root",
            str(root),
            "--dry-run",
        ],
        cwd=ROOT,
        timeout_seconds=300.0,
    )
    if process.return_code != 0:
        raise SystemExit(
            f"Paper acceptance 失败（exit={process.return_code}）：\n"
            f"{process.stdout[-4000:]}\n{process.stderr[-4000:]}"
        )
    match = FLOW_COUNTS.search(process.stdout)
    if match is None:
        raise SystemExit(
            "Paper acceptance 虽退出成功，但没有可解析的主链成交计数：\n"
            f"{process.stdout[-4000:]}"
        )
    return {
        "elapsed_ms": process.elapsed_ms,
        "orders_submitted": int(match.group(1)),
        "fills": int(match.group(2)),
        "ledger_entries": int(match.group(3)),
        "peak_rss_bytes": process.peak_rss_bytes,
        "process_tree_peak_rss_bytes": process.peak_tree_rss_bytes,
        "peak_process_count": process.peak_process_count,
        "tree_cpu_seconds": process.tree_cpu_seconds,
        "tree_io_read_bytes": process.tree_io_read_bytes,
        "tree_io_write_bytes": process.tree_io_write_bytes,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary")
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument(
        "--out",
        type=Path,
        default=ROOT / "benchmarks" / "results" / "crypto-paper-e2e.json",
    )
    args = parser.parse_args()
    if args.repeats < MIN_REPEATS:
        parser.error(f"--repeats 至少为 {MIN_REPEATS}")

    binary = find_binary(args.binary)
    samples: list[dict[str, int | float | None]] = []
    with tempfile.TemporaryDirectory(prefix="qx-crypto-paper-e2e-", dir=ROOT / "target") as temporary:
        work = Path(temporary)
        run_acceptance(binary, work / "warmup")
        for index in range(args.repeats):
            samples.append(run_acceptance(binary, work / f"sample-{index}"))

    elapsed = [float(sample["elapsed_ms"]) for sample in samples]
    flow_signature = {
        (sample["orders_submitted"], sample["fills"], sample["ledger_entries"])
        for sample in samples
    }
    if len(flow_signature) != 1:
        raise SystemExit(f"Paper 主链计数跨重复不稳定：{sorted(flow_signature)}")

    def percentile_optional(key: str, fraction: float) -> int | float | None:
        values = [sample[key] for sample in samples if isinstance(sample[key], (int, float))]
        return round(float(percentile(values, fraction)), 3) if values else None

    first_flow = next(iter(flow_signature))

    report = {
        "schema_version": 1,
        "evidence_kind": "crypto_paper_worker_acceptance_e2e",
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "hardware": hardware_fingerprint(),
        "measurement_source_fingerprint": measurement_source_fingerprint(ROOT),
        "measurement_source_fingerprint_schema": MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA,
        "binary": binary.name,
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "scenario": "tools/paper_acceptance.py: scheduler -> strategy -> Paper execution -> Ledger/EventLog/audit, same-root idempotent rerun, and independent-root parity",
        "profile_coverage_claim": False,
        "measurement_method": {
            "process_tree": "run_measured samples the Python acceptance driver and descendants every 10 ms when psutil is available",
            "memory": "main-process OS peak RSS plus sampled aggregate process-tree RSS; tree samples may miss short spikes",
            "scope": "each sample creates two isolated roots, runs the Paper chain, verifies an idempotent rerun, checks audit termination and compares independent fact surfaces",
        },
        "records": [
            {
                "benchmark": "end_to_end",
                "operations": "crypto-paper-worker-acceptance-v1",
                "samples": args.repeats,
                "median_ms": round(percentile(elapsed, 0.50), 3),
                "p95_ms": round(percentile(elapsed, 0.95), 3),
                "raw_ms": [round(sample, 3) for sample in elapsed],
                "verified_main_chain_counts_per_independent_root": {
                    "orders_submitted": first_flow[0],
                    "fills": first_flow[1],
                    "ledger_entries": first_flow[2],
                },
                "peak_rss_bytes_p95": percentile_optional("peak_rss_bytes", 0.95),
                "process_tree_peak_rss_bytes_p95": percentile_optional(
                    "process_tree_peak_rss_bytes", 0.95
                ),
                "peak_process_count_p95": percentile_optional("peak_process_count", 0.95),
                "tree_cpu_seconds_p95": percentile_optional("tree_cpu_seconds", 0.95),
                "tree_io_read_bytes_p95": percentile_optional("tree_io_read_bytes", 0.95),
                "tree_io_write_bytes_p95": percentile_optional("tree_io_write_bytes", 0.95),
                "raw_samples": samples,
            }
        ],
        "limitations": [
            "The worker acceptance fixture uses one Paper order and does not model funding provider retrieval, eight-hour funding cadence, or an externally deployed database.",
            "Maker/taker, precision, weighted rate-limit, partial-fill and funding-ledger performance is measured separately by qx_zhenlu_crypto_paper_lifecycle_v2; this E2E row is not a complete crypto-paper profile claim.",
        ],
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
