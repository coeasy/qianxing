#!/usr/bin/env python3
"""Measure product-spec-bound CLI backtests using the seven market identity fixtures.

This is a local performance proxy, not a claim that the full Paper, Testnet, or
multi-venue profile has been exercised. It binds each run to the fixture's real
instrument identity and TradingInstrumentSpec and preserves that scope in JSON.
"""

from __future__ import annotations

import argparse
import copy
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
FIXTURES = ROOT / "maturity" / "fixtures" / "scenarios"
SIZES = (1_000, 8_000, 64_000)
REPEATS = 5
SCALE = 1_000_000_000
FIXTURE_DOMAINS = {
    "ashare-equity": "A-share equity instrument",
    "cn-futures": "Chinese futures instrument",
    "cn-options": "Chinese option contract",
    "global-equity": "global equity instrument",
    "fx-cfd": "FX/CFD instrument",
    "crypto-spot": "crypto spot instrument",
    "crypto-perpetual": "crypto perpetual instrument",
}
BASE_PRICE = {
    "ashare-equity": 100 * SCALE,
    "cn-futures": 3_500 * SCALE,
    "cn-options": 2 * SCALE,
    "global-equity": 100 * SCALE,
    "fx-cfd": 11 * SCALE // 10,
    "crypto-spot": 50_000 * SCALE,
    "crypto-perpetual": 30_000 * SCALE,
}
UNMEASURED_PROFILE_TARGETS = {
    "ashare-research": "the profile requires A-share rules, calendar, PIT and corporate-action behavior",
    "ashare-paper": "the profile requires the Paper worker and virtual execution path",
    "cn-futures-research": "the profile requires night-session calendar, margin, fees and rollover behavior",
    "cn-options-research": "the profile requires options chain, exercise/assignment and seller-margin behavior",
    "global-equity-paper": "the profile requires Paper execution, calendar, FX valuation and corporate actions",
    "fx-paper": "the profile requires Paper execution, rollover, margin and hedge-mode behavior",
    "crypto-paper": "the profile requires Paper execution, partial fills, rate limiting and funding behavior",
    "crypto-testnet": "requires authenticated testnet order/ack/reconciliation evidence",
    "multi-venue-arb": "requires paired venue pipeline, group state, compensation and reconciliation",
}
UNSUPPORTED_FIXTURES = {
    "cn-options": "the Bar backtest rejects option contracts without chain, exercise and assignment inputs",
}


def load_fixture(name: str) -> tuple[dict, dict]:
    payload = json.loads((FIXTURES / f"{name}.json").read_text(encoding="utf-8"))
    spec = payload["instrument_spec"]
    symbol = spec["instrument"]["symbol"]
    venue = spec["instrument"]["venue"]
    return spec, {"instrument": f"{symbol}.{venue}", "start_ms": spec["valid_from"]}


def generate_frame(size: int, fixture: str, spec: dict, identity: dict, path: Path) -> None:
    tick = int(spec["price_tick"])
    base = BASE_PRICE[fixture]
    if tick <= 0 or base <= 8 * tick or base % tick:
        raise ValueError(f"fixture {fixture} has incompatible benchmark price/tick: {base}/{tick}")
    prices = [base + ((index % 17) - 8) * tick for index in range(size)]
    volume_unit = max(int(spec["qty_step"]), int(spec["min_qty"]))
    ts = [int(identity["start_ms"]) + index * 60_000 for index in range(size)]
    document = {
        "instrument": identity["instrument"],
        "source": f"profile-market-proxy-{fixture}-v1",
        "ts": ts,
        "open_raw": prices,
        "high_raw": [price + tick for price in prices],
        "low_raw": [price - tick for price in prices],
        "close_raw": [price + ((index % 3) - 1) * tick for index, price in enumerate(prices)],
        "volume_raw": [volume_unit * (10 + index % 100) for index in range(size)],
    }
    path.write_text(json.dumps(document, separators=(",", ":")), encoding="utf-8")


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def runtime_config(fixture: str, size: int, identity: dict, workdir: Path) -> Path:
    template_path = ROOT / "deploy" / "qianxing.runtime.builtin-strategy.example.json"
    config = json.loads(template_path.read_text(encoding="utf-8"))
    config["storage"]["data_dir"] = str(workdir / f"storage-{fixture}-{size}")
    config["strategy"]["instrument"] = identity["instrument"]
    for worker in config["workers"]:
        if worker.get("role") == "strategy":
            worker["symbols"] = [identity["instrument"]]
    config_path = workdir / f"{fixture}-{size}.runtime.json"
    config_path.write_text(json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8")
    return config_path


def run_once(
    binary: Path, frame: Path, market_spec: Path, quantity: int, config: Path
) -> tuple[float, str, int, str]:
    started = time.perf_counter()
    proc = subprocess.run(
        [
            str(binary),
            "backtest",
            "builtin",
            "sma_cross",
            str(frame),
            str(market_spec),
            str(quantity),
            "--config",
            str(config),
        ],
        cwd=frame.parent,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    elapsed_ms = (time.perf_counter() - started) * 1_000.0
    digest = ""
    for line in proc.stdout.splitlines():
        if "result_hash=" in line:
            digest = line.split("result_hash=", 1)[1].split()[0]
    return elapsed_ms, digest, proc.returncode, proc.stderr[-2_000:]


def find_binary(value: str | None) -> Path:
    candidates = [Path(value)] if value else [
        ROOT / "target" / "release" / "qx-cli.exe",
        ROOT / "target" / "release" / "qx-cli",
    ]
    for path in candidates:
        if path.is_file():
            return path.resolve()
    raise SystemExit("找不到 qx-cli release binary；请先 cargo build --release -p qx-cli")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary")
    parser.add_argument("--sizes", default=",".join(map(str, SIZES)))
    parser.add_argument("--repeats", type=int, default=REPEATS)
    parser.add_argument("--out", type=Path, default=ROOT / "maturity" / "evidence" / "profile-market-backtest.json")
    args = parser.parse_args()
    sizes = [int(value) for value in args.sizes.split(",") if value.strip()]
    if not sizes or any(size < 3 for size in sizes) or len(sizes) != len(set(sizes)):
        parser.error("--sizes 必须是互不重复且至少为 3 的正整数")
    if args.repeats < 2:
        parser.error("--repeats 至少为 2")
    binary = find_binary(args.binary)
    report = {
        "schema_version": 1,
        "evidence_kind": "product_spec_bound_cli_backtest_proxy",
        "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "hardware": hardware_fingerprint(),
        "binary": str(binary),
        "scenario": "qx-cli backtest builtin sma_cross with fixture instrument + TradingInstrumentSpec",
        "profile_coverage_claim": False,
        "market_workloads": [],
        "unmeasured_profile_targets": UNMEASURED_PROFILE_TARGETS,
        "unsupported_market_fixtures": [],
        "profile_target_statuses_updated": False,
    }
    temporary_root: Path | None = None
    with tempfile.TemporaryDirectory(
        prefix="qx-profile-bench-",
        dir=ROOT / "target",
        ignore_cleanup_errors=True,
    ) as workdir:
        temp_root = Path(workdir)
        temporary_root = temp_root
        for fixture, domain in FIXTURE_DOMAINS.items():
            spec, identity = load_fixture(fixture)
            if fixture in UNSUPPORTED_FIXTURES:
                report["unsupported_market_fixtures"].append(
                    {
                        "fixture": fixture,
                        "market_domain": domain,
                        "status": "not_measured",
                        "reason": UNSUPPORTED_FIXTURES[fixture],
                    }
                )
                continue
            quantity = max(1, (int(spec["min_qty"]) + SCALE - 1) // SCALE)
            market_path = temp_root / f"{fixture}-market.json"
            market_path.write_text(json.dumps(spec), encoding="utf-8")
            profile_rows = []
            for size in sizes:
                frame_path = temp_root / f"{fixture}-{size}.json"
                generate_frame(size, fixture, spec, identity, frame_path)
                config_path = runtime_config(fixture, size, identity, temp_root)
                _, warm_hash, warm_code, warm_error = run_once(
                    binary, frame_path, market_path, quantity, config_path
                )
                if warm_code != 0 or not warm_hash:
                    raise SystemExit(f"{fixture} warmup failed (N={size}, exit={warm_code}): {warm_error}")
                samples, hashes = [], []
                for _ in range(args.repeats):
                    elapsed, result_hash, return_code, stderr = run_once(
                        binary, frame_path, market_path, quantity, config_path
                    )
                    if return_code != 0 or not result_hash:
                        raise SystemExit(f"{fixture} failed (N={size}, exit={return_code}): {stderr}")
                    samples.append(elapsed)
                    hashes.append(result_hash)
                if len(set(hashes)) != 1 or hashes[0] != warm_hash:
                    raise SystemExit(f"{fixture} result hash changed across repeats (N={size})")
                ordered = sorted(samples)
                profile_rows.append(
                    {
                        "bars": size,
                        "repeats": args.repeats,
                        "p50_ms": round(statistics.median(samples), 3),
                        "p95_ms": round(ordered[min(len(ordered) - 1, round(0.95 * (len(ordered) - 1)))], 3),
                        "result_hash": hashes[0],
                        "deterministic_across_repeats": True,
                    }
                )
            report["market_workloads"].append(
                {
                    "fixture": fixture,
                    "market_domain": domain,
                    "instrument": identity["instrument"],
                    "product": spec["product"],
                    "settlement_currency": spec["settlement_currency"],
                    "quantity": quantity,
                    "runtime_config_sha256": sha256(config_path),
                    "market_spec_sha256": sha256(market_path),
                    "scope_limitations": [
                        "synthetic BarFrame generated at runtime",
                        "measures CLI Bar backtest with market spec only",
                        "does not exercise full profile-specific Paper, calendar, corporate-action, funding, or venue workflows",
                    ],
                    "rows": profile_rows,
                }
            )
        # Save completed measurements before temporary-directory cleanup. A cleanup
        # failure must not discard a fully completed benchmark run.
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(
            json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
        print(
            f"saved {len(report['market_workloads'])} market-spec workload records; "
            f"profile targets remain unmeasured: {args.out}"
        )
    if temporary_root is not None and temporary_root.exists():
        print(f"warning: benchmark temporary files remain at {temporary_root}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
