# 首跑：从安装到第一份报告

这一页只回答一个问题：**装完之后，头几条命令敲什么**。它覆盖 Rust CLI、Python wheel 和本机 Web Console 三种入口，
每条命令都可以直接复制；一份命令属于哪个二进制写在行内，不做混写。能力全景与排障口径不在这里，见
[工业化易用性收口指南](工业化易用性收口指南-V1.md)；完整安装前置与坑表见[安装指南](installation.md)。

v0.4.2 回测 / Paper 正式版已发布。Python 使用 [PyPI 的 qianxing 0.4.2](https://pypi.org/project/qianxing/0.4.2/)，Rust 工程使用 [crates.io 的 qx-sdk 0.4.2](https://crates.io/crates/qx-sdk/0.4.2)，CLI 与 Web Console 从 [GitHub Release v0.4.2](https://github.com/coeasy/qianxing/releases/tag/v0.4.2) 下载。本文中的 `quickstart --profile paper` 是 tag 后加入的一键首跑入口，尚未进入 v0.4.2 CLI 或 wheel；v0.4.2 仍可使用该版本已有的 Paper 命令和控制流程。

发布后复核还发现 GitHub Release 与 PyPI 三平台同名 wheel 的 SHA-256 均不一致；Python 首跑请从 PyPI 安装，不要把 GitHub 附件 wheel 当作相同构件。差异及 CLI 构建提交见[发布证据](../maturity/evidence/release-v0.4.2-publication-2026-11-25.json)。

> 标注口径：**【已实现】**= 命令在当前工作树里可执行，且行为由常驻用例或门禁钉住；
> **进阶**= 能力存在，但不属于首跑承诺，本页只在末尾点名，不给步骤。本页不重复登记未实现的设计意图，
> 那类条目一律留在方案文档里标【设计意图，未排期】。

## 我该选哪一支

| 你要做的事 | 选这支 | 装完得到的命令 |
|---|---|---|
| 只想跑回测、看报告，不写 Python 策略 | A · Rust CLI | `qx-cli`（Windows 上 `qx-cli.exe`） |
| 用 Python 写策略、取 A 股/加密行情、把运行收进项目台账 | B · Python wheel | `qianxing` |
| 想在本机浏览器看服务并提交研究级 Bar 回测 | C · Web Console | `qx-cli console` |
| 两支都要（Python 写策略，Rust 跑服务端/worker） | B 里补一句 `--cli` | `qianxing` + `qx-cli` |

A/B 两支的**首跑承诺相同**：零配置建一个能跑的项目 → 跑一轮 → 出一份可复核的报告。差别只在数据默认值：
A 支自带一份样例 BarFrame，B 支要么用内置合成样例，要么装一个 extra 换真实源。
C 是本机 Paper/API 控制台；它需要服务端数据文件，首跑前置见下方 C 节。

CLI 还提供按角色分流的五步指南：`qx-cli guide researcher`、`qx-cli guide strategy-author`、
`qx-cli guide operator` 和 `qx-cli guide integrator`。脚本可运行 `qx-cli guide --json` 读取版本化步骤，
也可在 `--json` 后指定一个角色筛选结果。

---

## A. 只装 CLI（不需要 Python）【已实现】

最快方式是从 [GitHub 最新 Release](https://github.com/coeasy/qianxing/releases/latest) 下载对应平台的预编译 CLI，无需安装 Rust 或 Python。当前稳定版 v0.4.2 的资产名为 Windows x64 `qx-cli-v0.4.2-Windows-X64.exe`、Linux x64 `qx-cli-v0.4.2-Linux-X64`、Apple Silicon `qx-cli-v0.4.2-macOS-ARM64`；Release 同时提供 SHA-256 校验文件。下载后在文件所在目录直接运行：

```powershell
# Windows x64
Invoke-WebRequest https://github.com/coeasy/qianxing/releases/download/v0.4.2/qx-cli-v0.4.2-Windows-X64.exe -OutFile qx-cli.exe
.\qx-cli.exe quickstart my-qx --html
```

```bash
# Linux x64（Debian 12 / Ubuntu 22.04 基线）
curl -L https://github.com/coeasy/qianxing/releases/download/v0.4.2/qx-cli-v0.4.2-Linux-X64 -o qx-cli
chmod +x qx-cli
./qx-cli quickstart my-qx --html
```

```bash
# macOS Apple Silicon
curl -L https://github.com/coeasy/qianxing/releases/download/v0.4.2/qx-cli-v0.4.2-macOS-ARM64 -o qx-cli
chmod +x qx-cli
./qx-cli quickstart my-qx --html
```

要将 `qx-cli` 安装到 PATH 并由 Cargo 编译，可在仓库外固定到当前 tag：

```bash
cargo install --git https://github.com/coeasy/qianxing --tag v0.4.2 --locked qx-cli
qx-cli quickstart my-qx                          # 建项目 → 静态检查 → 回测 → 生成/核验证据包 → 安全状态
qx-cli quickstart my-rsi --strategy rsi           # 同一条链路直接运行 RSI 内置策略
qx-cli quickstart my-ashare --profile ashare     # A 股样例 → 静态检查 → 回测 → 生成/核验证据包 → 安全状态
qx-cli quickstart my-okx --profile ccxt --days 30 --timeframe 1h --html  # 真实公开行情 → 回测 → HTML/SVG 与证据包
```

`my-paper --profile paper` 一键首跑仅适用于包含 tag 后变更的当前源码或基于它构建的 CLI；v0.4.2 预编译 CLI 和 `cargo install --tag v0.4.2` 不支持此 quickstart 参数。
Binance Spot 示例需要包含本分支改动的后续 CLI；已发布的 v0.4.1 和现存 v0.4.2 tag 尚不含 `--market-type` 参数。

已在仓库内开发时可用 `cargo install --path crates/qx-cli --locked`；该方式需要完整源码和 Rust 工具链。Release 二进制没有
仓库相对路径依赖，可从任意工作目录运行。

`builtin`、`ashare` 与 `ccxt` 回测 quickstart 内部执行六步：`init → doctor → backtest → report --evidence → evidence verify-run → status`；
它只写项目目录，产物落项目内 `data/`。EvidenceBundle 校验报告引用的产物摘要、RunManifest 身份和输入数据指纹；默认不重跑策略，因此公共 CCXT 首跑不会为了验包再下载一遍行情。
`--profile paper` 则单独执行四步：`init --profile paper → doctor → Scheduler/Strategy/Paper Execution/Ledger → status`，不把 Paper 订单与历史回测报告混在同一项目链里。
任一步失败就停下，只回显**那一步**的命令原文，以退出码 2 结束：

内置策略的 Bar 回测会在运行前复用 qx-app 只读预检，显示标的、数据根数和内容指纹；预检阻断时会列出失败项，
不会开始执行该轮策略。实际回测仍会重新读取数据并做自己的完整校验。自定义策略的 warmup 由策略运行时契约检查。

`--profile` 支持 `builtin`（默认）、`ashare`、`ccxt` 与 `paper`。A 股首跑使用仓库附带的 6 根样例 Bar 和冻结规格，
完成一笔本地回测；不会连接外部行情源或发送订单。`ccxt` 首跑默认从 OKX 获取最近 30 天 BTC/USDT 永续 1h 公共 K 线，
使用市场公开默认费率并冻结成本文件，生成可复核的回测报告；它不读取密钥、不启用执行 worker，也不提交订单。
支持 `okx`、`binance` 和 `bitget` 公共行情；默认 `--market-type swap`，指定 `--exchange` 后会自动选择该交易所的 BTC/USDT 永续标的。`--market-type spot` 当前仅支持 Binance，并默认选择 BTC/USDT 现货；需要其他标的时传 `--instrument`。`--days`、`--timeframe` 可调整窗口，上限为 365 天且最多获取 100000 根 Bar。
策略默认 `ema_cross`，也可用 `--strategy` 选择单标的内置策略；用 `qx-cli builtin-strategies` 查看完整清单。
此真实行情 quickstart 需要安装 `qianxing[ccxt]` 对应的 CCXT 依赖并可访问所选交易所的公共行情 API。单独的 CCXT sandbox/实盘下单仍需凭据、权限和逐交易所外部验收。
若只安装了 Rust CLI，可在同一 Python 环境补装行情 worker：`python -m pip install "qianxing[ccxt]"`；运行时用 `QX_PYTHON` 指向该环境的解释器。
双腿套利策略需要两份行情，quickstart 会拒绝它们并提示使用 `multi-builtin` 入口。

Paper 有独立的首跑入口（当前源码 / 后续版本）：

```bash
qx-cli quickstart ./paper-demo --profile paper
```

该入口是在 v0.4.2 tag 后加入的；从当前源码构建的 CLI 会在新目录生成 `paper` 运行配置，完成静态检查、Scheduler → Strategy → Paper Execution → Ledger 主链路和状态检查。成交来自本地合成 L1 报价，
不会联网、读取交易凭据或发送实盘订单；它证明的是本机 Paper 管道可执行，不代表真实交易所撮合或收益。该流程保护已有持久账本，不支持 `--force`；
每次新的首跑请使用新的项目目录。Paper 与回测 quickstart 分开运行，避免把合成 Paper 成交混进历史回测结果。

```bash
cd my-qx
qx-cli init qianxing.runtime.json --strategy macd   # 配置 + 样例 BarFrame + README.qianxing.md
qx-cli doctor qianxing.runtime.json                 # 静态检查：不联网、不启动 worker、不下单
qx-cli backtest qianxing.runtime.json               # 跑一轮 MACD 回测，产物落 data_dir 指向的 runs/
evidence=$(qx-cli report qianxing.runtime.json --evidence | sed -n 's/^\[Report\] evidence=//p')
qx-cli evidence verify-run "$evidence"
qx-cli status qianxing.runtime.json                 # 本地配置/worker/回测结果的安全状态；不连交易所
```

上面的 Bash 命令从报告输出中提取证据包路径，`evidence verify-run` 检查 bundle、报告产物和行情输入的一致性，
不声称独立重跑策略。需要复算结果时，另行显式传 `--recompute`。

要可视化结果就在回测后加 `--html`：`qx-cli report qianxing.runtime.json --html` 写一份可离线打开的
HTML 和三张独立 SVG，`-o reports/latest.html` 改输出前缀。

`quickstart` 与逐条执行的回测链相等不是文档承诺：同一条输入下两者的 `result_hash` 必须逐字符相同，
由 `crates/qx-cli/tests/quickstart_first_run.rs` 钉成用例。落盘份数同样由用例逐个计数
（`init --strategy macd` 落 11 份（含 Grafana 面板）、首轮回测再写 6 份、EvidenceBundle 1 份，共 18 份，见 `crates/qx-cli/tests/init_onboarding_process.rs` 与 `crates/qx-cli/tests/quickstart_first_run.rs`）。`--html` 额外生成一份 HTML 和三张 SVG，共 22 份。

脚本、Python 和控制台集成可以追加 `--json-result`（当前源码构建提供；v0.4.2 预编译 CLI 尚不支持）：

```bash
qx-cli quickstart ./qianxing-demo --html --json-result
```

成功后会追加一行 `[QuickstartResult] {…}`，其 JSON 包含 `schema_version=1`、运行摘要与结果指纹、报告/证据包路径，以及证据校验状态。Python wheel 使用该合同读取 Rust CLI 结果；不要解析人类可读日志中的路径文案。Paper profile 使用相同外层合同，报告和回测字段为 `null`，并在 `paper` 对象中列出本地执行事实。该结果行不替代 `EvidenceBundle` 本身或独立复算。

---

## B. 只装 wheel（Python 策略作者）【已实现】

### B1 装

普通使用直接从 PyPI 安装当前稳定包：

```bash
python -m pip install "qianxing==0.4.2"
qianxing doctor
```

需要交易所真实公共行情时安装带 CCXT 的可选依赖：

```bash
python -m pip install "qianxing[ccxt]==0.4.2"
```

只有从 GitHub Release 下载 wheel 或测试本地构建时，才需要按文件名安装指定 wheel：

```bash
python -m pip install dist/qianxing-0.4.2-cp310-abi3-win_amd64.whl    # 按平台下载 v0.4.2 wheel 后替换文件名
python -c "import qianxing as qx; print(qx.doctor()['native_extension_compatibility'])"
```

基础包没有必装的第三方 Python 依赖。离线安装时先取得适配本机平台的 wheel，再运行 `python -m pip install --no-index <wheel 文件路径>`。
`doctor` 里要看的是三格：原生扩展是否 `matched`、用例集是否 `complete`、`report_renderer` 的
`origin` 是 `bundled` / `external` / `missing` 中的哪一个。
其中 `doctor.cli` 只检查 PATH 上可独立调用的 `qx-cli`；它显示 `unavailable` 不代表 wheel 安装不完整，也不妨碍 `qianxing quickstart`。wheel 自带的报告器单独显示在 `report_renderer`：只装 Python wheel 时应看到 `origin=bundled`、`status=ready`。只有需要在 Python 命令之外直接使用 Rust CLI 时，才需要额外安装 `qx-cli`。

wheel 由 `tools/build_python_wheel.ps1` / `.sh` 从当前工作树构建；它在打 wheel 之前先把宿主平台的
`qx-cli` 复制进包内 `qianxing/_bin/`，所以只装 wheel 的用户不需要再装 Rust 也能出报告。这个先后顺序本身
是常驻判据（`tools/check_architecture.py`），包内载荷由 `tools/check_python_wheel.py` 核对。

在源码仓库里试命令时，把 `qianxing` 换成 `python -m qianxing.cli`——两者进的是同一个 `cli.main`
（`python/pyproject.toml` 的 `[project.scripts]` 只指向它）。

### B2 零配置建一个能跑的项目

#### 离线样例（默认）

把“建项目 → 预检 → 回测 → 生成并核验报告”合成一步：

```bash
qianxing quickstart ./demo
qianxing quickstart ./demo-formats --format html,xlsx,json --report-output reports/first-run.html
```

它会在指定目录创建 600 根内置合成 Bar 样例，复用下方相同的项目快照、预检、Rust 引擎、运行回执和报告校验流程；
成功输出会包含 `preflight`、`project_run_id`、`artifacts_verified` 和报告路径。样例数据会显式标记
`source=sample`、`is_sample=true`，命令不会联网取行情，也不代表真实行情或策略收益。目标目录若已有项目，命令会停止并保留原文件；
可改用新目录，或继续按下方 `project init` / `project run` / `project report` 分步操作。`--cli` 可显式指定报告渲染器，
默认优先使用 wheel 内的 `qx-cli`，再查找 PATH。

Python sample quickstart 会额外生成并离线核验 `run_evidence.bundle_path` 指向的 EvidenceBundle；校验输入身份和已登记文件摘要，不自动重跑策略。分步项目报告可用 `qianxing project report ./demo RUN_ID --evidence` 生成并核验同类证据。

#### 本地 Paper 生命周期（当前源码 / 后续版本）

`--profile paper` 一键首跑尚未包含在 PyPI `qianxing==0.4.2` 中。使用当前源码构建并安装 wheel 后，Python 入口复用 wheel 内匹配版本的 Rust CLI：

```bash
qianxing quickstart ./paper-demo --profile paper
```

返回 JSON 的 `paper` 对象包含本轮新订单与 Ledger 分录数，`data` 明确标记报价为 `local_synthetic_l1`、没有网络、没有读取凭据、没有实盘订单。
目标目录必须是新的，Paper 的持久订单与账本不会由 quickstart 自动清空；此入口不需要安装 `ccxt` extra。

```bash
qianxing quickstart ./demo-custom --rows 120 --cli qx-cli
```

#### 真实公共行情（只读 CCXT）

安装带 CCXT 可选依赖的 wheel 后，使用下列命令下载公开行情并生成回测报告。

```bash
python -m pip install "qianxing[ccxt]==0.4.2"
qianxing quickstart ./okx-btc --profile ccxt --exchange okx --days 30 --timeframe 1h
qianxing quickstart ./bitget-btc --profile ccxt --exchange bitget --days 52 --timeframe 15m
```

省略 `--instrument` 时，永续会按交易所自动选用 `BTC/USDT:USDT.<EXCHANGE>`，Binance Spot 使用 `BTC/USDT.BINANCE`；策略默认 `ema_cross`，可用
`--strategy` 指定单标的内置策略。该路径调用公共行情 API，不读取账户凭据、不启用执行 worker、不提交订单；结果中的
`report_html_path` 给出报告路径，`run_evidence.bundle_path` 给出证据包路径，`quickstart_output` 保留完整 Rust CLI 首跑日志。
`run_evidence.integrity_verified` 与 `input_identity_recomputed` 表示工件摘要和行情输入身份已离线核验；
`result_recomputed` 明确说明没有额外重跑策略。默认只生成 HTML，因此 `--format` 仅接受 `html`。
窗口限制、Bar 数量上限、缺失数据校验和
费用来源沿用 Rust CLI 的 CCXT 首跑实现，详见上方 A 节。

`--market-type spot` 是本分支新增、尚未发布的 Binance 现货匿名公共行情：它固定使用 `https://data-api.binance.vision/api/v3`，并只加载 Spot 市场；此端点用于 Binance 的[公开 Spot REST 行情接口](https://developers.binance.com/en/docs/products/spot/rest-api)。生成的配置没有 API 凭据，行情 worker 也会拒绝私有、下单、账户和 WebSocket 操作。OKX、Bitget 与 Binance 永续的既有默认仍为 `swap`。

若从包含该改动的源码分支构建 wheel，可这样运行 Binance Spot 首跑；现有 v0.4.1 wheel 和 v0.4.2 tag 尚不支持：

```bash
qianxing quickstart ./binance-spot --profile ccxt --exchange binance --market-type spot --days 30 --timeframe 1h
```

### B3 分步创建与运行项目

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

`--demo` 之外还有 `--source/--instrument/--start/--end/--cash/--currency/--frequency/--adjustment`（见 B6）、
`--strategy-file` / `--input-spec`（把已有文件接进项目）、`--rows`（只作用于合成样例）。
合成样例的策略允许小数数量，因此初始资金低于一整单位的价格时仍可产生示例交易。
生成的策略文件包含 `QUANTITY_STEP_RAW`；接入真实标的时应根据该市场的数量精度、最小交易数量和整手规则调整，并由交易规则校验。

对已有项目使用 `project init --demo --force` 会替换生成的策略、数据包、输入配置和项目清单；
请先保存自己的策略修改。数据包会先生成并校验，数据、输入、策略和清单安装期间遇到普通文件错误时
会尝试恢复旧文件；如果自动恢复失败，错误消息会给出保留备份和 `recovery.json` 对照表的目录。
进程中断后，初始化和项目读取会拒绝使用包含 `.qianxing-init-*` 暂存目录的项目，避免把
可能混合了新旧版本的文件当作完整项目。确保原初始化进程已停止后，运行：

```bash
qianxing project init-recovery-info ./demo
```

该命令只列出原文件、备份和当前文件的存在状态，不自动恢复或删除。请先复制整个项目和暂存目录，
再结合 `recovery.json` 和备份核对并手动恢复；暂存目录没有映射时也应先人工检查。确认数据、策略、
输入及清单一致并另存备份后，移走暂存目录，重新运行 `project doctor`。此检查不是并发锁，
不要在初始化仍在运行时同时恢复或修改项目。

### B4 先看，再跑

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

### B5 出报告

```bash
qianxing project report ./demo                                      # 最近一次运行的 HTML（须成功）
qianxing project report ./demo PROJECT_RUN_ID                       # 指定某次运行
qianxing project report ./demo PROJECT_RUN_ID --format html,xlsx,json
```

省略编号时，按记录的开始时间选择最近一次运行；若该运行未成功、记录损坏或开始时间并列，命令会提示检查记录并显式指定编号，不会回退到旧结果。并发启动新运行时，选择以读取记录时的状态为准。报告结果中包含所选 `project_run_id`。`PROJECT_RUN_ID` 可从 `qianxing project runs ./demo` 获取。渲染器按**单链**选中即用，不做静默替换：
`--cli PATH` > wheel 内置的 `qianxing/_bin/qx-cli` > PATH 上的 `qx-cli`；三个落点都不存在就报
`missing` 并停下，不会替你找一个。版本/模式目录摘要/平台任一不匹配同理——诊断里直接给 `--cli PATH` 的修法，
并写明 `No fallback was attempted`。

报告失败只失败在报告这一格：`report-status.json` 记 `status: failed` + `report_renderer_<状态>` +
`report_sha256: null`，而 run 收据保持它自己的终态，回测成功不会因为报告没出而被改写。
产物字节先复核再渲染，落盘的 HTML/XLSX/JSON 走同一套 bundle 发布事务（见
[回测报告布局](backtest-report-layout.md)）。

### B6 换成真实数据【已实现，A 股取数需装 extra】

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

### B7 用 `run` 简写直接跑一个策略文件

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

## C. 本机 Web Console（研究 / Paper）

`qx-cli console` 在回环地址启动同源控制台和 API；默认监听 `127.0.0.1:18091`，只用于本机研究与 Paper。页面默认 Bar 规格会读取 `sample.json`，因此先从仓库根目录把教学 BarFrame 放进 API 数据根目录：

```powershell
New-Item -ItemType Directory -Force .qianxing/api-data | Out-Null
Copy-Item deploy/qianxing.bar-frame.example.json .qianxing/api-data/sample.json
```

Linux/macOS：

```bash
mkdir -p .qianxing/api-data
cp deploy/qianxing.bar-frame.example.json .qianxing/api-data/sample.json
```

如果设置了 `QX_API_DATA_ROOT`，请把 `sample.json` 放到那个目录；未设置时服务按启动工作目录使用 `.qianxing/api-data`。仓库外使用预编译 CLI 时，样例 BarFrame 可从 [v0.4.2 教学 fixture](https://raw.githubusercontent.com/coeasy/qianxing/v0.4.2/deploy/qianxing.bar-frame.example.json) 下载为 `sample.json`，并保存到刚创建的 `.qianxing/api-data/`。

不含源码仓库时，可用以下命令下载样例：

```powershell
Invoke-WebRequest -Uri https://raw.githubusercontent.com/coeasy/qianxing/v0.4.2/deploy/qianxing.bar-frame.example.json -OutFile .qianxing/api-data/sample.json
```

```bash
curl -L https://raw.githubusercontent.com/coeasy/qianxing/v0.4.2/deploy/qianxing.bar-frame.example.json -o .qianxing/api-data/sample.json
```

源码 checkout 已包含 `web/console/`。使用预编译 CLI 时，先把对应版本的 [Web Console 发布包](https://github.com/coeasy/qianxing/releases/download/v0.4.2/qianxing-web-console-v0.4.2.tar.gz) 解压到当前工作目录的 `web/console/`（归档里的 `qianxing-web-console/` 目录层级需去掉），然后创建本机 Paper 配置并启动：

```powershell
New-Item -ItemType Directory -Force web/console | Out-Null
Invoke-WebRequest -Uri https://github.com/coeasy/qianxing/releases/download/v0.4.2/qianxing-web-console-v0.4.2.tar.gz -OutFile qianxing-web-console.tar.gz
tar -xzf qianxing-web-console.tar.gz -C web/console --strip-components=1
qx-cli console --init qianxing.runtime.console.json
qx-cli console qianxing.runtime.console.json
```

Linux/macOS 的发布包安装方式：

```bash
mkdir -p web/console
curl -L https://github.com/coeasy/qianxing/releases/download/v0.4.2/qianxing-web-console-v0.4.2.tar.gz -o qianxing-web-console.tar.gz
tar -xzf qianxing-web-console.tar.gz -C web/console --strip-components=1
qx-cli console --init qianxing.runtime.console.json
qx-cli console qianxing.runtime.console.json
```

在仓库 checkout 中，也可直接使用 `deploy/qianxing.runtime.console.example.json` 替代 `--init` 生成的配置。

服务会在终端打印一次性本机入口 URL；打开后点击“连接”，在“Bar 研究工作流”先点“只校验数据”，通过后点“回测并复核”。样例是合成教学数据，不联网、不代表真实行情或策略收益，也不会读取凭据或发送订单。CLI 与静态包必须使用同一版本。v0.4.2 中选择策略会自动更新运行 ID，手工修改其他输入后点击“生成新运行 ID”。

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
