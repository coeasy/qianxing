#!/usr/bin/env python3
"""Measure one qx-cli multi-account batch across account and strategy-slot counts."""

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
from run_profile_backtest import generate_frame, load_fixture
from source_fingerprint import MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA, measurement_source_fingerprint

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_ACCOUNT_COUNTS = (2, 8, 16, 32)
DEFAULT_STRATEGY_COUNTS = (1, 4)
DEFAULT_BARS_PER_ACCOUNT = 64_000
DEFAULT_REPEATS = 3
INITIAL_CASH_RAW = 100_000_000_000_000
QUANTITY_RAW = 1_000_000_000
STRATEGY_KINDS = (
    "sma_cross",
    "ema_cross",
    "momentum",
    "rsi",
    "donchian_breakout",
    "atr_trend",
    "keltner_trend",
    "vwap_reversion",
)


def find_binary(value: str | None) -> Path:
    candidates = [Path(value)] if value else [
        ROOT / "target" / "release" / "qx-cli.exe",
        ROOT / "target" / "release" / "qx-cli",
    ]
    for path in candidates:
        if path.is_file():
            return path.resolve()
    raise SystemExit("找不到 qx-cli release binary；请先 cargo build --release -p qx-cli")


def strategy_spec(account_index: int, slot_index: int, strategies_per_account: int) -> dict:
    kind = STRATEGY_KINDS[slot_index % len(STRATEGY_KINDS)]
    return {
        "kind": kind,
        "strategy_id": f"acct-{account_index:02}-strategy-{slot_index:02}",
        # Keep each slot's requested notional within its equal capital reservation.
        "quantity_raw": QUANTITY_RAW // strategies_per_account,
        "fast_window": 2 + slot_index % 3,
        "slow_window": 5 + slot_index % 4,
        "period": 3 + slot_index % 5,
        "threshold_bps": 0,
    }


def build_spec(
    accounts: int,
    strategies_per_account: int,
    bars_path: Path,
    output_root: Path,
    instrument: str,
) -> dict:
    if strategies_per_account < 1 or strategies_per_account > len(STRATEGY_KINDS):
        raise ValueError(f"strategies_per_account must be in 1..={len(STRATEGY_KINDS)}")
    runs = []
    for account_index in range(accounts):
        strategy_members = [
            strategy_spec(account_index, slot, strategies_per_account)
            for slot in range(strategies_per_account)
        ]
        run = {
            "schema_version": 1,
            "run_id": f"capacity-a{accounts}-s{strategies_per_account}-{account_index:02}",
            "account_id": f"capacity-account-{account_index:02}",
            "instrument": instrument,
            "bars_path": str(bars_path),
            "settlement_currency": "USDT",
            "initial_cash_raw": INITIAL_CASH_RAW,
            "seed": 20261022,
            "output_dir": str(output_root / f"account-{account_index:02}"),
            "strategy": strategy_members[0],
        }
        if strategies_per_account > 1:
            reserved = INITIAL_CASH_RAW // strategies_per_account
            run["portfolio"] = {
                "primary_priority": 0,
                "primary_reserved_capital_raw": reserved,
                "additional_strategies": [
                    {
                        "priority": slot,
                        "reserved_capital_raw": reserved,
                        "strategy": strategy_members[slot],
                    }
                    for slot in range(1, strategies_per_account)
                ],
            }
        runs.append(run)
    return {
        "schema_version": 1,
        "batch_id": f"capacity-a{accounts}-s{strategies_per_account}",
        "runs": runs,
    }


def run_once(binary: Path, spec_path: Path, cwd: Path) -> tuple[float, dict, int | None, int | None, int, float | None, int | None, int | None]:
    measured = run_measured(
        [str(binary), "app", "multi-account-backtest", str(spec_path)], cwd=cwd
    )
    elapsed, stdout, stderr, code, peak_rss, tree_rss, process_count = measured
    if code != 0:
        raise RuntimeError(f"multi-account batch failed (exit={code}): {stderr[-3000:]}")
    try:
        result = json.loads(stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"multi-account batch returned invalid JSON: {error}: {stdout[-1000:]}") from error
    return elapsed, result, peak_rss, tree_rss, process_count, measured.tree_cpu_seconds, measured.tree_io_read_bytes, measured.tree_io_write_bytes


def result_identity(result: dict, expected_accounts: int) -> tuple[list[str], int, int]:
    summary = result.get("summary", {})
    runs = result.get("runs", [])
    if (
        len(runs) != expected_accounts
        or summary.get("accounts_total") != expected_accounts
        or summary.get("accounts_succeeded") != expected_accounts
        or summary.get("accounts_failed") != 0
        or summary.get("complete") is not True
    ):
        raise RuntimeError(f"multi-account result is incomplete: {summary}")
    hashes = []
    for run in runs:
        outcome = run.get("outcome")
        if not outcome or not outcome.get("result_hash"):
            raise RuntimeError(f"account result is missing its result hash: {run.get('account_id')}")
        hashes.append(outcome["result_hash"])
    return hashes, int(summary.get("successful_fills", 0)), int(summary.get("successful_initial_cash_raw", 0))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary")
    parser.add_argument("--accounts", default=",".join(map(str, DEFAULT_ACCOUNT_COUNTS)))
    parser.add_argument("--strategies-per-account", default=",".join(map(str, DEFAULT_STRATEGY_COUNTS)))
    parser.add_argument(
        "--workloads",
        help="explicit account:strategy pairs, e.g. 2:1,2:4,4:1; overrides the Cartesian matrix",
    )
    parser.add_argument("--bars", type=int, default=DEFAULT_BARS_PER_ACCOUNT)
    parser.add_argument("--repeats", type=int, default=DEFAULT_REPEATS)
    parser.add_argument("--out", type=Path, default=ROOT / "maturity" / "evidence" / "multi-account-capacity-2026-10-22.json")
    args = parser.parse_args()
    accounts_values = [int(value) for value in args.accounts.split(",") if value.strip()]
    strategy_values = [int(value) for value in args.strategies_per_account.split(",") if value.strip()]
    if not accounts_values or any(value < 2 or value > 32 for value in accounts_values) or len(set(accounts_values)) != len(accounts_values):
        parser.error("--accounts values must be unique integers in 2..=32")
    if not strategy_values or any(value < 1 or value > len(STRATEGY_KINDS) for value in strategy_values) or len(set(strategy_values)) != len(strategy_values):
        parser.error(f"--strategies-per-account values must be unique integers in 1..={len(STRATEGY_KINDS)}")
    if args.workloads:
        try:
            workload_pairs = [tuple(int(part) for part in value.split(":")) for value in args.workloads.split(",")]
        except ValueError:
            parser.error("--workloads must be comma-separated account:strategy pairs")
        if any(len(pair) != 2 or not 2 <= pair[0] <= 32 or not 1 <= pair[1] <= len(STRATEGY_KINDS) for pair in workload_pairs):
            parser.error(f"--workloads values must be account:strategy pairs with accounts 2..=32 and strategies 1..={len(STRATEGY_KINDS)}")
        if len(set(workload_pairs)) != len(workload_pairs):
            parser.error("--workloads pairs must be unique")
    else:
        workload_pairs = [(accounts, strategies) for accounts in accounts_values for strategies in strategy_values]
    if args.bars < 3 or args.repeats < 2:
        parser.error("--bars must be >= 3 and --repeats must be >= 2")

    binary = find_binary(args.binary)
    instrument_spec, identity = load_fixture("crypto-spot")
    report = {
        "schema_version": 1,
        "evidence_kind": "multi_account_multi_strategy_bar_capacity",
        "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "hardware": hardware_fingerprint(),
        "binary": str(binary),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "measurement_source_fingerprint_schema": MEASUREMENT_SOURCE_FINGERPRINT_SCHEMA,
        "measurement_source_fingerprint": measurement_source_fingerprint(ROOT),
        "bars_per_account": args.bars,
        "repeats": args.repeats,
        "configured_workloads": [
            {"accounts": accounts, "strategies_per_account": strategies}
            for accounts, strategies in workload_pairs
        ],
        "method": "one qx-cli multi-account-backtest process; every account runs the same synthetic BarFrame through the normal per-account BacktestSpec path and optional same-account strategy portfolio",
        "profile_coverage_claim": False,
        "profile_target_statuses_updated": False,
        "scope_limitations": [
            "synthetic crypto spot Bars without TradingInstrumentSpec or venue rules",
            "MultiAccountBacktestSpec accounts execute sequentially; this measures batch memory and wall time, not internal scheduler concurrency",
            "does not exercise external provider, Paper, network, or multi-venue profile paths",
            "process-tree RSS is a sampled aggregate and may miss brief spikes",
        ],
        "workloads": [],
    }
    with tempfile.TemporaryDirectory(prefix="mac-", dir=ROOT / "target", ignore_cleanup_errors=True) as directory:
        workdir = Path(directory)
        frame = workdir / f"crypto-spot-{args.bars}.json"
        generate_frame(args.bars, "crypto-spot", instrument_spec, identity, frame)
        for account_count, strategy_count in workload_pairs:
                output_root = workdir / f"runs-a{account_count}-s{strategy_count}"
                spec = build_spec(account_count, strategy_count, frame, output_root, identity["instrument"])
                spec_path = workdir / f"spec-a{account_count}-s{strategy_count}.json"
                spec_path.write_text(json.dumps(spec, separators=(",", ":")), encoding="utf-8")
                warm_elapsed, warm, _, _, _, _, _, _ = run_once(binary, spec_path, workdir)
                warm_hashes, warm_fills, initial_cash = result_identity(warm, account_count)
                times, hashes_runs, rss, tree_rss, process_counts = [], [], [], [], []
                cpu_seconds, io_read_bytes, io_write_bytes = [], [], []
                for _ in range(args.repeats):
                    elapsed, result, peak, tree_peak, process_count, cpu, io_read, io_write = run_once(binary, spec_path, workdir)
                    hashes, fills, repeated_cash = result_identity(result, account_count)
                    if hashes != warm_hashes or fills != warm_fills or repeated_cash != initial_cash:
                        raise RuntimeError(f"non-deterministic result for accounts={account_count}, strategies={strategy_count}")
                    times.append(elapsed)
                    hashes_runs.append(hashes)
                    if peak is not None:
                        rss.append(peak)
                    if tree_peak is not None:
                        tree_rss.append(tree_peak)
                    process_counts.append(process_count)
                    if cpu is not None:
                        cpu_seconds.append(cpu)
                    if io_read is not None:
                        io_read_bytes.append(io_read)
                    if io_write is not None:
                        io_write_bytes.append(io_write)
                ordered = sorted(times)
                report["workloads"].append({
                    "accounts": account_count,
                    "strategies_per_account": strategy_count,
                    "total_strategy_slots": account_count * strategy_count,
                    "bars_per_account": args.bars,
                    "bars_total": account_count * args.bars,
                    "bars_times_strategy_slots": account_count * strategy_count * args.bars,
                    "warmup_wall_ms": round(warm_elapsed, 3),
                    "p50_wall_ms": round(statistics.median(times), 3),
                    "p95_wall_ms": round(ordered[min(len(ordered) - 1, round(0.95 * (len(ordered) - 1)))], 3),
                    "peak_rss_bytes_max": max(rss) if rss else None,
                    "aggregate_process_tree_peak_rss_bytes_max": max(tree_rss) if tree_rss else None,
                    "peak_process_count": max(process_counts, default=1),
                    "process_tree_cpu_seconds_p50": round(statistics.median(cpu_seconds), 3) if cpu_seconds else None,
                    "process_tree_io_read_bytes_max": max(io_read_bytes) if io_read_bytes else None,
                    "process_tree_io_write_bytes_max": max(io_write_bytes) if io_write_bytes else None,
                    "successful_fills": warm_fills,
                    "result_hashes_by_account": warm_hashes,
                    "deterministic_across_repeats": all(values == warm_hashes for values in hashes_runs),
                })
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"saved {len(report['workloads'])} multi-account capacity workloads: {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
