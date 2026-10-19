#!/usr/bin/env python3
"""Measure the A-share rules-on CLI Bar backtest path.

This benchmark binds real A-share rules, fee settings, a generated CN trading
calendar, and a PIT-visible corporate action to qx-cli. Its synthetic prices
make it a throughput workload, not proof that each individual rule branch is
correct; profile status therefore remains unmeasured.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import re
import statistics
import subprocess
import sys
import tempfile
import time
from datetime import date, datetime, time as day_time, timedelta, timezone
from pathlib import Path

from hardware import hardware_fingerprint

ROOT = Path(__file__).resolve().parent.parent
SCALE = 1_000_000_000
INSTRUMENT = "000001.SZSE"
SIZES = (1_000, 8_000, 64_000)
REPEATS = 5
SHANGHAI = timezone(timedelta(hours=8))
STRESS_CASH_DIVIDEND_RAW = SCALE // 10


def trading_days(count: int) -> list[date]:
    result: list[date] = []
    current = date(2024, 1, 2)
    while len(result) < count:
        if current.weekday() < 5:
            result.append(current)
        current += timedelta(days=1)
    return result


def minute_timestamps(day: date) -> list[int]:
    values: list[int] = []
    for start in (day_time(9, 30), day_time(13, 0)):
        for minute in range(120):
            stamp = datetime.combine(day, start, SHANGHAI) + timedelta(minutes=minute)
            values.append(int(stamp.timestamp() * 1_000))
    return values


def generate_workload(
    size: int, directory: Path, action_count: int = 1
) -> tuple[Path, Path, Path, Path]:
    # The dataset registry key is instrument-scoped. Give each workload size a
    # valid, distinct A-share symbol so shared development storage cannot bind
    # a second BarFrame fingerprint to the same dataset id.
    instrument = f"{size:06d}.SZSE"
    days = trading_days((size + 239) // 240)
    timestamps = [stamp for day in days for stamp in minute_timestamps(day)][:size]
    # A low-amplitude deterministic cycle creates repeated EMA crossings while
    # keeping ordinary prices inside the main-board daily limit band.
    tick = 10_000_000
    base = 10 * SCALE
    closes = [base + ((index % 31) - 15) * tick for index in range(size)]
    opens = [closes[index - 1] if index else closes[0] for index in range(size)]
    frame = {
        "instrument": instrument,
        "source": "ashare-profile-rules-on-synthetic-v1",
        "ts": timestamps,
        "open_raw": opens,
        "high_raw": [max(opens[i], closes[i]) + tick for i in range(size)],
        "low_raw": [min(opens[i], closes[i]) - tick for i in range(size)],
        "close_raw": closes,
        "volume_raw": [1_000 * SCALE] * size,
    }
    frame_path = directory / f"ashare-{size}.frame.json"
    frame_path.write_text(json.dumps(frame, separators=(",", ":")), encoding="utf-8")

    calendar_path = directory / "calendar.json"
    calendar = {
        "calendar_id": "cn-synthetic-weekday-benchmark-v1",
        "trading_days": [day.isoformat() for day in days],
        "sessions": [["09:30", "11:30"], ["13:00", "15:00"]],
    }
    calendar_path.write_text(json.dumps(calendar, separators=(",", ":")), encoding="utf-8")

    actions_path = directory / "actions.json"
    actions = [
        {
            "instrument": instrument,
            "ex_date": (
                days[min(len(days) - 1, 100)] if action_count == 1 else days[index % len(days)]
            ).isoformat(),
            "published_at": "2024-01-01T00:00:00+08:00",
            "action_type": "cash_dividend",
            # Keep the stress schedule large enough to affect result/equity hashes. Tiny
            # per-share amounts can round away in fixed-point accounting and turn a many-event
            # timing run into a scan-only workload with no effective actions.
            "cash_dividend_raw": STRESS_CASH_DIVIDEND_RAW,
            "share_ratio_num": 1,
            "share_ratio_den": 1,
            "source": "synthetic-profile-benchmark",
        }
        for index in range(action_count)
    ]
    actions_path.write_text(json.dumps(actions, separators=(",", ":")), encoding="utf-8")

    source_config = json.loads(
        (ROOT / "deploy" / "qianxing.runtime.ashare.example.json").read_text(encoding="utf-8")
    )
    config = copy.deepcopy(source_config)
    # Each workload size gets its own catalog. Otherwise the same dataset id
    # (instrument) is registered with different BarFrame fingerprints and the
    # second size is correctly rejected as a manifest identity conflict.
    config["storage"]["data_dir"] = str(directory / "storage")
    strategy = config["strategy"]
    strategy["instrument"] = instrument
    for worker in config["workers"]:
        if worker.get("role") == "strategy":
            worker["symbols"] = [instrument]
    strategy["ashare_rules_path"] = str(ROOT / "deploy" / "qianxing.ashare.rules.json")
    strategy["ashare_actions_path"] = str(actions_path)
    strategy["ashare_calendar_path"] = str(calendar_path)
    strategy["builtin_fast_window"] = 2
    strategy["builtin_slow_window"] = 3
    config_path = directory / "runtime.json"
    config_path.write_text(json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8")
    spec_path = ROOT / "deploy" / "qianxing.ashare.spot.spec.json"
    return frame_path, spec_path, config_path, actions_path


def run_once(binary: Path, frame: Path, spec: Path, config: Path) -> tuple[float, str, int, str, str]:
    started = time.perf_counter()
    process = subprocess.run(
        [
            str(binary),
            "backtest",
            "builtin",
            "ema_cross",
            str(frame),
            str(spec),
            "100",
            "--config",
            str(config),
        ],
        cwd=frame.parent,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    elapsed_ms = (time.perf_counter() - started) * 1_000
    match = re.search(r"result_hash=([0-9a-f]{16})", process.stdout)
    return elapsed_ms, match.group(1) if match else "", process.returncode, process.stderr[-2_000:], process.stdout


def find_binary(value: str | None) -> Path:
    candidates = [Path(value)] if value else [ROOT / "target" / "release" / "qx-cli.exe", ROOT / "target" / "release" / "qx-cli"]
    for path in candidates:
        if path.is_file():
            return path.resolve()
    raise SystemExit("找不到 qx-cli release binary；请先 cargo build --release -p qx-cli")


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def summary_from_stdout(stdout: str) -> dict:
    match = re.search(r"\[Artifacts\] summary=(\S+)", stdout)
    if not match:
        return {}
    path = Path(match.group(1))
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary")
    parser.add_argument("--sizes", default=",".join(map(str, SIZES)))
    parser.add_argument("--repeats", type=int, default=REPEATS)
    parser.add_argument("--actions", type=int, default=1)
    parser.add_argument("--out", type=Path, default=ROOT / "maturity" / "evidence" / "ashare-rules-backtest.json")
    args = parser.parse_args()
    sizes = [int(value) for value in args.sizes.split(",") if value.strip()]
    if not sizes or any(size < 3 for size in sizes) or len(sizes) != len(set(sizes)):
        parser.error("--sizes 必须是互不重复且至少为 3 的正整数")
    if args.repeats < 2:
        parser.error("--repeats 至少为 2")
    if args.actions < 1:
        parser.error("--actions 至少为 1")

    binary = find_binary(args.binary)
    report = {
        "schema_version": 1,
        "evidence_kind": "ashare_rules_on_cli_backtest_throughput",
        "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "hardware": hardware_fingerprint(),
        "binary": str(binary),
        "binary_sha256": sha256(binary),
        "scenario": "qx-cli backtest builtin ema_cross with A-share rules, fee model, CN weekday calendar, and PIT-visible corporate action",
        "profile_coverage_claim": False,
        "profile_target_status_updated": False,
        "limitations": [
            "synthetic deterministic minute bars; not market-data quality evidence",
            "weekday-only synthetic trading calendar does not model exchange holidays or ad-hoc closures",
            "throughput measurement does not prove every T+1, lot, limit, suspension, or corporate-action behavior branch",
            "does not exercise Paper worker lifecycle or broker integration",
        ],
        "workloads": [],
    }
    with tempfile.TemporaryDirectory(prefix="qx-ashare-profile-", dir=ROOT / "target") as workdir:
        directory = Path(workdir)
        for size in sizes:
            frame, spec, config, action_path = generate_workload(
                size, directory, args.actions
            )
            _, warm_hash, code, error, warm_stdout = run_once(binary, frame, spec, config)
            if code or not warm_hash:
                raise SystemExit(f"A-share warmup failed (N={size}, exit={code}): {error}\n{warm_stdout[-2000:]}")
            samples: list[float] = []
            hashes: list[str] = []
            sample_stdout = ""
            for _ in range(args.repeats):
                elapsed, digest, code, error, sample_stdout = run_once(binary, frame, spec, config)
                if code or not digest:
                    raise SystemExit(f"A-share run failed (N={size}, exit={code}): {error}\n{sample_stdout[-2000:]}")
                samples.append(elapsed)
                hashes.append(digest)
            if len(set(hashes)) != 1 or hashes[0] != warm_hash:
                raise SystemExit(f"A-share result hash changed across repeats (N={size})")
            control_hash = None
            action_effect_verified = None
            if args.actions > 1:
                control_directory = directory / f"control-{size}"
                control_directory.mkdir()
                control_frame, control_spec, control_config, _ = generate_workload(
                    size, control_directory, action_count=1
                )
                _, control_hash, control_code, control_error, control_stdout = run_once(
                    binary, control_frame, control_spec, control_config
                )
                if control_code or not control_hash:
                    raise SystemExit(
                        f"A-share single-action control failed (N={size}, exit={control_code}): "
                        f"{control_error}\n{control_stdout[-2000:]}"
                    )
                action_effect_verified = control_hash != warm_hash
                if not action_effect_verified:
                    raise SystemExit(
                        f"A-share action stress schedule did not change result hash (N={size}, "
                        f"actions={args.actions}); the workload is not exercising effective actions"
                    )
            ordered = sorted(samples)
            rejected = re.search(r"rejected_orders=(\d+)", sample_stdout)
            summary = summary_from_stdout(sample_stdout)
            cost_source = (
                summary.get("execution_costs", {}).get("source", "")
                if isinstance(summary.get("execution_costs"), dict)
                else ""
            )
            report["workloads"].append(
                {
                    "bars": size,
                    "corporate_actions": args.actions,
                    "repeats": args.repeats,
                    "instrument": f"{size:06d}.SZSE",
                    "result_hash": hashes[0],
                    "single_action_control_result_hash": control_hash,
                    "action_effect_verified": action_effect_verified,
                    "p50_ms": round(statistics.median(samples), 3),
                    "p95_ms": round(ordered[min(len(ordered) - 1, round(0.95 * (len(ordered) - 1)))], 3),
                    "bars_per_second_at_p50": round(size / (statistics.median(samples) / 1_000), 2),
                    "rejected_orders_last_repeat": int(rejected.group(1)) if rejected else None,
                    "rules_binding_observed": "ashare-rules:" in cost_source,
                    "cost_binding_observed": bool(cost_source),
                    "calendar_fixture": "synthetic weekday sessions 09:30-11:30 / 13:00-15:00",
                    "input_sha256": {
                        "bar_frame": sha256(frame),
                        "calendar": sha256(config.parent / "calendar.json"),
                        "corporate_actions": sha256(action_path),
                        "runtime_config": sha256(config),
                        "rules": sha256(ROOT / "deploy" / "qianxing.ashare.rules.json"),
                        "market_spec": sha256(spec),
                    },
                    "corporate_action": {
                        "count": args.actions,
                        "type": "cash_dividend",
                        "ex_date": trading_days((size + 239) // 240)[min(len(trading_days((size + 239) // 240)) - 1, 100)].isoformat(),
                        "published_at": "2024-01-01T00:00:00+08:00",
                        "pit_visible_before_ex_date": True,
                    },
                    "deterministic_across_repeats": True,
                }
            )

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"saved {len(report['workloads'])} A-share rules-on workload records; ashare-research remains unmeasured: {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
