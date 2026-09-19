#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
牵星 Qianxing — 架构不变量自检（V9 收口）

把 V9 重构修掉的缺陷钉成可执行门禁，防止回归：
  1. 已删除的 crate 不得复活；
  2. `QX_PYTHON` 只能在 `python_interpreter()` 一处读取；
  3. 命令分派只有一份（`main.rs` 只调 `cli::run()`，命令字符串比较集中在 `cli.rs`）；
  4. 生产代码里不得出现"空风控门"（裸 `RiskGate::new()` 白名单 + 必须立刻加规则）；
  5. 单腿订单副作用只有一套实现（遗留 `ExecutionService`/`SpreadExecutionService` 与
     `MultiVenueSpreadExecutionService`+`VenueRouterMap` 已删除，单腿与多腿共用同一个
     `ExecutionGateway`，Paper 也走它）；
  6. Bar 回测引擎装配只有一份（`BacktestConfig {` 字面量唯一）；
  7. `maturity/capabilities.yaml` 结构完整且证据路径真实存在；
  8. 单文件行数预算只允许下降（棘轮），新增超 500 行文件必须显式登记；
  9. A 股公司行为的 PIT 过滤只发生在 JSON 加载闸门，被删的第二道闸门不得复活；
  10. 交易所回报侧的两条安全性质（终态不回退、精度越界只转待对账）钉在唯一归约入口，
     且 Paper / CCXT / Binance 三家共用同一份回报契约测试；
  11. 内核 `Ledger` 已按资产类别拆成目录模块，被拆掉的单文件不得复活。
  12. 行为用例的目录模块形状对 qx-cli 与 qx-execution 各查一遍（Phase 4o / 4r）：被拆掉的
     单文件不得复活、不得用 `#[path]` 指回单文件、用例条数只增不减、共享夹具只在 `tests/mod.rs`
     定义一份。
  13. qx-cli 的 crate 根已把十一个职责簇分两批搬进兄弟模块（Phase 4p / 4q）：每个模块在门槛内、
     在根上 `mod` + `pub(crate) use x::*` 成对挂载、根的顶层条目数不增、crate 根已退出行数登记集、
     每条链路的入口（smoke / runtime-check / live-check / 调度 / 行情桥 / 路径解析 / 策略契约）定义唯一。
  14. 回测编排 `crates/qx-cli/src/backtests/` 已是目录模块（Phase 4s）：原 1,379 行单文件不得复活、
      六个主题模块逐个在门槛内、在 `mod.rs` 成对挂载、共享装配的顶层条目数不增、八条回测链入口
      的定义点唯一。第 6 项的"装配只有一份"因此改为按目录聚合读取。

运行： python3 tools/check_architecture.py
刷新第 8 项的预算快照（改动后人工确认 diff）：
       python3 tools/check_architecture.py --snapshot
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")

ROOT = Path(__file__).resolve().parent.parent
CRATES = ROOT / "crates"
LINE_BUDGETS = ROOT / "maturity" / "line_budgets.yaml"
# 行数棘轮的登记门槛：低于此行数的文件不受预算约束。
OVERSIZED = 500

# 3. 命令分派：允许出现命令字符串比较的 CLI 文件。
CLI_DISPATCH_FILES = {"cli.rs"}
# 命令名分派的正则：CLI 命令变量与字面量比较。字段比较（如 `command.target == "*"`）
# 属于运行期寻址，不算第二分派点，故变量名后紧跟 `.` 的情况被排除。
DISPATCH_PATTERN = re.compile(
    r'\b(?:mode|cmd|command)\s*[!=]=\s*"'
    r'|\b(?:mode|cmd|command)\.as_str\(\)\s*[!=]='
    r'|matches!\(\s*\b(?:mode|cmd|command)\.as_str\(\)'
)
# 4. 允许构造裸 `RiskGate::new()` 的非测试代码，键为相对路径、值为期望出现次数。
#    `ecosystem_smoke.rs` 的一处是进程内确定性自校验演示（Phase 4p 前位于 `main.rs`），
#    紧随其后必须 add 规则（见下方专项断言）。
BARE_RISK_GATE_ALLOWLIST = {"crates/qx-cli/src/ecosystem_smoke.rs": 1}

failures: list[str] = []
checks = 0



def check(ok: bool, label: str, detail: str = "") -> None:
    global checks
    checks += 1
    if ok:
        print(f"[PASS] {label}")
    else:
        failures.append(label if not detail else f"{label} — {detail}")
        print(f"[FAIL] {label}{' — ' + detail if detail else ''}")


def rust_sources():
    for path in sorted(CRATES.glob("*/src/**/*.rs")):
        yield path


def is_test_scoped(index: int, lines: list[str]) -> bool:
    """判定某行是否位于文件尾部的 `#[cfg(test)] mod …` 之内（测试允许裸风控门）。

    属性挂在 `use` 上的条件导入不算测试模块起点，否则会把其后的生产代码误判为测试。
    """
    markers = [
        i
        for i, line in enumerate(lines[:index])
        if line.lstrip().startswith("#[cfg(test)]")
        and re.match(
            r"\s*(?:pub\([^)]*\)\s+)?mod\s+\w+\s*\{",
            next((l for l in lines[i + 1 : i + 6] if l.strip()), ""),
        )
    ]
    return bool(markers) and index > markers[-1]



def removed_crates_check() -> None:
    present = [
        name for name in ("qx-domain", "qx-kernel") if (CRATES / name).exists()
    ]
    check(
        not present,
        "V9 删除的 crate 未复活",
        f"仍存在 {present}",
    )


def python_interpreter_check() -> None:
    sites = []
    for path in rust_sources():
        text = path.read_text(encoding="utf-8")
        if re.search(r'env::var\(\s*"QX_PYTHON"', text):
            sites.append(path.relative_to(ROOT).as_posix())
    check(
        sites == ["crates/qx-cli/src/main.rs"],
        "QX_PYTHON 解释器只在 python_interpreter() 一处读取",
        f"读取点 {sites}",
    )


def worker_diagnostics_check() -> None:
    """Phase 4n：跨语言 worker 失败必须能自证原因（哪个程序、从哪来、子进程状态、stderr）。

    本机 `python` 可能只是 WindowsApps 的占位桩，与"策略代码抛异常"在协议层是同一句
    "worker 已关闭输出"；诊断文本因此成了这条链路的可运维性边界，得钉住。
    """
    origin_sites = [
        path.relative_to(ROOT).as_posix()
        for path in rust_sources()
        if "fn python_interpreter_origin" in path.read_text(encoding="utf-8")
    ]
    check(
        origin_sites == ["crates/qx-cli/src/main.rs"],
        "解释器来源判定只在 python_interpreter_origin() 一处",
        f"定义点 {origin_sites}",
    )

    host = (CRATES / "qx-cli/src/strategy_host.rs").read_text(encoding="utf-8")
    check(
        host.count("fn death_note(&mut self)") == 1
        and "try_wait()" in host
        and host.count("self.death_note()") >= 4,
        "策略 worker 失败诊断（程序/来源/退出码/stderr）只有一处实现并接满每个出口",
        f"定义 {host.count('fn death_note(&mut self)')} 次、"
        f"调用 {host.count('self.death_note()')} 次",
    )

    start = host.index("recv_timeout(")
    region = host[start : host.index("let line = match response", start)]
    check(
        "self.death_note()" in region and "??" not in region,
        "worker 无响应的协议错误必须附诊断后再冒泡（不得用两个问号裸传）",
        "响应通道错误未被包装，子进程状态与 stderr 会被丢掉",
    )

    ccxt = (CRATES / "qx-adapter/src/ccxt.rs").read_text(encoding="utf-8")
    check(
        "CCXT Worker 已退出，提交结果未知（程序={worker_program}）" in ccxt,
        "CCXT 无响应既保留未知结果语义又点名解释器",
        "EOF 文案缺少程序名或被改写",
    )


def cli_dispatch_check() -> None:
    main = (CRATES / "qx-cli/src/main.rs").read_text(encoding="utf-8")
    check(
        "cli::run();" in main and not DISPATCH_PATTERN.search(main),
        "main.rs 只做薄入口，命令分派不在 crate 根",
        "main.rs 仍含命令字符串比较或未调用 cli::run()",
    )
    offenders = []
    # 递归扫描：`venue_runtime/` 拆成目录模块后，子目录里的第二分派点同样必须被拦下。
    for path in sorted((CRATES / "qx-cli/src").rglob("*.rs")):
        name = path.name
        rel = path.relative_to(CRATES / "qx-cli/src")
        # `src/tests/` 是行为用例（Phase 4o 起为目录模块，原先是单文件 tests_main.rs）：
        # 断言里出现命令名字面量正是它们的职责，不算分派点。
        if name in CLI_DISPATCH_FILES or rel.parts[0] == "tests":
            continue
        hits = [
            line.strip()
            for line in path.read_text(encoding="utf-8").splitlines()
            if DISPATCH_PATTERN.search(line)
        ]
        if hits:
            rel = path.relative_to(CRATES / "qx-cli/src").as_posix()
            offenders.append(f"{rel}:{hits[0]}")
    check(not offenders, "命令名分派只存在于 cli.rs", f"额外分派点 {offenders}")



CLI_TESTS_DIR = "crates/qx-cli/src/tests"
# 用例条数下限棘轮：Phase 4o 把单文件 `tests_main.rs` 拆成目录模块时是 59 条。
# 拆分让"删几条用例来压行数"变成一条可行路径，因此行数只降不升的同时，用例数只能升。
CLI_TEST_FLOOR = 59
# 拆文件时最容易被复制进各个主题文件的共享夹具（风控上下文、隔离运行时目录）。
CLI_TEST_FIXTURES = (
    "smoke_paper_risk_context",
    "builtin_backtest_example_paths",
    "temp_cli_case_dir",
    "isolated_backtest_runtime",
)
# Phase 4r 用同一套形状约束收第二处：qx-execution 的 `src/tests.rs`（1,088 行）按
# 职责拆成目录模块。元组把 Phase 4o 的四项检查参数化，而不是复制一遍。
EXECUTION_TESTS_DIR = "crates/qx-execution/src/tests"
EXECUTION_TEST_FLOOR = 13
EXECUTION_TEST_FIXTURES = (
    "PortState",
    "PortVenue",
    "NeverCalledVenue",
    "RejectingRisk",
    "PortRouter",
    "port_order",
)
# (crate, 用例目录, 被拆掉的单文件名, 挂载模块的文件, 用例数下限, 共享夹具名)
TEST_MODULES = (
    ("qx-cli", CLI_TESTS_DIR, "tests_main.rs", "main.rs", CLI_TEST_FLOOR, CLI_TEST_FIXTURES),
    (
        "qx-execution",
        EXECUTION_TESTS_DIR,
        "tests.rs",
        "lib.rs",
        EXECUTION_TEST_FLOOR,
        EXECUTION_TEST_FIXTURES,
    ),
)
# 夹具可能是函数、结构体、枚举或类型别名；定义行允许带可见性前缀。
FIXTURE_DEF_PATTERN = r"^(?:pub(?:\([^)]*\))? )?(?:fn|struct|enum|trait|type) {}\b"


def test_module_shape_check() -> None:
    """行为用例的目录模块形状（V9 §8.3 第 5 项要求的删除侧收敛）。"""
    for crate, tests_dir, flat_name, mount_file, floor, fixtures in TEST_MODULES:
        src = CRATES / crate / "src"
        check(
            not (src / flat_name).exists(),
            f"{crate} 用例单文件 {flat_name} 已拆分且不得复活",
            f"{src.relative_to(ROOT).as_posix()}/{flat_name} 重新出现",
        )
        files = sorted((ROOT / tests_dir).glob("*.rs"))
        mount = (src / mount_file).read_text(encoding="utf-8")
        check(
            bool(files)
            and f'#[path = "{flat_name}"]' not in mount
            and "#[cfg(test)]\nmod tests;" in mount,
            f"{crate} 用例以目录模块挂载，不得用 #[path] 指回单文件",
            f"目录内文件 {len(files)} 个",
        )
        counts = {
            path.name: len(
                re.findall(r"^#\[test\]$", path.read_text(encoding="utf-8"), re.MULTILINE)
            )
            for path in files
        }
        check(
            sum(counts.values()) >= floor,
            f"{crate} 行为用例不少于 {floor} 条",
            f"当前 {sum(counts.values())} 条：{counts}",
        )
        defs = {
            name: [
                path.name
                for path in files
                if re.search(
                    FIXTURE_DEF_PATTERN.format(name),
                    path.read_text(encoding="utf-8"),
                    re.MULTILINE,
                )
            ]
            for name in fixtures
        }
        check(
            all(locations == ["mod.rs"] for locations in defs.values()),
            f"{crate} 共享测试夹具只在 tests/mod.rs 定义一份",
            f"定义位置 {defs}",
        )


CLI_P4P_MODULES = (
    "ecosystem_smoke",
    "runtime_wiring",
    "readiness",
    "configured_backends",
    "api_service",
    "runtime_check",
    "live_check",
    "scheduler",
    "market_bridges",
    "path_resolution",
    "strategy_binding",
)
# 两批搬家后 crate 根只剩 7 个顶层条目（分派薄壳）；写成上限而不是快照，防止职责又长回根文件。
CLI_ROOT_ITEM_CEILING = 7
# 每条链路的入口只允许有一处定义，且必须在它所属的模块里——搬家不能留下第二份实现，
# 也不能被"顺手又根一份"顶掉。
CLI_CHAIN_SYMBOLS = {
    "run_ecosystem_smoke": ("ecosystem_smoke.rs", r"^(?:pub(?:\(crate\))? )?fn run_ecosystem_smoke\("),
    "run_paper_smoke": ("ecosystem_smoke.rs", r"^(?:pub(?:\(crate\))? )?fn run_paper_smoke\("),
    "run_backtest": ("ecosystem_smoke.rs", r"^(?:pub(?:\(crate\))? )?fn run_backtest\("),
    "DemoProvider": ("ecosystem_smoke.rs", r"^(?:pub(?:\(crate\))? )?struct DemoProvider\b"),
    "collect_runtime_check_report": (
        "runtime_check.rs",
        r"^(?:pub(?:\(crate\))? )?fn collect_runtime_check_report\(",
    ),
    "collect_live_check_report": (
        "live_check.rs",
        r"^(?:pub(?:\(crate\))? )?fn collect_live_check_report\(",
    ),
    "dispatch_scheduled_jobs": (
        "scheduler.rs",
        r"^(?:pub(?:\(crate\))? )?fn dispatch_scheduled_jobs\(",
    ),
    "open_paper_market_bridges": (
        "market_bridges.rs",
        r"^(?:pub(?:\(crate\))? )?fn open_paper_market_bridges\(",
    ),
    "resolve_strategy_runtime_paths": (
        "path_resolution.rs",
        r"^(?:pub(?:\(crate\))? )?fn resolve_strategy_runtime_paths\(",
    ),
    "build_strategy_contract_input": (
        "strategy_binding.rs",
        r"^(?:pub(?:\(crate\))? )?fn build_strategy_contract_input\(",
    ),
}
ROOT_ITEM = re.compile(
    r"^(?:pub(?:\([^)]*\))? )?(?:async )?(?:unsafe )?(?:extern )?"
    r"(?:fn|struct|enum|trait|impl|type|const|static|union)\b"
)


def cli_root_module_check() -> None:
    """crate 根职责簇拆分（Phase 4p / 4q）：模块形状、配对挂载、根条目数与链路入口唯一。

    两轮都是纯搬家，语义等价由门禁外的 token 多重集比对与逐位相同的用例数承担；这里只钉
    "搬出去的形状不得自己长回来"：每个模块各自留在单文件门槛内（越界即需重新登记）、
    每个模块在根上 `mod` + `pub(crate) use x::*` 成对挂载（缺一即职责回流或出口丢失）、
    根的顶层条目数不增（实现不得再写回 `main.rs`）、crate 根必须已退出行数登记集，
    以及每条链路的入口只有一处定义。
    """
    root = (CRATES / "qx-cli/src/main.rs").read_text(encoding="utf-8")
    oversized = []
    for name in CLI_P4P_MODULES:
        path = CRATES / "qx-cli/src" / f"{name}.rs"
        if not path.exists():
            oversized.append(f"{name}.rs 不存在")
        elif len(path.read_text(encoding="utf-8").splitlines()) >= OVERSIZED:
            oversized.append(path.name)
    check(
        not oversized,
        "Phase 4p/4q 拆出的兄弟模块逐个在单文件行数门槛内",
        f"越界 {oversized or '无'}",
    )
    unmounted = [
        name
        for name in CLI_P4P_MODULES
        if f"mod {name};" not in root or f"pub(crate) use {name}::*;" not in root
    ]
    check(
        not unmounted,
        "拆出的模块在 crate 根以 mod + pub(crate) use x::* 成对挂载",
        f"缺配对 {unmounted or '无'}",
    )
    items = sum(1 for line in root.splitlines() if ROOT_ITEM.match(line))
    check(
        items <= CLI_ROOT_ITEM_CEILING,
        f"crate 根顶层条目不多于 {CLI_ROOT_ITEM_CEILING} 个（实现不得长回 main.rs）",
        f"当前 {items} 个",
    )
    root_lines = len(root.splitlines())
    budget = (ROOT / "maturity/line_budgets.yaml").read_text(encoding="utf-8").splitlines()
    still_registered = any(line.startswith("crates/qx-cli/src/main.rs:") for line in budget)
    check(
        root_lines < OVERSIZED and not still_registered,
        "crate 根 main.rs 已退出行数登记集（低于门槛且不在预算表内）",
        f"当前 {root_lines} 行，门槛 {OVERSIZED}，登记={still_registered}",
    )
    sources = sorted((CRATES / "qx-cli/src").rglob("*.rs"))
    for symbol, (owner, pattern) in CLI_CHAIN_SYMBOLS.items():
        regex = re.compile(pattern, re.MULTILINE)
        sites = [
            path.relative_to(CRATES / "qx-cli/src").as_posix()
            for path in sources
            if regex.search(path.read_text(encoding="utf-8"))
        ]
        check(
            sites == [owner],
            f"链路入口 {symbol} 的定义点唯一且在 {owner}",
            f"定义于 {sites}",
        )


# Phase 4s：`crates/qx-cli/src/backtests.rs`（1,379 行）按回测入口拆成目录模块，原单文件
# 退出行数登记集 —— 生产代码侧的第一次（用例侧的两次是 Phase 4o / 4r）。这里钉的是"搬出去
# 的形状不得自己长回来"：单文件不得复活、六个主题模块逐个在门槛内、在 mod.rs 成对挂载、
# 每条回测链的入口只有一处定义、共享装配留在 mod.rs 且其条目数不增。
CLI_BACKTESTS_DIR = "qx-cli/src/backtests"
CLI_BACKTESTS_MODULES = (
    "artifacts",
    "depth",
    "fast_backtest",
    "multi_builtin",
    "single_strategy",
    "strategy_backtest",
)
# mod.rs 只允许留共享装配（Phase 4s 拆完是 7 个顶层条目）；实现写回即越界。
CLI_BACKTESTS_MOUNT_CEILING = 8
BACKTEST_ENTRY_OWNERS = {
    "run_multi_builtin_backtest": "multi_builtin.rs",
    "run_ccxt_builtin_backtest": "multi_builtin.rs",
    "run_strategy_backtest": "strategy_backtest.rs",
    "persist_backtest_artifacts": "artifacts.rs",
    "run_fast_backtest_manifest": "fast_backtest.rs",
    "run_single_strategy_backtest": "single_strategy.rs",
    "run_builtin_backtest": "single_strategy.rs",
    "run_depth_backtest": "depth.rs",
}


def cli_backtest_module_check() -> None:
    """回测编排目录模块的形状门禁（Phase 4s）。"""
    check(
        not (CRATES / "qx-cli/src/backtests.rs").exists(),
        "回测编排单文件 backtests.rs 已拆分且不得复活",
        "crates/qx-cli/src/backtests.rs 重新出现",
    )
    oversized = []
    for name in CLI_BACKTESTS_MODULES:
        path = CRATES / CLI_BACKTESTS_DIR / f"{name}.rs"
        if not path.exists():
            oversized.append(f"{name}.rs 不存在")
        elif len(path.read_text(encoding="utf-8").splitlines()) >= OVERSIZED:
            oversized.append(path.name)
    check(
        not oversized,
        "Phase 4s 拆出的回测主题模块逐个在单文件行数门槛内",
        f"越界 {oversized or '无'}",
    )
    mount = (CRATES / CLI_BACKTESTS_DIR / "mod.rs").read_text(encoding="utf-8")
    unmounted = [
        name
        for name in CLI_BACKTESTS_MODULES
        if f"mod {name};" not in mount or f"pub(crate) use {name}::*;" not in mount
    ]
    check(
        not unmounted,
        "回测主题模块在 backtests/mod.rs 以 mod + pub(crate) use x::* 成对挂载",
        f"缺配对 {unmounted or '无'}",
    )
    items = sum(1 for line in mount.splitlines() if ROOT_ITEM.match(line))
    check(
        items <= CLI_BACKTESTS_MOUNT_CEILING,
        f"回测共享装配在 mod.rs 的顶层条目不多于 {CLI_BACKTESTS_MOUNT_CEILING} 个（实现不得长回 mod.rs）",
        f"当前 {items} 个",
    )
    sources = sorted((CRATES / CLI_BACKTESTS_DIR).glob("*.rs"))
    for symbol, owner in BACKTEST_ENTRY_OWNERS.items():
        regex = re.compile(rf"^(?:pub(?:\(crate\))? )?fn {symbol}\(", re.MULTILINE)
        sites = [
            path.name
            for path in sources
            if regex.search(path.read_text(encoding="utf-8"))
        ]
        check(
            sites == [owner],
            f"回测入口 {symbol} 的定义点唯一且在 backtests/{owner}",
            f"定义于 {sites}",
        )


def bare_risk_gate_check() -> None:
    seen: dict[str, list[int]] = {}
    unseeded: list[str] = []
    for path in rust_sources():
        rel = path.relative_to(ROOT).as_posix()
        lines = path.read_text(encoding="utf-8").splitlines()
        for index, line in enumerate(lines):
            if "RiskGate::new()" not in line:
                continue
            if "/tests/" in rel or rel.endswith("_tests.rs") or is_test_scoped(index, lines):
                continue
            seen.setdefault(rel, []).append(index + 1)
            # 空风控门 = fail-open：构造后 5 行内必须 add 规则，或显式接收外部 RuleSet。
            window = " ".join(lines[index : index + 6])
            if ".add(" not in window and "strategy_risk_gate" not in window:
                unseeded.append(f"{rel}:{index + 1}")
    unexpected = {rel: lines for rel, lines in seen.items() if rel not in BARE_RISK_GATE_ALLOWLIST}
    drifted = sorted(
        rel
        for rel, want in BARE_RISK_GATE_ALLOWLIST.items()
        if len(seen.get(rel, [])) != want
    )
    check(
        not unexpected and not drifted and not unseeded,
        "生产代码不存在未登记/无规则的风控门",
        f"未登记 {unexpected or '无'}；白名单计数变化 {drifted or '无'}；无规则 {unseeded or '无'}",
    )



EXEC_CORE_FILE = "crates/qx-execution/src/lib.rs"
SPREAD_BARRIER_FILE = "crates/qx-zhenlu/src/lib.rs"
SPREAD_BARRIER_CALL = "spread_group_barrier("
# 生产提交入口：任何在此类文件中调用它们的语句都是"提交一条腿"的现场，
# 必须先过多腿屏障。新增入口时把其所在文件纳入本清单，并接上屏障。
LEG_SUBMIT_ENTRY = re.compile(
    r"execute_paper_submit_effect\(|execute_submit_order_with_worker_risk\("
)


def execution_single_track_check() -> None:
    """订单副作用只允许一套实现（Phase 3 的"执行单轨化"）。

    遗留的 `ExecutionService`/`SpreadExecutionService` 与只在单元测试中构造的
    `MultiVenueSpreadExecutionService`+`VenueRouterMap` 均已删除：单腿与多腿共用
    同一个 `ExecutionGateway`（= `PortExecutionService`），多腿安全性由策略 worker
    的组快照、EventLog 归约、`HedgeRecoveryWorker` 补偿，以及提交前的
    `spread_group_barrier` 共同保证。这里钉住"不得再出现第二套实现/第二编排入口"，
    而不是给它们留白名单。正则用前后否定环视排除 `PortExecutionService` 等更长标识符。
    """
    legacy_tokens = {
        "第二套执行实现（ExecutionService/SpreadExecutionService）": re.compile(
            r"(?<![A-Za-z0-9_])(?:ExecutionService|SpreadExecutionService)(?![A-Za-z0-9_])"
        ),
        "第二多腿编排入口（MultiVenueSpreadExecutionService/VenueRouterMap）": re.compile(
            r"(?<![A-Za-z0-9_])(?:MultiVenueSpreadExecutionService|VenueRouterMap)(?![A-Za-z0-9_])"
        ),
    }
    for label, token in legacy_tokens.items():
        revived = {}
        for path in sorted(CRATES.glob("*/**/*.rs")):
            hits = len(token.findall(path.read_text(encoding="utf-8")))
            if hits:
                revived[path.relative_to(ROOT).as_posix()] = hits
        check(
            not revived,
            f"{label}已彻底移除且不得复活",
            f"重新出现 {revived or '无'}",
        )
    text = (ROOT / EXEC_CORE_FILE).read_text(encoding="utf-8")
    start = text.find("pub fn execute_paper_submit_effect")
    body = text[start : start + text[start:].find("\n}\n")] if start >= 0 else ""
    check(
        start >= 0 and "ExecutionGateway::new" in body,
        "Paper 提交与 worker 共用 ExecutionGateway，不回退第二套实现",
        "execute_paper_submit_effect 未走 ExecutionGateway",
    )
    gateways = len(re.findall(r"^pub type ExecutionGateway\b", text, re.MULTILINE))
    check(
        gateways == 1,
        "单腿网关别名只有一处定义（新增第二套实现须先改本门禁）",
        f"定义 {gateways} 处（期望 1）",
    )
    barrier_predicate = re.compile(r"^\s*pub fn blocks_new_leg_submission", re.MULTILINE)
    definitions = [
        path.relative_to(ROOT).as_posix()
        for path in sorted(CRATES.glob("*/**/*.rs"))
        if barrier_predicate.search(path.read_text(encoding="utf-8"))
    ]
    check(
        definitions == [SPREAD_BARRIER_FILE],
        "多腿停止提交的安全谓词只在 qx-zhenlu 定义一处（唯一口径）",
        f"定义于 {definitions}",
    )
    uncovered = []
    for path in sorted(CRATES.glob("qx-cli/src/**/*.rs")):
        rel = path.relative_to(CRATES / "qx-cli/src")
        # 用例文件里的提交调用是"被测现场"而不是生产入口。Phase 4o 起用例集中在
        # `src/tests/` 目录模块，该目录由 main.rs 的 `#[cfg(test)] mod tests;` 挂载，
        # 生产路径不可能落在里面（同一门禁另有专项断言钉住这个挂载写法）。
        if rel.parts[0] == "tests" or "test" in path.stem:
            continue
        source = path.read_text(encoding="utf-8")
        if LEG_SUBMIT_ENTRY.search(source) and SPREAD_BARRIER_CALL not in source:
            uncovered.append(path.relative_to(ROOT).as_posix())
    check(
        not uncovered,
        "每条腿的提交入口都在执行前调用 spread_group_barrier",
        f"未接屏障 {uncovered or '无'}",
    )


ASHARE_RULES_FILE = "crates/qx-xingban/src/ashare.rs"
ASHARE_PIT_TEST_FILE = "crates/qx-xingban/tests/ashare_pit_asof.rs"


def ashare_pit_check() -> None:
    """A 股公司行为的 PIT 闸门只有一处：`as_of` 过滤发生在 JSON 加载点。

    收口轮删除了零调用者的第二道闸门（`is_visible_at` /
    `corporate_actions_visible_at`）：规则快照里并不携带 `as_of` 供运行期复查，
    留着只会让人误以为回测侧还有第二次过滤。这里同时钉住"结果可区分"的测试存在。
    """
    removed = re.compile(
        r"(?<![A-Za-z0-9_])(?:is_visible_at|corporate_actions_visible_at)(?![A-Za-z0-9_])"
    )
    revived = {
        path.relative_to(ROOT).as_posix(): len(removed.findall(path.read_text(encoding="utf-8")))
        for path in sorted(CRATES.glob("*/**/*.rs"))
        if removed.findall(path.read_text(encoding="utf-8"))
    }
    check(
        not revived,
        "第二道 A 股 PIT 闸门（is_visible_at/corporate_actions_visible_at）已彻底移除",
        f"重新出现 {revived or '无'}",
    )
    text = (ROOT / ASHARE_RULES_FILE).read_text(encoding="utf-8")
    definitions = len(re.findall(r"^    fn visible_at\(", text, re.MULTILINE))
    callers = len(re.findall(r"contract\.visible_at\(", text))
    check(
        definitions == 1 and callers == 1,
        "A 股 PIT 可见性判定只有一个谓词函数、一个加载闸门调用点",
        f"定义 {definitions} 处、调用 {callers} 处（各期望 1）",
    )
    engine = (CRATES / "qx-xingban/src/backtest.rs").read_text(encoding="utf-8")
    hits = engine.count("published_at_ms")
    check(
        hits == 0,
        "回测引擎不自行判定 PIT 可见性（只消费加载后的快照）",
        f"backtest.rs 出现 published_at_ms {hits} 处",
    )
    pit_test = (ROOT / ASHARE_PIT_TEST_FILE).read_text(encoding="utf-8")
    cutoffs = len(re.findall(r'snapshot\("', pit_test))
    check(
        cutoffs >= 2 and "result_hash()" in pit_test and "assert_ne!" in pit_test,
        "A 股 PIT 结果可区分测试在位（同一数据集、不同 as_of）",
        f"研究截止日取样 {cutoffs} 次（期望 >=2）",
    )


def backtest_assembly_check() -> None:
    # Phase 4s 起回测编排是目录模块：口径仍是"这一族文件合起来只有一份装配"，
    # 所以按目录聚合读取，而不是钉死某个单文件路径。
    text = "".join(
        path.read_text(encoding="utf-8")
        for path in sorted((CRATES / "qx-cli/src/backtests").glob("*.rs"))
    )
    literals = re.findall(r"^\s*BacktestConfig \{$", text, re.MULTILINE)
    check(
        len(literals) == 1 and "fn into_config(self) -> BacktestConfig" in text,
        "Bar 回测引擎装配只有一份（BarBacktestAssembly::into_config）",
        f"BacktestConfig 字面量 {len(literals)} 处",
    )
    check(
        text.count("market_spec_with_margin(") >= 4,
        "market spec 读取共用单一入口",
        f"market_spec_with_margin 调用 {text.count('market_spec_with_margin(')} 处",
    )
    gates = len(re.findall(r"strategy_risk_gate\(", text))
    check(gates >= 4, "回测风控门全部经 strategy_risk_gate 构造", f"共 {gates} 处")


def capabilities_check() -> None:
    path = ROOT / "maturity/capabilities.yaml"
    if not path.exists():
        check(False, "maturity/capabilities.yaml 存在")
        return
    lines = path.read_text(encoding="utf-8").splitlines()
    blocks: dict[str, dict[str, str]] = {}
    inside = False
    current: str | None = None
    for line in lines:
        if re.match(r"^capabilities:\s*$", line):
            inside = True
            continue
        if inside and re.match(r"^\S", line):  # 下一个顶层键结束 capabilities 段
            inside, current = False, None
        if not inside:
            continue
        if key := re.match(r"^  ([a-z0-9_]+):\s*$", line):
            current = key.group(1)
            blocks[current] = {}
        elif current and (field := re.match(r"^    ([a-z_]+):\s*(\S.*)?$", line)):
            blocks[current][field.group(1)] = (field.group(2) or "").strip()
    required = {"implementation", "code_tested", "sandbox_tested", "production_approved"}
    incomplete = {
        name: sorted(required - set(fields))
        for name, fields in blocks.items()
        if required - set(fields)
    }
    # 声称已实现/已测但拿不出证据 = 空洞声明；反之未实现项必须写明受限原因。
    no_evidence = sorted(
        name
        for name, fields in blocks.items()
        if fields.get("implementation") == "true" or fields.get("code_tested") == "true"
        if "evidence" not in fields
    )
    undocumented = sorted(
        name for name, fields in blocks.items()
        if fields.get("implementation") not in (None, "true", "false")
        and "limitations" not in fields
    )
    check(
        len(blocks) >= 18 and not incomplete and not no_evidence and not undocumented,
        "能力矩阵每条都带四档状态、证据与未实现原因",
        f"条目 {len(blocks)}；缺字段 {incomplete or '无'}；缺证据 {no_evidence or '无'}；"
        f"缺受限说明 {undocumented or '无'}",
    )
    pattern = re.compile(r"^\s+-\s+(\S+\.(?:rs|py|json|yaml|toml|sh|sql|md|txt))\s*$")
    missing = sorted(
        {
            match.group(1)
            for line in lines
            if (match := pattern.match(line)) and not (ROOT / match.group(1)).exists()
        }
    )
    check(not missing, "能力矩阵证据路径全部存在", f"失效路径 {missing}")
    claimed_sandbox = sorted(
        name for name, fields in blocks.items() if fields.get("sandbox_tested") == "true"
    )
    check(
        not claimed_sandbox,
        "未拿到外部沙盒记录前 sandbox_tested 全为 false",
        f"越界声明 {claimed_sandbox}",
    )


VENUE_REPORT_TEST = "crates/qx-execution/tests/venue_report_contract.rs"
REPORT_FUNNEL_FILE = "crates/qx-execution/src/lib.rs"
SPEC_FUNNEL_DEFINITION = "crates/qx-core/src/trading.rs"
# 真实交易所回报的生产入口：一旦 worker 配了冻结规格，就必须把规格交给归约入口，
# 否则回报侧精度闸门形同虚设。新增生产回报路径时把文件加入本清单。
LIVE_REPORT_FILES = (
    "crates/qx-cli/src/venue_runtime/binance_stream_worker.rs",
    "crates/qx-cli/src/venue_runtime/ccxt_execution.rs",
    "crates/qx-cli/src/venue_runtime/ccxt_reconcile_worker.rs",
)


def venue_report_contract_check() -> None:
    """交易所回报侧的迟到/乱序与精度越界纪律只有一处实现，且三家共用一份契约测试。

    V9 §8.3 第 8 项收口：越界或迟到的回报只能留下 `ReconcileRequired` 事实，绝不能
    伪造成交或改写终态。闸门刻意放在 `ingest_venue_events_with_pipeline`（所有生产回报
    路径的唯一漏斗）而不是 Ledger 里——回测直接调 Ledger，放进 Ledger 会让实盘与回测
    口径分叉。这里钉住"唯一调用点 + 生产路径都带规格 + 三家 fixture 共用断言"。
    """
    funnel = (ROOT / REPORT_FUNNEL_FILE).read_text(encoding="utf-8")
    start = funnel.find("fn ingest_venue_events_with_pipeline")
    body = funnel[start : funnel.find("\n}\n", start)] if start >= 0 else ""
    check(
        start >= 0
        and body.count("validate_fill(") == 1
        and "ReconcileRequired" in body
        and "精度越界" in body,
        "成交回报精度闸门只在唯一归约入口生效并转待对账",
        f"入口存在={start >= 0}，闸门调用 {body.count('validate_fill(')} 处（期望 1）",
    )
    definer = (ROOT / SPEC_FUNNEL_DEFINITION).read_text(encoding="utf-8")
    predicates = len(re.findall(r"fn validate_fill\(", definer))
    copies = {
        path.relative_to(ROOT).as_posix(): path.read_text(encoding="utf-8").count("validate_fill(")
        for path in sorted(rust_sources())
        if path.relative_to(ROOT).as_posix() not in (REPORT_FUNNEL_FILE, SPEC_FUNNEL_DEFINITION)
        and "validate_fill(" in path.read_text(encoding="utf-8")
    }
    check(
        predicates == 1 and not copies,
        "回报精度判定只有一个谓词、一个调用点（不在 Ledger/回测侧重复判定）",
        f"定义 {predicates} 处；额外调用点 {copies or '无'}",
    )
    uncovered = [
        rel
        for rel in LIVE_REPORT_FILES
        if "ingest_venue_events(" in (ROOT / rel).read_text(encoding="utf-8")
        and "worker_report_spec(" not in (ROOT / rel).read_text(encoding="utf-8")
        and "load_worker_instrument_spec(" not in (ROOT / rel).read_text(encoding="utf-8")
    ]
    check(
        not uncovered,
        "每条生产回报路径都把冻结产品规格交给归约入口",
        f"未接规格 {uncovered or '无'}",
    )
    contract = (ROOT / VENUE_REPORT_TEST).read_text(encoding="utf-8")
    venues = len(re.findall(r"assert_venue_report_contract\(&mut", contract))
    check(
        venues == 3 and "精度越界" in contract and '"reconcile"' in contract,
        "Paper / CCXT / Binance 三家共用迟到回报与精度越界契约测试",
        f"共用断言调用 {venues} 次（期望 3）",
    )


LEDGER_DIR = "crates/qx-core/src/ledger"
LEDGER_TYPES = "crates/qx-core/src/ledger/mod.rs"


def ledger_kernel_split_check() -> None:
    """内核账簿按资产类别拆成目录模块，被拆分的单文件不得复活（V9 §3.2 第 5 项）。

    拆分刻意"只搬行不改语义"：`Ledger` 的类型与核心归约留在 `ledger/mod.rs`，
    成交/现金/公司行为/配股/认购可转债/只读投影各自的 `impl Ledger` 块各占一个文件，
    内核用例只用公开 API、整体迁到 `crates/qx-core/tests/ledger.rs`。
    这里钉住三条：旧单文件不存在、`Ledger` 结构体定义唯一且在 mod.rs、
    所有 `impl Ledger` 块只出现在该目录内且逐个都在行数门槛之下。
    """
    check(
        not (ROOT / "crates/qx-core/src/ledger.rs").exists(),
        "内核 Ledger 单文件已拆分且不得复活",
        "crates/qx-core/src/ledger.rs 重新出现",
    )
    definitions = [
        path.relative_to(ROOT).as_posix()
        for path in sorted(CRATES.glob("*/**/*.rs"))
        if re.search(r"^pub struct Ledger \{$", path.read_text(encoding="utf-8"), re.MULTILINE)
    ]
    check(
        definitions == [LEDGER_TYPES],
        "账簿状态结构体只有一处定义",
        f"定义于 {definitions}",
    )
    blocks = {}
    oversized = {}
    for path in sorted((ROOT / LEDGER_DIR).glob("*.rs")):
        text = path.read_text(encoding="utf-8")
        rel = path.relative_to(ROOT).as_posix()
        blocks[rel] = len(re.findall(r"^impl Ledger \{$", text, re.MULTILINE))
        if len(text.splitlines()) > OVERSIZED:
            oversized[rel] = len(text.splitlines())
    misplaced = {
        path.relative_to(ROOT).as_posix(): len(
            re.findall(r"^impl Ledger \{$", path.read_text(encoding="utf-8"), re.MULTILINE)
        )
        for path in sorted(CRATES.glob("*/**/*.rs"))
        if path.relative_to(ROOT).as_posix() not in blocks
        and re.search(
            r"^impl Ledger \{$", path.read_text(encoding="utf-8"), re.MULTILINE
        )
    }
    check(
        len(blocks) >= 6 and all(count == 1 for count in blocks.values()) and not misplaced,
        "账簿归约实现按资产类别分文件，且不在目录外另起 impl Ledger",
        f"目录内 impl 块 {blocks}；目录外 {misplaced or '无'}",
    )
    check(
        not oversized,
        "账簿各子模块都在单文件行数门槛之内",
        f"超限 {oversized or '无'}",
    )


def concept_registry_check() -> None:
    """同名概念的权威定义必须唯一，分层投影必须走登记过的那一个（V9 §3.2 第 6 项）。

    审计时 Position / Bar / Intent 三组概念各有 2–4 份定义，其中"不同层各一份、
    层间只有一个转换函数"是合法的；真正的问题是同一角色被重复实现——
    `qx-zhenlu` 与 `qx-genglu` 各写了一份 `PositionSnapshot` 持仓投影，
    与 `qx-risk::OrderRiskPosition` 承担同一角色，字段与算法逐字重复。
    Phase 4l 删掉那两份重复实现后，这里用一张登记表钉住"每个概念名允许在哪些文件里定义"：
    多一处（另起第二份）与少一处（把登记里的定义搬走或改名）都判红。
    """
    sources = sorted(CRATES.glob("*/**/*.rs"))
    registry = {
        # 持仓：内核账簿状态 / 风控投影 / 交易所回报线格式，三个角色各一份、名字互不相同。
        "PositionState": ["crates/qx-core/src/ledger/mod.rs"],
        "OrderRiskPosition": ["crates/qx-risk/src/lib.rs"],
        "PositionSnapshot": ["crates/qx-protocol/src/lib.rs"],
        # K 线：撮合与回测用的引擎 Bar（列式数据集到它只有一次投影）+ 数据集侧逐条记录。
        "Bar": ["crates/qx-data/src/schema.rs", "crates/qx-guanxing/src/lib.rs"],
        # 订单意图：SDK 原生 / 跨语言 JSON 契约 / C ABI 镜像 / 风控前的下单意图。
        "StrategyOrderIntent": ["crates/qx-strategy/src/lib.rs"],
        "StrategyContractIntent": ["crates/qx-runtime/src/lib.rs"],
        "QxOrderIntent": ["crates/qx-strategy/src/c_api.rs"],
        "OrderIntent": ["crates/qx-zhenlu/src/lib.rs"],
    }
    for name, allowed in sorted(registry.items()):
        pattern = re.compile(rf"^pub struct {name} \{{$", re.MULTILINE)
        found = [
            path.relative_to(ROOT).as_posix()
            for path in sources
            if pattern.search(path.read_text(encoding="utf-8"))
        ]
        check(
            found == sorted(allowed),
            f"概念 {name} 的定义位置与登记表一致",
            f"登记 {sorted(allowed)} / 实际 {found}",
        )
    projections = [
        path.relative_to(ROOT).as_posix()
        for path in sources
        if "impl From<&BarFrame> for Vec<Bar>" in path.read_text(encoding="utf-8")
    ]
    check(
        projections == ["crates/qx-datastruct/src/lib.rs"],
        "数据集列式 BarFrame 到引擎 Bar 只有一次投影定义",
        f"投影实现于 {projections}",
    )
    # 被删掉的第二份持仓投影与其归约入口不得复活：它们既无风控预检也无 fail-closed 门禁。
    revived = {}
    for identifier in ("reconcile_positions", "validate_against", "position_map"):
        found = [
            path.relative_to(ROOT).as_posix()
            for path in sources
            if re.search(rf"\b{identifier}\b", path.read_text(encoding="utf-8"))
        ]
        if found:
            revived[identifier] = found
    check(
        not revived,
        "已删除的第二套持仓归约与死校验器不得复活",
        f"重新出现于 {revived}",
    )
    # 持仓可用量口径只能有一个实现，否则回测/Paper/Live 会在"哪部分仓位算已占用"上分叉。
    occupancy = [
        path.relative_to(ROOT).as_posix()
        for path in sources
        if re.search(r"fn active_qty_for\b", path.read_text(encoding="utf-8"))
    ]
    check(
        occupancy == ["crates/qx-risk/src/lib.rs"],
        "持仓可用量判定只有一个实现",
        f"定义于 {occupancy}",
    )


CONFIG_STRUCTS = (
    "TlsPaths",
    "OperatorConfig",
    "ApiRuntimeConfig",
    "StorageRuntimeConfig",
    "MessagingRuntimeConfig",
    "WorkerConfig",
    "CredentialEnv",
    "CredentialFiles",
    "SchedulerRuntimeConfig",
    "RiskRulesConfig",
    "StrategyRuntimeConfig",
    "RuntimeConfig",
)

# 配置面之外、却同样从 JSON 反序列化的公开结构体：策略侧 JSONL 契约与快照/健康输出。
# Phase 4m 只收运行时配置面，没有顺手收紧这些类型的宽松度——那会改变 worker 协议与对外
# 输出的兼容边界，是独立决策。它们在此显式登记，新的可反序列化结构体必须进这两张表之一。
LOOSE_DESERIALIZE_STRUCTS = (
    "StrategyContractInput",
    "StrategyContractBars",
    "StrategyContractIntent",
    "StrategyContractOutput",
    "StrategyContext",
    "StrategyTargetSnapshot",
    "ServiceHealth",
    "HealthSnapshot",
)


def struct_attribute_blocks(text: str) -> dict[str, str]:
    """把每个行首 `pub struct` 紧邻其上的属性行并成一段文本，供属性级判定使用。

    只向上收集到第一个非属性、非注释、非空行为止，因此缩进在 `mod tests` 里的结构体
    与上一个条目的尾部代码都不会被误算进来。
    """
    lines = text.splitlines()
    blocks: dict[str, str] = {}
    for index, line in enumerate(lines):
        matched = re.match(r"^pub struct (\w+)\b", line)
        if not matched:
            continue
        collected: list[str] = []
        cursor = index - 1
        while cursor >= 0:
            previous = lines[cursor].strip()
            if previous.startswith("#["):
                collected.insert(0, previous)
            elif previous == "" or previous.startswith("//"):
                pass
            else:
                break
            cursor -= 1
        blocks[matched.group(1)] = " ".join(collected)
    return blocks


def runtime_config_fail_closed_check() -> None:
    """运行时配置面必须 fail-closed（V9 §3.2 第 4 项）。

    审计记录说"配置面是 Option 字段海，非法组合在反序列化与启动时不失败"。
    复核时先确认了两点不再成立的部分（全部读取路径经 `RuntimeConfig::from_json`、
    `validate()` 已有 460+ 行判定），但仍留下三类真实缺口：

    1. 配置结构体没有一处开启 `deny_unknown_fields`，把 `max_order_notional_raw`
       拼错就等价于"没配这条风控"，静默通过；
    2. 角色可选字段的可见性散落在 `validate()` 与 CLI 分支里，凭据/端点/规格这类
       字段配给永不读取它的角色（例如 api worker 带凭据）不会报错，而且这类判定
       只对 `enabled` 的 worker 生效；
    3. 凭据来源"二选一"只在 Binance 分支生效，其他 Venue 可以同时给两套或给一份
       半空的凭据。

    Phase 4m 把这三处收进类型系统（`qx-runtime::worker_policy`），这里钉住形状。
    """
    lib = (CRATES / "qx-runtime" / "src" / "lib.rs").read_text(encoding="utf-8")
    policy = (CRATES / "qx-runtime" / "src" / "worker_policy.rs").read_text(encoding="utf-8")
    blocks = struct_attribute_blocks(lib)
    missing = [
        name
        for name in CONFIG_STRUCTS
        if "deny_unknown_fields" not in blocks.get(name, "")
    ]
    check(
        not missing,
        "运行时配置结构体全部拒绝未知键",
        f"缺少 deny_unknown_fields: {missing}",
    )
    # 反向：名单不再按结构体名字后缀猜测，而是扫"`Deserialize` + 行首 `pub struct`"这一事实。
    # 新增可反序列化的公开结构体若既不进配置名单、也不进宽松名单，就会在这里报红，
    # 而不是带着"静默接受未知键"的状态进入运行时配置面。
    deserializable = {name for name, block in blocks.items() if "Deserialize" in block}
    check(
        deserializable - set(CONFIG_STRUCTS) == set(LOOSE_DESERIALIZE_STRUCTS),
        "运行时配置结构体名单与登记表一致",
        f"名单 {sorted(CONFIG_STRUCTS)} / 宽松 {sorted(LOOSE_DESERIALIZE_STRUCTS)} / "
        f"源码可反序列化 {sorted(deserializable)}",
    )
    check(
        lib.count("fn strip_config_comments") == 1
        and "let payload = strip_config_comments(payload)?;" in lib,
        "配置注释键剥离只有一处实现且被 from_json 使用",
        f"定义 {lib.count('fn strip_config_comments')} 次",
    )
    check(
        policy.count("pub enum FieldScope") == 1
        and policy.count("pub const fn field_scopes") == 1,
        "worker 角色字段可见性策略只有一处声明",
        f"FieldScope {policy.count('pub enum FieldScope')} 处 / field_scopes {policy.count('pub const fn field_scopes')} 处",
    )
    fn_body = policy.split("pub const fn field_scopes", 1)[-1].split("\n    }\n", 1)[0]
    check(
        "_ =>" not in fn_body,
        "角色字段策略逐角色声明，不得使用通配臂",
        "策略表里出现了 `_ =>` 通配臂",
    )
    variants = set(re.findall(r"^    (\w+),$", lib.split("pub enum WorkerRole {", 1)[-1].split("\n}", 1)[0], re.M))
    scoped = set(re.findall(r"Self::(\w+)", fn_body))
    check(
        variants == scoped and len(variants) == 10,
        "角色字段策略覆盖全部 WorkerRole 变体",
        f"枚举 {sorted(variants)} / 策略表 {sorted(scoped)}",
    )
    # 凭据来源判定必须与 Venue 无关：把规则收回 is_binance 分支就是回到旧缺口。
    check(
        "is_binance" not in policy,
        "凭据来源唯一性判定与 Venue 无关",
        "worker_policy.rs 重新按 Venue 收窄了凭据判定",
    )
    check(
        "if !worker.enabled" in lib
        and lib.index("worker.role_field_status()") < lib.index("if !worker.enabled"),
        "字段可见性判定先于 enabled 短路",
        "role_field_status 被挪到 enabled 分支之后，禁用 worker 会绕过校验",
    )
    sources = [path for path in sorted(CRATES.glob("*/src/**/*.rs")) if "worker_policy.rs" not in path.name]
    duplicated = [
        path.relative_to(ROOT).as_posix()
        for path in sources
        if re.search(r"const VENUE_ROLES|fn is_venue_role\b|fn uses_private_venue\b", path.read_text(encoding="utf-8"))
    ]
    check(
        not duplicated,
        "角色白名单不得在策略表之外另抄一份",
        f"重复出现于 {duplicated}",
    )


def source_line_counts() -> dict[str, int]:
    return {
        path.relative_to(ROOT).as_posix(): len(path.read_text(encoding="utf-8").splitlines())
        for path in sorted(CRATES.glob("*/src/**/*.rs"))
    }


def write_line_budgets() -> int:
    counts = {rel: n for rel, n in source_line_counts().items() if n > OVERSIZED}
    body = "\n".join(f"{rel}: {n}" for rel, n in sorted(counts.items()))
    LINE_BUDGETS.write_text(
        "# 单文件行数棘轮快照，由 tools/check_architecture.py --snapshot 生成。\n"
        f"# 只允许数值下降；确需增长或新增超 {OVERSIZED} 行文件时重新生成并审阅 diff。\n{body}\n",
        encoding="utf-8",
        newline="\n",
    )
    print(f"已写入 {LINE_BUDGETS.relative_to(ROOT).as_posix()}（{len(counts)} 个超 {OVERSIZED} 行文件）")
    return 0


def read_line_budgets() -> dict[str, int]:
    if not LINE_BUDGETS.exists():
        return {}
    return {
        match.group(1): int(match.group(2))
        for line in LINE_BUDGETS.read_text(encoding="utf-8").splitlines()
        if (match := re.match(r"^(\S+\.rs):\s*(\d+)\s*$", line))
    }


def line_budget_check() -> None:
    budgets = read_line_budgets()
    if not budgets:
        check(
            False,
            "行数棘轮快照存在",
            f"缺失 {LINE_BUDGETS.relative_to(ROOT).as_posix()}，运行 --snapshot 生成",
        )
        return
    counts = source_line_counts()
    violations = [
        f"{rel} 已不存在，请重新生成快照"
        for rel in budgets
        if rel not in counts
    ] + [
        f"{rel} {counts[rel]} 行 > 预算 {budgets[rel]}"
        for rel in budgets
        if rel in counts and counts[rel] > budgets[rel]
    ] + [
        f"{rel}({counts[rel]} 行) 未登记"
        for rel in sorted(counts)
        if rel not in budgets and counts[rel] > OVERSIZED
    ]
    check(not violations, "单文件行数预算只降不升、无未登记的超大文件", "；".join(violations))


def main() -> int:
    if "--snapshot" in sys.argv:
        return write_line_budgets()
    removed_crates_check()
    python_interpreter_check()
    worker_diagnostics_check()
    cli_dispatch_check()
    test_module_shape_check()
    cli_root_module_check()
    cli_backtest_module_check()
    bare_risk_gate_check()
    execution_single_track_check()
    ashare_pit_check()
    venue_report_contract_check()
    ledger_kernel_split_check()
    concept_registry_check()
    runtime_config_fail_closed_check()
    backtest_assembly_check()
    capabilities_check()
    line_budget_check()
    print()
    if failures:
        print(f"架构不变量自检失败 {len(failures)} 项：")
        for failure in failures:
            print(f"  ✗ {failure}")
        return 1
    print(f"架构不变量自检全部通过 ✓（{checks} 项）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
