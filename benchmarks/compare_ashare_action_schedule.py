#!/usr/bin/env python3
"""Compare A-share corporate-action scanning before and after timestamp indexing."""

from __future__ import annotations

import argparse
import hashlib
import json
import statistics
import subprocess
import tempfile
import time
from pathlib import Path

from hardware import hardware_fingerprint
from run_ashare_profile_backtest import ROOT, generate_workload, run_once


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def resolve_binary(value: str) -> Path:
    path = Path(value).resolve()
    if not path.is_file():
        raise SystemExit(f"找不到 qx-cli binary: {path}")
    return path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", required=True, help="未包含公司行为时间索引的 qx-cli")
    parser.add_argument("--after", required=True, help="包含公司行为时间索引的 qx-cli")
    parser.add_argument("--bars", type=int, default=64_000)
    parser.add_argument("--actions", type=int, default=512)
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument(
        "--out",
        type=Path,
        default=ROOT / "maturity" / "evidence" / "ashare-action-schedule-ab.json",
    )
    args = parser.parse_args()
    if args.bars < 3 or args.actions < 1 or args.repeats < 2:
        parser.error("--bars 至少 3、--actions 至少 1、--repeats 至少 2")

    before = resolve_binary(args.before)
    after = resolve_binary(args.after)
    before_sha256 = sha256(before)
    after_sha256 = sha256(after)
    if before_sha256 == after_sha256:
        parser.error("--before 与 --after 指向相同二进制内容")
    with tempfile.TemporaryDirectory(prefix="qx-ashare-action-ab-", dir=ROOT / "target") as raw:
        directory = Path(raw)
        frame, spec, config, actions = generate_workload(args.bars, directory, args.actions)
        binaries = {"before": before, "after": after}
        for label, binary in binaries.items():
            _, digest, code, error, output = run_once(binary, frame, spec, config)
            if code or not digest:
                raise SystemExit(f"{label} warmup failed (exit={code}): {error}\n{output[-2000:]}")

        samples = {"before": [], "after": []}
        hashes = {"before": set(), "after": set()}
        orders = ("before", "after")
        for repeat in range(args.repeats):
            order = orders if repeat % 2 == 0 else tuple(reversed(orders))
            for label in order:
                elapsed, digest, code, error, output = run_once(
                    binaries[label], frame, spec, config
                )
                if code or not digest:
                    raise SystemExit(
                        f"{label} run {repeat + 1} failed (exit={code}): {error}\n{output[-2000:]}"
                    )
                samples[label].append(elapsed)
                hashes[label].add(digest)

        if len(hashes["before"]) != 1 or hashes["before"] != hashes["after"]:
            raise SystemExit(f"result_hash mismatch: before={hashes['before']} after={hashes['after']}")

        report = {
            "schema_version": 1,
            "evidence_kind": "ashare_corporate_action_schedule_ab",
            "generated_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "hardware": hardware_fingerprint(),
            "before_binary": str(before),
            "before_binary_sha256": before_sha256,
            "after_binary": str(after),
            "after_binary_sha256": after_sha256,
            "workload": {
                "bars": args.bars,
                "corporate_actions": args.actions,
                "repeats": args.repeats,
                "result_hash": next(iter(hashes["before"])),
                "identical_input_sha256": {
                    "bar_frame": sha256(frame),
                    "calendar": sha256(directory / "calendar.json"),
                    "corporate_actions": sha256(actions),
                    "runtime_config": sha256(config),
                    "rules": sha256(ROOT / "deploy" / "qianxing.ashare.rules.json"),
                    "market_spec": sha256(spec),
                },
                "timing_ms": {
                    label: {
                        "samples": [round(value, 3) for value in values],
                        "p50": round(statistics.median(values), 3),
                        "p95": round(
                            sorted(values)[min(len(values) - 1, round(0.95 * (len(values) - 1)))],
                            3,
                        ),
                    }
                    for label, values in samples.items()
                },
                "p50_change_percent": round(
                    (statistics.median(samples["after"]) / statistics.median(samples["before"]) - 1)
                    * 100,
                    2,
                ),
            },
            "result_hash_equal": True,
            "profile_coverage_claim": False,
            "limitations": [
                "synthetic deterministic minute bars and weekday-only calendar",
                "corporate actions are sparse and do not represent exchange-wide production distributions",
                "timing compares complete CLI invocations, not an isolated kernel microbenchmark",
                "does not establish ashare-research profile completion",
            ],
        }

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"saved same-input A/B evidence: {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
