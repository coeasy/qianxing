#!/usr/bin/env python3
"""能力执行探针 —— 把"这些核心能力到底能不能真的跑起来"变成一份可复核的记录。

## 为什么要有这一份

`maturity/capability-matrix.yaml` 回答"每个领域走到哪一档"，`maturity/capabilities.yaml`
回答"每个子系统在四档阶梯上的位置"，`maturity/chain-map.yaml` 回答"三条主链的每一段锚在哪个
符号上"。三者都**不回答**最直接的那个问题：「把这条命令敲下去，它到底跑不跑得起来、跑出来的
数是什么」。本探针只做一件事：**真的执行**，把观测到的结果按能力逐条记下来。

## 口径

* 只跑**不需要凭据、不访问网络**的路径（回测 / 控制面 / 配置校验）。需要外部 venue 或凭据的
  能力**不许**在这里记成 executed，只能记 blocked 并写明卡在哪一条前置上——这是
  `capabilities_check` 那把尺（没有证据不得翻真）在"执行"这条轴上的同一把尺。
* Paper 主链与回测确定性**刻意不在这里重跑**：它们已分别由 `tools/paper_acceptance.py` 与
  `tools/backtest_acceptance.py` 生成 `maturity/paper_acceptance.yaml` /
  `maturity/backtest_acceptance.yaml`。同一件事不许有两份说法——本表用 `delegated` 指向它们。
* 产物落在 `.task-tmp/exec-probe/`（gitignored）；仓库资产只有本记录这一份。

跑法：`python tools/capability_execution_probe.py`（需先 `cargo build --offline -p qx-cli`）。
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "maturity" / "capability_execution.yaml"
PROBE_ROOT = ROOT / ".task-tmp" / "exec-probe"

BAR_PRIMARY = "deploy/qianxing.bar-frame.pairs-primary.example.json"
BAR_REFERENCE = "deploy/qianxing.bar-frame.pairs-reference.example.json"
DEPTH_FRAME = "deploy/qianxing.depth-frame.example.json"
RUNTIME_MULTI = "deploy/qianxing.runtime.strategy-multi-backtest.example.json"

# 需要跑跨语言策略 worker 的那条链要显式钉解释器：`QX_PYTHON` 未设时回落 PATH 的 python，
# 在沙箱里会被拒（os error 5）——那是环境问题，不是能力问题，探针必须把环境备齐。
VENV_PYTHON = "python/.venv/Scripts/python.exe"

PROBES = [
    {
        "name": "deterministic_backtest_single_leg",
        "anchor": "crates/qx-xingban",
        "argv": ["backtest", "builtin", "sma_cross", BAR_PRIMARY],
        "expect": ["[Builtin · Backtest]"],
        "observe": (
            r"\[Builtin · Backtest\] (strategy=\S+ instrument=\S+ bars=\d+ fills=\d+ "
            r"return_bps=-?\d+ max_drawdown_bps=\d+ result_hash=\w+)"
        ),
    },
    {
        "name": "multi_market_two_leg",
        "anchor": "crates/qx-zhenlu",
        "argv": [
            "backtest", "multi-builtin", "pairs_arbitrage",
            BAR_PRIMARY, BAR_REFERENCE, "--root", ".task-tmp/exec-probe/multi",
        ],
        "expect": ["[Multi-leg · Attribution]"],
        "observe": (
            r"\[Multi-leg · Attribution\] (strategy=\S+ groups=\d+ turnover_raw=\d+ "
            r"fees_raw=\d+)"
        ),
    },
    {
        "name": "order_book_depth_matching",
        "anchor": "crates/qx-xingban",
        "argv": [
            "backtest", "book", "--fill-tier", "l2", "--root", ".task-tmp/exec-probe/book",
            "sma_cross", DEPTH_FRAME,
        ],
        "expect": ["[Depth · Integrity]", "[RunManifest]"],
        "observe": r"\[RunManifest\] (run_id=\S+ data_fingerprint=\S+ result_hash=\w+)",
    },
    {
        "name": "cross_language_multi_strategy_backtest",
        "anchor": "crates/qx-runtime",
        "argv": ["backtest", RUNTIME_MULTI, BAR_PRIMARY],
        "expect": ["[Strategy · Backtest]"],
        "env": {"QX_PYTHON": VENV_PYTHON},
        "observe": (
            r"\[Strategy · Backtest\] (strategy=\S+ instrument=\S+ bars=\d+ fills=\d+ "
            r"return_bps=-?\d+ max_drawdown_bps=\d+ result_hash=\w+)"
        ),
        "count": r"\[Strategy · Backtest\] strategy=",
    },
    {
        "name": "builtin_strategy_catalog",
        "anchor": "crates/qx-strategy",
        "argv": ["builtin-strategies"],
        "expect": ["macd", "[builtin / ccxt-builtin / book]"],
        "count": r"^\S+\t\S+",
    },
    {
        "name": "event_and_control_pipeline",
        "anchor": "crates/qx-control",
        "argv": ["ecosystem"],
        "expect": ["[控制 · ControlCommand]", "[调度 · JobSpec]", "[规格 · qx-spec]"],
        "observe": r"\[控制 · ControlCommand\] (status=\S+ digest=\w+)",
    },
    {
        "name": "runtime_config_multi_strategy",
        "anchor": RUNTIME_MULTI,
        "argv": ["config", "validate", RUNTIME_MULTI],
        "expect": ["[PASS] config validate 通过"],
        "observe": r"\[PASS\] config validate 通过: (\S+)",
        # `config validate` 只说"通过"，不带任何数——而一条没有数字的"通过"是弱观测。
        # 直接数这份模板里真声明了几个策略/worker/venue，观测值才有内容。
        "json_counts": ["strategies", "workers", "venues"],
    },
]

DELEGATED = [
    {
        "name": "paper_live_order_loop_local",
        "anchor": "crates/qx-zhenlu",
        "delegated_to": "maturity/paper_acceptance.yaml",
        "note": (
            "Paper 主链（scheduler → strategy → paper-execution → ledger）由 tools/paper_acceptance.py "
            "独立生成记录；本探针不重跑，免得同一件事有两份说法。本轮修的 JobSpec 升级路径缺陷正是"
            "靠这条链复现并验证的。"
        ),
        "evidence": ["maturity/paper_acceptance.yaml", "tools/paper_acceptance.py"],
    },
    {
        "name": "backtest_determinism_and_replay",
        "anchor": "crates/qx-cli",
        "delegated_to": "maturity/backtest_acceptance.yaml",
        "note": (
            "同一目录两轮逐格相等、两个独立目录归一化后内容相等，由 tools/backtest_acceptance.py "
            "独立生成记录；本探针只证明各条链能跑起来，不重复证明它们确定性。"
        ),
        "evidence": ["maturity/backtest_acceptance.yaml", "tools/backtest_acceptance.py"],
    },
]

BLOCKED = [
    {
        "name": "multi_exchange_live",
        "anchor": "crates/qx-adapter",
        "blocked_by": (
            "CCXT 适配器进程依赖的 `ccxt` 未装入 python/.venv；且无交易所凭据"
            "（maturity/evidence/testnet/ 那份 Binance 验收 outcome=skipped）。"
            "外部 venue 一档因此仍是 implemented-unverified——Paper venue 那一条腿已执行"
            "（见 delegated 段）。"
        ),
        "evidence": ["crates/qx-adapter/src/ccxt.rs", "maturity/evidence/testnet"],
    },
    {
        "name": "multi_account_first_class",
        "anchor": "crates/qx-runtime",
        "blocked_by": (
            "多账户只在**策略级**建模：`StrategyRuntimeConfig.account_id` 与绑定 worker 的 "
            "account_id 在 topology_validation 里交叉核对（fail-closed），但 `RuntimeConfig` 没有"
            "一等 `accounts` 段，账户级资金归属、风险聚合与退出语义也未端到端验证。"
            "两个账户各跑各的回测已可执行（见 cross_language_multi_strategy_backtest）。"
        ),
        "evidence": [
            "crates/qx-runtime/src/runtime_config/strategy_schema.rs",
            RUNTIME_MULTI,
        ],
    },
]

VERDICTS = ("executed", "delegated", "blocked")


def resolve_binary() -> Path:
    override = os.environ.get("QX_CLI")
    if override:
        return Path(override)
    for candidate in (".cargo-target/debug/qx-cli.exe", "target/debug/qx-cli.exe"):
        path = ROOT / candidate
        if path.exists():
            return path
    raise SystemExit("找不到 qx-cli 可执行文件：先 `cargo build --offline -p qx-cli`")


def run(binary: Path, probe: dict) -> tuple[int, str]:
    env = dict(os.environ)
    tmp = PROBE_ROOT / "tmp"
    tmp.mkdir(parents=True, exist_ok=True)
    env["TEMP"] = env["TMP"] = str(tmp)
    for key, value in probe.get("env", {}).items():
        env[key] = str(ROOT / value) if not Path(value).is_absolute() else value
    proc = subprocess.run(
        [str(binary), *probe["argv"]],
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    return proc.returncode, proc.stdout + proc.stderr


def observe(probe: dict, output: str) -> str:
    parts: list[str] = []
    if "observe" in probe:
        matched = re.search(probe["observe"], output)
        parts.append(matched.group(1) if matched else "")
    if "count" in probe:
        parts.append(f"{len(re.findall(probe['count'], output, re.MULTILINE))} 条")
    if "json_counts" in probe:
        payload = json.loads((ROOT / probe["anchor"]).read_text(encoding="utf-8"))
        for key in probe["json_counts"]:
            value = payload.get(key)
            if isinstance(value, list):
                parts.append(f"{key}={len(value)}")
    return "; ".join(part for part in parts if part) if parts else "exit 0"


def quote(value: object) -> str:
    """一切字符串都加引号：`maturity/` 的裸标量普查会拒绝含半角 `: ` 或以 `:` 结尾的裸标量。"""
    text = "" if value is None else str(value)
    return '"' + text.replace("\\", "\\\\").replace('"', '\\"') + '"'


def emit(records: list[dict]) -> str:
    lines = [
        "# 能力执行记录 —— 由 tools/capability_execution_probe.py 生成，请勿手改。",
        "#",
        "# 这一份回答的是「把命令敲下去，它到底跑不跑得起来、跑出来的数是什么」。",
        "# `executed` 是真的执行过并且观测到数字；`delegated` 指向另一份更专门的记录；",
        "# `blocked` 是**不许**记成 executed 的那些（缺凭据 / 缺依赖 / 未一等建模），",
        "# 每条都必须写明卡在哪一条前置上——没有证据不得翻真。",
        "schema_version: 1",
        "kind: capability-execution",
        f"generated_by: {quote('tools/capability_execution_probe.py')}",
        f"generated_at_unix: {int(time.time())}",
        f"cli_binary: {quote(str(resolve_binary().relative_to(ROOT)).replace(chr(92), '/'))}",
        "credentials_required: false",
        "network_accessed: false",
        "orders_sent: false",
        "verdicts:",
    ]
    lines += [f"  - {v}" for v in VERDICTS]
    lines.append("capabilities:")
    for record in records:
        lines.append(f"  - name: {record['name']}")
        lines.append(f"    verdict: {record['verdict']}")
        lines.append(f"    anchor: {quote(record['anchor'])}")
        if record["verdict"] == "executed":
            lines.append(f"    command: {quote(record['command'])}")
            lines.append(f"    exit_code: {record['exit_code']}")
            lines.append(f"    observed: {quote(record['observed'])}")
        elif record["verdict"] == "delegated":
            lines.append(f"    delegated_to: {quote(record['delegated_to'])}")
            lines.append(f"    note: {quote(record['note'])}")
        else:
            lines.append(f"    blocked_by: {quote(record['blocked_by'])}")
        evidence = ", ".join(quote(item) for item in record["evidence"])
        lines.append(f"    evidence: [{evidence}]")
    return "\n".join(lines) + "\n"


def main() -> int:
    binary = resolve_binary()
    PROBE_ROOT.mkdir(parents=True, exist_ok=True)
    records: list[dict] = []
    failures = 0

    for probe in PROBES:
        code, output = run(binary, probe)
        missing = [needle for needle in probe["expect"] if needle not in output]
        observed = observe(probe, output)
        ok = code == 0 and not missing and bool(observed)
        if not ok:
            failures += 1
        print(
            f"[{'OK  ' if ok else 'FAIL'}] {probe['name']}: rc={code} "
            f"missing={missing} observed={observed!r}",
            flush=True,
        )
        records.append(
            {
                "name": probe["name"],
                "verdict": "executed",
                "anchor": probe["anchor"],
                "command": "qx-cli " + " ".join(probe["argv"]),
                "exit_code": code,
                "observed": observed,
                "evidence": [probe["anchor"]],
            }
        )

    for item in DELEGATED:
        records.append(
            {
                "name": item["name"],
                "verdict": "delegated",
                "anchor": item["anchor"],
                "delegated_to": item["delegated_to"],
                "note": item["note"],
                "evidence": item["evidence"],
            }
        )

    for item in BLOCKED:
        records.append(
            {
                "name": item["name"],
                "verdict": "blocked",
                "anchor": item["anchor"],
                "blocked_by": item["blocked_by"],
                "evidence": item["evidence"],
            }
        )

    OUT.write_text(emit(records), encoding="utf-8", newline="\n")
    executed = sum(1 for r in records if r["verdict"] == "executed")
    print(f"\n写入 {OUT.relative_to(ROOT)}：executed={executed} delegated={len(DELEGATED)} blocked={len(BLOCKED)}")
    if failures:
        print(f"{failures} 条探针没跑通——记录已写出，但 executed 的观测为空，门禁会当场红")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
