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
      `include_str!` 仓库里那份 schema，契约声明的键集合等于写侧产物，八个汇总钱字段全部可空
      （V11 Q70 之后权益也可能是"算不出"），`schema_version` 与协议名是 `from_json` 认的那一对，
      Python 侧的必填集合也等于契约的 required。
  22. 日历组件指纹两侧比同一份夹具（V11 R17 / T2）：Python 写侧产出文档与摘要、Rust 读侧重算，
      摘要在代码里没有第二份抄本，字段白名单与契约版本号逐项相等——旧格式文档 `sessions` 缺席
      必须读成"没有时段"，两侧一宽一严时 Python 登记的 bundle 会被 CLI 拒启。
  23. 合流不许把同一段判据接成两份（V12 §21 / #138）：`tools/*.py` 内不得出现逐字重复的连续
      10 行，每条判据的描述文本全仓只写一次。git 对"双方各自在同处追加"不产生冲突标记，而
      门禁没有编译器兜底——§20 那份重复判据是靠 `NameError` 崩在中途才暴露的，症状是退 1 却 0 条 FAIL。
  24. 本地构建路径自己就得跑架构门禁（V12 §22 / #139）：`build.bat` 与 `build.sh` 的第 [1/9] 步
      调用本脚本，失败当轮中止，且排在任何 `cargo` 之前。此前八步门禁里没有一步跑它，于是双击
      构建看到"全部完成"时，这四百多条不变量一条都没被证明过——只有 CI 证明过，而 CI 不是发布路径。
  25. 每个 `pub enum` 变体都要有人在生产代码里把它造出来（V12 §23 / #137）：生产者按**限定名**
      `Enum::Variant` 计数（`impl Enum` 花括号配平块体内的 `Self::Variant` 也算），读者侧剔掉
      `#[cfg(test)]` 条目与 `crates/*/src/tests/**` 整个目录。按裸名计数会假绿——实测
      `ServiceStatus::Degraded` 在生产文本里出现 14 次，于是同一份扫描看不见 `ConnectorState::Degraded`
      一次也没被赋过值。零生产者的变体要么删，要么进线格式允许清单，且清单每条都要自证：变体仍在册、
      所在枚举确实 derive 了 `Deserialize`（证据只认 `pub enum` 上方连续的 `#` 属性行，文件顶部那句
      `use serde::{Deserialize, …}` 不算）、且确实零生产者。
  26. "这一层没算"的账户钱字段名单只有一份，而且它由源码派生（V13 R1-A5）：协议里八个
      `Option<i128>` 钱字段减去生产文本真有赋值点的那些即为未算名单；契约的逐字段 description、
      能力矩阵的逐字段 limitation、接口文档点名的那一串、读侧 null 用例点名的集合四处都必须等于
      它。此前这条事实只活在一句合并命名里，四处口径谁改都不红；账户快照也允许用结构体字面量绕过
      `new()` 的"八格默认 None"，现在构造单点同样在册。

运行： python3 tools/check_architecture.py
刷新第 8 项的预算快照（改动后人工确认 diff）：
       python3 tools/check_architecture.py --snapshot
"""

from __future__ import annotations

import ast
import hashlib
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
# 判据 label 名册：`--snapshot` 把它摘成一份摘要写进机读快照，于是"条数没变、名字换了一个"
# 这类漂移也能被收尾那条判据看见（只数条数看不见重命名）。
labels: list[str] = []


def check(ok: bool, label: str, detail: str = "") -> None:
    global checks
    checks += 1
    labels.append(label)
    if ok:
        print(f"[PASS] {label}")
    else:
        failures.append(label if not detail else f"{label} — {detail}")
        print(f"[FAIL] {label}{' — ' + detail if detail else ''}")


def case_source(rel: str) -> str:
    """读取一份行为用例的文本：单文件，或 V12 D3 搬家后的目录模块（拼接全部子文件）。

    按目录取数是为了让"用例搬到哪"不再改变判据口径 —— 否则搬一次家就要改一次常量，
    而漏改的那一处会像 §12.1 预登记的那样红在 `read_text` 上，而不是红在行为上。
    """
    path = ROOT / rel
    if path.is_dir():
        return "\n".join(
            file.read_text(encoding="utf-8") for file in sorted(path.rglob("*.rs"))
        )
    return path.read_text(encoding="utf-8")


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
# V12 R1/R2/R3 共补 17 条（191 → 208 按磁盘实测：src/tests 146 → 157，crates/qx-cli/tests
# 45 → 51，后者是 R1 的报告读侧 4 条 + R3 的本金单源 2 条）。V12 D3 把集成用例计数改成递归，
# 地板口径第一次含到 `tests/<主题>/**` —— 搬家不减用例，新增用例也不能靠"换个目录"躲开地板。
# V12 §13.3 收口时再补 2 条（R1 读侧 `input_verified=` 那一格：命令行 1 条 + 排版单元 1 条；
# 那颗变异在上一版日志里"静态红、行为全绿"）。协议侧另补 1 条，不计入本地板：208 → 210。
# 两条线合流后按磁盘重测再抬，谁也不是历史值：上游 T/S 轮那批与本地 Q70/Q71/Q72 那批同场，
# 210 → 243（`^#[test]$` 实测：src/tests 191 全在顶层 + crates/qx-cli/tests 递归 52）。
# 同一轮 `EXECUTION_TEST_FLOOR` 也重测过：src/tests 15 + tests 递归 10 = 25，恰好仍是磁盘值。
# V12 §16 三遍清点后按磁盘重测：#120 的 outbox 常量绑定补了一条读回用例（243 → 244，
# `^#[test]$` 实测 src/tests 192 + crates/qx-cli/tests 递归 52）。本轮另加的 3 条 accept 停机
# 用例在 `crates/qx-api/tests`、5 条审计链用例在 `crates/qx-storage/tests`，各自由
# `API_ACCEPT_LOOP_CASES` / `CONTROL_AUDIT_CASES` 点名钉住在位，不进本地板。
# 244 → 262：V13 R1-A4 回合按磁盘重测（`^#[test]$` 实测 src/tests 210 + crates/qx-cli/tests
# 递归 52）。抬的比我这一轮加的 3 条多，是因为 V12 §16/#133/合流几轮各自加的用例没人回填这里
# —— 244 是 8 颗 venue 用例之前的历史值，与 §16 抓到的"两个历史值并存"是同一类漂移。
# 262 → 265：V13 R1-A6 回合按磁盘重测（`^#[test]$` 实测 src/tests 213 + 递归 52）。这 3 条就是
# 本轮新加的 deploy 模板读取覆盖用例（在册等 / 逐份真读 / 每类探针），没有历史欠账要补。
CLI_TEST_FLOOR = 265
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
# 25 -> 15：V13 R23 · P1-12 把 `crates/qx-execution/tests/` 的四份**跨 crate 契约**用例
# （paper_accounting / reconcile_port_contract / recovery_and_replay / venue_report_contract，
# 共 10 条）迁到 `crates/contract-tests/tests/`，以拆掉「qx-execution --dev--> qx-runtime」
# 这条压在正常边上的 dev 环。用例一条没删——它们仍在全仓地板里（`workspace_test_floor_check`
# 把新 crate 一并计入，总数不变），并由 `dev_dependency_cycle_check` 第 2 颗逐名钉在新位置。
# 本地板因此只覆盖 `src/tests` 的 15 条（与磁盘一致），不再是"目录 + 集成"两处之和。
EXECUTION_TEST_FLOOR = 15
EXECUTION_TEST_FIXTURES = (
    "PortState",
    "PortVenue",
    "NeverCalledVenue",
    "RejectingRisk",
    "PortRouter",
    "port_order",
)
# 全仓行为用例地板（V12 §16 第三遍）。`CLI_TEST_FLOOR` / `EXECUTION_TEST_FLOOR` 只钉得住两个 crate，
# 而 §16 的账目回合抓到的正是它们覆盖不到的那种失效：整树 `test result:` 计数从 843 掉到 821，
# 逐名比对才发现 39 条用例随死码删除或改名一起消失了 —— 没有下限的那些 crate 全绿通过。
# 地板取本回合磁盘实测的 `#[test]` 属性总数（`crates/*/src` + `crates/*/tests` 递归）。
# 851 → 853：V13 R1-A1 给除权除息锚的两条行为用例。
# 853 → 855：V13 R1-A2 给 Python↔Rust 信封往返与折算锚的两条行为用例。
# 855 → 860：V13 R1-A3 家族口径的三条（子串/整名/缺席）+ 编排消费点的两条
# （testnet 走私有 worker、「 Paper 」仍本地而 paper-proxy 必须被拒）。
# 860 → 863：V13 R1-A4 记账币种单源的三条（三条回落链同值 / 声明缺省==不声明 /
# 换币种必须动发布产物里的 result_hash）。
# 863 → 866：V13 R1-A6 deploy 模板读取覆盖的三条（登记表与磁盘逐名等 / 52 份逐份真读 /
# 每类读取器的坏内容探针）。整树实跑同轮 `cargo test --workspace` 是 92 个套件 858 条全绿，
# 866−858=8 就是 #144 那条"在册/实跑双口径"差，全部落在 feature 门后。
# 866 -> 1065：V13 R1-F 按磁盘重测。合流 c07ad22 之后这颗地板一直停在 866，而实测已是
# 1065 —— 余量 199 条＝全套件 18.7% 可以静默消失而门禁全绿，正是 §16 立这颗
# 地板要抓的那种失效（当年就是 39 条用例随死码删除一起消失、没有下限的 crate 全绿通过）。
# 分 crate 实测（按本文件的同一配方：`^[ \t]*#\[test\]$` + read_text 归一 CRLF）：
# qx-cli=389 qx-xingban=94 qx-runtime=83 qx-storage=76 qx-core=71 qx-api=56
# qx-adapter=49 qx-zhenlu=38 qx-data=28 qx-protocol=28 qx-execution=27
# qx-orchestrator=22 qx-strategy=22 qx-factor=20 qx-scheduler=15 qx-guanxing=8
# qx-provider=8 qx-risk=8 qx-genglu=7 qx-plugin=7 qx-control=9 qx-datastruct=5
# `#[tokio::test]` 同配方实测 0 条，故这个计数器没有整族漏计。
# 1065 -> 1069：V13 R1-H 控制面终态退场那轮的四条用例（qx-control 5 -> 9），其余 crate 逐名同数。
# 1069 -> 1070：V13 R4-C1 给两条 HTTP 读链补总量字节界那颗用例（qx-adapter 48 -> 49），其余 crate 逐名同数。
WORKSPACE_TEST_FLOOR = 1304  # 1233 -> 1304：2026-10-10 QX-DEV-PLAN-2026-10-10 阶段 2（T2-0 panic 边界与错误类别 / T2-1 `qx-app` 骨架 / T2-2 三个用例）按磁盘重测（同一配方：`^[ \t]*#\[test\]$` + read_text 归一 CRLF，`crates/*/src` 与 `crates/*/tests` 递归）。新 crate `qx-app` 带来 32 条（`src/tests/` 目录式 30 条：错误契约 11 / 规格契约 8 / 用例契约 11；`tests/independent_consumer.rs` 2 条 = 退出门 G2 的「独立 consumer 不启 qx-cli 也能跑完一次回测并复核」），`qx-cli` 449 -> 467（其中 3 条是 G1 的三入口等价性：CLI 真 spawn 二进制 / HTTP 走 `qx_api::ApiService::handle` / Python 真起解释器）。地板自 V13 R25 后一直停在 1233，实测已到 1304——余量 71 条（5.4%）可以整族消失而门禁照印全绿，正是这颗地板立起来要抓的那种失效。分 crate 实测：qx-cli=467 qx-core=101 qx-runtime=94 qx-xingban=94 qx-api=83 qx-storage=79 qx-adapter=58 qx-zhenlu=38 qx-app=32 qx-spec=32 qx-data=31 qx-protocol=28 qx-orchestrator=25 qx-strategy=24 qx-factor=20 qx-scheduler=18 qx-execution=17 contract-tests=15 qx-control=9 qx-guanxing=8 qx-provider=8 qx-genglu=7 qx-plugin=7 qx-datastruct=5 qx-risk=4。上一轮 1233 是 1083 -> 1233：V13 R25 按磁盘重测（本文件同一配方 `^[ \t]*#\[test\]$` + read_text 归一 CRLF，`crates/*/src` 与 `crates/*/tests` 递归）。地板自 R12/R13 后一直停在 1083，实测已到 1233 —— 余量 150 条＝12.2% 可以整族消失而门禁照印全绿，正是这颗地板立起来要抓的那种失效（§16 那一轮就是 39 条用例随死码删除一起消失）。分 crate 实测：qx-cli=449 qx-core=101 qx-xingban=94 qx-runtime=92 qx-api=81（含本轮新增的 `tests/console_operator_identity.rs`）qx-storage=79 qx-adapter=58 qx-zhenlu=38 qx-data=30 qx-protocol=28 qx-orchestrator=25 qx-strategy=24 qx-factor=20 qx-scheduler=18 qx-execution=17 qx-spec=17 contract-tests=15 qx-control=9 qx-guanxing=8 qx-provider=8 qx-genglu=7 qx-plugin=7 qx-datastruct=5 qx-risk=3。1082 -> 1083：V13 R12/R13 补一条 PostgreSQL 池容量为 1 的控制面事务回归用例（挂 #[ignore]，靠 CI 的 --ignored 或手工挂 QX_TEST_POSTGRES_DSN 来跑：不先 drop(client) 就是当场自死锁，事务已提交却永远回不了调用方）。1081 -> 1082：V13 R11 收口时补一条（WebSocket Close 扫描器的累积缓冲越界 fail-closed：跨读累积是新引入的内存面，客户端故意不完成帧头就能把缓冲无限拖大）。1073 -> 1081：V13 R11 补七条（C ABI 非法 side 拒绝 1 条、WebSocket Close 帧扫描器 5 条、托管子进程退出码携带原始码 1 条）。1071 -> 1073：V13 R10 补两条（Binance 缺 trade_id 转对账、恢复 worker 缺 venue_id 拒启动）。1070 -> 1071：V13 R7-a #284 把「版本号由宿主解码入口裁决」这颗判定钉进解码入口自己的用例（qx-runtime 83 -> 84），其余 crate 逐名同数。
# 门禁自身的判据数地板（V12 §17），取本回合实测的总条数。为什么要给量具本身再设一把尺：
# 本回合编辑 `TEST_MODULES` 时误删了 qx-cli 那一项元组，门禁当场少跑 4 条判据，却依旧打印
# "架构不变量自检全部通过 ✓" —— 判据可以整段消失而没人变红。这与 §16 抓到的"用例静默删除"
# 是同一类失效，只是这一次发生在量具自己身上，而 §16 的全部结论都建立在这把尺的输出上。
# 457 → 460：§22 给构建脚本新加的三条 #139 判据（调用存在 / 失败中止 / 排在 cargo 之前）。
# 460 → 463：§23 的变体级生产者三条（覆盖面在册 / 每个变体有限定名生产者 / 允许清单逐条可复核）。
# 463 → 466：V13 R1-A1 的除权除息锚三条（折算读已装载的公司行为 / 折算在跨日推导之后 / 行为用例在位）。
# 466 → 467：V13 R1-A1 第四颗——折算的输入必须有非测试写点，只数实例调用点。
# 467 → 481：V13 R1-A2 的跨语言对照 14 颗——版本号 / 三份字段名册逐项等 / 动作名册等 /
# 名册能被写侧自己认回（含往返用例）/ 定点单位单源 / 日期两种分隔 / 单位与日期各有用例 /
# 三份夹具在位 / 两侧共词根 / Rust 侧两条读回用例 / Python 侧重算比对两条。
# 481 → 489：V13 R1-A3 的 venue 单源 8 颗——定义点形状在册 / 判定式不得复活 / 消费者登记表 /
# 家族三条用例在册 / 编排 testnet 断言现场 / 编排 Paper 整名断言现场 / 账户命名断言现场 /
# 两份口径的差异写在注释里。
# 489 → 500：V13 R1-A4 的记账币种单源 11 颗——定义点唯一且值仍是 USDT / 全仓只有一颗同类常量 /
# 生产代码除定义点外没有第二处写死的 "USDT" / 定义处带着"缺省值≠白名单"的口径说明 /
# 消费者登记表 / 四条回落链各自仍读常量 / 三条用例在册 / 指纹用例钉的是 result_hash 而不是随墙钟动的 digest。
# 500 → 509：V13 R1-A6 的模板读取覆盖 9 颗——登记表三列对齐 / 登记表与磁盘逐名等 / Reader 变体清点 /
# 变体⊆已使用 / 变体⊆坏内容探针 / 每类读取落在生产读点 / 三条用例与模块挂载 / 预期结果口径 /
# 配对来源仍指向在册模板。这一把尺量的是"覆盖套件本身还在不在"：删掉登记项会被第 2 颗咬住，
# 把生产读点换成测试内自造校验会被第 6 颗咬住（GA/GC 变异各自实测打红）。
# 509 → 515：V13 R1-A5 的账户 null 名单同源 6 颗——名单由源码派生 / 构造只走 new() /
# 契约逐字段 description 点名两种身份且互斥 / 能力矩阵逐字段立 limitation 且合并命名不回来 /
# 接口文档点名并写明 null 不是 0 / 读侧 null 用例逐字段点名。这六颗是一条同源链的六个方向：
# 给某个字段接上生产者会同时打红第 1、3、4、5、6 颗（MA1 实测），反过来拆掉一个算点也是同样五颗
# （MA1b），四处口径各自改口而源码不动时只打红自己那一颗（MA3/MA4/MA5/MA7/MA8/MA9 各实测 1 颗）。
# 597 → 601：V13 R1-G 落地台账 #152（文档 `path:NN` 引用可达性）的四颗——前缀集由顶层目录
# 派生且非空 / 活文档逐处可落地（文件在盘上且行号不越界）/ 活文档引用条数地板 272 /
# 存档落空条数天花板 9 只降不升。
# 601 → 602：V13 R1-H 重落 V11 R6-1——删掉零构造者的 `CommandStatus::Rejected`，
# 终态词表收回到 `is_final` 体内一处（控制面 + CLI 幂等入口两个平面各一颗判据）。
GATE_CHECK_FLOOR = 936  # 928 -> 936：2026-10-10 QX-DEV-PLAN-2026-10-10 阶段 2 的关键路径 T2-0 → T2-1 → T2-2（退出门 G1 三入口 + G2 独立 consumer）。新建 `crates/qx-app`（应用层：`error.rs` 稳定错误类别 + `guard.rs` panic 边界 + `context.rs` 能力档 + `spec.rs` 版本化严格 JSON + `cases/{validate_dataset,run_backtest,verify_run}` 三个用例），`qx_app_check` 八颗：① 依赖集**精确**（5 个领域件 + serde/serde_json 共 7 名，`Cargo.toml` 是"应用层没有偷偷把 CLI/API/执行/存储拉进来"这条纪律的载体）；② `AppErrorCategory` 八类与路线图 §X1 那张表**逐条同名同序**（类别少一个或改名，调用方会把它送错下一步，而"能编译"对类别语义是盲的）；③ `maturity/app_use_cases.yaml` 九项登记面在盘、自述 kind 正确、三个用例九项逐格非空；④ 每个用例的键集恰好是那九项；⑤ 登记面用例集合 == `cases/mod.rs` 的 `pub use`，且每个用例都有自己的 `pub fn` 落点；⑥ `cases/mod.rs` 的九项文档表与登记面逐条相等（表头三列 + 第一列九行）；⑦ 登记面的 `capability` 与用例代码里的能力闸一致（说不管就必须真不管——"文档说不需要权限"与"代码里加了权限"两边都能编译）；⑧ qx-app 是 workspace 成员且三个门面都依赖它并有门面实现落点。同轮 `layer_dependency_check` 扩三格：`LAYER_FACADE_CRATES`（应用层不得反向依赖门面/适配/执行/存储）、`LAYER_FORBIDDEN_DEPS` 加 `qx-app`、`LAYER_SOLE_DEPENDENTS` 加 `qx-app → {qx-cli,qx-api,qx-python,contract-tests}`，并把"解析到全部 crate"的地板 24 → 26。**门禁自己的一处盲区也在这一轮暴露并修掉**：`clap_subcommand_parents` 过去只在 `cli_args.rs` 里找 `#[command(subcommand)]`，`App(AppArgs)` 这种"参数结构单独成文件"的父命令会被当成没有子命令，三条叶子从此没人跑过 `--help`（`data_validate_args.rs` / `plan_args.rs` / `console_args.rs` 早就把那种写法做成常规，只是它们都不带子命令）——现在两个取名册的助手都按「全 CLI 参数源」取事实。`UNLABELED_DIAGNOSTIC_CEILING` 的 `qx-cli` 55 → 53：`selfcheck.rs` 三处本来就带标签、只是把换行写进了字面量开头（`println!("\n[更路 · 重放校验]")`）被误记成未标签，按约定拆成 `println!()` + `println!("[组件 · 子域]")`（输出逐字节不变）；同轮 `qx-cli app` 新增一处机器结果直出（与 `--json` 那几条同一形状），净额 55 - 3 + 1 = 53——**新加机器输出就要在同一轮把债还掉**，不是把上界抬上去。上一轮 928 是 927 -> 928：2026-10-10 归档 V13 台账的历史部分——把 `docs/自研量化框架审计与重构方案-V13.md` 的 §9.1–§9.47（R1-A1 … R2 第三十五遍，原第 388–3371 行 / 2,984 行 / 活台账 3,820 行的 78%）拆入 `docs/archive/自研量化框架审计与重构方案-V13-逐轮执行记录.md`，活面只留 §1–§8 与 §9.48 起的逐轮记录（3,820 -> 841 行）；**原 §号一字未改**，所以旧指针（`…V13.md` §9.N，N ≤ 47）按号即得，同步改指的只有路径 token：`CHANGELOG.md` 21 处、`docs/archive/竞品对比与易用性改进优化计划-v1.md` 3 处、活面 §1–§8 内部 16 处。同轮给这份**新存档文档**补一颗牙齿（`doc_citation_reachability_check` 新增一颗 `TOTAL_DOC_CITATION_FLOOR`，活 234 + 存档 231 = 465）：单看活侧地板抓不住「存档被整份删掉」——删掉一份存档文档时 `live_total` 不动、`archive_dead` 从 9 掉到 0，而天花板判据是 `<=`，照样绿，于是「留而不删」这条纪律在门禁里没有牙齿；总地板让「拆/移只是换住址、删掉被引用的存档当场红」第一次可核。`LIVE_DOC_CITATION_FLOOR` 272 -> 234 是同一批 64 条引用换了住址（活 298 -> 234、存档 167 -> 231、活侧落空仍 0、存档落空仍 9），不是扫描集失效。上一轮 927 是 919 -> 927：QX-DEV-PLAN-2026-10-10 T1-1（阶段 1 最大一件 / 退出门 G1 第一条「同一运行可由 RunManifest 离线复算」）落运行证据包 RunEvidenceBundle（`crates/qx-spec/src/run_evidence.rs` + `schemas/run-evidence-v1.json` + `crates/qx-cli/src/run_evidence.rs` 构建器 + `report --evidence` 入口），`run_evidence_check` 八颗：① 对象 `pub` 字段集合与 schema 顶层 `required` **逐一相等**——`foundation_specs_check` 只核「版本常量 == const」，字段级是盲的，对象加一格而 schema 没加会让写出去的产物当场被自己的 schema 拒而门禁照印全绿；② 三条对象层纪律落在**代码**上（剥掉整行 `//` 注释）：空 `unverified` 拒 / `artifact_digests_verified=false` 拒 / `capability_level > L2` 拒——「写了纪律」与「纪律会拒」是两件事；③ `check_cross_references` 里那组元组与判据自己的期望表**逐条相等**（标签集合、被比字段、run 侧来源三处都对，且条数不为零——一张空表也能「不报错」）；④ 内容指纹与合成指纹**是两格**且交叉比对只拿 `composed_fingerprint` 去比 `run.data_fingerprint`——写这一格之前先看过真产物：RunManifest 的 `data_fingerprint` 是**合成**身份（`barframe:<内容哈希>` / `dataset-bundle:<指纹>`），而摘要 `input.fingerprint` 是纯内容哈希，第一版判据硬写这两者相等会让**每一份真产物都被拒**；⑤ 质量报告允许**缺席**（`Option`）不许**空**（`usable_tiers.is_empty()` 拒绝臂在盘）——`null` 是「这一档输入没有这份报告」，空 tiers 是「核过了，没有任何一档可用」，后者是一句该被拒的断言；⑥ 构建器是**生产**代码（不在 `tests/` 下）且被非测试文件真的调用——T1-1 交付的是「构建器 + schema」，只有 schema 等于半件事；⑦ `--evidence` 使用者可达（`cli_args.rs` 能力位 + help 写明）且 `write_run_evidence` 的调用点**排在** `recompute_declared_backtest_input` 之后——顺序反了会在拒绝路径上先落一份没核过的 `artifact_digests_verified=true`；⑧ 正向与反向（复核拒绝时不留证据包）两条常驻用例在盘。同轮把 `foundation_specs_check` 的登记面从七类扩到八类（`FOUNDATION_SPECS` / `FOUNDATION_OBJECT_TYPES` 与 `FoundationKind::ALL` 同步到 8），`maturity/schema-registry.json` 与 `maturity/artifact_migration.yaml` 各补一行 `run-evidence-v1`（`ARTIFACT_MIGRATION_MIN_ROWS` 10 -> 11 是「在册契约只增不减」的棘轮），并把 `qx-spec` 那条手抄用例表 `describe_reads_every_foundation_kind` 改成与 `FoundationKind::ALL` **逐条同名同序**断言——它叫「every foundation kind」却是一张手抄表，下次加一类会静默漏掉。上一轮 912 是 905 -> 912：QX-DEV-PLAN-2026-10-10 T0-3 把实盘轨的**边界**从三处散文（方案 §2 的五段表、`maturity/evidence/README.md` 的四档说明、验收脚本写进 `result.json` 的那句规则）收成一张机读登记面（`maturity/external_chain.yaml` + `external_chain_check` 七颗：登记面在盘且 kind/顶层键/四档/五段齐全、五段与方案 §2 那张表逐行同名同序且每段带失败即口径、翻转规则三处同源（登记面/方案/脚本文字，剥反引号后逐字相等）、脚本的**代码**真实现那条规则（live-check 豁免 + 只有 pass 才 allowed）、证据根现状与盘上逐份 `result.json` 一致且 `flip_allowed` 恰等于「存在 outcome=pass」、两档计数与 `capabilities.yaml` 逐值相等且**没有 pass 记录时两档必须全为 false**、第二交易所的 `--venue` 值在脚本 choices 里且该入口在命令面上是 L 档且默认关闭）。此前 `capabilities_check` 只核「未拿到沙盒记录前 `sandbox_tested` 全为 false」这一条，`external_acceptance_check` 只核脚本点名的入口/常量/配置/worker 与命令面对齐——**没有任何一处**登记过「实盘轨分几段、翻档的唯一依据是什么、今天有没有这份依据」，于是同一件事的三种说法可以各说各的：方案 §4 写「除 live-check 之外全部阶段退出码为 0」，而脚本写进结果包的是「全部阶段退出码为 0」，方案自己声称「脚本与文档同口径」而门禁里一条判据都没核过它（本轮实测并已按方案口径改正脚本那句文字）；反过来，「把 `implementation: true` 读成生产已批准」也没有任何判据拦——登记面今天才把这条界线写成可核对的一格。上一轮 905 是 898 -> 905：QX-DEV-PLAN-2026-10-10 T0-4 把「规范回测」的**夹具身份、产物摘要、耗时与内存**冻结成一份在册基线（`maturity/backtest_baseline.yaml` + `tools/backtest_baseline.py` + `backtest_baseline_check` 七颗：记录与脚本都在盘上且脚本真被 `main()` 调用过、顶层十块齐全且 schema/kind 自述正确、夹具身份逐项非空且 `data_fingerprint` 与 `maturity/backtest_acceptance.yaml` 同一份数据集、`result_hash` 与验收记录逐字相等、产物四类齐全且 `equity`/`fills` 摘要与验收记录逐字相等（这两份是纯数据、不含绝对路径，跨机可比；`summary`/`run_manifest` 内嵌产物路径故只比在册与摘要非空）、耗时与内存只核形状与非零（机器相关，不进判据数值）、脚本真用 `perf_counter`/`Popen`/OS 峰值 RSS 三处测量原语且记录不含绝对路径）。此前 `performance_baseline_check` 只核 `benchmarks/run_baseline.py` 在盘且 README 指向它——那份量的是性能轨的**脚本存在性**，没有任何一处把「这一轮的规范回测跑出来的是哪份夹具、哈希是多少、花了多少秒、峰值内存多少」钉成可比对的数；夹具被换掉、结果哈希漂移、`--release` 换成 debug，三者都不会红。上一轮 898 是 892 -> 898：QX-DEV-PLAN-2026-10-10 T0-2 把「46 条 CLI 入口」从名字清单升级为能力声明表（`maturity/command_surface.yaml` + `command_surface_classification_check` 六颗：表与 clap 命令表逐一相等、八格自述齐全、R/P/O/L 四档都有且父命令集合与 clap 的 subcommand 父命令表相等、L 档默认关闭且需私有凭据、P 档不需凭据、R 档无外部副作用）。此前 `cli_surface_coverage_check` 只核「每条入口能渲染自己的 --help」，没有任何一处声明某条入口属于 R/P/O/L 哪一档、默认面与权限是什么——新增入口不登记、把 L 档改成默认开启、给 P 档写上私有凭据，三者都不会红。上一轮 892 是 882 -> 892：V13 R31 控制台多账户作用域的读面前置——`/account/ledger` 与 `/reconcile/reports` 从「四条整体现读端点一律不收窄」里拆出来立一条按账户过滤的通道（`read_face_scope_check` 十颗：模块在盘且低于门槛+挂载、三张路由名单条目数自洽且两两不相交且每条都有活分派臂、查询键名单只有 read_scope 一份而 admission 不再自持 `*_PARAMS`、`accepted_query_params` 是唯一取名册出口且五张名单都在它体内、收窄键名单与底层类型的归属列一致（`LedgerEntry` 只有 `account_id`、`ReconcileReportSnapshot` 两列都有——账簿那一格只认一把键是数据结构决定的）、两条过滤臂真在结果集上过滤且不借用投影那族的 `missing_projection_response`、空结果回 200 而不是 `404 account_projection_not_found`、400/503 分工不合并、端点表只在真读收窄键的入口承诺查询串、九条行为用例在册）。此前 `api_surface_doc_check` 只比路由集合、`api_endpoint_table_routes.rs` 只比表与分派是否平，没有任何一颗判据看「这条臂有没有真读 `query`」——于是 `/account/ledger` 可以既在表里写 `[?account_id=]` 又一行过滤都没有，读者拿到的是默认账户那份流水而被当成了自己点名的账户；反过来把空结果改成 404 也不会红，而「这一轮没有事实」与「这个账户不在这份部署里」是两件事。上一轮 882 是 881 -> 882：V13 R28 多账户并行隔离补第七颗——并发用例真开两条线程跑两个账户的真实 `run_paper_execution_worker` 并用 `Barrier::new(2)` 强制重叠（顺序调用不算并发；`supervise` 里多个 worker 是同时活的，隔离必须在真并发下也成立）。881 -> 881 保持：V13 R28 公共组件面——诊断（进程日志）是唯一一处**没有**框架级单源的面（全仓无 `tracing`/`log`/`env_logger` 依赖，245 处诊断直接走 `println!`/`eprintln!`，靠非正式 `[组件 · 子域]` 前缀约定；`qx-api` 与 `qx-orchestrator` 已全量带标签），新增 `process_diagnostics_check` 三颗把它钉下来：① 引入诊断框架就必须一次迁移完（有框架依赖时未带标签站点必须为 0——半迁移是在既有约定之外多出**第三种**约定）；② 未带标签站点数只降不升（棘轮，表外 crate 要求 0）；③ 棘轮表是活名册（归零的 crate 必须从表里删掉、新增未标签站点的 crate 必须登记）。875 -> 878：V13 R28 收口 §17.11 登记的 CI 覆盖面缺口——`ci_feature_matrix_check` 新增三颗（`feature-off` 腿在盘且按 `--no-default-features --all-targets` 跑 clippy 带 `-D warnings` / 有 `cfg(not(feature = …))` 分支的 crate 都在腿上 / 腿点名的 crate 恰好等于声明了非默认特性的每个 workspace crate）。此前 `feature-matrix` 只 lint「特性开」那一侧：一颗只在 `#[cfg(feature = "sqlite")]` 里存在的模块，在默认特性下被 lint 到、在关掉特性时**整段不参与编译**——于是「关掉特性能不能编过」这件事只有 CI 的构建腿知道，lint 腿是瞎的。同轮把 `qx-cli` 的 `console` 与 `sqlite` 两条腿的 clippy 也接上。——把 `docs/自研量化框架审计与重构方案-V13.md` 的 §9.1–§9.47（R1-A1 … R2 第三十五遍，原第 388–3371 行 / 2,984 行 / 活台账 3,820 行的 78%）拆入 `docs/archive/自研量化框架审计与重构方案-V13-逐轮执行记录.md`，活面只留 §1–§8 与 §9.48 起的逐轮记录（3,820 -> 841 行）；**原 §号一字未改**，所以旧指针（`…V13.md` §9.N，N ≤ 47）按号即得，同步改指的只有路径 token：`CHANGELOG.md` 21 处、`docs/archive/竞品对比与易用性改进优化计划-v1.md` 3 处、活面 §1–§8 内部 16 处。同轮给这份**新存档文档**补一颗牙齿（`doc_citation_reachability_check` 新增一颗 `TOTAL_DOC_CITATION_FLOOR`，活 234 + 存档 231 = 465）：单看活侧地板抓不住「存档被整份删掉」——删掉一份存档文档时 `live_total` 不动、`archive_dead` 从 9 掉到 0，而天花板判据是 `<=`，照样绿，于是「留而不删」这条纪律在门禁里没有牙齿；总地板让「拆/移只是换住址、删掉被引用的存档当场红」第一次可核。`LIVE_DOC_CITATION_FLOOR` 272 -> 234 是同一批 64 条引用换了住址（活 298 -> 234、存档 167 -> 231、活侧落空仍 0、存档落空仍 9），不是扫描集失效。上一轮 927 是 919 -> 927：QX-DEV-PLAN-2026-10-10 T1-1（阶段 1 最大一件 / 退出门 G1 第一条「同一运行可由 RunManifest 离线复算」）落运行证据包 RunEvidenceBundle（`crates/qx-spec/src/run_evidence.rs` + `schemas/run-evidence-v1.json` + `crates/qx-cli/src/run_evidence.rs` 构建器 + `report --evidence` 入口），`run_evidence_check` 八颗：① 对象 `pub` 字段集合与 schema 顶层 `required` **逐一相等**——`foundation_specs_check` 只核「版本常量 == const」，字段级是盲的，对象加一格而 schema 没加会让写出去的产物当场被自己的 schema 拒而门禁照印全绿；② 三条对象层纪律落在**代码**上（剥掉整行 `//` 注释）：空 `unverified` 拒 / `artifact_digests_verified=false` 拒 / `capability_level > L2` 拒——「写了纪律」与「纪律会拒」是两件事；③ `check_cross_references` 里那组元组与判据自己的期望表**逐条相等**（标签集合、被比字段、run 侧来源三处都对，且条数不为零——一张空表也能「不报错」）；④ 内容指纹与合成指纹**是两格**且交叉比对只拿 `composed_fingerprint` 去比 `run.data_fingerprint`——写这一格之前先看过真产物：RunManifest 的 `data_fingerprint` 是**合成**身份（`barframe:<内容哈希>` / `dataset-bundle:<指纹>`），而摘要 `input.fingerprint` 是纯内容哈希，第一版判据硬写这两者相等会让**每一份真产物都被拒**；⑤ 质量报告允许**缺席**（`Option`）不许**空**（`usable_tiers.is_empty()` 拒绝臂在盘）——`null` 是「这一档输入没有这份报告」，空 tiers 是「核过了，没有任何一档可用」，后者是一句该被拒的断言；⑥ 构建器是**生产**代码（不在 `tests/` 下）且被非测试文件真的调用——T1-1 交付的是「构建器 + schema」，只有 schema 等于半件事；⑦ `--evidence` 使用者可达（`cli_args.rs` 能力位 + help 写明）且 `write_run_evidence` 的调用点**排在** `recompute_declared_backtest_input` 之后——顺序反了会在拒绝路径上先落一份没核过的 `artifact_digests_verified=true`；⑧ 正向与反向（复核拒绝时不留证据包）两条常驻用例在盘。同轮把 `foundation_specs_check` 的登记面从七类扩到八类（`FOUNDATION_SPECS` / `FOUNDATION_OBJECT_TYPES` 与 `FoundationKind::ALL` 同步到 8），`maturity/schema-registry.json` 与 `maturity/artifact_migration.yaml` 各补一行 `run-evidence-v1`（`ARTIFACT_MIGRATION_MIN_ROWS` 10 -> 11 是「在册契约只增不减」的棘轮），并把 `qx-spec` 那条手抄用例表 `describe_reads_every_foundation_kind` 改成与 `FoundationKind::ALL` **逐条同名同序**断言——它叫「every foundation kind」却是一张手抄表，下次加一类会静默漏掉。上一轮 912 是 905 -> 912：QX-DEV-PLAN-2026-10-10 T0-3 把实盘轨的**边界**从三处散文（方案 §2 的五段表、`maturity/evidence/README.md` 的四档说明、验收脚本写进 `result.json` 的那句规则）收成一张机读登记面（`maturity/external_chain.yaml` + `external_chain_check` 七颗：登记面在盘且 kind/顶层键/四档/五段齐全、五段与方案 §2 那张表逐行同名同序且每段带失败即口径、翻转规则三处同源（登记面/方案/脚本文字，剥反引号后逐字相等）、脚本的**代码**真实现那条规则（live-check 豁免 + 只有 pass 才 allowed）、证据根现状与盘上逐份 `result.json` 一致且 `flip_allowed` 恰等于「存在 outcome=pass」、两档计数与 `capabilities.yaml` 逐值相等且**没有 pass 记录时两档必须全为 false**、第二交易所的 `--venue` 值在脚本 choices 里且该入口在命令面上是 L 档且默认关闭）。此前 `capabilities_check` 只核「未拿到沙盒记录前 `sandbox_tested` 全为 false」这一条，`external_acceptance_check` 只核脚本点名的入口/常量/配置/worker 与命令面对齐——**没有任何一处**登记过「实盘轨分几段、翻档的唯一依据是什么、今天有没有这份依据」，于是同一件事的三种说法可以各说各的：方案 §4 写「除 live-check 之外全部阶段退出码为 0」，而脚本写进结果包的是「全部阶段退出码为 0」，方案自己声称「脚本与文档同口径」而门禁里一条判据都没核过它（本轮实测并已按方案口径改正脚本那句文字）；反过来，「把 `implementation: true` 读成生产已批准」也没有任何判据拦——登记面今天才把这条界线写成可核对的一格。上一轮 905 是 898 -> 905：QX-DEV-PLAN-2026-10-10 T0-4 把「规范回测」的**夹具身份、产物摘要、耗时与内存**冻结成一份在册基线（`maturity/backtest_baseline.yaml` + `tools/backtest_baseline.py` + `backtest_baseline_check` 七颗：记录与脚本都在盘上且脚本真被 `main()` 调用过、顶层十块齐全且 schema/kind 自述正确、夹具身份逐项非空且 `data_fingerprint` 与 `maturity/backtest_acceptance.yaml` 同一份数据集、`result_hash` 与验收记录逐字相等、产物四类齐全且 `equity`/`fills` 摘要与验收记录逐字相等（这两份是纯数据、不含绝对路径，跨机可比；`summary`/`run_manifest` 内嵌产物路径故只比在册与摘要非空）、耗时与内存只核形状与非零（机器相关，不进判据数值）、脚本真用 `perf_counter`/`Popen`/OS 峰值 RSS 三处测量原语且记录不含绝对路径）。此前 `performance_baseline_check` 只核 `benchmarks/run_baseline.py` 在盘且 README 指向它——那份量的是性能轨的**脚本存在性**，没有任何一处把「这一轮的规范回测跑出来的是哪份夹具、哈希是多少、花了多少秒、峰值内存多少」钉成可比对的数；夹具被换掉、结果哈希漂移、`--release` 换成 debug，三者都不会红。上一轮 898 是 892 -> 898：QX-DEV-PLAN-2026-10-10 T0-2 把「46 条 CLI 入口」从名字清单升级为能力声明表（`maturity/command_surface.yaml` + `command_surface_classification_check` 六颗：表与 clap 命令表逐一相等、八格自述齐全、R/P/O/L 四档都有且父命令集合与 clap 的 subcommand 父命令表相等、L 档默认关闭且需私有凭据、P 档不需凭据、R 档无外部副作用）。此前 `cli_surface_coverage_check` 只核「每条入口能渲染自己的 --help」，没有任何一处声明某条入口属于 R/P/O/L 哪一档、默认面与权限是什么——新增入口不登记、把 L 档改成默认开启、给 P 档写上私有凭据，三者都不会红。上一轮 892 是 882 -> 892：V13 R31 控制台多账户作用域的读面前置——`/account/ledger` 与 `/reconcile/reports` 从「四条整体现读端点一律不收窄」里拆出来立一条按账户过滤的通道（`read_face_scope_check` 十颗：模块在盘且低于门槛+挂载、三张路由名单条目数自洽且两两不相交且每条都有活分派臂、查询键名单只有 read_scope 一份而 admission 不再自持 `*_PARAMS`、`accepted_query_params` 是唯一取名册出口且五张名单都在它体内、收窄键名单与底层类型的归属列一致（`LedgerEntry` 只有 `account_id`、`ReconcileReportSnapshot` 两列都有——账簿那一格只认一把键是数据结构决定的）、两条过滤臂真在结果集上过滤且不借用投影那族的 `missing_projection_response`、空结果回 200 而不是 `404 account_projection_not_found`、400/503 分工不合并、端点表只在真读收窄键的入口承诺查询串、九条行为用例在册）。此前 `api_surface_doc_check` 只比路由集合、`api_endpoint_table_routes.rs` 只比表与分派是否平，没有任何一颗判据看「这条臂有没有真读 `query`」——于是 `/account/ledger` 可以既在表里写 `[?account_id=]` 又一行过滤都没有，读者拿到的是默认账户那份流水而被当成了自己点名的账户；反过来把空结果改成 404 也不会红，而「这一轮没有事实」与「这个账户不在这份部署里」是两件事。上一轮 882 是 881 -> 882：V13 R28 多账户并行隔离补第七颗——并发用例真开两条线程跑两个账户的真实 `run_paper_execution_worker` 并用 `Barrier::new(2)` 强制重叠（顺序调用不算并发；`supervise` 里多个 worker 是同时活的，隔离必须在真并发下也成立）。881 -> 881 保持：V13 R28 公共组件面——诊断（进程日志）是唯一一处**没有**框架级单源的面（全仓无 `tracing`/`log`/`env_logger` 依赖，245 处诊断直接走 `println!`/`eprintln!`，靠非正式 `[组件 · 子域]` 前缀约定；`qx-api` 与 `qx-orchestrator` 已全量带标签），新增 `process_diagnostics_check` 三颗把它钉下来：① 引入诊断框架就必须一次迁移完（有框架依赖时未带标签站点必须为 0——半迁移是在既有约定之外多出**第三种**约定）；② 未带标签站点数只降不升（棘轮，表外 crate 要求 0）；③ 棘轮表是活名册（归零的 crate 必须从表里删掉、新增未标签站点的 crate 必须登记）。875 -> 878：V13 R28 收口 §17.11 登记的 CI 覆盖面缺口——`ci_feature_matrix_check` 新增三颗（`feature-off` 腿在盘且按 `--no-default-features --all-targets` 跑 clippy 带 `-D warnings` / 有 `cfg(not(feature = …))` 分支的 crate 都在腿上 / 腿点名的 crate 恰好等于声明了非默认特性的每个 workspace crate）。此前 `feature-matrix` 只 lint「特性开着」的组合，`#[cfg(not(feature = "sqlite"))]` 那半边在 workspace 构建里从不被 clippy 看到（`qx-cli` 的 `default = ["sqlite"]` 经转发把 `qx-runtime/sqlite` 一并打开）——本机四条腿现跑各 rc 0 / 诊断 0 行；同轮这条腿当场抓出新写的 `parallel_run_isolation.rs` 里一处 `needless_borrows_for_generic_args`。869 -> 875：V13 R28 多账户 / 多 Venue 并行运行的隔离不变量（`parallel_run_isolation_check` 六颗：三条用例在册（两账户各跑真实执行 worker / 命令按账户分派 / 每账户一份投影）/ 第二账户日志名从唯一构造点 `account_event_log_name` 派生且用例里无字面量日志名 / 写侧两账户各跑一次真实 `run_paper_execution_worker` / 写侧两本账互查「对方账户现金为 0」/ `paper_submit_matches_worker` 拿订单账户比对 worker 声明账户 / `seed_paper_initial_cash` 按 worker 声明的账户入账而非硬编码 `main`）。此前这条不变量只有命名层用例（名字拼得对）与读侧去重用例（同一账户不分裂），**没有一条用例真的把两个账户的 worker 各跑一遍再翻开两本账看有没有串账**——命名对不等于事实不串。868 -> 869：V13 R27 三入口一致性用例（`contract-tests/tests/risk_parity.rs`）的 (c) 腿改为带规格取证（`risk_spec_fail_closed_check` 第 ⑥ 颗）——P1-5 fail-closed 的 parity 面收口：无规格入口会把"缺规格"当成一条业务拒绝，与 (a)/(b) 的规则链结果不可比。861 -> 868：V13 R26 P0-2 写面稳态追加不再整份拷贝状态 + EventLog 保留/归档策略落成"写下来的决定"（`pipeline_commit_rollback_check` 四颗：`self.clone()`+`*self = staged` 的旧写法不复活 / 两条写面（订单提交与事实归约）都有薄包装且都走强制重放回滚 / 重建短路被 `force` 门控住（否则 `next_seq` 漏回滚）/ 失败回滚行为用例在盘；`event_log_retention_policy_check` 三颗：EventLog 公开面无截断淘汰压缩入口 / 分段后端只有尾段可增长而满段不可变 / 策略三杠杆写在写面模块文档且与能力登记面互指；同轮 `seen_fills` 台账 census 跟着写面搬家到 `pipeline/commit.rs`）。上一轮 858 -> 861：V13 R26 门禁读数改由 `--snapshot` 写入机读快照（`gate_snapshot_check` 两颗 + 收尾一条「本轮实测 == 快照」漂移判据：快照在地板与条数上自洽、文档引用文件名而不是手抄条数）。上一轮 853 -> 858：V13 R26 qx-api 状态锁中毒不再 panic（`api_lock_fail_closed_check` 五颗：取锁收成可失败入口、读面错误收口把中毒判成 503/其余 400 且模块挂进 lib.rs、生产代码里不再有就地 `.expect("api state mutex poisoned")`、六条带键读面走收口且 /ready 报未就绪、中毒与对照组行为用例在盘；同轮 `QueryPort` 九个读法改回 `Result`）。上一轮 850 -> 853：V13 R26 账户级已实现/未实现盈亏接上生产者（`account_pnl_producer_check` 三颗：两个算点各唯一且都调 Ledger 派生方法、派生方法缺标记价即 None 且累计走 checked_add、读侧行为用例在盘；同轮 `account_money_field_registry_check` 的派生名单从五格收到三格）。上一轮 848 -> 850：V13 R26 发布版本单源 + CI 显式钉解释器（`release_version_single_source_check` 两颗：wheel 版本 == workspace 版本、rust-core 作业设 QX_PYTHON）。上一轮 843 -> 848：V13 R26 名义额规则缺规格 fail-closed（`risk_spec_fail_closed_check` 五颗：删净 `legacy_spot_spec`、`MaxNotionalRule` 体点名缺规格消息且不再合成临时规格、消息常量单点+有读者、`RiskGate::check_with_spec` 在盘且两条回测路径都传规格、一条行为用例在盘）。上一轮 838 -> 843：V13 R25 二轮 · 游标口径与登记面自身的可机读性（`web_console_field_wiring_check` 三十一颗 -> **三十四颗**：游标推进只认白名单里的写法且没有基线时不带 `after`、后端「回 seq > after、缺省从头给」与「事件序号从 0 起」这两个前提本身、API_PATHS 每一格都有取数读者；`capabilities_check` 三颗 -> **四颗**：maturity/ 的 8 份登记文件里没有会把 YAML 读成映射的裸标量（实测 8 份里曾有 2 份对标准解析器是 ScannerError）；`bounded_growth_and_reap_check` 九颗 -> **十颗**：死信台账在三条后端上都只有入账与读出，重投键带 attempts 且归零重投。立案的是「只有门禁读得懂的机读面」与「两套游标约定并存」那两类——前者对编辑器/CI lint/下一个工具是坏的，后者让页面每追平一次日志就必然吃一发 409。上一轮 801 -> 838：V13 R25 · 控制台 ⇔ 后端线格式的**字段级**接线（新增 `web_console_field_wiring_check` 三十一颗：读面七格名册与四份 Rust 事实源双向逐等、快照两层对手写 format! 字面量、balances 对 `json!`、顶层键再对 `schemas/account-snapshot-v1.json`、列 ⇔ 名册 ⇔ `<th>` 数、每格名册有运行时读者、`*_raw` 反向 census、定点标度单源单出口、时间轴标度两头钉在一起（`Ts` 的文档必须点名运行时唯一墙钟且那墙钟真是 epoch 毫秒）＋ 页面按毫秒渲染不再除 1e6、WS 帧词表双向 + 每帧有分派支、CSRF 两个字面量取自 console.rs 常量、BFF 不代理升级、**写面**请求体键清单对 `ControlCommand` 且两份 `<select>` 词表对 `CommandKind`/`Permission`）＋ 身份注入真的被用上（`console_front_check` 十七颗 -> **十九颗**：写面的身份覆盖唯一且排在权限裁决之前，加上无名册那一侧的常驻反例在盘）＋ 连接入口的两处如实（`web_console_check` 九颗 -> **十一颗**：带 ?token= 的入口预填本源、空基地址必须有可见反馈而不能静默无反应）＋ 监督器取用失败路径的收尸（`resource_lifecycle_and_lock_reentrancy_check` 两颗）。立案的是「路径级接线全绿而字段级全断」那一类：`web_console_check` 只比路由集合，一个字段都不看，于是页面可以按顶层读快照身份（真在 `header` 里）、给 `PositionSnapshot` 从来没有的 `side`/`cost_raw`/`market_value_raw` 开三列、金额不按 1e9 还原、WebSocket 每帧都不匹配却印「WS 已连接」，而门禁照印 853 全绿——与 `api_surface_doc_check` 在文档那一侧栽过的盲区同源。二十六颗分别钉：① 七格名册条目集合在册（少一格就少一份核对）；② 三张表都有列定义且表头 `<th>` 数与列数逐个相等；③ `positionRow`/`orderRow`/`eventRow`/`envelope` 与 `wire.rs::PositionSnapshot`、`wire.rs::OrderSnapshot`、`event.rs::Event`、`ProjectionEnvelope` 的 serde 字段名**双向**逐等（结构体里出现 `rename` 就当场炸——那意味着这份取数不再等于线格式）；④ `snapshotTop`/`snapshotHeader` 与 `AccountSnapshot::to_json` 那条手写 format! 字面量的两层键清单双向逐等（按 Rust 的 `{{`/`{}` 规则展开再按花括号深度取键，嵌套的 `reconcile` 三子键不会被算成顶层）；⑤ 写侧顶层键再与 `schemas/account-snapshot-v1.json` 的 properties 逐等（三份说法只有一份漂移也红）；⑥ `balances` 与 `/account/balances` 的 `json!` 响应体逐等；⑦ 每格名册都得有运行时读者（`fillTable`/`renderEvents` 的列核对或 `missingFields` 点名）——没人读的名册就是一张装饰性清单；⑧ 反向 census：app.js 里出现的每个 `*_raw` 键名都必须落在名册里（凭空开一列那一类故障反向也堵）；⑨ 定点标度只许一处定义且位数等于 `numeric.rs` 的 `SCALE` 次幂，除数由它推出（别处再除一次就是 1e9 与 1e8 并存）；⑩ `WS_FRAME_KINDS` 与 `ws.rs` 真发射的那七种帧名集合逐等，且每个帧名在 `handleFrame` 都有分派支（落空一种就是"帧被记成未知而实时面照印已连接"）；⑪ CSRF 的 cookie/头两个字面量取自 `console.rs` 的 `CONSOLE_CSRF_COOKIE`/`CONSOLE_CSRF_HEADER`，且写面真的把 token 取出来放回请求头；⑫ 同源 BFF 里不许出现升级转发（这条一变红，页面与文档那句「不适用」就成了谎话）、BFF 形态的降级在页面与说明文字里都在册。两颗收尸钉 `supervise_workers` 那一处 `child.stdin.take()`：失败分支必须先把 worker 挂进台账（`children.push(ManagedChild {` 两处），且不得在取用处就地 kill+wait——回收只有 `stop_managed_children` 那一处、按预算轮询 `try_wait`（与同函数里那颗「`child.wait()` 残留期望 0」是同一把尺）。上一轮 801 是 796 -> 801：V13 R24 · 控制台易用性三件（令牌缺失时临时生成兜底 + `--init` 脚手架 + `--generate-token`）：`qx-cli console` 在环境变量 `bootstrap_token_env` 缺失/为空时**临时生成**一枚一次性令牌并当场打印入口 URL（环境变量仍是首选来源，令牌仍不进命令行/配置文件、不落盘），`--init <path>` 写出与部署模板同形的就绪配置且**拒绝覆盖**已有文件，`--generate-token` 只打印一枚令牌与可粘贴的 `export` 行。新增 console_usability_check 五颗：① 令牌兜底在盘且环境变量仍是首选（`generate_bootstrap_token` + `std::env::var(&console.bootstrap_token_env)` + 临时令牌告示）；② 生成令牌有行为用例在盘（长度下限 + 十六进制 + 两次不同）；③ `--generate-token` 旗标声明且被 `serve_console` 读到；④ `--init` 旗标声明且被读到、且对已存在文件拒绝覆盖；⑤ `--init` 写出的模板与部署模板同形（绑回环、令牌只给环境变量名、无令牌字面量字段、写明 static_dir 与 operator）。上一轮 796 是 789 -> 796：M5' 的本地可验收子项落 **Paper 轨**（`tools/paper_acceptance.py` 跑仓库自己的 Paper venue：主链 `scheduler -> strategy -> paper-execution -> ledger` 两轮独立目录 + 同腿重跑，产出仓库资产 `maturity/paper_acceptance.yaml`），新增 paper_track_check 七颗：① 记录在盘且六格自述齐全（passed / 不需凭据 / 无外部 venue / 无网络 / 无订单 / 主链到 Executed）；② 记录由在盘脚本生成，且脚本真有四件比对（`compare_main_chain` / `compare_rerun` / `compare_independent` / `compare_no_credentials`）——只写一份"看起来通过"的记录不算；③ 主链四段写全且四个计数都是正数（跑出 0 笔成交的"通过"是空跑）；④ 记录正文不出现任何 venue 名称（先剥 `#` 注释再判，免得"外部证据在哪"那句散文被当越界声明）、实盘两档仍全 false（Paper 轨不替外部验收作保）；⑤ 终态退场与幂等（`pending_commands_after: 0` / 审计链校验通过 / 重跑不重复下单 / 两个独立目录事实面相等）——"跑通"与"跑完"不是一回事；⑥ 脚本默认跑仓库那份 paper 模板且在盘，并**主动摘掉**凭据环境变量（`CREDENTIAL_ENV_PREFIXES`）——"不需要凭据"是构造出来的，不是碰巧没配；⑦ `capabilities.yaml` 的 `backtest_only` 档登记了 Paper 轨自己的 `paper_acceptance_record` / `paper_acceptance_generator`（两条轨各有一份仓库资产，读者找得到证据在哪）。与回测轨同源，刻意同样**不**做「记录是否过时」判据（`generated_at_unix` 每跑一次都变，拿它当判据只会逼人写死时间戳）。上一轮 789 是 772 -> 789：M4'/M5' 落**同源 BFF 控制台**（`qx-api/src/console.rs` 会话/CSRF/身份注入/回环边界 + `qx-cli console` 子命令 + `api.console` 配置段与拓扑校验 + `deploy/qianxing.runtime.console.example.json` 模板），新增 console_front_check 十七颗：① BFF 模块在盘且 `mod console;`+`pub use console::*;` 挂进 lib.rs（否则门禁与调用方都看不见它）；② 六个公开常量（会话/CSRF cookie 名、CSRF 头名、令牌查询参数、缺省 TTL、静态资源表）齐备且资源表只认三份（路径穿越因此不可表达）；③ `ConsoleConfig::new` 是唯一构造入口且四类误配当场拒绝（空身份 / 令牌短于 16 / TTL 为 0 / static_dir 非目录）——这一层最贵的错是"配错了也起得来"；④ 会话 cookie 必须 `HttpOnly`+`SameSite=Strict`；⑤ CSRF cookie **刻意不带** `HttpOnly`（页面要读出来放进 `X-QX-CSRF` 头，双提交模式）但仍 `SameSite=Strict`；⑥ 非 GET 必须带 `X-QX-CSRF` 且与会话记住的那一枚逐字符相等（空头与不等都 403，不许放宽成"有头就过"）；⑦ 非 GET 还要过 `origin_matches_host`（带 `Origin` 的请求必须与 `Host` 同源——与 CSRF 头是两道独立的锁）；⑧ 身份由服务端按会话注入 `handle_inner(..., Some(&session.operator))`，且这一层不读命令体里可随便填的 `operator_id`（那是审计字段，不是认证）；⑨ 回环边界单源——判据只在 `console.rs` 定义、`qx-cli console` 绑监听之前先过它；⑩ 引导令牌只从 `bootstrap_token_env` 点名的环境变量读（配置里只有变量名，schema 不得出现令牌字面量字段）——命令行会进 shell 历史、配置文件会进版本库；⑪ `api.console` 段被 schema 声明、边界校验独立成 `console_validation.rs`、且在 `topology_validation.rs` 里 fail-closed 被调用；⑫ `Command::Console` 派发臂指向 `serve_console`（命令表里的入口不是装饰）；⑬ 部署模板在盘；⑭ 模板绑回环、令牌只给大写+下划线的环境变量名、正文无 `"bootstrap_token"` 字面量字段、写明 static_dir 与 operator；⑮ 行为用例在盘（真套接字端到端一条 + 回环/身份边界逐格一条）；⑯ 发布身份（`package_web_console.py`）里的「产品现在有什么」由在盘代码背书——`product_same_origin_bff`/`product_csrf`/`product_server_side_session` 声称 True 就必须真有 `console.rs`，`product_desktop_host` 与约定落点 `crates/qx-cli/src/desktop_host.rs` 的在场与否逐格相等（两边都不能各说各话）；同轮把该身份里 `csrf_supported`/`session_permissions_supported`/`desktop_host_supported` 三个**含糊字段**换成 `package_scope` + `product_*` 两组，免得「归档是 local-only 静态件」被读成「产品没有 BFF」。刻意不把 console 的 8 个错误码塞进 `deploy/README.md` 那张读面表：`console.rs` 用的是自己的 `json_response`（不落 `ApiResponse::json(`/`error_json(` 扫描口径），它不在 `read_face_source()` 的扫描集里，登记进去反而会让"读面 ⇔ 文档"那张表数不准；它的 401 只借了 `transport.rs` 的 reason 短语，那条已由 `api_shared_exits_and_status_lines.rs` 的 `arms.len() >= 9` 守着。上一轮 772 是 767 -> 772：M4'/M5' 续 · Web 控制台 local-only 边界硬化：静态包没有同源 BFF / CSRF / 服务端会话，连接入口与命令提交前都只允许 127.0.0.1 / localhost；发布身份显式登记 distribution_boundary 与 BFF/CSRF/会话/桌面 Host/Paper/sandbox/production 未完成状态。`release_supply_chain_check` 扩展发布包身份字段，`web_console_check` 新增两颗 local-only 判据。上一轮 767 是 764 -> 767：V13 R23 续 · M4'/M5' Web 静态控制台发布包：新增 `package_web_console.py` 确定性归档（版本/commit/Schema Registry 身份 + 三份资源 SHA256、gzip/tar 元数据归零），tag 发布流水线产出 `web-console` artifact、附 build provenance、汇入 Release 的 SHA256SUMS；`release_supply_chain_check` 加三颗判据（打包器+三条测试、job 打包/attestation/upload、Release 汇集与哈希清单）。764 -> 767：V13 R23 续 · §6.4 P2-2 落 http_surface_check（七颗）：自研 HTTP/WS 面覆盖表（maturity/http_surface.yaml）逐格与真实代码对账——self_built_file 全在盘、每行 anchor 符号真声明、每行 case 用例真在盘、verdict.decision=keep_self_built、accepted_gaps 恰好等于「非 covered 行」、migration_trigger 在场。755 -> 757：阶段四 M3' 把控制台从「只读」推进到「控制面」（受理 → 执行者判定 → 终态退场）。`web_console_check` 五颗 -> 七颗：原来那颗「不许出现 POST 或 /control/commands」换成**写面唯一**（`method: "POST"` 全文件只许出现一处，且必须是 `postJson(API_PATHS.controlCommands`；另加禁用名单 `/order/submit` / `binance-submit-order` / `paper-submit-order` / `/control/commands/execute`——下单只能走控制面受理，页面不得直连下单端点），并新增两颗：三阶段文案与状态词表（Accepted/Executed/Failed）在盘、页面如实交代 `403 authenticated_operator_required` 的身份边界（operator 来自 mTLS，页面不能自声明）。上一轮 755 是 743 -> 755：V13 R23 续 · §7 M1 落 `qx-core::contract` 稳定契约单点 + 命名转换矩阵（12 颗判据 contract_matrix_check）：把 P1-5 点名的三对同名兄弟（Bar / StrategyContext / DataProvider）从"隐式重复"（两处各写一份字段映射、谁也不知道还有第三处）变成"显式登记 + 与真实代码逐条对账"——规范单点在仓内唯一、同名兄弟真在盘、adapter 真有生产读者、没有未登记的第三份声明、且「同名但刻意不同层」的行必须写明理由；配套新增读面 `GET /schema/contract-matrix`（`qx_core::contract::CONTRACT_MATRIX` 的 JSON 形态，矩阵因此有真生产读者而不是躺在允许清单里）。上一轮 743 是 737 -> 743：V13 R23 续 · P0-1 把「回测轨」做成**不需要交易所凭据**就能完整验收的一条轨。卡点原本是 maturity/evidence/testnet/ 那份 Binance 验收 outcome=skipped（缺 QX_BINANCE_TESTNET_API_KEY/_SECRET），于是 sandbox_tested / production_approved 只能全 false，P0-1 一直挂在"未落地"；但这两档**只对需要外部 venue 的能力有意义**，而本仓主用法是回测与 Paper 闭环，一条凭据都不用。新增 tools/backtest_acceptance.py（两个独立目录各跑 quickstart + 同目录两轮 backtest；同目录重跑要求逐格相等含产物文件名与 config_fingerprint，跨目录只要求 result_hash / data_fingerprint / 归一化后的产物内容相等，equity/fills 逐字节相等；归一化只抹「绝对路径 + 内容寻址的 config_hash + 由它派生的文件名后缀」三样）+ maturity/backtest_acceptance.yaml（**仓库资产**，与未跟踪的 evidence/ 不同）+ crates/qx-cli/tests/backtest_acceptance_determinism.rs（两条行为用例）。补六颗判据（backtest_track_check 六颗）：① 默认档 backtest_only 且声明不需凭据/无外部 venue/那两档不适用；② 记录在盘且六格自述齐全；③ 记录由在盘脚本生成且脚本真有 compare_reruns/compare_independent/one_leg；④ result_hash 是 16 位十六进制且四类产物摘要齐全；⑤ 记录正文不出现任何 venue 名称（先剥 `#` 注释再判，免得"证据在哪"那句话被当越界声明）、实盘两档仍全 false；⑥ 行为用例在盘。刻意不做「记录是否过时」判据——generated_at_unix 每跑一次都变，拿它当判据只会逼人写死时间戳。上一轮 737 是 729 -> 737：V13 R23 · P1-11/DD-5 把错误码从字符串抽成**五元契约**（`crates/qx-core/src/error.rs`：`ErrorCode` 闭集 + `Retryability` 四档 + `ErrorContract` 五格 + `QxError::contract()` 唯一映射表，`code()` 从它派生），并把消费侧接上（`qx-cli/src/usage_errors.rs` 的 `qx_context` 显式消费契约、`runtime_wiring/pipeline_storage.rs` 四处 `QxError`→字符串边界不再摊成 `{error:?}`、`qx-runtime/src/pipeline.rs` 两处重试循环改按 `retryability` 分支），补八颗判据（error_code_contract_check 八颗）：① 三型落在 qx-core 的 error 模块且 lib.rs 重导出；② `ErrorContract` 五格字段齐；③ `ErrorCode::ALL` 是闭集（声明长度 == 列出的码 == `QxError` 变体数）；④ `contract()` 是唯一映射表且 `code()` 从它派生、自身不再 `match self`；⑤ CLI 展示层按 `reconcile_required`/`retryability.allows_retry()` 分级；⑥ 四处 QxError→字符串边界全走 `qx_context` 且文件里不再有 `{error:?}`；⑦ 热路径重试循环按 `retryability` 分支（两处）；⑧ 行为用例在盘（qx-core 侧自洽 + qx-cli 侧逐变体五格 / 展示层分级 / 真实打开边界带码三条）。刻意不纳入 HTTP/工作流状态串（qx-api 的 status、qx-control 的 CommandStatus）——它们是各自的读面，塞进 `ErrorCode` 会把闭集撑成开放集、③当场数不准。上一轮 729 是 723 -> 729：V13 R23 · P1-6/WP-19 把三把权益/保证金尺子收敛成单一 `ValuationContext`/`ValuationResult`（`qx-core/src/valuation.rs`：`Ledger::valuate` 单点派发 + `MarginState::valuate` 共用结果形状；回测两处手工派发与 CLI paper 保证金派发全部改走它），补六颗判据（valuation_single_source_check 六颗）：① 类型落在 qx-core 的 valuation 模块且 lib.rs 挂载+重导出；② `Ledger::valuate` 在盘且三把参数化尺子的派发只在它里面；③ `MarginState::valuate` 在盘且两个入口共用 `ValuationResult::new`（available 关系单源）；④ 生产源码全文扫描——三把参数化尺子只能出现在定义点（`ledger/query.rs`）与估值单点（`valuation.rs`），取 `production_text` 剥掉注释与测试项以免「文档提一句 / 用例直接调原语」数不准；⑤ `MarginState::equity`/`available` 委托 `valuate`；⑥ 派发等价性四条行为用例在盘（valuate 与原始尺子逐值相等、有/无汇率两把尺子给出不同的数）。刻意不纳入 `Ledger::equity_for`（乘数固定 1 的无参现货尺子，没有"选哪把"的歧义）。上一轮 723 是 717 -> 723：V13 R23 · P1-2/§7 M2 落 LiveEventPipeline 游标增量 refresh（`SqliteEventLogStore::read_since` 行级尾部读 + `refresh_latest` 三支：空尾部 no-op / 前缀一致只接尾部 / 前缀不符整份重建），补六颗判据（pipeline_cursor_refresh_check 六颗）：① SQLite 读侧 read_since 按 seq 只取尾部且不调 load_event_log（否则 O(N) 读放大原样回来）；② RuntimeEventStore::read_since 委派 SQLite、其余后端显式回落 Ok(None)（不是静默降级）；③ refresh_latest 三支齐备；④ apply_tail 用 P1-1 的 append_batch 接日志且不做整份重建索引；⑤ 增量与整份重建共用同一个 apply_index_event（归约单源，两条路径不能各写一份）；⑥ 读侧前缀校验与归约侧增量路径各有一条行为用例在盘（增量与整份重建的终态完全一致，纯行为断言抓不到退化，故同时把 tail_appends/rebuilds 两个内部计数器当回归证明面）。上一轮 717 是 711 -> 717：V13 R23 · P1-1/DD-2/§7 M2 落 EventLog 写侧单事务批量追加（`EventLog::append_batch` + `SqliteEventLogStore::append_batch` + 生产写面接线），补六颗判据（event_log_append_batch_check 六颗）：Kernel 的 append_batch 在盘且 append_checked 委托给它（逐条校验规则单源）、digest() 与前缀摘要 digest_of_prefix() 同源、SQLite 增量追加函数不调 load_event_log（否则 O(N) 读放大原样回来）、append_batch 在单事务里走增量路径、pipeline.rs 的 SQLite 分支真的调它、原子性与增量性各有一条行为用例在盘。上一轮 711 是 708 -> 711：V13 R23 · P1-12/§8 WP-22 建 `crates/contract-tests`（纯测试宿主），把两条压在正常边上的 dev 环（`qx-execution --dev--> qx-runtime`、`qx-risk --dev--> qx-zhenlu`）连同它们的 5 份跨 crate 契约用例（10 条）搬出生产 crate，补三颗判据（dev_dependency_cycle_check 三颗）：① 两条已知环不许回来；② 全仓不得存在「A --dev/build--> B 且 B --normal--> A」这种二点环（`cargo tree` 默认视图看不见 dev 边，反向正常边却真实存在——新写的 dev 边踩到任一正常边当场红）；③ 搬家不是丢用例——迁走的五份契约用例仍在 `crates/contract-tests/tests/` 且该 crate 在 workspace members 里（否则它们再不会被 `cargo test` 跑到）。`EXECUTION_TEST_FLOOR` 25 -> 15 是这次搬家的补偿口径：本地板只覆盖 `src/tests` 的 15 条，全仓地板把新 crate 一并计入、总数不变。上一轮 708 是 703 -> 708：V13 R23 · P0-3/DD-4 落 in-process 原生策略信任门（`crates/qx-cli/src/native_trust.rs`：`admit_in_process_c_abi` + `target_triple_matches`），补五颗判据（native_trust_check 五颗）：信任门模块在盘且两个判定都非测试专用、`load_c_abi_strategy` 在 `dlopen` 之前先过信任门、配置面暴露 `c_abi_trusted_native`/`c_abi_target_triple`、且信任门开关缺省为 false（默认拒绝）。上一轮基线 703 是 698 -> 703：V13 R23 · 阶段四 M1'/M2' 建只读 Web 控制台（web/console/{index.html,app.js,styles.css}），补五颗「前端 ⇔ 后端接线」判据（web_console_check 五颗）：控制台三件在盘、app.js 点名的每个 API 路径都在 qx-api 路由表里（前端引用一个后端不存在的端点当场红）、index.html 写明 cors_allowed_origins 跨源要求、写明 `qx-cli serve` 启动入口、且控制台不得长出写操作（出现 POST 或 /control/commands 即红——它就不再是只读控制台）。上一轮基线 698 是 697 -> 698：V13 R22 #222 修 `TEST_PATH` 盲区（它只认目录式 `src/tests/`、不认单文件式 `src/tests.rs`，于是「唯一读者写在 `src/tests.rs` 里」的 `pub` 项被当成有生产读者）并补一颗「测试模块两种形态」判据（`zero_reference_public_surface_check` 新增一颗）。口径定成一格：`(?:^|/)tests\.rs$` 补齐单文件形态；放宽后由单文件盲区放出来的 8 条零读者（`qx-datastruct::{close_at, from_view, resample_with_manifest, select_time_with_manifest}`、`qx-protocol::{from_wire_json, to_wire_json}`、`qx-xingban::{corporate_action_supported_by_ledger, corporate_actions_from_data}`）逐条进允许清单并写明理由（生产链路走 `BarFrame::from_json` / `apply_corporate_actions_json`，这些类型化程序化入口只有库内用例读者）。上一轮基线 697 是 695 -> 697：V13 R21 补两颗 fam12「二级入口逐颗用例」判据（cli_surface_coverage_check 新增两颗）。顶层命令表此前只覆盖父命令，`backtest ccxt-builtin` / `strategy list` 这类二级入口一颗用例都没点过名——`crates/qx-cli/tests/command_surface.rs` 新增 `NESTED_COMMANDS` 常量与 `every_nested_command_renders_its_own_help` 用例逐条实跑 `--help`，两颗判据分别钉：清单与四个父命令（config/run/strategy/backtest）的 clap 子命令表逐一相等、且清单真的被逐条喂给被测 binary（只列不跑 = 红）。上一轮基线 695 是 689 -> 695：V13 R20 补六颗 fam15「内核时间轴口径」判据（kernel_timeline_check 六颗）。README 与 crates/qx-core/Cargo.toml 把内核描述成"有确定性时钟与因果事件队列"，而 crates/qx-core/src/clock.rs 自己写着"这里不提供虚拟时钟"、lib.rs 写着"内核不提供虚拟时钟对象"——文档替一段不存在的代码作保（§13.1 修 README 过度声明的同族）。六颗分别钉：三件死实现（引擎/因果队列/时钟）不得以原名回到 crates/、clock.rs 只剩 pub type Ts = u64; 且唯一、三份口径来源不再写旧承诺、README 改口到真实推进路径（含优先级序）、因果优先级数值 Rust↔Python 单源、pipeline.rs 的 (ts, prio) 单调落盘判据唯一且在正文。上一轮基线 689 是 V13 R20 补五颗「手续费币种折算」判据（fee_settlement_currency_check 五颗）。回退面重放时发现 `crates/qx-core/src/ledger/fill.rs` 在祖先 `7e8f3db` 上是 334 行、带 fee_in_settlement_raw/base_asset_of，合流后只剩 261 行、把异币种手续费按面值记进结算币种——`Fill.fee_currency` 由两个适配器填充且已进事件指纹，归约侧整份不读它，是 D2 的原始缺陷（不在 §9.48 的 15 族名册里，是重放模块级判据时才暴露的第 16 族）。五颗分别钉：折算函数唯一且两条记账入口都过它、费用腿记折完的数不记回报原数、基准资产费用按成交自身价格定点折算、折算只对乘数为 1 的现货开放（衍生严格认结算币种）、七条用例逐形状在位。上一轮基线 684 是 V13 R18 补六颗 fam05「调度 owner 路由 fail closed + JobSpec 读侧」判据（scheduler_owner_routing_check 六颗）。恢复 crates/qx-scheduler/src/job_spec.rs（Trigger/JobWindow/JobSpec 与 owner 路由判据的落点）并把 owner 路由判据单源化：JOB_OWNER_ANY 一个常量、claimable_by 一处定义，领取端（workers.rs）与装配端（load_scheduler_state 的 validate_job_owners，新建与载入两条路径）共用它。六颗分别钉：通配 owner 只有一个常量且领取端不再就地比较 "*"、装配处 fail closed 且两条路径都要问、deploy 示例声明的作业清单都找得到、deploy 示例里每个启用作业都有可领取的启用 Strategy worker、JobSpec 三格零读者字段按 2026-10-06 方案 §13.2 保留不删 + 登记 limitation + 双向钉住、作业清单示例每一格顶层键都在 JobSpec 名单里。上一轮基线 678 是 V13 R17 补七颗 fam04「适配层 IO 预算与 venue 缓存」判据（io_budget_and_venue_cache_check 七颗）。恢复 crates/qx-adapter/src/io_budget.rs 与 crates/qx-adapter/src/venue_cache.rs，把 write_all_within 接进三处子进程 stdin 写入（CCXT worker / 策略 worker / 事件 consumer handler），把 evict_stale_terminal_orders 接进两个常驻 venue 的订单写入点（binance 3 / ccxt 2）并级联退派生索引。七颗分别钉：三处 stdin 写入都走 write_all_within 且无一处退回裸 write_all、io_budget 只按截止时间收写线程且只有超时那格把打断管道的责任交回调用方、WebSocket 单帧/整条消息/单次轮询三层长度预算各一个具名常量、venue 缓存封顶只退终态订单且越限才扫表、两个 venue 的每处订单写入都过封顶并级联清派生索引、binance 未知订单仍升级对账、针路 OMS 刻意不共用封顶。上一轮基线 671 是 V13 R16 补八颗 fam01「文件锁统一」判据（file_lock_single_source_check 七颗 + backtest_clock_honesty_check 的墙钟豁免在场一颗）。恢复 crates/qx-core/src/file_lock.rs（V11 §40 D1 的崩溃可恢复写锁：纯判据 decide_lock + 有界等待 + 按年龄接管孤儿锁 + Drop 只删自己那把），并把四处就地 create_new 锁（数据集注册表 qx-data/registry.rs、多腿状态 qx-zhenlu/lib.rs、存储信封 qx-storage/state_envelope.rs、作业 claim qx-storage/file/jobs.rs）全部改走 qx_core::FileLock。七颗分别钉：FileLock 只有一个定义点、全仓再无第二处 remove_file(*lock*) 的就地锁生命周期、四个消费文件都引用 FileLock::acquire 且不回归就地锁算式、以及「同一场竞争只接管一次」住在 decide_lock 里且循环把计数喂回判据、只按判据交出的年龄说话。file_lock 读墙钟（锁年龄与令牌 nonce）是唯一一格豁免，按路径点名并由「豁免文件仍在场」一颗核对。上一轮基线 663 是 V13 R15 补三颗 fam12「CLI 表面」判据。`cli_help_surface_check` 只核「help ≡ clap 表 ≡ cli.rs 派发」三侧集合相等，看不见「某入口其实一敲就崩 / 它自己的用法渲染不出来」；`cli_dispatch_check` 只核分派点唯一。三颗分别钉：crates/qx-cli/tests/command_surface.rs 的 CLI_COMMANDS 清单与 clap 表逐一相等（新增命令没进清单、或清单留了已删命令都红）、清单真的被逐条喂给被测 binary 跑 --help（只列不跑 = 清单退化成装饰）、config 的 clap 子命令集与 help 印出的 config <sub> 行逐一相等（第二层入口与顶层同一类断链）。上一轮基线 660 是 V13 R14 补两颗 fam11「分层处置」闭环判据。零读者那一族（zero_reference_public_surface_check / zero_reference_pub_crate_surface_check）只遍历在盘的定义，所以「函数被删掉、PUBLIC_SURFACE_ALLOWLIST 条目还在」这件事它永远看不见——条目会一直躺着，下一次有人把同名函数加回来时它就变成一张现成的免检通行证。两颗分别钉：允许清单里每个条目都对应一处真实定义（pub fn/pub const 或 pub(crate) fn）、每条理由都不是占位符（至少 6 个字符，当前最短的 7 个字符是 `任务 #119`）。上一轮基线 658 是 V13 R13 补三颗 fam02「停机令牌 + 连接上界」判据：V13 R2 #218 实测，监听循环按 stopped() 收摊后已握手的会话若不读停机令牌，线程就永远等在 wait_after 的 100ms 轮询里、join() 回不来，停机只能靠强杀；另一半是对端半开时读永远 TimedOut、写永远成功，那条线程与它占的连接预算永久留在账上。三颗分别钉：停机令牌字段只有一处且真被 store(true, Release) 置起、会话帧循环在阻塞等待之前先读令牌（load 早于第一个 wait_after）且读到即回 server_shutdown、空闲轮次上界常量只有一处且真被用来收摊。V13 R2 #218 实测：监听循环按 stopped() 收摊后，已握手的会话若不读停机令牌，线程就永远等在 wait_after 的 100ms 轮询里、join() 回不来，停机只能靠强杀；另一半是对端半开（不发 FIN、也不再写字节）时读永远 TimedOut、写永远成功，那条线程与它占的连接预算永久留在账上。三颗分别钉：停机令牌字段只有一处且真被 store(true, Release) 置起（有生产者、初始化关闭）、会话帧循环在阻塞等待之前先读令牌（load 早于第一个 wait_after）且读到即回 server_shutdown、空闲轮次上界常量只有一处且真被用来收摊。fam02 另外两格（快照历史有界、事件按账户键）已由 bounded_growth_and_reap_check 与 browser_admission_check 看守。上一轮基线 655 是 V13 R12 补四颗 fam10「运维读面：exposition 真换行 + 告警名册每个指标都有生产端」判据。V13 R2 第七遍实测到的缺陷形态是 /metrics 正文只有一行、行与行之间是字面的 `\n` 两个字符：抓取端把整份正文读成一行、一条样本都解析不出来，而告警侧是「永不触发」而不是「报错」——服务端与告警侧都不会自己出声。四颗分别钉：exposition 构造函数用真换行（体里出现 `\n` 转义、不出现字面的双反斜杠 n）、那条逐行解析用例在盘且解析器显式拒绝字面换行、告警名册里每个 qx_ 指标都能在生产源码里找到写出点（否则该告警永不触发）、名册每条规则都有 expr/severity/summary。取名册文本时同样先剥整行 `#` 注释：名册里那段解释 `qx_outbox_relay_parked` 的散文不剥掉，会替真规则满足「有生产者」那颗判据。上一轮基线 651 是 V13 R11 补七颗 fam13「CI 特性矩阵点亮每颗特性闸门 + NATS 用例有真执行的腿」判据。§12.2 把这一族登记为回退面时明写「新补的那条 NATS 腿只由 ci.yml 的文本存在性保证，门禁里没有判据核它——把它删掉或加上 --no-run 不会红」，本轮把那颗缺失的判据补上：① feature-matrix job 在盘且三步（clippy/check/test）都按 --no-default-features 逐组合跑（只在某些 feature 分支里才存在的代码不会被默认特性的 lint 看到，矩阵漏一颗特性那颗特性的代码就从「有 lint」退成「无 lint」）；② qx-cli 声明的每颗特性都出现在某个矩阵组合里，且矩阵里不出现不存在的特性名（拼错即红）；③ NATS 真执行腿按指纹整段钉住（显式点名两个 NATS 测试目标，因此与 `-- --ignored` 那条腿不可能混淆）；④ NATS 非 ignore 用例数不低于现场实测值 8。上一轮基线 644 是 V13 R10 补五颗 fam06「environment 词表单点与 production 判定唯一出口」判据。此前「这份运行时配置是不是 production」被手抄 14 处（运行时配置校验 9 处 + CLI 体检/就绪 5 处），各写一份 `environment.eq_ignore_ascii_case("production")`，全仓没有单源出口——危害不是现在算错，而是改口径（如 `production`→`prod`）时漏改的那一处加固静默失效，而它守的正是「production 禁止明文 API / C ABI 必须配 Ed25519 公钥 / Execution worker 必须配名义额上限」。五颗分别钉：写法常量 `PRODUCTION_ENVIRONMENT` 与判定 `RuntimeConfig::is_production` 各只有一处定义、词表从常量取 production 这一档、判定体引用常量不得内联字面量、手抄式 `eq_ignore_ascii_case("production")` 不得在生产代码复活、调用判定的生产文件与登记表逐一相等（新增闸门必须登记）。同 `VenueId::is_binance`（V13 §5 A3）一族。上一轮基线 639 是 V13 R9 补一颗「金额/价格/数量字段的缺键不许静默变成 0」。裸 `#[serde(default)]` 落在非 `Option` 的 Money/Price/Quantity 上，等于给「这份回报没带该字段」和「交易所明确报了 0」发同一张身份证；本仓两条正确形状分别是 `Option<Money>`（AccountPositionSnapshot 三格）与 `#[serde(default = "named_fn")]`（A 股规则配置）。剩下的三处全在 qx-core/src/event.rs 且危险方向都是 fail-closed，按名字登记在 BARE_MONEY_DEFAULT_ALLOWLIST。上一轮基线 638 是 V13 R8 补五颗静默抑制判据（丢结构体字段值必须当场写理由、`#[allow(dead_code)]` 必须当场写理由、`unreachable!` 必须带非空消息、不得留 `todo!`/`unimplemented!` 桩、生产源码的 TODO/FIXME 与登记表逐项相等）。这五种形状的共同点是「改错了也不红」：字段没人读就加一行 `let _ = x.y;` 按住、能力没接上就挂 allow、分支真到不了就写裸 unreachable!()、需求没做完就留桩，全是孤儿逻辑与静默降级的温床。上一轮基线 633 是 V13 R16 补四颗，守契约与存储读回侧的防御口径一致性。第一颗守 Python `StrategyInput.from_dict` 必须像同文件的 `StrategyIntent` / `StrategyOutput` 一样调 `_reject_unknown_keys`：策略作者把 `positions` 拼成 `positionz` 时，输入侧静默拿到空 dict，策略可能以为账户是空的而误触发。第二颗守 `FactorReport::validate` 必须像 `FactorConfig::validate` 一样校验 `missing_policy` 的 reject/skip/zero 词表：报告能被 `from_json` 反序列化，只有空串校验时手改一份 JSON 能放行到下游 `resolve_missing` 才报错，落点离改错的地方很远。第三颗守 `outbox.attempts` 读回必须走 `parse_sqlite_u32` / `parse_u32` 而不是 `as u32` 截断：attempts 以 TEXT 落盘，u64→u32 是真截断，手工改库写成超过 u32::MAX 会回绕成小值、绕开死信判定。第四颗守 `FactorConfig` 的源头词表本身没被改弱——两处词表不一致时改哪一侧都会让另一侧的校验变成摆设。627 -> 629：V13 R14 补两颗，守两个平台的托管启动器都有 supervise 之前的 runtime-check 前置闸门，且 bash 侧的闸门失败不得用 $? 当退出码。`supervise` 只走 plan_workers 的拓扑校验，不检查配置引用的文件是否存在（数据集 bundle、研究快照、秘密文件的存在性只在 runtime-check / live-check 里查）——修前 start-qianxing.ps1 有这道闸门、start-qianxing.sh 没有，Linux/macOS 上的坏配置会拉起 7 个子进程再 fail-fast 全杀，而不是在任何子进程起来之前就拒绝。第二颗守 `if ! cmd; then echo ...; exit "$?"; fi` 里的 $? 拿的是上一条 echo 的状态（0），调用方会以为闸门通过了：部署平台按 0 退出继续走下一步，而实际上一个子进程都没起来。624 -> 627：V13 R11 补三颗，守不可信输入的三处边界。第一颗守 WebSocket 的 Close 帧检测必须按帧头判 opcode 而不是按整块读缓冲逐字节扫：Close 的 opcode 0x8 只出现在帧起点，逐字节扫 `& 0x0f == 0x8` 会让 256 个字节值里的 16 个（0x08/0x18/…/0xF8）都命中，二进制行情载荷里这类字节很常见，客户端发一个正常的 text/binary 帧就会把服务端静默断连。第二颗守托管子进程的退出码必须以 Option<i32> 携带而不是先转字符串：转成字符串之后 panic 的 101、OOM 的 137 和干净退出的 0 在监控里长得一样，父进程只能一律按 2 退出。第三颗守 C ABI 的 side 必须是定宽整数加显式拒绝而不是 #[repr(C)] 枚举：插件把 side 写进宿主内存，而 Rust 读取一个不在已声明判别值里的 #[repr(C)] 枚举值本身就是未定义行为，match 里没有任何可达的拒绝臂——布局改成 u32 后两侧字节完全不变，既有插件无需重编译。621 -> 624：V13 R10 V13 R10 补三颗，守实盘与恢复路径的三处静默降级。第一颗守 Binance 成交回报缺 trade_id(t) 时转对账而不是归零：trade_id 是 seen_fill_keys 去重键的一部分，归零会让两笔都没有 t 的成交互相误去重、漏计一笔且无人告警（CCXT 侧对同一情形本来就 fail-closed，两边口径不该不一致）。第二颗守两个多腿恢复 worker 都必须显式配置 venue_id：恢复扫描按 venue 过滤敞口腿，空值在过滤函数里是有意的通配符，而默认值产出的是非空字符串，等于把通配符路径变成不可达——配置遗漏时 worker 会静默只扫一个 venue、漏掉其余 venue 的裸腿。第三颗守 worker 线程 panic 的报错保留 JoinError 的 Debug 输出（含 panic 消息与线程名），不丢成一句 "panic"。619 -> 621：V13 R9 补两颗。第一颗守 `sync_control` 三后端共用一枚公共前缀判据：旧写法 `existing.len() > records.len() || zip 不等` 把"链比本地快照长但前缀一致"也报成 Conflict，而 sqlite/postgres 的 sync_control 跑在控制面事务 commit 之后，两进程并发时后提交那份 plane 会包含先提交者的变更、链天然比另一方长——那一支本该幂等收口，却被报成 Conflict，等于"控制面状态已提交成功、transact_control 却返回 Err"；真分叉是前缀内容对不上，那一支三后端照旧都拦（`a_chain_ahead_of_the_snapshot_fails_the_transaction_inst_of_rewriting_history` 仍绿，它守的正是内容分叉而不是长度）。第二颗守 WS 的查询键拒绝点名请求实际打到的 target：WS 通道不占路由表，硬写 `/events/live` 会让连到别的路径的客户端收到谎报的路由名。616 -> 619：V13 R8 补上「`pub(crate) fn` 零读者」一颗（含元判据，共 3 项）——R7 之前删掉的四个 AuditStore 镜像方法（`AuditFileStore::root`/`after`、`SqliteAuditStore::path`、`PostgresAuditStore::after`）正是从这里数出来的：`pub fn` 那层按全仓裸词匹配，`after` 这个普通英文单词在 `qx-api` 的事件游标参数里出现 26 次，于是"零读者"永远数不出来。`pub(crate)` 天生只在本 crate 可见，按本 crate 计数是构造性正确的，不需要为跨 crate 假活口付代价；唯一的新孤儿是 `qx-cli::for_test`（复合 cfg 条件剥离不掉，唯一读者在 tests/ 下），已入允许清单。R7 的 615 -> 616 是「API 投影桥的投影失败永久退场不重试」：R6-A 第一次落地时只做了 `pipelines.remove`，而外层 `while` 下一轮会按 Vacant 重开同一本账本、再投一遍、再刷同一行错误日志（每 250 ms 一次），语义与它自己的注释不符；同一颗还守提交顺序（按账户投影先于全局投影）。614 -> 615 是 R5 的「成交幂等台账只增不减」：Binance `seen_fill_keys` 与 CCXT `seen_trade_ids` 是只增不减的去重台账，淘汰已见键就等于允许同一笔成交被 trace 两次（重复记账），而它们刻意不设水位，所以判据守的是「不得出现淘汰调用」而不是「有界」。R4-A/B/C 的三处已计入 614。判据与常量块一并追加在 `def main()` 之前，因为 15 处文档指针引用了本文件的行号、而历史引用最大行是 9363。
# V12 §21 / #138：合流类判据的三个参数（455 → 457 就是本轮新增的两条，仍按"当轮实测总条数"取值）。窗口取 10 行是实测拐点 —— W=8 在 `tools` 之外命中 1,180
# 处、到 W=10 仍有 781 处合法的并列实现（三条 venue 提交链、几家存储后端），所以逐字重复零容忍
# 只对 `tools/*.py` 生效（本轮实测该目录 5 份脚本 0 命中），Rust 那一侧交给编译器与整树用例。
# 描述条数地板按本轮实测 353 条字面 label 取整留余量；跌破说明"取数方式"本身失效，而不是判据变少。
MERGE_DUP_WINDOW = 10
MERGE_DUP_TRIVIAL = re.compile(r"^\s*(?:[}{()\[\];,]*|use\s+[^\s].*|#!?/\*?.*|//.*?)\s*$")
GATE_LABEL_FLOOR = 300


def workspace_test_floor_check() -> None:
    """全仓 `#[test]` 总数不得跌破实测地板，且按 crate 点名，跌破时能立刻定位到谁。"""
    per_crate = {}
    for crate_dir in sorted(p for p in CRATES.iterdir() if p.is_dir()):
        total = 0
        for sub in ("src", "tests"):
            root = crate_dir / sub
            if not root.is_dir():
                continue
            for path in sorted(root.rglob("*.rs")):
                total += len(
                    re.findall(
                        r"^[ \t]*#\[test\]$",
                        path.read_text(encoding="utf-8"),
                        re.MULTILINE,
                    )
                )
        if total:
            per_crate[crate_dir.name] = total
    found = sum(per_crate.values())
    check(
        found >= WORKSPACE_TEST_FLOOR,
        f"全仓行为用例不少于 {WORKSPACE_TEST_FLOOR} 条",
        f"当前 {found} 条，按 crate："
        + ", ".join(f"{name}={count}" for name, count in sorted(per_crate.items())),
    )


# —— Windows 批处理的"可解析性"（V12 §17）——
# cmd.exe 读不懂"UTF-8 中文 + LF 换行"的 .bat：GBK 的前导字节会把下一行的行首字符吃掉，
# 于是一个 `REM` 变成可执行语句，脚本在半途报一串"不是内部或外部命令"后 exit 1 ——
# 而 README 明写"Windows 下可直接双击 build.bat"。实测四象限：CRLF×{chcp,无 chcp} 都能
# 完整跑完 9 步；LF×{chcp,无 chcp} 都在第 5 行注释处崩。故判据钉三件事：行尾必须是 CRLF、
# 每行最后一个字节必须是 ASCII（中文只出现在行中间）、`.gitattributes` 必须钉住 eol=crlf
# （否则 autocrlf=false/input 的克隆会拿回 LF，双击即坏，而本机工作树看不出问题）。
BATCH_DIRS = (ROOT, ROOT / "tools", ROOT / "deploy", ROOT / "python", CRATES)


def windows_batch_parse_check() -> None:
    """仓库里的每个批处理脚本都得真的能被 cmd.exe 解析。"""
    bats = sorted(
        {
            path
            for base in BATCH_DIRS
            if base.is_dir()
            for pattern in ("*.bat", "*.cmd")
            for path in (
                base.glob(pattern) if base == ROOT else base.rglob(pattern)
            )
            if ".git" not in path.parts
            and "target" not in path.parts
            and ".venv" not in path.parts
        }
    )
    check(bool(bats), "Windows 构建脚本仍在仓库里", "找不到任何 .bat/.cmd：README 的双击口径失效")
    for path in bats:
        rel = path.relative_to(ROOT).as_posix()
        raw = path.read_bytes()
        lone_lf = raw.count(b"\n") - raw.count(b"\r\n")
        check(lone_lf == 0, f"{rel} 用 CRLF 换行（cmd.exe 读不了 LF 版中文批处理）",
              f"有 {lone_lf} 个孤立 LF")
        tail_rows = [
            i for i, line in enumerate(raw.split(b"\r\n"), 1) if line and line[-1] >= 0x80
        ]
        check(not tail_rows, f"{rel} 每行以 ASCII 结尾（中文不留行尾）",
              f"第 {tail_rows} 行以非 ASCII 字节结尾")
    attrs = ROOT / ".gitattributes"
    text = attrs.read_text(encoding="utf-8") if attrs.is_file() else ""
    # 按"实际存在哪种扩展名"要求对应规则：只写 *.cmd 的那一条不能替 *.bat 交差。
    for ext in sorted({path.suffix.lstrip(".").lower() for path in bats}):
        check(
            re.search(rf"^\*\.{ext}\s+\S.*eol=crlf", text, re.MULTILINE) is not None,
            f".gitattributes 把 .{ext} 钉成 eol=crlf",
            "缺这条规则时 autocrlf=false/input 的克隆会拿回 LF，双击即坏",
        )


# 两个平台的构建脚本必须是同一套门禁：V12 §17 之前 build.bat 已经走到 [8/8]，
# 而 build.sh 还停在 "[7/7] 运行 CLI 全链路与生态冒烟" 里塞三条命令 —— 标签口径一漂，
# 照着 README 数门禁的人就会漏掉 runtime-check 这一道。判据比的是"步骤标签 + cargo 命令集"。
#
# V12 §22（#139）把架构门禁本身接进来：八步里没有任何一步跑 `check_architecture.py`，
# 只有 CI 跑，于是本地双击 build.bat 拿到"全部完成"时，这 457+ 条不变量一条都没被证明过。
# 现在它是第 [1/9] 步（只读源码、约 40 秒，排在几分钟的 release 构建之前），而"接进来"这件事
# 本身要成为判据：调用存在、失败会中止、且排在 cargo 之前 —— 三条各管一种"看起来接上了"的假法。
def build_script_parity_check() -> None:
    """build.bat 与 build.sh 逐项跑同一组门禁命令。"""
    bat = (ROOT / "build.bat").read_bytes().decode("utf-8")
    sh = (ROOT / "build.sh").read_bytes().decode("utf-8")

    def labels(text: str):
        found = []
        for line in text.splitlines():
            match = re.search(r'^\s*echo\b[ "(]*\[(\d+)/(\d+)\]\s*(.*?)[."\'\s]*$', line)
            if match and int(match.group(1)) > 0:
                found.append((int(match.group(1)), int(match.group(2)), match.group(3).strip()))
        return found

    def is_comment(stripped: str) -> bool:
        # `#\b` 匹配不上：`#` 与后面的空格都是非词字符，中间没有词边界。老写法靠先
        # replace("REM ","") 再判前缀侥幸过关，于是一行写着 "…不外传，cargo 就永远看不见它…"
        # 的中文注释被当成了第八条门禁命令（V12 §19 #133 自己踩的）。
        return bool(re.match(r"^(?:REM\b|rem\b|#)", stripped))

    def cargo_lines(text: str):
        found = []
        for line in text.splitlines():
            stripped = line.strip()
            # 注释行里出现 "cargo" 这个词不算调用了一条门禁命令。
            if is_comment(stripped) or "cargo " not in stripped:
                continue
            found.append(re.sub(r"\s+", " ", stripped.split("cargo", 1)[1]).strip())
        return found

    def interpreter_handoff(text: str):
        """探测选中的解释器有没有真的交给 Rust 侧（导出 QX_PYTHON），且早于第一道门禁。"""
        lines = text.splitlines()

        def is_print(stripped: str) -> bool:
            # echo 行打印的是"给用户看的修法"，不是脚本自己的行为：
            # §19 M2 实测把 `export QX_PYTHON` 删掉后，报错提示里那句
            # `echo " 或: export QX_PYTHON=..."` 会把判据骗绿。
            return bool(re.match(r"^echo\b", stripped))

        export_at = next(
            (i for i, line in enumerate(lines)
             if not is_comment(line.strip()) and not is_print(line.strip())
             if re.search(r'(?:\bset\s+"?QX_PYTHON=|\bexport\s+QX_PYTHON\b)', line)),
            None,
        )
        first_gate = next(
            (i for i, line in enumerate(lines) if re.search(r'echo\b[ "(]*\[1/\d+\]', line)), None
        )
        return export_at, first_gate

    def gate_call(text: str):
        """真正调用架构门禁脚本的那一行。注释与 echo 提示语都不算（同 §19 #136 的口径）。"""
        return [
            (i, stripped)
            for i, line in enumerate(text.splitlines())
            if (stripped := line.strip()) and not is_comment(stripped)
            and not re.match(r"^echo\b", stripped) and "tools/check_architecture.py" in stripped
        ]

    def first_cargo(text: str):
        return next(
            (
                i
                for i, line in enumerate(text.splitlines())
                if not is_comment(line.strip()) and "cargo " in line
            ),
            None,
        )

    check(
        labels(bat) == labels(sh) and len(labels(bat)) == 9,
        "build.bat 与 build.sh 的门禁步骤逐项同名同序",
        f"bat {labels(bat)} vs sh {labels(sh)}",
    )
    check(
        sorted(cargo_lines(bat)) == sorted(cargo_lines(sh)) and len(cargo_lines(bat)) == 7,
        "两个构建脚本调用的是同一组 cargo 命令",
        f"bat {sorted(cargo_lines(bat))} vs sh {sorted(cargo_lines(sh))}",
    )
    # Rust 侧只认 QX_PYTHON 这一个变量（python_interpreter()），而 [4/9] 的两条 Python 桥契约用例
    # 与 [8/9] 的策略 worker 都由 Rust 去起 Python。脚本把探测出来的解释器留在自己的变量里，
    # 就等于探测白做：V12 §19 实测双击 build.bat 必然死在 Rust 测试那一步（QX_PYTHON 未设置 →
    # 回落 PATH 占位桩 → 2 failed）。所以"外传"本身要成判据，两侧各一条。
    for name, text in (("build.bat", bat), ("build.sh", sh)):
        export_at, first_gate = interpreter_handoff(text)
        check(
            export_at is not None and first_gate is not None and export_at < first_gate,
            f"{name} 把探测选中的解释器导出成 QX_PYTHON，且早于 [1/9]",
            f"导出行 {export_at}、[1/9] 行 {first_gate}（None 表示没有导出）",
        )
    # #139：门禁接进本地构建路径。三条各挡一种"看起来接上了"的假法 —— 只在注释或报错提示里
    # 出现脚本名、调用失败后继续往下跑、以及排在几分钟的 release 构建之后（等于每次都白等）。
    calls = {"build.bat": gate_call(bat), "build.sh": gate_call(sh)}
    check(
        all(len(hit) == 1 for hit in calls.values()),
        "build.bat 与 build.sh 各自真的调用一次架构门禁脚本",
        f"调用行 { {k: [i + 1 for i, _ in v] for k, v in calls.items()} }"
        "（空列表表示只有 CI 跑门禁，本地那句'全部完成'并不证明不变量）",
    )

    def next_command(text: str, idx: int) -> str:
        return next((line.strip() for line in text.splitlines()[idx + 1 :] if line.strip()), "")

    bat_call, sh_call = calls["build.bat"], calls["build.sh"]
    check(
        bool(bat_call)
        and next_command(bat, bat_call[0][0]) == "if errorlevel 1 goto :err"
        and bool(sh_call)
        and any("set -euo pipefail" in line for line in sh.splitlines()[: sh_call[0][0]]),
        "架构门禁失败会中止构建脚本，而不是继续跑后面的步骤",
        f"bat 调用行 {bat_call[:1]} 之后要紧跟 if errorlevel 1 goto :err，"
        f"sh 调用行 {sh_call[:1]} 之前要有 set -euo pipefail",
    )
    cargo_at = {"build.bat": first_cargo(bat), "build.sh": first_cargo(sh)}
    check(
        all(calls[name] and cargo_at[name] is not None and calls[name][0][0] < cargo_at[name]
            for name in calls),
        "架构门禁排在任何 cargo 命令之前（最便宜的全量判据先跑）",
        f"调用行 { {k: [i + 1 for i, _ in v] for k, v in calls.items()} }、"
        f"首个 cargo 行 { {k: (v + 1 if v is not None else None) for k, v in cargo_at.items()} }",
    )


# README 承诺 `./tools/build_python_wheel.ps1` 与 `bash tools/build_python_wheel.sh` 是构建安装包的
# 入口。V12 §17 清点时两条都不通：脚本默认解释器（Windows 的 `python` / POSIX 的 `python3`）往往没有
# pip，而两条脚本都是先跑完 cargo 构建、在最后一步 `pip wheel` 才炸；`.ps1` 里以中文结尾的注释行还会
# 被 Windows PowerShell 5.1 按 ANSI 码页读，行尾字节吞掉换行、把下一行代码并进注释 —— 探测因此从不执行。
def wheel_builder_check() -> None:
    """两个 wheel 构建脚本都得先探测 pip，且 .ps1 的行尾要能被 ANSI 码页读法安全解析。"""
    ps1 = ROOT / "tools" / "build_python_wheel.ps1"
    sh = ROOT / "tools" / "build_python_wheel.sh"
    for path in (ps1, sh):
        text = path.read_bytes().decode("utf-8")
        probe = text.find("-m pip --version")
        build = text.find("-m pip wheel")
        check(
            0 <= probe < build,
            f"{path.name} 在跑 pip wheel 之前先探测 pip",
            f"探测位置 {probe}、pip wheel 位置 {build}：缺探测时构建跑完才在最后一步失败",
        )
    raw = ps1.read_bytes()
    tail_rows = [i for i, line in enumerate(raw.splitlines(), 1) if line and line[-1] >= 0x80]
    check(
        not tail_rows,
        "build_python_wheel.ps1 每行以 ASCII 结尾（中文不留行尾）",
        f"第 {tail_rows} 行以非 ASCII 字节结尾：PS 5.1 按 ANSI 码页读会吞掉换行、把下一行并进注释",
    )
    # README 的 PowerShell 入口必须自带执行策略：Windows 客户端默认禁止运行未签名脚本，
    # `powershell -File tools/build_python_wheel.ps1` 一行代码都不执行就以 UnauthorizedAccess
    # 退出（V12 §18-C #132 实测：本机 `Get-ExecutionPolicy` 各级都是 Undefined，即默认 Restricted）。
    # 只核"入口行"：入口写在代码块里、整行 ASCII，散文里的反例（讲 Restricted 会怎样）带中文，不算。
    entries = [
        line.strip()
        for line in (ROOT / "README.md").read_text(encoding="utf-8").splitlines()
        if "build_python_wheel.ps1" in line
        and line.strip().isascii()
        and ("powershell" in line.lower() or line.strip().startswith("./"))
    ]
    without_policy = [line for line in entries if "-ExecutionPolicy Bypass" not in line]
    check(
        bool(entries) and not without_policy,
        "README 的每条 PowerShell wheel 入口都显式带 -ExecutionPolicy Bypass",
        f"入口行 {len(entries)} 条；缺策略声明 {without_policy}",
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
        # V12 D3 把这里改成递归：集成用例超线时同样只能搬家，而搬家后的形状是
        # `tests/<主题>/main.rs` + 兄弟模块 —— 只数目录顶层会让"拆"直接变成"丢"。
        counts.update(
            {
                f"tests/{path.relative_to(CRATES / crate / 'tests').as_posix()}": len(
                    re.findall(r"^#\[test\]$", path.read_text(encoding="utf-8"), re.MULTILINE)
                )
                for path in sorted((CRATES / crate / "tests").rglob("*.rs"))
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
    # V13 第三十一遍 ②-a/#266：示例配置的查找面也是同一形状的兄弟模块。
    "deploy_lookup",
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
    # 查找面三个入口各自只许有一处定义：默认路径重新分叉（`doctor` 一族回落到仓库、
    # `runtime-check` 一族拼当前目录）就是 V13 第三十一遍 ① 那一轮的起点。
    "resolve_deploy_path": (
        "deploy_lookup.rs",
        r"^(?:pub(?:\(crate\))? )?fn resolve_deploy_path\(",
    ),
    "pick_deploy_file": (
        "deploy_lookup.rs",
        r"^(?:pub(?:\(crate\))? )?fn pick_deploy_file\(",
    ),
    "locate_deploy_file": (
        "deploy_lookup.rs",
        r"^(?:pub(?:\(crate\))? )?fn locate_deploy_file\(",
    ),
    "read_example_json": (
        "deploy_lookup.rs",
        r"^(?:pub(?:\(crate\))? )?fn read_example_json\(",
    ),
    "relocate_deploy_path": (
        "deploy_lookup.rs",
        r"^(?:pub(?:\(crate\))? )?fn relocate_deploy_path\(",
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
    "risk_ratios",
    "run_record",
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
# V12 D6 删掉 `submit_order` / `submit_order_via_gateway{,_with_risk}` 三条死入口后，
# 名单同步收窄；"新增一条无人调用的提交入口"由 dead_public_entry_check 负责拦。
LEG_SUBMIT_ENTRY = re.compile(
    r"(?<![A-Za-z0-9_.])(?P<name>execute_paper_submit_effect"
    r"|submit_order_with_risk"
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


# V12 D6：只有这两族的 `pub fn` 受"零生产消费者即报"约束。
#
# 覆盖面刻意不收全仓：实测全仓口径会报出 21 条既有公共面（库 crate 对外的
# API、被 `pub use` 再导出的条目等），一条从一开始就写着"允许失败"的通用规则
# 只会掩盖本轮真正要防的东西——重复的**入口**。入口族是历史上已经长出过三条死
# 实现的地方（`submit_order` / `submit_order_via_gateway` / `_with_risk`，V12 D6
# 全部删除），所以新增同名形状的入口必须先证明自己有人调用。
PUBLIC_ENTRY_FAMILIES: tuple[tuple[str, re.Pattern[str]], ...] = (
    ("订单提交入口", re.compile(r"^pub fn (submit_order\w*)\s*[<(]", re.MULTILINE)),
    (
        "控制面状态读写",
        re.compile(r"^pub fn ((?:load|save)_control_state)\s*\(", re.MULTILINE),
    ),
)
# 测试现场不算消费者：D6 删掉的 `submit_order_via_gateway` 恰恰只被
# `crates/qx-execution/src/tests/spread_group_barrier.rs` 调用，按"任何引用"计数
# 会把它判成活的，本门禁就退化成摆设。
#
# V13 R22 #222：测试模块在 Rust 里有两种形态——目录式 `src/tests/`（含 `tests/mod.rs`）
# 与单文件式 `src/tests.rs`（V12 R2 为绕开行数棘轮，把八个 crate 的库内用例搬成了后者，
# `#[cfg(test)] mod tests;` 挂进来）。旧写法只认前一种，于是「唯一读者写在 `src/tests.rs`
# 里」的 `pub` 项被当成有生产读者——零读者判据在它们面前是瞎的；同一个过滤器还罩着
# `PUBLIC_ENTRY_FAMILIES`（能力族入口清点）与枚举变体生产者判据，所以这是一条会同时
# 松开三处前提的盲区。两种形态定成一个口径：`(?:^|/)tests\.rs$` 补齐单文件形态，
# 并由 `zero_reference_public_surface_check` 的一颗牙齿钉住（改回旧写法即红）。
TEST_PATH = re.compile(r"(?:^|/)tests/|(?:^|/)test_|_tests\.rs$|(?:^|/)tests\.rs$")


def _production_references(text: str, name: str) -> int:
    """统计一段源码里对 `name` 的引用，跳过注释行。

    注释里写一句"曾经有个 submit_order_xxx"不算有人调用它：判据若按子串计数，
    一条注释就能让死入口长期装活。
    """
    pattern = re.compile(rf"\b{name}\b")
    return sum(
        len(pattern.findall(line))
        for line in text.splitlines()
        if not line.lstrip().startswith("//")
    )


def dead_public_entry_check() -> None:
    """V12 D6：受管入口族里"没有任何生产消费者的 pub fn"即报。

    消费者 = 除定义文件之外、且不在测试路径里的任意一次同名引用（注释行不计）。
    局限如实记录：src 文件内 `#[cfg(test)]` 块里的引用也会被算成生产消费者（文件级
    判据做不到区分），因此这一项防的是"新增一条只有别处测试在调的入口"，不防"只在
    本文件测试里被调的入口"——后者由删除侧的人工清点兜住。
    """
    sources = {
        path.relative_to(ROOT).as_posix(): path.read_text(encoding="utf-8")
        for path in sorted(CRATES.glob("*/**/*.rs"))
    }
    dead: list[str] = []
    for location, source in sources.items():
        for family, pattern in PUBLIC_ENTRY_FAMILIES:
            for name in pattern.findall(source):
                consumers = [
                    other
                    for other, text in sources.items()
                    if other != location
                    and not TEST_PATH.search(other)
                    and _production_references(text, name)
                ]
                if not consumers:
                    dead.append(f"{family}::{name}（{location}）")
    check(
        not dead,
        "受管入口族的每个 pub fn 都有生产消费者（V12 D6：死入口不得留下）",
        f"零生产消费者 {dead or '无'}",
    )


# V12 §17：全仓 `pub fn` / `pub const` 的"零生产读者"判据。
#
# 为什么同文件引用也算读者：本仓库的主导写法是同文件包装链（`connect` 转
# `connect_with_config`、`limits` 读 `effective_limit_up_bp`）。第一轮按"除定义
# 文件之外"计数报出 104 条，其中约六成是这种包装，把包装链当孤儿会逼着清单收下
# 正常公共面；改成"任意生产文本引用"后剩 40 条，逐条人工判定后才落成下面的清单。
# 判据看不见的两类，如实记在这里：①一簇只被同文件另一个死函数调用的入口
# （scheduler 的 `run_cron_tick*` 族），②由宏/字符串路径间接到达的引用（serde 的
# `default = "fn_name"` 因为写在属性行里能被数到，`include!` 之类数不到）。
PUB_SURFACE_DEF = re.compile(r"^\s*pub (?:async )?(?:const (?:fn )?|fn )(\w+)", re.M)
# R4-C 的两处历史盲区：
# ① 旧式 `pub (?:async )?(?:fn|const) (\w+)` 在 `pub const fn` 上会把捕获组落到 `fn` 本身
#    （全仓 23 处 `pub const fn` 因此全部映射成 `crate::fn`，而 `fn` 这个词到处都是，
#    于是"零读者"永远数不出来）；改成 `const (?:fn )?` 先走，`pub const NAME` 与 `pub fn name`
#    仍各归各位。
# ② `pub use` 重导出行曾被当成"生产读者"：重导出只是把名字摆到 crate 根，没有任何调用。
#    `qx-data::load_bar_batch` 正是靠这一行活到本轮——它全仓只有一个调用点，且在 `#[cfg(test)]` 里。
RE_EXPORT_LINE = re.compile(r"^\s*pub(?:\s*\([^)]*\))?\s*use\s")
# V13 R8：`pub(crate) fn` 是上面那颗判据的第二层盲区——正则只认 `pub fn`/`pub const`，
# 于是 crate 内的公共面整层逃过（本仓 500 个 `pub(crate) fn`，其中 data_binding 的
# `validate_research_binding`、strategy_contract 的 `to_json_for` 只有测试读者）。它天生
# 只在本 crate 可见，所以读者只能在本 crate 里数——这一点是构造性的，不需要像 `pub fn`
# 那样为"跨 crate 裸词匹配"付代价值得（那种匹配正是 `qx-storage::after` 被 26 处无关的
# `after` 游标参数保活、而它自己零读者的原因）。
PUB_CRATE_SURFACE_DEF = re.compile(
    r"^\s*pub\s*\(crate\)\s+(?:async\s+)?fn\s+(\w+)",
    re.M,
)
CFG_TEST_MARKER = "#[cfg(test)]"
# 拼出来而不是写死：写出字面量会让下面那条元判据把这句定义本身数成违规。
NAAIVE_TRUNCATION = '.split("' + CFG_TEST_MARKER + '")[0]'

# 允许清单：`crate::name` -> 没有生产读者仍必须留下的理由（含任务号）。
# 每条理由都要能在被引文件里就地复核（本轮 23 条零读者全部逐条查过调用点）。
PUBLIC_SURFACE_ALLOWLIST: dict[str, str] = {
    # 对外契约：README / deploy 文档 / 跨语言 SDK 把这些名字当成公布的能力。
    "qx-plugin::sign_ed25519": "README 公布 Ed25519 签名能力；validate() 的 ed25519 分支是消费侧",
    "qx-storage::consume_batch_with_projection": "deploy/README 公布为外部 reducer 的原子投影+checkpoint 入口",
    # 运维契约的受约束读法：写侧 save_json_at 有越界防线，读侧必须是同一套防线的对称入口。
    "qx-storage::load_json_at": (
        "file/state.rs:18-20 把 `reconcile/<worker-id>.json` 公布为运维契约；"
        "生产读面要么固定文件名要么目录扫描，按相对路径读回的契约只有本入口"
    ),
    "qx-data::add_component": (
        "数据集 Bundle 的程序化装配入口（deploy/README 的三步冻结流程）；"
        "生产侧目前只从盘上 serde 读 Bundle 并过 validate()，故本入口零生产读者"
    ),
    # 断链：能力已实现、生产侧尚未接线；删除等于把缺口藏起来（任务号见理由）。
    "qx-factor::materialize": "任务 #119：因子物化链与 finkit 绑定未接 CLI 生产装配",
    "qx-factor::materialize_incremental": "任务 #119",
    "qx-factor::validate_finkit_artifact": "任务 #119：binding.artifact_digest 与工件摘要的一致性目前只在用例里比过",
    "qx-factor::compile_execution_plan": "任务 #119：计划编译只被 data_binding 用例调用",
    "qx-factor::compute_momentum": "任务 #119：因子计算的另一条入口（与 materialize 同族）",
    "qx-runtime::validate_for_strategy_context": "任务 #53：data_binding 整模块尚无生产装配",
    "qx-scheduler::retry_run": "任务 #129 收口：超时升级已接调度 tick，自动重跑刻意不接（失败时无法判定订单是否已出网），见 capabilities.yaml scheduler_run_retry_has_no_production_path",
    "qx-scheduler::run_cron_tick_with_manifest": "任务 #117：manifest 绑定的 worker 入口与 CLI 自选路径并存",
    "qx-scheduler::run_cron_tick_with_calendar_and_manifest": "任务 #117",
    "qx-scheduler::run_event_with_manifest": "任务 #117",
    "qx-scheduler::run_manual_with_manifest": "任务 #117",
    "qx-scheduler::start_run_with_manifest": "任务 #117",
    "qx-xingban::issuer_capital_at": "任务 #118 收口：快照随报告结构体存在但不进摘要也不进任何读模型，见 capabilities.yaml corporate_action_read_side_has_no_production_reader",
    "qx-data::load_bar_batch": (
        "断链（V13 R4-C 由「重导出即读者」盲区放出来）：批量装载按 request 逐条校验"
        " provider 不得返回越界行，能力在盘但生产入口走 qx-data::ingestion::ingest_bars"
        "（qx-cli/src/dataset_commands.rs:36 在调）；本入口只有 batch.rs 的 #[cfg(test)] 读者，"
        "删掉等于把缺口藏起来"
    ),
    # 2026-10-10 T2-2：`qx-core::default_maker_taker` 的允许清单条目已删除——`qx-app` 的
    # Bar 回测装配（`crates/qx-app/src/cases/run_backtest.rs`）真的调它，它重新有了生产读者，
    # 而「允许清单里的条目仍然没有生产读者」那颗判据要求条目随读者出现而退场。
    # 只剩测试读者的库内 API：用例本身就是回归证明，删面等于删证据。
    "qx-core::cash_dividend_entitlement_for": "任务 #118 收口：登记/结算走 append 的账簿分录，这条只读汇总查询只有用例读者，同上 limitation",
    "qx-core::convertible_bond_interest_entitlement_for": "任务 #118 收口：同 limitation",
    "qx-protocol::upsert_balance": "快照单源用例的唯一写入端",
    "qx-protocol::upsert_balances": "快照单源用例的唯一批量写入端",
    "qx-strategy::try_push_frame": "跨语言环：Rust 只读、worker 写；此入口是 Rust 侧往返证明",
    "qx-storage::for_checkpoint": "跨后端投影原子性用例的唯一构造端",
    # 风控同源契约用例 (a)(b) 两条腿：删掉就等于放弃"三条入口必须一致"的证明面。
    "qx-risk::evaluate_order_with_rules": (
        "contract-tests/tests/risk_parity.rs 的规范腿；生产分派是 RiskGate→evaluate_rules_only"
        "（回测/意图闸门）与 account_limits_only（提交端口），注释已按实说明"
    ),
    "qx-zhenlu::evaluate_order_with_rules": "同上：门面腿，证明 RiskContext 与规范实现逐字段一致",
    "qx-risk::rule_count": "用例取证：配置化的两条规则确实落进同一个闸门（qx-cli/src/tests/backtest_entries.rs）",
    # 跨 crate 用例推进多腿组状态的唯一入口；生产的"同一腿至多提交一次"住在网关 client_id 台账。
    "qx-zhenlu::begin_submission": (
        "qx-execution/qx-cli 用例用它把 SpreadOrderGroup 推到 Submitting；"
        "生产由 qx-execution/src/lib.rs:434-471 的 client_id 台账 + spread_group_barrier 保证"
    ),
    # V13 R8：`pub(crate)` 层新暴露出来的测试构造器。`_strip_cfg_test_items` 只剥离恰好等于
    # `#[cfg(test)]` 的标记行，`#[cfg(all(test, feature = "nats"))]` 这种复合条件剥离不掉，
    # 于是 `EventConsumerHandler::for_test` 看起来像一条生产定义；它的三个字段是本模块私有，
    # `tests/` 作为兄弟模块拿不到写权限，只能由定义模块开一个 test+nats 门控入口（其唯一
    # 读者在 tests/ 下，被 TEST_PATH 排除）。删掉就等于删掉 EventConsumerHandler 的可测性。
    "qx-cli::for_test": (
        "qx-cli/src/event_pipeline.rs 的 `#[cfg(all(test, feature = \"nats\"))]` 门控测试构造器；"
        "复合 cfg 条件剥离不掉，唯一读者在 tests/ 下的 crate 内公共面"
    ),
    # V13 R22 #222：`TEST_PATH` 补齐单文件 `src/tests.rs` 形态后放出来的零读者（8 条）。
    # 共同形状：生产链路走 JSON/字符串入口（`BarFrame::from_json` / `apply_corporate_actions_json`），
    # 这些类型化的程序化入口只有库内用例读者；删面等于把「程序化装配能力」这个缺口藏起来。
    "qx-protocol::to_wire_json": (
        "跨语言契约：文档写明「供 Rust/Python/其他 serde 实现使用的无损 JSON 往返格式」，"
        "Python 侧按同一 serde 形状独立实现，Rust 侧只有用例读者"
    ),
    "qx-protocol::from_wire_json": (
        "跨语言契约同上：wire 入口与稳定 JSON 入口共用同一道版本闸门，读侧只有用例读者"
    ),
    "qx-datastruct::from_view": (
        "程序化装配入口（DataView→BarFrame）；生产 CLI 走 `BarFrame::from_json`（qx-cli/src/backtests/、"
        "dataset_commands.rs、strategy_contract.rs 等处），本入口只有 src/tests.rs 与 tests/frame_contract.rs 读者"
    ),
    "qx-datastruct::close_at": (
        "只剩 src/tests.rs 读者：`close_raw.get(i).copied()` 的具名访问器，用例用它断言帧内容；"
        "删面等于删掉「按索引读收盘价」的可断言入口"
    ),
    "qx-datastruct::select_time_with_manifest": (
        "变换清单链的 Rust 侧入口；Python 桥（python/qianxing_bridge）按同算法镜像实现、"
        "两侧对照是变换清单跨语言一致性的证据面，Rust 侧只有用例读者"
    ),
    "qx-datastruct::resample_with_manifest": (
        "同上：resample 的清单化入口，Python 桥镜像实现，Rust 侧只有用例读者"
    ),
    "qx-xingban::corporate_actions_from_data": (
        "程序化转换入口（数据层公司行为→A 股规则事件）；生产装载走 `apply_corporate_actions_json`"
        "（qx-cli/src/backtests/ashare_binding.rs、runtime_check.rs），本入口只有 src/tests.rs 读者，"
        "见 capabilities.yaml ashare_corporate_action_ledger"
    ),
    "qx-xingban::corporate_action_supported_by_ledger": (
        "账本支持度查询（对 Suspension/CapitalChange 回 false，与其文档一致）；生产读侧未接线，"
        "只有 src/tests.rs 读者。已知边界：`Unknown` 也在匹配集里、返回 true（任务 #118 收口，"
        "见 capabilities.yaml corporate_action_read_side_has_no_production_reader）"
    ),
}


def _production_lines(text: str) -> list[str]:
    """去掉 `#[cfg(test)]` 整项与注释行后的生产文本。

    本文件测试块里的引用不算生产读者：第一轮实测里 `load_json_at` 等入口正是
    "只有本 crate 的 tests 在调"，把它们当活的会让判据退化成摆设。
    """
    return [
        line for line in _strip_cfg_test_items(text) if not line.lstrip().startswith("//")
    ]


def _strip_cfg_test_items(text: str) -> list[str]:
    """按花括号平衡剥掉 `#[cfg(test)]` 标注的整项，其余每一行原样保留。"""
    lines = text.split("\n")
    kept: list[str] = []
    index = 0
    while index < len(lines):
        line = lines[index]
        if line.strip() == CFG_TEST_MARKER:
            index += 1
            while index < len(lines) and "{" not in lines[index] and ";" not in lines[index]:
                index += 1
            depth = 0
            while index < len(lines):
                depth += lines[index].count("{") - lines[index].count("}")
                index += 1
                if depth <= 0:
                    break
            continue
        kept.append(line)
        index += 1
    return kept


def production_text(text: str) -> str:
    """一个源文件的"生产文本"：测试项整项消失、注释行消失，生产代码一行不少。

    这里原先有 18 处"按文件里第一次出现该标记处截断"的读法，隐含假设"测试块是文件里
    唯一且位于末尾的那个标记"。本轮给 `qx-api/src/lib.rs` 中途两个测试专用函数加上该
    属性后，四条判据同时瞎掉：端点表读出 0 条路由（17 条全被说成"只在文档"）、余额
    兜底/404 兜底/schema 常量三条读不到生产代码，而失败方向是"生产代码凭空消失"。
    按项剥作用域才不会把一个标记之后的生产代码连带丢掉。

    同一轮还抓到第二种瞎法：判据读的是代码，喂给它的文本里却留着注释 —— 把
    `transact_control` 里真正的 `sync_control` 调用删成一行注释，判据照绿，因为留下
    的散文里就写着那个名字。注释是给读代码的人看的，不是给判据当证据的。
    """
    return "\n".join(_production_lines(text))


def _fn_body(text: str, signature: str) -> str:
    """从 `signature` 起按花括号平衡取出函数体（用于"某一步必须在事务里发生"这类判据）。"""
    start = text.find(signature)
    if start < 0:
        return ""
    brace = text.find("{", start)
    if brace < 0:
        return ""
    depth = 0
    for index in range(brace, len(text)):
        depth += text[index] == "{"
        depth -= text[index] == "}"
        if depth == 0:
            return text[brace:index]
    return text[brace:]


# V12 §16：三个后端的控制面事务都必须喂只追加的审计链（少一个后端就是"换后端即失去防篡改"）。
CONTROL_TRANSACT_FILES = {
    "文件": "crates/qx-storage/src/file/state.rs",
    "SQLite": "crates/qx-storage/src/sqlite.rs",
    "PostgreSQL": "crates/qx-storage/src/postgres.rs",
}
CONTROL_AUDIT_CASE_FILE = "crates/qx-storage/tests/control_audit_chain_wired.rs"
CONTROL_AUDIT_CASES = (
    "every_committed_control_transaction_extends_the_durable_audit_chain",
    "a_business_rejected_transaction_grows_neither_the_snapshot_nor_the_chain",
    "a_chain_ahead_of_the_snapshot_fails_the_transaction_inst_of_rewriting_history",
    "a_chain_trailing_the_snapshot_reheals_on_the_next_transaction",
    "sqlite_control_transaction_feeds_the_same_audit_table",
)


CONTROL_AUDIT_CALL = ".sync_control("


def _code_body(path: str, signature: str) -> str:
    """一个函数去掉注释与测试项之后的函数体：判据要的是代码，不是散文。

    变异验证抓到过这一点：把 `state.rs` 里真正的 `sync_control` 调用换成一行注释，判据照样
    绿 —— 因为保留下来的上下文注释里就写着"sync_control"这个字。任何"某一步必须发生"的
    判据只要读散文，就能被一句注释满足，方向还是错的那一侧（代码没了、门禁说没事）。
    """
    return _fn_body(production_text((ROOT / path).read_text(encoding="utf-8")), signature)


def _ordered(text: str, *needles: str) -> bool:
    """若干锚点必须按给定顺序先后出现；任一格缺失就是「这一条不成立」，不是异常。

    直接拿 `text.index(...)` 比大小的臂，锚点被改掉时抛的是 ValueError，整份门禁一起崩掉，
    读数从"红"变成"没结论"。本轮枪击实测到的正是这一格：撤掉排空循环的字节上界，崩的是
    门禁本身而不是那一项判据。
    """
    position = -1
    for needle in needles:
        found = text.find(needle, position + 1)
        if found < 0:
            return False
        position = found
    return True


def control_audit_chain_check() -> None:
    """控制面事务必须把审计尾部追加进哈希链（V12 §16 断链：链曾经无人写）。"""
    blind = [
        label
        for label, path in CONTROL_TRANSACT_FILES.items()
        if CONTROL_AUDIT_CALL not in _code_body(path, "pub fn transact_control")
    ]
    check(
        not blind,
        f"三个后端的 transact_control 都在成功提交后调用 `{CONTROL_AUDIT_CALL}` 喂审计链",
        f"缺喂链一步 {blind}：该后端下 audit.json/qx_audit_entries 会永远为空，"
        "而 deploy/README.md 与后端段落把「审计链」写成已覆盖能力",
    )
    cases = (ROOT / CONTROL_AUDIT_CASE_FILE).read_text(encoding="utf-8")
    check(
        all(f"fn {name}(" in cases for name in CONTROL_AUDIT_CASES),
        "审计链接线用例在位：事务喂链、业务拒绝不长链、链超前必拒、链落后自愈",
        f"缺用例 {[name for name in CONTROL_AUDIT_CASES if f'fn {name}(' not in cases]}",
    )


API_SERVE_FILE = "crates/qx-api/src/lib.rs"
API_SERVE_LOOP_ENTRY = "crates/qx-cli/src/strategy_contract.rs"
# 两条 accept 循环（明文 / mTLS）各自都必须经同一个停机出口；只修一条等于没修。
API_ACCEPT_LOOPS = ("pub fn serve(", "pub fn serve_tls_mtls_with_stores(")
API_ACCEPT_LOOP_FILE = "crates/qx-api/tests/accept_loop_shutdown.rs"
API_ACCEPT_LOOP_CASES = (
    "plaintext_accept_loop_serves_then_returns_on_the_stop",
    "plaintext_accept_loop_returns_without_any_connection",
    "mtls_accept_loop_returns_on_the_stop",
)
# 策略意图是三语言共用的一份契约：字段集只在一处声明才是真的契约，第二份清单必然过期。
STRATEGY_INTENT_RUST_FILE = "crates/qx-runtime/src/strategy_contract/contract.rs"
STRATEGY_INTENT_SCHEMA_FILE = "schemas/strategy_api_v1.schema.json"
STRATEGY_INTENT_BRIDGE_FILE = "python/qianxing_bridge/strategy.py"
STRATEGY_INTENT_BRIDGE_CASE_FILE = "python/tests/test_strategy_contract.py"
STRATEGY_INTENT_BRIDGE_CASES = (
    "test_intent_carries_the_three_derivative_fields_rust_emits",
    "test_intent_rejects_derivative_fields_rust_would_reject",
)


def api_accept_loop_exit_check() -> None:
    """API 两条 accept 循环必须有停机出口，且 CLI 把监督器的停机请求接进去（V12 §16 第二遍）。

    `for stream in listener.incoming()` 只在 accept 报错时才结束：Ctrl+C 之后监督器只能
    把 worker 标成"已请求停机"，线程仍卡在 accept 里，`join()` 永不返回，投影线程与 TLS
    重载线程也就永远停不掉。判据读的是剥掉注释后的函数体（散文不算证据）。
    """
    loop_body = _code_body(API_SERVE_FILE, "fn await_connection")
    for signature in API_ACCEPT_LOOPS:
        body = _code_body(API_SERVE_FILE, signature)
        check(
            "Self::await_connection(" in body and "listener.incoming()" not in body,
            f"accept 循环 `{signature}` 经 await_connection 取连接（无 incoming() 死循环）",
            "循环里没有停机出口就回不来，`run serve` 之后 Ctrl+C 停不掉 worker",
        )
    for token in ("stopped()", "set_nonblocking(false)"):
        check(
            token in loop_body,
            f"await_connection 里 `{token}` 在位（停机判定 + accepted socket 回到阻塞态）",
            "少了停机判定则永远停不掉；少了 set_nonblocking(false) 则 Linux 上 accepted "
            "socket 继承监听端非阻塞位，请求读到一半以 WouldBlock 失败",
        )
    serve_call = _code_body(API_SERVE_LOOP_ENTRY, "pub(crate) fn run_runtime_api")
    check(
        serve_call.count("context.should_stop()") >= 2,
        "run_runtime_api 把监督器的停机请求接成两条 serve 的 stopped 闭包",
        "CLI 侧不接 should_stop，则 API 有了出口也没人触发它",
    )
    cases = (ROOT / API_ACCEPT_LOOP_FILE).read_text(encoding="utf-8")
    check(
        all(f"fn {name}(" in cases for name in API_ACCEPT_LOOP_CASES),
        "accept 停机用例在位：明文先服务后停机、明文零连接可停、mTLS 可停",
        f"缺用例 {[name for name in API_ACCEPT_LOOP_CASES if f'fn {name}(' not in cases]}",
    )


ADAPTER_HTTP_FILE = "crates/qx-adapter/src/lib.rs"
ADAPTER_HTTP_TEST_FILE = "crates/qx-adapter/src/tests.rs"
SUPERVISOR_CONTEXT_FILE = "crates/qx-runtime/src/supervision/supervisor.rs"
TOPOLOGY_VALIDATION_FILE = "crates/qx-runtime/src/runtime_config/topology_validation.rs"
HTTP_DEADLINE_CASES = (
    "dribbling_client_is_cut_by_the_overall_request_deadline_not_by_the_per_read_timeout",
    "a_request_inside_the_deadline_still_reads_through_in_one_piece",
    "a_dribbling_refused_peer_cannot_hold_the_accept_thread_past_the_drain_budget",
)
ADAPTER_WALL_CLOCK_CASES = (
    "dripping_upstream_is_cut_by_the_wall_clock_not_by_the_byte_cap",
)


def transport_read_deadline_check() -> None:
    """三条上游读链必须有**整体**时间截止，且截止在循环里逐轮生效（V13 R17-a/b/c）。

    单次读超时（qx-api 的 100 毫秒 `configure_connection`、qx-adapter 的 `set_read_timeout`）
    **每读到一块就复位**：一个每 99 毫秒滴一字的对端永远追不上它。三处症状不同——请求读链把
    一条连接线程加一份 `ConnectionBudget` 额度磨到 1 MiB 总量闸（约 26 小时）才放、拒绝排空链
    跑在唯一的 accept 线程上可以被一路滴、适配器两条上游读链把响应一路吃下去——缺口是同一句
    话：**只有单次读超时，没有整体截止**。所以每格判据钉的都是顺序（截止判定排在读取之前），
    而不是「这条读链上有 timeout」，后者注释里就写着。
    """
    transport = production_code_text(ROOT / API_TRANSPORT_FILE)
    check(
        "pub(crate) const HTTP_REQUEST_DEADLINE: Duration = Duration::from_secs(5);" in transport
        and "pub(crate) const HTTP_REFUSE_DRAIN_DEADLINE: Duration = Duration::from_millis(200);"
        in transport,
        "两条 HTTP 侧截止各自有名有常量（5 秒请求界 / 200 毫秒拒绝排空界）",
        "64 KiB 是字节上界不是时间上界：排空段没有总预算时，一条 socket 就能占死 accept 线程",
    )
    delegating = _code_body(API_TRANSPORT_FILE, "fn read_request<S")
    read_body = _code_body(API_TRANSPORT_FILE, "fn read_request_until")
    check(
        "read_request_until(stream, Instant::now() + HTTP_REQUEST_DEADLINE)" in delegating
        and "stream.read(&mut buffer)" not in delegating,
        "read_request 只负责给 read_request_until 递上截止（读取形状只有一处，界不会在两处分叉）",
        "两处各读一遍就有一处忘判截止——本轮之前正是这个形状：读链只有单次超时",
    )
    check(
        _ordered(
            read_body,
            "loop {",
            "if Instant::now() >= deadline",
            "let count = stream.read(&mut buffer)?;",
        ),
        "read_request_until 每轮先判整体截止再做本轮读（截止在循环之内，不是入口判一次）",
        "1 MiB 按 99 毫秒一字滴完约 26 小时，期间占着一条连接线程与一份连接预算，预算格占满后整站对外 503",
    )
    refused = _code_body(API_TRANSPORT_FILE, "fn refuse_connection")
    check(
        refused.count("write_http_response(&mut writer, response, &[])") == 1
        and "stream.shutdown(std::net::Shutdown::Write)" in refused
        and "Instant::now() + HTTP_REFUSE_DRAIN_DEADLINE" in refused
        and "while drained < 64 * 1024 {" in refused,
        "拒绝连接：写响应 -> 半关写方向 -> 带预算与字节上界地排空",
        "带未读缓冲直接 close 会以 RST 收场，客户端读到的是「连接被重置」而不是那条有名字的 503",
    )
    check(
        _ordered(
            refused,
            "stream.shutdown(std::net::Shutdown::Write)",
            "Instant::now() + HTTP_REFUSE_DRAIN_DEADLINE",
            "while drained < 64 * 1024 {",
        ),
        "排空必须在半关之后、且 200 毫秒预算从开始排空那刻起算",
        "反过来先排空就两边互等；预算若含写出与半关，正常拒绝也会把排空窗口提前吃光",
    )
    check(
        refused.count("if Instant::now() >= deadline") == 1
        and _ordered(
            refused,
            "while drained < 64 * 1024 {",
            "if Instant::now() >= deadline",
            "match writer.read(&mut discard)",
        ),
        "排空循环每轮自判截止（refuse 走的线程从没装过 read_timeout，滴字节时读不出错）",
        "只在入口判一次的话，每 99 毫秒滴一字的对端能让这条循环永远追不上截止",
    )
    adapter = production_code_text(ROOT / ADAPTER_HTTP_FILE)
    handshake = _code_body(ADAPTER_HTTP_FILE, "fn read_header_block")
    response = _code_body(ADAPTER_HTTP_FILE, "fn read_capped_response")
    check(
        "fn read_header_block<R: Read>(reader: &mut R, budget: Duration)" in adapter
        and "read_header_block(&mut session.stream, timeout)?" in adapter,
        "WSS 握手块的预算做成参数、调用点把 timeout 交进去",
        "只剩 `bytes.len() > 64 * 1024` 一臂时，滴满 64 KiB 之前这条链没有出口，而字节界只挡内存不挡时间",
    )
    for label, body, reader_call in (
        ("握手块", handshake, "read_exact(&mut byte)"),
        ("响应体", response, ".read(&mut buffer)"),
    ):
        check(
            _ordered(
                body,
                "let started = Instant::now();",
                "loop {",
                "if started.elapsed() >= budget",
                reader_call,
            ),
            f"适配器上游{label}读链：整体预算逐轮判且排在读取之前",
            "set_read_timeout 每读到一块就复位，滴水的上游能借它把连接无限续期，所以时间界要在循环之内",
        )
    check(
        adapter.count("read_capped_response(&mut stream, MAX_HTTP_RESPONSE_BYTES, self.timeout)")
        == 2,
        "两条 HTTP 读链（TLS 与明文）都把 self.timeout 交给整体预算，不是一处补一处漏",
        "两条 send 共用同一个读函数：只补一条，另一条照旧无界读下去，而走哪条由运行时的 scheme 决定",
    )
    api_cases = (ROOT / API_TRANSPORT_FILE).read_text(encoding="utf-8")
    missing_api = [name for name in HTTP_DEADLINE_CASES if f"fn {name}(" not in api_cases]
    check(
        not missing_api,
        "HTTP 截止用例在位：滴字节的请求被整体截止切断、界内请求照旧读通、滴字节的被拒对端拖不住 accept 线程",
        f"缺用例 {missing_api}（逐颗点名，缺哪臂就红哪臂）",
    )
    adapter_cases = (ROOT / ADAPTER_HTTP_TEST_FILE).read_text(encoding="utf-8")
    missing_adapter = [
        name for name in ADAPTER_WALL_CLOCK_CASES if f"fn {name}(" not in adapter_cases
    ]
    check(
        not missing_adapter,
        "适配器上游时间臂用例在位（握手块与响应体两条臂，外加界内正向对照）",
        f"缺用例 {missing_adapter}（少了正向对照就分不出「补上截止」与「把所有上游一律切断」）",
    )


def stop_aware_worker_tick_sleep_check() -> None:
    """worker 的节拍等待要切片并片间问停机令牌，上界仍由配置校验域给（V13 R17-f）。

    `context.sleep_ms(interval_ms)` 原先整段睡满：一次 SIGTERM 要等满这一拍才被读到，而
    `qx-runtime` 那边已经把停机令牌接进了循环，看上去「能停」，实际多久才停由 `tick_interval_ms`
    决定（校验域上限 300 秒）。同一轮拆掉的是 `interval_ms.min(1_000)`：那格钳位让配置里写的
    节拍在执行面上什么都不约束（L4 同族），现在等待照抄配置、总长上界由校验域给。
    """
    context_body = _code_body(SUPERVISOR_CONTEXT_FILE, "pub fn sleep_ms")
    check(
        _ordered(
            context_body,
            "while slept_ms < total_ms && !self.should_stop()",
            "(total_ms - slept_ms).min(200)",
            "std::thread::sleep",
        ),
        "WorkerContext::sleep_ms 按 200 毫秒切片、每片之前问停机令牌",
        "判定排在睡之后，最坏一次 SIGTERM 要多等一整片；整段睡满则最坏等满 300 秒才应停",
    )
    check(
        "std::thread::sleep(std::time::Duration::from_millis(total_ms))" not in context_body
        and "while slept_ms < total_ms {" not in context_body,
        "sleep_ms 没退化成整段睡满，也没退化成不读令牌的循环（两条旧形状各钉一次）",
        "这两条臂读的的位置不同：切片与终止条件是同一个 while，睡多久与问不问令牌是两处，并一条就会漏掉第二种改法",
    )
    tick_body = _code_body(SCHEDULER_WORKER_FILE, "fn run_scheduler_worker")
    check(
        "let interval_ms = config.scheduler.tick_interval_ms;" in tick_body
        and "context.sleep_ms(interval_ms);" in tick_body
        and "interval_ms.min(" not in tick_body,
        "调度 worker 照抄 tick_interval_ms 等待，不再就地钳成 1 秒",
        "钳位让那格配置在执行面上什么都不约束：运维按配置算的节拍与实际节拍是两回事（L4 同族）",
    )
    check(
        "if self.scheduler.tick_interval_ms == 0 || self.scheduler.tick_interval_ms > 300_000"
        in production_code_text(ROOT / TOPOLOGY_VALIDATION_FILE),
        "tick_interval_ms 的 1..=300000 校验域仍在位（切片等待的总长上界以此为据）",
        "去掉校验域，切片只保证「每 200 毫秒问一次」，而这个数最终要睡多久就没有上界了",
    )




def health_snapshot_knob_check() -> None:
    """`runtime-check` 的健康快照必须用真时钟与专用过期预算（V12 §16 #122）。

    全仓唯一的生产读者是 `collect_runtime_check_report`，它曾传 `0` 当当前时刻、
    `shutdown_timeout_ms` 当过期窗口：`now_ms.saturating_sub(heartbeat)` 于是不可能大于
    任何正数，`HealthRegistry::snapshot` 的过期→Degraded 分支在这唯一的读者身上结构性
    不成立；而"多久算心跳陈旧"在 serve 侧另有专用口径 `messaging.worker_stale_after_ms`
    （`api_service` 的 `read_worker_metrics`），同一问题不该有两个窗口。判据读代码不读散文。
    """
    body = _code_body(RUNTIME_CHECK_FILE, "fn collect_runtime_check_report")
    check(
        "runtime_timestamp_ms()" in body
        and "config.messaging.worker_stale_after_ms" in body
        and ".snapshot(0" not in body
        and "shutdown_timeout_ms" not in body,
        "runtime-check 的 snapshot 传真时钟 + worker_stale_after_ms，不传 0/停机预算",
        "传 0 则过期判定恒不成立（健康快照的 stale 分支没有生产读者）；"
        "传 shutdown_timeout_ms 则同一心跳新鲜度问题在仓库里有两个互相矛盾的窗口",
    )


def strategy_intent_three_language_check() -> None:
    """策略意图的字段集必须在 Rust / JSON schema / Python SDK 三侧同时相等（V12 §16 第三遍）。

    R4-k 当年把 `schemas/strategy_api_v1.schema.json` 逐字段对齐到 Rust，却没有任何判据把
    Python SDK 也钉进来，于是 Rust 后来加的 `margin_mode`/`position_mode`/`leverage` 只在
    两侧存在：Rust 写出这三个键（未设置时是 JSON null，键始终在），Python 的
    `_reject_unknown_keys` 照 `deny_unknown_fields` 的口径把它们判成未知键 —— 同一份契约
    自己产出的载荷自己读不回来，多腿衍生品策略跨语言往返直接 ValueError。三侧比对的口径：
    两侧都从"字段声明"这一事实取集合，不写第二份手工清单。
    """
    rust = production_text(
        (ROOT / STRATEGY_INTENT_RUST_FILE).read_text(encoding="utf-8")
    )
    rust_block = _fn_body(rust, "pub struct StrategyContractIntent {")
    rust_fields = set(re.findall(r"^\s*pub ([a-z_0-9]+):", rust_block, re.MULTILINE))
    schema = json.loads((ROOT / STRATEGY_INTENT_SCHEMA_FILE).read_text(encoding="utf-8"))
    schema_fields = set(schema["$defs"]["intent"]["properties"])
    python_text = (ROOT / STRATEGY_INTENT_BRIDGE_FILE).read_text(encoding="utf-8")
    python_block = python_text.split("class StrategyIntent:", 1)[-1].split("\nclass ", 1)[0]
    python_fields = set(
        re.findall(r"^    ([a-z_0-9]+): \S[^\n]*$", python_block, re.MULTILINE)
    )
    for name, group in (
        ("Python SDK", python_fields),
        ("JSON schema", schema_fields),
    ):
        check(
            group == rust_fields,
            f"策略意图字段集 {name} 与 Rust StrategyContractIntent 逐项相等",
            f"多 {sorted(group - rust_fields) or '无'}、少 {sorted(rust_fields - group) or '无'}",
        )
    check(
        {"margin_mode", "position_mode", "leverage"} <= python_fields,
        "Python 意图能表达多腿的保证金/持仓模式与杠杆（Rust 写出的键读得回来）",
        "缺任一字段则 Rust 自己产出的 intent 会被 Python 判成未知键",
    )
    cases = (ROOT / STRATEGY_INTENT_BRIDGE_CASE_FILE).read_text(encoding="utf-8")
    check(
        all(f"def {name}(" in cases for name in STRATEGY_INTENT_BRIDGE_CASES),
        "跨语言意图往返用例在位：三字段随 Rust 产出可读回、Rust 拒绝的取值 Python 也拒绝",
        f"缺用例 {[name for name in STRATEGY_INTENT_BRIDGE_CASES if f'def {name}(' not in cases]}",
    )


def gate_source_truncation_check() -> None:
    """门禁自己不许再按"第一次 `#[cfg(test)]`"截断源文件（V12 §16 的元判据）。"""
    self_source = Path(__file__).read_text(encoding="utf-8")
    naive = self_source.count(NAAIVE_TRUNCATION)
    check(
        naive == 0,
        "门禁读生产文本一律经 production_text 按项剥测试代码，不再按第一次 cfg(test) 截断",
        f"仍有 {naive} 处 `{NAAIVE_TRUNCATION}`：中途一个测试专用属性就能让后面的生产代码从判据里消失",
    )


def merge_duplicate_block_check() -> None:
    """V12 §21 / #138：合流悄悄接出来的第二份判据必须当场点名（两条）。

    git 合并"双方各自在同一个函数尾部追加一段"时不产生任何冲突标记：上游那份原样接在我方
    那份之后，文本合法、`check()` 照跑，于是同一条不变量印出两行 PASS，而"门禁至少执行 N 条
    判据"这个量具本身被抬高——条数地板只咬下跌侧，抓不到虚增。§20 合流出来的那一份就是这么
    进来的，第一次运行以 `GATE_EXIT=1` 却 0 条 FAIL 崩在中途（`NameError: reader`）才暴露。
    `crates/*/src` 那一侧不并入这条：实测同样的逐字重复窗口在 318 份受版本控制源码里 W=10
    命中 781 处、W=24 仍有 37 处，绝大多数是三条 venue 提交链、几家存储后端这类**合法的并列
    实现**，零容忍只会立一条假判据；Rust 侧重复的函数/常量/`mod` 由编译器直接拒，所以那半边
    的口径是"合流类提交必须跑整树 `cargo test --workspace --all-targets`"（V12 §20.3），
    而 Python 门禁这一侧没有编译器兜底，只有这条能常驻咬住。
    """
    windows: dict[tuple[str, tuple[str, ...]], int] = {}
    duplicated: list[str] = []
    own = Path(__file__).resolve()
    targets = sorted({*sorted((ROOT / "tools").glob("*.py")), own}, key=lambda p: str(p))
    for path in targets:
        rel = (
            path.relative_to(ROOT).as_posix()
            if path.is_relative_to(ROOT)
            else path.name
        )
        lines = [
            line.rstrip("\r\n").rstrip()
            for line in path.read_text(encoding="utf-8").splitlines()
        ]
        for start in range(max(0, len(lines) - MERGE_DUP_WINDOW + 1)):
            window = tuple(lines[start : start + MERGE_DUP_WINDOW])
            informative = sum(
                1
                for line in window
                if len(line) >= 12 and not MERGE_DUP_TRIVIAL.match(line)
            )
            if informative < MERGE_DUP_WINDOW - 2:
                continue
            key = (rel, window)
            first = windows.setdefault(key, start)
            if first != start:
                duplicated.append(f"{rel}:{first + 1}=={start + 1}")
    check(
        not duplicated,
        "tools 下的门禁脚本没有逐字重复的连续窗口：同一段判据只存在一份",
        f"疑似合流接出来的第二份 {duplicated}",
    )

    labels: dict[str, list[int]] = {}
    literal_labels = 0
    for node in ast.walk(ast.parse(Path(__file__).read_text(encoding="utf-8"))):
        if not (
            isinstance(node, ast.Call) and getattr(node.func, "id", None) == "check"
        ) or len(node.args) < 2:
            continue
        second = node.args[1]
        if isinstance(second, ast.Constant) and isinstance(second.value, str):
            literal_labels += 1
            labels.setdefault(second.value, []).append(node.lineno)
    shared = {text: sites for text, sites in labels.items() if len(sites) > 1}
    check(
        literal_labels >= GATE_LABEL_FLOOR and not shared,
        "每条判据的描述文本只写一次：两条 PASS 共用一句话就是判据被抄了第二份",
        f"字面描述 {literal_labels} 条（少于地板 {GATE_LABEL_FLOOR} 说明取数方式失效），重复描述 {shared}",
    )


def _production_sources() -> dict[str, list[str]]:
    """全仓生产文本：剔掉 `#[cfg(test)]` 整项与注释行，再排除 tests/ 文件。两条零读者判据共用。"""
    return {
        path.relative_to(ROOT).as_posix(): _production_lines(path.read_text(encoding="utf-8"))
        for path in sorted(CRATES.glob("*/src/**/*.rs"))
        if not TEST_PATH.search(path.relative_to(ROOT).as_posix())
    }


def _collect_surface_definitions(
    sources: dict[str, list[str]], def_pattern: re.Pattern
) -> dict[str, list[tuple[str, int, str]]]:
    definitions: dict[str, list[tuple[str, int, str]]] = {}
    for location, lines in sources.items():
        crate = location.split("/")[1]
        for number, line in enumerate(lines, 1):
            match = def_pattern.match(line)
            if match:
                definitions.setdefault(f"{crate}::{match.group(1)}", []).append(
                    (location, number, line)
                )
    return definitions


def _surface_orphans_and_stale(
    definitions: dict[str, list[tuple[str, int, str]]],
    sources: dict[str, list[str]],
    *,
    same_crate_only: bool,
) -> tuple[list[str], list[str]]:
    """同一套读者算术、两种可见性：`pub fn` 按全仓裸词数（跨 crate 的无关同名会保活死函数，
    那是 `qx-storage::after` 活下来的原因），`pub(crate) fn` 只能在本 crate 数（构造性正确）。"""
    orphans: list[str] = []
    stale: list[str] = []
    for key, sites in sorted(definitions.items()):
        crate, name = key.split("::", 1)
        pattern = re.compile(rf"\b{name}\b")
        definition_lines = {line for _, _, line in sites}
        readers = sum(
            1
            for location, lines in sources.items()
            for line in lines
            if (not same_crate_only or location.split("/")[1] == crate)
            and line not in definition_lines
            and RE_EXPORT_LINE.match(line) is None
            and pattern.search(line)
        )
        if readers == 0 and key not in PUBLIC_SURFACE_ALLOWLIST:
            where = "、".join(f"{location}:{number}" for location, number, _ in sites)
            orphans.append(f"{key}（{where}）")
        elif readers and key in PUBLIC_SURFACE_ALLOWLIST:
            stale.append(key)
    return orphans, stale


def zero_reference_public_surface_check() -> None:
    """V12 §17：全仓 `pub fn`/`pub const` 必须有生产读者，或在允许清单里写明理由。"""
    sources = _production_sources()
    definitions = _collect_surface_definitions(sources, PUB_SURFACE_DEF)
    orphans, stale = _surface_orphans_and_stale(definitions, sources, same_crate_only=False)
    check(
        not orphans,
        "每个 pub fn/pub const 都有生产读者，或在允许清单里写明理由（V12 §17）",
        f"零读者 {orphans or '无'}",
    )
    check(
        not stale,
        "允许清单里的条目仍然没有生产读者（长出读者就要从清单删掉）",
        f"清单过期 {stale or '无'}",
    )
    # R4-C：两处历史盲区各钉一颗。第①颗同时验「源码真值」（没有任何条目被捕获成 `fn`）
    # 与「正则契约」（`pub const fn` 与 `pub const NAME` 各归各位）；第②颗验 `pub use` 行被排除、
    # 而同名普通语句不被误排除。只改一行代码就能把这两颗盲区原样放回去。
    check(
        not any(key.endswith("::fn") for key in definitions)
        and PUB_SURFACE_DEF.match("    pub const fn foo() -> Self {").group(1) == "foo"
        and PUB_SURFACE_DEF.match("    pub const BAR_COUNT: usize = 4;").group(1) == "BAR_COUNT"
        and PUB_SURFACE_DEF.match("    pub async fn call(&mut self) {").group(1) == "call"
        and RE_EXPORT_LINE.match("pub use batch::{load_bar_batch, BarBatchItem};") is not None
        and RE_EXPORT_LINE.match("    let load_bar_batch = 1;") is None,
        "零读者判据的两处历史盲区仍然闭合：`pub const fn` 捕获到函数名而不是 `fn`，`pub use` 重导出行不算生产读者（V13 R4-C）",
        f"被捕获成 fn 的条目 {[k for k in definitions if k.endswith('::fn')] or '无'}、"
        f"`pub const fn` 捕获={PUB_SURFACE_DEF.match('    pub const fn foo() -> Self {{').group(1)!r}、"
        f"`pub const NAME` 捕获={PUB_SURFACE_DEF.match('    pub const BAR_COUNT: usize = 4;').group(1)!r}、"
        f"重导出行被识别={RE_EXPORT_LINE.match('pub use batch::{{load_bar_batch}};') is not None}、"
        f"普通语句被误排除={RE_EXPORT_LINE.match('    let load_bar_batch = 1;') is not None}",
    )
    # V13 R22 #222：测试模块的两种形态必须都认得。只认目录式 `src/tests/` 时，
    # 「唯一读者写在单文件 `src/tests.rs` 里」的 `pub` 项被当成有生产读者——判据当场变瞎，
    # 同一个过滤器罩着的 `PUBLIC_ENTRY_FAMILIES` 与枚举变体生产者两条判据也一并松开前提。
    check(
        TEST_PATH.search("crates/qx-protocol/src/tests.rs") is not None
        and TEST_PATH.search("crates/qx-protocol/src/tests/mod.rs") is not None
        and TEST_PATH.search("crates/qx-protocol/src/tests/frame.rs") is not None
        and TEST_PATH.search("crates/qx-protocol/src/lib.rs") is None
        and TEST_PATH.search("crates/qx-protocol/src/contest.rs") is None
        and TEST_PATH.search("crates/qx-protocol/src/attestation.rs") is None,
        "TEST_PATH 认得测试模块的两种形态：目录式 `src/tests/` 与单文件式 `src/tests.rs`（V13 R22 #222）",
        f"单文件式被识别={TEST_PATH.search('crates/qx-protocol/src/tests.rs') is not None}、"
        f"目录式被识别={TEST_PATH.search('crates/qx-protocol/src/tests/mod.rs') is not None}、"
        f"生产文件被误判={TEST_PATH.search('crates/qx-protocol/src/lib.rs') is not None}、"
        f"含 test 词干的生产文件被误判={TEST_PATH.search('crates/qx-protocol/src/contest.rs') is not None}",
    )


def zero_reference_pub_crate_surface_check() -> None:
    """V13 R8：`pub(crate) fn` 必须有本 crate 生产读者，或在允许清单里写明理由。"""
    sources = _production_sources()
    definitions = _collect_surface_definitions(sources, PUB_CRATE_SURFACE_DEF)
    orphans, stale = _surface_orphans_and_stale(definitions, sources, same_crate_only=True)
    check(
        not orphans,
        "每个 pub(crate) fn 都有本 crate 生产读者，或在允许清单里写明理由（V13 R8）",
        f"零读者 {orphans or '无'}",
    )
    check(
        not stale,
        "pub(crate) 允许清单里的条目仍然没有本 crate 生产读者（V13 R8）",
        f"清单过期 {stale or '无'}",
    )
    check(
        PUB_CRATE_SURFACE_DEF.match("pub(crate) fn foo(&self) -> u64 {").group(1) == "foo"
        and PUB_CRATE_SURFACE_DEF.match("    pub(crate) async fn bar() {").group(1) == "bar"
        and PUB_CRATE_SURFACE_DEF.match("    pub fn not_crate() {") is None
        and PUB_CRATE_SURFACE_DEF.match("    pub(crate) struct Foo {") is None,
        "pub(crate) 判据只捕获 crate 内函数名，不把 `pub fn` 与 `pub(crate) struct` 算进来（V13 R8）",
        "正则契约成立",
    )


ASHARE_RULES_FILE = "crates/qx-xingban/src/ashare.rs"
ASHARE_PIT_TEST_FILE = "crates/qx-xingban/tests/ashare_pit_asof.rs"


# V12 §19 #135：零读者判据只数 `pub fn`/`pub const`，于是三类"类型面"孤儿全从它眼皮底下走过去了：
# `qx-data/src/calendar.rs` 那一整张 trait（V11 §5 早就写明"全仓零实现"，却因为它不是 `pub fn`
# 而没人再数过它，并且它和 qx-scheduler 里那张真的在被用的 `TradingCalendar` 结构体同名 —— 按名字
# 数读者的判据会把对方的引用当成自己的读者，永远数出"有人用"）、`qx-guanxing` 的 `NumericExt`/
# `to_display`（trait 方法连 `pub` 关键字都不带，定义之外全仓零出现），以及 §18-B 删掉静态目录后
# 只剩自己判定臂的 `QualityIssue::CrossedBook`（#134）。两颗都收口掉，并把"同名类型只许有一个定义"
# 立成判据 —— 这是那条盲区唯一能被自动咬住的形式。
DEAD_TYPE_NAMES = ("NumericExt", "to_display")
CALENDAR_TYPE_NAME = "TradingCalendar"
CALENDAR_OWNER_CRATE = "crates/qx-scheduler"


def dead_type_surface_check() -> None:
    """被删的类型面不许复活，且日历这个名字全仓只有一个定义。"""
    sources = {
        path.relative_to(ROOT).as_posix(): _production_lines(path.read_text(encoding="utf-8"))
        for path in sorted(CRATES.glob("*/src/**/*.rs"))
        if not TEST_PATH.search(path.relative_to(ROOT).as_posix())
    }
    revived = sorted(
        {
            f"{location}:{name}"
            for location, lines in sources.items()
            for name in DEAD_TYPE_NAMES
            for line in lines
            if re.search(rf"(?<![A-Za-z0-9_]){name}(?![A-Za-z0-9_])", line)
        }
    )
    check(
        not revived,
        "NumericExt/to_display 这类「定义之外零出现」的类型面已删除且不复活",
        f"重新出现 {revived or '无'}",
    )
    definitions = sorted(
        f"{location}:{line.strip()[:60]}"
        for location, lines in sources.items()
        for line in lines
        if re.match(rf"^\s*pub\s+(?:struct|enum|trait)\s+{CALENDAR_TYPE_NAME}\b", line)
    )
    check(
        len(definitions) == 1 and definitions[0].startswith(CALENDAR_OWNER_CRATE),
        f"同名日历类型只有一个定义，且就在真的驱动调度的 {CALENDAR_OWNER_CRATE}",
        f"定义 {definitions or '无'}（qx-data 那张零实现的同名 trait 会在这里被数第二次）",
    )


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
    # V13 R1-A1（FN3 的正解）：除权除息日的锚不能只有"数据侧手工给"这一个出口 ——
    # 仓库已经把红利/送转/配股装载进 corporate_actions，锚却读不到它们。
    fold_start = trading.find("fn ex_rights_reference(")
    fold_body = trading[fold_start : trading.find("fn effective_limit_up_bp(", fold_start)]
    check(
        fold_start > 0
        and ".corporate_actions" in fold_body
        and "is_cash_dividend_action" in fold_body
        and "cash_dividend_raw" in fold_body
        and "rights_issue_price_raw" in fold_body
        and "self.price_tick" in fold_body,
        "除权除息折算读已装载的公司行为，结果落到最小报价单位",
        "ex_rights_reference 不再从 corporate_actions 折算红利/送转/配股",
    )
    check(
        "self.ex_rights_reference(raw, current.ts)" in anchor_body
        and anchor_body.index("bars[..index]")
        < anchor_body.index("self.ex_rights_reference(raw, current.ts)"),
        "折算只在跨日推导之后发生，手工覆盖表仍是锚的第一个出口",
        "previous_close 把推导出的昨收交回给折算这一步丢了",
    )
    check(
        all(
            case in unit_test
            for case in (
                "fn ex_rights_anchor_folds_the_cash_dividend_loaded_with_the_rules",
                "fn ex_rights_anchor_folds_dividend_bonus_shares_and_rights_issue_together",
                "Some(yuan(950))",
                "Some(yuan(688))",
            )
        ),
        "除权除息锚有行为用例：红利扣减到 9.50、红利+送转+配股一次折算到 6.88",
        f"{ASHARE_RULE_TEST_FILE} 不再咬住 FN3 的折算口径",
    )
    # V13 R1-A1 的第四颗判据：折算的**输入**必须有非测试写点。锚能算对不等于它收得到事实 ——
    # 若只有测试在装载公司行为，那么生产链上的除权日照样按不复权处理，而全树用例仍全绿。
    # 因此这里只数"实例方法调用点"，且把整份测试模块一并排除：定义处（前面没有 `.`）、
    # 注释行、文件尾 `#[cfg(test)]` 模块内、以及 `src/**/tests.rs` 这类整文件即测试的
    # 路径一律不算；`self.` 上的那一跳是定义处向 `_with_report` 的内部委托，不是写点。
    # 第一版漏掉了测试整文件与内部委托两类，M5/M6 两发变异实测把判据喂成假绿（本轮日志）。
    producers = []
    for path in rust_sources():
        if path.stem == "tests" or path.stem.startswith("tests_") or "tests" in path.parts:
            continue
        lines = path.read_text(encoding="utf-8").splitlines()
        for number, line in enumerate(lines):
            if line.lstrip().startswith("//") or is_test_scoped(number, lines):
                continue
            if re.search(r"(?<!self)\.apply_corporate_actions_json(?:_with_report)?\s*\(", line):
                producers.append(path.relative_to(CRATES).as_posix())
    cli_writers = sum(1 for name in producers if name.startswith("qx-cli/"))
    check(
        cli_writers >= 2,
        "公司行为装载有非测试写点：回测绑定与运行自检各一处",
        f"qx-cli 生产代码里的调用点为 {cli_writers} 处（期望 >=2），全部写点 {sorted(set(producers)) or '无'}",
    )


# V13 R1-A2：Python `qianxing_ashare` 与 Rust 读侧共用一套 A 股线格式。
#
# 两侧各自手抄了同一份字段名册与动作名册，此前**没有任何判据比过它们**：Python 写侧加
# 一个字段、或 Rust 读侧改一个白名单，全树用例照样绿，直到真数据进来才在装载处炸。这里
# 钉三层：名册逐项相等（含 Rust 声明的数组长度）、词表要能被写侧自己认回、以及一份两侧
# 共读的对照夹具（`python/tests/fixtures/ashare_actions_cross_check.*`）在场且两侧都读它。
ASHARE_PYTHON_FILE = "python/qianxing_ashare/__init__.py"
ASHARE_CROSS_STEM = "ashare_actions_cross_check"
ASHARE_CROSS_FIXTURES = (
    f"{ASHARE_CROSS_STEM}.rows.json",
    f"{ASHARE_CROSS_STEM}.payload.json",
    f"{ASHARE_CROSS_STEM}.expectations.json",
)
ASHARE_CROSS_PY_TEST = "python/tests/test_ashare_cross_language_contract.py"
FOLD_ANCHOR_TEST = "python_written_actions_fold_into_the_shared_ex_rights_reference"


def _literal_items(text: str, header: str, opener: str, closer: str) -> list[str]:
    """抓出一段字面量集合里的字符串项，顺序保持；找不到声明时返回空。"""

    start = text.find(header)
    if start < 0:
        return []
    try:
        # 先跳过声明的等号：Rust 那侧写成 `const X: [&str; 5] = [...]`，直接从名字后面
        # 找方括号会命中类型标注里的那一对，抓出来的就不是名册。
        opened = text.index(opener, text.index("=", start))
        closed = text.index(closer, opened)
    except ValueError:
        return []
    return re.findall(r'"([a-z0-9_]+)"', text[opened:closed])


def _snake_case(name: str) -> str:
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


def ashare_cross_language_contract_check() -> None:
    """A 股线格式的两侧名册必须逐项相等，且这份相等有共读夹具兜着。"""
    python_text = (ROOT / ASHARE_PYTHON_FILE).read_text(encoding="utf-8")
    rust_text = (ROOT / ASHARE_RULES_FILE).read_text(encoding="utf-8")
    py_test = (ROOT / ASHARE_CROSS_PY_TEST).read_text(encoding="utf-8")
    rs_test = (ROOT / ASHARE_RULE_TEST_FILE).read_text(encoding="utf-8")

    python_version = re.search(r"(?m)^ASHARE_SCHEMA_VERSION = (\d+)$", python_text)
    rust_version = re.search(r"(?m)^pub const ASHARE_SCHEMA_VERSION: u32 = (\d+);", rust_text)
    check(
        python_version is not None
        and rust_version is not None
        and python_version.group(1) == rust_version.group(1),
        "A 股契约版本号两侧同一格",
        f"Python={python_version and python_version.group(1)} "
        f"Rust={rust_version and rust_version.group(1)}",
    )

    for header, surface in (
        ("ASHARE_ACTION_ENVELOPE_FIELDS", "信封顶层"),
        ("ASHARE_ACTION_FIELDS", "单条公司行为"),
        ("ASHARE_CALENDAR_FIELDS", "交易日历"),
    ):
        py_items = _literal_items(python_text, f"{header} = ", "(", ")")
        rs_items = _literal_items(rust_text, f"const {header}", "[", "]")
        declared = re.search(rf"const {header}: \[&str; (\d+)\]", rust_text)
        mismatched = sorted(set(py_items) ^ set(rs_items))
        check(
            bool(py_items)
            and bool(rs_items)
            and declared is not None
            and len(py_items) == len(rs_items) == int(declared.group(1))
            and py_items == rs_items,
            f"{surface}字段名册两侧逐项一致（{len(rs_items)} 项，含顺序与 Rust 声明长度）",
            f"Python {len(py_items)} 项 / Rust {len(rs_items)} 项"
            f"（Rust 声明 {declared.group(1) if declared else '缺失'} 项）；"
            + (
                f"差异 {mismatched}"
                if mismatched
                else "项集相同但顺序不同"
                if py_items and rs_items
                and set(py_items) == set(rs_items)
                and py_items != rs_items
                else ""
            ),
        )

    python_types = _literal_items(python_text, "_CORPORATE_ACTION_TYPES = ", "{", "}")
    enum_start = rust_text.find("pub enum AshareCorporateActionType {")
    enum_body = (
        rust_text[enum_start : rust_text.find("}", enum_start)] if enum_start >= 0 else ""
    )
    rust_types = [
        _snake_case(name) for name in re.findall(r"(?m)^    ([A-Z][A-Za-z0-9]*),$", enum_body)
    ]
    check(
        bool(python_types)
        and bool(rust_types)
        and sorted(python_types) == sorted(rust_types)
        and len(rust_types) >= 16,
        f"公司行为动作名册两侧同一份（Rust 变体的 snake_case 名，{len(rust_types)} 个）",
        f"Python {len(python_types)} 个 / Rust {len(rust_types)} 个；"
        f"差异 {sorted(set(python_types) ^ set(rust_types)) or '无'}",
    )
    # 名册相等还不够：写侧的中文别名表是子串匹配，认不回自己的线格式名时
    # `suspension`/`new_share_issue` 会退化 unknown，停牌与增发语义在读回时丢掉（本轮实测）。
    canonical_body = python_text[
        python_text.find("def _canonical_action_type(") : python_text.find("def _bare_code(")
    ]
    check(
        "if text in _CORPORATE_ACTION_TYPES" in canonical_body,
        "写侧的行读取器认得自己写出的每一个动作名",
        f"{ASHARE_PYTHON_FILE} 的 _canonical_action_type 不再短路返回线格式名",
    )
    check(
        "test_every_canonical_action_name_round_trips_through_the_row_reader" in py_test,
        "动作名往返有行为用例兜着（名册里每个名字逐个认回）",
        f"{ASHARE_CROSS_PY_TEST} 丢掉了名册往返用例",
    )

    # 定点单位口径必须单源：`*_raw` 是已定点整数，其余别名按元/股乘 SCALE。
    normalize_body = python_text[
        python_text.find("def normalize_corporate_action_rows(") : python_text.find(
            "def _invoke_rows_method("
        )
    ]
    amount_calls = normalize_body.count("_amount_pick(")
    check(
        normalize_body.count("_nonnegative_scaled(") == 0
        and normalize_body.count("_nonnegative_raw(") == 0
        and amount_calls >= 14
        and "def _amount_pick(" in python_text
        and '_nonnegative_raw(raw, f"{field}_raw")' in python_text
        and "_nonnegative_scaled(_optional_pick(row, *aliases), field)" in python_text,
        f"A 股行数据的定点单位口径只有一处规则（`*_raw` 已定点，别名按元/股；{amount_calls} 个字段走它）",
        f"normalize_corporate_action_rows 里又出现直接乘 SCALE 的读法（_amount_pick 只覆盖 {amount_calls} 处）",
    )
    date_body = python_text[
        python_text.find("def _date_text(") : python_text.find("def _nonnegative_scaled(")
    ]
    check(
        'if " " in text:' in date_body
        and 'elif "T" in text:' in date_body
        and 'text.split(" ", 1)[0]' in date_body
        and 'text.split("T", 1)[0]' in date_body,
        "公告日期认得空格与 `T` 两种 ISO 分隔，不会回落成除权日",
        f"{ASHARE_PYTHON_FILE} 的 _date_text 又只切空格，带时区的时间戳会被丢成 ex_date",
    )
    check(
        all(
            name in py_test
            for name in (
                "test_iso_timestamp_announcement_date_is_not_redated_to_the_ex_date",
                "test_raw_named_input_columns_are_taken_as_already_scaled",
            )
        ),
        "日期与单位两处口径各有行为用例钉住",
        f"{ASHARE_CROSS_PY_TEST} 丢掉了日期/单位往返用例",
    )

    # 共读的对照夹具：三份文件在位，且两侧按同一个词根引用它。
    present = [
        name
        for name in ASHARE_CROSS_FIXTURES
        if (ROOT / "python" / "tests" / "fixtures" / name).exists()
    ]
    check(
        len(present) == len(ASHARE_CROSS_FIXTURES),
        f"跨语言对照夹具齐备（{len(ASHARE_CROSS_FIXTURES)} 份）",
        f"在场 {present}，期望 {list(ASHARE_CROSS_FIXTURES)}",
    )
    # 词根要逐字对上：只查子串时 `STEM = "..._cross_check_forked"` 照样算引用了同一份。
    check(
        f'STEM = "{ASHARE_CROSS_STEM}"' in py_test
        and f"{ASHARE_CROSS_STEM}.payload.json" in rs_test
        and f"{ASHARE_CROSS_STEM}.expectations.json" in rs_test,
        "两侧读的是同一份对照夹具（Python 词根与 Rust 夹具名逐字相同）",
        f"{ASHARE_CROSS_PY_TEST} 的 STEM 不再是 {ASHARE_CROSS_STEM}，或 {ASHARE_RULE_TEST_FILE} "
        f"不再按整名读 {ASHARE_CROSS_STEM} 的夹具",
    )
    # 折算锚那格必须是夹具里的期望值：把它抄成 `Some(7_000_000_000)` 字面量，跨语言对照
    # 就地失效（Rust 改口径时再无东西会红），所以要在断言现场查有没有裸数字。
    fold_start = rs_test.find(FOLD_ANCHOR_TEST)
    fold_body = rs_test[fold_start : rs_test.find("\n}\n", fold_start)] if fold_start >= 0 else ""
    literal_anchor = [
        line.strip()
        for line in fold_body.splitlines()
        if line.strip().startswith("Some(") and re.search(r"Some\(\s*[\d_]+\s*\)", line)
    ]
    check(
        all(
            case in rs_test
            for case in (
                "fn python_written_action_envelope_round_trips_into_the_rust_reader",
                FOLD_ANCHOR_TEST,
                'expectations["document"]',
                "expected_reference_raw",
            )
        )
        and bool(fold_body)
        and not literal_anchor,
        "Rust 读侧把信封读回并喂进折算锚，期望值取自共读的那份夹具",
        f"{ASHARE_RULE_TEST_FILE} 不再核对 Python 写出的 v1 信封"
        + (f"；锚定期望值被抄成字面量 {literal_anchor}" if literal_anchor else ""),
    )
    check(
        all(
            name in py_test
            for name in (
                "test_blessed_payload_is_what_the_python_writer_produces",
                "test_expectations_are_derived_from_the_blessed_payload",
                "test_anchor_reference_is_recomputed_from_the_payload_events",
            )
        ),
        "Python 侧在进程内重算 payload、期望值与折算锚，夹具不可手抄",
        f"{ASHARE_CROSS_PY_TEST} 丢掉了重算比对用例",
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
# V12 R1 §4.20 补两个**排队**入口：它们不亲自送单，但订单形状在入队之前就已定死，
# 所以按 `worker=None` 走同一条闸门。漏登记一个，就是"配了规则却没人执行"的下一个藏身处。
ASHARE_SUBMIT_CALL_SITES = {
    "crates/qx-cli/src/worker_entry.rs": ("binance-worker", "ccxt-worker"),
    "crates/qx-cli/src/venue_runtime/paper_worker.rs": ("paper-worker",),
    "crates/qx-cli/src/venue_runtime/paper_submit.rs": ("paper-submit-order",),
    "crates/qx-cli/src/venue_runtime/binance_submit.rs": ("binance-submit-order",),
    "crates/qx-cli/src/workers.rs": ("strategy-worker",),
    "crates/qx-cli/src/api_service.rs": ("serve",),
}


def ashare_submit_guard_check() -> None:
    """钉四件事：闸门只有一处定义、五个提交入口与两个排队入口各问一次、问的顺序早于副作用、
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
            f"{path.split('/')[-1]} 每个提交/排队入口都问过 A 股闸门（{'、'.join(labels)}）",
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
                "strategy_worker_entry_refuses_the_ashare_section_before_creating_any_state",
                "api_surface_refuses_the_ashare_section_before_creating_data_dir",
            )
        )
        and "WorkerRole::Strategy" in cases
        and "WorkerRole::MarketData" in cases,
        "进程内用例逐条咬住 Q65 口径（五个提交入口 + 两个排队入口的拒绝、不提交角色放行、副作用顺序）",
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
        # 按调用名计数，不按 `= 名字` 的形状：换行折行会让后者数到 0，而这条判据要问的
        # 只是"这条链问过读点没有、问了几次"。
        check(
            text.count("apply_configured_builtin_signal(") == 1,
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


# V12 #102：四个信号旋钮按 kind 生效，清单是唯一的说法来源。
BUILTIN_KNOB_TABLE_FILE = "crates/qx-strategy/src/builtin_signal.rs"
BUILTIN_KIND_FILES = {
    # 三处判决点 + 一处播报：清单外的一项既改不动结果，也没有拒一轮的权力。
    "crates/qx-strategy/src/builtin.rs": "内核读法与参数体检",
    "crates/qx-runtime/src/runtime_config/strategy_validation.rs": "运行时配置侧体检",
    "crates/qx-cli/src/strategy_binding.rs": "`--config` 侧播报",
    "crates/qx-cli/src/cli_help.rs": "`builtin-strategies` 列表",
}
BUILTIN_KNOB_TEST_FILE = "crates/qx-strategy/tests/builtin_signal_knobs.rs"
BUILTIN_KNOB_CLI_TEST_FILE = "crates/qx-cli/src/tests/backtest_signal_provenance.rs"


def builtin_knob_list_check() -> None:
    """kind→旋钮清单单一来源，两侧成对用例，播报与摘要都落"哪几项真的上场"。

    钉四件事：清单定义全仓唯一一处，四个消费点都问过它；曾经把"不上场的窗口"变成结果的
    两条暗通道（按 `slow_window` 开历史上限、按 `period` 给不读周期的 kind 计预热）不得复活；
    播报行与摘要块各写 `knobs` / `declared_unused` 两格；内核那侧的成对行为用例（清单外改不动
    结果 / 清单内改得动结果 / 清单覆盖 17 个 kind）与命令面那侧的两条产物用例都在。
    """
    table = (ROOT / BUILTIN_KNOB_TABLE_FILE).read_text(encoding="utf-8")
    definitions = {
        path.relative_to(ROOT).as_posix()
        for path in sorted(CRATES.glob("*/src/**/*.rs"))
        if not ({"tests"} <= set(path.parts) or "test" in path.stem)
        and "fn signal_knobs(" in non_test_source(path.read_text(encoding="utf-8"))
    }
    check(
        definitions == {BUILTIN_KNOB_TABLE_FILE},
        "kind→旋钮清单全仓只定义一处（第二处就会与各消费点的读法分叉）",
        f"定义文件 {sorted(definitions) or '无'}",
    )
    check(
        all(
            knob in table
            for knob in ("FastWindow", "SlowWindow", "Period", "ThresholdBps")
        )
        and "MACD_WINDOWS" in table
        and "HISTORY_FLOOR_BARS" in table,
        "清单所在的那一份同时持有四个旋钮、MACD 常数与历史上限地板（暗通道的两个源头）",
        "builtin_signal.rs 不再持有四个旋钮、MACD 常数或历史地板",
    )
    for rel, label in BUILTIN_KIND_FILES.items():
        text = (ROOT / rel).read_text(encoding="utf-8")
        check(
            any(
                needle in text
                for needle in ("signal_knobs(", "uses_signal_knob(", "signal_knob_list(")
            ),
            f"{rel} 问过清单（{label}）",
            "这一处不再按 kind 取用信号参数，清单外的一项会重新变成暗通道",
        )
    runtime_validation = (
        ROOT / "crates/qx-runtime/src/runtime_config/strategy_validation.rs"
    ).read_text(encoding="utf-8")
    guarded = runtime_validation.count("uses(qx_strategy::builtin_signal::BuiltinSignalKnob::")
    check(
        guarded == 4,
        "运行时配置侧的四个旋钮体检逐项问过清单（少一项=有一项重新无条件拒掉一轮）",
        f"问过清单的体检 {guarded} 项（期望 4）",
    )
    kernel = (ROOT / "crates/qx-strategy/src/builtin.rs").read_text(encoding="utf-8")
    history_body = re.search(r"fn max_history\(&self\)[^{]*\{(.*?)\n    \}", kernel, re.S)
    history_body = history_body.group(1) if history_body else ""
    check(
        "signal_knobs(" in history_body
        and "HISTORY_FLOOR_BARS" in history_body
        and history_body.count("self.config.slow_window") <= 1
        and history_body.count("self.config.period") <= 1,
        "历史上限只经清单取窗口，每个窗口至多读一次（`slow_window.max(period)` 曾让不读窗口的 kind 换结果）",
        "max_history 不再问清单，或把某个清单外的窗口直接混进了上限",
    )
    warmup_body = re.search(r"fn required_bars\(&self\)[^{]*\{(.*?)\n    \}", kernel, re.S)
    warmup_body = warmup_body.group(1) if warmup_body else ""
    check(
        all(
            arm in warmup_body
            for arm in ("BasisArbitrage | SpotFuturesArbitrage => 1", "Grid => 1")
        ),
        "不读周期/窗口的 kind 预热门槛不跟着旋钮走（基差与 grid 各占一行，等一个不读的指标=白等）",
        "required_bars 又给不读旋钮的 kind 按 period 计预热",
    )
    help_text = (ROOT / CLI_HELP_FILE).read_text(encoding="utf-8")
    check(
        "knobs=" in help_text and "declared_unused" in help_text,
        "帮助文本说出 knobs / declared_unused 两格的含义",
        "cli_help.rs 不再描述 #102 的生效面",
    )
    # 整份文件数子串会被注释喂饱：把 `builtin-strategies` 那一列从 println! 里删掉、
    # 只留注释里那句"knobs=…"，上一版判据照样绿（M5 反向验证抓到的）。
    check(
        help_text.count("knobs={}") == 1,
        "`builtin-strategies` 列表真把那列印进格式串（不是只写在注释里）",
        "cli_help.rs 里 `knobs={}` 出现 0 次或多于 1 次：生效列没了或有了第二处",
    )
    wrapper = (ROOT / BUILTIN_SIGNAL_WRAPPER_FILE).read_text(encoding="utf-8")
    artifacts = (ROOT / "crates/qx-cli/src/backtests/artifacts.rs").read_text(encoding="utf-8")
    # 同上：摘要块的两格要落在 json! 里，而且是**未被注释的那一行**。只数子串的话，
    # 把 `"declared_unused": …` 前面加两个斜杠就同时骗过了编译与判据（M2 抓到的）。
    def emitted_key(block: str, key: str) -> int:
        return sum(
            1
            for line in block.splitlines()
            if (stripped := line.strip()).startswith(f'"{key}":')
            and not stripped.startswith("//")
        )

    signal_block = re.search(
        r'summary\["signal"\] = serde_json::json!\(\{(.*?)\n\s*\}\);', artifacts, re.S
    )
    signal_block = signal_block.group(1) if signal_block else ""
    check(
        wrapper.count("knobs={}") == 1
        and wrapper.count("declared_unused={}") == 1
        and emitted_key(signal_block, "knobs") == 1
        and emitted_key(signal_block, "declared_unused") == 1,
        "播报行与摘要 signal 块各写一次「这一轮真正生效的是哪几项」",
        "生效面只剩数值（或只剩注释），读者又会以为 fast_window=5 真的选了东西",
    )
    cases = (ROOT / BUILTIN_KNOB_TEST_FILE).read_text(encoding="utf-8")
    cli_cases = (ROOT / BUILTIN_KNOB_CLI_TEST_FILE).read_text(encoding="utf-8")
    check(
        all(
            f"fn {name}(" in cases
            for name in (
                "knobs_outside_the_list_cannot_change_a_single_run",
                "every_knob_on_the_list_can_still_change_the_run",
                "macd_has_no_tunable_signal_knob_and_still_signals",
                "the_list_covers_every_builtin_kind",
            )
        )
        and all(
            f"fn {name}(" in cli_cases
            for name in (
                "knobs_outside_the_list_neither_fail_the_run_nor_change_the_summary_numbers",
                "the_list_names_the_kind_knobs_regardless_of_the_declaration",
            )
        ),
        "两侧成对用例都在：清单外改不动结果、清单内改得动结果、清单覆盖每个 kind、产物点名生效面",
        f"{BUILTIN_KNOB_TEST_FILE} 或 {BUILTIN_KNOB_CLI_TEST_FILE} 不再覆盖 #102",
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
        production = production_text((ROOT / path).read_text(encoding="utf-8"))
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
        '"schema_version": 5' in artifact
        and '"log_digest"' in artifact
        and '"ledger_entries"' in artifact
        and '"run_ledger_entries"' in artifact,
        "摘要把重放做过的三件事写成人各自可核对的字段",
        "replay 结论块缺键或 schema 版本回退",
    )
    depth = production_text((ROOT / REPLAY_ENGINE_FILES[1]).read_text(encoding="utf-8"))
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
INPUT_PROV_REPORT_FILE = "crates/qx-cli/src/report_command.rs"
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
        artifact.count('"schema_version": 5') == 1
        and artifact.count('"input": input_provenance_json(&input.input)') == 1
        and artifact.count("fn input_provenance_json(") == 1,
        "摘要以当前 schema 版本落一个 `input` 块，且这份形状只由 input_provenance_json 写一次",
        "input 块的写法出现多处或 schema/键名回退",
    )
    chain_parses = sum(
        production_text((ROOT / path).read_text(encoding="utf-8")).count(
            "Frame::from_json("
        )
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
    report = production_text((ROOT / INPUT_PROV_REPORT_FILE).read_text(encoding="utf-8"))
    # V12 R1 把正文的排版搬进 report_readout.rs：`input_verified=` 那一格的前缀住在读法里，
    # "没声明"这个结论仍由报告出口给出。两半各查一次，缺一半就红。
    readout_lines = (ROOT / REPORT_READOUT_MODULE).read_text(encoding="utf-8")
    check(
        report.count("recompute_declared_backtest_input(&summary)?") == 1
        and "not_declared" in report
        and 'input_verified={input_verified}' in readout_lines,
        "报告出口把复核失败向上抛，且旧 schema 只能被说成「没声明」而不是「已核对」",
        "run_report 不再以 `?` 传播复核结果、不再区分未声明，或读法侧丢了 input_verified 那一格",
    )
    single = production_text((ROOT / INPUT_PROV_SINGLE_FILE).read_text(encoding="utf-8"))
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
API_PROJECTION_BRIDGE_FILE = "crates/qx-cli/src/market_bridges.rs"
SNAPSHOT_ENDPOINT_FILE = "crates/qx-api/src/lib.rs"
SNAPSHOT_CLI_CASE_FILE = "crates/qx-cli/src/tests/api_snapshot_money_fields.rs"
SNAPSHOT_CORE_CASE_FILE = "crates/qx-protocol/tests/snapshot_single_source"
# 账户快照的八个汇总钱字段（`raw` 是整数量纲）。权益排首位是历史写法；其余七格在协议里
# 同样是 `Option<i128>`，只是权益"晚到"（V11 Q70）故单独排。**这份名单是全量**，
# 与下面"这一层算不出哪几格"的子集（`SNAPSHOT_UNCOMPUTED_FIELDS`）不是一回事。
SNAPSHOT_SCALARS = (
    "equity_raw",
    "available_raw",
    "margin_raw",
    "frozen_raw",
    "realized_pnl_raw",
    "unrealized_pnl_raw",
    "fees_raw",
    "funding_raw",
)
SNAPSHOT_OPTIONAL_FIELDS = tuple(name for name in SNAPSHOT_SCALARS if name != "equity_raw")
# 没有来源可算、必须停在 `None` 的那三个；`available_raw`/`fees_raw` 各有自己的算点，
# `realized_pnl_raw`/`unrealized_pnl_raw` 自 V13 R26 起由 Ledger 投影接上生产者。
SNAPSHOT_UNCOMPUTED_FIELDS = (
    "margin_raw",
    "frozen_raw",
    "funding_raw",
)
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
    writer = production_text((ROOT / DATASTRUCT_FRAME_FILE).read_text(encoding="utf-8"))
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


# V12 §18-B（#112/#114）：静态 BarFrame 曾经挂着一通往 `qx-provider` ProviderRegistry 的桥，
# 但全仓没有任何生产入口把帧注册进去（唯一的调用者是一条用例），而桥里的 `fetch` 把
# `query.end` 当成"数据到达时间"写进 `receive_time`/`received_at` 并哈希进血缘 ——
# 一个不可能失败的 PIT 断言。删掉整条桥才是诚实做法，这里钉住它不许复活。
BAR_FRAME_BRIDGE_NAMES = (
    "with_received_at",
    "with_registry_capability",
    "registry_received_at",
    "registry_capability",
    "default_registry_capability",
    "RegistryDataProvider",
)
# 帧读侧只该报告列数据与契约版本；这四个名字一旦出现就意味着有人又把
# "查询窗口末端"或"外部填的接收时间"接回这条链。
BAR_FRAME_PIT_FIELDS = ("as_of", "receive_time", "received_at", "unwrap_or(query.end)")
DATA_PROVIDER_MANIFEST = "crates/qx-data/Cargo.toml"
BRIDGE_ONLY_EDGES = ("qx-provider", "qx-guanxing")


def bar_frame_pit_honesty_check() -> None:
    """静态帧不声明接收时间，也没有把查询窗口冒充入库时间的注册入口（V12 §18-B）。"""
    revived = sorted(
        {
            f"{path.relative_to(ROOT).as_posix()}:{name}"
            for path in sorted(CRATES.glob("*/**/*.rs"))
            for name in BAR_FRAME_BRIDGE_NAMES
            if re.search(
                rf"(?<![A-Za-z0-9_]){name}(?![A-Za-z0-9_])", path.read_text(encoding="utf-8")
            )
        }
    )
    check(
        not revived,
        "帧到 ProviderRegistry 的桥与它的接收时间旋钮都不存在，全仓无一处复活",
        f"复活 {revived}",
    )
    provider = production_text((ROOT / DATA_PROVIDER_FILE).read_text(encoding="utf-8"))
    pit_fields = [name for name in BAR_FRAME_PIT_FIELDS if name in provider]
    check(
        not pit_fields,
        "帧读侧不声明数据到达时间：没有入口能把查询窗口末端写成 PIT 可见性",
        f"帧读侧出现 {pit_fields}",
    )
    manifest = (ROOT / DATA_PROVIDER_MANIFEST).read_text(encoding="utf-8")
    edges = [edge for edge in BRIDGE_ONLY_EDGES if f"{edge} = " in manifest]
    check(
        not edges,
        "qx-data 不再为一条没人走的注册桥背着 qx-provider/qx-guanxing",
        f"依赖边仍在 {edges}",
    )
    capabilities = (ROOT / "maturity/capabilities.yaml").read_text(encoding="utf-8")
    block_lines = capabilities.splitlines()
    start = next(
        (index for index, line in enumerate(block_lines) if line == "  ashare_json_provider:"),
        -1,
    )
    frame_block: list[str] = []
    for line in block_lines[start + 1:] if start >= 0 else []:
        if not line.startswith("    "):
            break  # 下一个 2 空格键就是条目边界，越过它会把别人条目的证据算进来
        frame_block.append(line)
    check(
        bool(frame_block) and not any("qx-provider" in line for line in frame_block),
        "能力矩阵不再拿 qx-provider 充当帧注册入口的证据",
        "帧条目里又出现注册桥证据或缺少条目本体",
    )


EVENT_EVIDENCE_FILE = "crates/qx-cli/src/readiness.rs"
EVENT_EVIDENCE_TEST_FILE = "crates/qx-cli/src/tests/event_backtest_evidence.rs"
# 研究快照的三个生产读点：worker 侧 Python/builtin 契约、native 合约读侧、API 就绪判定。
EVENT_EVIDENCE_READERS = (
    "crates/qx-cli/src/strategy_binding.rs",
    "crates/qx-cli/src/strategy_contract.rs",
    EVENT_EVIDENCE_FILE,
)


def event_backtest_evidence_check() -> None:
    """`event_verified` 必须由本地真实 RunManifest 复核，不能是快照里手抄的一格数字（V12 §18-B #115）。

    研究快照来自仓库外，`event_verified`/`event_manifest_digest` 就是别人写下的声明。声明本身
    不可信不是问题，问题是运行时曾经**只看声明**：`validate_for(..., require_event_verified)`
    只核对"这格 bool 是 true"，于是生产闸门把一个编造的 `u64` 当成了事件回测证据。本轮把复核
    接到回测产物上（`runs/*.run.json`），并用 qx-factor 的盖章入口算摘要，让口径只有一处定义。
    """

    def tight(path: str) -> str:
        return re.sub(r"\s+", "", production_text((ROOT / path).read_text(encoding="utf-8")))

    binding_body = _fn_body(tight(EVENT_EVIDENCE_FILE), "pub(crate)fnvalidate_research_snapshot_binding")
    check(
        "verify_event_backtest_evidence(root,research)" in binding_body,
        "研究快照绑定入口一定会走事件回测复核，不是摆在旁边等人想起来调用",
        f"绑定入口体内没看到复核调用：{binding_body[:120]}",
    )
    readers = {
        path: tight(path).count("validate_research_snapshot_binding(root,") for path in EVENT_EVIDENCE_READERS
    }
    stale_two_arg = {path: tight(path).count("validate_research_snapshot_binding(&config.strategy") for path in EVENT_EVIDENCE_READERS}
    check(
        all(count == 1 for count in readers.values()) and not any(stale_two_arg.values()),
        "三个研究快照读点都把 runtime root 交给绑定入口，没有第二处绕开复核",
        f"带 root 的调用 {readers}；旧的两参调用 {stale_two_arg}",
    )
    verify_body = _fn_body(tight(EVENT_EVIDENCE_FILE), "pub(crate)fnverify_event_backtest_evidence")
    check(
        "RunManifest::from_json(" in verify_body
        and "mark_event_verified(&manifest)" in verify_body
        and "stamped.event_manifest_digest!=candidate.event_manifest_digest" in verify_body
        and "manifest.digest()" not in verify_body,
        "证据按文件内容重算摘要，且摘要口径取自 qx-factor 的盖章入口而不是运行时另抄一份",
        f"复核函数形状不符：{verify_body[:160]}",
    )
    check(
        "if!candidate.event_verified{returnOk(());}" in verify_body
        and "candidate.event_manifest_digest.ok_or_else(" in verify_body
        and "clock_start>candidate.validation_start||manifest.clock_end<candidate.validation_end"
        in verify_body
        and '"strategy_version"' in verify_body
        and '"data_fingerprint"' in verify_body,
        "没声明的不拦、声明了缺摘要的当场拒、清单必须同血缘且覆盖整个验证窗口",
        "复核的放行/拒绝条件被改动：缺席口径、配对、区间或血缘点名任一不在位",
    )
    test_text = (ROOT / EVENT_EVIDENCE_TEST_FILE).read_text(encoding="utf-8")
    refusals = ("指不到真实产物", "按内容重算", "不符", "没有覆盖 candidate 的验证窗口")
    check(
        len(re.findall(r"#\[test\]", test_text)) >= 6
        and all(phrase in test_text for phrase in refusals),
        "事件回测证据复核有四类拒绝与一类放行的行为用例（门禁只看文本，用例才证明它会拒）",
        f"用例 {len(re.findall(r'#\[test\\]', test_text))} 条；缺少的拒绝口径 "
        f"{[phrase for phrase in refusals if phrase not in test_text] or '无'}",
    )



# V12 §19 #134：§18-B 删掉静态目录与 `QualityGate::check_quotes` 之后，`QualityIssue::CrossedBook`
# 只剩 `verdict()` 里自己那条臂 —— 一个永远构造不出来的"致命"变体，读代码的人会以为质量闸门
# 还判交叉报价。交叉判定的真读者只有一处（Binance 报价入口的 `is_crossed()`），所以这里两侧都咬：
# 枚举里每个变体必须有构造它的生产者，而 `CrossedBook` 不许在任何 crate 的生产代码里复活。
QUALITY_ISSUE_FILE = "crates/qx-guanxing/src/lib.rs"
CROSSED_QUOTE_NAME = "CrossedBook"
CROSSED_QUOTE_READER_FILE = "crates/qx-adapter/src/binance.rs"


def quality_issue_producer_check() -> None:
    """质量问题的每个变体都真的有人构造，交叉报价判定只剩 adapter 那一处读者。"""
    text = (ROOT / QUALITY_ISSUE_FILE).read_text(encoding="utf-8")
    lines = _production_lines(text)
    variants: list[str] = []
    inside = False
    for line in lines:
        if not inside:
            inside = bool(re.match(r"^\s*pub enum QualityIssue\b", line))
            continue
        if re.match(r"^\s*}\s*$", line):
            inside = False
            continue
        name = re.match(r"^\s*([A-Z][A-Za-z0-9]*)", line.strip())
        if name and not line.strip().startswith(("#", "///", "//")):
            variants.append(name.group(1))
    # 构造上下文：`issues.push(QualityIssue::X`、`vec![QualityIssue::X`、`issues: vec![…`。
    # `matches!(… QualityIssue::X { .. })` 这种判定臂不算构造者。
    producer = re.compile(r"(?:push\s*\(\s*|vec!\s*\[|issues\s*:\s*vec!\[)\s*QualityIssue::([A-Z][A-Za-z0-9]*)")
    produced = set()
    for line in lines:
        produced.update(producer.findall(line))
    unproduced = sorted(set(variants) - produced)
    check(
        len(variants) >= 8 and not unproduced,
        "QualityIssue 每个变体都有构造它的生产者（判定臂不算）",
        f"枚举变体 {len(variants)} 个；无生产者 {unproduced or '无'}",
    )
    revived = sorted(
        {
            path.relative_to(ROOT).as_posix()
            for path in sorted(CRATES.glob("*/src/**/*.rs"))
            if any(
                CROSSED_QUOTE_NAME in line
                for line in _production_lines(path.read_text(encoding="utf-8"))
            )
        }
    )
    adapter_lines = _production_lines((ROOT / CROSSED_QUOTE_READER_FILE).read_text(encoding="utf-8"))
    adapter_reads = sum(1 for line in adapter_lines if re.search(r"\.is_crossed\(\)", line))
    check(
        not revived and adapter_reads >= 1,
        "交叉报价只在 adapter 的报价入口判，质量闸门不再声明一个构造不出来的致命变体",
        f"复活于 {revived or '无'}；adapter 的 is_crossed() 读者 {adapter_reads} 处",
    )


# V12 §23 / #137：变体级生产者判据从"点名 QualityIssue 一个枚举"扩到全仓 `pub enum`。
# 为什么必须按**限定名**计数，本轮量到了实例：`ServiceStatus::Degraded` 在监管侧出现 14 次，
# 于是按裸名计数的扫描看不见 `ConnectorState::Degraded` 一次也没被赋过值 —— 同词不同枚举会
# 替孤儿变体伪造生产者。同一把尺子反过来也成立：`RecvTimeoutError::Disconnected` 掩盖了
# `ConnectorState::Disconnected`。因此本判据只认 `Enum::Variant`，以及 `impl Enum` 块体内
# 指向同一类型的 `Self::Variant`（按花括号配平取块，见 `_impl_blocks`）。
ENUM_HEADER = re.compile(r"^\s*pub enum (\w+)")
ENUM_VARIANT_LINE = re.compile(r"^([A-Z][A-Za-z0-9]*)\s*,?\s*$")
# 覆盖面地板：解析退化（例如有人给枚举加了跨行变体）会让"零变体"看起来全绿，所以条数本身要咬。
ENUM_SURFACE_FLOOR = 50
ENUM_VARIANT_SURFACE_FLOOR = 200
# 允许清单：`Enum::Variant` -> 为什么生产代码里永远数不到它的限定名，它却必须留着。
# 本清单只收"线格式生产者"一种理由，因此每条都要能被"该枚举确实 derive 了 Deserialize"
# 独立复核（见下面第三条判据），否则允许清单就成了任意 widened 的口子。
LINE_FORMAT_VARIANTS: dict[str, str] = {
    "CorporateActionType::Split": "qx-data/src/corporate_action.rs 带 serde rename_all=snake_case，"
                                  "线格式 \"split\" 即生产者；A 股侧落 unsupported 臂显式拒",
    "CorporateActionType::Merge": "同上，线格式 \"merge\"",
    "CorporateActionType::NewShareIssue": "同上，线格式 \"new_share_issue\"",
    "CorporateActionType::Repurchase": "同上，线格式 \"repurchase\"",
    "CorporateActionType::ConvertibleBondConversion": "同上，线格式 \"convertible_bond_conversion\"",
    "CorporateActionType::Suspension": "同上，线格式 \"suspension\"",
}


def _impl_blocks(text: str, type_name: str) -> str:
    """`impl Type` 与 `impl Trait for Type` 的花括号配平块体，供 `Self::Variant` 归类用。"""
    bodies: list[str] = []
    for header in re.finditer(rf"impl[^\n]*\b{re.escape(type_name)}\s*(?:<[^>\n]*>)?\s*\{{", text):
        depth = 0
        for index in range(header.end() - 1, len(text)):
            depth += 1 if text[index] == "{" else -1 if text[index] == "}" else 0
            if depth == 0:
                bodies.append(text[header.end() - 1 : index + 1])
                break
    return "\n".join(bodies)


def enum_variant_producer_check() -> None:
    """全仓每个 `pub enum` 变体都必须有人写它的限定名，除非它的生产者在线格式上。"""
    production = {
        path: _production_lines(path.read_text(encoding="utf-8"))
        for path in sorted(CRATES.glob("*/src/**/*.rs"))
        # `src/tests/**` 是挂在 `#[cfg(test)] mod tests;` 下的用例文件，整目录排除：
        # 否则"只有用例在写这个变体"会被数成生产者，判据退化成摆设。
        if "tests" not in path.parts
    }
    joined = {path: "\n".join(lines) for path, lines in production.items()}
    declared: dict[str, str] = {}
    deserializable: set[str] = set()
    for path, lines in production.items():
        for index, line in enumerate(lines):
            header = ENUM_HEADER.match(line)
            if not header:
                continue
            enum = header.group(1)
            # 只往回吃紧邻的属性行与注释行：`use serde::{Deserialize, …}` 那行不算证据，
            # 否则"枚举其实没 derive Deserialize"也能被文件顶部的 import 骗绿（M_F 实测）。
            cursor = index - 1
            attributes: list[str] = []
            while cursor >= 0 and lines[cursor].lstrip().startswith("#"):
                attributes.append(lines[cursor])
                cursor -= 1
            if any("Deserialize" in attribute for attribute in attributes):
                deserializable.add(enum)
            cursor = index + 1
            while cursor < len(lines) and not re.match(r"^\s*\}", lines[cursor]):
                variant = ENUM_VARIANT_LINE.match(lines[cursor].strip())
                if variant:
                    declared[f"{enum}::{variant.group(1)}"] = path.relative_to(ROOT).as_posix()
                cursor += 1
    # 限定名引用一次也数不到 = 既构造不出来、也没有任何一条判定臂认它。
    # `impl Enum` 块内的 `Self::Variant` 是同一个限定名的简写，必须一起数，
    # 否则 `AshareBoard::Etf`（只在 `impl AshareBoard` 里以 `Self::Etf` 出现）会被误判成孤儿。
    unproduced: list[str] = []
    qualified_refs: dict[str, int] = {}
    cache: dict[str, str] = {}

    def self_scope(enum: str) -> str:
        if enum not in cache:
            cache[enum] = "\n".join(
                _impl_blocks(blob, enum) for blob in joined.values() if "impl " in blob
            )
        return cache[enum]

    for key, rel in declared.items():
        enum, variant = key.split("::")
        hits = sum(len(re.findall(rf"\b{enum}::{variant}\b", blob)) for blob in joined.values())
        hits += len(re.findall(rf"\bSelf::{variant}\b", self_scope(enum)))
        qualified_refs[key] = hits
        if not hits and key not in LINE_FORMAT_VARIANTS:
            unproduced.append(f"{key}（{rel}）")
    check(
        len(declared) >= ENUM_VARIANT_SURFACE_FLOOR
        and len({key.split("::")[0] for key in declared}) >= ENUM_SURFACE_FLOOR,
        "变体生产者判据扫到了全仓的 pub enum（解析退化会让零变体假绿）",
        f"{len({key.split('::')[0] for key in declared})} 个枚举 / {len(declared)} 个变体，"
        f"地板 {ENUM_SURFACE_FLOOR}/{ENUM_VARIANT_SURFACE_FLOOR}",
    )
    check(
        not unproduced,
        "每个 pub enum 变体都有生产代码里的限定名生产者，或在允许清单里说明它的生产者在线格式上",
        f"无生产者变体 {sorted(unproduced)[:6] or '无'}",
    )
    stale: list[str] = []
    for key in LINE_FORMAT_VARIANTS:
        enum = key.split("::")[0]
        if key not in declared:
            stale.append(f"{key} 已不是任何枚举的变体")
        elif enum not in deserializable:
            stale.append(f"{enum} 没有 derive Deserialize，线格式理由不成立")
        elif qualified_refs.get(key):
            stale.append(f"{key} 已有 {qualified_refs[key]} 处限定名生产者，条目该删")
    check(
        not stale,
        "线格式允许清单逐条可复核：变体仍在册、所在枚举确实 Deserialize、且确实零生产者",
        f"清单 {len(LINE_FORMAT_VARIANTS)} 条；失效 {stale or '无'}",
    )


# 回测与重放路径的 crate：这三处的生产文本读一次系统墙钟，就意味着同一份输入跑两次结果不同。
CLOCK_FREE_BACKTEST_CRATES = ("qx-core", "qx-xingban", "qx-strategy")
WALL_CLOCK_TOKENS = ("SystemTime", "Instant::now", "Utc::now", "chrono::Local")
# 唯一一格豁免：锁文件的年龄与令牌 nonce 读的是墙钟，它判的是"另一个进程还活着吗"，
# 不进回测/重放的任何一条数值链（D1 的崩溃残留接管、K3 的接管竞据、R7-9 的年龄交出都靠它）。
# 反向说：这条链一旦把锁年龄当成模拟时间轴的一部分，豁免就成了假口供——所以豁免按路径点名，
# 并由下面那颗"豁免文件仍在场"的判据核对，不让它悄悄落到空处。
CLOCK_FREE_EXEMPT_FILES = ("crates/qx-core/src/file_lock.rs",)


def backtest_clock_honesty_check() -> None:
    """"回测不用系统时间"这条底线必须指得到真实闸门，不能挂在一个没人推进的时钟类型上（V12 §18-B #116）。

    `qx-core::TestClock` 连同 `advance_to` 曾是 README 设计底线第 2 条点名的对象，但全仓没有任何一条
    链推进过它 —— 文档承诺的是一个从未被使用过的类型，而真正决定时间轴的东西在数据侧：`process_bars`
    先排序、`validate_bars` 拒同标的非严格递增、帧读侧对乱序直接报错。本轮删掉那个类型、把 README 与
    内核文档改指真实闸门；下面的判据保证代码与文档两侧都不会再分叉回去。
    """

    def text_of(path: Path) -> str:
        return path.read_text(encoding="utf-8")

    # 判据 1：代码里不再存在那个已删除的类型。本文件必须例外 —— 禁止一个名字的判据
    # 自己得写出那个名字，否则无法解释为什么红。
    named: list[str] = []
    for path in sorted(CRATES.rglob("*.rs")) + sorted((ROOT / "tools").glob("*.py")):
        if path == Path(__file__):
            continue
        body = text_of(path)
        if "TestClock" in body or "advance_to" in production_text(body):
            named.append(str(path.relative_to(ROOT)))
    check(
        not named,
        "回测时钟口径不再挂在一个没人推进的类型上：全仓代码找不到 TestClock/advance_to",
        f"又出现点名的文件 {named}",
    )
    # 判据 2：README 那条底线必须指向真实闸门。只删掉承诺同样能让判据 1 变绿，
    # 所以这里正向要求它点名「qx-data + 严格递增」，而不是要求它别写某个词。
    lines = text_of(ROOT / "README.md").splitlines()
    start = next((index for index, line in enumerate(lines) if "不用系统时间" in line), -1)
    promise = " ".join(lines[start:start + 4]) if start >= 0 else ""
    check(
        "qx-data" in promise and "严格递增" in promise,
        "README 的「不用系统时间」底线把时间轴责任交给数据侧闸门，不再许诺虚拟时钟",
        f"该条底线现在的写法是：{promise[:160] or '（README 里已找不到这句）'}",
    )
    # 判据 3：三条回测 crate 的生产文本一次墙钟都不读。
    readers: dict[str, list[str]] = {}
    for crate in CLOCK_FREE_BACKTEST_CRATES:
        hits: list[str] = []
        for path in sorted((CRATES / crate / "src").rglob("*.rs")):
            if any(path == ROOT / rel for rel in CLOCK_FREE_EXEMPT_FILES):
                continue
            body = production_text(text_of(path))
            hits += [token for token in WALL_CLOCK_TOKENS if token in body]
        if hits:
            readers[crate] = hits
    check(
        not readers,
        "回测与重放路径的 crate 在生产文本里一次系统时间都不读（读一次就等于放弃可重放）",
        f"读到墙钟的 crate {readers}（豁免只看点名的 {list(CLOCK_FREE_EXEMPT_FILES)}）："
        "回测结果的每一个数都来自 bar 序列与撮合规则，掺进一次墙钟就读不出一份可复现的结论；"
        "把豁免写进这句话，是因为豁免缺席时这里会红得不解释自己",
    )
    check(
        all((ROOT / rel).exists() for rel in CLOCK_FREE_EXEMPT_FILES),
        "墙钟豁免的每一份文件都还在场（否则豁免就是一张没人核对的空头支票）",
        f"缺席 {[rel for rel in CLOCK_FREE_EXEMPT_FILES if not (ROOT / rel).exists()]}：豁免是按路径生效的，"
        "文件改名或删掉后这条 `continue` 就落在空处，判据看起来仍在扫三条 crate 实际却少了一格",
    )
    # 判据 4：真实闸门三处部件各在其位。
    validation = text_of(ROOT / "crates/qx-data/src/validation.rs")
    pipeline = production_text(text_of(ROOT / "crates/qx-data/src/pipeline.rs"))
    provider = production_text(text_of(ROOT / "crates/qx-data/src/provider.rs"))
    check(
        "must be strictly increasing" in validation
        and "bars.sort_by(" in pipeline
        and "validate_bars(&bars)" in pipeline
        and "BarFrame timestamps must be strictly increasing" in provider,
        "时间轴由数据侧决定：入库前排序 + 同标的 ts 严格递增 + 帧读侧乱序报错，三处缺一不可",
        "排序/校验/帧侧拒绝任一部件消失，回测时间轴就没人负责",
    )
    # 判据 5：闸门真的会拒/会排，用例为证（只看生产文本会放过把断言删掉的改法）。
    cases = (
        validation
        + text_of(ROOT / "crates/qx-data/src/provider.rs")
        + text_of(ROOT / "crates/qx-data/src/pipeline.rs")
    )
    check(
        "validate_bars(&[bar(2), bar(1)])" in cases
        and "rejects_column_mismatch_and_non_monotonic_input" in cases
        and "canonicalizes_before_validation" in cases,
        "非严格递增的时间轴会被当场拒掉，且排序、Bar 入库与 BarFrame 读侧三处都有行为用例",
        "乱序拒绝或用例名被删除/改名",
    )


SCHEDULER_DISPATCH_FILE = "crates/qx-cli/src/scheduler.rs"
SCHEDULER_DISPATCH_TEST_FILE = "crates/qx-cli/src/tests/scheduler_dispatch_support.rs"
# 库侧那套"带交易日历/事件/手工触发"的到期判定：生产文本里一次都不该出现（V12 §18-B #117）。
LIBRARY_ONLY_DUE_ENTRIES = ("due_jobs_with_calendar", "due_event_jobs", "due_manual_jobs")


def scheduler_dispatch_honesty_check() -> None:
    """调度 tick 只认「Cron + window=Any」，那么其余形状必须在装载时就拒（V12 §18-B #117）。

    `dispatch_scheduled_jobs` 走 `due_jobs`：它不看 `JobWindow`，也不认 `TradingCalendar`
    触发。库里另有 `due_jobs_with_calendar` 那条"生产入口"，它需要的日历至今只有用例往里面
    填过 `Session` —— 于是把窗口写成 `Session` 的作业照旧在收盘前后一样触发，而 `Manual`/
    `Event` 作业收下后再也不会出队。两者都是"配置声明了、运行时不认"，比拒绝更难发现，
    所以本轮把能力边界搬到装载点：`load_scheduler_state` 先拒后注册，整轮 tick 也先复核
    manifest 再盖章。
    """

    def tight(path: str) -> str:
        return re.sub(r"\s+", "", production_text((ROOT / path).read_text(encoding="utf-8")))

    loader = _fn_body(tight(SCHEDULER_DISPATCH_FILE), "pub(crate)fnload_scheduler_state")
    shape = _fn_body(tight(SCHEDULER_DISPATCH_FILE), "pub(crate)fnunsupported_dispatch_shape")
    declared = _fn_body(tight(SCHEDULER_DISPATCH_FILE), "fndeclared_jobs")
    check(
        "matches!(&job.trigger,Trigger::Cron(_))" in shape
        and "job.window!=JobWindow::Any" in shape
        and "只跑Cron触发且window=Any" in shape
        and "filter_map(unsupported_dispatch_shape)" in declared
        and "returnErr(refused.join" in declared
        and "declared_jobs(&jobs_path)?" in loader
        and loader.index("declared_jobs(&jobs_path)?") < loader.index(".register(job)"),
        "装载闸门按运行时的真实能力拒形状，且拒绝发生在写入 Scheduler 状态之前",
        f"装载入口或形状判定被改动：{loader[:120]}",
    )
    # 形状闸门搬到声明读取口之后，第二次启动也必须经过它；分叉判据管住另外三个方向
    # （V13 R17-e：作业集合只在状态缺失时重建，声明改动不会自动生效）。
    drift = _fn_body(tight(SCHEDULER_DISPATCH_FILE), "fnreject_job_set_drift")
    check(
        "reject_job_set_drift(&scheduler,&declared,&jobs_path)?" in loader
        and all(
            needle in drift
            for needle in (
                "新增的声明从未进入调度状态",
                "改动过的声明从未生效",
                "不在声明文件里",
                "重名job_id",
                "scheduler.len()>declared_ids.len()",
            )
        )
        and drift.index("declared_ids.len()!=declared.len()") < drift.index("forjobindeclared"),
        "第二次启动会把「声明与状态分叉」判成新增/改动/删掉/重名四个方向并当场拒，而不是让状态里的旧作业集合照旧开跑",
        f"分叉判据缺向或被改动：{drift[:160]}",
    )
    dispatch = _fn_body(tight(SCHEDULER_DISPATCH_FILE), "pub(crate)fndispatch_scheduled_jobs")
    check(
        "manifest.validate()?" in dispatch
        and "scheduler.due_jobs(tick,&completed)" in dispatch
        and dispatch.index("manifest.validate()?") < dispatch.index(".start_run_at("),
        "一次 tick 先复核 RunManifest 再拿它的摘要给 JobRun 盖血缘，到期判定只有 due_jobs 这一个入口",
        f"tick 入口的复核顺序被改动：{dispatch[:140]}",
    )
    library_readers = {
        entry: sum(
            1
            for path in CRATES.rglob("*.rs")
            if "qx-scheduler" not in path.parts
            and entry in production_text(path.read_text(encoding="utf-8"))
        )
        for entry in LIBRARY_ONLY_DUE_ENTRIES
    }
    check(
        all(count == 0 for count in library_readers.values()),
        "库里那三条带日历/事件/手工的到期判定在 qx-scheduler 之外没有生产读者（口径仍如实降级）",
        f"出现读者的库入口 {library_readers}：要么把生产 tick 接过去，要么删掉这条第二口径",
    )
    test_text = (ROOT / SCHEDULER_DISPATCH_TEST_FILE).read_text(encoding="utf-8")
    check(
        len(re.findall(r"#\[test\]", test_text)) == 4
        and "Trigger::TradingCalendar" in test_text
        and "Trigger::Event(" in test_text
        and "JobWindow::Session" in test_text
        and "只跑 Cron 触发且 window=Any" in test_text
        and "manifest.run_id.clear()" in test_text
        and "a_second_boot_that_disagrees_with_the_declaration_refuses_to_run" in test_text
        and "新增的声明从未进入调度状态" in test_text
        and "改动过的声明从未生效" in test_text
        and "不在声明文件里" in test_text
        and "重名 job_id" in test_text
        and "声明未变的第二次启动不该被分叉判据拒绝" in test_text,
        "四种被拒形状、被支持形状（含声明未变的第二次启动）、声明分叉四向与非法 manifest 各有经过真实入口的用例",
        f"用例数 {len(re.findall(r'#\[test\]', test_text))}；缺少的形状或分叉判据见判据文本",
    )
    capabilities = (ROOT / "maturity" / "capabilities.yaml").read_text(encoding="utf-8")
    check(
        re.search(
            r"- scheduler_dispatch_is_cron_and_any_window_only(?=[：\s])", capabilities
        )
        is not None,
        "对外能力矩阵把这条派发边界写成了 limitation，不是留在代码注释里",
        "capabilities.yaml 没有以 scheduler_dispatch_is_cron_and_any_window_only 立项的 limitation",
    )


# —— 调度 owner 路由 fail closed（V11 §41 E7 / 2026-10-06 方案 §8 WP-5）——

SCHEDULER_SRC_ROOTS = ("crates/qx-scheduler/src",)
JOB_SPEC_FILE = "crates/qx-scheduler/src/job_spec.rs"
SCHEDULER_CLAIM_FILE = "crates/qx-cli/src/workers.rs"
# 领取端旧写法：就地比较通配字面量。判据守的是它不许回来。
INLINE_WILDCARD_OWNER = 'queued.job.owner != "*"'
# 作业声明里只有写侧、没有读侧的三格。按仓库先例（#118/#170/#171）与 2026-10-06 方案
# §13.2 的裁定「保留不删 + 登记 limitation + 双向钉住」，而不是删面。
JOB_SPEC_ZERO_READER_FIELDS = ("input_refs", "output_refs", "permission_scope")
JOB_SPEC_ZERO_READER_LIMITATION = "job_spec_declaration_fields_have_no_production_reader"
JOB_SPEC_ZERO_READER_PIN_FILE = "crates/qx-cli/src/tests/zero_reader_fields.rs"
JOB_SPEC_ZERO_READER_PIN_CASE = "zero_reader_struct_fields_stay_registered_in_capabilities"


def scheduler_owner_routing_check() -> None:
    """调度链的最后一公里：作业 owner 必须真有人领取（V11 §41 E7）。

    Scheduler 只负责把到期作业入队，领取判据在 Strategy worker 那一侧。owner 拼错、
    指向未启用的 worker 时，`JobSpec::validate` 那一条"非空"判据挡不住任何东西：作业永远
    留在队列里，而 `start_run_at` 已经把 JobRun 标成 Running，命令面照样打印
    `READY processed=0` —— 整段调度事实就这样丢了。仓库自带的示例里 5 套拓扑正是这种
    形状（含 production 的对冲腿）。

    第二半是 JobSpec 的字段读侧：`input_refs`/`output_refs`/`permission_scope` 三格只有
    写侧。按 §13.2 的裁定，它们**保留不删**（删面等于把缺口藏起来），改为逐条登记
    limitation 并由用例双向钉住——所以这里钉的是"三条都在盘"，不是"三格被删掉"。
    """
    scheduler_src = surface_text(SCHEDULER_SRC_ROOTS)
    spec = (ROOT / JOB_SPEC_FILE).read_text(encoding="utf-8")
    assembly = (ROOT / "crates" / "qx-cli" / "src" / "scheduler.rs").read_text(encoding="utf-8")
    claim = (ROOT / SCHEDULER_CLAIM_FILE).read_text(encoding="utf-8")
    check(
        len(re.findall(r"pub const JOB_OWNER_ANY\b", scheduler_src)) == 1
        and scheduler_src.count("pub fn claimable_by(") == 1
        and INLINE_WILDCARD_OWNER not in claim
        and "claimable_by(&queued.job.owner, context.id())" in claim,
        "通配 owner 只有一个常量，领取判据不再就地比较 \"*\"，且与装配端共用同一处判据",
        f"JOB_OWNER_ANY {len(re.findall(r'pub const JOB_OWNER_ANY\\b', scheduler_src))} 处 / "
        f"claimable_by {scheduler_src.count('pub fn claimable_by(')} 处 / "
        f"workers.rs 就地字面量 {claim.count(INLINE_WILDCARD_OWNER)} 处",
    )
    check(
        scheduler_src.count("pub fn jobs(&self)") == 1
        and assembly.count("validate_job_owners(config, &scheduler)?;") >= 2
        and "fn validate_job_owners(" in assembly
        and "claimable_by(&job.owner, claimant)" in assembly,
        "作业 owner 在装配处 fail closed，且新建与载入两条路径都要问",
        f"只读视图 {scheduler_src.count('pub fn jobs(&self)')} 处 / "
        f"判定调用 {assembly.count('validate_job_owners(config, &scheduler)?;')} 处",
    )
    # 跨文件连通性：示例配置里的每个启用作业，都得有该拓扑内启用的 Strategy worker 领取。
    unroutable = []
    missing_jobs = []
    for path in sorted((ROOT / "deploy").glob("qianxing.runtime*.json")):
        runtime = json.loads(path.read_text(encoding="utf-8"))
        claimants = {
            worker["id"]
            for worker in runtime.get("workers", [])
            if worker.get("enabled") and worker.get("role") == "strategy"
        }
        configured = runtime.get("scheduler", {}).get("jobs_path")
        if not configured:
            continue
        jobs_path = Path(configured)
        if not jobs_path.is_absolute():
            parts = jobs_path.parts
            # 镜像 `resolve_runtime_relative_path`：相对配置文件目录，首段与目录同名时剥离。
            if parts and parts[0] == path.parent.name:
                jobs_path = path.parent.joinpath(*parts[1:]) if len(parts) > 1 else path.parent
            else:
                jobs_path = path.parent / jobs_path
        if not jobs_path.exists():
            missing_jobs.append(f"{path.name} -> {configured}")
            continue
        for job in json.loads(jobs_path.read_text(encoding="utf-8")):
            if not job.get("enabled"):
                continue
            if job["owner"] != "*" and job["owner"] not in claimants:
                unroutable.append(f"{path.name}:{job['job_id']} -> {job['owner']}")
    check(
        not missing_jobs,
        "deploy 示例声明的作业清单文件都在其相对布局下找得到",
        f"清单缺失 {missing_jobs}",
    )
    check(
        not unroutable,
        "deploy 示例里每个启用作业都有可领取的启用 Strategy worker（owner 路由连通）",
        f"无人领取 {unroutable}",
    )
    # §13.2：三格零读者字段的处置是「保留不删 + 登记 limitation + 双向钉住」。三条缺一
    # 不可——字段被删掉是"把缺口藏起来"，登记被偷偷删掉则让"接上读者就摘登记"的反向
    # 判据失去锚点。这里同时要求钉住用例在盘且仍点名这三格里的 `permission_scope`。
    capabilities = (ROOT / "maturity" / "capabilities.yaml").read_text(encoding="utf-8")
    pinned = (ROOT / JOB_SPEC_ZERO_READER_PIN_FILE).read_text(encoding="utf-8")
    spec_body = re.search(r"pub struct JobSpec \{([^}]*)\}", spec, re.S)
    roster = set(re.findall(r"pub ([a-z_]+):", spec_body.group(1))) if spec_body else set()
    # 登记键按 `- <key>(?=[：\s])` 认：`..._reader_RENAMED` 这种改名不会被子串匹配放过。
    registered = (
        re.search(rf"- {JOB_SPEC_ZERO_READER_LIMITATION}(?=[：\s])", capabilities) is not None
    )
    check(
        bool(spec_body)
        and all(field in roster for field in JOB_SPEC_ZERO_READER_FIELDS)
        and registered
        and JOB_SPEC_ZERO_READER_PIN_CASE in pinned
        and JOB_SPEC_ZERO_READER_FIELDS[2] in pinned,
        "JobSpec 三格零读者字段按先例「保留不删 + 登记 limitation + 双向钉住」，而不是删面",
        f"字段 {sorted(roster)} / 登记 {'有' if registered else '缺'} / "
        f"钉住用例 {'有' if JOB_SPEC_ZERO_READER_PIN_CASE in pinned else '缺'}",
    )
    # 声明名单不写死在门禁里：它从 `JobSpec` 的字段声明取出，示例的每一格顶层键必须是它的
    # 成员——多出一格没人读的空头声明会立刻被同一判据抓住。
    manifests = sorted((ROOT / "deploy").glob("qianxing.scheduler.*.json"))
    unread = {}
    for path in manifests:
        for job in json.loads(path.read_text(encoding="utf-8")):
            extra = sorted(set(job) - roster)
            if extra:
                unread.setdefault(path.name, []).extend(extra)
    check(
        bool(manifests) and not unread,
        f"deploy 里 {len(manifests)} 份作业清单示例的每一格顶层键都在 JobSpec 名单里",
        f"没人读的键 { {name: sorted(set(keys)) for name, keys in unread.items()} or '无'}",
    )


# 公司行为的三条只读查询：它们读的是引擎已经算出来、但产物读不到的东西（V12 §18-B #118）。
CORPORATE_ACTION_READ_ENTRIES = (
    "cash_dividend_entitlement_for",
    "convertible_bond_interest_entitlement_for",
    "issuer_capital_at",
)


def corporate_action_read_side_check() -> None:
    """发行股本与待支付权益查询没有生产读者，这件事必须写在对外矩阵里（V12 §18-B #118）。

    A 股回测在登记日把权益写进 Ledger、在支付日结算，钱本身是对的；缺的是读侧：
    `BacktestReport::issuer_capital_at` 与内核的两条 `*_entitlement_for` 只有用例调用，
    发行股本快照也随报告结构体存在却不进摘要。本轮不改产物形状（会让已 bless 的重放哈希漂移），
    只把口径如实登记，并要求任何人接上读者时同步撤销这条声明。
    """
    capabilities = (ROOT / "maturity" / "capabilities.yaml").read_text(encoding="utf-8")
    check(
        re.search(r"- corporate_action_read_side_has_no_production_reader(?=[：\s])", capabilities)
        is not None,
        "能力矩阵声明了「公司行为读侧三条查询无生产读者」，不是只在代码注释里承认",
        "capabilities.yaml 没有以 corporate_action_read_side_has_no_production_reader 立项的 limitation",
    )
    readers = {}
    for entry in CORPORATE_ACTION_READ_ENTRIES:
        sites = re.compile(rf"\.{entry}\(")
        # 集成测试目录整体就是测试代码（文件里没有 `#[cfg(test)]` 标记，production_text 截不掉），
        # 只有 src 下的调用才是接上了读者。
        readers[entry] = sum(
            len(sites.findall(production_text(path.read_text(encoding="utf-8"))))
            for path in CRATES.rglob("*.rs")
            if "tests" not in path.parts
        )
    check(
        all(count == 0 for count in readers.values()),
        "这三条查询在全部 crate 的生产文本里一次都没被调用（声明与现状同源，接上读者就要撤声明）",
        f"出现生产调用点 {readers}：撤销 capability limitation 与允许清单条目，并补产物读侧判据",
    )


# V12 §18-B #119：因子研究链的读侧接进了生产，写侧一个生产装配点都没有。
FACTOR_LIB_FILE = "crates/qx-factor/src/lib.rs"
# 真的把快照 JSON 解析进生产路径的三处装配。
FACTOR_SNAPSHOT_PARSE_FILES = (
    "crates/qx-cli/src/readiness.rs",
    "crates/qx-cli/src/strategy_binding.rs",
    "crates/qx-cli/src/strategy_contract.rs",
)
# 读侧的全部生产读者（含只带 context 进来的运行时闸门）。
FACTOR_RESEARCH_READER_FILES = FACTOR_SNAPSHOT_PARSE_FILES + (
    "crates/qx-runtime/src/data_binding.rs",
)
# 研究快照 JSON 的逐层字段契约：接口文档里的标签 -> 源码里的 serde 结构体。
FACTOR_SNAPSHOT_CONTRACT = (
    ("顶层", "StrategyResearchSnapshotWire"),
    ("candidate", "CandidateBindingWire"),
    ("candidate.config", "CandidateConfigWire"),
    ("artifacts[]", "FeatureArtifactWire"),
    ("reports[]", "FactorReport"),
)
# 库与用例里都有、CLI 生产装配里没有的写侧入口。
FACTOR_UNWIRED_WRITE_ENTRIES = (
    "materialize",
    "materialize_incremental",
    "compile_execution_plan",
    "validate_finkit_artifact",
    "compute_momentum",
)


def struct_field_names(text: str, struct_name: str) -> list[str]:
    """按字段名取一个结构体的对外 JSON 字段（`pub` 与否都算，属性行不算）。"""
    block = re.search(rf"struct {struct_name} \{{\n(.*?)\n\}}", text, re.S)
    if block is None:
        return []
    return re.findall(r"^\s+(?:pub )?(\w+):", block.group(1), re.M)


def factor_research_honesty_check() -> None:
    """因子研究链的两端口径都要钉住：读侧不许悄悄拆掉，写侧不许悄悄声称能跑（#119）。

    策略运行时会把 `research_snapshot_path` 指着的 JSON 解析成 `StrategyResearchSnapshot`，
    并按数据指纹与 `runs/` 里的事件回测清单复核；但仓库里没有任何命令生成这份 JSON ——
    物化、增量物化、执行计划编译、finkit 工件摘要校验、动量因子计算五个写侧入口只在用例
    里被调用。本轮把这件事写进对外矩阵与接口文档（含逐层字段契约），而不是临发布前新增
    一条命令面。
    """
    capabilities = (ROOT / "maturity" / "capabilities.yaml").read_text(encoding="utf-8")
    factor_lib = (ROOT / FACTOR_LIB_FILE).read_text(encoding="utf-8")
    readme = (ROOT / "deploy" / "README.md").read_text(encoding="utf-8")

    missing_claims = [
        token
        for token in (
            "factor_snapshot_json_has_no_in_repo_producer",
            "feature_artifact_values_have_no_production_reader",
        )
        if re.search(rf"- {token}(?=[：\s])", capabilities) is None
    ]
    check(
        not missing_claims,
        "能力矩阵把「快照无仓库内生产者」与「工件值无读者」立成 limitation，不是写在代码注释里",
        f"capabilities.yaml 缺以下 limitation 立项 {missing_claims}",
    )

    call_sites = {}
    for entry in FACTOR_UNWIRED_WRITE_ENTRIES:
        # 只数带接收者的调用（`.name(` / `::name(`），结构体与自由函数的定义行不算读者；
        # 接收者前缀本身就是分隔符，所以不再加词断言——否则 `qx_factor::compute_momentum(`
        # 会因为 `::` 前面是标识符字符而漏计。
        sites = re.compile(rf"(?:\.|::){entry}\s*[<(]")
        call_sites[entry] = sum(
            len(sites.findall(production_text(path.read_text(encoding="utf-8"))))
            for path in CRATES.rglob("*.rs")
            if "tests" not in path.parts
        )
    check(
        all(count == 0 for count in call_sites.values()),
        "物化/增量物化/计划编译/finkit 校验/动量计算在 src 生产文本里零调用（写侧确实无人装配）",
        f"出现生产调用点 {call_sites}：撤销 factor_snapshot_json_has_no_in_repo_producer 声明",
    )
    artifact_readers = sum(
        len(re.findall(r"\.artifacts\b", production_text((ROOT / file).read_text(encoding="utf-8"))))
        for file in FACTOR_RESEARCH_READER_FILES
    )
    check(
        artifact_readers == 0,
        "四处读侧确实只认 candidate 血缘与指纹，没有读 artifacts 里的逐标的因子值",
        f"读侧出现 {artifact_readers} 处 .artifacts 引用：撤销 feature_artifact_values_have_no_production_reader 声明",
    )

    drift = []
    for label, struct in FACTOR_SNAPSHOT_CONTRACT:
        code_fields = struct_field_names(factor_lib, struct)
        line = next((row for row in readme.splitlines() if row.startswith(f"{label}: ")), "")
        doc_fields = [name.strip() for name in line[len(label) + 2 :].split(",") if name.strip()]
        if not code_fields or sorted(doc_fields) != sorted(code_fields):
            drift.append(f"{label}({struct}) 文档={doc_fields} 代码={code_fields}")
    check(
        not drift,
        "接口文档逐层列出的快照字段与 serde 结构体的字段一一对齐（不多不少）",
        f"字段口径分叉 {drift}",
    )
    check(
        "不提供生成该文件的命令" in readme and "未知字段都会被拒绝" in readme,
        "接口文档直说快照由仓库外导出、且拼错的键会被拒绝，不让读者以为框架会生成",
        "deploy/README.md 的研究快照段落缺「不提供生成该文件的命令」或「未知字段都会被拒绝」口径",
    )
    missing_attr = [
        struct
        for _, struct in FACTOR_SNAPSHOT_CONTRACT
        if re.search(
            rf"#\[serde\(deny_unknown_fields\)\]\s*\n(?:pub )?struct {struct} \{{", factor_lib
        )
        is None
    ]
    check(
        not missing_attr,
        "五层快照 JSON 结构全部带 deny_unknown_fields（文档承诺与解析行为同源）",
        f"缺少该属性 {missing_attr}",
    )
    parsed = {
        file: production_text(
            (ROOT / file).read_text(encoding="utf-8")
        ).count("StrategyResearchSnapshot::from_json(")
        for file in FACTOR_SNAPSHOT_PARSE_FILES
    }
    check(
        all(count for count in parsed.values()),
        "快照读侧仍在就绪判定与两条策略装配里被真的解析（limitation 只承认写侧缺，不掩盖读侧）",
        f"StrategyResearchSnapshot::from_json 读者消失 {parsed}",
    )


# —— 调度重试链的诚实性（V12 §18-A #110 剩余 / #129）——

SCHEDULER_LIB_PATH = CRATES / "qx-scheduler" / "src" / "lib.rs"
SCHEDULER_WORKER_FILE = "crates/qx-cli/src/workers.rs"
# `retry_run` 是公布入口、`retry_run_at` 是它的时间参数版；两条都只被定义文件自己调用。
SCHEDULER_RETRY_ENTRIES = ("retry_run", "retry_run_at")
SCHEDULER_FINISH_CALL = "finish_run_with_code(queued.run.run_id,true,None,"


def scheduler_retry_honesty_check() -> None:
    """一次运行只有一次执行机会：这条链要么真的接上，要么如实写明没接（#129）。

    生产 tick 只把超过 deadline 的 Running 运行升级成 NeedsIntervention 并释放并发键；
    `finish_run_with_code` 在 CLI 侧唯一的调用点以 success=true 收口，所以 `JobStatus::Failed`
    → `next_retry_ts` → `retry_run_at` 这条自动重试链在 qx-scheduler 之外零生产调用者。
    本轮把它写进能力矩阵与接口文档，而不是在发布前顺手接上——交易作业失败时无法判定订单
    是否已经出网，自动重跑等于二次提交。同一轮修掉那个调用点真正的错：把「3 orders:
    SUBMITTED」这类结果码塞进 error_code，会让一条 Succeeded 运行在 /scheduler/runs 读出假错误码。
    """
    capabilities = (ROOT / "maturity" / "capabilities.yaml").read_text(encoding="utf-8")
    readme = (ROOT / "deploy" / "README.md").read_text(encoding="utf-8")
    check(
        re.search(
            r"- scheduler_run_retry_has_no_production_path(?=[：\s])", capabilities
        )
        is not None,
        "能力矩阵把「自动重试链无生产装配」立成 limitation，而不是只写在代码注释里",
        "capabilities.yaml 缺 scheduler_run_retry_has_no_production_path 立项",
    )
    callers = {}
    for entry in SCHEDULER_RETRY_ENTRIES:
        sites = re.compile(rf"(?:\.|::){entry}\s*[<(]")
        callers[entry] = sum(
            len(sites.findall(production_text(path.read_text(encoding="utf-8"))))
            for path in CRATES.rglob("*.rs")
            # 集成测试目录整体是测试代码；定义文件里 `retry_run` 本身就转调 `retry_run_at`，
            # 那一处是这条公共面的实现，不是"接线证据"。
            if "tests" not in path.parts and path != SCHEDULER_LIB_PATH
        )
    check(
        all(count == 0 for count in callers.values()),
        "到期重试入口在 qx-scheduler 之外零生产调用（limitation 说的确实是目前的事实）",
        f"出现生产调用点 {callers}：接上重跑就要撤 scheduler_run_retry_has_no_production_path",
    )
    check(
        "不会自动重跑" in readme and "结果码不进 `JobRun`" in readme,
        "接口文档写明一次运行的收口口径：不自动重跑，且成功运行不带 error_code",
        "deploy/README.md 的 Scheduler 段落缺「不会自动重跑」或「结果码不进 `JobRun`」口径",
    )
    worker = production_text((ROOT / SCHEDULER_WORKER_FILE).read_text(encoding="utf-8"))
    check(
        "".join(worker.split()).count(SCHEDULER_FINISH_CALL) == 1,
        "Strategy worker 的成功收口只交终态，不再把结果码当作 error_code 传给 JobRun",
        f"生产收口形状改变：去空白文本里应有恰好 1 处 {SCHEDULER_FINISH_CALL}",
    )
    # 零构造者的档位：`Pending` 与 `Paused` 全仓没有任何构造点与入边（`start_run` 直接建
    # `Running`，暂停走的是 `StrategyState`），只给每个 `match` 留一条永不为真的臂。
    # 按 `EventKind::Timer`/`MarketBar` 与 `CommandStatus::Rejected` 先例退役，并在这里钉住
    # 不许回来——门禁的 `enum_variant_producer_check` 把「模式臂里出现一次」也数成生产者，
    # 所以它看不见这种变体（实测：两颗变体各自只有 `validate_state` 那一条臂，判据照绿）。
    scheduler_text = production_text(SCHEDULER_LIB_PATH.read_text(encoding="utf-8"))
    check(
        "Pending" not in scheduler_text and "Paused" not in scheduler_text,
        "JobStatus 不留零构造者的档位（Pending/Paused 无生产者、无入边，已按 EventKind 先例退役）",
        f"qx-scheduler 生产代码里 Pending {scheduler_text.count('Pending')} 处 / "
        f"Paused {scheduler_text.count('Paused')} 处（期望 0/0：回来就得给出构造点与入边）",
    )


def snapshot_money_honesty_check() -> None:
    """账户快照不得把"没算过的钱"印成 0（V11 Q67 / 交易链路 TX2）。"""
    protocol = (ROOT / SNAPSHOT_PROTOCOL_FILE).read_text(encoding="utf-8")
    # 读模型侧不按 `#[cfg(test)]` 截断：api_service.rs 里那个标记挂在单个测试专用函数上，
    # 截断会把后面的生产代码一起丢掉。
    reader = (ROOT / SNAPSHOT_READ_FILE).read_text(encoding="utf-8")
    endpoint = production_text((ROOT / SNAPSHOT_ENDPOINT_FILE).read_text(encoding="utf-8"))

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
        "保证金/冻结/资金费这三个字段在本层没有来源，产码里不得出现给它们赋值的写法",
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
    core_cases = case_source(SNAPSHOT_CORE_CASE_FILE)
    # 端点用例住在 `#[cfg(test)] mod tests` 里，要用整文件而不是上面截过断的生产体。
    endpoint_cases = (ROOT / SNAPSHOT_ENDPOINT_FILE).read_text(encoding="utf-8")
    check(
        all(f"fn {name}(" in cli_cases for name in SNAPSHOT_MONEY_CASES)
        and "fn uncomputed_money_is_not_the_same_state_as_computed_zero(" in core_cases
        and "fn balances_endpoint_publishes_absent_money_as_null_not_zero(" in endpoint_cases,
        "五条读模型用例（结算账簿口径/费用同源/未算缺席/溢出即拒/缺标记价的权益缺席）"
        "加协议侧缺席≠零、端点侧 null 发布各一条在位",
        "缺少用例："
        f"{[name for name in SNAPSHOT_MONEY_CASES if f'fn {name}(' not in cli_cases]}"
        " / 协议侧缺席≠零用例在位="
        f"{'fn uncomputed_money_is_not_the_same_state_as_computed_zero(' in core_cases}"
        " / 端点侧 null 用例在位="
        f"{'fn balances_endpoint_publishes_absent_money_as_null_not_zero(' in endpoint_cases}",
    )


# === V13 R1-A5：账户级"本层没算"的名单要逐字段同源 ===
# 这条事实此前只活在能力矩阵的一句合并命名（`account_level_unrealized_pnl_margin_frozen_…_and_
# funding_raw_still_have_no_producer`）里，契约文本与接口文档更是只点到 `/account/balances` 那一格。
# 于是"这一层到底哪几格算不出"没有一处能被机器核对：给某个字段接上生产者、或反过来把 null 当成 0
# 读，四处口径都不会红。下面先把名单从**源码**派生（协议声明的八个 `Option<i128>` 钱字段，减去生产
# 文本里真有赋值点的那些），再拿它逐条比对契约、能力矩阵、接口文档与读侧用例。
ACCOUNT_MONEY_COMPUTED_MARKER = "本构建有生产者"
ACCOUNT_MONEY_ABSENT_MARKER = "本构建这一层没有生产者"
ACCOUNT_MONEY_DOC_MARKER = "账户级无生产者字段"
ACCOUNT_MONEY_CASE_NAME = "uncomputed_money_is_absent_rather_than_zero"
# V11 Q67 的旧写法：五个字段挤在一行合并命名里。它一回来，逐字段对齐就失去对象。
ACCOUNT_MONEY_MERGED_LIMITATION = "- account_level_"
# 结构体字面量构造点。`impl AccountSnapshot {` 后面跟的是 `pub fn`，不会误命中。
# 口径（MA10 实测）：生产文本只剥"整行注释"，行尾注释里的字面量照样算构造点。方向是 fail-closed ——
# 代价是有人在行尾注释举例如实时会红一次，把那条例子挪到整行注释就好；换成字符串感知的剥注释，
# 换来的是"一个串里出现 // 就整段消失"的假绿，那才是这条判据承受不起的错法。
ACCOUNT_SNAPSHOT_LITERAL = re.compile(r"AccountSnapshot\s*\{\s*[A-Za-z_]\w*\s*:")


def account_money_field_registry_check() -> None:
    """账户级"这一层没算"的字段名单：源码派生一份，契约/能力矩阵/接口文档/用例四处逐条对齐。"""
    protocol = (ROOT / SNAPSHOT_PROTOCOL_FILE).read_text(encoding="utf-8")
    struct_start = protocol.find("pub struct AccountSnapshot {")
    struct_body = protocol[struct_start : protocol.find("\n}", struct_start)]
    declared = re.findall(r"^    pub (\w+_raw): (Option<i128>|i128),$", struct_body, re.M)
    scalars = sorted(name for name, kind in declared if kind == "Option<i128>")
    if struct_start < 0 or not scalars:
        check(
            False,
            "账户快照的钱字段名单能从协议结构体读出来（读不出来时下面几条全是假绿）",
            f"{SNAPSHOT_PROTOCOL_FILE} 里找不到 `pub struct AccountSnapshot {{ … }}`",
        )
        return

    # 算点只认赋值形状：构造被下一条判据钉成"只能走 new()"，所以生产侧要给出一个数只能写赋值。
    # 协议定义文件本身只有"构造默认 None"与 SnapshotDiff 的逐字段转发，两处都不是算点。
    produced: set[str] = set()
    literals: list[str] = []
    for path in sorted(CRATES.glob("*/src/**/*.rs")):
        relative = path.relative_to(ROOT).as_posix()
        if TEST_PATH.search(relative) or relative == SNAPSHOT_PROTOCOL_FILE:
            continue
        text = production_text(path.read_text(encoding="utf-8"))
        for name in scalars:
            if re.search(rf"[A-Za-z_]\w*\.{name}\s*=[^=]", text):
                produced.add(name)
        if ACCOUNT_SNAPSHOT_LITERAL.search(text):
            literals.append(relative)
    uncomputed = [name for name in scalars if name not in produced]
    computed = [name for name in scalars if name in produced]
    check(
        sorted(uncomputed) == sorted(SNAPSHOT_UNCOMPUTED_FIELDS) and len(scalars) == 8,
        "无生产者的账户钱字段由源码派生，与判据常量同一份：谁接上生产者这一步先知道",
        f"协议声明 {len(scalars)} 格、派生未算 {uncomputed}、常量 {list(SNAPSHOT_UNCOMPUTED_FIELDS)}",
    )
    check(
        not literals,
        "生产代码构造账户快照只能走 AccountSnapshot::new()（八格默认 None），"
        "凭空造数必须留在赋值点上、被派生清单抓到",
        f"出现结构体字面量构造点 {literals}",
    )

    schema = json.loads((ROOT / SNAPSHOT_SCHEMA_FILE).read_text(encoding="utf-8"))
    descriptions = {
        name: str(schema["properties"].get(name, {}).get("description", "")) for name in scalars
    }
    schema_absent = sorted(n for n, d in descriptions.items() if ACCOUNT_MONEY_ABSENT_MARKER in d)
    schema_computed = sorted(n for n, d in descriptions.items() if ACCOUNT_MONEY_COMPUTED_MARKER in d)
    check(
        all(descriptions.values())
        and not (set(schema_absent) & set(schema_computed))
        and schema_absent == sorted(uncomputed)
        and schema_computed == sorted(computed),
        "对外公布的契约逐字段写明 null 的含义：未算的点名、有算点的点名，两种标记互斥且不漏格",
        f"缺描述 {[n for n, d in descriptions.items() if not d]}"
        f" / 契约称未算 {schema_absent} / 契约称有算点 {schema_computed}",
    )

    capabilities = (ROOT / "maturity" / "capabilities.yaml").read_text(encoding="utf-8")
    listed_absent = sorted(
        name
        for name in scalars
        if re.search(rf"^\s*- account_{name}_has_no_producer(?=[： (])", capabilities, re.M)
    )
    wrongly_claimed = sorted(
        name
        for name in computed
        if f"- account_{name}_has_no_producer" in capabilities
    )
    check(
        listed_absent == sorted(uncomputed)
        and not wrongly_claimed
        and ACCOUNT_MONEY_MERGED_LIMITATION not in capabilities,
        "能力矩阵把每一个无生产者字段各立一条 limitation：接上生产者就必须撤那一行，"
        "合并命名那句 blob 也不许回来",
        f"逐条在位 {listed_absent} / 有算点却被立成未算 {wrongly_claimed}"
        f" / 合并写法残留={ACCOUNT_MONEY_MERGED_LIMITATION in capabilities}",
    )

    readme = (ROOT / "deploy" / "README.md").read_text(encoding="utf-8")
    doc_sentence = re.search(ACCOUNT_MONEY_DOC_MARKER + r"\*?\*?：(.+?)。", readme, re.S)
    doc_names = (
        sorted(set(re.findall(r"`(\w+_raw)`", doc_sentence.group(1)))) if doc_sentence else []
    )
    check(
        doc_sentence is not None
        and doc_names == sorted(uncomputed)
        and "不是 0" in readme[doc_sentence.start() : doc_sentence.start() + 400],
        "接口文档向运维点名这份名单，并写明 null 不是 0：读侧不会把「没算」当成「没有」",
        f"命中标记句={doc_sentence is not None} / 文档点名 {doc_names}"
        f" / 派生未算 {sorted(uncomputed)}",
    )

    # 用例文本要剥注释：把那一格点名改成 `// ("frozen_raw", …)` 仍然是一处"用例还在"的假象。
    # 量的范围（MA6 / MA6b 实测）：这一颗问的是"这个字段在那条用例里还有没有名字"。用例现在有两处
    # 点名（None 断言的元组表 + 线格式循环的名册），只删其中一处它不红 —— 因为另一处仍在断言同一件事；
    # 两处都删掉才红。要更细的"每一处点名都必须在"就得钉死排版，那次重构会先被它咬住。
    case_source_text = without_line_comments(
        (ROOT / SNAPSHOT_CLI_CASE_FILE).read_text(encoding="utf-8")
    )
    case_start = case_source_text.find(f"fn {ACCOUNT_MONEY_CASE_NAME}(")
    case_body = (
        case_source_text[case_start : case_source_text.find("\n}\n", case_start)]
        if case_start >= 0
        else ""
    )
    case_names = sorted(set(re.findall(r'"(\w+_raw)"', case_body)))
    check(
        case_start >= 0
        and case_names == sorted(uncomputed)
        and r'\"margin_raw\":null' in case_body,
        "读侧那条 null 用例逐字段点名这三格，并真的断言了线格式与稳定 JSON 印 null 而不是 0",
        f"用例在位={case_start >= 0} / 用例点名 {case_names} / 派生未算 {sorted(uncomputed)}",
    )


# === V13 R26：账户级已实现/未实现盈亏的生产者必须是 Ledger 派生的 ===
# `account_money_field_registry_check` 只按"源码里有没有赋值点"派生名单——删掉算点它确实会红，
# 但它看不见算点**就地重算一遍公式**（于是与 Ledger 的已实现口径分叉）或把缺席退回成兜底 0。
ACCOUNT_PNL_PRODUCER_FILE = "crates/qx-cli/src/api_service.rs"
ACCOUNT_PNL_LEDGER_FILE = "crates/qx-core/src/ledger/query.rs"
# 两个算点的赋值形状与其必须调用的 Ledger 派生方法。
ACCOUNT_PNL_PRODUCERS = (
    ("realized_pnl_raw", "snapshot.realized_pnl_raw =", "realized_pnl_for("),
    ("unrealized_pnl_raw", "snapshot.unrealized_pnl_raw =", "unrealized_pnl_for("),
)
ACCOUNT_PNL_CASE_FILE = "crates/qx-cli/src/tests/api_snapshot_pnl_fields.rs"
ACCOUNT_PNL_CASE_NAME = "realized_and_unrealized_pnl_are_projected_from_the_ledger"


def account_pnl_producer_check() -> None:
    """账户级已实现/未实现盈亏自 V13 R26 起有生产者：算点取 Ledger 派生值，且有用例钉住。

    三颗分别钉：① 两个算点各只有一处且都调 Ledger 的派生方法（不是就地重算一遍公式，那会与
    Ledger 的已实现口径分叉）；② 派生方法住在 Ledger 查询面、未实现那把尺子缺标记价即 `None`
    （与 `equity_for` 同一条契约）、累计一律走 `checked_add`；③ 读侧行为用例在盘。
    """
    reader = production_text((ROOT / ACCOUNT_PNL_PRODUCER_FILE).read_text(encoding="utf-8"))
    ledger = production_text((ROOT / ACCOUNT_PNL_LEDGER_FILE).read_text(encoding="utf-8"))
    shapes = [
        (name, reader.count(assign), reader.count(call))
        for name, assign, call in ACCOUNT_PNL_PRODUCERS
    ]
    check(
        all(assign_count == 1 and call_count >= 1 for _, assign_count, call_count in shapes),
        "已实现/未实现盈亏各有一个算点，且都取 Ledger 的派生方法而不是就地重算一遍公式",
        f"算点/派生调用 {shapes}",
    )
    check(
        ledger.count("pub fn realized_pnl_for(&self, account_id: &str) -> Option<i128>") == 1
        and ledger.count("pub fn unrealized_pnl_for(") == 1
        and "marks.get(instrument)?" in _fn_body(ledger, "pub fn unrealized_pnl_for(")
        and "unwrap_or" not in _fn_body(ledger, "pub fn unrealized_pnl_for(")
        and "checked_add" in _fn_body(ledger, "pub fn unrealized_pnl_for("),
        "两个派生方法住在 Ledger 查询面：未实现那把尺子缺标记价即返回 None（与 equity 同一条契约），"
        "累计一律走 checked_add",
        "派生方法被删、搬走，或未实现那把尺子改成了兜底 0 / 会回绕的加法",
    )
    cases = (ROOT / ACCOUNT_PNL_CASE_FILE).read_text(encoding="utf-8")
    check(
        f"fn {ACCOUNT_PNL_CASE_NAME}(" in cases
        and "realized_pnl_raw" in cases
        and "unrealized_pnl_raw" in cases,
        "账户级盈亏的生产者有读侧用例：只开仓未平仓时是算出来的零、未实现与持仓行同一条现货尺子",
        f"{ACCOUNT_PNL_CASE_FILE} 里缺少用例 {ACCOUNT_PNL_CASE_NAME}",
    )


# === V13 R26：qx-api 的状态锁中毒不再是 panic，而是显式 503（§6.D3） ===
# 19 处就地 `.lock().expect("api state mutex poisoned")` 的公共危害不是"这一次请求失败"，而是
# 之后**每一个**请求线程都在同一次 `.lock()` 上 panic——一次中毒把整个读面变成永久断连，
# 运维侧看不到任何 5xx。收敛成一个可失败入口 + 一个状态码分档的收口，这里钉住它不退化。
API_LOCK_MODULE_FILE = "crates/qx-api/src/state_lock.rs"
API_LOCK_LIB_FILE = "crates/qx-api/src/lib.rs"
API_LOCK_HELPER = (
    "pub(crate) fn lock_state(state: &Mutex<ApiState>) -> Result<MutexGuard<'_, ApiState>, String>"
)
API_LOCK_POISONED_CONST = 'pub(crate) const API_STATE_LOCK_POISONED: &str = "api_state_lock_poisoned";'
API_LOCK_CASE_FILE = "crates/qx-api/tests/api_lock_fail_closed.rs"
API_LOCK_PANIC_PATTERN = re.compile(r'\.expect\(\s*"api state mutex poisoned"')


def api_lock_fail_closed_check() -> None:
    """`ApiState` 的锁中毒必须回 503，不许 panic，也不许兜成空数据（V13 R26 / §6.D3）。"""
    module = (ROOT / API_LOCK_MODULE_FILE).read_text(encoding="utf-8")
    lib = production_text((ROOT / API_LOCK_LIB_FILE).read_text(encoding="utf-8"))
    # 取锁体按花括号取（`rustfmt` 会把 `state.lock().map_err(...)` 折成三行，整串匹配会假红）。
    lock_body = _fn_body(production_text(module), "pub(crate) fn lock_state(")
    check(
        API_LOCK_HELPER in module
        and API_LOCK_POISONED_CONST in module
        and ".lock()" in lock_body
        and ".map_err(|_| API_STATE_LOCK_POISONED.to_string())" in lock_body,
        "取锁收成一个可失败入口：中毒返回原因串而不是 panic",
        f"{API_LOCK_MODULE_FILE} 里缺少 lock_state / 中毒常量 / map_err 收口",
    )
    check(
        "let status = if error == API_STATE_LOCK_POISONED {" in module
        and "mod state_lock;" in lib
        and "pub(crate) use state_lock::{lock_state, read_error_response};" in lib,
        "读面的错误收口把中毒判成 503、其余判成 400，且 state_lock 挂进 lib.rs 并重导出",
        "read_error_response 的状态码分档丢失，或 state_lock 没被挂载/重导出",
    )
    # 生产代码里不许再有就地 expect 的锁取用：一处复活，那条读面就又会在中毒时 panic。
    # 按 `production_text` 判（剥注释与测试项），免得 state_lock.rs 的模块 doc 自己举的例子被当成犯规。
    offenders = sorted(
        path.relative_to(ROOT).as_posix()
        for path in (ROOT / "crates/qx-api/src").rglob("*.rs")
        if not TEST_PATH.search(path.relative_to(ROOT).as_posix())
        and API_LOCK_PANIC_PATTERN.search(production_text(path.read_text(encoding="utf-8")))
    )
    check(
        offenders == [],
        "qx-api 生产代码里不再有 `.expect(\"api state mutex poisoned\")` 的就地取锁",
        f"仍会 panic 的位置 {offenders}",
    )
    check(
        lib.count("read_error_response(&error)") >= 6
        and "Err(error) => return Some(error)," in lib,
        "六条带键读面走 read_error_response 分档，/ready 把中毒原样报成未就绪原因",
        f"read_error_response 调用 {lib.count('read_error_response(&error)')} 处",
    )
    cases = (ROOT / API_LOCK_CASE_FILE).read_text(encoding="utf-8")
    check(
        "fn poisoned_state_lock_turns_http_reads_into_503_not_a_panic(" in cases
        and "fn healthy_state_lock_still_serves_the_same_reads(" in cases
        and "is_poisoned()" in cases,
        "行为用例在盘：真把锁弄中毒后逐条读面回 503，并有一条「锁健康时照常服务」的对照组",
        f"{API_LOCK_CASE_FILE} 里缺少中毒用例或对照组",
    )


# === V12 R2：账户快照的契约版本必须"会失败" ===
# 版本号有三处说法（Rust 常量 / 对外公布的契约文本 / Python 桥），必须同号，否则
# "只接受契约声明的 const"这句话本身就是假话。公布的那份文本自 V12 审计第三遍起只有
# `schemas/account-snapshot-v1.json` 一份——crate 里过去另手写了一份，两份的字段约束不一致。
SNAPSHOT_CONTRACT_REPO_FILE = "schemas/account-snapshot-v1.json"
SNAPSHOT_CONTRACT_BRIDGE_FILE = "python/qianxing_bridge/__init__.py"
# 两道版本闸门各守一个读入口（`from_json` 的稳定 JSON 与 `from_wire_json` 的 wire JSON）；
# `validate()` 只做"这一份文档自洽吗"，不放闸门 —— 自洽的跨版本文档必须拿到"版本不受支持"
# 这句话，而不是"某个字段缺失"，两件事实得分开说（V12 R2）。少一道闸门就等于放行。
SNAPSHOT_VERSION_GATES = (
    "if top_schema != ACCOUNT_SNAPSHOT_SCHEMA_VERSION as u64 {",
    "if snapshot.header.schema_version != ACCOUNT_SNAPSHOT_SCHEMA_VERSION {",
)
# `validate()` 里不得出现任何版本等值判断：闸门只放在两个读入口，"自洽体检"与
# "认不认这个版本"是两件事实，各自那句话要能单独说清（V12 R2）。
SNAPSHOT_VALIDATE_VERSION_CHECK = "self.header.schema_version !="
SNAPSHOT_RUST_VERSION = re.compile(
    r"pub const ACCOUNT_SNAPSHOT_SCHEMA_VERSION: u32 = (\d+);"
)
SNAPSHOT_SCHEMA_CONST = re.compile(r'"schema_version"\s*:\s*\{\s*"const"\s*:\s*(\d+)\s*\}')
SNAPSHOT_BRIDGE_VERSION = re.compile(r'value\["schema_version"\]\s*!=\s*(\d+)')
# header 那份副本：缺席才按顶层补写，声明了就必须读得成与顶层同号的无符号整数。
SNAPSHOT_HEADER_NORMALIZATION = (
    'match header.get("schema_version") {',
    "None => {",
    "declared.as_u64().ok_or_else(",
    "if declared != top_schema {",
)
SNAPSHOT_CONTRACT_CASES = (
    "self_consistent_unknown_schema_version_is_rejected_by_every_entry",
    "header_version_that_is_not_the_declared_integer_is_rejected",
    "header_without_version_is_the_stored_shape_and_still_round_trips",
    "known_version_constant_matches_the_served_contract",
    # 高版本且本构建恰好读不出形状的那一份：理由仍然必须是版本，不是字段缺失。
    "future_version_that_cannot_be_deserialized_is_still_refused_as_a_version_problem",
)


def without_line_comments(text: str) -> str:
    """去掉整行 `//`（含 `///` 文档）：注释里引用的契约片段不是第二份手抄。"""
    return "\n".join(
        line for line in text.splitlines() if not line.lstrip().startswith("//")
    )


def strategy_pipe_write_budget_check() -> None:
    """Python 策略管道传输的 stdin 写入必须与读取同受 timeout_ms 约束（V13 R2 #281 / R17 fam04）。

    `Jsonl`/`FramedJson` 是默认发布路径（strategy_schema.rs 默认 Jsonl）。修复前 write_all 直接
    跑在主线程，只有后面的 recv_timeout 收口——一旦子进程"活着但不再读 stdin"，超出 OS 匿名
    管道缓冲（~64KB）的那段 write_all 会永久阻塞，timeout_ms 管不到，本进程就此卡死。R17 把三处
    同类缺陷（策略 worker / event consumer handler / CCXT worker）收进唯一一条带预算的写入通道
    `qx_adapter::write_all_within`（crates/qx-adapter/src/io_budget.rs），判据改钉"这一处走的是统一
    通道、预算取自 self.timeout_ms、失败把打断管道的责任交回调用方并留可读诊断"。
    """
    host = (CRATES / "qx-cli/src/strategy_host.rs").read_text(encoding="utf-8")
    start = host.index("StrategyTransport::Jsonl | StrategyTransport::FramedJson => {")
    end = host.index("let line = match response", start)
    region = host[start:end]
    check(
        region.count("qx_adapter::write_all_within(") == 1
        and region.count(".write_all(") == 0
        and "self.stdin.as_mut()" not in region
        and region.index("write_all_within(") < region.index("recv_timeout("),
        "策略管道写入走统一带预算通道 write_all_within，请求路径不再直接写 stdin",
        f"write_all_within {region.count('qx_adapter::write_all_within(')}（期望 1）、"
        f"裸 write_all {region.count('.write_all(')}（期望 0）、"
        f"残留 as_mut 直接写={'self.stdin.as_mut()' in region}",
    )
    # 预算必须真的取自 timeout_ms，而不是拍一个大常数把无限阻塞换成"很久后才失败"。断言限定在
    # `write_all_within` 的调用窗口内：读侧那条 recv_timeout 也引用同一个字段，只查"区域里出现过"
    # 会把写侧被换成常数这件事放过去。
    wcall = ""
    if "write_all_within(" in region:
        wstart = region.index("write_all_within(")
        wcall = region[wstart : wstart + 240]
    check(
        "Duration::from_millis(self.timeout_ms)" in wcall,
        "写侧等待预算与读侧同源，取自 self.timeout_ms",
        "write_all_within 调用窗口内没有引用 self.timeout_ms",
    )
    # 失败是可观测事实：写侧失败带 death_note，且成功分支回收一次句柄（超时那支刻意不回置）。
    check(
        region.count("self.stdin = Some(stdin)") == 1 and region.count("death_note()") >= 1,
        "写侧失败留下可读诊断并在成功分支回收 stdin 句柄（不吞成断链/超时噪声）",
        f"句柄回收点 {region.count('self.stdin = Some(stdin)')}（期望 1）、"
        f"death_note 调用 {region.count('death_note()')}（期望 >=1）",
    )

def event_consumer_pipe_write_budget_check() -> None:
    """事件 consumer handler 的 stdin 写入必须与退出轮询同受 handler.timeout_ms 约束
    （V13 R2 第三十五遍 #282 / R17 fam04）。

    `invoke_event_consumer_handler`（event_pipeline.rs，`#[cfg(feature = "nats")]` 下）修复前把
    write_all 直接跑在主线程，只有后面的 try_wait 轮询守 timeout_ms——一旦用户配置的外部 handler
    "存活却不从 stdin 取字节"，超出 OS 匿名管道缓冲（~64KB）的那段 write_all 会永久阻塞，timeout_ms
    管不到，本进程就此卡死。跨进程 Outbox 事件里 AccountPositionSnapshot/AccountBalanceSnapshot 把
    整段 Vec 内联进单条 payload，足以越过 64KB，故不是假设场景。R17 与另外两处（策略 worker、CCXT
    worker）共用同一条带预算的写入通道 `qx_adapter::write_all_within`，判据改钉"这一处走的是统一
    通道、预算取自 handler.timeout_ms、失败留可读诊断"。
    """
    pipeline = (CRATES / "qx-cli/src/event_pipeline.rs").read_text(encoding="utf-8")
    start = pipeline.index('启动事件 consumer handler 失败')
    end = pipeline.index("let started = Instant::now();", start)
    region = pipeline[start:end]
    check(
        region.count("qx_adapter::write_all_within(") == 1
        and region.count(".write_all(") == 0
        and "child.stdin.take()" in region
        and region.index("child.stdin.take()") < region.index("write_all_within("),
        "事件 consumer 写入走统一带预算通道 write_all_within，主路径不再直接阻塞在 stdin",
        f"write_all_within {region.count('qx_adapter::write_all_within(')}（期望 1）、"
        f"裸 write_all {region.count('.write_all(')}（期望 0）",
    )
    # 预算必须真的取自 handler.timeout_ms，而不是拍一个大常数把无限阻塞换成"很久后才失败"。
    wcall = ""
    if "write_all_within(" in region:
        wstart = region.index("write_all_within(")
        wcall = region[wstart : wstart + 240]
    check(
        "Duration::from_millis(handler.timeout_ms)" in wcall,
        "写侧等待预算与退出轮询同源，取自 handler.timeout_ms",
        "write_all_within 调用窗口内没有引用 handler.timeout_ms",
    )
    # 失败是可观测事实：点名是写 stdin 失败，不吞成"handler 什么都没发生"。
    check(
        "consumer stdin 写入失败" in region,
        "写侧失败留下可读诊断（不吞成断链/超时噪声）",
        "写侧失败诊断不在位",
    )


def ccxt_pipe_write_budget_check() -> None:
    """公共 CCXT 进程桥的 stdin 写入必须与读取同受 timeout_ms 约束（V13 R4-A / R17 fam04）。

    `CcxtProcessClient::call`（qx-adapter/src/ccxt.rs）修复前把 write_all 直接跑在主线程，
    只有后面的 recv_timeout 收口——一旦 Python CCXT worker "活着但不再读 stdin"（例如卡在一次
    长网络调用里），超出 OS 匿名管道缓冲（~64KB）的那段 write_all 会永久阻塞，timeout_ms 管不到，
    本进程就此卡死。这与策略宿主（V13 R2 #281）和 event consumer handler（#282）是同一个缺陷的
    第三处。R17 把三处收进唯一一条带预算的写入通道 `qx_adapter::write_all_within`，判据改钉
    "这一处走的是统一通道、预算取自 self.timeout_ms、成功分支回收句柄"。
    """
    source = (CRATES / "qx-adapter/src/ccxt.rs").read_text(encoding="utf-8")
    start = source.index('fn call(&mut self, request: Value) -> Result<Value, String> {')
    end = source.index("let response: Value = serde_json::from_str(&line)", start)
    region = source[start:end]
    check(
        region.count("write_all_within(") == 1
        and region.count(".write_all(") == 0
        and region.index("let stdin = self") < region.index("write_all_within("),
        "CCXT 进程桥写入走统一带预算通道 write_all_within，主路径不再直接阻塞在 stdin",
        f"write_all_within {region.count('write_all_within(')}（期望 1）、"
        f"裸 write_all {region.count('.write_all(')}（期望 0）",
    )
    # 预算必须真的取自 timeout_ms，而不是拍一个大常数把无限阻塞换成"很久后才失败"。
    wcall = ""
    if "write_all_within(" in region:
        wstart = region.index("write_all_within(")
        wcall = region[wstart : wstart + 240]
    check(
        "Duration::from_millis(self.timeout_ms)" in wcall,
        "CCXT 写侧等待预算与读侧同源，取自 self.timeout_ms",
        "write_all_within 调用窗口内没有引用 self.timeout_ms",
    )
    # 超时是可观测事实：成功分支回收一次句柄，失败按"提交结果未知"报出去。
    check(
        region.count("self.stdin = Some(") == 1 and "提交结果未知" in region,
        "CCXT 写侧在成功分支回收 stdin 句柄，失败按提交结果未知报出",
        f"句柄回收点 {region.count('self.stdin = Some(')}（期望 1）、"
        f"未知口径在位={'提交结果未知' in region}",
    )


def postgres_connect_budget_check() -> None:
    """PostgreSQL 建池的握手必须有时间预算：否则一台接受 TCP 却不完成握手的库
    （或挂起的 DNS）会让同步启动路径按存储后端逐个无限阻塞（V13 R2 第三十三遍 #263）。"""
    raw = (ROOT / "crates/qx-storage/src/postgres.rs").read_text(encoding="utf-8")
    code = without_line_comments(raw)
    check(
        code.count(".connect_timeout(") == 1 and code.count("Client::connect(") == 0,
        "PostgreSQL 建池走带 connect_timeout 的 Config::connect，握手不再无界阻塞",
        f"connect_timeout 调用 {code.count('.connect_timeout(')}（期望 1）、"
        f"无界 Client::connect( 残留 {code.count('Client::connect(')}（期望 0）",
    )
    # 预算常量必须是正秒数：把它改成 0 或删掉，等于没设界。
    positive = re.search(
        r"const\s+CONNECT_TIMEOUT[^;]*?Duration::from_secs\(\s*([1-9]\d*)\s*\)\s*;", raw
    )
    check(
        positive is not None,
        "握手预算常量是一个正的 Duration::from_secs(N)，不是 0/未定义",
        "未找到正的 CONNECT_TIMEOUT = ... Duration::from_secs(N)",
    )


def snapshot_contract_version_check() -> None:
    """账户快照的版本校验必须认"已知版本"，而不是只认"前后自洽"（V12 R2 / §4.2）。"""
    protocol = production_text((ROOT / SNAPSHOT_PROTOCOL_FILE).read_text(encoding="utf-8"))

    check(
        len(SNAPSHOT_RUST_VERSION.findall(protocol)) == 1
        and all(protocol.count(needle) == 1 for needle in SNAPSHOT_VERSION_GATES)
        and protocol.count(SNAPSHOT_VALIDATE_VERSION_CHECK) == 0
        and protocol.count("self.header.schema_version == 0") == 1,
        "已知版本常量声明一处，两个读入口各有一道常量闸门，validate 只做自洽体检",
        f"常量 {len(SNAPSHOT_RUST_VERSION.findall(protocol))} 处、"
        f"闸门在位 {[protocol.count(needle) for needle in SNAPSHOT_VERSION_GATES]}（期望 [1, 1]）、"
        f"validate 里的等值判断 {protocol.count(SNAPSHOT_VALIDATE_VERSION_CHECK)}（期望 0）",
    )
    declared = [int(v) for v in SNAPSHOT_RUST_VERSION.findall(protocol)]
    repository = [
        int(v)
        for v in SNAPSHOT_SCHEMA_CONST.findall(
            (ROOT / SNAPSHOT_CONTRACT_REPO_FILE).read_text(encoding="utf-8")
        )
    ]
    bridge = [
        int(v)
        for v in SNAPSHOT_BRIDGE_VERSION.findall(
            (ROOT / SNAPSHOT_CONTRACT_BRIDGE_FILE).read_text(encoding="utf-8")
        )
    ]
    check(
        len(declared) == 1 and len(repository) == 1 and len(bridge) == 1
        and len({*declared, *repository, *bridge}) == 1,
        "Rust 常量、公布契约与 Python 桥三处版本号同号，跨语言不会一个认一个不认",
        f"Rust {declared} / 契约 {repository} / Python 桥 {bridge}",
    )
    # 公布的那份契约只能有仓库这一份文本：V12 审计第三遍发现 crate 里另手写了一份，
    # 两份对 positions/orders/fills/transfers 的取值类型说法不同，一份能过另一份不能，
    # 而 deploy/README.md 宣称"同一份 schema"。手抄第二份的形态必须写不出来。
    code_only = without_line_comments(protocol)
    served_from_file = code_only.count(
        'include_str!("../../../schemas/account-snapshot-v1.json")'
    )
    check(
        served_from_file == 1 and not SNAPSHOT_SCHEMA_CONST.findall(code_only),
        "服务端公布的契约正文由 include_str 取仓库那一份，Rust 代码里不再手写第二份 schema",
        f"include_str 取用在位={served_from_file}，"
        f"Rust 代码里另写的 schema_version const={SNAPSHOT_SCHEMA_CONST.findall(code_only)}",
    )
    # 顶层是唯一真值，所以 header 里那份"声明了却读不出整数"的副本只能被拒，
    # 不能走缺席分支被洗成合法值。
    check(
        all(protocol.count(needle) == 1 for needle in SNAPSHOT_HEADER_NORMALIZATION)
        and protocol.count("header.insert(\"schema_version\"") == 1,
        "header 里的版本副本先取出再判定：读不出与顶层相同整数即拒，缺席才按顶层归一",
        f"归一四段在位 {[protocol.count(n) for n in SNAPSHOT_HEADER_NORMALIZATION]}（期望全 1）、"
        f"补写点 {protocol.count('header.insert(\"schema_version\"')}（期望 1）",
    )
    cases_path = CRATES / "qx-protocol/src/tests.rs"
    cases = cases_path.read_text(encoding="utf-8")
    check(
        all(f"fn {name}(" in cases for name in SNAPSHOT_CONTRACT_CASES),
        "R2 用例在位：自洽高版本被拒、假版本号被拒、缺席仍是存储常态、常量与契约同号、"
        "读不出的高版本也报版本问题",
        f"缺少用例：{[name for name in SNAPSHOT_CONTRACT_CASES if f'fn {name}(' not in cases]}",
    )
    check(
        (ROOT / SNAPSHOT_PROTOCOL_FILE).read_text(encoding="utf-8").count(
            "#[cfg(test)]\nmod tests;"
        )
        == 1,
        "协议用例住在 src/tests.rs 并由 crate 根挂载，crate 根不再替测试付行数",
        "crate 根的 tests 挂载点不再是唯一一处 mod tests;",
    )


# === V12 TX5 / V11 R14+R15：账户快照的四张键表只有编码器那一份 ===
# `to_json` 是手写的稳定编码器，四张键表此前各抄一遍 `format!`：键落成裸数字（`{7:{…}}` 不是
# 合法 JSON）、枚举落成数字码，而读侧 `from_json` 走 serde，认的是字符串键与变体名。两份编码
# 一分叉，写出去的订单要么谁也解不回（R14），要么新字段被写侧静默丢掉（R15）。合流后四张表
# 整份交给 `from_json` 认的那一次 serde 序列化，数字码只剩 `state_hash` 那一条编码通道。
SNAPSHOT_ROW_TABLES = ("orders", "fills", "transfers")
# 数字码只服务哈希编码器：它们一旦回到 JSON 写入段，写侧与读侧就又各认各的码了。
SNAPSHOT_ROW_CODE_DEFS = (
    "fn side_code(side: Side) -> u64 {",
    "fn order_status_code(status: OrderStatus) -> u64 {",
)
SNAPSHOT_ROW_CODE_NAMES = ("side_code", "order_status_code")
CORE_ORDER_FILE = "crates/qx-core/src/order.rs"
SNAPSHOT_ROW_CASES = ("rows_keyed_by_integer_still_produce_and_recover_valid_json",)


def snapshot_row_wire_check() -> None:
    """四张键表的写出各经一次 serde，数字码不得回到 JSON 通道（V12 TX5，与 V11 R14/R15 同点）。"""
    protocol = production_text((ROOT / SNAPSHOT_PROTOCOL_FILE).read_text(encoding="utf-8"))
    wire = (ROOT / POSITION_WIRE_FILE).read_text(encoding="utf-8")
    # 写入段：`to_json` 到 `from_json` 之间。四张表的槽位只能由那两个编码器填。
    writer_start = protocol.find("pub fn to_json(&self) -> String {")
    writer = protocol[writer_start : protocol.find("pub fn from_json")]

    # 上游那次 `snapshot_json_table_check` 钉的是"编码器定义全仓唯一、四张表都经它"。
    # 这里只补它没管的半步：写入段自己不碰 serde，持仓的键换成字符串那一次只在编码器里发生。
    check(
        writer_start > 0
        and all(
            f"let {table} = json_table_entries(&self.{table});" in writer
            for table in SNAPSHOT_ROW_TABLES
        )
        and "let positions = json_position_entries(&self.positions);" in writer
        and writer.count("serde_json::to_string(") == 0
        and protocol.count("json_table_entries(&keyed)") == 1,
        "写入段的四张表各取一次编码器输出、自己不序列化，持仓换键只在编码器里发生一次",
        f"orders/fills/transfers 命中 "
        f"{[f'let {t} = json_table_entries(&self.{t});' in writer for t in SNAPSHOT_ROW_TABLES]}、"
        f"positions={'let positions = json_position_entries(&self.positions);' in writer}、写入段自抄 "
        f"serde={writer.count('serde_json::to_string(')}、持仓转键="
        f"{protocol.count('json_table_entries(&keyed)')}（期望 True×4、0、1）",
    )
    # 数字码只服务 state_hash：定义处唯一、wire.rs 不再另立一份、JSON 写入段碰不到它，
    # 读侧也不再有一层码表折算（折算层本身就是"两份编码分叉"的第三份）。
    check(
        all(protocol.count(definition) == 1 for definition in SNAPSHOT_ROW_CODE_DEFS)
        and all(wire.count(name) == 0 for name in SNAPSHOT_ROW_CODE_NAMES)
        and all(writer.count(f"{name}(") == 0 for name in SNAPSHOT_ROW_CODE_NAMES)
        and "side_from_code" not in protocol
        and "order_status_from_code" not in protocol,
        "方向/状态数字码只有一处定义且只走 state_hash 通道，JSON 写入段与读侧折算层都碰不到它",
        f"定义处 {[protocol.count(d) for d in SNAPSHOT_ROW_CODE_DEFS]}、wire.rs 命中 "
        f"{[(n, wire.count(n)) for n in SNAPSHOT_ROW_CODE_NAMES]}、写入段命中 "
        f"{[(n, writer.count(f'{n}(')) for n in SNAPSHOT_ROW_CODE_NAMES]}（期望 1/1、0/0、0/0）",
    )
    # 线格式的枚举拼写现在只剩一个来源：qx-core 上那两条裸派生。给声明加 #[serde(...)]
    # （改名、`other`/`default` 兜底）就等于把"认不出来"洗成另一个合法拼写。
    # 注意 `OrderStatus::Unknown` 是业务档位（真实交易里确有"状态未知"这一格），不是 serde
    # 兜底变体：它不会被反序列化器拿去接住未知输入，拼错的变体名在 TX5 用例里当场报错。
    core_order = (ROOT / CORE_ORDER_FILE).read_text(encoding="utf-8")
    spelled_bare = []
    for name in ("Side", "OrderStatus"):
        start = core_order.find(f"pub enum {name} {{")
        preceding = core_order[:start].splitlines()
        attributes = ""
        while preceding and preceding[-1].startswith("#"):
            attributes = preceding.pop() + "\n" + attributes
        block = core_order[start : core_order.find("}", start) + 1]
        spelled_bare.append(
            start > 0
            and "#[serde" not in attributes
            and "#[repr" not in attributes
            and "#[serde" not in block
        )
    check(
        all(spelled_bare),
        "Side/OrderStatus 按变体名裸派生，两个枚举上没有任何 #[serde(...)] 改名或兜底",
        "两个枚举之一带 #[serde(...)]/#[repr(...)] 属性（未知值会被静默读成某一边）",
    )
    cases = (CRATES / "qx-protocol/src/tests.rs").read_text(encoding="utf-8")
    check(
        all(f"fn {name}(" in cases for name in SNAPSHOT_ROW_CASES),
        "TX5 用例在位：四张表带行的快照写出→读回，且未知数字码与拼错的变体名同路被拒",
        f"缺少用例：{[n for n in SNAPSHOT_ROW_CASES if f'fn {n}(' not in cases]}",
    )
    rows_case = (CRATES / "qx-storage/tests/snapshot_rows_persist.rs").read_text(encoding="utf-8")
    check(
        rows_case.count("snapshot.orders.insert(") == 1
        and "fn file_snapshot_store_recovers_a_snapshot_that_has_rows()" in rows_case
        and "fn sqlite_snapshot_store_recovers_a_snapshot_that_has_rows()" in rows_case,
        "落库读回用例在位：文件与 SQLite 两个后端各跑一次带行快照，且真带了一行订单",
        "存储层用例回到只验纯现金快照的形状",
    )
    # V11 R10：把同一条纪律推到对账两格。R7 只补上了"有来源"，剩下的半步是 0 仍同时表示
    # "从未对账"与"对过且无差异"——看板会把没跑过对账的账户读成绿色。三处编码都必须分开。
    api_side = (CRATES / "qx-cli/src/api_service.rs").read_text(encoding="utf-8")
    reconcile_writer_start = api_side.find("fn apply_reconcile_reports(")
    reconcile_writer = api_side[reconcile_writer_start:]
    cuts = [
        cut
        for stop in ("\nfn ", "\npub(crate) fn ", "\npub fn ")
        if (cut := reconcile_writer.find(stop, 1)) > 0
    ]
    reconcile_writer = reconcile_writer[: min(cuts)] if cuts else reconcile_writer
    cli_cases = (ROOT / SNAPSHOT_CLI_CASE_FILE).read_text(encoding="utf-8")
    check(
        reconcile_writer_start > 0
        and wire.count("pub last_reconcile_ts: Option<u64>,") == 1
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
    core = production_text((ROOT / POSITION_CORE_FILE).read_text(encoding="utf-8"))
    wire = production_text((ROOT / POSITION_WIRE_FILE).read_text(encoding="utf-8"))
    protocol = production_text((ROOT / SNAPSHOT_PROTOCOL_FILE).read_text(encoding="utf-8"))
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
    protocol_cases = case_source(SNAPSHOT_CORE_CASE_FILE)
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
    api = production_text((ROOT / RECONCILE_REPORT_FILE).read_text(encoding="utf-8"))
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


# V12 §18（交易 TX6/TX7）：对账链上两处"只有一处真口径"被绕开。
# TX6 —— 柜台这一轮没报结算币种时，余额对账曾把它折成 0 参与比较，等于替交易所报了一个
# 它没报的数；现在缺席是 `venue_raw: None`，播报印「未报」。
# TX7 —— 差异投影成事实时用的是维度码（status_mismatch / filled_mismatch），而"能不能自动
# 收敛"这套动作口径（VerdictAction）从头到尾没有读者：远端权威推进与互斥迁移需人工在下游
# 是同一件事。现在两个维度的动作判据各只有一处定义，事实 reason 与报告 action 都取自动作口径。
BALANCE_ABSENCE_CASE_FILE = "crates/qx-runtime/tests/settlement_balance_absence.rs"
GENGLU_ORDER_RECONCILE_FILE = "crates/qx-genglu/src/reconcile/order.rs"
ADAPTER_RECONCILE_FILE = "crates/qx-adapter/src/reconcile.rs"
RUNTIME_PIPELINE_FILE = "crates/qx-runtime/src/pipeline.rs"
BALANCE_ABSENCE_CASES = (
    "reported_zero_balance_is_not_a_discrepancy",
    "missing_settlement_asset_is_reported_absent_not_as_zero",
    "reported_balance_keeps_the_number",
)
VERDICT_ACTION_CASES = (
    "the_same_status_dimension_reports_two_different_actions",
    "filled_direction_and_absences_keep_their_own_actions",
)


def reconcile_action_and_absence_check() -> None:
    """柜台未报的币种要报缺席，对账差异要按裁决动作归类，两处判据各只有一处真口径。"""
    pipeline = (ROOT / RUNTIME_PIPELINE_FILE).read_text(encoding="utf-8")
    binance_tight = "".join(
        production_text((ROOT / BINANCE_RECONCILE_FILE).read_text(encoding="utf-8")).split()
    )
    adapter = (ROOT / ADAPTER_RECONCILE_FILE).read_text(encoding="utf-8")
    adapter_prod = production_text(adapter)
    verdict = production_text((ROOT / GENGLU_ORDER_RECONCILE_FILE).read_text(encoding="utf-8"))
    absence_cases = (ROOT / BALANCE_ABSENCE_CASE_FILE).read_text(encoding="utf-8")

    check(
        pipeline.count("pub venue_raw: Option<i128>,") == 1
        and "if venue_raw == Some(ledger_raw)" in pipeline,
        "柜台余额是 Option<i128> 且只在报过数时才可能判为一致：没报与该币种为零是两种状态",
        "字段又回到 i128，或把缺席与 0 合并比较",
    )
    body = _fn_body(pipeline, "pub fn settlement_balance_discrepancies(")
    check(
        ".unwrap_or(0)" not in body
        and "net_cash_raw()" in body
        and 'QxError::ReconcileRequired("柜台余额相加溢出"' in body,
        "余额对账不再用 unwrap_or(0) 替柜台补一个数，溢出仍然硬失败",
        f"函数体里又出现补零 {[l.strip() for l in body.splitlines() if '.unwrap_or(0)' in l]}",
    )
    check(
        "discrepancy.venue_raw.map_or_else(" in binance_tight
        and '"未报".to_string()' in binance_tight,
        "Binance 播报里缺席的币种印「未报」，不印 0",
        "详情串又把缺席折成数字",
    )
    check(
        all(f"fn {name}(" in absence_cases for name in BALANCE_ABSENCE_CASES),
        "缺席口径三条用例在位：报了 0 不算差异、没报落 None 且与「只报别的币种」同形、报过数仍是那个数",
        f"缺用例 {[n for n in BALANCE_ABSENCE_CASES if f'fn {n}(' not in absence_cases]}",
    )
    check(
        verdict.count("pub fn status_action(") == 1
        and verdict.count("pub const fn filled_action(") == 1
        and verdict.count("== VerdictAction::ManualReview") == 2
        and verdict.count("can_transition_to") == 1,
        "状态与数量两个维度的动作判据各只有一处定义，裁决本身只调用不另起比较",
        f"定义 {[n for n in ('pub fn status_action(', 'pub const fn filled_action(') if verdict.count(n) != 1]}"
        f" / 裁决复用 {verdict.count('== VerdictAction::ManualReview')} 次 / 迁移判据 {verdict.count('can_transition_to')} 处",
    )
    check(
        "pub fn action(&self) -> VerdictAction" in adapter_prod
        and all(name in adapter_prod for name in ("status_action", "filled_action"))
        and "can_transition_to" not in adapter_prod
        and "> venue" not in adapter_prod,
        "适配器把动作归类委托给 qx-genglu 的同一对判据，自己不重复比较状态或数量",
        "适配器里又长出第二份迁移/回退比较",
    )
    check(
        binance_tight.count(
            ".require_reconcile(issue.client_order_id(),issue.action().reason_code())"
        )
        == 1
        and '"action":issue.action().reason_code(),' in binance_tight
        and ".require_reconcile(issue.client_order_id(),issue.reason_code())" not in binance_tight,
        "待对账事实的 reason 与报告 action 都取自动作口径：维度码 status_mismatch 不再是事实 reason",
        "写入点又回到维度码，或报告少了 action 一栏",
    )
    check(
        all(f"fn {name}(" in adapter for name in VERDICT_ACTION_CASES)
        and '"status_mismatch|resync"' in adapter
        and '"status_mismatch|manual_review"' in adapter,
        "同一维度两种动作有配对用例：状态合法前进 resync、互斥迁移 manual_review",
        f"缺用例 {[n for n in VERDICT_ACTION_CASES if f'fn {n}(' not in adapter]}",
    )


# V12 §18 TX8：租约域（epoch 秒）与墙钟（epoch 毫秒）之间只允许一处换算。
LEASE_WIRING_FILE = "crates/qx-cli/src/runtime_wiring.rs"
LEASE_SCHEDULER_FILE = "crates/qx-cli/src/scheduler.rs"
LEASE_WORKERS_FILE = "crates/qx-cli/src/workers.rs"
LEASE_EVENT_PIPELINE_FILE = "crates/qx-cli/src/event_pipeline.rs"
LEASE_CASE_FILE = "crates/qx-cli/src/tests/lease_clock_domain.rs"
# 每个文件里"租约域调用必须带 lease_ 前缀参数"的紧化写法与它的毫秒旧写法。
LEASE_CALL_SITES = (
    ("crates/qx-cli/src/workers.rs", ".available(lease_now)", ".available(now)"),
    (
        "crates/qx-cli/src/workers.rs",
        ".claim(queued.run.run_id,context.id(),lease_now,30)",
        ".claim(queued.run.run_id,context.id(),now,30)",
    ),
    (
        "crates/qx-cli/src/workers.rs",
        ".ack_at(queued.run.run_id,context.id(),lease.fencing_token,lease_now)",
        ".ack_at(queued.run.run_id,context.id(),lease.fencing_token,now)",
    ),
    (
        "crates/qx-cli/src/workers.rs",
        ".claim_command(command.command_id,context.id(),lease_now,30)",
        ".claim_command(command.command_id,context.id(),now,30)",
    ),
    (
        "crates/qx-cli/src/workers.rs",
        ".available_commands(lease_now)",
        ".available_commands(now)",
    ),
    (
        "crates/qx-cli/src/venue_runtime/binance_submit.rs",
        ".claim_command(command.command_id,&owner,lease_now,30)",
        ".claim_command(command.command_id,&owner,now,30)",
    ),
    (
        "crates/qx-cli/src/venue_runtime/ccxt_execution.rs",
        ".claim_command(command.command_id,&owner,lease_now,30)",
        ".claim_command(command.command_id,&owner,now,30)",
    ),
    (
        "crates/qx-cli/src/venue_runtime/paper_worker.rs",
        ".claim_command(command.command_id,context.id(),lease_now,30)",
        ".claim_command(command.command_id,context.id(),now,30)",
    ),
    (
        "crates/qx-cli/src/venue_runtime/paper_submit.rs",
        '.claim_command(command.command_id,"paper-execution",lease_now,30)',
        '.claim_command(command.command_id,"paper-execution",now,30)',
    ),
)
LEASE_ACK_PACKED = ".ack_command_at(command.command_id,{owner},lease.fencing_token,{now})"
LEASE_ACK_OWNERS = (
    ("crates/qx-cli/src/venue_runtime/binance_submit.rs", "&owner", 2),
    ("crates/qx-cli/src/venue_runtime/ccxt_execution.rs", "&owner", 2),
    ("crates/qx-cli/src/venue_runtime/paper_worker.rs", "context.id()", 2),
)
LEASE_ENQUEUE_SITES = (
    ("crates/qx-cli/src/workers.rs", 1),
    ("crates/qx-cli/src/spread.rs", 2),
    ("crates/qx-cli/src/api_service.rs", 1),
    ("crates/qx-cli/src/venue_runtime/binance_submit.rs", 1),
    ("crates/qx-cli/src/venue_runtime/ccxt_execution.rs", 1),
    ("crates/qx-cli/src/venue_runtime/paper_submit.rs", 1),
    ("crates/qx-cli/src/venue_runtime/paper_worker.rs", 1),
)
LEASE_CASES = (
    "lease_clock_truncates_the_wall_clock_to_seconds",
    "a_thirty_second_lease_survives_twenty_nine_seconds_of_work",
    "live_strategy_run_deadline_is_measured_in_the_lease_domain",
    "an_overdue_running_job_is_escalated_and_frees_its_concurrency_key",
    "escalating_the_stuck_run_lets_the_next_trading_day_dispatch",
)


def lease_clock_domain_check() -> None:
    """租约/调度按秒、墙钟按毫秒：跨域必须走 `lease_clock`，超时升级必须有读者（V12 §18 TX8）。"""
    wiring = production_text((ROOT / LEASE_WIRING_FILE).read_text(encoding="utf-8"))
    scheduler = (ROOT / LEASE_SCHEDULER_FILE).read_text(encoding="utf-8")
    scheduler_prod = production_text(scheduler)
    workers_prod = "".join(
        production_text((ROOT / LEASE_WORKERS_FILE).read_text(encoding="utf-8")).split()
    )
    pipeline_prod = "".join(
        production_text((ROOT / LEASE_EVENT_PIPELINE_FILE).read_text(encoding="utf-8")).split()
    )
    cases = (ROOT / LEASE_CASE_FILE).read_text(encoding="utf-8")

    check(
        wiring.count("pub(crate) fn lease_clock(") == 1
        and "timestamp_ms / 1_000" in _fn_body(wiring, "pub(crate) fn lease_clock("),
        "毫秒墙钟到租约秒只有一个换算入口，且它确实除以 1000",
        "lease_clock 定义数或换算式变了",
    )
    def tight(path: str) -> str:
        # 尾逗号归一：rustfmt 把长实参拆成多行时会给最后一个实参补上尾逗号，按字面比对的
        # 判据立刻读出 0 处 —— 本轮 paper_worker.rs 的 `ack_command_at` 秒域判据就是这么红的。
        # 判据要认的是「第 4 个实参拿的是哪把时钟」，不是换行与尾逗号（见 `_collapsed_code`）。
        return _collapsed_code(
            production_text((ROOT / path).read_text(encoding="utf-8")),
            tight=True,
        )

    bad_units = [
        f"{path}: 缺 {ok}" if ok not in tight(path) else f"{path}: 退回 {bad}"
        for path, ok, bad in LEASE_CALL_SITES
        if ok not in tight(path) or bad in tight(path)
    ]
    check(
        not bad_units,
        "作业队列与命令队列的 claim/available 一律拿秒域时钟：30 秒租约不再 30 毫秒就过期",
        f"; ".join(bad_units),
    )
    bad_acks = [
        f"{path}: ack_command_at 秒域写法 {tight(path).count(LEASE_ACK_PACKED.format(owner=owner, now='lease_now'))}/{expected}"
        for path, owner, expected in LEASE_ACK_OWNERS
        if tight(path).count(LEASE_ACK_PACKED.format(owner=owner, now="lease_now")) != expected
        or LEASE_ACK_PACKED.format(owner=owner, now="now") in tight(path)
    ]
    check(
        not bad_acks,
        "执行 worker 收尾确认与领取用同一把秒域时钟，作业不会被自己的租约判过期",
        f"; ".join(bad_acks),
    )
    bad_enqueues = []
    for path, expected in LEASE_ENQUEUE_SITES:
        text = tight(path)
        # 秒域写法只有这两种形状（就地换算与提前换算）；第三种写法就是往秒域队列里塞毫秒。
        lease_form = text.count(".enqueue_command(command.clone(),lease_now)") + text.count(
            ".enqueue_command(command,lease_clock("
        )
        if text.count(".enqueue_command(") != expected or lease_form != expected:
            bad_enqueues.append(f"{path} 秒域入队 {lease_form}/{expected}")
    check(
        not bad_enqueues,
        "八处命令队列入口全部按秒域写时间戳，且没有第九处绕开换算（同一队列混单位会让出队顺序倒置）",
        f"{'；'.join(bad_enqueues)}",
    )
    check(
        pipeline_prod.count(".pump_once(lease_clock(") == 3
        and ".pump_once(now" not in pipeline_prod
        and ".pump_once(runtime_timestamp_ms()" not in pipeline_prod
        and pipeline_prod.count("parked={}") == 3,
        "三个 Outbox relay 出口都把墙钟换算成秒后才比较租约，并把停在门后的条数一起念出来（V13 R5）",
        f"pump_once 秒域写法 {pipeline_prod.count('.pump_once(lease_clock(')} 处、"
        f"念出 parked 的出口 {pipeline_prod.count('parked={}')} 处（各期望 3）——"
        "单次批处理退 0 只代表这一页搬完了，不念 parked 就是把「还剩 N 条毒事件」报成「中继干净」",
    )
    dispatch = _fn_body(scheduler_prod, "pub(crate) fn dispatch_scheduled_jobs(")
    dispatch_tight = "".join(dispatch.split())
    check(
        "forruninscheduler.runs(){" in dispatch_tight
        and "scheduler.is_timed_out(run.run_id,lease_now)" in dispatch_tight
        and "scheduler.mark_timed_out(run.run_id,lease_now)" in dispatch_tight,
        "调度 tick 真的遍历运行并升级超时作业：is_timed_out/mark_timed_out 不再是只有测试在用的孤儿",
        "升级循环缺失",
    )
    check(
        "start_run_at(&job_id,trading_day,manifest.digest(),lease_now" in dispatch_tight
        and "start_run_at(&job_id,trading_day,manifest.digest(),now" not in dispatch_tight
        and "enqueue(job,run,lease_now)" in dispatch_tight,
        "派发写入的 JobRun 与入队时间戳都在秒域，与升级判定同一把时钟",
        "派发消息又拿毫秒当租约时钟",
    )
    check(
        "Err(qx_scheduler::SchedulerError::NotReady(_))" in dispatch
        and "skipped += 1;" in dispatch,
        "并发键被占用只跳过本轮，不让一个作业打死整个调度器",
        "NotReady 又变成整轮 Err",
    )
    live = _fn_body(scheduler_prod, "pub(crate) fn live_strategy_job(")
    live_tight = "".join(live.split())
    check(
        "deadline_ts:lease_now.saturating_add(job.timeout_seconds)" in live_tight
        and "60_000" not in live_tight
        and "timeout_seconds:LIVE_STRATEGY_TIMEOUT_SECONDS" in live_tight
        and "started_ts:lease_now" in live_tight,
        "实时策略作业的运行记录以秒填写，deadline 直接由 timeout_seconds 推出",
        "运行记录退回毫秒 deadline",
    )
    check(
        'queued={}timed_out={}skipped={}"' in workers_prod
        and "dispatch.timed_out" in workers_prod,
        "超时升级数量对运维可见，不是只在状态文件里默默变化",
        "调度播报少了 timed_out 一栏",
    )
    missing_cases = [name for name in LEASE_CASES if f"fn {name}(" not in cases]
    check(
        not missing_cases
        and "assert_eq!(takeover.fencing_token, lease.fencing_token + 1);" in cases
        and "assert_eq!(run.status, JobStatus::NeedsIntervention);" in cases
        and "assert_eq!(mid.timed_out, 0);" in cases
        and "assert_eq!(late.timed_out, 1);" in cases
        and "assert_eq!(late.queued, 1" in cases,
        "租约存活、未到期不误判、超时升级、并发键重新派发四断言各有用例钉住",
        f"缺用例 {missing_cases} 或断言被删/改向",
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
    api = production_text((ROOT / "crates/qx-api/src/lib.rs").read_text(encoding="utf-8"))
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
    protocol = production_text((ROOT / SNAPSHOT_PROTOCOL_FILE).read_text(encoding="utf-8"))
    endpoint = production_text((ROOT / SNAPSHOT_ENDPOINT_FILE).read_text(encoding="utf-8"))
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
    # 鉴权白名单按 `!matches!(route, …)` 取数。这里必须容忍换行/缩进：名单每加一条，
    # rustfmt 就会把这行折成多行，而"折行"与"名单被删空"是两件事——判据只该对后者出声。
    whitelist = re.search(r"!matches!\(\s*route\s*,([^)]*)\)", endpoint)
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
                "日历夹具或摘要文件缺一：这一对必须同时在磁盘上，缺哪一对当场点名",
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
    报错。CI 还会把 C++ 示例动态库交给 Rust `DynamicCAbiStrategy` 加载并执行一个事件，
    覆盖头文件布局、导出符号与实际回调的跨语言链路。
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
        # qx_order_side 是定宽整数的 typedef，不是枚举：宿主必须能拒绝非法判别值，
        # 而 #[repr(C)] 枚举读到未声明的值本身就是 UB，match 里没有可达的拒绝臂。
        "qx_order_side": "u32",
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
    check(
        "#define QX_STRATEGY_EXPORT __declspec(dllexport)" in header
        and "QX_STRATEGY_EXPORT const qx_strategy_vtable* qx_strategy_get_vtable(void);" in header,
        "Windows C++ 策略插件显式导出 Rust 动态宿主查找的 ABI 入口",
        "qx_strategy_get_vtable 缺少 Windows dllexport 声明",
    )


def snapshot_json_table_check() -> None:
    """稳定 JSON 的键表只有一份编码：写侧印成什么，读侧 `from_json` 就得认什么（V11 R14/R15）。

    订单/成交/划转的键是 `u64`，持仓的键是 `InstrumentId`。此前 `to_json` 手抄 `format!`
    把裸数字当对象键，产出的 `{77:{...}}` 连合法 JSON 都不是，`from_json`、SQLite/Postgres
    的 `load_json` 与 Python 侧的 `load_account_snapshot` 会在同一份产物上一起失败；枚举与
    标的也是同一类分叉（数字码 / 字符串标的 vs serde 的变体名 / 对象标的）。持仓更进一层：
    手抄那份少印 `instrument` 与任何新增字段，读侧却按 serde 认，于是新字段被写侧静默丢掉。
    """
    protocol = production_text((ROOT / SNAPSHOT_PROTOCOL_FILE).read_text(encoding="utf-8"))
    cases = case_source(SNAPSHOT_CORE_CASE_FILE)

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
# 本金来源的四种说法：没配、配了、配了且被 paper 侧同一个数确认过、多腿按本腿行情定资。
# 两两必须可区分（Q67 口径）：`account_base_source` 是读者判断"这一轮压在多少钱上、这个数
# 还有没有别处认账"的唯一出口。
ACCOUNT_BASE_SOURCES = (
    "builtin-default",
    "strategy-initial-cash",
    "strategy-initial-cash+paper-worker-cash",
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
    "a_principal_declared_the_same_way_on_both_sides_is_landed_as_agreeing",
    "two_different_principals_in_one_runtime_fail_at_the_backtest_entry",
)
# 逐字面量比太脆（rustfmt 会折行、注释会改口径词），这里只钉那些"改坏即换语义"的写法。
ACCOUNT_BASE_DEFAULT_DECL = "pub(crate) const DEFAULT_BACKTEST_INITIAL_CASH: i64 = 100_000;"
ACCOUNT_BASE_SOURCE_CONSTS = (
    "BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE",
    "BACKTEST_ACCOUNT_BASE_CONFIG_SOURCE",
    "BACKTEST_ACCOUNT_BASE_BOTH_DECLARED_SOURCE",
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
    # rustfmt 会把长的 `&str = "..."` 折到下一行，逐字比对先压平空白再谈"这一处声明"。
    source_decls = " ".join(account_base.split())
    missing_sources = [
        name
        for name, literal in zip(ACCOUNT_BASE_SOURCE_CONSTS, ACCOUNT_BASE_SOURCES)
        if f'pub(crate) const {name}: &str = "{literal}";' not in source_decls
    ]
    check(
        not missing_sources
        and multi_leg.count(BACKTEST_ACCOUNT_BASE_FUNDING_USE) == 1
        and "builtin-default" not in multi_leg,
        f"{len(ACCOUNT_BASE_SOURCES)} 种本金来源各有常量，多腿链只报自己的定资口径、不会冒充默认本金",
        f"缺常量 {missing_sources or '无'} / 多腿标注 {multi_leg.count(BACKTEST_ACCOUNT_BASE_FUNDING_USE)} 处",
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
    # 2b. 读法（V12 R3 收紧）：链侧不再自己摸那一格配置，一律经 `account_base.rs` 的两个出口
    #     之一拿本金。交叉校验就塞在那两个出口里，链侧自己读键等于绕过它 —— 于是这里既数
    #     "链侧直读键 0 处"，也数"每个出口各只定义一处"。
    chain_direct_reads = sum(
        part.count("config.strategy.initial_cash_raw")
        for part in (bar_chain, depth_chain, leg_funding, multi_leg)
    )
    check(
        chain_direct_reads == 0
        and bar_chain.count("account_base_from_config(") == 1
        and sum(
            part.count("configured_account_base(") for part in (bar_chain, depth_chain, leg_funding)
        )
        == 3
        and account_base.count("pub(crate) fn backtest_initial_cash(") == 1
        and account_base.count("pub(crate) fn account_base_from_config(") == 1
        and account_base.count("pub(crate) fn configured_account_base(") == 1
        and "configured_initial_cash_raw" not in account_base + chains + leg_funding,
        "回测链一律经 account_base 的出口拿本金，没有一条链自己摸那一格配置，也不存在第二个读键 helper（V11 Q72 / V12 R3）",
        f"链侧直读 {chain_direct_reads} 处 / 出口调用 {bar_chain.count('account_base_from_config(')}+"
        f"{sum(part.count('configured_account_base(') for part in (bar_chain, depth_chain, leg_funding))} 处",
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
        and '"schema_version": 5' in summary,
        "摘要以 v5 保留 account 块并扩展风险比率/RunRecord，期初本金仍取自真正记账的那一份（V11 Q72）",
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


# === V12 R3：一份 runtime 只有一个账户本金口径 ===
# 回测侧那一格（`strategy.initial_cash_raw`）是收益率分母与风控可用现金，Paper 侧那一格
# （`worker.paper_initial_cash_raw`）是虚拟账户的初始资金。两者过去互不知情：一份 runtime
# 写 100,000 与 200,000 也能照常启动，使用者却以为"我声明了一份本金"。
ACCOUNT_PRINCIPAL_HELPERS = (
    "fn account_principal_declarations(",
    "fn paper_declares_same_principal(",
    "pub(crate) fn reject_split_account_principal(",
    "pub(crate) fn account_principal_note(",
)
# 回测侧声明处的名字只许有一处定义：报错、来源标签与比对都从它派生，抄第二份就会漂移。
PRINCIPAL_SITE_DECL = 'const STRATEGY_PRINCIPAL_SITE: &str = "strategy.initial_cash_raw";'
PRINCIPAL_CONFLICT_MESSAGE = "两份互不相等的账户本金"
PRINCIPAL_WORKER_SITE = 'format!("worker[{}]", worker.id)'
# 两处 Paper 入账入口都要在真正入账（`seed_paper_initial_cash`）之前过闸门并说出"两处一致"。
PAPER_PRINCIPAL_ENTRIES = (
    "crates/qx-cli/src/venue_runtime/paper_submit.rs",
    "crates/qx-cli/src/venue_runtime/paper_worker.rs",
)
PAPER_SEED_CALL = "seed_paper_initial_cash(&mut pipeline"
ACCOUNT_PRINCIPAL_UNIT_CASES = "crates/qx-cli/src/tests/account_principal_source.rs"
ACCOUNT_PRINCIPAL_UNIT_TESTS = (
    "the_same_principal_declared_twice_is_reported_as_one_agreeing_number",
    "a_lone_backtest_declaration_is_not_reported_as_agreeing_with_anything",
    "two_paper_accounts_may_be_funded_differently_without_a_backtest_declaration",
    "two_different_principals_in_one_runtime_are_rejected_everywhere_they_are_read",
)


def account_principal_single_source_check() -> None:
    """V12 R3：本金交叉判据一处定义、回测出口先过闸门、Paper 入口先拒再入账。"""
    account_base = (ROOT / ACCOUNT_BASE_MODULE).read_text(encoding="utf-8")
    unit_cases = (ROOT / ACCOUNT_PRINCIPAL_UNIT_CASES).read_text(encoding="utf-8")
    test_mounts = (CRATES / "qx-cli/src/tests/mod.rs").read_text(encoding="utf-8")

    def body_of(signature: str) -> str:
        start = account_base.find(signature)
        if start < 0:
            return ""
        end = account_base.find("\n}", start)
        return account_base[start:] if end < 0 else account_base[start:end]

    # 1. 判据的四个部件与声明处名字各一处，且都住在 account_base.rs。
    wrong_parts = [
        signature
        for signature in ACCOUNT_PRINCIPAL_HELPERS
        if account_base.count(signature) != 1
    ]
    check(
        not wrong_parts
        and account_base.count(PRINCIPAL_SITE_DECL) == 1
        and "pub(crate) const BACKTEST_ACCOUNT_BASE_BOTH_DECLARED_SOURCE" in account_base,
        "本金交叉判据的四处部件与两处名字各只有一份定义，第四格来源是具名常量（V12 R3）",
        f"部件 {[signature for signature in ACCOUNT_PRINCIPAL_HELPERS if account_base.count(signature) != 1] or '齐'}"
        f" / 声明处名字 {account_base.count(PRINCIPAL_SITE_DECL)} 处",
    )
    # 2. 回测出口的顺序固定：先交叉拒，再折本金，等值时才改口成第四格来源。
    from_config = body_of("pub(crate) fn account_base_from_config(")
    check(
        "reject_split_account_principal(config)?;" in from_config
        and "backtest_initial_cash(config.strategy.initial_cash_raw)" in from_config
        and from_config.index("reject_split_account_principal(config)?;")
        < from_config.index("backtest_initial_cash(config.strategy.initial_cash_raw)"),
        "回测出口先拒两份互不相等的本金，再折成本金（V12 R3）",
        f"出口正文缺 {[n for n in ('reject_split_account_principal(config)?;', 'backtest_initial_cash(config.strategy.initial_cash_raw)') if n not in from_config] or '无'}",
    )
    check(
        "paper_declares_same_principal(config)" in from_config
        and "base.source = BACKTEST_ACCOUNT_BASE_BOTH_DECLARED_SOURCE;" in from_config,
        "两处等值时才把来源改口成第四格，孤立声明不得冒充「两处一致」（V12 R3）",
        "缺等值改口分支",
    )
    # 3. 不给配置的那半条出口同样只有一个读法：默认本金仍走 backtest_initial_cash(None)。
    config_exit = body_of("pub(crate) fn configured_account_base(")
    check(
        config_exit.count("account_base_from_config(") == 1
        and "read_runtime_config(" in config_exit
        and "backtest_initial_cash(None)" in config_exit,
        "带 config 的路径只能经带闸门的读法拿本金，不带 config 的按默认（V12 R3）",
        f"出口正文缺 {[n for n in ('read_runtime_config(', 'backtest_initial_cash(None)') if n not in config_exit] or '无'}",
    )
    # 4. 两份 Paper 入账入口：闸门排在真正入账之前，且等值时说得出"两处共用同一个本金"。
    for entry in PAPER_PRINCIPAL_ENTRIES:
        text = (ROOT / entry).read_text(encoding="utf-8")
        gate = "reject_split_account_principal(&config)?;"
        check(
            text.count(gate) == 1
            and text.count("account_principal_note(&config)") == 1
            and text.count(PAPER_SEED_CALL) == 1
            and text.index(gate) < text.index(PAPER_SEED_CALL),
            f"{entry} 在按 worker 那一格入账之前先拒两份本金、再把一致说出来（V12 R3）",
            f"闸门 {text.count(gate)} 处 / 说明行 {text.count('account_principal_note(&config)')} 处"
            f" / 入账 {text.count(PAPER_SEED_CALL)} 处",
        )
    # 5. 冲突报错要并列点出两处名字与各自的数，否则使用者不知道该删哪一格。
    check(
        PRINCIPAL_CONFLICT_MESSAGE in account_base
        and account_base.count(PRINCIPAL_WORKER_SITE) == 1,
        "冲突报错并列点出回测侧与 Paper 侧两处名字与各自的数（V12 R3）",
        f"文案={'在' if PRINCIPAL_CONFLICT_MESSAGE in account_base else '缺'}"
        f" / worker 侧名字 {account_base.count(PRINCIPAL_WORKER_SITE)} 处",
    )
    # 6. 用例面：四条判据读法在位，且新用例文件真的挂进测试目录模块。
    missing_cases = [name for name in ACCOUNT_PRINCIPAL_UNIT_TESTS if f"fn {name}(" not in unit_cases]
    check(
        not missing_cases and "mod account_principal_source;" in test_mounts,
        "R3 用例在位：等值/孤立声明/多 paper 账户各自定资/两份不等四条读法，且已挂载",
        f"缺用例 {missing_cases or '无'} / 挂载={'在' if 'mod account_principal_source;' in test_mounts else '缺'}",
    )


# === V12 R1：读模型对"摘要里到底有没有这一格"的单一读法 ===
REPORT_READOUT_MODULE = "crates/qx-cli/src/report_readout.rs"
REPORT_READOUT_COMMANDS = (
    "crates/qx-cli/src/config_commands.rs",
    "crates/qx-cli/src/report_command.rs",
)
REPORT_READOUT_UNIT_CASES = "crates/qx-cli/src/tests/report_readout.rs"
REPORT_READOUT_CLI_CASES = "crates/qx-cli/tests/report_readout_honesty.rs"
# 两个命令各自要的正文函数：一处定义、一处调用，正文只许有一份。
READOUT_LINE_BUILDERS = ("report_readout_lines", "latest_backtest_readout_lines")
# 读侧写死的"缺席"词与它的排版函数；第二条命令不得另起一个字面量。
READOUT_ABSENT_DECL = 'pub(crate) const READOUT_ABSENT: &str = "absent";'
# 摘要格子的读法：只此一份。命令侧再出现 `summary.get(...)`/`summary.pointer(...)` 就是第二份真值。
READOUT_DIRECT_ACCESS = re.compile(r"summary\s*\.\s*(?:get|pointer)\s*\(")
READOUT_BLOCK_SINCE = re.compile(r'\("(\w+)",\s*(\d+)\)')
WRITER_SCHEMA_VERSION = re.compile(r'"schema_version":\s*(\d+)')
READOUT_UNIT_TESTS = (
    "absent_keys_print_absent_while_declared_zero_still_prints_zero",
    "string_typed_and_negative_money_fields_still_read_as_numbers",
    "generation_note_tells_apart_older_schema_from_missing_block",
    "status_latest_backtest_lines_use_the_same_absent_wording",
    "multi_leg_cost_bps_reports_no_denominator_and_refuses_an_unprintable_ratio",
)
READOUT_CLI_TESTS = (
    "report_prints_absent_for_keys_this_generation_never_declared",
    "report_reads_a_declared_zero_as_zero_not_absent",
    "status_latest_backtest_line_shares_the_same_readout",
    "status_reports_no_summary_instead_of_an_empty_backtest",
)


def report_readout_honesty_check() -> None:
    """V12 R1：`report`/`status` 念摘要时的四条不变量——缺席词一处、格子读法一处、
    两个命令共用同一份正文、读侧认得的世代号与写侧落盘的同号。
    """
    readout = (ROOT / REPORT_READOUT_MODULE).read_text(encoding="utf-8")
    commands = "\n".join(
        (ROOT / path).read_text(encoding="utf-8") for path in REPORT_READOUT_COMMANDS
    )
    unit_cases = (ROOT / REPORT_READOUT_UNIT_CASES).read_text(encoding="utf-8")
    cli_cases = (ROOT / REPORT_READOUT_CLI_CASES).read_text(encoding="utf-8")
    summary_writer = (ROOT / SUMMARY_MODULE).read_text(encoding="utf-8")
    main = (CRATES / "qx-cli/src/main.rs").read_text(encoding="utf-8")

    # 1. 缺席只有一个词，且它不住在命令里：否则第二条命令可以挑一个 `0` 或 `-` 自称诚实。
    absent_literal = '"' + "absent" + '"' in commands
    check(
        readout.count(READOUT_ABSENT_DECL) == 1
        and not absent_literal
        and "unwrap_or(0)" not in commands,
        "读侧的缺席词只有 report_readout.rs 一处定义，命令侧不再兜底成 0（V12 R1）",
        f"定义 {readout.count(READOUT_ABSENT_DECL)} 处 / 命令侧 absent 字面量={'在' if absent_literal else '无'}",
    )
    # 2. 摘要格子的读法唯一：命令侧直接摸 JSON 就是第二份真值，它会与 `absent` 口径漂移。
    check(
        READOUT_DIRECT_ACCESS.search(commands) is None
        and all(
            readout.count(f"pub(crate) fn {name}(") == 1 and commands.count(f"{name}(") == 1
            for name in READOUT_LINE_BUILDERS
        ),
        "两个命令各用一份正文函数，正文函数各只定义一处（report_readout_lines / latest_backtest_readout_lines）",
        f"命令侧直读 {[m.group(0) for m in READOUT_DIRECT_ACCESS.finditer(commands)]}"
        f" / 正文定义 {[(name, readout.count(f'pub(crate) fn {name}(')) for name in READOUT_LINE_BUILDERS]}",
    )
    # 3. 世代必须当场说出来：`report` 与 `status` 的正文各自引用一次世代行，命令侧不得自己拼。
    check(
        readout.count("summary_generation_note(summary)") == 2
        and "summary_generation_note" not in commands
        and READOUT_BLOCK_SINCE.search(readout) is not None,
        "产物世代由正文第一行说出，且只在共用读法里拼一次",
        f"世代行 {readout.count('summary_generation_note(summary)')} 处（期望 2）"
        f" / 命令侧自带世代={'在' if 'summary_generation_note' in commands else '无'}",
    )
    # 4. 读侧的世代表必须跟着写侧升：写侧升到 v5 而读侧没跟上时，`account=not_declared_before_v4`
    #    这类文案会继续对着一份新产物说谎。
    writer_versions = [int(v) for v in WRITER_SCHEMA_VERSION.findall(summary_writer)]
    readout_generations = [int(v) for _, v in READOUT_BLOCK_SINCE.findall(readout)]
    check(
        bool(writer_versions)
        and bool(readout_generations)
        and max(readout_generations) == max(writer_versions),
        "读侧世代表里最新的块与摘要写侧的 schema_version 同号（写侧升级必须带着读侧一起升）",
        f"写侧 {writer_versions or '未声明'} / 读侧 {readout_generations or '无表'}",
    )
    # 5. 多腿的单位成本：分母为零是"算不出"，越界是失败，两处都不许悄悄落回一个合法数。
    multi_leg = (CRATES / "qx-cli/src/multi_leg.rs").read_text(encoding="utf-8")
    multi_chain = (ROOT / MULTI_LEG_CHAIN_FILE).read_text(encoding="utf-8")
    check(
        multi_leg.count("pub(crate) fn multi_leg_cost_bps(") == 1
        and multi_chain.count("multi_leg_cost_bps(") == 1
        and "unwrap_or(i64::MAX)" not in multi_chain
        and "cost_bps=net_cost*10000/turnover, null when turnover_raw=0" in multi_chain,
        "多腿 cost_bps 的口径定义一处、调用一处，产物假设行同时说明零换手是 null",
        f"定义 {multi_leg.count('pub(crate) fn multi_leg_cost_bps(')} 处"
        f" / 调用 {multi_chain.count('multi_leg_cost_bps(')} 处",
    )
    # 6. 用例面：缺席与"声明过的 0"必须成对出现，且端到端跑过真实子进程。
    check(
        all(f"fn {name}(" in unit_cases for name in READOUT_UNIT_TESTS)
        and all(f"fn {name}(" in cli_cases for name in READOUT_CLI_TESTS),
        "R1 用例在位：五条排版/读法单元 + 四条 report/status 命令行用例",
        f"缺单元 {[n for n in READOUT_UNIT_TESTS if f'fn {n}(' not in unit_cases]}"
        f" / 缺命令行 {[n for n in READOUT_CLI_TESTS if f'fn {n}(' not in cli_cases]}",
    )
    check(
        "mod report_readout;" in main and "pub(crate) use report_readout::*;" in main,
        "report_readout 在 crate 根以 mod + pub(crate) use 成对挂载",
        "缺挂载配对",
    )


API_ADMISSION_FILE = "crates/qx-api/src/admission.rs"
API_WS_FILE = "crates/qx-api/src/ws.rs"
API_TRANSPORT_FILE = "crates/qx-api/src/transport.rs"
API_CONNECTIONS_FILE = "crates/qx-api/src/connections.rs"
API_ASSEMBLY_FILE = "crates/qx-cli/src/api_service.rs"
API_BROWSER_CASE_FILE = "crates/qx-api/tests/browser_admission.rs"
# 两格配置的中性写法（`[]` 与 `null`）由生产模板携带，于是它们的 JSON 形状每轮都被
# `deploy_template_coverage` 那条读法解析一次——只在 schema 里声明的键是没人解析过的键。
API_ADMISSION_TEMPLATE = "deploy/qianxing.runtime.production.example.json"
BROWSER_ADMISSION_CASES = (
    "preflight_from_a_listed_origin_is_204_and_never_echoes_the_requested_headers",
    "preflight_from_an_unlisted_origin_says_403_with_its_own_code_name",
    "without_an_allowlist_the_preflight_falls_through_to_the_404_and_no_cors_headers_appear",
    "every_exit_of_a_cors_deployment_carries_the_same_allow_origin",
    "the_second_inflight_connection_is_refused_with_503_and_its_limit",
    "a_request_body_cannot_hijack_the_websocket_handshake",
    "the_websocket_branch_reads_the_query_string_before_it_writes_101",
    "a_malformed_percent_escape_is_a_400_about_the_request_not_a_404_about_the_account",
)


def browser_admission_check() -> None:
    """浏览器准入面（V13 R1-B）：跨源 allowlist、预检、并发连接预算、查询串解码。

    审计起点是四格全缺：没有任何跨源判定（浏览器控制台读不到本面）、没有预检应答
    （`POST /control/commands` 从浏览器根本发不出去）、一条连接一个线程且无上限、
    升级判定嗅的是整段请求文本（正文里出现那串字面量就能劫持握手）。判据一律读剥掉
    注释的函数体：散文里写着某个名字不算它发生了。
    """
    admission = production_text((ROOT / API_ADMISSION_FILE).read_text(encoding="utf-8"))
    lib = production_text((ROOT / API_SERVE_FILE).read_text(encoding="utf-8"))
    dispatch = _code_body(API_SERVE_FILE, "fn dispatch_request")
    upgrade = _code_body(API_ADMISSION_FILE, "fn is_websocket_upgrade")
    # `text.find("fn preflight")` 会先撞上 `fn preflight_headers`，所以签名带可见性前缀。
    preflight = _code_body(API_ADMISSION_FILE, "pub(crate) fn preflight(")
    written = _code_body(API_TRANSPORT_FILE, "fn write_http_response")
    refused = _code_body(API_TRANSPORT_FILE, "fn refuse_connection")
    over_budget = _code_body(API_CONNECTIONS_FILE, "fn refuse_over_budget")
    spawned = _code_body(API_CONNECTIONS_FILE, "fn spawn_connection")
    plaintext = _code_body(API_SERVE_FILE, "pub fn serve(")
    mtls = _code_body(API_SERVE_FILE, "pub fn serve_tls_mtls_with_stores(")
    assembly = _code_body(API_ASSEMBLY_FILE, "pub(crate) fn build_configured_api_service")
    references = _code_body(RUNTIME_CHECK_FILE, "pub(crate) fn validate_runtime_references")

    check(
        'header_value(request, "upgrade")' in upgrade
        and 'eq_ignore_ascii_case("websocket")' in upgrade
        and "request.contains(" not in upgrade
        and "to_lowercase()" not in upgrade
        and "if is_websocket_upgrade(request)" in dispatch,
        "WebSocket 升级判定只读 `Upgrade` 头部，分派入口调的就是这一个判定",
        "判定回到整段文本嗅探：POST 正文里写那串字面量就能把一条普通请求变成握手",
    )
    options_sites = sum(
        production_text(path.read_text(encoding="utf-8")).count('"OPTIONS"')
        for path in sorted((ROOT / "crates/qx-api/src").glob("*.rs"))
    )
    check(
        'eq_ignore_ascii_case("OPTIONS")' in preflight
        and "access-control-request-method" in preflight
        and options_sites == 1
        and '"OPTIONS"' not in lib
        and dispatch.index("preflight(self.cors.as_deref(), request)")
        < dispatch.index("self.handle_inner("),
        "预检在 `handle_inner` 的路由分派**之前**应答，`OPTIONS` 的判定点全仓只有 admission 一处",
        f"预检挪到分派之后，或路由表里长出第二条 OPTIONS 臂（判定点 {options_sites} 处）："
        "两处口径各讲一套跨源规则",
    )
    check(
        "response.status != 204" in written
        and written.index("Content-Length") > written.index("response.status != 204"),
        "204 响应不写 Content-Length（RFC 9110 §15.3.5），预检正文长度不被声明成「还有 0 字节要等」",
        "204 又带上 Content-Length：严格实现的浏览器把预检读成协议错误，而不是读成「允许」",
    )
    check(
        "Shutdown::Write" in refused
        and "read(&mut discard)" in refused
        and "write_http_response(" in refused
        and "refuse_connection(" in over_budget
        and "write_http_response(" not in over_budget,
        "超额的那条连接经 `refuse_connection` 体面拒掉：写出 503 → 半关写方向 → 排空对端已发来的字节",
        "回到「写完就关」：接收缓冲里还躺着未读的请求字节，close 以 RST 收场，"
        "客户端读到的是「连接被重置」而不是那条 503——文档承诺的码名于是送不到任何人手上",
    )
    check(
        all(
            token in body
            for body in (plaintext, mtls)
            for token in ("self.connection_budget.acquire()", "self.refuse_over_budget(&stream)")
        )
        and "self.spawn_connection(stream, now(), None, guard)" in plaintext
        and "self.spawn_connection(secured, now(), operator_id, guard)" in mtls
        and "let _guard = guard;" in spawned,
        "两条 accept 循环都在派线程**之前**判预算，守卫随连接线程一起活（`Drop` 归还额度）",
        "预算判定漏掉一条循环，或守卫不随线程活：额度要么不封顶，要么借出去不回来",
    )
    check(
        mtls.count("break Err(error)") == 1
        and mtls.count("continue;") == 2
        and "只关这一条连接" in mtls,
        "一次 TLS 握手失败只关那一条连接，不把整个 mTLS 监听循环带走",
        "握手失败又写成 `break Err(error)`：探活脚本往 mTLS 端口发一条明文，API 就此停止接受任何连接，"
        "而监督器只看到「worker 退出了」",
    )
    definitions = [
        path.relative_to(ROOT).as_posix()
        for path in sorted((ROOT / "crates").rglob("*.rs"))
        if "DEFAULT_MAX_CONCURRENT_CONNECTIONS: usize"
        in path.read_text(encoding="utf-8", errors="replace")
    ]
    check(
        definitions == [API_ADMISSION_FILE],
        "默认并发连接预算只有 admission 一个定义点，配置侧与文档都不抄第二份数字",
        f"定义点 {definitions}：改了默认值而另一处还写着旧数，两侧就各讲一个上限",
    )
    admitted = _code_body(API_WS_FILE, "fn admit_websocket")
    served = _code_body(API_WS_FILE, "fn serve_websocket")
    handshake_key_reads = 'header_value(request, "Sec-WebSocket-Key")'
    check(
        dispatch.index("self.metrics.requests_total.fetch_add(1, Ordering::Relaxed)")
        < dispatch.index("self.admit_websocket(")
        and dispatch.index("self.rate_limiter.try_acquire(rate_limit_bucket_seconds(ts))")
        < dispatch.index("self.admit_websocket(")
        and "Ok(session) => self.serve_websocket(stream, session)" in dispatch
        and "cors.websocket_denied(request)" in admitted
        and 'error_json("cors_origin_not_allowed")' in admitted
        and "Ok(WsSession {" in admitted
        and admitted.index("cors.websocket_denied(request)")
        < admitted.index("Ok(WsSession {")
        and admitted.count(handshake_key_reads) == 1
        and 'error_json("missing_websocket_key")' in admitted
        and "header_value(" not in served
        and "session.accept" in served,
        "升级分支与 HTTP 分支共用同一枚 requests_total 与同一只限流桶；跨源、身份与握手 key 三种判定"
        "全排在写下 101 之前（缺 key 那一支说的是 400 而不是掐连接），且握手 key 只被读一次、"
        "由会话原样带给帧循环",
        "跨源判定挪到 101 之后（或干脆没有）= 名单外的浏览器页面照样读得到事件流：浏览器不把 "
        "CORS 用在 WS 握手上，它照发 Upgrade 请求、只在响应侧拦，所以这份名单必须被这一支自己问过；"
        "key 在准入与帧循环各读一次 = 后一次永远走不到，那条拒绝臂是死分支",
    )
    check(
        dispatch.count("let cors = match &self.cors") == 1
        and dispatch.count("&cors") == 5
        and "policy.response_headers(request)" in dispatch
        and 'Err(response) => write_http_response(stream, &response, &cors)' in dispatch
        and '"Vary"' in admission,
        "跨源头在分派入口算一次，五条出口（升级前限流 429 与 503、升级前那一整串准入拒绝、"
        "预检拒绝 403、正常分派）带的是同一份",
        "某一条出口漏带头：那条路径跨源读不到，而浏览器只报「被 CORS 拦了」，看不出是哪一支",
    )
    check(
        "percent_decode(" in _code_body(API_ADMISSION_FILE, "fn query_param")
        and admission.count("fn percent_decode(") == 1,
        "查询串的百分号解码只有一个实现点，且 `query_param` 走的就是它",
        "解码点分裂成两份：`?account_id=main%zz` 又会把半个转义当成账户号去查投影，"
        "读到的是 404「没有这个账户」，而真正坏掉的是请求本身",
    )
    check(
        "qx_api::validate_admission_config(" in references
        and assembly.count("config.api.cors_allowed_origins") == 2
        and assembly.count("config.api.max_concurrent_connections") == 1
        and "with_cors_allowed_origins(config.api.cors_allowed_origins.clone())" in assembly
        and "with_max_concurrent_connections(limit)" in assembly,
        "准入两格配置有三个读点（schema 声明、`config validate`、serve 装配），"
        "而 validate 调的是 `qx-api` 那一份实现，不是第二套规则",
        "校验点被摘掉，或装配点用的不是配置里那个值：坏源与 0 上限又要等到 `serve` 起不来才现身",
    )
    cases = (ROOT / API_BROWSER_CASE_FILE).read_text(encoding="utf-8")
    missing = [name for name in BROWSER_ADMISSION_CASES if f"fn {name}(" not in cases]
    template = json.loads((ROOT / API_ADMISSION_TEMPLATE).read_text(encoding="utf-8"))
    check(
        not missing
        and "cors_allowed_origins" in template.get("api", {})
        and "max_concurrent_connections" in template.get("api", {}),
        "浏览器准入的八条常驻用例在位，且生产模板携带两格配置的中性写法",
        f"缺用例 {missing}；模板缺键 "
        f"{[k for k in ('cors_allowed_origins', 'max_concurrent_connections') if k not in template.get('api', {})]}",
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
    # 旧口径要求整行**只有一个**路径（结尾必须 `\s*$`），于是"路径 + 一句说明"那种证据行从不被核对：
    # V12 D3 把四份超线用例集搬成目录后，五条这样的行继续指向已经不存在的单文件，而 315 项全绿。
    # 上一版收住了"行首那一个"，可一行里写两个路径时第二个仍然逃检（任务 #87）：现在凡带仓库
    # 前缀的 token 逐个核对。`paper_submit.rs:50` 的行号后缀与 `deploy/data/*/runs/` 这类通配
    # 说明各有豁免；散文里不带前缀的简写（"depth.rs and …"）不强判。字符类里排掉 `"`，
    # 于是 `include_str!("../../../schemas/x.json")` 取到的是仓库里那份真文件。
    evidence_path = re.compile(
        r"(?:crates|tools|deploy|maturity|docs|schemas|python)/[^\s，。；、）()「」\"`]+"
    )
    missing = set()
    for line in lines:
        for token in evidence_path.findall(line):
            candidate = re.sub(r":\d+$", "", token).rstrip(".,:;")
            if "*" in candidate or "?" in candidate:
                continue
            if not (ROOT / candidate).exists():
                missing.add(candidate)
    check(
        not missing,
        "能力矩阵证据路径全部存在（一行里的每个路径 token 都核对，不止行首）",
        f"失效路径 {sorted(missing)}",
    )
    claimed_sandbox = sorted(
        name for name, fields in blocks.items() if fields.get("sandbox_tested") == "true"
    )
    check(
        not claimed_sandbox,
        "未拿到外部沙盒记录前 sandbox_tested 全为 false",
        f"越界声明 {claimed_sandbox}",
    )
    # 登记面自身要能被标准 YAML 读法吃下。门禁「不引 PyYAML」、逐行文本核对，所以一份只有门禁
    # 读得懂的 `.yaml` 是假的可机读：编辑器、CI lint、下一个工具拿到的是 ScannerError。形态就是
    # 「无引号标量里出现半角 `: `」——V13 R25 二轮实测 8 份里有 2 份坏在这里（能力矩阵 38 行、
    # HTTP 面 3 行），修法是给标量加引号且一个字都不改。判据与解析器的规则同式，
    # 并自带四条自证（放得进坏形态、豁免得了好形态），免得名额靠一条从不发声的空判据撑着。
    def plain_scalar_offenders(text: str) -> list[int]:
        hits: list[int] = []
        for number, line in enumerate(text.splitlines(), 1):
            match = re.match(r"^(\s*(?:-\s+|[\w-]+:\s+))(.*)$", line)
            if match is None:
                continue
            prefix, value = match.group(1), match.group(2)
            inner = re.match(r"^([\w-]+:\s+)(.*)$", value)
            if prefix.strip().endswith("-") and inner:
                value = inner.group(2)  # `- key: value` 是序列项里的映射开头，第一个 ": " 是分隔符
            if not value or value[0] in "'\"|>":
                continue
            if ": " in value or value.endswith(":"):
                hits.append(number)
        return hits

    registry = sorted((ROOT / "maturity").glob("*.yaml"))
    unreadable = {
        path.name: plain_scalar_offenders(path.read_text(encoding="utf-8"))
        for path in registry
        if plain_scalar_offenders(path.read_text(encoding="utf-8"))
    }
    check(
        len(registry) >= 8
        and not unreadable
        and plain_scalar_offenders("  - a: b: c\n") == [1]
        and plain_scalar_offenders("  - 'a: b: c'\n") == []
        and plain_scalar_offenders('    note: "x: y"\n') == []
        and plain_scalar_offenders("  - area: request_line_and_target\n") == []
        and plain_scalar_offenders("    note: >-\n      任意文字\n") == [],
        f"maturity/ 的 {len(registry)} 份登记文件里没有会把 YAML 读成映射的裸标量（标准解析器每份都吃得下）",
        f"裸标量带半角冒号空格的行 {unreadable or '无'}",
    )


def backtest_track_check() -> None:
    """P0-1：把「回测轨」做成**不需要交易所凭据**就能完整验收的一条轨。

    卡点原本是这样的：`maturity/evidence/testnet/` 那份 Binance 验收是 `outcome=skipped`
    （缺 `QX_BINANCE_TESTNET_API_KEY` / `_SECRET`），于是 `sandbox_tested` / `production_approved`
    只能全 false，`P0-1` 就一直挂在"未落地"。但这两档**只对需要外部 venue 的能力有意义**——
    本仓的主用法是回测与 Paper 闭环，它一条凭据都不用。把两条轨混在一张表里，会让
    「实盘待外部证据」被读成「整体未落地」。六颗分别钉：

    ① 默认档是 `backtest_only`，且它自己声明 `credentials_required: false` /
       `external_venues: none` / 写明那两档**不适用**（不是"待补"）；
    ② 验收记录 `maturity/backtest_acceptance.yaml` 在盘，且六格自述齐全（passed / 不需凭据 /
       无外部 venue / 未访问网络 / 未发订单 / 回放 verified）；
    ③ 记录由脚本生成，且脚本**真的**做了两件事——同目录重跑与两个独立目录比对。只写一份
       "看起来通过"的记录不算：`compare_reruns` / `compare_independent` / `one_leg` 三个函数
       都得在盘（少一个，"确定性"这句话就没人验）；
    ④ 记录里的 `result_hash` 是 16 位十六进制，四类产物摘要齐全；
    ⑤ **回测轨不得越界替实盘作保**：记录里不出现任何 venue 名称（binance/okx/ccxt/testnet），
       且 `capabilities.yaml` 里那两档仍全为 false——回测轨的存在没有把实盘轨的禁令松开；
    ⑥ 行为用例在盘（`crates/qx-cli/tests/backtest_acceptance_determinism.rs` 的两条）。

    刻意**不**把「记录是否过时」做成判据：记录是产物，`generated_at_unix` 每跑一次都变，
    拿它当判据只会逼人把时间戳写死。过时与否由重跑 `tools/backtest_acceptance.py` 回答。
    """
    profiles_text = case_source(CAPABILITIES_FILE)
    check(
        re.search(r"^default_profile:\s*backtest_only\s*$", profiles_text, re.M) is not None
        and re.search(
            r"^  backtest_only:\s*$(?:\n    .*)*?\n    credentials_required:\s*false\s*$",
            profiles_text,
            re.M,
        )
        is not None
        and re.search(r"^    external_venues:\s*none\s*$", profiles_text, re.M) is not None
        and "approval_scope:" in profiles_text
        and "不适用" in profiles_text,
        "默认档是 backtest_only，且该档声明「不需要凭据 / 无外部 venue / 那两档不适用」",
        "capabilities.yaml 的默认档不是 backtest_only，或该档没有写清凭据与档位适用范围",
    )
    record_path = ROOT / BACKTEST_ACCEPTANCE_RECORD
    record = record_path.read_text(encoding="utf-8") if record_path.is_file() else ""
    check(
        record_path.is_file()
        and all(fact in record for fact in BACKTEST_RECORD_FACTS),
        "回测轨验收记录在盘，且六格自述齐全（passed / 不需凭据 / 无外部 venue / 无网络 / 无订单 / 回放 verified）",
        f"缺 {BACKTEST_ACCEPTANCE_RECORD}，或缺事实："
        f"{[fact for fact in BACKTEST_RECORD_FACTS if fact not in record]}",
    )
    script = case_source(BACKTEST_ACCEPTANCE_SCRIPT)
    check(
        all(marker in script for marker in BACKTEST_SCRIPT_MARKERS)
        and re.search(r"^generated_by:\s*" + re.escape(BACKTEST_ACCEPTANCE_SCRIPT) + r"\s*$", record, re.M)
        is not None,
        "记录由在盘脚本生成，且脚本真的做了「同目录重跑 + 两个独立目录比对」两件事",
        f"缺 {BACKTEST_ACCEPTANCE_SCRIPT} 或它的 compare_reruns/compare_independent/one_leg，"
        "或记录的 generated_by 没指向它",
    )
    result_hash = re.search(r"^result_hash:\s*(\S+)\s*$", record, re.M)
    artifact_kinds = re.findall(r"^  (equity|fills|run_manifest|summary):\s*([0-9a-f]{64})\s*$", record, re.M)
    check(
        result_hash is not None
        and re.fullmatch(r"[0-9a-f]{16}", result_hash.group(1)) is not None
        and sorted(kind for kind, _ in artifact_kinds) == ["equity", "fills", "run_manifest", "summary"],
        "回测轨记录里的 result_hash 是 16 位十六进制，且四类产物摘要齐全",
        f"result_hash={result_hash.group(1) if result_hash else None}，产物 {sorted(k for k, _ in artifact_kinds)}",
    )
    # 先剥掉整行 `#` 注释再找 venue 名：记录头部的说明文字本来就要提"实盘轨的证据在哪"，
    # 把那句话当越界声明是误判（与门禁别处"取 production_text 剥注释"同口径）。
    record_body = "\n".join(
        line for line in record.splitlines() if not line.lstrip().startswith("#")
    )
    venue_names = ("binance", "okx", "ccxt", "testnet")
    leaked = [name for name in venue_names if name in record_body.lower()]
    check(
        not leaked
        and "sandbox_tested: true" not in profiles_text
        and "production_approved: true" not in profiles_text,
        "回测轨不越界替实盘作保：记录正文不出现任何 venue 名称，实盘两档仍全 false",
        f"记录正文里出现 {leaked}，或有能力被翻真",
    )
    check(
        (ROOT / BACKTEST_ACCEPTANCE_TEST).is_file()
        and all(
            case in case_source(BACKTEST_ACCEPTANCE_TEST)
            for case in BACKTEST_ACCEPTANCE_CASES
        ),
        "回测轨的行为面在盘（同目录重跑逐字节相等 + 两个独立目录 result_hash 相等且无凭据）",
        f"缺 {BACKTEST_ACCEPTANCE_TEST} 或它的两条用例",
    )


# —— P0-1：回测轨验收（不需要交易所凭据的那条轨）——
CAPABILITIES_FILE = "maturity/capabilities.yaml"
BACKTEST_ACCEPTANCE_RECORD = "maturity/backtest_acceptance.yaml"
BACKTEST_ACCEPTANCE_SCRIPT = "tools/backtest_acceptance.py"
BACKTEST_ACCEPTANCE_TEST = "crates/qx-cli/tests/backtest_acceptance_determinism.rs"
BACKTEST_ACCEPTANCE_CASES = (
    "same_directory_reruns_are_byte_identical",
    "independent_projects_agree_on_result_hash_without_credentials",
)
# 记录里必须出现的「这条轨不需要凭据」的六格自述。
BACKTEST_RECORD_FACTS = (
    "outcome: passed",
    "credentials_required: false",
    "external_venues: none",
    "network_accessed: false",
    "orders_sent: false",
    "replay_verdict: verified",
)
# 脚本里必须存在的两件事：同目录重跑、两个独立目录比对。
BACKTEST_SCRIPT_MARKERS = (
    "def compare_reruns(",
    "def compare_independent(",
    "def one_leg(",
)


# —— T0-4：回测基线（夹具身份 + 产物摘要 + 耗时 + 内存；数值跨机不可比，门禁不比对数值）——
BACKTEST_BASELINE_RECORD = "maturity/backtest_baseline.yaml"
BACKTEST_BASELINE_SCRIPT = "tools/backtest_baseline.py"
# 记录必须自述齐全的十格：身份 / 环境 / 夹具 / 结果 / 产物 / 耗时 / 内存。
BACKTEST_BASELINE_TOP = (
    "schema_version",
    "kind",
    "generated_by",
    "generated_at_unix",
    "environment",
    "fixture",
    "result_hash",
    "artifacts",
    "timing",
    "memory",
)
# 脚本里必须存在的测量原语：真计时、真起子进程、两个平台的**真峰值 RSS 读数**
# （Windows `GetProcessMemoryInfo` 的 PeakWorkingSetSize / Linux `/proc` 的 VmHWM）。
# 少一个，这份基线就有可能是手写的——「看起来有数字」不等于「真量过」。
BACKTEST_BASELINE_MARKERS = (
    "time.perf_counter()",
    "subprocess.Popen(",
    "PeakWorkingSetSize",
    "VmHWM",
)
# 与路径无关的纯数据产物：`equity.csv` / `fills.csv` 的内容不含绝对路径，跨目录必须逐字节相等
# （验收脚本自己就断言了这一点）。`summary` / `run_manifest` 内嵌本轮绝对路径，刻意**不**比对。
BACKTEST_BASELINE_PORTABLE_KINDS = ("equity", "fills")
BACKTEST_BASELINE_ARTIFACT_KINDS = ("equity", "fills", "run_manifest", "summary")
BACKTEST_BASELINE_INPUTS = (
    "qianxing.runtime.json",
    "qianxing.bar-frame.example.json",
    "qianxing.binance.spot.spec.json",
    "qianxing.dataset-bundle.bar-frame.example.json",
)


def _parse_baseline(record: str) -> dict:
    """把基线记录解析成嵌套 dict（两级映射 + 标量），不引 yaml 依赖。

    只认 `key:`（空值 → 开一个子块）与 `key: value`；`#` 整行注释跳过；块标量 `>-` 之后
    的缩进散文不匹配键形态，自然被略过。
    """
    root: dict = {}
    stack: list[tuple[int, dict]] = [(-1, root)]
    for raw in record.splitlines():
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        indent = len(raw) - len(raw.lstrip(" "))
        match = re.match(r"^([\w.-]+):\s*(.*?)\s*$", raw.strip())
        if match is None:
            continue
        key, value = match.group(1), match.group(2)
        while stack and stack[-1][0] >= indent:
            stack.pop()
        if value == "":
            child: dict = {}
            stack[-1][1][key] = child
            stack.append((indent, child))
        else:
            stack[-1][1][key] = value
    return root


def backtest_baseline_check() -> None:
    """T0-4：把「规范回测」的夹具身份、产物摘要、耗时与内存冻结成一份机读基线。

    `tools/backtest_acceptance.py` 证明的是「同一输入两次跑结果相等」，它**不记录**在这台机器上
    跑一轮要多久、占多少内存。缺了这份读数，「性能优化」与「性能回归」都无从比较，而计划 §6 的
    验收矩阵明确**不接受**「未测的高性能」。七颗分别钉：

    ① 基线在盘、`kind: backtest-baseline`、十格自述齐全（身份 / 环境 / 夹具 / 结果 / 产物 /
       耗时 / 内存）；
    ② 基线由在盘脚本生成，且脚本**真有测量原语**——真计时 + 真起子进程 + 两个平台的真峰值
       RSS 读数。只写一份"看起来有数字"的记录不算；
    ③ 夹具身份与回测轨验收记录**交叉一致**（`result_hash` + `data_fingerprint`）：同一夹具必须
       同一结果，不等就说明夹具漂移了、两份记录要一起改；
    ④ 与路径无关的纯数据产物（`equity` / `fills`）与验收记录**逐字节相等**——夹具或撮合引擎
       一漂移就红（`summary` / `run_manifest` 内嵌绝对路径，刻意不比对）；
    ⑤ 产物四类与夹具输入四份齐全，且每份摘要都是 64 位十六进制；
    ⑥ 耗时与内存是正数（平台不支持时如实记 `null` + 方法名）——**门禁不比对数值**：耗时与内存
       与机器有关，拿数值当判据只会逼人写死一台机器；
    ⑦ 基线正文不出现绝对路径 / 盘符 / 家目录——一台机器的细节不是可移植基线。

    与 `backtest_track_check` 同一口径，刻意**不**做「记录是否过时」判据：`generated_at_unix`
    每跑一次都变。过时与否由重跑 `tools/backtest_baseline.py` 回答。
    """
    record_path = ROOT / BACKTEST_BASELINE_RECORD
    record = record_path.read_text(encoding="utf-8") if record_path.is_file() else ""
    parsed = _parse_baseline(record)
    check(
        record_path.is_file()
        and parsed.get("kind") == "backtest-baseline"
        and all(key in parsed for key in BACKTEST_BASELINE_TOP)
        and all(
            isinstance(parsed.get(block), dict)
            for block in ("environment", "fixture", "artifacts", "timing", "memory")
        ),
        "回测基线在盘，且十格自述齐全（身份 / 环境 / 夹具 / 结果 / 产物 / 耗时 / 内存）",
        f"缺 {BACKTEST_BASELINE_RECORD}，或缺格 "
        f"{[key for key in BACKTEST_BASELINE_TOP if key not in parsed]}",
    )
    script = case_source(BACKTEST_BASELINE_SCRIPT)
    check(
        (ROOT / BACKTEST_BASELINE_SCRIPT).is_file()
        and parsed.get("generated_by") == BACKTEST_BASELINE_SCRIPT
        and all(marker in script for marker in BACKTEST_BASELINE_MARKERS),
        "基线由在盘脚本生成，且脚本真有测量原语（真计时 + 真起子进程 + 两平台真峰值 RSS 读数）",
        f"缺 {BACKTEST_BASELINE_SCRIPT} 或它的测量原语 "
        f"{[marker for marker in BACKTEST_BASELINE_MARKERS if marker not in script]}，"
        "或记录的 generated_by 没指向它",
    )
    acceptance = (
        (ROOT / BACKTEST_ACCEPTANCE_RECORD).read_text(encoding="utf-8")
        if (ROOT / BACKTEST_ACCEPTANCE_RECORD).is_file()
        else ""
    )
    accepted = _parse_baseline(acceptance)
    fixture = parsed.get("fixture") if isinstance(parsed.get("fixture"), dict) else {}
    check(
        accepted.get("result_hash") is not None
        and parsed.get("result_hash") == accepted.get("result_hash")
        and accepted.get("data_fingerprint") is not None
        and fixture.get("data_fingerprint") == accepted.get("data_fingerprint"),
        "夹具身份与回测轨验收记录交叉一致（同一夹具必须同一 result_hash / data_fingerprint）",
        f"基线 result_hash={parsed.get('result_hash')} vs 验收 {accepted.get('result_hash')}；"
        f"data_fingerprint={fixture.get('data_fingerprint')} vs {accepted.get('data_fingerprint')}",
    )
    artifacts = parsed.get("artifacts") if isinstance(parsed.get("artifacts"), dict) else {}
    accepted_contents = (
        accepted.get("artifact_contents")
        if isinstance(accepted.get("artifact_contents"), dict)
        else {}
    )
    check(
        all(
            re.fullmatch(r"[0-9a-f]{64}", str(artifacts.get(kind, ""))) is not None
            and artifacts.get(kind) == accepted_contents.get(kind)
            for kind in BACKTEST_BASELINE_PORTABLE_KINDS
        ),
        "与路径无关的纯数据产物（equity / fills）与验收记录逐字节相等（夹具或撮合一漂移就红）",
        f"基线 {[artifacts.get(kind) for kind in BACKTEST_BASELINE_PORTABLE_KINDS]} vs "
        f"验收 {[accepted_contents.get(kind) for kind in BACKTEST_BASELINE_PORTABLE_KINDS]}",
    )
    inputs = fixture.get("inputs") if isinstance(fixture.get("inputs"), dict) else {}
    check(
        sorted(artifacts) == sorted(BACKTEST_BASELINE_ARTIFACT_KINDS)
        and all(
            re.fullmatch(r"[0-9a-f]{64}", str(value)) is not None for value in artifacts.values()
        )
        and sorted(inputs) == sorted(BACKTEST_BASELINE_INPUTS)
        and all(re.fullmatch(r"[0-9a-f]{64}", str(value)) is not None for value in inputs.values()),
        "产物四类与夹具输入四份齐全，且每份摘要都是 64 位十六进制",
        f"产物 {sorted(artifacts)} / 输入 {sorted(inputs)}",
    )

    def positive(value: object) -> bool:
        try:
            return float(value) > 0  # type: ignore[arg-type]
        except (TypeError, ValueError):
            return False

    timing = parsed.get("timing") if isinstance(parsed.get("timing"), dict) else {}
    memory = parsed.get("memory") if isinstance(parsed.get("memory"), dict) else {}
    rss = memory.get("peak_rss_bytes")
    check(
        positive(timing.get("quickstart_seconds"))
        and positive(timing.get("backtest_seconds"))
        and positive(memory.get("sampling_interval_ms"))
        and (
            (rss not in (None, "null") and positive(rss))
            or (rss in (None, "null") and memory.get("method") == "unsupported_platform")
        ),
        "耗时与内存是正数（或平台不支持时如实记 null + 方法名）——门禁不比对数值，数值跨机不可比",
        f"timing={timing} memory={memory}",
    )
    body = "\n".join(
        line for line in record.splitlines() if not line.lstrip().startswith("#")
    )
    leaked = [needle for needle in ("C:\\", "/home/", "Users\\", "/tmp/") if needle in body]
    check(
        not leaked,
        "基线正文不出现绝对路径 / 盘符 / 家目录（一台机器的细节不是可移植基线）",
        f"正文里出现 {leaked}",
    )


# —— M5' 的本地可验收子项：Paper 轨验收（与回测轨同源，同样不需要交易所凭据）——
PAPER_ACCEPTANCE_RECORD = "maturity/paper_acceptance.yaml"
PAPER_ACCEPTANCE_SCRIPT = "tools/paper_acceptance.py"
PAPER_ACCEPTANCE_RUNTIME = "deploy/qianxing.runtime.paper-strategy.example.json"
# 记录里必须出现的「这条轨不需要凭据、且主链真的走到终态」的六格自述。
PAPER_RECORD_FACTS = (
    "outcome: passed",
    "credentials_required: false",
    "external_venues: none",
    "network_accessed: false",
    "orders_sent: false",
    "audit_reaches_executed: true",
)
# 脚本里必须存在的四件比对：主链四段 / 同腿重跑不重复下单 / 两个独立目录一致 / 无凭据也跑得通。
PAPER_SCRIPT_MARKERS = (
    "def compare_main_chain(",
    "def compare_rerun(",
    "def compare_independent(",
    "def compare_no_credentials(",
)
# 主链四段：调度 → 策略 → Paper 执行 → 账簿。少一段这条轨就没走完。
PAPER_MAIN_CHAIN = "main_chain: scheduler -> strategy -> paper-execution -> ledger"
PAPER_POSITIVE_COUNTS = ("orders_submitted", "fills", "ledger_entries", "independent_runs")


def paper_track_check() -> None:
    """M5' 的本地可验收子项：Paper 轨（主链 `scheduler -> strategy -> paper-execution -> ledger`）。

    与回测轨同源：这条轨**不需要任何交易所凭据**，它跑的是仓库自己的 Paper venue，因此
    `sandbox_tested` / `production_approved` 两档对它**不适用**（不是"待补"）。六颗分别钉：
    ① 记录在盘且六格自述齐全；② 记录由在盘脚本生成，且脚本真有四件比对（主链四段 / 同腿重跑
    不重复下单 / 两个独立目录事实面相等 / 无凭据也跑得通）；③ 主链四段写全且四个计数都是正数
    ——跑出 0 笔成交的"通过"是空跑；④ 记录正文不出现任何 venue 名称、实盘两档仍全 false
    （Paper 轨不替外部验收作保）；⑤ 终态退场与幂等（无遗留待执行命令、审计链校验通过、
    重跑不重复下单）；⑥ 脚本默认跑仓库那份 paper 模板且在盘，并**主动摘掉**凭据环境变量
    ——"不需要凭据"是构造出来的，不是碰巧没配。

    同样刻意**不**做「记录是否过时」判据：`generated_at_unix` 每跑一次都变，拿它当判据只会
    逼人写死时间戳。过时与否由重跑 `tools/paper_acceptance.py` 回答。
    """
    record_path = ROOT / PAPER_ACCEPTANCE_RECORD
    record = record_path.read_text(encoding="utf-8") if record_path.is_file() else ""
    check(
        record_path.is_file() and all(fact in record for fact in PAPER_RECORD_FACTS),
        "Paper 轨验收记录在盘，且六格自述齐全（passed / 不需凭据 / 无外部 venue / 无网络 / 无订单 / 主链到 Executed）",
        f"缺 {PAPER_ACCEPTANCE_RECORD}，或缺事实："
        f"{[fact for fact in PAPER_RECORD_FACTS if fact not in record]}",
    )
    script = case_source(PAPER_ACCEPTANCE_SCRIPT)
    check(
        all(marker in script for marker in PAPER_SCRIPT_MARKERS)
        and re.search(
            r"^generated_by:\s*" + re.escape(PAPER_ACCEPTANCE_SCRIPT) + r"\s*$", record, re.M
        )
        is not None,
        "记录由在盘脚本生成，且脚本真做了「主链四段 / 重跑 / 独立目录 / 无凭据」四件比对",
        f"缺 {PAPER_ACCEPTANCE_SCRIPT} 或它的四个 compare_*，或记录的 generated_by 没指向它",
    )
    counts = {}
    for name in PAPER_POSITIVE_COUNTS:
        found = re.search(rf"^{name}:\s*(\d+)\s*$", record, re.M)
        counts[name] = int(found.group(1)) if found else 0
    check(
        PAPER_MAIN_CHAIN in record
        and all(value > 0 for value in counts.values())
        and counts["independent_runs"] >= 2,
        "Paper 主链四段写全（scheduler -> strategy -> paper-execution -> ledger），四个计数都是正数",
        f"主链在场 {PAPER_MAIN_CHAIN in record}，计数 {counts}",
    )
    # 先剥整行 `#` 注释再找 venue 名：记录头部的说明文字本来就要提"外部证据在哪"，
    # 把那句话当越界声明是误判（与 `backtest_track_check` 同口径）。
    record_body = "\n".join(
        line for line in record.splitlines() if not line.lstrip().startswith("#")
    )
    profiles_text = case_source(CAPABILITIES_FILE)
    leaked = [name for name in ("binance", "okx", "ccxt", "testnet") if name in record_body.lower()]
    check(
        not leaked
        and "sandbox_tested: true" not in profiles_text
        and "production_approved: true" not in profiles_text,
        "Paper 轨不越界替外部验收作保：记录正文不出现任何 venue 名称，实盘两档仍全 false",
        f"记录正文里出现 {leaked}，或有能力被翻真",
    )
    check(
        "pending_commands_after: 0" in record
        and "audit_chain_verified: true" in record
        and "rerun_does_not_duplicate_orders: true" in record
        and "independent_dirs: fact_surface_equal" in record,
        "Paper 轨终态退场且幂等：无遗留待执行命令、审计链校验通过、重跑不重复下单、两个独立目录事实面相等",
        "记录里缺终态退场或幂等那一格——「跑通」与「跑完」不是一回事",
    )
    check(
        'DEFAULT_RUNTIME = WORKSPACE / "deploy" / "qianxing.runtime.paper-strategy.example.json"'
        in script
        and (ROOT / PAPER_ACCEPTANCE_RUNTIME).is_file()
        and "CREDENTIAL_ENV_PREFIXES" in script,
        "脚本默认跑仓库那份 paper 模板且在盘，并主动摘掉凭据环境变量（「不需要凭据」是构造出来的）",
        f"缺 {PAPER_ACCEPTANCE_RUNTIME}，或脚本没摘凭据环境变量",
    )
    check(
        re.search(
            rf"^    paper_acceptance_record:\s*{re.escape(PAPER_ACCEPTANCE_RECORD)}\s*$",
            profiles_text,
            re.M,
        )
        is not None
        and re.search(
            rf"^    paper_acceptance_generator:\s*{re.escape(PAPER_ACCEPTANCE_SCRIPT)}\s*$",
            profiles_text,
            re.M,
        )
        is not None,
        "能力档登记了 Paper 轨的验收记录与生成器（两条轨各有一份仓库资产，读者找得到证据在哪）",
        "`backtest_only` 档只登记了回测轨：Paper 轨的验收边界在能力档里没有落点",
    )

# —— §7 M1：稳定契约单点 + 命名转换矩阵 ——
CONTRACT_MODULE_FILE = "crates/qx-core/src/contract.rs"
CONTRACT_CORE_LIB = "crates/qx-core/src/lib.rs"
CONTRACT_API_FILE = "crates/qx-api/src/lib.rs"
CONTRACT_API_ROUTE = '("GET", "/schema/contract-matrix")'
CONTRACT_TEST_FILE = "crates/qx-core/tests/contract_matrix.rs"
CONTRACT_TEST_CASES = (
    "every_row_speaks_for_itself",
    "matrix_view_and_json_are_the_same_table",
    "the_three_same_name_siblings_are_registered",
)
# 矩阵一行的形状：`ContractConcept { concept: "…", … }` 一行写完。
CONTRACT_ROW = re.compile(r"ContractConcept\s*\{([^}]*)\}")
CONTRACT_STRING_FIELD = re.compile(r'(\w+):\s*"([^"]*)"')
CONTRACT_LIST_FIELD = re.compile(r"duplicates:\s*&\[([^\]]*)\]")
CONTRACT_FIELDS = (
    "concept",
    "canonical_types",
    "canonical_source",
    "duplicates",
    "adapter",
    "note",
)
# 矩阵登记的同名兄弟，必须真的存在同名声明；这三个概念是 P1-5 点名的那三对。
CONTRACT_SIBLING_TYPES = ("Bar", "StrategyContext", "DataProvider")


def _declares_pub_type(text: str, name: str) -> bool:
    """文件里有没有 `pub struct/enum/trait <name>`（按词边界，免得 `Bar` 命中 `BarFrame`）。"""
    return re.search(rf"pub\s+(?:struct|enum|trait)\s+{re.escape(name)}\b", text) is not None


def _parse_contract_rows(module: str) -> list[tuple[dict[str, str], list[str]]]:
    """把矩阵常量按行解析成 `(字段字典, 同名兄弟列表)`。一行一条，折行即解析不到。"""
    rows: list[tuple[dict[str, str], list[str]]] = []
    for line in module.splitlines():
        match = CONTRACT_ROW.search(line)
        if match is None:
            continue
        body = match.group(1)
        fields = dict(CONTRACT_STRING_FIELD.findall(body))
        listed = CONTRACT_LIST_FIELD.search(body)
        duplicates = re.findall(r'"([^"]+)"', listed.group(1)) if listed else []
        rows.append((fields, duplicates))
    return rows


def contract_matrix_check() -> None:
    """§7 M1：稳定契约单点（`qx_core::contract`）+ 命名转换矩阵与真实代码逐条对账。

    矩阵是**声明**，它自己不会保证任何事——所以这里把它对到代码上：规范单点在仓内唯一、
    同名兄弟真在盘、adapter 真有生产读者、没有未登记的第三个同名声明。缺了这几条，
    矩阵就会退化成一张"写了等于没写"的装饰表（`capabilities.yaml` 的教训同族）。
    """
    module_path = ROOT / CONTRACT_MODULE_FILE
    check(
        module_path.is_file(),
        "契约单点模块在盘（crates/qx-core/src/contract.rs）",
        f"缺 {CONTRACT_MODULE_FILE}",
    )
    if not module_path.is_file():
        return
    module = module_path.read_text(encoding="utf-8")
    lib = (ROOT / CONTRACT_CORE_LIB).read_text(encoding="utf-8")
    check(
        "pub mod contract;" in lib
        and re.search(r"pub use self::contract::\{[^}]*ContractConcept", lib) is not None
        and re.search(r"pub use self::contract::\{[^}]*CONTRACT_MATRIX", lib) is not None,
        "契约模块已挂载，并从 qx-core 根重导出 ContractConcept 与 CONTRACT_MATRIX",
        "lib.rs 缺 `pub mod contract;` 或重导出行",
    )
    check(
        re.search(
            r"pub struct ContractConcept\s*\{[^}]*\}",
            module,
            re.S,
        )
        is not None
        and all(
            re.search(rf"pub\s+{field}\s*:", module) is not None
            for field in CONTRACT_FIELDS
        ),
        "ContractConcept 六格字段齐（concept/canonical_types/canonical_source/duplicates/adapter/note）",
        "缺字段或结构体被改形",
    )
    check(
        "pub const CONTRACT_MATRIX: &[ContractConcept]" in module
        and "pub fn contract_matrix() -> &'static [ContractConcept]" in module
        and "CONTRACT_MATRIX\n}" in module.replace("    ", "")
        and re.search(
            r"pub fn contract_matrix_json\(\) -> String\s*\{[^}]*contract_matrix\(\)",
            module,
            re.S,
        )
        is not None,
        "矩阵单源：contract_matrix() 返回 CONTRACT_MATRIX，contract_matrix_json() 只序列化它",
        "视图函数没读常量，或 JSON 形态自己另写了一份表",
    )
    rows = _parse_contract_rows(module)
    check(
        len(rows) >= 8
        and all(fields.get("concept") and fields.get("canonical_source") for fields, _ in rows)
        and all(fields.get("canonical_source", "").startswith("crates/") for fields, _ in rows)
        and all(fields.get("note") for fields, _ in rows),
        "矩阵按行可解析且每行自述齐全（concept / canonical_source 是仓内相对路径 / note 非空）",
        f"解析到 {len(rows)} 行；缺字段的行 {[f.get('concept') for f, _ in rows if not f.get('note')]}",
    )
    canonical_missing = []
    for fields, _ in rows:
        source = ROOT / fields["canonical_source"]
        names = [name.strip() for name in fields["canonical_types"].split(",") if name.strip()]
        if not source.is_file() or not all(
            _declares_pub_type(source.read_text(encoding="utf-8"), name) for name in names
        ):
            canonical_missing.append(fields["concept"])
    check(
        not canonical_missing,
        "每行的规范单点真的声明了它点名的那些类型（`pub struct/enum/trait`）",
        f"规范单点对不上：{canonical_missing or '无'}",
    )
    sibling_missing = []
    for _, duplicates in rows:
        for entry in duplicates:
            type_name, _, rel = entry.partition("@")
            target = ROOT / rel
            if not rel or not target.is_file() or not _declares_pub_type(
                target.read_text(encoding="utf-8"), type_name
            ):
                sibling_missing.append(entry)
    check(
        not sibling_missing,
        "每个同名兄弟都在它登记的路径上真有一份同名声明（`类型名@路径` 两半都要对得上）",
        f"对不上的同名兄弟：{sibling_missing or '无'}",
    )
    # 没有未登记的第三个同名声明：登记了几个，仓内生产代码里就该有几个。
    production = _production_sources()
    declaration_counts: dict[str, int] = {}
    for name in CONTRACT_SIBLING_TYPES:
        declaration_counts[name] = sum(
            len(re.findall(rf"pub\s+(?:struct|enum|trait)\s+{re.escape(name)}\b", "\n".join(lines)))
            for lines in production.values()
        )
    registered: dict[str, int] = {name: 0 for name in CONTRACT_SIBLING_TYPES}
    for _, duplicates in rows:
        for entry in duplicates:
            type_name = entry.partition("@")[0]
            if type_name in registered:
                registered[type_name] += 1
    # 规范单点自己那一份也算：登记 n 个同名兄弟 = 仓内应当有 n+1 份声明。
    drifted = {
        name: (declaration_counts[name], registered[name] + 1)
        for name in CONTRACT_SIBLING_TYPES
        if declaration_counts[name] != registered[name] + 1
    }
    check(
        not drifted,
        "同名兄弟没有未登记的第三份：仓内声明数 == 规范单点 1 + 登记的同名兄弟数",
        f"实际/应有 {drifted or '无'}",
    )
    adapter_missing = []
    for fields, _ in rows:
        adapter = fields.get("adapter", "")
        if not adapter:
            continue
        function = adapter.rsplit("::", 1)[-1]
        if not any(
            re.search(rf"\bfn\s+{re.escape(function)}\b", "\n".join(lines))
            for lines in production.values()
        ):
            adapter_missing.append(adapter)
    check(
        not adapter_missing,
        "矩阵点名的 adapter 都真有生产定义（登记完就躺着不算数）",
        f"找不到 {adapter_missing or '无'}",
    )
    check(
        any(
            not fields.get("adapter") and "刻意" in fields.get("note", "")
            for fields, duplicates in rows
            if duplicates
        ),
        "同名兄弟要么有 adapter，要么在 note 里交代「刻意不同层」——两者都没有的形态不合法",
        "没有任何一行以「刻意不同层」的口径登记同名兄弟（三对里至少有一对是这样）",
    )
    api = production_text((ROOT / CONTRACT_API_FILE).read_text(encoding="utf-8"))
    check(
        CONTRACT_API_ROUTE in api and "contract_matrix_json()" in api,
        "矩阵有一条真读面：qx-api 的 GET /schema/contract-matrix 直接序列化 contract_matrix_json()",
        f"路由表里没有 {CONTRACT_API_ROUTE}，或它没调 contract_matrix_json()",
    )
    check(
        (ROOT / CONTRACT_TEST_FILE).is_file()
        and all(
            case in case_source(CONTRACT_TEST_FILE) for case in CONTRACT_TEST_CASES
        ),
        "契约矩阵的行为面在盘（每行自述 / 视图与 JSON 同表 / 三对同名兄弟在册）",
        f"缺 {CONTRACT_TEST_FILE} 或它的三条用例",
    )


# —— §6.4 P2-2：自研 HTTP/WS 面覆盖表与真实代码逐格对账 ——
HTTP_SURFACE_FILE = "maturity/http_surface.yaml"
HTTP_SURFACE_SELF_BUILT = (
    "crates/qx-api/src/transport.rs",
    "crates/qx-api/src/ws.rs",
    "crates/qx-api/src/admission.rs",
    "crates/qx-api/src/lib.rs",
)
HTTP_SURFACE_DECISION = "keep_self_built"
# 非 covered 行（gap / partial）才进 accepted_gaps；by_design 是刻意不做的选择，不计入缺口。
HTTP_SURFACE_NON_COVERED = ("gap", "partial")
HTTP_SURFACE_FIELDS = ("area", "status", "anchor", "case", "note")


def _parse_http_surface(text: str) -> tuple[list[dict[str, str]], dict[str, str], list[str]]:
    """解析 http_surface.yaml：返回 `(areas 列表, verdict 字段, self_built_file 列表)`。

    不引入 yaml 依赖，按本仓 capabilities/backtest 表既有的逐行解析口径。每张表一行、
    area 项跨多行（`  - area:` 起头，其下 `status/anchor/case/note` 四格在四空格缩进）。
    """
    lines = text.splitlines()
    self_built: list[str] = []
    areas: list[dict[str, str]] = []
    verdict: dict[str, str] = {}
    in_areas = in_verdict = False
    current: dict[str, str] | None = None
    for line in lines:
        if re.match(r"^self_built_file:\s*\S", line):
            self_built.append(line.split(":", 1)[1].strip())
            continue
        if re.match(r"^areas:\s*$", line):
            in_areas, in_verdict, current = True, False, None
            continue
        if re.match(r"^verdict:\s*$", line):
            in_areas, in_verdict, current = False, True, None
            continue
        if re.match(r"^\S", line):  # 顶层键结束当前段
            in_areas = in_verdict = False
            current = None
        if in_areas:
            if m := re.match(r"^  - area:\s*(.+?)\s*$", line):
                current = {"area": m.group(1)}
                areas.append(current)
            elif current is not None:
                if m := re.match(r"^    (status|anchor|case|note):\s*(.*?)\s*$", line):
                    current[m.group(1)] = m.group(2)
        elif in_verdict:
            if m := re.match(
                r"^  (decision|reason|accepted_gaps|migration_trigger):\s*(.*?)\s*$", line
            ):
                verdict[m.group(1)] = m.group(2)
    return areas, verdict, self_built


def http_surface_check() -> None:
    """§6.4 P2-2：自研 HTTP/WS 面覆盖表与真实代码逐格对账。

    「要不要把 API 迁到成熟 HTTP 库」不能靠感觉答。这张表把自研面逐格登记
    （每格写清「由哪个符号负责、哪条用例守着、覆盖到什么程度」），缺口是数出来的，
    不是估出来的。这里把它对到代码上：

    ① self_built_file 全部在盘；
    ② 每行可解析、四格（area/status/anchor/case）+ note 自述齐全；
    ③ 每行 anchor 的 `<文件> <符号>` 里，符号真在文件里声明（fn/const/struct/…）；
    ④ 每行 case 的 `<文件> <用例>` 里，用例真在测试文件里（`fn <用例>` 在盘）；
    ⑤ verdict.decision 落 keep_self_built，且 reason / migration_trigger 都非空；
    ⑥ accepted_gaps 恰好等于「非 covered 行」（gap/partial），by_design 不入列。

    任何一格与代码对不上，表就退化成一张"写了等于没写"的装饰表——这正是
    capabilities.yaml 早年的教训（登记了却从不核对）。
    """
    path = ROOT / HTTP_SURFACE_FILE
    if not path.is_file():
        check(False, "自研 HTTP 面覆盖表在盘（maturity/http_surface.yaml）", f"缺 {HTTP_SURFACE_FILE}")
        return
    areas, verdict, self_built = _parse_http_surface(path.read_text(encoding="utf-8"))
    check(
        self_built and all((ROOT / f).is_file() for f in self_built),
        "自研 HTTP 面覆盖表点名的 self_built_file 全部在盘",
        f"失效/缺失：{[f for f in self_built if not (ROOT / f).is_file()]}",
    )
    check(
        len(areas) >= 20
        and all(
            all(field in row and row[field] for field in HTTP_SURFACE_FIELDS)
            for row in areas
        ),
        "覆盖表按行可解析，且每行自述齐全（area/status/anchor/case/note 四格 + note）",
        f"解析到 {len(areas)} 行；"
        f"缺字段 {[r.get('area') for r in areas if not all(f in r and r[f] for f in HTTP_SURFACE_FIELDS)]}",
    )
    # 每行 anchor：`<文件> <符号>`，符号必须在文件里真有声明（fn/const/struct/enum/static/type/trait）。
    anchor_missing = []
    for row in areas:
        rel, _, symbol = row["anchor"].partition(" ")
        target = ROOT / rel
        if not rel or not symbol or not target.is_file():
            anchor_missing.append(row["area"])
            continue
        if re.search(
            rf"\b(?:fn|const|struct|enum|static|type|trait)\s+{re.escape(symbol)}\b",
            target.read_text(encoding="utf-8"),
        ) is None:
            anchor_missing.append(f"{row['area']}（{symbol}）")
    check(
        not anchor_missing,
        "每行点名的 anchor 符号真的在它登记的文件中声明（fn/const/struct/enum/static/type/trait）",
        f"对不上的 anchor：{anchor_missing or '无'}",
    )
    # 每行 case：`<文件> <用例>`，用例必须在测试文件里真有 `fn <用例>`。
    case_missing = []
    for row in areas:
        rel, _, test_fn = row["case"].partition(" ")
        target = ROOT / rel
        if not rel or not test_fn or not target.is_file():
            case_missing.append(row["area"])
            continue
        if re.search(rf"\bfn\s+{re.escape(test_fn)}\b", case_source(rel)) is None:
            case_missing.append(f"{row['area']}（{test_fn}）")
    check(
        not case_missing,
        "每行点名的 case 用例真的在它登记的测试文件里（fn <用例> 在盘）",
        f"找不到的用例：{case_missing or '无'}",
    )
    check(
        verdict.get("decision") == HTTP_SURFACE_DECISION
        and bool(verdict.get("reason"))
        and bool(verdict.get("migration_trigger")),
        "verdict 落 keep_self_built，且 reason 与 migration_trigger 都非空（迁库触发条件在场）",
        f"decision={verdict.get('decision')!r}，reason 空={not verdict.get('reason')}，"
        f"trigger 空={not verdict.get('migration_trigger')}",
    )
    # accepted_gaps 必须恰好等于「非 covered 行」（gap/partial），不多不少、不漏 by_design。
    non_covered = {row["area"] for row in areas if row["status"] in HTTP_SURFACE_NON_COVERED}
    listed = {g.strip() for g in verdict.get("accepted_gaps", "").split(",") if g.strip()}
    check(
        non_covered == listed,
        "accepted_gaps 恰好等于非 covered 行（gap/partial），by_design 不入列",
        f"应有 {sorted(non_covered)}；登记 {sorted(listed)}",
    )


VENUE_REPORT_TEST = "crates/contract-tests/tests/venue_report_contract"
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
    contract = case_source(VENUE_REPORT_TEST)
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
    内核用例只用公开 API、整体迁到 `crates/qx-core/tests/ledger/`（V12 D3 由单文件拆成目录模块）。
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


# V13 R1-A3：venue 识别的单源。
VENUE_FAMILY_DEFINITION = "crates/qx-core/src/venue.rs"
VENUE_ID_DEFINITION = "crates/qx-core/src/identity.rs"
# 允许读取家族判定的消费者文件集合：少一个 = 有人把判定搬回本地，多一个 = 另起第二份。
VENUE_FAMILY_CONSUMERS = (
    "crates/qx-cli/src/live_check.rs",
    "crates/qx-cli/src/market_bridges.rs",
    "crates/qx-cli/src/venue_runtime/binance_submit.rs",
    "crates/qx-cli/src/venue_runtime/binance_venue.rs",
    "crates/qx-cli/src/venue_runtime/ccxt_submit.rs",
    "crates/qx-cli/src/venue_runtime/paper_submit.rs",
    "crates/qx-cli/src/venue_runtime/paper_worker.rs",
    "crates/qx-cli/src/venue_runtime/worker_runtime.rs",
    "crates/qx-cli/src/worker_entry.rs",
    "crates/qx-orchestrator/src/lib.rs",
    "crates/qx-runtime/src/runtime_config/topology_validation.rs",
    "crates/qx-runtime/src/worker_policy.rs",
)
# 收拢之前这些写法各有 1–5 份抄本；现在它们是"另起第二份"的指纹，值 = 允许出现的那一处。
# 指纹都带 `venue` 接收者：`environment` 等同名写法读的是另一个字段（如 scheduler 的
# `environment.eq_ignore_ascii_case("paper")` 判的是部署环境，不是 Venue），不该被误伤。
VENUE_REIMPLEMENTATION = {
    'contains("binance")': (VENUE_FAMILY_DEFINITION,),
    'eq_ignore_ascii_case("BINANCE")': (VENUE_ID_DEFINITION,),
    'venue.eq_ignore_ascii_case("paper")': (),
    'venue == "paper"': (),
}
VENUE_TEST_CASES = (
    "binance_family_is_matched_by_substring_regardless_of_case",
    "paper_is_exact_and_a_paper_prefixed_venue_is_not_paper",
    "absent_venue_stays_absent_instead_of_becoming_other",
)


def venue_identity_check() -> None:
    """`venue_id` → Venue 家族只有一个读点（V13 §5 A3）。

    审计时同一个问题在仓库里有 19 处 worker 级写法：9 处问"是不是 Binance"
    （`qx-orchestrator` 五份逐字相同的 `.map(|venue| venue.to_ascii_lowercase()
    .contains("binance")) == Some(true)`、`qx-cli` 三份 `is_some_and` 变体、
    `qx-runtime` 一份局部 `fn is_binance`）、10 处问"是不是 Paper"（`is_some_and`
    与两种 `map(...).unwrap_or(...)` 抄法），另有标的级整名比较 `eq_ignore_ascii_case("BINANCE")`
    六处收进 `VenueId::is_binance`。危害方式不是"现在算错"，而是下一次改口径时只改其中一份
    —— 例如把子串改成整名，`binance-testnet`（仓内实测 13 处取值）会静默脱离 Binance 家族，
    编排照常返回 Ok 但那条 worker 没人拉起；把 Paper 的整名改成前缀匹配，`paper-proxy`
    会被当成虚拟撮合域。
    """
    production = {
        path.relative_to(ROOT).as_posix(): without_line_comments(
            non_test_source(path.read_text(encoding="utf-8"))
        )
        for path in rust_sources()
    }
    family = production.get(VENUE_FAMILY_DEFINITION, "")
    identity = production.get(VENUE_ID_DEFINITION, "")
    check(
        family.count("pub fn parse(") == 1
        and family.count("pub fn parse_option(") == 1
        and family.count("pub enum VenueFamily") == 1
        and identity.count("pub fn is_binance(") == 1,
        "Venue 家族与标的级 Binance 判定各只有一处定义",
        f"venue.rs parse={family.count('pub fn parse(')} parse_option="
        f"{family.count('pub fn parse_option(')} enum={family.count('pub enum VenueFamily')}；"
        f"identity.rs is_binance={identity.count('pub fn is_binance(')}",
    )
    revived = {}
    for fragment, allowed in VENUE_REIMPLEMENTATION.items():
        sites = [
            rel
            for rel, text in production.items()
            if fragment in text and rel not in allowed
        ]
        if sites:
            revived[fragment] = sites
    check(
        not revived,
        "已收拢的 venue 判定式不得在生产代码里另起第二份",
        f"重新出现于 {revived}",
    )
    consumers = sorted(
        rel
        for rel, text in production.items()
        if "VenueFamily::" in text and rel != VENUE_FAMILY_DEFINITION
    )
    check(
        consumers == sorted(VENUE_FAMILY_CONSUMERS),
        "读取 Venue 家族的消费者与登记表逐一对应（新增判定必须登记）",
        f"登记 {sorted(VENUE_FAMILY_CONSUMERS)} / 实际 {consumers}",
    )
    venue_tests = (ROOT / VENUE_FAMILY_DEFINITION).read_text(encoding="utf-8")
    check(
        all(f"fn {case}(" in venue_tests for case in VENUE_TEST_CASES)
        and venue_tests.count('#[test]') == len(VENUE_TEST_CASES),
        "家族口径的三条用例在册：子串、整名、缺席不得折进 Other",
        f"在册 {[case for case in VENUE_TEST_CASES if f'fn {case}(' in venue_tests]}，"
        f"#[test] 总数 {venue_tests.count('#[test]')}",
    )
    plan_body = _fn_body(
        (ROOT / "crates/qx-orchestrator/src/tests.rs").read_text(encoding="utf-8"),
        "fn worker_plan_routes_a_testnet_binance_venue_to_the_private_worker",
    )
    check(
        'venue_id: Some("binance-testnet".into())' in plan_body
        and 'Some("binance-worker")' in plan_body,
        "testnet 写法必须被编排认成 Binance 私有 worker（用例在断言现场，不在别处）",
        f"用例体可定位={bool(plan_body)}，缺输入格或期望格",
    )
    paper_plan_body = _fn_body(
        (ROOT / "crates/qx-orchestrator/src/tests.rs").read_text(encoding="utf-8"),
        "fn worker_plan_routes_only_the_exact_paper_venue_to_the_local_worker",
    )
    check(
        'venue_id = Some("paper-proxy".into())' in paper_plan_body
        and 'Some("paper-worker")' in paper_plan_body,
        "编排的 Paper 分支钉住整名口径：空白写法仍本地，paper 前缀写法必须被拒",
        f"用例体可定位={bool(paper_plan_body)}，缺本地分支或 paper-proxy 反例",
    )
    naming_body = _fn_body(
        (ROOT / "crates/qx-cli/src/tests/account_event_log_identity.rs").read_text(
            encoding="utf-8"
        ),
        "fn account_identity_normalizes_its_key_in_one_place",
    )
    check(
        '"binance-main-binance-testnet-events"' in naming_body
        and 'account_event_log_name("main", "binance-testnet")' in naming_body,
        "账户日志命名的用例钉住同一家族口径：testnet 不得换前缀把账户拆成两本账",
        f"用例体可定位={bool(naming_body)}",
    )
    # 口径分裂的证据：两处判定读的是不同字段，不得互相"统一"。注释里说清差异，
    # 所以这一条读的是带注释的原文，而不是上面剥掉注释的生产代码。
    family_docs = non_test_source((ROOT / VENUE_FAMILY_DEFINITION).read_text(encoding="utf-8"))
    identity_docs = non_test_source((ROOT / VENUE_ID_DEFINITION).read_text(encoding="utf-8"))
    check(
        "大小写无关的子串" in family_docs and "不能改成子串" in identity_docs,
        "两份口径的差异写在各自定义处注释里（家族读账户域、整名读产品 venue）",
        f"venue.rs 侧={('大小写无关的子串' in family_docs)}，"
        f"identity.rs 侧={('不能改成子串' in identity_docs)}，"
        "缺任一侧的口径说明，下一次'顺手统一'就会把它折成一处",
    )



# V13 §5 A4：记账币种的单源与它进入发布产物的那条链。
SETTLEMENT_CURRENCY_DEFINITION = "crates/qx-core/src/identity.rs"
SETTLEMENT_CURRENCY_CONST = "DEFAULT_SETTLEMENT_CURRENCY"
# 整串带引号的字面量：`BTCUSDT`、`"usdt"` 都不算命中，因为前者是标的名、后者是待归一的声明值。
SETTLEMENT_CURRENCY_LITERAL = '"USDT"'
# 三条回落链各自的读取点（**压掉空白**后比对：真实源码里这些链是跨行写的）。
# 第五处出现 = 有人新写了一条兜底却没接常量。
SETTLEMENT_CURRENCY_FALLBACK_FRAGMENTS = {
    ".unwrap_or_else(||DEFAULT_SETTLEMENT_CURRENCY.into())": "crates/qx-cli/src/backtests/mod.rs",
    ".unwrap_or(DEFAULT_SETTLEMENT_CURRENCY).to_ascii_uppercase()": "crates/qx-cli/src/venue_runtime/worker_runtime.rs",
    ".unwrap_or_else(||DEFAULT_SETTLEMENT_CURRENCY.to_string())": "crates/qx-cli/src/venue_runtime/worker_runtime.rs",
    "cash:BTreeMap::from([(DEFAULT_SETTLEMENT_CURRENCY.into(),Money::from_i64("
    "crate::backtests::DEFAULT_BACKTEST_INITIAL_CASH).raw(),)]),": "crates/qx-cli/src/ecosystem_smoke.rs",
}
# 除定义点外，读取常量的生产文件；`qx-core/src/lib.rs` 是再导出，也算消费者。
SETTLEMENT_CURRENCY_CONSUMERS = (
    "crates/qx-cli/src/backtests/mod.rs",
    "crates/qx-cli/src/ecosystem_smoke.rs",
    "crates/qx-cli/src/main.rs",
    "crates/qx-cli/src/venue_runtime/worker_runtime.rs",
    "crates/qx-core/src/contract.rs",
    "crates/qx-core/src/lib.rs",
)
SETTLEMENT_CURRENCY_CASE_FILE = "crates/qx-cli/src/tests/settlement_currency_single_source.rs"
SETTLEMENT_CURRENCY_CASES = (
    "every_settlement_currency_fallback_reads_the_same_default",
    "declaring_the_default_currency_matches_declaring_nothing",
    "changing_only_the_settlement_currency_moves_the_published_fingerprint",
)


def production_code_text(path: Path) -> str:
    """一行生产代码：既不在用例面里，也不在 `#[cfg(test)]` 之后，也不是注释行。

    `without_line_comments` 会连着 `///` 文档一起保留（`//` 是它的前缀），所以"某串字面量
    不得再出现在生产代码里"这类判据必须先把整行注释剥掉——否则单源常量自己的口径说明
    （`它只是缺省值，不是白名单`）会把自己判成第二处硬编码，而正确的修法是把注释删掉，
    正好毁掉那条防误改的说明。
    """
    source = non_test_source(path.read_text(encoding="utf-8", errors="replace"))
    return "\n".join(
        line for line in source.splitlines() if not line.strip().startswith("//")
    )


def is_case_scoped_source(path: Path) -> bool:
    """整份文件就是用例现场：`src/tests/` 下的主题文件，或以 `*_tests` / `tests` 命名的模块。

    与 `is_cli_case_file` 同义但按全仓路径判定（后者对 `qx-cli` 之外的路径会抛
    `relative_to` 的 ValueError），并且不把 `backtest.rs` 这类**生产**文件算作用例面——
    文件名里带 "test" 不等于那是测试。
    """
    return "tests" in path.parts[:-1] or path.stem.endswith("tests")


def settlement_currency_production_sites() -> dict[str, list[str]]:
    """生产代码里写死 `"USDT"` 整串字面量的位置（`文件 → 行号`），空表即达标。"""
    sites: dict[str, list[str]] = {}
    for path in rust_sources():
        if is_case_scoped_source(path):
            continue
        rel = path.relative_to(ROOT).as_posix()
        lines = production_code_text(path).splitlines()
        marked = [
            str(index)
            for index, line in enumerate(lines, 1)
            if SETTLEMENT_CURRENCY_LITERAL in line
        ]
        if marked:
            sites[rel] = marked
    return sites


def settlement_currency_definition_count() -> int:
    """生产代码里 `pub const *SETTLEMENT_CURRENCY*: …` 的出现处数（按文件计）。"""
    return sum(
        1
        for path in rust_sources()
        if not is_case_scoped_source(path)
        and re.search(r"pub const \w*SETTLEMENT_CURRENCY\w*:", production_code_text(path))
    )


def settlement_currency_check() -> None:
    """记账币种只有一个缺省值，且它必须留在发布产物里（V13 §5 A4）。

    审计时 `"USDT"` 作为"没人声明时按哪种币记账"的答案在 Rust 生产代码里写着四份
    （见 `logs/s28_currency_sites_a4.txt`：回测装配 `backtest_settlement_currency`、
    worker 自身声明的兜底 `worker_settlement_currency`、账户日志写入方全缺席时的兜底
    `settlement_currency_among_workers`、以及 `ecosystem_smoke` 自检查现金簿的键）。
    四份字面量本身没有算错任何东西——这正是它能活下来的原因。危害方式是**下一次改口径
    只改一处**：把公司内币种从 USDT 换成 USDC 时，回测侧改成了 USDC、worker 侧仍是
    USDT，两条链各自读一本账，产物却都自称"口径来自配置"，对账要等到人工看摘要才看得见。
    第四份更隐蔽：`ecosystem_smoke` 的现金簿键如果与引擎兜底不同，自检查策略会读到空账簿
    并安静地不下单，看起来像"行情没信号"。

    第二件事是**记账币种必须留在发布产物里**。它决定现金腿落在哪本账；换币种还能给出
    同一个 `result_hash`，产物就无法声明"这份收益是哪种币的收益"。实测这条链是通的
    （USDT/CNY 两跑给 `71e90bdf1b27a4dc` / `a4f465684b3454f0`），所以这里钉的是"不许退回"，
    而不是补一条从来没通过过的口径。
    """
    definition = production_code_text(ROOT / SETTLEMENT_CURRENCY_DEFINITION)
    definition_sites = f'pub const {SETTLEMENT_CURRENCY_CONST}: &str = '
    usdt_binding = f'{SETTLEMENT_CURRENCY_CONST}: &str = ' + SETTLEMENT_CURRENCY_LITERAL
    check(
        definition.count(definition_sites) == 1 and definition.count(usdt_binding) == 1,
        "记账币种的缺省值只有一处定义，且值仍是 USDT（换币种要改这一格，不是四处）",
        f"{SETTLEMENT_CURRENCY_DEFINITION} 定义点="
        f"{definition.count(definition_sites)}，绑定值={usdt_binding in definition}",
    )
    # 第二颗同类常量就是第二份口径：它可能叫 SETTLEMENT_CURRENCY / CASH_BOOK_CURRENCY，
    # 只要生产代码里再出现一颗 `*SETTLEMENT_CURRENCY*` 公共常量，上面那条"改一处"就失效了。
    check(
        settlement_currency_definition_count() == 1,
        "全仓只有定义点那一颗 *SETTLEMENT_CURRENCY* 公共常量",
        f"实际出现 {settlement_currency_definition_count()} 处",
    )
    sites = {
        rel: rows
        for rel, rows in settlement_currency_production_sites().items()
        if rel != SETTLEMENT_CURRENCY_DEFINITION
    }
    check(
        not sites,
        "币种兜底已全部接回常量：生产代码里除定义点外没有第二处写死的 "
        + SETTLEMENT_CURRENCY_LITERAL,
        f"重新硬编码于 {sites}",
    )
    # 定义处必须把"这是缺省值不是白名单"写在注释里：这条读的是带注释的原文，
    # 因为可执行行判据管不住"有人删掉那句说明"。
    definition_docs = non_test_source(
        (ROOT / SETTLEMENT_CURRENCY_DEFINITION).read_text(encoding="utf-8")
    )
    check(
        SETTLEMENT_CURRENCY_LITERAL in definition_docs
        and "缺省值" in definition_docs
        and "白名单" in definition_docs,
        "单源常量自己带着币种字面量与口径说明（缺省值≠合法币种白名单）",
        f"字面量={SETTLEMENT_CURRENCY_LITERAL in definition_docs}，"
        f"缺省值说明={'缺省值' in definition_docs}，白名单说明={'白名单' in definition_docs}",
    )
    consumers = sorted(
        rel
        for path in rust_sources()
        if not is_case_scoped_source(path)
        and SETTLEMENT_CURRENCY_CONST in production_code_text(path)
        for rel in [path.relative_to(ROOT).as_posix()]
        if rel != SETTLEMENT_CURRENCY_DEFINITION
    )
    check(
        consumers == sorted(SETTLEMENT_CURRENCY_CONSUMERS),
        "回落链消费者与登记表逐一对应（新增兜底必须登记，不许静默新增第五处）",
        f"登记 {sorted(SETTLEMENT_CURRENCY_CONSUMERS)} / 实际 {consumers}",
    )
    for fragment, rel in SETTLEMENT_CURRENCY_FALLBACK_FRAGMENTS.items():
        collapsed = re.sub(r"\s+", "", production_code_text(ROOT / rel))
        check(
            re.sub(r"\s+", "", fragment) in collapsed,
            f"回落链仍读单源常量：{rel} 的 {fragment}",
            f"{rel} 里找不到 {fragment}",
        )
    case_text = (ROOT / SETTLEMENT_CURRENCY_CASE_FILE).read_text(encoding="utf-8")
    check(
        all(f"fn {case}(" in case_text for case in SETTLEMENT_CURRENCY_CASES)
        and case_text.count("#[test]") == len(SETTLEMENT_CURRENCY_CASES),
        "单源口径的三条用例在册：三条回落链同值、声明缺省==不声明、换币种动产物哈希",
        f"在册 {[c for c in SETTLEMENT_CURRENCY_CASES if f'fn {c}(' in case_text]}，"
        f"#[test] 总数 {case_text.count('#[test]')}",
    )
    # 第三条用例比的是哪一格，是这条判据最容易"顺手改掉"的地方：digest 覆盖墙钟与
    # 隔离根目录路径，拿它当判据会把"币种动了"和"这次运行本身不同"混成一格。
    fingerprint_body = _fn_body(
        case_text,
        "fn changing_only_the_settlement_currency_moves_the_published_fingerprint(",
    )
    check(
        'run("USDT")' in fingerprint_body
        and 'run("CNY")' in fingerprint_body
        and 'summary["result_hash"]' in fingerprint_body
        and ".digest()" not in fingerprint_body,
        "币种指纹用例钉的是产物里的 result_hash，且同币种重跑必须相等（不许改用会随墙钟动的 digest）",
        f"用例体可定位={bool(fingerprint_body)}，USDT/CNY 侧或 result_hash 口径缺失",
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
    # `api.console` 段（同源 BFF 控制台）与 API 面同属部署配置，同样 `deny_unknown_fields`。
    "ConsoleRuntimeConfig",
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


# 棘轮覆盖的文件集合（V12 D3）：生产代码 + 集成用例两类。原来只数 `src/`，于是"把代码搬进
# 测试目录"能绕过棘轮，四个 ≥500 行的 `tests/*.rs` 长期不入账（V12 §4.16）。
LINE_RATCHET_GLOBS = ("*/src/**/*.rs", "*/tests/**/*.rs")


def source_line_counts() -> dict[str, int]:
    return {
        path.relative_to(ROOT).as_posix(): len(path.read_text(encoding="utf-8").splitlines())
        for pattern in LINE_RATCHET_GLOBS
        for path in sorted(CRATES.glob(pattern))
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


# V11 K2 / R7-d，V13 第 8 轮 A4/A5：出站投递预算、分页上界与停摆计数的判据落点。
OUTBOX_RELAY_FILE = "crates/qx-storage/src/lib.rs"
OUTBOX_BACKEND_FILES = (
    "crates/qx-storage/src/sqlite.rs",
    "crates/qx-storage/src/postgres.rs",
    "crates/qx-storage/src/file/outbox.rs",
)
OUTBOX_METRIC_FILE = "crates/qx-cli/src/event_pipeline.rs"
OUTBOX_PARK_CASE_FILE = "crates/qx-storage/tests/outbox_backend_semantics.rs"
OUTBOX_ALERT_FILE = "deploy/prometheus/qianxing-alerts.yml"
OUTBOX_PARKED_METRIC = "qx_outbox_relay_parked"
OUTBOX_BACKEND_LABELS = ("file", "sqlite", "postgres")
# 页界各家一处（SQL 占位符 vs 目录遍历后截断），问的是同一件事：把这一家自己的读收口成一页。
# 路径显式重复而不按 `OUTBOX_BACKEND_FILES` 的下标配对——顺序一动就会把 needle 配到别人身上并静默通过。
OUTBOX_PAGE_NEEDLES = (
    ("crates/qx-storage/src/sqlite.rs", "LIMIT ?3"),
    ("crates/qx-storage/src/postgres.rs", "LIMIT $3"),
    ("crates/qx-storage/src/file/outbox.rs", "deliverable.truncate(limit)"),
)
# 阈值绑回常量的读点：候选集排序与停摆计数各一次。某一家的绑定口只剩一处，形状就是
# 「parked 计数改成在内存里 filter 那一页」——它同时把状态量退回页数。
OUTBOX_THRESHOLD_NEEDLES = (
    ("crates/qx-storage/src/sqlite.rs", "db_string(crate::OUTBOX_MAX_ATTEMPTS as u64)"),
    ("crates/qx-storage/src/postgres.rs", "u64_text(crate::OUTBOX_MAX_ATTEMPTS as u64)"),
    ("crates/qx-storage/src/file/outbox.rs", "crate::outbox_exhausted(event.attempts)"),
)


def outbox_page_and_parked_check() -> None:
    """Outbox 的投递预算、分页上界与停摆计数：判据出口唯一、三家后端各自有界、计数有读者。

    修前的形状是一条永远发不出去的事件每轮都被重新端出，并按 `created_ts` 排在最前挤住整条
    尾巴；`available` 又没有上界，每轮 pump 全表读 payload；而"有条事件发不出去"这件事在
    指标、自报行与告警里都没有一格（V11 K2 / R7-d，第 8 轮 A4/A5 补执行者与读者）。
    """
    outbox = production_text((ROOT / OUTBOX_RELAY_FILE).read_text(encoding="utf-8"))
    pump = _fn_body(outbox, "pub fn pump_once(")
    backends = {
        rel: production_text((ROOT / rel).read_text(encoding="utf-8"))
        for rel in OUTBOX_BACKEND_FILES
    }
    check(
        outbox.count("pub const OUTBOX_MAX_ATTEMPTS") == 1
        and outbox.count("pub const fn outbox_exhausted") == 1
        and all("attempts >=" not in text for text in backends.values())
        and all(
            re.search(r"attempts[^\n]*>=\s*\d", text) is None
            for text in backends.values()
        ),
        "出站投递预算只有一个判据出口，三本后端都不自己数次数、也不把阈值抄成字面量（V11 K2、R7-d）",
        "判据搬进 SQL 就要在 file/sqlite/postgres 三处同步，而消费者侧的死信判据早就收在一处；"
        "阈值抄成 `>= 8` 之后改预算只动常量，SQL 里那两份悄悄不变，用例面仍然全绿",
    )
    check(
        -1 < pump.find("outbox_exhausted(event.attempts)") < pump.find("claim_outbox")
        and pump.count("OutboxRelayReport { parked, ..empty }") == 1
        and pump.count("report.parked =") == 0
        and pump.count("self.store.available_outbox(now, limit)?") == 1
        and "delivered >= limit" not in pump,
        "relay 在 claim 之前跳过停摆事件，页数上界只在 store 那一处读（V11 K2、R7-d）",
        "跳过排在 claim 之后，毒事件每轮仍占一条租约；页数上界在 relay 再数一遍是一份走不到的"
        "第二判据——三本后端的 LIMIT 由跨后端契约用例钉住，relay 这一遍永远不红",
    )
    check(
        outbox.count(
            "fn available_outbox(&self, now: u64, limit: usize)"
            " -> Result<Vec<OutboxEvent>, StorageError>;"
        )
        == 1
        and all(
            text.count("pub fn available(&self, now: u64, limit: usize)") == 1
            for text in backends.values()
        )
        and all(backends[rel].count(needle) == 1 for rel, needle in OUTBOX_PAGE_NEEDLES),
        "投递页数从 trait 一路绑到三本后端：每家的读各自带一份上界（V11 R7-d）",
        "少了任何一家，那一本后端的 pump 仍然每轮全库读 payload，而 relay 侧的 limit 参数照样"
        "传得出去、用例照样绿——只有点名到各家读的那一颗咬得住「预算落在哪一层」",
    )
    check(
        all(
            backends[rel].count(needle) >= 2 for rel, needle in OUTBOX_THRESHOLD_NEEDLES
        ),
        "阈值两处都绑回 `OUTBOX_MAX_ATTEMPTS`：候选集排序与停摆计数各一次（V11 R7-d）",
        "某一家的绑定口只剩一处的形状是「parked 计数改成在内存里 filter 那一页」——"
        "它同时把状态量退回页数，而 SQL 里多写一个字面量 8 由上一颗的 regex 挡",
    )
    check(
        outbox.count("fn count_parked_outbox(&self) -> Result<u64, StorageError>;") == 1
        and all(
            text.count("fn count_parked_outbox(&self) -> Result<u64, StorageError>") == 1
            and text.count("pub fn count_parked(&self) -> Result<u64, StorageError>") == 1
            for text in backends.values()
        )
        and "let parked = self.store.count_parked_outbox()?;" in pump
        and "report.parked.saturating_add(1)" not in pump,
        "停摆条数向库里问、再进 report：三本后端各一份不问页数的计数（V11 R7-d）",
        "逐行数 parked 的形状（`report.parked.saturating_add(1)`）在候选集被 limit 截断后只报"
        "这一页的观察值：库里三条都停摆而 limit=1 时运维看到 1 条，毒事件于是又消失三分之二",
    )
    metrics = (ROOT / OUTBOX_METRIC_FILE).read_text(encoding="utf-8")
    # 自报行的钉子点名到 worker 那一整行，不用裸 `parked={}`：同一个文件里还有两条一次性 relay
    # 出口也念 parked（V13 R5，由「三个出口」那颗判据绑数），裸钉子会让这两颗判据互相吃数。
    check(
        metrics.count("self.parked = report.parked;") == 1
        and metrics.count(f"{OUTBOX_PARKED_METRIC}{{{{worker=") == 1
        and metrics.count(
            "[Outbox relay worker={}] scanned={} published={} retried={} "
            "failures={} conflicts={} parked={}"
        )
        == 1,
        "停摆数一路到 Prometheus 指标与 relay worker 的自报行（V11 K2，第 8 轮 A5）",
        "relay 侧数了却没人读得到，等于把「有条事件发不出去」重新咽回去；"
        "指标行与自报行各一处，少一处就少一个读者",
    )
    alerts = (ROOT / OUTBOX_ALERT_FILE).read_text(encoding="utf-8")
    check(
        alerts.count(f"expr: {OUTBOX_PARKED_METRIC} > 0") == 1
        and f"increase({OUTBOX_PARKED_METRIC}" not in alerts,
        "告警按状态量判：`> 0` 而不是 `increase(...)`（V13 第 8 轮 A5）",
        "这一格是「库里现在还有几条发不出去」，relay 每轮原样写出而不是累加；"
        "换成 increase 之后一条长期停摆的事件只在入账那一刻报一次，此后永远静默",
    )
    cases = (ROOT / OUTBOX_PARK_CASE_FILE).read_text(encoding="utf-8")
    check(
        all(
            f"fn {name}" in cases
            for name in (
                "file_outbox_relay_unblocks_the_tail",
                "sqlite_outbox_relay_unblocks_the_tail",
                "file_outbox_relay_parks_events_after_the_attempt_budget",
            )
        ),
        "K2 用例在位：链尾解封在 file/sqlite 两后端各跑一次，停摆由重试一路跑出来（V11 K2）",
        "解封只在一个后端成立是不够的：候选集顺序由各家 SQL/目录自己排",
    )
    check(
        cases.count("fn assert_parked_rows_yield_the_page_head(") == 1
        and all(
            f'assert_parked_rows_yield_the_page_head(&store, "{rel}-page");' in cases
            for rel in OUTBOX_BACKEND_LABELS
        )
        and all(
            "assert_relay_parked_count_ignores_the_page("
            in _fn_body(
                cases, f"fn {rel}_outbox_relay_parked_count_ignores_the_page("
            )
            for rel in ("file", "sqlite")
        )
        and cases.count("store.available_outbox(20, 1).unwrap().len(),") == 1
        and cases.count("assert_eq!(store.count_parked_outbox().unwrap(), 3);") == 1
        and cases.count("assert_eq!(report.parked, 3,") == 1,
        "R7-d 用例在位：三本后端各钉一次「停摆退到页尾 + limit 真截页」，relay 侧两后端各钉一次"
        "「停摆数不问页数」（V11 R7-d）",
        "共用断言只在一家被调用等于没共用——顺序是各家自己排的。而「helper 定义了但没人调」"
        "在 test 二进制里只会被 dead_code 挡一半，门禁读调用点才认它接了线",
    )
    park_body = _fn_body(cases, "fn assert_parked_outbox_has_an_operator_exit(")
    page_body = _fn_body(cases, "fn assert_parked_rows_yield_the_page_head(")
    check(
        all(
            f'assert_parked_outbox_has_an_operator_exit(&store, "{rel}-park");'
            in _fn_body(cases, f"fn {test}(")
            for rel, test in (
                ("file", "file_outbox_contract"),
                ("sqlite", "sqlite_outbox_contract"),
                ("postgres", "postgres_outbox_lease_fencing_and_retry_contract"),
            )
        ),
        "人工出口那颗 helper 在三本后端各自的用例里各被调用一次，postgres 也在内（V13 第 8 轮 A4）",
        "`count_parked` 的 postgres 实现此前在任何一条腿上都没有调用者：写错 SQL、把停摆条数"
        "念成 0，CI 与本地都不会红。把这颗调用点从 postgres 用例里摘掉，这一格是唯一的读者",
    )
    check(
        park_body.count("OUTBOX_MAX_ATTEMPTS + 6") == 1
        and page_body.count("attempts: OUTBOX_MAX_ATTEMPTS + 6,") == 1,
        "停摆夹具跨数位边界（8 与 14），让「按数字比」与「按文本比」在两颗 helper 里各分岔一次"
        "（V13 第 8 轮 A4）",
        "这一列三本后端都存成文本，两行都写预算值时 8 与 8 的字典序与数字序同一个答案——"
        "摘掉 sqlite 的 `CAST(attempts AS INTEGER)`、摘掉 postgres 的 `::numeric`，用例照样绿",
    )
    check(
        "let before = store.count_parked_outbox().unwrap();" in park_body
        and park_body.count("before + 2") == 1
        and park_body.count("before + 1") == 1,
        "停摆条数按增量问库里的状态量：入账 +2、人工确认之后 +1，两处都现读（V13 第 8 轮 A4）",
        "服务容器作业里多条 postgres 腿共用同一个 DSN，写死绝对值会先被别人的夹具撞红；"
        "只读一次 `before` 当快照，则「ack 之后停摆行数掉下来」这件事重新变成没人问的一格",
    )
    check(
        cases.count("fn assert_outbox_delivery_order(") == 1
        and all(
            f'assert_outbox_delivery_order(&store, "{rel}-order");' in cases
            for rel in OUTBOX_BACKEND_LABELS
        )
        and cases.count("for sequence in [2u64, 20, 3] {") == 2
        and cases.count("assert_eq!(sequences, vec![2, 3, 20]);") == 1,
        "投递顺序的跨后端夹具在三本后端各跑一次，且刻意跨数位边界（2 / 20 / 3）（V11 R7-2）",
        "u64 列存成 TEXT 时字典序把 20 排在 2 与 3 之前，等于把同一毫秒落盘的一串事件念反；"
        "夹具不跨边界的话，摘掉三处 `CAST`/`::numeric` 都量不出来。那串数字出现两次是同一副"
        "夹具的两半——落盘一遍、收尾 claim/ack 一遍，改一半就会留下没人认领的行",
    )


# V11 E5：外部链路验收脚本（`docs/外部链路验收执行方案-V1.md` §2）敲的是命令面上的真入口。
# 脚本点名的命令、worker id 与凭据引用一旦和仓库现状分叉，"验收通过"就发生在一根本不存在
# 的路径上——这类断链平时不会响，只有真去跑外部验收才发现，而那一轮本该由门禁先拦住。
ACCEPTANCE_SCRIPT = "tools/binance_testnet_acceptance.py"
ACCEPTANCE_DOC = "docs/外部链路验收执行方案-V1.md"
# 调用点的统一形状：`run(binary, ["命令", …])` 与 `json_step(binary, "步骤名", ["命令", …])`。
ACCEPTANCE_INVOKE = re.compile(
    r'\(\s*binary\s*,\s*(?:"[^"]*"\s*,\s*)?\[\s*"([a-z][a-z0-9-]*)"', re.M
)
ACCEPTANCE_CONSTANT = re.compile(
    r'^(KEY_ENV|SECRET_ENV|EXECUTION_WORKER|RECONCILE_WORKER|BASE_CONFIG) = .*"([^"]*)"$', re.M
)
# 第二交易所（OKX 经 CCXT 沙盒）走同一份脚本的 `--venue okx`，不复制脚本。这一组常量原先
# 不在上面那条判据的名单里，于是 `--venue okx` 的链路可以整条断掉而门禁不响——「第二个交易所
# 已通」这句话就没有可核对的支点。
ACCEPTANCE_CONSTANT_OKX = re.compile(
    r'^(OKX_BASE_CONFIG|OKX_CCXT_CONFIG|OKX_EXECUTION_WORKER|OKX_RECONCILE_WORKER|OKX_KEY_ENV'
    r"|OKX_SECRET_ENV|OKX_PASS_ENV) = .*\"([^\"]*)\"$",
    re.M,
)
# 子进程调用体：注释行里写的反例（"`subprocess.run(capture_output=True)` 没有截止"）不是调用点，
# 先按行剥掉 `#` 开头的行再取，否则调用计数会被自己的说明文字加一个。
ACCEPTANCE_RUN_CALL = re.compile(r"subprocess\.run\((.*?)\)", re.S)


def acceptance_run_calls(script: str) -> list[str]:
    """返回验收脚本里的 subprocess.run 调用体（排除整行注释）。"""
    code = "\n".join(line for line in script.splitlines() if not line.lstrip().startswith("#"))
    return ACCEPTANCE_RUN_CALL.findall(code)


def external_acceptance_check() -> None:
    """验收脚本 ↔ 命令面 ↔ 验收配置三方对齐。"""
    check(
        (ROOT / ACCEPTANCE_SCRIPT).is_file() and (ROOT / ACCEPTANCE_DOC).is_file(),
        "外部链路验收的脚本与方案文档同在场",
        "缺一即方案不可执行或脚本没有口径来源",
    )
    script = (ROOT / ACCEPTANCE_SCRIPT).read_text(encoding="utf-8")
    constants = dict(ACCEPTANCE_CONSTANT.findall(script))
    check(
        set(constants)
        == {
            "KEY_ENV",
            "SECRET_ENV",
            "EXECUTION_WORKER",
            "RECONCILE_WORKER",
            "BASE_CONFIG",
        },
        "验收脚本的被测常量齐备（下面几项按名字取值）",
        f"读到 {sorted(constants)}",
    )
    invoked = set(ACCEPTANCE_INVOKE.findall(script))
    table = clap_command_table((ROOT / CLI_ARGS_FILE).read_text(encoding="utf-8"))
    check(
        len(invoked) >= 5 and invoked <= set(table),
        f"验收脚本点名的 {len(invoked)} 个入口全部存在于 clap 命令表",
        f"命令表里没有 {sorted(invoked - set(table)) or '无'}",
    )
    help_body = (
        (ROOT / CLI_HELP_FILE)
        .read_text(encoding="utf-8")
        .split('r#"', 1)[-1]
        .split('"#', 1)[0]
    )
    documented = help_printed_commands(help_body)
    check(
        bool(invoked) and invoked <= documented,
        "验收脚本点名的入口都写进了 help（照方案敲与照 help 敲是同一件事）",
        f"帮助里没写 {sorted(invoked - documented) or '无'}",
    )
    # V13 第 8 轮 C7：验收脚本的每一腿都敲真子进程，而 `capture_output=True` 的读侧没有截止。
    # 被测链上一旦出现"连得上但不回话"的形状（NATS ack、WS 帧滴答、DB 持锁），挂住的那一腿
    # 既不失败也不通过——验收门禁替一次阻塞作保，是这类门禁最坏的失效方式。
    calls = acceptance_run_calls(script)
    check(
        len(calls) == 1 and "timeout=LEG_BUDGET_SECONDS" in calls[0],
        "验收脚本唯一那条子进程调用带整体截止（V13 第 8 轮 C7）",
        f"读到 {len(calls)} 条 subprocess.run 调用，带 timeout=LEG_BUDGET_SECONDS 的 "
        f"{sum('timeout=LEG_BUDGET_SECONDS' in body for body in calls)} 条"
        "（多一条无截止的调用就是多一条能挂死的腿；摘掉 timeout= 同理）",
    )
    budget = re.search(r"^LEG_BUDGET_SECONDS = (\d+)$", script, re.M)
    check(
        budget is not None
        and int(budget.group(1)) > 0
        and "except subprocess.TimeoutExpired as expired:" in script,
        "挂住那一腿按正数预算超时，且超时处被接住而不是抛成 traceback",
        f"预算 = {budget.group(1) if budget else '没钉'} / 超时捕获 "
        f"{'在' if 'except subprocess.TimeoutExpired as expired:' in script else '不在'}"
        "（预算为 0 会让每一腿当场超时；没接住则 CI 读到的是崩溃而不是验收失败）",
    )
    check(
        "raise SystemExit(" in script
        and "sys.exit(EXIT_FAILURE)" in script
        and '"exit_code": None' in script,
        "超时按失败退出码收口并先写进阶段记录，结果包里的挂死有可核对的那一格",
        "抛出、退出码或阶段记录缺一：挂住的那腿就变成 pass 或无声消失",
    )
    base = ROOT / "deploy" / constants.get("BASE_CONFIG", "")
    check(base.is_file(), "验收配置 BASE_CONFIG 指向仓库里的真文件", f"缺 {base}")
    if not base.is_file():
        return
    config_text = base.read_text(encoding="utf-8")
    config = json.loads(config_text)
    workers = {str(worker.get("id")): worker for worker in config.get("workers", [])}
    orphan = [
        name
        for key in ("EXECUTION_WORKER", "RECONCILE_WORKER")
        for name in [constants.get(key, "")]
        if not workers.get(name, {}).get("enabled")
    ]
    check(
        not orphan,
        "验收脚本点名的 worker 在验收配置里存在且已启用",
        f"禁用或缺失 {orphan}",
    )
    declared_env = set(re.findall(r'"(QX_[A-Z0-9_]+)"', config_text))
    checked_env = {constants.get("KEY_ENV", ""), constants.get("SECRET_ENV", "")}
    check(
        bool(declared_env) and declared_env == checked_env,
        "验收配置的凭据引用与脚本检查的环境变量是同一组名字",
        f"只在配置 {sorted(declared_env - checked_env) or '无'} / 只在脚本 "
        f"{sorted(checked_env - declared_env) or '无'}（多出来的引用会让脚本以为凭据齐了）",
    )

    # `--venue okx`：choices 里少了 okx，或 CCXT 这一组常量/配置文件/worker 缺一个，
    # 「第二交易所沙盒已通」就变成没有支点的表述。这里按与 Binance 同一口径逐格核对。
    okx_constants = dict(ACCEPTANCE_CONSTANT_OKX.findall(script))
    okx_expected = {
        "OKX_BASE_CONFIG",
        "OKX_CCXT_CONFIG",
        "OKX_EXECUTION_WORKER",
        "OKX_RECONCILE_WORKER",
        "OKX_KEY_ENV",
        "OKX_SECRET_ENV",
        "OKX_PASS_ENV",
    }
    okx_choices = re.search(r'choices=\[([^\]]*)\]', script)
    okx_choices_ok = okx_choices is not None and '"okx"' in okx_choices.group(1)
    check(
        set(okx_constants) == okx_expected and okx_choices_ok,
        "第二交易所 `--venue okx` 的常数组齐备且 choices 里真有 okx（不复制脚本而是参数化）",
        f"缺常量 {sorted(okx_expected - set(okx_constants)) or '无'} / 多出 "
        f"{sorted(set(okx_constants) - okx_expected) or '无'} / choices 含 okx="
        f"{okx_choices_ok}",
    )
    okx_file_gaps = [
        name
        for name in ("OKX_BASE_CONFIG", "OKX_CCXT_CONFIG")
        if not (ROOT / "deploy" / okx_constants.get(name, "")).is_file()
    ]
    check(
        not okx_file_gaps,
        "okx 验收的配置与 CCXT 配置文件都指向仓库里的真文件",
        f"缺 {[okx_constants.get(name, '') for name in okx_file_gaps]}",
    )
    okx_base = ROOT / "deploy" / okx_constants.get("OKX_BASE_CONFIG", "")
    okx_ccxt = ROOT / "deploy" / okx_constants.get("OKX_CCXT_CONFIG", "")
    if okx_ccxt.is_file():
        ccxt_text = okx_ccxt.read_text(encoding="utf-8")
        ccxt_declared = set(re.findall(r'"(QX_[A-Z0-9_]+)"', ccxt_text))
        ccxt_checked = {
            okx_constants.get("OKX_KEY_ENV", ""),
            okx_constants.get("OKX_SECRET_ENV", ""),
            okx_constants.get("OKX_PASS_ENV", ""),
        }
        check(
            bool(ccxt_declared) and ccxt_declared == ccxt_checked,
            "CCXT 配置的凭据引用与脚本检查的环境变量是同一组名字（同 Binance 那一组口径）",
            f"只在配置 {sorted(ccxt_declared - ccxt_checked) or '无'} / 只在脚本 "
            f"{sorted(ccxt_checked - ccxt_declared) or '无'}",
        )
    if okx_base.is_file():
        okx_workers = {
            str(worker.get("id")): worker for worker in json.loads(okx_base.read_text(encoding="utf-8")).get("workers", [])
        }
        okx_orphan = [
            name
            for key in ("OKX_EXECUTION_WORKER", "OKX_RECONCILE_WORKER")
            for name in [okx_constants.get(key, "")]
            if not okx_workers.get(name, {}).get("enabled")
        ]
        check(
            not okx_orphan,
            "okx 验收脚本点名的 worker 在 okx 验收配置里存在且已启用",
            f"禁用或缺失 {okx_orphan}",
        )


# —— T0-3：外部链路（实盘轨）的边界登记 ——
# 实盘轨「能翻到哪一档、凭什么翻、今天翻没翻」此前散在三处散文里：方案 §2 的五段表、
# `maturity/evidence/README.md` 的四档说明、以及验收脚本写进 `result.json` 的那句规则。
# 散在散文里的口径没人对账，于是同一件事的三种说法可以各说各的——实测就有一处：方案 §4 写
# 「除 live-check 之外全部阶段退出码为 0」，而脚本写进结果包的是「全部阶段退出码为 0」，
# 方案自己声称「脚本与文档同口径」而门禁里一条判据都没核过它。本族把这张边界登记成机读表
# 并逐格对账，同时守住那条最贵的界线：**代码在盘（implementation）不是生产已批准**。
EXTERNAL_CHAIN_FILE = "maturity/external_chain.yaml"
EXTERNAL_CHAIN_KIND = "external-chain-boundary"
EXTERNAL_CHAIN_TOP = (
    "schema_version",
    "kind",
    "updated_at",
    "evidence_root",
    "sandbox_evidence_dir",
    "production_evidence_dir",
    "acceptance_script",
    "acceptance_plan",
    "capabilities_registry",
    "flip_rule",
    "second_venue",
)
EXTERNAL_CHAIN_TIERS = (
    "implementation",
    "code_tested",
    "sandbox_tested",
    "production_approved",
)
# 由源码 / 本地测试翻真的两档：它们的翻真依据里出现证据记录，就等于把"代码在盘"读成了"已批准"。
EXTERNAL_CHAIN_CODE_TIERS = ("implementation", "code_tested")
# 翻转规则的唯一措辞（方案里给关键词加了反引号，比对前先剥掉）。
EXTERNAL_CHAIN_FLIP_RULE = "outcome=pass 且除 live-check 之外全部阶段退出码为 0"
# 脚本里那两处**代码**：live-check 豁免 + 只有 pass 才 allowed。只改文字不改代码要能红，
# 否则"同口径"就只是两句碰巧一样的话，而不是同一条规则。
EXTERNAL_CHAIN_FLIP_GUARD = 'record["stage"] != "live-check" and record["exit_code"] != 0'
EXTERNAL_CHAIN_FLIP_ALLOWED = '"allowed": outcome == "pass",'
EXTERNAL_CHAIN_SECOND_ENTRY = "ccxt-submit-order"


def _unquote(value: str) -> str:
    """剥掉 YAML 里成对的引号（登记面给带 `: ` 的标量加了引号，比对前要还原）。"""
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "'\"":
        return value[1:-1]
    return value


def _parse_external_chain(
    text: str,
) -> tuple[dict[str, str], dict[str, dict[str, str]], list[dict[str, str]]]:
    """逐行解析 external_chain.yaml：顶层标量 + `tiers` 映射 + `segments` 列表。

    顶层标量写进 `scalars`（`current_status` 段带前缀），块标量 `>-` 的缩进正文自然被略过。
    """
    scalars: dict[str, str] = {}
    tiers: dict[str, dict[str, str]] = {}
    segments: list[dict[str, str]] = []
    section: str | None = None
    current: dict[str, str] | None = None
    for raw in text.splitlines():
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        if m := re.match(r"^(tiers|segments|current_status):\s*$", raw):
            section, current = m.group(1), None
            continue
        if re.match(r"^\S", raw):  # 顶层标量（含块标量头）
            section, current = None, None
            if m := re.match(r"^([\w.-]+):\s*(.+?)\s*$", raw):
                scalars[m.group(1)] = _unquote(m.group(2))
            continue
        if section in ("tiers", "segments"):
            if m := re.match(r"^  - name:\s*(.+?)\s*$", raw):
                current = {"name": m.group(1)}
                if section == "tiers":
                    tiers[m.group(1)] = current
                else:
                    segments.append(current)
            elif current is not None and (m := re.match(r"^    ([\w-]+):\s*(.*?)\s*$", raw)):
                current[m.group(1)] = m.group(2)
        elif section == "current_status":
            if m := re.match(r"^  ([\w-]+):\s*(.+?)\s*$", raw):
                scalars[f"current_status.{m.group(1)}"] = m.group(2)
    return scalars, tiers, segments


def _evidence_outcome_tally(root: str) -> dict[str, int]:
    """按盘上逐份 `result.json` 数 outcome。读不动的记成 `unreadable`，不静默跳过。"""
    tally: dict[str, int] = {}
    base = ROOT / root
    if not base.is_dir():
        return tally
    for path in sorted(base.rglob("result.json")):
        try:
            outcome = json.loads(path.read_text(encoding="utf-8")).get("outcome")
        except (json.JSONDecodeError, OSError):
            outcome = "unreadable"
        key = str(outcome)
        tally[key] = tally.get(key, 0) + 1
    return tally


def _plan_segment_names(plan: str) -> list[str]:
    """取方案 §2 那张表的第一列（段名），按行序；分隔行与表头不算。"""
    names: list[str] = []
    inside = False
    for line in plan.splitlines():
        if line.startswith("| 段 | 命令 | 产出 | 失败即 |"):
            inside = True
            continue
        if inside:
            if not line.startswith("|"):
                break
            if set(line) <= set("|-: "):
                continue
            cells = [cell.strip() for cell in line.strip("|").split("|")]
            if len(cells) == 4:
                names.append(cells[0])
    return names


def _capability_tier_true_count(field: str) -> int:
    """数 `maturity/capabilities.yaml` 里某一档为 true 的条目数（profiles 段没有这些键）。"""
    text = (ROOT / "maturity/capabilities.yaml").read_text(encoding="utf-8")
    return len(re.findall(rf"^    {field}: true\s*$", text, re.M))


def external_chain_check() -> None:
    """T0-3：把实盘轨的边界（五段 / 翻转规则 / 证据根 / 现状）登记成一张机读表并逐格对账。

    七颗分别钉：
    ① 登记面在盘、可解析、`kind` 自述正确、顶层键齐、四档与五段都在；
    ② 五段与方案 §2 那张表**逐行同名同序**，且每段都写了「失败即」口径；
    ③ 翻转规则三处同源：本文件 / 方案 §4 / 验收脚本写进 `result.json` 的那句；
    ④ 脚本的**代码**真实现了那条规则（live-check 豁免 + 只有 pass 才 allowed）；
    ⑤ 证据根现状与盘上逐份 `result.json` 一致，且 `flip_allowed` 恰等于「存在 outcome=pass」；
    ⑥ 两档计数与 `capabilities.yaml` 逐值相等；**没有 pass 记录时两档必须全为 false**；
       且前两档的翻真依据里不出现证据记录——"代码在盘"永远翻不成"生产已批准"；
    ⑦ 第二交易所的 `--venue` 值在脚本 choices 里，且该下单入口在命令面上是 L 档且默认关闭。
    """
    path = ROOT / EXTERNAL_CHAIN_FILE
    text = path.read_text(encoding="utf-8") if path.is_file() else ""
    scalars, tiers, segments = _parse_external_chain(text)
    missing = [key for key in EXTERNAL_CHAIN_TOP if key not in scalars]
    check(
        path.is_file()
        and scalars.get("kind") == EXTERNAL_CHAIN_KIND
        and not missing
        and scalars.get("acceptance_plan") == ACCEPTANCE_DOC
        and scalars.get("acceptance_script") == ACCEPTANCE_SCRIPT
        and sorted(tiers) == sorted(EXTERNAL_CHAIN_TIERS)
        and len(segments) == 5
        and all(seg.get("fail_closed") for seg in segments),
        "外链边界登记在盘、kind 自述正确、顶层键齐、四档与五段都在（每段带失败即口径）",
        f"缺 {EXTERNAL_CHAIN_FILE} / 缺格 {missing} / 档 {sorted(tiers)} / 段 {len(segments)} / "
        f"方案指向 {scalars.get('acceptance_plan')} / 脚本指向 {scalars.get('acceptance_script')}",
    )
    plan = (ROOT / ACCEPTANCE_DOC).read_text(encoding="utf-8")
    planned = _plan_segment_names(plan)
    declared = [seg.get("name", "") for seg in segments]
    check(
        bool(planned) and declared == planned,
        f"五段与方案 §2 的表逐行同名同序（方案 {len(planned)} 段，登记 {len(declared)} 段）",
        f"方案 {planned} vs 登记 {declared}",
    )
    script = (ROOT / ACCEPTANCE_SCRIPT).read_text(encoding="utf-8")
    in_plan = EXTERNAL_CHAIN_FLIP_RULE in plan.replace("`", "")
    in_script = EXTERNAL_CHAIN_FLIP_RULE in script
    check(
        scalars.get("flip_rule") == EXTERNAL_CHAIN_FLIP_RULE and in_plan and in_script,
        "翻转规则三处同源：登记面 / 方案 §4 / 验收脚本写进结果包的那句（剥反引号后逐字相等）",
        f"登记面 {'同' if scalars.get('flip_rule') == EXTERNAL_CHAIN_FLIP_RULE else '不同'} / "
        f"方案 {'在' if in_plan else '不在'} / 脚本 {'在' if in_script else '不在'}",
    )
    check(
        EXTERNAL_CHAIN_FLIP_GUARD in script and EXTERNAL_CHAIN_FLIP_ALLOWED in script,
        "验收脚本的代码真实现了这条规则（live-check 豁免 + 只有 pass 才 allowed），不只是文字",
        f"豁免臂 {'在' if EXTERNAL_CHAIN_FLIP_GUARD in script else '不在'} / "
        f"allowed {'在' if EXTERNAL_CHAIN_FLIP_ALLOWED in script else '不在'}",
    )
    root = scalars.get("evidence_root", "")
    tally = _evidence_outcome_tally(root)
    recorded = ",".join(f"{name}:{count}" for name, count in sorted(tally.items()))
    total = sum(tally.values())
    allowed = tally.get("pass", 0) > 0
    check(
        bool(root)
        and (ROOT / root).is_dir()
        and scalars.get("current_status.evidence_records") == str(total)
        and scalars.get("current_status.outcomes") == recorded
        and scalars.get("current_status.sandbox_tested_flip_allowed") == str(allowed).lower(),
        "证据根现状与盘上逐份 result.json 一致，且 flip_allowed 恰等于「存在 outcome=pass」",
        f"盘上 {total} 份 / {recorded}（flip_allowed={allowed}）vs 登记 "
        f"{scalars.get('current_status.evidence_records')} 份 / "
        f"{scalars.get('current_status.outcomes')} / "
        f"{scalars.get('current_status.sandbox_tested_flip_allowed')}",
    )
    sandbox_true = _capability_tier_true_count("sandbox_tested")
    production_true = _capability_tier_true_count("production_approved")
    code_clean = all(
        "outcome" not in tiers.get(name, {}).get("flipped_by", "")
        and "evidence" not in tiers.get(name, {}).get("flipped_by", "")
        for name in EXTERNAL_CHAIN_CODE_TIERS
    )
    check(
        scalars.get("current_status.sandbox_tested_true_count") == str(sandbox_true)
        and scalars.get("current_status.production_approved_true_count") == str(production_true)
        and (sandbox_true + production_true == 0 or allowed)
        and code_clean,
        "两档计数与 capabilities.yaml 逐值相等；没有 pass 记录时两档必须全为 false；"
        "前两档的翻真依据里不出现证据记录（代码在盘 ≠ 生产已批准）",
        f"盘上 sandbox={sandbox_true} / production={production_true}；存在 pass 记录={allowed}；"
        f"前两档 flipped_by 干净={code_clean}",
    )
    venue = scalars.get("second_venue", "")
    choices = re.search(r"choices=\[([^\]]*)\]", script)
    venue_ok = bool(venue) and choices is not None and f'"{venue}"' in choices.group(1)
    surface = _parse_command_surface(
        (ROOT / COMMAND_SURFACE_FILE).read_text(encoding="utf-8")
    )
    entry = next(
        (row for row in surface if row.get("name") == EXTERNAL_CHAIN_SECOND_ENTRY), None
    )
    check(
        venue_ok
        and entry is not None
        and entry.get("level") == "L"
        and entry.get("default_enabled") == "false",
        "第二交易所的 --venue 值在脚本 choices 里，且该下单入口在命令面上是 L 档且默认关闭",
        f"venue={venue or '没登记'} choices 含它={venue_ok} / {EXTERNAL_CHAIN_SECOND_ENTRY} "
        f"档位={entry.get('level') if entry else '不在册'} "
        f"默认开启={entry.get('default_enabled') if entry else '-'}",
    )


# V13 R1-D：三处"只进不出"的常驻内存与两处生命周期无界，收口后的形状。
# 这四颗在真进程上都不是确定性的（跑几十天的 API 进程、不肯退出的子进程、只追加不退场
# 的审计段、一问一答却攒着迟到应答的泵），所以判据钉的是形状：上界定义在哪、谁负责
# 退场、退场时点不点名、预算从配置一路接到哪一行。
SNAPSHOT_HISTORY_FILE = "crates/qx-api/src/snapshot_history.rs"
API_ROOT_FILE = "crates/qx-api/src/lib.rs"
REAP_FILE = "crates/qx-orchestrator/src/reap.rs"
ORCHESTRATOR_ROOT_FILE = "crates/qx-orchestrator/src/lib.rs"
CCXT_PUMP_FILE = "crates/qx-adapter/src/ccxt.rs"
BINANCE_VENUE_FILE = "crates/qx-adapter/src/binance.rs"
PIPELINE_LEDGER_FILE = "crates/qx-runtime/src/pipeline.rs"
# P0-2(a)：写面提交（含两条 ingest 路径）从 pipeline.rs 切到子模块，台账 census 必须跟着走。
PIPELINE_COMMIT_FILE = "crates/qx-runtime/src/pipeline/commit.rs"
CONTROL_PLANE_FILE = "crates/qx-control/src/lib.rs"
SNAPSHOT_HISTORY_CASES = (
    "republishing_the_same_snapshot_never_grows_the_window",
    "republishing_keeps_the_live_baseline_at_the_newest_end",
    "the_window_holds_the_newest_snapshots_and_forgets_the_oldest",
)
REAP_CASES = (
    "nothing_to_reap_is_done_without_consuming_the_budget",
    "a_fully_reaped_roster_wins_over_the_expired_budget",
    "the_budget_expires_exactly_on_the_dot",
    "waiting_never_sleeps_past_the_remaining_budget",
    "the_timeout_message_names_every_straggler",
    "a_real_child_is_killed_and_reaped_inside_the_budget",
    "reaping_an_empty_roster_is_ok",
)
# V13 R1-H（重落 V11 R6-1）：`CommandStatus` 曾经带一个 `Rejected` 变体，全仓零构造者——
# 拒绝发生在受理之前，`submit`/`submit_as` 校验不过就返回 `ControlError`，命令与审计记录
# 都不落盘，所以「已拒绝」从来不是一个能被写出来的审计状态。零构造者的变体让每个 match
# 都多一条永不为真的臂，读代码的人还得逐个去证它走不到；删掉之后终态词表只许出现一次。
COMMAND_FINALITY_BODY = "matches!(self, Self::Executed | Self::Failed)"
SPREAD_IDEMPOTENCY_FILE = "crates/qx-cli/src/spread.rs"


def bounded_growth_and_reap_check() -> None:
    """R1-D 的四颗：API 快照历史有界、监督器收尾按预算、CCXT 泵通道有界、控制面待办不回扫。

    每一颗都对应一个"跑久了才发作"的形状，用例侧只能钉住可判定的那一半（纯函数与真子
    进程各一条），另一半——上界本身与预算接线——由这里守着。
    """
    history = (ROOT / SNAPSHOT_HISTORY_FILE).read_text(encoding="utf-8")
    history_code = production_text(history)
    api_root = (ROOT / API_ROOT_FILE).read_text(encoding="utf-8")
    # 退场门槛必须**逐字**是那个上界：末尾带上 `{` 才钉得住。少了它，`> MAX_SNAPSHOT_HISTORY`
    # 是 `> MAX_SNAPSHOT_HISTORY * 8 {` 的前缀，宣称 1_024 而实际常驻 8_192 也全绿（H1 实测）。
    eviction_header = "while self.by_hash.len() > MAX_SNAPSHOT_HISTORY {"
    scaled_cap = re.findall(r"MAX_SNAPSHOT_HISTORY\s*[*+\-/]", history_code)
    check(
        history_code.count("pub(super) const MAX_SNAPSHOT_HISTORY: usize = 1_024;") == 1
        and history_code.count(eviction_header) == 1
        and not scaled_cap
        and history_code.count("self.insertion.pop_front()") == 1
        and api_root.count("snapshot_history: SnapshotHistory,") == 2
        and api_root.count("BTreeMap<u64, AccountSnapshot>") == 0
        and api_root.count("mod snapshot_history;") == 1,
        "API 快照历史是一张封顶表，两处读模型都用它而不是只进不出的裸 map（V13 R1-D）",
        f"上界定义 {history_code.count('pub(super) const MAX_SNAPSHOT_HISTORY: usize = 1_024;')} 处、"
        f"退场循环 {history_code.count(eviction_header)} 处、"
        f"队首淘汰 {history_code.count('self.insertion.pop_front()')} 处（各期望 1）；"
        f"对上界做算术 {scaled_cap or '无'}（期望无：退场门槛一旦被乘除，宣称的份数与实际常驻的"
        "份数就不是同一个数，内存照着倍数长而判据全绿）；"
        f"lib.rs 里换成 SnapshotHistory 的字段 {api_root.count('snapshot_history: SnapshotHistory,')} 处"
        f"（期望 2：全局那份与按账户投影那份），残留的裸 map 写法 "
        f"{api_root.count('BTreeMap<u64, AccountSnapshot>')} 处（期望 0）",
    )
    check(
        -1 < history_code.find("position(|stored| *stored == hash)")
        < history_code.find("push_back(hash)")
        and history.count("#[test]") == len(SNAPSHOT_HISTORY_CASES)
        and all(f"fn {case}(" in history for case in SNAPSHOT_HISTORY_CASES),
        "重装同一份摘要把它挪回队尾，且三条窗口用例在册（V13 R1-D）",
        f"挪位 {history_code.find('position(|stored| *stored == hash)')} / 入队 "
        f"{history_code.find('push_back(hash)')}（前者必须在后者之前，否则安静账户里唯一那份"
        "基线永远排在队首，下一轮波动就退掉客户端正在用的那一格）；"
        f"#[test] {history.count('#[test]')} 条，在册 "
        f"{[case for case in SNAPSHOT_HISTORY_CASES if f'fn {case}(' in history]}",
    )

    supervisor = production_text((ROOT / ORCHESTRATOR_ROOT_FILE).read_text(encoding="utf-8"))
    reap_text = (ROOT / REAP_FILE).read_text(encoding="utf-8")
    reap_code = production_text(reap_text)
    check(
        supervisor.count("fn stop_managed_children") == 0
        and reap_code.count("fn stop_managed_children") == 1
        and "child.wait()" not in supervisor + reap_code
        and (supervisor + reap_code).count("try_wait()") == 2
        and reap_code.count(".wait()") == 0,
        "托管收尾只有一处实现，按 try_wait 轮询而不是无条件 wait（V13 R1-D）",
        f"定义点 {supervisor.count('fn stop_managed_children')} + "
        f"{reap_code.count('fn stop_managed_children')} 处（期望 0 + 1，搬家不能留下第二份）；"
        f"`child.wait()` 残留 {supervisor.count('child.wait()') + reap_code.count('child.wait()')} 处"
        f"（期望 0）、裸 `.wait()` {reap_code.count('.wait()')} 处（期望 0）——那个调用会一直堵到"
        "进程真的消失，父进程可以在这里停到天荒地老",
    )
    supervise = _fn_body(supervisor, "pub fn supervise_workers(")
    check(
        "Duration::from_millis(config.shutdown_timeout_ms)" in supervise
        and supervisor.count("(Err(error), Err(reap_error))") == 1
        and -1 < reap_code.find("!alive.iter().any(") < reap_code.find("elapsed >= budget")
        and reap_code.count("budget.saturating_sub(elapsed).min(REAP_POLL_INTERVAL)") == 1
        and reap_code.count("pub(crate) fn reap_round") == 1
        and reap_code.count("ReapDecision::TimedOut") == 2
        and reap_code.count("reap_timeout_report(&stragglers, budget)") == 1
        and reap_code.count('stragglers.join(", ")') == 1,
        "收尾预算从配置一路接到回收处，轮询有界性是一条可判定的算术，超预算的 worker 被点名（V13 R1-D）",
        f"预算接线在 supervise_workers 里={'Duration::from_millis(config.shutdown_timeout_ms)' in supervise}；"
        f"回收失败与业务失败合并那一臂 {supervisor.count('(Err(error), Err(reap_error))')} 处（期望 1，"
        "少了它收尾报错会被丢掉，父进程带着孤儿静静退出）；"
        f"判序 存活={reap_code.find('!alive.iter().any(')} 预算={reap_code.find('elapsed >= budget')}"
        "（先判收工再判超时，否则最后一轮全部收工的正常收尾会被报成超时）；"
        f"步长塞进剩余预算 {reap_code.count('budget.saturating_sub(elapsed).min(REAP_POLL_INTERVAL)')} 处"
        f"（期望 1，否则 1 ms 的预算也会先睡 50 ms）；点名 {reap_code.count('reap_timeout_report(&stragglers, budget)')} 处",
    )
    check(
        reap_text.count("#[test]") == len(REAP_CASES)
        and all(f"fn {case}(" in reap_text for case in REAP_CASES),
        "回收判据逐条有常驻用例，含一条真子进程（V13 R1-D）",
        f"#[test] {reap_text.count('#[test]')} 条（期望 {len(REAP_CASES)}），在册 "
        f"{[case for case in REAP_CASES if f'fn {case}(' in reap_text]}",
    )

    pump = production_text((ROOT / CCXT_PUMP_FILE).read_text(encoding="utf-8"))
    host_pump = production_text(
        (CRATES / "qx-cli/src/strategy_host.rs").read_text(encoding="utf-8")
    )
    check(
        pump.count("mpsc::sync_channel(1)") == 1
        and pump.count("mpsc::channel()") == 0
        and host_pump.count("let (sender, responses) = mpsc::sync_channel(1);") == 1
        and pump.count("read_capped_line(&mut reader, crate::MAX_WORKER_LINE_BYTES)") == 1
        and host_pump.count("read_capped_worker_line(&mut reader, DEFAULT_MAX_FRAME_BYTES)") == 2
        and pump.count(".read_line(") == 0,
        "CCXT 与策略 worker 子进程 stdout 的泵通道队列有界、每行读取带字节上限（V13 R1-D / R2 / R5 / R4-A）",
        f"CCXT 有界通道 {pump.count('mpsc::sync_channel(1)')} 处（期望 1：stdout 泵；写侧回话通道 R17 已并入 "
        "io_budget 的 write_all_within，不再各留一条）、无界写法 "
        f"{pump.count('mpsc::channel()')} 处（期望 0）；策略 worker 那条 "
        f"{host_pump.count('let (sender, responses) = mpsc::sync_channel(1);')} 处（期望 1）。"
        "两处泵线程每读一行/一帧就往通道里塞，而调用侧每次只取一条：无界意味着 Worker 的杂印与"
        "上一轮迟到的应答会一路攒下去，内存随运行时长增长。"
        f"行长闸：CCXT {pump.count('read_capped_line(&mut reader, crate::MAX_WORKER_LINE_BYTES)')} 处"
        f"（期望 1）、旧 read_line {pump.count('.read_line(')} 处（期望 0）、"
        f"策略 worker {host_pump.count('read_capped_worker_line(&mut reader, DEFAULT_MAX_FRAME_BYTES)')} 处"
        "（期望 2：stderr 诊断与 stdout 应答）——队列有界只卡住条数，一字节不换行的一行仍能把"
        "本进程吃到内存耗尽，因为 lines()/read_line 只在 EOF 或 io 错误处停",
    )

    # V13 R5 / R6-A → R17 fam04：成交幂等台账的退场只允许经由 venue 缓存的**终态封顶**。
    # user stream / `trades` 查询 / reconcile 都会把同一笔成交重投，淘汰一条已见键就等于允许
    # 同一笔 fill 被 trace 两次——重复记账，本仓库唯一不接受的失败方式。R17 按「最新版规划」
    # 恢复 fam04 的 venue 缓存封顶（crates/qx-adapter/src/venue_cache.rs）之后，两个适配器的
    # 去重台账与订单表一起按 client_id 升序退场（只退终态订单），安全性由"订单一并退场 ⇒ 重投
    # 的成交落到『本地订单不存在』分支"兜底（两个适配器各有一条具名出口，逐字钉住）；运行期
    # `LiveEventPipeline.seen_fills` 没有共用这把封顶，仍只增不减。判据因此从"一律不许淘汰"
    # 改成"退场只许走终态封顶这一条唯一通道，且两个适配器都必须留着『订单已退场』的出口"。
    binance_raw = (ROOT / BINANCE_VENUE_FILE).read_text(encoding="utf-8")
    ccxt_raw = (ROOT / CCXT_PUMP_FILE).read_text(encoding="utf-8")
    pipeline_raw = (ROOT / PIPELINE_LEDGER_FILE).read_text(encoding="utf-8")
    # 写面提交（两条 ingest 路径）在 P0-2(a) 之后住在 commit.rs，census 两处一起数。
    pipeline_code = production_text(pipeline_raw) + production_text(
        (ROOT / PIPELINE_COMMIT_FILE).read_text(encoding="utf-8")
    )
    binance = production_text(binance_raw)
    ccxt = production_text(ccxt_raw)
    DEDUP_EVICTION = re.compile(
        r"seen_(?:fill_keys|trade_ids|fills)\s*\.\s*(?:remove|retain|drain|pop|clear|truncate)\b"
    )
    DEDUP_NOTE = "只增不减的成交幂等台账"
    check(
        binance.count("seen_fill_keys.insert") == 1
        and ccxt.count("seen_trade_ids.entry") == 1
        and pipeline_code.count("seen_fills.insert") == 3
        # 两个适配器的去重台账各只有一处退场，且都由 `forget_orders` 承载。
        and len(DEDUP_EVICTION.findall(binance)) == 1
        and len(DEDUP_EVICTION.findall(ccxt)) == 1
        and "fn forget_orders(&mut self, evicted: &BTreeSet<u64>)" in binance
        and "fn forget_orders(&mut self, evicted: &BTreeSet<u64>)" in ccxt
        # 退场只能由封顶驱动：每处订单写入点都紧跟着一次封顶 + 一次级联。
        and binance.count("let evicted = evict_stale_terminal_orders(&mut self.orders);") == 3
        and binance.count("self.forget_orders(&evicted);") == 3
        and ccxt.count("let evicted = evict_stale_terminal_orders(&mut self.orders);") == 2
        and ccxt.count("self.forget_orders(&evicted);") == 2
        # 两个适配器都留着「订单已退场」的出口，退场不会把回报静默吞掉。
        and 'QxError::ReconcileRequired("Binance 成交对应订单不存在".into())' in binance
        and "CCXT 本地订单不存在" in ccxt
        # 运行期台账没有共用这把封顶，仍只增不减。
        and not DEDUP_EVICTION.search(pipeline_code)
        and pipeline_raw.count(DEDUP_NOTE) == 1,
        "成交幂等台账的退场只经由 venue 缓存的终态封顶，且两个适配器都留着『订单已退场』的出口（V13 R5 / R6-A / R17）",
        f"Binance 入账点 {binance.count('seen_fill_keys.insert')}（期望 1）、"
        f"CCXT 入账点 {ccxt.count('seen_trade_ids.entry')}（期望 1）、"
        f"运行期台账入账点 {pipeline_code.count('seen_fills.insert')}（期望 3：两条 ingest 路径各一处、"
        "重启时把整本 EventLog 的 Filled 全插回来一处）；"
        f"去重台账退场调用 Binance={len(DEDUP_EVICTION.findall(binance))} "
        f"CCXT={len(DEDUP_EVICTION.findall(ccxt))}（各期望 1，且只在 forget_orders 里）；"
        f"封顶调用 Binance={binance.count('let evicted = evict_stale_terminal_orders(&mut self.orders);')}"
        f" CCXT={ccxt.count('let evicted = evict_stale_terminal_orders(&mut self.orders);')}；"
        f"运行期台账退场={bool(DEDUP_EVICTION.search(pipeline_code))}（期望 False，仍只增不减）"
        "——退场资格由 venue_cache 的终态筛选把住（只退终态订单），被退订单再收到回报会落到"
        "各适配器既有的『本地订单不存在』出口，不会重复记账；谁把退场挪出 forget_orders 或"
        "摘掉那两个出口，重复记账的口子就开回来了",
    )

    # V13 R6-A：API 投影桥的两条投影失败都是内容漂移或序号缺口这类**不会自愈**的冲突。
    # 退场必须记进一份永久名单才是终局：只清当轮的 `pipelines` map 项，外层 `while` 下一轮
    # 到这里是 Vacant，会重开同一本账本、再投一遍、再刷同一行错误日志（每 250 ms 一次）。
    # R6-A 第一次落地时只做了 `pipelines.remove`，语义与它自己的注释不符。同时守提交顺序：
    # 按账户投影必须先落地先判完，否则它失败时全局那格已经推进，同一个账户的 `/events` 与
    # `/events?account_id=&venue_id=` 从此各讲一份。
    bridges_raw = (ROOT / API_PROJECTION_BRIDGE_FILE).read_text(encoding="utf-8")
    bridges = production_text(bridges_raw)
    account_idx = bridges.find("service.project_account_event_log(")
    global_idx = bridges.find("service.project_event_log(")
    check(
        bridges.count(
            "let mut permanently_retired = BTreeSet::<(String, String)>::new();"
        )
        == 1
        and bridges.count("permanently_retired.insert(pipeline_key.clone())") == 2
        and bridges.count("permanently_retired.contains(&pipeline_key)") == 1
        and account_idx != -1
        and global_idx != -1
        and account_idx < global_idx,
        "API 投影桥的投影失败永久退场不重试，且按账户投影先于全局投影提交（V13 R6-A / R7）",
        f"永久退场名单定义 {bridges.count('let mut permanently_retired = BTreeSet::<(String, String)>::new();')} 处（期望 1）、"
        f"入名单点 {bridges.count('permanently_retired.insert(pipeline_key.clone())')} 处（期望 2：两条投影失败各一）、"
        f"循环跳过点 {bridges.count('permanently_retired.contains(&pipeline_key)')} 处（期望 1）；"
        f"提交顺序 account@{account_idx} < global@{global_idx} = {account_idx != -1 and global_idx != -1 and account_idx < global_idx}"
        "（期望 True）——`refresh()` 失败那一路**不**记入名单是刻意的：读盘失败可能是瞬时 IO，"
        "下一轮重开重试才正确；而投影失败是账本内容本身冲突，重试只会无限刷屏",
    )

    control = (ROOT / CONTROL_PLANE_FILE).read_text(encoding="utf-8")
    control_code = production_text(control)
    pending_body = _fn_body(control_code, "pub fn pending(&self)")
    check(
        control_code.count("fn finalized_command_ids(&self) -> BTreeSet<u64>") == 1
        and "self.finalized_command_ids()" in pending_body
        and "self.audit.iter().rev().any(" not in control_code
        and control_code.count("pub const fn is_final(self) -> bool") == 1
        and control_code.count("record.status.is_final()") == 4
        and control_code.count("if prior.is_final() {") == 1,
        "控制面待办一次算出终态集合，不再逐条命令回扫整段只追加的审计（V13 R1-D）",
        f"终态集合定义 {control_code.count('fn finalized_command_ids(&self) -> BTreeSet<u64>')} 处（期望 1）、"
        f"pending 里调用={'self.finalized_command_ids()' in pending_body}、"
        f"旧的逐条回扫残留 {control_code.count('self.audit.iter().rev().any(')} 处（期望 0）；"
        f"表达式位置的终态判据 is_final 定义 {control_code.count('pub const fn is_final(self) -> bool')} 处（期望 1）、"
        f"调用点 {control_code.count('record.status.is_final()')} / {control_code.count('if prior.is_final() {')} 处"
        f"（期望 4 / 1：终态集合、退场排序、恢复校验的两条守卫，以及 execute 的 AlreadyFinal）",
    )

    # 死信台账与成交幂等台账同形：它是**只追加**的。三条后端（文件 / SQLite / Postgres）都只有
    # INSERT 与 SELECT，而重投递的幂等键是 `{event_id}:replay:{attempts}`（attempts 归零后重投）——
    # 删掉一行死信就等于允许同一事件在同一个 attempts 上再投一次，那是「重复记账」的另一张脸。
    # 所以这里守的是「不得出现淘汰调用」，不是「有界」（V13 R25 二轮；capabilities.yaml 的
    # `consumer_state_processed_ids_and_dead_letters_grow_without_retention` 记的是同一件事的另一半）。
    dead_letter_sources = {
        rel: (ROOT / rel).read_text(encoding="utf-8")
        for rel in (
            "crates/qx-storage/src/sqlite.rs",
            "crates/qx-storage/src/postgres.rs",
            "crates/qx-storage/src/file/consumers.rs",
        )
    }
    # 两条 SQL 后端的台账是一张表，文件后端的台账是消费者状态里的一个 `dead_letters` 数组：
    # 名字不同，「只追加」这一条性质相同。
    dead_letter_faces = {
        "crates/qx-storage/src/sqlite.rs": "qx_consumer_dead_letters",
        "crates/qx-storage/src/postgres.rs": "qx_consumer_dead_letters",
        "crates/qx-storage/src/file/consumers.rs": "dead_letters",
    }
    dead_letter_eviction = re.compile(
        r"DELETE FROM qx_consumer_dead_letters"
        r"|dead_letters\s*\.\s*(?:remove|retain|drain|pop|clear|truncate)\b"
    )
    evicting = sorted(
        rel for rel, text in dead_letter_sources.items() if dead_letter_eviction.search(text)
    )
    replay_code = production_text(
        (ROOT / "crates/qx-cli/src/event_pipeline.rs").read_text(encoding="utf-8")
    )
    check(
        len(dead_letter_sources) == 3
        and all(face in dead_letter_sources[rel] for rel, face in dead_letter_faces.items())
        and not evicting
        and 'format!("{}:replay:{}", replay.event_id, record.attempts)' in replay_code
        and "replay.attempts = 0;" in replay_code,
        "死信台账在三条后端上都只有入账与读出，没有淘汰调用；重投幂等键带 attempts 且归零重投（V13 R25 二轮）",
        f"出现淘汰调用的后端 {evicting or '无'}；"
        f"三处台账面都在册={all(face in dead_letter_sources[rel] for rel, face in dead_letter_faces.items())}；"
        f"重投键写法在盘={'format!(\"{}:replay:{}\"' in replay_code}、attempts 归零={'replay.attempts = 0;' in replay_code}"
        "——谁加一条 DELETE 或 retain，同一事件就能在同一个 attempts 上被投第二次，去重就只剩运气",
    )


def command_status_vocabulary_single_source_check() -> None:
    """控制命令状态不留零构造者的变体，终态词表只在 `is_final` 体内出现一次（V13 R1-H）。

    重落 V11 R6-1：那份交付面被合流 c07ad22 按上游树整体裁定覆盖掉了，`Rejected` 变体连同
    它在 `from_json` 的两条模式臂、`spread.rs` 的两处抄写一起回来了。零构造者的变体不是
    无害装饰：每个 `match` 都得为它写一条永不为真的臂，而「这条臂走不到」只能靠人逐个去证；
    词表抄在三个平面上，加一个变体就得同步改三处，漏一处恢复校验就松。
    """
    control = production_text((ROOT / CONTROL_PLANE_FILE).read_text(encoding="utf-8"))
    spread = production_text((ROOT / SPREAD_IDEMPOTENCY_FILE).read_text(encoding="utf-8"))
    # 模式位置上把终态词表又抄一遍（`CommandStatus::X | CommandStatus::Y`）。函数调用进不去
    # 模式，所以以前只能靠「两处口径一致」的人肉约定；改成带守卫的 match 之后这种抄写就该绝迹。
    pattern_copies = re.findall(r"CommandStatus::\w+\s*\|\s*CommandStatus::", control + spread)
    check(
        control.count("Rejected") == 0
        and control.count(COMMAND_FINALITY_BODY) == 1
        and control.count("pub const fn is_final(self) -> bool") == 1
        and not pattern_copies
        and spread.count("record.status.is_final()") == 1,
        "控制命令状态没有零构造者的变体，终态词表只在 is_final 体内出现一次（V13 R1-H）",
        f"控制面生产代码里 Rejected 残留 {control.count('Rejected')} 处（期望 0：拒绝发生在受理之前，"
        f"命令与审计记录都不落盘，所以它不是一个能被写出来的审计状态）、"
        f"终态词表 {control.count(COMMAND_FINALITY_BODY)} 处（期望 1，在 is_final 体内）、"
        f"is_final 定义 {control.count('pub const fn is_final(self) -> bool')} 处（期望 1）；"
        f"模式位置上又抄一遍词表 {pattern_copies or '无'}（期望无）；"
        f"CLI 幂等入口把终态判定委托给 is_final {spread.count('record.status.is_final()')} 处（期望 1）",
    )


# V11 Q0a：执行平面的成本口径只有一个定义点，且生产 Paper 不得回落到零费。
# V11 Q0c：该定义点必须**读配置**，且成本规则文件的读者全仓唯一。
EXECUTION_FEE_MODEL_FILE = "crates/qx-cli/src/runtime_wiring.rs"
FEE_KERNEL_FILE = "crates/qx-core/src/fee.rs"
COST_RULES_FILE = "crates/qx-xingban/src/cost_rules.rs"
COST_RULES_TEMPLATE = "deploy/qianxing.costs.example.json"
PAPER_FEE_TEST_FILE = "crates/contract-tests/tests/paper_accounting.rs"
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
# V12 D3：该用例文件已按主题拆成目录模块，判据按目录拼接取数。
MULTI_LEG_CASE_FILE = "crates/qx-cli/tests/multi_leg_attribution"
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
    cases = case_source(MULTI_LEG_CASE_FILE)
    check(
        all(kind in cases for kind in MULTI_LEG_KIND_NAMES)
        and "fn vetoed_leg_never_pairs_against_a_filled_counterpart" in cases
        and "fn multi_leg_funding_bound_fails_loudly_instead_of_capping_cash" in cases,
        "四条多腿 kind 与两个反向验证用例必须齐备",
        "multi_leg_attribution 用例目录的覆盖不再咬住 Q0e 口径",
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
        "multi_leg_attribution 用例目录的覆盖不再咬住 Q58 口径",
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
        # 用例树按同一条口径排除（下一条 DEFAULT_INSTRUMENT_SPEC_VERSION 的清点早就这么写）：
        # 这一族判据钉的是**生产**里不许有第二个落点，而 V13 R1-A6 的模板覆盖用例正是要在
        # 测试里问一次"这份文件按哪种形状读的"，把它算成落点等于禁止给规格建用例。
        and "/tests/" not in f"/{path.relative_to(ROOT).as_posix()}"
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
        production = production_text((ROOT / path).read_text(encoding="utf-8"))
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


# —— deploy 模板读取覆盖（V13 R1-A6）——
# 52 份顶层模板里曾有 12 份在代码与 CI 中零引用，而 CI 只按 `config validate` 读 1 份。
# "零引用"当时被当成了"该删"，实测却推翻了这个前提：`qianxing.scheduler.jobs.smoke.json`
# 正是另两份 runtime 模板挂着的 jobs_path。真正缺的是读取，于是补了一条覆盖用例
# （`crates/qx-cli/src/tests/deploy_template_coverage.rs`）：每份模板都必须被它在生产里
# 对应的那个读法解析一次。下面这几颗判据钉的是那份用例本身别烂掉——它一旦被动过手脚
# （少登记、把读取器换成走过场、把某个变体从坏内容探针里漏掉），全仓其余用例都是绿的。
COVERAGE_SOURCE = "crates/qx-cli/src/tests/deploy_template_coverage.rs"
# 每类模板必须真的调用到生产的那一个读点，而不是在覆盖用例里另写一份字段校验。
COVERAGE_PRODUCTION_READERS = (
    "read_runtime_config(",
    "validate_runtime_references(",
    "market_spec_from_value(",
    "validate_ccxt_worker_binding(",
    "read_bar_frame_for_backtest(",
    "barframe_dataset_identity(",
    "DepthFrame::from_json(",
    "DatasetBundleManifest",
    "ArrowDatasetManifest::from_json(",
    "ashare_backtest_binding(",
    "apply_corporate_actions_json_with_report(",
    "apply_calendar_json_with_report(",
    "ExecutionCostRules::load(",
    "filter_map(unsupported_dispatch_shape)",
    "StrategyTargetSnapshot",
    "order_from_submit_command(",
    "parse_fast_backtest_manifest(",
)
COVERAGE_TEST_FNS = (
    "every_deploy_template_is_registered_with_a_reader",
    "every_deploy_template_loads_through_its_registered_reader",
    "every_reader_class_rejects_a_broken_template",
)


def _balanced_paren_args(text: str, open_index: int) -> str:
    """从 `(` 处取到配对的 `)`，用于把 `Reader::MarketSpec(Some("x"))` 的参数完整切出来。"""
    if open_index >= len(text) or text[open_index] != "(":
        return ""
    depth = 0
    for index in range(open_index, len(text)):
        if text[index] == "(":
            depth += 1
        elif text[index] == ")":
            depth -= 1
            if depth == 0:
                return text[open_index : index + 1]
    return ""


# —— 示例配置的唯一读取口（V13 第三十一遍 ① 尾 #271）——
# 改前每个入口各写一遍 `读取…失败 {path}: {error}`，只有 `runtime-check` 那一族把
# 「这一份示例在别处存在」接进了报错正文。于是同一棵树、同一个无关启动目录里
# `runtime-check` 会指路，而 `fast-backtest` 只留一行 os error 3
# （`logs/s750_pass31_standalone_fast_backtest.txt`）。现在收成一处，下面这几条钉的是
# "不许再有人手写第二份"：漏斗的定义位置、它是否真的把补话接上、以及每条链是否在用它。
FUNNEL_FILE = "crates/qx-cli/src/deploy_lookup.rs"
CLI_ARGS_FILE = "crates/qx-cli/src/cli_args.rs"
LOOKUP_PARSER = "parse_deploy_path"
EXAMPLE_DEFAULT_PREFIX = 'default_value = "deploy/'
# 「别处那一份」的补话只许由唯一读取口接上：别的文件一旦自己拼 `读取…失败 …: 原因`，
# 那句补话就漏了，而报错形状看着仍然对——`read_runtime_config` 曾长期是这样。
RELOCATION_HINT_CALLER = "deploy_relocation_hint("
# V13 #274：`example_defaults` 那条只扫带 `default_value` 的路径参数，看不见「读取示例输入」
# 却没有默认值（甚至必填）的位置参数。改前同一棵树上 `runtime-check` 会搬迁、而 `backtest`
# /`paper-submit-order`/`reconcile` 只回一行 os error 3（`logs/s774_pass32_lookup_unmounted_before.txt`）。
# 逐变体点名这些读取入口的路径参数必须挂上查找面解析器，半接面不再可能悄悄回来。
LOOKUP_MOUNTED_READ_ARGS = {
    "Backtest": ["runtime", "frame", "spec"],
    "PaperSubmitOrder": ["path", "command_path"],
    "Reconcile": ["path"],
}
# 每份文件至少要有这么多次漏斗调用：少于登记数，就是有人把某一格读取改回了直写文案。
FUNNEL_CONSUMERS = {
    "crates/qx-cli/src/runtime_wiring.rs": 1,
    "crates/qx-cli/src/backtests/fast_backtest.rs": 1,
    "crates/qx-cli/src/backtests/mod.rs": 2,
    "crates/qx-cli/src/backtests/artifacts.rs": 2,
    "crates/qx-cli/src/backtests/single_strategy.rs": 1,
    "crates/qx-cli/src/backtests/strategy_backtest.rs": 2,
    "crates/qx-cli/src/backtests/ashare_binding.rs": 3,
    "crates/qx-cli/src/dataset_commands.rs": 6,
}


def example_read_funnel_check() -> None:
    """示例配置的读取报错只在查找面拼一份，且三条链上的每个入口都经它。"""
    source = (ROOT / FUNNEL_FILE).read_text(encoding="utf-8")
    start = source.find("pub(crate) fn read_example_json(")
    check(
        start >= 0,
        "示例配置的唯一读取口定义在查找面模块里",
        f"{FUNNEL_FILE} 里没有 read_example_json 的定义",
    )
    body = source[start:] if start >= 0 else ""
    body = body[: body.find("\n}")]
    check(
        "std::fs::read_to_string(" in body and "deploy_relocation_hint(" in body,
        "唯一读取口自己读文件，并把「别处那一份」接在同一句报错里",
        f"漏斗函数体缺读点或缺补话: {body[:120]!r}",
    )
    shortfall = []
    for relative, required in sorted(FUNNEL_CONSUMERS.items()):
        actual = (ROOT / relative).read_text(encoding="utf-8").count("read_example_json(")
        if actual < required:
            shortfall.append(f"{relative} 只剩 {actual} 处（登记 {required} 处）")
    check(
        not shortfall,
        "快速回测 / 数据集 / A 股规则三条链上的示例读取全部经唯一读取口",
        "有人把某格读取改回了手写文案: " + "; ".join(shortfall),
    )
    # 屏幕上的第一句口径已经被产物与用例钉住（src/tests/backtest_input_provenance.rs），
    # 漏斗一旦改口，那两处读者会各说各话；这里把格式串本身登记住。
    check(
        '"读取{label}失败 {}: {error}{}"' in body,
        "报错正文沿用各入口改前的第一句措辞，只是末尾接上补话",
        f"漏斗的格式串改了: {body[:200]!r}",
    )
    copies = {}
    for path in rust_sources():
        relative = path.relative_to(ROOT).as_posix()
        if relative == FUNNEL_FILE or "/tests/" in relative:
            continue
        count = path.read_text(encoding="utf-8").count(RELOCATION_HINT_CALLER)
        if count:
            copies[relative] = count
    check(
        not copies,
        "「这一份示例在别处存在」的补话只在唯一读取口拼装（没有第二处手写同一句）",
        f"有人手抄了第二处报错拼装: {copies}",
    )
    # 哪些入口的默认值该走查找面，由命令表自己回答，不留给散文：
    # 少挂一处解析器，同一棵树上就会出现「默认值读得到、显式给同一条路径读不到」那格不对称。
    table = (ROOT / CLI_ARGS_FILE).read_text(encoding="utf-8")
    example_defaults = [
        line.strip() for line in table.splitlines() if EXAMPLE_DEFAULT_PREFIX in line
    ]
    unwired = [
        line for line in example_defaults if LOOKUP_PARSER not in line
    ]
    check(
        bool(example_defaults),
        "命令表里确实存在以示例配置形状作默认值的路径参数（判据无对象即报，不静默给绿）",
        f"{CLI_ARGS_FILE} 里找不到 {EXAMPLE_DEFAULT_PREFIX} 的默认值，这条判据已失去对象",
    )
    check(
        not unwired,
        "以示例配置形状作默认值的路径参数全部挂上查找面解析器",
        f"这些默认值不经过查找面（少挂 {LOOKUP_PARSER}）: {unwired}",
    )
    # 必填/可选的「读取示例输入」位置参数逐个必须挂解析器。上面的 default_value 扫描看不见它们，
    # 少了这条就会重现「默认值读得到、手打同一条路径读不到」的半接面（#274）。
    table_lines = table.splitlines()
    missing_mount = []
    for variant, fields in sorted(LOOKUP_MOUNTED_READ_ARGS.items()):
        opened = [
            index
            for index, line in enumerate(table_lines)
            if line.strip() == f"{variant} {{"
        ]
        if not opened:
            missing_mount.append(f"{variant} 变体找不到（判据失去对象）")
            continue
        for field in fields:
            decl = next(
                (
                    index
                    for index in range(opened[0] + 1, len(table_lines))
                    if table_lines[index].strip() in ("}", "},")
                    or (
                        table_lines[index].strip().startswith(f"{field}: ")
                        and "PathBuf" in table_lines[index]
                    )
                ),
                None,
            )
            if decl is None or not table_lines[decl].strip().startswith(f"{field}: "):
                missing_mount.append(f"{variant}.{field} 路径参数找不到（判据失去对象）")
                continue
            mounted = False
            probe = decl - 1
            while probe >= 0 and table_lines[probe].lstrip().startswith(("#[", "//")):
                if "value_parser = parse_deploy_path" in table_lines[probe]:
                    mounted = True
                    break
                probe -= 1
            if not mounted:
                missing_mount.append(f"{variant}.{field} 未挂 {LOOKUP_PARSER}")
    check(
        not missing_mount,
        "读取示例输入的位置参数（含无默认值的必填项）全部挂上查找面解析器",
        f"查找面又只剩一半，无关目录里这些入口会只回 os error 3: {missing_mount}",
    )


def deploy_template_coverage_check() -> None:
    """deploy 模板的读取覆盖登记表与磁盘清单、与生产读点、与坏内容探针三方对齐。"""
    source = (ROOT / COVERAGE_SOURCE).read_text(encoding="utf-8")
    deploy_files = sorted(
        path.name for path in (ROOT / "deploy").glob("*.json") if path.is_file()
    )
    body_start = source.find("const COVERAGE:")
    body_end = source.find("\n];", body_start)
    body = source[body_start:body_end] if body_start >= 0 else ""
    entries = re.findall(
        r'"([\w.\-]+\.json)",\s*Reader::(\w+)',
        body,
    )
    expected = re.findall(r"Expected::(\w+)", body)
    check(
        bool(entries) and len(entries) == len(expected),
        "deploy 模板覆盖登记表在册（文件名→读取器→预期三列逐条对齐）",
        f"登记表解析出 {len(entries)} 条读取器、{len(expected)} 条预期，两者必须逐条相等",
    )
    registered = [name for name, _ in entries]
    check(
        sorted(registered) == deploy_files and len(set(registered)) == len(registered),
        "登记表与 deploy 顶层 JSON 清单逐名相等（新增不登记即红、删除不销账也红）",
        f"登记 {len(registered)} 份 / 磁盘 {len(deploy_files)} 份，"
        f"只在登记={sorted(set(registered) - set(deploy_files))} "
        f"只在磁盘={sorted(set(deploy_files) - set(registered))}",
    )
    enum_start = source.find("enum Reader {")
    enum_end = source.find("\n}", enum_start)
    enum_body = source[enum_start:enum_end]
    variants = re.findall(r"^\s{4}(\w+)[(,]", enum_body, re.MULTILINE)
    check(
        len(variants) >= 15,
        "读取器变体清点得出来（新增一类却不登记就红）",
        f"enum Reader 解析出 {len(variants)} 个变体: {variants}",
    )
    used = {reader for _, reader in entries}
    check(
        set(variants) <= used,
        "Reader 的每个变体都至少被一份模板用到（没有无人验证的读取器）",
        f"未被使用的变体: {sorted(set(variants) - used)}",
    )
    probe_start = source.find("fn reader_classes() -> Vec<Reader>")
    probe_end = source.find("\n}", probe_start)
    probe = re.findall(r"Reader::(\w+)", source[probe_start:probe_end])
    check(
        set(variants) <= set(probe),
        "Reader 的每个变体都进了坏内容探针（走过场的读取器当场可检出）",
        f"探针漏掉的变体: {sorted(set(variants) - set(probe))}",
    )
    missing_readers = [
        symbol for symbol in COVERAGE_PRODUCTION_READERS if symbol not in source
    ]
    check(
        not missing_readers,
        "每类模板的读取都落在生产读点上，而不是覆盖用例里另写一份校验",
        f"文件里找不到这些生产读点: {missing_readers}",
    )
    mounted = (ROOT / "crates/qx-cli/src/tests/mod.rs").read_text(encoding="utf-8")
    fns = [name for name in COVERAGE_TEST_FNS if f"fn {name}()" in source]
    check(
        "mod deploy_template_coverage;" in mounted and len(fns) == len(COVERAGE_TEST_FNS),
        "覆盖用例的三条判据在位且模块已挂载（半挂载的拆分会让判据静默失效）",
        f"挂载={('mod deploy_template_coverage;' in mounted)}，在册用例 {len(fns)}/3",
    )
    refused = re.findall(
        r'"([\w.\-]+\.json)",\s*Reader::\w+,[\s\S]{0,400}?Expected::Refuses', body
    )
    ok_count = expected.count("Ok")
    check(
        len(refused) + ok_count == len(entries),
        "每份模板都写明了预期结果，Refuses 只点名在册的已知缺口",
        f"Ok={ok_count} Refuses={len(refused)} 登记={len(entries)}，Refuses 名单: {refused}",
    )
    pairing = set()
    for match in re.finditer(r"Reader::(\w+)", body):
        open_index = match.end()
        args = _balanced_paren_args(body, open_index if body[open_index : open_index + 1] == "(" else -1)
        pairing.update(re.findall(r'"([\w.\-]+\.json)"', args))
    dangling = sorted(pairing - set(deploy_files))
    check(
        not dangling,
        "登记表里点名的配对来源必须仍是磁盘上存在的模板（配对失效不能静默换文件）",
        f"配对指向不存在的文件: {dangling}",
    )


# 一次性提交入口的终态不变量（V13 第三十一遍 ② #273）。
#
# 命令一旦被控制面记成 Accepted，它同时也已经(或即将)进了队列：这一段里任何用 `?` 或 `return`
# 抛出函数的失败，都会留下一条永不结束的 Accepted、一份没人释放的租约，而同 request_id 重投
# 只会撞幂等闸门（控制面对 command_id 与 request_id 都做幂等），文案却还在指人"重试"。
# 缺行情是这条路径上最常命中的失败，实测见 `logs/s769_pass32_btc_paper_submit.txt`。
SUBMIT_TERMINAL_ENTRIES = {
    "Paper": (
        "crates/qx-cli/src/venue_runtime/paper_submit.rs",
        "pub(crate) fn run_paper_submit_order(",
    ),
    "Binance": (
        "crates/qx-cli/src/venue_runtime/binance_submit.rs",
        "pub(crate) fn run_binance_submit_order(",
    ),
}
# Accepted 绑定与终态回写按语句取段：改掉任一形状都会让判据失去取段的位置，当场报而不是静默瞎。
ACCEPTED_BIND = "let accepted = accepted_result"
TERMINAL_WRITEBACK = ".transact(|plane| plane.execute(command.command_id, now, |_| action.clone()))"
SUBMIT_ACCEPT_CALL = "plane.submit_as("
# Accepted 之后、动作值之前仍在册的队列管线退出点。它们与动作失败不同：入队或领取失败时命令
# 可能已被常驻 worker 领走，把它写成 Failed 会覆盖别人的裁决，所以收口口径要单独定（#273 残口）。
QUEUE_ESCAPES_AFTER_ACCEPTED = {
    "Paper": ("enqueue_command", "claim_command"),
    "Binance": (),
}
# 队列确认只按「领到过租约」计，不按裁决计：裁决为 Failed 时同样要 ack，否则条目要等租约过期才出队。
QUEUE_ACK_CALL = ".ack_command_at("
TERMINAL_REJECTION_HELPER = "terminal_submit_rejection"
TERMINAL_REJECTION_DEF = "fn terminal_submit_rejection("
TERMINAL_REJECTION_TAIL = "换新的 command_id 与 request_id 重新提交"
SUBMIT_ATTEMPT_HELPER = "paper_submit_match_attempt"
SUBMIT_ATTEMPT_DEF = "fn paper_submit_match_attempt("
SUBMIT_ATTEMPT_CONSUMERS = {
    "crates/qx-cli/src/venue_runtime/paper_submit.rs",
    "crates/qx-cli/src/venue_runtime/paper_worker.rs",
}
TERMINAL_STATE_CASE_FILE = "crates/qx-cli/src/tests/paper_submit_terminal_state.rs"
TERMINAL_STATE_CASE_FNS = (
    "paper_submit_order_without_market_quote_terminates_and_acks_the_queue",
    "paper_submit_order_with_market_quote_still_reaches_executed",
    "paper_execution_worker_terminates_a_quote_less_command_and_keeps_running",
)
SUBMIT_ENTRY_FILES = {
    "Paper": "crates/qx-cli/src/venue_runtime/paper_submit.rs",
    "Binance": "crates/qx-cli/src/venue_runtime/binance_submit.rs",
    "Ccxt": "crates/qx-cli/src/venue_runtime/ccxt_submit.rs",
}
# 一次性提交入口的拓扑一致性判据：账户 / 交易所对不上就要落成终态 Failed，不得把订单写进
# 另一台 worker 的 EventLog（V13 R28）。三条入口显式点名或按账户挑 worker，所以这道闸门
# 必须在动作函数之前；挪到动作里、或退回「取第一台 / 就地再写一遍」都会让多账户隔离断掉。
SUBMIT_TOPOLOGY_GUARDS = {
    "Paper": ("paper_submit_matches_worker", "run_paper_submit_order("),
    "Binance": ("binance_submit_matches_worker", "run_binance_submit_order("),
    "Ccxt": ("ccxt_submit_matches_worker", "run_ccxt_submit_order("),
}
SUBMIT_TOPOLOGY_ORDER = {
    # 拓扑判据要排在风控闸门与动作函数之前：先判「这台 worker 能不能收这笔订单」，
    # 再判「这台 worker 有没有风控规格」。顺序反了会先报缺配置，把真原因盖住。
    "Paper": ("require_worker_risk_spec", "paper_submit_action"),
    "Binance": ("require_worker_risk_spec", "binance_submit_action"),
    "Ccxt": ("require_worker_risk_spec", "ccxt_submit_action"),
}
STRING_LITERAL_RE = re.compile(r'"(?:[^"\\]|\\.)*"')
PUNCT_SPACING_RE = re.compile(r"\s*([(),;])\s*")
QUESTION_RE = re.compile(r"\?")
RETURN_RE = re.compile(r"\breturn\b")


def _collapsed_code(text: str, strip_strings: bool = False, tight: bool = False) -> str:
    """去掉换行与尾逗号后的代码文本：判据认的是语句与顺序，不是 rustfmt 的换行选择。

    本轮把 venue_runtime 过一遍 rustfmt 就抓到这格盲区 —— 多行拆分会给最后一个实参补上尾逗号，
    按字面比对的判据立刻读出 0 处（`lease_clock_domain_check` 的 ack 判据就是这么红的）。
    改名要连改判据，可格式一变判据必须自己认得两种写法，所以这里把「换行与尾逗号」移出取数口径。
    默认口径把空白压成单空格、再抹掉 `(),;` 旁的空白，在册字面量按可读写法登记、匹配时用
    `_squeezed()` 走同一条变换；`tight=True` 是全删空白，只服务那些本来就按无空格登记的判据。
    `strip_strings` 只给按 `?` 找退出点的判据用：插值 `{error:?}` 里的 `?` 不是退出点；
    而指路尾句那族判据必须看得见字面量本身，走默认的不剥字符串口径。
    """
    body = STRING_LITERAL_RE.sub('""', text) if strip_strings else text
    if tight:
        return "".join(body.split()).replace(",)", ")")
    flat = PUNCT_SPACING_RE.sub(r"\1", " ".join(body.split()))
    return flat.replace(",)", ")")


def _squeezed(needle: str) -> str:
    """把可读写法的在册字面量换成与 `_collapsed_code` 同一条变换，两侧才在同一口径上比对。"""
    return PUNCT_SPACING_RE.sub(r"\1", " ".join(needle.split()))


def _accepted_flow(path: str, signature: str) -> tuple[str, str, str]:
    """一条提交入口的（Accepted 之后到终态回写的代码段, 回写之后的代码段, 缺口说明）。"""
    flat = _collapsed_code(_code_body(path, signature), strip_strings=True)
    if not flat:
        return "", "", f"{signature} 的函数体取不到（改名、搬家或半挂载）"
    submit = flat.find(_squeezed(SUBMIT_ACCEPT_CALL))
    bind = flat.find(_squeezed(ACCEPTED_BIND))
    writeback = flat.find(_squeezed(TERMINAL_WRITEBACK))
    if submit < 0 or bind < 0 or writeback < 0 or not submit < bind < writeback:
        return "", "", (
            "submit_as/Accepted 绑定/终态回写缺失或顺序颠倒"
            f"(submit_as@{submit} Accepted@{bind} 终态@{writeback})"
        )
    bind_end = flat.find("?", bind)
    if bind_end < 0:
        return "", "", "Accepted 绑定之后没有 `?`，取段位置失效"
    return flat[bind_end + 1 : writeback], flat[writeback + len(_squeezed(TERMINAL_WRITEBACK)) :], ""


def submit_terminal_state_check() -> None:
    """Accepted → 动作值 → 终态回写 → 队列确认是一条不许中途跑路的链（#273）。"""
    shapes = []
    regions = {}
    ack_paths = {}
    for label, (relative, signature) in sorted(SUBMIT_TERMINAL_ENTRIES.items()):
        region, after_writeback, gap = _accepted_flow(relative, signature)
        if gap:
            shapes.append(f"{label} {gap}")
            continue
        regions[label] = region
        index = after_writeback.find(_squeezed(QUEUE_ACK_CALL))
        if index >= 0:
            ack_paths[label] = after_writeback[:index]
    check(
        not shapes,
        "两条一次性提交入口都保住「submit_as → Accepted 绑定 → 动作值 → 终态回写」这一段",
        "; ".join(shapes),
    )
    escape_gap = []
    for label, region in sorted(regions.items()):
        statements = []
        start = 0
        for match in QUESTION_RE.finditer(region):
            statements.append(region[start : match.end()])
            start = match.end()
        pending = list(QUEUE_ESCAPES_AFTER_ACCEPTED[label])
        for index, statement in enumerate(statements):
            # 一处退出认给「离它最近的那个在册调用」：合并成一条语句时，早先的调用不该顶掉它。
            hit = None
            for name in pending:
                if name in statement and (hit is None or statement.rfind(name) > statement.rfind(hit)):
                    hit = name
            if hit is None:
                escape_gap.append(f"{label} 第 {index + 1} 处 `?` 退出不在册: …{statement[-50:]}")
            else:
                pending.remove(hit)
        for name in pending:
            escape_gap.append(f"{label} 在册退出点 {name} 已经没有 `?`，登记该删")
    check(
        not escape_gap,
        "Accepted 之后的 `?` 退出只允许在册的那几处队列管线调用（新增或修好都要当场改登记）",
        "; ".join(escape_gap),
    )
    early = {
        label: len(RETURN_RE.findall(region))
        for label, region in sorted(regions.items())
        if RETURN_RE.search(region)
    }
    check(
        not early,
        "Accepted 之后到终态回写之间不得用 `return` 绕过回写",
        f"这些入口里有提前返回: {early}",
    )
    check(
        set(ack_paths) == {"Paper"}
        and not any(RETURN_RE.search(gap) for gap in ack_paths.values()),
        "Paper 入口的队列确认排在终态回写之后，且中间不夹裁决分支（失败也要 ack）",
        f"实际有队列确认的入口: {sorted(ack_paths)}，夹了返回的: "
        f"{sorted(label for label, gap in ack_paths.items() if RETURN_RE.search(gap))}",
    )
    definitions = {}
    copies = {}
    routed = {}
    attempt_defs = {}
    attempt_calls = {}
    for path in rust_sources():
        relative = path.relative_to(ROOT).as_posix()
        if "/tests/" in relative:
            continue
        source = _collapsed_code(production_text(path.read_text(encoding="utf-8")))
        defs = source.count(_squeezed(TERMINAL_REJECTION_DEF))
        if defs:
            definitions[relative] = defs
        if TERMINAL_REJECTION_TAIL in source and not defs:
            copies[relative] = source.count(TERMINAL_REJECTION_TAIL)
        calls = source.count(_squeezed(f"{TERMINAL_REJECTION_HELPER}(")) - defs
        if calls:
            routed[relative] = calls
        if source.count(_squeezed(SUBMIT_ATTEMPT_DEF)):
            attempt_defs[relative] = source.count(_squeezed(SUBMIT_ATTEMPT_DEF))
        used = source.count(_squeezed(f"{SUBMIT_ATTEMPT_HELPER}(")) - source.count(
            _squeezed(SUBMIT_ATTEMPT_DEF)
        )
        if used:
            attempt_calls[relative] = used
    check(
        definitions == {SUBMIT_ENTRY_FILES["Paper"]: 1},
        "「这一手已记为终态失败」的指路口径只有一个定义点",
        f"定义点分布: {definitions}",
    )
    check(
        not copies,
        "指路尾句没有被手抄到第二处（要换措辞就改定义点，不能在调用点各写一份）",
        f"手抄处: {copies}",
    )
    check(
        set(routed) == set(SUBMIT_ENTRY_FILES.values()),
        "Paper、Binance 与 CCXT 三条一次性提交链路都经这族指路口径",
        f"实际经它的文件: {routed}",
    )
    check(
        attempt_defs == {SUBMIT_ENTRY_FILES["Paper"]: 1},
        "同一次撮合尝试只有一个定义点",
        f"定义点分布: {attempt_defs}",
    )
    check(
        attempt_calls == {relative: 1 for relative in sorted(SUBMIT_ATTEMPT_CONSUMERS)},
        "一次性验收入口与常驻 worker 循环各调用一次同一个撮合尝试（第三条手写裁决当场可检出）",
        f"实际调用点: {attempt_calls}",
    )
    mounted = (ROOT / "crates/qx-cli/src/tests/mod.rs").read_text(encoding="utf-8")
    case_text = (ROOT / TERMINAL_STATE_CASE_FILE).read_text(encoding="utf-8")
    missing = [name for name in TERMINAL_STATE_CASE_FNS if f"fn {name}()" not in case_text]
    check(
        "mod paper_submit_terminal_state;" in mounted and not missing,
        "缺行情终态用例的三条判据在位且模块已挂载（半挂载的拆分会让判据静默失效）",
        f"挂载={('mod paper_submit_terminal_state;' in mounted)}，缺的用例: {missing}",
    )


def submit_topology_guard_check() -> None:
    """一次性提交入口的拓扑一致性判据必须落在动作函数之前（V13 R28）。

    三条入口都要把「这笔订单归谁」判清楚才动手：Binance / CCXT 显式点名 worker，Paper 按
    账户/交易所挑 worker。判据挪进动作函数、或 Paper 退回「取第一台」，都会把订单写进另一台
    worker 的 EventLog —— 多账户共享一份控制面时隔离当场失效。判据认的是动作函数被调用之前
    有没有那次拓扑比对，所以把比对删掉、或换成任何别的表达式都会红。
    """
    gaps = []
    for label, (guard, signature) in sorted(SUBMIT_TOPOLOGY_GUARDS.items()):
        path = SUBMIT_ENTRY_FILES[label]
        flat = _collapsed_code(_code_body(path, signature), strip_strings=True)
        if not flat:
            gaps.append(f"{label} {signature} 的函数体取不到（改名、搬家或半挂载）")
            continue
        guard_at = flat.find(_squeezed(guard))
        action_at = flat.find(_squeezed(SUBMIT_TOPOLOGY_ORDER[label][1]))
        if guard_at < 0:
            gaps.append(f"{label} 入口没有调用 {guard}")
        elif action_at < 0:
            gaps.append(f"{label} 入口找不到动作函数 {SUBMIT_TOPOLOGY_ORDER[label][1]}")
        elif not guard_at < action_at:
            gaps.append(
                f"{label} 的拓扑判据排在动作函数之后或之后缺失"
                f"(guard@{guard_at} action@{action_at})"
            )
    check(
        not gaps,
        "三条一次性提交入口都在动作函数之前完成账户/交易所拓扑比对",
        "; ".join(gaps),
    )

PAPER_PIPELINE_FILE = "crates/qx-cli/src/venue_runtime/paper_worker.rs"
# 这四格缺任意一格，就说明末行又退回读累计量、或零新增/真验收两条通道塌回一条。
PAPER_PIPELINE_DELTA_NEEDLES = (
    "let orders_before = market_pipeline.orders().len();",
    "let new_orders = orders_now.saturating_sub(orders_before);",
    "if new_orders > 0 {",
    "本轮零新增",
)
# 改前那条无条件成功句：只印累计 orders/ledger 并打 ✓，同日空转也照打（#275 现场）。
PAPER_PIPELINE_LEGACY_GREEN = "ledger_entries={} ✓"
# 顺序锚：成功 ✓ 必须在增量守卫那一支里，零新增那句在其后。
PAPER_PIPELINE_DELTA_GUARD = "if new_orders > 0 {"
PAPER_PIPELINE_SUCCESS_NEEDLE = "本轮新增) ✓"
PAPER_PIPELINE_ZERO_NEW_NEEDLE = "本轮零新增"


def paper_check_delta_honesty_check() -> None:
    """`paper-check` 末行按「本轮新增」给结论，同日空转不得再打验收 ✓（#275）。

    改前同一目录同日连跑两遍，第二遍调度 `skipped=1`、策略与执行各 `processed=0`，
    末行却照旧印累计数并打 ✓（`logs/s783_pass32_paper_check_doublerun_after_fix.txt` 记的那次
    现场）——把一次空转报成一次端到端验收通过。这里不锁 rustfmt 的换行，只锁这条链的
    取数口径：进场基线、增量子、增量分支与零新增分支四格都在位，旧的无条件累计 ✓ 不得
    复活，且成功 ✓ 必须排在 `new_orders > 0` 分支里、零新增那句排在 `else` 之后。
    """
    source = (ROOT / PAPER_PIPELINE_FILE).read_text(encoding="utf-8")
    missing = [needle for needle in PAPER_PIPELINE_DELTA_NEEDLES if needle not in source]
    check(
        not missing,
        "paper-check 末行按本轮增量给结论：进场基线、增量子、增量分支与零新增分支都在位",
        f"缺这些构件: {missing}",
    )
    check(
        PAPER_PIPELINE_LEGACY_GREEN not in source,
        "旧的无条件累计 ✓ 语句不得复活（同日重跑空转不许再冒充端到端验收通过）",
        f"源码里仍有无条件成功句: {PAPER_PIPELINE_LEGACY_GREEN!r}",
    )
    guard = source.find(PAPER_PIPELINE_DELTA_GUARD)
    success = source.find(PAPER_PIPELINE_SUCCESS_NEEDLE)
    zero_new = source.find(PAPER_PIPELINE_ZERO_NEW_NEEDLE)
    check(
        0 <= guard < success < zero_new,
        "成功 ✓ 排在增量守卫之后、零新增那句排在其后（顺序颠倒即空转与真验收又混成一格）",
        f"增量守卫@{guard} 成功句@{success} 零新增@{zero_new}",
    )


PYPROJECT_FILE = "python/pyproject.toml"
CCXT_ADAPTER_FILE = "python/qianxing_ccxt/__init__.py"
ASHARE_PROVIDER_FILE = "python/qianxing_ashare/__init__.py"
# #276：基础安装（`pip install <wheel>`，不带 extras、没有索引）必须一次装成，所以顶层
# dependencies 里不许再有 ccxt/tzdata 这类「要到调用点才需要」的第三方运行时包。
WHEEL_FORBIDDEN_MANDATORY_DEPS = ("ccxt", "tzdata")
# 交易所适配与 A 股时区/数据源都是可选能力，各自要有能装回来的 extra。
WHEEL_REQUIRED_EXTRAS = (
    "ccxt",
    "ccxt-pro",
    "tz",
    "a-share",
    "a-share-akshare",
    "a-share-baostock",
    "a-share-easy-tdx",
)


def wheel_optional_dependency_check() -> None:
    """wheel 把 ccxt/tzdata 降为可选 extras，让离线 `pip install` 一次装成（V13 #276）。

    改前它们写在顶层 `dependencies`，没有索引时 `pip install <wheel>` 以
    "ccxt was not found ... cannot be used" 直接失败，而四个包 import 时都不碰它们——
    `qianxing_ccxt` 在调用点 `importlib.import_module("ccxt")` 惰性加载、缺时抛点名
    `[ccxt]` 的可执行错误。项目自己的安装文档因此一直挂 `--offline --no-deps`。
    三查：顶层 dependencies 不含 ccxt/tzdata；能力 extras 全套定义；适配器报错点名的
    `qianxing[extra]` 每一个都在 pyproject 真有其名（#157 一族：报错指的东西不许不存在）。
    """
    import tomllib

    data = tomllib.loads((ROOT / PYPROJECT_FILE).read_text(encoding="utf-8"))
    mandatory = data["project"].get("dependencies", [])
    offenders = [d for d in mandatory if any(name in d for name in WHEEL_FORBIDDEN_MANDATORY_DEPS)]
    check(
        not offenders,
        "wheel 顶层 dependencies 不含 ccxt/tzdata：基础安装离线可装成（#276）",
        f"这些又变回强制依赖: {offenders}；改前离线 pip install 报 'ccxt was not found ... cannot be used'",
    )
    extras = data["project"].get("optional-dependencies", {})
    missing_extras = [name for name in WHEEL_REQUIRED_EXTRAS if name not in extras]
    check(
        not missing_extras,
        "wheel 定义了 ccxt/ccxt-pro/tz/a-share* 全套可选 extras",
        f"缺这些 extras: {missing_extras}",
    )
    adapter = (ROOT / CCXT_ADAPTER_FILE).read_text(encoding="utf-8")
    referenced = set(re.findall(r"qianxing\[([a-z0-9][a-z0-9-]*)\]", adapter))
    check(
        bool(referenced) and referenced <= set(extras),
        "适配器报错点名的 qianxing[extra] 全部有定义（不许指一个不存在的 extra）",
        f"报错引用 {sorted(referenced)}；未定义 {sorted(referenced - set(extras))}",
    )
    # #278：A 股数据源缺件时的可执行提示过去写成 `pip install -e '.[a-share-*]'`，那只在源码
    # checkout 里成立——按 #276 装了 wheel 的用户手里没有本地工程可 `-e`，这条指路把最容易撞上的
    # 缺件提示指回一条走不通的命令。使用正式发行名 `qianxing[a-share-*]`，两类受众都能执行。
    provider = (ROOT / ASHARE_PROVIDER_FILE).read_text(encoding="utf-8")
    check(
        "pip install -e '.[" not in provider,
        "A 股缺件提示不再用源码专用 `pip install -e '.[...]'`（wheel 用户执行不了，#278）",
        "qianxing_ashare 里仍能找到 `-e '.[` 形式的安装提示",
    )
    ashare_referenced = set(re.findall(r"qianxing\[([a-z0-9][a-z0-9-]*)\]", provider))
    a_share_refs = {name for name in ashare_referenced if name.startswith("a-share")}
    check(
        bool(a_share_refs) and a_share_refs <= set(extras),
        "A 股缺件提示点名的 qianxing[a-share-*] 全部有定义（#278）",
        f"引用 {sorted(a_share_refs)}；未定义 {sorted(a_share_refs - set(extras))}",
    )
    # #278 补：上面几条把这两个文件当文本 grep（安装提示 / 指针判据），从不确认它们仍是合法
    # Python。#278 一度把缺件消息改成 "..." 里套 "..."，grep 全绿而模块 import 当场 SyntaxError——
    # 正是三查要防的断链。这里用内置 compile() 逐个语法核对五个发布包的每个 .py（只检语法、
    # 不落 .pyc、无副作用），让「改一句面向用户的提示把整个包改崩」这类回归在门禁就被点名。
    broken_modules = []
    for _pkg in ("qianxing", "qianxing_bridge", "qianxing_strategy", "qianxing_ashare", "qianxing_ccxt"):
        for _src in sorted((ROOT / "python" / _pkg).rglob("*.py")):
            try:
                compile(_src.read_text(encoding="utf-8"), str(_src), "exec")
            except SyntaxError as _exc:
                broken_modules.append(f"{_src.relative_to(ROOT)}:{_exc.lineno}: {_exc.msg}")
    check(
        not broken_modules,
        "五个发布包的每个 .py 都是合法 Python（门禁不止 grep 文本，#278 补）",
        f"这些模块语法错误、import 即崩: {broken_modules}",
    )


# 台账 #152（V11 起在册，V13 R1-G 落地）：证据行 `path:NN` 的可达性判据。
# 文档里的行号引用会随代码漂移，而漂移后的引用比没有引用更坏——它把读者送到一处无关的代码上，
# 读者会照着那段代码去核对本轮的结论。V13 R1-F 实测：四份活文档 192 处引用里有 4 处已经落空
# （两处文件不存在、两处行号越界），全部是「当轮成立、后来搬家」那一类。判据取弱式可达性
# （文件在盘上 + 行号不超过该文件行数），不取「该行内容等于文档说的那句话」：后者要维护一份
# 逐条期望文本，与本仓已有的行数棘轮不成比例，而前者已经能抓住搬家与删除这两类主要漂移。
#
# 前缀集不写死，由仓库自己的顶层目录派生（第一颗判据钉住它非空），新增顶层目录自动进入扫描。
# 两个顶层目录被显式排除，理由是实测的：
#   - `tests/` 只有 `e2e/README.md`，一个 `.rs` 都没有；文档里的 `tests/mod.rs:409` 是
#     `crates/qx-cli/src/tests/mod.rs:409` 的裸尾简写（R1-G 实测活文档里 21 处这种写法），
#     按根相对路径去核会整批判成「文件不存在」，那是判据读错了写法，不是文档错了。
#   - `data/` 在 `.gitignore` 的 `/data/` 里，是运行产物目录，其中的路径不是可引用的源码事实。
DOC_CITATION_SKIP_PARTS = ("target", "node_modules", ".git", "__pycache__", "dist", ".venv")
DOC_CITATION_NON_SOURCE_TOP = ("tests", "data")
DOC_CITATION_EXTS = (
    "rs", "py", "json", "yaml", "yml", "md", "toml", "sh",
    "txt", "log", "csv", "html", "js", "ts",
)
DOC_ARCHIVE_PREFIX = "docs/archive/"
# 活文档引用条数地板：跌破说明扫描集本身失效——rglob 没走到、前缀集被改空、或文档被整段删掉——
# 而不是「文档写得更干净了」。引用变多不设上限。
# 272 是 V13 R26 终字节复测（30 份活 .md，含这条 CHANGELOG 自己那 2 处）；更早的 195 是 R1-G 在七份
# 2026-10 规划稿还住在 docs/ 时量的，那批现已移入 docs/archive/。
# GitHub CI 的可重复基线为 229：本机附加的、未纳入版本控制的方案输入不参与仓库地板。
LIVE_DOC_CITATION_FLOOR = 229
# 存档（`docs/archive/**`）的落空条数天花板：R1-G 实测 9 条。存档是「那一轮当时成立」的记录，
# 把它的行号改成今天的落点等于销毁当时的信息，所以不按活文档的零容忍处理；
# 但天花板只降不升——再往存档里写一条落空的引用会当场红。
ARCHIVE_DEAD_CITATION_CEILING = 9
# 活 + 存档的引用总条数地板：版本库基线 229 + 231 = 460；本机未跟踪方案输入不参与地板。
# 单看活侧地板抓不住「存档被整份删掉」：删掉一份存档文档时 `live_total` 不动、`archive_dead` 从 9 掉到 0
# （天花板判据是 `<=`，照样绿），于是「留而不删」这条纪律在门禁里**没有牙齿**。总地板把它补上：
# 引用从活侧搬到存档侧时总数不变（拆/移只是换住址），少掉一份被引用的存档文档当场红。
TOTAL_DOC_CITATION_FLOOR = 460


def doc_citation_reachability_check() -> None:
    """台账 #152：文档里每一处 `path:NN` 都必须还能落地（活文档零容忍，存档只降不升）。"""
    prefixes = tuple(
        sorted(
            entry.name
            for entry in ROOT.iterdir()
            if entry.is_dir()
            and entry.name not in DOC_CITATION_SKIP_PARTS
            and entry.name not in DOC_CITATION_NON_SOURCE_TOP
            and not entry.name.startswith(".")
        )
    )
    pattern = re.compile(
        r"(?<![\w./-])((?:"
        + "|".join(prefixes)
        + r")/[\w./\-*]+?\.(?:"
        + "|".join(DOC_CITATION_EXTS)
        + r")):(\d+(?:\s*[,，]\s*\d+)*)"
    )

    # 手工下钻而不是 `ROOT.rglob`：rglob 会走进 `target/`（本机十几万个文件），
    # 判据的耗时会盖过它要抓的那类漂移。
    docs: list[Path] = []
    stack = [ROOT]
    while stack:
        for entry in sorted(stack.pop().iterdir()):
            if entry.is_dir():
                if entry.name in DOC_CITATION_SKIP_PARTS or entry.name.startswith("."):
                    continue
                stack.append(entry)
            elif entry.suffix == ".md":
                docs.append(entry)
    docs.sort()

    line_counts: dict[str, int] = {}
    live_total = 0
    archive_total = 0
    live_dead: list[str] = []
    archive_dead: list[str] = []
    for doc in docs:
        rel = doc.relative_to(ROOT).as_posix()
        archived = rel.startswith(DOC_ARCHIVE_PREFIX)
        text = doc.read_text(encoding="utf-8", errors="replace")
        for match in pattern.finditer(text):
            target, numbers = match.group(1), match.group(2)
            for raw_number in re.split(r"[,，]", numbers):
                number = int(raw_number.strip())
                if archived:
                    archive_total += 1
                else:
                    live_total += 1
                candidate = ROOT / target
                reason = None
                if not candidate.exists():
                    reason = f"{rel} -> {target}:{number} 文件不存在"
                else:
                    if target not in line_counts:
                        line_counts[target] = len(
                            candidate.read_text(
                                encoding="utf-8", errors="replace"
                            ).split("\n")
                        )
                    if number > line_counts[target]:
                        reason = (
                            f"{rel} -> {target}:{number} 越界"
                            f"（该文件 {line_counts[target]} 行）"
                        )
                if reason is not None:
                    (archive_dead if archived else live_dead).append(reason)

    check(
        len(prefixes) >= 5,
        "文档引用判据的前缀集由仓库顶层目录派生，空集会让这颗判据静默全绿",
        f"派生出的前缀集是 {prefixes}",
    )
    check(
        not live_dead,
        "活文档里每一处 `path:NN` 都能落地：文件在盘上、行号不越界（台账 #152）",
        f"{len(live_dead)} 处落空，前 12 条 {live_dead[:12]}",
    )
    check(
        live_total >= LIVE_DOC_CITATION_FLOOR,
        "活文档的 `path:NN` 引用不少于实测地板：跌破是扫描集失效，不是文档变干净",
        f"本轮扫到 {live_total} 条（{len(docs)} 份 .md），地板 {LIVE_DOC_CITATION_FLOOR}",
    )
    check(
        len(archive_dead) <= ARCHIVE_DEAD_CITATION_CEILING,
        "存档文档的落空引用只降不升：存档保留当轮事实，但不许再往里写新的死指针",
        f"本轮 {len(archive_dead)} 条 / 天花板 {ARCHIVE_DEAD_CITATION_CEILING}，"
        f"存档共 {archive_total} 条引用，前 12 条 {archive_dead[:12]}",
    )
    check(
        live_total + archive_total >= TOTAL_DOC_CITATION_FLOOR,
        "活 + 存档的 `path:NN` 引用总条数不少于实测地板：拆/移只换住址，删掉被引用的存档当场红",
        f"本轮活 {live_total} + 存档 {archive_total} = {live_total + archive_total}，"
        f"地板 {TOTAL_DOC_CITATION_FLOOR}",
    )


# 策略输出的失败诊断同样是一臂一句、Rust 与 Python 两侧宿主共用（C++ worker 只写版本号、不做这套校验；V13 R7 收口 · 台账 #284 · R7-a）：这两侧曾经各自
# 把六种失败折叠成同一句话，而最需要点名的那一格（返回 dict 时漏写 schema_version）恰好被
# 念成「身份与输入不一致」，作者按那句话去查 request_id 永远查不出问题。
STRATEGY_OUTPUT_RUST_FILE = "crates/qx-runtime/src/strategy_contract/contract.rs"
STRATEGY_OUTPUT_RUST_SIG = "pub fn validate_for(&self, input: &StrategyContractInput"
STRATEGY_OUTPUT_RUST_ENTRY_SIG = "pub fn from_json_for(input: &str, request: &StrategyContractInput"
STRATEGY_OUTPUT_RUST_PREFIX = "StrategyContractOutput "
STRATEGY_OUTPUT_BRIDGE_CLASS = "class StrategyOutput:"
STRATEGY_OUTPUT_BRIDGE_SIG = "def validate_for(self, request: StrategyInput) -> None:"
STRATEGY_OUTPUT_BRIDGE_NEXT = "    def to_dict("
STRATEGY_OUTPUT_BRIDGE_PREFIX = "strategy output "
STRATEGY_OUTPUT_COLLAPSED = (
    "strategy output identity or expiry does not match input",
    "与输入身份、标的或有效期不一致",
)
STRATEGY_OUTPUT_CASE_FILE = "crates/qx-runtime/src/strategy_contract/tests.rs"
STRATEGY_OUTPUT_CASES = (
    "python_strategy_contract_is_versioned_pit_bounded_and_identity_bound",
    "strategy_output_decode_entry_rejects_foreign_schema_version",
)


def _python_scope(text: str, left: str, right: str) -> str:
    """取 `left` 与 `right` 之间那一段 Python 文本，并剥掉 `#` 注释行；锚点不唯一时返回空串。

    空串会让下游判据读不到任何诊断句而当场 FAIL，方向是对的（代码没了则门禁说有事）。
    这里不 `die`/`exit 2`：那会吞掉本条之后整点名册的读数。剥注释是因为注释里写着被禁的那句
    折叠话术，散文不能替代码作证，也不能替代码定罪。
    """
    parts = text.split(left)
    if len(parts) != 2:
        return ""
    scope = parts[1].split(right, 1)[0]
    return "\n".join(line for line in scope.split("\n") if not line.lstrip().startswith("#"))


def strategy_output_arm_diagnostics_check() -> None:
    """策略输出的每格失败必须各说各话，版本判定必须长在解码入口（V13 R7 收口 · 台账 #284 · R7-a）。

    判据两侧都从源码里取诊断句集合，不写第二份手工清单：Rust `validate_for` 的每个 `if` 臂
    返回一句、Python `StrategyOutput.validate_for` 的每个 `if` 臂抛一句，两侧各自「句数 ≥ 6
    且互不相同」。把六臂抄回同一句、或删掉某一臂，都会在这里当场红，而不是只红在一次用例里。
    """
    rust_body = _code_body(STRATEGY_OUTPUT_RUST_FILE, STRATEGY_OUTPUT_RUST_SIG)
    bridge_text = (ROOT / STRATEGY_INTENT_BRIDGE_FILE).read_text(encoding="utf-8")
    py_body = _python_scope(bridge_text, STRATEGY_OUTPUT_BRIDGE_SIG, STRATEGY_OUTPUT_BRIDGE_NEXT)
    readings = {}
    for label, body, prefix in (
        ("Rust", rust_body, STRATEGY_OUTPUT_RUST_PREFIX),
        ("Python", py_body, STRATEGY_OUTPUT_BRIDGE_PREFIX),
    ):
        arms = re.findall(f'"{re.escape(prefix)}([^"]*)"', body)
        readings[label] = arms
        check(
            len(arms) >= 6 and len(set(arms)) == len(arms),
            f"策略输出 {label} 侧每臂一句：诊断句 {len(arms)} 条、去重后 {len(set(arms))} 条、下限 6",
            f"臂数不足或又有两臂共用同一句话；重复的那条 {sorted({a for a in arms if arms.count(a) > 1})}",
        )
        check(
            bool(arms) and "schema_version" in arms[0],
            f"策略输出 {label} 侧第一臂点的是 schema_version（缺键那一格先被念出来）",
            f"首臂读成 {arms[0] if arms else '（读不到任何诊断句）'}",
        )
    live = production_text(rust_body) + _python_scope(
        bridge_text, STRATEGY_OUTPUT_BRIDGE_CLASS, "\nclass "
    )
    check(
        not any(collapsed in live for collapsed in STRATEGY_OUTPUT_COLLAPSED),
        "两侧代码里都没有「身份或有效期不一致」那句折叠诊断（不许回来）",
        f"命中 {[c for c in STRATEGY_OUTPUT_COLLAPSED if c in live]}",
    )
    conditions = {}
    for label, body, terminator in (("Rust", rust_body, " {"), ("Python", py_body, ":")):
        found = [
            line.split("if ", 1)[1].split(terminator, 1)[0].strip()
            for line in body.split("\n")
            if line.lstrip().startswith("if ") and "self.expires_at" in line
        ]
        conditions[label] = found[0] if len(found) == 1 else ""
    check(
        bool(conditions["Rust"])
        and conditions["Rust"].replace(" && ", " and ").replace("input.", "request.")
        == conditions["Python"],
        "过期地板两侧同一个式子（读到 Rust 那臂归一化后与 Python 逐字相等）",
        f"Rust `{conditions['Rust'] or '未取到唯一一臂'}` vs Python "
        f"`{conditions['Python'] or '未取到唯一一臂'}`",
    )
    rust_entry = _code_body(STRATEGY_OUTPUT_RUST_FILE, STRATEGY_OUTPUT_RUST_ENTRY_SIG)
    py_entry = _python_scope(bridge_text, "def from_dict(cls, value: Mapping[str, Any], request: StrategyInput)", "\n    @classmethod")
    check(
        "validate_for(request)" in rust_entry
        and "result.validate_for(request)" in py_entry
        and 'value.get("schema_version", SCHEMA_VERSION)' not in py_entry,
        "版本号由宿主解码入口裁决：两侧读回都过 validate_for，Python 缺键不采纳自家默认版本",
        "少了入口上的那次校验则 worker 已放行、宿主照收；把缺键默认成 SCHEMA_VERSION "
        "则「漏写版本号」在 Python 侧被当成合法产出，只有 Rust 拒它",
    )
    cases = (ROOT / STRATEGY_OUTPUT_CASE_FILE).read_text(encoding="utf-8")
    check(
        all(f"fn {name}(" in cases for name in STRATEGY_OUTPUT_CASES),
        "跨语言版本与逐臂点名用例在位：六臂各说各话 + 解码入口拒别家版本号",
        f"缺用例 {[name for name in STRATEGY_OUTPUT_CASES if f'fn {name}(' not in cases]}",
    )




# V13 R9：三个后端的 sync_control 只能比公共前缀。旧写法 `existing.len() > records.len() ||
# zip 不等` 把"链比本地快照长、但前缀逐条一致"也报成 Conflict。sqlite/postgres 的
# sync_control 跑在控制面事务 commit **之后**，两进程并发时后提交那份 plane 会包含先提交者的
# 变更，链因此天然比另一方长——那一支本该幂等收口，却被报成 Conflict，等于"控制面状态已经
# 提交成功、transact_control 却返回 Err"。真正的分叉是公共前缀内容对不上，那一支三后端都得拦。
SYNC_CONTROL_FILES = {
    "文件": "crates/qx-storage/src/lib.rs",
    "SQLite": "crates/qx-storage/src/sqlite.rs",
    "PostgreSQL": "crates/qx-storage/src/postgres.rs",
}
SYNC_CONTROL_COMMON_PREFIX = "existing.len().min(records.len())"
SYNC_CONTROL_OLD_LENGTH_TEST = "existing.len() > records.len()"

# V13 R9：WS 通道不占路由表，任何带 Upgrade: websocket 的 target 都会走到准入这一支，
# 所以查询键拒绝必须点名请求实际打到的路径，不能硬写 /events/live。
WS_ADMISSION_FILE = "crates/qx-api/src/ws.rs"
WS_ADMISSION_SIGNATURE = "fn admit_websocket"
WS_REFUSED_ACTUAL_ROUTE = "{path} 不接受查询参数 {name}"
WS_REFUSED_HARDCODED_ROUTE = "/events/live 不接受查询参数"


def audit_sync_control_prefix_check() -> None:
    """sync_control 三后端共用一枚公共前缀判据，不把长度当分叉信号。"""
    blind = []
    for label, path in SYNC_CONTROL_FILES.items():
        body = _code_body(path, "pub fn sync_control")
        if SYNC_CONTROL_COMMON_PREFIX not in body or SYNC_CONTROL_OLD_LENGTH_TEST in body:
            blind.append(label)
    check(
        not blind,
        "sync_control 三后端共用一枚公共前缀判据：链比请求长但前缀一致按幂等收口，只有前缀内容对不上才报 Conflict",
        f"{blind} 的 sync_control 仍按长度判分叉：sqlite/postgres 的 sync_control 跑在控制面事务 "
        "commit 之后，两进程并发时链天然比先跑的那方长，那一支会被报成 Conflict——控制面状态已经"
        "提交成功，transact_control 却返回 Err。三后端同一枚判据也是换后端不换结论的前提",
    )


def websocket_refused_param_route_check() -> None:
    """WS 的查询键拒绝点名请求实际打到的 target。"""
    body = _code_body(WS_ADMISSION_FILE, WS_ADMISSION_SIGNATURE)
    check(
        WS_REFUSED_ACTUAL_ROUTE in body and WS_REFUSED_HARDCODED_ROUTE not in body,
        "WS 的查询键拒绝点名请求实际打到的 target，不硬写 /events/live",
        "WS 通道不占路由表，任何带 Upgrade: websocket 的 target 都会走到准入这一支；硬写 "
        "/events/live 会让连到别的路径的客户端收到谎报的路由名，运维照着那句话去查会查错地方",
    )


# V13 R10：实盘与恢复路径上的三处静默降级。成交回报的 trade_id 是 seen_fill_keys
# 去重键的一部分，归零会让两笔都没有 t 的成交互相误去重（漏计一笔且无人告警）；
# 恢复 worker 的 venue_id 缺省会静默收窄扫描范围、漏掉其他 venue 的裸腿；
# worker 线程 panic 的 payload 被丢成一句 "panic" 则把故障定位的工作量推回现场。
BINANCE_FILL_BODY = ("crates/qx-adapter/src/binance.rs", "fn ingest_user_event")
WORKER_ENTRY_FILE = "crates/qx-cli/src/worker_entry.rs"
WORKER_ENTRY_HELPER = "pub(crate) fn recovery_worker_venue_id"
WORKER_SHUTDOWN_FILE = "crates/qx-cli/src/worker_shutdown.rs"


def live_path_fail_closed_check() -> None:
    """实盘与恢复路径的三处静默降级都必须 fail-closed。"""
    binance_body = _code_body(*BINANCE_FILL_BODY)
    check(
        'get("t")' in binance_body
        and "ok_or_else" in binance_body
        and ".unwrap_or(0)" not in binance_body,
        "Binance 成交回报缺 trade_id(t) 时转对账，不得归零",
        "trade_id 是 seen_fill_keys 去重键的一部分：归零会让两笔都没有 t 的成交"
        "互相误去重，漏计一笔且无人告警。CCXT 侧对同一情形本来就 fail-closed",
    )
    worker_entry = production_text((ROOT / WORKER_ENTRY_FILE).read_text(encoding="utf-8"))
    check(
        WORKER_ENTRY_HELPER in worker_entry
        and worker_entry.count("recovery_worker_venue_id(&worker)?") == 2
        and 'unwrap_or_else(|| "BINANCE".into())' not in worker_entry
        and 'unwrap_or_else(|| "ccxt".into())' not in worker_entry,
        "两个多腿恢复 worker 都必须显式配置 venue_id，不得静默默认",
        "恢复扫描按 venue 过滤敞口腿，空值在过滤函数里是有意的通配符；产出非空默认值"
        "等于把通配符路径变成不可达，配置遗漏时 worker 会静默只扫一个 venue、"
        "漏掉其余 venue 的裸腿",
    )
    shutdown = _code_body(WORKER_SHUTDOWN_FILE, "fn join_worker_handle")
    check(
        "{error:?}" in shutdown and 'format!("{label} panic")' not in shutdown,
        "worker 线程 panic 的报错保留 payload，不丢成一句 'panic'",
        "丢掉 JoinError 的 Debug 输出，运维拿到的是「线程挂了」而看不到挂在哪里，"
        "等于把故障定位的工作量推回现场",
    )


def untrusted_input_boundaries_check() -> None:
    """不可信输入的三处边界：WebSocket Close 帧、托管子进程退出码、C ABI 插件的 side。"""
    ws = (ROOT / "crates/qx-api/src/ws.rs").read_text(encoding="utf-8")
    check(
        "struct WsCloseScanner" in ws
        and "opcode == 0x8" in ws
        and "WS_CLOSE_PENDING_CAP" in ws
        and "self.pending.len() > WS_CLOSE_PENDING_CAP" in ws
        and ".any(|byte| (*byte & 0x0f) == 0x8)" not in ws,
        "WebSocket Close 检测按帧头判 opcode，累积缓冲必须有上界",
        "Close 帧的 opcode 0x8 只出现在帧起点。按整个读缓冲逐字节扫 `& 0x0f == 0x8`，"
        "256 个字节值里有 16 个会命中（0x08/0x18/…/0xF8），二进制行情载荷里这类字节很常见，"
        "客户端发一个正常的 text/binary 帧就会把服务端静默断连。改成跨读累积之后又多了"
        "一个原先不存在的内存面：客户端每次只给一两字节、故意不完成帧头，缓冲就能无限长大，"
        "所以上界越界必须 fail-closed 而不是继续攒",
    )
    supervisor = production_text(
        (ROOT / "crates/qx-orchestrator/src/supervisor_stop.rs").read_text(encoding="utf-8")
    )
    check(
        "struct ProcessExit" in supervisor
        and "pub raw_code: Option<i32>" in supervisor
        and "Result<Option<ProcessExit>, String>" in supervisor,
        "托管子进程的退出码以 Option<i32> 携带，不得先转成字符串",
        "status.code() 转成字符串之后，panic 的 101、OOM 的 137 和干净退出的 0 在监控里"
        "长得一样，父进程只能一律按 2 退出——故障分类的工作量被推回读日志",
    )
    c_api = production_text((ROOT / "crates/qx-strategy/src/c_api.rs").read_text(encoding="utf-8"))
    check(
        "pub const QX_ORDER_SIDE_BUY: u32" in c_api
        and "pub side: u32" in c_api
        and "C ABI intent side 非法" in c_api
        and "pub enum QxOrderSide" not in c_api,
        "C ABI 的 side 是定宽整数加显式拒绝，不得是 #[repr(C)] 枚举",
        "插件把 side 写进宿主内存。Rust 读取一个不在已声明判别值里的 #[repr(C)] 枚举值"
        "本身就是未定义行为，match 里没有任何可达的拒绝臂——签名过的插件写 3 就能在宿主上"
        "触发 UB。布局改成 u32 后两侧字节完全不变，既有插件无需重编译",
    )


def resource_lifecycle_and_lock_reentrancy_check() -> None:
    """资源持有者的错误路径覆盖，与连接池内 Mutex 重入。"""
    postgres = production_text((ROOT / "crates/qx-storage/src/postgres.rs").read_text(encoding="utf-8"))
    control_block = postgres[postgres.index("pub fn transact_control<T, E, F>"):].split(
        "\n}"
    )[0]
    check(
        "drop(client);" in control_block
        and control_block.index("drop(client);") < control_block.index(".sync_control("),
        "PostgreSQL 控制面事务在同步审计尾部之前必须先放掉池内连接锁",
        "`sync_control` 内部走 `read()` 会再取一次 `lock_client()`，而池索引是 "
        "`next_client % clients.len()`——池容量为 1 时两次都落到 `clients[0]`，"
        "`std::sync::Mutex` 不可重入就是当场自死锁：控制面事务已经提交成功，"
        "却永远回不了调用方，API 受理与 worker 回写两条生产路径全停。"
        "`pool_size` 合法范围含 1（1..=128），不是假设中的边界值",
    )
    ccxt = production_text((ROOT / "crates/qx-adapter/src/ccxt.rs").read_text(encoding="utf-8"))
    spawn_body = _fn_body(ccxt, "pub fn spawn(")
    check(
        "CCXT Worker stdin 不可用" in spawn_body
        and "CCXT Worker stdout 不可用" in spawn_body
        and spawn_body.count("let _ = child.kill();") >= 2
        and spawn_body.count("let _ = child.wait();") >= 2
        and ".ok_or_else(|| \"CCXT Worker stdin 不可用\".to_string())" not in spawn_body,
        "CCXT 子进程 spawn 之后的管道取失败必须先把子进程收掉",
        "`Stdio::piped()` 之后 `stdin`/`stdout` 必为 `Some`，这条路径今天走不到；"
        "但上游一旦把 worker 改成无管道模式，裸 `?` 就会留下一个无人回收的孤儿 Python 进程。"
        "`strategy_host.rs` 的同一处已经有 kill+wait，两处口径不该不一致。"
        "判据只读 `spawn` 的函数体：`call()` 里那次 `take()` 是取用完就 `Some(handle)` "
        "还给自己的恢复式取用，语义不同，不在此列",
    )
    # 监督器同一族的那处 `take()`：上一轮只钉了 ccxt，orchestrator 与 strategy_host 的同类
    # 站点没人核对，而这里的修法与 ccxt 那条**不同**——它必须把子进程挂进台账，由唯一那处
    # 回收点按预算轮询收摊，而不是就地一次无条件 `child.wait()`（那条纪律本身就有判据）。
    supervisor = production_text((ROOT / ORCHESTRATOR_ROOT_FILE).read_text(encoding="utf-8"))
    supervise_body = _fn_body(supervisor, "pub fn supervise_workers(")
    check(
        supervise_body.count("child.stdin.take()") == 1
        and "stop_channel: None" in supervise_body
        and supervise_body.count("children.push(ManagedChild {") == 2,
        "监督器唯一的 stdin 取用在失败路径上先把 worker 挂进台账（两处 push：None 那一格与正常那一格）",
        f"take 站点 {supervise_body.count('child.stdin.take()')} 处（期望 1）、"
        f"挂进台账的 None 分支 {'stop_channel: None' in supervise_body}、"
        f"ManagedChild 入册 {supervise_body.count('children.push(ManagedChild {')} 处（期望 2）"
        "——裸 `?` 或只留一处 push 都会让那条错误路径绕过唯一的回收点",
    )
    check(
        "child.wait()" not in supervise_body and ".kill();" not in supervise_body,
        "监督器不在取用失败处就地 kill+wait：回收只有 stop_managed_children 那一处（按预算轮询 try_wait）",
        "第二份收尾实现回来了，而且带着一句可以停到天荒地老的无条件 wait",
    )
    strategy_host = production_text((ROOT / "crates/qx-cli/src/strategy_host.rs").read_text(encoding="utf-8"))
    check(
        "struct RingFileGuard" in strategy_host
        and "impl Drop for RingFileGuard" in strategy_host
        and "ring_guard.register(" in strategy_host
        and "ring_guard.disown()" in strategy_host,
        "共享环临时文件从路径生成那一刻就有清理守卫接管",
        "`.input`/`.output` 是 memmap2 打开的 mmap 文件，默认 64 MiB 一个。正常路径由 "
        "`PythonStrategyClient` 的 `Drop` 回收，但 `SharedRingWriter::create` 与 "
        "`SharedRingReader::open` 任意一步失败时 `ring_paths` 还没被赋值，那条 `Drop` 不触发，"
        "`%TEMP%` 里就永久留下一对几十 MiB 的孤儿文件",
    )


def launcher_pregate_check() -> None:
    """两个平台的进程托管启动器都必须有 supervise 之前的 runtime-check 前置闸门。"""
    sh = (ROOT / "deploy/start-qianxing.sh").read_text(encoding="utf-8")
    ps1 = (ROOT / "deploy/start-qianxing.ps1").read_text(encoding="utf-8")
    # 顺序判据只能读可执行行：两个脚本的注释块都先提到 `supervise`（解释为什么要有这道
    # 闸门），按整份文本取 index 会把注释里的名字当成第一次出现，判据当场变红或变绿。
    # bash 与 PowerShell 都是 `#` 起头到行尾。
    code_only = {name: "\n".join(
        line for line in text.splitlines() if not line.lstrip().startswith("#")
    ) for name, text in (("start-qianxing.sh", sh), ("start-qianxing.ps1", ps1))}
    check(
        all("runtime-check" in code for code in code_only.values())
        and all(code.index("runtime-check") < code.index("supervise") for code in code_only.values()),
        "两个平台的托管启动器都在 supervise 之前跑 runtime-check",
        "`supervise` 只走 `plan_workers` 的拓扑校验，不检查配置引用的文件是否真的存在"
        "（数据集 bundle、研究快照、秘密文件的存在性只在 runtime-check / live-check 里查）。"
        "缺一个文件时各 worker 会各自在启动阶段失败，监督器随后按 fail-fast 把其余进程停掉："
        "7 个子进程被拉起又杀掉、process-logs 里落下日志，而不是在任何子进程起来之前就拒绝。"
        "修前 `start-qianxing.ps1` 有这道闸门、`start-qianxing.sh` 没有，"
        "Linux/macOS 上的坏配置与 Windows 走的是两条不同的失败路径",
    )
    check(
        "exit 2" in sh
        and "exit \"$?\"" not in sh,
        "bash 启动器的闸门失败要退出非零，不能把 $? 当退出码",
        "`if ! cmd; then echo ...; exit \"$?\"; fi` 里的 `$?` 拿的是上一条 `echo` 的状态（0），"
        "调用方会以为闸门通过了：部署平台按 0 退出继续走下一步，而实际上一个子进程都没起来。"
        "固定 `exit 2` 与本脚本另外两处拒绝（配置缺失、可执行文件缺失）同码",
    )


def schema_defense_consistency_check() -> None:
    """契约未知字段口径、因子缺值策略词表、存储读回侧的 u32 范围检查。"""
    strategy_py = (ROOT / "python/qianxing_bridge/strategy.py").read_text(encoding="utf-8")
    input_body = strategy_py[strategy_py.index('def from_dict(cls, value: Mapping[str, Any]) -> "StrategyInput":'):]
    input_body = input_body[: input_body.index("@classmethod")]
    check(
        '_reject_unknown_keys(value, cls, "strategy input")' in input_body,
        "Python StrategyInput.from_dict 拒绝契约之外的键",
        "同一份文件里的 StrategyIntent / StrategyOutput 的 from_dict 都调用了 `_reject_unknown_keys`，"
        "只有 StrategyInput 漏了：策略作者把 `positions` 拼成 `positionz` 时，输入侧静默拿到空 dict，"
        "策略可能以为账户是空的而误触发。允许集从 dataclass 字段派生，SDK 加字段时不会变成"
        "第二份手工同步清单",
    )
    factor = production_text((ROOT / "crates/qx-factor/src/lib.rs").read_text(encoding="utf-8"))
    report_body = _fn_body(factor, "impl FactorReport {\n    pub fn validate")
    check(
        'self.missing_policy.as_str(), "reject" | "skip" | "zero"' in report_body,
        "FactorReport::validate 校验 missing_policy 词表",
        "`FactorReport.missing_policy` 从 config 抄来（`run_analysis`），但报告本身能被 `from_json` "
        "反序列化。只有 `trim().is_empty()` 校验时，手改一份报告 JSON 把值写成 `\"whatever\"` 会放行，"
        "而下游 `resolve_missing` 拿到它才报「missing_policy 非法」——报错落点离改错的地方很远，"
        "与 `FactorConfig::validate` 的口径不一致",
    )
    sqlite = production_text((ROOT / "crates/qx-storage/src/sqlite.rs").read_text(encoding="utf-8"))
    postgres = production_text((ROOT / "crates/qx-storage/src/postgres.rs").read_text(encoding="utf-8"))
    check(
        "fn parse_sqlite_u32" in sqlite
        and "fn parse_u32" in postgres
        and "outbox.attempts\")? as u32" not in sqlite
        and 'outbox.attempts")? as u32' not in postgres,
        "outbox.attempts 读回走范围检查而不是 as u32 截断",
        "`attempts` 以 TEXT 落盘（sqlite 侧 `parse_sqlite_u64`、postgres 侧 `u64_text`），"
        "原先读回是 `... as u32`：u64→u32 是真截断，手工改库写成超过 u32::MAX 会回绕成一个小值，"
        "被误判成「仍在正常重试」而绕开死信判定。写侧本身上界 8，正常链路碰不到。"
        "`schema_version` 那一格不是同一个问题：sqlite 侧以 i64 落盘、`i64 as u32` 是真截断（已修）；"
        "postgres 侧以 INTEGER(i32) 落盘，`i32 as u32` 是 32 位↔32 位按位重解释，"
        "`((x as i32) as u32) == x` 恒成立，无损，保留原样",
    )
    check(
        'self.missing_policy.as_str(), "reject" | "skip" | "zero"' in _fn_body(
            factor, "impl FactorAnalysisConfig {\n    fn validate"
        ),
        "FactorAnalysisConfig::validate 仍是 missing_policy 词表的源头口径",
        "上面那颗判据守的是 FactorReport 补齐了 FactorAnalysisConfig 的口径；这颗守源头本身没被改弱——"
        "两处词表如果不一致，改哪一侧都会让另一侧的校验变成摆设",
    )


BASELINE_FILE = ROOT / "maturity" / "baseline.yaml"
BASELINE_MIN_FROZEN = 12
BASELINE_IDENTITY_FIELDS = (
    "version",
    "git_commit",
    "target_triple",
    "profile",
    "schema_registry_version",
    "sha256",
    "sbom",
)


def baseline_freeze_check() -> None:
    """M0 基线冻结（docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md §7 M0）。

    版本常量散在十几个 crate 里，改一处不影响另一处；把期望值收进 maturity/baseline.yaml 之后，
    源码与台账任一侧漂移都会在这里变红。同一条判据顺带守住「发布版本三处一致」与「产物身份字段齐全」，
    把 P2-4 发布供应链的前置条件从文档承诺变成会红的判据。
    """
    if not BASELINE_FILE.is_file():
        check(
            False,
            "M0 基线冻结清单存在",
            f"缺失 {BASELINE_FILE.relative_to(ROOT).as_posix()}",
        )
        return
    text = BASELINE_FILE.read_text(encoding="utf-8")
    entries = re.findall(
        r'^\s{2}([a-z0-9_]+):\s*"([^"]+?)::([A-Z][A-Z0-9_]*)=([^"]+)"\s*$',
        text,
        re.MULTILINE,
    )
    check(
        len(entries) >= BASELINE_MIN_FROZEN,
        f"基线清单登记的冻结版本不少于 {BASELINE_MIN_FROZEN} 条",
        f"只解析到 {len(entries)} 条（少登记一条就少一道牙齿）",
    )
    mismatches: list[str] = []
    for ident, rel, const, expected in entries:
        source = ROOT / rel
        if not source.is_file():
            mismatches.append(f"{ident}: 源文件不在盘上 {rel}")
            continue
        found = re.search(
            rf"\bconst\s+{const}\s*:\s*[^=]+?=\s*([^;]+);",
            source.read_text(encoding="utf-8"),
        )
        if found is None:
            mismatches.append(f"{ident}: 源码里找不到常量 {const}")
            continue
        actual = found.group(1).strip().strip('"')
        if actual != expected:
            mismatches.append(f"{ident}: {const} 源码={actual} 基线={expected}")
    check(
        not mismatches,
        "冻结版本与源码常量逐条一致（任一侧漂移即红）",
        "；".join(mismatches),
    )
    cargo = re.search(
        r"\[workspace\.package\][\s\S]*?\nversion\s*=\s*\"([^\"]+)\"",
        (ROOT / "Cargo.toml").read_text(encoding="utf-8"),
    )
    pyproject = re.search(
        r'^version\s*=\s*"([^"]+)"',
        (ROOT / "python" / "pyproject.toml").read_text(encoding="utf-8"),
        re.MULTILINE,
    )
    baseline = re.search(r'^release_version:\s*"([^"]+)"', text, re.MULTILINE)
    versions = {
        "baseline": baseline.group(1) if baseline else None,
        "cargo": cargo.group(1) if cargo else None,
        "pyproject": pyproject.group(1) if pyproject else None,
    }
    check(
        None not in versions.values() and len(set(versions.values())) == 1,
        "发布版本三处一致（baseline / Cargo.toml / pyproject.toml）",
        f"现读 {versions}",
    )
    missing_fields = [
        field
        for field in BASELINE_IDENTITY_FIELDS
        if not re.search(rf"^\s*-\s*{field}\s*$", text, re.MULTILINE)
    ]
    check(
        not missing_fields,
        "发布产物身份字段齐全（version/commit/triple/profile/schema/sha256/sbom）",
        f"缺 {missing_fields}",
    )


RELEASE_WORKFLOW = ROOT / ".github" / "workflows" / "release.yml"


def release_supply_chain_check() -> None:
    """P2-4 发布供应链：一个 tag 必须产出可复现的 binary / wheel / C++ SDK + SHA256 + SBOM +
    provenance，并挂到 Release。身份字段与 maturity/baseline.yaml 的 artifact_identity 同口径，
    否则「可复现发布件」只是文档里的一个词。"""
    if not RELEASE_WORKFLOW.is_file():
        check(
            False,
            "发布工作流存在",
            f"缺失 {RELEASE_WORKFLOW.relative_to(ROOT).as_posix()}",
        )
        return
    text = RELEASE_WORKFLOW.read_text(encoding="utf-8")
    check(
        re.search(r"^\s*tags:\s*$", text, re.MULTILINE) is not None and "v*" in text,
        "发布工作流按 tag 触发（v*）",
        "缺 tag 触发口径：没有它就只能手动发布，可复现性无从谈起",
    )
    required = {
        "binary 构建": "cargo build --release --locked -p qx-cli",
        "wheel 构建": "build_python_wheel.sh",
        "C++ SDK 构建": "cmake --build",
        "SHA256 清单": "sha256sum",
        "SBOM": "sbom.json",
        "构建来源证明": "attest-build-provenance",
        "Release 发布": "gh release create",
    }
    missing = [name for name, token in required.items() if token not in text]
    check(
        not missing,
        "发布工作流覆盖 binary/wheel/SDK/SHA256/SBOM/provenance/Release 七件",
        f"缺 {missing}",
    )
    web_package = ROOT / "tools/package_web_console.py"
    web_package_test = ROOT / "python/tests/test_web_console_package.py"
    package_text = web_package.read_text(encoding="utf-8") if web_package.is_file() else ""
    package_tests = web_package_test.read_text(encoding="utf-8") if web_package_test.is_file() else ""
    package_cases = (
        "test_archive_is_reproducible_and_contains_verified_identity",
        "test_unavailable_or_external_resources_are_rejected",
        "test_release_identity_rejects_invalid_version_and_short_commit",
    )
    check(
        web_package.is_file()
        and all(token in package_text for token in (
            "ASSET_FILES",
            "IDENTITY_NAME",
            "release-identity.json",
            "hashlib.sha256",
            "mtime = 0",
            "SCHEMA_REGISTRY_FILE",
            "registry.get(",
            "distribution_boundary",
            "requires_same_origin_bff",
            "package_scope",
            "product_same_origin_bff",
            "product_csrf",
            "product_server_side_session",
            "product_desktop_host",
            "sandbox_accepted",
            "production_accepted",
        ))
        and web_package_test.is_file()
        and all(case in package_tests for case in package_cases),
        "Web 控制台发布包由确定性打包器生成，版本身份/资源摘要与三条行为契约用例在盘",
        f"打包脚本存在={web_package.is_file()}，测试存在={web_package_test.is_file()}，"
        f"缺用例={[case for case in package_cases if case not in package_tests]}",
    )
    web_job_start = re.search(r"(?m)^  web-console:\s*$", text)
    web_job = ""
    if web_job_start is not None:
        web_job_tail = text[web_job_start.start() :]
        next_job = re.search(r"(?m)^  [a-z][a-z0-9_-]*:\s*$", web_job_tail[1:])
        web_job = web_job_tail[: next_job.start() + 1] if next_job is not None else web_job_tail
    check(
        bool(web_job)
        and "tools/package_web_console.py" in web_job
        and "attest-build-provenance@v2" in web_job
        and "name: web-console" in web_job
        and "path: dist/*.tar.gz" in web_job,
        "tag 发布工作流打包 Web 控制台、为归档生成 provenance 并上传 web-console artifact",
        f"web-console job 片段不完整：{web_job[:240]!r}",
    )
    release_start = re.search(r"(?m)^  release:\s*$", text)
    release_job = text[release_start.start() :] if release_start is not None else ""
    release_needs = re.search(r"(?m)^\s+needs:\s*\[([^\]]+)\]", release_job)
    check(
        release_needs is not None
        and "web-console" in release_needs.group(1)
        and "actions/download-artifact@v4" in release_job
        and "dist/*" in release_job,
        "GitHub Release 汇集 Web 控制台 artifact，并纳入统一发布附件与 SHA256 清单",
        f"release needs={release_needs.group(1) if release_needs else None!r}",
    )
    identity_fields = (
        "version",
        "git_commit",
        "target_triple",
        "profile",
        "schema_registry_version",
        "sha256",
        "sbom",
    )
    missing_identity = [
        field for field in identity_fields if f'"{field}"' not in text
    ]
    check(
        not missing_identity,
        "发布身份文件登记了 baseline.artifact_identity 的全部字段",
        f"缺 {missing_identity}",
    )


BENCHMARK_DRIVER = ROOT / "benchmarks" / "run_baseline.py"


def performance_baseline_check() -> None:
    """M0 性能基线：驱动器必须存在，且 benchmarks/README.md 指向它。

    基线最容易退化成「曾经写过一份文档」——驱动器被删、README 还留着那张表格。
    两侧一起核对，删任一侧即红。
    """
    check(
        BENCHMARK_DRIVER.is_file(),
        "性能基线驱动器存在（benchmarks/run_baseline.py）",
        "缺失即基线无从复现",
    )
    readme = ROOT / "benchmarks" / "README.md"
    check(
        readme.is_file() and "run_baseline.py" in readme.read_text(encoding="utf-8"),
        "benchmarks/README.md 指向性能基线驱动器",
        "README 与驱动器必须互相点名，否则基线退化成一句话",
    )


# 地基规格对象（规划 §6.2 / §7）：八类声明式文档各自只有一份 JSON Schema、一个版本常量、
# 一处 `pub struct` 定义，且 `qx_spec::describe` 是它们唯一的生产读入漏斗（CLI `plan` 命令）。
FOUNDATION_SPECS = (
    ("schemas/project-manifest-v1.json", "crates/qx-spec/src/project.rs", "PROJECT_MANIFEST_SCHEMA_VERSION", "schema_version"),
    ("schemas/dataset-manifest-v2.json", "crates/qx-data/src/catalog_v2.rs", "DATASET_MANIFEST_V2_SCHEMA_VERSION", "manifest_version"),
    ("schemas/experiment-spec-v1.json", "crates/qx-spec/src/experiment.rs", "EXPERIMENT_SPEC_SCHEMA_VERSION", "schema_version"),
    ("schemas/run-record-v1.json", "crates/qx-spec/src/run_record.rs", "RUN_RECORD_SCHEMA_VERSION", "schema_version"),
    ("schemas/run-evidence-v1.json", "crates/qx-spec/src/run_evidence.rs", "RUN_EVIDENCE_SCHEMA_VERSION", "schema_version"),
    ("schemas/capability-manifest-v1.json", "crates/qx-spec/src/capability.rs", "CAPABILITY_MANIFEST_SCHEMA_VERSION", "schema_version"),
    ("schemas/evidence-bundle-v1.json", "crates/qx-spec/src/evidence.rs", "EVIDENCE_BUNDLE_SCHEMA_VERSION", "schema_version"),
    ("schemas/schema-registry-v1.json", "crates/qx-spec/src/schema_registry.rs", "SCHEMA_REGISTRY_SCHEMA_VERSION", "schema_version"),
)
FOUNDATION_OBJECT_TYPES = (
    "ProjectManifest",
    "DatasetManifestV2",
    "ExperimentSpec",
    "RunRecord",
    "RunEvidenceBundle",
    "CapabilityManifest",
    "EvidenceBundle",
    "SchemaRegistry",
)
FOUNDATION_SPEC_CRATE = "qx-spec"
FOUNDATION_DISPATCH_FILE = "crates/qx-spec/src/lib.rs"


def foundation_specs_check() -> None:
    """地基规格对象：Schema 与 Rust 常量同源、每个对象只有一处定义、CLI 是唯一读入漏斗。

    规划（docs/qianxing-项目结构与GitHub竞品对比及优化方案-2026-10-06.md §6.2）要求这些对象
    成为「统一身份」，因此它们的失败方式就是漂移：Schema 与常量各写一份、同名概念出现第二份
    定义、或对象建好却没有任何生产读入者。三件事分别在这里变红。
    """
    schema_issues: list[str] = []
    for rel, rust_rel, const, version_key in FOUNDATION_SPECS:
        path = ROOT / rel
        if not path.is_file():
            schema_issues.append(f"{rel} 不在盘上")
            continue
        try:
            schema = json.loads(path.read_text(encoding="utf-8"))
        except ValueError as error:
            schema_issues.append(f"{rel} 不是合法 JSON: {error}")
            continue
        properties = schema.get("properties", {})
        required = schema.get("required", [])
        if "$id" not in schema:
            schema_issues.append(f"{rel} 缺 $id")
        if not required or not set(required) <= set(properties):
            schema_issues.append(
                f"{rel} 的 required 越界 {sorted(set(required) - set(properties))}"
            )
        if schema.get("additionalProperties") is not False:
            schema_issues.append(f"{rel} 未关闭 additionalProperties（未知字段必须当场拒绝）")
        declared = properties.get(version_key, {}).get("const")
        found = re.search(
            rf"pub const {const}: u32 = (\d+);",
            (ROOT / rust_rel).read_text(encoding="utf-8"),
        )
        if found is None:
            schema_issues.append(f"{rust_rel} 找不到常量 {const}")
        elif declared != int(found.group(1)):
            schema_issues.append(
                f"{rel} {version_key}.const={declared} 与 {const}={found.group(1)} 不一致"
            )
    check(
        not schema_issues,
        "八类地基规格各有一份严格 Schema，版本常量与 Rust 侧逐条一致",
        "；".join(schema_issues),
    )

    duplicates: dict[str, list[str]] = {}
    for name in FOUNDATION_OBJECT_TYPES:
        pattern = re.compile(rf"^\s*pub struct {name} \{{", re.MULTILINE)
        duplicates[name] = [
            path.relative_to(ROOT).as_posix()
            for path in sorted(CRATES.glob("*/src/**/*.rs"))
            if pattern.search(path.read_text(encoding="utf-8"))
        ]
    bad = {name: sites for name, sites in duplicates.items() if len(sites) != 1}
    check(
        not bad,
        "八类地基对象的 `pub struct` 各只有一处定义（同名概念不得出现第二份）",
        f"定义数不为 1 的对象 {bad}",
    )

    workspace = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    cli_manifest = (ROOT / "crates" / "qx-cli" / "Cargo.toml").read_text(encoding="utf-8")
    check(
        f'"crates/{FOUNDATION_SPEC_CRATE}"' in workspace
        and f'qx-spec = {{ path = "../{FOUNDATION_SPEC_CRATE}" }}' in cli_manifest,
        "qx-spec 是 workspace 成员且被 qx-cli 依赖（地基对象必须有生产读入者）",
        f"workspace 成员={'是' if f'crates/{FOUNDATION_SPEC_CRATE}' in workspace else '否'}；"
        f"qx-cli 依赖={'是' if 'qx-spec' in cli_manifest else '否'}",
    )

    body = _fn_body(
        (ROOT / FOUNDATION_DISPATCH_FILE).read_text(encoding="utf-8"),
        "pub fn describe(",
    )
    arms = re.findall(r"FoundationKind::(\w+) =>", body)
    check(
        len(arms) == len(FOUNDATION_OBJECT_TYPES) and len(set(arms)) == len(arms),
        "describe 的派发臂覆盖全部八类地基对象且不重不漏",
        f"派发臂 {arms}（期望 {len(FOUNDATION_OBJECT_TYPES)} 条）",
    )

    project = (ROOT / "crates/qx-cli/src/project_manifest.rs").read_text(encoding="utf-8")
    initializer = (ROOT / "crates/qx-cli/src/init_project.rs").read_text(encoding="utf-8")
    project_cases = (ROOT / "crates/qx-cli/src/tests/project_manifest_init.rs").read_text(encoding="utf-8")
    check(
        "write_init_project_manifest(" in initializer
        and 'qx_spec::describe("project", &payload)' in project
        and "project-manifest-init" in project_cases
        and "project-manifest-strategy-init" in project_cases,
        "ProjectManifest 由 init 与 strategy init 写出，并经统一规格漏斗与两条首跑用例验证",
        "项目清单定义、生产接线或正向用例断链",
    )

    record = (ROOT / "crates/qx-cli/src/backtests/run_record.rs").read_text(encoding="utf-8")
    artifacts = (ROOT / "crates/qx-cli/src/backtests/artifacts.rs").read_text(encoding="utf-8")
    provenance_cases = (ROOT / "crates/qx-cli/src/tests/backtest_input_provenance.rs").read_text(encoding="utf-8")
    check(
        "persist_verified_run_record(manifest_path, &summary_path, &equity_path, &fills_path)?" in artifacts
        and "verify_declared_run_record(summary)?;" in artifacts
        and "qx_strategy::file_digest::sha256_file_hex(path)" in record
        and "report_refuses_when_a_run_record_artifact_digest_no_longer_matches" in provenance_cases,
        "v5 摘要在重放验证后写 RunRecord，report 重算产物摘要且篡改用例会拒绝",
        "RunRecord 的写侧/读侧/反向验证链有断点",
    )

    capability = (ROOT / "crates/qx-spec/src/capability.rs").read_text(encoding="utf-8")
    check(
        "self.sandbox_tested && self.evidence.is_empty()" in capability
        and "self.production_approved && !self.sandbox_tested" in capability,
        "能力清单把「未拿到沙盒/生产证据不得声明已通过」写成对象层硬约束",
        "capability.rs 的 evidence 闸门被删弱（声明已通过却没有证据）",
    )

    # DatasetManifestV2 的第二个生产写侧（`data-validate`）必须是「只读诊断」而不是又一条严格读链：
    # 复用 `BarFrame::from_json`/`JsonBarFrameProvider` 会让脏数据只剩一句拒绝，质量报告反而写不出来。
    # 四件事一起钉：容忍性（不借严格读侧）、缺口算式、清单过规格校验、入口写进 help。
    validator = (ROOT / "crates/qx-cli/src/data_validate.rs").read_text(encoding="utf-8")
    help_text = (ROOT / "crates/qx-cli/src/cli_help.rs").read_text(encoding="utf-8")
    check(
        "fn assess_frame(" in validator
        and "div_ceil(interval_ms)" in validator
        and "BarFrame::from_json" not in validator
        and "JsonBarFrameProvider" not in validator
        and "manifest.validate()?;" in validator
        and "data-validate <bar-frame.json>" in help_text,
        "DatasetManifestV2 的只读诊断入口容忍脏时间轴、不借严格读侧、清单过规格校验且写进 help",
        "data-validate 链的容忍性、规格校验或入口文案有断点",
    )

    # 八类「统一身份」对象必须有一个**使用者可达**的读入入口：`describe` 是唯一漏斗，但若没有
    # CLI 入口，使用者就只能间接经过 init / 回测链看到其中几类，另外几类无处读入自己的清单。
    plan_commands = (ROOT / "crates/qx-cli/src/plan_commands.rs").read_text(encoding="utf-8")
    check(
        "pub(crate) fn plan_readout(" in plan_commands
        and "describe(&args.kind, &payload)" in plan_commands
        and "plan <kind> <file>" in help_text,
        "八类地基对象有使用者可达的读入入口（plan <kind> <file> 复用同一份 describe 漏斗）",
        "地基对象只有进程内读入者、缺使用者可达的 CLI 入口",
    )


# —— 契约层与登记层（规划 §6.2 / §13.2 / §14.3 / §17 / §18 M0）——
# 地基对象（上一轮的七类）只解决了「对象存在」；这一层解决「对象被真实契约、真实场景、
# 真实依赖规则和真实目标台账钉住」。本仓库不引 yaml 依赖，所以这里的 YAML 一律按行手写解析：
# 格式必须保持「顶层键: / 两空格子键: / 四空格字段: 值」的规整形状，改坏缩进会当场变红。

STRATEGY_INPUT_SCHEMA = ROOT / "schemas" / "strategy-api-input-v1.json"
STRATEGY_INPUT_TEST = ROOT / "crates" / "qx-runtime" / "tests" / "strategy_contract_input_schema.rs"
TARGETS_FILE = ROOT / "maturity" / "targets.yaml"
LEVELS_FILE = ROOT / "maturity" / "levels.yaml"
SCHEMA_REGISTRY_FILE = ROOT / "maturity" / "schema-registry.json"
SCENARIO_FIXTURES_DIR = ROOT / "maturity" / "fixtures" / "scenarios"
SCENARIO_FIXTURES_TEST = ROOT / "crates" / "qx-spec" / "tests" / "scenario_fixtures.rs"
SCENARIO_REQUIRED = (
    "ashare-equity",
    "cn-futures",
    "cn-options",
    "global-equity",
    "fx-cfd",
    "crypto-spot",
    "crypto-perpetual",
)
TARGETS_DIMENSIONS = (
    "correctness",
    "data",
    "availability",
    "durability",
    "recovery",
    "latency",
    "security",
    "observability",
    "compatibility",
    "performance",
)
TARGETS_TOPOLOGIES = ("single_node", "distributed")
LEVEL_IDS = ("L0", "L1", "L2", "L3", "L4")
LEVEL_SUBJECT_GROUPS = ("markets", "venues", "order_types", "strategy_languages")

# 数据/策略/控制层共同禁止依赖的「下游应用面」：下单、执行、适配器、API、存储、运行时。
# 抽成常量而不是逐条抄写——逐条抄写会在这里造出 10 行以上的逐字重复窗口，被本文件自己的
# merge_duplicate_block_check 抓住（本轮实测踩到过）。
LAYER_APP_SIDE_CRATES = (
    "qx-adapter",
    "qx-api",
    "qx-cli",
    "qx-execution",
    "qx-risk",
    "qx-runtime",
    "qx-storage",
    "qx-xingban",
    "qx-zhenlu",
)

# §15.2 的门面集合：应用层**不得**反向依赖它们。单独一份而不是复用 LAYER_APP_SIDE_CRATES——
# 应用层正当地依赖 qx-xingban / qx-zhenlu / qx-risk 这些领域件（它要装配它们），
# 所以"下游应用面"那份名单对 qx-app 太宽，直接复用会把合法依赖判成违规。
LAYER_FACADE_CRATES = (
    "qx-adapter",
    "qx-api",
    "qx-cli",
    "qx-execution",
    "qx-python",
    "qx-runtime",
    "qx-storage",
)

# §14.3 的依赖规则：以**当前真实依赖图**为基线。规则不是「希望如此」，而是
# 「今天就是这样，谁改谁红」——新增一条违规边，门禁当场失败。
LAYER_FORBIDDEN_DEPS = (
    ("qx-data", LAYER_APP_SIDE_CRATES, "§14.3-2 qx-data 只管数据身份与质量，不下单、不记账"),
    ("qx-strategy", LAYER_APP_SIDE_CRATES, "§14.3-3 qx-strategy 只产生 decision/intent，不产生 Accepted/Fill"),
    ("qx-risk", ("qx-adapter", "qx-api", "qx-cli", "qx-execution", "qx-runtime", "qx-storage", "qx-xingban"), "§14.3-4 qx-risk 只裁决风险（可依赖 qx-zhenlu 的订单类型，但不得依赖适配器/执行/存储/API）"),
    ("qx-xingban", ("qx-adapter", "qx-api", "qx-cli", "qx-execution", "qx-storage"), "§14.3-5 qx-xingban 只做研究/回测撮合与成本模型"),
    ("qx-control", LAYER_APP_SIDE_CRATES, "§14.3-8 qx-control 只收命令，执行者由应用层提供"),
    ("qx-storage", ("qx-adapter", "qx-cli", "qx-execution", "qx-risk", "qx-runtime", "qx-strategy", "qx-xingban"), "§14.3-9 qx-storage 只提供持久化端口与事务边界"),
    ("qx-app", LAYER_FACADE_CRATES, "§15.2 门面依赖应用层（facade → application → domain/ports），应用层不得反向依赖任何门面/适配/执行/存储"),
)
# §14.3-6/7/10 是「谁可以依赖谁」的反向规则：某个 crate 的**被依赖集合**必须被钉死。
LAYER_SOLE_DEPENDENTS = (
    (
        "qx-adapter",
        ("qx-execution", "qx-cli", "contract-tests"),
        "§14.3-6 qx-execution 是外部回报到执行事实的唯一转换层（qx-adapter 只能被它与 CLI 依赖；"
        "`contract-tests` 是 P1-12 建的纯测试宿主，只在 dev 边上读它做三家 venue 回报契约，无生产依赖）",
    ),
    (
        "qx-api",
        ("qx-cli",),
        "§14.3-7 API/报告/快照/指标都是投影（qx-api 只能被 CLI 依赖，领域层不得反向依赖）",
    ),
    ("qx-cli", (), "§14.3-10 qx-cli 是叶子（不得被任何 crate 反向依赖）"),
    (
        "qx-app",
        ("qx-cli", "qx-api", "qx-python", "contract-tests"),
        "§15.2 应用层只被门面依赖（CLI / HTTP API / Python SDK）；领域层与其它 crate 不得反向依赖它",
    ),
)


def _block_lines(text: str, header: str) -> list[str]:
    """取顶层 `header:` 映射块里缩进 >= 2 的行（到下一个顶格非空行为止）。"""
    out: list[str] = []
    inside = False
    for line in text.splitlines():
        if re.match(rf"^{re.escape(header)}:\s*$", line):
            inside = True
            continue
        if inside:
            if line.strip() and not line.startswith(" "):
                break
            out.append(line)
    return out


def _yaml_groups(block: list[str], indent: int = 2) -> dict[str, dict[str, str]]:
    """把 `  name:` + `    key: value` 的块解析成 {name: {key: value}}。"""
    groups: dict[str, dict[str, str]] = {}
    current: str | None = None
    for line in block:
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        lead = len(line) - len(line.lstrip())
        if lead == indent:
            matched = re.match(rf"^\s{{{indent}}}([A-Za-z0-9_.\-]+):\s*(.*)$", line)
            if matched:
                current = matched.group(1)
                groups.setdefault(current, {})
                if matched.group(2).strip():
                    groups[current]["__value__"] = matched.group(2).strip().strip('"')
                continue
        if lead > indent and current is not None:
            matched = re.match(r"^\s*([A-Za-z0-9_.\-]+):\s*(.*)$", line)
            if matched:
                groups[current][matched.group(1)] = matched.group(2).strip().strip('"')
    return groups


def _levels_subjects(text: str) -> dict[str, dict[str, dict[str, str]]]:
    """解析 `subjects:` → 类别 → 主体 → {level, evidence}。"""
    result: dict[str, dict[str, dict[str, str]]] = {}
    category: str | None = None
    subject: str | None = None
    for line in _block_lines(text, "subjects"):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        lead = len(line) - len(line.lstrip())
        if lead == 2:
            matched = re.match(r"^  ([A-Za-z0-9_\-]+):\s*$", line)
            if matched:
                category = matched.group(1)
                result.setdefault(category, {})
                subject = None
        elif lead == 4 and category is not None:
            matched = re.match(r"^    ([A-Za-z0-9_.\-]+):\s*$", line)
            if matched:
                subject = matched.group(1)
                result[category].setdefault(subject, {})
        elif lead == 6 and category is not None and subject is not None:
            matched = re.match(r"^      ([A-Za-z0-9_\-]+):\s*(.*)$", line)
            if matched:
                result[category][subject][matched.group(1)] = matched.group(2).strip().strip('"')
    return result


def _release_version() -> str | None:
    text = (ROOT / "maturity" / "baseline.yaml").read_text(encoding="utf-8")
    found = re.search(r'^release_version:\s*"([^"]+)"', text, re.MULTILINE)
    return found.group(1) if found else None


def _crate_internal_deps() -> dict[str, set[str]]:
    """逐 crate 解析内部依赖（内联表与 `[dependencies.qx-*]` 表头两种写法都认）。"""
    graph: dict[str, set[str]] = {}
    for manifest in sorted(CRATES.glob("*/Cargo.toml")):
        text = manifest.read_text(encoding="utf-8")
        deps = set(re.findall(r"^(qx-[a-z0-9-]+)\s*=\s*\{", text, re.MULTILINE))
        deps |= set(re.findall(r"^\[(?:dependencies|dev-dependencies|build-dependencies)\.(qx-[a-z0-9-]+)\]", text, re.MULTILINE))
        graph[manifest.parent.name] = deps
    return graph


def strategy_input_schema_check() -> None:
    """策略契约**输入方向**的正式 JSON Schema（规划 §4.2 第 8 条 / §18 M0）。

    在此之前只有 output 方向有 schema，输入方向「Rust 有结构、靠文档」。这里钉住文件在盘上、
    是严格 schema、声明的版本与 `qx_strategy::STRATEGY_API_VERSION` 同源，以及钉住它的用例存在
    （schema 与结构体的逐字段一致性由那条用例负责，静态门禁不重复实现 serde 语义）。
    """
    if not STRATEGY_INPUT_SCHEMA.is_file():
        check(
            False,
            "策略输入方向的 JSON Schema 存在",
            f"缺失 {STRATEGY_INPUT_SCHEMA.relative_to(ROOT).as_posix()}",
        )
        return
    try:
        schema = json.loads(STRATEGY_INPUT_SCHEMA.read_text(encoding="utf-8"))
    except ValueError as error:
        check(False, "策略输入 Schema 是合法 JSON", str(error))
        return
    issues: list[str] = []
    if "$id" not in schema:
        issues.append("缺 $id")
    if schema.get("additionalProperties") is not False:
        issues.append("未关闭 additionalProperties（未知字段必须当场拒绝）")
    properties = schema.get("properties", {})
    required = schema.get("required", [])
    if not required or not set(required) <= set(properties):
        issues.append(f"required 越界 {sorted(set(required) - set(properties))}")
    declared = properties.get("schema_version", {}).get("const")
    strategy_api = re.search(
        r"pub const STRATEGY_API_VERSION: u32 = (\d+);",
        (ROOT / "crates/qx-strategy/src/lib.rs").read_text(encoding="utf-8"),
    )
    if strategy_api is None:
        issues.append("qx-strategy 找不到 STRATEGY_API_VERSION")
    elif declared != int(strategy_api.group(1)):
        issues.append(f"schema_version.const={declared} 与 STRATEGY_API_VERSION={strategy_api.group(1)} 不一致")
    contract = (ROOT / "crates/qx-runtime/src/strategy_contract/contract.rs").read_text(encoding="utf-8")
    if "STRATEGY_CONTRACT_SCHEMA_VERSION: u32 = qx_strategy::STRATEGY_API_VERSION" not in contract:
        issues.append("contract.rs 的版本常量不再以 STRATEGY_API_VERSION 为单一来源")
    check(not issues, "策略输入方向有严格 Schema，且版本与策略 API 单一来源一致", "；".join(issues))
    check(
        STRATEGY_INPUT_TEST.is_file(),
        "策略输入 Schema 有钉住它的用例（逐字段对齐 Rust 结构体）",
        f"缺失 {STRATEGY_INPUT_TEST.relative_to(ROOT).as_posix()}",
    )
    # 输入方向此前只钉了 Rust ↔ schema（那条用例），Python SDK 不在这条链上——而两侧都按
    # `deny_unknown_fields` / `_reject_unknown_keys` fail-closed，Rust 加一格输入字段而 Python
    # 不跟，跨语言往返会当场 ValueError。意图方向早有 `strategy_intent_three_language_check` 三侧
    # 比对，输入方向照同一把尺子补齐：两侧都从「字段声明」这一事实取集合，不写第二份手工清单。
    rust_text = production_text((ROOT / STRATEGY_INTENT_RUST_FILE).read_text(encoding="utf-8"))
    rust_input = set(
        re.findall(
            r"^\s*pub ([a-z_0-9]+):",
            _fn_body(rust_text, "pub struct StrategyContractInput {"),
            re.MULTILINE,
        )
    )
    python_text = (ROOT / STRATEGY_INTENT_BRIDGE_FILE).read_text(encoding="utf-8")
    python_input = set(
        re.findall(
            r"^    ([a-z_0-9]+): \S[^\n]*$",
            python_text.split("class StrategyInput:", 1)[-1].split("\nclass ", 1)[0],
            re.MULTILINE,
        )
    )
    for name, group in (("JSON schema", set(properties)), ("Python SDK", python_input)):
        check(
            group == rust_input,
            f"策略输入字段集 {name} 与 Rust StrategyContractInput 逐项相等",
            f"多 {sorted(group - rust_input) or '无'}、少 {sorted(rust_input - group) or '无'}",
        )


def nonfunctional_targets_check() -> None:
    """工业级非功能目标台账（规划 §17）。

    §17 要求目标值以 profile / 场景 / 硬件 / 版本 / 证据路径为键，且不许把目标值当成当前实现现状。
    这里钉住九个场景 profile 一个不少、六个键齐全、硬件能在本文件里解析、十个维度三段齐全，
    以及最关键的诚实牙齿：`measured` 必须有真实证据路径，`unmeasured` 必须写 `unavailable`。
    """
    if not TARGETS_FILE.is_file():
        check(False, "非功能目标台账存在", f"缺失 {TARGETS_FILE.relative_to(ROOT).as_posix()}")
        return
    text = TARGETS_FILE.read_text(encoding="utf-8")
    declared = re.search(
        r"pub const PROJECT_PROFILES: \[&str; \d+\] = \[([\s\S]*?)\];",
        (ROOT / "crates/qx-spec/src/project.rs").read_text(encoding="utf-8"),
    )
    expected = set(re.findall(r'"([^"]+)"', declared.group(1))) if declared else set()
    hardware = _yaml_groups(_block_lines(text, "hardware_profiles"))
    dimensions = _yaml_groups(_block_lines(text, "dimensions"))
    targets = _yaml_groups(_block_lines(text, "profile_targets"))
    release = _release_version()

    issues: list[str] = []
    if not expected:
        issues.append("解析不到 PROJECT_PROFILES（空集会让这颗判据静默全绿）")
    missing = sorted(expected - set(targets))
    if missing:
        issues.append(f"缺 profile {missing}")
    extra = sorted(set(targets) - expected)
    if extra:
        issues.append(f"多出未登记 profile {extra}")
    for name, fields in sorted(targets.items()):
        for key in ("scenario", "topology", "hardware", "version", "measurement_status", "evidence"):
            if not fields.get(key):
                issues.append(f"{name} 缺 {key}")
        if fields.get("topology") not in TARGETS_TOPOLOGIES:
            issues.append(f"{name} topology={fields.get('topology')} 非法")
        if fields.get("hardware") and fields["hardware"] not in hardware:
            issues.append(f"{name} 的 hardware={fields['hardware']} 不在 hardware_profiles 内")
        if release and fields.get("version") != release:
            issues.append(f"{name} version={fields.get('version')} 与 release_version={release} 不一致")
        status = fields.get("measurement_status")
        evidence = fields.get("evidence", "")
        if status == "unmeasured":
            if evidence != "unavailable":
                issues.append(f"{name} 标未测量却写了证据 {evidence}（目标不得冒充现状）")
        elif status == "measured":
            if evidence == "unavailable" or not (ROOT / evidence).exists():
                issues.append(f"{name} 标已测量但证据路径落空 {evidence}")
        else:
            issues.append(f"{name} measurement_status={status} 非法")
    missing_dims = sorted(set(TARGETS_DIMENSIONS) - set(dimensions))
    if missing_dims:
        issues.append(f"dimensions 缺 {missing_dims}")
    for dim, fields in sorted(dimensions.items()):
        for key in ("research", "production", "acceptance"):
            if not fields.get(key):
                issues.append(f"维度 {dim} 缺 {key}")
    check(
        not issues,
        "非功能目标按 profile/场景/硬件/版本/证据登记，且目标不被写成现状",
        "；".join(issues),
    )


def capability_levels_check() -> None:
    """能力等级登记册（规划 §13.2）。

    §13.2 要求任何市场/Venue/订单类型/策略语言单独登记 L0–L4，且 L1 不代表 L3、L3 不自动代表 L4。
    这里钉住结构完整性，并用一条诚实牙齿把「先把等级调高」这种改字行为挡回去：
    `capabilities.yaml` 里 `sandbox_tested: true` 的条数为 0 时，本文件任何主体都不许声明 L3+。
    """
    if not LEVELS_FILE.is_file():
        check(False, "能力等级登记册存在", f"缺失 {LEVELS_FILE.relative_to(ROOT).as_posix()}")
        return
    text = LEVELS_FILE.read_text(encoding="utf-8")
    definitions = _yaml_groups(_block_lines(text, "level_definitions"))
    subjects = _levels_subjects(text)
    issues: list[str] = []
    if sorted(definitions) != sorted(LEVEL_IDS):
        issues.append(f"level_definitions={sorted(definitions)} 未覆盖 {list(LEVEL_IDS)}")
    for level, fields in sorted(definitions.items()):
        for key in ("name", "requires"):
            if not fields.get(key):
                issues.append(f"{level} 缺 {key}")
    for group in LEVEL_SUBJECT_GROUPS:
        if not subjects.get(group):
            issues.append(f"subjects.{group} 为空")
    capabilities = (ROOT / "maturity" / "capabilities.yaml").read_text(encoding="utf-8")
    sandbox_approved = len(re.findall(r"^\s+sandbox_tested:\s*true\s*$", capabilities, re.MULTILINE))
    highest = 0
    for group, entries in sorted(subjects.items()):
        for name, fields in sorted(entries.items()):
            level = fields.get("level", "")
            if level not in LEVEL_IDS:
                issues.append(f"{group}.{name} level={level} 非法")
                continue
            rank = int(level[1])
            highest = max(highest, rank)
            evidence = fields.get("evidence", "")
            if rank >= 3 and (evidence == "unavailable" or not (ROOT / evidence).exists()):
                issues.append(f"{group}.{name} 声明 {level} 却没有真实证据路径（{evidence}）")
    if sandbox_approved == 0 and highest >= 3:
        issues.append(
            f"capabilities.yaml 的 sandbox_tested 全为 false，却已有主体声明 L3+（最高 L{highest}）"
        )
    check(not issues, "能力等级逐主体登记，且等级不得越过现有证据", "；".join(issues))


def scenario_fixtures_check() -> None:
    """七场景最小 fixture（规划 §18 M0）。

    M0 要求「为 A 股、国内期货、国内期权、国际股票、FX、加密现货/永续各提供最小 fixture」。
    这里钉住七份都在、都是合法 JSON、四块身份齐全；逐字段能否读进真实类型由 qx-spec 的
    `scenario_fixtures` 用例负责（静态门禁不重复实现 serde 语义）。
    """
    if not SCENARIO_FIXTURES_DIR.is_dir():
        check(
            False,
            "七场景 fixture 目录存在",
            f"缺失 {SCENARIO_FIXTURES_DIR.relative_to(ROOT).as_posix()}",
        )
        return
    issues: list[str] = []
    seen: set[str] = set()
    for path in sorted(SCENARIO_FIXTURES_DIR.glob("*.json")):
        try:
            payload = json.loads(path.read_text(encoding="utf-8"))
        except ValueError as error:
            issues.append(f"{path.name} 不是合法 JSON: {error}")
            continue
        name = payload.get("scenario")
        if not name:
            issues.append(f"{path.name} 缺 scenario 字段")
            continue
        seen.add(name)
        for block in ("instrument_spec", "dataset", "experiment", "run"):
            if not isinstance(payload.get(block), dict):
                issues.append(f"{name} 缺 {block} 块")
    missing = sorted(set(SCENARIO_REQUIRED) - seen)
    if missing:
        issues.append(f"缺场景 {missing}")
    check(not issues, "七场景最小 fixture 齐全且四块身份完整", "；".join(issues))
    check(
        SCENARIO_FIXTURES_TEST.is_file(),
        "七场景 fixture 有钉住它们的用例（逐块读进真实类型并校验）",
        f"缺失 {SCENARIO_FIXTURES_TEST.relative_to(ROOT).as_posix()}",
    )


def layer_dependency_check() -> None:
    """§14.3 的十条领域依赖规则。

    这些规则过去只写在文档里。这里把它们变成会红的判据：逐 crate 解析 Cargo.toml 的内部依赖，
    禁止边出现即红；三条反向规则（唯一转换层 / 投影只能被 CLI 依赖 / CLI 是叶子）钉住
    「谁可以依赖谁」。规则以当前真实依赖图为基线，落地时已用反向变异实测（临时加一条违规边
    会让本判据当场变红）。
    """
    graph = _crate_internal_deps()
    issues: list[str] = []
    core_deps = sorted(dep for dep in graph.get("qx-core", set()) if dep.startswith("qx-"))
    if core_deps:
        issues.append(f"§14.3-1 qx-core 不依赖任何内部 crate，实际依赖 {core_deps}")
    for crate, forbidden, rule in LAYER_FORBIDDEN_DEPS:
        hit = sorted(set(forbidden) & graph.get(crate, set()))
        if hit:
            issues.append(f"{rule}：{crate} → {hit}")
    dependents: dict[str, set[str]] = {}
    for crate, deps in graph.items():
        for dep in deps:
            dependents.setdefault(dep, set()).add(crate)
    for target, allowed, rule in LAYER_SOLE_DEPENDENTS:
        extra = sorted(dependents.get(target, set()) - set(allowed))
        if extra:
            issues.append(f"{rule}：{target} 被 {extra} 依赖")
    check(not issues, "§14.3 的十条领域依赖规则成立（禁止边与唯一转换层）", "；".join(issues))
    check(
        len(graph) >= 26,
        "依赖门禁解析到了全部 crate（解析集为空会让这颗判据静默全绿）",
        f"只解析到 {len(graph)} 个 crate",
    )


# —— P1-12：dev/build 依赖边不得反向压在正常边上（二点环）——
# 方案 §5.4 P1-12 的两条环：`qx-execution --dev--> qx-runtime`（反向 qx-runtime --normal--> qx-execution）
# 与 `qx-risk --dev--> qx-zhenlu`（反向 qx-zhenlu --normal--> qx-risk）。两条都已迁 `contract-tests`。
DEV_CYCLE_TEST_HOST = "contract-tests"
DEV_CYCLE_FORBIDDEN_EDGES = (
    ("qx-execution", "qx-runtime", "qx-runtime --normal--> qx-execution"),
    ("qx-risk", "qx-zhenlu", "qx-zhenlu --normal--> qx-risk"),
)
# 迁走的五份契约用例：搬家不是丢用例，逐名钉在新位置（`EXECUTION_TEST_FLOOR` 由 25 降 15 的补偿）。
DEV_CYCLE_MOVED_CASES = (
    "tests/paper_accounting.rs",
    "tests/reconcile_port_contract.rs",
    "tests/recovery_and_replay.rs",
    "tests/venue_report_contract/main.rs",
    "tests/risk_parity.rs",
)


def _toml_section_entries(text: str, section: str) -> list[str]:
    """取 `[section]` 段里的条目行（到下一个 `[...]` 表头为止，含注释行）。"""
    out: list[str] = []
    inside = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            inside = stripped == f"[{section}]"
            continue
        if inside:
            out.append(line)
    return out


def _crate_edges(sections: tuple[str, ...]) -> dict[str, set[str]]:
    """逐 crate 解析指定 TOML 段里的内部依赖（`qx-x = { ... }` 与 `[dependencies.qx-x]` 两种写法都认）。"""
    edges: dict[str, set[str]] = {}
    for manifest in sorted(CRATES.glob("*/Cargo.toml")):
        text = manifest.read_text(encoding="utf-8")
        deps: set[str] = set()
        for section in sections:
            body = "\n".join(_toml_section_entries(text, section))
            deps |= set(re.findall(r"^(qx-[a-z0-9-]+)\s*=\s*\{", body, re.MULTILINE))
            deps |= set(
                re.findall(rf"^\[{re.escape(section)}\.(qx-[a-z0-9-]+)\]", text, re.MULTILINE)
            )
        if deps:
            edges[manifest.parent.name] = deps
    return edges


def dev_dependency_cycle_check() -> None:
    """方案 §5.4 P1-12 / §8 WP-22：dev/build 边不得反向压在正常边上（两条环迁 `contract-tests`）。

    为什么这不是"洁癖"：`cargo tree` 的**默认**视图不展开 dev/build 边，于是
    「qx-execution --dev--> qx-runtime」在默认视图里完全隐形，而反向的正常边
    「qx-runtime --normal--> qx-execution」又真实存在——依赖图"看起来"是树，实际有环。
    读图的人（包括本门禁自己）会据此得出错的结论。Cargo 允许这种环，所以只有判据能拦住它。

    三颗分别钉：① 两条已知环不许回来；② 更一般地，全仓不得存在「A --dev/build--> B 且
    B --normal--> A」这种二点环（新写的 dev 边踩到任一正常边当场红）；③ 搬家不是丢用例——
    迁走的五份契约用例必须仍在 `crates/contract-tests/tests/` 里，且该 crate 在盘。
    """
    dev = _crate_edges(("dev-dependencies", "build-dependencies"))
    normal = _crate_edges(("dependencies",))
    issues = [
        f"{crate} --dev--> {dep} 又回来了（{why}）"
        for crate, dep, why in DEV_CYCLE_FORBIDDEN_EDGES
        if dep in dev.get(crate, set())
    ]
    reversals = sorted(
        f"{crate} --dev--> {dep}（反向 {dep} --normal--> {crate}）"
        for crate, deps in dev.items()
        for dep in sorted(deps)
        if crate in normal.get(dep, set())
    )
    check(
        not issues and not reversals,
        "dev/build 依赖边不得反向压在正常边上（二点环：默认视图看不见，反向边却真实存在）",
        "；".join(issues + reversals),
    )
    host = CRATES / DEV_CYCLE_TEST_HOST
    missing = sorted(rel for rel in DEV_CYCLE_MOVED_CASES if not (host / rel).is_file())
    check(
        not missing and host.is_dir(),
        f"跨 crate 契约用例仍在 {DEV_CYCLE_TEST_HOST} 里（搬家不得变成丢用例）",
        f"缺 {missing}" if missing else "宿主目录不在盘",
    )
    member = f'"{CRATES.relative_to(ROOT).as_posix()}/{DEV_CYCLE_TEST_HOST}"'
    check(
        member in (ROOT / "Cargo.toml").read_text(encoding="utf-8"),
        f"{DEV_CYCLE_TEST_HOST} 是 workspace 成员（否则搬过去的用例再不会被 cargo test 跑到）",
        f"Cargo.toml 的 members 里找不到 {member}",
    )


# —— P1-1 / DD-2：EventLog 写侧单事务批量追加（`append_batch`）——
EVENT_LOG_SOURCING = "crates/qx-core/src/sourcing.rs"
SQLITE_BACKEND_FILE = "crates/qx-storage/src/sqlite.rs"
PIPELINE_FILE = "crates/qx-runtime/src/pipeline.rs"
EVENT_LOG_BATCH_TEST = "crates/qx-core/tests/event_log_append_batch.rs"
SQLITE_EVENT_LOG_TEST = "crates/qx-storage/tests/sqlite_event_log.rs"
SQLITE_APPEND_BATCH_CASE = "sqlite_event_log_append_batch_is_incremental_and_atomic_with_outbox"
INCREMENTAL_APPEND_FN = "append_event_log_in_transaction"
# P1-2：共享 EventLog 的游标增量刷新（读侧 `read_since` + 归约侧 `refresh_latest`）。
SQLITE_READ_SINCE_CASE = "sqlite_event_log_read_since_returns_only_the_verified_tail"
PIPELINE_CURSOR_CASE = "sqlite_pipeline_refresh_takes_the_cursor_incremental_path"
# P1-6：估值单点（`ValuationContext` / `ValuationResult`）。
VALUATION_MODULE = "crates/qx-core/src/valuation.rs"
EQUITY_RULER_DEFS = "crates/qx-core/src/ledger/query.rs"
VALUATION_CASE_FILE = "crates/qx-core/tests/valuation.rs"
PARAMETERIZED_EQUITY_RULERS = (
    "equity_for_with_spec_and_fx",
    "equity_for_with_spec",
    "equity_for_with_multiplier",
)
VALUATION_DISPATCH_CASES = (
    "ledger_valuate_equals_the_multiplier_ruler_for_spot",
    "ledger_valuate_uses_the_contract_spec_ruler_without_fx",
    "ledger_valuate_switches_to_the_fx_ruler_only_when_rates_are_given",
    "margin_state_valuate_shares_the_result_shape",
)
# P1-11 / DD-5：错误码五元契约（`ErrorCode` / `Retryability` / `ErrorContract`）。
ERROR_CONTRACT_FILE = "crates/qx-core/src/error.rs"
QX_CORE_LIB_FILE = "crates/qx-core/src/lib.rs"
USAGE_ERRORS_FILE = "crates/qx-cli/src/usage_errors.rs"
PIPELINE_STORAGE_FILE = "crates/qx-cli/src/runtime_wiring/pipeline_storage.rs"
ERROR_CONTRACT_TEST = "crates/qx-cli/src/tests/error_code_contract.rs"
ERROR_CONTRACT_FIELDS = (
    "code: ErrorCode",
    "retryability: Retryability",
    "reconcile_required: bool",
    "safe_to_retry: bool",
    "user_message: &'a str",
)
ERROR_RETRYABILITY_CLASSES = ("Never", "Allowed", "AfterReconcile", "AfterBackoff")
ERROR_CONTRACT_CASES = (
    "every_variant_exposes_the_five_tuple_contract",
    "cli_diagnostics_carry_the_code_and_the_next_step",
    "event_log_boundary_diagnostics_carry_the_machine_readable_code",
)


def _rust_fn_body(text: str, name: str) -> str | None:
    """取 `fn <name>(...)` 的函数体（按花括号配对），找不到返回 `None`。"""
    match = re.search(rf"\bfn {re.escape(name)}\s*[(<]", text)
    if match is None:
        return None
    start = text.find("{", match.end())
    if start < 0:
        return None
    depth = 0
    for index in range(start, len(text)):
        char = text[index]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return text[start : index + 1]
    return None


def event_log_append_batch_check() -> None:
    """P1-1 / DD-2 / §7 M2：EventLog 写侧单事务批量追加（`append_batch`）。

    此前写侧只有整份 `save`：SQLite 参考后端每次追加都要把已落库的 N 条事件读回来逐行比对
    （`load_event_log`），写放大随 N 线性增长（G4）；而 Kernel 侧逐条 `append_checked` 在中途
    失败时会把前半批留在日志里——调用方以为整批被拒、实际写进一半（半个事务）。六颗分别钉：
    ① Kernel 有 `append_batch` 且 `append_checked` 委托给它（校验规则单源，不留第二份逐条实现）；
    ② `digest()` 走 `digest_of_prefix()`（前缀摘要与全量摘要同源，增量核对才成立）；
    ③ SQLite 的增量追加函数在盘且**不调用** `load_event_log`（否则 O(N) 读放大原样回来）；
    ④ `SqliteEventLogStore::append_batch` 在单事务里走增量路径；⑤ 生产写面的 SQLite 分支真的
    调 `append_batch`（不是加了个没人调的方法）；⑥ 原子性与增量性各有一条行为用例在盘。
    """
    sourcing = case_source(EVENT_LOG_SOURCING)
    sqlite = case_source(SQLITE_BACKEND_FILE)
    pipeline = case_source(PIPELINE_FILE)
    check(
        re.search(
            r"pub fn append_batch\(&mut self, batch: &\[Event\]\) -> QxResult<usize>", sourcing
        )
        is not None
        and re.search(
            r"fn append_checked\(&mut self, e: Event\) -> QxResult<\(\)> \{\s*self\.append_batch\(",
            sourcing,
        )
        is not None,
        "Kernel 的 append_batch 在盘，且 append_checked 委托给它（逐条校验规则单源）",
        "找不到 append_batch，或 append_checked 没有委托（出现第二份逐条实现）",
    )
    check(
        "pub fn digest_of_prefix(&self, len: usize) -> Option<u64>" in sourcing
        and re.search(r"pub fn digest\(&self\) -> u64 \{\s*self\.digest_of_prefix\(", sourcing)
        is not None,
        "全量摘要 digest() 与前缀摘要 digest_of_prefix() 同源（增量核对才有意义）",
        "digest_of_prefix 不在盘，或 digest() 没走它",
    )
    incremental = _rust_fn_body(sqlite, INCREMENTAL_APPEND_FN)
    check(
        incremental is not None and "load_event_log(" not in incremental,
        f"SQLite 增量追加 {INCREMENTAL_APPEND_FN} 不重读整条历史（不调 load_event_log）",
        "函数不在盘" if incremental is None else "函数体里出现 load_event_log(",
    )
    method = _rust_fn_body(sqlite, "append_batch")
    check(
        method is not None
        and INCREMENTAL_APPEND_FN + "(" in method
        and "transaction_with_behavior" in method,
        "SqliteEventLogStore::append_batch 在单事务里走增量追加",
        "方法不在盘，或没走增量路径 / 没开事务",
    )
    check(
        re.search(r"Self::Sqlite\(store\) => store\.append_batch\(", pipeline) is not None,
        "生产写面的 SQLite 分支真的走 append_batch（不是加了个没人调的方法）",
        "pipeline.rs 的 SQLite 分支没有走 append_batch",
    )
    check(
        (ROOT / EVENT_LOG_BATCH_TEST).is_file()
        and SQLITE_APPEND_BATCH_CASE in case_source(SQLITE_EVENT_LOG_TEST),
        "append_batch 的原子性（Kernel）与增量性（SQLite）各有一条行为用例在盘",
        f"缺 {EVENT_LOG_BATCH_TEST} 或 {SQLITE_EVENT_LOG_TEST}::{SQLITE_APPEND_BATCH_CASE}",
    )


def pipeline_cursor_refresh_check() -> None:
    """P1-2 / §7 M2：`LiveEventPipeline` 改游标增量 refresh（G5 收敛）。

    此前每次 `refresh()` 都把整条共享日志读回来 + 整份重放重建（`rebuild_ledger` +
    `rebuild_runtime_indexes` + 整份替换），而它在**每条外部事实之前**都会被调一次——没有别的
    写者时这些工作全是白做，代价随日志长度线性增长。现在分三支：无新事实直接返回；本地仍是
    store 的前缀时只接尾部；前缀对不上或后端没有行级尾部读时退回整份重建。六颗分别钉：

    ① SQLite 读侧 `read_since` 在盘、按 `seq > ?` 只取尾部、且不重读整条历史（不调 `load_event_log`）；
    ② `RuntimeEventStore::read_since` 把 SQLite 委派下去，其余后端**显式**回落 `Ok(None)`；
    ③ `refresh_latest` 三支齐备（空尾部 no-op / 有尾部走 `apply_tail` / `None` 走 `rebuild_from_store`）；
    ④ `apply_tail` 用 P1-1 的 `append_batch` 接日志，且**不**整份重建索引；
    ⑤ 增量与整份重建共用同一个 `apply_index_event`（两份归约路径不能各写一份）；
    ⑥ 读侧前缀校验与归约侧增量路径各有一条行为用例在盘。
    """
    sqlite = case_source(SQLITE_BACKEND_FILE)
    pipeline = case_source(PIPELINE_FILE)
    read_since = _rust_fn_body(sqlite, "read_since")
    check(
        re.search(
            r"pub fn read_since\(\s*&self,\s*name: &str,\s*after_seq: Option<u64>,"
            r"\s*expected_prefix: Option<\(&Event, usize\)>,",
            sqlite,
        )
        is not None
        and read_since is not None
        and re.search(r"WHERE name = \?1 AND seq > \?2 ORDER BY position ASC", read_since)
        is not None
        and "load_event_log(" not in read_since,
        "SQLite 读侧 read_since 按 seq 只取尾部，且不重读整条历史",
        "read_since 不在盘" if read_since is None else "签名不符 / 没按 seq 过滤 / 仍调 load_event_log",
    )
    check(
        re.search(
            r"Self::Sqlite\(store\) => store\s*\.read_since\(name, after_seq, expected_prefix\)",
            pipeline,
        )
        is not None
        and re.search(r"_ => Ok\(None\)", pipeline) is not None,
        "RuntimeEventStore::read_since 委派 SQLite，其余后端显式回落 Ok(None)",
        "pipeline.rs 没把 read_since 委派给 SQLite，或缺少非 SQLite 后端的显式回落",
    )
    refresh = _rust_fn_body(pipeline, "refresh_latest")
    check(
        refresh is not None
        and "read_since(&self.log_name, after_seq, prefix)" in refresh
        and re.search(r"Some\(tail\) if tail\.is_empty\(\) => Ok\(\(\)\)", refresh) is not None
        and "self.apply_tail(&tail)" in refresh
        and "self.rebuild_from_store(false)" in refresh,
        "refresh_latest 三支齐备：空尾部 no-op / 有尾部增量 / 前缀不符整份重建",
        "refresh_latest 不在盘或分支不全",
    )
    apply_tail = _rust_fn_body(pipeline, "apply_tail")
    check(
        apply_tail is not None
        and "self.log.append_batch(tail)?" in apply_tail
        and "apply_index_event(event)?" in apply_tail
        and "rebuild_runtime_indexes(" not in apply_tail,
        "apply_tail 用 append_batch 接日志 + 逐条增量应用索引，不做整份重建",
        "apply_tail 不在盘，或没走 append_batch / 仍在整份重建索引",
    )
    rebuild_indexes = _rust_fn_body(pipeline, "rebuild_runtime_indexes")
    check(
        rebuild_indexes is not None
        and "apply_index_event(" in rebuild_indexes
        and apply_tail is not None
        and "apply_index_event(" in apply_tail,
        "增量与整份重建共用同一个 apply_index_event（归约单源）",
        "rebuild_runtime_indexes 与 apply_tail 没有共用 apply_index_event",
    )
    check(
        SQLITE_READ_SINCE_CASE in case_source(SQLITE_EVENT_LOG_TEST)
        and PIPELINE_CURSOR_CASE in pipeline,
        "游标增量：读侧前缀校验与归约侧增量路径各有一条行为用例在盘",
        f"缺 {SQLITE_EVENT_LOG_TEST}::{SQLITE_READ_SINCE_CASE} 或 {PIPELINE_FILE}::{PIPELINE_CURSOR_CASE}",
    )


def valuation_single_source_check() -> None:
    """P1-6 / WP-19：三把权益/保证金尺子收敛成单一 `ValuationContext`/`ValuationResult`。

    收敛前同一个问题（「这账户值多少、还能用多少保证金」）有三处各自成立的算法：
    ① `Ledger` 的 `equity*` 家族（现货按乘数 / 衍生按合约规格 / 跨币种再叠 FX）——算术的正确
    落点，但**选了哪一把由调用方自己判断**；② `MarginState::equity`/`available` 的保证金口径；
    ③ 回测里按「有杠杆规格吗 / 有汇率吗」手工二选一的 `equity_for` 派发——同一段派发在
    `qx-xingban` 里**抄了两份**。六颗分别钉：

    ① `ValuationContext` / `ValuationResult` 在 `qx-core` 的 `valuation` 模块里，且 lib.rs
       既挂了模块又做了重导出（不然别的 crate 用不上）；
    ② `Ledger::valuate` 在盘，且**它自己**包含三把参数化尺子的名字（派发只此一处）；
    ③ `MarginState::valuate` 在盘，且两个入口都经 `ValuationResult::new`（`available` 关系单源）；
    ④ 生产源码全文扫描：三把**参数化**尺子只能出现在 `ledger/query.rs`（定义点 + `Ledger::valuate`
       的派发）——回测/CLI 里那几处手工派发必须消失。取 `production_text` 是为了剥掉注释与测试项：
       否则文档里提一句尺子名，或 `#[cfg(test)]` 用例里直接调原语，都会让「还有谁在直接派发」数不准；
    ⑤ `MarginState::equity` / `available` 委托 `valuate`（保证金算式不许在 `trading.rs` 里再写一份）；
    ⑥ 派发等价性有四条行为用例在盘（`valuate` 必须与它派发到的原始尺子**逐值相等**，且
       有/无汇率两把尺子给出不同的数——不然等式验不出派发）。

    刻意**不**纳入本颗的：`Ledger::equity_for`（乘数固定 1 的无参现货尺子）。它没有"选哪把"
    的歧义，`qx-cli` 的账户快照与策略上下文直接调它不构成第二处派发。
    """
    valuation = case_source(VALUATION_MODULE)
    check(
        (ROOT / VALUATION_MODULE).is_file()
        and "pub mod valuation;" in case_source("crates/qx-core/src/lib.rs")
        and "pub use self::valuation::{ValuationContext, ValuationResult};"
        in case_source("crates/qx-core/src/lib.rs"),
        "ValuationContext / ValuationResult 落在 qx-core 的 valuation 模块，且 lib.rs 挂载并重导出",
        f"缺 {VALUATION_MODULE}，或 lib.rs 没挂模块 / 没重导出",
    )
    ledger_valuate = _rust_fn_body(case_source(EQUITY_RULER_DEFS), "valuate")
    check(
        ledger_valuate is not None
        and all(ruler in ledger_valuate for ruler in PARAMETERIZED_EQUITY_RULERS),
        "Ledger::valuate 在盘，且三把参数化尺子的派发只在它里面",
        "Ledger::valuate 不在盘，或它没有覆盖全部三把尺子",
    )
    check(
        re.search(
            r"pub fn valuate\(self, currency: impl Into<String>\) -> Option<ValuationResult>",
            valuation,
        )
        is not None
        and (valuation + case_source(EQUITY_RULER_DEFS)).count("ValuationResult::new(") >= 2,
        "MarginState::valuate 在盘，且两个入口共用 ValuationResult::new（available 关系单源）",
        "MarginState::valuate 不在盘，或两个入口没有共用 ValuationResult::new",
    )
    offenders: list[str] = []
    for path in rust_sources():
        rel = path.relative_to(ROOT).as_posix()
        if rel == EQUITY_RULER_DEFS:
            continue
        text = production_text(path.read_text(encoding="utf-8"))
        if any(ruler in text for ruler in PARAMETERIZED_EQUITY_RULERS):
            offenders.append(rel)
    check(
        not offenders,
        "参数化权益尺子在生产源码里只出现在定义点与估值单点（其余一律走 Ledger::valuate）",
        f"这些文件仍在直接派发参数化尺子：{sorted(offenders)}",
    )
    trading = case_source("crates/qx-core/src/trading.rs")
    margin_equity = _rust_fn_body(trading, "equity")
    margin_available = _rust_fn_body(trading, "available")
    check(
        margin_equity is not None
        and "self.valuate(" in margin_equity
        and margin_available is not None
        and "self.valuate(" in margin_available,
        "MarginState::equity / available 委托 valuate（保证金算式不在 trading.rs 里再写一份）",
        "MarginState 的 equity/available 没有委托 valuate",
    )
    check(
        (ROOT / VALUATION_CASE_FILE).is_file()
        and all(case in case_source(VALUATION_CASE_FILE) for case in VALUATION_DISPATCH_CASES),
        "估值派发有四条行为用例在盘（valuate 与原始尺子逐值相等、有/无汇率给出不同的数）",
        f"缺 {VALUATION_CASE_FILE}，或派发等价性用例不全",
    )


# —— V13 R26：名义额规则缺规格 fail-closed（删掉 `legacy_spot_spec` 兼容分支）——
RISK_RULES_FILE = "crates/qx-risk/src/rules.rs"
RISK_LIB_FILE = "crates/qx-risk/src/lib.rs"
ZHENLU_LIB_FILE = "crates/qx-zhenlu/src/lib.rs"
BAR_BACKTEST_FILE = "crates/qx-xingban/src/backtest.rs"
BOOK_BACKTEST_FILE = "crates/qx-xingban/src/orderbook_backtest.rs"
RISK_FAIL_CLOSED_CASE = "max_notional_rule_fails_closed_without_a_product_spec"
# 三入口一致性用例的 (c) 腿（deprecated `RiskGate`）：V13 R26 起必须带规格取证。
RISK_PARITY_FILE = "crates/contract-tests/tests/risk_parity.rs"
RISK_PARITY_HELPER = "fn assert_gate_matches_canonical("
RISK_PARITY_SPEC_ARG = "Some(&perpetual_spec())"


def risk_spec_fail_closed_check() -> None:
    """V13 R26：`MaxNotionalRule` 缺产品规格时 fail-closed，不再合成临时现货规格。

    `MaxNotionalRule` 此前在 `context.instrument_spec` 为 `None` 时调用 `legacy_spot_spec`
    合成一份"1:1 现货"临时规格——与 `OrderRiskContext::validate_order`「账户限额在场即
    拒绝」的口径自相矛盾，也与全仓"缺数据即拒绝"的哲学相悖。五颗分别钉：

    ① `legacy_spot_spec` 在全仓生产源码里**一处都不剩**（删了函数却留着 import 就是死代码）；
    ② `MaxNotionalRule::check` 体内点名缺规格消息常量，且**不再**出现 `legacy_spot_spec` /
       `fallback_spec`（否则 fail-closed 又退回去）；
    ③ 该消息常量在 `qx-risk/lib.rs` 只有一个定义点，且被 `rules.rs` 引用（有生产读者）；
    ④ `RiskGate::check_with_spec` 在盘，且两条回测生产路径都走它（`check_with_price` 在
       `backtest.rs`/`orderbook_backtest.rs` 的生产文本里**一处不剩**）——否则配了名义额
       上限的回测会静默走"无规格"分支；
    ⑤ 一条行为用例在盘（缺规格拒绝且点名缺什么、给规格后按真实折算放行）；
    ⑥ 三入口一致性用例的 (c) 腿也带规格取证：`assert_gate_matches_canonical` 的**函数体**
       里必须出现 `check_with_spec(` 与规格实参——否则 (c) 会把 fail-closed 的"缺规格"
       当成一条业务拒绝，与 (a)/(b) 的规则链结果不可比，用例红而没人知道红在哪。
    """
    sources = _production_sources()
    legacy = sorted(
        location
        for location, lines in sources.items()
        if any("legacy_spot_spec" in line for line in lines)
    )
    check(
        not legacy,
        "`legacy_spot_spec` 兼容分支已从全仓生产源码删除（缺规格不再合成临时现货规格）",
        f"这些文件仍有 legacy_spot_spec：{legacy}",
    )
    rules = production_text(case_source(RISK_RULES_FILE))
    start = rules.find("impl RiskRule for MaxNotionalRule {")
    end = rules.find("impl RiskRule for NoShortRule {")
    body = rules[start:end] if 0 <= start < end else ""
    check(
        "MAX_POSITION_NOTIONAL_MISSING_SPEC_MESSAGE" in body
        and "legacy_spot_spec" not in body
        and "fallback_spec" not in body,
        "MaxNotionalRule::check 缺规格即 fail-closed（点名缺什么，不再合成临时规格）",
        f"MaxNotionalRule 体没有按 fail-closed 收口：{body[:120]!r}",
    )
    lib = case_source(RISK_LIB_FILE)
    check(
        lib.count("pub const MAX_POSITION_NOTIONAL_MISSING_SPEC_MESSAGE") == 1
        and "MAX_POSITION_NOTIONAL_MISSING_SPEC_MESSAGE" in rules,
        "缺规格拒绝消息单点定义在 qx-risk/lib.rs，且被规则实现引用（有生产读者）",
        "缺规格消息常量没有单点定义，或规则实现没引用它",
    )
    bar = production_text(case_source(BAR_BACKTEST_FILE))
    book = production_text(case_source(BOOK_BACKTEST_FILE))
    check(
        "pub fn check_with_spec(" in case_source(ZHENLU_LIB_FILE)
        and "check_with_spec(" in bar
        and "check_with_spec(" in book
        and "check_with_price(" not in bar
        and "check_with_price(" not in book,
        "RiskGate::check_with_spec 在盘，两条回测生产路径都传规格（不再走无规格的 check_with_price）",
        "缺 check_with_spec，或回测仍在调用无规格的 check_with_price",
    )
    check(
        RISK_FAIL_CLOSED_CASE in case_source(RISK_LIB_FILE),
        "缺规格 fail-closed 有一条行为用例在盘",
        f"缺行为用例 {RISK_FAIL_CLOSED_CASE}",
    )
    parity_helper = _fn_body(production_text(case_source(RISK_PARITY_FILE)), RISK_PARITY_HELPER)
    check(
        "check_with_spec(" in parity_helper and RISK_PARITY_SPEC_ARG in parity_helper,
        "三入口一致性用例的 (c) 腿走带规格入口（无规格会把 fail-closed 当成一条业务拒绝）",
        "assert_gate_matches_canonical 没带规格取证："
        f"{parity_helper[:120]!r}",
    )


# —— V13 R26：发布版本单源 + CI 显式钉解释器 ——
PYPROJECT_FILE = "python/pyproject.toml"
WORKSPACE_CARGO = "Cargo.toml"
CI_WORKFLOW = ".github/workflows/ci.yml"


def release_version_single_source_check() -> None:
    """V13 R26：wheel 版本与 `qx-cli --version` 同一个值；CI 显式钉住 Python 解释器。

    ① `python/pyproject.toml` 的 `[project] version` 必须等于 workspace `Cargo.toml` 的
       `[workspace.package] version`——`qx-cli --version` 走 `CARGO_PKG_VERSION`，也就是
       后者。两处各写一份时，装了 wheel 的机器无法自证它对应哪一轮构建；
    ② `rust-core` 作业显式设 `QX_PYTHON`，否则 `e2e_and_python_contract` 的两条 worker
       契约用例回落 PATH 上的 `python`，「Python 链在 CI 绿」不等于真的跑过。
    """
    workspace = case_source(WORKSPACE_CARGO)
    ws_match = re.search(
        r'(?ms)^\[workspace\.package\].*?^version\s*=\s*"([^"]+)"', workspace
    )
    pyproject = case_source(PYPROJECT_FILE)
    py_match = re.search(r'(?ms)^\[project\].*?^version\s*=\s*"([^"]+)"', pyproject)
    check(
        ws_match is not None
        and py_match is not None
        and ws_match.group(1) == py_match.group(1),
        "wheel 版本（python/pyproject.toml）与 workspace Cargo.toml 版本同一个值（qx-cli --version 同源）",
        f"workspace={ws_match.group(1) if ws_match else None}，"
        f"pyproject={py_match.group(1) if py_match else None}",
    )
    rust_core = _yaml_job_block(_without_yaml_comments(case_source(CI_WORKFLOW)), "rust-core")
    check(
        "QX_PYTHON" in rust_core,
        "CI 的 rust-core 作业显式设 QX_PYTHON（Python worker 契约用例不靠 PATH 回落）",
        "rust-core 作业没有 QX_PYTHON",
    )


def error_code_contract_check() -> None:
    """P1-11 / DD-5：错误码从字符串抽成**五元契约**（`ErrorCode` / `Retryability` / `ErrorContract`）。

    「错误跨 crate 主要靠字符串」的原始症状有两半：一半在产出侧——`QxError::code()` 只是把变体名
    翻成字符串，而「能不能重试、要不要先对账」散在各调用方就地 `match` 变体各写一份；另一半在
    消费侧——CLI 把 `QxError` 摊成 `format!("…: {error:?}")`，落进日志的只有一句中文加 Rust 的
    Debug 形状。于是同一个问题（这条错误能不能重发）在 8 个变体 × N 个调用点上各有各的答案，
    而改口径时漏改的那一处**静默失效**。八颗分别钉：

    ① `ErrorCode` / `Retryability` / `ErrorContract` 三型落在 `qx-core` 的 `error` 模块，且 lib.rs
       重导出（不重导出等于别的 crate 用不上，契约退化成本 crate 私有）；
    ② `ErrorContract` 五格字段齐——DD-5 的五元就是这五格，少一格就退回"只有码没有处置"；
    ③ `ErrorCode::ALL` 是**闭集**：声明长度、列出的码、`QxError` 的变体数三者相等。遍历 `ALL`
       才可能保证"新增变体时映射表不会漏一格"，靠人记的清单迟早漂移；
    ④ `QxError::contract()` 是全仓**唯一**映射表（8 个码 + 4 类重试资格都在体里），且 `code()`
       从它派生（`self.contract().code`）而**自身不再 `match self`**——两份映射表就是两条口径；
    ⑤ CLI 展示层 `qx_context` 显式消费契约（取 `error.contract()` 并按 `reconcile_required` /
       `retryability.allows_retry()` 分级提示），而不是只把消息抄一遍；
    ⑥ 打开 EventLog 的四处 `QxError`→字符串边界全部走 `qx_context`，且该文件里**不再**出现把
       `QxError` 摊成 `{error:?}` 的转换（那就是原始症状本身）；
    ⑦ 热路径重试循环按 `retryability` 分支（`pipeline.rs` 里两处 `allows_retry()`），不再靠错误
       文本或就地 `matches!` 判断"这条能不能重发"；
    ⑧ 行为用例在盘：`qx-core` 侧契约自洽 + `qx-cli` 侧消费面三条（逐变体五格、展示层分级、
       真实打开边界带码）。③④只证明表是全的，证明不了"消费侧真的按它分支"。

    刻意**不**纳入本颗的：HTTP/工作流层面的状态串（`qx-api` 的 status、`qx-control` 的
    `CommandStatus`）。它们是各自的读面，不是"错误分类"——塞进 `ErrorCode` 只会把闭集撑成
    开放集，闭集一旦开放，③那条判据就再也数不准。
    """
    error_rs = case_source(ERROR_CONTRACT_FILE)
    lib_rs = case_source(QX_CORE_LIB_FILE)
    check(
        "pub enum ErrorCode" in error_rs
        and "pub enum Retryability" in error_rs
        and "pub struct ErrorContract<'a>" in error_rs
        and "pub use self::error::{ErrorCode, ErrorContract, QxError, QxResult, Retryability};"
        in lib_rs,
        "错误码三型（ErrorCode / Retryability / ErrorContract）落在 qx-core 的 error 模块，且 lib.rs 重导出",
        f"缺 {ERROR_CONTRACT_FILE} 里的类型，或 lib.rs 没重导出（跨 crate 用不上）",
    )
    missing_fields = [field for field in ERROR_CONTRACT_FIELDS if field not in error_rs]
    check(
        not missing_fields,
        "ErrorContract 五格字段齐（DD-5 五元：码 / 重试资格 / 对账 / 安全重试 / 展示消息）",
        f"缺字段：{missing_fields}",
    )
    all_decl = re.search(r"pub const ALL: \[ErrorCode; (\d+)\] = \[(.*?)\];", error_rs, re.S)
    declared = int(all_decl.group(1)) if all_decl is not None else 0
    listed = re.findall(r"ErrorCode::(\w+),", all_decl.group(2)) if all_decl is not None else []
    variants = sorted(set(re.findall(r"QxError::(\w+)\(_\)", error_rs)))
    check(
        all_decl is not None
        and declared == len(listed) == len(variants) == 8
        and sorted(listed) == variants,
        "ErrorCode::ALL 是闭集：声明长度、列出的码、QxError 的变体数三者相等（一型一码）",
        f"声明 {declared} / 列出 {len(listed)} / QxError 变体 {len(variants)}，或清单与变体对不上",
    )
    contract_body = _rust_fn_body(error_rs, "contract")
    code_body = _rust_fn_body(error_rs, "code")
    check(
        contract_body is not None
        and all(f"ErrorCode::{name}" in contract_body for name in listed)
        and all(
            f"Retryability::{name}" in contract_body
            for name in ERROR_RETRYABILITY_CLASSES
        )
        and code_body is not None
        and "self.contract().code" in code_body
        and "match self" not in code_body,
        "QxError::contract 是全仓唯一映射表（8 码 + 4 类重试资格），code() 从它派生且自身不再 match",
        "contract 不在盘 / 覆盖不全，或 code() 又写了一份自己的 match（两份口径）",
    )
    qx_context = _rust_fn_body(case_source(USAGE_ERRORS_FILE), "qx_context")
    check(
        qx_context is not None
        and "error.contract()" in qx_context
        and "retryability.allows_retry()" in qx_context
        and "reconcile_required" in qx_context,
        "CLI 展示层 qx_context 显式消费五元契约（码 + 对账 / 可重试两格分级）",
        "usage_errors.rs 没有 qx_context，或它没有消费契约（退回只有消息）",
    )
    storage = case_source(PIPELINE_STORAGE_FILE)
    routed = storage.count("usage_errors::qx_context(")
    check(
        routed >= 4 and "{error:?}" not in storage,
        "打开 EventLog 的四处 QxError→字符串边界都走 qx_context，且不再把 QxError 摊成 {error:?}",
        f"只有 {routed} 处走 qx_context，或仍有 {{error:?}} 把 QxError 摊成自由文本",
    )
    check(
        case_source(PIPELINE_FILE).count("error.contract().retryability.allows_retry()") >= 2,
        "热路径重试循环按 retryability 分支（不再靠错误文本判断能不能重发）",
        "pipeline.rs 的重试循环没有消费契约",
    )
    check(
        (ROOT / ERROR_CONTRACT_TEST).is_file()
        and all(case in case_source(ERROR_CONTRACT_TEST) for case in ERROR_CONTRACT_CASES)
        and "contract_separates_retryable_from_safe_to_retry" in error_rs,
        "五元契约：qx-core 侧自洽用例 + qx-cli 侧消费面三条用例在盘",
        f"缺 {ERROR_CONTRACT_TEST}，或消费面用例不全 / qx-core 侧自洽用例被删",
    )


def schema_registry_check() -> None:
    """模式登记册实例（规划 §6.2 / M6）。

    §6.2 要求 SchemaRegistry 统一管理 JSON / C ABI / Arrow / wire event 与兼容策略。这里钉住
    登记册覆盖 `schemas/` 下的每一份契约、每份都写清生产方/消费方/golden fixture，且所有指针
    都落得回盘上——「哪份契约在哪、谁产谁消、跨版本怎么兼容」不能只存在于人脑里。
    """
    if not SCHEMA_REGISTRY_FILE.is_file():
        check(False, "模式登记册实例存在", f"缺失 {SCHEMA_REGISTRY_FILE.relative_to(ROOT).as_posix()}")
        return
    try:
        registry = json.loads(SCHEMA_REGISTRY_FILE.read_text(encoding="utf-8"))
    except ValueError as error:
        check(False, "模式登记册是合法 JSON", str(error))
        return
    entries = registry.get("entries", [])
    issues: list[str] = []
    registered = [entry.get("path", "") for entry in entries]
    duplicated = sorted({path for path in registered if registered.count(path) > 1})
    if duplicated:
        issues.append(f"重复登记 {duplicated}")
    on_disk = {path.relative_to(ROOT).as_posix() for path in sorted((ROOT / "schemas").glob("*.json"))}
    missing = sorted(on_disk - set(registered))
    if missing:
        issues.append(f"schemas/ 下未登记 {missing}")
    dangling = sorted(path for path in registered if not (ROOT / path).is_file())
    if dangling:
        issues.append(f"登记路径落空 {dangling}")
    ids = [entry.get("schema_id", "") for entry in entries]
    if len(set(ids)) != len(ids):
        issues.append("schema_id 有重复")
    for entry in entries:
        schema_id = entry.get("schema_id", "?")
        if not entry.get("producer") or not entry.get("consumers"):
            issues.append(f"{schema_id} 缺 producer 或 consumers")
        fixtures = entry.get("golden_fixtures") or []
        if not fixtures:
            issues.append(f"{schema_id} 缺 golden fixture")
        for fixture in fixtures:
            if not (ROOT / fixture).is_file():
                issues.append(f"{schema_id} 的 fixture 落空 {fixture}")
    check(not issues, "模式登记册覆盖 schemas/ 全部契约且指针全部落地", "；".join(issues))


# === T1-2（QX-DEV-PLAN-2026-10-10 阶段 1 / 关键路径节点）：产物 schema/version 关系图与迁移表 ===
# `schema_registry_check` 核的是「哪份契约在哪、谁产谁消、golden fixture 落不落得回盘」——它
# **不问**「旧产物读进来时会怎样」。于是「同一份产物跨版本」这件事全仓没有一处登记：版本常量
# 被删、读侧不再比对常量、拒绝文案退化成一句泛泛的解析失败、常驻用例从「具名拒绝」退回裸
# `is_err()`，四者都不会红。校准 C3 要的是「旧产物读取必须走显式迁移或给出具名拒绝原因」，
# 这一族把它收成一张可核对的登记面（`maturity/artifact_migration.yaml`）并与真实代码逐格对账。
ARTIFACT_MIGRATION_FILE = ROOT / "maturity" / "artifact_migration.yaml"
ARTIFACT_MIGRATION_KIND = "artifact-migration"
ARTIFACT_MIGRATION_TOP = ("schema_version", "kind", "updated_at", "registry")
ARTIFACT_MIGRATION_FIELDS = (
    "schema_id",
    "reader",
    "version_field",
    "version_const",
    "on_unknown_version",
    "refusal_reason",
    "refusal_case",
)
# 允许的处置集。**当前只有 `refuse` 一态**：本仓 10 份在册契约都有独立版本号，读侧都在解析
# 之前显式比对常量。判据核「允许集 == 实用集」，所以将来引入第二种处置（例如没有版本号、
# 只靠字段集合拒绝未知键的契约）必须**同时**改这里与登记面 —— 只往表里加一行新态，判据当场红。
ARTIFACT_MIGRATION_DISPOSITIONS = ("refuse",)
# 地板：`maturity/schema-registry.json` 里每份在册契约都要有一行（今天 10 份）。
# 只降不升的棘轮：本仓在册契约只增不减（2026-10-10 起 11 份）。新增契约若忘了在本表登记
# 「读侧版本处置」，② 的集合相等先红；有人删契约、连带从表里删行时，这道地板是第二把锁。
ARTIFACT_MIGRATION_MIN_ROWS = 11
# 版本常量声明的形状：`pub const X: u32 = …`。右值刻意不核（可以是字面量也可以是表达式，
# `STRATEGY_CONTRACT_SCHEMA_VERSION` 就是 `qx_strategy::STRATEGY_API_VERSION`）。
ARTIFACT_MIGRATION_CONST = "pub const {gate}: u32"


def _parse_artifact_migration(
    text: str,
) -> tuple[dict[str, str], list[dict[str, str]], list[dict[str, str]]]:
    """逐行解析 T1-2 登记面（门禁不引 PyYAML）：返回 (顶层标量, artifacts 行, migrations 行)。"""
    scalars: dict[str, str] = {}
    artifacts: list[dict[str, str]] = []
    migrations: list[dict[str, str]] = []
    section = ""
    for line in text.splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        indent = len(line) - len(line.lstrip())
        stripped = line.strip()
        if indent == 0:
            key, _, value = stripped.partition(":")
            key, value = key.strip(), value.strip()
            section = key if key in ("artifacts", "migrations") and value != "[]" else ""
            if key not in ("artifacts", "migrations"):
                scalars[key] = value
            continue
        target = artifacts if section == "artifacts" else migrations if section == "migrations" else None
        if target is None:
            continue
        if stripped.startswith("- "):
            target.append({})
            stripped = stripped[2:].strip()
        if target:
            key, _, value = stripped.partition(":")
            target[-1][key.strip()] = _strip_yaml_quotes(value.strip())
    return scalars, artifacts, migrations


def _strip_yaml_quotes(value: str) -> str:
    """剥掉成对引号：登记面给带 `: ` 或 `{` 的标量加了引号，比对前要还原。"""
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "'\"":
        return value[1:-1]
    return value


def _artifact_code_only(path: Path) -> str:
    """去掉整行注释后的正文：版本判定与拒绝文案必须落在**代码**上，注释里提一句不算。

    不剥的话，「把真调用退回旧写法、只在注释里留锚点」这种变异照样绿 —— 判据读的是散文
    而不是代码，等于给退化留了一条侧门。
    """
    return "\n".join(
        line
        for line in path.read_text(encoding="utf-8").splitlines()
        if not line.lstrip().startswith("//")
    )


def _refusal_case_verdict(crate: str, name: str, version_field: str) -> str:
    """在 `crates/<crate>` 全树里找名为 `name` 的 `#[test]`，返回空串表示通过。

    只核「用例在盘」不够 —— T1-2 要的是**具名**拒绝，所以用例体必须真的对拒绝文案说话：
    出现 `contains(`（对文案断言）或 `<version_field>=`（把被拒的版本当参数点名）。一个裸
    `assert!(…is_err())` 两样都没有，那正是这一轮要堵的形态（只核「失败」不核「具名原因」）。
    """
    for path in sorted((CRATES / crate).rglob("*.rs")):
        text = path.read_text(encoding="utf-8")
        if re.search(r"#\[test\][\s\S]{0,800}?fn\s+" + re.escape(name) + r"\s*\(", text) is None:
            continue
        body = text[text.index(f"fn {name}") :][:2000]
        if "contains(" in body or f"{version_field}=" in body:
            return ""
        return f"{name} 体里既无 contains( 也无 {version_field}=（只核失败、不核具名原因）"
    return f"crates/{crate} 全树找不到 #[test] fn {name}("


def artifact_migration_check() -> None:
    """T1-2：产物 schema/version 关系图与迁移表（校准 C3）。

    `schema_registry_check` 只核「谁产谁消 + fixture 落地」——没有任何一处核「旧产物读进来时是
    走迁移还是具名拒绝」。七颗分别钉：

    ① 登记面在盘、顶层四格自述齐全（schema_version / kind / updated_at / registry），且
       `registry` 指向的模式登记册实例真的在盘（关系图不许指一份不存在的台账）；
    ② `artifacts` 的 `schema_id` 集合与 `maturity/schema-registry.json` 的 `entries` **逐一相等**，
       且行数不低于地板——关系图漏一份在册契约（新契约不登记版本处置）或多一份不存在的产物都红；
    ③ 每行七格齐全，`on_unknown_version` 落在允许集里，且**允许集与实用集相等**——往表里加
       一种新处置而不扩判据的允许集，等于给自己发一张免检通行证，当场红；
    ④ `refuse` 行：读侧文件在盘，且**真的**声明 `pub const <version_const>: u32` 并**真的**在
       正文里出现 `!= <version_const>`——「读侧会比对版本」这句话必须落在代码上，不是只写在表里；
    ⑤ `refuse` 行：`refusal_reason` 逐字出现在读侧文件里（表里的文案是代码里那一句，不是转述），
       且 `version_field` 也在文件里（版本字段名不许是表里自造的）；④⑤ 都只读**代码**（剥掉整行
       `//` 注释）——把真调用退回旧写法、只在注释里留锚点，照样红；
    ⑥ `refuse` 行：`refusal_case` 是读侧所属 crate 全树里的 `#[test]`，且用例体真的对拒绝文案
       说话（`contains(` 或点名被拒版本）——裸 `assert!(…is_err())` 不算；
    ⑦ `migrations` 每行四格齐全（from_version / to_version / migration_fn / reader）且
       `migration_fn` 在盘；今天为空表，空表是被判据承认的状态（本仓没有就地升级的读侧），
       但一旦登记就必须有在盘的迁移函数——「登记了迁移」不许是空头支票。
    """
    if not ARTIFACT_MIGRATION_FILE.is_file():
        check(
            False,
            "T1-2 版本关系图登记面在盘",
            f"缺失 {ARTIFACT_MIGRATION_FILE.relative_to(ROOT).as_posix()}",
        )
        return
    scalars, artifacts, migrations = _parse_artifact_migration(
        ARTIFACT_MIGRATION_FILE.read_text(encoding="utf-8")
    )
    registry_rel = scalars.get("registry", "")
    check(
        all(scalars.get(key) for key in ARTIFACT_MIGRATION_TOP)
        and scalars.get("kind") == ARTIFACT_MIGRATION_KIND
        and scalars.get("schema_version") == "1"
        and bool(registry_rel)
        and (ROOT / registry_rel).is_file(),
        "T1-2 登记面自述齐全且指向在盘的模式登记册实例",
        f"顶层 {sorted(scalars)} / registry={registry_rel!r} 不在盘",
    )
    try:
        registry = json.loads((ROOT / registry_rel).read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        check(False, "T1-2 登记面所指的模式登记册实例可解析", str(error))
        return
    registered = {entry.get("schema_id", "") for entry in registry.get("entries", [])}
    listed = {row.get("schema_id", "") for row in artifacts}
    check(
        len(artifacts) >= ARTIFACT_MIGRATION_MIN_ROWS
        and len(listed) == len(artifacts)
        and listed == registered,
        "T1-2 版本关系图与模式登记册按 schema_id 逐一相等（不多不少）",
        f"漏登记 {sorted(registered - listed)} / 多登记 {sorted(listed - registered)}"
        f" / 重复 {len(artifacts) - len(listed)}",
    )
    dispositions = {row.get("on_unknown_version", "") for row in artifacts}
    incomplete = [
        row.get("schema_id", "?")
        for row in artifacts
        if any(not row.get(field) for field in ARTIFACT_MIGRATION_FIELDS)
    ]
    check(
        not incomplete
        and dispositions == set(ARTIFACT_MIGRATION_DISPOSITIONS),
        "T1-2 每行七格齐全，且处置的允许集与实用集相等（新态必须同时扩判据）",
        f"缺格 {incomplete or '无'} / 实用 {sorted(dispositions)} vs 允许 "
        f"{sorted(ARTIFACT_MIGRATION_DISPOSITIONS)}",
    )
    gate_issues: list[str] = []
    reason_issues: list[str] = []
    case_issues: list[str] = []
    for row in artifacts:
        if row.get("on_unknown_version") != "refuse":
            continue
        schema_id = row.get("schema_id", "?")
        reader = ROOT / row.get("reader", "")
        if not reader.is_file():
            gate_issues.append(f"{schema_id} 读侧落空 {row.get('reader')}")
            reason_issues.append(f"{schema_id} 读侧落空")
            case_issues.append(f"{schema_id} 读侧落空")
            continue
        text = _artifact_code_only(reader)
        gate = row.get("version_const", "")
        if ARTIFACT_MIGRATION_CONST.format(gate=gate) not in text or f"!= {gate}" not in text:
            gate_issues.append(f"{schema_id} 缺 `pub const {gate}: u32` 或 `!= {gate}`")
        if row.get("refusal_reason", "") not in text or row.get("version_field", "") not in text:
            reason_issues.append(f"{schema_id} 拒绝文案或版本字段名不在读侧")
        crate = Path(row.get("reader", "")).parts[1:2]
        if not crate:
            case_issues.append(f"{schema_id} reader 路径取不出 crate")
            continue
        verdict = _refusal_case_verdict(
            crate[0], row.get("refusal_case", ""), row.get("version_field", "")
        )
        if verdict:
            case_issues.append(f"{schema_id} {verdict}")
    check(
        not gate_issues,
        "T1-2 读侧真的声明版本常量且真的在正文比对它",
        "；".join(gate_issues) or "无",
    )
    check(
        not reason_issues,
        "T1-2 登记面的拒绝文案与版本字段名逐字来自读侧代码",
        "；".join(reason_issues) or "无",
    )
    check(
        not case_issues,
        "T1-2 每条处置都有常驻用例，且用例对具名拒绝说话（不是裸 is_err）",
        "；".join(case_issues) or "无",
    )
    migration_issues: list[str] = []
    for row in migrations:
        label = f"{row.get('from_version', '?')}->{row.get('to_version', '?')}"
        if any(
            not row.get(field)
            for field in ("from_version", "to_version", "migration_fn", "reader")
        ):
            migration_issues.append(f"{label} 缺格")
            continue
        reader = ROOT / row["reader"]
        if not reader.is_file():
            migration_issues.append(f"{label} 读侧落空 {row['reader']}")
            continue
        if f"fn {row['migration_fn']}(" not in reader.read_text(encoding="utf-8"):
            migration_issues.append(f"{label} 迁移函数 {row['migration_fn']} 不在 {row['reader']}")
    check(
        not migration_issues,
        "T1-2 显式迁移表每行四格齐全且迁移函数在盘（空表合法）",
        f"不成立 {migration_issues or '无'}",
    )


# === T1-1（QX-DEV-PLAN-2026-10-10 阶段 1 / 退出门 G1 第一条）：运行证据包 ===
# 契约住在 qx-spec（对象 + schema），构建器住在 qx-cli（qx-app 尚未落地，见 T2-1 的纠偏声明）。
RUN_EVIDENCE_MODULE = ROOT / "crates" / "qx-spec" / "src" / "run_evidence.rs"
RUN_EVIDENCE_SCHEMA = ROOT / "schemas" / "run-evidence-v1.json"
RUN_EVIDENCE_BUILDER = ROOT / "crates" / "qx-cli" / "src" / "run_evidence.rs"
RUN_EVIDENCE_REPORT = ROOT / "crates" / "qx-cli" / "src" / "report_command.rs"
RUN_EVIDENCE_CLI_ARGS = ROOT / "crates" / "qx-cli" / "src" / "cli_args.rs"
RUN_EVIDENCE_HELP = ROOT / "crates" / "qx-cli" / "src" / "cli_help.rs"
RUN_EVIDENCE_CASES_DIR = ROOT / "crates" / "qx-cli" / "src" / "tests"
RUN_EVIDENCE_CASES = (
    "report_evidence_aggregates_a_recomputable_run_evidence_bundle",
    "report_evidence_is_not_written_when_the_verification_chain_refuses",
)
# 对象里声称「与 run 块逐字相等」的每一格 → run 块里被读的那一格。这张表是**判据的期望值**，
# 与 `check_cross_references` 里那组元组逐条对账：改一处而忘另一处当场红。
RUN_EVIDENCE_CROSS_PAIRS = (
    ("dataset.composed_fingerprint", "data_fingerprint"),
    ("identity.config_digest", "config_hash"),
    ("identity.strategy_version", "strategy_version"),
    ("build.code_commit", "code_commit"),
    ("build.runtime_version", "runtime_version"),
)


def _rust_struct_fields(text: str, name: str) -> list[str]:
    """取 `pub struct <name> { … }` 体里的 `pub <字段>:` 名单（按出现顺序）。"""
    match = re.search(rf"pub struct {re.escape(name)} \{{(.*?)\n\}}", text, re.DOTALL)
    if match is None:
        return []
    return re.findall(r"^\s*pub ([a-z_][a-z0-9_]*):", match.group(1), re.MULTILINE)


def run_evidence_check() -> None:
    """T1-1：运行证据包（退出门 G1 第一条「同一运行可由 RunManifest 离线复算」）。

    `foundation_specs_check` 只核「schema 与版本常量同源」——**字段级**是盲的：对象加一格而
    schema 没加（或反之），写出去的产物当场被自己的 schema 拒，而门禁照印全绿。八颗分别钉：

    ① `RunEvidenceBundle` 的 `pub` 字段集合与 schema 的顶层 `required` **逐一相等**（两边只写一份）；
    ② 三条对象层纪律落在**代码**上（剥掉整行 `//` 注释）：空 `unverified` 拒、未核摘要拒、
       档位越界拒——「写了纪律」与「纪律会拒」是两件事；
    ③ `check_cross_references` 里那组元组与判据自己的期望表**逐条相等**（标签集合、run 侧字段集合、
       以及每一格实际读的是哪一处 `self.<块>.<字段>`），且元组条数不为零——一张空表也能"不报错"；
    ④ 内容指纹与合成指纹**是两格**：`RunEvidenceDataset` 同时声明 `content_fingerprint` 与
       `composed_fingerprint`，且交叉比对只拿 `composed_fingerprint` 去比 `run.data_fingerprint`
       （真产物里是 `barframe:<内容哈希>` vs `<内容哈希>`，拿内容去比会拒掉每一份真产物）；
    ⑤ 质量报告允许**缺席**、不许**空**：Rust 侧是 `Option<RunEvidenceQualityReport>`，且
       `check_dataset` 里有 `usable_tiers.is_empty()` 的拒绝分支（`null` 是「没有这份报告」，
       空 tiers 是「核过了，没有任何一档可用」——后者是一句该被拒的断言）；
    ⑥ 构建器在盘且是**生产**代码（不在 `tests/` 下），并被非测试文件真的调用——T1-1 交付的是
       「构建器 + schema」，只有 schema 等于半件事；契约有了没人产，是这一族最典型的断链；
    ⑦ 入口使用者可达且**顺序**正确：`cli_args.rs` 声明 `--evidence` 能力位、help 写明它、
       且 `write_run_evidence` 的调用点排在 `recompute_declared_backtest_input` **之后**——
       顺序反了会在拒绝路径上先落一份没核过的 `artifact_digests_verified=true`；
    ⑧ 常驻用例在盘：正向一条 + 反向（复核拒绝时不留证据包）一条。
    """
    missing = [
        path.relative_to(ROOT).as_posix()
        for path in (
            RUN_EVIDENCE_MODULE,
            RUN_EVIDENCE_SCHEMA,
            RUN_EVIDENCE_BUILDER,
            RUN_EVIDENCE_REPORT,
            RUN_EVIDENCE_CLI_ARGS,
            RUN_EVIDENCE_HELP,
        )
        if not path.is_file()
    ]
    if missing:
        check(False, "T1-1 运行证据包契约与构建器都在盘", f"缺失 {missing}")
        return
    contract = RUN_EVIDENCE_MODULE.read_text(encoding="utf-8")
    builder = RUN_EVIDENCE_BUILDER.read_text(encoding="utf-8")
    report = RUN_EVIDENCE_REPORT.read_text(encoding="utf-8")
    cli_args = RUN_EVIDENCE_CLI_ARGS.read_text(encoding="utf-8")
    help_text = RUN_EVIDENCE_HELP.read_text(encoding="utf-8")
    code_only = _artifact_code_only(RUN_EVIDENCE_MODULE)

    # ① 字段集合两侧逐一相等。
    schema = json.loads(RUN_EVIDENCE_SCHEMA.read_text(encoding="utf-8"))
    declared = list(schema.get("required", []))
    rust_fields = _rust_struct_fields(contract, "RunEvidenceBundle")
    check(
        bool(rust_fields) and set(rust_fields) == set(declared) and len(rust_fields) == len(declared),
        "T1-1 运行证据包对象字段与 schema 顶层 required 逐一相等",
        f"Rust {sorted(rust_fields)} vs schema {sorted(declared)}",
    )

    # ② 三条纪律落在代码上。
    disciplines = {
        "空 unverified 拒": "if self.unverified.is_empty()",
        "未核摘要拒": "if !self.verification.artifact_digests_verified",
        "档位越界拒": "if self.verification.capability_level > Self::MAX_LOCAL_LEVEL",
    }
    absent = [label for label, anchor in disciplines.items() if anchor not in code_only]
    check(
        not absent,
        "T1-1 三条证据纪律落在代码上（空未验证清单 / 未核摘要 / 档位越界各有一条拒绝臂）",
        f"缺 {absent or '无'}",
    )

    # ③ 交叉一致性表与判据期望逐条相等。
    cross_body = _fn_body(contract, "fn check_cross_references(")
    # 元组有单行与多行两种写法（rustfmt 按行长决定），所以尾逗号是可选的。
    tuples = re.findall(
        r'\(\s*"([^"]+)",\s*&self\.([a-z_]+)\.([a-z_]+),\s*&self\.run\.([a-z_]+),?\s*\)',
        cross_body,
    )
    actual_pairs = {label: (block, field, run_field) for label, block, field, run_field in tuples}
    expected_pairs = {
        label: (label.split(".")[0], label.split(".")[1], run_field)
        for label, run_field in RUN_EVIDENCE_CROSS_PAIRS
    }
    check(
        len(tuples) == len(RUN_EVIDENCE_CROSS_PAIRS) and actual_pairs == expected_pairs,
        "T1-1 交叉一致性表与判据期望逐条相等（标签、被比字段、run 侧来源三处都对）",
        f"实测 {sorted(actual_pairs.items())}（期望 {sorted(expected_pairs.items())}）",
    )

    # ④ 内容指纹与合成指纹是两格，且比对用的是后者。
    dataset_fields = _rust_struct_fields(contract, "RunEvidenceDataset")
    dataset_schema = schema.get("properties", {}).get("dataset", {})
    dataset_required = set(dataset_schema.get("required", []))
    compared_run_fields = {run_field for _, _, _, run_field in tuples}
    check(
        {"content_fingerprint", "composed_fingerprint"} <= set(dataset_fields)
        and {"content_fingerprint", "composed_fingerprint"} <= dataset_required
        and "data_fingerprint" in compared_run_fields
        and not any(field == "content_fingerprint" for _, _, field, _ in tuples),
        "T1-1 内容指纹与合成指纹分成两格，交叉比对只拿合成指纹对 run.data_fingerprint",
        f"Rust 字段 {dataset_fields}；被比对 {sorted(compared_run_fields)}",
    )

    # ⑤ 质量报告允许缺席、不许空。
    check(
        "pub quality_report: Option<RunEvidenceQualityReport>" in contract
        and "usable_tiers.is_empty()" in code_only,
        "T1-1 质量报告可为 null（这一档输入没有报告）但不许是一份空报告",
        "quality_report 的类型或空报告拒绝臂被删弱",
    )

    # ⑥ 构建器是生产代码且被真的调用。
    builder_is_test = "tests" in RUN_EVIDENCE_BUILDER.relative_to(ROOT).as_posix().split("/")
    callers = [
        path.relative_to(ROOT).as_posix()
        for path in sorted((CRATES / "qx-cli" / "src").rglob("*.rs"))
        if "tests" not in path.relative_to(ROOT).as_posix().split("/")
        and "write_run_evidence(" in path.read_text(encoding="utf-8")
        and path != RUN_EVIDENCE_BUILDER
    ]
    check(
        "pub(crate) fn write_run_evidence(" in builder
        and not builder_is_test
        and bool(callers),
        "T1-1 运行证据包构建器是生产代码且有非测试调用者",
        f"构建器={'在盘' if builder else '缺'}；生产调用者 {callers or '无'}",
    )

    # ⑦ 入口可达且顺序正确。
    verify_at = report.find("recompute_declared_backtest_input(&summary)?")
    evidence_at = report.find("write_run_evidence(")
    check(
        "evidence: bool" in cli_args
        and "--evidence" in help_text
        and 0 <= verify_at < evidence_at,
        "T1-1 --evidence 使用者可达，且证据包写在复核链之后（拒绝路径不留半份证据包）",
        f"cli_args={'有' if 'evidence: bool' in cli_args else '缺'}；help={'有' if '--evidence' in help_text else '缺'}；"
        f"复核链@{verify_at} 证据包@{evidence_at}",
    )

    # ⑧ 常驻用例在盘。
    cases_text = "\n".join(
        path.read_text(encoding="utf-8")
        for path in sorted(RUN_EVIDENCE_CASES_DIR.rglob("*.rs"))
    )
    missing_cases = [name for name in RUN_EVIDENCE_CASES if f"fn {name}(" not in cases_text]
    check(
        not missing_cases,
        "T1-1 运行证据包正向与反向常驻用例都在盘",
        f"缺失 {missing_cases or '无'}",
    )


# 结果可读性层（易用性 P3）：HTML/SVG 报告的形状门禁。
REPORT_READABILITY_MODULES = (
    "crates/qx-cli/src/report_html.rs",
    "crates/qx-cli/src/report_svg.rs",
    "crates/qx-cli/src/report_command.rs",
)
# 报告模板里不得出现的 token（注释行不计）：内联 SVG 不带命名空间、无外链脚本/图片/样式。
REPORT_EXTERNAL_TOKENS = ("xmlns", "http", "<script", "<img", "<link ", "<iframe")
# 这份产物能被当研究件、不被当投资建议的底线声明。
REPORT_DISCLAIMER = "未连接真实交易所"


def _code_lines(relative: str) -> str:
    """取一份源码去掉注释行后的正文：模板泄漏检查只看代码与字符串字面量，不看注释里的说明。"""
    text = (ROOT / relative).read_text(encoding="utf-8")
    return "\n".join(
        line for line in text.splitlines() if not line.lstrip().startswith("//")
    )


def report_readability_check() -> None:
    """结果可读性层（易用性 P3 / 竞品对比 §6 P3）：`report --html` 产物的形状门禁。

    竞品默认交付一张标买卖点的图或一份交互式 HTML，我们此前只落 csv/summary，把「读结果」
    整段留给了用户（竞品对比 §1「看不见」）。这一层把那一格补上，判据守三条最容易悄悄退化的
    性质：模块在盘且在门槛内（不许长回单文件）、模板**无外部资源引用**（内联 SVG 不带命名空间、
    无外链脚本/图片/样式，出现即红——既防意外外链，也保证产物离线可双击打开）、以及
    「未连接真实交易所」声明必须还在（这是这份产物不被当成投资建议的底线）。
    """
    root = (CRATES / "qx-cli/src/main.rs").read_text(encoding="utf-8")
    missing = [rel for rel in REPORT_READABILITY_MODULES if not (ROOT / rel).is_file()]
    check(not missing, "结果可读性模块在盘", f"缺失 {missing or '无'}")
    oversized = [
        rel
        for rel in REPORT_READABILITY_MODULES
        if len((ROOT / rel).read_text(encoding="utf-8").splitlines()) >= OVERSIZED
    ]
    check(not oversized, "结果可读性模块在单文件行数门槛内", f"越界 {oversized or '无'}")
    unmounted = [
        Path(rel).stem
        for rel in REPORT_READABILITY_MODULES
        if not mount_pair_present(root, Path(rel).stem)
    ]
    check(not unmounted, "结果可读性模块在 crate 根成对挂载", f"缺配对 {unmounted or '无'}")
    leaks = [
        f"{rel}:{token}"
        for rel in REPORT_READABILITY_MODULES
        for token in REPORT_EXTERNAL_TOKENS
        if token in _code_lines(rel)
    ]
    check(not leaks, "报告模板无外部资源引用（出现外链 token 即红）", "；".join(leaks) or "无")
    html = (ROOT / REPORT_READABILITY_MODULES[0]).read_text(encoding="utf-8")
    check(
        REPORT_DISCLAIMER in html,
        "HTML 报告保留「未连接真实交易所」声明",
        f"缺少「{REPORT_DISCLAIMER}」",
    )
    callers = [
        path.relative_to(ROOT).as_posix()
        for path in sorted((CRATES / "qx-cli/src").rglob("*.rs"))
        if "/tests/" not in path.as_posix()
        and "fn write_report_html(" not in path.read_text(encoding="utf-8")
        and "write_report_html(" in path.read_text(encoding="utf-8")
    ]
    check(
        callers == ["crates/qx-cli/src/report_command.rs"],
        "报告落盘的调用点唯一（report 出口）",
        f"调用于 {callers}",
    )


# V13 R8：四种「看着像代码、其实什么都没说」的静默抑制形状。它们共同的特点是
# **改错了也不会红**：字段没人读就加一行 `let _ = x.y;` 把它按住、能力没接上就挂
# `#[allow(dead_code)]`、分支真到不了就写 `unreachable!()`、需求没做完就留 `TODO`。
# 门禁不管「理由是否成立」（那要靠人读），只管「理由必须当场写出来、且不许悄悄多出来」：
# 每一条都得在同一行或紧邻上一行留下出处，数量与登记表逐项相等，多一个就红。
SILENT_FIELD_DISCARD = re.compile(r"^\s*let\s+_\s*=\s*[A-Za-z_]\w*\.[A-Za-z_]\w*\s*;")
ALLOW_DEAD_CODE_LINE = re.compile(r"^\s*#\[allow\(dead_code\)\]")
# 生产源码里登记在册的 TODO/FIXME 标记：键为相对路径，值为登记次数（与允许清单同口径，
# 长出新的或删掉旧的都要同步改这里，否则两个方向都红）。
TODO_MARKER_ALLOWLIST: dict[str, int] = {"crates/qx-risk/src/rules.rs": 1}
STUB_MARKERS = ("todo!(", "unimplemented!(")


def _production_lines_keeping_comments() -> dict[str, list[str]]:
    """生产文本，但**保留注释行**：本轮四颗判据要看的是「代码旁边有没有写出理由」。

    与 `_production_sources()` 的唯一差别就是不丢注释 —— 那个函数丢注释是为了不让散文
    冒充证据，这里正好相反：注释本身就是本轮要核对的证据。
    """
    return {
        path.relative_to(ROOT).as_posix(): _strip_cfg_test_items(
            path.read_text(encoding="utf-8")
        )
        for path in sorted(CRATES.glob("*/src/**/*.rs"))
        if not TEST_PATH.search(path.relative_to(ROOT).as_posix())
    }


def silent_suppression_check() -> None:
    """V13 R8：静默抑制（丢字段、裸 allow、无消息 unreachable、无排期 TODO）全部上闸。"""
    sources = _production_lines_keeping_comments()

    discards = [
        f"{location}:{number}"
        for location, lines in sources.items()
        for number, line in enumerate(lines, 1)
        if SILENT_FIELD_DISCARD.match(line) and "//" not in line
    ]
    check(
        not discards,
        "生产源码里丢弃结构体字段值必须当场写明理由（`let _ = x.y;` 同行要带注释）",
        f"无理由丢弃 {discards or '无'}",
    )

    bare_allows = [
        f"{location}:{number}"
        for location, lines in sources.items()
        for number, line in enumerate(lines, 1)
        if ALLOW_DEAD_CODE_LINE.match(line)
        and "//" not in line
        and not (number > 1 and lines[number - 2].lstrip().startswith("//"))
    ]
    check(
        not bare_allows,
        "生产源码的 `#[allow(dead_code)]` 必须当场写明理由（同行或紧邻上一行的注释）",
        f"无理由 allow {bare_allows or '无'}",
    )

    message_less = [
        location
        for location, lines in sources.items()
        if sum(line.count("unreachable!(") for line in lines)
        != sum(line.count('unreachable!("') for line in lines)
    ]
    check(
        not message_less,
        "生产源码的 `unreachable!` 必须带非空消息（裸 `unreachable!()` 到不了现场就没人知道为什么）",
        f"无消息 {message_less or '无'}",
    )

    stubs = [
        f"{location}:{number}"
        for location, lines in sources.items()
        for number, line in enumerate(lines, 1)
        if any(marker in line for marker in STUB_MARKERS)
    ]
    check(
        not stubs,
        "生产源码不得留 `todo!`/`unimplemented!` 桩（要么实现，要么登记进 capabilities.yaml）",
        f"桩 {stubs or '无'}",
    )

    markers = {
        location: sum(line.count("TODO") + line.count("FIXME") for line in lines)
        for location, lines in sources.items()
    }
    markers = {location: count for location, count in markers.items() if count}
    check(
        markers == TODO_MARKER_ALLOWLIST,
        "生产源码的 TODO/FIXME 标记与登记表逐项相等（多一个或删一个都要同步登记表）",
        f"实得 {markers}，登记 {TODO_MARKER_ALLOWLIST}",
    )


# V13 R9：裸 `#[serde(default)]` 落在**非 `Option`** 的 Money/Price/Quantity 上，等于给
# 「这份回报没带该字段」和「交易所明确报了 0」发同一张身份证。本仓已有两条正确形状，且
# 都写在注释里：`Option<Money>`（`AccountPositionSnapshot` 的三格——注释明写「`None` 表示
# 这一份回报根本没带该字段，`Some(0)` 表示交易所明确说了是零」，并交代了此前是不可区分的
# `Money` 导致现货连接器把「没报」当成「没有浮亏」）与 `#[serde(default = "named_fn")]`
# （A 股规则配置：缺 `lot_size` 落到 `default_lot_size()` 而不是 0）。剩下的裸默认全仓只有
# 三处、都在 `qx-core/src/event.rs`，且危险方向都是 fail-closed，所以按名字登记而不是改
# 线格式（改线格式要动 EventLog 字节与全部读侧）。
BARE_SERDE_DEFAULT = re.compile(r"^\s*#\[serde\(default\)\]\s*$")
MONEY_FIELD_DECL = re.compile(r"^\s*(?:pub\s+)?([a-z_][a-z_0-9]*)\s*:\s*([^,]+),")
MONEY_TYPES = ("Money", "Price", "Quantity")
BARE_MONEY_DEFAULT_ALLOWLIST: dict[tuple[str, str], str] = {
    ("crates/qx-core/src/event.rs", "borrowed"): (
        "现货账户没有负债字段，缺键读成 0 是正确语义；净现金口径 free+locked-borrowed 由 "
        "AccountBalance::net_cash_raw 单点实现，且只用于对账观察，不直接改账簿"
    ),
    ("crates/qx-core/src/event.rs", "bid_qty"): (
        "老 EventLog 没有这两格，缺键读成 0；0 在 qx-execution/src/lib.rs 与 "
        "qx-runtime/src/pipeline.rs 一律判成退化报价（`<= 0` 即拒），不会当成无限容量"
    ),
    ("crates/qx-core/src/event.rs", "ask_qty"): "同 bid_qty：缺键读成 0，消费侧判退化报价",
}


def _bare_money_defaults() -> list[tuple[str, str]]:
    """生产源码里「裸 `#[serde(default)]` + 非 Option 的 Money/Price/Quantity 字段」清单。"""
    found: list[tuple[str, str]] = []
    for location, lines in _production_lines_keeping_comments().items():
        for index, line in enumerate(lines):
            if not BARE_SERDE_DEFAULT.match(line):
                continue
            for following in lines[index + 1 : index + 4]:
                stripped = following.strip()
                if not stripped or stripped.startswith("//") or stripped.startswith("#["):
                    continue
                match = MONEY_FIELD_DECL.match(following)
                if match and match.group(2).strip() in MONEY_TYPES:
                    found.append((location, match.group(1)))
                break
    return found


# 公共组件面：诊断（进程日志）为什么**没有**统一到一个框架上，以及它欠下的账有多大。
#
# 本仓对跨切面关注点的惯例是「单源 + 门禁钉住」（金额标度、结算币种、venue 身份、文件锁、
# IO 预算、错误码五元契约……）。诊断是唯一一处**没有**框架级单源的面：全仓没有任何
# `tracing`/`log`/`env_logger` 依赖，245 处诊断直接走 `println!`/`eprintln!`，靠一条非正式的
# `[组件 · 子域] 消息` 前缀约定保持可读（`qx-api` 与 `qx-orchestrator` 已全量带标签）。
# 这三颗把那个决定与它欠下的账钉下来：① 引入框架就必须**一次迁移完**（半迁移比现状更坏——
# 那是在既有约定之外多出**第三种**约定）；② 未带标签的站点数只降不升（棘轮）；③ 棘轮表是
# 活名册（归零的 crate 必须从表里删掉、新增未标签站点的 crate 必须登记），不许退化成一张
# 躺着不动的允许清单（与 `surface_allowlist_hygiene_check` 同一条纪律）。
DIAGNOSTIC_SITE_PATTERN = re.compile(r'(?:println|eprintln)!\s*\(\s*"((?:[^"\\]|\\.)*)"')
DIAGNOSTIC_FRAMEWORK_NAMES = ("tracing", "env_logger", "slog", "fern", "log4rs")
# 未带组件标签的诊断站点数上界（只降不升棘轮）。不在表里的 crate 一律要求 0。
# V13 R29：`qx-cli` 从 100 降到 55（本轮把 `cli.rs` 整段错误/诊断输出收到 `[qx-cli · CLI]` 标签下；
# `main.rs` 的 `rejected`/`acked` 与若干 JSON 直出站点被测试**逐字**断言，故意留在债里不迁）。
# 2026-10-10 T2-2：55 -> 53。`selfcheck.rs` 三处**本来就带标签**、只是把换行写进了字面量开头
# （`println!("\n[更路 · 重放校验]")`），于是被本判据误记成未标签站点——按约定拆成
# `println!()` + `println!("[组件 · 子域]")`（输出逐字节不变），三处归位；同轮 `qx-cli app` 新增
# 一处机器结果直出（`app_commands.rs` 的 payload 行，与 data_validate/plan/report 的 `--json`
# 同一形状），净额 55 - 3 + 1 = 53。**新加机器输出就要在同一轮把债还掉**，不是把上界抬上去。
UNLABELED_DIAGNOSTIC_CEILING = {"qx-cli": 53}


def _diagnostic_site_census() -> tuple[dict[str, int], int, int]:
    """按 crate 统计未带组件标签的诊断站点数，返回 (按 crate 的未标签数, 未标签总数, 站点总数)。

    只数**生产文本**（`_production_sources()` 已剔掉 `#[cfg(test)]` 整项、注释行与 tests/ 文件）：
    用例里的调试打印不是产品诊断面。
    """
    unlabeled: dict[str, int] = {}
    total = 0
    unlabeled_total = 0
    for location, lines in _production_sources().items():
        crate = location.split("/")[1]
        for match in DIAGNOSTIC_SITE_PATTERN.finditer("\n".join(lines)):
            total += 1
            if not match.group(1).startswith("["):
                unlabeled[crate] = unlabeled.get(crate, 0) + 1
                unlabeled_total += 1
    return unlabeled, unlabeled_total, total


def _declared_logging_frameworks() -> dict[str, list[str]]:
    """声明了诊断框架依赖的 crate（`crates/*/Cargo.toml` 的依赖键名，含 `[dependencies.x]` 形态）。"""
    found: dict[str, list[str]] = {}
    for manifest in sorted(CRATES.glob("*/Cargo.toml")):
        text = manifest.read_text(encoding="utf-8")
        keys = set(re.findall(r"^\s*([A-Za-z_][A-Za-z0-9_-]*)\s*=", text, re.MULTILINE))
        keys |= set(
            re.findall(
                r"^\[(?:dev-|build-)?dependencies\.([A-Za-z_][A-Za-z0-9_-]*)\]",
                text,
                re.MULTILINE,
            )
        )
        hits = sorted(keys & set(DIAGNOSTIC_FRAMEWORK_NAMES))
        if hits:
            found[manifest.parent.name] = hits
    return found


def process_diagnostics_check() -> None:
    """公共组件面：诊断框架的「一次迁移完」闸门 + 未标签站点的只降不升棘轮 + 活名册。"""
    frameworks = _declared_logging_frameworks()
    unlabeled, unlabeled_total, total = _diagnostic_site_census()
    check(
        not frameworks or unlabeled_total == 0,
        "引入诊断框架就必须一次迁移完：有框架依赖时未带组件标签的站点必须为 0（半迁移是第三种约定）",
        f"框架依赖 {frameworks} / 未标签站点 {unlabeled_total}（共 {total} 处）",
    )
    over = {
        crate: count
        for crate, count in unlabeled.items()
        if count > UNLABELED_DIAGNOSTIC_CEILING.get(crate, 0)
    }
    check(
        not over,
        "未带组件标签的诊断站点数只降不升（棘轮；不在表里的 crate 一律要求 0）",
        f"越界 {over} / 登记 {UNLABELED_DIAGNOSTIC_CEILING}",
    )
    check(
        set(unlabeled) == set(UNLABELED_DIAGNOSTIC_CEILING),
        "棘轮表是活名册：归零的 crate 必须从表里删掉，新增未标签站点的 crate 必须登记",
        f"实测 {sorted(unlabeled)} / 登记 {sorted(UNLABELED_DIAGNOSTIC_CEILING)}",
    )


def bare_money_default_check() -> None:
    """V13 R9：金额/价格/数量字段的「缺键」不许静默变成 0（要么 Option，要么命名默认值）。"""
    found = sorted(_bare_money_defaults())
    registered = sorted(BARE_MONEY_DEFAULT_ALLOWLIST)
    check(
        found == registered,
        "非 Option 的 Money/Price/Quantity 不得用裸 `#[serde(default)]`（要么改 Option、"
        "要么用命名默认值 `default = \"fn\"`、要么按名字登记）",
        f"实得 {found}，登记 {registered}",
    )


PRODUCTION_ENVIRONMENT_DEFINITION = "crates/qx-runtime/src/runtime_config/schema.rs"
PRODUCTION_ENVIRONMENT_CONST = 'pub const PRODUCTION_ENVIRONMENT: &str = "production";'
PRODUCTION_PREDICATE_SIGNATURE = "pub fn is_production("
# 收拢之前这 14 处各写一份手抄判定；现在它是"另起第二份"的指纹，值 = 允许出现的那一处。
# 与 venue 的"允许留一处"不同，这里改口径后一处都不该再出现，所以映射值为空元组。
ENVIRONMENT_REIMPLEMENTATION = {
    'eq_ignore_ascii_case("production")': (),
}
# 调 `.is_production()` 的生产文件。新增 production 专属闸门必须在此登记：
# 出口留着、调用点却退回手抄式，就是本族要防的静默降级。
PRODUCTION_PREDICATE_CONSUMERS = (
    "crates/qx-cli/src/live_check.rs",
    "crates/qx-cli/src/readiness.rs",
    "crates/qx-cli/src/strategy_binding.rs",
    "crates/qx-cli/src/strategy_contract.rs",
    "crates/qx-runtime/src/runtime_config/strategy_validation.rs",
    "crates/qx-runtime/src/runtime_config/topology_validation.rs",
)


def environment_production_single_source_check() -> None:
    """`environment == "production"` 只有一个出口（fam06，同 V13 §5 A3 的 venue 族）。

    审计时"这份运行时配置是不是 production"被手抄了 14 处：运行时配置校验内 9 处
    （strategy_validation 5 + topology_validation 4）各写一份
    `environment.eq_ignore_ascii_case("production")`，CLI 体检/就绪侧 5 处（live_check 1 +
    readiness 2 + strategy_binding 1 + strategy_contract 1）同样各写一份，而全仓没有一处
    单源的 `is_production`。危害不是"现在算错"——这 14 处今天都对——而是下一次改口径
    （例如把 `production` 收敛成 `prod`）时只改到其中几份：漏掉的那一处加固会**静默失效**，
    而它守的恰恰是"production 禁止明文 API""production C ABI 必须配 Ed25519 公钥"
    "production Execution worker 必须配名义额上限"这类闸门。收拢后：写法单源
    [`PRODUCTION_ENVIRONMENT`]、判定单源 [`RuntimeConfig::is_production`]，14 处全部改走出口。
    """
    production = {
        path.relative_to(ROOT).as_posix(): production_text(path.read_text(encoding="utf-8"))
        for path in sorted(CRATES.glob("*/src/**/*.rs"))
        if not TEST_PATH.search(path.relative_to(ROOT).as_posix())
    }
    schema = production.get(PRODUCTION_ENVIRONMENT_DEFINITION, "")
    definitions = sum(
        text.count(PRODUCTION_PREDICATE_SIGNATURE) for text in production.values()
    )
    check(
        schema.count(PRODUCTION_ENVIRONMENT_CONST) == 1
        and definitions == 1
        and PRODUCTION_PREDICATE_SIGNATURE in schema,
        "production 档的写法与「是不是 production」判定各只有一处定义",
        f"const={schema.count(PRODUCTION_ENVIRONMENT_CONST)} is_production={definitions}",
    )
    vocab_lines = [
        line
        for line in schema.splitlines()
        if "ENVIRONMENT_VOCAB" in line and "=" in line and "pub const" in line
    ]
    check(
        len(vocab_lines) == 1
        and "PRODUCTION_ENVIRONMENT" in vocab_lines[0]
        and '"production"' not in vocab_lines[0],
        "environment 词表从 PRODUCTION_ENVIRONMENT 取 production 这一档，不得另写字符串",
        f"词表行={vocab_lines}",
    )
    body = _fn_body(schema, PRODUCTION_PREDICATE_SIGNATURE)
    check(
        "PRODUCTION_ENVIRONMENT" in body and '"production"' not in body,
        "is_production 判定体必须引用 PRODUCTION_ENVIRONMENT，不得内联字面量",
        f"判定体={body!r}",
    )
    revived = {}
    for fragment, allowed in ENVIRONMENT_REIMPLEMENTATION.items():
        sites = [
            rel
            for rel, text in production.items()
            if fragment in text and rel not in allowed
        ]
        if sites:
            revived[fragment] = sites
    check(
        not revived,
        "已收拢的 production 判定式不得在生产代码里另起第二份",
        f"重新出现于 {revived}",
    )
    consumers = sorted(
        rel for rel, text in production.items() if ".is_production()" in text
    )
    check(
        consumers == sorted(PRODUCTION_PREDICATE_CONSUMERS),
        "调用 production 判定的生产文件与登记表逐一对应（新增闸门必须登记）",
        f"登记 {sorted(PRODUCTION_PREDICATE_CONSUMERS)} / 实际 {consumers}",
    )


CI_WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
CLI_CARGO_MANIFEST = ROOT / "crates" / "qx-cli" / "Cargo.toml"
# 真执行那条 NATS 腿必须**显式点名**两个 NATS 测试目标：`-- --ignored` 那条腿不点目标，
# 所以「点名了目标的那一步」与它不可能混淆，再要求那一步不带 --ignored/--no-run 即可。
NATS_LIVE_TARGETS = ("--test nats_jetstream", "--test nats_wait_budget")
NATS_CASE_FILES = (
    "crates/qx-storage/tests/nats_jetstream.rs",
    "crates/qx-storage/tests/nats_wait_budget.rs",
)
NATS_LIVE_CASE_FLOOR = 8


def _yaml_job_block(text: str, job: str) -> str:
    """按两空格缩进的顶层 job 名切出该 job 的文本块（不引 PyYAML）。"""
    match = re.search(rf"^  {re.escape(job)}:\s*$", text, re.MULTILINE)
    if match is None:
        return ""
    following = re.search(r"^  [A-Za-z0-9_-]+:\s*$", text[match.end():], re.MULTILINE)
    end = match.end() + following.start() if following else len(text)
    return text[match.end():end]


def _cli_feature_names() -> list[str]:
    """`crates/qx-cli/Cargo.toml` 的 `[features]` 段里除 `default` 外的键名。"""
    text = CLI_CARGO_MANIFEST.read_text(encoding="utf-8")
    match = re.search(r"^\[features\]\s*$", text, re.MULTILINE)
    if match is None:
        return []
    body = text[match.end():]
    following = re.search(r"^\[", body, re.MULTILINE)
    if following:
        body = body[: following.start()]
    names = re.findall(r"^([A-Za-z_][A-Za-z0-9_-]*)\s*=", body, re.MULTILINE)
    return [name for name in names if name != "default"]


def _without_yaml_comments(text: str) -> str:
    """去掉整行 `#` 注释：YAML 的注释符是 `#`（不是 Rust 的 `//`）。

    本轮实测：`service-backends` 里有一行注释写着「上一条腿只跑 `--ignored`」，若不剥注释，
    「`--ignored` 腿仍在盘」这颗判据会被**那句散文**满足——把真命令换成 `cargo version`
    也不红。注释是给读代码的人看的，不是给判据当证据的。
    """
    return "\n".join(
        line for line in text.splitlines() if not line.lstrip().startswith("#")
    )


# §17.11 登记的 CI 覆盖面缺口（V13 R28 收口）：`feature-matrix` 只 lint「特性开着」的组合，
# 而 `#[cfg(not(feature = "…"))]` 那半边在 workspace 构建里从不被 clippy 看到（`qx-cli` 的
# `default = ["sqlite"]` 经转发把 `qx-runtime/sqlite` 一并打开）。这一腿补上「特性全关」的另一半。
FEATURE_OFF_JOB = "feature-off"


def _workspace_feature_crates() -> dict[str, set[str]]:
    """每个 workspace crate 声明的**非默认**特性名（`crates/*/Cargo.toml` 的 `[features]`）。"""
    declared: dict[str, set[str]] = {}
    for manifest in sorted(CRATES.glob("*/Cargo.toml")):
        text = manifest.read_text(encoding="utf-8")
        match = re.search(r"^\[features\]\s*$", text, re.MULTILINE)
        if match is None:
            continue
        body = text[match.end():]
        following = re.search(r"^\[", body, re.MULTILINE)
        if following:
            body = body[: following.start()]
        names = set(re.findall(r"^([A-Za-z_][A-Za-z0-9_-]*)\s*=", body, re.MULTILINE))
        names.discard("default")
        if names:
            declared[manifest.parent.name] = names
    return declared


def _crates_with_feature_off_branches(declared: dict[str, set[str]]) -> set[str]:
    """`src/**` 里出现 `cfg(not(feature = "<自己声明的特性>"))` 的 crate。

    这些 crate 才有「关掉特性」才会编译的那半边代码，也就是 feature-off 腿真正要 lint 的对象。
    """
    found: set[str] = set()
    for name, features in declared.items():
        for source in sorted((CRATES / name / "src").rglob("*.rs")):
            text = source.read_text(encoding="utf-8")
            if any(f'cfg(not(feature = "{feature}"))' in text for feature in features):
                found.add(name)
                break
    return found


def ci_feature_matrix_check() -> None:
    """fam13：CI 特性矩阵必须点亮 qx-cli 的每颗特性闸门，且 NATS 用例有真执行的腿。

    §12.2 把这一族登记为「回退面」时明写：新补的那条 NATS 腿**只由 ci.yml 的文本存在性保证**，
    门禁里没有判据核它——把它删掉或加上 `--no-run` 不会红。特性矩阵同理：只在某些 feature
    分支里才存在的代码不会被默认特性的 lint 看到，矩阵一旦漏掉一颗特性，那颗特性的代码就
    从「有 lint」退成「无 lint」。这里钉四件事：① `feature-matrix` job 在盘，且三步
    （clippy/check/test）都按 `--no-default-features` 逐组合跑；② qx-cli 声明的每颗特性都
    至少出现在一个矩阵组合里，且矩阵里不出现不存在的特性名（拼错即红）；③ 存储契约里那条
    **点名**两个 NATS 测试目标的腿真执行（不带 `--ignored`/`--no-run`），且 `--ignored` 那条
    腿仍在盘（是并存而不是替换）；④ NATS 非 ignore 用例数不低于现场实测值。⑤ §17.11 登记过的
    另一半覆盖面：`feature-off` 腿在盘，且它点名的 crate 恰好等于「声明了非默认特性的每个
    workspace crate」（新增一颗特性 crate 必须进腿），同时覆盖所有真有
    `cfg(not(feature = "…"))` 分支的 crate（那才是这腿存在的理由）。
    """
    check(CI_WORKFLOW.is_file(), "CI 工作流存在", f"缺失 {CI_WORKFLOW.name}")
    if not CI_WORKFLOW.is_file():
        return
    ci_text = CI_WORKFLOW.read_text(encoding="utf-8")
    job = _yaml_job_block(ci_text, "feature-matrix")
    check(
        bool(job)
        and "--no-default-features" in job
        and '--features "${{ matrix.features }}"' in job
        and all(step in job for step in ("cargo clippy", "cargo check", "cargo test")),
        "feature-matrix job 按矩阵逐组合跑 clippy/check/test（--no-default-features）",
        f"job 可定位={bool(job)}",
    )
    declared = _cli_feature_names()
    entries = re.findall(r"^\s*-\s*([A-Za-z0-9_,]+)\s*$", job, re.MULTILINE)
    covered = {token for entry in entries for token in entry.split(",")}
    check(
        bool(declared) and set(declared) <= covered,
        "qx-cli 声明的每颗特性都出现在 CI 矩阵的某个组合里",
        f"声明 {sorted(declared)} / 覆盖 {sorted(covered)}",
    )
    check(
        not (covered - set(declared)),
        "CI 矩阵里的每个 token 都是 qx-cli 真有的特性（拼错即红）",
        f"多出 {sorted(covered - set(declared))}",
    )
    backends = re.sub(
        r"\s+",
        " ",
        _without_yaml_comments(_yaml_job_block(ci_text, "service-backends")),
    )
    steps = re.split(r"- name:", backends)
    nats_steps = [step for step in steps if NATS_LIVE_TARGETS[0] in step]
    check(
        len(nats_steps) == 1
        and all(target in nats_steps[0] for target in NATS_LIVE_TARGETS)
        and "--ignored" not in nats_steps[0]
        and "--no-run" not in nats_steps[0],
        "NATS 契约用例有一条真执行的腿（点名两个 NATS 测试目标、非 --ignored/--no-run）",
        f"点名 nats_jetstream 的腿有 {len(nats_steps)} 条"
        + (f"：{nats_steps[0][:160]}" if nats_steps else "（无）"),
    )
    check(
        re.search(r"cargo test -p qx-storage[^;]*--ignored", backends) is not None,
        "`--ignored` 那条存储契约腿仍在盘（真执行腿与它并存，不是替换）",
        "缺 --ignored 腿",
    )
    live_cases = 0
    for rel in NATS_CASE_FILES:
        text = (ROOT / rel).read_text(encoding="utf-8")
        live_cases += text.count("#[test]") - text.count("#[ignore")
    check(
        live_cases >= NATS_LIVE_CASE_FLOOR,
        f"NATS 非 ignore 用例不少于 {NATS_LIVE_CASE_FLOOR} 颗（少一颗就是有人删掉了执行对象）",
        f"实得 {live_cases}",
    )
    # §17.11 登记：`feature-matrix` 只 lint「特性开着」的组合，`#[cfg(not(feature = "…"))]`
    # 那半边在 workspace 构建里从不被 clippy 看到。三颗钉住补上来的那一腿。
    feature_off = _without_yaml_comments(_yaml_job_block(ci_text, FEATURE_OFF_JOB))
    # 三个旗标必须落在**同一个** clippy 步骤里：按整段 job 找子串时，`--all-targets` 会被
    # 相邻的 `cargo check` 步骤满足——摘掉 clippy 那一份也照绿（本轮变异 ② 实测踩到）。
    clippy_steps = [
        step for step in re.split(r"- name:", feature_off) if "cargo clippy" in step
    ]
    check(
        len(clippy_steps) == 1
        and all(
            flag in clippy_steps[0]
            for flag in ("--no-default-features", "--all-targets", "-D warnings")
        ),
        "feature-off 腿的 clippy 步骤按 --no-default-features --all-targets 跑且带 -D warnings",
        f"含 cargo clippy 的步骤 {len(clippy_steps)} 条"
        + (f"：{clippy_steps[0][:200]}" if clippy_steps else "（无）"),
    )
    declared_crates = _workspace_feature_crates()
    needs_off_leg = _crates_with_feature_off_branches(declared_crates)
    # 注释里写 `-p qx-runtime` 不算点名（`_without_yaml_comments` 已剥整行注释）。
    linted = set(
        re.findall(r"(?<![A-Za-z0-9-])-p\s+([A-Za-z0-9_-]+)", feature_off)
    )
    check(
        bool(needs_off_leg) and needs_off_leg <= linted,
        "有 `cfg(not(feature = …))` 分支的 crate 都在 feature-off 腿上（那半边代码只有这腿 lint 得到）",
        f"需要 {sorted(needs_off_leg)} / 腿上 {sorted(linted)}",
    )
    check(
        linted == set(declared_crates),
        "feature-off 腿点名的 crate 恰好是声明了非默认特性的每个 workspace crate（新增特性 crate 必须进腿）",
        f"腿 {sorted(linted)} / 声明 {sorted(declared_crates)}",
    )


PROMETHEUS_BUILDER_FILE = "crates/qx-api/src/lib.rs"
PROMETHEUS_TEST_FILE = "crates/qx-api/tests/prometheus_exposition.rs"
PROMETHEUS_LINE_CASE = "metrics_body_is_line_separated_prometheus_exposition"
OPS_ALERT_FILE = OUTBOX_ALERT_FILE
OPS_ALERT_RULE_FLOOR = 7


def ops_read_surface_check() -> None:
    """fam10：运维读面的 exposition 必须是真换行，告警名册的每个指标都要有生产端。

    V13 R2 第七遍实测到的缺陷形态是 `/metrics` 正文**只有一行**、行与行之间是字面的 `\\n`
    两个字符：抓取端会把整份正文读成一行、一条样本都解析不出来，而告警侧是「永不触发」而不是
    「报错」——服务端与告警侧都不会自己出声。这里钉四件事：① exposition 构造函数用真换行
    （体里出现 `\\n` 转义、不出现字面的 `\\\\n`）；② 那条「逐行解析」的用例在盘，且它的解析器
    显式拒绝字面 `\\n`；③ 告警名册里出现的每个 `qx_*` 指标都能在生产源码里找到写出点（名字对
    不上就是一条永不触发的告警）；④ 名册的每条规则都有 expr/severity/summary。
    """
    builder = production_text(
        (ROOT / PROMETHEUS_BUILDER_FILE).read_text(encoding="utf-8")
    )
    body = _fn_body(builder, "pub fn to_prometheus(")
    check(
        bool(body) and "\\n" in body and "\\\\n" not in body,
        "Prometheus exposition 构造函数用真换行，不用字面 `\\\\n`（V13 R2 第七遍）",
        f"体可定位={bool(body)}；含真换行={'\\n' in body}；含字面双反斜杠 n={'\\\\n' in body}",
    )
    exposition_case = (ROOT / PROMETHEUS_TEST_FILE).read_text(encoding="utf-8")
    has_case = f"fn {PROMETHEUS_LINE_CASE}(" in exposition_case
    rejects_literal = '!body.contains("\\\\n")' in exposition_case
    check(
        has_case and rejects_literal,
        "逐行解析用例在盘：正文里出现字面反斜杠+n 即断言失败（不是 contains 读名字）",
        f"用例在册={has_case}；解析器拒绝字面换行={rejects_literal}",
    )
    roster = _without_yaml_comments((ROOT / OPS_ALERT_FILE).read_text(encoding="utf-8"))
    referenced = sorted(set(re.findall(r"qx_[a-z0-9_]+", roster)))
    produced = "\n".join("\n".join(lines) for lines in _production_sources().values())
    orphans = [name for name in referenced if name not in produced]
    check(
        bool(referenced) and not orphans,
        "告警名册里每个 qx_ 指标都能在生产源码里找到写出点（否则该告警永不触发）",
        f"引用 {referenced}；无生产者 {orphans}",
    )
    rules = re.split(r"^\s*- alert:", roster, flags=re.MULTILINE)[1:]
    incomplete = [
        index
        for index, rule in enumerate(rules, 1)
        if not ("expr:" in rule and "severity:" in rule and "summary:" in rule)
    ]
    check(
        len(rules) >= OPS_ALERT_RULE_FLOOR and not incomplete,
        f"告警名册不少于 {OPS_ALERT_RULE_FLOOR} 条规则，每条都有 expr/severity/summary",
        f"实得 {len(rules)} 条；缺格的序号 {incomplete}",
    )


API_WS_FILE = "crates/qx-api/src/ws.rs"
API_ROOT_FILE = "crates/qx-api/src/lib.rs"
SHUTDOWN_TOKEN_FIELD = "session_shutdown: Arc<AtomicBool>"
SHUTDOWN_SET = "self.session_shutdown.store(true, Ordering::Release);"
SHUTDOWN_CHECK = "self.session_shutdown.load(Ordering::Acquire)"
IDLE_BOUND_CONST = "const WS_MAX_IDLE_ROUNDS: u32 = 18_000;"


def api_read_model_liveness_check() -> None:
    """fam02 的「停机令牌 + 连接上界」两格：会话循环必须读停机令牌、必须有空闲上界。

    V13 R2 #218 实测的缺陷：监听循环按 `stopped()` 收摊后，已经握手的会话若不读停机令牌，
    线程就永远等在 `wait_after` 的 100ms 轮询里，`join()` 回不来，停机只能靠强杀。另一半是
    空闲上界：对端半开（不发 FIN、也不再写一个字节）时读永远 `TimedOut`、写永远成功，
    那条线程与它占的连接预算就永久留在账上。这里钉四件事：① 停机令牌字段只有一处、且真被
    `store(true, Release)` 置起（有生产者）；② 会话帧循环**在阻塞等待之前**先读令牌
    （`load` 出现在第一个 `wait_after(` 之前）；③ 读到停机令牌时回 `server_shutdown` 并
    正常返回；④ 空闲轮次上界常量只有一处、且真被用来收摊（`idle_rounds >=` 那一处）。
    """
    ws_text = production_text((ROOT / API_WS_FILE).read_text(encoding="utf-8"))
    root_text = production_text((ROOT / API_ROOT_FILE).read_text(encoding="utf-8"))
    check(
        root_text.count(SHUTDOWN_TOKEN_FIELD) == 1
        and root_text.count(SHUTDOWN_SET) >= 1
        and root_text.count("session_shutdown: Arc::new(AtomicBool::new(false))") >= 1,
        "停机令牌字段只有一处，且真被置起（有生产者、初始化关闭）",
        f"字段 {root_text.count(SHUTDOWN_TOKEN_FIELD)} 处；"
        f"置起 {root_text.count(SHUTDOWN_SET)} 处；"
        f"初始化 {root_text.count('session_shutdown: Arc::new(AtomicBool::new(false))')} 处",
    )
    session = _fn_body(ws_text, "pub(crate) fn serve_websocket<S: Read + Write>(")
    check_index = session.find(SHUTDOWN_CHECK)
    wait_index = session.find("wait_after(")
    check(
        check_index >= 0
        and wait_index >= 0
        and check_index < wait_index
        and "server_shutdown" in session,
        "会话帧循环在阻塞等待之前先读停机令牌，读到即回 server_shutdown 并返回（V13 R2 #218）",
        f"令牌读点={check_index}；首个 wait_after={wait_index}；"
        f"（读点必须存在且早于 wait_after）",
    )
    check(
        ws_text.count(IDLE_BOUND_CONST) == 1
        and ws_text.count("idle_rounds >= WS_MAX_IDLE_ROUNDS") == 1
        and "idle_timeout" in ws_text,
        "会话有空闲轮次上界，越限即主动收摊（半开对端不得把连接预算永久占住）",
        f"上界常量 {ws_text.count(IDLE_BOUND_CONST)} 处；"
        f"比较点 {ws_text.count('idle_rounds >= WS_MAX_IDLE_ROUNDS')} 处",
    )


SURFACE_ALLOWLIST_REASON_MIN = 6


def surface_allowlist_hygiene_check() -> None:
    """fam11 的「分层处置」闭环：允许清单里不许留幽灵条目，理由不许是占位符。

    零读者那一族的判据（`zero_reference_public_surface_check` 等）只遍历**在盘的**定义，
    所以「函数被删掉了、清单条目还在」这件事它永远看不见——条目会一直躺在那儿，下一次有人
    把同名函数加回来时，它就变成一张现成的免检通行证。这是那条闭环上唯一没被咬住的口子。
    另半边是理由本身：`""`/`TBD` 这类占位符与没登记等价。
    """
    sources = _production_sources()
    defined = set(_collect_surface_definitions(sources, PUB_SURFACE_DEF))
    defined |= set(_collect_surface_definitions(sources, PUB_CRATE_SURFACE_DEF))
    phantom = sorted(set(PUBLIC_SURFACE_ALLOWLIST) - defined)
    check(
        not phantom,
        "允许清单里每个条目都对应一处真实定义（函数删了、条目还在 = 幽灵条目）",
        f"幽灵条目 {phantom or '无'}",
    )
    placeholders = sorted(
        key
        for key, reason in PUBLIC_SURFACE_ALLOWLIST.items()
        if len(reason.strip()) < SURFACE_ALLOWLIST_REASON_MIN
    )
    check(
        not placeholders,
        f"允许清单的每条理由都不是占位符（至少 {SURFACE_ALLOWLIST_REASON_MIN} 个字符）",
        f"过短 {placeholders or '无'}",
    )


# —— fam12 / WP-12：CLI 表面「命令表逐颗用例 + config 子命令集与 help 同口径」——
# `cli_help_surface_check` 只核「help ≡ clap 表 ≡ cli.rs 派发」三侧集合相等，看不见「某入口一敲
# 就崩 / 它自己的用法渲染不出来」；`cli_dispatch_check` 只核分派点唯一。这两件由本族的用例与判据
# 补上：`crates/qx-cli/tests/command_surface.rs` 对每条命令实跑一次 `--help`（真走一遍 clap 解析
# 与该命令自己的用法渲染）。清单不是装饰——判据核它真的被逐条消费。
CLI_SURFACE_TEST_FILE = "crates/qx-cli/tests/command_surface.rs"
CLI_COMMANDS_CONST = re.compile(r"const CLI_COMMANDS: \[&str; (\d+)\] = \[(.*?)\];", re.S)
# `config` 子命令在 help 里是 `  config <sub> …` 形状（两空格缩进，与顶层入口同一缩进级）。
HELP_CONFIG_LINE = re.compile(r"^  config ([a-z][a-z0-9-]*)", re.MULTILINE)
# 二级入口清单：`"config explain"` 这类「父 子」两段式字符串。
CLI_NESTED_COMMANDS_CONST = re.compile(r"const NESTED_COMMANDS: \[&str; (\d+)\] = \[(.*?)\];", re.S)


# CLI 参数源的合并文本：命令表与子命令表不再只住在 cli_args.rs——`data_validate_args.rs` /
# `plan_args.rs` / `console_args.rs` 早就把「参数结构体单独成文件」做成常规（cli_args.rs 顶格在
# 行数棘轮上），T2-2 的 `app_args.rs` 是第一个**自带子命令**的那种。子命令表若只扫 cli_args.rs，
# 那种父命令会被当成「没有子命令」，它的叶子从此没人跑过 `--help`——所以两个取名册的助手都按
# 「全 CLI 参数源」取事实。
CLI_ARGS_AUX_GLOB = "*_args.rs"


def cli_args_sources() -> str:
    """`cli_args.rs` 与所有外部参数结构文件的合并文本（见 [`CLI_ARGS_AUX_GLOB`]）。"""
    root = ROOT / CLI_ARGS_FILE
    paths = [root] + sorted(root.parent.glob(CLI_ARGS_AUX_GLOB))
    return "\n".join(path.read_text(encoding="utf-8") for path in paths)


def _block_body(text: str, header: str) -> str:
    """从 `header` 起取到下一个顶格的 `}` 为止（`enum`/`struct` 的花括号体）。"""
    start = text.find(header)
    if start < 0:
        return ""
    body = text[start:]
    end = body.find("\n}")
    return body if end < 0 else body[:end]


def _subcommand_variant(body: str) -> str | None:
    """取一段变体/结构体定义里 `#[command(subcommand)]` 字段的类型名。

    可见性前缀可有可无：`cli_args.rs` 的内联变体写的是私有字段（`action: Option<ConfigCommand>`），
    而单独成文件的参数结构写的是 `pub(crate) action: Option<AppCommand>`。
    """
    matched = re.search(
        r'#\[command\(subcommand\)\]\s*\n\s*(?:pub(?:\([^)]*\))?\s+)?\w+\s*:\s*Option<([A-Za-z]\w*)>',
        body,
    )
    return matched.group(1) if matched else None


def clap_subcommand_parents(args_text: str, sources: str) -> dict[str, str]:
    """顶层里带 `#[command(subcommand)]` 的父命令：`命令名 -> 子命令枚举类型`。

    子命令枚举名从字段类型取事实（`action: Option<ConfigCommand>`），不按 `+Command` 后缀猜——
    变体名是 `Config`、枚举名是 `ConfigCommand`，两者并无机械对应，猜就会把判据钉在错的锚上。

    两种写法都认：内联结构体变体（`Config { #[command(subcommand)] action: Option<ConfigCommand> }`），
    以及把参数结构单独成文件的那种（`App(AppArgs)`，子命令字段在 `app_args.rs` 里）——后者要按
    **全 CLI 参数源**（`sources`）再找一次。少认这一种，父命令会被当成"没有子命令"，
    它的叶子就再没人实跑 `--help`（这是 T2-2 引入第一个外部子命令父命令时暴露出来的盲区）。
    """
    start = args_text.find("pub(crate) enum Command {")
    if start < 0:
        return {}
    body = args_text[start:]
    end = body.find("\n}\n")
    block = body if end < 0 else body[:end]
    parents: dict[str, str] = {}
    for chunk in block.split('#[command(name = "')[1:]:
        name = chunk.split('"', 1)[0]
        variant = _subcommand_variant(chunk)
        if variant is None:
            tuple_matched = re.search(
                rf'{re.escape(name)}"\)\]\s*\n\s*[A-Za-z]\w*\(([A-Za-z]\w*)\)', chunk
            )
            if tuple_matched is not None:
                variant = _subcommand_variant(
                    _block_body(sources, f"struct {tuple_matched.group(1)} {{")
                )
        if variant is not None:
            parents[name] = variant
    return parents


def clap_nested_command_table(sources: str, variant: str) -> set[str]:
    """某个子命令枚举的子命令集：`pub(crate) enum <variant> { … }` 块里的 `#[command(name=…)]`。

    `sources` 是**全 CLI 参数源**的合并文本（见 [`cli_args_sources`]）：子命令枚举可以和顶层
    命令表不在同一个文件里（`AppCommand` 在 `app_args.rs`）。
    """
    block = _block_body(sources, f"pub(crate) enum {variant} {{")
    return set(re.findall(r'#\[command\(name = "([a-z][a-z0-9-]*)"\)\]', block))


def cli_surface_coverage_check() -> None:
    """fam12 的牙齿：命令表逐颗用例、config 子命令集与 help 同口径、二级入口逐条用例。

    「命令表逐颗用例」不是把名字抄进一张常量就完事——那样清单会退化成装饰。判据同时核两件：
    清单与 clap 表逐一相等（新增命令没进清单 = 红；清单留了已删命令 = 红），且清单真的被逐条
    喂给被测 binary 跑 `--help`（只列不跑 = 红）。`config` 子命令集同理：clap 的 `ConfigCommand`
    变体名必须与 help 印出的 `config <sub>` 行逐一相等，否则「help 里有、敲下去没有」或反之——
    与顶层入口同一类断链，只是发生在第二层。
    **二级入口**（`config <sub>` / `run <sub>` / `strategy <sub>` / `backtest <sub>`）此前只靠顶层
    那张表覆盖父命令，`backtest ccxt-builtin`、`strategy list` 这类叶子一颗用例都没点过名：它一敲
    就崩、或自己的用法渲染不出来时没人红。判据对 `NESTED_COMMANDS` 常量核同样的两件——与四个父
    命令的 clap 子命令表逐一相等，且真的被逐条喂给被测 binary 跑 `--help`。
    """
    args_text = (ROOT / CLI_ARGS_FILE).read_text(encoding="utf-8")
    table = clap_command_table(args_text)
    test_text = (ROOT / CLI_SURFACE_TEST_FILE).read_text(encoding="utf-8")
    matched = CLI_COMMANDS_CONST.search(test_text)
    listed = set(re.findall(r'"([a-z][a-z0-9-]*)"', matched.group(2))) if matched else set()
    declared = int(matched.group(1)) if matched else -1
    check(
        matched is not None and declared == len(listed) and listed == set(table),
        "命令表逐颗用例：CLI_COMMANDS 清单与 clap 表逐一相等",
        f"清单 {len(listed)} 项（声明 {declared}）；clap 表 {len(table)} 项；"
        f"清单有而 clap 无 {sorted(listed - set(table)) or '无'}；"
        f"clap 有而清单无 {sorted(set(table) - listed) or '无'}",
    )
    check(
        "for name in CLI_COMMANDS" in test_text
        and 'args([name, "--help"])' in test_text,
        "逐颗用例真的把清单逐条喂给被测 binary 跑 `--help`（而不是只把名字列在常量里）",
        "CLI_COMMANDS 没被逐条消费：清单会退化成一张没人用的装饰表",
    )
    # `config` 子命令集：clap 的 `ConfigCommand` 变体名 ≡ help 的 `config <sub>` 行。
    config_start = args_text.find("pub(crate) enum ConfigCommand {")
    config_block = args_text[config_start:]
    config_end = config_block.find("\n}\n")
    config_subs = set(
        re.findall(
            r'#\[command\(name = "([a-z][a-z0-9-]*)"\)\]',
            config_block if config_end < 0 else config_block[:config_end],
        )
    )
    help_source = (ROOT / CLI_HELP_FILE).read_text(encoding="utf-8")
    help_body = help_source.split('r#"', 1)[-1].split('"#', 1)[0]
    documented = set(HELP_CONFIG_LINE.findall(help_body))
    check(
        bool(config_subs) and config_subs == documented,
        "config 子命令集与 help 印出的 `config <sub>` 行逐一相等",
        f"clap 子命令 {sorted(config_subs)}；help 印出 {sorted(documented)}",
    )
    # 二级入口逐条用例：`NESTED_COMMANDS` 必须与四个父命令的 clap 子命令表逐一相等，
    # 且真的被逐条喂给被测 binary 跑 `--help`（与顶层那张表同一口径）。
    expected_nested = {
        f"{parent} {sub}"
        for parent, enum_name in clap_subcommand_parents(args_text, cli_args_sources()).items()
        for sub in clap_nested_command_table(cli_args_sources(), enum_name)
    }
    nested_matched = CLI_NESTED_COMMANDS_CONST.search(test_text)
    nested_listed = (
        set(re.findall(r'"([a-z][a-z0-9-]* [a-z][a-z0-9-]*)"', nested_matched.group(2)))
        if nested_matched
        else set()
    )
    nested_declared = int(nested_matched.group(1)) if nested_matched else -1
    check(
        nested_matched is not None
        and nested_declared == len(nested_listed)
        and nested_listed == expected_nested,
        "二级入口逐颗用例：NESTED_COMMANDS 清单与 clap 子命令表逐一相等",
        f"清单 {len(nested_listed)} 项（声明 {nested_declared}）；clap 表 {len(expected_nested)} 项；"
        f"清单有而 clap 无 {sorted(nested_listed - expected_nested) or '无'}；"
        f"clap 有而清单无 {sorted(expected_nested - nested_listed) or '无'}",
    )
    check(
        "for spec in NESTED_COMMANDS" in test_text,
        "二级入口清单真的被逐条喂给被测 binary 跑 `--help`（而不是只把名字列在常量里）",
        "NESTED_COMMANDS 没被逐条消费：清单会退化成一张没人用的装饰表",
    )


# V11 §40 D1：文件写锁的判据只有一个定义点。这里的危害方式与信封/退避同族——
# "抢锁靠 create_new、释放靠删文件"一旦被就地重写第二份，丢掉的就是孤儿锁接管那条出路，
# 而那条出路只有在进程被杀之后才看得见，用例最容易漏。
FILE_LOCK_KERNEL = "crates/qx-core/src/file_lock.rs"
FILE_LOCK_CONSUMERS = (
    "crates/qx-data/src/registry.rs",
    "crates/qx-zhenlu/src/lib.rs",
    "crates/qx-storage/src/state_envelope.rs",
    "crates/qx-storage/src/file/jobs.rs",
)


# fam04（V13 §9.48 回退面逐族重落）：适配层 IO 预算与 venue 缓存。子进程 stdin 的三处写入点
# 与两条长度预算的落点。写阻塞没有用例能在"不挂死"的前提下断言（变异实测：摘掉预算退回调用
# 线程上的裸 `write_all`，用例是挂住而不是变红），所以只能由门禁守住"三处都不许退回裸写"。
STDIN_BUDGET_SITES = (
    ("crates/qx-adapter/src/ccxt.rs", "CCXT Worker stdin 不可用"),
    ("crates/qx-cli/src/strategy_host.rs", "worker stdin 不可用"),
    ("crates/qx-cli/src/event_pipeline.rs", "事件 consumer handler stdin"),
)
IO_BUDGET_FILE = "crates/qx-adapter/src/io_budget.rs"
VENUE_CACHE_FILE = "crates/qx-adapter/src/venue_cache.rs"
ADAPTER_ROOT_FILE = "crates/qx-adapter/src/lib.rs"
# 每个 venue 的订单缓存 growth 点数量，以及退场时必须一起级联掉的派生索引（按空白压平后比对）。
VENUE_CACHE_SITES = (
    (
        "crates/qx-adapter/src/binance.rs",
        3,
        (
            "self.venue_order_ids.retain(|client_id,_|!evicted.contains(client_id))",
            "self.seen_fill_keys.retain(|key|!evicted.contains(&key.0))",
        ),
    ),
    (
        "crates/qx-adapter/src/ccxt.rs",
        2,
        (
            "self.remote_ids.retain(|client_id,_|!evicted.contains(client_id))",
            "self.seen_trade_ids.retain(|client_id,_|!evicted.contains(client_id))",
            "self.cumulative_costs.retain(|client_id,_|!evicted.contains(client_id))",
        ),
    ),
)
BINANCE_FILE = "crates/qx-adapter/src/binance.rs"
# V11 R4-8：针路 OMS 的订单表刻意不共用上面那把封顶——登记的是"刻意的不同"，不是漏网。
OMS_FILE = "crates/qx-zhenlu/src/oms.rs"


def io_budget_and_venue_cache_check() -> None:
    """fam04：三处子进程 stdin 写入都进预算、venue 订单缓存封顶且级联退派生索引。

    两条链路各自有一处"没有用例能在不挂死/不丢事实的前提下断言"的形状，所以判据只能钉
    静态形状：写入点不许退回裸 `write_all`；封顶只退终态订单、越限才扫表、退场连派生索引
    一起退，且 OMS 那张表刻意不共用它。
    """
    stdin_regressions = []
    for rel, marker in STDIN_BUDGET_SITES:
        body = production_text((ROOT / rel).read_text(encoding="utf-8"))
        if ".write_all(" in body:
            stdin_regressions.append(f"{rel} 又直接在调用线程上 write_all 子进程 stdin")
        elif "write_all_within(" not in body or marker not in body:
            stdin_regressions.append(f"{rel} 不再经带预算的写入通道")
    check(
        not stdin_regressions,
        "三处子进程 stdin 写入都走 write_all_within，没有一处退回裸 write_all（V11 N8）",
        "；".join(stdin_regressions)
        + "（配置里的 timeout_ms 于是只保护读、不保护写：对端不读时管道写满，"
        "调用线程永远停在写那行）",
    )
    io_budget = production_text((ROOT / IO_BUDGET_FILE).read_text(encoding="utf-8"))
    check(
        io_budget.count("recv_timeout(timeout)") == 1
        and io_budget.count(".recv()") == 0
        and io_budget.count("on_timeout();") == 1
        and io_budget.count('=> Err(format!("写入失败: {error}")),') == 1,
        "预算只按截止时间收写线程，且只有超时那一格负责把打断管道的责任交给调用方（V11 N8）",
        "无截止的 `.recv()` 让超时分支永不成立；普通写错误若也走 on_timeout，正常退出"
        "的子进程会被顺手杀掉，两条分支在产物里就分不出是谁杀的",
    )
    # V11 N11：WebSocket 的长度预算必须同时管住单帧、整条消息与单次轮询的帧数，且各只有一个
    # 具名常量。只卡单帧时，对端把一条超大消息切成一串各自合法的片段就能把读侧缓冲无限堆大。
    adapter = production_text((ROOT / ADAPTER_ROOT_FILE).read_text(encoding="utf-8"))
    check(
        adapter.count("const MAX_WEBSOCKET_FRAME_BYTES: u64 = 16 * 1024 * 1024;") == 1
        and adapter.count("const MAX_WEBSOCKET_MESSAGE_BYTES: usize = 16 * 1024 * 1024;") == 1
        and adapter.count("const MAX_WEBSOCKET_FRAMES_PER_POLL: usize = 64;") == 1
        and adapter.count("if length > MAX_WEBSOCKET_FRAME_BYTES {") == 1
        and adapter.count(
            "current.payload.len().saturating_add(payload.len()) > MAX_WEBSOCKET_MESSAGE_BYTES"
        )
        == 1
        and adapter.count("if consumed_frames > MAX_WEBSOCKET_FRAMES_PER_POLL {") == 1,
        "WebSocket 长度预算按单帧 / 整条消息 / 单次轮询三层各用一个具名常量（V11 N11）",
        "只卡单帧时，对端把一条超大消息切成一串各自合法的片段就能把行情读侧的缓冲无限堆大"
        "——那条链路是每条订阅共用的一颗进程；两处用不同的数则会出现『单帧放行、整条拒收』"
        "这种没人能解释的口径",
    )
    # V11 N9：柜台适配器的订单缓存是启动期灌进来的历史副本，必须有封顶。封顶本身可以断言
    # （越限退场后条数、未终态订单还在场、派生索引跟着退），但"退错东西"的代价是丢事实，
    # 所以退场资格与失败收口两头都要钉住。
    cache = production_text((ROOT / VENUE_CACHE_FILE).read_text(encoding="utf-8"))
    check(
        cache.count("pub(crate) const MAX_CACHED_ORDERS: usize = 8_192;") == 1
        and cache.count("const ORDER_EVICT_HYSTERESIS: usize = 1_024;") == 1
        and cache.count("if orders.len() <= MAX_CACHED_ORDERS {") == 1
        and cache.count(".filter(|(_, order)| order.status.is_terminal())") == 1
        and cache.count(".take(overflow)") == 1
        and cache.count(".retain(|client_id, _| !evicted.contains(client_id))") == 1,
        "venue 订单缓存封顶只挑终态订单，且越限才扫表、一次退到迟滞线（V11 N9）",
        "去掉 is_terminal 那层筛选就是拿丢活跃订单换内存——被退场的单还在接成交回报，"
        "下一笔成交会按『未知本地订单』炸开；去掉越限判断则启动期每条历史订单都扫一遍全表",
    )
    cache_growths = []
    for rel, sites, derived in VENUE_CACHE_SITES:
        body = production_text((ROOT / rel).read_text(encoding="utf-8"))
        if body.count("let evicted = evict_stale_terminal_orders(&mut self.orders);") != sites:
            cache_growths.append(f"{rel} 的封顶调用点不是 {sites} 处")
        if "fn forget_orders(&mut self, evicted: &BTreeSet<u64>)" not in body:
            cache_growths.append(f"{rel} 缺少派生索引级联")
        flat = "".join(body.split())
        for statement in derived:
            if statement not in flat:
                cache_growths.append(f"{rel} 的 {statement.split('.')[1]} 没跟着订单退场")
    check(
        not cache_growths,
        "两个常驻 venue 的每处订单写入都过封顶，退场时级联清掉全部派生索引（V11 N9）",
        "；".join(cache_growths)
        + "（只退 Order 不退远端订单号与成交去重键，缓存里照样留着账户全部历史，"
        "封顶就成了摆设；漏掉一个 growth 点，那个点灌进来的历史就绕过封顶常驻）",
    )
    binance_body = production_text((ROOT / BINANCE_FILE).read_text(encoding="utf-8"))
    check(
        binance_body.count("Binance 用户回报对应未知本地订单") == 1
        and binance_body.count("if !self.orders.contains_key(&client_order_id) {") == 1,
        "已退场订单的回报仍按分歧升级对账，不被缓存策略静默收下（V11 N9）",
        "封顶一旦同时删掉 bind_remote_order 的未知订单闸门，退场就从一个内存问题变成"
        "『柜台回报认不到本地订单时没人报警』——那条回报会被当成可忽略的账户事件丢掉",
    )
    # V11 R4-8：上面那把封顶只管柜台适配器自己那份缓存，针路 OMS 的订单表刻意没有共用它。
    # 这不是漏网，是一处不对称：实盘 ingest 认不到本地订单会升级对账，回填却把同一种
    # "订单不存在"直接问号上抛——照抄封顶的净效果是账户历史长过迟滞线之后 EventLog 再也打不开。
    oms_prod = production_text((ROOT / OMS_FILE).read_text(encoding="utf-8"))
    check(
        "evict_stale_terminal_orders" not in oms_prod
        and "MAX_CACHED_ORDERS" not in oms_prod
        and "const MAX_" not in oms_prod
        and ".retain(" not in oms_prod,
        "针路 OMS 的订单表没有偷偷共用 venue 那把封顶（V11 R4-8：登记的是刻意的不同）",
        "封顶一旦抄进 oms.rs，被退场的订单在实盘侧走 ReconcileRequired（那是既有的一等回答），"
        "在回填侧却走 `apply_fill(fill)?` 上抛——回填是按事件顺序重放整条日志的，退掉一笔早期"
        "终态订单就等于把它后面每一笔成交都变成『订单不存在』，open() 当场失败",
    )


def file_lock_single_source_check() -> None:
    """文件写锁只有一个内核定义点，四处消费全部委托它（V11 §40 D1）。

    旧形状是四处各自 `OpenOptions::create_new` 抢锁、释放时删文件；释放依赖进程活着走到
    那一步，Ctrl-C / OOM / 断电留下的锁文件会让对应链路此后每一次运行都失败，且报错既不
    点名锁在哪、也不给任何出路。收口后判据守三件：定义点唯一、没有任何一处再就地删锁文件、
    四个消费文件都引用内核。另加一颗守「同一场竞争只接管一次」住在纯判据函数里（而不是
    退回循环里的一句 `if` 守卫），并由抢锁循环把自家计数喂回去。
    """
    crate_sources = {
        path.relative_to(ROOT).as_posix(): path.read_text(encoding="utf-8")
        for path in sorted(CRATES.glob("*/src/*.rs")) + sorted(CRATES.glob("*/src/**/*.rs"))
        if "_test" not in path.name
    }
    lock_defs = sorted(
        rel
        for rel, source in crate_sources.items()
        if re.search(r"^pub struct FileLock\b", source, re.MULTILINE)
    )
    check(
        lock_defs == [FILE_LOCK_KERNEL],
        "文件写锁 FileLock 只有一个定义点（qx-core::file_lock）",
        f"定义于 {lock_defs}，期望 {[FILE_LOCK_KERNEL]}",
    )
    hand_rolled = sorted(
        rel
        for rel, source in crate_sources.items()
        if rel != FILE_LOCK_KERNEL
        and re.search(r"remove_file\([^;]*lock", source, re.IGNORECASE)
    )
    check(
        not hand_rolled,
        "锁文件的抢占与释放不再各写一份（只有内核删自己的锁）",
        f"就地实现锁生命周期于 {hand_rolled}",
    )
    for rel in FILE_LOCK_CONSUMERS:
        source = crate_sources.get(rel, "")
        delegates = "FileLock::acquire" in source
        regressed = re.search(r"remove_file\([^;]*lock", source, re.IGNORECASE)
        check(
            delegates and not regressed,
            f"{Path(rel).name} 的写锁委托 qx-core::file_lock",
            f"引用统一锁={'是' if delegates else '否'}；就地锁算式={'有' if regressed else '无'}",
        )
    # "同一场竞争只接管一次"这条判据必须住在纯判据函数里，并由抢锁循环把自家计数喂回去。规则若
    # 退回循环里的一句 `if` 守卫，纯函数就答不出"第二次见到孤儿锁"——那一支在单进程用例里永远走不到
    # （删掉孤儿锁之后下一轮 create_new 必然成功），于是连"只接管一次"这个命名都在空转。判据在函数里、
    # 循环却不传计数，同样等于没有这条判据（V11 D1 变异 L3 的两半）。
    kernel_source = crate_sources.get(FILE_LOCK_KERNEL, "")
    rule_in_kernel = "age >= stale_after && !takeover_used" in kernel_source
    loop_feeds_the_flag = (
        re.search(
            r"decide_lock\(\s*[^,]+,\s*policy\.stale_after,\s*takeover_used\s*\)", kernel_source
        )
        is not None
    )
    # 年龄只由判据交出的那一格带回报出的口径（V11 R7-9）：循环自己再 `age_of` 一次并把
    # `Takeover { .. }` 丢掉，同一份观察就有了两种写法——判据看到的年龄与说出去的年龄可以分叉。
    verdict_carries_the_age = (
        "LockDecision::Takeover { age } => {" in kernel_source
        and "LockDecision::Wait { age } => {" in kernel_source
        and "last_age = age_of(&path)" not in kernel_source
    )
    check(
        rule_in_kernel
        and loop_feeds_the_flag
        and verdict_carries_the_age
        and kernel_source.count("takeover_used = true;") == 1,
        "锁的『同一场竞争只接管一次』住在 decide_lock 里，且抢锁循环把计数喂回判据、只按判据交出的年龄说话",
        f"判据在纯函数={'是' if rule_in_kernel else '否'}；循环回传计数={'是' if loop_feeds_the_flag else '否'}"
        f"；年龄由判据带回={'是' if verdict_carries_the_age else '否'}"
        f"；计数写入 {kernel_source.count('takeover_used = true;')} 次",
    )


def fee_settlement_currency_check() -> None:
    """成交手续费只能记在它自己的币种上（V11 D2）。

    `Fill.fee_currency` 由 Binance 的 `commissionAsset` 与 CCXT 的 `fee_currency` 填充、
    并已进入事件指纹，归约侧却可以整份不读它——异币种抵扣的手续费会被按面值记进结算币种，
    账本凭空多（或少）一笔钱且没有任何信号（0.001 BNB 记成 0.001 USDT 低估两个数量级）。
    正确形状只有三档：同币种/未报币种按面值；基准资产费用用**这笔成交自己的价格**折算
    （不需要外部汇率，挡下它等于挡掉主力连接器上每天正常发生的全部成交）；其余币种拒记并
    转待对账。衍生条款不折算——那一层要过 `contract_size` 与 inverse 口径，算错的代价比拒记更高。
    """
    fill_source = production_text(
        (ROOT / "crates/qx-core/src/ledger/fill.rs").read_text(encoding="utf-8")
    )
    resolvers = len(re.findall(r"fee_in_settlement_raw\(", fill_source))
    check(
        resolvers >= 3 and "fill.fee_currency" in fill_source and "ReconcileRequired" in fill_source,
        "手续费币种判定住在唯一的折算函数里，现货与衍生两条记账入口都过它（V11 D2）",
        f"fee_in_settlement_raw 出现 {resolvers} 次（定义 + 两处调用），"
        f"读取 fee_currency={'fill.fee_currency' in fill_source}",
    )
    check(
        fill_source.count("Money::from_raw(-fill.fee.raw())") == 0
        and fill_source.count("Money::from_raw(-fee_raw)") == 2,
        "费用腿一律记折算后的数，不记回报里的原数（按面值入账正是 D2 的原始缺陷）",
        f"-fill.fee.raw() 残留 {fill_source.count('Money::from_raw(-fill.fee.raw())')} 处、"
        f"-fee_raw {fill_source.count('Money::from_raw(-fee_raw)')} 处",
    )
    check(
        "fn base_asset_of(" in fill_source
        and "checked_mul(fee_raw)" in fill_source
        and "checked_div(SCALE)" in fill_source,
        "基准资产费用用成交自身价格折算（定点乘除，不引入外部汇率）",
        "base_asset_of 或折算的 checked 乘除被删掉",
    )
    check(
        "multiplier == 1" in fill_source
        and fill_source.count("fee_in_settlement_raw(fill, currency, None)") == 1,
        "折算只对乘数为 1 的现货口径开放，衍生条款严格认结算币种",
        "乘数闸门或衍生侧的严格口径不见了",
    )
    ledger_cases = case_source("crates/qx-core/tests/ledger")
    missing = [
        name
        for name in (
            "spot_base_asset_fee_converts_at_the_fill_price",
            "base_asset_fee_is_refused_when_the_symbol_does_not_name_it",
            "multiplier_path_does_not_convert_base_asset_fees",
            "base_asset_fee_that_converts_to_zero_leaves_no_fee_leg",
            "fill_with_foreign_fee_currency_is_not_booked_at_face_value",
            "derivative_fill_with_foreign_fee_currency_is_rejected",
            "fee_currency_gate_only_bites_on_reported_foreign_currency",
        )
        if f"fn {name}(" not in ledger_cases
    ]
    check(
        not missing,
        "七条用例分别钉住「基准资产费用按成交价折算、符号没点名则拒、乘数路径不折算、"
        "折到 0 不留腿、第三币种拒记、衍生同拒、闸门只管异币种」",
        f"crates/qx-core/tests/ledger 少了 {missing}",
    )


KERNEL_DEAD_FILE_NAMES = ("engine.rs", "queue.rs")
KERNEL_DEAD_SYMBOLS = ("TestClock", "ClockError", "CausalQueue", "EngineCtx", "EngineRunReport")
KERNEL_PROMISE_PHRASES = ("确定性时钟", "因果事件队列")
KERNEL_PROMISE_FILES = ("README.md", "crates/qx-core/src/lib.rs", "crates/qx-core/Cargo.toml")
KERNEL_README_ANCHORS = ("内核里没有时钟对象", "append_at_engine", "qx-xingban", "EventLog")
KERNEL_PRIORITY_CONSTANTS = ("TIMER", "FEEDBACK", "MARKET", "COMMAND", "MATCH", "APPLY", "POST")
KERNEL_APPEND_RULE = "if(effective_ts,priority)<=(previous.ts,previous.prio)"


def kernel_timeline_check() -> None:
    """qx-core 的时间轴只剩单位口径，因果序由写入处的单调判据负责（V11 P2）。

    README 与 Cargo.toml 曾把内核描述成"有确定性时钟与因果事件队列"，而 `clock.rs` 自己写着
    "这里**不提供虚拟时钟**"、`lib.rs` 写着"内核不提供虚拟时钟对象"。文档承诺一件代码里从未
    发生的事，正是本仓反复要堵的那半颗缺陷（§13.1 修 README 过度声明的同族）。判据守三件：
    那三件死实现（引擎 / 因果队列 / 时钟）不得以原名回到 `crates/`；三份口径来源不再写旧承诺、
    且 README 改口到真实推进路径；因果优先级数值只有一处真相、Python 参考形状逐项跟着它走。
    """

    def posix(p: Path) -> str:
        return str(p.relative_to(ROOT)).replace("\\", "/")

    def flat_text(path: Path) -> str:
        # 压掉空白再比：rustfmt 会按行宽折声明，字面拆行不该让门禁假红。
        return "".join(path.read_text(encoding="utf-8").split())

    sources = sorted(CRATES.rglob("*.rs"))
    revived = [posix(p) for p in sources if p.name in KERNEL_DEAD_FILE_NAMES]
    source_texts = [p.read_text(encoding="utf-8") for p in sources]
    redefinitions = [
        name
        for name in KERNEL_DEAD_SYMBOLS
        if any(
            re.search(rf"\b(struct|enum|trait|impl)\s+{name}\b", text) for text in source_texts
        )
    ]
    check(
        not revived and not redefinitions,
        "被删掉的引擎、因果队列与时钟没有以原名回到 crates/（V11 P2）",
        f"复现的文件 {revived or '无'}、重新定义过的符号 {redefinitions or '无'}：这三件曾是零调用的实现，"
        "抄回来就等于重新制造「文档承诺、代码没接」那半颗缺陷",
    )
    core_lib = "".join((ROOT / "crates/qx-core/src/lib.rs").read_text(encoding="utf-8").split())
    ts_defs = [posix(p) for p in sources if "pubtypeTs=u64;" in flat_text(p)]
    check(
        ts_defs == ["crates/qx-core/src/clock.rs"]
        and "pubmodclock;" in core_lib
        and "pubmodengine;" not in core_lib
        and "pubmodqueue;" not in core_lib
        and "pubuseself::clock::Ts;" in core_lib
        and all(name not in core_lib for name in KERNEL_DEAD_SYMBOLS),
        "`clock.rs` 只剩一格单位口径 `pub type Ts = u64;`，且它是内核里唯一一份（V11 P2）",
        f"`Ts` 的定义落在 {ts_defs or '没有一处'}（期望恰好 crates/qx-core/src/clock.rs）："
        "这条别名是 event/order/sourcing 共用的时间戳口径，删不掉也不该有第二份写法；"
        "引擎与队列的模块声明一旦回来，lib.rs 的公开面就重新长出零调用的入口",
    )
    lingering = [
        f"{rel} 仍写着「{phrase}」"
        for rel in KERNEL_PROMISE_FILES
        for phrase in KERNEL_PROMISE_PHRASES
        if phrase in (ROOT / rel).read_text(encoding="utf-8")
    ]
    check(
        not lingering,
        "三份口径来源都不再承诺那三件死实现（V11 P2）",
        f"残留 {lingering or '无'}：这三处是读者真正会读的句子，留着旧承诺就等于让文档替一段"
        "不存在的代码作保",
    )
    readme = (ROOT / "README.md").read_text(encoding="utf-8")
    missing = [needle for needle in KERNEL_README_ANCHORS if needle not in readme]
    check(
        not missing
        and "TIMER < FEEDBACK < MARKET < COMMAND < MATCH < APPLY < POST" in readme
        and "qx-xingban" in core_lib
        and "pipeline.rs" in core_lib,
        "设计底线改口到真实推进路径：回测按 bar 序列、实盘按到达顺序推进 `EventLog`（V11 P2）",
        f"README 缺 {missing or '无'}：这几格是「时间轴由推进方决定」的可读证据，"
        "少了任何一格，改口就退化成另一句「不用系统时间」式的空话",
    )
    rust_prio = {
        name: int(value)
        for name, value in re.findall(
            r"pub const ([A-Z_]+): u8 = (\d+);",
            (ROOT / "crates/qx-core/src/event.rs").read_text(encoding="utf-8"),
        )
    }
    py_text = (ROOT / "tools/validate_core.py").read_text(encoding="utf-8")
    py_match = re.search(
        r"(PRIO_[A-Z_]+(?:\s*,\s*PRIO_[A-Z_]+)*)\s*=\s*\(([^)]*)\)", py_text
    )
    python_prio: dict[str, int] = {}
    if py_match is not None:
        names = re.findall(r"PRIO_([A-Z_]+)", py_match.group(1))
        values = [int(v) for v in re.findall(r"\d+", py_match.group(2))]
        python_prio = dict(zip(names, values))
    check(
        set(rust_prio) == set(KERNEL_PRIORITY_CONSTANTS)
        and all(rust_prio[k] < rust_prio["POST"] for k in rust_prio if k != "POST")
        and python_prio == rust_prio,
        "因果优先级的数值只有一处真相，Python 参考形状逐项跟着它走（V11 P2）",
        f"Rust {sorted(rust_prio.items())} vs Python {sorted(python_prio.items())}：队列删掉之后，"
        "这条序的全部实现就是「数值 + 写入处单调」，数值分叉会让两边对同一条 (ts, prio) 判出不同结果",
    )
    pipeline = "".join(
        production_text(
            (ROOT / "crates/qx-runtime/src/pipeline.rs").read_text(encoding="utf-8")
        ).split()
    )
    check(
        pipeline.count(KERNEL_APPEND_RULE) == 1 and "fnappend_at_engine(" in pipeline,
        "(ts, prio) 单调落盘的判据只有写入处那一份，且在正文不在用例里（V11 P2）",
        f"命中 {pipeline.count(KERNEL_APPEND_RULE)} 次（期望 1）：这条判据是删掉调度队列之后因果序的"
        "全部实现，出现第二份写法就意味着两条写路径可以对同一个时点排出不同的顺序",
    )


# 阶段四 M1'/M2'/M3'：Web 控制台（只读读面 + 控制面写面）。路线图 §18 明写「不应把 HTTP/WS API
# 自动称为前后端完整贯通」——贯通要有一条**可核对的接线表**，而不是"页面能打开"。前端调用一个
# 后端不存在的端点、或它绕过控制面直连下单端点，都必须在这里当场变红。
WEB_CONSOLE_DIR = "web/console"
WEB_CONSOLE_FILES = ("index.html", "app.js", "styles.css")
# 控制台**唯一**允许的写面：控制面受理入口。它不直接下单——下单要走这条命令，由控制面
# 受理后交给执行者，所以"页面直连某个下单端点"这类形状必须被禁掉（下面那份名单）。
WEB_CONSOLE_WRITE_PATH = "/control/commands"
WEB_CONSOLE_WRITE_CALL = "postJson(API_PATHS.controlCommands"
WEB_CONSOLE_FORBIDDEN_WRITE_TOKENS = (
    "/order/submit",
    "binance-submit-order",
    "paper-submit-order",
    "/control/commands/execute",
)
# M3' 的三阶段与状态词表：页面复述的是控制面的语义（受理 202 / 执行者判定 / 终态退场），
# 状态词表直接取 `qx-control::CommandStatus` 的变体名。
WEB_CONSOLE_PHASES = ("受理", "执行者判定", "终态退场")
WEB_CONSOLE_STATUS_VOCAB = ("Accepted", "Executed", "Failed")


def web_console_check() -> None:
    """控制台与 qx-api 路由表逐一接线，写面只有控制面受理这一条（V13 R23 · M1'/M2'/M3'）。

    M1'/M2' 时这条判据守的是"它不许长出写操作"；M3' 把它推进到控制面之后，守的东西变成
    **写面唯一**：读面板照旧只读，唯一的写操作是 `POST /control/commands`，而它不直接下单。
    另外钉住三阶段在盘与身份边界如实交代——页面不能自声明 operator，那是 mTLS 边界的事。
    """
    console = ROOT / WEB_CONSOLE_DIR
    missing = [name for name in WEB_CONSOLE_FILES if not (console / name).is_file()]
    check(
        not missing,
        "Web 控制台的三件在盘（index.html / app.js / styles.css）",
        f"缺失 {missing}",
    )
    if missing:
        return
    app = (console / "app.js").read_text(encoding="utf-8")
    html = (console / "index.html").read_text(encoding="utf-8")
    # 只看代码，不看注释：控制台的注释里会解释"受理 202 不是已经执行"，那是说明而不是逻辑。
    # 与门禁别处「先剥整行注释再判」同口径。
    code = re.sub(r"//[^\n]*", "", app)
    code = re.sub(r"/\*.*?\*/", "", code, flags=re.S)
    # 控制台在 app.js 里逐条点名它要调的路径；逐条与 qx-api 的路由表核对。
    declared = set(re.findall(r'"(/[a-z][a-z0-9/_-]*)"', code))
    api = production_text((ROOT / "crates/qx-api/src/lib.rs").read_text(encoding="utf-8"))
    routes = {
        path for _method, path in re.findall(r'\("(GET|POST|PUT|DELETE)", "(/[^"]*)"\)', api)
    }
    unknown = sorted(path for path in declared if path not in routes)
    check(
        bool(declared) and not unknown,
        "控制台引用的每个 API 路径都在 qx-api 路由表里（前端 ⇔ 后端接线表逐一相等）",
        f"控制台引用了后端不存在的路径 {unknown}；路由表 {sorted(routes)}",
    )
    check(
        "cors_allowed_origins" in html,
        "控制台写明跨源读取必须在部署配置登记 cors_allowed_origins（否则是浏览器 CORS 拦截，不是服务端故障）",
        "index.html 丢了那条 CORS 说明，用户会把跨源拦截误读成后端坏了",
    )
    check(
        "qx-cli serve" in html,
        "控制台写明后端服务入口是 `qx-cli serve <runtime.json>`",
        "没有启动入口说明的控制台等于一个打不开的页面",
    )
    posts = code.count('method: "POST"')
    intruders = [token for token in WEB_CONSOLE_FORBIDDEN_WRITE_TOKENS if token in code]
    check(
        posts == 1
        and WEB_CONSOLE_WRITE_CALL in code
        and f'"{WEB_CONSOLE_WRITE_PATH}"' in code
        and not intruders,
        "控制台的写面只有一条：POST /control/commands（下单只能走这条命令，页面不得直连任何下单端点）",
        f"POST 调用 {posts} 处 / 受理入口 {WEB_CONSOLE_WRITE_CALL in code} / 越界写面 {intruders}",
    )
    check(
        all(phase in app + html for phase in WEB_CONSOLE_PHASES)
        and all(status in code for status in WEB_CONSOLE_STATUS_VOCAB),
        "控制面三阶段在盘：受理 → 执行者判定 → 终态退场（状态词表 Accepted/Executed/Failed 与 qx-control 同源）",
        f"缺阶段 {[p for p in WEB_CONSOLE_PHASES if p not in app + html]} / "
        f"缺状态词 {[s for s in WEB_CONSOLE_STATUS_VOCAB if s not in code]}",
    )
    check(
        "authenticated_operator_required" in code,
        "控制台如实交代身份边界：operator 来自 mTLS 认证边界，页面不能自声明（403 authenticated_operator_required）",
        "页面没处理 403，用户会把'身份来自认证边界'误读成'这个页面坏了'",
    )
    check(
        "isLoopbackControlTarget(base)" in code
        and 'parsed.hostname === "127.0.0.1"' in code
        and 'parsed.hostname === "localhost"' in code,
        "静态控制台连接入口只允许本机回环：没有同源 BFF / CSRF / 会话时不能连远端 API",
        "连接判定被放宽后，静态控制台会被当成可公网使用的控制台",
    )
    check(
        "isLoopbackControlTarget(state.base)" in code
        and "renderLocalOnlyRejected" in code,
        "命令提交前再次检查本机回环边界，拒绝路径向用户说明静态包不是生产控制台",
        "控制命令入口绕过了 local-only 边界",
    )
    check(
        "function bffEntryBase()" in code
        and 'has("token")' in code
        and "const seeded = saved || bffEntryBase();" in code,
        "同源 BFF 形态的入口 URL 预填本源：带 ?token= 的入口只有 BFF 一种解释，监听地址不必从横幅手抄",
        "预填被摘掉（连接入口回到空白）或条件丢失（静态形态也预填），后者会把静态服务的源当成 API 源连上去",
    )
    check(
        "if (!base) {" in code and "if (!base) return;" not in code,
        "空基地址要有可见反馈（徽标 + 健康面板一行），「连接」不能静默无反应",
        "静默返回回来了：点按钮没有任何信号，运维会读成页面坏了或已经连上",
    )


# ---- V13 R25：控制台 ⇔ 后端线格式的**字段级**接线 ----
#
# `web_console_check` 只比路径：路由集合相等就印绿，一个字段都不看（`api_surface_doc_check`
# 在文档那一侧栽过同一个盲区，本轮前端这一侧也栽了——页面照着后端从来没有的键开了三列，
# 于是那三列永久是 —，而徽标印「已连接」）。这一节把判据推进到"页面读的每个键名都必须是
# 后端真的序列化出来的那一组"，四份事实源全部从 Rust 源码取，不在门禁里写第二份手抄清单。
CONSOLE_APP_JS = "web/console/app.js"
CONSOLE_INDEX_HTML = "web/console/index.html"
# 名册 → 该形状在 Rust 侧的唯一事实源：(文件, 结构体名)。这四个形状是 serde 直接派生的。
CONSOLE_SERDE_SOURCES = {
    "positionRow": ("crates/qx-protocol/src/wire.rs", "PositionSnapshot"),
    "orderRow": ("crates/qx-protocol/src/wire.rs", "OrderSnapshot"),
    "eventRow": ("crates/qx-core/src/event.rs", "Event"),
    "envelope": ("crates/qx-protocol/src/lib.rs", "ProjectionEnvelope<T>"),
}
# 快照那两层不是 serde：写侧是 `AccountSnapshot::to_json` 里那一条手写的 format! 字面量，
# 深度（身份在 `header` 里）与「cash_raw 是按币种的 map」只在那条字面量里表达。
CONSOLE_SNAPSHOT_SOURCE = ("crates/qx-protocol/src/lib.rs", "QIANXING_ACCOUNT")
CONSOLE_SNAPSHOT_ROSTERS = ("snapshotTop", "snapshotHeader")
# 余额那一格是 `/account/balances` 的 `json!` 响应体，键清单从那个臂体里取。
CONSOLE_BALANCES_MARKER = '("GET", "/account/balances") =>'
# 写面也是线格式：`POST /control/commands` 的请求体被直接 `serde_json::from_str` 成
# `ControlCommand`（没有中间的 DTO），命令种类与权限档是 serde 外部标签枚举。页面把这两格做成
# `<select>`，那份选项清单就是前端唯一能表达的词表——后端加一档而页面没有那一格，那条命令永远
# 发不出去；页面列了后端不认的一档，用户只会拿到 400。读面名册已逐格钉过，写面这一格此前没人核对。
CONSOLE_CONTROL_SOURCE = ("crates/qx-control/src/lib.rs", "ControlCommand")
CONSOLE_CONTROL_ENUMS = {"cmd-kind": "CommandKind", "cmd-permission": "Permission"}


def _js_object_literal(source: str, declaration: str) -> str:
    """取 `declaration` 之后第一个花括号平衡的对象字面量（含两侧花括号）。"""
    start = source.find(declaration)
    if start < 0:
        return ""
    brace = source.find("{", start)
    if brace < 0:
        return ""
    depth = 0
    for index in range(brace, len(source)):
        depth += source[index] == "{"
        depth -= source[index] == "}"
        if depth == 0:
            return source[brace:index + 1]
    return source[brace:]


def js_string_arrays(source: str, declaration: str) -> dict[str, list[str]]:
    """`const NAME = { key: ["a", "b"], … };` → `{key: [a, b]}`。元素必须是带引号的字面量。"""
    body = _js_object_literal(source, declaration)
    rosters: dict[str, list[str]] = {}
    for match in re.finditer(r"([A-Za-z0-9_]+):\s*\[([^\]]*)\]", body):
        items = [item.strip() for item in match.group(2).split(",") if item.strip()]
        assert all(
            item.startswith('"') and item.endswith('"') and len(item) >= 2 for item in items
        ), f"{declaration} 的 {match.group(1)} 里出现非引号元素：取数口径需要先修，{items}"
        rosters[match.group(1)] = [item.strip('"') for item in items]
    return rosters


def js_table_specs(source: str) -> dict[str, tuple[str, list[str]]]:
    """`TABLE_SPECS` → `{表 id: (名册名, [列键名])}`。"""
    body = _js_object_literal(source, "const TABLE_SPECS =")
    specs: dict[str, tuple[str, list[str]]] = {}
    for match in re.finditer(
        r'"([a-z-]+)":\s*\{\s*roster:\s*"([A-Za-z0-9_]+)",\s*columns:\s*\[([^\]]*)\]\s*\}', body
    ):
        columns = [item.strip().strip('"') for item in match.group(3).split(",") if item.strip()]
        specs[match.group(1)] = (match.group(2), columns)
    return specs


def rust_struct_field_names(source: str, struct_name: str) -> list[str]:
    """serde 直接派生的结构体字段名。`rename` 一出现就当场炸——那意味着这份取数不再等于线格式。"""
    pattern = re.compile(rf"pub struct {re.escape(struct_name)}(?:<[^>{{]*>)?\s*\{{")
    hits = list(pattern.finditer(source))
    assert len(hits) == 1, f"{struct_name} 的声明命中 {len(hits)} 处（期望恰好 1）：取数口径不唯一"
    brace = hits[0].end() - 1
    body = _js_object_literal(source[brace:], "{")
    assert "rename" not in body, f"{struct_name} 里出现 #[serde(rename…)]，字段名不再等于结构体字段名"
    return [match.group(1) for match in re.finditer(r"\bpub\s+([a-z0-9_]+)\s*:", body)]


def rust_enum_variant_names(source: str, enum_name: str) -> list[str]:
    """无数据变体的 `pub enum` 变体名清单。出现带数据的变体（`Variant(…)` / `Variant{…}`）就当场炸——
    那种形状页面用 `<select>` 表达不出来，要先改这份取数口径而不是让它静默漏掉一格。"""
    hits = list(re.finditer(rf"pub enum {re.escape(enum_name)}\s*\{{", source))
    assert len(hits) == 1, f"{enum_name} 的声明命中 {len(hits)} 处（期望恰好 1）：取数口径不唯一"
    body = _js_object_literal(source[hits[0].end() - 1 :], "{").strip().strip("{}")
    names: list[str] = []
    for raw in body.split(","):
        item = raw.strip()
        if not item:
            continue
        match = re.fullmatch(r"([A-Z][A-Za-z0-9]*)", item)
        assert match, f"{enum_name} 里出现非无数据变体的形状 {item!r}：取数口径要先修"
        names.append(match.group(1))
    assert names, f"{enum_name} 一个变体都没取到：取数口径失灵"
    return names


def html_select_option_values(html: str, select_id: str) -> list[str]:
    """`<select id="X">` 里的 `<option value="…">` 清单，按页面顺序。"""
    match = re.search(
        rf'<select id="{re.escape(select_id)}"[^>]*>(.*?)</select>', html, flags=re.S
    )
    assert match, f"index.html 里找不到 id 为 {select_id} 的 <select>"
    values = [item.group(1) for item in re.finditer(r'<option value="([^"]+)"', match.group(1))]
    assert values, f"{select_id} 的 <select> 里一个选项都没有"
    return values


def js_control_command_body_keys(app: str) -> list[str]:
    """`controlCommandFromForm` 里那份请求体字面量的键清单，按页面顺序（写面的名册）。"""
    hits = re.findall(r"function controlCommandFromForm\(\)", app)
    assert len(hits) == 1, f"controlCommandFromForm 的声明命中 {len(hits)} 处（期望恰好 1）"
    # 先取函数体那一层花括号，再在里面找对象字面量：`const command =` 在文件里有两处同形声明
    # （另一处是调用方 `const command = controlCommandFromForm();`），不封顶就会误判成不唯一。
    scope = _js_object_literal(app, "function controlCommandFromForm()")
    assert scope.count("const command =") == 1, "controlCommandFromForm 函数体里 `const command =` 不唯一"
    body = _js_object_literal(scope, "const command =")
    assert "?" not in body, "写面字面量里出现三元表达式：键名取数口径要先修（`a ? b : c` 会被当成键）"
    keys = [item.group(1) for item in re.finditer(r"([A-Za-z0-9_]+)\s*:", body)]
    assert keys, "写面字面量一个键都没取到：取数口径失灵"
    return keys


def _expand_format_literal(literal: str) -> str:
    """按 Rust 的 format! 规则把 `{{`/`}}` 还原成字面花括号，把 `{}` 占位换成 `\x00`。"""
    out: list[str] = []
    index = 0
    while index < len(literal):
        char = literal[index]
        if char == "{":
            if literal.startswith("{{", index):
                out.append("{")
                index += 2
                continue
            end = literal.index("}", index)
            out.append("\x00")
            index = end + 1
            continue
        if char == "}":
            paired = literal.startswith("}}", index)
            out.append("}")
            index += 2 if paired else 1
            continue
        out.append(char)
        index += 1
    return "".join(out)


def _json_keys_one_level(body: str) -> list[str]:
    """一个对象**体内**深度 0 处的键名：嵌套对象（`reconcile` 的三个子键）不会被算成顶层。"""
    keys: list[str] = []
    depth = 0
    index = 0
    while index < len(body):
        char = body[index]
        if char == '"':
            end = index + 1
            while end < len(body) and body[end] != '"':
                end += 2 if body[end] == "\\" else 1
            if end >= len(body):
                break
            if body[end + 1:].lstrip().startswith(":") and depth == 0:
                keys.append(body[index + 1:end])
            index = end + 1
            continue
        depth += char == "{"
        depth -= char == "}"
        index += 1
    return keys


def snapshot_to_json_layers() -> tuple[list[str], list[str]]:
    """`AccountSnapshot::to_json` 那一条 format! 字面量的顶层键与 `header` 层的键。"""
    rel, marker = CONSOLE_SNAPSHOT_SOURCE
    source = (ROOT / rel).read_text(encoding="utf-8")
    hit = source.find(marker)
    assert hit >= 0, f"{rel} 里找不到写侧字面量的锚点 {marker}"
    line = source[source.rfind("\n", 0, hit) + 1:source.find("\n", hit)]
    literal = line.strip().strip(",").strip('"')
    shape = _expand_format_literal(literal.replace('\\"', '"'))
    obj = _js_object_literal(shape, "{")
    assert obj.startswith("{") and obj.endswith("}"), f"写侧字面量不是一个平衡对象：{shape[:80]}"
    body = obj[1:-1]
    top = _json_keys_one_level(body)
    header_at = body.find('"header"')
    assert header_at >= 0, "写侧字面量里没有 header 那一层：快照的身份被挪出层了"
    open_brace = body.index("{", header_at)
    depth = 0
    for index in range(open_brace, len(body)):
        depth += body[index] == "{"
        depth -= body[index] == "}"
        if depth == 0:
            header = _json_keys_one_level(body[open_brace + 1:index])
            break
    else:
        raise AssertionError("header 那层的花括号不平衡")
    return top, header


def json_object_keys_of(marker: str) -> list[str]:
    """`qx-api` 某个臂里 `json!({...})` 的键清单（余额那一格的写侧事实源）。

    取数从 `json!(` 之后的那层对象开始，不是从臂体开始：臂体里 `match { … }` 的花括号
    会把 `json!` 那一层推到深度 2，按臂体取就一条键都数不出来。
    """
    api = production_text((ROOT / "crates/qx-api/src/lib.rs").read_text(encoding="utf-8"))
    body = _fn_body(api, marker)
    assert body, f"取不到 {marker} 的臂体"
    start = body.find("json!(")
    assert start >= 0, f"{marker} 的臂体里没有 json! 响应体：余额那一格的写侧口径变了"
    obj = _js_object_literal(body[start:], "{")
    return _json_keys_one_level(obj[1:-1])


def ws_frame_types() -> list[str]:
    """`ws.rs` 真的发射出去的那几种帧：字面量 `\\"type\\":\\"…` 逐个取。"""
    ws = production_text((ROOT / "crates/qx-api/src/ws.rs").read_text(encoding="utf-8"))
    return sorted(set(re.findall(r'\\"type\\":\\"([a-z_]+)\\"', ws)))


def rust_str_const(rel: str, name: str) -> str:
    source = (ROOT / rel).read_text(encoding="utf-8")
    match = re.search(rf'pub const {name}: &str = "([^"]*)"', source)
    assert match, f"{rel} 里取不到常量 {name}"
    return match.group(1)


def html_table_header_counts(html: str) -> dict[str, int]:
    """`<table id="x">` → 该表表头行的 `<th>` 个数（只取到第一个 `</tr>`）。"""
    out: dict[str, int] = {}
    for match in re.finditer(r'<table id="([a-z-]+)">.*?</tr>', html, flags=re.S):
        out[match.group(1)] = match.group(0).count("<th>")
    return out


def numeric_scale_decimals() -> int:
    """`qx-core::numeric::SCALE` 是几个 10 次幂——页面还原定点标度必须按它读。"""
    source = (ROOT / "crates/qx-core/src/numeric.rs").read_text(encoding="utf-8")
    match = re.search(r"pub const SCALE: i128 = ([0-9_]+);", source)
    assert match, "取不到 SCALE 常量：定点标度没有事实源了"
    value = int(match.group(1).replace("_", ""))
    exponent = 0
    while value % 10 == 0 and value > 1:
        value //= 10
        exponent += 1
    assert value == 1, f"SCALE 不是 10 的整数次幂（{match.group(1)}），次幂口径不成立"
    return exponent


def web_console_field_wiring_check() -> None:
    """控制台读的每个键名都必须是后端真的发出来的那一组，四个方向逐一对齐（V13 R25）。"""
    console = ROOT / WEB_CONSOLE_DIR
    if not (console / "app.js").is_file() or not (console / "index.html").is_file():
        check(False, "控制台字段级接线判据可读（app.js 与 index.html 都在盘）", "前端两件事先补")
        return
    app = (console / "app.js").read_text(encoding="utf-8")
    html = (console / "index.html").read_text(encoding="utf-8")
    code = re.sub(r"//[^\n]*", "", app)
    code = re.sub(r"/\*.*?\*/", "", code, flags=re.S)

    rosters = js_string_arrays(app, "const RESPONSE_FIELDS =")
    check(
        set(rosters) == set(CONSOLE_SERDE_SOURCES) | set(CONSOLE_SNAPSHOT_ROSTERS) | {"balances"},
        "名册的条目集合就是承诺的七个形状（少一格就少一份接线核对）",
        f"读到 {sorted(rosters)}",
    )
    specs = js_table_specs(app)
    check(
        len(specs) == 3 and all(table in html for table in specs),
        "三张表都有列定义，且都在 index.html 里真的存在",
        f"列定义 {sorted(specs)}",
    )

    # 方向一：名册 ⇔ 后端线格式，逐集合双向相等。
    for roster, (rel, struct) in CONSOLE_SERDE_SOURCES.items():
        truth = rust_struct_field_names((ROOT / rel).read_text(encoding="utf-8"), struct)
        check(
            rosters.get(roster) == truth,
            f"{roster} ⇔ {rel} 的 {struct} 字段名逐个相等（双向）",
            f"名册 {rosters.get(roster)} / 后端 {truth}",
        )
    top, header = snapshot_to_json_layers()
    check(
        rosters.get("snapshotTop") == top,
        "snapshotTop ⇔ AccountSnapshot::to_json 写侧字面量的顶层键逐个相等（双向）",
        f"名册 {rosters.get('snapshotTop')} / 写侧 {top}",
    )
    check(
        rosters.get("snapshotHeader") == header,
        "snapshotHeader ⇔ 同一条写侧字面量里 header 那层的键逐个相等（身份在 header，不在顶层）",
        f"名册 {rosters.get('snapshotHeader')} / 写侧 {header}",
    )
    check(
        rosters.get("balances") == json_object_keys_of(CONSOLE_BALANCES_MARKER),
        "balances ⇔ /account/balances 的 json! 响应体键集合相等",
        f"名册 {rosters.get('balances')} / 后端 {json_object_keys_of(CONSOLE_BALANCES_MARKER)}",
    )
    schema = json.loads(
        (ROOT / "schemas/account-snapshot-v1.json").read_text(encoding="utf-8")
    )
    check(
        sorted(top) == sorted(schema["properties"]),
        "快照写侧字面量的顶层键与 schemas/account-snapshot-v1.json 的 properties 相等（三份说法只有一份漂移就红）",
        f"写侧 {sorted(top)} / schema {sorted(schema['properties'])}",
    )

    # 方向二：列 ⇔ 名册 ⇔ 表头。三处任何一处多一列少一列都会红——幻影列就是这么立案的。
    headers = html_table_header_counts(html)
    for table, (roster, columns) in sorted(specs.items()):
        check(
            roster in rosters and set(columns) <= set(rosters.get(roster, [])),
            f"{table} 的每一列都在 {roster} 名册里（页面不得读名册外的键）",
            f"列 {columns} / 名册 {rosters.get(roster)}",
        )
        check(
            headers.get(table) == len(columns),
            f"{table} 的表头 <th> 数（{headers.get(table)}）与列定义数（{len(columns)}）相等",
            "表头与取数列不一致：有一列永久读不出东西，或有一列没有表头",
        )

    # 方向三：每一格名册都得有运行时读者，否则它就是一张没人核对的清单（孤儿面）。
    readers = []
    for roster in sorted(rosters):
        used = (
            f'missingFields("{roster}"' in code
            or any(spec_roster == roster for spec_roster, _ in specs.values())
        )
        if not used:
            readers.append(roster)
    check(
        not readers,
        "每一格名册都被运行时真的读一次（fillTable/renderEvents 的列核对，或 missingFields 点名）",
        f"没有读者的名册 {readers}",
    )
    # 方向四（反向）：代码里出现的每一个 `*_raw` 键名都必须落在名册里。
    read_raw = set(re.findall(r"\b[a-z][a-z0-9_]*_raw\b", code))
    promised = {name for names in rosters.values() for name in names}
    check(
        read_raw <= promised,
        "app.js 里读到的每个 *_raw 键名都在名册里（凭空开一列的那类故障反向也堵）",
        f"名册外出现的键 {sorted(read_raw - promised)}",
    )

    # 方向五（写面）：`POST /control/commands` 的请求体直接反序列化成 `ControlCommand`，页面那份
    # 字面量的键清单与两个 `<select>` 的选项清单就是前端能表达的全部——后端加一档而页面没有那一格，
    # 那条命令永远发不出去；页面列出后端不认的一档，用户只会拿到 400。两侧都必须逐格相等。
    control_rel, control_struct = CONSOLE_CONTROL_SOURCE
    control_src = (ROOT / control_rel).read_text(encoding="utf-8")
    body_keys = js_control_command_body_keys(app)
    wire_fields = rust_struct_field_names(control_src, control_struct)
    check(
        body_keys == wire_fields,
        f"写面请求体的键清单 ⇔ {control_rel} 的 {control_struct} 字段名逐个相等（双向）",
        f"页面 {body_keys} / 后端 {wire_fields}",
    )
    for select_id, enum_name in sorted(CONSOLE_CONTROL_ENUMS.items()):
        options = html_select_option_values(html, select_id)
        variants = rust_enum_variant_names(control_src, enum_name)
        check(
            options == variants,
            f"{select_id} 的选项清单 ⇔ {enum_name} 的变体逐个相等（双向）",
            f"页面 {options} / 后端 {variants}",
        )

    # 定点标度：页面的还原位数必须就是 SCALE 的次幂，且只许定义一次。
    decimals = re.findall(r"const RAW_DECIMALS = (\d+);", app)
    scale = numeric_scale_decimals()
    check(
        len(decimals) == 1 and decimals[0] == str(scale),
        f"控制台只有一处定点标度定义，且位数等于 numeric.rs 的 SCALE（1e{scale}）",
        f"页面定义 {decimals} / SCALE 次幂 {scale}",
    )
    check(
        code.count("RAW_BASE") >= 2 and "10n ** BigInt(RAW_DECIMALS)" in code,
        "标度换算只有一个出口（除数由 RAW_DECIMALS 推出，别处不得再除一次）",
        "出现第二处手抄折算，1e9 与 1e8 就能在页面上并存",
    )

    # 时间标度：这条轴是 epoch 毫秒。上一轮页面按 `clock.rs` 那句「纳秒」注释除了 1e6，
    # 于是 2026 年的 as_of 全渲染成 1970/1/1——文档口径与运行时唯一的墙钟不一致，读侧就
    # 差三个数量级。判据把两头钉在一起：`Ts` 的文档必须点名那个墙钟，墙钟必须真是毫秒。
    clock_doc = (ROOT / "crates/qx-core/src/clock.rs").read_text(encoding="utf-8")
    ts_doc = re.search(r"((?:^///.*\n)+)pub type Ts = u64;", clock_doc, flags=re.M)
    wiring = production_text(
        (ROOT / "crates/qx-cli/src/runtime_wiring.rs").read_text(encoding="utf-8")
    )
    check(
        ts_doc is not None
        and "毫秒" in ts_doc.group(1).splitlines()[0]
        and "runtime_timestamp_ms" in ts_doc.group(1)
        and "fn runtime_timestamp_ms() -> u64" in wiring
        and "duration.as_millis() as u64" in wiring,
        "时间轴标度单源一致：`Ts` 的文档口径就是运行时唯一墙钟的口径（epoch 毫秒，纳秒只在延迟配置里）",
        "墙钟换了单位而 `Ts` 的注释没跟着改（或反过来）：下游按哪一头读都可能差三个数量级",
    )
    check(
        "new Date(Number(raw))" in code
        and "1000000n" not in code
        and 'const TS_COLUMNS = ["ts", "as_of", "receive_time", "engine_time"];' in code,
        "控制台把这一列当毫秒渲染（epoch 毫秒直接进 Date，不再除 1e6），四个时间列走同一个出口",
        "页面重新把毫秒当成纳秒：快照 as_of 与事件 ts 会整体退回到 1970 年",
    )

    # 游标口径。`after` 在服务端只有一种解释：**已经看到的那一条的序号**，回的是 `seq > after`
    # 那一段（`event_cursor.rs`）。页面里写成 `seq + 1` 就是把「下一个要取的」当成「已看到的」——
    # 只要追平日志就恒有 `after >= next_seq`，下一轮必然 409，然后每次 409 都要多跑一趟
    # envelope 才能自愈；而把「还没有基线」写成 0，在空日志上同样是 409（空日志的 next_seq 也
    # 是 0，事件序号却真是从 0 起的）。所以这里钉三件事：写入形态只有白名单里那几种、
    # 没有基线时**不带** `after`、后端那一侧的口径与「序号从 0 起」这两个前提本身没被人换掉。
    cursor_writes = [
        line.strip() for line in code.splitlines() if re.search(r"state\.cursor\s*=(?!=)", line)
    ]
    legal_cursor_writes = {
        "state.cursor = null;",
        "state.cursor = seq;",
        "state.cursor = state.cursor === null ? seq : Math.max(state.cursor, seq);",
    }
    check(
        bool(cursor_writes)
        and not [line for line in cursor_writes if line not in legal_cursor_writes]
        and "state.cursor = state.cursor === null ? seq : Math.max(state.cursor, seq);" in code
        and code.count("noteCursor(seq);") >= 3
        and "state.cursor === null ? API_PATHS.events : " in code,
        "游标推进只有一个出口，写入形态全在白名单里（没有 `seq + 1` 那一类），没有基线时不带 `after`",
        f"越界的游标写入 {[line for line in cursor_writes if line not in legal_cursor_writes] or '无'}；"
        f"出口调用 {code.count('noteCursor(seq);')} 处",
    )
    cursor_backend = production_text(
        (ROOT / "crates/qx-api/src/event_cursor.rs").read_text(encoding="utf-8")
    )
    open_faces = (ROOT / "crates/qx-runtime/tests/event_log_open_faces.rs").read_text(
        encoding="utf-8"
    )
    check(
        "if after >= next_seq {" in cursor_backend
        and "events.filter(|event| event.seq > after)" in cursor_backend
        and "let Some(after) = after else" in cursor_backend
        and "assert_eq!(pipeline.log().events()[0].seq, 0)" in open_faces,
        "后端游标语义仍是「回 seq > after，缺省从头给」，且事件序号从 0 起（页面那一侧的口径以这两条为前提）",
        "后端换了游标口径或序号起点，页面的单一出口与「null 表示没有基线」就同时失真",
    )

    # 名册的格与路径都得有人读：`API_PATHS` 里留一条没人取的路径，就是装饰性清单一格；
    # 通道名在第二处重抄字面量，改路由时就会只改一处。
    api_roster = re.search(r"const API_PATHS = \{(.*?)\n\};", app, re.S)
    roster_keys = (
        re.findall(r"^\s*([A-Za-z][A-Za-z0-9]*):", api_roster.group(1), re.M) if api_roster else []
    )
    unread = [key for key in roster_keys if f"API_PATHS.{key}" not in code]
    check(
        len(roster_keys) >= 10
        and not unread
        and "const WS_PATH = API_PATHS.eventsLive;" in code
        and app.count('"/events/live"') == 1,
        f"API_PATHS 的 {len(roster_keys)} 条路径都有取数读者，WebSocket 通道名取自名册而不是第二份字面量",
        f"没人取的路径 {unread}",
    )

    # WS 帧词表：页面按这份词表分派，未在册的帧登记而不是静默丢。
    frames = ws_frame_types()
    match = re.search(r"const WS_FRAME_KINDS = \[([^\]]*)\]", app)
    kinds = (
        [item.strip().strip('"') for item in match.group(1).split(",") if item.strip()]
        if match
        else []
    )
    check(
        sorted(kinds) == frames,
        f"WS_FRAME_KINDS 与 ws.rs 真的发射的那 {len(frames)} 种帧名逐个相等（双向）",
        f"页面 {sorted(kinds)} / 后端 {frames}",
    )
    undropped = [frame for frame in frames if f'type === "{frame}"' not in code]
    check(
        not undropped,
        "后端发的每一种帧在 handleFrame 里都有分派支（落空就是一种：帧被记成未知而实时面照印已连接）",
        f"没有分派支的帧 {undropped}",
    )

    # 写面凭据接线：双提交的两个名字两侧必须逐字符相等。
    cookie = rust_str_const("crates/qx-api/src/console.rs", "CONSOLE_CSRF_COOKIE")
    header_name = rust_str_const("crates/qx-api/src/console.rs", "CONSOLE_CSRF_HEADER")
    page_cookie = re.search(r'const CSRF_COOKIE_NAME = "([^"]+)"', app)
    page_header = re.search(r'const CSRF_HEADER_NAME = "([^"]+)"', app)
    check(
        bool(page_cookie) and bool(page_header)
        and page_cookie.group(1) == cookie
        and page_header.group(1).lower() == header_name,
        f"控制台的 CSRF cookie/头名取自 console.rs 常量（{cookie} / {header_name}）",
        f"页面 {page_cookie and page_cookie.group(1)} / {page_header and page_header.group(1)}",
    )
    check(
        "headers[CSRF_HEADER_NAME] = csrf" in code and "csrfToken()" in code,
        "写面真的把 CSRF 取出来放回请求头（不读它，同源 BFF 的写面就是一条永久 403 的死路）",
        "双提交在页面上没有实现",
    )

    # 同源 BFF 的实时面：那一层不转发 WebSocket 升级，页面与文档都得如实说。
    console_rs = production_text((ROOT / "crates/qx-api/src/console.rs").read_text(encoding="utf-8"))
    bff_forwards_ws = any(token in console_rs.lower() for token in ("upgrade", "websocket"))
    check(
        not bff_forwards_ws,
        "同源 BFF 确实不代理 WebSocket 升级（这条判据一变红，页面与文档的那句「不适用」就成谎话）",
        "console.rs 出现了升级转发：控制台侧的降级说明要连文档一起改",
    )
    check(
        "不转发 WebSocket" in html and "state.viaBff" in code and "wsStatusText()" in code,
        "BFF 形态下实时面退化为轮询，页面与说明文字都说清（徽标不得报「WS 已连接」）",
        "降级路径不再如实交代",
    )


# P0-3 / DD-4：in-process 原生策略的信任门。原生库 `dlopen` 进宿主后拥有整个地址空间，
# 所以「默认只允许独立进程、in-process 要显式信任门 + 签名信任根 + 架构匹配」这条裁定
# 必须有会红的判据守着——否则下一次有人把 `admit_in_process_c_abi` 那一行删掉，门禁不会响。
NATIVE_TRUST_FILE = "crates/qx-cli/src/native_trust.rs"
NATIVE_TRUST_GATE = "admit_in_process_c_abi"


def native_trust_check() -> None:
    """in-process C ABI 的信任门在位、且真的挡在 `dlopen` 之前（V13 R23 · P0-3 / DD-4）。"""
    path = ROOT / NATIVE_TRUST_FILE
    check(
        path.is_file(),
        "in-process 原生策略的信任门模块在盘（qx-cli/src/native_trust.rs）",
        f"缺失 {NATIVE_TRUST_FILE}",
    )
    if not path.is_file():
        return
    trust = production_text(path.read_text(encoding="utf-8"))
    host = (ROOT / "crates/qx-cli/src/strategy_host.rs").read_text(encoding="utf-8")
    check(
        f"fn {NATIVE_TRUST_GATE}(" in trust and "fn target_triple_matches(" in trust,
        "信任门提供「是否放行」与「三元组匹配」两个判定，且都不是测试专用",
        "信任门被掏空成空实现或只留测试读者",
    )
    check(
        f"native_trust::{NATIVE_TRUST_GATE}(strategy)?" in host,
        "`load_c_abi_strategy` 在 `dlopen` 之前先过信任门（挡在加载之前，而不是加载之后）",
        "加载点不再调用信任门——in-process 原生库又可以不签名进宿主了",
    )
    schema = (ROOT / "crates/qx-runtime/src/runtime_config/strategy_schema.rs").read_text(
        encoding="utf-8"
    )
    check(
        "c_abi_trusted_native: bool" in schema and "c_abi_target_triple: Option<String>" in schema,
        "配置面暴露 `c_abi_trusted_native`（缺省 false）与 `c_abi_target_triple` 两格",
        "信任门开关或目标三元组字段被删",
    )
    check(
        "#[serde(default)]\n    pub c_abi_trusted_native: bool" in schema,
        "信任门开关缺省为 false（默认拒绝，不是默认放行）",
        "把 default 改成 true 等于默认放行未签名原生库",
    )


# M4'/M5'：同源 BFF 控制台（`qx-api/src/console.rs`）。`web/console/` 单独部署时与 API 不同源，
# 会话 / CSRF / 身份三件事都只能靠"只允许本机回环"这种网络位置来挡；这一层把它们搬到服务端。
# 下面这些格要么是安全边界（cookie 属性、CSRF 头、回环、令牌来源），要么是
# 「前端 ⇔ 后端 ⇔ 部署模板」的三方接线——任一处被删掉，浏览器里看起来都还正常，所以必须有会红的判据。
CONSOLE_FRONT_FILE = "crates/qx-api/src/console.rs"
CONSOLE_SERVE_FILE = "crates/qx-cli/src/console_serve.rs"
CONSOLE_SCHEMA_FILE = "crates/qx-runtime/src/runtime_config/schema.rs"
CONSOLE_VALIDATION_FILE = "crates/qx-runtime/src/runtime_config/console_validation.rs"
CONSOLE_TOPOLOGY_FILE = "crates/qx-runtime/src/runtime_config/topology_validation.rs"
CONSOLE_TEMPLATE_FILE = "deploy/qianxing.runtime.console.example.json"
CONSOLE_CASES_FILE = "crates/qx-runtime/src/runtime_config/topology_tests.rs"
# 会话/CSRF 的名字与缺省 TTL 是「前端 ⇔ 后端 ⇔ 部署」共同引用的事实，逐个钉住。
CONSOLE_PUBLIC_CONSTS = (
    "CONSOLE_SESSION_COOKIE",
    "CONSOLE_CSRF_COOKIE",
    "CONSOLE_CSRF_HEADER",
    "CONSOLE_TOKEN_QUERY",
    "DEFAULT_CONSOLE_SESSION_TTL_SECONDS",
    "CONSOLE_ASSETS",
)
CONSOLE_ASSET_NAMES = ("index.html", "app.js", "styles.css")
# `ConsoleConfig::new` 的四类拒绝：这一层最贵的错是"配错了也起得来"。
CONSOLE_CONFIG_REJECTIONS = (
    "console.operator 不能为空",
    "引导令牌太短",
    "session_ttl_seconds 不能为 0",
    "static_dir 不是目录",
)
CONSOLE_SOCKET_CASE = "one_accepted_connection_is_answered_end_to_end_over_a_real_socket"
CONSOLE_BOUNDARY_CASE = "console_surface_refuses_routable_binds_and_unknown_operators"


def _console_loopback_host(bind: str) -> bool:
    """部署模板里的 bind 是不是回环：与 `console_bind_is_loopback` 同口径的最小判定。"""
    host = bind.rsplit(":", 1)[0].strip("[]")
    return host in ("::1", "localhost") or host.startswith("127.")


def console_front_check() -> None:
    """同源 BFF 控制台（V13 R23 · M4'/M5'）：会话 / CSRF / 身份 / 回环 / 令牌来源逐格钉住。"""
    path = ROOT / CONSOLE_FRONT_FILE
    check(
        path.is_file(),
        "同源 BFF 控制台模块在盘（qx-api/src/console.rs）",
        f"缺失 {CONSOLE_FRONT_FILE}",
    )
    if not path.is_file():
        return
    raw_front = path.read_text(encoding="utf-8")
    front = production_text(raw_front)
    serve = production_text((ROOT / CONSOLE_SERVE_FILE).read_text(encoding="utf-8"))
    lib = production_text((ROOT / "crates/qx-api/src/lib.rs").read_text(encoding="utf-8"))
    check(
        "mod console;" in lib and "pub use console::*;" in lib,
        "BFF 模块挂进 lib.rs 并重导出（mod console; + pub use console::*;）",
        "模块在盘却没挂上：门禁与调用方都看不见它",
    )
    missing = [name for name in CONSOLE_PUBLIC_CONSTS if f"pub const {name}" not in front]
    check(
        not missing and all(f'"{asset}"' in front for asset in CONSOLE_ASSET_NAMES),
        "会话/CSRF/令牌查询参数与缺省 TTL 六个公开常量齐备，静态资源表只认那三份",
        f"缺常量 {missing}",
    )
    check(
        "pub fn new(" in front and all(item in front for item in CONSOLE_CONFIG_REJECTIONS),
        "`ConsoleConfig::new` 是唯一构造入口，且四类误配当场拒绝（空身份 / 令牌太短 / TTL 为 0 / 静态目录不存在）",
        f"缺拒绝 {[item for item in CONSOLE_CONFIG_REJECTIONS if item not in front]}",
    )
    # cookie 属性按"带 Path=/ 的那一行"取，避免与文件里别处的说明文字混淆。
    session_line = next(
        (
            line
            for line in front.splitlines()
            if "Path=/" in line and "CONSOLE_SESSION_COOKIE" in line
        ),
        "",
    )
    csrf_line = next(
        (
            line
            for line in front.splitlines()
            if "Path=/" in line and "CONSOLE_CSRF_COOKIE" in line
        ),
        "",
    )
    check(
        "HttpOnly" in session_line and "SameSite=Strict" in session_line,
        "会话 cookie 硬化：HttpOnly（脚本偷不走会话）+ SameSite=Strict（跨站不发）",
        f"会话 cookie 属性行 {session_line!r}",
    )
    check(
        "SameSite=Strict" in csrf_line and "HttpOnly" not in csrf_line,
        "CSRF cookie 刻意不带 HttpOnly（页面要读出来放进 X-QX-CSRF 头，双提交模式）但仍 SameSite=Strict",
        f"CSRF cookie 属性行 {csrf_line!r}",
    )
    check(
        ".get(CONSOLE_CSRF_HEADER)" in front
        and "!presented.is_empty() && presented == session.csrf" in front
        and "self.csrf_matches(&parsed, &session)" in front,
        "非 GET 必须带 X-QX-CSRF 头，且与会话记住的那一枚逐字符相等（空头与不等都 403）",
        "CSRF 双提交校验被删，或被放宽成「有头就过」",
    )
    check(
        "fn origin_matches_host(" in front and "origin_matches_host(&parsed)" in front,
        "非 GET 还要过同源校验：带 Origin 的请求必须与 Host 同源（与 CSRF 头是两道独立的锁）",
        "同源校验被删：跨站诱导的写请求只剩一道锁",
    )
    check(
        "handle_inner(" in front
        and "Some(&session.operator)" in front
        and "operator_id" not in front,
        "身份由服务端按会话注入 `handle_inner`（operator 取自会话，不读命令体里可随便填的 operator_id）",
        "身份注入被摘掉，或改回读请求体自声明的 operator",
    )
    # 注入之后还得真的**用上**。V13 R25 端到端实测抓到的形状是：那句覆盖住在 `match &self.policy` 的
    # `Some(policy)` 臂里，于是 `api.operators: {}`（控制台模板与 `--init` 的出厂形状）这条路上永远
    # 走不到它——审计里落的是页面自称的那个 operator_id，而页面与文档都写着它不读这个字段。
    submit_body = _fn_body(
        production_text((ROOT / API_ROOT_FILE).read_text(encoding="utf-8")), "fn submit_command("
    )
    check(
        submit_body.count("command.operator_id =") == 1
        and "if let Some(operator_id) = authenticated {" in submit_body
        and submit_body.find("command.operator_id =") < submit_body.find("let granted ="),
        "写面的身份覆盖只有一处，且排在权限裁决之前（不挂在「有没有配 operator 名册」那一臂里）",
        "身份覆盖搬回 policy 臂：没配 operators 名册的部署里，客户端就能把每条命令记在别人名下",
    )
    identity_case = (ROOT / "crates/qx-api/tests/console_operator_identity.rs").read_text(
        encoding="utf-8"
    )
    check(
        "ApiService::new(" in identity_case
        and "with_policy" not in identity_case
        and "Some(OPERATOR)" in identity_case,
        "无名册那一侧有常驻反例在盘（走 ConsoleFront 公开面写一条命令，再从 /control/audit 回读身份）",
        "用例改成配了名册的形状就钉不住这一格——缺陷恰好藏在只配了名册才执行的那一臂里",
    )
    check(
        "fn console_bind_is_loopback(" in front
        and "address.ip().is_loopback()" in front
        and "qx_api::console_bind_is_loopback(&console.bind)" in serve,
        "回环边界单源：判据只在 console.rs 一处定义，`qx-cli console` 绑监听之前先过它",
        "回环判据被绕开或复制出第二份，控制台可以绑到外部接口",
    )
    schema = production_text((ROOT / CONSOLE_SCHEMA_FILE).read_text(encoding="utf-8"))
    check(
        "std::env::var(&console.bootstrap_token_env)" in serve
        and "pub bootstrap_token_env: String" in schema
        and "pub bootstrap_token:" not in schema,
        "引导令牌只从 `bootstrap_token_env` 点名的环境变量读（配置里只有变量名，没有令牌字面量字段）",
        "令牌来源被放宽成命令行/配置文件，秘密会进 shell 历史与版本库",
    )
    topology = production_text((ROOT / CONSOLE_TOPOLOGY_FILE).read_text(encoding="utf-8"))
    check(
        "pub struct ConsoleRuntimeConfig {" in schema
        and "pub console: Option<ConsoleRuntimeConfig>," in schema
        and (ROOT / CONSOLE_VALIDATION_FILE).is_file()
        and "console_boundary(console, &self.api.operators)?" in topology,
        "`api.console` 段被 schema 声明、边界校验独立成模块，且在拓扑校验里 fail-closed 被调用",
        "配置段加了却不校验：绑到外部接口的控制台照样起得来",
    )
    cli = (ROOT / "crates/qx-cli/src/cli.rs").read_text(encoding="utf-8")
    check(
        "Command::Console(args)" in cli and "serve_console(&args)" in cli,
        "`qx-cli console` 有派发臂且指向 `serve_console`（命令表里的入口不是装饰）",
        "命令表列了 console 却没有派发分支",
    )
    template_path = ROOT / CONSOLE_TEMPLATE_FILE
    check(
        template_path.is_file(),
        "控制台部署模板在盘（deploy/qianxing.runtime.console.example.json）",
        f"缺失 {CONSOLE_TEMPLATE_FILE}",
    )
    if template_path.is_file():
        raw = template_path.read_text(encoding="utf-8")
        console = json.loads(raw).get("api", {}).get("console", {})
        env_name = str(console.get("bootstrap_token_env", ""))
        check(
            _console_loopback_host(str(console.get("bind", "")))
            and re.fullmatch(r"[A-Z][A-Z0-9_]*", env_name) is not None
            and '"bootstrap_token"' not in raw
            and bool(console.get("static_dir"))
            and bool(console.get("operator")),
            "模板绑回环地址、令牌只给环境变量名（大写+下划线）、正文无令牌字面量字段，且写明 static_dir 与 operator",
            f"bind={console.get('bind')!r} env={env_name!r}",
        )
    check(
        "#[cfg(test)]" in raw_front
        and CONSOLE_SOCKET_CASE in raw_front
        and CONSOLE_BOUNDARY_CASE in case_source(CONSOLE_CASES_FILE),
        "行为用例在盘：真套接字上端到端一条 + 回环/身份边界逐格一条",
        "会话/CSRF/回环的判定只剩散文，没有会跑红的用例",
    )
    # 发布身份里的「产品现在有什么」必须由在盘代码背书：声明 True 就要真有那个实现，
    # 实现落了却没翻声明也当场红（两边都不能各说各话）。桌面 Host 的落点按约定登记。
    packager = (ROOT / "tools/package_web_console.py").read_text(encoding="utf-8")
    backed_by = {
        "product_same_origin_bff": CONSOLE_FRONT_FILE,
        "product_csrf": CONSOLE_FRONT_FILE,
        "product_server_side_session": CONSOLE_FRONT_FILE,
        "product_desktop_host": "crates/qx-cli/src/desktop_host.rs",
    }
    mismatched = []
    for field, backing in backed_by.items():
        declared = re.search(rf'"{field}": (True|False)', packager)
        if declared is None:
            mismatched.append(f"{field} 未登记")
            continue
        if (declared.group(1) == "True") != (ROOT / backing).is_file():
            mismatched.append(f"{field}={declared.group(1)} 与 {backing} 在盘不符")
    check(
        not mismatched,
        "发布身份的产品能力声明由在盘代码背书（声称有 BFF/CSRF/会话就必须真有 console.rs）",
        f"{mismatched}",
    )


# V13 R24 · 控制台易用性三件：令牌缺失时临时生成（环境变量仍优先）、`--init` 脚手架、`--generate-token`。
# 这三样都是"改错了也不红"的形状——删掉兜底只会让忘了 export 的人启不来（不是报错），
# 删掉 `--init` 的覆盖保护会静默改写别人的配置，模板改歪会让脚手架产出一份绑外部接口的配置。
CONSOLE_ARGS_FILE = "crates/qx-cli/src/console_args.rs"
CONSOLE_TOKEN_FALLBACK_MARKER = "已临时生成一次性引导令牌"
CONSOLE_SCAFFOLD_REFUSAL = "已存在，拒绝覆盖"
CONSOLE_TOKEN_TEST = "generated_bootstrap_token_is_long_hex_and_varies"


def console_usability_check() -> None:
    """控制台易用性（V13 R24）：令牌兜底 / `--init` 脚手架 / `--generate-token` 逐格钉住。"""
    raw_serve = (ROOT / CONSOLE_SERVE_FILE).read_text(encoding="utf-8")
    serve = production_text(raw_serve)
    args = (ROOT / CONSOLE_ARGS_FILE).read_text(encoding="utf-8")
    check(
        "fn generate_bootstrap_token(" in serve
        and "std::env::var(&console.bootstrap_token_env)" in serve
        and CONSOLE_TOKEN_FALLBACK_MARKER in serve,
        "引导令牌兜底：环境变量仍是首选来源，缺失时临时生成一枚并当场打印它是临时的",
        "令牌兜底被删（忘了 export 就启不来），或环境变量优先级被绕过",
    )
    check(
        "#[cfg(test)]" in raw_serve and CONSOLE_TOKEN_TEST in raw_serve,
        "生成的令牌有行为用例在盘（长度下限 + 十六进制 + 两次不同）",
        "令牌生成只剩散文，没有会跑红的用例",
    )
    check(
        "pub(crate) generate_token: bool" in args and "args.generate_token" in serve,
        "`--generate-token` 旗标已声明且被 `serve_console` 读到（打印令牌后退出，不启动服务）",
        "旗标声明了却没人读，或被绑成 `_` 丢弃",
    )
    check(
        "pub(crate) init: Option<PathBuf>" in args
        and "args.init" in serve
        and CONSOLE_SCAFFOLD_REFUSAL in serve,
        "`--init` 旗标已声明且被读到，且对已存在文件拒绝覆盖（静默改写等于替别人签字）",
        "`--init` 覆盖保护被删，或旗标没人读",
    )
    template = re.search(
        r'CONSOLE_RUNTIME_TEMPLATE: &str = r#"(.*?)"#;', raw_serve, re.DOTALL
    )
    ok = False
    detail = "未找到 CONSOLE_RUNTIME_TEMPLATE 常量"
    if template is not None:
        raw = template.group(1)
        try:
            console = json.loads(raw).get("api", {}).get("console", {})
            env_name = str(console.get("bootstrap_token_env", ""))
            ok = (
                _console_loopback_host(str(console.get("bind", "")))
                and re.fullmatch(r"[A-Z][A-Z0-9_]*", env_name) is not None
                and '"bootstrap_token"' not in raw
                and bool(console.get("static_dir"))
                and bool(console.get("operator"))
            )
            detail = f"bind={console.get('bind')!r} env={env_name!r}"
        except json.JSONDecodeError as error:
            detail = f"模板不是合法 JSON: {error}"
    check(
        ok,
        "`--init` 写出的模板与部署模板同形：绑回环、令牌只给环境变量名、无令牌字面量字段、写明 static_dir 与 operator",
        detail,
    )


# === V13 R26 / §6.E1：门禁读数只有一处机读来源 ===
# 此前 463 / 515 / 610 / 851 / 895 这些读数全靠人抄当轮日志（README 里的地板甚至停在 843）：
# 抄错就长期失真，而没有任何判据看得见"文档说的条数 ≠ 门禁真跑的条数"。现在 `--snapshot`
# 把本轮实测条数、地板常量与 label 名册摘要写进 `maturity/gate_snapshot.json`，门禁收尾时拿
# 本轮实测与它逐值比对——加了判据却忘了刷新，当场红。文档只引用文件名，不再手抄数字。
GATE_SNAPSHOT_FILE = ROOT / "maturity" / "gate_snapshot.json"
GATE_SNAPSHOT_DOCS = ("README.md",)
GATE_SNAPSHOT_DOC_NAME = "maturity/gate_snapshot.json"
# 手抄读数的形状：`895 条 [PASS]` / `895 项 [PASS]`（含加粗）。文档里再出现它 = 数字又被写死。
GATE_SNAPSHOT_HAND_COPIED = re.compile(r"\d{3,4}\s*(?:条|项)\s*\*{0,2}\[?PASS")
GATE_SNAPSHOT_MODE = "--snapshot" in sys.argv


def gate_snapshot_payload(total: int) -> dict[str, object]:
    """本轮实测事实：条数、地板常量、label 名册摘要（不含时间戳——同判据重跑必须逐字节相等）。"""
    return {
        "gate": "tools/check_architecture.py",
        "checks": total,
        "gate_check_floor": GATE_CHECK_FLOOR,
        "labels_sha256": hashlib.sha256("\n".join(labels).encode("utf-8")).hexdigest(),
    }


def write_gate_snapshot(total: int) -> None:
    GATE_SNAPSHOT_FILE.write_text(
        json.dumps(gate_snapshot_payload(total), ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    print(
        f"已写入 {GATE_SNAPSHOT_FILE.relative_to(ROOT).as_posix()}（{total} 条判据）"
    )


def read_gate_snapshot() -> dict[str, object]:
    try:
        snapshot = json.loads(GATE_SNAPSHOT_FILE.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return snapshot if isinstance(snapshot, dict) else {}


def gate_snapshot_check() -> None:
    """门禁读数只有一处机读来源：`maturity/gate_snapshot.json`（由 `--snapshot` 写入）。

    条数必须与「快照缺失」这一分支无关：`--snapshot` 那一轮快照还不存在、其余轮存在，
    若两轮各发不同条数的判据，收尾那条「本轮实测 == 快照」永远不可能成立（生成轮记下
    N、复核轮算成 N+1）。故本函数**恒发三条**，缺失时由前两条各自按 `--snapshot` 放行。
    """
    snapshot = read_gate_snapshot()
    # `--snapshot` 那一轮盘上那份马上就会被收尾重写，拿它做判据只会挡住生成本身
    # （改了地板常量之后，旧快照必然与常量不等）。所以生成轮把它当"不在盘"处理，
    # 一致性由随后的普通轮核对。
    present = bool(snapshot) and not GATE_SNAPSHOT_MODE
    check(
        present or GATE_SNAPSHOT_MODE,
        "门禁读数快照在盘且可解析",
        f"缺失或不可解析 {GATE_SNAPSHOT_DOC_NAME}，跑 --snapshot 生成",
    )
    check(
        not present
        or (
            isinstance(snapshot.get("checks"), int)
            and isinstance(snapshot.get("gate_check_floor"), int)
            and snapshot["gate_check_floor"] == GATE_CHECK_FLOOR
            and snapshot["checks"] >= GATE_CHECK_FLOOR
        ),
        "快照里的条数与地板都是整数，地板与常量同一份（改地板常量必须重跑 --snapshot）",
        f"快照 {snapshot}",
    )
    hand_copied = {
        name: len(GATE_SNAPSHOT_HAND_COPIED.findall((ROOT / name).read_text(encoding="utf-8")))
        for name in GATE_SNAPSHOT_DOCS
    }
    hand_copied = {name: count for name, count in hand_copied.items() if count}
    missing_ref = [
        name
        for name in GATE_SNAPSHOT_DOCS
        if GATE_SNAPSHOT_DOC_NAME not in (ROOT / name).read_text(encoding="utf-8")
    ]
    check(
        hand_copied == {} and missing_ref == [],
        "文档引用机读快照文件名而不是手抄条数（抄错就长期失真的那一类）",
        f"仍手抄读数 {hand_copied} / 未引用快照 {missing_ref}",
    )


# P0-2(b)：EventLog 的保留/归档策略。本仓对"只增长且无法在本轮修掉"的形态一律
# 「保留 + 登记 + 钉住」，但策略本身必须是一处**写下来的决定**而不是沉默的缺口——
# 否则下一个人无从知道"没有保留"是判断还是遗漏，也不知道归档该按什么粒度做。
EVENT_LOG_SOURCE = "crates/qx-core/src/sourcing.rs"
# 策略正文住在写面的模块文档里（`sourcing.rs` 只留一行指针，那个文件顶格在行数棘轮上）。
EVENT_LOG_POLICY_DOC = "crates/qx-runtime/src/pipeline.rs"
EVENT_LOG_SEGMENT_SOURCE = "crates/qx-storage/src/lib.rs"
EVENT_LOG_REGISTRY = "maturity/capabilities.yaml"
# 册内保留的入口名册：EventLog 的公开面上一个都不许有（有 = 活日志能被改短）。
EVENT_LOG_EVICTION_VERBS = (
    "truncate",
    "retain",
    "drain",
    "split_off",
    "rotate",
    "compact",
    "purge",
    "prune",
)
# 策略的三条杠杆：册内保留被禁止 / 轮转靠换 log_name / 归档粒度是封段。
EVENT_LOG_POLICY_ANCHORS = ("册内保留被禁止", "轮转是运维杠杆", "归档粒度是封段")
# 登记面上与本策略绑定的两条 limitation 名。
EVENT_LOG_POLICY_REGISTRY_NAMES = (
    "event_log_has_no_retention_or_compaction_trigger",
    "event_log_steady_state_tick_cost_is_linear_in_log_length",
)


def event_log_retention_policy_check() -> None:
    """P0-2(b)：EventLog 的保留/归档策略写下来、且与代码事实一致。

    三颗分别钉：① 册内保留被禁止（`impl EventLog` 的公开面里没有任何截断/淘汰/压缩入口）；
    ② 归档粒度是封段且满段不可改写（分段写入路径的"只有尾段可增长"判据在盘）；
    ③ 策略三条杠杆写进 `EventLog` 的文档，并在 `maturity/capabilities.yaml` 的登记面上有落点
    （策略与登记不能各说各话）。
    """
    impl_body = _fn_body(production_text((ROOT / EVENT_LOG_SOURCE).read_text(encoding="utf-8")),
                         "impl EventLog {")
    offenders = [
        verb
        for verb in EVENT_LOG_EVICTION_VERBS
        if re.search(rf"\bfn\s+{verb}\b", impl_body)
    ]
    check(
        impl_body and offenders == [],
        "EventLog 的公开面没有截断/淘汰/压缩入口（活日志不许被改短）",
        f"取到 impl 体 {len(impl_body)} 字符 / 越界入口 {offenders}",
    )
    segment_body = production_text(
        (ROOT / EVENT_LOG_SEGMENT_SOURCE).read_text(encoding="utf-8")
    )
    check(
        "active_segment_can_extend" in segment_body
        and "old.lines().count() < self.max_events_per_segment" in segment_body,
        "分段后端只有尾段可增长、满段永远不可变（封段因此是可归档单元）",
        "分段写入路径里没有『只有未满的尾段可扩展』这条判据",
    )
    raw = (ROOT / EVENT_LOG_POLICY_DOC).read_text(encoding="utf-8")
    registry = (ROOT / EVENT_LOG_REGISTRY).read_text(encoding="utf-8")
    missing = [anchor for anchor in EVENT_LOG_POLICY_ANCHORS if anchor not in raw]
    # 登记项必须是**列表项**（`- <名>：`），不能只靠正文里的一句交叉引用满足——
    # 那两条 limitation 名在本文件别处也被引用（「与 X 同族」），裸子串判据看不见条目被摘掉。
    unregistered = [
        name
        for name in EVENT_LOG_POLICY_REGISTRY_NAMES
        if re.search(rf"^\s*-\s*{re.escape(name)}\s*[:：]", registry, re.MULTILINE) is None
    ]
    check(
        missing == [] and unregistered == [],
        "保留/归档策略三条杠杆写在写面的模块文档里，且与能力登记面互指",
        f"文档缺锚点 {missing} / 登记面缺 {unregistered}",
    )


# P0-2(a)：写面稳态追加不许再靠"整份状态拷贝"换提交/回滚语义。
PIPELINE_SOURCE = "crates/qx-runtime/src/pipeline.rs"
COMMIT_SOURCE = "crates/qx-runtime/src/pipeline/commit.rs"
# 旧写法：`let mut staged = self.clone();` … `*self = staged;`（每次追加 O(日志长度)）。
CLONE_COMMIT_MARKERS = ("let mut staged = self.clone();", "*self = staged;")
COMMIT_ROLLBACK_WRAPPERS = (
    "fn register_order_with_correlation_once(",
    "fn ingest_once(",
)
COMMIT_ROLLBACK_CALL = "self.rebuild_from_store(true)?;"
COMMIT_ROLLBACK_CASE = "failed_ingest_rolls_back_the_appended_fact_and_the_seq_cursor"


def pipeline_commit_rollback_check() -> None:
    """P0-2(a)：写面提交不再整份拷贝状态，失败靠强制重放回滚（且回滚有行为证据）。

    四颗分别钉：① 生产源码里没有"整份拷贝式提交"的旧写法复活；② 两条写面各自的薄包装在盘
    且都走强制重放回滚（少一条就是那条写面偷偷退回拷贝或退回会短路的普通重建）；③ 短路被
    `force` 门控住（`!force &&` 在盘，否则 `next_seq` 会漏回滚）；④ 行为用例在盘。
    """
    production = production_text((ROOT / PIPELINE_SOURCE).read_text(encoding="utf-8"))
    commit = production_text((ROOT / COMMIT_SOURCE).read_text(encoding="utf-8"))
    resurrected = [
        marker
        for marker in CLONE_COMMIT_MARKERS
        if marker in production or marker in commit
    ]
    check(
        resurrected == [],
        "写面提交不再整份拷贝状态（`self.clone()` + `*self = staged` 的旧写法已消失）",
        f"旧写法复活：{resurrected}",
    )
    missing_wrappers = [name for name in COMMIT_ROLLBACK_WRAPPERS if name not in commit]
    rollback_calls = commit.count(COMMIT_ROLLBACK_CALL)
    check(
        missing_wrappers == [] and rollback_calls == len(COMMIT_ROLLBACK_WRAPPERS),
        "两条写面（订单提交 / 事实归约）都有薄包装，且都走强制重放回滚",
        f"缺包装 {missing_wrappers} / 强制回滚调用 {rollback_calls} 处",
    )
    rebuild_body = _fn_body(production, "fn rebuild_from_store(")
    check(
        "fn rebuild_from_store(&mut self, force: bool) -> QxResult<()>" in production
        and "!force &&" in rebuild_body,
        "整份重建的短路被 `force` 门控住（失败回滚必须强制重放，否则 next_seq 漏回滚）",
        "重建体里没有 `force: bool` 形参或 `!force &&` 短路门",
    )
    check(
        f"fn {COMMIT_ROLLBACK_CASE}(" in (ROOT / PIPELINE_SOURCE).read_text(encoding="utf-8"),
        "失败回滚有行为用例在盘（断事件数与 seq 游标都收回，且回滚后仍可继续）",
        f"缺用例 {COMMIT_ROLLBACK_CASE}",
    )


# V13 R28：多账户 / 多 Venue / 多运行模式并行运行的隔离不变量。
#
# 「同时跑」的前提是：同一份 runtime 里两个账户各跑各的执行 worker 时，事实只落进自己
# 那本账户 EventLog、命令也只被同账户 worker 消费。此前这条不变量只有命名层用例（名字拼得
# 对）与读侧去重用例（同一账户不分裂），**没有一条用例真的把两个账户的 worker 各跑一遍、
# 再翻开两本账看有没有串账**——命名对不等于事实不串。七颗牙齿：① 四条用例在册；
# ② 第二账户日志名从唯一构造点派生（不抄字面量）；③ 写侧两账户各跑真实执行 worker；
# ④ 写侧两本账互查「对方账户现金为 0」；⑤ 命令按账户分派；⑥ 初始资金按 worker 声明的账户入账；
# ⑦ 并发用例真开两条线程 + 屏障强制重叠（顺序调用不算并发）。
PARALLEL_ISOLATION_CASE_FILE = "crates/qx-cli/src/tests/parallel_run_isolation.rs"
PARALLEL_ISOLATION_CASES = (
    "two_accounts_on_one_venue_keep_separate_books",
    "a_submit_command_scoped_to_one_account_is_invisible_to_the_other_account_worker",
    "each_configured_account_gets_its_own_projection_source",
    "two_accounts_running_concurrently_keep_separate_books",
)
# 并发那条用例的现场形状：两条线程 + 一个屏障（否则"并发"只是顺序调用的另一种写法）。
PARALLEL_CONCURRENT_CASE = "two_accounts_running_concurrently_keep_separate_books"
PARALLEL_CONCURRENCY_BARRIER = "Barrier::new(2)"
PARALLEL_THREAD_SPAWN = "thread::spawn("
# 第二账户的日志名必须从唯一构造点派生：用例里出现字面量日志名 = 命名口径长出第二份。
PARALLEL_SECOND_LOG_LITERAL = "paper-account-b-paper-events"
PARALLEL_ACCOUNT_LOG_HELPER = "fn account_log(account: &str) -> String {"
PARALLEL_IDENTITY_CALL = 'account_event_log_name(account, "paper")'
# 写侧两个 worker id：主账户用字面量，第二账户用常量（常量定义本身也要在盘）。
PARALLEL_MAIN_WORKER_LITERAL = '"paper-execution"'
PARALLEL_SECOND_WORKER_CONST = 'const SECOND_WORKER_ID: &str = "paper-execution-b";'
PARALLEL_REAL_ENTRY = "run_paper_execution_worker("
# 串账断言：两本账各查一次「对方账户的现金为 0」。
PARALLEL_CROSS_ACCOUNT_READS = (
    'cash_for(SECOND_ACCOUNT, "USDT")',
    'cash_for("main", "USDT")',
)
# 生产侧：命令按账户分派 + 初始资金按 worker 声明的账户入账（不是硬编码 `main`）。
PAPER_SUBMIT_SOURCE = "crates/qx-cli/src/venue_runtime/paper_submit.rs"
PAPER_SUBMIT_MATCH_SIG = "fn paper_submit_matches_worker("
PAPER_SUBMIT_ACCOUNT_FILTER = "order.account_id==expected_account"
SEED_CASH_SIG = "fn seed_paper_initial_cash("
SEED_CASH_ACCOUNT_FROM_WORKER = "account_id:account_id.into(),"


def parallel_run_isolation_check() -> None:
    """V13 R28：多账户 / 多 Venue 并行运行的隔离不变量（写侧、命令路由、读侧、真并发）。"""
    case_text = case_source(PARALLEL_ISOLATION_CASE_FILE)
    check(
        all(f"fn {case}(" in case_text for case in PARALLEL_ISOLATION_CASES)
        and case_text.count("#[test]") == len(PARALLEL_ISOLATION_CASES),
        "多账户并行隔离四条用例在册：两账户各跑真实 worker / 命令按账户分派 / 每账户一份投影 / 真并发",
        f"在册 {[c for c in PARALLEL_ISOLATION_CASES if f'fn {c}(' in case_text]}，"
        f"#[test] 总数 {case_text.count('#[test]')}",
    )
    helper_body = _fn_body(case_text, PARALLEL_ACCOUNT_LOG_HELPER)
    check(
        PARALLEL_IDENTITY_CALL in helper_body
        and PARALLEL_SECOND_LOG_LITERAL not in case_text,
        "第二账户日志名从唯一构造点派生，用例里不抄字面量",
        f"派生调用在盘={PARALLEL_IDENTITY_CALL in helper_body}，"
        f"字面量日志名出现={PARALLEL_SECOND_LOG_LITERAL in case_text}",
    )
    isolation_body = _fn_body(case_text, f"fn {PARALLEL_ISOLATION_CASES[0]}(")
    check(
        PARALLEL_MAIN_WORKER_LITERAL in isolation_body
        and "SECOND_WORKER_ID" in isolation_body
        and PARALLEL_SECOND_WORKER_CONST in case_text
        and isolation_body.count(PARALLEL_REAL_ENTRY) == 2,
        "写侧两个账户各跑一次真实执行 worker（不是在本用例里重造入账逻辑）",
        f"主账户 id 在盘={PARALLEL_MAIN_WORKER_LITERAL in isolation_body}，"
        f"第二 worker 常量在盘={PARALLEL_SECOND_WORKER_CONST in case_text}，"
        f"真实入口调用 {isolation_body.count(PARALLEL_REAL_ENTRY)} 次",
    )
    check(
        all(needle in isolation_body for needle in PARALLEL_CROSS_ACCOUNT_READS),
        "写侧两本账互查一次『对方账户现金为 0』（串账会落在错误的一侧而当场可见）",
        f"缺反向读数 {[n for n in PARALLEL_CROSS_ACCOUNT_READS if n not in isolation_body]}",
    )
    concurrent_body = _fn_body(case_text, f"fn {PARALLEL_CONCURRENT_CASE}(")
    check(
        concurrent_body.count(PARALLEL_THREAD_SPAWN) == 2
        and PARALLEL_CONCURRENCY_BARRIER in concurrent_body
        and concurrent_body.count(PARALLEL_REAL_ENTRY) == 2,
        "并发用例真的开两条线程跑两个账户的真实 worker，并用屏障强制重叠（顺序调用不算并发）",
        f"线程 {concurrent_body.count(PARALLEL_THREAD_SPAWN)} 条 / "
        f"屏障在盘={PARALLEL_CONCURRENCY_BARRIER in concurrent_body} / "
        f"真实入口 {concurrent_body.count(PARALLEL_REAL_ENTRY)} 次",
    )
    submit_source = production_code_text(ROOT / PAPER_SUBMIT_SOURCE)
    match_body = re.sub(r"\s+", "", _fn_body(submit_source, PAPER_SUBMIT_MATCH_SIG))
    seed_body = re.sub(r"\s+", "", _fn_body(submit_source, SEED_CASH_SIG))
    check(
        PAPER_SUBMIT_ACCOUNT_FILTER in match_body,
        "命令按账户分派：`paper_submit_matches_worker` 拿订单账户比对 worker 声明账户",
        f"取到函数体 {len(match_body)} 字符 / 缺账户比对",
    )
    check(
        SEED_CASH_ACCOUNT_FROM_WORKER in seed_body,
        "初始资金按 worker 声明的账户入账（硬编码 `main` 会让多账户共享控制面时串账）",
        f"取到函数体 {len(seed_body)} 字符 / 缺 worker 账户入账",
    )


# —— V13 R31：现读模型的按账户收窄通道（/account/ledger、/reconcile/reports）——
# 从「四条整体现读端点一律不收窄」里拆出两条：它们读的仍是整份现读模型，但底层类型本来就带
# 账户归属列（`LedgerEntry.account_id`；`ReconcileReportSnapshot` 还多一列 `venue_id`），所以
# `?account_id=` 真的能过滤；`/scheduler/runs` 与 `/control/audit` 的 `JobRun` / `AuditRecord`
# 没有那一列，只能继续回 400。这一族最容易出的退化是四个方向：过滤臂退回只回整份台账（收窄变
# 成摆设）、空结果改成 404（借了投影那族的口径，读者把「这一轮没有事实」读成「这个账户不存
# 在」）、三张路由名单挪走一半（admission 的名单与分派的名单从此各说各话）、以及名单只写在
# 注释里（代码照旧不收窄）。取数一律走 `production_text`，注释里的锚点不算证据。
READ_SCOPE_FILE = "crates/qx-api/src/read_scope.rs"
READ_SCOPE_CASE_FILE = "crates/qx-cli/src/tests/api_read_route_query_contract.rs"
READ_SCOPE_CASES = (
    "keyless_read_routes_refuse_a_query_they_cannot_honor",
    "model_filter_routes_honor_the_account_scope",
    "reconcile_reports_narrow_by_venue_alone",
    "model_filter_routes_return_an_empty_array_not_a_missing_projection",
    "model_filter_routes_refuse_the_keys_their_data_cannot_honor",
    "projection_scoped_routes_keep_their_own_key_contract",
    "endpoint_table_only_promise_query_keys_where_the_route_reads_them",
    "keyed_and_keyless_route_lists_are_disjoint_and_both_live",
    "snapshot_diff_returns_only_the_documented_non_200_codes",
)
READ_SCOPE_PROJECTION = "const PROJECTION_SCOPED_ROUTES: [&str; 7] = ["
READ_SCOPE_MODEL_FILTER = "const MODEL_FILTER_ROUTES: [(&str, &[&str]); 2] = ["
READ_SCOPE_KEYLESS = "const KEYLESS_READ_ROUTES: [&str; 2] = ["
READ_SCOPE_LEDGER_PARAMS = "const LEDGER_FILTER_PARAMS: [&str; 1] = ["
READ_SCOPE_REPORT_PARAMS = "const REPORT_FILTER_PARAMS: [&str; 2] = ["
READ_SCOPE_ARM_LEDGER = '("GET", "/account/ledger") => match ScopeFilter::from_query(query) {'
READ_SCOPE_ARM_REPORT = '("GET", "/reconcile/reports") => match ScopeFilter::from_query(query) {'
READ_SCOPE_404 = "account_projection_not_found"


def _const_literals(marker: str, text: str) -> list[str]:
    """取 `marker` 起、到 `];` 止那段里的全部字符串字面量（收窄键名或路由名）。"""
    start = text.find(marker)
    if start < 0:
        return []
    end = text[start:].find("];")
    if end < 0:
        return []
    return text[start : start + end].split('"')[1::2]


def _arm_text(text: str, marker: str) -> str:
    """取 `marker` 起到花括号平衡结束的那段分派臂。

    marker 必须连同 `=>` 与 `match` 表达式一起写出：收窄调用就写在 `=>` 与 `{` 之间，
    若只取 `{` 之后的函数体，判据看见的就是一个已经收过的窄集合，过滤与否全凭注释。
    marker 找不到时回空串——把过滤臂退回旧写法会让整个锚点失配，判据当场红而不是照绿。
    """
    start = text.find(marker)
    if start < 0:
        return ""
    body = _fn_body(text, marker)
    if not body:
        return ""
    return text[start : text.find("{", start)] + body


def read_face_scope_check() -> None:
    """V13 R31：读面的三张路由名单是收窄口径的单点，过滤臂与键形状口径逐臂钉住。"""
    scope_path = ROOT / READ_SCOPE_FILE
    scope_raw = scope_path.read_text(encoding="utf-8") if scope_path.is_file() else ""
    scope = production_text(scope_raw)
    check(
        bool(scope)
        and len(scope_raw.splitlines()) < OVERSIZED
        and "mod read_scope;" in production_text(
            (ROOT / "crates/qx-api/src/lib.rs").read_text(encoding="utf-8")
        ),
        "读面收窄契约住在 read_scope.rs（低于行数门槛、不进棘轮登记集）且已挂载进 lib.rs",
        f"在盘={bool(scope_raw)} / 行数={len(scope_raw.splitlines())} / 门槛={OVERSIZED}",
    )

    projection = _const_literals(READ_SCOPE_PROJECTION, scope)
    keyless = _const_literals(READ_SCOPE_KEYLESS, scope)
    filtered = _const_literals(READ_SCOPE_MODEL_FILTER, scope)
    dispatch = production_text((ROOT / "crates/qx-api/src/lib.rs").read_text(encoding="utf-8"))
    live = [route for route in projection + keyless + filtered if f'"/{route[1:]}"' in dispatch]
    groups = [set(projection), set(keyless), set(filtered)]
    check(
        len(projection) == 7
        and len(filtered) == 2
        and len(keyless) == 2
        and len(set(projection) | set(keyless) | set(filtered)) == len(projection) + len(filtered) + len(keyless)
        and len(live) == len(projection) + len(filtered) + len(keyless),
        "三张路由名单条目数自洽、两两不相交，且每条都在 handle_inner 里有活的分派臂",
        f"带键={len(projection)} 过滤={len(filtered)} 无键={len(keyless)} 有分派臂={len(live)}",
    )

    admission = production_text((ROOT / "crates/qx-api/src/admission.rs").read_text(encoding="utf-8"))
    check(
        admission.count("crate::read_scope::accepted_query_params") == 1
        and admission.count("crate::read_scope::event_scoped_params") == 1
        and not re.search(r"^\s*const\s+\w*_PARAMS", admission, re.MULTILINE),
        "查询键名单只有 read_scope 一份：admission 只借名单、不再自持 *_PARAMS 常量",
        f"accepted_query_params 引用 {admission.count('crate::read_scope::accepted_query_params')} 次 / "
        f"event_scoped_params 引用 {admission.count('crate::read_scope::event_scoped_params')} 次",
    )
    check(
        all(marker in scope for marker in (READ_SCOPE_MODEL_FILTER, READ_SCOPE_KEYLESS))
        and all(
            name in scope
            for name in ("EVENT_SCOPED_PARAMS", "SNAPSHOT_DIFF_PARAMS", "ACCOUNT_SCOPED_PARAMS")
        )
        and "accepted_query_params(route)?" in admission
        and "fn accepted_query_params(" not in admission,
        "accepted_query_params 是唯一取名册的出口：admission 真的委派到它、自己不再定义一份，"
        "五张名单都在 read_scope 体内",
        "取数出口或名单被挪走",
    )

    ledger_struct = production_text((ROOT / "crates/qx-core/src/ledger/mod.rs").read_text(encoding="utf-8"))
    ledger_body = _fn_body(ledger_struct, "pub struct LedgerEntry {")
    report_body = _fn_body(dispatch, "pub struct ReconcileReportSnapshot {")
    ledger_params = _const_literals(READ_SCOPE_LEDGER_PARAMS, scope)
    report_params = _const_literals(READ_SCOPE_REPORT_PARAMS, scope)
    check(
        "pub account_id: String" in ledger_body
        and "venue_id" not in ledger_body
        and "pub account_id: String" in report_body
        and "pub venue_id: String" in report_body
        and ledger_params == ["account_id"]
        and report_params == ["account_id", "venue_id"],
        "收窄键名单与底层类型的归属列一致：LedgerEntry 只有 account_id 一列，"
        "ReconcileReportSnapshot 两列都有（账簿那一格只认一把键是数据结构决定的，不是取巧）",
        f"ledger 参数={ledger_params} / report 参数={report_params}",
    )

    ledger_arm = _arm_text(dispatch, READ_SCOPE_ARM_LEDGER)
    report_arm = _arm_text(dispatch, READ_SCOPE_ARM_REPORT)
    tight = [re.sub(r"\s+", "", arm) for arm in (ledger_arm, report_arm)]
    check(
        len(tight[0]) > 0
        and len(tight[1]) > 0
        and all("ScopeFilter::from_query(query)" in arm and "scope_keeps_account" in arm for arm in tight)
        and "scope_keeps_venue" in tight[1]
        and not any("missing_projection_response" in arm for arm in tight),
        "两条过滤臂真在结果集上过滤（读代码不读注释），且不借用投影那族的 missing_projection_response",
        f"ledger 臂 {len(tight[0])} 字符 / report 臂 {len(tight[1])} 字符",
    )
    check(
        all("ApiResponse::json(200," in arm for arm in tight)
        and not any(READ_SCOPE_404 in arm for arm in tight),
        "过滤不出条目时回 200 空数组，而不是投影那族的 404 account_projection_not_found",
        "过滤臂改回了整份台账或借用了 404 口径",
    )
    check(
        all(
            "read_error_response(&error)" in arm
            and "ApiResponse::json(503,error_json(&error))" in arm
            for arm in tight
        ),
        "两条过滤臂保住 400/503 的分工：键形状错回 400，现读模型取不到回 503，两者不合并",
        "错误出口被合并，键拼错会被读成后端故障（或反过来）",
    )

    readme = (ROOT / "deploy/README.md").read_text(encoding="utf-8")
    header = "| 端点 | 语义 | 非 200 口径 |"
    rows: dict[str, str] = {}
    for line in readme[readme.find(header) :].splitlines():
        if not line.startswith("| "):
            break
        if not line.startswith("| `"):
            continue  # 分隔行 `| --- |`，不是数据行
        if "/account/ledger" in line:
            rows["ledger"] = line
        if "/reconcile/reports" in line:
            rows["report"] = line
        if "/scheduler/runs" in line:
            rows["keyless"] = line
    check(
        "[?account_id=" in rows.get("ledger", "")
        and "[?account_id=&venue_id=" in rows.get("report", "")
        and all(READ_SCOPE_404 not in rows.get(name, "") for name in ("ledger", "report"))
        and "[?" not in rows.get("keyless", ""),
        "端点表只在这些入口真读收窄键的地方承诺查询串：两条过滤臂写出各自的键，无键那一支不写 [?",
        f"ledger={len(rows.get('ledger', ''))} 字符 / report={len(rows.get('report', ''))} / "
        f"keyless={len(rows.get('keyless', ''))}",
    )
    cases = case_source(READ_SCOPE_CASE_FILE)
    check(
        all(f"fn {case}(" in cases for case in READ_SCOPE_CASES)
        and cases.count("#[test]") == len(READ_SCOPE_CASES),
        "读面收窄的九条行为用例在册：键形状 / 按账户过滤 / 单独 venue / 空结果口径 / "
        "名单外键 / 带键读面自身口径 / 端点表承诺 / 三张名单互斥",
        f"在册 {[c for c in READ_SCOPE_CASES if f'fn {c}(' in cases]}，"
        f"#[test] 总数 {cases.count('#[test]')}",
    )


# ---- QX-DEV-PLAN-2026-10-10 T0-2：顶层 CLI 命令面的 R/P/O/L 能力分类 ----
# 「46 条 CLI 入口」此前只是一张名字清单（`crates/qx-cli/tests/command_surface.rs` 的 CLI_COMMANDS
# 逐条跑 `--help`），没有任何一处声明某条入口属于 R/P/O/L 哪一档、默认面是什么、要不要私有凭据、
# 会不会产生外部副作用。缺了这张声明，SDK/API 侧无从判断一条入口能不能被第三方门面复用，门禁上也
# 钉不住「L 档默认关闭」。本判据把 maturity/command_surface.yaml 与 clap 命令表逐一对账。
COMMAND_SURFACE_FILE = "maturity/command_surface.yaml"
COMMAND_SURFACE_LEVELS = ("R", "P", "O", "L")
COMMAND_SURFACE_FIELDS = (
    "name",
    "level",
    "parent",
    "surface",
    "credentials",
    "external_side_effect",
    "default_enabled",
    "note",
)


def _parse_command_surface(text: str) -> list[dict[str, str]]:
    """解析 command_surface.yaml：返回 `commands` 段的条目列表（不引 yaml 依赖，逐行解析）。"""
    entries: list[dict[str, str]] = []
    current: dict[str, str] | None = None
    in_commands = False
    for line in text.splitlines():
        if re.match(r"^commands:\s*$", line):
            in_commands, current = True, None
            continue
        if re.match(r"^\S", line):  # 顶层键结束当前段
            in_commands, current = False, None
            continue
        if not in_commands:
            continue
        if m := re.match(r"^  - name:\s*(.+?)\s*$", line):
            current = {"name": m.group(1)}
            entries.append(current)
        elif current is not None and (
            m := re.match(
                r"^    (level|parent|surface|credentials|external_side_effect|"
                r"default_enabled|note):\s*(.*?)\s*$",
                line,
            )
        ):
            current[m.group(1)] = m.group(2)
    return entries


def command_surface_classification_check() -> None:
    """T0-2：把 46 条 CLI 入口从「名字清单」升级为「能力声明表」。

    每条入口声明它属于哪一档（R 研究 / P Paper / O 运维 / L 实盘）、默认面、是否需要私有凭据、
    是否产生外部副作用、是否默认可用。判据捕获的故障样本：① 新增 CLI 入口却没登记能力档——该入口
    属于哪一档、默认面与权限无人声明（G0 要抓的「入口无声明」）；② 把某条 L 档入口改成
    `default_enabled: true`——实盘能力被「全能力包」自动开启（主计划 §5 明令禁止）；③ 给 P 档入口
    写上 `credentials: private`——Paper 轨本应一条凭据都不用。

    父命令（clap 里带 `#[command(subcommand)]` 的 `config`/`run`/`strategy`/`backtest`）是命名空间，
    等级取子命令里的最高权限档、本身不直接产生副作用，故三条不变量只对叶子入口生效；父命令另由
    「逐条标 parent: true 且集合与 clap 的父命令表相等」一颗钉住。
    """
    path = ROOT / COMMAND_SURFACE_FILE
    if not path.is_file():
        check(
            False,
            "CLI 命令面能力分类表在盘（maturity/command_surface.yaml）",
            f"缺 {COMMAND_SURFACE_FILE}",
        )
        return
    entries = _parse_command_surface(path.read_text(encoding="utf-8"))
    args_text = (ROOT / CLI_ARGS_FILE).read_text(encoding="utf-8")
    table = set(clap_command_table(args_text))
    parents = set(clap_subcommand_parents(args_text, cli_args_sources()))
    names = [entry.get("name", "") for entry in entries]
    incomplete = [
        entry.get("name") for entry in entries
        if not all(entry.get(field) for field in COMMAND_SURFACE_FIELDS)
    ]
    check(
        len(entries) == len(table)
        and not incomplete
        and all(entry.get("parent") in ("true", "false") for entry in entries),
        "命令面分类表按行可解析，且每行八格自述齐全"
        "（name/level/parent/surface/credentials/external_side_effect/default_enabled/note）",
        f"解析到 {len(entries)} 行（clap 表 {len(table)}）；缺格 {incomplete or '无'}",
    )
    check(
        len(names) == len(set(names)) and set(names) == table,
        "分类表覆盖的入口与 clap 命令表逐一相等（新增入口没登记 = 红；登记了已删入口 = 红）",
        f"表有而 clap 无 {sorted(set(names) - table) or '无'}；"
        f"clap 有而表无 {sorted(table - set(names)) or '无'}",
    )
    levels = {entry.get("level") for entry in entries}
    parent_rows = {entry["name"] for entry in entries if entry.get("parent") == "true"}
    check(
        levels <= set(COMMAND_SURFACE_LEVELS)
        and levels == set(COMMAND_SURFACE_LEVELS)
        and parent_rows == parents,
        "等级只取 R/P/O/L 四档且四档都有，父命令（clap 里带 subcommand 的）逐条标 parent: true",
        f"出现的档 {sorted(levels)}；父命令 表={sorted(parent_rows)} clap={sorted(parents)}",
    )
    leaves = [entry for entry in entries if entry.get("parent") != "true"]
    live_bad = [
        entry["name"]
        for entry in leaves
        if entry.get("level") == "L"
        and (entry.get("default_enabled") != "false" or entry.get("credentials") != "private")
    ]
    check(
        not live_bad,
        "L 档入口一律 default_enabled: false 且 credentials: private（实盘不能被全能力包自动开启）",
        f"越界 {live_bad or '无'}",
    )
    paper_bad = [
        entry["name"]
        for entry in leaves
        if entry.get("level") == "P" and entry.get("credentials") != "none"
    ]
    check(
        not paper_bad,
        "P 档入口一律 credentials: none（Paper 轨一条凭据都不用）",
        f"越界 {paper_bad or '无'}",
    )
    research_bad = [
        entry["name"]
        for entry in leaves
        if entry.get("level") == "R" and entry.get("external_side_effect") != "none"
    ]
    check(
        not research_bad,
        "R 档入口一律 external_side_effect: none（研究面不产生外部账户副作用）",
        f"越界 {research_bad or '无'}",
    )


# —— 阶段 2 / 退出门 G1·G2：应用层用例（`qx-app`）——
# 依赖方向由 `layer_dependency_check` 的禁止边守着；这里守的是**另外四件事**：
# ① 依赖集是精确的（依赖清单本身是"没有偷偷把门面拉进来"这条纪律的载体）；
# ② 错误类别闭集与路线图 §X1 那张表逐条同名同序（类别少一个，调用方就会把它送错下一步）；
# ③ 九项用例登记面与 `cases/mod.rs` 的文档表、`pub use`、`pub fn` 三处对账（改了这里不改登记面会红）；
# ④ 三个门面真的都依赖应用层（G1 的"三入口同一 use case"在依赖图上就成立，不靠约定）。
APP_CRATE = "crates/qx-app"
APP_CRATE_NAME = "qx-app"
APP_ERROR_FILE = "crates/qx-app/src/error.rs"
APP_CASES_FILE = "crates/qx-app/src/cases/mod.rs"
APP_CASES_DIR = "crates/qx-app/src/cases"
APP_REGISTRY_FILE = "maturity/app_use_cases.yaml"
APP_REGISTRY_KIND = "app-use-case-registry"
# G1 的三个门面。它们各自的门面实现落点也一并钉住：源码面在盘 = "入口存在"这件事有牙齿，
# 不必靠 Python 扩展是否已构建来决定。
APP_FACADES = (
    ("qx-cli", "crates/qx-cli/src/app_commands.rs"),
    ("qx-api", "crates/qx-api/src/app_surface.rs"),
    ("qx-python", "crates/qx-python/src/lib.rs"),
)
# 路线图 §2 的九项，顺序即登记面与文档表的顺序。`(文档表第一列, 登记面键名)`。
APP_USE_CASE_ITEMS = (
    ("输入 schema", "input_schema"),
    ("输出 schema", "output_schema"),
    ("运行权限", "capability"),
    ("幂等键", "idempotency_key"),
    ("取消行为", "cancellation"),
    ("事件/进度", "progress"),
    ("产物清单", "artifacts"),
    ("错误类别", "error_categories"),
    ("能力等级", "capability_level"),
)
APP_ALLOWED_INTERNAL_DEPS = frozenset(
    {"qx-core", "qx-datastruct", "qx-strategy", "qx-xingban", "qx-zhenlu"}
)
APP_ALLOWED_EXTERNAL_DEPS = frozenset({"serde", "serde_json"})
# 登记面写的是能力档的稳定串（路线图 §5 的 R/P/O/L），代码里写的是枚举变体名——两处口径不同形，
# 所以映射表在这里写一次，而不是让判据去猜（"RESEARCH" 与 "Research" 差一个大小写，猜就会假绿）。
APP_CAPABILITY_VARIANTS = {
    "RESEARCH": "Research",
    "PAPER": "Paper",
    "OPERATOR": "Operator",
    "LIVE": "Live",
}


def _app_dependency_names() -> tuple[set[str], set[str]]:
    """`crates/qx-app/Cargo.toml` 的 `[dependencies]` 段里声明的依赖名（内部 / 外部）。"""
    text = (ROOT / APP_CRATE / "Cargo.toml").read_text(encoding="utf-8")
    names = set()
    for line in _toml_section_entries(text, "dependencies"):
        matched = re.match(r"^\s*([A-Za-z_][A-Za-z0-9_-]*)\s*=", line)
        if matched:
            names.add(matched.group(1))
    internal = {name for name in names if name.startswith("qx-")}
    return internal, names - internal


def _app_error_category_variants() -> list[str]:
    """`AppErrorCategory` 的变体名，按声明顺序。"""
    text = (ROOT / APP_ERROR_FILE).read_text(encoding="utf-8")
    block = _block_body(text, "pub enum AppErrorCategory {")
    return re.findall(r"^\s*([A-Z][A-Za-z0-9]*),\s*$", block, re.MULTILINE)


def _registered_app_error_categories() -> list[str]:
    """机读应用契约里的错误类别闭集；不依赖本机附加的方案文档。"""
    text = (ROOT / APP_REGISTRY_FILE).read_text(encoding="utf-8")
    matched = re.search(
        r"^error_categories:\s*\n((?:  - [A-Za-z]+\s*\n)+)",
        text,
        re.MULTILINE,
    )
    return re.findall(r"^  - ([A-Za-z]+)\s*$", matched.group(1), re.MULTILINE) if matched else []


def _app_use_case_registry() -> list[dict[str, str]]:
    """解析 `maturity/app_use_cases.yaml` 的 `use_cases` 段（逐行，不引 yaml 依赖）。"""
    entries: list[dict[str, str]] = []
    current: dict[str, str] | None = None
    in_use_cases = False
    for line in (ROOT / APP_REGISTRY_FILE).read_text(encoding="utf-8").splitlines():
        if re.match(r"^use_cases:\s*$", line):
            in_use_cases, current = True, None
            continue
        if re.match(r"^\S", line):
            in_use_cases, current = False, None
            continue
        if not in_use_cases:
            continue
        if matched := re.match(r"^  - name:\s*(.+?)\s*$", line):
            current = {"name": matched.group(1).strip('"')}
            entries.append(current)
        elif current is not None and (matched := re.match(r"^    ([a-z_]+):\s*(.*?)\s*$", line)):
            current[matched.group(1)] = matched.group(2).strip('"')
    return entries


def _app_doc_table() -> tuple[list[str], list[str]]:
    """`cases/mod.rs` 模块文档里那张九项表的 (表头用例名, 第一列项名)。"""
    text = (ROOT / APP_CASES_FILE).read_text(encoding="utf-8")
    rows: list[list[str]] = []
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped.startswith("//! |"):
            continue
        cells = [cell.strip() for cell in stripped[len("//! |") :].rstrip("|").split("|")]
        if len(cells) < 2 or all(set(cell) <= {"-", ":"} for cell in cells):
            continue
        rows.append(cells)
    if not rows:
        return [], []
    header = re.findall(r"`([a-z_]+)`", " ".join(rows[0][1:]))
    return header, [row[0] for row in rows[1:]]


def qx_app_check() -> None:
    """阶段 2 / 退出门 G1·G2：`qx-app` 的依赖集、错误类别闭集、九项登记面与三入口。

    依赖方向（应用层不得反向依赖门面）由 `layer_dependency_check` 的禁止边守着，这里不重复；
    本判据守的是四件它看不见的事，逐条都对应一类"改错了也不红"的故障：

    ① 依赖集**精确**——`Cargo.toml` 是"应用层没有偷偷把 CLI/API/执行/存储拉进来"这条纪律的
       载体，一张含糊的清单守不住它（多一项少一项都要有人回答）；
    ② 错误类别闭集与路线图 §X1 那张表**逐条同名同序**——类别少一个或改一个名字，调用方就会把
       它送错下一步，而"能编译"这件事对类别语义是盲的；
    ③ 九项用例登记面三处对账（登记面 ⇔ 文档表 ⇔ `pub use`/`pub fn`）——路线图 §2 的九项是契约，
       最容易漏的"取消行为"与"幂等键"不写下来时，读者的默认假设是"应该会取消/应该幂等"；
    ④ 三个门面都依赖应用层、且各自的门面实现源码在盘——G1 的"三入口同一 use case"在依赖图上
       与源码面上都成立，不靠"Python 扩展构建过没有"来决定。
    """
    internal, external = _app_dependency_names()
    check(
        internal == set(APP_ALLOWED_INTERNAL_DEPS) and external == set(APP_ALLOWED_EXTERNAL_DEPS),
        "qx-app 的依赖集是精确的（5 个领域件 + serde/serde_json，共 7 名，无门面/适配/执行/存储）",
        f"内部 {sorted(internal)}（应为 {sorted(APP_ALLOWED_INTERNAL_DEPS)}）；"
        f"外部 {sorted(external)}（应为 {sorted(APP_ALLOWED_EXTERNAL_DEPS)}）",
    )

    registered_categories = _registered_app_error_categories()
    variants = _app_error_category_variants()
    check(
        bool(registered_categories) and registered_categories == variants,
        "错误类别闭集与版本化应用契约逐条同名同序（少一类/改名/换序都红）",
        f"登记面 {registered_categories}；AppErrorCategory {variants}",
    )

    registry = _app_use_case_registry()
    registered = [entry.get("name", "") for entry in registry]
    incomplete = [
        entry.get("name")
        for entry in registry
        if not all(entry.get(key) for _, key in APP_USE_CASE_ITEMS)
    ]
    declared_kind = re.search(
        r"^kind:\s*(\S+)\s*$",
        (ROOT / APP_REGISTRY_FILE).read_text(encoding="utf-8"),
        re.MULTILINE,
    )
    check(
        len(registry) == 3
        and len(registered) == len(set(registered))
        and not incomplete
        and declared_kind is not None
        and declared_kind.group(1) == APP_REGISTRY_KIND,
        "九项用例登记面在盘、自述 kind 正确、三个用例的九项逐格非空",
        f"用例 {registered}；缺格 {incomplete or '无'}；kind {declared_kind.group(1) if declared_kind else '缺'}",
    )
    key_order = {
        entry.get("name"): [key for key, _ in sorted(
            ((key, None) for key in entry if key != "name"),
            key=lambda pair: pair[0],
        )]
        for entry in registry
    }
    check(
        all(
            key_order.get(entry.get("name")) == sorted(key for _, key in APP_USE_CASE_ITEMS)
            for entry in registry
        ),
        "登记面每个用例的键集恰好是那九项（多一格少一格都红）",
        f"实得 {key_order}",
    )

    cases_text = (ROOT / APP_CASES_FILE).read_text(encoding="utf-8")
    reexported = set(re.findall(r"^pub use [a-z_]+::([a-z_]+);$", cases_text, re.MULTILINE))
    doc_names, doc_items = _app_doc_table()
    missing_fns = sorted(
        name
        for name in registered
        if not (ROOT / APP_CASES_DIR / f"{name}.rs").is_file()
        or f"pub fn {name}("
        not in (ROOT / APP_CASES_DIR / f"{name}.rs").read_text(encoding="utf-8")
    )
    check(
        set(registered) == reexported and not missing_fns,
        "登记面的用例集合 == `cases/mod.rs` 的 `pub use`，且每个用例都有自己的 `pub fn` 落点",
        f"登记 {sorted(registered)}；重导出 {sorted(reexported)}；缺 `pub fn` {missing_fns or '无'}",
    )
    check(
        doc_names == registered
        and doc_items == [label for label, _ in APP_USE_CASE_ITEMS],
        "`cases/mod.rs` 的九项文档表与登记面逐条相等（表头三个用例名 + 第一列九项）",
        f"文档表用例 {doc_names}（应 {registered}）；文档表九项 {doc_items}",
    )

    # 能力闸口径：登记面说 `RESEARCH` 的用例必须在代码里真的设闸；说 `none` 的必须真的没设。
    # 这条有牙齿——"文档说不需要权限"与"代码里加了权限"是两件互相矛盾的事，而两边都能编译。
    gate_mismatch: list[str] = []
    for entry in registry:
        name = entry.get("name", "")
        body = (ROOT / APP_CASES_DIR / f"{name}.rs").read_text(encoding="utf-8")
        declared = entry.get("capability")
        has_gate = "context.require(" in body
        if declared == "none":
            if has_gate:
                gate_mismatch.append(f"{name} 登记为不设闸，代码里却有 require")
            continue
        variant = APP_CAPABILITY_VARIANTS.get(declared or "")
        if variant is None:
            gate_mismatch.append(f"{name} 的 capability={declared!r} 不是 R/P/O/L 的稳定串")
        elif not has_gate or f"CallerCapability::{variant}" not in body:
            gate_mismatch.append(
                f"{name} 登记为 {declared}，代码里没有 `CallerCapability::{variant}` 的 require"
            )
    check(
        not gate_mismatch,
        "登记面的 capability 与用例代码里的能力闸一致（说不管就必须真不管）",
        "；".join(gate_mismatch),
    )

    graph = _crate_internal_deps()
    missing_facades = [
        crate
        for crate, source in APP_FACADES
        if not (ROOT / source).is_file() or APP_CRATE_NAME not in graph.get(crate, set())
    ]
    check(
        f'"{APP_CRATE}"' in (ROOT / "Cargo.toml").read_text(encoding="utf-8")
        and not missing_facades,
        "qx-app 是 workspace 成员，且三个门面（CLI / HTTP / Python）都依赖它并有门面实现落点",
        f"缺门面 {missing_facades or '无'}",
    )


def main() -> int:
    if "--snapshot" in sys.argv:
        # `--snapshot` 刷新两份机读快照：先写行数棘轮，再跑一遍完整门禁、收尾写门禁读数快照。
        write_line_budgets()
    merge_duplicate_block_check()
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
    dead_public_entry_check()
    zero_reference_public_surface_check()
    zero_reference_pub_crate_surface_check()
    dead_type_surface_check()
    gate_source_truncation_check()
    control_audit_chain_check()
    audit_sync_control_prefix_check()
    websocket_refused_param_route_check()
    live_path_fail_closed_check()
    untrusted_input_boundaries_check()
    resource_lifecycle_and_lock_reentrancy_check()
    launcher_pregate_check()
    schema_defense_consistency_check()
    api_accept_loop_exit_check()
    transport_read_deadline_check()
    stop_aware_worker_tick_sleep_check()
    health_snapshot_knob_check()
    strategy_intent_three_language_check()
    strategy_output_arm_diagnostics_check()
    workspace_test_floor_check()
    windows_batch_parse_check()
    build_script_parity_check()
    wheel_builder_check()
    ashare_pit_check()
    ashare_limit_anchor_check()
    ashare_cross_language_contract_check()
    venue_report_contract_check()
    ashare_backtest_binding_check()
    ashare_submit_guard_check()
    builtin_signal_check()
    builtin_knob_list_check()
    replay_kernel_check()
    input_provenance_check()
    ledger_kernel_split_check()
    concept_registry_check()
    venue_identity_check()
    settlement_currency_check()
    fee_settlement_currency_check()
    kernel_timeline_check()
    storage_retry_check()
    outbox_page_and_parked_check()
    external_acceptance_check()
    external_chain_check()
    bounded_growth_and_reap_check()
    module_mount_check()
    runtime_config_fail_closed_check()
    backtest_assembly_check()
    paper_fee_same_source_check()
    kernel_claim_check()
    multi_leg_honesty_check()
    market_spec_single_reader_check()
    market_spec_source_check()
    two_leg_partition_check()
    example_read_funnel_check()
    deploy_template_coverage_check()
    submit_terminal_state_check()
    submit_topology_guard_check()
    paper_check_delta_honesty_check()
    wheel_optional_dependency_check()
    snapshot_money_honesty_check()
    account_money_field_registry_check()
    account_pnl_producer_check()
    api_lock_fail_closed_check()
    postgres_connect_budget_check()
    strategy_pipe_write_budget_check()
    event_consumer_pipe_write_budget_check()
    ccxt_pipe_write_budget_check()
    snapshot_contract_version_check()
    snapshot_row_wire_check()
    position_money_honesty_check()
    reconcile_round_honesty_check()
    reconcile_action_and_absence_check()
    control_plane_honesty_check()
    lease_clock_domain_check()
    api_surface_doc_check()
    web_console_check()
    web_console_field_wiring_check()
    native_trust_check()
    console_front_check()
    console_usability_check()
    paper_track_check()
    account_snapshot_schema_check()
    calendar_fingerprint_caliper_check()
    c_abi_header_check()
    snapshot_json_table_check()
    bar_frame_contract_check()
    bar_frame_pit_honesty_check()
    quality_issue_producer_check()
    enum_variant_producer_check()
    event_backtest_evidence_check()
    backtest_clock_honesty_check()
    scheduler_dispatch_honesty_check()
    scheduler_owner_routing_check()
    corporate_action_read_side_check()
    factor_research_honesty_check()
    scheduler_retry_honesty_check()
    backtest_account_base_check()
    account_principal_single_source_check()
    report_readout_honesty_check()
    browser_admission_check()
    capabilities_check()
    backtest_track_check()
    backtest_baseline_check()
    contract_matrix_check()
    http_surface_check()
    baseline_freeze_check()
    release_supply_chain_check()
    performance_baseline_check()
    line_budget_check()
    doc_citation_reachability_check()
    command_status_vocabulary_single_source_check()
    foundation_specs_check()
    strategy_input_schema_check()
    nonfunctional_targets_check()
    capability_levels_check()
    scenario_fixtures_check()
    layer_dependency_check()
    dev_dependency_cycle_check()
    event_log_append_batch_check()
    pipeline_cursor_refresh_check()
    valuation_single_source_check()
    risk_spec_fail_closed_check()
    release_version_single_source_check()
    error_code_contract_check()
    schema_registry_check()
    artifact_migration_check()
    run_evidence_check()
    report_readability_check()
    silent_suppression_check()
    process_diagnostics_check()
    bare_money_default_check()
    environment_production_single_source_check()
    ci_feature_matrix_check()
    ops_read_surface_check()
    api_read_model_liveness_check()
    surface_allowlist_hygiene_check()
    cli_surface_coverage_check()
    file_lock_single_source_check()
    io_budget_and_venue_cache_check()
    gate_snapshot_check()
    event_log_retention_policy_check()
    pipeline_commit_rollback_check()
    parallel_run_isolation_check()
    read_face_scope_check()
    command_surface_classification_check()
    qx_app_check()
    # 含本条自身：+1 才是本轮真正会打印的总条数，所以地板常量按"含这一条"取值。
    check(
        checks + 1 >= GATE_CHECK_FLOOR,
        f"门禁自身至少执行 {GATE_CHECK_FLOOR} 条判据（少一条就是有人整段删掉了判据）",
        f"本轮只执行了 {checks + 1} 条",
    )
    # 收尾：本轮实测条数与 label 名册必须与机读快照逐值相等；`--snapshot` 那一轮改为写入。
    if GATE_SNAPSHOT_MODE:
        write_gate_snapshot(checks + 1)
        check(True, f"本轮实测条数已写入机读快照 `{GATE_SNAPSHOT_DOC_NAME}`（--snapshot）")
    else:
        recorded = read_gate_snapshot()
        check(
            recorded.get("checks") == checks + 1
            and recorded.get("labels_sha256")
            == gate_snapshot_payload(checks + 1)["labels_sha256"],
            f"本轮实测条数与 label 名册同机读快照 `{GATE_SNAPSHOT_DOC_NAME}` 逐值相等",
            f"快照 {recorded.get('checks')} / 本轮 {checks + 1}（加/改判据请跑 --snapshot 刷新）",
        )
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
