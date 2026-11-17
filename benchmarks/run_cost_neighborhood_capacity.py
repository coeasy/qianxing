#!/usr/bin/env python3
"""Measure the real CLI cost of adaptive-search fee-neighborhood replays.

The workload uses a synthetic crypto-spot BarFrame and the production `run-experiment`
path. It verifies that every one-dimensional radius-1 neighbor is stress-replayed and
records deterministic candidate identities plus process-tree resource measurements.
It does not represent a supplier or complete product profile.
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
from typing import Any

from hardware import hardware_fingerprint
from process_metrics import run_measured
from run_adaptive_search_capacity import experiment_spec
from run_profile_backtest import find_binary, generate_frame, load_fixture
from source_fingerprint import MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA, measurement_source_fingerprint

ROOT = Path(__file__).resolve().parent.parent


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def percentile(values: list[float], percent: float) -> float:
    ordered = sorted(values)
    index = min(len(ordered) - 1, round(percent * (len(ordered) - 1)))
    return round(ordered[index], 3)


def run_workload(binary: Path, spec: dict[str, Any], expected_budget: int) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="qx-cost-neighborhood-spec-", dir=ROOT / "target") as temp:
        spec_path = Path(temp) / "experiment.json"
        spec_path.write_text(json.dumps(spec, separators=(",", ":")), encoding="utf-8")
        measured = run_measured(
            [str(binary), "app", "run-experiment", str(spec_path)], cwd=ROOT
        )
    if measured.return_code != 0:
        raise RuntimeError(
            f"cost-neighborhood experiment failed (exit={measured.return_code}): "
            f"{measured.stderr[-2000:]}"
        )
    payload = json.loads(measured.stdout)
    trials = payload.get("search", {}).get("trials", [])
    candidates = payload.get("candidates", [])
    if len(trials) != expected_budget or len(candidates) != expected_budget:
        raise RuntimeError(
            f"expected {expected_budget} search candidates, got trials={len(trials)} "
            f"and candidate_results={len(candidates)}"
        )
    scenario_results = payload.get("cost_sensitivity", [])
    if len(scenario_results) != 1:
        raise RuntimeError(f"expected one fee-stress scenario, got {len(scenario_results)}")
    scenario = scenario_results[0]
    neighborhood = scenario.get("neighborhood_cost_robustness")
    if not isinstance(neighborhood, dict):
        raise RuntimeError("the declared fee-neighborhood gate was not reported")
    neighbors = neighborhood.get("candidates", [])
    possible = neighborhood.get("possible_neighbors")
    if (
        neighborhood.get("radius") != 1
        or neighborhood.get("complete") is not True
        or neighborhood.get("evaluated_neighbors") != possible
        or neighborhood.get("successful_neighbors") != possible
        or len(neighbors) != possible
        or any(not item.get("outcome") or item.get("error") for item in neighbors)
    ):
        raise RuntimeError(
            "fee-neighborhood replay did not cover every theoretical neighbor: "
            f"{json.dumps(neighborhood, ensure_ascii=False)[:1200]}"
        )
    center_outcome = scenario.get("outcome")
    if not center_outcome or scenario.get("error"):
        raise RuntimeError("selected candidate fee-stress replay did not succeed")

    result_hashes = [
        candidate["outcome"]["result_hash"]
        for candidate in candidates
        if candidate.get("outcome") is not None
    ]
    stress_hashes = [center_outcome["result_hash"]] + [
        item["outcome"]["result_hash"] for item in neighbors
    ]
    if len(result_hashes) != expected_budget or not all(stress_hashes):
        raise RuntimeError("one or more baseline/stress replays omitted a result hash")
    return {
        "wall_ms": round(measured.elapsed_ms, 3),
        "peak_rss_bytes": measured.peak_rss_bytes,
        "process_tree_peak_rss_bytes": measured.peak_tree_rss_bytes,
        "peak_process_count": measured.peak_process_count,
        "process_tree_cpu_seconds": measured.tree_cpu_seconds,
        "process_tree_io_read_bytes": measured.tree_io_read_bytes,
        "process_tree_io_write_bytes": measured.tree_io_write_bytes,
        "stdout_bytes": len(measured.stdout.encode("utf-8")),
        "proposal_trace_sha256": sha256_bytes(
            json.dumps(trials, sort_keys=True, separators=(",", ":")).encode("utf-8")
        ),
        "baseline_result_hashes": result_hashes,
        "stress_result_hashes": stress_hashes,
        "selected_ordinal": scenario.get("selected_ordinal"),
        "neighborhood": {
            "radius": neighborhood["radius"],
            "possible_neighbors": possible,
            "evaluated_neighbors": neighborhood["evaluated_neighbors"],
            "successful_neighbors": neighborhood["successful_neighbors"],
            "passed_neighbors": neighborhood["passed_neighbors"],
            "complete": neighborhood["complete"],
            "passed": neighborhood["passed"],
        },
        "candidate_count": len(candidates),
        "total_backtest_replays": len(candidates) + 1 + len(neighbors),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary")
    parser.add_argument("--sizes", default="8000,64000")
    parser.add_argument("--budget", type=int, default=8)
    parser.add_argument("--repeats", type=int, default=2)
    parser.add_argument(
        "--out",
        type=Path,
        default=ROOT / "maturity" / "evidence" / "adaptive-search-cost-neighborhood-capacity-2026-10-23.json",
    )
    args = parser.parse_args()
    sizes = [int(value) for value in args.sizes.split(",") if value.strip()]
    if not sizes or any(value < 100 for value in sizes) or len(sizes) != len(set(sizes)):
        parser.error("--sizes 必须是互不重复且至少为 100 的正整数")
    if not 5 <= args.budget <= 64:
        parser.error("--budget 必须位于 5..=64")
    if args.repeats < 2:
        parser.error("--repeats 至少为 2")

    binary = find_binary(args.binary)
    market, identity = load_fixture("crypto-spot")
    report: dict[str, Any] = {
        "schema_version": 1,
        "evidence_kind": "verified_adaptive_search_cost_neighborhood_capacity",
        "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "hardware": hardware_fingerprint(),
        "binary": str(binary),
        "binary_sha256": sha256_file(binary),
        "measurement_source_fingerprint_schema": MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA,
        "measurement_source_fingerprint": measurement_source_fingerprint(ROOT),
        "benchmark_script_sha256": sha256_file(Path(__file__).resolve()),
        "scenario": "seeded finite-grid TPE-KDE plus complete radius-1 fee-stress neighborhood replays",
        "profile_coverage_claim": False,
        "profile_target_statuses_updated": False,
        "resource_measurement": {
            "peak_rss_field": "peak_rss_bytes",
            "process_tree_peak_rss_field": "process_tree_peak_rss_bytes",
            "process_tree_method": "benchmarks/process_metrics.py OS process counters and optional psutil 10ms tree sampling",
            "cpu_and_io": "process-tree CPU seconds and OS read/write bytes when exposed; null remains unavailable",
        },
        "limitations": [
            "synthetic crypto-spot Bars and one builtin SMA strategy",
            "finite fast_window grid; no floating-point parameter space",
            "one fee-stress scenario, one parameter dimension, radius 1, one process",
            "short-process sampling; not a full research, supplier, or product profile",
        ],
        "workloads": [],
    }
    with tempfile.TemporaryDirectory(prefix="qx-cost-neighborhood-cap-", dir=ROOT / "target") as temp:
        workdir = Path(temp)
        for size in sizes:
            frame = workdir / f"crypto-spot-{size}.bars.json"
            generate_frame(size, "crypto-spot", market, identity, frame)
            samples: list[dict[str, Any]] = []
            for repeat in range(args.repeats):
                experiment_id = f"cost-neighborhood-cap-{size}-{args.budget}"
                spec = experiment_spec(
                    experiment_id,
                    frame,
                    workdir / f"out-{size}-{repeat + 1}",
                    size,
                    args.budget,
                    seed=20261023,
                )
                spec["search"]["neighborhood_radius"] = 1
                spec["cost_sensitivity"] = [
                    {
                        "name": "fee-stress-neighborhood",
                        "execution_cost_rules": {
                            "name": "fee-stress",
                            "maker_bp": 20,
                            "taker_bp": 100,
                            "latency_base_ns": 0,
                            "latency_insert_ns": 0,
                        },
                        "max_training_return_degradation_bps": 10_000,
                        "max_neighborhood_training_return_degradation_bps": 10_000,
                        "fill_model": "best_price",
                    }
                ]
                samples.append(run_workload(binary, spec, args.budget))
            proposals = {sample["proposal_trace_sha256"] for sample in samples}
            baseline_results = {tuple(sample["baseline_result_hashes"]) for sample in samples}
            stress_results = {tuple(sample["stress_result_hashes"]) for sample in samples}
            if len(proposals) != 1 or len(baseline_results) != 1 or len(stress_results) != 1:
                raise RuntimeError(f"nondeterministic fee-neighborhood replay for {size} Bars")
            walls = [sample["wall_ms"] for sample in samples]
            cpu_samples = [
                sample["process_tree_cpu_seconds"]
                for sample in samples
                if sample["process_tree_cpu_seconds"] is not None
            ]
            report["workloads"].append(
                {
                    "bars": size,
                    "candidate_budget": args.budget,
                    "algorithm": "tpe_kde",
                    "search_space": "finite_fast_window_grid",
                    "fee_scenario_count": 1,
                    "repeats": args.repeats,
                    "candidate_count": args.budget,
                    "total_backtest_replays_per_repeat": samples[-1]["total_backtest_replays"],
                    "selected_ordinal": samples[-1]["selected_ordinal"],
                    "neighborhood": samples[-1]["neighborhood"],
                    "proposal_trace_sha256": samples[-1]["proposal_trace_sha256"],
                    "baseline_result_hashes_unique_count": len(set(samples[-1]["baseline_result_hashes"])),
                    "stress_result_hashes_unique_count": len(set(samples[-1]["stress_result_hashes"])),
                    "repeat_proposals_stable": len(proposals) == 1,
                    "repeat_baseline_results_stable": len(baseline_results) == 1,
                    "repeat_stress_results_stable": len(stress_results) == 1,
                    "wall_p50_ms": round(statistics.median(walls), 3),
                    "wall_p95_ms": percentile(walls, 0.95),
                    "process_tree_peak_rss_bytes_max": max(
                        (sample["process_tree_peak_rss_bytes"] for sample in samples if sample["process_tree_peak_rss_bytes"] is not None),
                        default=None,
                    ),
                    "peak_process_count": max(sample["peak_process_count"] for sample in samples),
                    "process_tree_cpu_seconds_p50": round(statistics.median(cpu_samples), 3)
                    if cpu_samples
                    else None,
                    "process_tree_io_read_bytes_max": max(
                        (sample["process_tree_io_read_bytes"] for sample in samples if sample["process_tree_io_read_bytes"] is not None),
                        default=None,
                    ),
                    "process_tree_io_write_bytes_max": max(
                        (sample["process_tree_io_write_bytes"] for sample in samples if sample["process_tree_io_write_bytes"] is not None),
                        default=None,
                    ),
                    "scope_claim": "adaptive_search_fee_neighborhood_only",
                }
            )

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"saved {len(report['workloads'])} verified cost-neighborhood workloads: {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
