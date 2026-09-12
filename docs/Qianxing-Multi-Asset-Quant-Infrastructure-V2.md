# Qianxing Multi-Asset Quant Infrastructure V2

## 1. 目标

Qianxing V2 的目标不是继续堆叠策略脚本，而是把现有确定性 Rust Kernel、研究、回测、执行与运维模块收敛为可复现、可审计、可扩展的多资产量化基础设施。

核心原则：

- 交易事实必须由确定性 Kernel/Domain 契约表达。
- 外部数据源、交易所和插件不得把供应商类型泄漏到 Kernel。
- Backtest / Paper / Live 共享同一套核心事实、订单语义和重放约束。
- 所有研究和回测运行必须绑定数据版本与 fingerprint。
- Python/C++ 只通过稳定 schema/contract 进入系统，不获得 Kernel 可变句柄。
- 热路径不依赖浮点、不依赖系统时间、不依赖无序遍历。

## 2. 当前实现基线

### 已落地

- `qx-core`：确定性时钟、因果事件队列、身份、定点数值、订单状态、Ledger、EventLog、ReplayVerifier。
- `qx-domain`：独立领域契约 crate，已纳入 workspace。
- `qx-data`：独立数据基础设施 crate，已纳入 workspace。
- `qx-runtime`：已有生产运行配置、实盘事件归约、健康状态和策略跨进程契约；已声明对 `qx-domain` 与 `qx-data` 的依赖。
- `qx-factor`：已有 PIT 特征、因子变换、分析报告和研究工件能力。
- `qx-strategy`：已有稳定 Strategy API 和多语言策略边界。
- `qx-execution` / `qx-oms` / `qx-storage`：已有执行、订单、持久化基础设施。

### 本轮新增

`qx-domain`：

- Instrument 不变量校验。
- DomainEvent 不变量校验。
- Order 生命周期状态机，增加 `PartiallyFilled`。
- Trade 契约。
- Append-only Ledger / LedgerEntry 契约。
- Position / Portfolio 校验。
- RunManifest reproducibility 校验。

`qx-data`：

- Canonical `Bar` schema。
- DatasetManifest。
- DataProvider / DataStorage。
- TradingCalendar / CorporateAction。
- Validation + canonical pipeline。
- Deterministic DataCache。
- Incremental merge。
- DatasetRegistry + DatasetResolver。

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
qx-oms / qx-execution
   ↑
qx-api / qx-cli / control plane
```

说明：`qx-core` 仍是已有确定性 Kernel 的事实来源；`qx-domain` 是 V2 新增的稳定业务契约层。迁移期间禁止直接复制 Kernel 行为形成第二套不一致实现，Domain 只固化跨模块契约和不变量，Kernel 继续拥有确定性执行语义。

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
DatasetResolver
      ↓
Factor / Strategy / Runtime
```

规则：

1. Strategy 禁止直接调用外部 Provider。
2. 同一 `dataset_id + version` 不允许注册不同 manifest。
3. Runtime/Research 必须验证 fingerprint。
4. 增量更新以 `(instrument, timestamp)` 为稳定主键。
5. Bar 进入系统前必须通过时间顺序、OHLC、volume 校验。

## 5. Unified Runtime

目标统一：

```text
Backtest
Paper
Live
```

共享：

- 事件身份与顺序。
- RunManifest。
- DatasetRef / fingerprint。
- Order lifecycle。
- Ledger 语义。
- Replay/checkpoint。
- Health / lifecycle。

### 当前状态

`qx-runtime` 现有生产能力已经成熟，但 V2 新增的 `context/event_bus/lifecycle/replay/health/runtime_contract` 需要继续完成 crate 根导出与现有 RuntimeConfig/LiveEventPipeline 的合流。这一项标记为 **Partial**，不能宣称已完成。

## 6. Factor Runtime

目标：

- Factor registry。
- Dependency DAG。
- 公共子表达式共享。
- ExecutionPlan。
- Feature cache。
- Incremental compute。
- PIT enforcement。

现有 `qx-factor` 已覆盖研究工件、因子分析和变换，下一阶段优先在现有 crate 内收敛，而不是另造第二套因子实现。

## 7. Strategy / Portfolio / Risk

### Strategy

统一生命周期：

```text
initialize
on_event
generate_signal
rebalance
finalize
```

现有多语言策略 contract 继续保留，并要求所有输出最终通过 Risk/OMS。

### Portfolio

目标能力：

- allocation
- rebalance
- optimizer
- attribution

### Risk

目标能力：

- exposure
- drawdown
- VaR
- stress
- factor risk

Risk 不允许被策略绕过。

## 8. Backtest / Paper / Live 主链路

```text
Canonical Market Event
        ↓
Strategy
        ↓
Signal / Intent
        ↓
Risk Gate
        ↓
OMS / Order
        ↓
Execution / Fill
        ↓
Ledger / Portfolio
        ↓
Replay / Audit
```

回测不得读取未来 Bar；Paper/Live 不得使用与回测不同的订单生命周期和 Ledger 规则。

## 9. 质量门禁

每个阶段必须通过：

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

关键行为门禁：

- deterministic replay
- no lookahead
- order terminal-state invariants
- ledger append-only / deduplication
- dataset fingerprint lock
- PIT validation
- runtime production fail-closed

## 10. 实施状态

| Phase | 状态 | 说明 |
| --- | --- | --- |
| 0 代码审计 | Done | `QIANXING_CODE_AUDIT_V2.md` |
| 1 Domain contracts | In Progress | crate 已接入 workspace，本轮补状态机/Trade/Ledger |
| 2 Data infrastructure | In Progress | schema/catalog/cache/incremental/registry/resolver 已落地 |
| 3 Unified Runtime | Partial | 依赖已接线，V2 runtime 模块仍需合流到 crate 根 |
| 4 Factor Runtime | Partial | 现有研究能力较强，DAG/ExecutionPlan/增量计划继续收敛 |
| 5 Portfolio/Risk | Partial | 复用现有风险和回测能力，避免重复实现 |
| 6 Backtest/Paper/Live convergence | In Progress | 现有主链路已存在，继续统一新 Domain/Data contract |
| 7 Production hardening | In Progress | CI、fail-closed、replay、跨语言 contract 持续门禁化 |

## 11. 下一批开发顺序

1. 先保持最新 SHA `cargo fmt` 绿色。
2. 收敛 Clippy / workspace tests 的真实红灯。
3. 将 V2 runtime 新模块正式导出并与现有 RuntimeConfig/LiveEventPipeline 合流。
4. 把 DatasetRef/Resolver 接进 StrategyContext 和 RunManifest。
5. 在现有 `qx-factor` 内实现 DAG + ExecutionPlan + cache key + incremental compute。
6. 继续 Portfolio/Risk/Backtest 的同一事件链收敛。

## 12. 禁止事项

- 不以 mock 代替核心链路验收。
- 不降低 CI 门禁来换绿灯。
- 不复制一套与 `qx-core` 不一致的订单/Ledger 执行语义。
- 不允许 Strategy 直接访问 Provider、Venue 或可变 Ledger。
- 不允许插件进入确定性热路径。
- 不在 CI 未执行到真实步骤时宣称通过。
