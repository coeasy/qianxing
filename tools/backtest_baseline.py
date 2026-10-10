#!/usr/bin/env python3
"""回测基线采集（T0-4）：把「规范回测」的夹具身份、产物摘要、耗时与内存冻结成一份机读基线。

## 为什么要有这条基线

`tools/backtest_acceptance.py` 证明的是「同一输入两次跑结果相等」——它**不记录**在这台机器上
跑一轮要多久、占多少内存。没有这份读数，「性能优化」与「性能回归」都无从比较，而计划 §6 的
验收矩阵明确**不接受**「未测的高性能」，T1-5 也要求「先有前后可比数据再谈热点修复」。

## 冻的是什么（夹具按身份冻结，不复制生成物）

夹具 = 仓库自己的 `quickstart` 生成的那套输入。它是**确定性**的：同版本、同输入必得同一
`data_fingerprint` 与同一 `result_hash`（`backtest_acceptance.py` 已把这条钉死）。

**刻意不把生成物复制进仓库**：那会造出第二份真值源，与模板静默漂移——本仓对此的既有口径是
「一份文本，不是两份手抄」（见 `qx-protocol` 用 `include_str!` 发布契约那一处）。所以夹具按
**身份**冻结：逐输入文件 sha256 + `data_fingerprint` + `config_fingerprint`，并与
`maturity/backtest_acceptance.yaml` 的 `result_hash` / `data_fingerprint` 交叉核对——
夹具一漂移，两份记录必须一起动，否则 `backtest_baseline_check` 当场红。

## 数字的性质（别把它读成质量指标）

耗时与内存**与机器有关**：CPU、磁盘、杀软、并行负载都会改变它。本文件记录的是「某一台机器
某一轮」的读数，连同环境（os / arch / python / binary / profile）一起落盘，**只供同机前后对比**。
门禁**不比对数值**——拿数值当判据只会逼人写死一台机器（与 `backtest_acceptance.py` 刻意不做
「记录是否过时」判据同一口径）。

## 内存怎么量

峰值 RSS 由操作系统报，不是采样估算：

* Windows —— `GetProcessMemoryInfo` 的 `PeakWorkingSetSize`（内核维护的真峰值）；
* Linux —— `/proc/<pid>/status` 的 `VmHWM`；
* 其他平台 —— 记 `null` 并在 `method` 里如实写明 `unsupported_platform`（不猜、不填零）。

读数在子进程存活期间按 `sampling_interval_ms` 轮询取到，进程退出后无法再读（句柄失效），
所以轮询到退出为止、取最后一次读数。

退出码：0 全过；2 有判据不成立。判据不成立时**不写**基线（fail closed）——留一份
「看起来通过了」的基线比没有基线更坏。
"""

from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

WORKSPACE = Path(__file__).resolve().parents[1]
RECORD = WORKSPACE / "maturity" / "backtest_baseline.yaml"
ACCEPTANCE = WORKSPACE / "maturity" / "backtest_acceptance.yaml"

EXIT_FAILURE = 2
LEG_BUDGET_SECONDS = 300
SAMPLING_INTERVAL_SECONDS = 0.02

# 与 backtest_acceptance.py 同一份：这些环境变量在回测轨上必须缺席。
CREDENTIAL_ENV_PREFIXES = ("QX_BINANCE_", "QX_CCXT_", "QX_OKX_")

RUN_MANIFEST_PATH = re.compile(r"\[RunManifest\]\s+path=(\S+)")
ARTIFACTS_LINE = re.compile(r"\[Artifacts\]\s+summary=(\S+)\s+equity=(\S+)\s+fills=(\S+)")
BACKTEST_RESULT_HASH = re.compile(r"\[Strategy · Backtest\].*?result_hash=(\S+)")

# 夹具的输入面：`backtest` 吃的那三份 + 数据集清单（数据本身由 `data_fingerprint` 覆盖）。
FIXTURE_INPUTS = (
    "qianxing.runtime.json",
    "qianxing.bar-frame.example.json",
    "qianxing.binance.spot.spec.json",
    "qianxing.dataset-bundle.bar-frame.example.json",
)
ARTIFACT_KINDS = ("equity", "fills", "run_manifest", "summary")
FIXTURE_NAME = "quickstart-bar-frame-btcusdt"


def default_binary() -> Path:
    for candidate in (
        WORKSPACE / ".cargo-target" / "debug" / "qx-cli.exe",
        WORKSPACE / ".cargo-target" / "debug" / "qx-cli",
        WORKSPACE / "target" / "debug" / "qx-cli.exe",
        WORKSPACE / "target" / "debug" / "qx-cli",
    ):
        if candidate.is_file():
            return candidate
    raise SystemExit("找不到被测 binary：先 `cargo build -p qx-cli`，或用 --binary 指定")


def scrubbed_env() -> dict[str, str]:
    return {
        name: value
        for name, value in os.environ.items()
        if not name.startswith(CREDENTIAL_ENV_PREFIXES)
    }


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def build_profile(binary: Path) -> str:
    parts = {part.lower() for part in binary.parts}
    if "release" in parts:
        return "release"
    if "debug" in parts:
        return "debug"
    return "unknown"


# ---- 峰值 RSS：由操作系统报，不靠采样估算 -------------------------------------------------


def _windows_peak_rss(pid: int) -> int | None:
    class ProcessMemoryCounters(ctypes.Structure):
        _fields_ = [
            ("cb", ctypes.c_ulong),
            ("PageFaultCount", ctypes.c_ulong),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
        ]

    process_query_limited_information = 0x1000
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    psapi = ctypes.WinDLL("psapi", use_last_error=True)
    handle = kernel32.OpenProcess(process_query_limited_information, False, pid)
    if not handle:
        return None
    try:
        counters = ProcessMemoryCounters()
        counters.cb = ctypes.sizeof(counters)
        if not psapi.GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb):
            return None
        return int(counters.PeakWorkingSetSize)
    finally:
        kernel32.CloseHandle(handle)


def _linux_peak_rss(pid: int) -> int | None:
    try:
        text = Path(f"/proc/{pid}/status").read_text(encoding="utf-8")
    except OSError:
        return None
    for line in text.splitlines():
        if line.startswith("VmHWM:"):
            parts = line.split()
            if len(parts) >= 2 and parts[1].isdigit():
                return int(parts[1]) * 1024
    return None


def peak_rss_bytes(pid: int) -> tuple[int | None, str]:
    """返回 `(峰值字节数, 读数方法)`；读不到就如实记 `None` + 原因，不猜、不填零。"""
    if sys.platform == "win32":
        return _windows_peak_rss(pid), "win32_GetProcessMemoryInfo_PeakWorkingSetSize"
    if sys.platform.startswith("linux"):
        return _linux_peak_rss(pid), "linux_proc_status_VmHWM"
    return None, "unsupported_platform"


def run_measured(argv: list[str], cwd: Path) -> tuple[int, str, float, int | None, str]:
    """跑一条命令，量它的墙钟耗时与子进程峰值 RSS。

    stdout 落**临时文件**而不是管道：本函数在子进程存活期间只轮询内存、不读管道，
    管道缓冲写满就会把子进程堵死（经典 deadlock）——回测输出现在不大，但这条腿要长期用。
    """
    started = time.perf_counter()
    peak: int | None = None
    method = "unsupported_platform"
    with tempfile.TemporaryFile() as sink:
        process = subprocess.Popen(
            argv,
            cwd=str(cwd),
            stdout=sink,
            stderr=subprocess.STDOUT,
            env=scrubbed_env(),
        )
        while process.poll() is None:
            value, method = peak_rss_bytes(process.pid)
            if value is not None and (peak is None or value > peak):
                peak = value
            time.sleep(SAMPLING_INTERVAL_SECONDS)
        elapsed = time.perf_counter() - started
        sink.seek(0)
        output = sink.read().decode("utf-8", errors="replace")
    return process.returncode, output, elapsed, peak, method


def snapshot_from_output(output: str, project: Path) -> dict[str, Any]:
    manifest = RUN_MANIFEST_PATH.search(output)
    artifacts = ARTIFACTS_LINE.search(output)
    result_hash = BACKTEST_RESULT_HASH.search(output)
    if not manifest or not artifacts or not result_hash:
        raise SystemExit(f"`backtest` 的输出缺少可解析的产物行：\n{output}")
    manifest_path = Path(manifest.group(1))
    paths = {
        "summary": Path(artifacts.group(1)),
        "equity": Path(artifacts.group(2)),
        "fills": Path(artifacts.group(3)),
        "run_manifest": manifest_path,
    }
    record_path = manifest_path.with_name(
        manifest_path.name.replace(".run.json", ".record.json")
    )
    for path in (*paths.values(), record_path):
        if not path.is_file():
            raise SystemExit(f"输出点名的产物不在盘：{path}")
    record = json.loads(record_path.read_text(encoding="utf-8"))
    summary = json.loads(paths["summary"].read_text(encoding="utf-8"))
    return {
        "result_hash": summary.get("result_hash") or result_hash.group(1),
        "data_fingerprint": record.get("input_digest"),
        "config_fingerprint": record.get("config_fingerprint"),
        "artifacts": {kind: sha256_file(path) for kind, path in paths.items()},
    }


def collect(binary: Path, label: str) -> dict[str, Any]:
    root = Path(os.environ.get("TEMP", "/tmp")) / f"qianxing-backtest-baseline-{label}"
    if root.exists():
        shutil.rmtree(root)
    root.mkdir(parents=True)
    project = root / "project"

    code, output, quickstart_seconds, _, _ = run_measured(
        [str(binary), "quickstart", str(project)], root
    )
    if code != 0:
        raise SystemExit(f"quickstart 以 {code} 退出：\n{output}")

    code, output, backtest_seconds, peak_rss, method = run_measured(
        [
            str(binary),
            "backtest",
            str(project / "qianxing.runtime.json"),
            str(project / "qianxing.bar-frame.example.json"),
            str(project / "qianxing.binance.spot.spec.json"),
        ],
        root,
    )
    if code != 0:
        raise SystemExit(f"backtest 以 {code} 退出：\n{output}")

    snapshot = snapshot_from_output(output, project)
    snapshot["inputs"] = {
        name: sha256_file(project / name)
        for name in FIXTURE_INPUTS
        if (project / name).is_file()
    }
    snapshot["quickstart_seconds"] = quickstart_seconds
    snapshot["backtest_seconds"] = backtest_seconds
    snapshot["peak_rss_bytes"] = peak_rss
    snapshot["rss_method"] = method
    return snapshot


def acceptance_digest(field: str) -> str | None:
    if not ACCEPTANCE.is_file():
        return None
    for line in ACCEPTANCE.read_text(encoding="utf-8").splitlines():
        if line.startswith(f"{field}:"):
            return line.split(":", 1)[1].strip()
    return None


def verify(snapshot: dict[str, Any]) -> list[str]:
    problems: list[str] = []
    if not snapshot["result_hash"]:
        problems.append("result_hash 为空：摘要没有交出确定性指纹")
    if not snapshot["data_fingerprint"]:
        problems.append("data_fingerprint 为空：夹具身份没有交出摘要")
    if set(snapshot["artifacts"]) != set(ARTIFACT_KINDS):
        problems.append(f"产物种类不全：{sorted(snapshot['artifacts'])}")
    if set(snapshot["inputs"]) != set(FIXTURE_INPUTS):
        problems.append(f"夹具输入面不全：{sorted(snapshot['inputs'])}")
    if snapshot["backtest_seconds"] <= 0:
        problems.append("backtest 耗时非正：计时没接上")
    # 夹具身份与回测轨验收记录交叉核对——夹具一漂移，两份记录必须一起动。
    expected_hash = acceptance_digest("result_hash")
    if expected_hash and snapshot["result_hash"] != expected_hash:
        problems.append(
            f"result_hash 与 backtest_acceptance.yaml 不等：{snapshot['result_hash']!r} vs {expected_hash!r}"
            "（同一夹具必须同一结果；不等说明夹具漂移了，两份记录要一起改）"
        )
    expected_fp = acceptance_digest("data_fingerprint")
    if expected_fp and snapshot["data_fingerprint"] != expected_fp:
        problems.append(
            f"data_fingerprint 与 backtest_acceptance.yaml 不等："
            f"{snapshot['data_fingerprint']!r} vs {expected_fp!r}"
        )
    return problems


def render(snapshot: dict[str, Any], binary: Path) -> str:
    inputs = "\n".join(
        f"    {name}: {digest}" for name, digest in sorted(snapshot["inputs"].items())
    )
    artifacts = "\n".join(
        f"  {kind}: {digest}" for kind, digest in sorted(snapshot["artifacts"].items())
    )
    rss = "null" if snapshot["peak_rss_bytes"] is None else str(snapshot["peak_rss_bytes"])
    return (
        "# 回测基线（T0-4）——由 tools/backtest_baseline.py 生成，请勿手改。\n"
        "#\n"
        "# 这份记录的是「某一台机器某一轮」的读数，**与机器有关，不是质量指标**：\n"
        "# 耗时与内存只供同机前后对比，跨机不可比。门禁不比对数值，只核形状、\n"
        "# 「脚本真在测量」与「夹具身份与 maturity/backtest_acceptance.yaml 一致」。\n"
        "schema_version: 1\n"
        "kind: backtest-baseline\n"
        "generated_by: tools/backtest_baseline.py\n"
        f"generated_at_unix: {int(time.time())}\n"
        "environment:\n"
        f"  os: {platform.system()}\n"
        f"  arch: {platform.machine()}\n"
        f"  python: {platform.python_version()}\n"
        f"  binary: {binary.name}\n"
        f"  profile: {build_profile(binary)}\n"
        "fixture:\n"
        f"  name: {FIXTURE_NAME}\n"
        f"  data_fingerprint: {snapshot['data_fingerprint']}\n"
        f"  config_fingerprint: {snapshot['config_fingerprint']}\n"
        "  inputs:\n"
        f"{inputs}\n"
        f"result_hash: {snapshot['result_hash']}\n"
        "artifacts:\n"
        f"{artifacts}\n"
        "timing:\n"
        f"  quickstart_seconds: {snapshot['quickstart_seconds']:.3f}\n"
        f"  backtest_seconds: {snapshot['backtest_seconds']:.3f}\n"
        "memory:\n"
        f"  peak_rss_bytes: {rss}\n"
        f"  method: {snapshot['rss_method']}\n"
        f"  sampling_interval_ms: {int(SAMPLING_INTERVAL_SECONDS * 1000)}\n"
        "notes: >-\n"
        "  夹具按身份冻结（逐输入文件 sha256 + data_fingerprint），不把生成物复制进仓库——\n"
        "  那会造出第二份真值源与模板静默漂移。耗时与内存与机器有关，跨机不可比，\n"
        "  门禁不比对数值；它只保证「夹具没漂、脚本真在测量、产物摘要在盘」。\n"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description="回测基线采集（T0-4）")
    parser.add_argument("--binary", type=Path, default=None)
    parser.add_argument("--dry-run", action="store_true", help="只跑判据、不写基线")
    args = parser.parse_args()
    binary = args.binary or default_binary()

    snapshot = collect(binary, "a")
    problems = verify(snapshot)
    if problems:
        print("回测基线采集未通过（fail closed，不写记录）：")
        for problem in problems:
            print(f"  x {problem}")
        return EXIT_FAILURE

    rss = snapshot["peak_rss_bytes"]
    print(f"回测基线通过：result_hash={snapshot['result_hash']}")
    print(
        f"  夹具 data_fingerprint={snapshot['data_fingerprint']}；产物 {len(snapshot['artifacts'])} 份"
    )
    print(
        f"  耗时 quickstart={snapshot['quickstart_seconds']:.3f}s "
        f"backtest={snapshot['backtest_seconds']:.3f}s"
    )
    print(f"  内存峰值 RSS={rss}（{snapshot['rss_method']}）")
    if not args.dry_run:
        RECORD.write_text(render(snapshot, binary), encoding="utf-8", newline="\n")
        print(f"已写入 {RECORD.relative_to(WORKSPACE).as_posix()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
