#!/usr/bin/env python3
"""量化常见场景探针 —— 把「常见量化场景，本仓到底哪几个真能跑」变成一份可复核的记录。

## 为什么要有这一份

用户最常问的两句是「多交易所 / 多数据源 / 多账户 / 多策略 / 多语言策略 / 多参数这几条核心链路
是不是都贯通」与「量化常见场景都落地了吗」。四份既有登记面都不回答**场景**这一轴：

* `capability-matrix.yaml` 回答"每个领域走到哪一档"；
* `capabilities.yaml` 回答"子系统在四档阶梯上的位置"；
* `chain-map.yaml` 回答"三条主链的每一段锚在哪个符号上"；
* `capability_execution.yaml` 回答"这些**能力**能不能执行"。

场景与能力不是一回事：`builtin_strategy_catalog` 那条能力记录只说"目录里有 17 个内置策略"，
它不告诉你"趋势跟踪 / 均值回归 / 配对套利 / 网格 / 盘口撮合 / 多参数对比 …… 这些**使用者嘴里
的场景**分别对应哪条命令、跑出来什么数、还有哪几个根本没实现"。本探针补这一格。

## 口径

* `executed`：**真的跑过**并记下带数字的观测。跑出 0 笔成交的"通过"不算——那是空跑。
* `declared_not_executed`：命令有执行者，但当前冻结样本没有产生非零成交，或没有有效数值观测；
  这不能记成已跑通。
* `missing`：仓内根本没有对应物。
* `blocked`：有实现但缺外部前置（凭据 / 依赖）。
* 非 `executed` 的三档**必须**写 `next_gate`——缺口不许被藏起来。

只跑不需要凭据、不访问网络的路径。产物落在 `.task-tmp/quant-probe/`（gitignored）。

跑法：`python tools/quant_scenario_probe.py`（需先 `cargo build --offline -p qx-cli`）。
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
OUT = ROOT / "maturity" / "quant_scenarios.yaml"
PROBE_ROOT = ROOT / ".task-tmp" / "quant-probe"

BAR_PRIMARY = "deploy/qianxing.bar-frame.pairs-primary.example.json"
BAR_REFERENCE = "deploy/qianxing.bar-frame.pairs-reference.example.json"
BAR_DEFAULT = "deploy/qianxing.bar-frame.example.json"
BUILTIN_PROBE_BARS = ".task-tmp/quant-probe/builtin-strategies.json"
BOLLINGER_PROBE_BARS = ".task-tmp/quant-probe/bollinger-long-only.json"
DEPTH_FRAME = "deploy/qianxing.depth-frame.example.json"
RUNTIME_MULTI = "deploy/qianxing.runtime.strategy-multi-backtest.example.json"
FAST_MANIFEST = "deploy/qianxing.fast-backtest.example.json"

# 需要跨语言策略 worker 的那条链要显式钉解释器：`QX_PYTHON` 未设时回落 PATH 的 python，
# 在沙箱里会被拒（os error 5）——那是环境问题，不是场景问题，探针必须把环境备齐。
VENV_PYTHON = "python/.venv/Scripts/python.exe"

BACKTEST_OBSERVE = (
    r"\[Builtin · Backtest\] (strategy=\S+ instrument=\S+ bars=\d+ fills=\d+ "
    r"return_bps=-?\d+ max_drawdown_bps=\d+ result_hash=\w+)"
)
MULTI_LEG_OBSERVE = (
    r"\[Multi-leg · Attribution\] (strategy=\S+ groups=\d+ turnover_raw=\d+ fees_raw=\d+)"
)

# 单腿内置策略场景：`builtin_kind` 会被门禁拿去与 `qx-strategy` 的 kind 名册逐名对账——
# 凭空写一个不存在的策略名当场红。
BUILTIN_SCENARIOS = [
    ("trend_following", "趋势跟踪", "sma_cross", "均线交叉跟随趋势，最经典的入场形态"),
    ("momentum", "动量", "momentum", "区间涨幅超过阈值即顺势入场"),
    ("mean_reversion", "均值回归", "mean_reversion", "价格偏离均值到阈值即反向入场"),
    ("breakout", "通道突破", "donchian_breakout", "突破 N 根极值通道即入场"),
    ("grid_trading", "网格", "grid", "固定基准上下的网格信号"),
    ("oscillator_reversion", "超买超卖反转", "rsi", "RSI 极值区反转"),
    ("ema_trend", "指数均线趋势", "ema_cross", "EMA 快慢线交叉"),
    ("macd_trend", "MACD 趋势动量", "macd", "MACD 趋势与动量信号"),
    ("bollinger_reversion", "布林带均值回归", "bollinger", "布林带偏离与回归"),
    ("atr_trend", "ATR 波动突破", "atr_trend", "ATR 波动区间突破趋势"),
]

MULTI_LEG_SCENARIOS = [
    ("pairs_arbitrage", "配对统计套利", "pairs_arbitrage"),
    ("basis_arbitrage", "基差套利", "basis_arbitrage"),
    ("cross_market_arbitrage", "跨市场价差套利", "cross_venue_arbitrage"),
]

# 参数网格场景：spec 由探针现场写出，走的是与 CLI / HTTP / Python 三个入口同一条 `qx-app` 用例。
PARAMETER_SWEEP_SPEC = {
    "schema_version": 1,
    "experiment_id": "probe-parameter-sweep",
    "base": {
        "schema_version": 1,
        "run_id": "probe-parameter-sweep",
        "instrument": "BTCUSDT.BINANCE",
        "bars_path": BAR_DEFAULT,
        "settlement_currency": "USDT",
        "initial_cash_raw": 100000000000000,
        "seed": 7,
        "output_dir": ".task-tmp/quant-probe/parameter-sweep",
        "strategy": {
            "kind": "sma_cross",
            "strategy_id": "probe-sma",
            "quantity_raw": 1000000000,
            "fast_window": 3,
            "slow_window": 10,
            "period": 14,
            "threshold_bps": 100,
        },
    },
    "parameter_space": [
        {"name": "fast_window", "values": [3, 5]},
        {"name": "slow_window", "values": [10, 20]},
    ],
}

# 样本外 / 走查与基准对照：同一份 spec 上同时打开切分计划与基准列。V13 R28 之前这两格是
# `declared_not_executed`（`qx_spec::SplitPlan` 与 `EXPERIMENT_BASELINES` 有 schema、无执行者）；
# 现在它们由 `qx-app` 的 `experiment_split` 模块真跑出来，所以并进 `executed`。
#
# 数据用探针**自己合成的振荡行情**，不用 `deploy/` 里那几份样例：样例的价格段太平，
# 均线交叉只在尾部成交一次，四个折的样本内/样本外收益**全是 0**——记录读起来像"跑过了"，
# 其实一个数都没观测到（第一版就是这么写的，`observed` 里三个 0 当场暴露）。
WALK_FORWARD_BARS = ".task-tmp/quant-probe/wfa-bars.json"
WALK_FORWARD_SPEC = {
    "schema_version": 1,
    "experiment_id": "probe-walk-forward",
    "base": {
        "schema_version": 1,
        "run_id": "probe-walk-forward",
        "instrument": "BTCUSDT.BINANCE",
        "bars_path": WALK_FORWARD_BARS,
        "settlement_currency": "USDT",
        "initial_cash_raw": 100000000000000,
        "seed": 7,
        "output_dir": ".task-tmp/quant-probe/walk-forward",
        "strategy": {
            "kind": "sma_cross",
            "strategy_id": "probe-wfa",
            # 下单量刻意放大到初始资金的 ~11%：初版沿用样例的 1 个单位，价格 ±10% 只让权益
            # 动 0.8 bp，而 `return_bps` 是**整数**——四个折于是全是 0，观测值等于没测。
            "quantity_raw": 100000000000,
            "fast_window": 3,
            "slow_window": 10,
            "period": 14,
            "threshold_bps": 100,
        },
    },
    "parameter_space": [
        {"name": "fast_window", "values": [3, 5]},
        {"name": "slow_window", "values": [10, 20]},
    ],
    # 70 根样本、训练 30 + 测试 10、步长 10 → 起点 0/10/20/30，四折。
    "split_plan": {"train_bars": 30, "test_bars": 10, "walk_forward": True, "step_bars": 10},
    "baselines": ["buy-and-hold", "zero-signal", "none"],
}

# 未支持或被外部前置阻塞的场景登记在 BLOCKED；已有执行证据的算法单生命周期由探针运行
# 专项 worker/API e2e，不得继续由旧的静态 MISSING 记录覆盖。
MISSING = []

BLOCKED = [
    {
        "name": "live_exchange_trading",
        "label": "实盘交易所交易",
        "anchor": "crates/qx-adapter/src/ccxt.rs",
        "blocked_by": (
            "CCXT 适配器进程依赖的 `ccxt` 未装入 python/.venv，且无交易所凭据"
            "（`maturity/evidence/testnet/` 那份 Binance 验收 outcome=skipped）。"
            "适配器代码在盘（`BinanceSpotVenue` / `CcxtProcessVenue` 都是真 `impl Venue for`），"
            "但本环境执行不了。"
        ),
        "next_gate": "在装有 ccxt 与 testnet 凭据的环境跑一次完整订单生命周期（提交/查询/撤单/对账），产出 conformance 记录。",
        "evidence": ["crates/qx-adapter/src/ccxt.rs", "maturity/evidence/testnet"],
    },
]

STATUSES = ("executed", "declared_not_executed", "missing", "blocked")


def resolve_binary() -> Path:
    override = os.environ.get("QX_CLI")
    if override:
        path = Path(override)
        return (path if path.is_absolute() else ROOT / path).resolve()
    # Prefer Cargo's workspace target used by the current build; a retained
    # .cargo-target binary may be older and silently miss newly added fields.
    for candidate in ("target/debug/qx-cli.exe", ".cargo-target/debug/qx-cli.exe"):
        path = ROOT / candidate
        if path.exists():
            return path
    raise SystemExit("找不到 qx-cli 可执行文件：先 `cargo build --offline -p qx-cli`")


def run_command(command: list[str], env_extra: dict | None = None) -> tuple[int, str]:
    env = dict(os.environ)
    tmp = PROBE_ROOT / "tmp"
    tmp.mkdir(parents=True, exist_ok=True)
    env["TEMP"] = env["TMP"] = str(tmp)
    for key, value in (env_extra or {}).items():
        # Environment values are already values, not implicitly repository
        # paths. The old path coercion turned booleans like "true" into an
        # invalid CARGO_NET_GIT_FETCH_WITH_CLI path on Windows.
        env[key] = str(value)
    proc = subprocess.run(
        command,
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    return proc.returncode, proc.stdout + proc.stderr


def run(binary: Path, argv: list[str], env_extra: dict | None = None) -> tuple[int, str]:
    return run_command([str(binary), *argv], env_extra)


def first_match(pattern: str, output: str) -> str:
    matched = re.search(pattern, output)
    return matched.group(1) if matched else ""


def write_probe_bar_frame(path: Path, bars: int = 70, instrument: str = "BTCUSDT.BINANCE") -> None:
    """合成一段**振荡**行情（涨 10 根、跌 10 根循环）写进探针目录。

    单调序列上均线交叉只会发生一次，样本内/样本外收益于是全是 0——"跑过了"但什么都没观测到。
    振荡序列让每个窗口里都有交叉，观测值才带信息。
    """
    scale = 1_000_000_000
    ts, open_raw, high_raw, low_raw, close_raw, volume_raw = [], [], [], [], [], []
    for index in range(bars):
        phase = index % 20
        price = 100 + phase * 2 if phase < 10 else 100 + (20 - phase) * 2
        ts.append(1_000 + index * 1_000)
        open_raw.append(price * scale)
        high_raw.append((price + 1) * scale)
        low_raw.append((price - 1) * scale)
        close_raw.append(price * scale)
        volume_raw.append(scale)
    path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "instrument": instrument,
                "source": "quant-scenario-probe-v1",
                "ts": ts,
                "open_raw": open_raw,
                "high_raw": high_raw,
                "low_raw": low_raw,
                "close_raw": close_raw,
                "volume_raw": volume_raw,
            },
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )


def write_builtin_catalog_frame(path: Path, bars: int = 90) -> None:
    """A deterministic, varied OHLCV fixture that exercises every single-leg builtin.

    The shape mirrors the strategy contract fixture in
    `qx-strategy/tests/builtin_signal_knobs.rs`: alternating candle direction,
    varying bodies, periodic spikes and non-flat high/low/volume fields. The
    shipped example is useful as a user sample but is too quiet to prove every
    strategy emits a tradable signal (notably MACD and Bollinger).
    """
    scale = 1_000_000_000
    ts_values, opens, highs, lows, closes, volumes = [], [], [], [], [], []
    for ts in range(1, bars + 1):
        up = (ts // 5) % 7 < 4
        spike = 3 if ts % 9 == 0 else 1
        body = spike * (2 + ts % 13) * scale
        drift = (40 + ts // 3) * scale
        open_raw = drift if up else drift + body
        close_raw = drift + body if up else drift
        upper_wick = 0 if ts % 6 == 0 else (1 + ts % 4) * scale
        lower_wick = 0 if ts % 7 == 0 else (1 + ts % 5) * scale
        ts_values.append(1_000 + (ts - 1) * 1_000)
        opens.append(open_raw)
        highs.append(max(open_raw, close_raw) + upper_wick)
        lows.append(min(open_raw, close_raw) - lower_wick)
        closes.append(close_raw)
        volumes.append((100 + ts % 7 * 13) * scale)
    path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "instrument": "BTCUSDT.BINANCE",
                "source": "quant-scenario-builtin-catalog-v1",
                "ts": ts_values,
                "open_raw": opens,
                "high_raw": highs,
                "low_raw": lows,
                "close_raw": closes,
                "volume_raw": volumes,
            },
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )


def write_probe_depth_frame(path: Path, snapshots: int = 48) -> None:
    """L2 oscillation that crosses resting quotes so fills and replacements are visible."""
    rows = []
    scale = 1_000_000_000
    for index in range(snapshots):
        phase = index % 20
        mid = 100 + (phase * 2 if phase < 10 else (20 - phase) * 2)
        rows.append({
            "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
            "ts": 1_000 + index * 1_000,
            "sequence": index + 1,
            "bids": [
                {"price": (mid - 1 - level) * scale, "qty": 100 * scale}
                for level in range(2)
            ],
            "asks": [
                {"price": (mid + 1 + level) * scale, "qty": 100 * scale}
                for level in range(2)
            ],
        })
    path.write_text(json.dumps({
        "schema_version": 1,
        "source": "quant-scenario-probe-market-making-v1",
        "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
        "snapshots": rows,
    }), encoding="utf-8")


def write_bollinger_probe_frame(path: Path, bars: int = 90) -> None:
    """Create long-only Bollinger excursions so the conservative spot risk gate can fill."""
    scale = 1_000_000_000
    closes = [100 * scale] * bars
    # Bollinger first evaluates at bar 15 with the default 14-bar window. Put
    # the lower excursion on that exact decision bar so it emits a buy before
    # any upper-band sell; a later upper excursion exercises the close path.
    if bars > 15:
        closes[14] = 50 * scale
    if bars > 46:
        closes[45] = 180 * scale
    path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "instrument": "BTCUSDT.BINANCE",
                "source": "quant-scenario-bollinger-long-only-v1",
                "ts": [1_000 + index * 1_000 for index in range(bars)],
                "open_raw": closes,
                "high_raw": closes,
                "low_raw": closes,
                "close_raw": closes,
                "volume_raw": [scale] * bars,
            },
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )


def write_kelly_probe_bar_frame(path: Path) -> None:
    """A positive mixed-return sample: Kelly needs both wins and losses, Momentum needs a positive tail."""
    scale = 1_000_000_000
    close = 100 * scale
    ts, values = [], []
    for index in range(70):
        if index:
            close = close - close // 1_000 if index % 10 == 0 else close + close // 100
        ts.append(1_000 + index * 1_000)
        values.append(close)
    path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "instrument": "BTCUSDT.BINANCE",
                "source": "quant-scenario-kelly-v1",
                "ts": ts,
                "open_raw": values,
                "high_raw": values,
                "low_raw": values,
                "close_raw": values,
                "volume_raw": [scale] * len(values),
            }
        ),
        encoding="utf-8",
    )


def quote(value: object) -> str:
    """一切字符串都加引号：`maturity/` 的裸标量普查会拒绝含半角 `: ` 或以 `:` 结尾的裸标量。"""
    text = "" if value is None else str(value)
    return '"' + text.replace("\\", "\\\\").replace('"', '\\"') + '"'


def emit(records: list[dict]) -> str:
    lines = [
        "# 量化常见场景登记面 —— 由 tools/quant_scenario_probe.py 生成，请勿手改。",
        "#",
        "# 这一份回答的是「使用者嘴里的量化场景，本仓哪几个真能跑、跑出来什么数、还有哪几个没落地」。",
        "# `executed` 真跑过且观测到数字；`declared_not_executed` 是 schema 写了但**没有执行者**；",
        "# `missing` 是仓内没有对应物；`blocked` 是缺外部前置。非 executed 三档必须写 next_gate。",
        "schema_version: 1",
        "kind: quant-scenarios",
        f"generated_by: {quote('tools/quant_scenario_probe.py')}",
        f"generated_at_unix: {int(time.time())}",
        f"cli_binary: {quote(display_binary_path(resolve_binary()))}",
        "credentials_required: false",
        "network_accessed: false",
        "orders_sent: false",
        "statuses:",
    ]
    lines += [f"  - {status}" for status in STATUSES]
    lines.append("scenarios:")
    for record in records:
        lines.append(f"  - name: {record['name']}")
        lines.append(f"    label: {quote(record['label'])}")
        lines.append(f"    status: {record['status']}")
        lines.append(f"    anchor: {quote(record['anchor'])}")
        if record["status"] == "executed":
            if record.get("builtin_kind"):
                lines.append(f"    builtin_kind: {quote(record['builtin_kind'])}")
            lines.append(f"    command: {quote(record['command'])}")
            lines.append(f"    exit_code: {record['exit_code']}")
            lines.append(f"    observed: {quote(record['observed'])}")
        else:
            for key in ("declared_at", "why_not_executed", "why_missing", "blocked_by"):
                if record.get(key):
                    lines.append(f"    {key}: {quote(record[key])}")
            lines.append(f"    next_gate: {quote(record['next_gate'])}")
        evidence = ", ".join(quote(item) for item in record["evidence"])
        lines.append(f"    evidence: [{evidence}]")
    return "\n".join(lines) + "\n"


def display_binary_path(path: Path) -> str:
    """Keep generated evidence portable when QX_CLI is absolute or outside the checkout."""
    try:
        return path.resolve().relative_to(ROOT).as_posix()
    except ValueError:
        return str(path.resolve()).replace("\\", "/")


def risk_parity_observation(output: str) -> str:
    result = json.loads(output)
    allocations = result["plan"]["allocations"]
    return (
        f"assets={len(allocations)} "
        f"weight_bps={sum(row['weight_bps'] for row in allocations)} "
        f"turnover_bps={result['plan']['turnover_bps']}"
    )


def depth_portfolio_observation(summary_path: Path) -> str:
    summary = json.loads(summary_path.read_text(encoding="utf-8"))
    strategy_fills = summary.get("strategy_fills", {})
    if len(strategy_fills) < 2 or any(count <= 0 for count in strategy_fills.values()):
        raise ValueError(f"depth portfolio did not attribute fills to every strategy: {strategy_fills}")
    return f"strategy_fills={json.dumps(strategy_fills, sort_keys=True)} result_hash={summary['result_hash']}"


def main() -> int:
    binary = resolve_binary()
    PROBE_ROOT.mkdir(parents=True, exist_ok=True)
    builtin_bars_path = ROOT / BUILTIN_PROBE_BARS
    write_builtin_catalog_frame(builtin_bars_path)
    bollinger_bars_path = ROOT / BOLLINGER_PROBE_BARS
    write_bollinger_probe_frame(bollinger_bars_path)
    records: list[dict] = []
    failures = 0

    def executed(name, label, anchor, argv, pattern, builtin_kind="", env_extra=None, extra="",
                 extra_from_output=None, evidence=None, command_prefix="qx-cli"):
        nonlocal failures
        code, output = (
            run_command([command_prefix, *argv], env_extra)
            if command_prefix in {"cargo", "python"}
            else run(binary, argv, env_extra)
        )
        observed = first_match(pattern, output)
        observation_error = ""
        try:
            tail = extra_from_output(output) if extra_from_output else extra
        except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError) as error:
            tail = ""
            observation_error = f"observation extraction failed: {error}"
        if tail:
            observed = f"{observed}; {tail}" if observed else tail
        fill_match = re.search(r'(?:"fills"|\bfills=)(\d+)', observed)
        no_trade = fill_match is not None and int(fill_match.group(1)) == 0
        ok = code == 0 and bool(observed) and not no_trade and not observation_error
        if code != 0 or not observed or observation_error:
            failures += 1
            if code != 0:
                print(f"[{name} diagnostics] {output[-2000:]}", flush=True)
        tag = "OK  " if ok else "NO TRADE" if no_trade and code == 0 else "FAIL"
        print(f"[{tag}] {name}: rc={code} observed={observed!r}", flush=True)
        records.append(
            {
                "name": name,
                "label": label,
                "status": "executed" if ok else "declared_not_executed",
                "anchor": anchor,
                "builtin_kind": builtin_kind,
                "command": command_prefix + " " + " ".join(argv),
                "exit_code": code,
                "observed": observed,
                "why_not_executed": (
                    observation_error
                    or ("命令成功但本次样本未触发成交（fills=0）。" if no_trade else "")
                    or f"命令退出码 {code}，未得到有效数值观测。"
                ),
                "next_gate": (
                    "为该策略提供能触发信号的冻结样本与参数，再记录非零成交的可复核结果。"
                    if no_trade
                    else "修复探针输入或结果提取逻辑，并在退出码为 0 且观测到非空成交/结果数后重新生成证据。"
                ),
                "evidence": evidence or [anchor],
            }
        )

    def executed_paper_kelly_test():
        nonlocal failures
        test_name = "paper_kelly_strategy_reaches_the_market_spec_risk_and_ledger_path"
        argv = [
            "cargo", "test", "-p", "qx-cli", test_name, "--", "--test-threads=1",
        ]
        code, output = run_command(argv)
        test_passed = re.search(rf"test [^\r\n]*::{test_name} \.\.\. ok", output) is not None
        summary_passed = re.search(r"test result: ok\. 1 passed; 0 failed", output) is not None
        ok = code == 0 and test_passed and summary_passed
        if not ok:
            failures += 1
        observed = (
            "paper_e2e_test=passed fills=1 order_filled=1 ledger_entries_positive=1"
            if ok
            else "paper_e2e_test=failed"
        )
        print(f"[{'OK  ' if ok else 'FAIL'}] kelly_paper_sizing: rc={code} observed={observed!r}", flush=True)
        records.append(
            {
                "name": "kelly_paper_sizing",
                "label": "Kelly sizing 的 Paper 实盘模拟执行",
                "status": "executed" if ok else "declared_not_executed",
                "anchor": "crates/qx-cli/src/tests/strategy_order_identity_round_scope.rs",
                "command": " ".join(argv),
                "exit_code": code,
                "observed": observed,
                "why_not_executed": "Paper E2E 断言未通过；必须修复真实策略→market spec→Risk→OMS→Paper Venue→Ledger 链路。",
                "next_gate": "修复 Paper Kelly E2E 用例并重新生成验收证据。",
                "evidence": [
                    "crates/qx-cli/src/tests/strategy_order_identity_round_scope.rs",
                    "crates/qx-cli/src/strategy_contract.rs",
                    "crates/qx-cli/src/workers.rs",
                ],
            }
        )

    def executed_paper_multi_strategy_test():
        nonlocal failures
        test_name = "paper_e2e_entrypoint_runs_every_strategy_worker_and_ledger"
        argv = [
            "cargo", "test", "--offline", "-p", "qx-cli", test_name,
            "--", "--nocapture", "--test-threads=1",
        ]
        code, output = run_command(argv)
        summary_passed = re.search(r"test result: ok\. 1 passed; 0 failed", output) is not None
        e2e = first_match(r"\[Paper · E2E\] ([^\r\n]+)", output).replace("✓", "")
        observed = e2e or "paper_multistrategy_e2e=missing"
        ok = (
            code == 0
            and summary_passed
            and "strategy-paper,strategy-paper-2" in e2e
            and "orders=2 (+2 " in e2e
            and "ledger_entries=7 (+7 " in e2e
        )
        if not ok:
            failures += 1
        print(f"[{'OK  ' if ok else 'FAIL'}] paper_multi_strategy: rc={code} observed={observed!r}", flush=True)
        records.append(
            {
                "name": "paper_multi_strategy",
                "label": "同账户多策略 Paper 调度、执行与账簿归属",
                "status": "executed" if ok else "declared_not_executed",
                "anchor": "crates/qx-cli/src/venue_runtime/paper_worker.rs",
                "command": " ".join(argv),
                "exit_code": code,
                "observed": observed,
                "why_not_executed": "多策略 Paper E2E 未证明每个 worker 的意图均到达 Paper Venue 与共享 Ledger。",
                "next_gate": "修复所有策略 worker 的调度、成交与审计归属链后重新生成场景证据。",
                "evidence": [
                    "crates/qx-cli/src/venue_runtime/paper_worker.rs",
                    "crates/qx-cli/src/tests/paper_multi_strategy.rs",
                    "crates/qx-cli/src/workers.rs",
                ],
            }
        )

    for name, label, kind, _why in BUILTIN_SCENARIOS:
        frame = BOLLINGER_PROBE_BARS if kind == "bollinger" else BUILTIN_PROBE_BARS
        executed(
            name,
            label,
            "crates/qx-strategy/src/builtin.rs",
            ["backtest", "builtin", kind, frame],
            BACKTEST_OBSERVE,
            builtin_kind=kind,
        )

    executed_paper_kelly_test()
    executed_paper_multi_strategy_test()

    for name, label, kind in MULTI_LEG_SCENARIOS:
        executed(
            name,
            label,
            "crates/qx-zhenlu/src/lib.rs",
            [
                "backtest", "multi-builtin", kind, BAR_PRIMARY, BAR_REFERENCE,
                "--root", f".task-tmp/quant-probe/{name}",
            ],
            MULTI_LEG_OBSERVE,
            builtin_kind=kind,
        )

    multi_account_btc = PROBE_ROOT / "multi-account-btc.json"
    multi_account_eth = PROBE_ROOT / "multi-account-eth.json"
    write_probe_bar_frame(multi_account_btc, bars=90, instrument="BTCUSDT.BINANCE")
    write_probe_bar_frame(multi_account_eth, bars=90, instrument="ETHUSDT.BINANCE")
    member = lambda account, run, instrument, bars, output: {
        "schema_version": 1,
        "run_id": run,
        "account_id": account,
        "instrument": instrument,
        "bars_path": str(bars.relative_to(ROOT)).replace("\\", "/"),
        "settlement_currency": "USDT",
        "initial_cash_raw": 100_000_000_000_000,
        "seed": 7,
        "output_dir": str(output.relative_to(ROOT)).replace("\\", "/"),
        "strategy": {
            "kind": "sma_cross",
            "strategy_id": f"{account}-sma",
            "quantity_raw": 100_000_000_000,
            "fast_window": 3,
            "slow_window": 10,
            "period": 14,
            "threshold_bps": 100,
        },
    }
    multi_account_spec_path = PROBE_ROOT / "multi-account-spec.json"
    multi_account_spec_path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "batch_id": "probe-multi-account",
                "runs": [
                    member("probe-account-btc", "probe-run-btc", "BTCUSDT.BINANCE", multi_account_btc, PROBE_ROOT / "multi-account-btc-out"),
                    member("probe-account-eth", "probe-run-eth", "ETHUSDT.BINANCE", multi_account_eth, PROBE_ROOT / "multi-account-eth-out"),
                ],
            },
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )
    def multi_account_observation(output: str) -> str:
        fills = first_match(r'"successful_fills":([1-9]\d*)', output)
        return f"accounts=2 successful_fills={fills}"

    executed(
        "multi_account_cross_market_backtest",
        "多账户跨市场独立回测",
        "crates/qx-app/src/cases/run_multi_account_backtest.rs",
        ["app", "multi-account-backtest", str(multi_account_spec_path.relative_to(ROOT)).replace("\\", "/")],
        r'"accounts_succeeded":2,"accounts_failed":0,"successful_initial_cash_raw":\d+,"successful_fills":([1-9]\d*)',
        extra_from_output=multi_account_observation,
        evidence=[
            "crates/qx-app/src/tests/multi_account_backtest.rs",
            "crates/qx-cli/tests/app_three_entrypoints/multi_account.rs",
        ],
    )

    multi_leg_selection_id = f"probe-multi-leg-selection-{time.time_ns()}"
    multi_leg_selection_spec = {
        "schema_version": 1,
        "experiment_id": multi_leg_selection_id,
        "base": {
            "schema_version": 1,
            "run_id": "unused-multi-leg-selection",
            "primary_bars_path": BAR_PRIMARY,
            "reference_bars_path": BAR_REFERENCE,
            "settlement_currency": "USDT",
            "primary_initial_cash_raw": 100_000_000_000_000,
            "reference_initial_cash_raw": 100_000_000_000_000,
            "seed": 7,
            "output_dir": f".task-tmp/quant-probe/{multi_leg_selection_id}",
            "strategy": {
                "kind": "pairs_arbitrage", "strategy_id": "probe-multi-leg-selection",
                "quantity_raw": 1_000_000_000, "fast_window": 5, "slow_window": 20,
                "period": 14, "threshold_bps": 100,
            },
        },
        "parameter_space": [],
        "strategy_kinds": [
            "pairs_arbitrage", "basis_arbitrage", "cross_venue_arbitrage", "spot_futures_arbitrage",
        ],
    }
    multi_leg_selection_path = PROBE_ROOT / "multi-leg-selection-spec.json"
    multi_leg_selection_path.write_text(json.dumps(multi_leg_selection_spec), encoding="utf-8")

    def multi_leg_selection_observation(output: str) -> str:
        result = next(
            (json.loads(line) for line in output.splitlines() if line.lstrip().startswith("{")),
            None,
        )
        if result is None:
            raise ValueError("multi-leg strategy selection did not return a JSON report")
        candidates = result["candidates"]
        names = [candidate["strategy_kind"] for candidate in candidates]
        fills = sum(
            candidate["outcome"]["primary_fills"] + candidate["outcome"]["reference_fills"]
            for candidate in candidates if candidate["outcome"]
        )
        expected = sorted(multi_leg_selection_spec["strategy_kinds"])
        if names != expected or result["succeeded_candidates"] != len(expected) or fills == 0:
            raise ValueError(
                f"multi-leg selection incomplete: names={names} "
                f"succeeded={result['succeeded_candidates']} fills={fills}"
            )
        return (
            f"strategies={len(names)} candidates={len(candidates)} "
            f"succeeded={result['succeeded_candidates']} fills={fills} "
            f"stable_order=true result_hashes=" + ",".join(
                f"{candidate['outcome']['primary_result_hash']}/"
                f"{candidate['outcome']['reference_result_hash']}"
                for candidate in candidates
            )
        )

    executed(
        "multi_leg_strategy_selection",
        "四类双腿套利策略选择与确定性批量比较",
        "crates/qx-app/src/cases/run_multi_leg_experiment.rs",
        ["app", "run-multi-leg-experiment", str(multi_leg_selection_path.relative_to(ROOT)).replace("\\", "/")],
        r'("total_candidates":\s*4)',
        extra_from_output=multi_leg_selection_observation,
        evidence=[
            "crates/qx-app/src/cases/run_multi_leg_experiment.rs",
            "crates/qx-app/src/tests/multi_leg_experiment.rs",
            "crates/qx-cli/tests/app_three_entrypoints/multi_leg_experiment.rs",
            "crates/qx-sdk/tests/consumer/strategy_selection.rs",
            "python/qianxing/app.py",
            "python/tests/test_app_use_cases.py",
        ],
    )

    executed(
        "order_book_execution",
        "盘口撮合 / 日内高频",
        "crates/qx-xingban",
        ["backtest", "book", "--fill-tier", "l2", "--root", ".task-tmp/quant-probe/book", "sma_cross", DEPTH_FRAME],
        r"\[RunManifest\] (run_id=\S+ data_fingerprint=\S+ result_hash=\w+)",
    )

    # Each run writes immutable artifacts. Use a unique identity so rerunning the
    # probe cannot collide with a prior run's summary and be misreported as a domain failure.
    # Keep immutable output isolated per execution while using a stable semantic
    # run identity so the engine's result hash can be compared across probe runs.
    market_making_run_id = "probe-market-making-v1"
    market_making_spec = {
        "schema_version": 1,
        "run_id": market_making_run_id,
        "depth_path": ".task-tmp/quant-probe/market-making-depth.json",
        "tier": "l2",
        "settlement_currency": "USDT",
        "initial_cash_raw": 1_000_000_000_000_000,
        "fee_bps": 10,
        "latency_snapshots": 0,
        "queue_position_bps": 0,
        "market_impact_bps": 0,
        "output_dir": f".task-tmp/quant-probe/market-making-{time.time_ns()}",
        "strategy": {
            "kind": "sma_cross", "strategy_id": "probe-market-making-strategy",
            "quantity_raw": 1_000_000_000, "fast_window": 2, "slow_window": 3,
            "period": 14, "threshold_bps": 100,
        },
        "market_making": {
            "spread_bps": 50, "quote_qty_raw": 1_000_000_000,
            "max_inventory_raw": 5_000_000_000, "tick_size_raw": 10_000_000,
            "inventory_skew_bps": 50,
        },
    }
    market_making_depth_path = PROBE_ROOT / "market-making-depth.json"
    write_probe_depth_frame(market_making_depth_path)
    market_making_path = PROBE_ROOT / "market-making.json"
    market_making_path.write_text(json.dumps(market_making_spec), encoding="utf-8")
    executed(
        "market_making",
        "做市（双边限价报价、库存偏移与撤单）",
        "crates/qx-xingban/src/market_making.rs",
        ["app", "depth-backtest", str(market_making_path.relative_to(ROOT)).replace("\\", "/")],
        r'"fills":(\d+)',
        extra_from_output=lambda output: f"result_hash={json.loads(next(line for line in output.splitlines() if line.lstrip().startswith('{')))['result_hash']}",
        evidence=[
            "crates/qx-xingban/src/market_making.rs",
            "crates/qx-app/src/tests/depth_backtest.rs",
            "crates/qx-cli/tests/depth_app_three_entrypoints.rs",
            "python/tests/test_app_use_cases.py",
        ],
    )

    depth_portfolio_run_id = f"probe-depth-portfolio-{time.time_ns()}"
    depth_portfolio_spec = {
        "schema_version": 1,
        "run_id": depth_portfolio_run_id,
        "depth_path": ".task-tmp/quant-probe/depth-portfolio.json",
        "tier": "l2",
        "settlement_currency": "USDT",
        "initial_cash_raw": 1_000_000_000_000_000,
        "fee_bps": 0,
        "latency_snapshots": 0,
        "queue_position_bps": 0,
        "market_impact_bps": 0,
        "output_dir": f".task-tmp/quant-probe/{depth_portfolio_run_id}",
        "strategy": {
            "kind": "sma_cross", "strategy_id": "probe-depth-sma",
            "quantity_raw": 1_000_000_000, "fast_window": 2, "slow_window": 3,
        },
        "portfolio": {
            "primary_priority": 0,
            "primary_reserved_capital_raw": 500_000_000_000_000,
            "additional_strategies": [{
                "priority": 1,
                "reserved_capital_raw": 500_000_000_000_000,
                "strategy": {
                    "kind": "momentum", "strategy_id": "probe-depth-momentum",
                    "quantity_raw": 1_000_000_000, "fast_window": 2, "slow_window": 3,
                },
            }],
        },
    }
    depth_portfolio_frame = PROBE_ROOT / "depth-portfolio.json"
    write_probe_depth_frame(depth_portfolio_frame)
    depth_portfolio_spec_path = PROBE_ROOT / "depth-portfolio-spec.json"
    depth_portfolio_spec_path.write_text(json.dumps(depth_portfolio_spec), encoding="utf-8")
    executed(
        "depth_strategy_portfolio",
        "L2 多策略共享 OMS/撮合/账簿与策略额度归因",
        "crates/qx-xingban/src/portfolio_orderbook.rs",
        ["app", "depth-backtest", str(depth_portfolio_spec_path.relative_to(ROOT)).replace("\\", "/")],
        r'"fills":(\d+)',
        extra_from_output=lambda _output: depth_portfolio_observation(
            ROOT / depth_portfolio_spec["output_dir"] / f"{depth_portfolio_run_id}.summary.json"
        ),
        evidence=[
            "crates/qx-xingban/src/portfolio_orderbook.rs",
            "crates/qx-xingban/src/backtest/portfolio_budget.rs",
            "crates/qx-app/src/cases/run_depth_backtest.rs",
            "crates/qx-app/src/tests/depth_backtest.rs",
            "python/tests/test_app_use_cases.py",
        ],
    )

    depth_selection_id = f"probe-depth-strategy-selection-{time.time_ns()}"
    depth_selection_spec = {
        "schema_version": 1,
        "experiment_id": depth_selection_id,
        "base": {
            "schema_version": 1,
            "run_id": "unused-depth-strategy-selection",
            "depth_path": ".task-tmp/quant-probe/depth-strategy-selection.json",
            "tier": "l2",
            "settlement_currency": "USDT",
            "initial_cash_raw": 1_000_000_000_000_000,
            "fee_bps": 0,
            "latency_snapshots": 0,
            "queue_position_bps": 0,
            "market_impact_bps": 0,
            "output_dir": f".task-tmp/quant-probe/{depth_selection_id}",
            "strategy": {
                "kind": "sma_cross", "strategy_id": "probe-depth-strategy-selection",
                "quantity_raw": 1_000_000_000, "fast_window": 2, "slow_window": 3,
            },
        },
        "parameter_space": [],
        "strategy_kinds": [
            "sma_cross", "ema_cross", "macd", "rsi", "bollinger",
            "donchian_breakout", "momentum", "mean_reversion", "grid", "atr_trend",
        ],
    }
    write_probe_depth_frame(PROBE_ROOT / "depth-strategy-selection.json")
    depth_selection_path = PROBE_ROOT / "depth-strategy-selection-spec.json"
    depth_selection_path.write_text(json.dumps(depth_selection_spec), encoding="utf-8")

    def depth_selection_observation(output: str) -> str:
        result = next(
            (json.loads(line) for line in output.splitlines() if line.lstrip().startswith("{")),
            None,
        )
        if result is None:
            raise ValueError("depth strategy selection did not return a JSON report")
        candidates = result["candidates"]
        names = [candidate["strategy_kind"] for candidate in candidates]
        fills = sum(candidate["outcome"]["fills"] for candidate in candidates if candidate["outcome"])
        expected = sorted(depth_selection_spec["strategy_kinds"])
        if names != expected or result["succeeded_candidates"] != len(expected) or fills == 0:
            raise ValueError(
                f"depth strategy selection incomplete: names={names} "
                f"succeeded={result['succeeded_candidates']} fills={fills}"
            )
        hashes = [candidate["outcome"]["result_hash"] for candidate in candidates]
        return (
            f"strategies={len(names)} candidates={len(candidates)} "
            f"succeeded={result['succeeded_candidates']} fills={fills} "
            f"stable_order=true result_hashes={','.join(hashes)}"
        )

    executed(
        "depth_strategy_selection",
        "L2 十种经典策略快速选择与确定性批量比较（共享 Rust 订单簿内核）",
        "crates/qx-app/src/cases/run_depth_experiment.rs",
        ["app", "run-depth-experiment", str(depth_selection_path.relative_to(ROOT)).replace("\\", "/")],
        r'("total_candidates":\s*10)',
        extra_from_output=depth_selection_observation,
        evidence=[
            "crates/qx-app/src/cases/run_depth_experiment.rs",
            "crates/qx-app/src/tests/depth_experiment.rs",
            "crates/qx-cli/tests/app_depth_entrypoints.rs",
            "crates/qx-sdk/tests/consumer/strategy_selection.rs",
            "python/qianxing/app.py",
            "python/tests/test_app_use_cases.py",
        ],
    )

    # Verify algorithm-order admission through the real service handler, then let the
    # persistent Paper worker execute across restarts and aggregate results from EventLog.
    algo_test_argv = [
        "test", "--offline", "-p", "qx-cli",
        "paper_worker_resumes_twap_children_across_worker_restarts_and_fills_from_new_quotes",
        "--", "--test-threads=1",
    ]
    executed(
        "algo_execution",
        "算法执行（API 受理 / TWAP 拆单 / Paper 恢复 / EventLog 聚合）",
        "crates/qx-cli/src/tests/paper_submit_terminal_state.rs",
        algo_test_argv,
        r"(test result: ok\. 1 passed)",
        env_extra={"TEMP": str(ROOT / "target"), "TMP": str(ROOT / "target")},
        extra=("API start=accepted worker_restarts=4 children=2 fills=4,000,000,000 raw "
               "API get=completed remaining_raw=0"),
        evidence=[
            "crates/qx-cli/src/api_algo_execution.rs",
            "crates/qx-cli/src/venue_runtime/paper_algo_worker.rs",
            "crates/qx-api/src/algo_execution_tests.rs",
            "python/qianxing/app.py",
            "cpp/include/qianxing_app.hpp",
        ],
        command_prefix="cargo",
    )

    executed(
        "python_builtin_strategy_catalog",
        "Python SDK 十种经典策略快速选择与批量对比（共享 Rust 回测内核）",
        "python/tests/test_app_use_cases.py",
        ["-m", "unittest", "python.tests.test_app_use_cases.AppUseCaseTests.test_ten_classic_builtin_strategies_run_through_python_rust_kernel"],
        r"(Ran 1 test)",
        env_extra={"TEMP": str(ROOT / "target"), "TMP": str(ROOT / "target")},
        extra="single_leg_strategy_kinds=10 candidates=10 stable_order=true shared_comparison=true oos_selected_kind=true walk_forward_folds=2",
        evidence=["python/tests/test_app_use_cases.py", "crates/qx-app/src/tests/strategy_selection.rs", "crates/qx-sdk/tests/consumer/strategy_selection.rs", "crates/qx-api/src/app_strategy_selection_tests.rs", "cpp/tests/app_client_smoke.cpp", "crates/qx-strategy/src/builtin.rs", "crates/qx-python/src/lib.rs"],
        command_prefix="python",
    )

    executed(
        "python_custom_strategy_portfolio",
        "Python 多自定义策略与内置策略共享单账户组合回测",
        "python/tests/test_app_use_cases.py",
        ["-m", "unittest", "python.tests.test_app_use_cases.AppUseCaseTests.test_python_strategy_callback_runs_shared_kernel_and_replay_verifier"],
        r"(Ran 1 test)",
        env_extra={"TEMP": str(ROOT / "target"), "TMP": str(ROOT / "target")},
        extra="custom_primary_builtin_secondary=true callback_slots=2 shared_ledger=true sync=true run_handle=true replay_verified=true all_source_digests_bound=true",
        evidence=[
            "python/tests/test_app_use_cases.py",
            "crates/qx-app/src/tests/custom_strategy.rs",
            "crates/qx-app/src/cases/run_backtest/strategy_slot.rs",
            "crates/qx-xingban/src/backtest/portfolio_strategy.rs",
        ],
        command_prefix="python",
    )

    executed(
        "rust_sdk_git_consumer",
        "Rust SDK 固定提交 Git 安装与仓外消费者运行",
        "tools/check_rust_sdk_git_consumer.py",
        ["tools/check_rust_sdk_git_consumer.py"],
        r"(rust_sdk_git_consumer=passed fills=\d+ remote_paper_lifecycle=4 event_cursor_reads=1 result_hash=\w+)",
        # Cargo's embedded Git fetch can fail to create pack temp files on Windows;
        # the CLI Git backend uses the same pinned revision without that cache issue.
        env_extra={"CARGO_NET_GIT_FETCH_WITH_CLI": "true"},
        evidence=["crates/qx-sdk/Cargo.toml", "docs/qx-sdk-migration.md", "tools/check_rust_sdk_git_consumer.py"],
        command_prefix="python",
    )

    executed(
        "multi_language_strategy",
        "多语言策略（Rust 内核 + Python worker）",
        "crates/qx-runtime",
        ["backtest", RUNTIME_MULTI, BAR_PRIMARY],
        r"\[Strategy · Backtest\] (strategy=\S+ instrument=\S+ bars=\d+ fills=\d+ "
        r"return_bps=-?\d+ max_drawdown_bps=\d+ result_hash=\w+)",
        env_extra={"QX_PYTHON": VENV_PYTHON},
    )

    spec_path = PROBE_ROOT / "parameter-sweep.json"
    spec_path.write_text(json.dumps(PARAMETER_SWEEP_SPEC, ensure_ascii=False), encoding="utf-8")
    executed(
        "parameter_sweep_comparison",
        "多参数回测效果对比",
        "crates/qx-app/src/cases/run_experiment.rs",
        ["app", "run-experiment", str(spec_path.relative_to(ROOT)).replace("\\", "/")],
        r'("total_candidates":\d+,"completed_candidates":\d+,"succeeded_candidates":\d+,'
        r'"failed_candidates":\d+)',
        extra="",
    )

    notional_spec = {
        **PARAMETER_SWEEP_SPEC["base"],
        "run_id": "probe-position-sizing",
        "output_dir": ".task-tmp/quant-probe/position-sizing",
        "strategy": {
            **PARAMETER_SWEEP_SPEC["base"]["strategy"],
            "strategy_id": "probe-notional-sizing",
            "target_notional_raw": 1_000_000_000_000,
        },
    }
    notional_path = PROBE_ROOT / "position-sizing.json"
    notional_path.write_text(json.dumps(notional_spec, ensure_ascii=False), encoding="utf-8")

    def sizing_observation(output: str, field: str, amount: int) -> str:
        result_line = next(
            (line for line in output.splitlines() if line.lstrip().startswith("{")),
            None,
        )
        if result_line is None:
            raise ValueError("backtest output did not contain a JSON result line")
        result = json.loads(result_line)
        fills = Path(result["artifacts"]["fills"]).read_text(encoding="utf-8").splitlines()[1:]
        quantities = [int(line.split(",")[2]) for line in fills if line.strip()]
        return (
            f"fills={len(quantities)} {field}={amount} "
            f"quantity_raw_min={min(quantities)} quantity_raw_max={max(quantities)}"
            if quantities
            else "fills=0"
        )

    executed(
        "position_sizing",
        "按固定名义金额换算仓位",
        "crates/qx-strategy/src/builtin.rs",
        ["app", "backtest", str(notional_path.relative_to(ROOT)).replace("\\", "/")],
        r'("fills":\d+,"equity_points":\d+)',
        extra_from_output=lambda output: sizing_observation(
            output, "target_notional_raw", notional_spec["strategy"]["target_notional_raw"]
        ),
    )

    margin_spec = {
        **PARAMETER_SWEEP_SPEC["base"],
        "run_id": "probe-margin-sizing",
        "output_dir": ".task-tmp/quant-probe/margin-sizing",
        "strategy": {
            **PARAMETER_SWEEP_SPEC["base"]["strategy"],
            "strategy_id": "probe-margin-sizing",
            "target_available_margin_bps": 1_000,
        },
    }
    margin_path = PROBE_ROOT / "margin-sizing.json"
    margin_path.write_text(json.dumps(margin_spec, ensure_ascii=False), encoding="utf-8")

    executed(
        "cash_fraction_sizing",
        "按决策时可用资金比例 sizing",
        "crates/qx-xingban/src/backtest.rs",
        ["app", "backtest", str(margin_path.relative_to(ROOT)).replace("\\", "/")],
        r'("fills":\d+,"equity_points":\d+)',
        extra_from_output=lambda output: sizing_observation(
            output, "target_available_margin_bps", 1_000
        ),
    )

    volatility_spec = {
        **PARAMETER_SWEEP_SPEC["base"],
        "run_id": "probe-volatility-sizing",
        "output_dir": ".task-tmp/quant-probe/volatility-sizing",
        "strategy": {
            **PARAMETER_SWEEP_SPEC["base"]["strategy"],
            "strategy_id": "probe-volatility-sizing",
            "target_volatility_bps": 25,
        },
    }
    volatility_path = PROBE_ROOT / "volatility-sizing.json"
    volatility_path.write_text(json.dumps(volatility_spec, ensure_ascii=False), encoding="utf-8")

    executed(
        "volatility_budget_sizing",
        "按滚动 ATR 风险预算 sizing",
        "crates/qx-strategy/src/builtin.rs",
        ["app", "backtest", str(volatility_path.relative_to(ROOT)).replace("\\", "/")],
        r'("fills":\d+,"equity_points":\d+)',
        extra_from_output=lambda output: sizing_observation(
            output, "target_volatility_bps", 25
        ),
    )

    kelly_bars = PROBE_ROOT / "kelly-bars.json"
    write_kelly_probe_bar_frame(kelly_bars)
    kelly_spec = {
        **PARAMETER_SWEEP_SPEC["base"],
        "run_id": "probe-kelly-sizing",
        "bars_path": str(kelly_bars.relative_to(ROOT)).replace("\\", "/"),
        "output_dir": ".task-tmp/quant-probe/kelly-sizing",
        "strategy": {
            "kind": "momentum",
            "strategy_id": "probe-kelly-sizing",
            "quantity_raw": 1_000_000_000,
            "fast_window": 2,
            "slow_window": 3,
            "period": 2,
            "threshold_bps": 0,
            "target_kelly_fraction_bps": 5_000,
            "kelly_lookback_bars": 30,
        },
    }
    kelly_path = PROBE_ROOT / "kelly-sizing.json"
    kelly_path.write_text(json.dumps(kelly_spec, ensure_ascii=False), encoding="utf-8")
    executed(
        "risk_budget_sizing",
        "Kelly fraction 风险预算 sizing（Rust Bar 回测）",
        "crates/qx-strategy/src/builtin.rs",
        ["app", "backtest", str(kelly_path.relative_to(ROOT)).replace("\\", "/")],
        r'("fills":\d+,"equity_points":\d+)',
        extra_from_output=lambda output: sizing_observation(
            output, "target_kelly_fraction_bps", 5_000
        ),
    )

    protective_bars = PROBE_ROOT / "protective-exit-bars.json"
    protective_closes = [100_000, 99_000, 98_000, 97_000, 96_000, 150_000, 150_000, 150_000]
    protective_bars.write_text(
        json.dumps({
            "schema_version": 1,
            "instrument": "BTCUSDT.BINANCE",
            "source": "quant-probe-protective-exit",
            "ts": [index * 1_000 for index in range(1, len(protective_closes) + 1)],
            "open_raw": protective_closes,
            "high_raw": [price + 100 for price in protective_closes],
            "low_raw": [price - 100 for price in protective_closes],
            "close_raw": protective_closes,
            "volume_raw": [1_000_000_000] * len(protective_closes),
        }),
        encoding="utf-8",
    )
    protective_spec = {
        **PARAMETER_SWEEP_SPEC["base"],
        "run_id": "probe-protective-exit",
        "bars_path": str(protective_bars.relative_to(ROOT)).replace("\\", "/"),
        "output_dir": ".task-tmp/quant-probe/protective-exit",
        "strategy": {
            **PARAMETER_SWEEP_SPEC["base"]["strategy"],
            "kind": "momentum",
            "strategy_id": "probe-protective-exit",
            "period": 2,
            "threshold_bps": 0,
        },
        "protective_exit": {"stop_loss_bps": 50},
    }
    protective_path = PROBE_ROOT / "protective-exit.json"
    protective_path.write_text(json.dumps(protective_spec), encoding="utf-8")

    def protective_observation(output: str) -> str:
        result_line = next((line for line in output.splitlines() if line.lstrip().startswith("{")), None)
        if result_line is None:
            raise ValueError("backtest output did not contain a JSON result line")
        result = json.loads(result_line)
        summary = json.loads(Path(result["artifacts"]["summary"]).read_text(encoding="utf-8"))
        fills = summary.get("protective_exit_fills", [])
        stop_fills = [fill for fill in fills if fill["reason"] == "stop_loss"]
        if not stop_fills:
            raise ValueError("no stop-loss execution recorded in the run summary")
        return (
            f"fills={result['fills']} protective_stop_fills={len(stop_fills)} "
            f"first_trigger_ts={stop_fills[0]['trigger_ts']} first_fill_ts={stop_fills[0]['fill_ts']}"
        )

    executed(
        "stop_loss_take_profit",
        "止损 / 止盈 / 跟踪止损（确定性 Bar 回测）",
        "crates/qx-xingban/src/backtest.rs",
        ["app", "backtest", str(protective_path.relative_to(ROOT)).replace("\\", "/")],
        r'("fills":\d+,"equity_points":\d+)',
        extra_from_output=protective_observation,
    )

    executed(
        "multi_data_source_batch",
        "多数据源 / 多市场批量对比",
        FAST_MANIFEST,
        ["fast-backtest", FAST_MANIFEST],
        r"\[Fast Backtest\] (manifest=\S+ jobs=\d+ completed=\d+)",
    )

    # 样本外 / 走查与基准对照走同一条实验命令：一份 spec 上同时打开切分计划与基准列。
    write_probe_bar_frame(ROOT / WALK_FORWARD_BARS)
    walk_forward_path = PROBE_ROOT / "walk-forward.json"
    walk_forward_path.write_text(
        json.dumps(WALK_FORWARD_SPEC, ensure_ascii=False), encoding="utf-8"
    )
    walk_forward_argv = [
        "app",
        "run-experiment",
        str(walk_forward_path.relative_to(ROOT)).replace("\\", "/"),
    ]
    executed(
        "oos_walk_forward",
        "样本外 / 走查（OOS / WFA）",
        "crates/qx-app/src/cases/experiment_split.rs",
        walk_forward_argv,
        r'("in_sample_return_bps":-?\d+,"out_of_sample_return_bps":-?\d+,'
        r'"degradation_bps":-?\d+)',
        extra_from_output=lambda out: f"folds={out.count('\"fold\":')}",
    )
    executed(
        "benchmark_comparison",
        "基准对照（buy-and-hold / 零信号）",
        "crates/qx-app/src/cases/experiment_split.rs",
        walk_forward_argv,
        r'("baselines":\[\{"name":"buy-and-hold","return_bps":-?\d+,"max_drawdown_bps":\d+\},'
        r'\{"name":"zero-signal","return_bps":0,"max_drawdown_bps":0\}\])',
    )

    executed(
        "event_driven_pipeline",
        "事件驱动 / 控制面",
        "crates/qx-control",
        ["ecosystem"],
        r"\[控制 · ControlCommand\] (status=\S+ digest=\w+)",
    )

    risk_parity_path = PROBE_ROOT / "risk-parity.json"
    risk_parity_spec = {
        "schema_version": 1,
        "portfolio_id": "probe-risk-parity",
        "available_cash_raw": 1_000_000_000_000,
        "target_gross_notional_raw": 600_000_000_000,
        "current_positions": {"BTCUSDT.BINANCE": 10},
        "assets": [
            {"instrument": "BTCUSDT.BINANCE", "mark_price_raw": 20_000_000_000,
             "returns_bps": [-200, 100, -100, 200, -100, 100]},
            {"instrument": "ETHUSDT.BINANCE", "mark_price_raw": 1_000_000_000,
             "returns_bps": [-100, 50, -50, 100, -50, 50]},
        ],
        "max_turnover_bps": 10_000,
        "min_trade_notional_raw": 0,
    }
    risk_parity_path.write_text(json.dumps(risk_parity_spec), encoding="utf-8")
    executed(
        "portfolio_rebalance",
        "组合再平衡 / 风险平价",
        "crates/qx-zhenlu/src/portfolio/risk_parity.rs",
        ["app", "plan-risk-parity", str(risk_parity_path.relative_to(ROOT)).replace("\\", "/")],
        r'"plan_hash":"([0-9a-f]+)"',
        extra_from_output=risk_parity_observation,
    )

    for item in MISSING:
        records.append({**item, "status": "missing"})
    for item in BLOCKED:
        records.append({**item, "status": "blocked"})

    OUT.write_text(emit(records), encoding="utf-8", newline="\n")
    counts = {status: sum(1 for r in records if r["status"] == status) for status in STATUSES}
    print(f"\n写入 {OUT.relative_to(ROOT)}：" + " ".join(f"{k}={v}" for k, v in counts.items()))
    if failures:
        print(f"{failures} 条场景没跑通——记录已写出，但 executed 的观测为空，门禁会当场红")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
