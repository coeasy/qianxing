# 牵星（Qianxing）最终架构方案 V2：Barter 对齐版

更完整的跨项目比较、可视化终态和实施门禁见：[牵星终极改造计划 V1](牵星终极改造计划-V1.md)。

状态：架构提案，作为 V8 审计方案之后的收敛基线  
审阅基线：`main@57cb62a`（2026-09-14）  
参考项目：[barter-rs/barter-rs](https://github.com/barter-rs/barter-rs)

## 1. 本次修订结论

牵星仍然定位为：

> **Rust 原生、多资产、事件驱动的量化交易引擎内核。**

本次参考 Barter 后，不改变 qianxing 的产品边界，也不复制 Barter 的 crate 名称和交易所实现；重点吸收它在以下方面的工程优势：

1. 用一个明确的 Engine 组合根装配行情、策略、风控、执行和状态，而不是让 CLI 或 Runtime 隐式拼装业务链路；
2. 用索引化、面向数据的状态管理支撑热路径，外部稳定 ID 与内部数组索引分离；
3. 用同一事件驱动引擎覆盖 Backtest、Paper 和 Live，只替换 Feed、Clock、Matcher 和 Execution；
4. 把 AuditStream、状态副本和外部 EngineCommand 作为正式能力，查询、监控和控制不侵入热路径；
5. 用清晰的 `MarketStream`、`ExecutionClient`、`Strategy`、`RiskManager` 等边界降低组件替换成本。

牵星相对 Barter 的差异保持不变：

- qianxing 以 `qx-core` 的确定性事件、定点数值、Ledger、Replay 和多资产交易事实为根；
- qianxing 需要更强的数据血缘、PIT、账户隔离、衍生品规则、跨语言协议和生产审计；
- 因子、指标和研究计算由 finkit 提供，qianxing 只消费版本化研究产物；
- qianxing 的插件首先采用启动期静态装配，非受信任插件不进入交易进程。

## 2. Barter 值得借鉴的架构能力

Barter 将 Engine、Instrument、Data、Execution、Integration 拆为独立库，并通过 Engine 组合统一实时交易、Paper 和回测；其公开 README 还明确展示了索引化 Instrument、可替换的 MarketStream/Execution、Iterator feed、AuditStream、TradingState 和外部命令等能力。[Barter 项目说明](https://github.com/barter-rs/barter-rs)

### 2.1 应吸收的部分

| Barter 优势 | qianxing 的落地方式 |
| --- | --- |
| 清晰的 workspace crate 分层 | 保留 qianxing 现有领域边界；新增“Engine 组合根”职责，不再由 `qx-cli` 承担装配 |
| `IndexedInstruments` 与直接索引 | 为 Instrument、Account、Venue、Strategy 建立 Registry→Index；热路径使用稳定 index，边界层使用 typed ID |
| MarketStream / ExecutionClient 可替换 | 收敛到 `MarketDataPort` / `VenuePort`，并对 Paper、CCXT、Binance、未来券商执行同一 contract tests |
| 统一 Engine 运行 Backtest/Paper/Live | `EngineFeed`、`Clock`、`Matcher`、`ExecutionPort` 可替换，订单/成交/Ledger 归约保持不变 |
| Builder + 明确生命周期 | `EngineBuilder → validate → build → init → run → shutdown`，默认从 `TradingDisabled` 开始 |
| AuditStream + 状态副本 | 从同一 Engine 事实流派生 `AuditStream` 和 `EngineStateReplica`，供 API、监控和报告使用 |
| 外部命令 | 将 Enable/Disable、CancelAll、ClosePositions、Reconcile、Shutdown 变成有序、可审计的 `EngineCommand` |
| Mock/真实组件等价 | Synthetic fixture、Paper Venue 和真实 Venue 必须共享端口和回报契约，不能共享“看起来相似”的旁路实现 |

### 2.2 不应照搬的部分

- 不把 qianxing 的 `qx-core` 降级为只服务于 Engine 的普通数据结构包；交易事实仍由它唯一拥有。
- 不为了追求并行而让多个线程同时修改同一份 Ledger/Order 状态；qianxing 首先采用单写者确定性归约，IO 和数据解析可以并行。
- 不将 `HashMap<String, ...>` 直接放入热路径；外部 symbol 解析只能发生在边界。
- 不把 Barter 的交易所覆盖范围、数值类型或教育用途假设当成 qianxing 的生产保证。
- 不将 `AuditStream` 另建成第二套事实库；它必须是 EventLog/Engine facts 的有序派生流。

## 3. V2 总体架构

```text
┌─────────────────────────────────────────────────────────────────────┐
│ Interface / Control                                                 │
│ qx-api · qx-cli · Python/C++ SDK · qx-control                       │
│ Query / EngineCommand / Subscription                                │
└──────────────────────────────┬──────────────────────────────────────┘
                               ▼
┌─────────────────────────────────────────────────────────────────────┐
│ Engine Composition Root                                              │
│ qx-application::EngineBuilder / Engine                              │
│ validate → build → init → run → shutdown                            │
│                                                                     │
│ Strategy · Portfolio · Risk · OMS · Execution · Audit               │
└───────────────┬─────────────────────────┬───────────────────────────┘
                │                         │
                ▼                         ▼
┌───────────────────────────┐   ┌─────────────────────────────────────┐
│ Deterministic State Core   │   │ Replaceable Feeds / Side Effects    │
│ qx-core · qx-oms           │   │ MarketDataPort · VenuePort           │
│ IndexedState · EventLog    │   │ Backtest · Paper · CCXT · Binance    │
│ Ledger · Replay · Manifest │   │ Storage · Outbox · Reconcile         │
└───────────────┬───────────┘   └──────────────────┬──────────────────┘
                │                                  │
                └───────────────┬──────────────────┘
                                ▼
┌─────────────────────────────────────────────────────────────────────┐
│ Runtime / Operations                                                 │
│ qx-runtime · qx-scheduler · qx-orchestrator · qx-storage · adapters  │
│ workers · health · shutdown · recovery · metrics                    │
└─────────────────────────────────────────────────────────────────────┘

Research Plane (outside trading kernel):
finkit → versioned ResearchArtifact → ResearchBinding → StrategyContext
```

### 3.1 组合根的责任

`EngineBuilder` 是应用装配点，不是新的交易事实来源。它负责：

- 注册并冻结 Instrument、Account、Venue、Strategy 和规则版本；
- 验证 DataSet、finkit artifact、配置 fingerprint 和 RunManifest 的血缘一致性；
- 装配 `MarketDataPort`、`VenuePort`、`RiskPort`、`EventAppender`、`OrderStore`、`ReconcilePort`；
- 选择 `Live`、`Paper`、`Backtest`、`Replay` feed mode；
- 设置 `TradingState`、Audit mode、超时和停机策略；
- 在缺少生产必需能力时 fail-closed，而不是构造一个降级 Engine。

`qx-cli`、`qx-api` 和 Worker 只调用 Builder/Application Service，不再自行构造订单、写 Ledger 或拼接 Venue 流程。

建议的最小接口形态：

```rust
pub struct EngineBuilder<C, M, V, R> {
    config: C,
    market: M,
    venue: V,
    risk: R,
}

impl<C, M, V, R> EngineBuilder<C, M, V, R> {
    pub fn validate(self) -> Result<ValidatedEngineConfig, EngineError>;
    pub fn build(self) -> Result<Engine, EngineError>;
}

pub trait EngineFeed {
    type Event;
    fn next_event(&mut self) -> Result<Option<Self::Event>, EngineError>;
}
```

这里的类型仅表达边界。落地时应复用现有 `qx-application` 端口和 `qx-runtime` 配置，不创建与之平行的第二套 Port/Runtime 类型。

## 4. 索引化热路径状态

### 4.1 两层身份模型

```text
外部边界：InstrumentId / AccountId / VenueId / StrategyId
           │ Registry resolve
           ▼
热路径：InstrumentIndex / AccountIndex / VenueIndex / StrategyIndex
           │ direct index
           ▼
Vec<InstrumentState> / Vec<AccountState> / Vec<VenueState>
```

规则：

1. 外部 ID 永远稳定、可序列化、可审计；内部 index 只在某次 Engine 装配后有效。
2. Registry 完成后，Engine 运行期禁止改变 index 分配；新增 Instrument 必须启动新版本或显式重建状态。
3. 热路径按 index 访问市场、持仓、风险和订单状态；symbol、JSON 和 `HashMap` 只存在于边界/索引层。
4. 所有 index table 保存 canonical ID，EventLog 记录 ID 而不是裸数组位置，保证重放和跨进程传输稳定。
5. 多账户、多 Venue 和多策略状态必须分区存储，禁止用一个“当前账户”隐式切换。

### 4.2 EngineState 的目标形态

```text
EngineState
├── trading_state: Disabled | Enabled | Draining | ShuttingDown
├── clock / sequence / causal_watermark
├── instruments: Vec<InstrumentState>
├── accounts: Vec<AccountState>
├── venues: Vec<VenueState>
├── strategies: Vec<StrategyState>
├── orders: OrderArena / indexed order table
├── portfolios: PortfolioState
├── risk: RiskState
└── manifest / audit_cursor / last_event_digest
```

`EngineState` 是热路径状态；`EngineStateReplica` 是只读副本。后者可以由 API、UI、指标和 Telegram 等非热路径消费者订阅，但不得反向修改 Engine。

## 5. 同一引擎覆盖三种运行模式

### 5.1 可替换组件

```text
                 Backtest       Paper          Live
Clock            TestClock      Test/Wall      LiveClock
EngineFeed       Iterator       MarketData     MarketData
Matcher          Xingban        PaperVenue    External Venue
ExecutionPort    Simulated       Paper         CCXT/Binance/Broker
ReconcilePort    Replay          Local         REST + UserStream
```

以下组件不随模式复制：

- StrategyContext 与 StrategyDecision；
- Portfolio allocation 与 OrderIntent；
- RiskEngine 和 RiskDecision；
- OMS 状态机；
- Fill、Position、Cash、Margin、Fee 的 Ledger 归约；
- EventLog、AuditStream、ReplayVerifier 和 RunManifest。

### 5.2 Feed mode

至少定义四种 feed mode：

1. `Iterator`：同步、可复现的历史/测试输入；用于单测、回测和 golden vector。
2. `Replay`：从 EventLog 重放真实事实；用于恢复、回归和故障分析。
3. `Live`：异步接收 MarketData 和 VenueEvent；用于 Paper/Live。
4. `Hybrid`：历史数据预热后切换实时流；用于盘前初始化和仿真。

Feed 只产生事件，不拥有交易状态。所有状态改变都由 Engine 的有序 reducer 完成。

### 5.3 生命周期与交易状态

```text
build
  → validate
  → init (spawn IO tasks)
  → TradingDisabled
  → TradingEnabled
  → Draining (cancel/close/reconcile)
  → shutdown
```

默认禁止在未完成配置、数据、凭证、风险和对账校验前进入 `TradingEnabled`。`CancelAll`、`ClosePositions`、`Reconcile` 和 `Shutdown` 都必须通过 EngineCommand 进入同一因果队列。

## 6. AuditStream、外部命令与状态副本

### 6.1 三类流

```text
MarketStream      外部市场事实，进入 Engine
CommandStream     外部控制意图，进入 Engine
AuditStream       Engine 处理结果，离开 Engine
```

`AuditStream` 至少包含：

- 输入事件摘要、序号、时间和因果优先级；
- StrategyDecision、RiskDecision、OrderIntent；
- OrderSubmitted/Accepted/Rejected/Fill/Cancel/Unknown；
- Ledger mutation、ReconcileRequired、恢复和停机事实；
- EngineState digest、RunManifest 和错误分类。

它既可以实时消费，也可以落盘为审计/监控/回放输入；但不能绕过 `qx-core` EventLog 形成另一套交易真相。

### 6.2 EngineCommand

```rust
enum EngineCommand {
    EnableTrading { operator: OperatorId, reason: String },
    DisableTrading { operator: OperatorId, reason: String },
    CancelOrders { filter: InstrumentFilter },
    ClosePositions { filter: InstrumentFilter },
    Reconcile { account: AccountId, scope: ReconcileScope },
    Snapshot,
    Shutdown { reason: String },
}
```

命令必须经过权限、幂等键、序列号和审计校验；外部 API 只能投递命令或读取副本，不能直接调用 `Venue.submit` 或修改 `EngineState`。

## 7. crate 收敛方案

| 层 | qianxing crate | V2 责任 |
| --- | --- | --- |
| Facts | `qx-core` | Event、Clock、定点数值、Order、Fill、Ledger、Replay、RunManifest |
| Identity | `qx-domain`、`qx-fenye` | 多资产领域、Instrument/Venue/Account registry 和 symbol 映射 |
| Market | `qx-data`、`qx-guanxing`、`qx-provider`、`qx-datastruct` | Canonical data、质量门、PIT、Dataset、Provider 和列式边界 |
| Application | `qx-application` | EngineBuilder、Use Case、Ports、Command/Query、状态副本适配 |
| Strategy | `qx-strategy` | Strategy trait、跨语言协议、Context、Decision、worker 边界 |
| Portfolio/Risk | `qx-portfolio`、`qx-risk`、`qx-zhenlu` | 组合分配、RiskEngine、路由；逐步删除历史重复门面 |
| OMS/Execution | `qx-oms`、`qx-execution` | 唯一订单状态机、VenuePort、外部副作用、未知结果和恢复 |
| Simulation | `qx-xingban` | Backtest/Paper 撮合、成本、延迟、容量、保证金 |
| Runtime | `qx-runtime`、`qx-scheduler`、`qx-orchestrator` | 进程/Worker 生命周期、feed 装配、健康、停机和调度 |
| Adapters | `qx-adapter`、`qx-storage` | Venue/Provider、EventLog/Outbox/快照/事务后端 |
| Interface | `qx-api`、`qx-control`、`qx-cli` | Query、EngineCommand、配置、鉴权、产品入口 |
| Research boundary | `qx-factor`（迁移期） | 只保留兼容转换和 artifact contract；算法归属 finkit |

依赖规则：

```text
qx-core
  ↑
qx-domain / qx-fenye
  ↑
qx-data / qx-guanxing / qx-provider / qx-datastruct
  ↑
qx-application / qx-strategy / qx-portfolio / qx-risk / qx-oms
  ↑
qx-execution / qx-xingban
  ↑
qx-runtime / qx-scheduler / qx-orchestrator
  ↑
qx-api / qx-control / qx-adapter / qx-storage / qx-cli
```

`qx-application` 可以依赖抽象 Port，但不能依赖 `qx-runtime::LiveEventPipeline` 这样的具体实现；`qx-runtime` 负责把具体 Pipeline 适配到 Port。这样既借鉴 Barter 的组合根，也避免 Runtime 重新夺回业务事实所有权。

## 8. finkit 研究边界

```text
finkit
  Dataset → Factor/Indicator → FeatureArtifact → Report → Publish
                                                      │
                                                      ▼
qianxing ResearchBinding
  artifact_id/version/schema/digest/as_of/data_fingerprint
       → StrategyContext → Portfolio → Risk → OrderIntent
```

qianxing 只校验和消费以下信息：

- artifact 版本、schema 版本和 definition digest；
- Dataset fingerprint、instrument universe 和 symbol mapping 版本；
- `as_of`、训练/验证时间窗和运行时可见性；
- 发布者、签名、质量状态和可复现引用；
- artifact 与 Strategy、RunManifest、EventLog 的 lineage。

迁移规则：

- 新策略默认只使用 finkit artifact；
- `qx-factor` 的 DAG、指标和报告实现不再扩张；
- `target_qty` / `target_snapshot` 仅保留回测/Paper 兼容路径；
- 生产运行没有完整 ResearchBinding 时 fail-closed；
- 迁移结束后将 `qx-factor` 收敛为轻量 contract crate 或从 workspace 移除。

## 9. 实施优先级

### P0：建立 Engine 组合根和统一运行契约

1. 在 `qx-application` 增加 EngineBuilder、EngineFeed、TradingState、AuditMode 和 EngineCommand 的正式边界；
2. 把当前 CLI/Worker 的装配迁移到 Builder，保留兼容入口但禁止新增旁路；
3. 固化 `Iterator`、`Replay`、`Live`、`Hybrid` feed mode；
4. 用同一 golden vector 验证 Backtest、Paper 和 Replay 的订单/账簿结果一致。

### P1：索引化状态和状态副本

1. 建立 `InstrumentRegistry`、`AccountRegistry`、`VenueRegistry`、`StrategyRegistry`；
2. 引入不可变 `IndexLayout` 和 `EngineState`，先覆盖 Instrument market state、Position、Order 和 Risk snapshot；
3. 为 API/监控实现只读 `EngineStateReplica`；
4. 做基于 workload 的基准测试，用结果决定是否进一步做分区并行，而不是预先引入并发复杂度。

### P1：统一外部控制和审计

1. 将 Enable/Disable、CancelAll、ClosePositions、Reconcile、Shutdown 纳入 EngineCommand；
2. 从同一 reducer 输出 AuditStream；
3. 将 AuditStream 接入 EventLog、状态副本、指标和 CLI 报告；
4. 增加 out-of-order、重复命令、重复回报和重启后的 cursor 恢复测试。

### P2：finkit 和多语言产品化

1. 冻结 ResearchArtifact/ResearchBinding schema；
2. 增加 Rust/Python/C++ 等价 vectors，确保策略只输出 Decision/Intent；
3. 将策略 Worker 的超时、崩溃、内存限制和恢复接到 Engine 生命周期；
4. 非受信任 C/C++ 插件默认独立进程，受信任动态库也必须通过签名和 ABI 门禁。

### P2：性能和可靠性门禁

- Registry→Index 解析基准和 Engine hot-loop benchmark；
- 事件吞吐、p99 延迟、分配次数、回放速度和状态快照大小；
- Paper/Live 断线、未知订单结果、重复回报、停机和恢复；
- PostgreSQL/NATS、备份恢复、fencing、HA 和跨节点能力单独验收，不混入单机完成度。

## 10. 验收标准

### 10.1 架构门禁

- `qx-core` 不依赖 Runtime、Storage、API、网络和具体适配器；
- Application 依赖 Port，不依赖具体 `LiveEventPipeline`；
- CLI/API 不直接修改 Ledger 或调用 Venue 副作用；
- 单一 OMS、RiskEngine、EventLog、Ledger 和 Replay 真相；
- Registry 建立后热路径不进行 symbol 字符串解析。

### 10.2 行为门禁

```text
Iterator Backtest
  == Replay(EventLog)
  == Paper(MarketData + PaperVenue)
  == Live(External MarketData + Venue)
```

这里的“相等”指核心订单、成交、持仓、现金、费用和风险事实遵守同一语义；撮合价格和外部延迟可以按模式不同，但差异必须写入 RunManifest 和 AuditStream。

- 相同输入、配置、seed 和 artifact fingerprint 产生相同 EventLog digest；
- no-lookahead、PIT、Risk reject-before-side-effect、Order terminal-state 和 Ledger 守恒测试通过；
- EngineStateReplica 落后或断线不影响热路径，恢复后可由 AuditStream 追平；
- External EngineCommand 具备权限、幂等、序列和审计验证；
- Paper、CCXT、Binance、未来券商通过同一提交/回报/对账 contract tests。

### 10.3 性能门禁

性能不设脱离 workload 的固定数字，先发布可复现基准：

- 每秒处理事件数；
- 单事件 p50/p95/p99 延迟；
- 每事件分配次数和峰值 RSS；
- Instrument/Account/Order 索引访问耗时；
- EventLog replay 吞吐和 snapshot restore 时间。

任何“并行优化”都必须证明：结果 digest 与单写者基线一致。

## 11. 仍然明确禁止的退化

- 不在 `qx-cli`、Worker、API 中重新实现 Engine loop；
- 不让 Backtest/Paper/Live 维护不同的 OMS、Risk 或 Ledger；
- 不用状态副本作为交易事实写入口；
- 不以 `HashMap<String, Instrument>` 取代 Registry/Index；
- 不让插件、finkit 或策略直接写入 `qx-core` 可变状态；
- 不把 AuditStream 当成第二套 EventLog；
- 不以 mock 的 Paper 或协议单测宣称真实交易所已验收；
- 不把“支持多线程”当成“交易状态可以无序并发修改”。

## 12. 最终判断

Barter 提供了 qianxing 当前最需要补强的 Engine 产品化骨架：**可组合、可替换、可观察、可控制、可复用**。qianxing 应在此骨架上保留自己的核心优势：确定性、多资产规则、PIT 数据血缘、finkit 研究产物、账户/组合/风险、Ledger、跨语言和生产审计。

最终目标不是“做一个更大的 Barter”，而是：

```text
Barter 的 Engine 组合与数据导向状态
    + qianxing 的确定性交易事实与多资产 Ledger
    + finkit 的研究计算与可发布 artifact
    + qianxing 的跨语言、插件和生产控制面
    = 可回测、可纸面、可实盘、可重放的 Rust 原生交易内核
```

后续开发顺序固定为：

```text
EngineBuilder / Feed mode
  → IndexedState / StateReplica
  → AuditStream / EngineCommand
  → finkit ResearchBinding
  → Backtest/Paper/Live contract parity
  → 性能与恢复门禁
  → HA、更多 Venue 和完整插件隔离
```
