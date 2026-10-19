#!/usr/bin/env python3
"""牵星性能基线驱动器（M0 / 关闭 G6「无性能基线」）。

口径（诚实声明，别把没测的读成测了）：
  - 本脚本只驱动**已构建的 qx-cli 公开命令面**，测的是端到端墙钟：
    `backtest builtin <strategy> <frame_N>`（读帧 → 校验 → 逐事件回测 → 印摘要）。
  - 因此它覆盖「回测吞吐随 N 的变化」与「同样输入必得同样结果（result_hash 跨重复稳定）」。
  - 它**不**测内核内部的 append/refresh/replay/read-model/recovery 逐段 p50/p95 ——
    那几段需要 in-process 钩子（Rust bench target），本轮按「零新依赖」原则未落，
    缺口登记在 docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md §6.6 与 §4.3 G6。

用法：
  python benchmarks/run_baseline.py --binary target/release/qx-cli.exe
  python benchmarks/run_baseline.py --sizes 1000,8000,32000 --repeats 7 --json
"""

from __future__ import annotations

import argparse
import json
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from hardware import hardware_fingerprint

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_SIZES = (1000, 8000, 64000)
DEFAULT_REPEATS = 7
BAR_INTERVAL_MS = 60_000
SCALE = 1_000_000_000  # 与内核一致：定点 1e9
MARGINAL_KNEE_THRESHOLD_PCT = 20.0


def find_binary(explicit: str | None) -> Path:
    if explicit:
        path = Path(explicit)
        if not path.is_file():
            raise SystemExit(f"找不到 qx-cli：{explicit}")
        return path
    for candidate in (
        REPO_ROOT / "target" / "release" / "qx-cli.exe",
        REPO_ROOT / "target" / "release" / "qx-cli",
        REPO_ROOT / "target" / "debug" / "qx-cli.exe",
        REPO_ROOT / "target" / "debug" / "qx-cli",
    ):
        if candidate.is_file():
            return candidate
    raise SystemExit("找不到 qx-cli，请先 cargo build --release -p qx-cli 或用 --binary 指定")


def generate_frame(size: int, destination: Path) -> None:
    """生成确定性 BarFrame（无随机源：价格是下标的有界函数）。"""
    ts, opens, highs, lows, closes, volumes = [], [], [], [], [], []
    for i in range(size):
        ts.append((i + 1) * BAR_INTERVAL_MS)
        base = 100 * SCALE + (i % 100) * (SCALE // 100)
        drift = (i % 7 - 3) * (SCALE // 1000)
        open_p = base
        close_p = base + drift
        opens.append(open_p)
        closes.append(close_p)
        highs.append(max(open_p, close_p) + SCALE // 1000)
        lows.append(min(open_p, close_p) - SCALE // 1000)
        volumes.append(SCALE + i)
    document = {
        "instrument": "BTCUSDT.BINANCE",
        "source": "benchmark-baseline-v1",
        "ts": ts,
        "open_raw": opens,
        "high_raw": highs,
        "low_raw": lows,
        "close_raw": closes,
        "volume_raw": volumes,
    }
    destination.write_text(json.dumps(document), encoding="utf-8")


def run_once(binary: Path, frame: Path) -> tuple[float, str, int]:
    started = time.perf_counter()
    proc = subprocess.run(
        [str(binary), "backtest", "builtin", "sma_cross", str(frame)],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    elapsed_ms = (time.perf_counter() - started) * 1000.0
    result_hash = ""
    for line in proc.stdout.splitlines():
        if "result_hash=" in line:
            tail = line.split("result_hash=", 1)[1]
            result_hash = tail.split()[0].strip()
    return elapsed_ms, result_hash, proc.returncode


def percentile(samples: list[float], fraction: float) -> float:
    ordered = sorted(samples)
    if not ordered:
        return float("nan")
    index = min(len(ordered) - 1, max(0, round(fraction * (len(ordered) - 1))))
    return ordered[index]


def scaling_curve(rows: list[dict]) -> dict:
    """按相邻 N 的 p50 差分输出端到端边际每 Bar 成本。"""
    ordered = sorted(rows, key=lambda row: row["bars"])
    points = [
        {
            "bars": row["bars"],
            "p50_ms": row["p50_ms"],
            "amortized_p50_ms_per_bar": round(row["p50_ms"] / row["bars"], 9),
        }
        for row in ordered
    ]
    segments = []
    candidate_positive_knee = None
    previous_slope = None
    for previous, current in zip(ordered, ordered[1:]):
        bars_delta = current["bars"] - previous["bars"]
        if bars_delta <= 0:
            raise ValueError("scaling curve bars must be unique and increasing")
        slope = (current["p50_ms"] - previous["p50_ms"]) / bars_delta
        slope_change_pct = (
            None
            if previous_slope is None or previous_slope == 0
            else round((slope - previous_slope) / abs(previous_slope) * 100.0, 3)
        )
        segment = {
            "from_bars": previous["bars"],
            "to_bars": current["bars"],
            "delta_p50_ms": round(current["p50_ms"] - previous["p50_ms"], 3),
            "marginal_p50_ms_per_bar": round(slope, 9),
            "marginal_p50_us_per_bar": round(slope * 1_000.0, 3),
            "slope_change_pct_vs_previous_segment": slope_change_pct,
        }
        segments.append(segment)
        if (
            candidate_positive_knee is None
            and slope_change_pct is not None
            and previous_slope is not None
            and slope > previous_slope
            and slope > 0
            and slope_change_pct > MARGINAL_KNEE_THRESHOLD_PCT
        ):
            candidate_positive_knee = current["bars"]
        previous_slope = slope
    return {
        "method": "adjacent p50 wall-time differences; includes process startup and JSON parsing",
        "candidate_knee_threshold_pct": MARGINAL_KNEE_THRESHOLD_PCT,
        "candidate_positive_knee_at_bars": candidate_positive_knee,
        "candidate_positive_knee_is_not_a_statistical_claim": True,
        "interpretation": (
            "positive knee means marginal wall-time per Bar increased beyond the threshold; "
            "a decreasing slope is reported as economies of scale, not a knee"
        ),
        "points": points,
        "segments": segments,
    }


def main() -> int:
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
        sys.stderr.reconfigure(encoding="utf-8", errors="replace")
    parser = argparse.ArgumentParser(description="牵星性能基线驱动器")
    parser.add_argument("--binary", default=None, help="qx-cli 路径（缺省自动探测 target/）")
    parser.add_argument("--sizes", default=",".join(str(s) for s in DEFAULT_SIZES))
    parser.add_argument("--repeats", type=int, default=DEFAULT_REPEATS)
    parser.add_argument("--out", default=str(REPO_ROOT / "benchmarks" / "results" / "baseline.json"))
    parser.add_argument("--json", action="store_true", help="只输出 JSON")
    args = parser.parse_args()

    binary = find_binary(args.binary)
    sizes = [int(token) for token in args.sizes.split(",") if token.strip()]
    if not sizes or any(size <= 0 for size in sizes):
        parser.error("--sizes 必须是一个或多个正整数")
    if len(set(sizes)) != len(sizes):
        parser.error("--sizes 中的规模不能重复")
    if args.repeats < 2:
        parser.error("--repeats 至少为 2，才能报告重复稳定性")
    report = {
        "schema_version": 1,
        "hardware": hardware_fingerprint(),
        "binary": str(binary),
        "sizes": sizes,
        "repeats": args.repeats,
        "scenario": "backtest builtin sma_cross <frame_N>",
        "measured": "end-to-end wall clock (parse + validate + event-driven backtest + summary)",
        "not_measured": ["kernel append/refresh/replay/read-model/recovery per-stage p50/p95"],
        "rows": [],
    }
    with tempfile.TemporaryDirectory(prefix="qx-baseline-", dir=REPO_ROOT / "target") as workdir:
        for size in sizes:
            frame = Path(workdir) / f"frame-{size}.json"
            generate_frame(size, frame)
            # 预热一次，再计时；预热结果不参与统计。
            _, warm_hash, warm_rc = run_once(binary, frame)
            if warm_rc != 0:
                print(f"[FAIL] 预热回测退出码 {warm_rc}（N={size}）", file=sys.stderr)
                return 1
            samples, hashes = [], []
            for _ in range(args.repeats):
                elapsed, result_hash, rc = run_once(binary, frame)
                if rc != 0:
                    print(f"[FAIL] 回测退出码 {rc}（N={size}）", file=sys.stderr)
                    return 1
                samples.append(elapsed)
                hashes.append(result_hash)
            deterministic = len(set(hashes)) == 1 and warm_hash == hashes[0]
            report["rows"].append(
                {
                    "bars": size,
                    "repeats": args.repeats,
                    "p50_ms": round(percentile(samples, 0.50), 3),
                    "p95_ms": round(percentile(samples, 0.95), 3),
                    "min_ms": round(min(samples), 3),
                    "max_ms": round(max(samples), 3),
                    "mean_ms": round(statistics.fmean(samples), 3),
                    "result_hash": hashes[0],
                    "deterministic_across_repeats": deterministic,
                }
            )
            if not deterministic or not hashes[0]:
                print(f"[FAIL] N={size} 的 result_hash 缺失或重复运行不稳定", file=sys.stderr)
                return 1

    report["linear_scaling_curve"] = scaling_curve(report["rows"])

    output_path = Path(args.out)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(
        json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True), encoding="utf-8"
    )

    if args.json:
        print(json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True))
        return 0

    print(f"binary: {report['binary']}")
    print(f"scenario: {report['scenario']}")
    print()
    print("| bars | p50 (ms) | p95 (ms) | min | max | result_hash | 确定性 |")
    print("| ---: | ---: | ---: | ---: | ---: | --- | :---: |")
    for row in report["rows"]:
        print(
            f"| {row['bars']} | {row['p50_ms']} | {row['p95_ms']} | {row['min_ms']} | "
            f"{row['max_ms']} | `{row['result_hash']}` | "
            f"{'PASS' if row['deterministic_across_repeats'] else 'FAIL'} |"
        )
    curve = report["linear_scaling_curve"]
    print()
    print("相邻规模边际成本（端到端墙钟，含进程启动与 JSON 解析）")
    print("| bars | p50 delta (ms) | marginal (us/Bar) | slope change |")
    print("| ---: | ---: | ---: | ---: |")
    for segment in curve["segments"]:
        change = segment["slope_change_pct_vs_previous_segment"]
        change_label = "n/a" if change is None else f"{change:+.1f}%"
        print(
            f"| {segment['from_bars']} → {segment['to_bars']} | {segment['delta_p50_ms']} | "
            f"{segment['marginal_p50_us_per_bar']} | {change_label} |"
        )
    candidate = curve["candidate_positive_knee_at_bars"]
    print(f"candidate positive knee: {candidate if candidate is not None else 'none observed'}")
    print()
    try:
        shown_path = output_path.resolve().relative_to(REPO_ROOT).as_posix()
    except ValueError:
        shown_path = str(output_path.resolve())
    print(f"written: {shown_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
