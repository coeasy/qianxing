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

```bash
# 构建
cargo build --release

# 运行端到端演示（含确定性自校验）
cargo run -p qx-cli --release

# 纸面交易 / 对账验收
cargo run -p qx-cli --release -- paper
# 本地对账契约 smoke；真实 Binance 单轮对账使用下方 runtime 入口
cargo run -p qx-cli --release -- reconcile
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

实现状态与未完成外部边界见：现行重构基线 [自研量化框架重构方案 V9](docs/自研量化框架重构方案-V9.md)，历史审计见 [V8 架构审计与重构方案](docs/自研量化框架架构审计与重构方案-V8.md) 和 [工业级落地验收与差距清单](docs/工业级落地验收与差距清单-V1.md)。

能力证据分级见：[maturity/capabilities.yaml](maturity/capabilities.yaml)。默认 `single_node` 使用 SQLite/Files；PostgreSQL、NATS、真实交易所沙盒和券商柜台不会因为代码或 feature 存在而被标记为生产批准。

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

Barter 对齐后的最终目标架构见：[牵星最终架构方案 V2：Barter 对齐版](docs/牵星最终架构方案-V2-Barter对齐版.md)。

跨项目对比、可视化终态、产品工作流与分阶段实施门禁见：[牵星终极改造计划 V1](docs/牵星终极改造计划-V1.md)。

模块是否拆分、哪些能力需要扩展以及新 crate/进程的拆分门禁见：[牵星架构拆分与扩展决策 V1](docs/牵星架构拆分与扩展决策-V1.md)。

可视化、控制面、快照/游标、实时投影和三轮端到端链路审计见：[Qianxing Visualization Architecture V1](Qianxing-Visualization-Architecture-V1.md)。

工业化易用性收口入口和发布前检查见：[工业化易用性收口指南 V1](docs/工业化易用性收口指南-V1.md)。

演示会输出：

```
[观星 · 质量门] bars=400 判定=Ok
[星板 · 回测 A] 成交=... 手续费=... 总收益=...% 最大回撤=...% 终值=...
[更路 · 重放校验]
  ① 同输入两次运行哈希一致 : true
  ② 改参数后哈希发生变化   : true
[卯眼 · 插件清单注册]
  插件数=2 加载顺序=["sys.simulation", "sys.transaction-cost"] 独占冲突=[]
全部自校验通过 ✓
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

## 当前状态

完整架构方案：[`自研量化框架重规划方案-V5.md`](自研量化框架重规划方案-V5.md)

本项目已完成 **Phase 0–4 的确定性内核与研究/协议闭环**，并补齐了 V5.1 的账户隔离账簿、合约乘数估值、真实事件重放、PIT 数据边界、L1 撮合容量、延迟/保证金模型、PaperVenue 恢复、Provider/因子/协议/调度/控制面的可执行实现。
当前已增加可运行 HTTP/WebSocket 控制面、Operator 权限校验、单进程/共享文件/可选 SQLite 事务 API 限流、Snapshot/Diff 与事件游标、持续实时事件总线、有序关闭、可热替换 TLS 配置、PEM 证书加载/轮询式安全重载、mTLS `ServerConfig` 与客户端证书到 Operator 的可信映射、明文/TLS API 服务端入口、文件恢复、带并发追加锁的链式持久化审计、可选 SQLite/PostgreSQL 审计链/快照/任务租约/fencing token/EventLog 后端、Python/JSON DataStruct、PyO3 原生扩展与本机构建 wheel、Linux/Windows（Python 3.10/3.12/3.13）wheel CI 矩阵、Arrow C Data Interface 借用零拷贝与拥有型跨语言释放边界、带 rustls TLS 客户端、带 API key 握手头的 TLS WebSocket 用户流底座、HMAC-SHA256 签名边界、超时/限频/成交回报幂等的 REST Provider/Venue 适配器基线、Binance Spot 主网/Testnet HMAC 签名下单/撤单、`allOrders`/开放订单对账、公共 L1 REST `bookTicker`/WSS 流、签名用户流订阅会话、可注入重连退避驱动与 `executionReport` 用户事件映射、支持环境变量或 Secret Manager 投影文件且在新会话/新订单/新对账轮次重新加载凭证的独立 Binance 行情/用户流/对账 worker、LiveEventPipeline 事件→Kernel/EventLog/Ledger/账户余额快照归约与订单重启恢复、结算币种差异报告、Cron/Calendar/Event/Manual 调度与 JobWindow/Worker/带失败码与退避的确定性重试、超时人工介入、可校验 RunManifest、Provider 来源哈希与 JSON 血缘恢复、PIT 财务视图、按样本计算 IC/RankIC/衰减/换手的因子报告、带训练/验证区间和执行/风险模型绑定的因子候选、真实组内中性化、插件 manifest schema/hash 校验与 Ed25519 发布签名验证、带基准/持仓/费用/换手/回撤的回测报告、共享文件系统 claim 锁/fencing token 任务队列与租约、因子 DAG 和多 Venue 路由评分；连接池/读写分离、跨节点 HA、MQ、真实账户网络验收、其他供应商用户流认证/订阅与事件映射、逐家签名协议、证书签发、manylinux/musllinux 兼容性、发布签名和 WASM 仍需按实际供应商与部署环境接入。详见[工业级产品化实施路线图 V1](docs/工业级产品化实施路线图-V1.md)。

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

按架构方案的阶段划分：

| 阶段 | 内容 | 状态 |
|---|---|---|
| 0 | 统一领域契约 + 事件溯源 + 精确重放 | ✅ 已落地 |
| 1 | 插件清单注册（扩展点声明/manifest/依赖求解/静态装配计划） | ✅ 已落地（启动期能力注册；运行时动态加载与热替换未实现，按 V7 方案 A 收敛定位） |
| 2 | 回测引擎（TestClock + 因果队列 + 撮合模型） | ✅ 已落地 |
| 3 | 账簿、L1、PaperVenue、限频、恢复与对账契约 | ✅ 基础能力已落地 |
| 4 | 真实 Venue / 多数据源生产接入 | 🟡 Binance Spot REST/L1/用户事件与 EventLog 归约基线已落地，真实账户验收及其他供应商待接入 |
| 4 | ProviderRegistry / 因子计算 / QIFI 快照与 Diff / Scheduler | ✅ 可执行实现，含 DAG、CronSpec、Worker、重试和 Diff；生产适配待接入 |
| 5-7 | 真实 Venue / 多账户路由 / 生产可靠性 / WASM | 🟡 Binance 单账户审计执行入口已落地，多账户/跨节点生产执行器待接入 |

## 许可

Apache-2.0

> 本框架仅参考业界项目的公开架构与模块设计思路（LEAN、RQAlpha、NautilusTrader、vn.py、
> Hummingbot、CCXT、vectorbt、QUANTAXIS），不复制任何第三方源代码。
