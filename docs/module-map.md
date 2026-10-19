# 模块地图

## 模块

| 模块 | 职责 |
|---|---|
| `qx-core` **牵星** | 确定性内核：时间戳口径（`clock.rs` 只有 `pub type Ts = u64;`，**内核里没有时钟对象** —— 时间轴由推进方决定）、事件溯源、重放校验，以及 `VenueId` / `InstrumentId` / `MarketId` / `CanonicalProduct` 身份契约（`identity.rs`）；因果序由 `EventLog::validate` 与 `qx-runtime` 的 `append_at_engine` 单点裁决（`TIMER < FEEDBACK < MARKET < COMMAND < MATCH < APPLY < POST`）；合约规格与市场状态分离落在 `trading.rs`（**无独立 `fenye` 模块** —— 该模块已删且不复活，此处按代码事实改口，见 docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md WP-15） |
| `qx-guanxing` **观星** | 市场数据语义：`DataSourceId`、质量门、标准化、`as_of()` point-in-time 可见性；维护被执行与研究层共用的紧凑市场类型 |
| `qx-data` | 摄取与数据集基础设施：提供方适配、目录、存储与增量管道；带 instrument 的源记录在进入市场语义层前显式转换。两 crate 的边界决策见 `maturity/data_plane_boundary.yaml` |
| `qx-xingban` **星板** | Bar/L1 Tick/L2 订单簿撮合与仿真：成本、延迟、保证金、因果回测 |
| `qx-zhenlu` **针路** | 执行与路由：风控门禁、OMS、路由决策 |
| `qx-risk` | 风控规则集：`RiskRule`（禁空 / 最大数量 / 最大名义）与保守默认规则集版本 |
| `qx-genglu` **更路** | 订单维度对账裁决（**绩效指标见 `qx-xingban`、成交归因见 `qx-cli`、现金对账见 `qx-runtime`**，本 crate 刻意不重复实现） |
| `qx-plugin` **卯眼/榫头** | 能力清单注册表：manifest schema/哈希校验与 Ed25519 签名、扩展点贡献声明、独占冲突检测、依赖求解、Profile/Bundle/Patch 静态装配计划（不含运行时动态加载） |
| `qx-factor` | 因子与特征：版本、PIT 工件、分析报告、候选策略绑定 |
| `qx-protocol` | 账户协议：Canonical Snapshot、Diff、QIFI 兼容边界 |
| `qx-provider` | 数据提供方：能力矩阵、稳定选择、主备故障切换 |
| `qx-scheduler` | 调度契约：JobSpec、依赖、交易日历、幂等与重试 |
| `qx-control` | 控制面：权限、审计命令、终态退场（**事件订阅游标在 `qx-api`**，本 crate 不含） |
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
| `cpp/` | C++ Strategy API v1 / C ABI v2（intent 长度与容量协商）、CMake 示例 |
| `qx-cli` | 单一 CLI binary：命令分派、worker 装配、回测与运维命令，外加进程内确定性自校验 |
| `web/console/` | Web 控制台（M1' 快照 / M2' 实时事件 / M3' 控制面）：读面板只调 `qx-api` 的 `GET` 读面与 WebSocket 增量，**写面只有一条** `POST /control/commands`（受理 → 执行者判定 → 终态退场），**不直接下单**；它点名的每个端点由门禁 `web_console_check` 与后端路由表逐一核对 |

## English role index

| Crate / surface | English role |
|---|---|
| `qx-core` | Deterministic domain kernel, event log, replay, and identity contracts. |
| `qx-guanxing` | Market-data semantics, quality, normalization, and point-in-time visibility. |
| `qx-data` | Data ingestion, provider adapters, datasets, and storage pipelines. |
| `qx-xingban` | Backtest simulation, order matching, costs, latency, and margin models. |
| `qx-zhenlu` | Order execution governance, risk admission, and routing decisions. |
| `qx-risk` | Reusable risk rules and conservative default policies. |
| `qx-genglu` | Order-level reconciliation and discrepancy decisions. |
| `qx-plugin` | Plugin manifests, capability validation, and static composition plans. |
| `qx-factor` | Versioned factors, point-in-time artifacts, and research reports. |
| `qx-protocol` | Account snapshots, diffs, and external account protocol boundaries. |
| `qx-provider` | Data-provider capability selection and primary/backup failover. |
| `qx-scheduler` | Job contracts, dependencies, calendars, idempotency, and retries. |
| `qx-control` | Authorization, audit commands, and control-plane lifecycle. |
| `qx-api` | Framework-independent query, control, event-stream, and WebSocket APIs. |
| `qx-datastruct` | Columnar market-data structures and JSON/Arrow interchange. |
| `qx-adapter` | REST, TLS, WebSocket, venue, and provider integrations. |
| `qx-runtime` | Runtime topology, worker supervision, shutdown, and health state. |
| `qx-orchestrator` | Validated worker startup planning and process lifecycle management. |
| `qx-storage` | Event logs, queues, snapshots, audit records, and database backends. |
| `qx-strategy` | Rust strategy interfaces, contexts, events, and native SDK. |
| `qx-execution` | Stable execution ports and venue-side order submission boundaries. |
| `python/qianxing_ccxt` | Public CCXT REST data and multi-exchange trading integration. |
| `qx-python` | PyO3 native bindings and Arrow C Data Interface capsules. |
| `cpp/` | C++ strategy API, C ABI, and CMake integration examples. |
| `qx-cli` | Command-line interface for backtests, operations, and local workflows. |
| `web/console/` | Web operations console backed by the read and control API. |

`qx-cli` 是单个 binary（决策：不为拆进程而拆 crate），内部按职责分文件：命令语法与命令表只有一份，在 `cli_args.rs` 由 clap 派生（V10 P2b，旧手写字符串解析已整体删除、不留双轨），`cli.rs` 保留对 `Command` 的一次显式 `match`，未识别的命令或未知参数只回错误正文、clap 给出的最接近入口建议与该入口自己的 `Usage:`，再补两行「下一步」，仍以退出码 2 fail closed（改前每条用法错误甩出的是整篇入口摘要：实测 162 行 / 12,319 B（`logs/s691_pass28_before_fix_error_wall.txt`，改前 debug 树上五条命令 162–164 行）；V13 R2 #257/#258 收成未知入口 8 行 / 289 B、参数形状不对 10 行 / 359 B，退出码仍 2），`tools/check_architecture.py` 校验「clap 命令表 ≡ `cli.rs` 分支集合 ≡ help 印出的入口」；`worker_entry.rs` 用类型系统里的 `WorkerRole::is_venue_role()` 加一张 `VenueEntry` 登记表同时服务 `ccxt-worker` 与 `binance-worker`，新增角色只需在登记表上补一条 Venue 绑定判定；跨语言子进程的 Python 解释器统一由 `QX_PYTHON` 解析（缺省 `python`）。

