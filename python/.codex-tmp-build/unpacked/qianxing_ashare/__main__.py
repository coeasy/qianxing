"""A 股数据源和快速选股命令行入口。"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path

from . import (
    AshareActionManifest,
    AshareActionQuery,
    AshareManifest,
    AshareQuery,
    AshareTradingCalendar,
    BarFrame,
    build_dataset_bundle_manifest,
    create_provider,
    screen_bar_frames,
    serialize_corporate_actions,
)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="python -m qianxing_ashare")
    sub = parser.add_subparsers(dest="command", required=True)
    fetch = sub.add_parser("fetch", help="获取并标准化 A 股 K 线")
    fetch.add_argument("--provider", default="auto", choices=("auto", "akshare", "baostock", "easy_tdx"))
    fetch.add_argument("--code", required=True)
    fetch.add_argument("--start", required=True)
    fetch.add_argument("--end", required=True)
    fetch.add_argument("--frequency", default="daily")
    fetch.add_argument("--adjustment", default="none", choices=("none", "qfq", "hfq"))
    fetch.add_argument("--output", required=True, type=Path)
    fetch.add_argument("--manifest", type=Path)
    actions = sub.add_parser("actions", help="获取并标准化 A 股公司行为")
    actions.add_argument("--provider", default="auto", choices=("auto", "akshare", "baostock", "easy_tdx"))
    actions.add_argument("--code", required=True)
    actions.add_argument("--start", required=True)
    actions.add_argument("--end", required=True)
    actions.add_argument("--as-of", help="PIT 截止时间；只保留已公开可见事件")
    actions.add_argument("--output", required=True, type=Path)
    actions.add_argument("--manifest", type=Path)
    bundle = sub.add_parser("bundle", help="将 A 股组件 manifest 绑定为 DatasetBundleManifest")
    bundle.add_argument("--bundle-id", required=True)
    bundle.add_argument("--version", required=True)
    bundle.add_argument("--source", required=True)
    bundle.add_argument("--bars-manifest", required=True, type=Path)
    bundle.add_argument("--bars-frame", type=Path, help="可选 BarFrame；提供后使用 Rust qx-data fingerprint")
    bundle.add_argument("--actions-manifest", type=Path)
    bundle.add_argument("--calendar", type=Path)
    bundle.add_argument("--output", required=True, type=Path)
    screen = sub.add_parser("screen", help="对目录中的 BarFrame 做快速初筛")
    screen.add_argument("--bars-dir", required=True, type=Path)
    screen.add_argument("--output", required=True, type=Path)
    screen.add_argument("--min-return-bps", type=int, default=-10_000)
    screen.add_argument("--min-avg-volume-raw", type=int, default=0)
    screen.add_argument("--limit", type=int)
    screen.add_argument("--backtest-manifest", type=Path, help="可选：按筛选结果生成 fast-backtest manifest")
    screen.add_argument("--runtime", type=Path, help="manifest 中每个 job 使用的 runtime")
    screen.add_argument("--market-spec", type=Path, help="manifest 中每个 job 使用的 market spec")
    return parser


def main() -> int:
    args = _parser().parse_args()
    if args.command == "fetch":
        args.output.parent.mkdir(parents=True, exist_ok=True)
        query = AshareQuery(args.code, args.start, args.end, args.frequency, args.adjustment)
        frame, manifest = create_provider(args.provider).fetch(query)
        args.output.write_text(frame.to_json(), encoding="utf-8")
        manifest_path = args.manifest or args.output.with_suffix(".manifest.json")
        manifest_path.write_text(manifest.to_json(), encoding="utf-8")
        print(json.dumps({"instrument": frame.instrument, "bars": len(frame.ts), "output": str(args.output), "manifest": str(manifest_path)}, ensure_ascii=False))
        return 0
    if args.command == "actions":
        args.output.parent.mkdir(parents=True, exist_ok=True)
        query = AshareActionQuery(args.code, args.start, args.end, args.as_of)
        actions, manifest = create_provider(args.provider).fetch_corporate_actions(query)
        # v1 信封：顶层携带 schema_version/source/as_of，Rust 侧据此启用严格模式
        # 并按 published_at <= as_of 复核 PIT 可见性；actions 行本身保持兼容。
        args.output.write_text(
            serialize_corporate_actions(
                actions,
                source=manifest.provider,
                instrument=manifest.instrument,
                as_of=query.as_of,
            ),
            encoding="utf-8",
        )
        manifest_path = args.manifest or args.output.with_suffix(".manifest.json")
        manifest_path.write_text(manifest.to_json(), encoding="utf-8")
        print(json.dumps({"instrument": manifest.instrument, "actions": len(actions), "output": str(args.output), "manifest": str(manifest_path)}, ensure_ascii=False))
        return 0
    if args.command == "bundle":
        bars_manifest = AshareManifest.from_json(args.bars_manifest.read_text(encoding="utf-8"))
        actions_manifest = None
        if args.actions_manifest:
            actions_manifest = AshareActionManifest.from_json(
                args.actions_manifest.read_text(encoding="utf-8")
            )
        calendar = None
        if args.calendar:
            calendar = AshareTradingCalendar.from_json(args.calendar.read_text(encoding="utf-8"))
        bars_frame = None
        if args.bars_frame:
            bars_frame = BarFrame.from_json(args.bars_frame.read_text(encoding="utf-8"))
        bundle = build_dataset_bundle_manifest(
            args.bundle_id,
            args.version,
            args.source,
            bars_manifest,
            actions_manifest,
            calendar,
            bars_frame,
        )
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(bundle, ensure_ascii=False, indent=2), encoding="utf-8")
        print(json.dumps({"bundle_id": args.bundle_id, "components": sorted(bundle["components"]), "output": str(args.output)}, ensure_ascii=False))
        return 0
    args.output.parent.mkdir(parents=True, exist_ok=True)
    paths = []
    skipped = 0
    required = {"instrument", "source", "ts", "open_raw", "high_raw", "low_raw", "close_raw", "volume_raw"}
    for path in sorted(args.bars_dir.glob("*.json")):
        if path.name.endswith(".manifest.json"):
            continue
        try:
            value = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            skipped += 1
            continue
        if not isinstance(value, dict) or not required.issubset(value):
            skipped += 1
            continue
        paths.append(path)
    if not paths:
        raise SystemExit(f"no BarFrame JSON found in {args.bars_dir}")
    rows = screen_bar_frames(paths, min_return_bps=args.min_return_bps, min_avg_volume_raw=args.min_avg_volume_raw, limit=args.limit)
    args.output.write_text(json.dumps(rows, ensure_ascii=False, indent=2), encoding="utf-8")
    manifest_path = None
    if args.backtest_manifest:
        if not args.runtime:
            raise SystemExit("--backtest-manifest requires --runtime")
        manifest_path = args.backtest_manifest
        manifest_path.parent.mkdir(parents=True, exist_ok=True)
        base = manifest_path.parent.resolve()
        runtime = os.path.relpath(args.runtime.resolve(), base)
        jobs = []
        for row in rows:
            bars_path = row.get("bars_path")
            if not bars_path:
                continue
            job = {"runtime": runtime, "bars": os.path.relpath(Path(bars_path).resolve(), base)}
            if args.market_spec:
                job["market_spec"] = os.path.relpath(args.market_spec.resolve(), base)
            jobs.append(job)
        manifest_path.write_text(json.dumps({"schema_version": 1, "jobs": jobs}, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps({"candidates": len(rows), "scanned": len(paths), "skipped": skipped, "output": str(args.output), "backtest_manifest": str(manifest_path) if manifest_path else None}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
