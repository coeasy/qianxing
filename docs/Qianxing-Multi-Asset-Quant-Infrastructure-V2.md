# Qianxing Multi-Asset Quant Infrastructure V2

## 1. 目标

Qianxing V2 的目标不是继续堆叠策略脚本，而是把现有确定性 Rust Kernel、研究、回测、执行与运维模块收敛为可复现、可审计、可扩展的多资产量化基础设施。

核心原则：

- `qx-core` 是交易事实与确定性执行语义的唯一来源。
- `qx-domain` 只提供稳定领域语义、读模型与跨模块契约，不复制 Kernel 的 Order/Event/Fill/Ledger/RunManifest。
- 外部数据源、交易所和插件不得把供应商类型泄漏到 Kernel。
- Backtest / Paper / Live 共享同一套核心事实、订单语义、Ledger 和重放约束。
- 所有研究、因子、策略和回测运行必须绑定不可变数据版本与 fingerprint。
- Python/C++ 只通过稳定 schema/contract 进入系统，不获得 Kernel 可变句柄。
- 热路径不依赖浮点、不依赖系统时间、不依赖无序遍历。
- Production 必须 fail-closed；不得为了 CI 绿色降低生产安全约束。

## 2. 当前实现基线

### Kernel / Domain

- `qx-core`：确定性时钟、因果事件队列、身份、定点数值、完整订单状态、Fill、Ledger、EventLog、ReplayVerifier、RunManifest。
- `qx-domain`：已纳入 workspace，保留 Asset/Instrument、Position、Portfolio 等稳定领域契约；Event/Order/Fill/Ledger/RunManifest 直接复用 `qx-core`，不维护第二套交易事实。

### Data

`qx-data` 已纳入 workspace，并已实现：

- Canonical `Bar` schema。
- `DatasetManifest`。
- DataProvider / DataStorage。
- TradingCalendar / CorporateAction。
- Validation + canonical pipeline。
- Deterministic DataCache。
- Incremental merge。
- Batch loader。
- Range read / upsert storage。
- Canonical dataset fingerprint。
- `DatasetRegistry` + `DatasetResolver`。
- 可序列化、可独立校验的 `DatasetRef`。

### Runtime

`qx-runtime` 已有并继续复用：

- 生产运行配置与 config fingerprint。
- `LiveEventPipeline` 与实盘事件归约。
- 健康状态、worker lifecycle、shutdown。
- StrategyContext 与稳定的跨进程策略 contract。
- Paper / Live 运行时配置与进程边界 E2E。
- Production storage fail-closed：EventLog/Outbox 在 production 必须使用 PostgreSQL transactional backend。

V2 新增：

- `RuntimeDatasetBinding`：把 `DatasetRef` 解析结果锁定到准确的 `DatasetManifest`。
- `RuntimeResearchBinding`：把 Dataset fingerprint、FactorExecutionPlan、StrategyContext 和 `qx-core::RunManifest` 锁到同一数据血缘和 PIT 时间点。
- Dataset / Factor / Strategy / RunManifest 任一 fingerprint 或 as-of 不一致均 fail-closed。

此前新增但未进入真实 crate 的 `context.rs`、`event_bus.rs`、`health.rs`、`lifecycle.rs`、`replay.rs`、`runtime_contract.rs` prototype 已全部删除。它们与已有 RuntimeConfig、HealthRegistry、LiveEventPipeline、qx-core EventLog/ReplayVerifier 重复，继续导出会形成第二套 Runtime 真相。

### Factor

`qx-factor` 原本已经具备：

- PIT FeatureDefinition / FeatureArtifact。
- FactorCatalog。
- 确定性 dependency DAG / cycle detection / topological execution order。
- FactorReport、CandidateBinding、StrategyResearchSnapshot。
- 研究工件与策略候选血缘校验。

V2 新增并已接入公开 API：

- `FactorExecutionPlan`。
- `FactorPlanNode`。
- deterministic definition digest。
- deterministic cache-key lineage。
- transitive closure，只编译请求因子实际依赖的节点。
- 公共依赖只进入计划一次，形成公共子表达式共享边界。
- `FactorIncrementalProvenance`。
- 时间倒退拒绝。
- 区分 unchanged / PIT cutoff advance / data fingerprint change / graph-definition change。
- fingerprint 或图结构改变时 fail-closed 到 full recompute。

注意：ExecutionPlan 与增量 provenance 已完成，但“只计算 DirtyRange 的实际增量 kernel/materialization worker”仍是下一阶段任务，不能把 provenance 误称为完整增量执行器。

### Execution / Risk / Audit

- `qx-zhenlu` 已有 `RiskContext`、`RiskGate`、MaxQty/MaxNotional/NoShort、保证金、reduce-only、订单规格校验、SignalMerger、TargetPosition、OrderIntent、Router、Venue/PaperVenue。
- `qx-oms` 保持唯一订单生命周期归约入口之一，不新增平行 OMS。
- `qx-genglu` 已有收益、最大回撤、Sharpe 展示指标，以及订单、现金、持仓、费用、资金费、成交等对账能力。
- `qx-xingban` 已有撮合/仿真、成本、延迟、保证金能力。

因此后续 Portfolio/Risk/Backtest 工作以“贯通和强化已有链路”为主，不创建第二套 Risk/Simulation/Audit 实现。

## 3. 目标依赖方向

```text
qx-core
   ↑
qx-domain
   ↑
qx-data / qx-factor
   ↑
qx-strategy
   ↑
qx-runtime
   ↑
qx-zhenlu / qx-oms / qx-execution / qx-xingban
   ↑
qx-genglu / qx-api / qx-cli / control plane
```

约束：

1. `qx-core` 永远是 Event / Order / Fill / Ledger / RunManifest 的事实来源。
2. `qx-domain` 可以定义高层领域语义和读模型，但不得复制 Kernel 状态机。
3. Provider / Adapter / Plugin 必须位于 Kernel 外部。
4. Runtime 负责装配、血缘、运行模式和 worker 生命周期，不拥有第二套交易状态。
5. Strategy 只能输出 Signal / Intent，所有订单必须经过 Portfolio/Risk/OMS/Execution。

## 4. 数据基础设施

标准链路：

```text
External Provider
      ↓
DataProvider
      ↓
Canonical Schema
      ↓
Validation
      ↓
DatasetManifest / fingerprint
      ↓
DataStorage / DataCache
      ↓
DatasetRegistry / DatasetResolver
      ↓
DatasetRef
      ↓
FactorExecutionPlan
      ↓
StrategyContext / RuntimeResearchBinding
      ↓
RunManifest
```

规则：

1. Strategy 禁止直接调用外部 Provider。
2. 同一 `dataset_id + version` 不允许注册不同 manifest。
3. Runtime/Research 必须验证 fingerprint。
4. 增量更新以 `(instrument, timestamp)` 为稳定主键。
5. Bar 进入系统前必须通过时间顺序、OHLC、volume 校验。
6. DatasetRef 的 id/version/fingerprint 必须全部非空，Resolver 必须精确匹配 manifest。
7. FactorExecutionPlan 的 input fingerprint 必须与 DatasetRef 一致。
8. StrategyContext 与 RunManifest 必须继续沿用同一 fingerprint。

## 5. Unified Runtime

统一运行模式：

```text
Backtest
Paper
Live
```

共享：

- qx-core 事件身份与顺序。
- RunManifest。
- DatasetRef / fingerprint。
- FactorExecutionPlan lineage。
- StrategyContext contract。
- Order lifecycle。
- Ledger 语义。
- EventLog / ReplayVerifier。
- Health / worker lifecycle。

### 当前状态

V2 不再创建第二套 RuntimeMode/EventBus/ReplayEngine/HealthSnapshot。真实运行时以现有 `RuntimeConfig`、`RuntimeSupervisor`、`HealthRegistry`、`LiveEventPipeline` 和 qx-core replay primitive 为基础。

Dataset → FactorPlan → StrategyContext → RunManifest 的不可变血缘边界已接入 `qx-runtime`。下一步重点不是继续加 Runtime 类型，而是让实际 worker/materializer 和 Backtest/Paper/Live 都消费同一个 binding。

## 6. Factor Runtime

已完成：

- Factor registry。
- Dependency DAG。
- cycle detection。
- deterministic topological order。
- ExecutionPlan。
- 公共依赖去重。
- definition digest。
- cache-key lineage。
- incremental provenance。
- PIT/fingerprint/as-of validation。

下一步：

1. 把 `FactorExecutionPlan` 接进实际 materialization worker。
2. 为 append-only 数据实现真正的 DirtyRange / incremental compute。
3. 持久化节点 cache artifact，并把 artifact digest 写入研究/运行 manifest。
4. 让 batch compute、streaming compute 和 replay 使用同一 ExecutionPlan。
5. 增加“增量执行结果 == 全量重算结果”的确定性门禁。

## 7. Strategy / Portfolio / Risk

### Strategy

统一语义：

```text
Dataset/Research Snapshot
        ↓
StrategyContext
        ↓
Signal / OrderIntent
```

现有 Rust/Python/C++ contract 保留。Strategy 不直接访问 Provider、Venue、Ledger，不允许绕过 Risk/OMS。

### Portfolio

已有基础：`SignalMerger` + `TargetPosition` + domain Portfolio read model。

下一阶段补齐：

- 多策略 allocation / capital budget。
- target normalization。
- rebalance plan。
- gross/net exposure snapshot。
- concentration limits。
- asset / venue / strategy exposure aggregation。
- optimizer contract 与确定性 fallback。
- attribution lineage。

### Risk

已有订单级门禁：

- max quantity。
- max notional。
- no-short。
- reduce-only。
- instrument spec / tick / lot validation。
- initial margin / available margin。
- projected position notional。

下一阶段补齐组合级：

- gross / net exposure。
- concentration。
- leverage / margin utilization。
- drawdown gate。
- stress scenarios。
- VaR/reporting contract。
- factor exposure / factor risk。

原则：Risk 只能拒绝或明确返回原因，不得静默改变策略业务含义。

## 8. Backtest / Paper / Live 主链路

```text
DatasetRef / Canonical Market Event
        ↓
FactorExecutionPlan / Materializer
        ↓
StrategyContext
        ↓
Signal / Intent
        ↓
Portfolio Allocation / Rebalance
        ↓
Risk Gate
        ↓
OMS / Order
        ↓
Execution / Fill
        ↓
qx-core Ledger
        ↓
Replay / Audit / Attribution
```

硬约束：

- 回测不得读取未来 Bar。
- Paper/Live 不得使用与回测不同的订单生命周期和 Ledger 规则。
- 同一输入 DatasetRef + config + seed 必须可复现。
- Simulation 与 Live 的差异只能位于执行适配/成交模型边界，不得分叉交易事实模型。

## 9. 质量门禁

每个阶段必须通过：

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

当前 CI 还包含：

- Independent deterministic semantics。
- qx-cli sqlite feature build/test。
- qx-cli nats feature build/test。
- qx-cli postgres feature build/test。
- qx-cli postgres+nats feature build/test。
- Runtime/Paper contracts。
- C++ SDK smoke。

关键行为门禁：

- deterministic replay。
- no lookahead。
- order terminal-state invariants。
- ledger append-only / deduplication。
- dataset fingerprint lock。
- FactorExecutionPlan deterministic digest。
- PIT validation。
- RuntimeResearchBinding lineage validation。
- production PostgreSQL transactional storage fail-closed。

在 Runtime/Data/Factor 与 prototype cleanup 完成后，`d8099e3dd98789d72f58e8499ba6820ab9faa400` 已达到同一 SHA 全矩阵绿色。

## 10. 实施状态

| Phase | 状态 | 说明 |
| --- | --- | --- |
| 0 代码审计 | Done | `QIANXING_CODE_AUDIT_V2.md` |
| 1 Domain contracts | Done / 持续演进 | 已接入 workspace；核心交易事实统一复用 qx-core，不再维护平行状态机 |
| 2 Data infrastructure | In Progress | schema/catalog/cache/incremental/batch/storage/registry/resolver/fingerprint 已落地；后续补真实 Provider 与大规模持久化 |
| 3 Unified Runtime | In Progress | Dataset→FactorPlan→StrategyContext→RunManifest 已合流；重复 prototype 已删除；下一步接实际 worker/backtest |
| 4 Factor Runtime | In Progress | DAG + ExecutionPlan + cache lineage + incremental provenance 已完成；下一步真正 DirtyRange materialization |
| 5 Portfolio/Risk | In Progress | 已有 SignalMerger、TargetPosition、RiskGate/RiskContext；下一步组合分配与组合级风险 |
| 6 Backtest/Paper/Live convergence | In Progress | 现有主链路已存在；继续统一 Factor materialization、Portfolio/Risk 与 RunManifest |
| 7 Production hardening | In Progress | PostgreSQL fail-closed、CI、replay、跨语言 contract 已门禁化，继续扩展故障/恢复/压力测试 |

## 11. 下一批开发顺序

1. 将 `FactorExecutionPlan` 接进实际 factor materialization worker。
2. 实现 append-only DirtyRange / incremental compute，并验证增量结果与全量重算完全一致。
3. 将 materialized artifact digest 接入 StrategyResearchSnapshot / RunManifest 血缘。
4. 在现有 `SignalMerger` / `TargetPosition` 基础上增加 Portfolio allocation、rebalance plan、gross/net exposure。
5. 在现有 `RiskContext` / `RiskGate` 基础上增加组合级 concentration、leverage、drawdown/stress/factor-risk 门禁。
6. 把 Portfolio/Risk 结果接入同一 Backtest/Paper/Live 事件链。
7. 扩展 `qx-genglu` attribution / reconciliation，让最终报告可回溯到 dataset、factor plan、strategy、intent、order、fill、ledger。
8. 每一批均要求同一 SHA 的 Format、Clippy、Workspace tests、deterministic semantics、feature matrix、Runtime/Paper、C++ smoke 全部绿色。

## 12. 禁止事项

- 不以 mock 代替核心链路验收。
- 不降低 CI 门禁来换绿灯。
- 不复制一套与 `qx-core` 不一致的 Event/Order/Fill/Ledger/RunManifest。
- 不创建第二套 RuntimeMode/EventBus/ReplayEngine/Health 状态体系。
- 不重复实现 qx-factor 已存在的 DAG/cycle detection。
- 不允许 Strategy 直接访问 Provider、Venue 或可变 Ledger。
- 不允许插件进入确定性热路径。
- 不允许数据 fingerprint / factor plan / StrategyContext / RunManifest 血缘不一致时继续运行。
- 不在 CI 未执行到真实步骤时宣称通过。
