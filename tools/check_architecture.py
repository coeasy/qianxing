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
     `ExecutionGateway`，Paper 也走它）；多腿跨腿屏障的判定与执行同样只在网关一处，
     CLI 侧既不保留实现也不保留"先查后提"的薄壳，且每个提交入口都必须显式回答
     `spread_store` 参数（V10 §6.1）；
  6. Bar 回测引擎装配只有一份（`BacktestConfig {` 字面量唯一）；撮合口径同样只有一个构造点
     （`backtests/fill_model.rs`），装配字段取自绑定，且 `strategy.fill_model` 既有 schema
     声明又被 `config validate` 覆盖，并有"换模型 → 结果与描述子变化"的行为用例（V11 Q1a）；
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
  15. 产品规格（market spec）JSON 的读法只有一处（`market_spec.rs`，V11 Q54）：回测链与 worker
      都必须调用同一个 loader，生产代码不得在别处反序列化或构造 `TradingInstrumentSpec`，
      三项定点精度（价格一档 / 数量步长 / 最小下单量）不得从缺失字段兜底出来。
  16. "哪些内置策略是双腿套利"这一条事实的两处表述（内核谓词 `needs_reference_leg` 与 CLI
      准入名单 `MULTI_LEG_KINDS`）必须逐项相等：改一边不改另一边会让某个入口悄悄放行。
  17. `--config` 的 A 股段在回测侧只有一份读法（`backtests/ashare_binding.rs`，V11 Q61）：两条
      Bar 回测链都通过它绑定规则与自带费率，承不了 A 股交易制度的深度档链与多腿链必须显式拒绝，
      且这四条处理方式都有命令行用例咬住结果差异。
  18. 回测产物必须声明"跑的是哪一份输入"，而且这句话得能被重算（V11 Q66 / Q1b）：输入身份只在
      `backtests/artifacts.rs` 一处读、一处写，落盘的摘要带 `input` 块（schema v3），`qx report`
      按声明路径走**同一个读点**重算并逐格比对，对不上或读不到就拒绝出报告；RunManifest 的
      `data_fingerprint` 回落用的是被注册表复核过的数据集指纹，不是引擎对自己切片的自哈希。
  19. "没算过的钱"在协议上必须说不清，而不是印成一个合法的 0（V11 Q67 账户级标量 / Q68 持仓行
      与实盘回报）：内核观察、线格式行、事件摘要、稳定 JSON 四条路径各自都只有一处表达缺席，
      且摘要与哈希带存在性标记；读模型自拼的持仓行承认算不出这两个钱字段；CCXT 持仓回报缺
      `side` 或带连接器的 `unknown` 占位时只能拒收——数量符号顺着权益、保证金、风控一路算下去，
      猜不得。
  20. C++ 插件 ABI 的两份镜像逐字段相等（V11 S13）：`cpp/include/qianxing_strategy.h` 与
      `crates/qx-strategy/src/c_api.rs` 之间没有编译期耦合，错位只会读成坏内存；字段名、顺序、
      宽度与 vtable 条目都按名字比对，CI 的 `cpp-sdk` 作业不加载产物，所以这条只能静态咬。
  21. 账户快照的对外契约只有一份文本，且两侧读侧与它同宽（V11 S10 / T3）：服务常量按字节
      `include_str!` 仓库里那份 schema，契约声明的键集合等于写侧产物，七个钱字段可空、权益不可空，
      `schema_version` 与协议名是 `from_json` 认的那一对，Python 侧的必填集合也等于契约的 required。
  22. 日历组件指纹两侧比同一份夹具（V11 R17 / T2）：Python 写侧产出文档与摘要、Rust 读侧重算，
      摘要在代码里没有第二份抄本，字段白名单与契约版本号逐项相等——旧格式文档 `sessions` 缺席
      必须读成"没有时段"，两侧一宽一严时 Python 登记的 bundle 会被 CLI 拒启。

运行： python3 tools/check_architecture.py
刷新第 8 项的预算快照（改动后人工确认 diff）：
       python3 tools/check_architecture.py --snapshot
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")

ROOT = Path(__file__).resolve().parent.parent
CRATES = ROOT / "crates"


def surface_text(roots: tuple[str, ...]) -> str:
    """把一个 crate 里若干"曾经是同一个文件"的源码拼成一份文本供形状断言扫描。

    V10 P2c 把 `qx-runtime/src/lib.rs`（3,660 行）拆成目录模块。按单文件路径取源码的
    门禁会因此静默失去覆盖面——空文本既不会命中 `deny_unknown_fields`，也不会命中重复
    定义。这里显式按"拆分后的落点集合"取文本：任一文件被搬走导致集合缺失时，相应断言
    仍然红，而不是悄悄变绿。
    """
    chunks: list[str] = []
    for root in roots:
        path = ROOT / root
        files = sorted(path.rglob("*.rs")) if path.is_dir() else [path]
        for file in files:
            chunks.append(file.read_text(encoding="utf-8"))
            # 文件边界必须是"断行"，否则上一文件末行会与下一文件首行粘连成假 token。
            chunks.append("\n")
    return "".join(chunks)
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
#    V10 §4.3 收口后留空：`all`/`verify` 的回测自校验已改调 `qx-xingban` 真实内核，
#    风控门统一由 `BarBacktestAssembly` 经 `strategy_risk_gate` 构造，CLI 里不再自带第二套。
BARE_RISK_GATE_ALLOWLIST: dict[str, int] = {}

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


# V10 §4.3 第 3/4 项 + §7.2 新不变量：帮助里印出的入口集合必须等于真正能派发的集合。
# P2b 之后派发不再是字符串比较而是 clap 派生的 `Command` 枚举，因此命令表的事实来源
# 变成 `cli_args.rs`（`#[command(name = "…")]` + 变体名），`cli.rs` 只交出显式分支。
CLI_HELP_FILE = "crates/qx-cli/src/cli_help.rs"
CLI_DISPATCH_FILE = "crates/qx-cli/src/cli.rs"
CLI_ARGS_FILE = "crates/qx-cli/src/cli_args.rs"
CLI_RUN_DISPATCH_FILE = "crates/qx-cli/src/config_commands.rs"
# `help` 的三种拼写在进入 clap 之前共用一条预检分支，帮助文本只登记规范名。
CLI_HELP_META_ALIASES = {"--help", "-h"}
CLI_HELP_PRECHECK = re.compile(r'Some\("help"\)\s*\|')
# 帮助用法行：恰好两空格缩进、首个 token 即命令名；说明行是六空格缩进。
HELP_USAGE_LINE = re.compile(r"^  ([a-z][a-z0-9-]*)$|^  ([a-z][a-z0-9-]*)[ \t]", re.MULTILINE)
HELP_RUN_LINE = re.compile(r"^  run <([a-z0-9|-]+)>", re.MULTILINE)
# clap 命令表条目：`#[command(name = "x")]` 紧跟其后的 `Command` 变体标识符。
CLAP_COMMAND_ENTRY = re.compile(
    r'#\[command\(name = "([a-z][a-z0-9-]*)"\)\]\s*\n\s*([A-Za-z][A-Za-z0-9]*)'
)
RUN_ARM_LINE = re.compile(r'^ {8}"([a-z][a-z0-9-]*)"(?:\s*\|\s*"([a-z][a-z0-9-]*)")* =>', re.M)
RUN_ENTRY_CONST = re.compile(r"const RUN_ENTRY_POINTS: \[&str; (\d+)\] = \[(.*?)\];", re.S)


def help_printed_commands(help_text: str) -> set[str]:
    """帮助正文里"印出来给人照着敲"的顶层命令名。"""
    names: set[str] = set()
    for line in help_text.splitlines():
        if not line.startswith("  ") or line.startswith("   "):
            continue  # 说明行（六空格）与段落标题（零缩进）都不是入口
        matched = HELP_USAGE_LINE.match(line)
        if matched:
            names.add(matched.group(1) or matched.group(2))
    return names


def clap_command_table(args_text: str) -> dict[str, str]:
    """clap 派生的顶层命令表：`命令名 -> Command 变体名`。"""
    start = args_text.find("pub(crate) enum Command {")
    if start < 0:
        return {}
    body = args_text[start:]
    end = body.find("\n}\n")
    return dict(CLAP_COMMAND_ENTRY.findall(body if end < 0 else body[:end]))


def dispatched_commands(cli_text: str, table: dict[str, str]) -> set[str]:
    """`cli.rs` 真正接住的命令名：clap 表里每个变体都要有一条显式 `Command::X` 分支。"""
    names = {
        name
        for name, variant in table.items()
        if re.search(rf"\bCommand::{variant}\b", cli_text)
    }
    if CLI_HELP_PRECHECK.search(cli_text):
        names.add("help")
    return names - CLI_HELP_META_ALIASES


def run_unified_arms(source: str) -> set[str]:
    """`run_unified_command` 的 match 分支实际接住的入口名。"""
    body = source.split("fn run_unified_command", 1)[-1].split("_ =>", 1)[0]
    arms: set[str] = set()
    for matched in RUN_ARM_LINE.finditer(body):
        arms.update(group for group in matched.groups() if group)
    return arms


# V11 Q0b 旗标诚实性：clap 声明的每个长旗标都必须被真正读到。
# `#[arg(long…)]` 只出现在带长旗标的字段上（位置参数用的是 value_parser 等其它元数据）。
CLI_LONG_FLAG_FIELD = re.compile(
    r'#\[arg\([^)]*long[^)]*\)\]\s*\n\s*(?P<name>[a-z_][a-z0-9_]*):'
)
# 分派里把字段绑成 `_` 就是"收下但不处理"的形状（`config: _,`）。
CLI_DISCARD_BINDING = re.compile(r'(?<![A-Za-z0-9_])(?P<name>[a-z_][a-z0-9_]*):\s*_\s*,')


def cli_flag_honesty_check() -> None:
    """被解析后丢弃的旗标比没有旗标更坏：使用者以为换了风控与费用口径，实际什么都没生效。

    V11 §4 P0 第 2 项的形状是 `Command::Backtest` 父命令声明 `--config`、`cli.rs:323` 以
    `config: _` 收下。收口后两条判据：分派正文里不得出现任何 `ident: _` 丢弃绑定；
    `cli_args.rs` 声明的每个长旗标字段都必须在 `cli.rs` 被点名（新声明却没人读的旗标同样红）。
    """
    args_text = (ROOT / CLI_ARGS_FILE).read_text(encoding="utf-8")
    cli_text = (ROOT / CLI_DISPATCH_FILE).read_text(encoding="utf-8")
    discards = sorted(
        {
            matched.group("name")
            for matched in CLI_DISCARD_BINDING.finditer(cli_text)
        }
    )
    check(
        not discards,
        "cli.rs 分派不得把 clap 字段绑成 `_` 后丢弃",
        f"丢弃形状 {discards or '无'}",
    )
    flags = sorted({matched.group("name") for matched in CLI_LONG_FLAG_FIELD.finditer(args_text)})
    unread = [name for name in flags if re.search(rf"\b{name}\b", cli_text) is None]
    check(
        bool(flags) and not unread,
        f"cli_args.rs 声明的 {len(flags)} 个长旗标全部被 cli.rs 读到",
        f"未出现在分派里 {unread or '（清单为空）'}",
    )


def cli_help_surface_check() -> None:
    """命令面诚实化：help 入口集合 ≡ clap 命令表 ≡ `cli.rs` 显式派发分支，且 `run` 子入口三处口径一致。

    三侧都从源码取事实：帮助正文每条入口独占一行（两空格缩进），命令表来自 `cli_args.rs`
    的 clap 派生，派发来自 `cli.rs` 的 `Command::X` 分支。派发有而帮助没写 = 存在没人知道的
    能力；帮助写了而派发没有 = 照着提示敲会拿到"未知命令"（V10 §4.3 第 4 项）；clap 有变体
    而 `cli.rs` 没分支 = 参数能解析却没有实现，是最坏的一种（编译期也只靠 `_ =>` 兜住）。
    `run` 再单独三方对齐：help 的 `run <a|b|…>` 行、`RUN_ENTRY_POINTS` 常量
    （错误文案由它拼出）、`run_unified_command` 的 match 分支。
    """
    help_source = (ROOT / CLI_HELP_FILE).read_text(encoding="utf-8")
    help_body = help_source.split('r#"', 1)[-1].split('"#', 1)[0]
    documented = help_printed_commands(help_body)
    table = clap_command_table((ROOT / CLI_ARGS_FILE).read_text(encoding="utf-8"))
    cli_source = (ROOT / CLI_DISPATCH_FILE).read_text(encoding="utf-8")
    dispatched = dispatched_commands(cli_source, table)
    # `help` 不经 clap 子命令（`disable_help_subcommand`），而是 clap 之前的同一条预检分支，
    # 因此命令表要并上它才与帮助、派发同口径。
    declared = set(table) | ({"help"} if CLI_HELP_PRECHECK.search(cli_source) else set())
    check(
        bool(table) and documented == declared == dispatched,
        "help 印出的入口、clap 命令表与 cli.rs 显式派发分支三者相等",
        f"命令表 {len(declared)} 项；只在帮助里 {sorted(documented - declared) or '无'}；"
        f"clap 有变体但 cli.rs 无分支 {sorted(declared - dispatched) or '无'}；"
        f"只在派发里 {sorted(dispatched - documented) or '无'}",
    )
    source = (ROOT / CLI_RUN_DISPATCH_FILE).read_text(encoding="utf-8")
    advertised = set(HELP_RUN_LINE.findall(help_body)[0].split("|"))
    const_entries = RUN_ENTRY_CONST.search(source)
    entries = set(re.findall(r'"([a-z][a-z0-9-]*)"', const_entries.group(2))) if const_entries else set()
    declared = int(const_entries.group(1)) if const_entries else -1
    arms = run_unified_arms(source)
    check(
        const_entries is not None
        and declared == len(entries)
        and advertised == entries == arms,
        "run 的帮助入口、RUN_ENTRY_POINTS 常量与 match 分支三者相等",
        f"帮助 {sorted(advertised)}；常量 {sorted(entries)}（声明 {declared} 项）；"
        f"分支 {sorted(arms)}",
    )



CLI_TESTS_DIR = "crates/qx-cli/src/tests"
# 用例条数下限棘轮：Phase 4o 把单文件 `tests_main.rs` 拆成目录模块时是 59 条。
# 拆分让"删几条用例来压行数"变成一条可行路径，因此行数只降不升的同时，用例数只能升。
# V11 Q65 把口径抬到实测总数（与 V10 Q57 同一做法）：地板停在历史值就等于没有防守。
# Q62 补上重放闸门用例后再抬一次（158 → 161）。Q63 给两条写来源的链各补一条用例（161 → 163）。
# Q66 给"产物声明的输入能否重算"补六条（163 → 169，按磁盘 `^#[test]$` 实测总数）。
# Q67 给账户快照的钱字段口径补四条（169 → 173；协议侧那条在 qx-protocol 集成用例里，不计本地板）。
# Q68 把同一条纪律推到持仓行与 CCXT 回报入口：ccxt 侧三条 + 读模型行侧两条（173 → 178，
# 按磁盘 `^#[test]$` 实测总数：src/tests 139 + crates/qx-cli/tests 39）。
# Q71 给"组合收益按钱算而不是两条腿平均"补一条单元 + 一条端到端复算用例。地板本身落后于
# 磁盘：Q69/Q70 落地时新增了三条用例却没有回写（178 → 181 只发生在磁盘上），本轮连同 Q71 两条
# 一起抬到实测总数 183（src/tests 143 + crates/qx-cli/tests 40）。
# R 轮再抬一次：深度链拒绝撮合模型一条（R11）、无键读模型同账户一条（R12）、
# 账户快照稳定 JSON 往返一条（R14，那条在 qx-protocol 侧，不计本地板）（178 → 187）。
# S 轮：api worker 身份两条（S1）+ 运维读模型现读两条（S3）（187 → 191）。
# T 轮抬到实测总数：对账 worker 身份五条（T4/S12）+ 日历指纹跨语言夹具三条（T2/R17）（191 → 199）。
# 两条线合流后按磁盘重测再抬：Q70/Q71 那三条与 T 轮那八条同场，谁也不是历史值（199 → 202，
# src/tests 162 + crates/qx-cli/tests 40）。
# Q72 给"回测压在多少钱上"补三条读法单元 + 五条命令行用例（183 → 191，按磁盘 `^#[test]$`
# 实测总数：src/tests 146 + crates/qx-cli/tests 45）。其中 3 条在 `#[cfg(feature = "nats")]`
# 后面，默认 `cargo test -p qx-cli` 跑 188 条 —— 地板按磁盘口径计，换 feature 组合不得少用例。
# 两条线合流后按磁盘重测再抬，谁也不是历史值（202 → 210：src/tests 165 + crates/qx-cli/tests 45）。
CLI_TEST_FLOOR = 210
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
# V11 Q57 把该目录连同集成测试的用例总数（25）写回下限。注释此前声称"删一条就红"，
# 但 14 这个数早已落后实际条目数，下限只是"粗粒度地板"：抬到实测总数才真能挡住静默删除。
EXECUTION_TEST_FLOOR = 25
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
        # P2a 把跨 crate 的用例搬进了集成测试目录（`crates/<crate>/tests/`）。它们同样是该
        # crate 的行为用例，必须计入下限口径，否则"换个目录"就能凭空让用例数下降。
        counts.update(
            {
                f"tests/{path.name}": len(
                    re.findall(r"^#\[test\]$", path.read_text(encoding="utf-8"), re.MULTILINE)
                )
                for path in sorted((CRATES / crate / "tests").glob("*.rs"))
            }
        )
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
    # V11 Q54 起 `init` 一族与产品规格读法也按同一形状拆出（纯搬家，语义不变）。
    "init_project",
    "market_spec",
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
    "run_init_with_profile": (
        "init_project.rs",
        r"^(?:pub(?:\(crate\))? )?fn run_init_with_profile\(",
    ),
    "run_strategy_init": (
        "init_project.rs",
        r"^(?:pub(?:\(crate\))? )?fn run_strategy_init\(",
    ),
    "repository_deploy_path": (
        "init_project.rs",
        r"^(?:pub(?:\(crate\))? )?fn repository_deploy_path\(",
    ),
    "market_spec_from_value": (
        "market_spec.rs",
        r"^(?:pub(?:\(crate\))? )?fn market_spec_from_value\(",
    ),
    "ccxt_market_to_spec": (
        "market_spec.rs",
        r"^(?:pub(?:\(crate\))? )?fn ccxt_market_to_spec\(",
    ),
    "ccxt_margin_rule_from_market": (
        "market_spec.rs",
        r"^(?:pub(?:\(crate\))? )?fn ccxt_margin_rule_from_market\(",
    ),
}
ROOT_ITEM = re.compile(
    r"^(?:pub(?:\([^)]*\))? )?(?:async )?(?:unsafe )?(?:extern )?"
    r"(?:fn|struct|enum|trait|impl|type|const|static|union)\b"
)


def mount_pair_present(text: str, name: str) -> bool:
    """模块是否在挂载文件里 `mod` + `pub(crate) use x::*` 成对挂载（按整行判定）。

    早先这里是子串包含判定，于是 `// pub(crate) use x::*;` 一行注释也算挂载通过 ——
    拆出去的出口被注释掉、实现改回挂载点，门禁照样绿（V11 Q61 门禁轮变异实测）。
    """
    return bool(
        re.search(rf"^\s*(?:pub(?:\([^)]*\))?\s+)?mod {name}\s*;$", text, re.MULTILINE)
        and re.search(rf"^\s*pub\(crate\) use {name}::\*;$", text, re.MULTILINE)
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
        name for name in CLI_P4P_MODULES if not mount_pair_present(root, name)
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
    "account_base",
    "artifacts",
    "ashare_binding",
    "config_declarations",
    "depth",
    "fast_backtest",
    "fill_model",
    "kernels",
    "leg_funding",
    "multi_builtin",
    "risk_binding",
    "signal_binding",
    "single_strategy",
    "strategy_backtest",
)
# mod.rs 只允许留共享装配（Phase 4s 拆完是 7 个顶层条目）；实现写回即越界。
CLI_BACKTESTS_MOUNT_CEILING = 8
BACKTEST_ENTRY_OWNERS = {
    "run_multi_builtin_backtest": "multi_builtin.rs",
    # V11 Q58 把 CCXT 入口搬到自己委派的那条链旁边：它取完 OHLCV 就调
    # single_strategy.rs 的 run_builtin_backtest，与双腿归因链没有共用装配。
    "run_ccxt_builtin_backtest": "single_strategy.rs",
    "run_strategy_backtest": "strategy_backtest.rs",
    "persist_backtest_artifacts": "artifacts.rs",
    "run_fast_backtest_manifest": "fast_backtest.rs",
    "run_single_strategy_backtest": "single_strategy.rs",
    "run_builtin_backtest": "single_strategy.rs",
    "run_depth_backtest": "depth.rs",
    # V11 Q61：A 股段的读法只有一份，三条链对它的处理方式（绑定 / 拒绝）都由下面这三个
    # 入口承担；复制一份加载逻辑就等于回到"两条链各自挑口径"。
    "ashare_backtest_binding": "ashare_binding.rs",
    "configured_ashare_binding": "ashare_binding.rs",
    "reject_ashare_rules_config": "ashare_binding.rs",
    # V11 Q66：输入身份的读点与写点各只有一处。声明与复核若各自解一遍 JSON，
    # "跑的是哪份数据"就会有两个都能自圆其说的答案。
    "read_bar_frame_for_backtest": "artifacts.rs",
    "read_depth_frame_for_backtest": "artifacts.rs",
    "barframe_dataset_identity": "artifacts.rs",
    "barframe_input_provenance": "artifacts.rs",
    "depth_frame_input_provenance": "artifacts.rs",
    "recompute_declared_backtest_input": "artifacts.rs",
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
        name for name in CLI_BACKTESTS_MODULES if not mount_pair_present(mount, name)
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
    # 反向：mod.rs 挂载的主题模块必须全在登记表里，否则新模块会绕过逐文件的行数检查。
    mounted = set(re.findall(r"^mod (\w+);$", mount, re.MULTILINE))
    check(
        mounted == set(CLI_BACKTESTS_MODULES),
        "backtests/mod.rs 挂载的主题模块与登记表一致",
        f"登记 {sorted(CLI_BACKTESTS_MODULES)} / 实际 {sorted(mounted)}",
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
    # V10 §4.5：静默宽松的风控门必须不可达。四类"空门"构造（裸 RiskGate/RuleSet 的
    # new 与 default）都等于 fail-open，非测试生产代码里出现即违规（白名单当前为空）。
    bypass = (
        "RiskGate::new()",
        "RuleSet::new()",
        "RiskGate::default()",
        "RuleSet::default()",
    )
    seen: dict[str, list[int]] = {}
    unseeded: list[str] = []
    for path in rust_sources():
        rel = path.relative_to(ROOT).as_posix()
        lines = path.read_text(encoding="utf-8").splitlines()
        for index, line in enumerate(lines):
            if not any(token in line for token in bypass):
                continue
            if "/tests/" in rel or rel.endswith("_tests.rs") or is_test_scoped(index, lines):
                continue
            seen.setdefault(rel, []).append(index + 1)
            # 空风控门 = fail-open：构造后 5 行内必须 add 规则，或显式接收外部 RuleSet。
            if "RiskGate::new()" in line:
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
# V10 §6.1 把跨腿屏障的**判定与执行**都下沉到 qx-execution 网关，CLI 侧那份实现已删除。
# 于是"CLI 文件里有没有出现 `spread_group_barrier(` 调用"不再是可用的覆盖面口径：
# 生产提交入口不再调它，改口径后如果仍按标识符发现，任何新提交点漏接组存储都不会变红。
# 现在的口径有三条：屏障定义点唯一在 qx-execution、qx-cli 不得有第二份实现、
# 每个提交入口（声明与调用）都必须显式回答 `spread_store` 参数。
SPREAD_GATEWAY_BARRIER_DEF = re.compile(r"^pub fn spread_group_barrier\(", re.MULTILINE)
CLI_BARRIER_IMPL = re.compile(r"\bfn\s+spread_group_barrier\b")
# `blocks_new_leg_submission` 是屏障的唯一安全谓词：它在 CLI 里重新出现，就等于把判定
# 搬回了网关之外（V10 §4.10 的原状）。
CLI_BARRIER_JUDGEMENT_PRIMITIVES = (re.compile(r"blocks_new_leg_submission\s*\("),)
# 生产提交入口：函数名后紧跟实参/形参左括号。名字前的 `.` 与标识符字符被排除，
# 所以 `run_binance_submit_order(` / `venue.submit_order(` 不会被误认成提交入口。
LEG_SUBMIT_ENTRY = re.compile(
    r"(?<![A-Za-z0-9_.])(?P<name>execute_paper_submit_effect"
    r"|submit_order_via_gateway_with_risk|submit_order_via_gateway"
    r"|submit_order_with_risk|submit_order"
    r"|execute_submit_order_with_worker_risk|execute_submit_order_with_risk"
    r"|execute_binance_submit_effect)\s*(?P<paren>\()"
)
# 显式组存储表达式：形参写 `spread_store: Option<&dyn SpreadOrderGroupStore>`，
# 实参写 `Some(&spread_store)` / `Some(&group_store)`；裸 `None` 在生产路径不算回答。
SPREAD_STORE_ARG = re.compile(r"(?:\bspread_store\b|\bgroup_store\b|FileSpreadOrderGroupStore)")


def balanced_args(text: str, open_paren: int) -> str:
    """取自 `open_paren` 处开括号内部的全部文本（字符串字面量里的括号不算）。"""
    depth = 0
    in_string = False
    index = open_paren
    while index < len(text):
        char = text[index]
        if in_string:
            if char == "\\":
                index += 2
                continue
            if char == '"':
                in_string = False
        elif char == '"':
            in_string = True
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 0:
                return text[open_paren + 1 : index]
        index += 1
    return ""


def split_top_level(args: str) -> list[str]:
    """按顶层逗号切分实参/形参列表，嵌套括号与字符串字面量内的逗号不参与切分。"""
    parts: list[str] = []
    current: list[str] = []
    depth = 0
    in_string = False
    index = 0
    while index < len(args):
        char = args[index]
        if in_string:
            if char == "\\":
                current.append(args[index : index + 2])
                index += 2
                continue
            in_string = char != '"'
            current.append(char)
        elif char == '"':
            in_string = True
            current.append(char)
        elif char in "([{":
            depth += 1
            current.append(char)
        elif char in ")]}":
            depth -= 1
            current.append(char)
        elif char == "," and depth == 0:
            parts.append("".join(current))
            current = []
        else:
            current.append(char)
        index += 1
    parts.append("".join(current))
    return [part.strip() for part in parts if part.strip()]



def execution_single_track_check() -> None:
    """订单副作用只允许一套实现（Phase 3 的"执行单轨化"）。

    遗留的 `ExecutionService`/`SpreadExecutionService` 与只在单元测试中构造的
    `MultiVenueSpreadExecutionService`+`VenueRouterMap` 均已删除：单腿与多腿共用
    同一个 `ExecutionGateway`（= `PortExecutionService`），多腿安全性由策略 worker
    的组快照、EventLog 归约、`HedgeRecoveryWorker` 补偿，以及**网关内部**在写入任何
    事实之前调用的 `spread_group_barrier` 共同保证（V10 §6.1 把该屏障从 CLI 下沉到此）。
    这里钉住"不得再出现第二套实现/第二编排入口"，而不是给它们留白名单。正则用前后
    否定环视排除 `PortExecutionService` 等更长标识符。
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
    # (a) 屏障判定的定义点唯一，且在 qx-execution 网关侧。
    barrier_defs = [
        path.relative_to(ROOT).as_posix()
        for path in sorted(CRATES.glob("*/**/*.rs"))
        if SPREAD_GATEWAY_BARRIER_DEF.search(path.read_text(encoding="utf-8"))
    ]
    check(
        barrier_defs == [EXEC_CORE_FILE],
        "多腿屏障判定只在 qx-execution 定义一处（下沉网关后不得再有第二处）",
        f"定义于 {barrier_defs}",
    )
    # (b) CLI 侧不得再有任何 `spread_group_barrier` 实现：V10 §4.10 的原始缺陷就是屏障住在
    #     CLI、网关被绕过时约束消失；P1b 下沉后 CLI 连"薄壳预检"也不再保留（提交路径由网关在
    #     写入任何事实之前自己执行屏障，留一个可调用的壳就等于留一条"先查后提"的竞态口径）。
    #     判定原语 `blocks_new_leg_submission` 同时禁止出现在 CLI——那是屏障唯一的裁决谓词，
    #     只允许住在 qx-zhenlu（定义）与 qx-execution（调用它做判定）两侧。
    cli_clones: list[str] = []
    for path in sorted(CRATES.glob("qx-cli/src/**/*.rs")):
        source = path.read_text(encoding="utf-8")
        location = path.relative_to(ROOT).as_posix()
        if CLI_BARRIER_IMPL.search(source):
            cli_clones.append(f"{location} 重新定义了 spread_group_barrier（判定必须只在网关）")
        for primitive in CLI_BARRIER_JUDGEMENT_PRIMITIVES:
            if primitive.search(source):
                cli_clones.append(f"{location} 重新内联了判定原语 {primitive.pattern}")
    check(
        not cli_clones,
        "qx-cli 不得定义或内联多腿屏障（判定只在 qx-execution 网关）",
        f"违规 {cli_clones or '无'}",
    )
    # (c) 每个提交入口都必须显式回答组存储：函数**声明**的最后一个形参、以及函数**调用**
    #     的最后一个实参都必须是组存储表达式。新增一条忘记接组存储的提交链——无论是自己
    #     写个新入口还是调既有入口——都会落进这张清单。生产路径给裸 `None` 单独记一条，
    #     因为那等价于把屏障降级成可跳过。
    missing_store: list[str] = []
    fail_open_none: list[str] = []
    for path in sorted(CRATES.glob("qx-cli/src/**/*.rs")):
        rel = path.relative_to(CRATES / "qx-cli/src")
        # 用例文件里的提交调用是"被测现场"而不是生产入口。Phase 4o 起用例集中在
        # `src/tests/` 目录模块，该目录由 main.rs 的 `#[cfg(test)] mod tests;` 挂载，
        # 生产路径不可能落在里面（同一门禁另有专项断言钉住这个挂载写法）。
        if rel.parts[0] == "tests" or "test" in path.stem:
            continue
        source = path.read_text(encoding="utf-8")
        location = path.relative_to(ROOT).as_posix()
        for match in LEG_SUBMIT_ENTRY.finditer(source):
            args = balanced_args(source, match.start("paren"))
            elements = split_top_level(args)
            label = f"{location}:{source[: match.start()].count(chr(10)) + 1}"
            if not elements:
                # `foo()` 空参不是提交入口的形状（ trait 方法声明等），跳过。
                continue
            last = elements[-1]
            if last == "None":
                # 裸 `None` 是"回答了但选择放行"：生产路径带 `spread_group_id` 的腿
                # 会因此绕过屏障，与 V10 §6.1 第 1 条纪律相反，单列一类。
                fail_open_none.append(f"{label} {match.group('name')}")
            elif not SPREAD_STORE_ARG.search(last):
                # 形参列表的 `Option<&dyn SpreadOrderGroupStore>` 与实参的
                # `Some(&spread_store)` 都能命中；漏掉整个参数则落在这里。
                missing_store.append(f"{label} {match.group('name')} -> {last[:60]}")
    check(
        not missing_store,
        "qx-cli 每个腿提交入口都显式给出 spread_store 形参/实参",
        f"未回答组存储 {missing_store or '无'}",
    )
    check(
        not fail_open_none,
        "qx-cli 生产提交路径不得以裸 None 交出组存储（那等于跳过屏障）",
        f"裸 None {fail_open_none or '无'}",
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


# V11 Q60：涨跌停的锚。日线数据上"上一根 Bar"与"上一交易日"重合，缺陷因此不可见；
# 分钟线一旦用它当锚，±10% 的板就窄化成"最近几分钟 ±10%"，封死的涨停线照样成交。
ASHARE_TRADING_FILE = "crates/qx-xingban/src/ashare/trading.rs"
ASHARE_LIMIT_TEST_FILE = "crates/qx-xingban/tests/ashare_limit_anchor.rs"
ASHARE_RULE_TEST_FILE = "crates/qx-xingban/src/ashare/tests.rs"


def ashare_limit_anchor_check() -> None:
    """A 股涨跌停锚只有一个取法，且它锚的是**上一交易日收价**。

    钉四件事：取锚点单一（引擎侧不得另抄一份"上一根 Bar"）；覆盖表先于推导（除权除息日
    的昨收只能由数据侧给）；跨日扫描取的是上一时段的**最后一根**（`.rev()`）；以及
    "同一根封板线在两种锚下结果不同"的行为用例在位 —— 缺了最后这条，前三条都只是文本。
    """
    trading = (ROOT / ASHARE_TRADING_FILE).read_text(encoding="utf-8")
    start = trading.find("pub fn previous_close(")
    anchor_body = trading[start : trading.find("pub fn limits(", start)]
    engine = (CRATES / "qx-xingban/src/backtest.rs").read_text(encoding="utf-8")
    definitions = len(re.findall(r"(?m)^    pub fn previous_close\(", trading))
    calls = engine.count("rules.previous_close(")
    check(
        start > 0 and definitions == 1 and calls == 1,
        "涨跌停的昨收锚只有一个定义点与一个引擎调用点",
        f"定义 {definitions} 处、引擎调用 {calls} 处（各期望 1）",
    )
    check(
        "previous_close_raw.get(&current.ts)" in anchor_body
        and anchor_body.index("previous_close_raw.get(&current.ts)")
        < anchor_body.index("bars[..index]")
        and ".rev()" in anchor_body
        and "Self::day_key(bar.ts) != day" in anchor_body,
        "昨收锚按上一交易日推导（覆盖表优先、跨日后取该日最后一根）",
        "previous_close 重新退回'上一根 Bar 的收价'或丢掉了覆盖表",
    )
    rules_text = (ROOT / ASHARE_RULES_FILE).read_text(encoding="utf-8")
    doc = trading[:start]
    check(
        all(needle in doc for needle in ("不复权", "上一交易日"))
        and "除权除息" in rules_text,
        "锚的口径与'覆盖表才是除权除息出口'写进代码文档",
        f"{ASHARE_TRADING_FILE} 或 {ASHARE_RULES_FILE} 的昨收说明被删",
    )
    limit_test = (ROOT / ASHARE_LIMIT_TEST_FILE).read_text(encoding="utf-8")
    unit_test = (ROOT / ASHARE_RULE_TEST_FILE).read_text(encoding="utf-8")
    check(
        all(
            case in text
            for text, cases in (
                (
                    limit_test,
                    (
                        "fn a_sealed_intraday_limit_up_blocks_the_fill_when_anchored_to_the_session_close",
                        "fn anchoring_to_the_previous_bar_would_fill_on_that_same_board",
                        "report.fills.is_empty()",
                    ),
                ),
                (
                    unit_test,
                    ("fn limit_band_anchors_to_the_previous_session_close_not_the_previous_bar",),
                ),
            )
            for case in cases
        ),
        "同一根封板线在两种锚下结果不同，且有单元测试钉住锚本身",
        f"{ASHARE_LIMIT_TEST_FILE} 或 {ASHARE_RULE_TEST_FILE} 不再咬住 Q60 口径",
    )


# V11 Q61：`--config` 的 A 股段在回测侧的读法，与钉住它的命令行行为用例。
ASHARE_BINDING_FILE = "crates/qx-cli/src/backtests/ashare_binding.rs"
ASHARE_BINDING_TEST_FILE = "crates/qx-cli/tests/ashare_builtin_backtest.rs"


def ashare_backtest_binding_check() -> None:
    """A 股段（规则快照 + 自带费率）在四条回测链上的处理方式只有一处定义。

    钉四件事：JSON 加载与 `enabled` 判定只在 `ashare_binding.rs` 出现一次；两条 Bar 链各自
    调用绑定入口（少一处就是"给了 --config 却不生效"）；深度档与多腿链各自调用拒绝入口且不
    绑定（少一处就是收下配置再静默丢掉）；命令行行为用例逐条对着 stdout 断言结果真的变了。
    """
    backtests = CRATES / "qx-cli/src/backtests"
    loader = (ROOT / ASHARE_BINDING_FILE).read_text(encoding="utf-8")
    loads = {
        path.relative_to(ROOT).as_posix(): count
        for path in sorted(CRATES.glob("*/src/**/*.rs"))
        if not ({"tests"} <= set(path.parts) or "test" in path.stem)
        and (
            count := len(
                re.findall(
                    r"AshareRuleConfig\s*=\s*serde_json::from_str",
                    non_test_source(path.read_text(encoding="utf-8")),
                )
            )
        )
    }
    check(
        loads == {ASHARE_BINDING_FILE: 1},
        "A 股规则快照的 JSON 读法全仓只有一处（新增读法即第二份口径）",
        f"解析点 {loads or '无'}",
    )
    check(
        loader.count("if !rules.enabled") == 1 and "ashare_rules_path 后 enabled 必须为 true" in loader,
        "配了快照就必须启用，这条判定也只有一处",
        "enabled 判定被删或多出第二份",
    )
    strategy_chain = (backtests / "single_strategy.rs").read_text(encoding="utf-8")
    check(
        strategy_chain.count("ashare_backtest_binding(") == 1
        and strategy_chain.count("configured_ashare_binding(") == 1
        # 两条 Bar 链都在同一份文件里：策略链读已解析的三元组，内置链只拿得到 `--config` 路径，
        # 所以各自调用一个绑定入口。少任一处，那条链就重新变成"给了配置却不生效"。
        and strategy_chain.count("virtual_trading.ashare_rules = Some(") == 2
        # 费率模型必须跟着规则一起换：只装规则不换费用，等于 A 股的佣金/印花税/过户费
        # 被 maker/taker 兜底冒充，而 stdout 上看不出任何异常。
        and strategy_chain.count("binding.fee") == 2
        and strategy_chain.count("assembly.fee = ") == 2
        and "t_plus_one=" in strategy_chain,
        "两条 Bar 回测链各自绑定 A 股规则与费率，并把它印进 stdout",
        f"绑定调用 {strategy_chain.count('ashare_backtest_binding(')}/"
        f"{strategy_chain.count('configured_ashare_binding(')} 处（各期望 1）、"
        f"装配件 {strategy_chain.count('virtual_trading.ashare_rules = Some(')} 处（期望 2）、"
        f"费率件 {strategy_chain.count('binding.fee')}/"
        f"{strategy_chain.count('assembly.fee = ')} 处（各期望 2）",
    )
    for name, label in (("depth.rs", "backtest book"), ("multi_builtin.rs", "backtest multi-builtin")):
        text = (backtests / name).read_text(encoding="utf-8")
        check(
            text.count("reject_ashare_rules_config(") == 1
            and "ashare_rules = Some(" not in text
            and f'"{label}"' in text,
            f"{name} 承不了 A 股段，必须当场拒绝 {label}",
            f"拒绝调用 {text.count('reject_ashare_rules_config(')} 处（期望 1）",
        )
    cases = (ROOT / ASHARE_BINDING_TEST_FILE).read_text(encoding="utf-8")
    check(
        all(
            needle in cases
            for needle in (
                "fn builtin_entry_binds_the_declared_ashare_rules(",
                "fn ashare_fee_model_replaces_the_builtin_cost_rates(",
                "fn broken_ashare_sections_fail_closed_on_the_builtin_entry(",
                "fn entries_without_ashare_hooks_reject_the_config(",
                "fn strategy_chain_rejects_the_same_broken_pairing(",
                '"整手"',
                "source=ashare-rules:",
            )
        ),
        "命令行子进程用例逐条咬住 Q61 口径（结果变化 + 两处拒绝）",
        f"{ASHARE_BINDING_TEST_FILE} 不再覆盖 Q61",
    )


# V11 Q65：A 股段在 Paper/Live 提交侧的唯一闸门。回测链能执行制度，提交链不能 ——
# 所以这里的正确形状是"配了就拒"，而拒绝必须早于任何副作用。
ASHARE_SUBMIT_GUARD_FILE = "crates/qx-cli/src/runtime_wiring.rs"
ASHARE_SUBMIT_GUARD_TEST_FILE = "crates/qx-cli/src/tests/ashare_submit_guard.rs"
# 会提交订单的入口：两个 worker 入口 + 一个 Paper worker + 两个一次性提交入口。
ASHARE_SUBMIT_CALL_SITES = {
    "crates/qx-cli/src/worker_entry.rs": ("binance-worker", "ccxt-worker"),
    "crates/qx-cli/src/venue_runtime/paper_worker.rs": ("paper-worker",),
    "crates/qx-cli/src/venue_runtime/paper_submit.rs": ("paper-submit-order",),
    "crates/qx-cli/src/venue_runtime/binance_submit.rs": ("binance-submit-order",),
}


def ashare_submit_guard_check() -> None:
    """钉四件事：闸门只有一处定义、五个提交入口各问一次、问的顺序早于副作用、
    不提交订单的角色不受影响（否则"研究用配置"连启动都做不到）。
    """
    wiring = (ROOT / ASHARE_SUBMIT_GUARD_FILE).read_text(encoding="utf-8")
    check(
        wiring.count("pub(crate) fn reject_ashare_rules_on_submit_path(") == 1
        and wiring.count("fn worker_submits_orders(") == 1
        and "WorkerRole::Execution | WorkerRole::SpreadRecovery" in wiring,
        "提交侧的 A 股闸门只定义一处，且只管会提交新订单的角色",
        "定义或角色判定不再唯一",
    )
    check(
        wiring.count("reject_ashare_rules_config(") == 1
        and "T+1" in wiring
        and "strategy backtest" in (ROOT / ASHARE_BINDING_FILE).read_text(encoding="utf-8"),
        "提交侧复用回测侧那一份拒绝文案，并点名缺的能力是 T+1 结算状态",
        "文案出现第二处定义，或没有说明为什么接不上",
    )
    for path, labels in ASHARE_SUBMIT_CALL_SITES.items():
        text = (ROOT / path).read_text(encoding="utf-8")
        check(
            text.count("reject_ashare_rules_on_submit_path(") == len(labels)
            and all(f'"{label}"' in text for label in labels),
            f"{path.split('/')[-1]} 每个提交入口都问过 A 股闸门（{'、'.join(labels)}）",
            f"调用 {text.count('reject_ashare_rules_on_submit_path(')} 处（期望 {len(labels)}）",
        )
    venue = "".join(
        (CRATES / "qx-cli/src/venue_runtime" / name).read_text(encoding="utf-8")
        for name in sorted(
            path.name for path in (CRATES / "qx-cli/src/venue_runtime").glob("*.rs")
        )
    )
    check(
        "ashare_backtest_binding(" not in venue
        and "AshareRuleConfig" not in venue
        and "AShareFeeModel" not in venue,
        "提交侧不得自己装配 A 股规则或费率（半接半丢比不接更危险）",
        "venue_runtime 里出现了 A 股装配点",
    )
    cases = (ROOT / ASHARE_SUBMIT_GUARD_TEST_FILE).read_text(encoding="utf-8")
    check(
        all(
            f"fn {name}(" in cases
            for name in (
                "paper_submit_entry_refuses_the_ashare_section_before_touching_anything",
                "paper_worker_entry_refuses_to_start_an_execution_worker_with_ashare_rules",
                "binance_submit_entry_refuses_the_ashare_section_before_reading_the_command",
                "ccxt_worker_entry_refuses_the_ashare_section_before_touching_the_ccxt_config",
                "only_roles_that_submit_orders_meet_the_ashare_gate",
            )
        )
        and "WorkerRole::Strategy" in cases
        and "WorkerRole::MarketData" in cases,
        "进程内用例逐条咬住 Q65 口径（五个入口的拒绝 + 不提交角色放行 + 副作用顺序）",
        f"{ASHARE_SUBMIT_GUARD_TEST_FILE} 不再覆盖 Q65",
    )
    help_text = (ROOT / CLI_HELP_FILE).read_text(encoding="utf-8")
    check(
        help_text.count("strategy.ashare_rules_path") >= 3
        and "  strategy-worker <runtime.json> <worker-id> [--once]" in help_text,
        "帮助文本写明提交类入口会当场拒绝 A 股段，而策略 worker 不受影响",
        "cli_help.rs 不再描述 Q65 的口径",
    )


# V11 Q64：`strategy.builtin_*` 四个信号参数在四条 Bar 回测链上的读点、生效与可见性。
BUILTIN_SIGNAL_READER_FILE = "crates/qx-cli/src/strategy_binding.rs"
BUILTIN_SIGNAL_STRATEGY_CHAIN_FILE = "crates/qx-cli/src/strategy_host.rs"
BUILTIN_SIGNAL_TEST_FILE = "crates/qx-cli/tests/builtin_signal_from_config.rs"
# 交易链路（paper/live）不经过回测链，用进程内用例钉同一个读点。
BUILTIN_SIGNAL_WORKER_TEST_FILE = "crates/qx-cli/src/tests/strategy_worker_entries.rs"
# V11 Q72 把这条包装连同 `builtin_signal_note` 从 `single_strategy.rs` 搬进了独立模块
# （那文件当时 511 行，撞上 500 行门槛）。读点调用仍留在三条链自己手里，所以只有这一处
# 路径按"定义在哪"取，链侧检查按"谁调用"取。
BUILTIN_SIGNAL_WRAPPER_FILE = "crates/qx-cli/src/backtests/signal_binding.rs"
# 每条内置链都要问一次配置；少了这一处就退回到写死默认（Q64 的原缺陷形状）。
BUILTIN_SIGNAL_CHAINS = {
    "single_strategy.rs": "backtest builtin",
    "depth.rs": "backtest book",
    "multi_builtin.rs": "backtest multi-builtin",
}


def builtin_signal_check() -> None:
    """信号参数只有一个赋值点，四条链各自问过它，并且都把生效口径印进 stdout。

    钉五件事：四项覆盖的赋值语句全仓只出现一次；策略链与三条内置链各有一处读点调用；
    覆盖之后必须重做参数体检（`BuiltinStrategyConfig::new` 只看得到写死默认）；四条链各印
    一行 `[X · Signal]` 且判词由同一个函数生成；帮助文本点名这四个键；行为用例逐条咬住"换参数
    就是换结果"。少了任何一项，`--config` 就又变成一句假话。
    """
    backtests = CRATES / "qx-cli/src/backtests"
    reader = (ROOT / BUILTIN_SIGNAL_READER_FILE).read_text(encoding="utf-8")
    strategy_chain = (ROOT / BUILTIN_SIGNAL_STRATEGY_CHAIN_FILE).read_text(encoding="utf-8")
    assigns = {
        path.relative_to(ROOT).as_posix(): count
        for path in sorted(CRATES.glob("*/src/**/*.rs"))
        if not ({"tests"} <= set(path.parts) or "test" in path.stem)
        and (
            count := len(
                re.findall(
                    r"config\.(?:fast_window|slow_window|period|threshold_bps) = ",
                    non_test_source(path.read_text(encoding="utf-8")),
                )
            )
        )
    }
    check(
        assigns == {BUILTIN_SIGNAL_READER_FILE: 4},
        "strategy.builtin_* 四项的覆盖赋值全仓只有一处（新增赋值点即第二份口径）",
        f"赋值点 {assigns or '无'}",
    )
    check(
        reader.count("pub(crate) fn apply_builtin_signal_overrides(") == 1
        and strategy_chain.count("apply_builtin_signal_overrides(&mut config, strategy);") == 1,
        "读点定义一处，且 `strategy backtest`/paper 那条链确实问过它",
        f"定义 {reader.count('pub(crate) fn apply_builtin_signal_overrides(')} 处、"
        f"策略链调用 {strategy_chain.count('apply_builtin_signal_overrides(&mut config, strategy);')} 处（各期望 1）",
    )
    wrapper = (ROOT / BUILTIN_SIGNAL_WRAPPER_FILE).read_text(encoding="utf-8")
    wrapper_definitions = {
        path.name
        for path in sorted((CRATES / "qx-cli/src/backtests").glob("*.rs"))
        if "pub(crate) fn apply_configured_builtin_signal(" in path.read_text(encoding="utf-8")
    }
    check(
        wrapper.count("pub(crate) fn apply_configured_builtin_signal(") == 1
        and wrapper_definitions == {Path(BUILTIN_SIGNAL_WRAPPER_FILE).name},
        "内置链侧的包装定义一处，三条链各问过它一次（调用点检查在下面的逐链表里）",
        f"定义 {wrapper.count('pub(crate) fn apply_configured_builtin_signal(')} 处 / 定义文件 {sorted(wrapper_definitions)}",
    )
    for name, label in BUILTIN_SIGNAL_CHAINS.items():
        text = (backtests / name).read_text(encoding="utf-8")
        check(
            text.count("= apply_configured_builtin_signal(") == 1,
            f"{name} 必须读 `--config` 里的信号参数（{label}）",
        "读点调用不再是 1 处，这条链会退回写死默认",
        )
    check(
        strategy_chain.count("let mut config = BuiltinStrategyConfig {") == 1,
        "策略链的内置配置只能由那一个构造点装配（否则第二处会绕过覆盖）",
        f"构造点 {strategy_chain.count('let mut config = BuiltinStrategyConfig {')} 处（期望 1）",
    )
    check(
        re.search(
            r"fn apply_configured_builtin_signal\([^)]*\)[^{]*\{.*?"
            r"apply_builtin_signal_overrides\(config, &strategy\);\s*\n\s*config\.validate\(\)(\?;|\.map_err\()",
            wrapper,
            re.S,
        )
        is not None,
        "覆盖之后必须重做参数体检，非法信号组合整轮失败",
        "包装里没有覆盖后紧跟 validate 的形状",
    )
    check(
        re.search(
            r"fn builtin_strategy_config_from_runtime\([^)]*\)[^{]*\{.*?config\.validate\(\)\?;\s*\n\s*Ok\(config\)",
            strategy_chain,
            re.S,
        )
        is not None,
        "策略链那条链也要在覆盖之后复检，非法组合不许走到印口径",
        "策略链的装配尾部没有 validate → Ok(config) 的形状",
    )
    printed = {
        name: sum(text.count(marker) for marker in markers)
        for name, markers in (
            ("single_strategy.rs", ("[Builtin · Signal]", "[Strategy · Signal]")),
            ("depth.rs", ("[Depth · Signal]",)),
            ("multi_builtin.rs", ("[Multi · Signal]",)),
        )
        for text in [(backtests / name).read_text(encoding="utf-8")]
    }
    note_calls = sum(
        (backtests / name).read_text(encoding="utf-8").count("builtin_signal_note(&")
        for name in BUILTIN_SIGNAL_CHAINS
    )
    check(
        printed == {"single_strategy.rs": 2, "depth.rs": 1, "multi_builtin.rs": 1}
        and note_calls == 4,
        "四条 Bar 链各印一行生效口径，且判词由共用的那句生成（builtin/strategy/book/multi）",
        f"Signal 行 {printed}、共用判词调用 {note_calls} 处（期望 2/1/1 与 4）",
    )
    note_definitions = sum(
        len(
            re.findall(
                r"fn builtin_signal_note\(",
                non_test_source(path.read_text(encoding="utf-8")),
            )
        )
        for path in sorted(CRATES.glob("*/src/**/*.rs"))
        if not ({"tests"} <= set(path.parts) or "test" in path.stem)
    )
    check(
        note_definitions == 1,
        "生效口径的措辞全仓只定义一次（各链自己拼字符串就会各说一套）",
        f"定义 {note_definitions} 处（期望 1）",
    )
    help_text = (ROOT / CLI_HELP_FILE).read_text(encoding="utf-8")
    check(
        all(
            key in help_text
            for key in (
                "builtin_fast_window",
                "builtin_slow_window",
                "builtin_period",
                "builtin_threshold_bps",
            )
        )
        and "写了就必须生效" in help_text,
        "帮助文本点名四个信号键并承诺写了就必须生效",
        "cli_help.rs 不再描述 Q64 的读法",
    )
    check(
        help_text.count("[Depth · Signal]") == 1 and help_text.count("[Multi · Signal]") == 1,
        "深度档与多腿链的帮助都写明信号参数在本链生效",
        "Q64 的两条链在帮助里仍是沉默的",
    )
    cases = (ROOT / BUILTIN_SIGNAL_TEST_FILE).read_text(encoding="utf-8")
    check(
        all(
            f"fn {name}(" in cases
            for name in (
                "declared_windows_change_the_builtin_result",
                "declared_period_and_threshold_each_move_the_result",
                "illegal_declared_parameters_fail_closed",
                "single_sided_override_is_rechecked_before_the_provenance_line",
                "both_bar_chains_read_the_same_declared_signal",
                "depth_chain_applies_the_declared_signal",
                "multi_leg_chain_applies_the_declared_signal",
            )
        ),
        "命令行子进程用例逐条咬住 Q64 口径（四条链 + 两类非法组合）",
        f"{BUILTIN_SIGNAL_TEST_FILE} 不再覆盖 Q64",
    )
    worker_case = (ROOT / BUILTIN_SIGNAL_WORKER_TEST_FILE).read_text(encoding="utf-8")
    check(
        "fn builtin_worker_reads_the_declared_signal_parameters(" in worker_case
        and "builtin_strategy_config_from_runtime" in worker_case,
        "交易链路（paper/live）那侧也有一条进程内用例钉住同一份声明信号",
        f"{BUILTIN_SIGNAL_WORKER_TEST_FILE} 不再覆盖 Q64",
    )


# V11 Q62：重放必须是"重新驱动一遍事实源"，而不是把同一段事件切片再哈希一次。
REPLAY_KERNEL_FILE = "crates/qx-core/src/sourcing.rs"
REPLAY_ENGINE_FILES = (
    "crates/qx-xingban/src/backtest.rs",
    "crates/qx-xingban/src/orderbook_backtest.rs",
)
REPLAY_ARTIFACT_FILE = "crates/qx-cli/src/backtests/artifacts.rs"
REPLAY_CLI_TEST_FILE = "crates/qx-cli/src/tests/backtest_replay_gate.rs"
# 报告出口必须把重放失败向上抛。写成 `.ok()` 吞掉等于闸门还在、返回值永远为真，
# 光数调用次数看不出来，所以把传播形状本身钉成一条字符串。
ENGINE_REPLAY_GATE_NEEDLE = "ReplayVerifier::verify(report.event_log.events(), &report.ledger)?;"


def function_body(text: str, name: str) -> str:
    """取 `pub fn name(` 到其函数体结束（下一个四空格缩进的 `}`）的文本。

    门禁要钉的是"这一步到底做没做"，把整份文件当字符串数次数会让相邻函数的调用混进来。
    """
    start = text.find(f"pub fn {name}(")
    if start < 0:
        return ""
    rest = text[start:]
    end = rest.find("\n    }")
    return rest if end < 0 else rest[: end + len("\n    }")]


def replay_kernel_check() -> None:
    """重放内核唯一、三条判据齐、恒等口径不复活、两条引擎链与产物落盘点都问过它。

    每一项各有独立取证面：内核定义唯一（改名/插第二份定义会红）、内核体三步齐
    （摘掉 `log.validate()` 会红）、报告侧的条数比较还在（把它写成恒等式会红）、
    `rebuild_from`/`"replay_hash"` 这类被证伪的口径不得复活、两条链的报告出口各问一次
    且以 `?;` 向上抛（换成 `.ok();` 会红）、摘要落盘点问在写文件之前、摘要形状、
    深度链的因果槽位、以及两侧用例的名字。
    """
    kernel = (ROOT / REPLAY_KERNEL_FILE).read_text(encoding="utf-8")
    check(
        kernel.count("pub fn replay(") == 1
        and kernel.count("pub fn replay_facts(") == 1
        and kernel.count("pub fn rebuild_ledger(") == 1
        and kernel.count("pub fn verify(") == 1,
        "重放只有一个内核函数，其余口径都是它的投影",
        "内核函数不再各自唯一",
    )
    body = function_body(kernel, "replay")
    check(
        "log.append_checked(event.clone())" in body
        and "ledger.apply_entry(entry.clone())" in body
        and "log.validate()?" in body,
        "重放内核三条都做：逐条重新接受事实、重新入账、整本再校验",
        f"内核体缺少其中一步：{body[:160]}",
    )
    check(
        "facts.ledger_entries != ledger.entries().len()"
        in function_body(kernel, "verify"),
        "报告侧留有一条真会失败的判据：重放账簿条数必须等于运行账簿条数",
        "verify 不再比较两侧条数（退化成不可能失败的自校验）",
    )
    tautology = sum(
        path.read_text(encoding="utf-8").count("rebuild_from(") for path in rust_sources()
    ) + sum(
        path.read_text(encoding="utf-8").count('"replay_hash":') for path in rust_sources()
    ) + sum(
        path.read_text(encoding="utf-8").count("fn replay_hash(") for path in rust_sources()
    )
    check(
        tautology == 0,
        "被证伪的恒等口径（rebuild_from / replay_hash）不得在任何 crate 复活",
        f"命中 {tautology} 处"
    )
    for path in REPLAY_ENGINE_FILES:
        production = (ROOT / path).read_text(encoding="utf-8").split("#[cfg(test)]")[0]
        check(
            production.count("ReplayVerifier::verify(") == 1
            and ENGINE_REPLAY_GATE_NEEDLE in production,
            f"{path.split('/')[-1]} 的报告出口先过重放校验才返回，失败向上抛而不是就地吞掉",
            f"生产侧调用 {production.count('ReplayVerifier::verify(')} 处（期望 1），"
            f"且需原样出现 {ENGINE_REPLAY_GATE_NEEDLE}",
        )
    artifact = (ROOT / REPLAY_ARTIFACT_FILE).read_text(encoding="utf-8")
    gate_at = artifact.find("ReplayVerifier::verify(")
    write_at = artifact.find("write_backtest_artifact(")
    check(
        artifact.count("ReplayVerifier::verify(") == 1 and 0 <= gate_at < write_at,
        "摘要落盘前问过重放，且问在写任何工件之前",
        f"verify@{gate_at} 首次写文件@{write_at}",
    )
    check(
        '"schema_version": 4' in artifact
        and '"log_digest"' in artifact
        and '"ledger_entries"' in artifact
        and '"run_ledger_entries"' in artifact,
        "摘要把重放做过的三件事写成人各自可核对的字段",
        "replay 结论块缺键或 schema 版本回退",
    )
    depth = (ROOT / REPLAY_ENGINE_FILES[1]).read_text(encoding="utf-8").split("#[cfg(test)]")[0]
    check(
        depth.count("Priority::FEEDBACK") == 2
        and depth.count("Priority::APPLY") == 0
        and depth.count("Priority::TIMER") == 1,
        "深度链把迟到成交回报排在同戳命令之前、初始入金排在观测起点（否则事实流不可重放）",
        f"FEEDBACK={depth.count('Priority::FEEDBACK')} APPLY={depth.count('Priority::APPLY')} "
        f"TIMER={depth.count('Priority::TIMER')}",
    )
    cli_cases = (ROOT / REPLAY_CLI_TEST_FILE).read_text(encoding="utf-8")
    check(
        all(
            f"fn {name}(" in cli_cases
            for name in (
                "summary_publishes_the_replay_facts_it_actually_checked",
                "artifacts_refuse_to_land_when_a_ledger_entry_has_no_fact_event",
                "artifacts_refuse_to_land_when_the_fact_stream_is_not_canonically_ordered",
            )
        ),
        "落盘侧三条用例钉住「该写的写了、两种坏事实源都拒落盘」",
        f"{REPLAY_CLI_TEST_FILE} 不再覆盖 Q62",
    )
    bar_cases = (ROOT / REPLAY_ENGINE_FILES[0]).read_text(encoding="utf-8")
    check(
        bar_cases.count("assert_replay_matches(") == 3
        and "assert_eq!(replayed.entries(), report.ledger.entries());" in bar_cases
        and "ReplayVerifier::verify(report.event_log.events(), &unbacked)" in bar_cases,
        "Bar 链逐条比对重放账簿，并留有一条「凭空多记」的反向证据",
        "重放用例的形状变了（helper 定义/两处调用/反向证据）",
    )


# V11 Q66 / Q1b 第一批：产物声明的输入身份必须能被重算，且只有一处读、一处写。
INPUT_PROV_ARTIFACT_FILE = "crates/qx-cli/src/backtests/artifacts.rs"
INPUT_PROV_SINGLE_FILE = "crates/qx-cli/src/backtests/single_strategy.rs"
INPUT_PROV_REPORT_FILE = "crates/qx-cli/src/config_commands.rs"
INPUT_PROV_TEST_FILE = "crates/qx-cli/src/tests/backtest_input_provenance.rs"
# 这三条链各自解一遍帧就等于"声明的输入"与"重算的输入"各有自己的答案。
INPUT_PROV_CHAIN_FILES = (
    "crates/qx-cli/src/backtests/strategy_backtest.rs",
    "crates/qx-cli/src/backtests/depth.rs",
    INPUT_PROV_REPORT_FILE,
)
INPUT_PROV_CASES = (
    "bar_chain_declares_the_identity_the_registry_already_verified",
    "report_refuses_when_the_declared_input_changed_after_the_run",
    "report_refuses_when_the_declared_input_file_is_gone",
    "tampered_frame_cannot_take_the_registered_identity_of_the_clean_one",
    "depth_chain_declares_and_revalidates_its_own_frame",
    "summary_without_an_input_block_is_not_declared_rather_than_verified",
)
INPUT_PROV_VERSION_CONSTANTS = ("BARFRAME_DATASET_VERSION", "DEPTH_FRAME_DATASET_VERSION")


def input_provenance_check() -> None:
    """回测产物的输入身份：一处读、一处写、报告侧真会失败（V11 Q66 / Q1b）。"""
    artifact = (ROOT / INPUT_PROV_ARTIFACT_FILE).read_text(encoding="utf-8")

    def body_of(name: str) -> str:
        start = artifact.find(f"fn {name}(")
        if start < 0:
            return ""
        end = artifact.find("\n}\n", start)
        return artifact[start:] if end < 0 else artifact[start:end]

    check(
        artifact.count('"schema_version": 4') == 1
        and artifact.count('"input": input_provenance_json(&input.input)') == 1
        and artifact.count("fn input_provenance_json(") == 1,
        "摘要以当前 schema 版本落一个 `input` 块，且这份形状只由 input_provenance_json 写一次",
        "input 块的写法出现多处或 schema/键名回退",
    )
    chain_parses = sum(
        (ROOT / path)
        .read_text(encoding="utf-8")
        .split("#[cfg(test)]")[0]
        .count("Frame::from_json(")
        for path in INPUT_PROV_CHAIN_FILES
    )
    check(
        chain_parses == 0,
        "三条会落产物的链都不自己解帧：入口读与复核读都问 artifacts.rs 的读点",
        f"链上直接反序列化 {chain_parses} 处",
    )
    check(
        "load_bars_with_manifest(" in body_of("barframe_dataset_identity")
        and "Provider 与 BarFrame 列式输入不一致" in body_of("barframe_dataset_identity"),
        "BarFrame → 数据集身份那一步既过 Provider 又逐列比对，声明的指纹就是被复核过的那个",
        "barframe_dataset_identity 不再同时具备 Provider 读取与列式一致性检查",
    )
    recompute = body_of("recompute_declared_backtest_input")
    check(
        "read_bar_frame_for_backtest(" in recompute
        and "barframe_dataset_identity(" in recompute
        and "read_depth_frame_for_backtest(" in recompute
        and '"dataset_id"' in recompute
        and '"dataset_version"' in recompute
        and '"fingerprint"' in recompute
        and "回测产物声明的输入与实况不符" in recompute
        and "未知的回测输入种类" in recompute
        and "回测摘要的 input 块缺" in recompute,
        "复核走同一读点重算，三格逐字段比，坏 kind 与缺字段都判失败",
        f"recompute 体的形状变了：{recompute[:160]}",
    )
    report = (ROOT / INPUT_PROV_REPORT_FILE).read_text(encoding="utf-8").split("#[cfg(test)]")[0]
    check(
        report.count("recompute_declared_backtest_input(&summary)?") == 1
        and "input_verified=not_declared" in report,
        "报告出口把复核失败向上抛，且旧 schema 只能被说成「没声明」而不是「已核对」",
        "run_report 不再以 `?` 传播复核结果，或不再区分未声明",
    )
    single = (ROOT / INPUT_PROV_SINGLE_FILE).read_text(encoding="utf-8").split("#[cfg(test)]")[0]
    self_hash_fallback = sum(
        path.read_text(encoding="utf-8").count('format!("{:016x}", report.input_data_hash)')
        for path in CRATES.rglob("*.rs")
    )
    check(
        'format!("{}:{}", input.kind, input.fingerprint)' in single
        and self_hash_fallback == 0,
        "RunManifest 的 data_fingerprint 回落用被复核过的数据集指纹，引擎自哈希不再冒充输入身份",
        f"回落口径 {self_hash_fallback} 处仍是 report.input_data_hash",
    )
    constants = dict(
        (name, value)
        for name, value in re.findall(
            r"pub\(crate\) const (\w+): &str = \"([^\"]*)\";", artifact
        )
        if name in INPUT_PROV_VERSION_CONSTANTS
    )
    literals = {
        path.relative_to(ROOT).as_posix(): path.read_text(encoding="utf-8").count(
            '"barframe-json-v1"'
        )
        + path.read_text(encoding="utf-8").count('"depth-frame-v1"')
        for path in sorted(CRATES.rglob("*.rs"))
    }
    literals = {path: count for path, count in literals.items() if count}
    check(
        len(constants) == 2 and len(set(constants.values())) == 2,
        "两档输入形状（BarFrame / 深度帧）各有唯一版本号，且没共用同一个标签",
        f"常量取值 {constants}",
    )
    check(
        literals == {INPUT_PROV_ARTIFACT_FILE: 2},
        "版本号字面量只在 artifacts.rs 出现一次，链上不得自带兜底版本",
        f"实际字面量分布 { {k: v for k, v in literals.items() if v} }",
    )
    cases = (ROOT / INPUT_PROV_TEST_FILE).read_text(encoding="utf-8")
    check(
        all(f"fn {name}(" in cases for name in INPUT_PROV_CASES),
        "六条用例钉住「声明等于重算、篡改即拒、缺文件即拒、同一身份不容两种内容、深度链同形、未声明不等于通过」",
        f"{INPUT_PROV_TEST_FILE} 不再覆盖 {INPUT_PROV_CASES}",
    )


# V11 Q1a 第二批：Bar 链撮合口径的单点装配，与钉住它的行为用例。
BAR_FILL_MODEL_FILE = "crates/qx-cli/src/backtests/fill_model.rs"
FILL_MODEL_TEST_FILE = "crates/qx-cli/src/tests/backtest_fill_model.rs"


def backtest_assembly_check() -> None:
    # V11 Q1a 第二批：撮合口径的单点装配与它的行为用例。路径按文件取，因为"装配只有一份"
    # 判的是整个目录的聚合形状，而这两处要判的是具体文件里的字段与用例名。
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
    # V10 P0b 之后，四条回测入口不再各自直接 `strategy_risk_gate(...)`：入口经
    # `backtest_risk_binding(...)` 取配置，只有绑定层与共享装配允许直接构造。因此两条口径
    # 一起数——任何入口改回写死规则都会让总数掉到 4 以下。
    gates = len(re.findall(r"strategy_risk_gate\(", text))
    bound_entries = len(re.findall(r"=\s*backtest_risk_binding\(", text))
    bypass = len(re.findall(r"RiskGate::new\(", text))
    check(
        gates + bound_entries >= 4 and bypass == 0,
        "回测风控门全部经 strategy_risk_gate 构造",
        f"直接构造 {gates} 处 / 配置绑定入口 {bound_entries} 处 / 绕过 {bypass} 处",
    )
    # V11 Q1a 第二批：撮合口径与风控/费用同构——只能在 `fill_model.rs` 构造，装配处
    # 不再写死模型。`into_config` 一旦退回 `Box::new(NextBarOpenFillModel)`，
    # `strategy.fill_model` 就重新变成 Q0c 判过的"宣称能配、无人读取"死配置面。
    fill_single_source = (ROOT / BAR_FILL_MODEL_FILE).read_text(encoding="utf-8")
    elsewhere = text.replace(fill_single_source, "")
    constructions = len(re.findall(r"Box::new\(\w*FillModel", elsewhere))
    check(
        constructions == 0
        and re.search(r"^\s*fill: self\.fill,$", elsewhere, re.MULTILINE) is not None,
        "Bar 撮合模型只在 fill_model.rs 构造，装配字段取自绑定",
        f"其余文件构造 {constructions} 处 / 装配字段未取自 self.fill",
    )
    # 三条 Bar 装配链（策略 / 内置 / 多腿的每一条腿）都要向 `bar_fill_model` 要口径。
    bindings = len(re.findall(r"=\s*bar_fill_model\(", elsewhere))
    check(
        bindings >= 3 and "configured_fill_model(" in elsewhere,
        "Bar 回测入口的撮合口径全部经 bar_fill_model 解析",
        f"解析点 {bindings} 处 / 命令行配置读取 {'有' if 'configured_fill_model(' in elsewhere else '无'}",
    )
    # 配置面三件套：schema 声明、`config validate` 覆盖、行为用例。少了任一件就是
    # "配置写着生效、没人校验"或"改了模型产物看不出来"，与 Q0c 同一判据。
    schema = (ROOT / STRATEGY_SCHEMA_FILE).read_text(encoding="utf-8")
    validation = (ROOT / RUNTIME_CHECK_FILE).read_text(encoding="utf-8")
    check(
        "pub fill_model: Option<String>," in schema,
        "运行时配置声明 strategy.fill_model",
        f"{STRATEGY_SCHEMA_FILE} 缺少该字段",
    )
    check(
        "fill_model_failure(" in validation,
        "config validate 覆盖 fill_model（与装配同一张表）",
        f"{RUNTIME_CHECK_FILE} 未调用 fill_model_failure",
    )
    fill_cases = (ROOT / FILL_MODEL_TEST_FILE).read_text(encoding="utf-8")
    check(
        "fn each_reachable_fill_model_changes_the_result_and_is_declared" in fill_cases,
        "存在「换撮合模型 → 回测结果与产物描述子随之变化」的行为用例（Q1a 第二批证据）",
        f"缺少 {FILL_MODEL_TEST_FILE} 中的模型驱动用例",
    )


# V11 Q67：账户快照的七个汇总钱字段必须能区分"算过"与"没算"（交易链路读模型侧）。
SNAPSHOT_PROTOCOL_FILE = "crates/qx-protocol/src/lib.rs"
SNAPSHOT_READ_FILE = "crates/qx-cli/src/api_service.rs"
SNAPSHOT_ENDPOINT_FILE = "crates/qx-api/src/lib.rs"
SNAPSHOT_CLI_CASE_FILE = "crates/qx-cli/src/tests/api_snapshot_money_fields.rs"
SNAPSHOT_CORE_CASE_FILE = "crates/qx-protocol/tests/snapshot_single_source.rs"
# 没有来源可算、必须停在 `None` 的那五个；`available_raw`/`fees_raw` 各有自己的算点。
SNAPSHOT_UNCOMPUTED_FIELDS = (
    "margin_raw",
    "frozen_raw",
    "realized_pnl_raw",
    "unrealized_pnl_raw",
    "funding_raw",
)
SNAPSHOT_OPTIONAL_FIELDS = ("available_raw",) + SNAPSHOT_UNCOMPUTED_FIELDS + ("fees_raw",)
SNAPSHOT_SCALARS = ("equity_raw",) + SNAPSHOT_OPTIONAL_FIELDS
SNAPSHOT_MONEY_CASES = (
    "available_is_the_settlement_cash_and_not_a_copy_of_equity",
    "published_fees_are_the_sum_of_the_fills_on_the_same_snapshot",
    "uncomputed_money_is_absent_rather_than_zero",
    "overflowing_fee_total_is_refused_instead_of_wrapping",
    "equity_without_a_mark_price_is_absent_rather_than_the_remaining_cash",
)
# V11 R14/R15：四张键表在稳定 JSON 里没有第二份编码，写侧就是 serde 那一份。
SNAPSHOT_SERDE_TABLES = ("orders", "fills", "transfers")
# V11 R18：跨语言夹具——Rust 写侧原样产出、Python 读侧原样吃下，两侧各钉一次。
SNAPSHOT_FIXTURE_FILE = "python/tests/fixtures/account-snapshot-v1.sample.json"
SNAPSHOT_BRIDGE_CASE_FILE = "python/tests/test_bridge.py"
# 夹具里四张键表的键：数据面自证编码口径，改写侧就得连带重产夹具。
SNAPSHOT_FIXTURE_KEYS = (
    '"positions":{"BTCUSDT.BINANCE":',
    '"orders":{"77":',
    '"fills":{"9":',
    '"transfers":{"3":',
)
# 手抄 format! 留下的四条模板片段：它们一旦回来，`from_json` 就又解不回来了。
SNAPSHOT_TABLE_TEMPLATES = (
    'order_id\\":{}',
    'fill_id\\":{}',
    'transfer_id\\":{}',
    '{}:{{\\"quantity_raw\\":{}',
)
# V11 R16：BarFrame JSON 的契约版本与键集合跨语言只有一份口径。
DATASTRUCT_FRAME_FILE = "crates/qx-datastruct/src/lib.rs"
DATA_PROVIDER_FILE = "crates/qx-data/src/provider.rs"
DATA_SCHEMA_FILE = "crates/qx-data/src/schema.rs"
BRIDGE_INIT_FILE = "python/qianxing_bridge/__init__.py"
FRAME_CONTRACT_TEST_FILE = "crates/qx-datastruct/tests/frame_contract.rs"
FRAME_CONTRACT_CASES = (
    "a_written_frame_declares_the_version_its_readers_honour",
    "a_frame_from_a_newer_contract_version_is_refused",
    "a_versionless_document_still_reads_as_legacy",
)


def bar_frame_contract_check() -> None:
    """BarFrame JSON 文档自己声明版本，且声明的口径与两侧读侧是同一份（V11 R16）。

    `qx_datastruct::BarFrame::to_json` 此前不印 `schema_version`，而 `qx-data` 的
    `parse_bar_frame` 与 Python 的 `BarFrame.from_json` 都把"缺省即旧格式"当成走宽松分支的
    信号：Rust 自己写出的帧因此永远进不了它们各自的严格模式（`source` 必填、未知字段拒绝），
    同一份帧的数据集 Manifest 却已经按当前版本记血缘。三处版本号与两份键集合必须同口径。
    """
    writer = (ROOT / DATASTRUCT_FRAME_FILE).read_text(encoding="utf-8").split("#[cfg(test)]")[0]
    provider = (ROOT / DATA_PROVIDER_FILE).read_text(encoding="utf-8")
    schema = (ROOT / DATA_SCHEMA_FILE).read_text(encoding="utf-8")
    bridge = (ROOT / BRIDGE_INIT_FILE).read_text(encoding="utf-8")
    versions: dict[str, int] = {}
    for label, pattern, source in (
        ("qx-datastruct", r"pub const BAR_FRAME_JSON_SCHEMA_VERSION: u32 = (\d+);", writer),
        ("qx-data/DATA_SCHEMA_VERSION", r"pub const DATA_SCHEMA_VERSION: u32 = (\d+);", schema),
        ("qianxing_bridge", r"^BAR_FRAME_SCHEMA_VERSION = (\d+)", bridge),
    ):
        found = re.search(pattern, source, re.MULTILINE)
        versions[label] = int(found.group(1)) if found else -1
    # `qx-data` 的常量是 `DATA_SCHEMA_VERSION` 的别名，本身不印数字，所以按存在性计入。
    versions["qx-data"] = (
        1 if "pub const BAR_FRAME_SCHEMA_VERSION: u32 = DATA_SCHEMA_VERSION;" in provider else -1
    )
    check(
        len(versions) == 4
        and all(value == 1 for value in versions.values())
        and '"{{\\"schema_version\\":{},\\"instrument\\":' in writer
        and "wire.schema_version > BAR_FRAME_JSON_SCHEMA_VERSION" in writer,
        "BarFrame 的三处契约版本常量同为一个数字，写侧把它印成文档第一格、读侧对更高版本 fail closed",
        f"版本号 {versions}",
    )
    rust_keys: list[str] = []
    literal = re.search(
        r'pub fn to_json\(&self\) -> String \{\s*format!\(\s*"((?:[^"\\]|\\.)*)"', writer
    )
    if literal:
        rust_keys = re.findall(r'\\"([a-z_]+)\\"', literal.group(1))
    fields = re.search(r"BAR_FRAME_JSON_FIELDS = \(([^)]*)\)", bridge, re.DOTALL)
    python_keys = re.findall(r'"([a-z_]+)"', fields.group(1)) if fields else []
    check(
        len(rust_keys) == len(python_keys) + 1
        and rust_keys[:1] == ["schema_version"]
        and set(rust_keys) == {"schema_version", *python_keys},
        "写侧印出的键集合 = Python v1 严格模式允许的键集合（少一格就会被判未知键）",
        f"Rust {rust_keys}；Python {python_keys}",
    )
    cases = (ROOT / FRAME_CONTRACT_TEST_FILE).read_text(encoding="utf-8")
    missing_cases = [name for name in FRAME_CONTRACT_CASES if f"fn {name}()" not in cases]
    check(
        not missing_cases,
        "帧契约用例在位：声明版本、拒绝未来版本、无版本老文档仍走兼容分支",
        f"缺用例 {missing_cases}",
    )



def snapshot_money_honesty_check() -> None:
    """账户快照不得把"没算过的钱"印成 0（V11 Q67 / 交易链路 TX2）。"""
    protocol = (ROOT / SNAPSHOT_PROTOCOL_FILE).read_text(encoding="utf-8")
    # 读模型侧不按 `#[cfg(test)]` 截断：api_service.rs 里那个标记挂在单个测试专用函数上，
    # 截断会把后面的生产代码一起丢掉。
    reader = (ROOT / SNAPSHOT_READ_FILE).read_text(encoding="utf-8")
    endpoint = (ROOT / SNAPSHOT_ENDPOINT_FILE).read_text(encoding="utf-8").split("#[cfg(test)]")[0]

    check(
        all(
            protocol.count(f"pub {name}: Option<i128>,") == 1
            for name in SNAPSHOT_OPTIONAL_FIELDS
        )
        and protocol.count("pub equity_raw: Option<i128>,") == 1,
        "八个汇总钱字段在协议上全是 Option<i128>，没有任何一个还能退回不可区分的 i128（V11 Q70）",
        "有字段退回不可区分的 i128（未算与算出为零又会长成同一个数）",
    )
    # 取法体内逐个直取：一行字面量计数挡得住"改回 Some(…) 硬包"，挡不住
    # `Some(self.equity_raw.unwrap_or(0))` 这种同样能折平未算的写法，所以盯整段体。
    raw_start = protocol.find("fn scalar_money_raw(&self)")
    raw_end = protocol.find(chr(10) + "    }", raw_start) if raw_start >= 0 else -1
    raw_body = protocol[raw_start:raw_end] if raw_end > raw_start >= 0 else ""
    raw_missing = [name for name in SNAPSHOT_SCALARS if f"self.{name}," not in raw_body]
    check(
        protocol.count("fn scalar_money_raw(&self) -> [Option<i128>; 8]") == 1
        and raw_body.count("self.equity_raw,") == 1
        and raw_missing == []
        and "Some(" not in raw_body
        and "unwrap_or" not in raw_body,
        "八个标量钱的取法只有一处、八项逐字段直取，体内没有 Some/unwrap_or 能把未算折成零",
        f"取法定义 {protocol.count('fn scalar_money_raw(&self) -> [Option<i128>; 8]')} 处、"
        f"体内权益 {raw_body.count('self.equity_raw,')} 处、缺项 {raw_missing}",
    )
    check(
        protocol.count("Self::write_scalar_money(&mut h, self.scalar_money_raw())") == 2,
        "state_hash 与 scalar_hash 共用同一份标量写入，两处不会各说一套",
        "有一处哈希不再走共用的 write_scalar_money",
    )
    # V11 Q68 把标量与持仓行共用的存在性标记搬进 `write_optional_money`：锚点跟着搬，
    # 但仍要求标量那条链只经由 write_scalar_money 一次转发到它。
    hashing = protocol[protocol.find("fn write_optional_money") :]
    check(
        protocol.count("fn write_scalar_money(") == 1
        and protocol.count("Self::write_optional_money(hasher, value);") == 1
        and 'hasher.write_u64(u64::from(value.is_some()));' in hashing[:200]
        and "value.unwrap_or_default()" in hashing[:200],
        "哈希带存在性标记：None 与 Some(0) 是两份状态，未算永远改不出一个「看起来算过」的 0",
        "存在性标记被摘掉，未算与算出为零会撞成同一个哈希",
    )
    # 槽位只数稳定 JSON 那一条格式串：持仓快照也印 margin_raw/unrealized_pnl_raw，
    # 全文件计数会把两种形状混在一起。
    slots = [
        line for line in protocol.splitlines() if '\\"equity_raw\\":{}' in line
    ]
    arg_start = protocol.find("scalars[0],")
    args = protocol[arg_start : protocol.find("positions,", arg_start)] if arg_start >= 0 else ""
    check(
        protocol.count("let scalars = self.scalar_json_values();") == 1
        and protocol.count('None => "null".to_string()') == 1
        and len(slots) == 1
        and all(slot.count(f'\\"{name}\\":{{}}') == 1 for slot in slots for name in SNAPSHOT_SCALARS)
        and args != ""
        and all(f"scalars[{index}]," in args for index in range(8))
        and not any(f"self.{name}" in args for name in SNAPSHOT_SCALARS),
        "稳定 JSON 的八个钱槽位只由 scalar_json_values 填，未算印 null 而不是合法的 0",
        f"钱槽位写法不再唯一（格式串 {len(slots)} 条、实参 {args!r}）或 null 口径丢失",
    )
    # 逐字面量禁抄法要盯住"改协议后仍然编译得过"的写法：available 已是 Option<i128>，
    # 旧的 `= snapshot.equity_raw` 现在根本通不过类型检查，只禁它等于禁一条回不来的形态。
    all_rust = "".join(path.read_text(encoding="utf-8") for path in CRATES.rglob("*.rs"))
    equity_copy = [
        form
        for form in (
            "snapshot.available_raw = snapshot.equity_raw",
            "snapshot.available_raw = Some(snapshot.equity_raw)",
            "available_raw.unwrap_or(snapshot.equity_raw)",
            "available_raw.unwrap_or(equity_raw)",
        )
        if form in all_rust
    ]
    check(
        equity_copy == [],
        "「可用资金 = 权益副本」这条抄法不得回到任何一处（含 Some(…) 包起来的可编译写法）",
        f"读模型又把压在持仓上的那段钱说成可自由花掉：{equity_copy}",
    )
    equity_start = reader.find("snapshot.equity_raw =")
    equity_stmt = (
        reader[equity_start : reader.find(";", equity_start) + 1]
        if equity_start >= 0
        else ""
    )
    check(
        reader.count("snapshot.equity_raw =") == 1
        and "equity_for(" in equity_stmt
        and "cash_for" not in equity_stmt
        and "unwrap_or" not in equity_stmt,
        "权益只在现金与每一条持仓的标记价都读得出时发布；算不出即缺席，不退回纯现金（V11 Q70）",
        f"权益语句被改回兜底写法或算点丢失：{equity_stmt.strip()!r}",
    )
    check(
        reader.count("snapshot.available_raw = Some(") == 1
        and "cash_for(account_id, pipeline.settlement_currency())"
        in reader[reader.find("snapshot.available_raw") :],
        "可用资金取本条快照记账的那一本结算账簿现金",
        "available 的算点丢失或换成了别的口径",
    )
    check(
        reader.count("snapshot.fees_raw = Some(fees_raw);") == 1
        and reader.count("fill.fee_raw.checked_add(fees_raw)") == 1
        and reader.count("snapshot.fees_raw = Some(0)") == 0,
        "账户费用合计由本快照的逐笔成交费用加出，且溢出即拒而不是回绕",
        "费用算点丢失、被写回常量 0，或改用了会回绕的加法",
    )
    # 整文件扫描，不在 `#[cfg(test)]` 处截断：被截断的读模型尾部正是这些赋值会落下的地方。
    fabricated = {
        path.relative_to(ROOT).as_posix(): sum(
            path.read_text(encoding="utf-8").count(f"snapshot.{name} =")
            for name in SNAPSHOT_UNCOMPUTED_FIELDS
        )
        for path in sorted(CRATES.rglob("src/**/*.rs"))
        if "src/tests" not in path.as_posix()
    }
    fabricated = {path: count for path, count in fabricated.items() if count}
    check(
        fabricated == {},
        "保证金/冻结/已实现/未实现/资金费这五个字段在本层没有来源，产码里不得出现给它们赋值的写法",
        f"凭空造数的位置 {fabricated}",
    )
    balances = endpoint[endpoint.find('("GET", "/account/balances")') :]
    balances = balances[: balances.find('("GET", "/control/audit")')]
    check(
        balances.count(
            '"available_raw": snapshot.as_ref().and_then(|snapshot| snapshot.available_raw)'
        )
        == 1
        and balances.count(
            '"margin_raw": snapshot.as_ref().and_then(|snapshot| snapshot.margin_raw)'
        )
        == 1
        and balances.count(
            '"equity_raw": snapshot.as_ref().map(|snapshot| snapshot.equity_raw)'
        )
        == 1
        and "equity_raw).unwrap_or" not in balances
        and "available_raw).unwrap_or" not in balances
        and "margin_raw).unwrap_or" not in balances,
        "/account/balances 把未算发布成 null，而不是给它兜一个 0",
        "余额端点对 equity/available/margin 又用了兜底写法，或不再原样透出这三个取法",
    )
    cli_cases = (ROOT / SNAPSHOT_CLI_CASE_FILE).read_text(encoding="utf-8")
    core_cases = (ROOT / SNAPSHOT_CORE_CASE_FILE).read_text(encoding="utf-8")
    # 端点用例住在 `#[cfg(test)] mod tests` 里，要用整文件而不是上面截过断的生产体。
    endpoint_cases = (ROOT / SNAPSHOT_ENDPOINT_FILE).read_text(encoding="utf-8")
    check(
        all(f"fn {name}(" in cli_cases for name in SNAPSHOT_MONEY_CASES)
        and "fn uncomputed_money_is_not_the_same_state_as_computed_zero(" in core_cases
        and "fn balances_endpoint_publishes_absent_money_as_null_not_zero(" in endpoint_cases,
        "五条读模型用例（结算账簿口径/费用同源/未算缺席/溢出即拒/缺标记价的权益缺席）"
        "加协议侧缺席≠零、端点侧 null 发布各一条在位",
        f"缺少用例：{[name for name in SNAPSHOT_MONEY_CASES if f'fn {name}(' not in cli_cases]}",
    )
    # V11 R10：把同一条纪律推到对账两格。R7 只补上了"有来源"，剩下的半步是 0 仍同时表示
    # "从未对账"与"对过且无差异"——看板会把没跑过对账的账户读成绿色。三处编码都必须分开。
    wire = (ROOT / POSITION_WIRE_FILE).read_text(encoding="utf-8")
    reconcile_writer = reader[reader.find("fn apply_reconcile_reports") :]
    cuts = [
        cut
        for stop in ("\nfn ", "\npub(crate) fn ")
        if (cut := reconcile_writer.find(stop, 1)) > 0
    ]
    reconcile_writer = reconcile_writer[: min(cuts)] if cuts else reconcile_writer
    check(
        wire.count("pub last_reconcile_ts: Option<u64>,") == 1
        and wire.count("pub discrepancy_count: Option<u32>,") == 1
        and wire.count("AccountSnapshot::write_optional_money(hasher, self.") == 2
        and wire.count("AccountSnapshot::money_json(self.") == 2
        and reconcile_writer.count("= last_reconcile_ts;") == 1
        and reconcile_writer.count("last_reconcile_ts.map(|_|") == 1
        and "unwrap_or(0)" not in reconcile_writer
        and "fn account_snapshot_reconcile_fields_come_from_the_reports_on_disk(" in cli_cases,
        "对账两格由「本账户有没有报告」这一个问题决定在不在，缺席与算出的零在类型/哈希/JSON 上都不是同一份",
        "对账侧退回不可区分的整数，或写侧又给缺席兜了一个合法的 0",
    )


# V11 Q68：把 Q67 那条"没算过的钱不能印成 0"的纪律推到**持仓行**与**实盘回报入口**
# （交易链路 TX2b）。三处此前各自独立地会凭空造数：内核持仓观察的三个钱字段是不可区分
# 的 `Money`、线格式行与读模型回退行写死 0、CCXT 缺 `side` 时默认多头。
POSITION_CORE_FILE = "crates/qx-core/src/event.rs"
POSITION_WIRE_FILE = "crates/qx-protocol/src/wire.rs"
POSITION_CCXT_FILE = "crates/qx-cli/src/ccxt_facts.rs"
POSITION_RUNTIME_FILE = "crates/qx-runtime/src/pipeline.rs"
POSITION_CCXT_CASE_FILE = "crates/qx-cli/src/tests/ccxt_position_facts_honesty.rs"
# 内核侧三个字段 / 线格式侧两列（线格式只有一列保证金）。
POSITION_CORE_FIELDS = ("unrealized_pnl", "initial_margin", "maintenance_margin")
POSITION_ROW_FIELDS = ("unrealized_pnl_raw", "margin_raw")
POSITION_CCXT_CASES = (
    "unknown_position_side_is_refused_instead_of_guessed_as_long",
    "flat_position_without_side_is_skipped_before_the_direction_gate",
    "unreported_position_money_stays_unreported_and_zero_stays_zero",
)
POSITION_ROW_CASES = (
    "ledger_fallback_position_row_leaves_uncomputed_money_absent",
    "venue_reported_row_keeps_reported_zero_and_unreported_absent",
)


def position_money_honesty_check() -> None:
    """持仓行的未算钱不得印成 0，缺方向的持仓不得靠猜定符号（V11 Q68 / TX2b）。"""
    core = (ROOT / POSITION_CORE_FILE).read_text(encoding="utf-8").split("#[cfg(test)]")[0]
    wire = (ROOT / POSITION_WIRE_FILE).read_text(encoding="utf-8").split("#[cfg(test)]")[0]
    protocol = (ROOT / SNAPSHOT_PROTOCOL_FILE).read_text(encoding="utf-8").split("#[cfg(test)]")[0]
    ccxt = (ROOT / POSITION_CCXT_FILE).read_text(encoding="utf-8")
    # 禁零与方向口径只能落在持仓函数上：余额侧的 `.unwrap_or(Money::ZERO)` 说的是
    # "这本账簿里没有这个币种"，那语义本来就是零。
    position_fn = ccxt[ccxt.find("pub(crate) fn ccxt_position_facts") :]
    position_fn = position_fn[: position_fn.find("pub(crate) fn ccxt_funding_fact")]
    reader = (ROOT / SNAPSHOT_READ_FILE).read_text(encoding="utf-8")
    runtime = (ROOT / POSITION_RUNTIME_FILE).read_text(encoding="utf-8")

    struct_region = core[
        core.find("pub struct AccountPositionSnapshot") : core.find(
            "pub struct FundingRateSnapshot"
        )
    ]
    # 这条只能由静态项钉住：serde 对 `Option<T>` 字段的缺键本来就读成 `None`，摘掉
    # `#[serde(default)]` 不改变今天的行为（Q68 变异 MQ68a 实测），真实行为由内核那条
    # 摘要用例锁着。留着声明是为了下次有人把类型改回裸 `Money` 时在同一行上撞墙。
    check(
        all(
            re.search(
                rf'#\[serde\(default\)\]\s*\n\s*pub {name}: Option<Money>,', struct_region
            )
            for name in POSITION_CORE_FIELDS
        )
        and "Money::ZERO" not in struct_region,
        "内核持仓观察的三个钱字段是 Option<Money>，且每个都带 #[serde(default)]（老日志缺键读成未报）",
        "有字段退回不可区分的 Money，或回读时会给缺席补一个伪造的零",
    )
    digest = core[core.find("h.write_u64(14);") :]
    digest = digest[: digest.find("h.write_u64(position.leverage")]
    check(
        digest.count("h.write_u64(u64::from(value.is_some()));") == 1
        and digest.count("value.map(Money::raw).unwrap_or_default()") == 1
        and not any(f"position.{name}.raw()" in digest for name in POSITION_CORE_FIELDS),
        "事件摘要逐字段带存在性标记：未报与报为零是两份内容，改日志里的 null 为 0 会改动摘要",
        f"摘要里的存在性标记 {digest.count('h.write_u64(u64::from(value.is_some()));')} 处，"
        f"或又出现不区分未报的裸 .raw() 写入",
    )
    check(
        all(wire.count(f"pub {name}: Option<i128>,") == 1 for name in POSITION_ROW_FIELDS)
        and all(wire.count(f"{name}: None,") == 1 for name in POSITION_ROW_FIELDS)
        # 价格继续用 0 表达"没有"：定点价格里 0 不是合法值，钱没有这个性质。
        and wire.count("pub mark_price_raw: i128,") == 1,
        "线格式行的两个钱列是 Option<i128> 且默认缺席，价格列仍按 0 哨兵保持不变",
        "行字段退回 i128 或 Default 又兜了一个 0",
    )
    check(
        wire.count("unrealized_pnl_raw: observation.unrealized_pnl.map(Money::raw)") == 1
        and wire.count("margin_raw: observation.initial_margin.map(Money::raw)") == 1
        and wire.count("unrealized_pnl: wire.unrealized_pnl_raw.map(Money::from_raw)") == 1
        and wire.count("initial_margin: wire.margin_raw.map(Money::from_raw)") == 1
        and wire.count("maintenance_margin: None,") == 1,
        "内核观察 ↔ 线格式行的钱列折算各只有一处，线格式没有的那一列折算回未报而不是 0",
        "折算层出现了第二份手抄，或缺失列被兜成零",
    )
    hashing = protocol[protocol.find("fn write_optional_money") :]
    check(
        protocol.count("fn write_optional_money(") == 1
        and 'hasher.write_u64(u64::from(value.is_some()));' in hashing[:200]
        and protocol.count("Self::write_optional_money(&mut h, value);") == 1,
        "行哈希与账户标量共用同一个带存在性标记的写入助手，两处不会各说一套",
        "存在性标记助手被复制或行哈希绕开了它",
    )
    check(
        protocol.count("fn money_json(") == 1
        and protocol.count("Self::money_json") == 1
        and "let positions = json_position_entries(&self.positions);" in protocol
        and not [slot for slot in ("Self::money_json(value.", '\\"quantity_raw\\":{}') if slot in protocol],
        "持仓行的钱槽位不再手抄：整行交给 serde、未报由 Option 印成 null，手填槽位只剩 money_json 一个来源",
        f"money_json 定义 {protocol.count('fn money_json(')} 处、使用 "
        f"{protocol.count('Self::money_json')} 处、持仓行渲染 "
        f"{protocol.count('json_position_entries(&self.')} 处",
    )
    # 抄法回归要盯"改协议后仍然编译得过"的写法：整仓逐行扫，字段名在行首缩进后才算，
    # 免得把 available_margin_raw 这类同后缀字段误伤成命中。
    fabricated_rows = [
        f"{path.relative_to(ROOT).as_posix()}:{name}"
        for path in sorted(CRATES.rglob("*.rs"))
        for text in (path.read_text(encoding="utf-8"),)
        for name in POSITION_ROW_FIELDS + POSITION_CORE_FIELDS
        if re.search(rf"^\s*{name}: (?:0|Money::ZERO)\b", text, re.MULTILINE)
    ]
    check(
        fabricated_rows == [],
        "生产与用例里都不给持仓行的钱字段兜一个写死的 0",
        f"凭空造数的位置 {fabricated_rows}",
    )
    check(
        reader.count("unrealized_pnl_raw: None,") == 1 and reader.count("margin_raw: None,") == 1,
        "读模型用 Ledger 自拼的那一行持仓承认自己算不出这两个钱字段",
        "回退行又开始替交易所报数",
    )
    check(
        position_fn.count('Some(value @ ("long" | "short"))') == 1
        and "读到 {read:?}" in position_fn
        and "position_side: Some(side)" in position_fn
        and not any(
            form in position_fn
            for form in ('unwrap_or("long"', '=> "long"', 'unwrap_or_else(|| "long"', '"unknown" =>')
        ),
        "CCXT 持仓方向只认 long/short（大小写归一），缺失与连接器占位的 unknown 一律 fail-closed",
        "方向闸门又允许默认多头，或占位值被当成可接受输入",
    )
    skip_at = position_fn.find("if contracts == 0 {")
    gate_at = position_fn.find("let side = match")
    check(
        0 <= skip_at < gate_at,
        "零数量行在方向闸门之前跳过：平仓位通常不带方向，不该被 fail-closed 连带打死",
        f"跳过点 {skip_at} 不在方向闸门 {gate_at} 之前",
    )
    check(
        position_fn.count(
            "let optional_money = |key: &str| -> Result<Option<Money>, String>"
        )
        == 1
        and all(
            position_fn.count(f'optional_money("{name}")') == 1
            for name in ("unrealized_pnl_raw", "initial_margin_raw", "maintenance_margin_raw")
        )
        and position_fn.count("if value.is_null() {") == 2
        and "Money::ZERO" not in position_fn,
        "CCXT 回报的三个钱字段只由一个 optional_money 读法取，省略与 null 都读成未报",
        "钱字段读法出现第二处复制或又被兜成零",
    )
    normalize_region = runtime[runtime.find("fn normalize_positions") :]
    normalize_region = normalize_region[: normalize_region.find("fn storage_error")]
    guards = re.findall(
        r"\.is_some_and\(\|(pnl|margin)\| \1\.raw\(\) (?:== i128::MIN|< 0)\)",
        normalize_region,
    )
    check(
        sorted(guards) == ["margin", "margin", "pnl"]
        and not re.search(
            r"^\s*(?:initial|maintenance)_margin\.raw\(\) < 0", runtime, re.MULTILINE
        ),
        "持仓快照的非法保证金守卫建立在「报了才判」上，不给未报补一个可比较的零",
        f"守卫只剩 {sorted(guards)}，未报会被当成零参与判定",
    )
    ccxt_cases = (ROOT / POSITION_CCXT_CASE_FILE).read_text(encoding="utf-8")
    reader_cases = (ROOT / SNAPSHOT_CLI_CASE_FILE).read_text(encoding="utf-8")
    protocol_cases = (ROOT / SNAPSHOT_CORE_CASE_FILE).read_text(encoding="utf-8")
    core_cases = (ROOT / POSITION_CORE_FILE).read_text(encoding="utf-8")
    runtime_cases = (ROOT / POSITION_RUNTIME_FILE).read_text(encoding="utf-8")
    check(
        all(f"fn {name}(" in ccxt_cases for name in POSITION_CCXT_CASES)
        and all(f"fn {name}(" in reader_cases for name in POSITION_ROW_CASES)
        and "fn unreported_position_money_is_not_hashed_as_zero(" in core_cases
        and "fn absent_venue_money_stays_absent_through_the_wire_folding(" in protocol_cases
        and "fn uncomputed_position_money_is_not_the_same_state_as_computed_zero("
        in protocol_cases
        and "fn default_position_row_reports_no_money(" in protocol_cases
        and "ETH/USDT:USDT.OKX" in runtime_cases,
        "Q68 的八处行为用例在位（方向 fail-closed/平仓位跳过/钱字段未报/摘要区分/折算区分/行两态/默认行不报钱/落盘回读）",
        f"缺 ccxt 用例 {[n for n in POSITION_CCXT_CASES if f'fn {n}(' not in ccxt_cases]}、"
        f"缺读模型用例 {[n for n in POSITION_ROW_CASES if f'fn {n}(' not in reader_cases]}、"
        f"缺内核摘要用例={'unreported_position_money_is_not_hashed_as_zero' not in core_cases}、"
        f"缺折算/行用例={[n for n in ('absent_venue_money_stays_absent_through_the_wire_folding', 'uncomputed_position_money_is_not_the_same_state_as_computed_zero', 'default_position_row_reports_no_money') if f'fn {n}(' not in protocol_cases]}、"
        f"缺落盘回读夹具={'ETH/USDT:USDT.OKX' not in runtime_cases}",
    )



# V11 Q69（交易链路 TX3）：一轮 CCXT 对账有两半发现，此前各自只喂到一面 —— 本地订单查不到
# 远端结果只进 `ReconcileRequired` 事实流，远端活动订单本地归并不了只进报告与健康判定；于是
# 「有一笔单子结果未知」在三个面上互相矛盾（worker 报 Ready、报告说没问题、事实流在喊待对账）。
# 报告的三个覆盖度计数同批收敛：Binance 侧硬写 0 等于替账户宣称「没有持仓、没有资金费」。
CCXT_RECONCILE_FILE = "crates/qx-cli/src/venue_runtime/ccxt_reconcile_worker.rs"
BINANCE_RECONCILE_FILE = "crates/qx-cli/src/venue_runtime/binance_reconcile.rs"
RECONCILE_REPORT_FILE = "crates/qx-api/src/lib.rs"
RECONCILE_ROUND_CASE_FILE = "crates/qx-cli/src/tests/ccxt_reconcile_round.rs"
RECONCILE_DISCOVERY_CASE_FILE = "crates/qx-cli/src/tests/worker_observability.rs"
RECONCILE_COVERAGE_FIELDS = (
    "position_snapshots_count",
    "funding_rate_snapshots_count",
    "cashflow_count",
)
RECONCILE_OBSERVED_FLAGS = ("positions_observed", "funding_rates_observed", "cashflows_observed")
RECONCILE_ROUND_CASES = (
    "ccxt_reconcile_round_routes_both_discovery_halves_to_report_and_fact_stream",
    "ccxt_reconcile_service_status_degrades_on_a_local_only_finding",
)


def reconcile_round_honesty_check() -> None:
    """对账的两半发现要同时喂到事实流、报告与健康；覆盖度计数不得把「没取」印成 0。"""
    worker = (ROOT / CCXT_RECONCILE_FILE).read_text(encoding="utf-8")
    binance = (ROOT / BINANCE_RECONCILE_FILE).read_text(encoding="utf-8")
    api = (ROOT / RECONCILE_REPORT_FILE).read_text(encoding="utf-8").split("#[cfg(test)]")[0]
    cases = (ROOT / RECONCILE_ROUND_CASE_FILE).read_text(encoding="utf-8")
    discovery_cases = (ROOT / RECONCILE_DISCOVERY_CASE_FILE).read_text(encoding="utf-8")
    # 去空白后再比形状：这些写法会被 rustfmt 折行，逐字面量数会误报。
    tight = "".join(worker.split())

    check(
        worker.count("ccxt_reconcile_round(&open_order_issues, &local_order_issues)") == 1,
        "CCXT 一轮对账的两半发现只经 ccxt_reconcile_round 汇总一次",
        "汇总点丢失、少喂一半，或出现第二份手抄",
    )
    report_at = worker.find("persist_reconcile_report(ReconcileReportInput")
    heartbeat_at = worker.find("context.heartbeat", report_at)
    status_at = worker.find("let service_status")
    check(
        0 <= report_at < heartbeat_at < status_at,
        "报告写入与健康判定排在汇总之后：先落报告再降级，顺序倒了等于本轮结论丢失",
        f"report={report_at} heartbeat={heartbeat_at} status={status_at}",
    )
    report_region = worker[report_at:heartbeat_at]
    status_region = worker[status_at:]
    check(
        report_region.count("additional_order_issues: &round.order_issues") == 1
        and "additional_order_issues: &open_order_issues" not in worker,
        "对账报告的清单就是汇总后的两半，不再回手只挑远端那份",
        "报告又只收远端一半",
    )
    check(
        "ccxt_reconcile_service_status(&round, &balance_discrepancies)" in status_region
        and worker.count("fn ccxt_reconcile_service_status(") == 1
        and worker.count("round.order_issues.is_empty() && balance_discrepancies.is_empty()")
        == 1
        and "open_order_issues.is_empty()" not in worker,
        "worker 健康判定读同一份 round.order_issues（本地-only 的发现也要降级）",
        "健康判定又只看远端一半：有单子结果未知时仍会报 Ready",
    )
    check(
        worker.count(".require_reconcile(") == 1
        and "for fact in &round.require_reconcile" in worker
        and worker.count("with_tag(fact.event_tag)") == 1,
        "待对账事实的写入点只有一处，条目与标签都来自汇总清单",
        f"写入点 {worker.count('.require_reconcile(')} 处",
    )
    round_fn = worker[worker.find("pub(crate) fn ccxt_reconcile_round(") :]
    check(
        round_fn.count('issue.get("client_order_id")?.as_u64()?') == 1
        and "parse::<u64>" not in round_fn
        and round_fn.count(".chain(local_order_issues)") == 1,
        "只有能对上本地订单的发现才落 ReconcileRequired：对不上的远端孤单不得把字符串客户号当本地句柄",
        "句柄判据被放宽成「带 client_order_id 就落事实」，或清单少并一半",
    )
    check(
        all(
            api.count(f"#[serde(default)]\n    pub {name}: Option<usize>,") == 1
            for name in RECONCILE_COVERAGE_FIELDS
        )
        and not any(f"pub {name}: usize," in api for name in RECONCILE_COVERAGE_FIELDS),
        "报告的三个覆盖度计数是 Option<usize> 且带 serde default：没取≠取了且为空，老报告缺键仍读得回来",
        f"字段形态 {[name for name in RECONCILE_COVERAGE_FIELDS if f'pub {name}: Option<usize>,' not in api]}",
    )
    check(
        all(f"{name}: None," in binance for name in RECONCILE_COVERAGE_FIELDS)
        and not any(f"{name}: 0," in binance for name in RECONCILE_COVERAGE_FIELDS),
        "Binance 现货这条链对自己从不查询的三项报「没取」，不再写死 0",
        "覆盖度计数又回到常量 0",
    )
    check(
        all(f"let mut {flag} = false;" in worker for flag in RECONCILE_OBSERVED_FLAGS)
        and worker.count("positions_observed = true;") == 1
        and worker.count("funding_rates_observed = true;") == 1
        and worker.count("cashflows_observed = true;") == 2
        and all(f"{flag}.then" in tight for flag in RECONCILE_OBSERVED_FLAGS),
        "CCXT 侧每个覆盖度计数都由「本轮真的取到了」的观测位门控，能力缺失被跳过时报缺席而不是零",
        f"观测位缺失 {[f for f in RECONCILE_OBSERVED_FLAGS if f'let mut {f} = false;' not in worker]}",
    )
    check(
        all(f"fn {name}(" in cases for name in RECONCILE_ROUND_CASES)
        and "fn ccxt_open_orders_report_unknown_and_unmapped_remote_risk(" in discovery_cases,
        "Q69 用例在位：两半发现同时进报告与事实流，四类远端归并口径另有独立用例咬住",
        f"缺用例 {[n for n in RECONCILE_ROUND_CASES if f'fn {n}(' not in cases]}"
        f" / 缺归并用例={'ccxt_open_orders_report_unknown_and_unmapped_remote_risk' not in discovery_cases}",
    )


def control_plane_honesty_check() -> None:
    """控制面按 role 找 worker、只读运维端点按磁盘读现状、可用值只列一份（V11 S1/S2/S3/S5/S6）。"""
    contract = (ROOT / "crates/qx-cli/src/strategy_contract.rs").read_text(encoding="utf-8")
    workers = (ROOT / "crates/qx-cli/src/workers.rs").read_text(encoding="utf-8")
    api = (ROOT / "crates/qx-api/src/lib.rs").read_text(encoding="utf-8")
    assembly = (ROOT / "crates/qx-cli/src/api_service.rs").read_text(encoding="utf-8")
    init = (ROOT / "crates/qx-cli/src/init_project.rs").read_text(encoding="utf-8")
    help_text = (ROOT / "crates/qx-cli/src/cli_help.rs").read_text(encoding="utf-8")
    identity_cases = (
        ROOT / "crates/qx-cli/src/tests/runtime_api_worker_identity.rs"
    ).read_text(encoding="utf-8")
    live_cases = (ROOT / "crates/qx-cli/src/tests/api_query_models_live.rs").read_text(
        encoding="utf-8"
    )

    check(
        'supervisor.spawn_worker("api"' not in contract
        and contract.count("supervisor.spawn_worker(&api_worker_id") == 2
        and contract.count("fn configured_api_worker_id(") == 1
        and 'worker.enabled && worker.role == WorkerRole::Api' in contract
        and ".ok_or_else(" in contract[contract.find("fn configured_api_worker_id(") :],
        "serve 把 API 服务注册到按 role 解析出的 worker 名下，且缺该 role 时报错而不是回落字面量",
        "字面量 api 回到 spawn 点，或解析函数又允许挑别的 worker",
    )
    check(
        all(
            f"fn {name}(" in identity_cases
            for name in (
                "renamed_api_worker_is_the_one_the_supervisor_accepts",
                "api_worker_lookup_fails_closed_without_an_enabled_one",
            )
        ),
        "S1 用例在位：改名后的 api worker 仍能注册，而字面量必须失败",
        "缺用例或改名，用例不再能区分字面量与按 role 解析",
    )
    check(
        api.count("pub fn with_query_models_provider") == 1
        and api.count("fn query_models(&self)") == 1
        and assembly.count(
            ".with_query_models_provider(move || load_api_query_models(&query_models_config))"
        )
        == 1,
        "三个只读运维端点装了现读 provider，且 provider 与启动装载共用同一个读点",
        "provider 又回到只读一次，或两条路各写一份读模型构造代码",
    )
    routes = api[api.find('("GET", "/scheduler/runs")') : api.find('("GET", "/account/snapshot/diff")')]
    check(
        routes.count("self.query_models()") == 3
        and "self.job_runs()" not in routes
        and "self.ledger_entries()" not in routes
        and "self.reconcile_reports()" not in routes,
        "调度/账簿/对账三个端点全部改读现读结果，读不到就报 503 而不是念启动那份",
        f"三处里仍有回落到 boot 快照的读点：query_models()={routes.count('self.query_models()')}",
    )
    check(
        all(
            f"fn {name}(" in live_cases
            for name in (
                "reconcile_report_endpoint_follows_the_worker_written_file",
                "ledger_endpoint_sees_fills_appended_after_boot",
            )
        ),
        "S3 用例在位：启动之后落盘的报告与成交都必须读得到，两条机制各自验证",
        "用例缺任一条：只剩文件扫描或只剩日志重放，另一条机制回退了无人发现",
    )
    check(
        workers.count("if context.should_stop() {") == 2
        and "should_stop" in workers,
        "Scheduler 与 Strategy 两个生产循环读停机令牌，与其他 worker 循环同一口径",
        "任一回退：那两个循环的唯一出口又只剩 once 或抛错",
    )
    profile_line = next(
        (line for line in help_text.splitlines() if "--profile <" in line),
        "",
    )
    listed = (
        sorted(profile_line.split("--profile <", 1)[1].split(">", 1)[0].split("|"))
        if profile_line
        else []
    )
    declared = sorted(
        init[init.find("const INIT_PROFILES") :].split("= [", 1)[1].split("]", 1)[0].split('"')[
            1::2
        ]
    )
    check(
        listed == declared == ["ashare", "backtest", "base", "builtin", "ccxt", "multi-venue", "paper"]
        and init.count('INIT_PROFILES.join("、")') == 2
        and "可用值：base、paper" not in init,
        "init --profile 的可用值只有一份：help 的表、错误文案与实际接受的集合三处相等",
        f"help={listed} declared={declared}，或错误文案又抄了一份清单",
    )
    harness = (ROOT / "crates/qx-cli/src/tests/mod.rs").read_text(encoding="utf-8")
    check(
        harness.count("assert_binary_fresh(&binary);") == 2
        and "fn assert_binary_fresh(" in harness,
        "子进程用例的两条 binary 取用分支都过新鲜度守卫（V11 S6）",
        "守卫只装在 option_env! 取得到值的那条分支上：`cargo test --bin` 走的是回落分支且不重链 "
        "qx-cli.exe，过期 binary 会让子进程断言对着旧行为静默变绿",
    )


def api_surface_doc_check() -> None:
    """`serve` 的端点表与路由集合逐一相等：文档没写到的端点等于没有前端（V11 S7）。

    运维端点是这套框架对外唯一的读面，"代码里有、文档里没有"和"文档里有、代码里 404"
    是同一类断链的两个方向，所以把对齐关系写成门禁而不是靠人记得改 README。
    """
    api = (ROOT / "crates/qx-api/src/lib.rs").read_text(encoding="utf-8").split("#[cfg(test)]")[0]
    declared = set(re.findall(r'\("(GET|POST|PUT|DELETE)", "(/[^"]*)"\)', api))
    readme = (ROOT / "deploy/README.md").read_text(encoding="utf-8")
    documented = set()
    for line in readme.splitlines():
        if not line.startswith("| `"):
            continue
        for method, path in re.findall(r"`(GET|POST|PUT|DELETE) (/[^`\[?]+)", line):
            documented.add((method, path))
    check(
        bool(declared) and documented == declared,
        "deploy/README.md 的端点表与 qx-api 路由集合完全一致",
        f"只在代码 {sorted(declared - documented)} / 只在文档 {sorted(documented - declared)}",
    )
    check(
        "未列出的路径一律 404" in readme
        and '_ => ApiResponse::text(404, "not found")' in api,
        "端点表声明的兜底口径与代码一致：未列路径 404 而不是静默 200",
        "路由表兜底被改，或文档删了那句声明",
    )


# V11 S10 / T3：对外发布的那一份账户快照 JSON Schema，以及"契约说的"与三方实现是否同宽。
SNAPSHOT_SCHEMA_FILE = "schemas/account-snapshot-v1.json"
SNAPSHOT_SCHEMA_ROUTE = "/schema/account-snapshot-v1"
SNAPSHOT_SCHEMA_CASE_FILE = "crates/qx-protocol/tests/snapshot_schema_contract.rs"
SNAPSHOT_BRIDGE_FILE = "python/qianxing_bridge/__init__.py"
SNAPSHOT_SCHEMA_CASES = (
    "served_schema_is_the_repository_file_verbatim",
    "served_schema_declares_every_key_the_writer_emits",
    "schema_keeps_uncomputed_money_nullable",
    "reader_rejects_versions_the_served_schema_does_not_declare",
    "reader_refuses_a_self_consistent_document_from_another_version",
    "schema_frame_matches_the_protocol_it_describes",
)

# V11 R17/T2：日历组件指纹的跨语言夹具——Python 写侧产出文档与摘要，Rust 读侧重算同一格。
CALENDAR_FIXTURE_DIR = "python/tests/fixtures"
CALENDAR_FIXTURE_PAIRS = (
    ("calendar-component-v1.json", "calendar-component-v1.fingerprint"),
    ("calendar-component-legacy.json", "calendar-component-legacy.fingerprint"),
)
CALENDAR_RUST_TEST_FILE = "crates/qx-cli/src/tests/calendar_component_fingerprint.rs"
CALENDAR_RUST_RULES_FILE = "crates/qx-xingban/src/ashare.rs"
CALENDAR_PYTHON_MODULE_FILE = "python/qianxing_ashare/__init__.py"
CALENDAR_PYTHON_TEST_FILE = "python/tests/test_ashare.py"
CALENDAR_RUST_CASES = (
    "calendar_component_fingerprints_match_the_shared_python_digest",
    "calendar_digest_follows_the_canonical_fields_not_the_document_layout",
    "calendar_fixtures_load_through_the_backtest_rules_registry",
)
CALENDAR_PYTHON_CASES = (
    "def test_calendar_component_digest_is_the_shared_cross_language_pair",
    "def test_calendar_component_fingerprint_moves_only_with_canonical_fields",
)


def account_snapshot_schema_check() -> None:
    """账户快照的对外契约只有一份文本，且它承诺的每一格两侧都做得到（V11 S10 / T3）。

    `schemas/account-snapshot-v1.json` 与服务常量 `ACCOUNT_SNAPSHOT_JSON_SCHEMA` 此前是两份手抄：
    文件多 `title` 与顶层 `additionalProperties`，双方又都漏掉写侧真实印出的八个钱字段，比对时
    没有谁是权威；读侧 `from_json` 只核对协议字符串与两处版本号是否自相矛盾，一份按别的版本
    自洽封存的文档会被当成 v1 收下，而对面 Python 的 `load_account_snapshot` 对同一份产物直接
    抛错。现在常量 `include_str!` 那份文件——手抄在编译期就没了退路——门禁转去钉"契约说的"与
    "写侧印的、读侧认的、对面语言认的"是同一件事。
    """
    protocol = (ROOT / SNAPSHOT_PROTOCOL_FILE).read_text(encoding="utf-8").split("#[cfg(test)]")[0]
    endpoint = (ROOT / SNAPSHOT_ENDPOINT_FILE).read_text(encoding="utf-8").split("#[cfg(test)]")[0]
    try:
        schema = json.loads((ROOT / SNAPSHOT_SCHEMA_FILE).read_text(encoding="utf-8"))
        emitted = json.loads((ROOT / SNAPSHOT_FIXTURE_FILE).read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        check(
            False,
            "契约文档与写侧夹具都是可解析的 JSON",
            f"{SNAPSHOT_SCHEMA_FILE} / {SNAPSHOT_FIXTURE_FILE}: {error}",
        )
        return
    properties = schema["properties"]
    version = re.search(r"pub const ACCOUNT_SNAPSHOT_SCHEMA_VERSION: u32 = (\d+);", protocol)
    supported = int(version.group(1)) if version else None

    copies = [
        path.relative_to(ROOT).as_posix()
        for path in sorted(CRATES.glob("*/src/**/*.rs"))
        if '"$schema"' in path.read_text(encoding="utf-8")
    ]
    check(
        re.search(
            r"pub const ACCOUNT_SNAPSHOT_JSON_SCHEMA: &str =\s*"
            r'include_str!\("\.\./\.\./\.\./schemas/account-snapshot-v1\.json"\)',
            protocol,
        )
        is not None
        and not copies,
        "账户快照 JSON Schema 只有一份文本：常量按字节包含 schemas/ 那份，生产 Rust 侧无第二份手抄",
        f"常量不再是 include_str!，或这些文件里又出现了 schema 字面量 {copies}",
    )
    check(
        schema["type"] == "object"
        and set(schema["required"]) <= set(properties)
        and all(name in properties for name in SNAPSHOT_SCALARS),
        "契约自身自洽：required 的每一项都在 properties 里，八个钱字段各自有声明",
        f"required 越界 {sorted(set(schema['required']) - set(properties))}"
        f" / 缺钱字段 {[name for name in SNAPSHOT_SCALARS if name not in properties]}",
    )
    check(
        sorted(emitted) == sorted(properties),
        "契约声明的顶层键集合正好等于写侧产物印出的那些：少一格是漏说，多一格是空头承诺",
        f"只在契约 {sorted(set(properties) - set(emitted))}"
        f" / 只在产物 {sorted(set(emitted) - set(properties))}",
    )
    # 可空口径不抄名单：契约允许 null 的那些，必须正好是协议里类型为 `Option<i128>` 的那些。
    # 手抄名单在 V11 合流轮被实测证伪过一次——Q70 把 `equity_raw` 变成 `Option<i128>`，
    # 而契约那侧还写着"权益不可空"，于是契约对自家写侧每天印出的 null 说了谎。
    typed = {
        name: re.search(rf"pub {name}: (Option<i128>|i128),", protocol)
        for name in SNAPSHOT_SCALARS
    }
    optional_by_type = sorted(
        name for name, match in typed.items() if match and match.group(1) == "Option<i128>"
    )
    nullable = sorted(
        name for name in properties if isinstance(properties[name].get("type"), list)
    )
    uncomputed = sorted(key for key, value in emitted.items() if value is None)
    check(
        all(match is not None for match in typed.values())
        and nullable == optional_by_type
        and set(uncomputed) <= set(nullable)
        and len(uncomputed) < len(SNAPSHOT_SCALARS),
        "钱字段的可空声明逐项等于协议类型：`Option<i128>` 才可空，夹具里两种状态同时存在",
        f"协议 Option {optional_by_type} / 契约可空 {nullable}"
        f" / 缺类型声明 {[name for name, match in typed.items() if match is None]}"
        f" / 夹具未算 {uncomputed}",
    )
    named = 'Some("QIANXING_ACCOUNT")' in protocol
    check(
        supported is not None
        and properties["schema_version"].get("const") == supported
        and properties["protocol"].get("const") == "QIANXING_ACCOUNT"
        and named,
        "契约里的协议名与版本号，就是读侧 `from_json` 认的那一对",
        f"读侧常量 {supported} / 契约 const {properties['schema_version'].get('const')}"
        f" / 协议名同源 {named}",
    )
    served = re.search(
        r'\("GET", "/schema/account-snapshot-v1"\)\s*=>\s*\{?\s*ApiResponse::json\(\s*200,\s*'
        r"ACCOUNT_SNAPSHOT_JSON_SCHEMA\s*\)",
        endpoint,
    )
    whitelist = re.search(r"!matches!\(route,([^)]*)\)", endpoint)
    listed = whitelist.group(1).strip() if whitelist else ""
    check(
        served is not None and SNAPSHOT_SCHEMA_ROUTE in listed,
        "GET /schema/account-snapshot-v1 发出的就是这份常量，且客户端取契约不必带操作者凭据",
        f"发出常量={served is not None} / 鉴权白名单 {listed}",
    )
    bridge = (ROOT / SNAPSHOT_BRIDGE_FILE).read_text(encoding="utf-8")
    python_version = re.search(r'value\["schema_version"\] != (\d+)', bridge)
    required_block = re.search(r"required = \{(.*?)\}", bridge, re.S)
    python_required = (
        sorted(re.findall(r'"([^"]+)"', required_block.group(1))) if required_block else []
    )
    check(
        python_version is not None
        and supported is not None
        and int(python_version.group(1)) == supported
        and python_required == sorted(schema["required"]),
        "对面 Python 的快照读侧与契约同宽：认同一个版本号、要求同一组必填键",
        f"Python 版本 {python_version.group(1) if python_version else None} vs {supported}"
        f" / 必填差 {sorted(set(python_required) ^ set(schema['required']))}",
    )
    cases = (ROOT / SNAPSHOT_SCHEMA_CASE_FILE).read_text(encoding="utf-8")
    missing_cases = [name for name in SNAPSHOT_SCHEMA_CASES if f"fn {name}()" not in cases]
    check(
        not missing_cases,
        "契约的六条判据各有常驻用例：一份文本、键集合覆盖写侧、可空口径、版本闸门与跨版本文档",
        f"缺用例 {missing_cases}",
    )


def calendar_fingerprint_caliper_check() -> None:
    """日历组件指纹的两份 canonical 字节必须比同一份夹具（V11 R17 / T2）。

    Python `AshareTradingCalendar.component_fingerprint` 把摘要登记进 DatasetBundle，Rust
    `dataset_component_file_fingerprint` 在回测启动前对同一份文件重算，两边各自手写一遍规范
    字节、中间只有一句 docstring（"必须和 CLI 保持一致"）连着——全仓没有一条用例比过这两个
    数。夹具与摘要落在 `python/tests/fixtures/`：Python 用写侧函数产出、Rust 用读侧重算，两侧
    各自读同一对文件比同一个值，所以任何一侧改了 canonical 字节都会有一侧变红，而摘要在代码里
    没有第二份抄本可抄。同一份文档的字段白名单与契约版本号也一并逐项比对：它们是严格模式判定
    的另一半，漂了就会让一侧收下、另一侧拒收。
    """
    fixture_dir = ROOT / CALENDAR_FIXTURE_DIR
    pairs = []
    for document, digest in CALENDAR_FIXTURE_PAIRS:
        doc_path, digest_path = fixture_dir / document, fixture_dir / digest
        if not doc_path.is_file() or not digest_path.is_file():
            check(
                False,
                "日历夹具与摘要成对存在，两侧用例读的是同一对文件",
                f"缺 {document if not doc_path.is_file() else ''}"
                f" {digest if not digest_path.is_file() else ''}".strip(),
            )
            continue
        pairs.append((document, digest, digest_path.read_text(encoding="utf-8").strip()))
    readers = {
        CALENDAR_RUST_TEST_FILE: (ROOT / CALENDAR_RUST_TEST_FILE).read_text(encoding="utf-8")
        if (ROOT / CALENDAR_RUST_TEST_FILE).is_file()
        else "",
        CALENDAR_PYTHON_TEST_FILE: (ROOT / CALENDAR_PYTHON_TEST_FILE).read_text(encoding="utf-8"),
    }
    unread = [
        name
        for path, text in readers.items()
        for name in [n for pair in CALENDAR_FIXTURE_PAIRS for n in pair]
        if name not in text
    ]
    check(
        len(pairs) == len(CALENDAR_FIXTURE_PAIRS) and not unread,
        "日历夹具与摘要成对存在，两侧用例读的是同一对文件",
        f"没有引用的夹具 {sorted(set(unread))}" if unread else "",
    )
    scanned = list(CRATES.rglob("*.rs")) + list((ROOT / "python").rglob("*.py"))
    copied = []
    for path in sorted(scanned):
        text = path.read_text(encoding="utf-8", errors="ignore")
        if any(digest in text for _, _, digest in pairs):
            copied.append(path.relative_to(ROOT).as_posix())
    check(
        pairs and not copied,
        "期望摘要只在夹具旁边那一份文件里：代码与用例都读文件，不各自抄一份数",
        f"出现抄本 {copied}",
    )

    rules = (ROOT / CALENDAR_RUST_RULES_FILE).read_text(encoding="utf-8")
    module = (ROOT / CALENDAR_PYTHON_MODULE_FILE).read_text(encoding="utf-8")
    rust_fields = re.search(
        r"const ASHARE_CALENDAR_FIELDS: \[&str; (\d+)\] = \[(.*?)\];", rules, re.S
    )
    python_fields = re.search(r"^ASHARE_CALENDAR_FIELDS = \((.*?)\)", module, re.S | re.M)
    rust_list = re.findall(r'"([^"]+)"', rust_fields.group(2)) if rust_fields else []
    python_list = re.findall(r'"([^"]+)"', python_fields.group(1)) if python_fields else []
    rust_version = re.search(r"pub const ASHARE_SCHEMA_VERSION: u32 = (\d+);", rules)
    python_version = re.search(r"^ASHARE_SCHEMA_VERSION = (\d+)", module, re.M)
    declared = int(rust_fields.group(1)) if rust_fields else None
    check(
        bool(rust_fields)
        and bool(python_fields)
        and rust_list == python_list
        and declared == len(rust_list)
        and rust_version is not None
        and python_version is not None
        and rust_version.group(1) == python_version.group(1),
        "日历文档的字段白名单与契约版本号在两侧逐项相等，声明的个数也等于白名单长度",
        f"Rust {rust_list} / Python {python_list}"
        f" / 版本 {rust_version.group(1) if rust_version else None}"
        f" vs {python_version.group(1) if python_version else None}"
        f" / 声明 {declared} != {len(rust_list)}",
    )

    missing = [
        name for name in CALENDAR_RUST_CASES if f"fn {name}()" not in readers[CALENDAR_RUST_TEST_FILE]
    ] + [name for name in CALENDAR_PYTHON_CASES if name not in readers[CALENDAR_PYTHON_TEST_FILE]]
    check(
        not missing,
        "日历指纹的三条 Rust 与两条 Python 常驻反例都在：共摘要、只随规范字段动、夹具能被回测链装载",
        f"缺用例 {missing}",
    )


def c_abi_header_check() -> None:
    """C++ SDK 的头文件是 Rust `#[repr(C)]` 的手抄镜像，镜像必须逐字段比对（V11 S13）。

    `cpp/include/qianxing_strategy.h` 与 `crates/qx-strategy/src/c_api.rs` 描述同一份插件 ABI，
    两侧之间没有任何编译期或链接期耦合：少一个字段、换一个顺序，宿主读到的就是错位内存而不是
    报错。CI 的 `cpp-sdk` 作业只编译 C++ 例子、不把它交给 Rust 宿主加载，所以对齐只能靠这份比对。
    """
    header = (ROOT / "cpp/include/qianxing_strategy.h").read_text(encoding="utf-8")
    rust = (ROOT / "crates/qx-strategy/src/c_api.rs").read_text(encoding="utf-8")

    def to_camel(name: str) -> str:
        # 只有 `vtable` 一段的惯用大小写不是首字母大写（Rust 侧写作 VTable）。
        return "".join(
            {"vtable": "VTable"}.get(part, part[:1].upper() + part[1:])
            for part in name.split("_")
        )

    scalars = {
        "uint64_t": "u64",
        "int64_t": "i64",
        "uint32_t": "u32",
        "int32_t": "i32",
        "uint8_t": "u8",
        "size_t": "usize",
        "char": "c_char",
        "void": "c_void",
    }

    def c_type_to_rust(ctype: str) -> str:
        ctype = " ".join(ctype.split())
        mutable = True
        if ctype.startswith("const "):
            ctype, mutable = ctype[6:], False
        if ctype.endswith("*"):
            return f"*{'mut' if mutable else 'const'} {c_type_to_rust(ctype[:-1])}"
        if ctype in scalars:
            return scalars[ctype]
        if ctype.startswith("qx_"):
            return to_camel(ctype)
        raise KeyError(f"未登记的 C 类型 {ctype}：先补映射，别让它绕过比对")

    c_fields: dict[str, list[tuple[str, str]]] = {}
    for body, tag in re.findall(r"typedef struct[\w ]*\{([^}]*)\}\s*(\w+);", header, re.S):
        if "(*" in body:
            # vtable 里的回调只比名字与顺序；签名两侧各按本语言写法表达，文本不可能逐字相等。
            c_fields[to_camel(tag)] = [
                (c_type_to_rust(ctype), name)
                for ctype, name in re.findall(r"^\s*(\w+) (\w+);", body, re.M)
            ] + [("fn", name) for name in re.findall(r"\(\*(\w+)\)", body)]
            continue
        fields = []
        for line in body.splitlines():
            line = re.sub(r"/\*.*?\*/", "", line).strip().rstrip(";")
            if not line:
                continue
            ctype, _, name = line.rpartition(" ")
            fields.append((c_type_to_rust(ctype), name))
        c_fields[to_camel(tag)] = fields
    c_enums = {
        to_camel(tag): [int(value) for value in re.findall(r"=\s*(\d+)", body)]
        for body, tag in re.findall(r"typedef enum[\w ]*\{([^}]*)\}\s*(\w+);", header, re.S)
    }

    rust_fields: dict[str, list[str]] = {}
    rust_types: dict[str, dict[str, str]] = {}
    rust_enums: dict[str, list[int]] = {}
    for match in re.finditer(r"pub (struct|enum) (\w+) \{([\s\S]*?)\n\}", rust):
        kind, name, body = match.group(1), match.group(2), match.group(3)
        prefix = rust[: match.start()]
        if "#[repr(C)]" not in prefix[prefix.rfind("\n}") :]:
            continue
        if kind == "enum":
            rust_enums[name] = [int(value) for value in re.findall(r"=\s*(\d+),", body)]
            continue
        rust_fields[name] = re.findall(r"pub (\w+): ", body)
        rust_types[name] = dict(re.findall(r"pub (\w+): ([^,\n]+),", body))

    only_header = sorted((set(c_fields) | set(c_enums)) - (set(rust_fields) | set(rust_enums)))
    only_rust = sorted((set(rust_fields) | set(rust_enums)) - (set(c_fields) | set(c_enums)))
    check(
        bool(c_fields) and bool(c_enums) and not (only_header or only_rust),
        "C ABI 的头文件与 Rust `#[repr(C)]` 是同一组类型，两侧都没有多出或缺失的一份",
        f"只在头文件 {only_header} / 只在 Rust {only_rust}",
    )
    mismatch = [
        f"{name}: 头 {[field[1] for field in fields]} != Rust {rust_fields.get(name)}"
        for name, fields in sorted(c_fields.items())
        if [field[1] for field in fields] != rust_fields.get(name)
    ]
    check(
        not mismatch,
        "每个 C ABI 结构的字段名与顺序在头文件与 Rust 侧逐位相等",
        "；".join(mismatch) or "字段顺序就是内存布局，错一位插件读到的是邻居字段",
    )
    typed = [
        f"{name}.{fname}: 头 {ctype} != Rust {rust_types.get(name, {}).get(fname)!r}"
        for name, fields in sorted(c_fields.items())
        for ctype, fname in fields
        if ctype != "fn" and rust_types.get(name, {}).get(fname, "").replace(" ", "") != ctype.replace(" ", "")
    ]
    check(
        not typed,
        "每个 C ABI 字段的类型按映射表逐字段相等（定宽整数、指针与嵌套结构都核对）",
        "；".join(typed) or "类型换一位宽就是静默截断，比字段错位更难查",
    )
    header_version = re.search(r"#define QX_STRATEGY_API_VERSION\s+(\d+)", header)
    rust_version = re.search(r"QX_C_STRATEGY_API_VERSION: u32 = (\d+)", rust)
    check(
        c_enums == rust_enums
        and bool(header_version)
        and bool(rust_version)
        and header_version.group(1) == rust_version.group(1),
        "枚举判别值序列与 ABI 版本常量两侧相等",
        f"枚举 头 {c_enums} / Rust {rust_enums}；版本 头 {header_version and header_version.group(1)}"
        f" / Rust {rust_version and rust_version.group(1)}",
    )


def snapshot_json_table_check() -> None:
    """稳定 JSON 的键表只有一份编码：写侧印成什么，读侧 `from_json` 就得认什么（V11 R14/R15）。

    订单/成交/划转的键是 `u64`，持仓的键是 `InstrumentId`。此前 `to_json` 手抄 `format!`
    把裸数字当对象键，产出的 `{77:{...}}` 连合法 JSON 都不是，`from_json`、SQLite/Postgres
    的 `load_json` 与 Python 侧的 `load_account_snapshot` 会在同一份产物上一起失败；枚举与
    标的也是同一类分叉（数字码 / 字符串标的 vs serde 的变体名 / 对象标的）。持仓更进一层：
    手抄那份少印 `instrument` 与任何新增字段，读侧却按 serde 认，于是新字段被写侧静默丢掉。
    """
    protocol = (ROOT / SNAPSHOT_PROTOCOL_FILE).read_text(encoding="utf-8").split("#[cfg(test)]")[0]
    cases = (ROOT / SNAPSHOT_CORE_CASE_FILE).read_text(encoding="utf-8")

    check(
        protocol.count("fn json_table_entries<K: Serialize, V: Serialize>") == 1
        and protocol.count("serde_json::to_string(table)") == 1,
        "四张键表的稳定 JSON 只有一个渲染点，且它委托 serde 而不是再抄一份字段顺序",
        f"渲染点 {protocol.count('fn json_table_entries<K: Serialize, V: Serialize>')} 处、"
        f"serde 委托 {protocol.count('serde_json::to_string(table)')} 处",
    )
    rendered = [f"let {name} = json_table_entries(&self.{name});" for name in SNAPSHOT_SERDE_TABLES]
    check(
        all(line in protocol for line in rendered)
        and protocol.count("json_table_entries(&self.") == len(SNAPSHOT_SERDE_TABLES)
        and protocol.count("fn json_position_entries(") == 1
        and "let positions = json_position_entries(&self.positions);" in protocol
        and protocol.count("fn instrument_key(") == 1
        and protocol.count("instrument_key(") == 3
        and not [template for template in SNAPSHOT_TABLE_TEMPLATES if template in protocol],
        "持仓/订单/成交/划转全部经该渲染点，标的键只有一个写法，文件里没有剩下的手抄键值模板",
        f"表渲染 {protocol.count('json_table_entries(&self.')} 处、持仓渲染 "
        f"{protocol.count('json_position_entries(&self.')} 处、标的键 "
        f"{protocol.count('instrument_key(')} 处、"
        f"残留模板 {[template for template in SNAPSHOT_TABLE_TEMPLATES if template in protocol]}",
    )
    check(
        "fn stable_json_tables_round_trip_through_the_reader(" in cases,
        "往返用例在位：稳定 JSON 既是合法 JSON、又与 serde 那份同源、还能被 from_json 解回同一份状态",
        "缺 stable_json_tables_round_trip_through_the_reader",
    )
    fixture = (ROOT / SNAPSHOT_FIXTURE_FILE).read_text(encoding="utf-8")
    enum_slots = ('"side":"Sell"', '"status":"Accepted"')
    check(
        all(key in fixture for key in SNAPSHOT_FIXTURE_KEYS)
        and all(slot in fixture for slot in enum_slots),
        "跨语言夹具带着写侧的真实编码：四张键表的键是字符串、枚举是变体名，不是数字键/数字码",
        f"夹具缺键表 {[key for key in SNAPSHOT_FIXTURE_KEYS if key not in fixture]}"
        f" / 缺枚举格 {[slot for slot in enum_slots if slot not in fixture]}",
    )
    bridge_case = (ROOT / SNAPSHOT_BRIDGE_CASE_FILE).read_text(encoding="utf-8")
    fixture_name = Path(SNAPSHOT_FIXTURE_FILE).name
    golden_case = "fn the_cross_language_sample_is_what_the_writer_emits("
    python_loads = "load_account_snapshot(ACCOUNT_SNAPSHOT_SAMPLE"
    check(
        golden_case in cases
        and fixture_name in cases
        and python_loads in bridge_case
        and fixture_name in bridge_case,
        "同一份夹具两侧各钉一次：Rust 比对 `to_json` 全等，Python 把它喂进 `load_account_snapshot`"
        "——读侧不再只吃手抄的空表字典",
        f"Rust 金样用例={golden_case in cases}、Python 读同一份夹具={python_loads in bridge_case}",
    )


# V11 Q72（回测链路 FN9）：三条单腿回测链把整条收益率与全部风控判定压在一个 100,000 的
# 常数上，既不声明也不可见。把仓库自带夹具的 `quantity` 从 1 抬到 2 就零成交，而 stdout 只
# 印 `return_bps=0`，读起来像"这策略不赚不赔"，真相是"这两单位没人给它算过钱"。收口形状与
# 费用/撮合模型同源：常数和读法各一处、来源分三种、本金进装配的必答题、摘要多一个 `account`
# 块，套不下一格本金的多腿链则当场拒收那格声明。
ACCOUNT_BASE_MODULE = "crates/qx-cli/src/backtests/account_base.rs"
BAR_CHAIN_FILE = "crates/qx-cli/src/backtests/single_strategy.rs"
DEPTH_CHAIN_FILE = "crates/qx-cli/src/backtests/depth.rs"
LEG_FUNDING_FILE = "crates/qx-cli/src/backtests/leg_funding.rs"
MULTI_LEG_CHAIN_FILE = "crates/qx-cli/src/backtests/multi_builtin.rs"
ASSEMBLY_MODULE = "crates/qx-cli/src/backtests/mod.rs"
SUMMARY_MODULE = "crates/qx-cli/src/backtests/artifacts.rs"
STRATEGY_SCHEMA = "crates/qx-runtime/src/runtime_config/strategy_schema.rs"
ACCOUNT_BASE_CASES = "crates/qx-cli/src/tests/backtest_account_base.rs"
ACCOUNT_BASE_CLI_CASES = "crates/qx-cli/tests/backtest_account_base.rs"
# 每条单腿链都要把"这一轮压在多少钱上、这个数从哪来"印在结果之前。
ACCOUNT_BASE_PRINT_MARKERS = ("[Strategy · Account]", "[Builtin · Account]", "[Depth · Account]")
# 本金来源的三种说法：没配、配了、多腿按本腿行情定资。前两种必须可区分（Q67 口径）。
ACCOUNT_BASE_SOURCES = (
    "builtin-default",
    "strategy-initial-cash",
    "multi-leg-funding-rule",
)
ACCOUNT_BASE_UNIT_TESTS = (
    "undeclared_backtest_cash_answers_with_the_one_named_default",
    "declared_backtest_cash_lands_verbatim_with_its_own_source",
    "non_positive_declared_cash_fails_rather_than_becoming_a_default",
)
ACCOUNT_BASE_CLI_TESTS = (
    "builtin_entry_prints_the_default_it_booked_with",
    "a_declared_account_base_is_what_makes_the_second_unit_fillable",
    "a_non_positive_declaration_fails_closed",
    "summary_chains_land_the_account_base_they_actually_used",
    "the_multi_leg_entry_refuses_a_single_account_number_and_names_its_own_rule",
)
# 逐字面量比太脆（rustfmt 会折行、注释会改口径词），这里只钉那些"改坏即换语义"的写法。
ACCOUNT_BASE_DEFAULT_DECL = "pub(crate) const DEFAULT_BACKTEST_INITIAL_CASH: i64 = 100_000;"
ACCOUNT_BASE_SOURCE_CONSTS = (
    "BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE",
    "BACKTEST_ACCOUNT_BASE_CONFIG_SOURCE",
    "BACKTEST_ACCOUNT_BASE_FUNDING_RULE_SOURCE",
)
BACKTEST_ACCOUNT_BASE_FUNDING_USE = (
    '"account_base_source": BACKTEST_ACCOUNT_BASE_FUNDING_RULE_SOURCE'
)
ASSEMBLY_CASH_PARAM = "initial_cash: Money,"
SUMMARY_ACCOUNT_BLOCK = '"source": input.account_base.source,'
SCHEMA_CASH_FIELD = "pub initial_cash_raw: Option<i128>,"
SCHEMA_CASH_ATTR = '#[serde(default, skip_serializing_if = "Option::is_none")]'


def backtest_account_base_check() -> None:
    """回测的账户本金：一处常数、一条读法、来源可见、非正声明当场拒。"""
    account_base = (ROOT / ACCOUNT_BASE_MODULE).read_text(encoding="utf-8")
    bar_chain = (ROOT / BAR_CHAIN_FILE).read_text(encoding="utf-8")
    depth_chain = (ROOT / DEPTH_CHAIN_FILE).read_text(encoding="utf-8")
    leg_funding = (ROOT / LEG_FUNDING_FILE).read_text(encoding="utf-8")
    multi_leg = (ROOT / MULTI_LEG_CHAIN_FILE).read_text(encoding="utf-8")
    assembly = (ROOT / ASSEMBLY_MODULE).read_text(encoding="utf-8")
    summary = (ROOT / SUMMARY_MODULE).read_text(encoding="utf-8")
    schema = (ROOT / STRATEGY_SCHEMA).read_text(encoding="utf-8")
    unit_cases = (ROOT / ACCOUNT_BASE_CASES).read_text(encoding="utf-8")
    cli_cases = (ROOT / ACCOUNT_BASE_CLI_CASES).read_text(encoding="utf-8")
    chains = bar_chain + depth_chain

    # 1. 默认本金只许住在一个具名常数里。生产代码再抄一个 `100_000` 字面量，就等于让某条
    #    链绕过必答题——编译不会红，但它的分母又变成私下的了。
    literal_sites = []
    for path in sorted((CRATES / "qx-cli" / "src").rglob("*.rs")):
        if "tests" in path.relative_to(CRATES / "qx-cli" / "src").parts:
            continue
        lines = path.read_text(encoding="utf-8").splitlines()
        for index, line in enumerate(lines):
            if not re.search(r"from_i64\(100_?000\)", line):
                continue
            if is_test_scoped(index, lines):
                continue
            literal_sites.append(f"{path.name}:{index + 1}")
    check(
        account_base.count(ACCOUNT_BASE_DEFAULT_DECL) == 1
        and not literal_sites,
        "默认回测本金是 account_base.rs 里唯一一处具名常数，生产代码不得再抄 100,000 字面量（V11 Q72）",
        f"定义 {account_base.count(ACCOUNT_BASE_DEFAULT_DECL)} 处 / 字面量 {literal_sites or '无'}",
    )
    check(
        all(
            f'pub(crate) const {name}: &str = "{literal}";' in account_base
            for name, literal in zip(ACCOUNT_BASE_SOURCE_CONSTS, ACCOUNT_BASE_SOURCES)
        )
        and multi_leg.count(BACKTEST_ACCOUNT_BASE_FUNDING_USE) == 1
        and "builtin-default" not in multi_leg,
        "三种本金来源各有常量，多腿链只报自己的定资口径、不会冒充默认本金",
        f"来源常量 {[n for n in ACCOUNT_BASE_SOURCE_CONSTS if n not in account_base]}"
        f" / 多腿标注 {multi_leg.count(BACKTEST_ACCOUNT_BASE_FUNDING_USE)} 处",
    )
    # 2. 装配侧：本金是构造参数，字段私有 ⇒ 新链漏答不过编译，答完也不能在别处改写。
    check(
        assembly.count(ASSEMBLY_CASH_PARAM) == 2
        and "pub(crate) initial_cash" not in assembly
        and "Money::from_i64" not in assembly
        and assembly.count(".initial_cash =") == 0
        and sum(part.count(".initial_cash =") for part in (bar_chain, depth_chain, multi_leg)) == 0,
        "BarBacktestAssembly 的本金只能由构造参数给出：字段私有、装配处无默认、事后不可回写（V11 Q72）",
        f"参数位 {assembly.count(ASSEMBLY_CASH_PARAM)} 处 / 公开字段={'有' if 'pub(crate) initial_cash' in assembly else '无'}"
        f" / 装配默认={assembly.count('Money::from_i64')}",
    )
    check(
        bar_chain.count("backtest_initial_cash(") == 2
        and depth_chain.count("backtest_initial_cash(") == 1
        and bar_chain.count("config.strategy.initial_cash_raw") == 1
        and bar_chain.count("configured_initial_cash_raw(") == 1
        and depth_chain.count("configured_initial_cash_raw(") == 1
        and leg_funding.count("pub(crate) fn backtest_initial_cash(") == 0
        and account_base.count("pub(crate) fn backtest_initial_cash(") == 1,
        "三条单腿回测链各自读过那一格声明、再把本金折成本地数字，读法只有 account_base.rs 一处实现（V11 Q72）",
        f"Bar 链 {bar_chain.count('backtest_initial_cash(')}/{bar_chain.count('configured_initial_cash_raw(')}"
        f" / 深度 {depth_chain.count('backtest_initial_cash(')}/{depth_chain.count('configured_initial_cash_raw(')}",
    )
    # 3. 可见性：结果行之前先印本金，stdout 与摘要共用同一句写法（不是两处各排一次版）。
    check(
        all(chains.count(marker) == 1 for marker in ACCOUNT_BASE_PRINT_MARKERS)
        and chains.count("backtest_account_base_note(") == 3,
        "三条单腿链各印一行本金，且都经 backtest_account_base_note 排版（V11 Q72）",
        f"标记 {[m for m in ACCOUNT_BASE_PRINT_MARKERS if chains.count(m) != 1]}"
        f" / 印点 {chains.count('backtest_account_base_note(')} 处",
    )
    check(
        account_base.count("if raw <= 0") == 1
        and "不是正的本金" in account_base
        and account_base.count("Money::from_raw(raw)") == 1,
        "非正声明当场报错而不是回落默认，正数声明原样折成 Money（V11 Q72）",
        f"闸门 {account_base.count('if raw <= 0')} 处（期望 1）",
    )
    check(
        summary.count(SUMMARY_ACCOUNT_BLOCK) == 1
        and summary.count("input.account_base.cash.raw()") == 1
        and '"schema_version": 4' in summary,
        "摘要以 v4 落 account 块，期初本金与来源两格都取自真正记账的那一份（V11 Q72）",
        f"来源格 {summary.count(SUMMARY_ACCOUNT_BLOCK)} / 本金格 {summary.count('input.account_base.cash.raw()')}",
    )
    # 4. 配置面：省略时序列化字节不变，已 bless 的 config_fingerprint 才不会集体失真。
    check(
        schema.count(SCHEMA_CASH_FIELD) == 1
        and SCHEMA_CASH_ATTR in schema
        and schema.count("initial_cash_raw: None,") == 1,
        "strategy.initial_cash_raw 是 Option<i128> 定点数、省略时不落键，默认构造写 None（V11 Q72）",
        f"字段 {schema.count(SCHEMA_CASH_FIELD)} / 属性={'在' if SCHEMA_CASH_ATTR in schema else '缺'}",
    )
    check(
        leg_funding.count("pub(crate) fn reject_configured_initial_cash(") == 1
        and re.search(r'reject_configured_initial_cash\([^)]*\)\?;', multi_leg) is not None
        and multi_leg.count("reject_configured_initial_cash(") == 1,
        "两条腿各有本金的多腿链当场拒收那格单账户声明，拒绝理由住在共用读法里（V11 Q72）",
        f"定义 {leg_funding.count('pub(crate) fn reject_configured_initial_cash(')} 处"
        f" / 调用 {multi_leg.count('reject_configured_initial_cash(')} 处，其中带 ? 传播的"
        f" {len(re.findall(r'reject_configured_initial_cash[(][^)]*[)][?];', multi_leg))} 处（各期望 1）",
    )
    # 5. 用例面：本金改结果的这一条必须端到端跑过真实子进程。
    check(
        all(f"fn {name}(" in unit_cases for name in ACCOUNT_BASE_UNIT_TESTS)
        and all(f"fn {name}(" in cli_cases for name in ACCOUNT_BASE_CLI_TESTS),
        "Q72 用例在位：三条读法各一条单元，命令行侧默认/改结果/拒非正/摘要/多腿拒收各一条",
        f"缺单元 {[n for n in ACCOUNT_BASE_UNIT_TESTS if f'fn {n}(' not in unit_cases]}"
        f" / 缺命令行 {[n for n in ACCOUNT_BASE_CLI_TESTS if f'fn {n}(' not in cli_cases]}",
    )


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
SUBMIT_GATE_TEST = "crates/qx-execution/src/tests/venue_submit_contract.rs"
REPORT_FUNNEL_FILE = "crates/qx-execution/src/lib.rs"
SPEC_FUNNEL_DEFINITION = "crates/qx-core/src/trading.rs"
# 真实交易所回报的生产入口：一旦 worker 配了冻结规格，就必须把规格交给归约入口，
# 否则回报侧精度闸门形同虚设。新增生产回报路径时把文件加入本清单。
LIVE_REPORT_FILES = (
    "crates/qx-cli/src/venue_runtime/binance_stream_worker.rs",
    "crates/qx-cli/src/venue_runtime/ccxt_execution.rs",
    "crates/qx-cli/src/venue_runtime/ccxt_reconcile_worker.rs",
    "crates/qx-cli/src/spread.rs",
)
HEDGE_GATE_TEST = "crates/qx-execution/src/tests/recovery_and_replay.rs"


def venue_report_contract_check() -> None:
    """交易所回报侧的迟到/乱序与精度越界纪律只有一处谓词，且三家共用一份契约测试。

    V9 §8.3 第 8 项收口：越界或迟到的回报只能留下 `ReconcileRequired` 事实，绝不能
    伪造成交或改写终态。闸门刻意放在成交进入账本的**全部三个入口**：归约入口
    （`ingest_venue_events_with_pipeline`，所有生产用户流回报的漏斗）、普通提交同步回包
    （`PortExecutionService::submit`）与对冲补偿提交（`HedgeRecoveryWorker`）。
    V11 Q57 就是因为第三入口曾经独走一条路：同一笔越界成交走用户流被拦下、走补偿回包
    却直接入账，而且补偿腿只追加裸 `Fill`，账簿退回乘数 1。
    这里钉住"谓词唯一 + 两个提交入口共用同一道闸门 + 补偿路径带规格落库 + 生产路径都带
    规格 + 三家 fixture 共用断言"，而不是回测侧的 Ledger（放进 Ledger 会让实盘与回测分叉）。
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
    submit_start = funnel.find("    pub fn submit(")
    submit_end = funnel.find("\n    }\n", submit_start) if submit_start >= 0 else -1
    submit_body = funnel[submit_start:submit_end] if submit_start >= 0 and submit_end > submit_start else ""
    check(
        submit_body.count("= gate_submit_facts(") == 1
        and "violation.reason_tag()" in submit_body
        and "mark_reconcile(" in submit_body,
        "提交同步返回的成交也过同一精度闸门并转待对账（V11 Q56）",
        f"submit 体可定位={bool(submit_body)}，闸门调用 {submit_body.count('= gate_submit_facts(')} 处（期望 1）",
    )
    check(
        funnel.count("= gate_submit_facts(") == 2,
        "两道提交入口共用同一个提交闸门，不得有第三份内联校验（V11 Q57）",
        f"{REPORT_FUNNEL_FILE} 内 {funnel.count('= gate_submit_facts(')} 处（期望 2）",
    )
    hedge_start = funnel.find("    pub fn execute_with_validator(")
    hedge_end = funnel.find("\n    }\n", hedge_start) if hedge_start >= 0 else -1
    hedge_body = funnel[hedge_start:hedge_end] if hedge_start >= 0 and hedge_end > hedge_start else ""
    append_start = funnel.find("    fn append_fact(")
    append_end = funnel.find("\n    }\n", append_start) if append_start >= 0 else -1
    append_body = funnel[append_start:append_end] if append_start >= 0 and append_end > append_start else ""
    check(
        hedge_body.count("= gate_submit_facts(") == 1
        and "instrument_spec(&order)" in hedge_body
        and "ReconcileRequired" in hedge_body,
        "对冲补偿提交过同一道闸门，且规格解析失败时拒绝补偿并转待对账（V11 Q57）",
        f"worker 体可定位={bool(hedge_body)}，闸门调用 {hedge_body.count('= gate_submit_facts(')} 处（期望 1）",
    )
    check(
        "ExecutionEvent::FillWithSpec" in append_body
        and "spec.clone()" in append_body,
        "补偿成交落库时带上冻结规格，衍生品腿不得退回乘数 1 记账（V11 Q57）",
        f"append_fact 体可定位={bool(append_body)}",
    )
    check(
        funnel.count("validate_fill(") == 2,
        "精度闸门调用点恰为两处：归约入口 + 共用提交闸门（不得有第三处）",
        f"{REPORT_FUNNEL_FILE} 内 {funnel.count('validate_fill(')} 处（期望 2）",
    )
    check(
        funnel.count('"invalid-submit-response"') == 1
        and funnel.count('"submit-fill-out-of-spec"') == 1,
        "提交未知的两类原因段各只有一处定义（放在共用闸门的 reason_tag 里）",
        f"形状 {funnel.count(chr(34) + 'invalid-submit-response' + chr(34))} 处、"
        f"精度 {funnel.count(chr(34) + 'submit-fill-out-of-spec' + chr(34))} 处（期望各 1）",
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
        "回报精度判定只有一个谓词（不在 Ledger/回测侧重复判定，调用点只允许归约入口与提交闸门）",
        f"定义 {predicates} 处；额外调用点 {copies or '无'}",
    )
    submit_test = (ROOT / SUBMIT_GATE_TEST).read_text(encoding="utf-8")
    check(
        "submit_returned_fills_share_the_precision_gate" in submit_test
        and '"embedded-spec-wins-over-frozen-spec"' in submit_test
        and 'vec!["reconcile"]' in submit_test,
        "存在「提交同步回报越界 → 只留一条待对账事实」的行为用例（V11 Q56 证据）",
        f"用例文件 {SUBMIT_GATE_TEST} 缺三形状之一",
    )
    hedge_test = (ROOT / HEDGE_GATE_TEST).read_text(encoding="utf-8")
    check(
        "hedge_compensation_submit_shares_the_submit_precision_gate" in hedge_test
        and '"off-tick-sync-fill-reconciles"' in hedge_test
        and '"spec-resolution-failure-blocks-submit"' in hedge_test
        and '"accepted", "fill-with-spec"' in hedge_test,
        "存在「补偿回包越界只留待对账 / 在 tick 上带规格落库 / 规格解析失败不提交」的行为用例（V11 Q57 证据）",
        f"用例文件 {HEDGE_GATE_TEST} 缺四形状之一",
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
        "PositionSnapshot": ["crates/qx-protocol/src/wire.rs"],
        # K 线：撮合与回测用的引擎 Bar（列式数据集到它只有一次投影）+ 数据集侧逐条记录。
        "Bar": ["crates/qx-data/src/schema.rs", "crates/qx-guanxing/src/lib.rs"],
        # 订单意图：SDK 原生 / 跨语言 JSON 契约 / C ABI 镜像 / 风控前的下单意图。
        "StrategyOrderIntent": ["crates/qx-strategy/src/lib.rs"],
        "StrategyContractIntent": ["crates/qx-runtime/src/strategy_contract/contract.rs"],
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


# 原 `qx-runtime/src/lib.rs` 拆分后的全部落点（V10 P2c）；配置面形状断言按此集合取源码。
RUNTIME_CONFIG_TEXT = surface_text(
    (
        "crates/qx-runtime/src/lib.rs",
        "crates/qx-runtime/src/runtime_config",
        "crates/qx-runtime/src/strategy_contract",
        "crates/qx-runtime/src/supervision",
    )
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
    lib = RUNTIME_CONFIG_TEXT
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


# P1c 的两个单点（V10 §4.9）：文件状态信封与重试退避策略。此处只查"定义点唯一"，
# 因为重复实现的危害方式是"下一次改动只改了其中一份"——用例能钉住当下，钉不住未来
# 出现的第二份；把定义点收在一张清单里才让"另起一份"当场变红。
STORAGE_ENVELOPE = "crates/qx-storage/src/state_envelope.rs"
RETRY_KERNEL = "crates/qx-core/src/retry.rs"

# 信封必须独占的能力：序列化/校验/事务/原子替换/追加锁。
ENVELOPE_PRIMITIVES = (
    "encode_state_json",
    "decode_state_json",
    "write_state_json",
    "transact_state_json",
    "write_atomic_path",
    "acquire_storage_lock",
)

# 借用统一策略的三方（连接器重连 / 调度器重试 / 存储尝试计数）。
RETRY_CONSUMERS = (
    "crates/qx-adapter/src/binance.rs",
    "crates/qx-scheduler/src/retry_policy.rs",
    "crates/qx-storage/src/lib.rs",
)

# 就地重算退避的指纹：位移形式的指数退避算式。薄封装（如连接器的 `delay_for` 只转发给
# `retry::Backoff`）是期望形状，因此不禁止函数名本身，只禁止"算式搬到消费者手里"。
RETRY_REIMPLEMENTATION = (re.compile(r"1_u128\s*<<|1u128\s*<<|1_u64\s*<<|1u64\s*<<"),)


def non_test_source(source: str) -> str:
    """截掉 `#[cfg(test)]` 之后的部分：门禁只约束生产代码，用例手写盘是合法现场。"""
    index = source.find("#[cfg(test)]")
    return source if index < 0 else source[:index]


def is_cli_case_file(path: Path) -> bool:
    """`qx-cli/src/tests/` 下的主题文件整体就是用例现场（挂在 `cfg(test)` 模块里，
    文件本身不带标记），因此按路径认领，口径同 V10 P1b 的提交入口清单。"""
    relative = path.relative_to(CRATES / "qx-cli" / "src")
    return relative.parts[0] == "tests" or "test" in path.stem


def module_declarations(path: Path) -> set[str]:
    """一个文件里写下的 `mod 名字;` 集合（含 `pub` / `pub(crate)` 变体）。"""
    return set(
        re.findall(
            r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;",
            path.read_text(encoding="utf-8"),
            re.MULTILINE,
        )
    )


def module_mount_check() -> None:
    """`crates/*/src` 下不得存在未挂载的 `.rs`（V10 P1c 期间真实踩到的失效形态）。

    拆分进行到一半时，新目录模块与 crate 根的内联实现会**同时存在**：新那份没人 `mod`，
    于是既不参与编译、也不被任何测试覆盖，而旧那份继续是唯一的生产实现。`cargo check`
    对这种"多出一份死源码"完全无感，只有按挂载关系清点才能发现——本轮就是这么抓到
    `qx-storage/src/file/` 与 lib.rs 内联四份 store 并存的。
    """
    unmounted: list[str] = []
    for crate in sorted(path for path in CRATES.iterdir() if path.is_dir()):
        source_root = crate / "src"
        if not source_root.is_dir():
            continue
        directories: dict[Path, list[Path]] = {}
        for path in source_root.rglob("*.rs"):
            if "bin" in path.relative_to(source_root).parts:
                continue  # src/bin/*.rs 由 cargo 自动发现为二进制目标
            directories.setdefault(path.parent, []).append(path)
        declared: dict[Path, set[str]] = {}
        for directory, files in directories.items():
            names: set[str] = set()
            for path in files:
                names.update(
                    re.findall(
                        r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;",
                        path.read_text(encoding="utf-8"),
                        re.MULTILINE,
                    )
                )
            declared[directory] = names
        for directory, files in directories.items():
            names_here = {path.name for path in files}
            # 2018 版式允许 `src/ashare.rs` + `src/ashare/json.rs`：子文件的声明写在
            # 同名的父文件里，而不是目录内的 mod.rs，所以认领范围要并上这一份。
            sidecar = directory.parent / f"{directory.name}.rs"
            mounted = set(declared[directory])
            if sidecar.is_file():
                mounted |= module_declarations(sidecar)
            for path in files:
                if path.stem in ("lib", "main", "build", "mod"):
                    continue
                if path.stem not in mounted:
                    unmounted.append(path.relative_to(ROOT).as_posix())
            if (
                directory != source_root
                and "mod.rs" in names_here
                and directory.name not in declared[directory.parent]
            ):
                unmounted.append((directory / "mod.rs").relative_to(ROOT).as_posix())
    check(
        not unmounted,
        "crate 源码树每个 .rs 都被某处 mod 挂载（未挂载即死源码，编译与用例都不覆盖）",
        f"未挂载 {unmounted}",
    )


def storage_retry_check() -> None:
    """存储写路径与重试退避各只有一个定义点（V10 §6 P1c / §7.2）。"""
    storage_files = sorted(CRATES.glob("qx-storage/src/**/*.rs"))
    storage_text = {
        path.relative_to(ROOT).as_posix(): non_test_source(
            path.read_text(encoding="utf-8")
        )
        for path in storage_files
    }
    # (a) 信封原语的定义点唯一，且全部住在 state_envelope.rs。泛型签名带 `<T>`，
    #     因此按 `fn 名字` 后接 `(` 或 `<` 认领，而不是要求紧跟左括号。
    for name in ENVELOPE_PRIMITIVES:
        definition = re.compile(rf"\bfn {name}\s*[<(]")
        definitions = sorted(
            rel for rel, source in storage_text.items() if definition.search(source)
        )
        check(
            definitions == [STORAGE_ENVELOPE],
            f"存储信封原语 {name} 只有一个定义点",
            f"定义于 {definitions}",
        )
    # (b) 落盘动作只有一个出口：信封之外的生产代码不得直接 std::fs::write，
    #     否则就绕过了"临时文件 + fsync + 原子改名"的崩溃安全边界。
    direct_writes = sorted(
        rel
        for rel, source in storage_text.items()
        if rel != STORAGE_ENVELOPE and "std::fs::write(" in source
    )
    check(
        not direct_writes,
        "qx-storage 生产代码只有信封一处落盘（绕过即失去原子替换）",
        f"直接写文件于 {direct_writes}",
    )
    # (c) 退避形状与延迟算式只住在 qx-core::retry：热路径不读时钟，时间由调用方注入，
    #     这样回测与故障注入能确定性复现同一条重连序列。
    kernel = (ROOT / RETRY_KERNEL).read_text(encoding="utf-8")
    enum_sites = [
        path.relative_to(ROOT).as_posix()
        for path in sorted(CRATES.glob("*/**/*.rs"))
        if re.search(r"^\s*pub enum Backoff \{$", path.read_text(encoding="utf-8"), re.MULTILINE)
    ]
    check(
        enum_sites == [RETRY_KERNEL],
        "退避形状 Backoff 只有一个定义点",
        f"定义于 {enum_sites}",
    )
    check(
        "Instant::now" not in kernel and "SystemTime" not in kernel,
        "退避策略为纯函数、不读系统时钟",
        "qx-core/src/retry.rs 出现了时钟读取",
    )
    # (d) 三方消费者必须引用统一策略，且不得就地保留退避算式。
    for rel in RETRY_CONSUMERS:
        source = non_test_source((ROOT / rel).read_text(encoding="utf-8"))
        fingerprints = [p.pattern for p in RETRY_REIMPLEMENTATION if p.search(source)]
        check(
            "retry::" in source and not fingerprints,
            f"{Path(rel).name} 的重试判定委托 qx-core::retry",
            f"引用统一策略={'是' if 'retry::' in source else '否'}；"
            f"就地退避算式 {fingerprints or '无'}",
        )
    # (e) 四个文件状态存储各自只有一份定义，且住在 `src/file/`。P1c 真实踩到的形态是
    #     "拆出来的那份没人 mod、lib.rs 里内联那份继续当唯一实现"——两份同时存在而编译无感，
    #     所以这里既数定义点个数，也钉住它所在的文件，拆完不允许退回内联。
    store_homes = {
        "JsonStateStore": "crates/qx-storage/src/file/state.rs",
        "FileConsumerStateStore": "crates/qx-storage/src/file/consumers.rs",
        "FileOutboxStore": "crates/qx-storage/src/file/outbox.rs",
        "FileJobQueue": "crates/qx-storage/src/file/jobs.rs",
    }
    for store, home in sorted(store_homes.items()):
        definitions = sorted(
            rel
            for rel, source in storage_text.items()
            if re.search(rf"^\s*pub struct {store}\b", source, re.MULTILINE)
        )
        check(
            definitions == [home],
            f"文件状态存储 {store} 只有目录模块里的一份定义",
            f"定义于 {definitions}，期望 {home}",
        )



# V11 Q0a：执行平面的成本口径只有一个定义点，且生产 Paper 不得回落到零费。
# V11 Q0c：该定义点必须**读配置**，且成本规则文件的读者全仓唯一。
EXECUTION_FEE_MODEL_FILE = "crates/qx-cli/src/runtime_wiring.rs"
FEE_KERNEL_FILE = "crates/qx-core/src/fee.rs"
COST_RULES_FILE = "crates/qx-xingban/src/cost_rules.rs"
COST_RULES_TEMPLATE = "deploy/qianxing.costs.example.json"
PAPER_FEE_TEST_FILE = "crates/qx-execution/tests/paper_accounting.rs"
COST_PROVENANCE_TEST_FILE = "crates/qx-cli/src/tests/backtest_cost_provenance.rs"
STRATEGY_SCHEMA_FILE = "crates/qx-runtime/src/runtime_config/strategy_schema.rs"
BAR_ASSEMBLY_FILE = "crates/qx-cli/src/backtests/mod.rs"
DEPTH_BACKTEST_FILE = "crates/qx-cli/src/backtests/depth.rs"
RUNTIME_CHECK_FILE = "crates/qx-cli/src/runtime_check.rs"
EXECUTION_FEE_MODEL_DEF = re.compile(
    r"^pub\(crate\) fn (?:default_)?execution_cost_binding(?:_from_config)?\(", re.MULTILINE
)
COST_RULES_LOAD_CALL = re.compile(r"ExecutionCostRules::load\(")
PAPER_COST_CONSTRUCTION = re.compile(
    r"(?<![A-Za-z0-9_])(?:PaperVenue::new|execute_paper_submit_effect)\s*(?P<paren>\()"
)
# 默认费率常数的**定义**形状（`pub const DEFAULT_*_BP: i64 = …`）；再导出不算第二处定义。
FEE_CONST_DEF = re.compile(r"^pub const DEFAULT_(?:MAKER|TAKER)_BP:", re.MULTILINE)


def paper_fee_same_source_check() -> None:
    """V11 §4.1 的缺陷形状：`PaperVenue` 自带零费默认、`with_fee_model` 只有单元测试调用，
    于是生产 Paper 的 `Fill.fee` 恒为 0，Paper 曲线系统性优于同输入回测。

    收口后的口径有四条，每条都靠"抽掉它就变红"钉住：默认费率常数的定义点唯一在内核
    （`qx-xingban::cost_rules` 只做再导出）；CLI 侧成本口径由 `execution_cost_binding*`
    单点提供，且它是成本规则文件在全仓的唯一读者；qx-cli 生产代码的每个 Paper 构造点
    都显式给出费用模型且不出现 `ZeroFeeModel`；Bar 回测装配的 `fee` 与 `latency` 成对取自
    同一份绑定，并存在把费用真的记进 Ledger 的行为用例。

    Q0c 加的两条：`strategy.cost_rules_path` 必须既有 schema 声明、又被 `config validate`
    覆盖（否则是"宣称能配、无人校验"）；深度档的三层优先级必须落在成本绑定上，分派层
    不得再自己给出费率缺省值（Q0b 的 `qx_core::DEFAULT_TAKER_BP` 引用在合并后回到分派层
    也会在这里变红）。
    """
    const_defs = [
        path.relative_to(ROOT).as_posix()
        for path in sorted(CRATES.glob("*/src/**/*.rs"))
        if FEE_CONST_DEF.search(path.read_text(encoding="utf-8"))
    ]
    check(
        const_defs == [FEE_KERNEL_FILE],
        "默认 maker/taker 费率常数只在 qx-core::fee 定义（再导出不算第二处）",
        f"定义于 {const_defs}",
    )
    reexport = (ROOT / COST_RULES_FILE).read_text(encoding="utf-8")
    check(
        "pub use qx_core::fee::{DEFAULT_MAKER_BP, DEFAULT_TAKER_BP}" in reexport,
        "cost_rules 只再导出费率常数，不重复定义默认值",
        "缺少对 qx_core::fee 的再导出",
    )
    definitions = [
        path.relative_to(ROOT).as_posix()
        for path in sorted(CRATES.glob("qx-cli/src/**/*.rs"))
        if not is_cli_case_file(path)
        and EXECUTION_FEE_MODEL_DEF.search(path.read_text(encoding="utf-8"))
    ]
    check(
        definitions == [EXECUTION_FEE_MODEL_FILE],
        "执行成本口径只有一个构造点（execution_cost_binding*）",
        f"定义于 {definitions}",
    )
    # Q0c：成本规则文件必须有唯一读者。第二处 `ExecutionCostRules::load` 意味着
    # 校验与装配开始各读各的，那正是"配置写着生效、跑起来用的是默认值"的分裂形状。
    # 用例直接调用加载器是被测现场，与 `non_test_source` 同一口径豁免。
    loaders = [
        path.relative_to(ROOT).as_posix()
        for path in sorted(CRATES.glob("qx-cli/src/**/*.rs"))
        if not is_cli_case_file(path)
        and COST_RULES_LOAD_CALL.search(non_test_source(path.read_text(encoding="utf-8")))
    ]
    check(
        loaders == [EXECUTION_FEE_MODEL_FILE],
        "成本规则文件的读者全仓唯一（runtime_wiring）",
        f"出现于 {loaders}",
    )
    check(
        (ROOT / COST_RULES_TEMPLATE).is_file(),
        "成本规则模板随仓库发布（配置面的可复制起点）",
        f"缺少 {COST_RULES_TEMPLATE}",
    )
    missing: list[str] = []
    zero_fee: list[str] = []
    for path in sorted(CRATES.glob("qx-cli/src/**/*.rs")):
        # 用例文件里的 Paper 构造是被测现场，允许显式零费；口径同 V10 P1b 的提交入口清单。
        if is_cli_case_file(path):
            continue
        source = path.read_text(encoding="utf-8")
        location = path.relative_to(ROOT).as_posix()
        for match in PAPER_COST_CONSTRUCTION.finditer(source):
            args = balanced_args(source, match.start("paren"))
            # 形参声明里的 `fee_model: Box<dyn FeeModel + Send>` 同样算显式回答。
            if "fee_model" not in args and "execution_fee_model()" not in args:
                missing.append(
                    f"{location}:{source[: match.start()].count(chr(10)) + 1}"
                    f" {match.group(0).strip()} -> {args[:50]}"
                )
        if re.search(r"\bZeroFeeModel\b", source):
            zero_fee.append(location)
    check(
        not missing,
        "qx-cli 每个 Paper 构造点都显式给出费用模型",
        f"未给出 {missing or '无'}",
    )
    check(
        not zero_fee,
        "qx-cli 生产代码不得以零费构造 Paper（零费只允许出现在用例里）",
        f"出现于 {zero_fee or '无'}",
    )
    assembly = (ROOT / BAR_ASSEMBLY_FILE).read_text(encoding="utf-8")
    check(
        re.search(r"^\s*fee: costs\.fee_model\(\),$", assembly, re.MULTILINE) is not None
        and re.search(r"^\s*latency: costs\.latency_model\(\),$", assembly, re.MULTILINE)
        is not None,
        "Bar 回测装配的费用与延迟成对来自同一份成本绑定（Q0a 同源 + Q0c 来自配置）",
        "backtests/mod.rs 的 fee/latency 字段未取自 ExecutionCostBinding",
    )
    check(
        "ZeroLatency" not in assembly,
        "Bar 回测装配不再写死零延迟（零延迟只能由成本绑定推导）",
        "backtests/mod.rs 仍出现 ZeroLatency 字面量",
    )
    behavior = (ROOT / PAPER_FEE_TEST_FILE).read_text(encoding="utf-8")
    check(
        "fn paper_spot_fill_charges_the_shared_fee_model_into_the_ledger" in behavior,
        "存在把 Paper 费用记进 Ledger 的可执行用例（Q0a 的行为面证据）",
        f"缺少 {PAPER_FEE_TEST_FILE} 中的费用记账用例",
    )
    # Q0b→Q0c：深度档的缺省费率从"cli.rs 里的游离字面量"下沉到成本绑定。分派只负责
    # 把 `Option<i64>` 原样传下去，三层优先级（旗标 > 成本规则 > 内核默认）在深度入口判定。
    dispatch = (ROOT / CLI_DISPATCH_FILE).read_text(encoding="utf-8")
    depth = (ROOT / DEPTH_BACKTEST_FILE).read_text(encoding="utf-8")
    check(
        "fee_bps.unwrap_or(qx_core::DEFAULT_TAKER_BP)" not in dispatch,
        "深度档缺省费率不再由分派层写死",
        "cli.rs 仍在分派处给出费率缺省值",
    )
    check(
        "fee_bps.unwrap_or(costs.rules.taker_bp)" in depth and "fee_bps: Option<i64>" in depth,
        "深度档缺省费率取自成本绑定（三层优先级在入口落地）",
        "backtests/depth.rs 未按 `Option<i64>` 形参向 ExecutionCostBinding 要缺省值",
    )
    # Q0c：配置面必须"配了就生效"，且 `config validate` 与装配共用同一份校验。
    schema = (ROOT / STRATEGY_SCHEMA_FILE).read_text(encoding="utf-8")
    validation = (ROOT / RUNTIME_CHECK_FILE).read_text(encoding="utf-8")
    check(
        "pub cost_rules_path: Option<String>," in schema,
        "运行时配置声明 strategy.cost_rules_path",
        f"{STRATEGY_SCHEMA_FILE} 缺少该字段",
    )
    check(
        "cost_rules_problem(" in validation,
        "config validate 覆盖 cost_rules_path（与装配同一个读者）",
        f"{RUNTIME_CHECK_FILE} 未调用 cost_rules_problem",
    )
    provenance = (ROOT / COST_PROVENANCE_TEST_FILE).read_text(encoding="utf-8")
    check(
        "fn cost_rules_file_changes_backtest_fees" in provenance,
        "存在「改配置里的 taker_bp → 回测手续费产物随之变化」的行为用例（Q0c 证据）",
        f"缺少 {COST_PROVENANCE_TEST_FILE} 中的成本驱动用例",
    )


# V11 Q0d：撮合内核的表述必须与真实消费者一致。
ORDERBOOK_KERNEL_FILE = "crates/qx-xingban/src/orderbook.rs"
TICK_BACKTEST_FILE = "crates/qx-xingban/src/tick_backtest.rs"
PAPER_VENUE_FILE = "crates/qx-zhenlu/src/lib.rs"
USER_FACING_READMES = ("README.md", "deploy/README.md")
# paper 与回测目前真正共享的执行平面符号；"同源"类表述只能落在这几个名字上。
SHARED_EXECUTION_SYMBOLS = (
    "FeeModel",
    "apply_fill_to_books",
    "apply_ledger_fill",
    "Ledger",
    "Oms",
)
MULTI_LEG_KIND_NAMES = (
    "pairs_arbitrage",
    "basis_arbitrage",
    "cross_venue_arbitrage",
    "spot_futures_arbitrage",
)
PAPER_TOKEN = re.compile(r"Paper|paper|模拟盘")
# 只认领"撮合/簿/内核"这一类同源性说法；`回测与实盘共享同一规则内核`（README）是
# 规则口径，归 Q0a/Q0c 的费用与风控同源检查管，不在这里判。
KERNEL_CLAIM_TOKEN = re.compile(r"(?:共用|共享|同一|都走|复用)[^。\n]{0,14}(?:内核|撮合|订单簿)")
KERNEL_CLAIM_NEGATION = re.compile(
    r"不成立|不得|并非|不是|没有|不再|不走|谎称|未实现|避免|区别|差异|必须写明"
)
SENTENCE_SPLIT = re.compile(r"[。；;\n]")


def kernel_claim_check() -> None:
    """V11 §4.3 的表述缺陷：`orderbook.rs` 曾称"可被历史 Tick 回放、Paper 模拟和性能基准
    共同使用"，而 paper 的成交实际由 `PaperVenue::on_quote` 用首档一次性 touch 产生，
    与簿内核没有任何符号耦合。Q0d 只改表述、不改行为（改行为是 §9 登记的 Q1d）。

    六条判据各自抽掉就变红：paper 侧不得出现簿内核符号（负向事实）、`on_quote` 必须真的
    存在（文档指向不落空）、Tick 链必须确实复用 L2 引擎（正向事实）、簿内核文档必须点名两处
    真实消费者并写明 paper 首档 touch + Q1d 排期，最后是全局连坐——任何把 Paper 与"同一撮合
    /共用内核"写进同一句、又没引用真实共享符号的表述一律红。

    Q1d 落地（paper 改走簿内核）时必须**同时**改掉第 1 条与第 4/5 条的期望，二者是一对，
    否则检查会变成把旧表述钉死的化石。
    """
    zhenlu = {
        # 整份文件都扫，不能用 `non_test_source`：zhenlu 的 `lib.rs:16` 就挂着一条
        # `#[cfg(test)] use`，截断后只剩 15 行，`PaperVenue` 本体反而扫不到。
        # 同时只看代码耦合——注释里出现 OrderBook 是合法的如实表述，文字表述归第 6 条判。
        path.relative_to(ROOT).as_posix(): re.sub(
            r"//.*", "", path.read_text(encoding="utf-8")
        )
        for path in sorted(CRATES.glob("qx-zhenlu/src/**/*.rs"))
    }
    coupled = [
        location
        for location, source in zhenlu.items()
        if re.search(r"\bOrderBook|\bBookLevel", source)
    ]
    check(
        not coupled,
        "Paper 侧（qx-zhenlu）不引用逐档簿内核符号（成交由首档 touch 产生）",
        f"出现了簿内核引用 {coupled or '无'}",
    )
    venue = (ROOT / PAPER_VENUE_FILE).read_text(encoding="utf-8")
    check(
        re.search(r"impl PaperVenue \{", venue) is not None
        and re.search(r"pub fn on_quote\(", venue) is not None,
        "文档指向的 paper 成交入口真实存在（PaperVenue::on_quote）",
        f"{PAPER_VENUE_FILE} 不再定义 on_quote",
    )
    tick = (ROOT / TICK_BACKTEST_FILE).read_text(encoding="utf-8")
    check(
        re.search(r"use crate::\{[^}]*OrderBookBacktestEngine", tick, re.DOTALL)
        is not None,
        "Tick 链确实复用 L2 的 OrderBookBacktestEngine（文档中的正向同源表述）",
        "tick_backtest.rs 不再引用 OrderBookBacktestEngine",
    )
    doc = (ROOT / ORDERBOOK_KERNEL_FILE).read_text(encoding="utf-8")
    doc_head = non_test_source(doc)
    check(
        "orderbook_backtest" in doc_head
        and "tick_backtest" in doc_head
        and "逐档" in doc_head,
        "簿内核文档点名真实消费者（L2 深度回测 + L1 Tick 回测）",
        "orderbook.rs 模块文档缺少消费者",
    )
    check(
        "PaperVenue::on_quote" in doc_head
        and "首档" in doc_head
        and "Q1d" in doc_head,
        "簿内核文档写明 paper 走首档 touch 并挂上 Q1d 排期",
        "orderbook.rs 模块文档缺少 paper 口径或 Q1d 指向",
    )
    offenders: list[str] = []
    scopes: list[tuple[str, str]] = [
        (
            path.relative_to(ROOT).as_posix(),
            non_test_source(path.read_text(encoding="utf-8")),
        )
        for path in sorted(CRATES.glob("*/src/**/*.rs"))
        if "tests" not in path.parts and "test" not in path.stem
    ] + [
        (rel, (ROOT / rel).read_text(encoding="utf-8")) for rel in USER_FACING_READMES
    ]
    for location, source in scopes:
        for sentence in SENTENCE_SPLIT.split(source):
            if not (
                PAPER_TOKEN.search(sentence) and KERNEL_CLAIM_TOKEN.search(sentence)
            ) or KERNEL_CLAIM_NEGATION.search(sentence):
                continue
            if any(symbol in sentence for symbol in SHARED_EXECUTION_SYMBOLS):
                continue
            offenders.append(f"{location}: {sentence.strip()[:56]}")
    check(
        not offenders,
        "「Paper 与回测同一撮合内核」类表述必须同句引用真实共享符号",
        f"无据表述 {offenders or '无'}",
    )


def multi_leg_honesty_check() -> None:
    """V11 §6 Q0e：多腿归因只承认实际成交，一腿被挡时另一腿必须留下显式待对账事实。

    十五条判据各自抽掉就变红：配对口径必须只看 `filled_qty_raw`（回填成交）、裸腿必须
    被登记而不是静默计入某个组、单腿定资要逐腿按本腿行情帧与生效费率算、算不出来必须
    报错而不是截断到 `i64::MAX`（§4.18 的同一类失真）、诚实性事实必须同时出现在 stdout
    与产物里、四条 kind 的端到端用例齐备，最后是费用口径的偏离声明：
    `TradingInstrumentSpec` 没有 maker/taker 字段，两腿只能共用
    一份显式成本绑定，产物必须自己说明这一点，否则读者会以为每腿按各自 venue 费率计过。

    后五条属 V11 Q58：产品形态只有 market spec 说得了，缺 spec 的腿一律按现货乘数 1 记账，
    所以"衍生品才计提保证金/资金费"这件事必须有一道先于撮合的规格闸门、腿级的衍生品过滤、
    产物侧的口径披露，以及四种规格组合各自的行为用例。最后三条属 V11 Q71：组合收益只许
    按两条腿的钱算一次、产物要留下可复算的两端，而组级合计折叠只能住在归因内核里一处。
    """

    def top_level_fn(text: str, signature: str) -> str:
        start = text.index(signature)
        end = text.find("\n}" + "\n", start)
        return text[start:] if end < 0 else text[start:end]

    pairing_path = CRATES / "qx-cli/src/multi_leg.rs"
    pairing = pairing_path.read_text(encoding="utf-8")
    body = top_level_fn(pairing, "pub(crate) fn multi_leg_group_attributions(")
    check(
        "planned_qty_raw" not in body and "filled_qty_raw" in body,
        "多腿成组配对只看实际成交（不得用信号计划量配对）",
        f"{pairing_path.relative_to(ROOT).as_posix()} 的配对逻辑重新引用了计划量",
    )
    check(
        "pub(crate) struct MultiLegPendingReconcile" in pairing
        and "pending_reconcile.push(" in body,
        "一腿成交、对手腿落空时必须登记裸腿待对账事实",
        "multi_leg.rs 不再定义或写入 MultiLegPendingReconcile",
    )
    cash_path = CRATES / "qx-cli/src/backtests/leg_funding.rs"
    report_path = CRATES / "qx-cli/src/backtests/multi_builtin.rs"
    cash = cash_path.read_text(encoding="utf-8")
    report = report_path.read_text(encoding="utf-8")
    cash_body = top_level_fn(cash, "pub(crate) fn multi_leg_leg_cash(")
    check(
        "超出账户可表示上限" in cash_body
        and ".map_err(" in cash_body
        and cash_body.count("i64::MAX") == 1,
        "多腿单腿定资越界必须报错，可表示上限只允许出现在诊断文案里",
        f"{cash_path.relative_to(ROOT).as_posix()} 重新引入了静默截断定资",
    )
    check(
        all(
            token in report
            for token in (
                "multi_leg_leg_cash(quantity, &primary_bars, costs.rules.taker_bp",
                "multi_leg_leg_cash(quantity, &reference_bars, costs.rules.taker_bp",
            )
        ),
        "多腿定资逐腿取本腿行情帧与生效费率，不得共用全局口径",
        "multi_builtin.rs 的定资调用退回了单一口径",
    )
    check(
        all(
            token in report
            for token in (
                "[Multi-leg · Integrity]",
                "[Multi-leg · Reconcile]",
                "\"pending_reconcile\"",
                "\"legs\"",
                "裸腿事实与残余成交不闭合",
            )
        ),
        "多腿计划与成交的差距必须同时进 stdout 与产物，并有闭合守卫",
        "multi_builtin.rs 缺少诚实性导出或闭合守卫",
    )
    cases = (CRATES / "qx-cli/tests/multi_leg_attribution.rs").read_text(encoding="utf-8")
    check(
        all(kind in cases for kind in MULTI_LEG_KIND_NAMES)
        and "fn vetoed_leg_never_pairs_against_a_filled_counterpart" in cases
        and "fn multi_leg_funding_bound_fails_loudly_instead_of_capping_cash" in cases,
        "四条多腿 kind 与两个反向验证用例必须齐备",
        "multi_leg_attribution.rs 的用例覆盖不再咬住 Q0e 口径",
    )
    check(
        "market spec carries no maker/taker field" in report,
        "多腿产物必须声明两腿共用一份成本绑定（spec 无费率字段的既有偏离）",
        "multi_builtin.rs 的 assumptions 缺少费用口径偏离声明",
    )
    guard_body = top_level_fn(cash, "pub(crate) fn multi_leg_spec_guard(")
    check(
        "primary_spec.is_none()" in guard_body
        and "腿没有 market spec" in guard_body
        and "没有一条腿是衍生品" in guard_body,
        "多腿规格闸门必须对缺规格与全现货两种组合都报错",
        "leg_funding.rs 的 multi_leg_spec_guard 不再覆盖这两类非法组合",
    )
    check(
        "multi_leg_spec_guard(" in report
        and report.index("multi_leg_spec_guard(") < report.index("let primary_report = run_leg("),
        "规格闸门必须先于任何腿级撮合，而不是跑完再补声明",
        "multi_builtin.rs 的规格闸门落到了撮合之后",
    )
    funding_body = top_level_fn(pairing, "pub(crate) fn multi_leg_leg_buckets(")
    margin_body = top_level_fn(pairing, "pub(crate) fn multi_leg_leg_margin(")
    check(
        "spec.product.is_derivative()" in funding_body
        and "spec.product.is_derivative()" in margin_body
        and "return Ok(0)" in margin_body,
        "资金费与保证金只向 market spec 声明为衍生品的腿计提，其余腿记 0",
        "multi_leg.rs 的腿级计费重新接受了现货或无规格腿",
    )
    check(
        '"market_specs"' in report
        and '"margin_model"' in report
        and "any(|spec| spec.product.is_derivative())" in report,
        "产物必须写出每条腿规格的来源，并按规格真实推导 margin_model",
        "multi_builtin.rs 不再披露规格来源，或 margin_model 退回常量",
    )
    check(
        all(
            case in cases
            for case in (
                "fn funding_without_leg_spec_is_refused_before_matching",
                "fn funding_on_spot_only_legs_is_refused",
                "fn only_derivative_legs_bear_margin_and_funding",
                "fn declared_derivative_product_without_primary_spec_is_refused",
            )
        )
        and "none-no-derivative-leg-spec" in cases,
        "四种规格组合（缺规格/全现货/混合/只声明衍生品）必须各有行为用例",
        "multi_leg_attribution.rs 的用例覆盖不再咬住 Q58 口径",
    )
    # V11 Q71：组合收益口径。两条腿的本金各按本腿行情定资、天然不等，把两个腿级 `return_bps`
    # 平均念出来的是"给便宜腿和贵腿同样权重"的那个东西——它既不是组合收益率，也不是任何一条
    # 腿的收益率（仓库自带夹具在 quantity=100 下实测 -189bp 被念成 -102bp）。判据收三头：调用
    # 点只能调这一份实现、实现里合计本金非正必须报错（印 0 会把"没法度量"伪装成"不赚不赔"）、
    # 复算所需的两腿本金与期末权益必须同时落在产物里。
    # 实现放在 `leg_funding.rs`：它和 `multi_leg_leg_cash` 是同一件事的两端（先按本腿行情定
    # 资，再按那份本金称收益），拆到两处就等于"权重"这个口径又有了第二个答案。
    combined_body = top_level_fn(cash, "pub(crate) fn multi_leg_combined_return_bps(")
    check(
        "multi_leg_combined_return_bps([" in report
        and "i64::from(primary_report.return_bps)" not in report
        and "本金合计必须为正" in combined_body
        and "Ok(0)" not in combined_body
        and "unwrap_or" not in combined_body
        and combined_body.count("checked_add(initial_raw)") == 1,
        "多腿组合收益只有一处算法：合计盈亏 ÷ 合计本金，且算不出时报错而不是折成 0",
        f"调用点 {report.count('multi_leg_combined_return_bps([')} 处、"
        f"退回平均 {report.count('i64::from(primary_report.return_bps)')} 处、"
        f"折成零形状 {combined_body.count('Ok(0)') + combined_body.count('unwrap_or')} 处",
    )
    unit_cases = (CRATES / "qx-cli/src/tests/execution_and_multi_leg.rs").read_text(
        encoding="utf-8"
    )
    check(
        "fn multi_leg_combined_return_weights_each_leg_by_its_own_capital" in unit_cases
        and "fn combined_return_pools_both_legs_by_capital_instead_of_averaging_bps" in cases
        and "primary_final_equity_raw" in report
        and "reference_final_equity_raw" in report,
        "组合收益必须同时有加权/平均分叉的行为用例与可复算的产物两端",
        "Q71 的用例或产物两端缺一：本金加权用例、端到端复算用例、两腿期末权益落盘",
    )
    # 本轮把组级合计折叠从编排入口搬进归因内核（`multi_builtin.rs` 越过了 Phase 4s 的 500
    # 行兄弟模块线，而这段 fold 本来就是在合计内核自己的产物）。搬家要能被抓住：入口重新
    # 自己折一遍 loop 的话，产物 `totals` 与费用闭合守卫就会各读一份数。
    check(
        "multi_leg_group_totals(&groups)" in report
        and "total_fees_raw" not in report
        and top_level_fn(pairing, "pub(crate) fn multi_leg_group_totals(").count(
            "total_fees_raw"
        )
        == 1,
        "组级合计只在 multi_leg_group_totals 一处折叠，编排入口不得再各自 loop 一遍",
        f"入口重inline={report.count('total_fees_raw')} 处、"
        f"内核合计={top_level_fn(pairing, 'pub(crate) fn multi_leg_group_totals(').count('total_fees_raw')} 处",
    )


# V11 Q54：产品规格（market spec）的读法必须只有一处。回测链曾经只认 CCXT 归一化形状，
# 而 `init` 打印的首条回测命令带的却是冻结产品规格（`qianxing.binance.spot.spec.json`），
# 照文档操作的人第一条命令就撞 "CCXT market 缺少 base"；同一个文件的 worker 侧又能读它。
# 另一侧的失效更安静：CCXT 快照缺 `price_tick_raw` 时曾被兜底成 1（=1e-9），字段校验照过，
# 于是 `one_tick_slippage` 的一档和下单取整闸门双双变成"看不见的猜测"。
MARKET_SPEC_READER_FILE = "crates/qx-cli/src/market_spec.rs"
MARKET_SPEC_TEST_FILE = "crates/qx-cli/src/tests/market_spec_single_source.rs"
# 三项定点口径：同时是一档滑点、数量步长与最小下单量的来源，缺失必须报错而不是兜底。
MARKET_SPEC_PRECISION_FIELDS = ("price_tick", "qty_step", "min_qty")
# "另一个地方自己解析/自己构造一份产品规格"的几种形状。按去空白后的文本匹配，
# 否则 rustfmt 把 `let spec: TradingInstrumentSpec =\n serde_json::from_value(...)` 折行就漏判。
MARKET_SPEC_CONSTRUCTION = (
    re.compile(r"TradingInstrumentSpec\{"),
    re.compile(r"TradingInstrumentSpec=serde_json::from"),
    re.compile(r"from_(?:str|value)::<TradingInstrumentSpec"),
)


def market_spec_single_reader_check() -> None:
    """market spec 单一读法 + 精度口径不得从缺失字段兜底（V11 Q54）。"""
    elsewhere = []
    for path in sorted((CRATES / "qx-cli/src").rglob("*.rs")):
        relative = path.relative_to(ROOT).as_posix()
        if relative == MARKET_SPEC_READER_FILE or "/tests/" in f"/{relative}":
            continue
        flat = re.sub(r"\s+", "", path.read_text(encoding="utf-8"))
        if any(pattern.search(flat) for pattern in MARKET_SPEC_CONSTRUCTION):
            elsewhere.append(relative)
    check(
        not elsewhere,
        "产品规格只在 market_spec.rs 解析或构造，其余链路一律走 loader",
        f"另起一份解析 {elsewhere}",
    )
    reader = (ROOT / MARKET_SPEC_READER_FILE).read_text(encoding="utf-8")
    check(
        all(
            f'{field}: declared_raw_number(market, "{field}_raw"' in reader
            for field in MARKET_SPEC_PRECISION_FIELDS
        )
        and not re.search(r"unwrap_or\(\s*1\s*\)", reader),
        "三项定点精度必须显式声明，缺失即拒绝而非兜底",
        "market_spec.rs 的精度读法退回兜底",
    )
    check(
        'market.get("base_currency").is_some()' in reader,
        "market spec 的两种形状按 base_currency 判定，而非 try-parse 回落",
        "market_spec.rs 缺少形状判定，写错的规格会被误报成 CCXT 缺字段",
    )
    for chain, path in {
        "回测链": "crates/qx-cli/src/backtests/mod.rs",
        "worker 链": "crates/qx-cli/src/venue_runtime/worker_runtime.rs",
    }.items():
        check(
            "market_spec_from_value(" in (ROOT / path).read_text(encoding="utf-8"),
            f"{chain} 经 market_spec_from_value 读产品规格",
            f"{path} 未调用 loader",
        )
    cases = (ROOT / MARKET_SPEC_TEST_FILE).read_text(encoding="utf-8")
    check(
        all(
            name in cases
            for name in (
                "fn both_market_spec_shapes_resolve_to_the_same_spec",
                "fn an_under_specified_ccxt_market_is_refused_not_invented",
                "fn the_backtest_chain_reads_the_generated_frozen_spec",
            )
        ),
        "存在两种形状同源、缺精度即拒、回测链吃冻结规格的行为用例（V11 Q54 证据）",
        f"缺少 {MARKET_SPEC_TEST_FILE} 中的 Q54 用例",
    )


# V11 Q63：来源标签曾经只回答"有没有传 --market-spec"，于是仓库自己生成的那份产品规格
# （`qianxing.binance.spot.spec.json`，带 base_currency）在产物里被写成 `ccxt-market-spec-v1`。
# 规格内容不进 `model_fingerprint`，`contract_size` 与三项精度也都不进，所以这个字段是唯一
# 承载"这份规格按哪种形状读的"的地方——写错等于把两种不同的记账口径声明成同一种。
MARKET_SPEC_SOURCE_FILE = "crates/qx-cli/src/market_spec.rs"
# 把来源写进产物的两条链 + 那条只打印不落摘要的 builtin 链。
MARKET_SPEC_SOURCE_CHAINS = {
    "策略回测": "crates/qx-cli/src/backtests/single_strategy.rs",
    "深度回测": "crates/qx-cli/src/backtests/depth.rs",
}
MARKET_SPEC_SOURCE_CONSTANTS = (
    "CCXT_MARKET_SPEC_VERSION",
    "PRODUCT_MARKET_SPEC_VERSION",
    "DEFAULT_INSTRUMENT_SPEC_VERSION",
)


def _identity_block(text: str, anchor: str) -> str:
    """取 `RunManifestIdentity {` 到它自己那个右花括号之间的块（不含括号）。

    必须按块取，而不是在整个文件里找字段名：链上 `let MarketSpecLoad { ... source:
    instrument_spec_version, }` 的解构语句同样带着 `instrument_spec_version,`，按文件找
    子串会在"产物那一行改回常量"之后照样命中（V11 Q63 第一轮的门禁变异没咬住，就是这个原因）。
    """
    start = text.find(anchor)
    if start < 0:
        return ""
    depth = 0
    for index in range(start + len(anchor) - 1, len(text)):
        if text[index] == "{":
            depth += 1
        elif text[index] == "}":
            depth -= 1
            if depth == 0:
                return text[start:index]
    return ""


def market_spec_source_check() -> None:
    """规格来源标签必须由"实际读成的形状"决定，且只有 loader 那一个落点（V11 Q63）。"""
    reader = (ROOT / MARKET_SPEC_SOURCE_FILE).read_text(encoding="utf-8")

    def body_of(name: str) -> str:
        start = reader.find(f"fn {name}(")
        if start < 0:
            return ""
        end = reader.find("\n}\n", start)
        return reader[start:] if end < 0 else reader[start:end]

    predicate = body_of("market_value_is_product_spec")
    label = body_of("market_spec_source_label")
    check(
        reader.count("fn market_value_is_product_spec(") == 1
        and reader.count("fn market_spec_source_label(") == 1
        and "market_value_is_product_spec(market)" in label,
        "形状判定只有一处，来源标签向它问同一个问题",
        f"谓词定义 {reader.count('fn market_value_is_product_spec(')} 处、"
        f"标签定义 {reader.count('fn market_spec_source_label(')} 处，"
        "标签体未复用谓词（各自复述一遍迟早读成两个答案）",
    )
    check(
        'market.get("base_currency").is_some()' in predicate
        and reader.count('market.get("base_currency").is_some()') == 1
        and "base_currency" not in label,
        "形状问题只在谓词里问一次，标签不复述判据",
        f"谓词体 {predicate!r} / 标签体 {label!r} 与 base_currency 判据的分布不再唯一",
    )
    values = dict(
        (name, value)
        for name, value in re.findall(
            r"pub\(crate\) const (\w+): &str = \"([^\"]*)\";", reader
        )
        if name in MARKET_SPEC_SOURCE_CONSTANTS
    )
    check(
        len(values) == 3 and len(set(values.values())) == 3,
        "三档来源（CCXT 形状/产品形状/无规格）各自有名，且没有两档共用一个标签",
        f"常量取值 {values}",
    )
    calls = {
        path.relative_to(ROOT).as_posix(): path.read_text(encoding="utf-8").count(
            "market_spec_source_label("
        )
        for path in sorted((CRATES / "qx-cli/src").rglob("*.rs"))
        if "market_spec_source_label(" in path.read_text(encoding="utf-8")
    }
    check(
        calls == {MARKET_SPEC_SOURCE_FILE: 1, "crates/qx-cli/src/backtests/mod.rs": 1},
        "来源标签只在 loader 单点问一次：定义在 market_spec.rs、取用在 backtests/mod.rs",
        f"实际出现处 {calls}",
    )
    defaults = {
        path.relative_to(ROOT).as_posix(): path.read_text(encoding="utf-8").count(
            "DEFAULT_INSTRUMENT_SPEC_VERSION"
        )
        for path in sorted((CRATES / "qx-cli/src").rglob("*.rs"))
        if "DEFAULT_INSTRUMENT_SPEC_VERSION" in path.read_text(encoding="utf-8")
        and "/tests/" not in f"/{path.relative_to(ROOT).as_posix()}"
    }
    check(
        defaults == {
            MARKET_SPEC_SOURCE_FILE: 1,
            "crates/qx-cli/src/backtests/mod.rs": 1,
        },
        "没传规格那一档由 loader 独占，链上不得自带兜底标签",
        f"实际出现处 {defaults}",
    )
    for chain, path in MARKET_SPEC_SOURCE_CHAINS.items():
        production = (ROOT / path).read_text(encoding="utf-8").split("#[cfg(test)]")[0]
        block = _identity_block(production, "RunManifestIdentity {")
        check(
            re.search(r"^\s+instrument_spec_version,$", block, re.MULTILINE) is not None
            and "instrument_spec_version:" not in block
            and "if spec_path.is_some()" not in production,
            f"{chain} 的 identity 块里来源只可能是 loader 返回的那个变量",
            f"{path} 的 RunManifestIdentity 块不再只搬用 loader 值: {block[-160:]!r}",
        )
    printed = (ROOT / MARKET_SPEC_SOURCE_CHAINS["策略回测"]).read_text(encoding="utf-8")
    check(
        '"[Builtin · Execution] fill_model={} source={} spec_source={}"' in printed
        and "fill_model_name, fill_model_source, instrument_spec_version" in printed,
        "不落摘要的 builtin 链把同一个来源打印出来，占位符与实际参数一一对应",
        "single_strategy.rs 的 Execution 行不再披露或改印别处的规格来源",
    )
    cases = (ROOT / MARKET_SPEC_TEST_FILE).read_text(encoding="utf-8")

    def case_slice(signature: str) -> str:
        start = cases.find(signature)
        if start < 0:
            return ""
        end = cases.find("\n}\n", start)
        return cases[start:] if end < 0 else cases[start:end]

    helper = case_slice("fn published_spec_source(")
    strategy_case = case_slice("fn each_market_spec_shape_publishes_its_own_source_label(")
    depth_case = case_slice("fn the_depth_chain_publishes_the_shape_it_actually_read(")
    check(
        '"run_manifest"' in helper
        and "instrument_spec_version" in helper
        and all(
            entry in body
            for body in (strategy_case, depth_case)
            for entry in (
                "spec.exists().then_some(&spec)",
                "published_spec_source(&summary)",
                *MARKET_SPEC_SOURCE_CONSTANTS,
            )
        )
        and re.search(r"published\.len\(\)\s*,\s*3", strategy_case) is not None
        and re.search(r"published\.len\(\)\s*,\s*3", depth_case) is not None
        and "run_strategy_backtest(" in strategy_case
        and "run_depth_backtest(" in depth_case,
        "两条写来源的链各有经过真实入口、逐档核对产物标签的行为用例（V11 Q63 证据）",
        f"{MARKET_SPEC_TEST_FILE} 不再覆盖来源标签",
    )


# V11 Q55：同一个事实在两层各有一份表述——内核的 `needs_reference_leg()`（缺
# `reference_instrument` 即非法）与 CLI 的回测准入名单 `MULTI_LEG_KINDS`（只有
# `backtest multi-builtin` 收这四个 kind）。两份都是必要的第一手登记（一层不能反向依赖
# 另一层的形状），但它们必须逐项相等：只改一边就会出现"内核认为合法、入口却拒绝"或
# 相反的静默放行。这里按文本取两侧的 kind 名单做集合比对。
TWO_LEG_KINDS = {
    "PairsArbitrage",
    "BasisArbitrage",
    "CrossVenueArbitrage",
    "SpotFuturesArbitrage",
}
KERNEL_KIND_PREDICATE = "crates/qx-strategy/src/builtin.rs"
CLI_KIND_PARTITION = "crates/qx-cli/src/cli_help.rs"
# 取"从某个锚点起到下一个块结束"之间的片段，避免把同文件里的其它 kind 列表算进来。
ARBITRAGE_BLOCK_END = re.compile(r"\n(?:    (?:pub )?fn |;)")


def _kinds_between(text: str, anchor: str) -> set[str]:
    start = text.index(anchor)
    tail = text[start + len(anchor) :]
    end = ARBITRAGE_BLOCK_END.search(tail)
    body = tail[: end.start()] if end else tail
    return set(re.findall(r"::(\w*Arbitrage)\b", body))


def two_leg_partition_check() -> None:
    """双腿 kind 的内核谓词与 CLI 准入名单必须逐项相等（V11 Q55）。"""
    kernel = _kinds_between(
        (ROOT / KERNEL_KIND_PREDICATE).read_text(encoding="utf-8"),
        "pub const fn needs_reference_leg",
    )
    cli = _kinds_between(
        (ROOT / CLI_KIND_PARTITION).read_text(encoding="utf-8"),
        "const MULTI_LEG_KINDS",
    )
    check(
        kernel == TWO_LEG_KINDS and cli == TWO_LEG_KINDS,
        "双腿套利 kind 的内核谓词与 CLI 准入名单逐项相等",
        f"内核 {sorted(kernel)} / CLI {sorted(cli)} / 期望 {sorted(TWO_LEG_KINDS)}",
    )
    init_surface = (ROOT / "crates/qx-cli/src/init_project.rs").read_text(encoding="utf-8")
    check(
        "fn single_leg_builtin_strategy" in init_surface
        and "kind.needs_reference_leg()" in init_surface,
        "init 的两个单标的入口必须按内核谓词拒绝双腿 kind",
        "init_project.rs 缺少 single_leg_builtin_strategy 或其谓词调用",
    )


def main() -> int:
    if "--snapshot" in sys.argv:
        return write_line_budgets()
    removed_crates_check()
    python_interpreter_check()
    worker_diagnostics_check()
    cli_dispatch_check()
    cli_help_surface_check()
    cli_flag_honesty_check()
    test_module_shape_check()
    cli_root_module_check()
    cli_backtest_module_check()
    bare_risk_gate_check()
    execution_single_track_check()
    ashare_pit_check()
    ashare_limit_anchor_check()
    venue_report_contract_check()
    ashare_backtest_binding_check()
    ashare_submit_guard_check()
    builtin_signal_check()
    replay_kernel_check()
    input_provenance_check()
    ledger_kernel_split_check()
    concept_registry_check()
    storage_retry_check()
    module_mount_check()
    runtime_config_fail_closed_check()
    backtest_assembly_check()
    paper_fee_same_source_check()
    kernel_claim_check()
    multi_leg_honesty_check()
    market_spec_single_reader_check()
    market_spec_source_check()
    two_leg_partition_check()
    snapshot_money_honesty_check()
    position_money_honesty_check()
    reconcile_round_honesty_check()
    control_plane_honesty_check()
    api_surface_doc_check()
    account_snapshot_schema_check()
    calendar_fingerprint_caliper_check()
    c_abi_header_check()
    snapshot_json_table_check()
    bar_frame_contract_check()
    backtest_account_base_check()
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
