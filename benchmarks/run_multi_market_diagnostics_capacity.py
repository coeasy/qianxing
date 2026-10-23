#!/usr/bin/env python3
"""Measure verified multi-market Bar diagnosis and offline report generation.

This benchmark prepares a shared-account multi-market run once, then measures the real
``strategy diagnose-multi-bar --html`` path. Inputs are synthetic crypto-spot BarFrames and
the recorded resource profile does not claim a complete product profile.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import statistics
import tempfile
import time
from pathlib import Path
from typing import Any

from hardware import hardware_fingerprint
from process_metrics import run_measured
from run_profile_backtest import (
    SCALE,
    find_binary,
    generate_frame,
    load_fixture,
)
from source_fingerprint import MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA, measurement_source_fingerprint

ROOT = Path(__file__).resolve().parent.parent
INSTRUMENTS = (
    "BTCUSDT.BINANCE",
    "ETHUSDT.BINANCE",
    "SOLUSDT.BINANCE",
    "ADAUSDT.BINANCE",
    "XRPUSDT.BINANCE",
    "DOGEUSDT.BINANCE",
    "LINKUSDT.BINANCE",
    "BNBUSDT.BINANCE",
)


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def percentile(values: list[float], percent: float) -> float:
    ordered = sorted(values)
    return round(ordered[min(len(ordered) - 1, round(percent * (len(ordered) - 1)))], 3)


def prepare_run(binary: Path, size: int, market_count: int, workdir: Path) -> dict[str, Any]:
    fixture = "crypto-spot"
    market_template, identity_template = load_fixture(fixture)
    initial_capital_raw = 100_000_000_000_000
    reserved_capital_raw = initial_capital_raw // market_count
    markets = []
    input_paths = []
    quantity = max(1, (int(market_template["min_qty"]) + SCALE - 1) // SCALE)
    for index, instrument in enumerate(INSTRUMENTS[:market_count]):
        identity = {**identity_template, "instrument": instrument}
        frame_path = workdir / f"market-{index}-{size}.bars.json"
        generate_frame(size, fixture, market_template, identity, frame_path)
        input_paths.append(frame_path)
        markets.append(
            {
                "instrument": instrument,
                "bars_path": str(frame_path),
                "priority": index,
                "market_spec": None,
                "fee_schedule": None,
                "strategies": [
                    {
                        "priority": 0,
                        "reserved_capital_raw": reserved_capital_raw,
                        "strategy": {
                            "kind": "sma_cross",
                            "strategy_id": f"diag-cap-{index}",
                            "quantity_raw": quantity * SCALE,
                            "fast_window": 3,
                            "slow_window": 8,
                        },
                    }
                ],
            }
        )
    output_dir = workdir / "runs"
    spec_path = workdir / "multi-market.json"
    spec_path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "run_id": f"multi-market-diagnostics-cap-{market_count}-{size}",
                "account_id": "diagnostics-capacity-account",
                "settlement_currency": "USDT",
                "initial_cash_raw": initial_capital_raw,
                "seed": 17,
                "output_dir": str(output_dir),
                "markets": markets,
            },
            separators=(",", ":"),
        ),
        encoding="utf-8",
    )
    run = run_measured(
        [str(binary), "app", "multi-market-backtest", str(spec_path)], cwd=ROOT
    )
    if run.return_code != 0:
        raise RuntimeError(
            f"multi-market setup failed (markets={market_count}, bars={size}, "
            f"exit={run.return_code}): {run.stderr[-2_000:]}"
        )
    outcome = json.loads(run.stdout)
    summary = Path(outcome["artifact_path"]).resolve()
    if not summary.is_file() or int(outcome["markets_total"]) != market_count:
        raise RuntimeError("multi-market setup omitted the expected verified summary")
    return {
        "summary": summary,
        "spec": spec_path,
        "input_paths": input_paths,
        "setup_result_hash": str(outcome["shared_result_hash"]),
        "setup_wall_ms": round(run.elapsed_ms, 3),
    }


def measure_diagnostics(
    binary: Path, summary: Path, expected_markets: int, expected_bars: int, output: Path
) -> dict[str, Any]:
    measured = run_measured(
        [
            str(binary),
            "strategy",
            "diagnose-multi-bar",
            str(summary),
            "--output",
            str(output),
            "--html",
        ],
        cwd=ROOT,
    )
    if measured.return_code != 0:
        raise RuntimeError(
            f"multi-market diagnostics failed (exit={measured.return_code}): "
            f"{measured.stderr[-2_000:]}"
        )
    diagnostics = json.loads(output.read_text(encoding="utf-8"))
    if not isinstance(diagnostics, list) or len(diagnostics) != expected_markets:
        raise RuntimeError("diagnostics output omitted a declared strategy slot")
    if any(int(item.get("input_bars", -1)) != expected_bars for item in diagnostics):
        raise RuntimeError("diagnostics input coverage differs from the prepared BarFrames")
    report_name = summary.name.replace(".multi-market.json", ".multi-market.report.html")
    html = summary.with_name(report_name)
    if not html.is_file():
        raise RuntimeError("diagnostics command omitted the offline HTML report")
    html_bytes = html.read_bytes()
    html_text = html_bytes.decode("utf-8")
    if "strategyDiagnostics" not in html_text or "按标的权益贡献" not in html_text:
        raise RuntimeError("multi-market HTML report omitted diagnosis or contribution sections")
    payload = output.read_bytes()
    return {
        "wall_ms": round(measured.elapsed_ms, 3),
        "peak_rss_bytes": measured.peak_rss_bytes,
        "process_tree_peak_rss_bytes": measured.peak_tree_rss_bytes,
        "peak_process_count": measured.peak_process_count,
        "process_tree_cpu_seconds": measured.tree_cpu_seconds,
        "process_tree_io_read_bytes": measured.tree_io_read_bytes,
        "process_tree_io_write_bytes": measured.tree_io_write_bytes,
        "diagnostic_output_bytes": len(payload),
        "diagnostic_output_sha256": sha256_bytes(payload),
        "html_output_bytes": len(html_bytes),
        "html_output_sha256": sha256_bytes(html_bytes),
        "lookahead_statuses": sorted(
            str(item.get("lookahead", {}).get("status", "missing")) for item in diagnostics
        ),
        "startup_statuses": sorted(
            str(item.get("startup", {}).get("status", "missing")) for item in diagnostics
        ),
        "pit_statuses": sorted(
            str(item.get("data_visibility", {}).get("status", "missing")) for item in diagnostics
        ),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary")
    parser.add_argument("--sizes", default="1000,8000,64000")
    parser.add_argument("--markets", default="3,4,8")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument(
        "--out",
        type=Path,
        default=ROOT / "maturity" / "evidence" / "multi-market-diagnostics-capacity-2026-10-23.json",
    )
    args = parser.parse_args()
    sizes = [int(value) for value in args.sizes.split(",") if value.strip()]
    market_counts = [int(value) for value in args.markets.split(",") if value.strip()]
    if not sizes or any(size < 3 or size > 65_536 for size in sizes) or len(sizes) != len(set(sizes)):
        parser.error("--sizes 必须是互不重复且落在 3..=65536 的正整数")
    if not market_counts or any(count < 3 or count > len(INSTRUMENTS) for count in market_counts):
        parser.error(f"--markets 必须落在 3..={len(INSTRUMENTS)}")
    if len(market_counts) != len(set(market_counts)):
        parser.error("--markets 不能重复")
    if args.repeats < 2:
        parser.error("--repeats 至少为 2")
    for market_count in market_counts:
        if max(sizes) * market_count > 1_000_000:
            parser.error("每个 workload 的市场数 × Bar 数不能超过诊断总事件预算 1,000,000")

    binary = find_binary(args.binary)
    evidence: dict[str, Any] = {
        "schema_version": 1,
        "evidence_kind": "verified_multi_market_strategy_diagnostics_capacity",
        "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "hardware": hardware_fingerprint(),
        "binary": str(binary),
        "binary_sha256": sha256_file(binary),
        "measurement_source_fingerprint_schema": MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA,
        "measurement_source_fingerprint": measurement_source_fingerprint(ROOT),
        "benchmark_script_sha256": sha256_file(Path(__file__).resolve()),
        "scenario": "verified shared-account multi-market Bar backtest followed by qx-cli strategy diagnose-multi-bar --html",
        "profile_coverage_claim": False,
        "profile_target_statuses_updated": False,
        "workload_contract": {
            "input": "synthetic crypto-spot BarFrames with one Rust built-in SMA strategy slot per market",
            "measured_stage": "diagnose-multi-bar process, including artifact/input identity checks, replay verification, strategy replays, JSON and offline HTML generation",
            "setup_stage": "the multi-market backtest is prepared once per workload and its elapsed time is recorded separately",
            "fixed_diagnostic_context": "declared reserved capital and empty positions; not event-time account-state reconstruction",
            "pit": "visibility_unknown without an independently verified provider revision/publication ledger",
        },
        "resource_measurement": {
            "process_tree_peak_rss": "OS process counters plus optional psutil 10ms process-tree sampling",
            "cpu_and_io": "process-tree counters when exposed by the host; null remains unavailable",
        },
        "limitations": [
            "synthetic crypto-spot Bar data and built-in SMA strategy slots",
            "one Rust strategy slot per market; no external strategy or dynamic sizing",
            "fixed cash/empty positions diagnosis context does not reconstruct shared account state at each event",
            "does not measure private supplier traffic, PIT publication history, Paper workers, long-running stability, or full product profiles",
            "short-process process-tree sampling may miss transient RSS peaks",
        ],
        "workloads": [],
    }
    with tempfile.TemporaryDirectory(prefix="qx-multi-market-diagnostics-cap-", dir=ROOT / "target") as temp:
        root = Path(temp)
        for size in sizes:
            for market_count in market_counts:
                workdir = root / f"bars-{size}-markets-{market_count}"
                workdir.mkdir()
                prepared = prepare_run(binary, size, market_count, workdir)
                summary = prepared["summary"]
                samples = []
                for repeat in range(args.repeats):
                    output = workdir / f"diagnostics-{repeat + 1}.json"
                    samples.append(
                        measure_diagnostics(binary, summary, market_count, size, output)
                    )
                diagnostic_hashes = {item["diagnostic_output_sha256"] for item in samples}
                html_hashes = {item["html_output_sha256"] for item in samples}
                if len(diagnostic_hashes) != 1 or len(html_hashes) != 1:
                    raise RuntimeError(
                        f"workload output hashes changed across repeats: "
                        f"markets={market_count}, bars={size}"
                    )
                primary_verified_input_bytes = sum(
                    path.stat().st_size for path in prepared["input_paths"]
                )
                primary_verified_input_bytes += (
                    prepared["spec"].stat().st_size + summary.stat().st_size
                )
                workload = {
                    "markets": market_count,
                    "strategy_slots": market_count,
                    "bars_per_market": size,
                    "total_strategy_bar_events": market_count * size,
                    "repeats": args.repeats,
                    "setup_backtest_wall_ms": prepared["setup_wall_ms"],
                    "setup_result_hash": prepared["setup_result_hash"],
                    "verified_summary_sha256": sha256_file(summary),
                    "primary_verified_input_bytes": primary_verified_input_bytes,
                    "diagnostic_wall_p50_ms": round(
                        statistics.median(item["wall_ms"] for item in samples), 3
                    ),
                    "diagnostic_wall_p95_ms": percentile(
                        [item["wall_ms"] for item in samples], 0.95
                    ),
                    "peak_rss_bytes_max": max(
                        (item["peak_rss_bytes"] for item in samples if item["peak_rss_bytes"] is not None),
                        default=None,
                    ),
                    "process_tree_peak_rss_bytes_max": max(
                        (
                            item["process_tree_peak_rss_bytes"]
                            for item in samples
                            if item["process_tree_peak_rss_bytes"] is not None
                        ),
                        default=None,
                    ),
                    "peak_process_count": max(item["peak_process_count"] for item in samples),
                    "process_tree_cpu_seconds_p50": round(
                        statistics.median(
                            [
                                item["process_tree_cpu_seconds"]
                                for item in samples
                                if item["process_tree_cpu_seconds"] is not None
                            ]
                        ),
                        3,
                    )
                    if any(item["process_tree_cpu_seconds"] is not None for item in samples)
                    else None,
                    "diagnostic_output_bytes": samples[0]["diagnostic_output_bytes"],
                    "html_output_bytes": samples[0]["html_output_bytes"],
                    "diagnostic_output_sha256": samples[0]["diagnostic_output_sha256"],
                    "html_output_sha256": samples[0]["html_output_sha256"],
                    "lookahead_statuses": samples[0]["lookahead_statuses"],
                    "startup_statuses": samples[0]["startup_statuses"],
                    "pit_statuses": samples[0]["pit_statuses"],
                    "repeated_samples": samples,
                }
                evidence["workloads"].append(workload)
                print(
                    f"{market_count} markets x {size:,} Bars: "
                    f"diagnostics P50/P95 {workload['diagnostic_wall_p50_ms']}/"
                    f"{workload['diagnostic_wall_p95_ms']} ms, "
                    f"tree RSS {workload['process_tree_peak_rss_bytes_max']} bytes"
                )
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"evidence: {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
