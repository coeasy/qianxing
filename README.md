# 牵星 Qianxing

**分级校准，量天定位。**

确定性量化交易与回测内核 · 多交易所 · 多数据源 · 多账户 · 多策略 · 多语言策略

---

## 什么是牵星

**牵星**取自明代"过洋牵星术"——以分级量具（牵星板）测星高以定纬度。
它的方法论是：**先把数据分级，再把模型校准，然后在不确定中定量定位。**

这正是回测真实性的来源。牵星不追求更复杂的滑点公式，而是让每个撮合模型先回答
**"我不知道什么"**：L2/L3 沿真实深度行走，L1 用概率模型，Bar 只重构有限路径——
**数据档位不足时直接拒绝运行**，绝不从 K 线里读出不存在的盘口，也不静默换成更宽松的假设。

## 为什么用它

- **回测与实盘共享同一规则内核** —— 只替换数据、事件驱动、提交与反馈，策略代码零改动
- **多交易所是身份问题，不是连接问题** —— `CanonicalProduct` / `InstrumentId`（qx-core）与 `DataSourceId`（qx-guanxing）分层分离，绝不相互覆盖
- **多数据源固定主源** —— 备源只做回填与离群校验；四级存储 + 机器可读质量门
- **多账户是结算边界，不是资金字段** —— 账户 ID 贯穿全链，双树风控
- **多策略是治理，不是多线程** —— 隔离 + 净额求解 + 显式优先级 + 归因
- **插件只做启动期清单注册，不做运行时热插拔** —— manifest 声明向哪个扩展点贡献什么，Registry 负责校验、冲突检测与依赖求解并产出静态装配计划；内核组件仍由编译期决定，可替换性以不破坏确定性为上限

## 模块

| 模块 | 职责 |
|---|---|
| `qx-core` **牵星** | 确定性内核：时钟、因果事件队列、事件溯源、重放校验，以及 `VenueId` / `InstrumentId` / `MarketId` / `CanonicalProduct` 身份契约；内含 **分野** `fenye` 模块（合约规格与市场状态分离、符号映射只增不覆盖） |
| `qx-guanxing` **观星** | 数据平面：`DataSourceId`、质量门、标准化、`as_of()` point-in-time 可见性 |
| `qx-data` | 多资产数据基础设施：统一市场数据契约、目录、摄取与增量管道（提供方不进内核） |
| `qx-xingban` **星板** | Bar/L1 Tick/L2 订单簿撮合与仿真：成本、延迟、保证金、因果回测 |
| `qx-zhenlu` **针路** | 执行与路由：风控门禁、OMS、路由决策 |
| `qx-risk` | 风控规则集：`RiskRule`（禁空 / 最大数量 / 最大名义）与保守默认规则集版本 |
| `qx-genglu` **更路** | 审计与对账：绩效指标、归因、订单对账 |
| `qx-plugin` **卯眼/榫头** | 能力清单注册表：manifest schema/哈希校验与 Ed25519 签名、扩展点贡献声明、独占冲突检测、依赖求解、Profile/Bundle/Patch 静态装配计划（不含运行时动态加载） |
| `qx-factor` | 因子与特征：版本、PIT 工件、分析报告、候选策略绑定 |
| `qx-protocol` | 账户协议：Canonical Snapshot、Diff、QIFI 兼容边界 |
| `qx-provider` | 数据提供方：能力矩阵、稳定选择、主备故障切换 |
| `qx-scheduler` | 调度契约：JobSpec、依赖、交易日历、幂等与重试 |
| `qx-control` | 控制面：权限、审计命令、事件订阅游标 |
| `qx-api` | 框架无关的本地查询、控制命令、事件流与 WebSocket 边界；写操作只经 `ControlPlane` |
| `qx-datastruct` | 列式 BarFrame、PIT 视图、JSON/Arrow C Data Interface |
| `qx-adapter` | REST/TLS/WebSocket 传输与 Venue/Provider 适配边界；含 Binance Spot REST/L1 行情基线 |
| `qx-runtime` | 运行时拓扑配置、worker 监督、停机信号与健康状态 |
| `qx-orchestrator` | 运行编排：把已校验的 `RuntimeConfig` 变成 worker 启动计划并管理子进程生命周期；不执行策略、不下单 |
| `qx-storage` | 文件/分段 EventLog、控制面、队列、快照、审计以及 SQLite/PostgreSQL 事务后端 |
| `qx-strategy` | Rust Strategy API、上下文/事件/多订单意图和原生策略 SDK |
| `qx-execution` | Venue 回报统一归约、SubmitOrder 执行副作用边界，以及执行事实/订单/风控/行情/对账/Venue 的稳定端口（V10 P2a 由独立端口 crate 并入） |
| `python/qianxing_ccxt` | 公共 CCXT 多交易所 REST 数据/交易连接层；CCXT Pro 仅保留后续扩展接口 |
| `qx-python` | PyO3 原生扩展、Arrow C Data Interface capsule 协议 |
| `cpp/` | C++ Strategy API v1 稳定 C ABI、CMake 示例 |
| `qx-cli` | 单一 CLI binary：命令分派、worker 装配、回测与运维命令，外加进程内确定性自校验 |

`qx-cli` 是单个 binary（决策：不为拆进程而拆 crate），内部按职责分文件：命令语法与命令表只有一份，在 `cli_args.rs` 由 clap 派生（V10 P2b，旧手写字符串解析已整体删除、不留双轨），`cli.rs` 保留对 `Command` 的一次显式 `match`，未识别的命令或未知参数打印 `未知命令或未知参数: <x>` 并以退出码 2 fail closed，`tools/check_architecture.py` 校验「clap 命令表 ≡ `cli.rs` 分支集合 ≡ help 印出的入口」；`worker_entry.rs` 用类型系统里的 `WorkerRole::is_venue_role()` 加一张 `VenueEntry` 登记表同时服务 `ccxt-worker` 与 `binance-worker`，新增角色只需在登记表上补一条 Venue 绑定判定；跨语言子进程的 Python 解释器统一由 `QX_PYTHON` 解析（缺省 `python`）。

## 安装

本仓库不发布到 PyPI / crates.io，三条路径都从源码装。下面每条命令都是本轮（2026-09-26）在本机实测过的，
实测值写在每条路径的最后一行，日志在 `%TEMP%/qx_v12p2/logs/`（对应数字见「当前状态」）。

### A. 只装 CLI —— 最短路径，不需要 Python

前置只有一样：Rust 工具链（本轮实测 `cargo 1.98.1` / `rustc 1.98.1`）。在仓库根目录：

```bash
cargo install --path crates/qx-cli --locked      # 完全离线的机器再加 --offline
```

得到 `qx-cli`（Windows 上是 `qx-cli.exe`），落在 `~/.cargo/bin`；把该目录加进 PATH 之后，**在任意目录**
都能开一个自包含项目并跑通回测，不依赖仓库里的任何相对路径：

```bash
mkdir my-qx && cd my-qx
qx-cli init qianxing.runtime.json --strategy macd   # 生成配置 + 样例 BarFrame + README.qianxing.md
qx-cli doctor qianxing.runtime.json                 # 静态检查：不联网、不启动 worker、不下单
qx-cli backtest qianxing.runtime.json               # 真跑一轮 MACD 回测，产物落 data/<root>/runs/
qx-cli report qianxing.runtime.json                 # 读回最新一份摘要
qx-cli status qianxing.runtime.json                 # 本地配置/worker/回测结果的安全状态；不连交易所
qx-cli help                                         # 全部入口清单
```

实测：`cargo install` 用时 2m10s；在仓库外的临时目录里 `help` / `init` / `doctor` / `backtest` / `report` /
`status` 六条全部退出码 0，回测写出 summary / equity.csv / fills.csv 与 RunManifest，
`result_hash=26fdd6b52d020700`。纯回测与 Paper 链路只有这一个 binary —— 只有跨语言策略 worker 与 CCXT
worker 才需要 Python（见 C）。

### B. 装开发 / 发布环境 —— 跑全部八道门禁

```bash
build.bat          # Windows，cmd.exe 里跑
bash build.sh      # Linux / macOS
```

八步依次是格式检查、release 构建、Rust 测试、clippy、Python/JSON 边界测试、核心语义自校验、CLI 全链路
与生态冒烟、运行时拓扑校验；两份脚本被门禁按步骤名与命令多重集逐项断言同序同条。前置是 Python 3.10+
（本轮实测 3.12.13）且能 `import tzdata`。第 `[0/8]` 步自己按 `QX_PYTHON` → 仓库 venv → PATH 挑一个**真能
打印版本号**的解释器，并把它导出成 `QX_PYTHON` 交给后面由 Rust 起的子进程（V12 §19 #133），所以不需要
用户预先设环境变量。仓库 venv 离线重建：

```bash
cd python && uv venv .venv --python 3.12 && uv pip install tzdata
```

实测：`build.bat` 八步全过、`BUILD_BAT_EXIT=0`，其中 `[3/8]` 是 92 个 `test result:` 段全 ok / 843 passed /
0 failed。

### C. 装 Python 侧 —— 跨语言策略与 CCXT worker

wheel 带的是 `qx-python` 编译出的原生扩展（Windows `.pyd` / Linux `.so`）加四个纯 Python 包，两条入口
分平台。**任何 `cargo` 之前**先探测解释器有没有 pip，缺 pip 即退出码 1 并印两条修法。

```bash
# Windows：默认执行策略是 Restricted，直接跑 .ps1 会以 SecurityError 退出，所以入口必须显式带 Bypass
powershell -NoProfile -ExecutionPolicy Bypass -File tools/build_python_wheel.ps1 -Python python\.venv\Scripts\python.exe
# Linux / macOS（PYTHON 指向你那个能跑的解释器，缺省会落到 PATH 上的 python3）
PYTHON=python/.venv/bin/python bash tools/build_python_wheel.sh
```

装进一个干净 venv 并复核原生扩展确实可用：

```bash
uv venv .venv-qx --python 3.12
uv pip install --offline --no-deps dist/qianxing_bridge-0.1.0-cp312-cp312-win_amd64.whl
uv pip install --offline tzdata          # Windows 必需：qianxing_ashare 导入时即解析 Asia/Shanghai
python -c "import qianxing_bridge.native as n; print(n.available())"
```

`--no-deps` 是诚实口径：wheel 自身只带四个包与原生扩展，`ccxt` 只在真的跑 CCXT worker 时才需要（运行时
才 import），装它请走你自己的索引或本地缓存。实测：本轮 wheel 216439 字节，内嵌 `_qianxing_native.pyd`
的 md5 与当轮 `target/release/_qianxing_native.dll` 逐字节相同；干净 venv 里四个包全部导入成功，
`native.available() → True`，衍生品三字段 `cross/hedge/3` 往返一致，`margin_mode="weird"` 仍按契约抛
`ValueError`。

### 装不上时的四个坑（都是本机踩过的）

| 症状 | 真正的原因 | 修法 |
|---|---|---|
| `[0/8]` 报「找不到可用的 Python 3 解释器」，或 `[3/8]` 恰好 2 条 Python 桥用例失败 | Windows 上 PATH 里的 `python` 是 Microsoft Store 占位桩：退出码 0、什么都不打印 | 按 `[0/8]` 印出的两条修法之一：建仓库 venv，或 `set QX_PYTHON=<完整绝对路径>`（必须是文件路径，写成目录会让整树测试以 `TEST_EXIT=101` 崩） |
| 跑 `.ps1` 入口直接 `SecurityError / UnauthorizedAccess`，一行脚本都没执行 | Windows 客户端默认执行策略是 Restricted，未签名脚本一律拒 | 用上面写着的形式：`-ExecutionPolicy Bypass -File`；README 里每条 PowerShell 入口都由门禁核对带着 Bypass |
| `[5/8]` 或 A 股相关用例 `ZoneInfoNotFoundError` | Windows 没有系统 IANA 时区库，`tzdata` 是 `python/pyproject.toml` 里声明的平台依赖 | `python -m pip install tzdata`（或 `uv pip install tzdata`） |
| 改了 `build.bat` 之后 `[1/8]` 之前就炸，报 `\'不是内部或外部命令\'` | 用 `sed -i`/MSYS 工具编辑会把 CRLF 抹成 LF，cmd.exe 读不了带中文的 LF 批处理 | 归一化回 CRLF，且按字节做（`raw.replace(b"\r\n", b"\n").replace(b"\n", b"\r\n")`）；门禁会数孤立 LF |

## 快速开始

下面这段是可复制的常用链路示例，写法是在**源码仓库里**直接用 cargo 跑；已经按上面「安装 A」装好的人会
得到同名 binary，把 `cargo run -p qx-cli -- ` 换成 `qx-cli ` 即可，参数与行为完全一致（同一份 clap 命令表）。
参数与入口的权威来源是 `cargo run -p qx-cli -- help`
（本轮实测 41 个顶层入口、`deploy/` 下 52 份示例配置），完整使用口径见
[工业化易用性收口指南](docs/工业化易用性收口指南-V1.md)。

```bash
# 构建
cargo build --release

# 运行端到端演示（含确定性自校验）
cargo run -p qx-cli --release

# 纸面交易验收
cargo run -p qx-cli --release -- paper
# Binance 单轮对账：本地来源是运行时配置指向的账本，远端来源是该 worker 的交易所账户
cargo run -p qx-cli --release -- reconcile deploy/qianxing.runtime.production.example.json reconciler-main
# V5.1 生态层验收
cargo run -p qx-cli --release -- ecosystem
# 一条命令验收 Scheduler → Strategy → Paper Execution → Ledger
cargo run -p qx-cli --release -- paper-e2e deploy/qianxing.runtime.paper-strategy.example.json

# 构建 Python wheel（包含 qx-python 原生扩展；Windows PowerShell）
# 默认执行策略是 Restricted，直接跑 .ps1 会以 SecurityError 退出，所以入口必须显式带 Bypass
powershell -NoProfile -ExecutionPolicy Bypass -File tools/build_python_wheel.ps1 -Python python\.venv\Scripts\python.exe
# Linux/macOS：
bash tools/build_python_wheel.sh

# 全量测试
cargo test --workspace

# 架构不变量自检（V9 收口：单一分派点、无空风控门、回测装配唯一、能力矩阵证据、行数棘轮）
python3 tools/check_architecture.py

# 工业化统一入口：初始化、回测、Paper 主链路和实盘前检查
cargo run -p qx-cli -- help
cargo run -p qx-cli -- init qianxing.runtime.json
# 一步生成绑定内置 MACD 和样例数据的可回测项目
cargo run -p qx-cli -- init qianxing.runtime.json --strategy macd
# 一次检查配置、路径和策略输入；不连接交易所、不发送订单
cargo run -p qx-cli -- doctor qianxing.runtime.json
cargo run -p qx-cli -- doctor qianxing.runtime.json --json
# 查看有效配置摘要；脚本需要 JSON 时加 --json
cargo run -p qx-cli -- config explain qianxing.runtime.json
cargo run -p qx-cli -- config explain qianxing.runtime.json --json
cargo run -p qx-cli -- config fingerprint qianxing.runtime.json
cargo run -p qx-cli -- backtest
cargo run -p qx-cli -- run paper
cargo run -p qx-cli -- status qianxing.runtime.json
cargo run -p qx-cli -- report qianxing.runtime.json
cargo run -p qx-cli -- report qianxing.runtime.json --json
cargo run -p qx-cli -- builtin-strategies
cargo run -p qx-cli -- strategy list
cargo run -p qx-cli -- strategy init macd qianxing.strategy.macd.json
cargo run -p qx-cli -- strategy backtest qianxing.strategy.macd.json deploy/qianxing.bar-frame.example.json
cargo run -p qx-cli -- backtest builtin sma_cross deploy/qianxing.bar-frame.example.json
cargo run -p qx-cli -- backtest multi-builtin spot_futures_arbitrage deploy/qianxing.bar-frame.pairs-primary.example.json deploy/qianxing.bar-frame.pairs-reference.example.json --quantity 2
cargo run -p qx-cli -- fast-backtest deploy/qianxing.fast-backtest.example.json
cargo run -p qx-cli -- backtest deploy/qianxing.runtime.builtin-strategy.example.json deploy/qianxing.bar-frame.example.json
cargo run -p qx-cli -- dataset-bundle deploy/qianxing.dataset-bundle.example.json data/datasets
cargo run -p qx-cli -- dataset-bundle deploy/qianxing.dataset-bundle.bar-frame.example.json data/datasets deploy/qianxing.bar-frame.example.json
cargo run -p qx-cli -- runtime-check deploy/qianxing.runtime.ccxt.example.json
cargo run -p qx-cli -- runtime-check deploy/qianxing.runtime.multi-venue-arbitrage.example.json
cargo run -p qx-cli -- runtime-check deploy/qianxing.runtime.example.json --json
cargo run -p qx-cli -- live-check deploy/qianxing.runtime.production.example.json --json
cargo run -p qx-cli -- paper-check deploy/qianxing.runtime.paper-strategy.example.json
cargo run -p qx-cli -- live-check deploy/qianxing.runtime.production.example.json

# 内置 Rust、Python/C++ JSONL 策略直接复用 Bar 回测引擎；CCXT 实时模式会持续维护闭合 BarFrame 并按 digest 触发策略
cargo run -p qx-cli -- backtest deploy/qianxing.runtime.strategy-backtest.example.json deploy/qianxing.bar-frame.example.json

# backtest 会在运行时 data_dir/runs 下生成 summary.json、equity.csv、fills.csv，
# 并与同一回测的 RunManifest 使用相同前缀，便于归档和二次分析

# 校验运行时拓扑配置，并启动 paper API（默认示例配置）
cargo run -p qx-cli --release -- runtime-check deploy/qianxing.runtime.example.json
cargo run -p qx-cli --release -- serve deploy/qianxing.runtime.example.json
# serve 暴露的 17 条 HTTP 路由与 WebSocket 事件流逐条列在
# deploy/README.md 的「HTTP 读面与控制面路由」一节（含鉴权、游标与限流边界）
# 以独立进程启动已配置的 Binance 行情/用户流/执行/对账 worker
cargo run -p qx-cli --release -- binance-worker deploy/qianxing.runtime.example.json <worker-id>
# 跨平台监督器：启动拓扑中全部受管 worker，任一异常退出则停止其余 worker
cargo run -p qx-cli --release -- supervise deploy/qianxing.runtime.example.json
# 执行/对账 worker 单轮验收：
cargo run -p qx-cli --release -- binance-worker deploy/qianxing.runtime.example.json <worker-id> --once
# 通过审计后的 SubmitOrder 命令执行（示例默认为 dry_run）
cargo run -p qx-cli --release -- binance-submit-order deploy/qianxing.runtime.production.example.json binance-user-main deploy/qianxing.submit-order.example.json
# 本地 Paper 控制面→队列→成交→Ledger 闭环（不连接网络）
cargo run -p qx-cli --release -- paper-submit-order deploy/qianxing.runtime.paper-strategy.example.json deploy/qianxing.paper-submit-order.example.json
```

### 外部链路验收（缺凭据即 fail closed）

```bash
# Binance Spot 测试网络验收：runtime-check/live-check 必须先通过，
# 缺少 QX_BINANCE_TESTNET_API_KEY / QX_BINANCE_TESTNET_API_SECRET 时以退出码 3 跳过交易所步骤
python tools/binance_testnet_acceptance.py --binary target/release/qx-cli
# CI 使用 --allow-skip，让离线半边始终执行
python tools/binance_testnet_acceptance.py --binary target/release/qx-cli --allow-skip

# C++ 外部策略进程：分别用 JSON 与列式两种协议跑一遍共享内存环契约
python tools/verify_cpp_worker.py build/cpp/Release/qianxing_strategy_jsonl.exe shared_memory_json
python tools/verify_cpp_worker.py build/cpp/Release/qianxing_strategy_jsonl.exe shared_memory_columnar
```

`deploy/qianxing.runtime.binance-testnet.example.json` 是测试网络拓扑模板：行情走
`wss://stream.testnet.binance.vision/ws`，执行与对账 worker 默认指向 `paper` 之外的
testnet 账户，凭据只从环境变量读取，配置文件中不落任何密钥。

Windows 下用 `build.bat`（cmd.exe 里跑；本轮实测把全部 8 步端到端跑通了一次，见 V12 §19.1），POSIX 下用 `bash build.sh`。两份脚本跑**同一组 8 步门禁**，
门禁按命令多重集与步骤名逐项断言两者同序同条（`build_script_parity_check`）。
第 `[0/8]` 步先选出一个真的可用的 Python 解释器，候选顺序是 `QX_PYTHON` →
仓库 venv（Windows `python\.venv\Scripts\python.exe`，POSIX `python/.venv/bin/python`）→ PATH 上的 `python`；
判定不信退出码（WindowsApps 的 `python` 存根退出码为 0 却什么都不打印），要求候选把版本号印出来，
再要求它能 `import tzdata`（`python/pyproject.toml` 里声明的 Windows 依赖）。三者都不合格时以退出码 1
停下并印出候选清单与两条修法。挑中之后**必须把它外传成 `QX_PYTHON`**（`build.bat` 用
`for %%I in ("%QX_PY%") do set "QX_PYTHON=%%~fI"`，`build.sh` 用 `export QX_PYTHON`）：`[3/8]` 的两条 Python
桥契约用例与 `[7/8]` 的策略 worker 是 Rust 去起 Python，而 Rust 只读 `QX_PYTHON` 这一个变量
（`crates/qx-cli/src/main.rs` 的 `python_interpreter_origin()`），脚本自己的 `QX_PY` 到不了 cargo 的环境；
漏掉这一步时 `[0/8]` 打印"已选中"却仍在 `[3/8]` 以 `202 passed; 2 failed` 收场（V12 §19 #133）。
`build_script_parity_check` 逐脚本核对"导出那一行存在且早于 `[1/8]`"，并且只认命令、不认报错提示里
那句教用户怎么设 `QX_PYTHON` 的 `echo`（§19 #136）。`.gitattributes` 把 `*.bat`/`*.cmd` 钉成 CRLF、`*.sh` 钉成 LF：
带 UTF-8 中文的 LF 版批处理 cmd.exe 读不了，会在文件中途以 `\'不是内部或外部命令\'` 死掉
（`windows_batch_parse_check` 逐文件核对行尾与"每行以 ASCII 字节结尾"）。

上面那两条 wheel 构建命令同样有前置条件：`tools/build_python_wheel.ps1` / `.sh` 最后一步用的是 `pip wheel`，
所以它们在任何 `cargo` 之前先跑 `-m pip --version` 探测，解释器没有 pip 就以退出码 1 停下并印两条修法
（装 pip，或离线走 `uv build --wheel --offline --no-build-isolation`）—— 由 `wheel_builder_check` 按
"探测必须早于 `pip wheel`"逐脚本核对。`.ps1` 整体是 ASCII-only：Windows PowerShell 5.1 按 ANSI 码页读无 BOM 文件，
行尾的中文会让下一行代码整行消失。第三条前置是**执行策略**：Windows 客户端默认策略禁止运行未签名脚本，
`powershell -File tools/build_python_wheel.ps1` 与 `./tools/build_python_wheel.ps1` 都会在一行代码都不执行的
情况下以 `UnauthorizedAccess` 退出，所以上面的入口写成 `-ExecutionPolicy Bypass -File`（同一条判据核对
"README 里每一行 PowerShell 入口都带着 Bypass"）。安装包产物落在 `dist/`，而 `/dist/` 在 `.gitignore` 里，
故它是本地产物、不随仓库分发。

实现状态与未完成外部边界见：[自研量化框架审计与重构方案 V12](docs/自研量化框架审计与重构方案-V12.md)
（当轮实测的功能盘点、架构事实、不合理清单与分阶段方案）与逐轮收口记录
[自研量化框架重构方案 V11](docs/自研量化框架重构方案-V11.md)（§10–§39 保留为历史日志，其 §1–§9 的结论以 V12 为准）。
V11 每轮都带 `file:line` 与当轮门禁日志；V12 §11–§14 的数字来自 `HEAD = 72c347e` 那一轮，
§15 记录上游 4 个提交合流后的同口径重测，§16 是三遍连通性清点的收口（含 §15→§16 之间用例计数变化的
逐名归零），§17 是第四遍：构建与安装面清点（`build.bat` / `build.sh` / `.gitattributes` / 解释器探测）。
下面这组"当前状态"数字是 §17 那一轮重测的。

能力证据按 `implementation` / `code_tested` / `sandbox_tested` / `production_approved` 四档分级（见下方文档地图
里的 `maturity/capabilities.yaml`）。默认 `single_node` 使用 SQLite/Files；PostgreSQL、NATS、真实交易所沙盒和券商柜台不会因为代码或 feature 存在而被标记为生产批准。

## 持续集成作业

`.github/workflows/ci.yml` 把每条链路都钉在独立的作业上，任何一条断裂都会红：

| 作业 | 覆盖链路 |
| --- | --- |
| `rust-core` | 全 workspace fmt/clippy/test、独立确定性自校验、架构不变量与能力矩阵证据门禁（`tools/check_architecture.py`）、Python 适配层契约 |
| `python-wheel` | Linux/Windows × Python 3.10/3.12/3.13 wheel 构建、装入干净环境后原生扩展可用且指纹与纯 Python 一致 |
| `feature-matrix` | `qx-cli` 在 sqlite / postgres / nats / postgres+nats 四种特性组合下可编译可测试 |
| `service-backends` | `postgres:16` 与 `nats -js` 服务容器下跑 Outbox 租约/围栏/重试与 JetStream 一发一收幂等契约（`--ignored`） |
| `runtime-contracts` | 全部非生产 runtime 示例的 `runtime-check`、`live-check` 失败即闭、Paper 进程边界 E2E |
| `cpp-sdk` | Ubuntu/Windows/macOS 三平台 C++ SDK 构建、ring smoke、JSONL 契约、JSON 与列式两种共享内存协议 |
| `venue-acceptance` | Binance 测试网络验收；无凭据只做离线 fail-closed 半边（`--allow-skip`） |

## 文档地图

仓库里只保留**当前仍然正确**的文档；历史方案与审计（V1–V10 各代计划、Barter 对齐稿、可视化终态稿、
产品化路线图、差距清单、release note）已删除，需要回溯时在 git 历史里按文件名取。

| 文档 | 什么时候读 |
|---|---|
| [自研量化框架审计与重构方案 V12](docs/自研量化框架审计与重构方案-V12.md) | 想知道**这一轮实测**的项目功能、依赖形状、核心功能完成度判定，以及下一版重构怎么排（含 7 条待用户拍板的决策） |
| [自研量化框架重构方案 V11](docs/自研量化框架重构方案-V11.md) | 想知道**每轮修了什么**：§10–§39 是 Q0a–Q72 与 R/S/T 三批的逐轮收口记录（带当轮门禁日志）；其 §1–§9 的 anatomy 已被 V12 取代 |
| [工业化易用性收口指南](docs/工业化易用性收口指南-V1.md) | 上手：最短可用路径、回测入口族与配置落点、产物字段、读模型 `null` 口径、发布前检查 |
| [deploy/README.md](deploy/README.md) | 运维：运行时配置、worker 拓扑、CCXT/A 股接入、SubmitOrder、存储后端、停机与故障 |
| [CCXT 多交易所接入与策略运行方案](docs/CCXT多交易所接入与策略运行方案-V1.md) | CCXT worker 契约、凭据隔离、失败即闭的归约规则 |
| [A 股数据源接入与快速选股回测方案](docs/A股数据源接入与快速选股回测方案-V1.md) | A 股提供方、代码归一化、公司行为与八条交易制度 |
| [外部链路验收执行方案](docs/外部链路验收执行方案-V1.md) | 有凭据时怎么按五段阶梯验收，以及 `sandbox_tested` 何时才允许翻转 |
| [工业级多语言策略与高性能交易方案](docs/工业级多语言策略与高性能交易方案-V1.md) | Rust/C++/Python 策略契约与共享内存传输的边界 |
| [虚拟交易与衍生品统一模型](docs/虚拟交易与衍生品统一模型-V1.md) | 现货/杠杆/永续/交割的撮合与记账统一口径 |
| [maturity/capabilities.yaml](maturity/capabilities.yaml) | 机器可读的能力证据分级：每条能力的实现/代码测试/沙盒/生产批准四档 + 证据 + 缺口 |
| [CHANGELOG.md](CHANGELOG.md) | 逐轮变更、验收数字与"本轮没修什么" |

任何一条命令怎么用、参数是什么，以 `cargo run -p qx-cli -- help` 为准：那份摘要由 clap 的命令定义派生，
架构门禁校验「help ≡ `cli.rs` 派发分支」，因此它不会与实现漂移；本 README 不复述完整命令表。

演示会输出（2026-09-23 本机 `cargo run -p qx-cli -- all` 实抓）：

```
[观星 · 质量门 · DEMO 合成输入] bars=400 判定=Ok

[星板 · 回测 A · DEMO 合成输入] 内核=qx-xingban::BacktestEngine(bar) 成交=1 手续费=0.5171 总收益=0.09% 最大回撤=0.07% 终值=100096.72
[更路 · RunManifest · DEMO 合成输入] digest=7e11720745002a39

[更路 · 重放校验]
  ① 同输入两次运行哈希一致 : true
  ② 改参数后哈希发生变化   : true

[卯眼 · 插件装配]
  插件数=2 加载顺序=["sys.simulation", "sys.transaction-cost"]
  独占冲突=[]

全部自校验通过 ✓
[针路 · PaperVenue] 订单接受/报价成交/断线转对账/恢复通过 ✓
```

## 设计底线（改动前请先读）

1. **热路径不用浮点** —— 金额/价格/数量一律 128-bit 定点（`SCALE = 1e9`）。
   IEEE-754 的 NaN 位模式不确定，会直接破坏 bit-level 可重放。
2. **不用系统时间** —— 回测只消费输入数据自带的时间戳；时间轴由 `qx-data` 先排序、再用
   「同一标的 ts 必须严格递增」的闸门决定，帧读侧对乱序同样直接报错。内核不提供虚拟时钟对象，
   真实墙钟只出现在 paper/live 的 worker 循环里。
3. **不用无序容器做顺序敏感迭代** —— 顺序敏感处一律 `BTreeMap` / `Vec` + 排序。
4. **不用 `DefaultHasher` 做摘要** —— 其输出不保证跨版本稳定，改用内置 FNV-1a。
5. **不用外部 RNG** —— `rand` 实现细节可能随版本变化，自实现 xorshift64\* 锁定种子语义。
6. **同时间戳按因果优先级排序** —— 不是任意顺序。`MARKET < COMMAND < MATCH < APPLY < POST`。
7. **bar t 决策，bar t+1 开盘成交** —— 从结构上杜绝 cheat-on-close。
8. **只读投影不做第二个事实源** —— API、状态查看与任何前端只消费 EventLog 派生的快照与游标，
   写操作一律经 `ControlPlane` 落审计后再进内核；投影不得回写交易状态。
9. **"没算过"必须显式缺席** —— 钱字段用 `null`（`None`）表达"这一层没算/交易所没报"，`0` 只表达"算过且为零"；
   只有价格为保留哨兵语义继续用 `0`（0 不是合法定点价格）。缺数据的字段兜一个合法的 0 等于替交易所报数。

## 当前状态

下面的数字全部是 2026-09-26 在本机实测得到的（合流上游 4 个提交之后又跑完三遍连通性清点 + 一遍构建与
安装面清点 + 一遍断链逐条判定 + 一遍把 `build.bat` 全 8 步端到端跑通的收口，以 V12 §19 那一轮为准），
不是从旧文档抄来的：
23 个 crate、`help` 印出 52 行用法、
其中 41 个入口名（clap 命令表 40 项 + `help` 本身，门禁按集合断言三者相等）、`strategy list` 列 17 个内置策略
（13 个单标的 + 4 个只被 `backtest multi-builtin` 接受的套利 kind）、`git ls-files deploy` 里 44 份
`*example*.json` 配置、`python tools/check_architecture.py` 455 项不变量全绿（其中含一条全仓地板：
`crates/*/src` 与 `crates/*/tests` 递归的 `#[test]` 总数不得低于磁盘实测的 851）。

能力矩阵把每条能力钉在四档证据上（`maturity/capabilities.yaml`，本轮实测）：19 个能力块、268 条证据行
（其中 238 行以仓库内路径开头，逐行经门禁核对存在性）、73 条 limitation。**19 块中 16 个同时满足
`implementation` 与 `code_tested`；`sandbox_tested` 与 `production_approved` 无一为真**；
`postgres` / `nats` / `broker_gateway` 三条连 `implementation` 都是 `false`，只有 feature 矩阵或接口占位。
因此可宣称的边界是：
**本机可重放的确定性回测、Paper 闭环、以及 CCXT/Binance 的代码级契约** —— 不是"已对接真实账户"。
整树测试（带 Python 解释器，`QX_PYTHON` 指向可用的 CPython）92 个 `test result:` 段全 ok、
843 passed、0 failed；不带 `QX_PYTHON` 时那 2 条 Python 桥用例必红（本机事实，见 V12 §15.4），
而 `build.bat` 从本轮起会把 `[0/8]` 探测出的解释器真的导出成 `QX_PYTHON`，所以整脚本一次跑通：
八步全过、`BUILD_BAT_EXIT=0`（V12 §19.1 #133）。
**`python/.venv/Scripts/python.exe` 不是恒可依赖的**：它在上一轮曾在一台并发 uv 进程的干扰下消失过，
本轮用 `uv venv .venv --python 3.12 --clear` + `uv pip install tzdata` 离线重建（3.12.13，tzdata 2026.4），
上一段的数字就是用它跑出来的；重建不改变"它会消失"这个风险，所以 `build.bat`/`build.sh` 从第 `[0/8]` 步
起就探测解释器而不是信任 venv 在场（V12 §17.2）。被这条假设牵动的还有 `python -m unittest` 一类命令 ——
换解释器时结果不变，前提不变。
被 `#[cfg(feature = "sqlite")]` 挡住的存储用例不在其中，必须另跑 `cargo test -p qx-storage --features sqlite`
（本轮 10 段全 ok / 56 passed / 0 failed）。同一轮的 `tools/validate_core.py` 与
`python -m unittest discover -s python/tests`（49 条，1 skip）均退出码 0。

| 链路 | 已落地且有本地测试证据 | 明确未收口（不得当作已完成） |
|---|---|---|
| 回测 | Bar 单标的链（撮合/成本/延迟/保证金四模型可配）、深度 `backtest book`（`--fill-tier` 只认 `l1`/`l2`：前者走 Tick 且要求帧内每档单档盘口，多一档即拒；后者走订单簿，按 `L2L3` 吃掉帧里带的全部深度，没有第三个 `l3` 旗标）、双腿 `multi-builtin`（组合收益按两条腿的钱合算，并落两腿期初本金与期末权益）、`fast-backtest` 并行、Bar 与深度链各写四份同前缀产物与可重算的输入指纹、事件重放结论；账户本金可经 `strategy.initial_cash_raw` 声明，三条单腿链各印一行生效本金与来源，摘要 v4 落 `account` 块，非法声明与非正本金当场报错（V11 Q71/Q72）；一份 runtime 里回测侧与 Paper 侧的本金必须同号，否则三条链与两个 Paper 入账入口都在任何副作用之前拒掉（V12 R3）；`report`/`status` 的读侧改成 `Option` 语义：缺键印 `absent`、声明过的 0 仍印 0，首行报产物世代与三块的有无，复核结论独占 `input_verified=` 一格（V12 R1）；四个信号旋钮按 kind 收成唯一一张表（`BuiltinStrategyKind::signal_knobs()`），内核需求、运行时体检、stdout 播报与摘要的 `signal{knobs,declared_unused}` 全问它，读者第一次能从产物里区分"没配"与"配了但不生效"（V12 R4 / #102）；`run_manifest.json` 的兄弟路径、`data_fingerprint` 与产物身份有了真正的生产读者，缺失或被篡改都拒（V12 R4-i） | 被 git 跟踪的 16 份 blessed 摘要停在 `schema_version: 1`（当前代码落 v4，缺 `input` 与 `account` 两块），且没有任何用例校验这些跟踪产物；重 bless 与"摘要世代写进文件名"的取舍仍未拍板（V11 §27.5 / §34.5 第 3 条，任务 #53）；多腿链仍无 `[X · Account]` 播报，因为它同时是四条链里最长的一条（V11 Q72 §34.5 第 1、4 条）；`multi-builtin` 只在 `--root` 点名时写 1 份归因产物，没有 Bar/深度链那四份同前缀产物、也没有 RunManifest；清单外的旋钮**不 fail-closed**、照常跑只列进 `declared_unused`，"配了但不生效"只有看播报与产物才看得见（V12 §14.2 的偏离记录）；集成用例把产物写进仓库 `deploy/data/**/runs/`（跑完 72 条未跟踪产物），#82 未修 |
| 交易 · Paper | 控制面→队列→成交→Ledger、三条入账入口共用精度闸门、拒单与拒绝原因写进产物、崩溃窗口恢复；账户快照的八个钱标量全部区分"算过"与"没算"，权益在任一持仓缺标记价时报 `null` 而不是剩余现金（V11 Q67/Q68/Q70）；七个会提交/排队订单的入口（含 `serve` 与 `strategy-worker`）一律在第一个语句拒 A 股段（V11 Q65 + V12 R1）；账户快照的契约版本真的认版本 —— 只认 `schema_version=1`，且版本判定排在字段解析之前（V12 R2）；带订单的快照写得出也读得回 —— 四张键表（`positions`/`orders`/`fills`/`transfers`）整份交给 `from_json` 认的那一份 serde 编码（`json_table_entries` / `json_position_entries`），键一律是带引号的字符串，`Side`/`OrderStatus` 印变体名而不是数字码，未知变体名当场拒（V12 R4 / TX5，与 V11 R14/R15/R18 同一收口点） | 本地 Ledger 拼出的持仓行没有浮盈/保证金生产者，恒定报 `null`；账户级五个钱字段同样无来源；权益报 `null` 时读侧看不出缺的是哪条标记价（V11 §32.5 第 5 条）；`fills`/`transfers` 行只编码数值与已同形的字符串，因此没有读侧折算层，也就没有"未知码"可拒（V12 §14.5 记为口径而非缺陷） |
| 交易 · 实盘 | Binance Spot 直连与公共 CCXT 的提交/回报/对账代码路径，缺凭据即退出码 3 fail closed；一轮 CCXT 对账的两半发现（远端孤单 / 本地无远端结果）经同一份清单同时落到事实流、持久报告与健康判定（V11 Q69）；两条用户流的重连都有连续失败预算 —— Binance 按累计次数计改为 `delivered > 0` 清零并统一走 `qx-core::retry`，CCXT Pro `watch_orders` 从"固定间隔无限重连"补上 500ms 起 / 8s 封顶 / 10 次连续（V12 R4-b/R4-c） | 零真实账户往返：`sandbox_tested=false`；预算与退避只在进程内证过，网络真断时的重连语义无外部记录；Binance 对账链从不查询持仓/资金费/账单，报告只能报 `null`（取数器仍缺，V11 §31.5 第 5 条）；CCXT 资金费快照缺 `timestamp_ms` 时仍兜 0（§31.5 第 1 条）；远端孤单挂单只有报告与健康两面，没有可落事实流的本地句柄（§31.5 第 4 条，口径而非缺陷）；衍生品无直连（只经 CCXT） |
| 数据 | 四级质量门、PIT `as_of()` 可见性、DatasetBundle 与组件指纹、A 股公司行为台账与八条交易制度 | 外部数据源正确性只能按供应商逐个验收，本机不可证明 |
| 运行时 | worker 监督与停机阶梯、共享文件系统租约/fencing token、outbox relay（sqlite/postgres/nats 四种 feature 组合可编译）；停机阶梯接上了生产者 —— `Ctrl+C`/`SIGTERM` 真的落到 `RuntimeSupervisor::request_shutdown`，等到 `shutdown_timeout_ms` 报 `StopTimedOut` 而不是无限等（V12 R4-a）；只读投影的契约贯通同轮收口：`/account/snapshot/envelope` 的 `data` 复用唯一编码器、与本进程公布的 schema 逐字段等值，schema 正文改成 `include_str!` 单一来源，两条事件读链的 `after` 统一为严格大于（V12 R4-h/A2/R4-g） | PostgreSQL/NATS 无生产批准；券商柜台无供应商协议；停机信号投递本身没有用例 —— 用例钉住的是阶梯与预算，不是真把 `SIGTERM` 发给进程（Windows 下也没有可投递的 `SIGTERM`，V12 §14.8） |

跨语言策略传输默认兼容 JSONL，也支持 `transport: "framed_json"` 的 QXSF 二进制分帧（版本、序号、长度上限、CRC32）；`transport: "shared_memory_json"` 会将同一 QXSF 帧放入双向固定槽位 SPSC mmap ring；`transport: "shared_memory_columnar"` 会将 Bar 历史编码为 QXCB 固定宽度列后放入同一 ring，适合减少行情数值 JSON 解析。三种传输的载荷由同一份 `schemas/strategy_api_v1.schema.json` 描述：`required` 名单按 Rust 非 `#[serde(default)]` 字段逐项对齐，定点 `raw` 值只走 JSON 整数，未知键两侧同拒；一条已知差异写进 schema 的 `description` 而不是靠改口径掩盖 —— 大写枚举名过得了 Rust 运行时、过不了 schema。示例：

```powershell
cargo run -p qx-cli -- backtest deploy/qianxing.runtime.strategy-framed.example.json deploy/qianxing.bar-frame.example.json
```

共享内存策略 Worker 回测：

```powershell
cargo run -p qx-cli -- backtest deploy/qianxing.runtime.strategy-shared.example.json deploy/qianxing.bar-frame.example.json
```

列式共享内存策略 Worker 回测：

```powershell
cargo run -p qx-cli -- backtest deploy/qianxing.runtime.strategy-columnar.example.json deploy/qianxing.bar-frame.example.json
```

共享内存传输层微基准（不代表完整策略端到端延迟）：

```powershell
cargo run --release -p qx-strategy --example ring_bench
```

本地 SubmitOrder 已补齐 Paper `Queue→Fill→Ledger` 可执行闭环；生产配置下的真实 Binance 交易、秘密/证书托管、跨节点 PostgreSQL/MQ 和部署级高可用仍按审计表单独验收。
运行时还提供可恢复的 `scheduler-worker` 与 `strategy-worker`：Scheduler 恢复 JobSpec/Run 状态并投递 JobQueue，Strategy 管理生命周期、消费任务、执行 `Signal→Portfolio→RiskGate→OrderIntent` 并提交审计化 SubmitOrder；策略按账户/Venue 读取对应事件日志计算当前持仓，Paper 执行器在控制面已落终态但队列尚未确认的崩溃窗口只清理旧队列、不重复产生副作用；策略进程不会绕过 Risk/OMS 直接调用 Venue。
Paper 主链路还提供 `paper-e2e` 统一验收入口，按 Scheduler→Strategy→Paper Execution 顺序运行一轮并检查订单、Ledger、审计和队列终态。

三条容易被读过头的边界，写在这里而不是散落在阶段表里：

- **插件只有启动期装配**：`qx-plugin` 做 manifest schema/哈希校验与 Ed25519 签名验证、扩展点贡献声明、
  独占冲突检测、依赖求解并产出静态装配计划；运行时动态加载与热替换**没有实现**，内核组件由编译期决定。
- **回测与实盘同源的是规则，不是撮合**：风控、费用、延迟、保证金四模型与事件归约同源且有门禁；
  撮合按数据档位分内核（Bar / Tick / 订单簿三套回测内核，Paper 成交由 `PaperVenue::on_quote` 首档 touch 产生），
  与回测簿内核**不是**同一台撮合机 —— 读到"同一内核"时不要把"同一撮合"一起读进去。
- **WASM、跨节点 HA、连接池/读写分离、逐家签名协议、manylinux/musllinux 与发布签名仍未接入**，
  需要按实际供应商与部署环境立项，不由 feature 开关存在而宣称完成。

## 许可

Apache-2.0

> 本框架仅参考业界项目的公开架构与模块设计思路（LEAN、RQAlpha、NautilusTrader、vn.py、
> Hummingbot、CCXT、vectorbt、QUANTAXIS），不复制任何第三方源代码。
