#!/usr/bin/env python3
"""Compare full multi-market CLI runs before and after visible-Bar binary lookup."""

from __future__ import annotations

import argparse
import hashlib
import json
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from hardware import hardware_fingerprint

ROOT = Path(__file__).resolve().parent.parent
SCALE = 1_000_000_000
INSTRUMENTS = ("BTCUSDT.BINANCE", "ETHUSDT.KRAKEN", "SOLUSDT.BYBIT")


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_frame(directory: Path, instrument: str, offset: int, bars: int) -> Path:
    tick = 10_000_000
    base = 10 * SCALE
    closes = [base + ((index % 31) - 15) * tick for index in range(bars)]
    opens = [closes[index - 1] if index else closes[0] for index in range(bars)]
    payload = {
        "instrument": instrument,
        "source": "multi-market-visible-lookup-ab-v1",
        "ts": [1_700_000_000_000 + offset + index * 60_000 for index in range(bars)],
        "open_raw": opens,
        "high_raw": [max(opens[index], closes[index]) + tick for index in range(bars)],
        "low_raw": [min(opens[index], closes[index]) - tick for index in range(bars)],
        "close_raw": closes,
        "volume_raw": [1_000 * SCALE] * bars,
    }
    path = directory / f"{instrument}.frame.json"
    path.write_text(json.dumps(payload, separators=(",", ":")), encoding="utf-8")
    return path


def build_spec(
    directory: Path,
    label: str,
    ordinal: int,
    frames: list[Path],
    bars: int,
) -> Path:
    markets = []
    for index, instrument in enumerate(INSTRUMENTS):
        markets.append(
            {
                "instrument": instrument,
                "bars_path": str(frames[index]),
                "priority": index,
                "market_spec": None,
                "fee_schedule": None,
                "strategies": [
                    {
                        "priority": 0,
                        "reserved_capital_raw": 20_000 * SCALE,
                        "strategy": {
                            "kind": "sma_cross",
                            "strategy_id": f"slot-{index}",
                            "quantity_raw": SCALE,
                            "fast_window": 2,
                            "slow_window": 4,
                            "period": 14,
                            "threshold_bps": 100,
                        },
                    }
                ],
            }
        )
    run_id = f"visible-index-{label}-{ordinal}"
    spec = {
        "schema_version": 1,
        "experiment_id": run_id,
        "target_strategy_id": "slot-0",
        "base": {
            "schema_version": 1,
            "run_id": run_id,
            "account_id": "bench-account",
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000 * SCALE,
            "seed": 7,
            "output_dir": str(directory / "outputs" / run_id),
            "markets": markets,
        },
        "parameter_space": [{"name": "fast_window", "values": [2]}],
        "strategy_kinds": ["sma_cross"],
    }
    path = directory / f"{run_id}.experiment.json"
    path.write_text(json.dumps(spec, separators=(",", ":")), encoding="utf-8")
    return path


def run_once(binary: Path, spec_path: Path) -> tuple[float, str]:
    started = time.perf_counter()
    process = subprocess.run(
        [str(binary), "app", "run-multi-market-experiment", str(spec_path)],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    elapsed_ms = (time.perf_counter() - started) * 1_000
    if process.returncode:
        raise RuntimeError(
            f"{binary.name} failed ({process.returncode}): "
            f"{process.stderr[-2_000:]} {process.stdout[-1_000:]}"
        )
    result = json.loads(process.stdout)
    return elapsed_ms, result["candidates"][0]["outcome"]["shared_result_hash"]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", required=True, type=Path)
    parser.add_argument("--after", required=True, type=Path)
    parser.add_argument("--bars", type=int, default=8_000)
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument(
        "--out",
        type=Path,
        default=ROOT / "maturity" / "evidence" / "multi-market-visible-lookup-ab.json",
    )
    args = parser.parse_args()
    if args.bars < 8 or args.repeats < 2:
        parser.error("--bars 至少 8、--repeats 至少 2")
    binaries = {"before": args.before.resolve(), "after": args.after.resolve()}
    for binary in binaries.values():
        if not binary.is_file():
            parser.error(f"找不到 qx-cli binary: {binary}")
    binary_sha256 = {name: sha256(path) for name, path in binaries.items()}
    if binary_sha256["before"] == binary_sha256["after"]:
        parser.error("--before 与 --after 指向相同二进制内容")

    samples: dict[str, list[float]] = {"before": [], "after": []}
    hashes: dict[str, set[str]] = {"before": set(), "after": set()}
    with tempfile.TemporaryDirectory(prefix="qx-multi-market-visible-", dir=ROOT / "target") as raw:
        directory = Path(raw)
        frames = [
            write_frame(
                directory,
                instrument,
                index * (args.bars + 1) * 60_000,
                args.bars,
            )
            for index, instrument in enumerate(INSTRUMENTS)
        ]
        for label, binary in binaries.items():
            spec = build_spec(directory, label, 0, frames, args.bars)
            _, digest = run_once(binary, spec)
            hashes[label].add(digest)
        for repeat in range(args.repeats):
            order = ("before", "after") if repeat % 2 == 0 else ("after", "before")
            for label in order:
                spec = build_spec(directory, label, repeat + 1, frames, args.bars)
                elapsed, digest = run_once(binaries[label], spec)
                samples[label].append(elapsed)
                hashes[label].add(digest)
        if len(hashes["before"]) != 1 or hashes["before"] != hashes["after"]:
            raise SystemExit(f"result_hash mismatch: before={hashes['before']} after={hashes['after']}")

        p50 = {label: statistics.median(values) for label, values in samples.items()}
        report = {
            "schema_version": 1,
            "evidence_kind": "multi_market_visible_bar_lookup_release_ab",
            "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "hardware": hardware_fingerprint(),
            "before_binary": str(binaries["before"]),
            "before_binary_sha256": binary_sha256["before"],
            "after_binary": str(binaries["after"]),
            "after_binary_sha256": binary_sha256["after"],
            "workload": {
                "markets": len(INSTRUMENTS),
                "bars_per_market": args.bars,
                "timeline_layout": "three disjoint consecutive time ranges; the first market emits signals while the other markets contain only future bars",
                "strategy": "sma_cross fast_window=2 slow_window=4",
                "repeats": args.repeats,
                "result_hash": next(iter(hashes["before"])),
                "frame_sha256": [sha256(path) for path in frames],
                "timing_ms": {
                    label: {
                        "samples": [round(value, 3) for value in values],
                        "p50": round(p50[label], 3),
                        "p95": round(
                            sorted(values)[
                                min(len(values) - 1, round(0.95 * (len(values) - 1)))
                            ],
                            3,
                        ),
                    }
                    for label, values in samples.items()
                },
                "p50_change_percent": round(
                    (p50["after"] / p50["before"] - 1) * 100,
                    2,
                ),
            },
            "result_hash_equal": True,
            "profile_coverage_claim": False,
            "limitations": [
                "synthetic deterministic bars with separated market timelines",
                "measures the full CLI experiment path, not only the lookup function",
                "does not establish production multi-market profile performance",
            ],
        }

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"saved same-input multi-market A/B evidence: {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
