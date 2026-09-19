#!/usr/bin/env python3
"""Binance Spot 测试网络验收脚本（fail-closed）。

链路：runtime-check → live-check 安全断言 → 公共探测 → 私有探测 → 幂等下单 →
重复下单拒绝 → 断线重启恢复 → 对账 → 终态快照。

设计约束：
* 缺少 `QX_BINANCE_TESTNET_API_KEY` / `QX_BINANCE_TESTNET_API_SECRET` 时，只运行
  不需要凭据的离线前两段，然后以退出码 3 结束（skip 而不是假通过）。
  `--allow-skip` 才会把该情形折叠为退出码 0，供 CI 的“未验收”分支使用。
* 只有显式 `--send-orders` 才会把命令的 `dry_run` 置为 false；默认全程不向
  交易所发送真实订单。
"""

from __future__ import annotations

import argparse
import copy
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

WORKSPACE = Path(__file__).resolve().parents[1]
BASE_CONFIG = WORKSPACE / "deploy" / "qianxing.runtime.binance-testnet.example.json"
EXECUTION_WORKER = "binance-execution-main"
RECONCILE_WORKER = "reconciler-main"
KEY_ENV = "QX_BINANCE_TESTNET_API_KEY"
SECRET_ENV = "QX_BINANCE_TESTNET_API_SECRET"

EXIT_FAILURE = 2
EXIT_SKIPPED = 3

# Windows 控制台默认 GBK，中文日志需要显式切到 UTF-8。
for stream in (sys.stdout, sys.stderr):
    if hasattr(stream, "reconfigure"):
        stream.reconfigure(encoding="utf-8", errors="replace")


def log(message: str) -> None:
    print(f"[testnet-acceptance] {message}", flush=True)


def run(binary: Path, args: list[str]) -> subprocess.CompletedProcess[str]:
    command = [str(binary), *args]
    log("$ " + " ".join(command))
    # Windows 控制台默认 GBK，而 qx-cli 的中文诊断是 UTF-8。
    return subprocess.run(command, capture_output=True, text=True, encoding="utf-8")


def fail(step: str, result: subprocess.CompletedProcess[str]) -> None:
    detail = (result.stderr or result.stdout).strip()
    raise SystemExit(f"验收步骤 `{step}` 失败（退出码 {result.returncode}）：{detail}")


def next_to_base_config(value: str) -> str:
    """调度器文件按配置文件所在目录解析，复制配置后需要改写成绝对路径。"""
    path = Path(value)
    return str(path if path.is_absolute() else (BASE_CONFIG.parent / path).resolve())


def from_workspace(value: str) -> str:
    """`instrument_spec_path` 这类冻结规格按仓库根目录解析。"""
    path = Path(value)
    return str(path if path.is_absolute() else (WORKSPACE / path).resolve())


def build_acceptance_config(workdir: Path) -> Path:
    """复制验收配置到工作目录，只改写状态路径以把运行事实隔离在验收目录内。"""
    payload: dict[str, Any] = copy.deepcopy(
        json.loads(BASE_CONFIG.read_text(encoding="utf-8"))
    )
    payload["storage"]["data_dir"] = str((workdir / "acceptance-data").resolve())
    scheduler = payload.get("scheduler") or {}
    if "jobs_path" in scheduler:
        scheduler["jobs_path"] = next_to_base_config(scheduler["jobs_path"])
    for key in ("state_path", "job_queue_path"):
        if key in scheduler:
            scheduler[key] = str(workdir / scheduler[key])
    for worker in payload.get("workers", []):
        for key in ("instrument_spec_path", "market_spec_path"):
            if worker.get(key):
                worker[key] = from_workspace(worker[key])
    strategy = payload.get("strategy") or {}
    if strategy.get("target_snapshot_path"):
        strategy["target_snapshot_path"] = str(workdir / strategy["target_snapshot_path"])
    path = workdir / "qianxing.runtime.binance-testnet.acceptance.json"
    path.write_text(json.dumps(payload, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    return path


def build_command(client_id: int, dry_run: bool, instrument: str) -> dict[str, Any]:
    symbol, venue = instrument.split(".")
    order = {
        "client_id": client_id,
        "instrument": {"symbol": symbol, "venue": venue},
        "side": "Buy",
        "qty": 1_000_000_000,
        "limit": 100_000_000_000,
        "status": "Submitted",
        "filled": 0,
        "account_id": "main",
        "trace": None,
    }
    return {
        "command_id": client_id,
        "request_id": f"testnet-acceptance-{client_id}-{int(time.time())}",
        "operator_id": "binance-testnet-acceptance",
        "reason": "Binance testnet acceptance drill",
        "kind": "SubmitOrder",
        "target": str(client_id),
        "payload": {"order_json": json.dumps(order, separators=(",", ":"))},
        "permission": "Trading",
        "dry_run": dry_run,
    }


def json_step(binary: Path, step: str, args: list[str]) -> dict[str, Any]:
    result = run(binary, args)
    if result.returncode != 0:
        fail(step, result)
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise SystemExit(f"验收步骤 `{step}` 输出不是 JSON：{error}") from error


def assert_fail_closed(binary: Path, config: Path) -> None:
    """离线安全门禁：校验与 live-check 本身都不得触网或发单。"""
    result = run(binary, ["runtime-check", str(config), "--json"])
    if not result.stdout.strip():
        fail("runtime-check", result)
    check = json.loads(result.stdout)
    if not check.get("ok") or result.returncode != 0:
        raise SystemExit(
            f"验收配置未通过 runtime-check（退出码 {result.returncode}）："
            f"{check.get('failures') or result.stderr.strip()}"
        )
    live = run(binary, ["live-check", str(config), "--json"])
    if not live.stdout.strip():
        fail("live-check", live)
    if live.returncode == 0:
        log("live-check 通过：交易前置条件齐备")
    payload = json.loads(live.stdout)
    for field in ("network_accessed", "orders_sent"):
        if payload.get(field) is not False:
            raise SystemExit(f"验收步骤 `live-check` 破坏了 `{field} is False` 契约：{payload}")
    log("live-check 已确认只读校验：network_accessed=False orders_sent=False")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, help="qx-cli 可执行文件路径")
    parser.add_argument("--workdir", type=Path, help="验收产物目录，默认临时目录")
    parser.add_argument("--instrument", default="BTCUSDT.BINANCE")
    parser.add_argument("--allow-skip", action="store_true", help="缺少凭据时以退出码 0 结束")
    parser.add_argument("--send-orders", action="store_true", help="允许向测试网络发送真实订单")
    args = parser.parse_args()

    # 运行时示例里的 scheduler.jobs_path 等按进程工作目录解析，统一切到仓库根。
    os.chdir(WORKSPACE)
    binary = args.binary
    if binary is None:
        binary = next(
            (
                candidate
                for candidate in (
                    WORKSPACE / "target" / "debug" / "qx-cli.exe",
                    WORKSPACE / "target" / "debug" / "qx-cli",
                )
                if candidate.exists()
            ),
            None,
        )
        if binary is None:
            raise SystemExit(
                "找不到 target/debug 下的 qx-cli；先执行 "
                "`cargo build -p qx-cli --features sqlite,postgres` 或用 --binary 指定"
            )
    binary = Path(binary).resolve()

    owned = args.workdir is None
    workdir = args.workdir or Path(tempfile.mkdtemp(prefix="qianxing-testnet-"))
    workdir.mkdir(parents=True, exist_ok=True)
    config = build_acceptance_config(workdir)
    try:
        assert_fail_closed(binary, config)

        missing = [name for name in (KEY_ENV, SECRET_ENV) if not os.environ.get(name)]
        if missing:
            log(f"缺少凭据 {', '.join(missing)}：跳过需要交易所参与的步骤")
            return 0 if args.allow_skip else EXIT_SKIPPED

        dry_run = not args.send_orders
        client_id = int(time.time())
        command_path = workdir / "submit-order.json"
        command_path.write_text(
            json.dumps(build_command(client_id, dry_run, args.instrument), indent=2) + "\n",
            encoding="utf-8",
        )

        probe = run(binary, ["binance-public-probe", "testnet", args.instrument])
        if probe.returncode != 0:
            fail("binance-public-probe", probe)
        private = run(binary, ["binance-private-probe", str(config), EXECUTION_WORKER])
        if private.returncode != 0:
            fail("binance-private-probe", private)

        submit = run(binary, ["binance-submit-order", str(config), EXECUTION_WORKER, str(command_path)])
        if submit.returncode != 0:
            fail("binance-submit-order", submit)
        log(f"下单步骤完成（dry_run={dry_run}）：{submit.stdout.strip().splitlines()[-1:]}")

        duplicate = run(
            binary, ["binance-submit-order", str(config), EXECUTION_WORKER, str(command_path)]
        )
        combined = duplicate.stdout + duplicate.stderr
        if duplicate.returncode == 0 or "幂等" not in combined:
            raise SystemExit(
                "重复提交同一 request_id 必须被控制面拒绝（未知结果不得二次发单）："
                f"退出码 {duplicate.returncode} 输出 {combined.strip()}"
            )
        log("重复 request_id 被控制面拒绝：未知结果不会自动重发")

        restart = run(binary, ["binance-worker", str(config), EXECUTION_WORKER, "--once"])
        if restart.returncode != 0:
            fail("binance-worker 恢复", restart)
        reconcile = run(binary, ["reconcile", str(config), RECONCILE_WORKER])
        if reconcile.returncode != 0:
            fail("reconcile 对账", reconcile)
        status = json_step(binary, "status", ["status", str(config), "--json"])
        (workdir / "status.json").write_text(json.dumps(status, indent=2), encoding="utf-8")
        log(f"验收通过：产物见 {workdir}")
        return 0
    finally:
        if owned:
            shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except SystemExit as exit_signal:
        # 文本异常统一按“验收失败”退出码返回，CI 据此阻断而不是误判为跳过。
        if isinstance(exit_signal.code, str):
            print(f"[testnet-acceptance] 验收中止：{exit_signal.code}", file=sys.stderr)
            sys.exit(EXIT_FAILURE)
        raise
