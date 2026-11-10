# 首跑：从安装到第一份报告

这一页只回答一个问题：**装完之后，头几条命令敲什么**。它按安装方式分两支，每条命令都可以直接复制；
一份命令属于哪个二进制写在行内，不做混写。能力全景与排障口径不在这里，见
[工业化易用性收口指南](工业化易用性收口指南-V1.md)；完整安装前置与坑表见[安装指南](installation.md)。

> 标注口径：**【已实现】**= 命令在当前工作树里可执行，且行为由常驻用例或门禁钉住；
> **进阶**= 能力存在，但不属于首跑承诺，本页只在末尾点名，不给步骤。本页不重复登记未实现的设计意图，
> 那类条目一律留在方案文档里标【设计意图，未排期】。

## 我该选哪一支

| 你要做的事 | 选这支 | 装完得到的命令 |
|---|---|---|
| 只想跑回测、看报告，不写 Python 策略 | A · Rust CLI | `qx-cli`（Windows 上 `qx-cli.exe`） |
| 用 Python 写策略、取 A 股/加密行情、把运行收进项目台账 | B · Python wheel | `qianxing` |
| 两支都要（Python 写策略，Rust 跑服务端/worker） | B 里补一句 `--cli` | `qianxing` + `qx-cli` |

两支的**首跑承诺相同**：零配置建一个能跑的项目 → 跑一轮 → 出一份可复核的报告。差别只在数据默认值：
A 支自带一份样例 BarFrame，B 支要么用内置合成样例，要么装一个 extra 换真实源。

---

## A. 只装 CLI（不需要 Python）【已实现】

前置只有 Rust 工具链。装好后 `qx-cli` 落在 `~/.cargo/bin`，**在任意目录**都能跑，不需要把仓库带着走：

```bash
cargo install --path crates/qx-cli --locked     # 完全离线的机器再加 --offline
qx-cli quickstart my-qx                          # 建 macd 项目 → 静态检查 → 回测 → 读回摘要 → 安全状态
qx-cli quickstart my-ashare --profile ashare     # 建 A 股样例项目 → 静态检查 → 回测 → 读回摘要 → 安全状态
```

`quickstart` 内部执行的就是下面这五条；它只写 `my-qx/` 这一个目录，产物落项目内 `data/`。
任一步失败就停下，只回显**那一步**的命令原文，以退出码 2 结束：

`--profile` 当前支持 `builtin`（默认）与 `ashare`。A 股首跑使用仓库附带的 6 根样例 Bar 和冻结规格，
完成一笔本地回测；不会连接外部行情源或发送订单。CCXT 的 sandbox 首跑仍需用户提供凭据并完成外部验收。

```bash
cd my-qx
qx-cli init qianxing.runtime.json --strategy macd   # 配置 + 样例 BarFrame + README.qianxing.md
qx-cli doctor qianxing.runtime.json                 # 静态检查：不联网、不启动 worker、不下单
qx-cli backtest qianxing.runtime.json               # 跑一轮 MACD 回测，产物落 data_dir 指向的 runs/
qx-cli report qianxing.runtime.json                 # 读回最新一份摘要
qx-cli status qianxing.runtime.json                 # 本地配置/worker/回测结果的安全状态；不连交易所
```

要可视化结果就在回测后加 `--html`：`qx-cli report qianxing.runtime.json --html` 写一份可离线打开的
HTML 和三张独立 SVG，`-o reports/latest.html` 改输出前缀。

`quickstart` 与逐条敲那五条的相等关系不是文档承诺：同一条输入下两者的 `result_hash` 必须逐字符相同，
由 `crates/qx-cli/tests/quickstart_first_run.rs` 钉成用例。落盘份数同样由用例逐个计数
（`init --strategy macd` 落 11 份（含 Grafana 面板）、首轮回测再写 6 份，共 17 份，见 `crates/qx-cli/tests/init_onboarding_process.rs`）。

---

## B. 只装 wheel（Python 策略作者）【已实现】

### B1 装

```bash
python -m pip install dist/qianxing-0.4.0-cp310-abi3-win_amd64.whl    # 平台换成你的那份
python -c "import qianxing as qx; print(qx.doctor()['status'])"
```

顶层 `dependencies` 是空的（`python/pyproject.toml`），所以这一步不联网也装得成，`--no-deps` 不再是必需项。
`doctor` 里要看的是三格：原生扩展是否 `matched`、用例集是否 `complete`、`report_renderer` 的
`origin` 是 `bundled` / `external` / `missing` 中的哪一个。

wheel 由 `tools/build_python_wheel.ps1` / `.sh` 从当前工作树构建；它在打 wheel 之前先把宿主平台的
`qx-cli` 复制进包内 `qianxing/_bin/`，所以只装 wheel 的用户不需要再装 Rust 也能出报告。这个先后顺序本身
是常驻判据（`tools/check_architecture.py`），包内载荷由 `tools/check_python_wheel.py` 核对。

在源码仓库里试命令时，把 `qianxing` 换成 `python -m qianxing.cli`——两者进的是同一个 `cli.main`
（`python/pyproject.toml` 的 `[project.scripts]` 只指向它）。

### B2 零配置建一个能跑的项目

```bash
qianxing project init ./demo --demo
```

这一条落 9 份文件，其中数据是**内置合成样例**、清单里显式标注不可用于研究结论：

```
qianxing.strategy.json          # 项目清单：策略文件、input_spec、data.source
input.json                      # 一份版本化 BacktestSpec，project 相对路径
strategies/main.py              # 单钩子示例策略（只有 on_bar）
data/bars.json                  # 合成 BarFrame
data/bars.manifest.json         # 行情血缘清单
data/snapshot.json              # DatasetSnapshot（冻结本轮用的字节）
data/snapshot.context.json
data/bundle.json                # 数据集包清单 + 四枚 sha256
.gitignore
```

`--demo` 之外还有 `--source/--instrument/--start/--end/--cash/--currency/--frequency/--adjustment`（见 B5）、
`--strategy-file` / `--input-spec`（把已有文件接进项目）、`--rows`（只作用于合成样例）。

### B3 先看，再跑

```bash
qianxing project doctor ./demo          # 静态检查：issues 为空即 ready，默认不执行用户代码
qianxing project plan ./demo --explain   # 逐键打印有效值 + 来源 + 被谁覆盖
qianxing project run ./demo              # 本地执行一轮，登记 run 收据与产物
```

`project doctor` 默认只做静态元数据与语法检查（输出里 `user_code_executed: false`、
`execution_boundary: static_metadata_and_syntax_only`）；要真跑策略预检得显式加 `--dynamic`，那一步会在
带超时的子进程里执行你的代码。

`plan --explain` 给的是"这个值最后为什么是这个值"：每个键带 `value` / `source` / `overridden`，
来源标签形如 `project_manifest:<绝对路径>#$.input_spec`、`input_spec:…#$.bars_path`，路径类键还带
`resolution: project_relative_path` 与 `original_value`。数据侧单独一格 `data_selection`，里面是
`data.source`（含 `requested` 与最终生效源）、`data.cache_root`、`data.retries`，并标
`scope: future_fetch_only` —— 这一格管的是**以后取数**用哪个源，本轮已冻结的数据集仍以
`datasets[].snapshot_sha256` 为准，改了源也不会换掉已经落盘的行情。

### B4 出报告

```bash
qianxing project report ./demo PROJECT_RUN_ID                       # 默认 HTML
qianxing project report ./demo PROJECT_RUN_ID --format html,xlsx,json
```

`PROJECT_RUN_ID` 用 `qianxing project runs ./demo` 列出的那个 id。渲染器按**单链**选中即用，不做静默替换：
`--cli PATH` > wheel 内置的 `qianxing/_bin/qx-cli` > PATH 上的 `qx-cli`；三个落点都不存在就报
`missing` 并停下，不会替你找一个。版本/模式目录摘要/平台任一不匹配同理——诊断里直接给 `--cli PATH` 的修法，
并写明 `No fallback was attempted`。

报告失败只失败在报告这一格：`report-status.json` 记 `status: failed` + `report_renderer_<状态>` +
`report_sha256: null`，而 run 收据保持它自己的终态，回测成功不会因为报告没出而被改写。
产物字节先复核再渲染，落盘的 HTML/XLSX/JSON 走同一套 bundle 发布事务（见
[回测报告布局](backtest-report-layout.md)）。

### B5 换成真实数据【已实现，A 股取数需装 extra】

```bash
qianxing data sources        # 跨源能力矩阵：市场 / 频率 / 复权 / 公司行为 / 日历 / 凭据 / extra / 许可
qianxing data doctor         # 本机每个源的状态；一个 extra 都没装也退 0，逐源给安装命令
qianxing data fetch --source akshare --code 000001.SZSE --start 20240101 --end 20241231 \
    --output bars.json --manifest bars.manifest.json
qianxing data install --sample --target ./demo/data     # 离线装内置合成样例包
```

源的选择只有一条链，优先级从高到低：

```
--source  >  项目清单 data.source  >  QIANXING_DATA_SOURCE  >  内置样例
```

**只有 `--source auto` 允许"按顺序挑第一个可用"**，且命中后最终生效的源会写进清单与 `plan --explain`；
其余任何一环失败都停下报错，点名是哪个源、为什么不可用、该装哪个 extra，**不**回落到下一个源。

首跑换真实数据不用重写 spec——建项目时把源带上即可：

```bash
python -m pip install "qianxing[a-share-akshare]"
qianxing project init ./demo --demo --source akshare \
    --instrument 000001.SZSE --start 20240101 --end 20241231
qianxing project run ./demo
qianxing project report ./demo PROJECT_RUN_ID --format html,xlsx,json
```

不装 extra、不写 `--source` 时，前两条退化成"合成样例 + 零配置"，后两条完全相同。

取数默认写内容寻址缓存（键是"源 + 标的 + 频率 + 复权 + 区间 + 请求版本"，目录可用
`QIANXING_DATA_CACHE` 改）：`--offline` 只读缓存、未命中即报错并列出缺失键，`--refresh` 强制重取，
`--no-cache` 读写都关掉。缓存不参与证据链——run 读的仍是清单里的 `source_hash` 与快照摘要。
多标的取数默认整批失败、零产物，`--allow-partial` 才落盘并逐条登记失败项。

### B6 用 `run` 简写直接跑一个策略文件

```bash
cd demo
qianxing run -f strategies/main.py --bars data/bars.json     # 不写 JSON 也能起
qianxing run -f strategies/main.py --data data               # 走数据集包（bars + snapshot + 血缘）
qianxing run -f strategies/main.py --spec input.json         # 显式版本化 spec
```

`-f` 只是把"策略文件 + 起止日期 + 初始资金 + 标的"合成为同一份 `BacktestSpec`，
再走 `run_strategy_file` 那条既有路径：`-s/--start`、`-e/--end` 收 `YYYYMMDD` 或 `YYYY-MM-DD`，
`-a/--cash` 是**大单位**（内部换算成 raw），`--currency` 在非 A 股/非样例时必填。
同一份行情下，简写与显式 spec 的 `result_hash` 必须一致——这条相等由
`python/tests/test_native_project_runs.py` 的离线首跑用例钉住，不靠文档自说。
缺省只用本地已有数据，**不联网**；要联网取数显式加 `--source`。

写进阶钩子（Tick、Order Book、Timer、成交回报与预热）不属于首跑步骤，单独成页：
[策略作者进阶：Tick、OrderBook、Timer、成交回报与预热](strategy-hooks.md)。

---

## 出错时先看这三行

| 屏幕上出现 | 它在说什么 | 下一步 |
|---|---|---|
| `INPUT_SPEC_MISSING` / `input_spec is required` | 项目没有 `input_spec`，或用 `project init` 建时没带 `--demo` | `qianxing project init ./demo --demo`，或在清单里补 `input_spec` |
| `report renderer missing` / `mismatch` | 渲染器三个落点都没命中，或命中了但身份不匹配 | 装匹配版本的 wheel，或 `--cli PATH` 指到同版本 `qx-cli`；`qianxing doctor` 的 `report_renderer` 会指出它选中了哪一个 |
| `AshareProviderError … extra=a-share-xxx` | 这个源没装，错误正文点名要装哪个 extra | `python -m pip install "qianxing[a-share-xxx]"`；不确定谁能用就先 `qianxing data sources` |

数据侧的失败类别与退出码是一张固定表（`invalid-request` 2 / `credentials` 3 / `missing-extra` 3 / `network` 4 /
`partial` 5 / `unsupported` 6 / `cache-miss` 7 / `provider` 8），`qianxing data env` 打印参与取数决策的环境变量表。
这张表只有一份：`python/qianxing_ashare/sources.py` 的 `DATA_ERROR_EXIT_CODES`，本页抄的是它的读数，
由 `python/tests/test_ashare.py` 的 `test_data_error_ladder_is_the_only_source_of_data_exit_codes` 双向钉住
（改代码或改本页都会红）；`credentials` 是保留行，当前没有任何数据源会抛出它。
其余项目命令的退出码口径：CLI 自身用法错误与被判定的失败输入走 `QianxingError` → 2，取消走 130。

## 首跑不承诺的事

远端沙箱研究（E2B / llm-sandbox / OpenSandbox / MicroSandbox）、CCXT worker、服务端部署与实盘就绪都在
首跑之外：`project init` 与首跑文档只承诺 **local `trusted_python` 后端**，`doctor` 把远端后端一律登记为
`unverified`，即使对应 extra 已安装也不说"就绪"（要跑请先读
[部署与运维](../deploy/README.md) 与[外部链路验收执行方案](外部链路验收执行方案-V1.md)）。
`project doctor --dynamic`、`project run` 执行的是本机可信 Python，不是沙箱。

## 本轮实测（2026-10-30，Windows 11 / CPython 3.13，源码树内 `python -m qianxing.cli`）

在仓库外一次性新建的临时目录里逐条执行，读数如下；`qianxing` 控制台脚本与
`python -m qianxing.cli` 共用 `cli.main`，干净 venv 装 wheel 的等价链路见[安装指南](installation.md)的 R20 验收记录。

| 命令 | 读数 |
|---|---|
| `project init <tmp>/demo --demo` | `status: created`，落 **9** 份文件；`source=sample`、`rows=600`、`instrument=SYNTHETIC.SAMPLE`、`is_sample=true` |
| `project doctor ./demo` | `status: ready`、`issues: []`、`user_code_executed: false`，退出码 0 |
| `project plan ./demo --explain` | `data_selection.configuration` 含 `data.source`（`source: manifest`、`value: sample`）、`data.cache_root`、`data.retries`，`scope: future_fetch_only` |
| `project run ./demo` | `result_hash=095d45d044ddc688`、`fills=1`、`equity_points=600`、`return_bps=-16`、`max_drawdown_bps=27`、`data_fingerprint=barframe:980739134a4bdbf3` |
| `run -f strategies/main.py --bars data/bars.json` | `result_hash=095d45d044ddc688` —— 与上一行逐字符相同 |
| `project report ./demo <run id> --format html,xlsx,json`（不带 `--cli`） | `report_renderer.origin=bundled`、`status=ready`、版本/模式目录摘要/平台三格全 `matched` → `status: succeeded`，落 `report.html` / `report.xlsx`(140,734 B) / `report.json` + 三张 SVG + `report.log` + `report-status.json`；`report_sha256=1e538ae3…`、`source_artifacts_verified_after_render: true`，退出码 0 |
| `data install --sample --target <tmp>/proj/data` | `installed SYNTHETIC.SAMPLE rows=600 source=sample`，退出码 0 |
| `data doctor`（未装任何 A 股 extra） | 逐源打印 `not-installed` + 安装命令，末行 `data.source=sample origin=default`，退出码 0 |
| `data sources` | 6 行能力矩阵（akshare / baostock / easy_tdx / mootdx / ccxt / sample） |
| `project report ./demo <run id>`（把包内副本换成一份身份过期的旧 `qx-cli.exe`） | 选中 `origin=bundled`、版本与平台 `matched`、模式目录摘要 `mismatch` → 拒绝执行且**没有**改试 PATH 上的副本；`report-status.json` 记 `reason: report_renderer_mismatch`、`report_sha256: null`，run 收据仍 `succeeded` |

倒数第二行是"wheel 自带渲染器"的正向腿：包内 `qianxing/_bin/qx-cli.exe` 由构建脚本在本轮源码上重新构建后放入，
命令里没有 `--cli`，PATH 上也没有 `qx-cli`（`which qx-cli` 为空）。最后一行是"不做静默回落"的反向取证：
故意把包内副本换成一个身份不匹配的旧二进制，命令停下而不是换源。
