# 模块地图

## 模块

| 模块 | 职责 |
|---|---|
| `qx-core` **牵星** | 确定性内核：时间戳口径（`clock.rs` 只有 `pub type Ts = u64;`，**内核里没有时钟对象** —— 时间轴由推进方决定）、事件溯源、重放校验，以及 `VenueId` / `InstrumentId` / `MarketId` / `CanonicalProduct` 身份契约（`identity.rs`）；因果序由 `EventLog::validate` 与 `qx-runtime` 的 `append_at_engine` 单点裁决（`TIMER < FEEDBACK < MARKET < COMMAND < MATCH < APPLY < POST`）；合约规格与市场状态分离落在 `trading.rs`（**无独立 `fenye` 模块** —— 该模块已删且不复活，此处按代码事实改口，见 docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md WP-15） |
| `qx-guanxing` **观星** | 市场数据语义：`DataSourceId`、质量门、标准化、`as_of()` point-in-time 可见性；维护被执行与研究层共用的紧凑市场类型 |
| `qx-data` | 摄取与数据集基础设施：提供方适配、目录、存储与增量管道；带 instrument 的源记录在进入市场语义层前显式转换。两 crate 的边界决策见 `maturity/data_plane_boundary.yaml` |
| `qx-app` | 应用用例与研究事实 owner：回测/实验流程、产物持久化与身份、报告读取/核验，以及多腿/多市场的可复算账务事实；Bar/Depth 单次运行共享 `ResearchRunReportV1` 与 `ArtifactReader`，多腿 `MultiLegResearchReportFactsV1` 重放 EventLog 并聚合复核共享账簿、逐腿成交/收益/资金费、成本归因、权益曲线、组合收益/回撤和待对账成交额；多市场 v1 内嵌与 v2 流式 EventLog 报告均在 qx-app 核验重放、有限分页、成交数和订单-标的归属；多账户 Bar/Depth 的父 RunManifest、批次 summary、加权收益与子身份指纹由 qx-app 复核/重算；CLI/API/Python 负责装配、授权或呈现，不另算一份领域结果 |
| `qx-xingban` **星板** | Bar/L1 Tick/L2 订单簿撮合与仿真：成本、延迟、保证金、因果回测 |
| `qx-zhenlu` **针路** | 执行与路由：风控门禁、OMS、路由决策 |
| `qx-risk` | 风控规则集：`RiskRule`（禁空 / 最大数量 / 最大名义）与保守默认规则集版本 |
| `qx-genglu` **更路** | 订单维度对账裁决（**回测撮合指标见 `qx-xingban`、研究成交归因事实见 `qx-app`、结算余额差异投影见 `qx-runtime`**，本 crate 刻意不重复实现） |
| `qx-plugin` **卯眼/榫头** | 能力清单注册表：manifest schema/哈希校验与 Ed25519 签名、扩展点贡献声明、独占冲突检测、依赖求解、Profile/Bundle/Patch 静态装配计划（不含运行时动态加载） |
| `qx-factor` | 因子与特征：版本、PIT 工件、分析报告、候选策略绑定 |
| `qx-protocol` | 账户协议：Canonical Snapshot、Diff、QIFI 兼容边界 |
| `qx-provider` | 数据提供方：能力矩阵、稳定选择、主备故障切换 |
| `qx-scheduler` | 调度契约：JobSpec、依赖、交易日历、幂等与重试 |
| `qx-control` | 控制面：权限、审计命令、终态退场（**事件订阅游标在 `qx-api`**，本 crate 不含） |
| `qx-api` | HTTP/查询与控制命令边界；研究报告按多市场、多腿、Depth、多账户拆为 adapter 模块，负责请求授权、有界文件读取、输入装配与呈现；多市场 v1 内嵌与 v2 流式 EventLog 的回放、成交事实与订单-标的归属由 qx-app reader 复用；多腿共享 EventLog 的重放与账簿/逐腿成交聚合由 qx-app reader 完成；多账户报告通过 rooted reader 复核父 RunManifest/子运行，并将已复核子事实交给 qx-app 汇总；跨账户事件分页、子报告装配和交互呈现仍属 adapter；多腿质量清单证据、共享账户账簿、逐腿成交/收益/资金费、成本归因、权益收益/回撤由 qx-app 聚合核验，API 仍负责多腿交互 DTO、事件上下文和分页；写操作只经 `ControlPlane` |
| `qx-datastruct` | 列式 BarFrame、PIT 视图、JSON/Arrow C Data Interface |
| `qx-adapter` | REST/TLS/WebSocket 传输与 Venue/Provider 适配边界；含 Binance Spot REST/L1 行情基线 |
| `qx-runtime` | 运行时拓扑配置、worker 监督、停机信号与健康状态 |
| `qx-orchestrator` | 运行编排：把已校验的 `RuntimeConfig` 变成 worker 启动计划并管理子进程生命周期；不执行策略、不下单 |
| `qx-storage` | 文件/分段 EventLog、控制面、队列、快照、审计以及 SQLite/PostgreSQL 事务后端 |
| `qx-strategy` | Rust Strategy API、上下文/事件/多订单意图和原生策略 SDK |
| `qx-execution` | Venue 回报统一归约、SubmitOrder 执行副作用边界，以及执行事实/订单/风控/行情/对账/Venue 的稳定端口（V10 P2a 由独立端口 crate 并入） |
| `qx-service` | worker/服务装配层：将按 venue/account 隔离的 ExchangeGateway 路由注册表绑定到行情、执行、账户查询、用户流、对账与恢复 worker；从 worker 指标生成端口级运行态健康投影并并入 API `/ready` O 层；行情/私有流、Venue submit/cancel、账户查询与对账各从真实业务调用边界提供探针；不把这些窄端口合成单个交易所巨型接口 |
| `python/qianxing_ccxt` | 公共 CCXT 多交易所 REST 数据/交易连接层；CCXT Pro 仅保留后续扩展接口 |
| `qx-python` | PyO3 原生扩展、Arrow C Data Interface capsule 协议；Python SDK 的 Bar/Depth/多账户及多腿报告事实读取委托 qx-app 版本化 reader |
| `cpp/` | C++ Strategy API v1 / C ABI v2（intent 长度与容量协商；可选订单状态和衍生品政策导出）、CMake 示例 |
| `qx-cli` | 单一 CLI binary：命令分派、worker 装配、回测与运维命令，外加进程内确定性自校验；`app read-report`、`app read-depth-report`、`app read-multi-account-report` 与 `app read-multi-leg-report` 调用 qx-app 版本化核心报告 DTO；多账户 Bar/Depth 富报告复用 qx-app 已验证的批次与子账户 DTO，Depth 事件使用有界 EventReport reader；多市场 Bar 的输入/质量指纹、v1 内嵌与 v2 流式 EventLog replay、权益贡献账簿复算，以及多市场 Depth 的输入/质量清单校验和数据身份复算委托 qx-app，CLI 留下本地路径解析、报告投影及 HTML/XLSX 渲染；并提供独立账户 Bar/Depth 回测命令 |
| `web/console/` | Web 控制台（M1' 快照 / M2' 实时事件 / M3' 控制面）：读面板只调 `qx-api` 的 `GET` 读面与 WebSocket 增量，**写面只有一条** `POST /control/commands`（受理 → 执行者判定 → 终态退场），**不直接下单**；它点名的每个端点由门禁 `web_console_check` 与后端路由表逐一核对 |

## English role index

| Crate / surface | English role |
|---|---|
| `qx-core` | Deterministic domain kernel, event log, replay, and identity contracts. |
| `qx-guanxing` | Market-data semantics, quality, normalization, and point-in-time visibility. |
| `qx-data` | Data ingestion, provider adapters, datasets, and storage pipelines. |
| `qx-app` | Application use cases, artifact persistence and identity, verified report reads, and replay-derived research facts. |
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
| `qx-api` | Query and control adapters, bounded research-report projections, event-stream, and WebSocket APIs. |
| `qx-datastruct` | Columnar market-data structures and JSON/Arrow interchange. |
| `qx-adapter` | REST, TLS, WebSocket, venue, and provider integrations. |
| `qx-runtime` | Runtime topology, worker supervision, shutdown, and health state. |
| `qx-orchestrator` | Validated worker startup planning and process lifecycle management. |
| `qx-storage` | Event logs, queues, snapshots, audit records, and database backends. |
| `qx-strategy` | Rust strategy interfaces, contexts, events, and native SDK. |
| `qx-execution` | Stable execution ports and venue-side order submission boundaries. |
| `qx-service` | Service and worker assembly, including account-scoped ExchangeGateway routing and per-port health projections. |
| `python/qianxing_ccxt` | Public CCXT REST data and multi-exchange trading integration. |
| `qx-python` | PyO3 native bindings and Arrow C Data Interface capsules. |
| `cpp/` | C++ strategy API, C ABI, and CMake integration examples. |
| `qx-cli` | Command-line interface for backtests, operations, and local workflows. |
| `web/console/` | Web operations console backed by the read and control API. |

`qx-cli::report_multi_account::details` 独立拥有 Bar/Depth 成交行映射、多账户诊断输入契约/指纹读取和标准运行诊断快照的幂等写入，报告 facade 复用同一 bounded detail reader。 `qx-cli::report_multi_account::render` 独立拥有账户报告公平行预算、权益降采样、有界 HTML 数据投影及离线 HTML 模板；Bar/Depth 组装经 facade 复用相同渲染器。 `qx-cli::report_multi_account::bar` 与 `depth` 分别拥有经过 qx-app 复核后的账户批次投影、逐账户诊断快照、输出保护、Evidence/XLSX/HTML 发布流程；facade 只识别输入类型并分派。 `qx-cli::report_multi_account` 的 Bar/Depth 输出覆盖检查共享一套产物/Evidence 路径判定，类型适配器只声明源文件文案和账户诊断快照保护策略，避免两份接近重复的覆盖规则漂移。 `qx-cli::report_multi_market::diagnostics` 独立拥有多市场 Bar 内置策略诊断生成、诊断身份绑定、对照集合解析与 HTML/XLSX 投影；报告 facade 负责读取已复核报告并调用该诊断层。 `qx-cli::report_multi_market_depth::projection` 独立拥有多市场深度应用层 report read request、选中 summary locator 到 FileArtifactReader 的绑定和 JSON report projection；`diagnostics` 独立拥有多市场深度内置策略诊断重放、诊断身份绑定、对照诊断读取/投影以及 Evidence 引用复核；depth 报告 facade 保留稳定 crate 导出。 `qx-cli::report_multi_market::projection` 独立拥有已验证多市场产物到报告 JSON 的投影、market id 列表与相对路径解析；它复核 ReplayVerifier、质量 manifest、CanonicalRunSpec、RunManifest、权益/成交/事件流。 `qx-cli::report_multi_market::render` 独立拥有单次/对照 HTML 模板渲染及权益/成交/事件 payload 降采样；父 facade 只串接报告构造与渲染。 Python `qianxing.app` 保留稳定模型/用例导出，SDK doctor/安装兼容性探测归 `qianxing.sdk_diagnostics`；共享 JSON HTTP 传输归 `qianxing.client_transport`；远端 HTTP 句柄与 `AppClient` 实现在 `qianxing.client_remote`，Paper/Live 控制客户端归 `qianxing.client_trading`，本地 `AsyncCoreClient`、`RunHandle`、`AsyncRunHandle` 与 `RunEvent` 归 `qianxing.client_local`；三者均通过兼容导出保持 `qianxing.app` / 分层 SDK 公共导入。`qianxing.cli` 保留命令分派与兼容私有导入；策略回调诊断输入的 BarFrame 校验、多市场 slot 解析和输入路径发现集中在 `qianxing.strategy_diagnostics`。实验候选展开、配置恢复和逐候选偏差诊断集中在 `qianxing.experiment_diagnostics`，经显式回调注入复用前者与 qianxing-strategy 诊断服务。实验报告摘要投影集中在 `qianxing.experiment_summary`；CLI 保留 `_experiment_summary` 兼容门面。`qianxing.experiment_strategy` 集中多市场候选到策略工厂的适配；CLI 保留原兼容门面；策略身份解析、manifest 参数绑定与 warmup 包装统一委托给 `qianxing.strategy_execution`。这些职责模块都不反向导入 CLI。

`qx-app::cases::run_experiment::recovery` 独立拥有搜索候选 checkpoint 读写/恢复、既有报告身份/产物复核、schema 补齐/复用和 retryable 失败分类；实验编排 facade 只组装恢复请求并将不可复用结果交回候选循环。 `qx-app::cases::experiment_fold_search::checkpoint` 独立拥有折内候选与成本情景 checkpoint 的身份校验、产物复核、序列化/持久化及 artifact fingerprint；fold-search facade 只消费这些恢复接口并负责候选评分与选择。 `qx-app::cases::run_multi_market_depth_experiment::identity` 独立拥有多市场盘口实验 CanonicalRunSpec 与 RunManifest 组装；`persistence` 负责标准报告/RunRecord 发布与读回复核；`candidate_verification` 负责候选运行产物身份/EventLog 复核及既有实验恢复，父模块保留搜索、DTO 和稳定编排入口。`qx-app::cases::run_multi_market_experiment::recovery` 独立拥有既有多市场 Bar 实验报告的规格/数据身份复核、完整 envelope 读回复核与 legacy 报告升级；编排 facade 只组装恢复请求并在无可复用结果时进入候选搜索。 `qx-app::cases::run_depth_experiment::identity` 独立拥有深度实验的 CanonicalRunSpec 身份构造；`run_depth_experiment::persistence` 独立拥有候选产物复核、结果/RunRecord 发布、既有结果升级与读回复核，父模块保留稳定 DTO 和编排 facade。 `qx-app::cases::run_experiment::persistence` 独立拥有实验 RunManifest 构造、报告/RunRecord 发布与读回复核，以及候选回测结果核验；实验编排保留旧调用语义并通过兼容模块入口委托。 `qx-app::cases::run_experiment::identity` 独立拥有实验 CanonicalRunSpec 组装、精确整数规范化、结果 envelope 校验和稳定报告哈希；Bar/Depth 与多实验用例继续通过原 `run_experiment` facade 复用这些函数。 `run_experiment::report_assembly` 独立拥有候选比较、OOS/基准/折结果投影和 RunExperimentResult 装配，再委托 persistence 发布与复核。 `run_experiment::cost_sensitivity` 独立拥有成本情景/邻域回测、checkpoint 复用与写入、训练窗和 OOS 折 delta 评分；主编排只传递已选候选、搜索轨迹、策略工厂和输入身份。 `run_experiment::candidate_execution` 独立拥有离散/连续 proposal、逐候选配置与 checkpoint 复核/写入、早停/训练指标及搜索反馈；facade 传入预检后的数据窗和身份，并把候选、组合及搜索轨迹交给成本敏感性与报告阶段。`qx-app::cases::experiment_search::continuous` 独立拥有连续分段参数的随机/TPE/Pareto proposal、核密度与连续 trial 状态；共享 seeded `SplitMix64` 归 `experiment_search::rng`，离散搜索主体沿原 `experiment_search` 门面调用。 `experiment_search::neighborhood` 独立拥有离散/连续邻居枚举、训练窗覆盖统计与 drawdown assessment，版本化邻域 DTO 留在 facade。 `run_multi_market_experiment::search` 独立拥有候选计划及多市场离散/连续 proposal 状态，运行流程通过兼容父模块使用。 `run_multi_market_experiment::candidate_execution` 独立拥有首轮 proposal 迭代、训练候选执行、adaptive trial 反馈和首折搜索报告；facade 把预建网格与策略注入回调交给该阶段，再将候选计划/结果传给 fold execution。 `run_multi_market_experiment::preparation` 独立拥有目标策略槽解析、候选网格、联合 Bar 时间轴、折执行上限与首训练窗口 preflight；`report_assembly` 独立拥有最终 DTO、取消门和经 artifacts owner 的发布/读回复核。 `run_multi_market_experiment::result_mapping` 独立拥有候选排序、折报告 DTO 和失败候选映射。 `run_multi_market_experiment::fold_execution` 独立拥有 walk-forward 折训练、训练窗候选筛选/选择、样本外执行和折级取消传播；编排 facade 传入首折候选、计划、联合时间轴及注入策略回调并消费折报告。 `run_multi_market_experiment::spec` 独立拥有输入/结果 JSON contract 与规格校验；`candidate_spec` 独立拥有逐候选/fold run id、输出目录、target slot 和参数编译；`factory_binding` 独立拥有自定义 Strategy factory 调用、原始内置 slot 身份合同绑定及注入校验；版本化 DTO 留在 `types`。`qx-cli::report_html::event_trace` 独立拥有摘要 EventLog digest/count 校验、可复用事件类型到订单 ID 映射与有界内嵌事件表；report_html facade 保留 read_event_trace/event_kind_identity 的稳定兼容导出。 `qx-app::report_read::model` 独立拥有 Bar/Depth 核心报告、provider 报告和事件分页 DTO 及其版本常量；`report_read::artifact_views` 独立拥有核心报告组装、CSV 投影和 bounded ArtifactReader primitives；`report_read::standard` 独立拥有单次 Bar/Depth 与 Provider 报告读取；`report_read::single_event` 独立拥有单市场 EventLog 重放和有界报告分页；`report_read::multi_market_events` 独立拥有多市场 EventLog 重放、分页、订单归属和 legacy 事件报告流程。`report_read.rs` 保留因果 Bar 上下文与兼容门面，reader 和 model 模块由它统一重导出，`qx-app` crate 根的既有导出保持不变；DTO 模型不依赖存储适配器或用例实现。

`qx-cli` 是单个 binary（决策：不为拆进程而拆 crate），内部按职责分文件；Strategy worker 的 live admission 判据独立位于 `workers/strategy_worker/admission.rs`，worker 主体消费对账、命令结果、重试与 warmup 判定：命令语法与命令表只有一份，在 `cli_args.rs` 由 clap 派生（V10 P2b，旧手写字符串解析已整体删除、不留双轨），`cli.rs` 保留对 `Command` 的一次显式 `match`，未识别的命令或未知参数只回错误正文、clap 给出的最接近入口建议与该入口自己的 `Usage:`，再补两行「下一步」，仍以退出码 2 fail closed（改前每条用法错误甩出的是整篇入口摘要：实测 162 行 / 12,319 B（`logs/s691_pass28_before_fix_error_wall.txt`，改前 debug 树上五条命令 162–164 行）；V13 R2 #257/#258 收成未知入口 8 行 / 289 B、参数形状不对 10 行 / 359 B，退出码仍 2），`tools/check_architecture.py` 校验「clap 命令表 ≡ `cli.rs` 分支集合 ≡ help 印出的入口」；`worker_entry.rs` 用类型系统里的 `WorkerRole::is_venue_role()` 加一张 `VenueEntry` 登记表同时服务 `ccxt-worker` 与 `binance-worker`，新增角色只需在登记表上补一条 Venue 绑定判定；跨语言子进程的 Python 解释器统一由 `QX_PYTHON` 解析（缺省 `python`）。

Strategy worker 的 `commands.rs` 独立拥有 Pause/Resume 控制命令补队列、租约 fencing、控制面终态回写/确认与 StrategyRuntime 状态迁移；worker 主循环只传递当前控制面、命令队列和策略运行态。 `live_input.rs` 独立拥有实时输入 digest 检查、EventLog pending batch 收集、无策略事件范围的 cursor 确认、live Strategy Job 构造/游标范围绑定及 job/digest 持久化；主循环负责组装宿主能力并调用此阶段。`execution_preparation.rs` 独立拥有普通 Strategy job 的 live manifest 校验、初始 stale gate、EventLog 范围/事实装配、checkpoint bootstrap 判定和 instrument 解析；stale job 由主循环计数并跳过，后续评估/intent 提交仍复用同一 lease context。`strategy_evaluation.rs` 独立拥有 contract adapter dispatch、checkpoint restore/warmup host lifecycle 证据更新与 live readiness 判定；worker 保留 StrategyJob lease 与执行输入并消费其投影结果。`order_intents.rs` 独立拥有 readiness checkpoint-only 判定后的 contract intent/target-quantity 到 Order 投影、spread-group 身份生成、Strategy order quota 保留与 submit command 构造；调用端在二次 stale gate 后才保留 commands 并写 spread-group side effect。`strategy_submission.rs` 独立拥有普通 outcome 的 PreparedEvaluation write-ahead、spread-group snapshot 持久化、逐命令 lease/stale/reconciliation gate、SubmitOrder 持久化与 command result journal 回写，并标识可重试的未决 reconciliation。`strategy_outcome.rs` 独立编排普通 Strategy Job 的 execution preparation、adapter evaluation、二次 lease/stale gate、order intent、submission、共享 finalize 及失败分流；facade 只传入运行时依赖并消费 Retry disposition，保持已提交 cursor/checkpoint 不被失败回写覆盖。`job_recovery.rs` 独立拥有 queued job 的 Scheduler 终态识别与确认、已提交 cursor/checkpoint 的 fencing lease 重领、非 live JobRun 恢复终态写回及 PreparedEvaluation 清理；`prepared_recovery.rs` 独立拥有 PreparedEvaluation 的 job/facts/config 身份复核、fencing lease 重领、lease token 升级与 journal 持久化；`prepared_command_replay.rs` 独立拥有逐条 prepared command 恢复提交、live intent fencing/reconciliation 与 command result journal 持久化；`prepared_finalize.rs` 独立拥有普通 Strategy outcome、PreparedEvaluation recovery 与 checkpoint-only live bootstrap 共用的 checkpoint/cursor commit、非 live JobRun 终态、queue acknowledgement 与 journal 清理收口；bootstrap 保留先清 journal 再 ack，普通 outcome 先清当前 journal 再收口，PreparedEvaluation recovery 保留先 ack 再清单次 journal。策略 worker 的 `lifecycle.rs` 独立拥有执行结果投影、失败 disposition 判定、失败前 fencing lease 重领和 Scheduler JobRun 失败落盘，以及成功终态映射；Strategy worker 的 `execution_updates.rs` 负责子模块兼容门面；`execution_updates/checkpoint.rs` 负责 checkpoint 身份、lease fencing 与 revision 状态迁移；`execution_updates/execution_facts.rs` 负责有界 EventLog 范围装配及账户/行情 DTO；`execution_updates/cursor_state.rs` 独立拥有版本化 cursor/checkpoint 状态、迁移、fencing 校验和持久化；`execution_updates/prepared_evaluation.rs` 独立拥有有界 PreparedEvaluation journal 持久化，`execution_updates/replay.rs` 独立拥有 EventLog Order/Fill 到 Strategy 更新 DTO 的确定性投影。worker 保留对 cursor、journal、事实装配及 replay 子模块的兼容调用路径。

`prepared_finalize.rs` 在普通执行成功路径中也统一负责 commit 前的 owner lease refresh；PreparedEvaluation recovery 与 checkpoint-only bootstrap 继续复用当前 fresh lease，具体调用方式由请求上的 `refresh_lease_before_commit` 显式区分。

`cases/run_multi_market_backtest/report_persistence.rs` 独立拥有已验证 PortfolioBarSessionReport 的多市场产物 assembly、canonical RunManifest、exact equity/fill/event trace 输出、既有产物冲突检查与完全相同结果的幂等修复；执行 engine 的 `run_multi_market_backtest.rs` 只把已校验 report 和身份材料传给持久化阶段。

`cases/run_multi_market_backtest/market_preparation.rs` 独立拥有逐市场 BarFrame 读取与 instrument/行数验证、dataset manifest quality、合约/保证金/费用投影、策略槽装配及 strategy/data fingerprint；run facade 将生成的 markets、routed strategies 和身份材料连接到共享账户 PortfolioBarEngine。

`cases/run_multi_market_backtest/engine_execution.rs` 独立拥有共享账户 `PortfolioBarEngine`/lifecycle 组装、funding input、取消传播及 execution-window 的 trade-start 策略 gate；engine facade 连接准备、身份计算、执行和已验证报告持久化。

`qx-cli::report_html::strategy_bias` 独立拥有策略正确性诊断面板的 bounded PIT、前视与预热发现投影及 HTML 转义；report_html facade 保留页面组装并调用该 section。

`qx-cli::report_html::fills` 独立拥有四种成交 CSV 行形状解析、精确数量/价格/费用校验、成交身份投影、bounded tail reader 和成交表格转义渲染；HTML facade 与 XLSX / 多账户报告 reader 共用同一类型和读点。
`qx-cli::report_xlsx::strategy_diagnostics` 独立拥有策略正确性诊断摘要工作表、完整 JSON 证据工作表与 UTF-16 安全分片；XLSX facade 按既有流程追加这些 sheets，通用 Cell/Sheet 模型仍由 facade 定义。

`qx-cli::report_xlsx::rows` 独立拥有 Overview、Equity、Fills 与 Benchmark 工作表的类型化行投影；XLSX facade 保留运行输入装配、可选表选择和工作簿发布。

`qx-cli::run_evidence_multi_account_depth::paths` 独立拥有多账户深度证据包的相对路径规范化、包内文件/缺失输入 containment 校验及便携账户产物路径投影；证据 facade 通过该模块处理路径，不改变 staging、验证和独立复算顺序。

`qx-cli::run_evidence_multi_account_depth::verify_inputs` 独立拥有多账户深度包逐账户输入清单的全量覆盖、账户/规格路径对应、既存与声明缺失状态以及输入/质量清单摘要验证；父 workflow 在原验证阶段调用该模块。

`qx-cli::run_evidence_multi_account_depth::validation` 独立拥有批次/规格身份、稳定规格指纹、成功账户身份、账户 Depth Evidence 与来源 summary 所属账户核验；pack 与 verify 经 facade 共享这些校验，原调用顺序保持不变。

`qx-cli::run_evidence_multi_account_joint::portable_paths` 独立拥有联合账户证据包的市场路径重定位、父级/子级 artifact identity 绑定与 `ArtifactReader` 路径映射；pack/verify/recompute facade 保留原复核与发布顺序。

`qx-cli::run_evidence_multi_account::paths` 独立拥有多账户 Bar Evidence 的相对路径约束、缺失输入/文件 containment、summary bundle 定位和便携 artifact 路径投影；`verify_inputs` 独立拥有 v1/v2 manifest 输入清单的账户覆盖、缺失状态与 SHA-256 核验，facade 保持校验先于独立复算。

`qx-cli::run_evidence_multi_account::validation` 独立拥有成功账户结果与规格身份检查，以及账户 Evidence 与批次结果、规格和 BarFrame 类型的身份对照；pack/verify facade 在原校验位置调用，复核与发布顺序不变。

`qx-cli::strategy_host::providers` 独立拥有 Python local/E2B/LLM sandbox/Microsandbox/OpenSandbox worker 选择与 provider 环境配置；provider credentials 继续单独进入 host-provider environment，host facade 仍负责策略能力注入、启动标记、worker 生命周期与响应协议。

`qx-cli::report_command::verified_summary` 独立拥有支持版本的 summary schema gate、Bar/Depth summary 到共享 report DTO 的校验重建、artifact location identity 比对和 qx-app readback；report command 以稳定重导出供 comparison command 共用，并保留输出/benchmark/HTML/XLSX 编排。

`qx-cli::report_command::workflow` 独立拥有 report 家族分派、摘要读面复核、benchmark/diagnostics 投影以及 HTML/XLSX/CSV/Evidence 产物的原子提交顺序；`report_command` 仅保留参数入口和稳定 workflow 重导出。

`qx-cli::report_command::diagnostics` 独立拥有有界策略诊断文件读取、策略/标的/复核数据指纹身份校验，以及 app Bar 报告诊断展示投影；report command 保留其输入选择及原有调用位置，并通过兼容重导出供其他报告复用同一身份校验。

`qx-cli::strategy_host::builtin` 独立拥有内建策略的 Runtime 配置、账户/市场绑定、双腿 BarFrame 读取、按时间排序的 MarketEvent 投递与统一 contract 输出；host facade 继续重导出稳定 `invoke_builtin_strategy` 入口。

`qx-cli::strategy_host::c_abi` 独立拥有 C ABI 插件的信任准入与 capability-verified 动态加载、初始化/checkpoint/订单/成交/账户回调归约及错误事件映射；host facade 保留稳定 crate-private 重导出，ExecutionCallback 排序随适配器归位。

`qx-cli::strategy_host::python_client` 独立拥有 Python worker 的 provider policy 接线、隔离环境组装、启动标记、capability acknowledgment、请求/响应与 durable checkpoint 生命周期；facade 保留既有 client 类型及 invoke 入口重导出，provider 配置仍归 `providers`。

`qx-cli::strategy_host::process_client` 独立拥有通用 Strategy 子进程 spawn、reserved capability environment、host-provider environment 和 capability acknowledgment 清理/核验；Python client 调用该模块完成进程创建，共享 C++/外部进程入口仍由 facade 类型兼容导出。

`qx-cli::strategy_binding::contract_input` 独立拥有 StrategyContractInput 的构造、更新合并、账户执行持仓投影、市场 Bar/Event 加载及事实指纹计算；strategy_binding facade 重导出稳定构造入口，策略 evaluator 继续留在 facade。

`qx-cli::run_evidence_multi_account_depth::recompute` 独立拥有多账户深度 Evidence 的构建 revision 绑定、逐账户包内输入重定位、隔离 staging 复算及结果身份对拍；verify facade 先完成输入清单/Evidence 校验，再按请求进入此模块。

`qx-cli::run_evidence_multi_account_joint::recompute` 独立拥有联合账户 Evidence 的父 RunManifest/code revision 绑定、市场输入包内定位、隔离目录复算及结果/RunManifest/便携报告汇总对拍；facade 保留账户 Evidence 与便携报告校验，并在二者通过后才进入复算。

`qx-cli::run_evidence_experiment_recompute::single_market_bundle::bundle_io` 独立拥有实验包源 locator 的歧义解析与去重复制、portable `ArtifactReader` 的登记引用/摘要读取与 containment；单市场 pack/verify orchestration 继续负责校验顺序和报告身份。

`qx-cli::run_evidence_multi_account_depth::pack` 独立拥有批次来源 Evidence 的先验复核、账户便携导出与再次复核、成功/失败账户输入及质量清单打包、portable fingerprint 重建和 manifest staging 原子发布；facade 仅保留稳定 CLI 入口。

`qx-app::multi_market_report::pnl_contributions` 独立拥有逐标的 PnL 曲线的有界采样/精确时间轴校验、市场集合匹配、共享权益重建及期末 Ledger/冻结市场规格重估；原 `multi_market_report` 路径继续重导出两个稳定公开 verifier。
`qx-app::multi_market_report::sidecars` 统一拥有多市场 Bar 权益/精确权益、成交 sidecar 与 replayed/streamed Filled 序列核验；应用层公开函数及 `VerifiedMultiMarketBarSidecarsV1` 在原 `multi_market_report` 路径稳定重导出。
`qx-app::multi_market_report::inputs` 统一拥有多市场质量记录集合、BarFrame/DepthFrame 内容身份和 DatasetManifestV2/DatasetSnapshotV1 回放适用性校验；输入 DTO 与验证函数仍由原 facade/lib 重导出。
`qx-app::multi_market_report::identity` 统一拥有与本地路径无关的 Bar/Depth 规格/策略指纹和按配置顺序生成的输入集身份；公共 DTO、函数及内部策略指纹适配均由原 facade 稳定重导出。
`qx-app::multi_market_depth_research_report::equity` 统一拥有完整权益曲线与精确金额的收益/回撤复算、交易额/费用明细和 Filled sidecar 对账；报告主流程只消费已核实的权益结果。
`qx-app::multi_market_depth_research_report::quality` 统一从 artifact/market_data 收集质量记录、核验声明市场集合和重复项，并投影 clean/issues 质量面板。
`qx-cli::report_multi_market_depth::publication` 统一负责多市场 Depth HTML/XLSX/CSV 渲染和暂存、Evidence 发布顺序及输出路径保护；Depth 报告 facade 保留参数校验、报告排序/对照和结果展示。
`qx-cli::report_multi_market::publication` 统一负责多市场 Bar Evidence、HTML/XLSX/CSV 生成与暂存，并保留 Evidence 先于报告文件提交的顺序；Bar 报告 facade 保留输入核验、对照排序和报告构造。
`qx-cli::app_commands::report_readers` 统一承接版本化 Bar/Depth/Provider/多账户/多腿/多市场报告和事件页的 CLI 输入解析、qx-app 调用及 DTO 序列化；app command facade 保留命令分派和统一 stdout/stderr 处理。
`qx-cli::app_commands::use_cases` 承接 app CLI 的数据集校验、Bar/Depth/Provider 回测、运行复核、风险/算法计划和实验用例适配；调用上下文与构建 revision 仍由 facade 统一生成，所有 handler 保持单次 qx-app 调用路径。
`qx-cli::report_xlsx::multi_account` 独立负责多账户 Bar/Depth 工作簿的账户、权益、成交、事件及诊断 sheet 投影和 workbook 写入；`report_xlsx` 保留原 crate 函数重导出，调用点不变。
`qx-cli::report_xlsx::multi_market_depth` 独立负责多市场 Depth 单次与对照工作簿的 Overview/DataQuality/Ranking/Benchmark sheet 投影及诊断表组装；`report_xlsx` 稳定重导出既有写入函数。
`qx-cli::config_commands::doctor` 独立组装静态 runtime doctor 检查、分层 readiness projection 与 capability query 输出；配置命令 facade 通过 crate 重导出保留已有 CLI 和 API/Runtime 调用点。
`qx-cli::config_commands::configuration` 独立执行配置 explain、fingerprint 和显式 force 的 config lock 物化；facade 保留原命令函数导出与路由。
`qx-cli::config_commands::status` 只读解析最近回测摘要并投影 runtime/backtest status，明确输出不访问网络、不发送订单；配置 facade 保留 CLI 与 report workflow 使用的原函数导出。
`qx-cli::report_benchmark::csv` 负责基准单次/多运行 CSV 序列化、run identity 列和 artifact sink 发布；`report_benchmark` 稳定重导出原写入 API，保留分析逻辑边界。
`qx-cli::cli::strategy_command` 承接 strategy 子命令的策略初始化、质量清单回测和 Bar/Depth/多市场/多账户诊断路由；顶层 CLI facade 保留唯一 `Command` 分派和统一错误/横幅行为。
`qx-cli::cli::config_command` 承接配置 Validate/Explain/Fingerprint/Lock 子命令路由与 reference validation 输出；顶层 facade 保留唯一 `Command` match 和统一 banner 行为。
`qx-cli::cli::backtest_command` 承接统一回测、Builtin/MultiBuiltin、CCXT Builtin、depth backtest、内置策略列表和快速批量回测入口；顶层 facade 只保留路由，builtin 专用执行仍由 `backtest_builtin` 复用。
`qx-cli::cli::worker_command` 承接 Scheduler/Strategy/Paper/Binance/CCXT worker 与 NATS Outbox/EventConsumer/DLQ CLI adapters；feature 不可用时的明确失败提示仍与相应命令同 owner。
`qx-cli::cli::execution_command` 承接 Binance 公私有探测与下单、CCXT 下单、Paper 下单/主链路验收和 Binance 对账 CLI adapters；网络枚举校验、报价参数约束和失败退出码在此集中维护。
`qx-cli::cli::runtime_command` 承接 Doctor、统一 `run` 子命令、Status、LiveCheck 与 RuntimeCheck 分派；能力查询过滤、默认配置路径、参数重组和既有失败出口留在同一 owner。
`qx-cli::cli::dataset_command` 承接数据集摄取与 bundle 保存、Bar/Depth 数据验证、快照发布和 Plan 规格读入；现有参数结构、可选快照上下文和统一 usage 错误出口在该模块集中维护。
`qx-cli::cli::market_data_command` 承接 CCXT 市场规格拉取和 OHLCV 历史数据获取；交易所配置、instrument、时间窗、timeframe 和输出路径按原有顺序传递，并保留命令错误出口。
`qx-cli::cli::project_command` 承接 init、quickstart 和 guide 路由；运行时配置默认文件名、空策略/场景参数拒绝、force 与 HTML 参数保持原有语义。
`qx-cli::cli::host_command` 承接 Serve/Console 服务入口、进程监督与恢复子进程路由；监督器 Usage/WorkerExit 的原始退出码映射在该 owner 中固定。
`qx-cli::cli::selfcheck_command` 承接默认启动自检、Ecosystem/Paper smoke、全量 All 与内核 Verify 命令；统一自检输出失败码和横幅前置顺序由该 adapter 族维护。
`qx-cli::run_evidence_multi_account_joint::validation` 统一校验联合账户结果与规格身份、稳定规格指纹及子账户 Evidence 的 run/result/data/market identity；pack 与 verify 通过 facade 共用该规则。
`qx-cli::run_evidence_multi_account::recompute` 独立执行多账户 Bar Evidence 的隔离批次复算：校验父 RunManifest revision、重定位包内 Bar 输入、运行 app backtest 并对拍批次摘要、规格指纹、账户顺序及子结果身份；facade 保持输入清单核验先于复算。
Bar 多账户验证 owner 同时校验账户 summary 的 Evidence 类型、summary artifact containment 与账户归属，pack 和 verify 复用该入口。
`qx-cli::run_evidence_multi_account::io` 统一负责 EvidenceBundle 读取、create-new 写入、SHA-256 文件核验与 staging 目录 RAII 清理；pack/verify facade 保留业务编排并复用该 IO owner。
`qx-cli::run_evidence_multi_account_joint::io` 唯一拥有联合账户 EvidenceBundle 读取、摘要校验、同步 create-new 写入和 staging 目录回收；recompute 与 facade 复用其 staging 序列及文件操作。
`qx-cli::run_evidence_multi_account_depth::io` 拥有深度 EvidenceBundle 读取、create-new 写入、SHA-256 校验及 staging RAII 生命周期；pack、verify、recompute 共享该模块的文件和 staging helper。
联合账户 `portable_paths` 还负责 package-relative path fail-closed 校验、canonical file/dir containment 与 exchange/account 目录键，pack/verify/recompute 通过 facade 共用这些规则。
`qx-cli::run_evidence_multi_account_joint::verify` 拥有联合账户便携包完整验证编排：manifest 与批次身份、逐账户子 Evidence、便携报告汇总，并在此前均通过后按需调用独立复算；根 facade 保留稳定入口。
`qx-cli::run_evidence_multi_account_joint::pack` 独立拥有联合账户包组装与发布编排，依次验证来源联合报告、子账户来源/便携 EvidenceBundle、重定位市场路径和父级工件，最后写 manifest 并原子 rename。
`qx-cli::run_evidence_multi_account::pack` 独立拥有 Bar 多账户便携包组装，校验成功账户来源与便携 Evidence、摘要归属和输入路径后写结果/spec/manifest 并原子发布；facade 保留稳定入口。
`qx-cli::run_evidence_multi_account::verify` 独立拥有 Bar 多账户包验证编排：manifest、批次/规格与成功账户身份、逐账户输入清单、便携 Evidence 覆盖，全部通过后才按需执行隔离复算；facade 保留稳定入口。
`qx-cli::run_evidence_multi_account_depth::verify` 拥有深度批次 Evidence 完整验证编排，依次核验 manifest、批次/成功账户身份、DepthFrame/质量清单输入、子账户 Evidence 与完整覆盖，最后才按需复算；facade 保留稳定入口。
Depth `paths` 还负责从 summary 文件名定位账户 EvidenceBundle，并生成稳定的账户目录键；pack 和 verify 共用同一定位规则。
`qx-cli::run_evidence_multi_account_depth::model` 唯一拥有深度多账户 Evidence 的序列化 manifest DTO、digest/member/input 记录和固定文件名；facade 仅向内部子模块重导出，`deny_unknown_fields` 与质量清单可选字段默认策略保持集中。
`qx-cli::run_evidence_multi_account::model` 唯一拥有 Bar 多账户 Evidence manifest、账户输入/身份记录、schema 版本及固定文件名；facade 仅向内部 pack/verify 子模块重导出，legacy schema 兼容和 serde 默认行为集中不变。
`qx-cli::run_evidence_multi_account_joint::model` 唯一拥有联合账户 Evidence manifest、账户记录、父工件映射、schema 版本及固定文件名；pack/verify 共享 DTO，无需在 facade 维护 wire schema 声明。
`qx-cli::run_evidence_multi_market_depth::assembly` 唯一拥有多市场 Depth 来源产物、RunManifest、逐市场 DepthFrame/质量报告、事件与 sidecar 的核验，以及 EvidenceBundle 组装和持久化；原 CLI 函数名保留为薄委托。
`qx-cli::run_evidence_multi_market_pack::assembly` 唯一拥有多市场 Bar 产物身份、事件重放、输入指纹与 EvidenceBundle 组装/持久化；遇到 Depth 产物仍路由到 Depth assembly，原 CLI 写入入口保持稳定委托。
`qx-cli::run_evidence_multi_market_recompute` 唯一重建多市场 Bar 输入/质量清单身份并执行隔离复算；`run_evidence_multi_market` 只通过稳定重导出供 Evidence verify 使用。
