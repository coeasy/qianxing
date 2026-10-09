# 牵星 Qianxing — 项目架构、实现逻辑与缺点分析

> 分析对象：https://github.com/coeasy/qianxing （main 分支）
> 依据：仓库 README、V13 审计与重构方案（`docs/自研量化框架审计与重构方案-V13.md`）、
> `maturity/capabilities.yaml`（更新至 2026-10-09）以及 `qx-core / qx-xingban / qx-zhenlu /
> qx-risk / qx-guanxing / qx-runtime / qx-cli` 等 crate 的源码。
> 文中所有"条数/行数"都是仓库自己某一轮的实测快照（已标注日期），不是我这边跑出来的。

---

## 0. 一句话定位

**牵星不是"一套能赚钱的策略"，而是一台"把结论压在哪些输入上说清楚"的确定性回测与纸面交易内核。**

它的产品目标写得非常克制，也很罕见地诚实：在没有交易所账号、没有网络凭据的机器上，把一份行情 + 一条策略 +
一套假设跑出**能被别人复算的结论**——同样输入必然得到同样结果，并且产物自己说得出这个结论压在哪些输入上。

名字取自明代"过洋牵星术"：先用分级量具（牵星板）测星高，再定纬度。映射到方法论就是
**先给数据分档（L2/L3、L1、Bar），再按档位选撮合模型**；档位不足时**直接拒绝运行**，
绝不从 K 线里读出不存在的盘口，也不静默降级成更宽松的假设。

它明确声明的三条"不是"：

| 不是 | README 原文口径 |
|---|---|
| 不是已对接真实账户的系统 | 四档能力证据里 `sandbox_tested` / `production_approved` **当前没有一条为真** |
| 不是热插拔插件平台 | `qx-plugin` 只做启动期清单注册与静态装配计划，运行时动态加载未实现 |
| 不是一台统一撮合机 | 回测与 Paper 同源的是**规则**（风控/费用/延迟/保证金 + 事件归约），**不是撮合** |

---

## 1. 仓库体量与工程纪律

| 维度 | 读数 | 出处/日期 |
|---|---|---|
| workspace 成员 | 25 个 crate（含 `contract-tests`） | 2026-10-04 |
| 生产代码行数 | 59,857 行（门禁 prod 口径，剔 `#[cfg(test)]` 与 test-scope 文件）；src 全量 89,916 行 | V13 §2.1，2026-09-26 |
| 最大 crate | `qx-cli` 18,040 prod 行（占生产文本 30%，其 src 有 35% 是内联用例） | 同上 |
| 架构门禁 | `tools/check_architecture.py`，895 项不变量（V13 那轮是 463 项，R1-A5 那轮 515 项） | README / V13 |
| 测试 | `cargo test --workspace` 1057 passed / 0 failed / 1 ignored（107 段）；Python 侧 64 tests | 2026-10-04 |
| CLI 入口 | clap 顶层命令表 40 + `help` = 41；门禁校验"clap 表 ≡ `cli.rs` 分支 ≡ help 印出的入口" | 同上 |
| 内置策略 | 17 个（13 个单标的 + 4 个套利 kind） | 同上 |
| 示例配置 | `deploy/` 顶层 52–53 份模板，逐份被生产读法真读一遍 | README |

一个很值得注意的工程特征：**这个项目把"文档说的"和"代码做的"之间的漂移当成一等公民来治理**。
`tools/check_architecture.py` 是一把近 900 项的尺子（单文件 837 KB），本地构建 `build.bat`/`build.sh`
九步里第 `[1/9]` 步就跑它，且排在任何 `cargo` 之前。它管的东西包括：
"生产代码里不得再出现 `"USDT"` 整串字面量"、"clap 命令表 ≡ 派发分支 ≡ help"、
"每个 `pub enum` 变体都要有生产构造点"、"文档里不许出现 64 位十六进制整档摘要"……
连 README 里写错的门禁条数都被单独立案（V13 §4 L3）。

---

## 2. 分层架构

```
┌─────────────────────────────────────────────────────────────────┐
│ 入口层                                                            │
│   qx-cli（单 binary，41 入口）  web/console（只读面板 + 唯一写面） │
│   python/{qianxing_ccxt, qianxing_strategy, qianxing_bridge,     │
│            qianxing_ashare}   cpp/（C ABI SDK）                   │
├─────────────────────────────────────────────────────────────────┤
│ 编排/运行层                                                        │
│   qx-orchestrator（worker 启动计划/子进程生命周期）                 │
│   qx-runtime（拓扑配置、pipeline 归约、supervisor、停机阶梯）       │
│   qx-scheduler（JobSpec/日历/幂等重试）  qx-api（HTTP 读面 + WS）   │
│   qx-control（权限、审计、终态退场）                                │
├─────────────────────────────────────────────────────────────────┤
│ 领域层（交易语义）                                                  │
│   qx-strategy（Strategy API / 内置策略 / C ABI / 共享内存 ring）    │
│   qx-zhenlu  针路：Signal→RiskGate→OMS→路由→PaperVenue            │
│   qx-risk    规则集 + 规范风控上下文（单一判定）                     │
│   qx-execution（Venue 回报归约、SubmitOrder 副作用边界、端口）      │
│   qx-genglu  更路：订单维度对账裁决                                 │
├─────────────────────────────────────────────────────────────────┤
│ 撮合/仿真层                                                        │
│   qx-xingban 星板：Bar 引擎 / L1 Tick 引擎 / L2 订单簿引擎         │
│               + 成本(费用/延迟/保证金) + A 股规则包                 │
├─────────────────────────────────────────────────────────────────┤
│ 数据层                                                             │
│   qx-guanxing 观星（Bar/Tick/质量门/as_of PIT）  qx-data（目录/摄取）│
│   qx-datastruct（列式 BarFrame/Arrow C Data）  qx-provider/qx-adapter│
│   qx-factor（因子/PIT 工件）                                        │
├─────────────────────────────────────────────────────────────────┤
│ 内核层                                                             │
│   qx-core 牵星：Fixed 定点、Ts、身份契约、Event/EventLog、          │
│                 Ledger、Order 状态机、FeeModel、重放校验、FileLock  │
├─────────────────────────────────────────────────────────────────┤
│ 底座                                                               │
│   qx-storage（文件/分段/SQLite/PostgreSQL + Outbox + 审计链）       │
│   qx-protocol（AccountSnapshot/QIFI 边界）  qx-spec  qx-plugin      │
│   qx-python（PyO3 原生扩展）                                       │
└─────────────────────────────────────────────────────────────────┘
```

依赖形状（V13 §2.2）：被依赖最广是 `qx-core`（19 条入边）、`qx-guanxing`（9）；
出站最多是 `qx-cli`（21，装配面）。两条 dev-only 回边（`qx-risk ⇄ qx-zhenlu`、`qx-execution ⇄ qx-runtime`）
被审计文档点名为**"域 crate 之间仍有概念耦合"的机器可读证据**。

---

## 3. 确定性是怎么做出来的（核心机制）

这是整个项目最有价值的部分。README 列了 9 条"设计底线"，源码里逐条有对应：

| 底线 | 实现落点 | 为什么 |
|---|---|---|
| **热路径不用浮点** | `qx-core/src/numeric.rs`：`Fixed/Price/Quantity/Money` 都是 `i128`，`SCALE = 1e9`，乘除走 `checked_mul/checked_div`（`(a*b)/SCALE`、`(a*SCALE)/b`），溢出返回 `Option` | IEEE-754 的 NaN 位模式不确定，会破坏 bit-level 可重放 |
| **内核里没有时钟对象** | `clock.rs` 只有一行 `pub type Ts = u64;`（epoch 毫秒）。时间轴由数据侧闸门决定（`qx-data::process_bars` 排序 + `validate_bars` 拒绝同标的非严格递增）。真实墙钟只出现在 paper/live worker 循环 | 留一个"没人推进过的虚拟时钟"等于承诺一件没发生的事 |
| **顺序敏感处不用无序容器** | `Oms.orders`、`EventLog.seqs` 等一律 `BTreeMap/BTreeSet` | 遍历顺序必须确定 |
| **不用 `DefaultHasher`** | 自实现 FNV-1a（`sourcing.rs`），喂 `u64/i128/bytes/text` | `DefaultHasher` 输出不保证跨版本稳定 |
| **不用外部 RNG** | `qx-xingban/src/rng.rs` 自实现 xorshift64\*，种子 0 时替换为常量 | `rand` 的实现细节会随版本变 |
| **同 ts 按因果优先级排序** | `prio::{TIMER=0, FEEDBACK=1, MARKET=2, COMMAND=3, MATCH=4, APPLY=5, POST=9}`，全序键 `(ts, prio, seq)`，由 `EventLog::validate` / `append_batch` 单点裁决 | 把"策略决策"排到"行情到达"之前 = 偷看未来 |
| **bar t 决策，bar t+1 开盘成交** | Bar 引擎在 `bars[i]` 只喂 `as_of(bars[i-1].ts)` 的历史，`NextBarOpenFillModel` 用 `bars[i].open` 成交 | 从结构上杜绝 cheat-on-close |
| **只读投影不做第二个事实源** | API/状态查看只消费 EventLog 派生快照，写操作一律经 `ControlPlane` 落审计 | 投影不得回写交易状态 |
| **"没算过" ≠ "是零"** | 钱字段用 `Option<i128>`，`None` = 未算，`Some(0)` = 算过且为零；读侧印 `absent` | 兜一个合法 0 等于替交易所报数 |

**事件溯源与重放的最小单元不是收益数字，而是整份 RunManifest**：
`run_id / code_commit / config_hash / data_fingerprint / input_components / clock_start / clock_end /
global_seed / determinism_mode / result_hash / strategy_version / instrument_spec_version /
model_fingerprint / input_event_hash / output_event_hash / runtime_version`。
`ReplayVerifier::verify(events, &Ledger)` 重放整条事件流重建 Ledger，与运行期 Ledger 比对；
两轮运行的 `log_digest` 与 `result_hash` 必须相等。

`EventLog::append_batch` 是**单事务语义**：先在影子状态里把整批的元数据、seq 冲突、因果序、
next_seq 溢出全校验完，全过才提交，失败时 `self` 一字不改——避免"以为整批被拒、实际写进一半"。

---

## 4. 三条主链路的实现逻辑

### 4.1 Bar 档策略回测（`qx-cli backtest`）

驱动固定在 `crates/qx-xingban/src/backtest.rs::BacktestEngine::run`，闭环是
**as_of → 风控 → OMS → 下一根 bar 撮合 → Fill → Ledger → EventLog**。

```
1) 入口校验
   - QualityGate::check(bars)：Fail/Quarantine 直接拒绝（空输入/非单调/负价/high<low/close 越界 → Fail；重复 ts → Quarantine；零成交量 → Warn）
   - DataView::try_new(bars, source) 计算 input_data_hash（FNV-1a）
   - 校验 multiplier>0、instrument_spec 与 instrument 一致、spec.validate()
   - 已给 spec 时 multiplier 必须为 1（否则"同一成交回测与实盘记出不同现金"）
   - data_tier.supports(fill.tier())：档位不符即报错（"撮合模型 X 需要 L2L3 数据，但当前只有 Bar"）
2) 装配模型：fill / fee / latency / margin 四个 descriptor 进 model_descriptors → 进指纹
   A 股规则包另加一条 descriptor，并预生成 issuer_capital_snapshots
3) 开户：Ledger.deposit(initial_cash) → 写 LedgerApplied(APPLY) 事件；可选 collateral 多币种抵押
4) 逐 bar 循环 for (i, bar)：
   a. A 股规则：prepare(ts) / previous_close / set_halted(!is_trading) / set_side_blocks(涨跌停判定)
   b. apply_virtual_events：资金费 / 利息 / 交割 三类业务事件进 Ledger + EventLog
   c. i>0 时取 history = view.as_of(bars[i-1].ts)  ← 策略只能看到 Bar(t-1)
      strategy.on_bar_orders_checked(history, instrument, bar.ts, position)
      （NativeBarStrategy adapter 里还有一道断言：visible.ts >= ts 直接报"收到不可见的当前或未来 Bar"）
   d. 订单校验链（每条都有明确拒绝理由，写进事件流）：
      - 账户/标的不一致 → Rejected
      - A 股规则（T+1、涨跌停、最小手数…）→ Rejected
      - product_policy.validate_for(spec)、reduce_only 判定
      - 保证金：derivative 走 margin.instrument_initial_margin(spec, qty, ref_price, leverage)；
                现货买入走**全额名义额 + 预估手续费**，且只能拿**可用现金**（不是权益）放行
        —— 注释写得很直白：用权益放行等于让策略拿已有持仓当现金继续买，Ledger 会静默透支成负现金
      - RiskGate::check_with_price（参考价用 bars[i-1].close，绝不用当前 bar 的 close）
   e. 通过 → 写 Submit(COMMAND) → Accepted(MATCH) → matcher.submit_at(order, bar.ts)
   f. matcher.on_bar(bar, bar.ts) 出 Fill：
      - FillTerms::resolve(spec, multiplier) → apply_fill_to_books(&mut ledger, &mut oms, &currency, &fill, terms)
      - 写 Filled(APPLY) + 每条 LedgerApplied(APPLY)
   g. 用 bar.close 做 marks 算 equity（equity_for，支持 fx_rates / spec / multiplier）
      - 可选强平：maintenance margin 判定 → 强平单走与策略平仓**同一套手续费**
   h. 记录 equity / benchmark_equity / positions / 更新 max_drawdown
5) 收尾：validate → result_hash = event_log.digest() → 生成 RunManifest
   → 落 summary.json / equity.csv / fills.csv / run.json（+ RunRecord 逐文件 SHA-256）
```

**7 种撮合模型，按数据档位分档（关键设计）**：

| 模型 | 档位 | 假设（写进 descriptor，进指纹） |
|---|---|---|
| `NextBarOpen` | Bar | 假设下一根 bar 开盘可全部成交；不建模排队与容量 |
| `BestPrice` | Bar | 最优价无限流动性——乐观上界，**不可用于容量评估** |
| `OneTickSlippage` | Bar | 固定一档滑点——保守上界 |
| `Probabilistic` | L1 | 触及限价按概率成交（xorshift RNG），建模 L1 下的成交不确定性 |
| `VolumeSensitive` | L2L3 | 最优价容量 = 近期成交量 × 比例；**缺 L2/L3 会高估可得流动性** |

`DataTier::supports()` 是硬闸门：L2L3 可用全部，L1 只能用 L1/Bar，Bar 只能用 Bar。
README 里的原话是"每个撮合模型先回答**我不知道什么**"。

**成本三模型**（`qx-xingban/src/cost.rs` + `cost_rules.rs`）：
延迟 `LatencyModel`（`Zero/Static`，配置里 `delay_ns` 才是纳秒，事件 ts 是毫秒）、
保证金 `MarginRule`（`NoMargin/FixedRate/Leverage/Tiered`，区分 initial 与 maintenance）、
费用 `FeeModel`（`MakerTaker` / `AShareFee`（佣金+最低佣金+卖出印花税+过户费）/ `Zero`）。
费用模型放在**内核**而不是回测层，理由是 `Fill.fee` 是内核事实字段，三个执行平面必须同口径。

**衍生品**：`TradingInstrumentSpec` + `OrderPolicy`（margin_mode / position_mode / leverage / position_side / reduce_only），
支持线性与反向合约（反向合约费用基准 `contract_size / price`，由 `set_inverse_fee_basis` 切换）、
双向持仓（one-way + long/short 拆分）、对冲模式、分级保证金、强平。

### 4.2 深度档回测（`qx-cli backtest book --fill-tier l1|l2`）

- **L2 订单簿内核**（`orderbook.rs` + `orderbook_backtest.rs`）：
  `DepthFrame{schema_version=1, source, instrument, snapshots}`，`OrderBookSnapshot` 强制校验
  （ts/sequence>0、档位价格数量>0、买盘严格降序、卖盘严格升序、买一不高于卖一）。
  `OrderBookMatchingEngine` 逐档吃单，价格优先 + 同价位数量优先；`OrderBookExecutionModel` 提供
  `fee_bps / latency_snapshots / queue_position_bps / market_impact_bps` 四个确定性旋钮。
- **L1 Tick 内核**（`tick_backtest.rs`）：把 bid/ask 首档折叠成 `OrderBookSnapshot` 后**复用同一个引擎**，
  "两者没有第二套订单语义"。`--fill-tier l1` 时帧里多一档盘口就失败。
- 产物与 Bar 链同构（四份同前缀 + 可重算输入指纹）。

### 4.3 Paper 闭环（`qx-cli paper-e2e`）

链路：**调度 → 策略 → 风控 → 队列 → 成交 → Ledger**，全在本机进程内。

- 事实归约： `qx-runtime/src/pipeline.rs::LiveEventPipeline::ingest`
  - 补 `EventMetadata`（source_id / source_kind / dedup_key / rule_version / context）
  - **去重**：dedup_key 命中 或 correlation_id+source_seq 命中 或 语义重放（Accepted/Cancelled/
    ReconcileRequired/AccountCashflow/FillWithSpec）即判重复；Fill 另有 `seen_fills` 台账
  - 外部事件 → `EventKind` + 因果优先级：行情 = MARKET，账户快照/回报 = FEEDBACK，
    成交/账簿 = APPLY
  - 事务边界内同时更新 OMS 与 Ledger；文件后端每次成功归约原子落盘
  - 对共享 EventLog 的并发写入有 3 次重试（按 `QxError::contract().retryability` 五元契约判资格）
- 撮合：`PaperVenue::on_quote` 对每条 `QuoteTick` 用**首档一次性 touch** 产生成交。
  **这不是回测的簿内核**——没有逐档队列、没有排队中的部分成交。README 与源码注释反复强调这一点。
  Paper 与回测真正共享的是 `FeeModel`、`qx_core::apply_fill_to_books` 与 `Ledger`。
- 崩溃窗口恢复：控制面已落终态但队列未确认时，恢复逻辑只清理旧队列、不重复产生副作用。
- 控制面：写操作只有 `ControlPlane`（审计 + 终态退场），Web 控制台只有一个 `POST /control/commands`。

### 4.4 实盘/对账链（代码级契约，非已认证）

- `qx-adapter`：`Binance Spot REST/L1 行情基线`、`CCXT` 适配、`reconcile`。缺凭据即**退出码 3 fail closed**。
- `qx-genglu`：订单维度对账裁决（绩效指标不在这里，刻意不重复实现）。
- 重连预算：Binance 用户流按累计失败次数、CCXT Pro `watch_orders` 500ms 起 / 8s 封顶 / 10 次连续。
- 运行面：`supervise`（跨平台监督器，任一 worker 异常退出则停其余）、`shutdown.rs` 的停机阶梯
  （`Ctrl+C`/`SIGTERM` → `RuntimeSupervisor::request_shutdown` → 超时报 `StopTimedOut`）。

### 4.5 多语言策略

- 统一契约：`qx-strategy::Strategy`（`on_init` / `on_event` → `StrategyDecision` → `to_orders`）。
  策略只消费不可变 `StrategyContext` 与 `MarketEvent`，**不得访问 Venue/EventLog/Ledger/凭证**，
  运行时仍对每个 intent 走 Risk/OMS。
- 三种传输，同一份 `schemas/strategy_api_v1.schema.json` 描述：
  `JSONL`（默认）/ `framed_json`（QXSF 二进制分帧，版本+序号+长度上限+CRC32）/
  `shared_memory_json` / `shared_memory_columnar`（QXCB 定宽列，双向固定槽位 SPSC mmap ring）。
- C++：`c_api.rs` 稳定 C ABI + VTable + SHA256 校验；Python：PyO3 原生扩展 + `qianxing_strategy.worker`。
- 跨语言子进程统一由 `QX_PYTHON` 解析（缺省回落 PATH 上的 `python`）。

---

## 5. 数据平面

`qx-guanxing` 提供 `DataView::as_of(ts)` 的 point-in-time 可见性——**前视偏差在数据层封禁，不靠策略自觉**。
四级质量门 `Verdict::{Ok, Warn, Quarantine, Fail}`：`Quarantine` 可审计但不得进实时信号，`Fail` 阻断启动。
`qx-data` 负责目录/摄取/增量管道/DatasetBundle 与组件指纹（provider 不进内核）；
`qx-provider` 做能力矩阵与主备故障切换（**备源只做回填与离群校验，多数据源固定主源**）。
A 股侧另有公司行为台账（16 个规范动作名、除权除息锚）与八条交易制度规则，
两侧线格式钉在 `python/tests/fixtures/ashare_actions_cross_check.*` 这一份**共读夹具**上。

---

## 6. 缺点与风险（分级）

### A. 首要风险：边界容易被"读过头"（不是缺陷，但最容易被误解）

1. **零真实账户往返**。能力矩阵 21 个能力块中 17 个同时满足 `implementation` + `code_tested`，
   但 `sandbox_tested` 与 `production_approved` **无一为真**。可宣称的边界只有：
   "本机可重放的确定性回测、Paper 闭环、以及 CCXT/Binance 的**代码级契约**"。
2. **回测与 Paper 不是同一台撮合机**。回测按档位分三套内核，Paper 是首档 touch。
   读到"同一内核"时不能把"同一撮合"一起读进去。
3. **插件不是运行时可插拔**。只有启动期静态装配。
4. **外部数据源正确性本机不可证明**。那份 A 股对照夹具是仓库自造样本，不是交易所落下来的记录。

### B. 正确性与半接线（审计文档自己列为 L1，最高优先级）

1. **账户级五个金额字段无生产者**：`unrealized_pnl / margin_raw / frozen_raw / realized_pnl / funding_raw`
   恒发布 `null`。这是"诚实化"的成果（不是造假），但风险在于**快照被下游当成完整账务读**；
   且权益报 `null` 时读侧**看不出缺的是哪条标记价**。
2. **持仓行浮盈/保证金同样无生产者**，本地 Ledger 拼出的持仓行恒定报 `null`。
3. **`python/qianxing_ashare` 2,050 行与 Rust 侧 A 股规则是两份实现**，Rust 侧无调用点，
   **没有任何判据能发现两者漂移**（"半接线"）。
4. **C++ 插件链只在测试里活**：CI 工作流里 `plugin` 关键词 0 命中，从不把插件加载进 Rust host。
5. **调度重试半边**：`Failed` 生产者与 `retry_run_at` 消费者之间没有生产路径——
   一次运行失败后要不要重跑，"生产装配里没有人决定"。
6. **调度分派只认 Cron + window=Any**：库侧的 `due_jobs_with_calendar` / `due_event_jobs` / `due_manual_jobs` 无生产调用者。
7. **五个控制命令无执行者**：`ChangeRiskLimit / CancelOrder / ReconcileAccount / RetryJob / SwitchVenue` 至今没有派发者。
8. **除权除息锚的治理债**（已修但留下形状代价）：修法是把已装载的 `corporate_actions` 接进折算，
   代价是随仓库发布的示例里那张覆盖表仍为空。
9. **A 股在 Paper/live 提交入口一律被拒**：因为 `PaperVenue` 没有 T+1 结算状态、无当日买入量、无板块级涨跌停锚。
10. **16 份被 git 跟踪的 blessed 摘要停在 `schema_version: 1`**（当前代码落 v4），且没有用例校验这些跟踪产物。
11. **`max_drawdown_raw` 等零读者字段**：`BacktestReport.max_drawdown_raw` 全仓零生产读者却照旧写进报告。
12. **`legacy_spot_spec` 兼容分支**：`MaxNotionalRule` 在无 `instrument_spec` 时**合成一份临时现货规格**而不是 fail-closed——
    与项目"缺数据即拒绝"的整体哲学相悖，源码里自己标了 TODO。

### C. 同一件事有多个主人（重复口径 / 静默错算）

1. **权益口径 8 个入口**：`equity / equity_with_multiplier / equity_for / equity_for_with_multiplier /
   equity_for_with_spec / equity_for_with_spec_and_fx` + `trading.rs` + `backtest.rs`——
   参数按"乘数/规格/汇率"三维组合展开，典型的组合爆炸式重载；读模型里还有"三把权益尺子"并存。
2. **跨 crate 同名公共类型 5 组**（`Bar`/`CorporateAction`/`RetryPolicy`/`StrategyContext`/`DataProvider`），
   `RiskDecision` 同时是 `pub enum`（qx-risk）与 `pub struct`（qx-execution），两个都活着。
3. **对账族类型 9 个定义点**，彼此关系只写在注释里，没有"谁的裁决进哪份产物"的机器可读表。
4. **两条 Bar 链在同一份配置上成交/收益一致，但 `result_hash` 不同**——
   根因是链元数据没进指纹，"同样输入必得同样结果"在跨链比较时说不通。
5. **同一份 BarFrame 有两个生产读点、两套字段集**：回测直读忽略未知字段，Provider 侧
   `schema_version>=1` 时换 `deny_unknown_fields` 严格重解——"多写一格"这类缺陷只在后半条链暴露。
6. **`notional()` / `bp_amount()` 用 `saturating_mul`**，与全仓 `checked_*` 风格不一致，
   极端值下会静默截断而不是报错。
7. **`serde_json` 没开 `arbitrary_precision`**：`*_raw: i128` 字段超过 u64 精度会丢。
8. **`InstrumentId::parse` 只要求点号两侧非空**，symbol 段无词表校验（可能吃进像路径的字符串）。
9. **`ControlCommand` 与 `Order` 都没开 `deny_unknown_fields`**。
10. **无 mTLS 时 `submit_command` 走客户端自报的 `command.permission`**——客户端权限即服务端授权。

### D. 性能与规模（已在能力矩阵里量化）

1. **写面稳态单价与事件日志总长度严格线性**：N 从 1000 涨到 8000，一次 `register_order` 追加
   从 0.0456 s/tick 到 0.3475 s/tick（倍率 1.90–2.03/翻倍）。原因是 `LiveEventPipeline::ingest_once`
   **每次追加都先 `self.clone()` 整份状态**；paper 循环里行情桥与恢复桥各开一个 pipeline，还要再加一份整本重放。
   这基本判了长周期 live 运行的死刑——**只适合回测与短周期 Paper**。
2. **事件日志没有保留/压缩/归档/上限**，只有增长；审计链 `audit.json`、消费者 `processed_event_ids`/
   `dead_letters`、成交去重台账（`seen_fill_keys`/`seen_trade_ids`）同样只增不减。
   且分段后端反而更贵（N=8000 单文件 0.343 s/tick vs 分段 0.417 s/tick）。
3. **`qx-api` 生产文本 51–52 处 `.expect(`，绝大多数是锁中毒即 panic**，在 HTTP 处理线程里
   这是"一个中毒 = 一个请求线程没了"的形状。
4. **`shutdown_token` 只在循环头检查**，块内部仍可能长跑。
5. **Python worker 写侧超时只能靠杀子进程**解除阻塞，最坏墙钟 2×timeout_ms，worker 一旦 wedge 需重启而非复用。
6. **子进程退出无法区分崩溃与正常终止**（CCXT worker 只看 stdout EOF）。

### E. 工程治理（量具本身的洞）

1. **门禁的条数/label 地板是人工回合产物**：463 / 515 / 610 / 851 / 895 都靠人抄当轮日志，抄错就长期失真——
   这条被反复登记却仍未解。
2. **"在册 ≠ 实跑"**：用例地板数的是磁盘 `#[test]` 文本出现次数，feature 门后的用例不编译也绿灯
   （V13 那轮差额 8 条，后来逐名点成 13 条并做了闭合名册——但形状风险仍在）。
3. **变体生产者判据只管 `pub enum` 且是单行扫描**：全仓按花括号配平有 389 颗变体，门禁只认到 247 颗，
   **142 颗不在判据里**；`pub(crate)`/私有枚举 16 处不在册。
4. **没有"死配置字段"判据**：字段有值、有序列化、没有读者（以及它的镜像：有读者但样例里恰好没值）都看不见。
5. **文档引用 CLI 输出字面量无人核对**：README/指南大量转述 `help` 印出的句子，被发现错了一轮多仍全绿。
6. **CI 不设 `QX_PYTHON`**，所以两条 Python worker 契约用例在 CI 里走回落分支——
   "Python 链在 CI 绿"≠"Python 链在 CI 真跑过"。
7. **wheel 版本恒 `0.1.0` 且与 `qx-cli --version` 未接同一版本源**——装出去的包无法自证是哪一轮构建。
8. **发布物核对是有窗口的**：release 重构建会让 dll 重链接，"wheel 内 `.pyd` ≡ dll"这类跨构建核对只在重打包前量得到。
9. **`build.sh` 没有执行位**；`--offline` 文档与实际依赖形状仍在对口径。

### F. 结构性技术债

1. **`qx-cli` 18k 生产行 + 10k 测试行挤在一个 crate**（占生产文本 30%），是命令面 + 装配面 + 内联用例三合一。
   这是"单 binary"的刻意选择，但让"生产 panic 面按 crate 归因"极易读错（含 test 口径 1,078 处 `.unwrap()`，剔掉只有 61 处）。
2. **37 个文件 ≥500 行全部登记、行数棘轮只降不升**：意味着"顺手拆"没收益、"必须拆"没触发器，
   最大 `qx-storage/src/sqlite.rs` 2,286 prod 行。
3. **两套存储后端（sqlite.rs / postgres.rs）逐字重复逻辑**，今天的对账靠门禁钉"单一写路径"，但形状仍是温床。
4. **尾部 8 个 crate prod < 550 行**（qx-risk 510 / qx-provider 470 / qx-guanxing 442 / qx-orchestrator 381 /
   qx-control 359 / qx-plugin 326 / qx-genglu 197 / qx-python 150），边界还能再合。
5. **`#[allow(` 17 处（qx-cli 12）**；生产 `.unwrap()` 61 处（qx-strategy 33、qx-cli 21）。
6. **`c07ad22` 合流回退名册 18 条**：文件锁统一、API 读模型有界、停机转发、IO 预算、owner 路由 fail closed
   等一族交付面按 `-s ours` 回退后登记为"已回退、未排期"——这是一笔明确的、尚未偿还的工程债。

### G. 明确未接入（按设计推迟，不是缺陷但要知道）

PostgreSQL / NATS 的 `implementation` 不是 `true`（`optional`），无生产批准；
券商柜台无厂商协议；WASM、跨节点 HA、连接池/读写分离、逐家签名协议、manylinux/musllinux、发布签名均未接入；
期权合约身份（行权价/到期日/认购认沽/行权方式/组合保证金）只建模了产品类型变体；
`maturity/targets.yaml` 与 `levels.yaml` 全部 `measurement_status=unmeasured`。

---

## 7. 如果要接手/改进，我的优先级建议

| 优先级 | 动作 | 理由 |
|---|---|---|
| P0 | 给账户级五个金额字段接真实生产者（或明确标注"永不产生"并让读侧能诊断缺哪条标记价） | 快照被下游当完整账务读是静默错账的最大来源 |
| P0 | 把 `ingest_once` 的"每次 clone 整份状态"改掉，并给 EventLog 定保留/归档策略 | 线性 tick 成本直接限制了可运行的周期长度 |
| P1 | 权益 8 入口收敛成 1 个 `EquityRequest` + 1 个函数；`RiskDecision` 改名消歧 | 组合爆炸式重载是后续所有口径 bug 的温床 |
| P1 | `python/qianxing_ashare` 与 Rust 规则做一次对照用例，或如实降级为"离线工具"并在 README 写明 | 两份实现、零判据，漂移不可发现 |
| P1 | 修 `legacy_spot_spec`：无 spec 时 fail-closed，与项目整体哲学对齐 | 现在它与"缺数据即拒绝"自相矛盾 |
| P2 | 门禁数字改由 `--snapshot` 写入，文档引用文件名而非写死数字 | 人工抄数是当前最大的单点亮红灯源 |
| P2 | CI 设 `QX_PYTHON`；wheel 版本与 `qx-cli --version` 接同一版本源 | 让"绿"真的等于"跑过" |
| P2 | `qx-api` HTTP 路径上的 `.expect(` 改成显式 503/错误返回 | 锁中毒即 panic 在服务进程里不可接受 |
| P3 | 大文件拆分只在上述改动顺路做，不立专项 | 棘轮已有下行出口，专项拆分只消耗回合 |

---

## 8. 总体评价

**这是一个把"可复核性"当第一性原理做到相当深的系统，工程纪律的强度明显超过多数同类开源项目**——
定点数值、无内核时钟、因果优先级、数据档位硬闸门、产物指纹链、近 900 项架构门禁、
逐轮日志留证 + 变异反向验证，这些都落在代码里而不是 PPT 里。它甚至愿意在 README 里用大篇幅写
"它不是什么"和"当前状态是某年某月某轮的快照"。

**它的短板也很清晰，而且作者自己知道得比外人更清楚**：
一是**大量"结构存在、生效点缺席"**（字段/包/端点有一份没人喂数据），
二是**"同一件事有多个主人"**（币种、权益、对账类型、venue 识别），
三是**写面成本随日志长度线性增长且没有保留策略**，这三条合起来决定了它今天是一台
**优秀的回测与研究机器、合格的 Paper 闭环，而不是一台可以长时间跑实盘的引擎**。

如果目标是"做研究与可复核回测"，它现在就能用；
如果目标是"接真实柜台长期运行"，P0 里的两项（金额字段生产者、事件日志成本与保留）必须先解决。
