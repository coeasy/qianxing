#!/usr/bin/env python3
"""Measure bounded adaptive parameter-search cost on verified synthetic Bar inputs.

This is a local crypto-spot proxy, not a full research or production profile. Each
workload runs the real CLI experiment use case and records candidate, CPU, memory,
I/O and deterministic replay evidence.
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
from run_profile_backtest import SCALE, find_binary, generate_frame, load_fixture
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


def experiment_spec(
    experiment_id: str,
    frame: Path,
    output_dir: Path,
    bars: int,
    budget: int,
    seed: int,
    continuous: bool = False,
    robustness_subwindows: int | None = None,
    early_pruning_drawdown_bps: int | None = None,
    early_pruning_min_training_bars: int = 64,
) -> dict:
    fast_values = list(range(2, budget + 2))
    slow_window = 1024 if continuous else budget + 8
    return {
        "schema_version": 1,
        "experiment_id": experiment_id,
        "base": {
            "schema_version": 1,
            "run_id": f"{experiment_id}-base",
            "instrument": "BTCUSDT.BINANCE",
            "bars_path": str(frame),
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000 * SCALE,
            "seed": seed,
            "output_dir": str(output_dir),
            "strategy": {
                "kind": "sma_cross",
                "strategy_id": "adaptive-search-capacity",
                "quantity_raw": SCALE,
                "fast_window": fast_values[0],
                "slow_window": slow_window,
            },
        },
        "parameter_space": [] if continuous else [{"name": "fast_window", "values": fast_values}],
        "split_plan": {
            "train_bars": bars * 2 // 3,
            "test_bars": bars - bars * 2 // 3,
            "walk_forward": False,
            "step_bars": 0,
            "purge_bars": 0,
            "embargo_bars": 0,
        },
        "search": {
            "algorithm": "tpe_kde",
            "seed": seed,
            "budget": budget,
            **(
                {"robustness_subwindows": robustness_subwindows}
                if robustness_subwindows is not None
                else {}
            ),
            **(
                {
                    "max_training_drawdown_bps": early_pruning_drawdown_bps,
                    "early_pruning": {
                        "min_training_bars": early_pruning_min_training_bars
                    },
                }
                if early_pruning_drawdown_bps is not None
                else {}
            ),
            **(
                {
                    "continuous_parameter_space": [
                        {"name": "fast_window", "min": 2, "max": 1000, "step": 1}
                    ]
                }
                if continuous
                else {}
            ),
        },
    }


def run_workload(binary: Path, spec: dict, expected_budget: int) -> dict:
    with tempfile.TemporaryDirectory(prefix="qx-search-spec-", dir=ROOT / "target") as temp:
        spec_path = Path(temp) / "experiment.json"
        spec_path.write_text(json.dumps(spec, separators=(",", ":")), encoding="utf-8")
        measured = run_measured(
            [str(binary), "app", "run-experiment", str(spec_path)], cwd=ROOT
        )
    if measured.return_code != 0:
        raise RuntimeError(
            f"adaptive search failed (exit={measured.return_code}): {measured.stderr[-2000:]}"
        )
    payload = json.loads(measured.stdout)
    trials = payload.get("search", {}).get("trials", [])
    if len(trials) != expected_budget:
        raise RuntimeError(f"expected {expected_budget} search trials, received {len(trials)}")
    candidate_results = payload.get("candidates", [])
    accounted = sum(
        candidate.get("outcome") is not None
        or candidate.get("error") is not None
        or candidate.get("pruning") is not None
        for candidate in candidate_results
    )
    if accounted != expected_budget:
        raise RuntimeError(
            f"candidate accounting mismatch: expected {expected_budget}, accounted for {accounted}"
        )
    expected_subwindows = spec["search"].get("robustness_subwindows")
    robust_scores = [trial.get("training_robustness_score_bps") for trial in trials]
    if expected_subwindows is None and any(score is not None for score in robust_scores):
        raise RuntimeError("legacy search unexpectedly returned a robustness score")
    if expected_subwindows is not None and any(score is None for score in robust_scores):
        raise RuntimeError("robust search omitted a training-window score")
    outcomes = [candidate.get("outcome") for candidate in payload.get("candidates", [])]
    result_hashes = [item["result_hash"] for item in outcomes if item]
    return {
        "wall_ms": round(measured.elapsed_ms, 3),
        "peak_rss_bytes": measured.peak_rss_bytes,
        "process_tree_peak_rss_bytes": measured.peak_tree_rss_bytes,
        "peak_process_count": measured.peak_process_count,
        "process_tree_cpu_seconds": measured.tree_cpu_seconds,
        "process_tree_io_read_bytes": measured.tree_io_read_bytes,
        "process_tree_io_write_bytes": measured.tree_io_write_bytes,
        "stdout_bytes": len(measured.stdout.encode("utf-8")),
        "report_sha256": sha256_bytes(measured.stdout.encode("utf-8")),
        "completed_candidates": payload["completed_candidates"],
        "succeeded_candidates": payload["succeeded_candidates"],
        "failed_candidates": payload["failed_candidates"],
        "pruned_candidates": payload.get("pruned_candidates", 0),
        "pruned_bars_saved": sum(
            max(0, spec["split_plan"]["train_bars"] - int(candidate["pruning"]["processed_bars"]))
            for candidate in candidate_results
            if candidate.get("pruning") is not None
        ),
        "pruning_trace_sha256": sha256_bytes(
            json.dumps(
                [candidate.get("pruning") for candidate in candidate_results],
                sort_keys=True,
                separators=(",", ":"),
            ).encode("utf-8")
        ),
        "candidate_result_hashes": result_hashes,
        "proposal_trace_sha256": sha256_bytes(
            json.dumps(trials, sort_keys=True, separators=(",", ":")).encode("utf-8")
        ),
        "training_robustness_scores_bps": robust_scores,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary")
    parser.add_argument(
        "--space",
        choices=("finite", "continuous"),
        default="finite",
        help="measure the legacy finite grid or the adaptive stepped-integer domain",
    )
    parser.add_argument("--sizes", default="8000,64000")
    parser.add_argument("--budgets", default="8,32,64")
    parser.add_argument(
        "--robustness-subwindows",
        type=int,
        help="select TPE candidates by worst training subwindow return (2..=8)",
    )
    parser.add_argument(
        "--early-pruning-drawdown-bps",
        type=int,
        help="enable training-window drawdown pruning at this hard budget (0..=10000 bps)",
    )
    parser.add_argument(
        "--early-pruning-min-training-bars",
        type=int,
        default=64,
        help="minimum processed training bars before checking the drawdown budget",
    )
    parser.add_argument("--repeats", type=int, default=2)
    parser.add_argument(
        "--out",
        type=Path,
        default=ROOT / "maturity" / "evidence" / "adaptive-search-capacity-2026-10-23.json",
    )
    args = parser.parse_args()
    sizes = [int(value) for value in args.sizes.split(",") if value.strip()]
    budgets = [int(value) for value in args.budgets.split(",") if value.strip()]
    if not sizes or any(value < 100 for value in sizes) or len(sizes) != len(set(sizes)):
        parser.error("--sizes 必须是互不重复且至少为 100 的正整数")
    if not budgets or any(value < 5 or value > 256 for value in budgets) or len(budgets) != len(set(budgets)):
        parser.error("--budgets 必须是互不重复且位于 5..=256 的整数")
    if args.repeats < 2:
        parser.error("--repeats 至少为 2")
    if args.robustness_subwindows is not None and not 2 <= args.robustness_subwindows <= 8:
        parser.error("--robustness-subwindows 必须位于 2..=8")
    if args.robustness_subwindows is not None and any(
        size * 2 // 3 < args.robustness_subwindows + 1 for size in sizes
    ):
        parser.error("训练 Bar 数必须至少为 robustness-subwindows + 1")
    if args.early_pruning_drawdown_bps is not None and not 0 <= args.early_pruning_drawdown_bps <= 10_000:
        parser.error("--early-pruning-drawdown-bps 必须位于 0..=10000")
    if args.early_pruning_min_training_bars < 2:
        parser.error("--early-pruning-min-training-bars 至少为 2")
    if args.early_pruning_drawdown_bps is None and args.early_pruning_min_training_bars != 64:
        parser.error("只有启用 --early-pruning-drawdown-bps 时才能调整早停最小 Bar 数")

    binary = find_binary(args.binary)
    report = {
        "schema_version": 1,
        "evidence_kind": "verified_adaptive_search_capacity",
        "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "hardware": hardware_fingerprint(),
        "binary": str(binary),
        "binary_sha256": sha256_file(binary),
        "measurement_source_fingerprint_schema": MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA,
        "measurement_source_fingerprint": measurement_source_fingerprint(ROOT),
        "benchmark_script_sha256": sha256_file(Path(__file__).resolve()),
        "scenario": (
            "qx-cli app run-experiment using seeded TPE-KDE over an adaptive stepped-integer fast_window range"
            if args.space == "continuous"
            else "qx-cli app run-experiment using seeded TPE-KDE over a finite fast_window grid"
        ),
        "objective": (
            {"kind": "worst_training_subwindow_return", "subwindows": args.robustness_subwindows}
            if args.robustness_subwindows is not None
            else {"kind": "full_training_return"}
        ),
        "input_profile": (
            "synthetic rise-then-decline path designed to exercise training drawdown pruning"
            if args.early_pruning_drawdown_bps is not None
            else "existing bounded synthetic crypto-spot SMA fixture"
        ),
        "early_pruning": (
            {
                "max_training_drawdown_bps": args.early_pruning_drawdown_bps,
                "min_training_bars": args.early_pruning_min_training_bars,
                "enabled": True,
            }
            if args.early_pruning_drawdown_bps is not None
            else {"enabled": False}
        ),
        "replay_verified": True,
        "profile_coverage_claim": False,
        "profile_target_statuses_updated": False,
        "resource_measurement": {
            "peak_rss_field": "peak_rss_bytes",
            "process_tree_peak_rss_field": "process_tree_peak_rss_bytes",
            "process_tree_method": "benchmarks/process_metrics.py OS process counters and optional psutil 10ms tree sampling",
            "cpu_and_io": "process-tree CPU seconds and OS read/write bytes when the platform exposes them; null remains unavailable",
        },
        "limitations": [
            "synthetic crypto spot Bar input and one builtin SMA strategy",
            (
                "bounded stepped-integer fast_window range; does not measure floating-point spaces"
                if args.space == "continuous"
                else "finite ordered fast_window grid; does not measure continuous-valued spaces"
            ),
            (
                "measures early pruning only under the declared monotone training drawdown budget"
                if args.early_pruning_drawdown_bps is not None
                else "does not measure early-pruning behavior"
            ),
            "training robustness subwindows are confined to the training interval; no OOS bars are used by this objective",
            "single process and non-walk-forward training/test split; does not measure parallel search",
            "short-process RSS and CPU/I/O counters can be unavailable or miss brief peaks",
        ],
        "workloads": [],
    }
    fixture = "crypto-spot"
    market, identity = load_fixture(fixture)
    with tempfile.TemporaryDirectory(prefix="qx-search-cap-", dir=ROOT / "target") as temp:
        workdir = Path(temp)
        for size in sizes:
            frame = workdir / f"{fixture}-{size}.bars.json"
            start = time.perf_counter()
            generate_frame(
                size,
                fixture,
                market,
                identity,
                frame,
                drawdown_stress=args.early_pruning_drawdown_bps is not None,
            )
            generation_ms = round((time.perf_counter() - start) * 1_000, 3)
            for budget in budgets:
                experiment_id = f"search-cap-{size}-{budget}"
                samples = []
                baseline_samples = []
                for repeat in range(args.repeats):
                    output_dir = workdir / f"out-{size}-{budget}-{repeat + 1}"
                    spec = experiment_spec(
                        experiment_id,
                        frame,
                        output_dir,
                        size,
                        budget,
                        seed=20261022,
                        continuous=args.space == "continuous",
                        robustness_subwindows=args.robustness_subwindows,
                        early_pruning_drawdown_bps=args.early_pruning_drawdown_bps,
                        early_pruning_min_training_bars=args.early_pruning_min_training_bars,
                    )
                    if args.early_pruning_drawdown_bps is not None:
                        baseline_spec = experiment_spec(
                            experiment_id,
                            frame,
                            workdir / f"baseline-{size}-{budget}-{repeat + 1}",
                            size,
                            budget,
                            seed=20261022,
                            continuous=args.space == "continuous",
                            robustness_subwindows=args.robustness_subwindows,
                        )
                        if repeat % 2 == 0:
                            baseline_samples.append(
                                run_workload(binary, baseline_spec, budget)
                            )
                            samples.append(run_workload(binary, spec, budget))
                        else:
                            samples.append(run_workload(binary, spec, budget))
                            baseline_samples.append(
                                run_workload(binary, baseline_spec, budget)
                            )
                    else:
                        samples.append(run_workload(binary, spec, budget))
                trace_hashes = {sample["proposal_trace_sha256"] for sample in samples}
                candidate_hashes = {
                    tuple(sample["candidate_result_hashes"]) for sample in samples
                }
                pruning_hashes = {sample["pruning_trace_sha256"] for sample in samples}
                if args.early_pruning_drawdown_bps is not None and not any(
                    sample["pruned_candidates"] for sample in samples
                ):
                    raise RuntimeError(
                        f"early-pruning workload exercised no pruning (N={size}, budget={budget})"
                    )
                baseline_replay = {
                    (sample["proposal_trace_sha256"], tuple(sample["candidate_result_hashes"]))
                    for sample in baseline_samples
                }
                if (
                    len(trace_hashes) != 1
                    or len(candidate_hashes) != 1
                    or len(pruning_hashes) != 1
                    or len(baseline_replay) > 1
                ):
                    raise RuntimeError(
                        f"search replay was not deterministic (N={size}, budget={budget})"
                    )
                pruned_wall_p50 = round(
                    statistics.median(item["wall_ms"] for item in samples), 3
                )
                baseline_wall_p50 = (
                    round(statistics.median(item["wall_ms"] for item in baseline_samples), 3)
                    if baseline_samples
                    else None
                )
                report["workloads"].append(
                    {
                        "bars": size,
                        "candidate_budget": budget,
                        "algorithm": "tpe_kde",
                        "search_space": args.space,
                        "objective": report["objective"],
                        "repeats": args.repeats,
                        "input_generation_ms": generation_ms,
                        "candidate_count_per_repeat": budget,
                        "succeeded_candidates": samples[-1]["succeeded_candidates"],
                        "failed_candidates": samples[-1]["failed_candidates"],
                        "pruned_candidates": samples[-1]["pruned_candidates"],
                        "pruned_bars_saved": samples[-1]["pruned_bars_saved"],
                        "pruning_trace_sha256": samples[-1]["pruning_trace_sha256"],
                        "proposal_trace_sha256": samples[-1]["proposal_trace_sha256"],
                        "candidate_result_hashes_unique_count": len(set(samples[-1]["candidate_result_hashes"])),
                        "training_robustness_scores_bps": samples[-1]["training_robustness_scores_bps"],
                        "wall_p50_ms": pruned_wall_p50,
                        "unpruned_baseline_wall_p50_ms": baseline_wall_p50,
                        "unpruned_to_pruned_wall_ratio": (
                            round(baseline_wall_p50 / pruned_wall_p50, 4)
                            if baseline_wall_p50 is not None and pruned_wall_p50 > 0
                            else None
                        ),
                        "wall_p95_ms": percentile([item["wall_ms"] for item in samples], 0.95),
                        "peak_rss_bytes_max": max((item["peak_rss_bytes"] for item in samples if item["peak_rss_bytes"] is not None), default=None),
                        "process_tree_peak_rss_bytes_max": max((item["process_tree_peak_rss_bytes"] for item in samples if item["process_tree_peak_rss_bytes"] is not None), default=None),
                        "peak_process_count": max(item["peak_process_count"] for item in samples),
                        "process_tree_cpu_seconds_p50": round(statistics.median([item["process_tree_cpu_seconds"] for item in samples if item["process_tree_cpu_seconds"] is not None]), 3) if any(item["process_tree_cpu_seconds"] is not None for item in samples) else None,
                        "process_tree_io_read_bytes_max": max((item["process_tree_io_read_bytes"] for item in samples if item["process_tree_io_read_bytes"] is not None), default=None),
                        "process_tree_io_write_bytes_max": max((item["process_tree_io_write_bytes"] for item in samples if item["process_tree_io_write_bytes"] is not None), default=None),
                        "stdout_bytes_max": max(item["stdout_bytes"] for item in samples),
                        "report_sha256_unique_count": len({item["report_sha256"] for item in samples}),
                        "scope_claim": (
                            "adaptive_search_with_drawdown_early_pruning"
                            if args.early_pruning_drawdown_bps is not None
                            else "continuous_integer_adaptive_search_only"
                        if args.space == "continuous"
                        else "adaptive_search_only"
                        ),
                    }
                )
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"saved {len(report['workloads'])} verified adaptive-search workloads: {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
