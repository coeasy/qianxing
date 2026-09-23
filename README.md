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

## 快速开始

下面这段是可复制的常用链路示例。参数与入口的权威来源是 `cargo run -p qx-cli -- help`
（本轮实测 52 条入口行 / 41 个命令名、`deploy/` 顶层 52 份示例配置），完整使用口径见
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
./tools/build_python_wheel.ps1
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

Windows 下可直接双击 `build.bat`。

实现状态与未完成外部边界见：现行重构基线 [自研量化框架重构方案 V11](docs/自研量化框架重构方案-V11.md)。
它逐轮记录审计与修复（每轮都带 `file:line` 与当轮门禁日志），是唯一仍在更新的方案文档。

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
| [自研量化框架重构方案 V11](docs/自研量化框架重构方案-V11.md) | 想知道**现在**代码的真实状态、每轮修了什么、还有哪些已知缺口（唯一在更新的方案文档） |
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
2. **不用系统时间** —— 回测只认 `TestClock`，时间只在 `advance_to` 时前进。
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

下面的数字全部是 2026-09-23 在本机实测得到的，不是从旧文档抄来的：23 个 crate、
`cargo run -p qx-cli -- help` 印出 52 条入口行、覆盖 41 个命令名（`config`/`backtest`/`strategy`/`run`
各自带子入口）、`builtin-strategies` 列 17 个内置策略
（13 个单标的 + 4 个只被 `backtest multi-builtin` 接受的套利 kind）、`deploy/` 顶层有 52 份示例配置、
`python tools/check_architecture.py` 不变量全绿（项数见当轮门禁日志）。

能力矩阵把每条能力钉在四档证据上（`maturity/capabilities.yaml`，本轮实测）：**18 个能力块中 15 个
同时满足 `implementation` 与 `code_tested`；`sandbox_tested` 与 `production_approved` 无一为真**；
`postgres` / `nats` / `broker_gateway` 三条连 `implementation` 都是 `false`，只有 feature 矩阵或接口占位。
矩阵的证据与 limitation 条数以当轮 `maturity/capabilities.yaml` 实测为准。因此可宣称的边界是：
**本机可重放的确定性回测、Paper 闭环、以及 CCXT/Binance 的代码级契约** —— 不是"已对接真实账户"。

| 链路 | 已落地且有本地测试证据 | 明确未收口（不得当作已完成） |
|---|---|---|
| 回测 | Bar 单标的链（撮合/成本/延迟/保证金四模型可配）、深度 `backtest book`（`--fill-tier` 只认 `l1`/`l2`：前者走 Tick 且要求单档盘口，后者走订单簿并按 `L2L3` 吃掉帧内全部深度）、双腿 `multi-builtin`（组合收益按两条腿的钱合算，并落两腿期初本金与期末权益）、`fast-backtest` 并行、Bar 与深度链各写四份同前缀产物与可重算的输入指纹，账户本金可经 `strategy.initial_cash_raw` 声明、来源分三种，三条单腿链各印一行生效本金与来源、摘要落 `account` 块，非法声明与非正本金当场报错（V11 Q72）、事件重放结论 | 被 git 跟踪的 16 份 blessed 摘要停在 `schema_version: 1`（当前代码落 v4，缺 `input` 与 `account` 两块），且没有任何用例校验这些跟踪产物；重 bless 与"摘要世代写进文件名"的取舍仍未拍板（V11 §27.5 / §28.5 第 5 条）；`multi-builtin` 只有 `--root` 点名时写 1 份归因产物，没有 RunManifest；回测本金与 paper 的 `worker.paper_initial_cash_raw` 是两格且互不知情（V11 §34.5 第 2 条）；多腿链仍无 `[X · Account]` 播报，因为 `multi_builtin.rs` 已到 498/500 行（§34.5 第 1、4 条） |
| 交易 · Paper | 控制面→队列→成交→Ledger、三条入账入口共用精度闸门、拒单与拒绝原因写进产物、崩溃窗口恢复、账户快照的对账两格由磁盘报告投影且"没对过/对过且零差异"分开发布（V11 R7/R10）；八个钱标量全部区分"算过"与"没算"，权益在任一持仓缺标记价时报 `null` 而不是剩余现金（V11 Q67/Q68/Q70） | 本地 Ledger 拼出的持仓行没有浮盈/保证金生产者，恒定报 `null`；账户级那五个钱字段在本层没有算点，同样按 `null` 发布而不是兜 0；权益报 `null` 时读侧看不出缺的是哪条标记价（V11 §32.5 第 5 条） |
| 交易 · 实盘 | Binance Spot 直连与公共 CCXT 的提交/回报/对账代码路径，缺凭据即退出码 3 fail closed；一轮 CCXT 对账的两半发现（远端孤单 / 本地无远端结果）经同一份清单同时落到事实流、持久报告与健康判定（V11 Q69） | 零真实账户往返：`sandbox_tested=false`；Binance 对账链从不查询持仓/资金费/账单，报告只能报 `null`（取数器仍缺，V11 §31.5 第 5 条）；CCXT 资金费快照缺 `timestamp_ms` 时仍兜 0（§31.5 第 1 条）；远端孤单挂单只有报告与健康两面，没有可落事实流的本地句柄（§31.5 第 4 条，口径而非缺陷）；衍生品无直连（只经 CCXT） |
| 数据 | 四级质量门、PIT `as_of()` 可见性、DatasetBundle 与组件指纹、A 股公司行为台账与八条交易制度 | 外部数据源正确性只能按供应商逐个验收，本机不可证明 |
| 运行时 | worker 监督与停机阶梯、共享文件系统租约/fencing token、outbox relay（sqlite/postgres/nats 四种 feature 组合可编译） | PostgreSQL/NATS 无生产批准；券商柜台无供应商协议 |

跨语言策略传输默认兼容 JSONL，也支持 `transport: "framed_json"` 的 QXSF 二进制分帧（版本、序号、长度上限、CRC32）；`transport: "shared_memory_json"` 会将同一 QXSF 帧放入双向固定槽位 SPSC mmap ring；`transport: "shared_memory_columnar"` 会将 Bar 历史编码为 QXCB 固定宽度列后放入同一 ring，适合减少行情数值 JSON 解析。示例：

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
