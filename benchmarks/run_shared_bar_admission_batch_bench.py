#!/usr/bin/env python3
"""Compare shared-Bar batch grouping with exact output equivalence checks."""

from __future__ import annotations

import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

from hardware import hardware_fingerprint

ROOT = Path(__file__).resolve().parent.parent
COMMAND = [
    "cargo",
    "run",
    "-p",
    "qx-xingban",
    "--example",
    "shared_bar_admission_batch_bench",
    "--release",
    "--offline",
    "-j1",
]


def main() -> int:
    output = ROOT / "maturity" / "evidence" / "rust-shared-bar-admission-batch-2026-10-18.json"
    process = subprocess.run(
        COMMAND,
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if process.returncode:
        sys.stderr.write(process.stdout)
        sys.stderr.write(process.stderr)
        return process.returncode

    try:
        results = [json.loads(line) for line in process.stdout.splitlines() if line.strip()]
    except json.JSONDecodeError as error:
        print(f"benchmark emitted invalid JSON: {error}", file=sys.stderr)
        return 1
    expected = {
        (callbacks, orders, distribution, timestamp_pattern)
        for callbacks in (16, 64, 256)
        for orders in (1, 4, 16)
        for distribution in ("concentrated", "spread")
        for timestamp_pattern in ("aligned", "staggered")
    }
    observed = {
        (
            row.get("callbacks"),
            row.get("orders_per_callback"),
            row.get("distribution"),
            row.get("timestamp_pattern"),
        )
        for row in results
    }
    if len(results) != 36 or observed != expected:
        print("benchmark output does not contain the complete 36-case matrix", file=sys.stderr)
        return 1
    if not all(row.get("outputs_equal") is True for row in results):
        print("BTreeMap and Vec/index grouping results differ", file=sys.stderr)
        return 1

    rustc = subprocess.run(
        ["rustc", "--version"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=True,
    )
    report = {
        "schema_version": 1,
        "benchmark": "shared_bar_admission_batch",
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "command": COMMAND,
        "hardware": hardware_fingerprint(),
        "rustc": rustc.stdout.strip(),
        "profile": "release",
        "methods": {
            "legacy": "BTreeMap<(market_index, visible_ts), Vec<order_id>> then explicit admission-order sort",
            "hash_candidate": "Vec<batch> plus HashMap key-to-index then the same explicit admission-order sort",
            "market_index_candidate": "Vec<batch> plus per-market Vec<(visible_ts, batch_index)> lookup then the same explicit admission-order sort",
            "primary_index_candidate": "Vec<batch> plus one direct per-market timestamp/index slot and a HashMap fallback only for additional visible timestamps",
            "reused_hash_candidate": "Vec<batch> plus a reusable HashMap whose buckets and batch/order Vec capacities are cleared and retained between timestamp batches",
            "aligned_dense_candidate": "reused Vec<batch> plus a direct market-index-to-batch-index vector when every callback sees the same prior-bar timestamp",
        },
        "results": results,
        "limitations": [
            "This microbenchmark isolates in-memory order grouping and admission-order sorting; it excludes strategy execution, matching, ledger writes, and EventLog replay.",
            "Synthetic callback/order distributions do not represent a measured production workload or any of the nine complete product profiles.",
            "The legacy BTreeMap method is reconstructed from the prior grouping data structure; output equality is checked before timing for every case.",
        ],
    }
    output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"saved shared Bar admission batch evidence to {output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
