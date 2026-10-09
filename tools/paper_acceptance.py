#!/usr/bin/env python3
"""Paper 轨验收脚本（M5' 的可本地验收子项）：**一条交易所凭据都不用**跑完整条 Paper 主链路。

## 为什么单独有一条「Paper 轨」

`maturity/evidence/README.md` 把验收分成两条互不相混的轨：**实盘轨**（需要真实交易所凭据，
证据在 `maturity/evidence/testnet|production/`）与**本仓主用法**（回测 + Paper 闭环，
一条凭据都不用）。回测轨已由 `tools/backtest_acceptance.py` 落成
`maturity/backtest_acceptance.yaml`；本脚本补上另一半——**Paper 主链路**。

Paper 与回测的关键差别：回测是纯计算、可与路径无关地逐字节复现；Paper 走的是
**控制面受理 → 执行者撮合 → 账簿落账**这条真链路，它写 EventLog、写审计链、写命令队列。
所以这条轨能验收的**不是**「重跑逐字节相等」，而是下面这些只有真跑过才成立的性质。

## 判据

**一条主链路真跑通**——`qx-cli paper-check` 在全新目录里退 0，且它自己印出的
`[Paper · E2E]` 行报「本轮新增」严格为正（`+0` 是当日调度跳过、复用上一轮既有事实，
那不是一次端到端重跑）。

**事实面独立复核**（不信 stdout，直接读产物）：

* `*-events.json`：`OrderSubmitted == 1`、`Filled == 1`、`LedgerApplied >= 1`、
  `AccountCashflow >= 1`；
* `audit.json`：存在 `status == "Executed"` 且 `result_code` 以 `PAPER_EXECUTED` 开头的记录，
  且**哈希链自洽**（每条的 `previous_hash` 等于上一条的 `entry_hash`）；
* `control-queue/`：跑完**没有**未确认命令。

**重跑不重复下单**——同一目录再跑一遍 `paper-check`：退出 0，且 `OrderSubmitted` 仍为 1
（订单总数一格不涨）。这正是 `maturity/evidence/README.md` 硬约束 #4「重跑不重复下单」
在 Paper 轨上的可执行形态。

**两个独立目录语义相等**——两处互不相干的项目各跑一轮，事实面逐格相等
（订单数、成交数、账簿分录数、审计状态序列）。

**无需凭据、不触网、不下真单**——子进程环境里的凭据变量被显式摘掉，且 `status --json`
自报 `network_accessed=false` / `orders_sent=false` / `environment=paper`。

退出码：0 全过；2 有判据不成立。任一判据不成立时**不写**验收记录（fail closed）——
留一份"看起来通过了"的记录比没有记录更坏。
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

WORKSPACE = Path(__file__).resolve().parents[1]
RECORD = WORKSPACE / "maturity" / "paper_acceptance.yaml"
DEFAULT_RUNTIME = WORKSPACE / "deploy" / "qianxing.runtime.paper-strategy.example.json"

EXIT_FAILURE = 2
LEG_BUDGET_SECONDS = 300

# 与回测轨同一份口径：这些环境变量在 Paper 轨上**必须缺席**——它们的存在会让
# 「不需要凭据」这句话失去证据。`QX_CONSOLE_` 是 M4' 同源控制台的引导令牌，
# 同样是凭据类，一并摘掉。
CREDENTIAL_ENV_PREFIXES = ("QX_BINANCE_", "QX_CCXT_", "QX_OKX_", "QX_CONSOLE_")

# `paper-check` 的末行。两个分支（真跑 / 当日零新增）共用这一份正则：
# `orders=N (+M 本轮新增) ledger_entries=K`，其中零新增那一支 M 就是 0。
E2E_LINE = re.compile(
    r"\[Paper · E2E\] scheduler=(\S+) strategy=(\S+) execution=(\S+) "
    r"orders=(\d+) \(\+(\d+) 本轮新增\) ledger_entries=(\d+)"
)

# 事实面要数的几格。名字就是事件枚举的变体名（`kind` 是单键对象）。
ORDER_SUBMITTED = "OrderSubmitted"
FILLED = "Filled"
LEDGER_APPLIED = "LedgerApplied"
ACCOUNT_CASHFLOW = "AccountCashflow"


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
    """去掉一切凭据环境变量后的子进程环境。"""
    return {
        name: value
        for name, value in os.environ.items()
        if not name.startswith(CREDENTIAL_ENV_PREFIXES)
    }


def run(argv: list[str], cwd: Path) -> tuple[int, str]:
    completed = subprocess.run(
        argv,
        cwd=str(cwd),
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        env=scrubbed_env(),
        timeout=LEG_BUDGET_SECONDS,
    )
    return completed.returncode, (completed.stdout or "") + (completed.stderr or "")


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def data_dir_of(runtime: Path) -> Path:
    """运行时配置里的 `storage.data_dir`。相对路径按启动目录解析（与 binary 同口径）。"""
    config = read_json(runtime)
    return Path(config["storage"]["data_dir"])


def event_log_path(root: Path, runtime: Path) -> Path:
    """账户事件日志：按 `<data_dir>/*-events.json` 找，且要求**恰好一份**。

    不硬写文件名：它由 `{venue}-{account}-…-events.json` 拼出，抄一份字面量就等于把
    "这份配置的账户是谁"写死在脚本里，换配置就静默指错文件。
    """
    data_dir = root / data_dir_of(runtime)
    logs = sorted(data_dir.glob("*-events.json"))
    if len(logs) != 1:
        raise SystemExit(f"期望 {data_dir} 下恰好一份 *-events.json，实得 {[p.name for p in logs]}")
    return logs[0]


def count_kinds(log_path: Path) -> dict[str, int]:
    counts: dict[str, int] = {}
    for event in read_json(log_path):
        kind = next(iter(event["kind"]))
        counts[kind] = counts.get(kind, 0) + 1
    return counts


def audit_statuses(root: Path, runtime: Path) -> list[tuple[str, str]]:
    """审计链的 (status, result_code) 序列，并顺带核对哈希链自洽。"""
    audit_path = root / data_dir_of(runtime) / "audit.json"
    records = read_json(audit_path)
    statuses: list[tuple[str, str]] = []
    previous = 0
    for entry in records:
        if entry["previous_hash"] != previous:
            raise SystemExit(
                f"审计链在第 {entry['sequence']} 条断开：previous_hash={entry['previous_hash']} "
                f"≠ 上一条 entry_hash={previous}"
            )
        previous = entry["entry_hash"]
        statuses.append((entry["record"]["status"], entry["record"]["result_code"]))
    return statuses


def pending_commands(root: Path, runtime: Path) -> list[str]:
    queue = root / data_dir_of(runtime) / "control-queue"
    if not queue.is_dir():
        return []
    return sorted(str(path.relative_to(queue)) for path in queue.rglob("*") if path.is_file())


def parse_e2e(output: str) -> dict[str, Any]:
    match = E2E_LINE.search(output)
    if not match:
        raise SystemExit(f"`paper-check` 的输出缺少可解析的 [Paper · E2E] 行：\n{output}")
    return {
        "scheduler": match.group(1),
        "strategy": match.group(2),
        "execution": match.group(3),
        "orders": int(match.group(4)),
        "new_orders": int(match.group(5)),
        "ledger_entries": int(match.group(6)),
    }


def read_status(binary: Path, runtime: Path, cwd: Path) -> dict[str, Any]:
    code, output = run([str(binary), "status", str(runtime), "--json"], cwd)
    if code != 0:
        raise SystemExit(f"`status --json` 以 {code} 退出：\n{output}")
    start, end = output.find("{"), output.rfind("}")
    if start < 0 or end < 0:
        raise SystemExit(f"`status --json` 没有交出 JSON：\n{output}")
    return json.loads(output[start : end + 1])


def paper_check(binary: Path, root: Path) -> dict[str, Any]:
    code, output = run([str(binary), "paper-check"], root)
    if code != 0:
        raise SystemExit(f"paper-check 以 {code} 退出：\n{output}")
    return parse_e2e(output)


def one_leg(binary: Path, runtime: Path, root: Path) -> dict[str, Any]:
    """独立目录里跑一轮 Paper 主链路，再同目录重跑一轮验「不重复下单」。"""
    first = paper_check(binary, root)
    log_path = event_log_path(root, runtime)
    first_kinds = count_kinds(log_path)
    first_audit = audit_statuses(root, runtime)
    first_pending = pending_commands(root, runtime)
    status = read_status(binary, runtime, root)

    second = paper_check(binary, root)
    second_kinds = count_kinds(log_path)

    return {
        "first": first,
        "second": second,
        "kinds": first_kinds,
        "second_kinds": second_kinds,
        "audit": first_audit,
        "pending": first_pending,
        "network_accessed": status.get("network_accessed"),
        "orders_sent": status.get("orders_sent"),
        "environment": status.get("environment"),
    }


def compare_main_chain(leg: dict[str, Any]) -> list[str]:
    """一轮主链路的事实面：真跑、有订单、有成交、有账簿分录、审计走到终态、队列排空。"""
    problems: list[str] = []
    if leg["first"]["new_orders"] <= 0:
        problems.append(
            f"paper-check 本轮零新增（+{leg['first']['new_orders']}）：当日调度跳过、复用既有事实，"
            "不是一次端到端重跑"
        )
    kinds = leg["kinds"]
    if kinds.get(ORDER_SUBMITTED, 0) != 1:
        problems.append(f"事件日志里 OrderSubmitted={kinds.get(ORDER_SUBMITTED, 0)}，期望恰好 1")
    if kinds.get(FILLED, 0) < 1:
        problems.append(f"事件日志里 Filled={kinds.get(FILLED, 0)}，期望至少 1")
    if kinds.get(LEDGER_APPLIED, 0) < 1:
        problems.append(f"事件日志里 LedgerApplied={kinds.get(LEDGER_APPLIED, 0)}，期望至少 1")
    if kinds.get(ACCOUNT_CASHFLOW, 0) < 1:
        problems.append(
            f"事件日志里 AccountCashflow={kinds.get(ACCOUNT_CASHFLOW, 0)}，期望至少 1"
            "（Paper 初始资金必须真的落账，而不是只在配置里写着）"
        )
    executed = [code for status, code in leg["audit"] if status == "Executed"]
    if not executed:
        problems.append(f"审计链没有 Executed 终态：{[s for s, _ in leg['audit']]}")
    elif not any(code.startswith("PAPER_EXECUTED") for code in executed):
        problems.append(f"Executed 的 result_code 不是 PAPER_EXECUTED：{executed}")
    if leg["pending"]:
        problems.append(f"跑完仍有未确认命令：{leg['pending'][:3]}")
    return problems


def compare_rerun(leg: dict[str, Any]) -> list[str]:
    """同目录重跑：不许出现第二笔订单。"""
    problems: list[str] = []
    if leg["second"]["new_orders"] != 0:
        problems.append(
            f"同目录重跑报出 +{leg['second']['new_orders']} 笔新订单：重跑必须幂等，不得重复下单"
        )
    before = leg["kinds"].get(ORDER_SUBMITTED, 0)
    after = leg["second_kinds"].get(ORDER_SUBMITTED, 0)
    if after != before:
        problems.append(f"同目录重跑把 OrderSubmitted 从 {before} 改成了 {after}：重复下单")
    return problems


def compare_independent(legs: list[dict[str, Any]]) -> list[str]:
    """两个独立目录：事实面逐格相等。"""
    problems: list[str] = []
    first, second = legs[0], legs[1]
    for field in (ORDER_SUBMITTED, FILLED, LEDGER_APPLIED, ACCOUNT_CASHFLOW):
        if first["kinds"].get(field, 0) != second["kinds"].get(field, 0):
            problems.append(
                f"独立目录 {field} 不等：{first['kinds'].get(field, 0)} vs {second['kinds'].get(field, 0)}"
            )
    if first["audit"] != second["audit"]:
        problems.append("独立目录审计状态序列不等（状态/结果码必须逐格相同）")
    return problems


def compare_no_credentials(legs: list[dict[str, Any]]) -> list[str]:
    problems: list[str] = []
    for index, leg in enumerate(legs):
        if leg["network_accessed"] is not False:
            problems.append(f"[leg{index}] status.network_accessed={leg['network_accessed']!r}，必须为 false")
        if leg["orders_sent"] is not False:
            problems.append(f"[leg{index}] status.orders_sent={leg['orders_sent']!r}，必须为 false")
        if leg["environment"] != "paper":
            problems.append(f"[leg{index}] status.environment={leg['environment']!r}，期望 paper")
    return problems


def write_record(legs: list[dict[str, Any]], binary: Path) -> None:
    first = legs[0]
    kinds = first["kinds"]
    RECORD.write_text(
        "# Paper 轨验收记录（M5' 的本地可验收子项）——由 tools/paper_acceptance.py 生成，请勿手改。\n"
        "#\n"
        "# 这条轨**不需要任何交易所凭据**：它跑的是仓库自己的 Paper venue（进程内撮合）。\n"
        "# `sandbox_tested` / `production_approved` 两档对这条轨**不适用**（不是「待补」）——\n"
        "# 那两档只对需要外部 venue 的能力有意义，证据在 maturity/evidence/testnet|production/。\n"
        "schema_version: 1\n"
        "kind: paper-acceptance\n"
        "outcome: passed\n"
        "generated_by: tools/paper_acceptance.py\n"
        f"generated_at_unix: {int(time.time())}\n"
        f"binary: {binary.name}\n"
        "credentials_required: false\n"
        "external_venues: none\n"
        "network_accessed: false\n"
        "orders_sent: false\n"
        "venue: paper\n"
        "main_chain: scheduler -> strategy -> paper-execution -> ledger\n"
        f"independent_runs: {len(legs)}\n"
        f"orders_submitted: {kinds.get(ORDER_SUBMITTED, 0)}\n"
        f"fills: {kinds.get(FILLED, 0)}\n"
        f"ledger_entries: {kinds.get(LEDGER_APPLIED, 0)}\n"
        "audit_chain_verified: true\n"
        "audit_reaches_executed: true\n"
        "pending_commands_after: 0\n"
        "rerun_does_not_duplicate_orders: true\n"
        "independent_dirs: fact_surface_equal\n",
        encoding="utf-8",
        newline="\n",
    )


def main() -> int:
    parser = argparse.ArgumentParser(description="Paper 轨验收（无需交易所凭据）")
    parser.add_argument("--binary", type=Path, default=None)
    parser.add_argument("--runtime", type=Path, default=DEFAULT_RUNTIME)
    parser.add_argument(
        "--root",
        type=Path,
        default=None,
        help="验收工作根目录（默认落在系统临时目录下，跑完保留以便复核）",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="只跑判据、不写 maturity/paper_acceptance.yaml",
    )
    args = parser.parse_args()
    binary = args.binary or default_binary()
    runtime = args.runtime.resolve()
    if not runtime.is_file():
        raise SystemExit(f"运行时配置不在盘：{runtime}")

    root = args.root or Path(os.environ.get("TEMP", "/tmp")) / "qianxing-paper-acceptance"
    if root.exists():
        shutil.rmtree(root)
    root.mkdir(parents=True)

    legs = []
    for label in ("a", "b"):
        leg_root = root / f"leg-{label}"
        leg_root.mkdir(parents=True)
        legs.append(one_leg(binary, runtime, leg_root))

    problems = (
        [p for leg in legs for p in compare_main_chain(leg)]
        + [p for leg in legs for p in compare_rerun(leg)]
        + compare_independent(legs)
        + compare_no_credentials(legs)
    )
    if problems:
        print("Paper 轨验收未通过（fail closed，不写记录）：")
        for problem in problems:
            print(f"  x {problem}")
        return EXIT_FAILURE

    kinds = legs[0]["kinds"]
    print(
        "Paper 轨验收通过："
        f"OrderSubmitted={kinds.get(ORDER_SUBMITTED, 0)} "
        f"Filled={kinds.get(FILLED, 0)} "
        f"LedgerApplied={kinds.get(LEDGER_APPLIED, 0)}"
    )
    print("  同目录重跑不重复下单、两个独立目录事实面相等、审计链自洽且走到 Executed")
    print(f"  凭据：不需要（子进程环境已摘掉 {'/'.join(CREDENTIAL_ENV_PREFIXES)}）")
    if not args.dry_run:
        write_record(legs, binary)
        print(f"已写入 {RECORD.relative_to(WORKSPACE).as_posix()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
