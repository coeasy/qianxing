# 牵星 Qianxing

**分级校准，量天定位。**

插件化量化交易与回测内核 · 多交易所 · 多数据源 · 多账户 · 多策略 · 多语言策略

---

## 什么是牵星

**牵星**取自明代"过洋牵星术"——以分级量具（牵星板）测星高以定纬度。
它的方法论是：**先把数据分级，再把模型校准，然后在不确定中定量定位。**

这正是回测真实性的来源。牵星不追求更复杂的滑点公式，而是让每个撮合模型先回答
**"我不知道什么"**：L2/L3 沿真实深度行走，L1 用概率模型，Bar 只重构有限路径——
**不匹配时自动降级并写入审计**，绝不从 K 线里读出不存在的盘口。

## 为什么用它

- **回测与实盘共享同一规则内核** —— 只替换数据、事件驱动、提交与反馈，策略代码零改动
- **多交易所是身份问题，不是连接问题** —— CanonicalProduct / InstrumentId / DataSourceId 三层分离，绝不相互覆盖
- **多数据源固定主源** —— 备源只做回填与离群校验；四级存储 + 机器可读质量门
- **多账户是结算边界，不是资金字段** —— 账户 ID 贯穿全链，双树风控
- **多策略是治理，不是多线程** —— 隔离 + 净额求解 + 显式优先级 + 归因
- **插件只向扩展点贡献，不修改内核** —— 「插件一次、运行静态」，可替换性以不破坏确定性为上限

## 模块

| 模块 | 职责 |
|---|---|
| `qx-core` **牵星** | 确定性内核：时钟、因果事件队列、事件溯源、重放校验 |
| `qx-fenye` **分野** | 身份与市场：Venue / Instrument / 双向版本化符号映射 |
| `qx-guanxing` **观星** | 数据平面：质量门、标准化、`as_of()` point-in-time 可见性 |
| `qx-xingban` **星板** | Bar/L1 Tick/L2 订单簿撮合与仿真：成本、延迟、保证金、因果回测 |
| `qx-zhenlu` **针路** | 执行与路由：风控门禁、OMS、路由决策 |
| `qx-genglu` **更路** | 审计与对账：绩效指标、归因、订单对账 |
| `qx-plugin` **卯眼/榫头** | 插件体系：扩展点贡献、manifest、依赖求解、Profile/Bundle/Patch |
| `qx-factor` | 因子与特征：版本、PIT 工件、分析报告、候选策略绑定 |
| `qx-protocol` | 账户协议：Canonical Snapshot、Diff、QIFI 兼容边界 |
| `qx-provider` | 数据提供方：能力矩阵、稳定选择、主备故障切换 |
| `qx-scheduler` | 调度契约：JobSpec、依赖、交易日历、幂等与重试 |
| `qx-control` | 控制面：权限、审计命令、事件订阅游标 |
| `qx-datastruct` | 列式 BarFrame、PIT 视图、JSON/Arrow C Data Interface |
| `qx-adapter` | REST/TLS/WebSocket 传输与 Venue/Provider 适配边界；含 Binance Spot REST/L1 行情基线 |
| `qx-runtime` | 运行时拓扑配置、worker 监督、停机信号与健康状态 |
| `qx-storage` | 文件/分段 EventLog、控制面、队列、快照、审计以及 SQLite/PostgreSQL 事务后端 |
| `qx-strategy` | Rust Strategy API、上下文/事件/多订单意图和原生策略 SDK |
| `qx-execution` | Venue 回报统一归约、SubmitOrder 执行副作用边界 |
| `python/qianxing_ccxt` | 公共 CCXT 多交易所 REST 数据/交易连接层，CCXT Pro 可选实时流 |
| `qx-python` | PyO3 原生扩展、Arrow C Data Interface capsule 协议 |
| `cpp/` | C++ Strategy API v1 稳定 C ABI、CMake 示例 |
| `qx-cli` | 端到端回测演示与确定性自校验 |

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

# 工业化统一入口：初始化、回测、Paper 主链路和实盘前检查
cargo run -p qx-cli -- help
cargo run -p qx-cli -- init qianxing.runtime.json
cargo run -p qx-cli -- backtest
cargo run -p qx-cli -- builtin-strategies
cargo run -p qx-cli -- builtin-backtest sma_cross deploy/qianxing.bar-frame.example.json
cargo run -p qx-cli -- strategy-backtest deploy/qianxing.runtime.builtin-strategy.example.json deploy/qianxing.bar-frame.example.json
cargo run -p qx-cli -- paper-check deploy/qianxing.runtime.paper-strategy.example.json
cargo run -p qx-cli -- live-check deploy/qianxing.runtime.production.example.json

# 内置 Rust、Python/C++ JSONL 策略直接复用 Bar 回测引擎
cargo run -p qx-cli -- strategy-backtest deploy/qianxing.runtime.strategy-backtest.example.json deploy/qianxing.bar-frame.example.json

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
cargo run -p qx-cli --release -- paper-submit-order deploy/qianxing.runtime.example.json deploy/qianxing.paper-submit-order.example.json
```

Windows 下可直接双击 `build.bat`。

实现状态与未完成外部边界见：[V5 落地审计](D:/work_code/quantwork/qianxing/自研量化框架V5落地审计.md) 和 [工业级落地验收与差距清单](D:/work_code/quantwork/qianxing/docs/工业级落地验收与差距清单-V1.md)。

工业化易用性收口入口和发布前检查见：[工业化易用性收口指南 V1](D:/work_code/quantwork/qianxing/docs/工业化易用性收口指南-V1.md)。

演示会输出：

```
[观星 · 质量门] bars=400 判定=Ok
[星板 · 回测 A] 成交=... 手续费=... 总收益=...% 最大回撤=...% 终值=...
[更路 · 重放校验]
  ① 同输入两次运行哈希一致 : true
  ② 改参数后哈希发生变化   : true
[卯眼 · 插件装配] ...
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
当前已增加可运行 HTTP/WebSocket 控制面、Operator 权限校验、单进程/共享文件/可选 SQLite 事务 API 限流、Snapshot/Diff 与事件游标、持续实时事件总线、有序关闭、可热替换 TLS 配置、PEM 证书加载/轮询式安全重载、mTLS `ServerConfig` 与客户端证书到 Operator 的可信映射、明文/TLS API 服务端入口、文件恢复、带并发追加锁的链式持久化审计、可选 SQLite/PostgreSQL 审计链/快照/任务租约/fencing token/EventLog 后端、Python/JSON DataStruct、PyO3 原生扩展与本机构建 wheel、Linux/macOS/Windows wheel CI 矩阵、Arrow C Data Interface 借用零拷贝与拥有型跨语言释放边界、带 rustls TLS 客户端、带 API key 握手头的 TLS WebSocket 用户流底座、HMAC-SHA256 签名边界、超时/限频/成交回报幂等的 REST Provider/Venue 适配器基线、Binance Spot 主网/Testnet HMAC 签名下单/撤单、`allOrders`/开放订单对账、公共 L1 REST `bookTicker`/WSS 流、签名用户流订阅会话、可注入重连退避驱动与 `executionReport` 用户事件映射、支持环境变量或 Secret Manager 投影文件且在新会话/新订单/新对账轮次重新加载凭证的独立 Binance 行情/用户流/对账 worker、LiveEventPipeline 事件→Kernel/EventLog/Ledger/账户余额快照归约与订单重启恢复、结算币种差异报告、Cron/Calendar/Event/Manual 调度与 JobWindow/Worker/带失败码与退避的确定性重试、超时人工介入、可校验 RunManifest、Provider 来源哈希与 JSON 血缘恢复、PIT 财务视图、按样本计算 IC/RankIC/衰减/换手的因子报告、带训练/验证区间和执行/风险模型绑定的因子候选、真实组内中性化、插件 manifest schema/hash 校验与 Ed25519 发布签名验证、带基准/持仓/费用/换手/回撤的回测报告、共享文件系统 claim 锁/fencing token 任务队列与租约、因子 DAG 和多 Venue 路由评分；连接池/读写分离、跨节点 HA、MQ、真实账户网络验收、其他供应商用户流认证/订阅与事件映射、逐家签名协议、证书签发、manylinux/musllinux 兼容性、发布签名和 WASM 仍需按实际供应商与部署环境接入。详见[工业级产品化实施路线图 V1](docs/工业级产品化实施路线图-V1.md)。

跨语言策略传输默认兼容 JSONL，也支持 `transport: "framed_json"` 的 QXSF 二进制分帧（版本、序号、长度上限、CRC32）；`transport: "shared_memory_json"` 会将同一 QXSF 帧放入双向固定槽位 SPSC mmap ring；`transport: "shared_memory_columnar"` 会将 Bar 历史编码为 QXCB 固定宽度列后放入同一 ring，适合减少行情数值 JSON 解析。示例：

```powershell
cargo run -p qx-cli -- strategy-backtest deploy/qianxing.runtime.strategy-framed.example.json deploy/qianxing.bar-frame.example.json
```

共享内存策略 Worker 回测：

```powershell
cargo run -p qx-cli -- strategy-backtest deploy/qianxing.runtime.strategy-shared.example.json deploy/qianxing.bar-frame.example.json
```

列式共享内存策略 Worker 回测：

```powershell
cargo run -p qx-cli -- strategy-backtest deploy/qianxing.runtime.strategy-columnar.example.json deploy/qianxing.bar-frame.example.json
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
| 1 | 插件骨架（扩展点/manifest/依赖求解/装配） | ✅ 已落地 |
| 2 | 回测引擎（TestClock + 因果队列 + 撮合模型） | ✅ 已落地 |
| 3 | 账簿、L1、PaperVenue、限频、恢复与对账契约 | ✅ 基础能力已落地 |
| 4 | 真实 Venue / 多数据源生产接入 | 🟡 Binance Spot REST/L1/用户事件与 EventLog 归约基线已落地，真实账户验收及其他供应商待接入 |
| 4 | ProviderRegistry / 因子计算 / QIFI 快照与 Diff / Scheduler | ✅ 可执行实现，含 DAG、CronSpec、Worker、重试和 Diff；生产适配待接入 |
| 5-7 | 真实 Venue / 多账户路由 / 生产可靠性 / WASM | 🟡 Binance 单账户审计执行入口已落地，多账户/跨节点生产执行器待接入 |

## 许可

Apache-2.0

> 本框架仅参考业界项目的公开架构与模块设计思路（LEAN、RQAlpha、NautilusTrader、vn.py、
> Hummingbot、CCXT、vectorbt、QUANTAXIS），不复制任何第三方源代码。
