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
* 每次运行都会在 `--evidence-root`（默认 `maturity/evidence/testnet`）下留下一个
  UTC 时间戳目录与 `result.json`（阶段退出码、耗时、outcome、是否允许翻转
  `sandbox_tested`）。目录不自动删除：`maturity/capabilities.yaml` 的
  `sandbox_tested` 只能凭一份 `outcome=pass` 的结果包翻转。
"""

from __future__ import annotations

import argparse
import copy
import json
import os
import subprocess
import sys
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

# V10 §6 P3：每段验收都必须留下**带时间戳的结果包**，否则 `sandbox_tested` 永远
# 没有可核对的证据来源。默认落在仓库内的证据目录，而不是跑完就删的临时目录。
DEFAULT_EVIDENCE_ROOT = WORKSPACE / "maturity" / "evidence" / "testnet"

# 逐步记录：包名 → 退出码 / 耗时 / 输出末行，供结果包与人工复核使用。
STAGE_RECORDS: list[dict[str, Any]] = []


def utc_stamp() -> str:
    return time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())


def write_result_package(
    workdir: Path,
    binary: Path,
    instrument: str,
    send_orders: bool,
    credentials_present: bool,
    outcome: str,
    started_at: float,
    detail: str = "",
) -> Path:
    """落一份机器可读的结果包；它是 `capabilities.yaml` 翻 `sandbox_tested` 的唯一依据。"""
    package = {
        "schema_version": 1,
        "kind": "binance-testnet-acceptance",
        "outcome": outcome,
        "started_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(started_at)),
        "finished_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "duration_ms": int((time.time() - started_at) * 1000),
        "binary": str(binary),
        "instrument": instrument,
        "send_orders": send_orders,
        "credentials_present": credentials_present,
        "stages": STAGE_RECORDS,
        "detail": detail,
        "sandbox_tested_flip": {
            "allowed": outcome == "pass",
            "rule": "只有 outcome=pass 且全部阶段退出码为 0 才允许把 capabilities.yaml 的"
            " sandbox_tested 置 true；skipped/fail 一律保持 false，且必须连同本包一起归档",
        },
    }
    path = workdir / "result.json"
    path.write_text(json.dumps(package, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    return path

# Windows 控制台默认 GBK，中文日志需要显式切到 UTF-8。
for stream in (sys.stdout, sys.stderr):
    if hasattr(stream, "reconfigure"):
        stream.reconfigure(encoding="utf-8", errors="replace")


def log(message: str) -> None:
    print(f"[testnet-acceptance] {message}", flush=True)


def run(binary: Path, args: list[str]) -> subprocess.CompletedProcess[str]:
    command = [str(binary), *args]
    log("$ " + " ".join(command))
    started = time.time()
    # Windows 控制台默认 GBK，而 qx-cli 的中文诊断是 UTF-8。
    result = subprocess.run(command, capture_output=True, text=True, encoding="utf-8")
    output = (result.stdout + result.stderr).strip().splitlines()
    STAGE_RECORDS.append(
        {
            "stage": args[0] if args else "unknown",
            "argv": args,
            "exit_code": result.returncode,
            "duration_ms": int((time.time() - started) * 1000),
            "output_tail": output[-3:],
        }
    )
    return result


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
    parser.add_argument(
        "--workdir",
        type=Path,
        help="验收产物目录；默认在 --evidence-root 下按 UTC 时间戳新建，且不会自动删除",
    )
    parser.add_argument(
        "--evidence-root",
        type=Path,
        default=DEFAULT_EVIDENCE_ROOT,
        help="结果包根目录，默认 maturity/evidence/testnet",
    )
    parser.add_argument("--instrument", default="BTCUSDT.BINANCE")
    parser.add_argument("--allow-skip", action="store_true", help="缺少凭据时以退出码 0 结束")
    parser.add_argument("--send-orders", action="store_true", help="允许向测试网络发送真实订单")
    args = parser.parse_args()
    started_at = time.time()

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

    # 验收目录不再跑完即删：结果包必须留在仓库内，人工与 CI 都能事后核对。
    workdir = args.workdir
    if workdir is None:
        scope = "orders" if args.send_orders else "dryrun"
        workdir = args.evidence_root / f"{utc_stamp()}-{scope}"
    workdir.mkdir(parents=True, exist_ok=True)
    config = build_acceptance_config(workdir)
    missing = [name for name in (KEY_ENV, SECRET_ENV) if not os.environ.get(name)]
    outcome, detail = "fail", "未到达终态"
    try:
        assert_fail_closed(binary, config)

        if missing:
            outcome = "skipped"
            detail = f"缺少凭据 {', '.join(missing)}：只执行了离线前两段"
            log(f"{detail}（skip 而不是假通过）")
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
        if any(
            record["stage"] != "live-check" and record["exit_code"] != 0
            for record in STAGE_RECORDS
        ):
            raise SystemExit("存在非零退出码阶段，不能判定为验收通过")
        outcome, detail = "pass", ""
        log(f"验收通过：产物见 {workdir}")
        return 0
    except SystemExit as error:
        if isinstance(error.code, int):
            outcome, detail = "fail", f"子进程以退出码 {error.code} 结束"
        else:
            outcome, detail = "fail", str(error.code)
        raise
    finally:
        package = write_result_package(
            workdir=workdir,
            binary=binary,
            instrument=args.instrument,
            send_orders=args.send_orders,
            credentials_present=not missing,
            outcome=outcome,
            started_at=started_at,
            detail=detail,
        )
        log(f"结果包（outcome={outcome}）：{package}")


if __name__ == "__main__":
    try:
        sys.exit(main())
    except SystemExit as exit_signal:
        # 文本异常统一按“验收失败”退出码返回，CI 据此阻断而不是误判为跳过。
        if isinstance(exit_signal.code, str):
            print(f"[testnet-acceptance] 验收中止：{exit_signal.code}", file=sys.stderr)
            sys.exit(EXIT_FAILURE)
        raise
