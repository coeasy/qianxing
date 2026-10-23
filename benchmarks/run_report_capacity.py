#!/usr/bin/env python3
"""Measure verified CLI research-report generation on one frozen backtest run.

This records report generation separately from backtest execution. It is a synthetic
crypto-spot workload and does not claim a complete product profile or production limit.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import statistics
import sys
import tempfile
import time
from pathlib import Path

from hardware import hardware_fingerprint
from process_metrics import run_measured
from run_profile_backtest import (
    BASE_PRICE,
    SCALE,
    directory_bytes,
    find_binary,
    generate_frame,
    load_fixture,
    runtime_config,
)
from source_fingerprint import MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA, measurement_source_fingerprint

ROOT = Path(__file__).resolve().parent.parent
FORMATS = ("json", "html", "xlsx")
DIAGNOSTIC_FORMATS = ("html-diagnostics", "xlsx-diagnostics")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def percentile(values: list[float], percent: float) -> float:
    ordered = sorted(values)
    index = min(len(ordered) - 1, round(percent * (len(ordered) - 1)))
    return round(ordered[index], 3)


def run_backtest(binary: Path, fixture: str, size: int, frame: Path, market: Path, config: Path) -> tuple[str, str]:
    quantity = max(1, (int(load_fixture(fixture)[0]["min_qty"]) + SCALE - 1) // SCALE)
    measured = run_measured(
        [
            str(binary), "backtest", "builtin", "sma_cross", str(frame), str(market),
            str(quantity), "--config", str(config),
        ],
        cwd=ROOT,
    )
    if measured.return_code != 0:
        raise RuntimeError(f"backtest setup failed (N={size}, exit={measured.return_code}): {measured.stderr[-2000:]}")
    result_hash = re.search(r"result_hash=([0-9a-fA-F]{16})", measured.stdout)
    summary = re.search(r"\[Artifacts\] summary=(\S+)", measured.stdout)
    if not result_hash or not summary:
        raise RuntimeError(f"backtest setup omitted result_hash/summary path: {measured.stdout[-2000:]}")
    return result_hash.group(1).lower(), str(Path(summary.group(1)).resolve())


def run_report(
    binary: Path,
    report_format: str,
    summary: Path,
    output: Path,
    diagnostics: Path | None = None,
) -> dict:
    command = [str(binary), "report", str(summary)]
    if report_format.endswith("-diagnostics"):
        if diagnostics is None:
            raise ValueError("diagnostic report format requires a bound diagnostics artifact")
        command.extend(["--strategy-diagnostics", str(diagnostics)])
        report_format = report_format.removesuffix("-diagnostics")
    if report_format == "json":
        command.append("--json")
    elif report_format == "html":
        command.extend(["--html", "--output", str(output)])
    elif report_format == "xlsx":
        command.extend(["--xlsx", str(output)])
    else:
        raise ValueError(f"unsupported report format: {report_format}")
    measured = run_measured(command, cwd=ROOT)
    if measured.return_code != 0:
        raise RuntimeError(
            f"{report_format} report failed (exit={measured.return_code}): {measured.stderr[-2000:]}"
        )
    payload = measured.stdout.encode("utf-8") if report_format == "json" else output.read_bytes()
    return {
        "wall_ms": round(measured.elapsed_ms, 3),
        "peak_rss_bytes": measured.peak_rss_bytes,
        "process_tree_peak_rss_bytes": measured.peak_tree_rss_bytes,
        "peak_process_count": measured.peak_process_count,
        "process_tree_cpu_seconds": measured.tree_cpu_seconds,
        "process_tree_io_read_bytes": measured.tree_io_read_bytes,
        "process_tree_io_write_bytes": measured.tree_io_write_bytes,
        "output_bytes": len(payload),
        "output_sha256": sha256_bytes(payload),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary")
    parser.add_argument("--sizes", default="1000,8000,64000")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument(
        "--with-diagnostics",
        action="store_true",
        help="also measure HTML/XLSX export with a bounded identity-matched diagnostic artifact",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=ROOT / "maturity" / "evidence" / "report-generation-capacity.json",
    )
    args = parser.parse_args()
    sizes = [int(value) for value in args.sizes.split(",") if value.strip()]
    if not sizes or any(value < 3 for value in sizes) or len(sizes) != len(set(sizes)):
        parser.error("--sizes 必须是互不重复且至少为 3 的正整数")
    if args.repeats < 2:
        parser.error("--repeats 至少为 2")
    binary = find_binary(args.binary)
    binary_hash = sha256_file(binary)
    report = {
        "schema_version": 1,
        "evidence_kind": "verified_research_report_generation_capacity",
        "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "hardware": hardware_fingerprint(),
        "binary": str(binary),
        "binary_sha256": binary_hash,
        "measurement_source_fingerprint_schema": MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA,
        "measurement_source_fingerprint": measurement_source_fingerprint(ROOT),
        "benchmark_script_sha256": sha256_file(Path(__file__).resolve()),
        "scenario": "qx-cli report JSON/HTML/XLSX with optional identity-bound diagnostic attachment over one replay-verified synthetic crypto-spot SMA backtest",
        "report_input_is_reused_across_formats": True,
        "profile_coverage_claim": False,
        "profile_target_statuses_updated": False,
        "diagnostic_fixture": {
            "synthetic": True,
            "checks_per_size": 200,
            "purpose": "exercise identity-bound HTML/XLSX rendering only; no strategy diagnosis is performed",
        },
        "resource_measurement": {
            "peak_rss_field": "peak_rss_bytes",
            "process_tree_peak_rss_field": "process_tree_peak_rss_bytes",
            "process_tree_method": "benchmarks/process_metrics.py OS process counters and optional psutil 10ms tree sampling",
            "cpu_and_io": "process-tree CPU seconds and OS read/write bytes when the platform exposes them; null remains unavailable",
        },
        "limitations": [
            "synthetic crypto spot Bar input and one built-in strategy",
            "report formats measured in separate qx-cli processes after the same backtest run",
            "diagnostic attachment is synthetic and its strategy analysis/replay time is not measured",
            "does not measure PDF export, multi-market/depth reports, internal parameter-search parallelism, or full Paper/live profiles",
            "short-process RSS and CPU/I/O counters can be unavailable or miss brief peaks",
        ],
        "workloads": [],
    }
    formats = FORMATS + DIAGNOSTIC_FORMATS if args.with_diagnostics else FORMATS
    fixture = "crypto-spot"
    spec, identity = load_fixture(fixture)
    with tempfile.TemporaryDirectory(prefix="qx-report-cap-", dir=ROOT / "target") as temp:
        workdir = Path(temp)
        for size in sizes:
            frame = workdir / f"{fixture}-{size}.bars.json"
            market = workdir / f"{fixture}-{size}.market.json"
            market.write_text(json.dumps(spec, separators=(",", ":")), encoding="utf-8")
            generation_started = time.perf_counter()
            generate_frame(size, fixture, spec, identity, frame)
            config = runtime_config(fixture, size, identity, workdir)
            generation_ms = round((time.perf_counter() - generation_started) * 1_000, 3)
            result_hash, summary_text = run_backtest(binary, fixture, size, frame, market, config)
            summary = Path(summary_text)
            if not summary.is_file():
                raise RuntimeError(f"summary artifact missing: {summary}")
            data_root = Path(json.loads(config.read_text(encoding="utf-8"))["storage"]["data_dir"])
            diagnostics_path = None
            if args.with_diagnostics:
                summary_payload = json.loads(summary.read_text(encoding="utf-8"))
                frame_payload = json.loads(frame.read_text(encoding="utf-8"))
                diagnostics_path = workdir / f"{fixture}-{size}.diagnostics.json"
                checks = [
                    {
                        "decision_as_of": int(frame_payload["ts"][min(index, size - 1)]),
                        "perturbation_profile": "prices_up",
                        "future_perturbation_changed_decision": False,
                        "intermediate_state_changed": False,
                        "changed_decision_fields": [],
                        "intermediate_state_changed_fields": [],
                        "baseline_decision": {"target_qty": "0", "confidence": 10_000},
                        "counterfactual_decision": {"target_qty": "0", "confidence": 10_000},
                    }
                    for index in range(min(200, size))
                ]
                diagnostics_payload = {
                    "schema_version": 1,
                    "kind": "qianxing-strategy-bias-diagnostics",
                    "strategy_id": summary_payload["strategy_id"],
                    "instrument": summary_payload["instrument"],
                    "data_fingerprint": summary_payload["input"]["fingerprint"],
                    "input_bars": size,
                    "as_of": int(frame_payload["ts"][-1]),
                    "lookahead": {
                        "status": "no_change_detected",
                        "checks_run": len(checks),
                        "decision_points_checked": len(checks),
                        "checks": checks,
                    },
                    "startup": {
                        "status": "stable_across_requested_windows",
                        "windows": [{"requested_bars": 20, "available_bars": 20}],
                    },
                    "data_visibility": {"status": "visibility_unknown"},
                }
                diagnostics_path.write_text(
                    json.dumps(diagnostics_payload, ensure_ascii=False, separators=(",", ":"))
                    + "\n",
                    encoding="utf-8",
                )
            base_input_bytes = frame.stat().st_size + market.stat().st_size + config.stat().st_size + directory_bytes(data_root)
            diagnostics_bytes = diagnostics_path.stat().st_size if diagnostics_path else None
            for report_format in formats:
                samples = []
                output_hashes = []
                uses_diagnostics = report_format.endswith("-diagnostics")
                input_bytes = base_input_bytes + (
                    diagnostics_bytes if uses_diagnostics and diagnostics_bytes is not None else 0
                )
                for repeat in range(args.repeats):
                    file_format = report_format.removesuffix("-diagnostics")
                    output = workdir / "reports" / f"{fixture}-{size}-{report_format}-{repeat + 1}.{file_format}"
                    output.parent.mkdir(parents=True, exist_ok=True)
                    sample = run_report(binary, report_format, summary, output, diagnostics_path)
                    samples.append(sample)
                    output_hashes.append(sample["output_sha256"])
                row = {
                    "bars": size,
                    "report_format": report_format,
                    "repeats": args.repeats,
                    "input_generation_ms": generation_ms,
                    "replay_verified_backtest_result_hash": result_hash,
                    "summary_sha256": sha256_file(summary),
                    "report_input_bytes": input_bytes,
                    "diagnostics_input_bytes": diagnostics_bytes if uses_diagnostics else None,
                    "wall_p50_ms": round(statistics.median(item["wall_ms"] for item in samples), 3),
                    "wall_p95_ms": percentile([item["wall_ms"] for item in samples], 0.95),
                    "peak_rss_bytes_max": max((item["peak_rss_bytes"] for item in samples if item["peak_rss_bytes"] is not None), default=None),
                    "process_tree_peak_rss_bytes_max": max((item["process_tree_peak_rss_bytes"] for item in samples if item["process_tree_peak_rss_bytes"] is not None), default=None),
                    "peak_process_count": max(item["peak_process_count"] for item in samples),
                    "process_tree_cpu_seconds_p50": round(statistics.median([item["process_tree_cpu_seconds"] for item in samples if item["process_tree_cpu_seconds"] is not None]), 3) if any(item["process_tree_cpu_seconds"] is not None for item in samples) else None,
                    "process_tree_io_read_bytes_max": max((item["process_tree_io_read_bytes"] for item in samples if item["process_tree_io_read_bytes"] is not None), default=None),
                    "process_tree_io_write_bytes_max": max((item["process_tree_io_write_bytes"] for item in samples if item["process_tree_io_write_bytes"] is not None), default=None),
                    "output_bytes_max": max(item["output_bytes"] for item in samples),
                    "output_sha256_unique_count": len(set(output_hashes)),
                    "scope_claim": "report_generation_only",
                }
                report["workloads"].append(row)
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"saved {len(report['workloads'])} verified report-generation workloads: {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
