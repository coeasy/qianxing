#!/usr/bin/env python3
"""Run the Rust-owned backtest metric and report projection acceptance set."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
COMMANDS = [
    [
        "cargo",
        "test",
        "-p",
        "qx-core",
        "--lib",
        "period_returns_preserve_sub_float_ulp_changes_in_large_raw_equity",
        "--offline",
    ],
    [
        "cargo",
        "test",
        "-p",
        "qx-core",
        "--lib",
        "aligned_period_returns_preserve_sub_float_ulp_changes_in_large_raw_values",
        "--offline",
    ],
    [
        "cargo",
        "test",
        "-p",
        "qx-app",
        "--lib",
        "artifact_store::backtest_summary::metrics_definition_tests",
        "--offline",
    ],
    [
        "cargo",
        "test",
        "-p",
        "qx-cli",
        "--bin",
        "qx-cli",
        "report_html::metric_definition_tests",
        "--offline",
    ],
    [
        "cargo",
        "test",
        "-p",
        "qx-cli",
        "--bin",
        "qx-cli",
        "report_xlsx::rows::metrics_projection_tests",
        "--offline",
    ],
    [
        "cargo",
        "test",
        "-p",
        "qx-cli",
        "--bin",
        "qx-cli",
        "report_benchmark::tests::explicit_calendar_uses_daily_metric_names_and_labels",
        "--offline",
    ],
]


def main() -> int:
    scratch = ROOT / "target" / "test-tmp"
    scratch.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment.update(
        {
            "CARGO_BUILD_JOBS": "1",
            "CARGO_PROFILE_DEV_DEBUG": "0",
            "TEMP": str(scratch),
            "TMP": str(scratch),
        }
    )
    for index, command in enumerate(COMMANDS, start=1):
        print(f"[{index}/{len(COMMANDS)}] {' '.join(command)}", flush=True)
        completed = subprocess.run(
            command,
            cwd=ROOT,
            env=environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            check=False,
        )
        print(completed.stdout, end="" if completed.stdout.endswith("\n") else "\n")
        if completed.returncode:
            print(f"failed with exit {completed.returncode}", file=sys.stderr)
            return completed.returncode
    print("backtest metrics contract: all six focused Rust checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
