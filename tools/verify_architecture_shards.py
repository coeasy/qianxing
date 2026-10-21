#!/usr/bin/env python3
"""Verify complete, non-overlapping architecture gate shard reports."""
from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GATE = ROOT / "tools" / "check_architecture.py"


def fail(message: str) -> int:
    print(f"架构分片汇总失败：{message}")
    return 1


def main() -> int:
    if len(sys.argv) != 3:
        print("用法：python tools/verify_architecture_shards.py <report-dir> <shard-count>")
        return 2
    report_dir = Path(sys.argv[1])
    try:
        shard_count = int(sys.argv[2])
        if shard_count < 2:
            return fail("shard-count 必须 >= 2")
        reports = [
            json.loads((report_dir / f"shard-{shard}.json").read_text(encoding="utf-8"))
            for shard in range(shard_count)
        ]
    except (OSError, ValueError) as error:
        return fail(f"分片报告不可读：{error}")

    spec = importlib.util.spec_from_file_location("qx_architecture_gate", GATE)
    if spec is None or spec.loader is None:
        return fail("无法加载架构门禁")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    expected = module._main_check_functions()
    actual_functions = []
    for shard, report in enumerate(reports):
        if report.get("schema_version") != 1 or report.get("shard") != shard or report.get("shards") != shard_count:
            return fail(f"分片 {shard} 的身份/schema 不匹配")
        if report.get("failures"):
            return fail(f"分片 {shard} 有 {len(report['failures'])} 项失败")
        functions = report.get("functions")
        if not isinstance(functions, list):
            return fail(f"分片 {shard} 缺 functions 列表")
        actual_functions.extend(functions)
    actual_functions.sort(key=lambda item: item.get("ordinal", -1))
    expected_roster = [{"ordinal": index, "name": name} for index, (name, _) in enumerate(expected)]
    actual_roster = [
        {"ordinal": item.get("ordinal"), "name": item.get("name")}
        for item in actual_functions
    ]
    if actual_roster != expected_roster:
        return fail(f"分片函数覆盖不完整或有重叠：期望 {len(expected_roster)} 个，实得 {len(actual_roster)} 个")
    print(
        f"架构分片汇总通过：{len(reports)} 片，"
        f"完整覆盖 {len(expected_roster)} 个具名检查且无重复"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
