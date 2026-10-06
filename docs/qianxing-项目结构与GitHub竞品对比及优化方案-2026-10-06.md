# 牵星 Qianxing：项目结构、实现审计、GitHub 竞品对比与优化方案

> 审计日期：2026-10-06  
> 代码基线：当前工作树 HEAD `9d46305`（main）  
> 仓库：[coeasy/qianxing](https://github.com/coeasy/qianxing)  
> 目标：以代码、测试、配置和 CI 的真实边界为准，梳理现状；对比同类开源项目；吸收有价值的产品和工程实践；形成可执行的改进路线。

## 1. 结论摘要

牵星已经不是一个单纯的 Python 回测脚本，而是一套以 Rust 为确定性内核、支持 Python/C++/Rust 策略、覆盖数据—策略—风控—撮合/执行—Ledger—报告/API 的本地量化交易运行时。它最有差异化的能力是：

1. **可复核性强**：定点数、事件全序、事件溯源、重放校验、输入指纹和运行清单共同约束结果。
2. **边界意识强**：数据源身份、产品身份、交易场所身份、账户和结算币种分层；数据档位不足时拒绝运行，不静默制造盘口信息。
3. **跨语言策略边界清楚**：策略只输出 signal/target/order intent，不直接拿到凭证、Venue、Ledger 或控制面；Python/C++ 与 Rust 共享风险和订单归约链路。
4. **安全收口做得深**：未知回报进入待对账，控制面写入需权限和审计，worker/网络读写有界，损坏状态 fail-closed，架构门禁和行为测试密集。

当前最大问题不是“核心能不能正确跑”，而是“别人能不能快速用、持续研究、可靠部署”：

- 结果主要是 JSON/CSV，缺少竞品常见的图表、HTML 报告和研究工作台。
- 缺少参数寻优、walk-forward、样本外验证和实验追踪。
- 多数真实交易能力还停留在代码契约；`maturity/capabilities.yaml` 当前 `sandbox_tested: true` 和 `production_approved: true` 均为 0。
- `qx-cli`、`qx-runtime`、`qx-storage` 集中了较多装配和基础设施职责，长期会拖慢新 Venue、新后端和新产品面的交付。
- 适配器、策略包、数据集和报告缺少统一的注册、版本、兼容和发布机制。

建议产品定位保持为：**“可复核的本地研究与受控交易内核”**，并通过一层轻量研究体验和一层标准化适配器生态，补齐“易用性”和“生产证据”，而不是立即扩展成云平台或全功能 GUI。

本轮进一步把目标提升为**分级交付的工业级量化交易操作系统**：先以 A 股/可转债、国内期货/期权、国际股票/FX、加密现货/衍生品和多账户多策略为场景基线，再通过 L0–L4 能力等级、单机/分布式拓扑、统一项目入口、场景 profile、Adapter Conformance、ExperimentSpec、EvidenceBundle 和生产恢复演练，把“规划支持”与“已验证支持”严格分开。详细蓝图见第 13–20 节。

## 2. 取证范围与现状基线

### 2.1 取证材料

- Rust workspace、23 个 crate 的 manifest、核心 `src`、集成测试和 `Cargo.lock`。
- `python/`、`cpp/`、`schemas/`、`deploy/`、`maturity/`、`tools/`、`.github/workflows/ci.yml`。
- `README.md` 与 `docs/` 中的产品边界和历史改造方案，并与代码交叉检查。
- GitHub 官方仓库和文档：NautilusTrader、LEAN、Hummingbot、Freqtrade、Jesse、vectorbt、Backtrader、QUANTAXIS、QuantDinger、quantdigger、Qlib、VeighNa/vn.py、RQAlpha、FinRL。

### 2.2 代码规模与工程基线

| 指标 | 当前事实 | 说明 |
| --- | ---: | --- |
| Rust workspace crate | 23 | `Cargo.toml` workspace members |
| 顶层 deploy JSON 模板 | 52 | 模板有登记表和真实 reader 覆盖测试 |
| 内置策略种类 | 17 | 13 个单标的 + 4 个两腿套利 |
| 能力矩阵条目 | 23 | `maturity/capabilities.yaml` |
| `sandbox_tested: true` | 0 | 代码契约不能替代供应商测试网证据 |
| `production_approved: true` | 0 | 当前不应宣称生产就绪 |
| Strategy API 版本 | v1 | Rust/C ABI/JSON/Python 共用主要语义 |
| 默认 CI 能力 | Rust、Python wheel、feature/backend、runtime/Paper、C++ SDK、Binance acceptance | 测试网凭据存在时才执行完整外部验收 |

本次本地验证包含架构门禁和 `cargo test --workspace --all-targets --no-fail-fast`；命令结果应以本轮终端输出和 CI 为最终证据，不能把历史文档中的测试数量、门禁数量或提交号直接当作当前数值。

## 3. 项目结构与职责边界

### 3.1 总体分层

```text
数据/配置/外部来源
        │
        ▼
qx-data + qx-guanxing + qx-provider + qx-datastruct + qx-factor
        │  canonical schema / PIT / quality gate / dataset fingerprint
        ▼
qx-core  ─────────────── qx-strategy / qx-python / cpp SDK
        │ identity / Fixed / EventLog / Order / Ledger / replay
        ▼
qx-xingban       qx-zhenlu       qx-risk       qx-genglu
回测撮合与成本     OMS/Venue/Paper     风控规则        对账裁决
        │
        ▼
qx-execution + qx-runtime + qx-orchestrator
统一执行事实、归约、worker 监督、停机和恢复
        │
        ▼
qx-storage + qx-api + qx-control + qx-scheduler
事件/队列/快照/审计、查询/API、控制面、调度
        │
        ▼
qx-adapter + qx-cli + deploy/ + maturity/ + tools/
外部 Venue、CLI 装配、模板、证据和门禁
```

### 3.2 crate 职责表

| 层 | crate/目录 | 关键实现 | 主要风险或演进点 |
| --- | --- | --- | --- |
| 核心领域 | `qx-core` | `Fixed/Money/Price/Quantity`、身份、`EventLog`、事件、订单状态、Ledger、重放 | 公共模型较多；新增产品容易形成规格组合爆炸 |
| 数据平面 | `qx-data`、`qx-guanxing`、`qx-provider` | 标准 Bar、目录/数据集、质量校验、数据源、PIT/as-of、增量摄取 | `Bar`/Provider/DataView 等概念仍有多个边界；研究工件还未完全进入运行时 |
| 列式边界 | `qx-datastruct` | `BarFrame`、固定列、Arrow C Data Interface、JSON/Python 转换 | C Data Interface 生命周期正确性需要持续做跨语言压力测试 |
| 回测 | `qx-xingban` | Bar、L1 Tick、L2 order book、多腿、费用、延迟、保证金、A 股规则 | Paper 的首档 touch 撮合与回测簿撮合不同，必须在产品和报告中明确 |
| 执行/风控 | `qx-zhenlu`、`qx-risk`、`qx-genglu`、`qx-execution` | OMS、风险门、Paper Venue、路由、执行事件归约、对账动作 | 状态迁移横跨多个 crate；新增 Venue 要同时维护多张映射表 |
| 策略 | `qx-strategy`、`qx-python`、`python/`、`cpp/` | Rust trait、17 种内置策略、JSONL/framed/shared-memory、PyO3、C ABI/ring | ABI/schema/发布版本尚未由统一 registry 管理；C++ 意图字段覆盖不完整 |
| 运行时 | `qx-runtime`、`qx-orchestrator` | `RuntimeConfig`、`LiveEventPipeline`、worker policy、监督、停机、恢复 | runtime 职责密集；应用装配、进程生命周期和事件归约仍较集中 |
| 存储 | `qx-storage` | 文件/分段 EventLog、SQLite/PostgreSQL、Outbox、队列、快照、审计、NATS | 多后端契约丰富，但写侧成本、事务语义和服务依赖需要可观测化 |
| 控制与查询 | `qx-api`、`qx-control`、`qx-protocol` | 本地 HTTP/WS、mTLS/CORS/限流、读模型、命令权限、审计、快照协议 | 自研传输面小而可控；后续需补稳定 OpenAPI/事件 schema 发布 |
| 接入与产品壳 | `qx-adapter`、`qx-cli`、`deploy/` | Binance/CCXT、TLS/WSS、单一 CLI、模板查找和 worker 装配 | `qx-cli` 依赖几乎所有内部 crate，是最大的应用耦合热点 |
| 调度/因子/插件 | `qx-scheduler`、`qx-factor`、`qx-plugin` | JobSpec、交易日历、幂等重试、因子报告/候选绑定、静态插件 manifest | 插件是启动期静态装配，不是热插拔；因子物化尚未成为完整生产链路 |

### 3.3 关键数据和执行链路

#### A. Bar/深度回测

```text
runtime.json + frame/dataset
  → 解析配置、市场规格、成本和风险规则
  → 读取数据并按实际内容计算 fingerprint
  → 数据质量/PIT/档位门禁
  → Strategy::on_event 或跨语言 worker
  → StrategyDecision / target / intents
  → RiskGate → OMS → Bar/L1/L2 MatchingEngine
  → Fill / fee / margin / ledger facts
  → EventLog → ReplayVerifier
  → summary.json + equity.csv + fills.csv + run.json
```

这里的关键不是单纯“能回测”，而是产物只有在事实流可重放、输入身份可复核时才落盘。`None/null` 表示未计算，`Some(0)/0` 表示已计算且为零，避免报告把未知伪装成零。

#### B. Paper/Live 执行

```text
Scheduler/Control command
  → strategy worker
  → order intent
  → shared Risk/OMS/ExecutionGateway
  → PaperVenue 或 Binance/CCXT adapter
  → Accepted/Fill/Cancelled/Unknown/ReconcileRequired
  → LiveEventPipeline
  → EventLog + Ledger + Outbox + account projection + audit
```

适配器不会直接修改 Ledger；网络错误或不完整回报进入未知/待对账状态，防止“提交失败后盲目补单”。这是交易正确性上的强项，应继续作为统一原则。

#### C. 多语言策略

策略边界是：

```text
Runtime → StrategyContractInput
Strategy → StrategyContractOutput / intents[]
        → Rust rebuild Order
        → Risk → OMS → Venue
```

Python 目前提供 `qianxing_bridge`、`qianxing_strategy`、`qianxing_ccxt`、`qianxing_ashare`；C++ 提供稳定 C ABI 和共享内存 ring。策略进程不应接触凭证、Ledger 或控制面，这个隔离应继续保持。

## 4. 现有优势与不足

### 4.1 相对同类项目的优势

| 方向 | 牵星优势 | 价值 |
| --- | --- | --- |
| 确定性 | 定点数、注入时钟、事件全序、重放验证 | 适合审计、回归、策略结果复算 |
| 数据诚实性 | 数据档位与撮合模型绑定，缺深度即拒 | 降低 K 线回测制造虚假精度的风险 |
| 账户和身份 | 产品/Instrument/Venue/DataSource/Account 分离 | 支持多市场、多账户、多币种扩展 |
| 事实归约 | 回测、Paper、外部回报最终进入统一事实和 Ledger | 避免每条连接器各写一套账 |
| 安全边界 | fail-closed、Unknown/待对账、凭证隔离、权限和审计 | 适合从研究逐步走向受控交易 |
| 多语言 | Rust、Python、C++ 共用意图和执行后门禁 | 兼顾研究效率和性能路径 |
| 工程纪律 | 架构不变量、模板覆盖、契约测试、构建矩阵 | 降低长期重构时的隐性回归 |

### 4.2 当前不足

1. **采用门槛高**：23 个 crate、多个 worker、众多 JSON 模板和运行时角色，对新用户不够友好。
2. **结果反馈弱**：CSV/JSON 适合机器审计，不适合快速判断策略表现、交易点和回撤来源。
3. **研究闭环不完整**：尚无统一实验规格、参数网格/贝叶斯优化、walk-forward、样本外报告和实验比较。
4. **数据入口不够产品化**：有 provider/ingestion/registry，但“下载—校验—补洞—注册—复跑”还没有成为一个统一用户路径。
5. **外部交易证据不足**：测试网验收脚本和 fail-closed 设计已经存在，但真实凭据、断线、重复回报、远端未知态的 evidence 尚未翻转能力矩阵。
6. **适配器治理不足**：缺少统一的 connector capability matrix、官方/社区/实验性分级、版本兼容和 conformance suite。
7. **职责集中**：`qx-cli` 是装配中枢，`qx-runtime` 同时承载配置/监督/管线，`qx-storage` 同时承载多个存储域；规模继续增长后会影响编译、发布和团队协作。
8. **契约维护偏手工**：策略输入方向还缺少与 Rust 结构同步的机器可读 schema；C++ 意图暂未表达全部衍生品保证金/持仓字段。

## 5. GitHub 同类项目对比

### 5.1 竞品定位

| 项目 | 核心定位 | 代表优势 | 对牵星的启示 |
| --- | --- | --- | --- |
| [NautilusTrader](https://github.com/nautechsystems/nautilus_trader) | Rust-native、事件驱动、多资产、多 Venue 生产交易平台 | 统一 kernel、MessageBus、Cache、Data/Execution Engine、Python facade；文档明确强调回测/实盘策略一致性 | 吸收 typed message bus、统一节点生命周期、adapter 分层和 crash-only 设计 |
| [QuantConnect LEAN](https://github.com/QuantConnect/Lean) | 专业级回测与实盘引擎，覆盖多资产和研究工作流 | 模型可插拔、CLI 项目流程、数据/回测/优化/实盘入口成熟 | 吸收 Project/Experiment/Optimizer 抽象和“研究到部署”命令链 |
| [Hummingbot](https://github.com/hummingbot/hummingbot) | CEX/DEX 加密交易机器人和连接器生态 | connector 数量、Paper/市场做市、策略模板、Dashboard/社区运营强 | 吸收 connector conformance、venue capability、策略模板和本地控制台 |
| [Freqtrade](https://github.com/freqtrade/freqtrade) | Python 加密交易 Bot | 配置向导、dry-run、webUI、绘图、hyperopt、Telegram/webhook 运维链成熟 | 吸收十分钟首跑、结果可视化、参数寻优和通知，但不牺牲确定性边界 |
| [Jesse](https://github.com/jesse-ai/jesse) | 易上手的 Python 加密策略研究/实盘框架 | 简短策略语法、多时间框架、回测/优化/live 一体化、本地 Web 体验 | 吸收策略模板、路由式项目结构和研究闭环，控制复杂度 |
| [vectorbt](https://github.com/polakowo/vectorbt) | 向量化/批量参数研究和可视化 | 大规模参数扫描、交互图表、Notebook 体验 | 作为可选 research accelerator，不替代牵星的事件级撮合内核 |
| [Backtrader](https://github.com/mementum/backtrader) | Python 回测与实时交易库 | 指标、broker 模型、订单类型、plot、示例多，上手成本低 | 吸收教学样例、指标/分析器组织方式和 plot 产物；注意其历史维护和许可证边界 |
| [QUANTAXIS](https://github.com/yutiansut/QUANTAXIS) | 面向股票/期货/期权的本地量化全栈 | 数据、回测、模拟、交易、可视化、多账户、调度和分布式；QIFI 账户协议；近版加入 QARS2 Rust 核心和 Arrow/共享内存桥 | 吸收统一账户快照/增量 Diff、跨语言兼容、数据桥和完整中文文档；避免多套后端和能力声明失真 |
| [QuantDinger](https://github.com/OpenByteInc/QuantDinger) | Local-first AI Trading OS / 自托管交易产品 | 研究→策略→回测→Paper/Live→监控一体化；Web/Mobile/API/MCP；策略演化、walk-forward、holdout、PBO/Deflated Sharpe/成本压力测试；生产 Compose 和 observability | 吸收产品化工作流、异步实验、运行时与 API 解耦、OpenAPI/监控/安全基线；不直接复制其更重的 SaaS/多租户范围 |
| [quantdigger](https://github.com/QuantFans/quantdigger) | Python 轻量回测和策略语法 | 简洁的策略 DSL、股票/期货/选股/套利/组合、Matplotlib 图形和大量入门示例 | 吸收“先让用户写出第一条策略”的 API 和教学样例；明确其仓库已声明停止维护，不作为生产架构样板 |
| [Qlib](https://github.com/microsoft/qlib) | AI-oriented Quant Research 平台 | 数据处理、因子、模型训练、组合、回测、在线服务、离线/在线数据模式，`qrun` 和模型/数据集生态 | 吸收 Data/Feature/Model/Backtest/Workflow 解耦、实验配置和模型基线；牵星保留事件撮合和交易事实边界 |
| [VeighNa/vn.py](https://github.com/vnpy/vnpy) | Python 交易系统开发框架和连接器生态 | 交易接口覆盖广、事件引擎、策略应用、组合策略、风控模块；4.0 增加 alpha 数据集/模型/策略/lab/Notebook 投研链 | 吸收 gateway/app 分层、连接器生态治理、研究到实盘的一致入口；加强牵星的中文文档和 adapter conformance |
| [RQAlpha](https://github.com/ricequant/rqalpha) | 可扩展、可替换的 Python 算法交易回测/交易框架 | 一行命令、Mod Hook、数据/回测/模拟/交易/分析全链路、多标的、文档和社区入口 | 吸收 Mod/Extension API、配置简化、分析报告和 API 稳定性；注意其仓库声明的非商业使用边界 |
| [FinRL](https://github.com/AI4Finance-Foundation/FinRL) | 金融强化学习教育和研究框架 | 市场环境—DRL Agent—金融应用三层、教程、基准、数据和模型实验；生产方向另演进到 FinRL-X | 吸收可复现实验、基准和教程组织；将 RL/ML 作为策略研究层，不能替代牵星的风险和执行内核 |

### 5.2 能力矩阵

| 能力 | 牵星 | Nautilus | LEAN | Hummingbot | Freqtrade/Jesse | 建议 |
| --- | --- | --- | --- | --- | --- | --- |
| 事件级确定性和重放 | **强** | 强 | 中上 | 中 | 中 | 保持为核心差异化 |
| 多资产/多 Venue 领域模型 | 中上 | **强** | **强** | 加密生态强 | 加密为主 | 优先提升 adapter 和 instrument registry |
| 多语言策略 | **Rust/Python/C++** | Rust/Python | C#/Python | Python | Python | 保持，统一 schema/SDK 发布 |
| 数据档位/输入 provenance | **强** | 中上 | 中上 | 中 | 中 | 做成用户可见的 Data Quality Report |
| Paper/dry-run | Paper 闭环已实现 | Sandbox/Backtest | 支持 | **强** | **强** | 完成测试网 evidence 和账户报告 |
| 结果图表/HTML | 当前偏弱 | 中 | **强** | Dashboard | **强** | P1 补 SVG/HTML，而非先做重 GUI |
| 参数优化/样本外 | 尚未形成统一链路 | 中 | **强** | 有限 | **强** | P2 建立 Experiment/Optimize/Walk-forward |
| 数据下载补洞 | 有基础组件 | 强 | **强** | 有 | 强 | 统一 Data CLI 和 manifest |
| 适配器生态治理 | 起步 | **强** | 强 | **强** | 中上 | 官方/社区/实验性分级 + conformance |
| 易用性/首跑 | CLI 和模板已具备 | 中 | **强** | 中上 | **强** | quickstart、doctor、报告和错误信息继续产品化 |
| 真实生产证据 | 当前 0 条 sandbox/production | 较强 | 较强 | 生态驱动 | 生态驱动 | 以 evidence 包翻转能力矩阵，不以文档宣称替代 |

### 5.3 应吸收与不应照搬的部分

应吸收：

- Nautilus 的 **DataEngine/ExecutionEngine/MessageBus/Cache** 组件化边界、adapter 官方分级和统一节点生命周期。
- LEAN 的 **项目 manifest + research/backtest/optimize/live CLI** 工作流与可插拔模型接口。
- Hummingbot 的 **connector capability matrix、connector conformance、Paper 柜台和策略模板生态**。
- Freqtrade/Jesse 的 **向导、策略模板、dry-run、plot/HTML、优化和通知**，把正确的内核能力变成用户能快速感知的产品。
- vectorbt 的 **批量实验和交互式可视化**，但把它放在研究加速层。

不应照搬：

- 不用向量化研究路径替代 L1/L2 事件撮合，否则会破坏延迟、盘口和因果语义。
- 不把统一交易接口做成“所有 Venue 都支持同一组最小字段”，缺失能力必须显式声明并拒绝不适配的订单。
- 不因为竞品支持更多交易所就放宽凭据、未知回报、市场规格和对账门禁。
- 不在没有证据时把 Paper、sandbox 和 production 都标成“可用”。

### 5.4 中国 Quant 生态专项深度对比

这些项目与 Nautilus/LEAN 的差异在于：它们更重视中文用户、券商/期货接口、数据获取、策略教学和本地部署。牵星如果只和 Rust-native 交易引擎比较，会低估真正影响用户选择的“数据、文档、连接器、图表和研究闭环”。

| 竞争面 | QUANTAXIS | QuantDinger | quantdigger | Qlib | VeighNa/vn.py | RQAlpha | 牵星当前判断 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 用户入口 | 全栈脚本/文档/组件 | 产品化 Web/API/MCP/AI 入口 | 简单 Python 类和 DSL | `qrun`/Notebook/配置工作流 | trader/app/gateway/Notebook | 一行命令 + 策略 API | 内核入口强，研究产品入口弱 |
| 数据 | MongoDB/ClickHouse、Tick/L2/因子、QAFetch | 多 provider、PostgreSQL/Redis/Kafka 运行栈 | 依赖传统 Python 数据源 | Qlib 数据库、DataServer、数据集和特征 | RQData/迅投研等网关与 alpha 数据 | 平台/Mod 数据源 | 有 manifest/PIT/质量门，但下载和共享数据产品化不足 |
| 账户协议 | QIFI、多账户、Diff、跨语言 | Strategy API V2、虚拟账户、运行记录 | broker/strategy 上下文 | 研究对象和组合执行器 | trader/portfolio/app 体系 | context/account/portfolio | 领域模型和事件事实更严格，但对外 SDK 体验尚弱 |
| 回测 | 股票/期货/套利/组合，QARS2 加速 | 服务端回测、实验、成本压力测试 | Bar 回测和 Matplotlib | 组合/执行器/模型工作流 | CTA/组合/alpha 研究 | 多证券回测和分析 | 撮合档位和重放最强，但分析与实验编排缺口明显 |
| 实盘 | CTP/QMT 等生态、模拟和交易 | crypto/股票/外汇，Paper/Live/监控 | 设计兼顾实盘但仓库已停止维护 | online serving，非重点连接器生态 | 国内外 gateway 数量和社区强 | 模拟盘/实盘平台衔接 | 适配器正确性强，真实 sandbox evidence 仍为零 |
| 可视化 | Web/图表/Notebook 生态 | Web、移动端、Dashboard、告警 | 自带简单 K 线/策略绘图 | 分析图表、Notebook | lab/Notebook/可视化分析 | analyzer/报告/微信邮件 | 结构化产物强，用户可视化弱 |
| 参数研究 | 因子研究和优化器开发中 | random/grid/TPE、walk-forward、holdout、稳健性指标 | 无系统优化闭环 | 模型/因子/Workflow/模型 Zoo | 参数优化与 alpha lab | Mod 和分析扩展 | 缺 ExperimentSpec、OOS、优化和 compare |
| 工程运行 | 微服务、RabbitMQ、Tornado、Mongo/ClickHouse | 明确的 API/worker/job/cache/DB/observability/Compose 边界 | 轻量但老旧 | 模块松耦合，依赖较重 | Python 模块/应用/gateway 生态 | Python Mod 体系 | 事务/恢复/门禁强，产品运行拓扑和部署样板不足 |
| 社区与文档 | 中文生态、组件多、文档持续演进 | 产品文档、中文/英文、Roadmap、贡献入口 | 示例友好但已停止维护 | 学术/工程文档和模型 Zoo | 中文社区、论坛、众多 gateway/app | 中文文档和平台社区 | 代码审计文档强，面向用户的教程/案例/生态弱 |

#### 5.4.1 QUANTAXIS：最接近的“多市场全栈 + Rust 演进”参照

QUANTAXIS 对牵星最有价值的不是某个单独策略，而是它把数据、账户、回测、交易、调度、可视化和多账户组织成用户可感知的完整产品，并用 QIFI 作为跨模块、跨语言的账户快照边界。当前仓库还展示了 QARS2 Rust 核心、QADataSwap/Arrow/共享内存和 Python 自动回退，这与牵星的 Rust 内核 + Python/C++ 策略方向高度同构。

牵星已经有 `qx-protocol`、`AccountSnapshot`、Diff、`Option<i128>` 诚实语义和 Arrow C Data Interface，因此不应再复制一套 QIFI；应吸收的是它的**产品级协议可见性**：

- 为账户快照、订单、成交、资金、持仓和 Diff 发布独立版本、示例、迁移指南和跨语言 golden fixtures。
- 在 `qx-protocol` 上增加“研究账户/执行账户/对账账户”的投影说明，让用户能从 Python/C++ 直接消费稳定账户模型。
- 将当前 `deploy/` 模板扩展成按市场/策略/运行模式组织的可运行示例，而不是只有配置文件。
- 将 Arrow/共享内存性能桥接纳入公开 benchmark；每次优化必须同时报告零拷贝、复制、内存和结果 hash。

需要保留差异：QUANTAXIS 的多组件、多数据库、多消息系统带来很强的覆盖面，也会增加安装、兼容和故障面。牵星应采用“一个内核事实流 + 可选基础设施后端”，而不是为覆盖面引入多个平行账本。

#### 5.4.2 QuantDinger：最值得学习的“产品与运营闭环”

QuantDinger 直接把“AI 研究 → 策略代码 → 回测 → Paper/Live → 监控”作为产品主链，并把 API、Web、移动端、MCP、持久化、durable worker、审计、Prometheus、OpenAPI、Compose 安全基线放在同一产品边界内。尤其值得吸收的是：

- 策略演化不是一个孤立的 optimizer，而是带 walk-forward、blind holdout、PBO、Deflated Sharpe、bootstrap、成本压力测试和异步历史的实验系统。
- v5 把 HTTP API、交易进程、调度、有限任务和长生命周期策略 runtime 拆开；这对牵星解决 `qx-cli/qx-runtime` 装配过重有直接参考价值。
- 通过 OpenAPI、request ID、JSON logs、Prometheus、dashboard、alert 和非 root/只读根文件系统，把“可运行”推进到“可运维”。

牵星不应直接照搬其多租户 SaaS、支付结算或更重的 AI agent 范围；建议先落地 `ExperimentSpec + RunRecord + Report + Worker Observability` 四件套，再决定是否开放 MCP/Agent Gateway。所有 AI 生成策略仍必须经过 schema、RiskGate、回测重放和人工批准。

#### 5.4.3 quantdigger：证明“简单策略 API”本身就是竞争力

quantdigger 已明确声明不再维护，但它仍有一个值得保留的产品教训：策略类、`on_init/on_bar/on_exit`、指标序列、画线和买卖动作能让新用户很快写出第一条策略。牵星当前的领域边界更严谨，却需要一个类似的教学 facade：

```text
StrategyTemplate
  on_start(context)
  on_bar(bar)
  emit_target(...) / emit_intent(...)
  on_stop(report)
```

这个 facade 只能生成策略意图，不能越过 Risk/OMS/Execution；它应编译或转换到现有 Strategy API v1，而不是建立第二套交易语义。

#### 5.4.4 Qlib 与 FinRL：补齐“模型研究层”，但不要污染执行内核

Qlib 的长处是完整的 Data/Feature/Model/Portfolio/Backtest/Workflow 和模型/数据集生态，FinRL 的长处是市场环境—DRL Agent—金融应用三层教学和可复现实验。两者都能提升牵星的研究吸引力，但其核心产物是 prediction、portfolio target 或 agent action，不是交易所事实。

建议边界：

```text
Qlib/FinRL/自研 ML/RL
  → Feature/Prediction/PortfolioTarget artifact
  → data fingerprint + model fingerprint + as_of validation
  → qx-strategy intent adapter
  → qx-risk → qx-xingban/qx-execution → EventLog/Ledger
```

这样可以吸收模型 Zoo、Dataset Zoo、Notebook、实验 benchmark 和训练/验证流程，却不会让 PyTorch、Gym、MLflow 或 Notebook 状态进入 `qx-core` 热路径。

#### 5.4.5 VeighNa/vn.py 与 RQAlpha：补齐连接器生态和扩展机制

VeighNa/vn.py 的竞争力来自大量 gateway/app、中文社区和 trader-first 的开发方式；4.0 还将数据集、模型、策略、lab、Notebook 串成 AI 投研流程。RQAlpha 的竞争力来自一行命令、稳定的策略 API、Mod Hook 和可替换组件。

牵星可分别吸收：

- **vn.py 的 gateway/app 分层**：一个 Venue adapter 只实现数据、交易、规格、对账和 capability，不把产品逻辑写进 CLI；官方、社区、实验性适配器分级维护。
- **RQAlpha 的 Mod 思路**：给 `qx-cli` 和 `qx-runtime` 增加正式 extension point，但仍采用启动期静态 manifest、签名和版本校验；不要立即开放任意动态代码加载。
- **两者的中文文档和样例密度**：每个新模块必须同时有一个最小例子、一个失败案例、一个回测产物和一个安全边界说明。

### 5.5 竞品优势到牵星改造项的映射

| 来源 | 借鉴能力 | 牵星落点 | 优先级 | 完成判据 |
| --- | --- | --- | --- | --- |
| QUANTAXIS | QIFI 账户快照、Diff、跨语言协议、Rust/Python fallback | `qx-protocol`、`schemas/`、Python/C++ SDK | P1 | 账户/订单/成交/持仓 fixtures 三语言互读，版本迁移可验证 |
| QUANTAXIS | 数据桥、共享内存、Arrow/Polars benchmark | `qx-datastruct`、`qx-python`、`benchmarks/` | P2 | 零拷贝与复制路径有吞吐/内存/hash 对照 |
| QuantDinger | AI→策略→回测→Paper/Live→监控产品链 | `qx project`、`RunRecord`、`report`、worker metrics | P1 | 一条命令产出 verified report、运行状态、监控和审计链接 |
| QuantDinger | walk-forward、blind holdout、PBO、成本压力测试 | `ExperimentSpec`、`qx-factor`、`qx-cli optimize` | P2 | 每次实验产出 train/test/OOS/robustness，不改变执行内核 |
| QuantDinger | API/worker/job/cache/DB 分离、OpenAPI、安全 Compose | `qx-api`、`qx-runtime`、`qx-orchestrator`、`deploy/` | P1/P5 | API 不拥有长循环；进程、健康、日志和资源限制可独立验收 |
| quantdigger | 极简策略 API、可视化示例、第一条策略体验 | `qx-strategy` facade、`examples/`、`qx-cli strategy new` | P1 | 新用户 10 分钟内写出并回测第一条策略，仍走 Risk/OMS |
| Qlib | Data/Feature/Model/Workflow/Model Zoo | `qx-factor`、`DatasetManifest`、`ExperimentSpec` | P2 | prediction/model artifact 带数据、代码、模型版本并可复跑 |
| FinRL | 可复现实验、环境/Agent/应用分层、教学 Notebook | `examples/research`、外部 strategy adapter | P2 | RL 只产出已验证的 target/intent，训练不进入执行事实层 |
| VeighNa/vn.py | gateway/app、国内外接口生态、研究到实盘入口 | `qx-adapter`、capability manifest、adapter SDK | P1/P4 | 新 Venue 通过 conformance suite 和 evidence 才能升级为 official |
| RQAlpha | Mod Hook、简单 CLI、可替换组件 | `qx-plugin`、application service、CLI profiles | P1/P5 | 扩展点静态注册、版本校验、依赖拓扑和最小样例齐全 |
| NautilusTrader | MessageBus、Cache、统一节点、crash-only | `qx-runtime`、`qx-api`、`qx-storage` | P5 | 事件/命令/数据 topic 和生命周期边界可独立测试 |
| LEAN | project/optimize/live CLI、可插拔模型 | `ProjectManifest`、`ExperimentSpec`、`RunRecord` | P1/P2 | project、backtest、optimize、paper、live 共用同一身份链 |
| Freqtrade/Jesse | dry-run、plot、通知、策略模板 | `report html`、`notify`、`strategy new` | P1 | 结果可视化、告警和策略模板不复制收益计算 |

### 5.6 竞争定位结论

竞品对比后的定位应从“Rust 高性能量化框架”升级为四层价值主张：

1. **信任层**：牵星比多数 Python 全栈项目更强调确定性、输入 provenance、数据档位和事件重放。
2. **研究层**：吸收 Qlib/FinRL/QuantDinger 的数据集、模型、实验和样本外验证能力，成为可复现研究平台。
3. **交易层**：吸收 QUANTAXIS/vn.py/RQAlpha 的多市场、连接器、账户协议和扩展生态，但所有路径都复用同一 Risk/OMS/Ledger。
4. **产品层**：吸收 QuantDinger/Freqtrade/Jesse/quantdigger 的首跑、策略模板、图表、通知和运维体验，降低 adoption gap。

因此牵星的胜负标准不是“连接器数量最多”或“模型数量最多”，而是：**在拥有现代研究体验和连接器生态之后，仍能对每个结果回答数据是什么、撮合假设是什么、订单事实是什么、能否重放、哪里尚未被真实市场验证。**

## 6. 优化目标架构

### 6.1 目标分层

```text
研究产品层
  qx project / data / backtest / optimize / walk-forward / report / compare
  ExperimentSpec + RunRegistry + HTML/SVG report

稳定应用层
  BacktestService / PaperService / LiveService / ReconcileService
  Command API + Query API + Scheduler

领域内核层
  qx-core + EventLog + Ledger + ReplayVerifier
  Strategy API + Risk + OMS + Execution facts

接入与基础设施层
  Data adapters / Venue adapters / storage backends / worker runtime
  connector capability + schema registry + evidence registry
```

目标是让 `qx-cli` 变成薄的命令适配层；业务编排进入独立 application service，核心领域仍保持单向依赖和单一事实流。

### 6.2 推荐的关键新对象

| 对象 | 作用 | 首要字段/约束 |
| --- | --- | --- |
| `ProjectManifest` | 统一项目入口，替代用户直接拼几十个 JSON | project_id、schema_version、runtime、datasets、strategies、artifact_root |
| `DatasetManifest v2` | 下载、校验、补洞和复现的统一身份 | dataset_id、source、instrument、time_range、timezone、tier、fingerprint、quality_report |
| `ExperimentSpec` | 参数、数据切分、成本、随机种子、输出和基线的唯一声明 | strategy_ref、parameter_space、train/test/walk-forward、seed、cost/risk/model versions |
| `RunRecord` | 记录一次运行及其产物，不依赖目录扫描猜最新结果 | run_id、status、input_digest、code_identity、started/finished、artifact_refs、replay_verdict |
| `CapabilityManifest` | Venue/Provider/Strategy/Storage 的能力和版本声明 | supported_products、order_types、data_tiers、reconcile、sandbox evidence、compatibility |
| `EvidenceBundle` | 把测试网、故障演练和性能结果变成可复核证据 | claim、window、environment、redacted_config_digest、logs、facts、result_digest、operator |
| `SchemaRegistry` | 统一管理 JSON、C ABI、Arrow、wire event 和兼容策略 | schema_id、version、producer/consumer、compatibility、golden fixtures |

## 7. 分阶段优化方案

### P0：生产边界与证据闭环（优先级最高）

目标：把“代码实现”推进到“可受控地证明真实链路”。

工作项：

1. 完成 Binance testnet 的三段验收：只读、dry-run/回报、显式受控下单；记录断线重连、重复成交、五百类响应、未知订单和恢复后的对账。
2. 为 CCXT 公共适配器至少选一个可用 sandbox 做同形验收，不能只用 Binance 作为唯一证据。
3. 将每次验收产物打包为 `EvidenceBundle`，写入 `maturity/evidence/<venue>/<run>/`，并由工具自动校验配置摘要、时间窗口、事实数量和结果 hash。
4. 将能力矩阵拆成 `code_tested`、`paper_tested`、`sandbox_tested`、`production_approved` 四个不可互相冒充的状态。
5. 为 production 配置增加发布前检查、凭据引用检查、数据库/消息服务连通性检查、回滚演练和人工批准记录。

验收标准：

- 至少一个 Venue 的 `sandbox_tested` 由真实 evidence 自动翻转；没有 evidence 时无法手工写成 true。
- 每个未知回报都有明确的 `Unknown → Reconcile → terminal` 轨迹。
- 重启后 EventLog、Ledger、Outbox、订单状态和报告可以复核到同一 run_id。
- 生产模板默认仍 fail-closed，不因新增测试网配置而放开真实下单。

### P1：十分钟首跑和结果可读性

目标：让新用户不读 crate 结构也能完成一次安全回测，并马上看懂结果。

工作项：

1. 引入 `qx project init`/`quickstart` 统一生成 `ProjectManifest`、最小数据集、策略模板和 README。
2. 统一 CLI 参数形状：配置使用一个明确位置或 `--project`，`doctor`、`backtest`、`report`、`status` 不再各有不同的路径习惯。
3. 增加 `report html` 和零前端依赖 SVG：权益曲线、回撤、成交点、月度收益、费用、滑点、持仓、风险拒单、数据档位和输入指纹。
4. 报告首页显示“结果可信度”：输入是否复核、重放是否通过、撮合档位、成本模型、数据质量和是否存在未计算字段。
5. 增加 `strategy new` 模板：Rust、Python、C++ 各一个最小模板；模板声明 API 版本、输入输出样例和测试命令。
6. `doctor` 输出分为 human summary 和 JSON diagnostics，错误只显示相关上下文和下一步，不打印整篇 help。

验收标准：

- 干净环境从 `project init` 到 `report html` 一条路径成功，且不写仓库外的隐含目录。
- 报告可独立打开，包含 `run_id`、`result_hash`、`input_fingerprint` 和 `replay_verdict`。
- 任何报告图表都从已有 summary/equity/fills 生成，不引入第二套收益计算。

### P2：研究实验闭环

目标：补齐“回测之后怎么继续研究”的产品能力。

工作项：

1. 建立 `ExperimentSpec`，统一参数空间、数据切分、手续费/滑点/延迟、初始资金、随机种子和策略版本。
2. 实现第一版网格/随机搜索，后续再接 Optuna 或其他优化器；优化器只能提交实验，不直接修改运行时或 Ledger。
3. 实现 walk-forward：滚动 train/test 窗口、窗口间隔、参数冻结、样本外 summary 和过拟合警告。
4. 对每次实验保存 `RunRecord`，支持 `compare`：收益、回撤、Sharpe/Sortino、换手、费用、成交率、风险拒单和数据覆盖度。
5. 引入策略基线和反事实对照：buy-and-hold、zero-signal、不同撮合档位/成本模型。
6. 对批量实验提供两条执行路径：事件级准确路径和可选的 vectorized research accelerator；两条路径输出必须声明语义差异，不允许结果静默混用。

验收标准：

- 相同 `ExperimentSpec + code_identity + dataset fingerprint` 必须得到相同参数排序和结果哈希。
- train/test 边界、as-of、公司行为和成本规则进入 run identity。
- `compare` 不重新计算收益，只读取已验证的 RunRecord 和产物。

### P3：数据产品化

目标：把现有 `qx-data` 能力变成可用的数据工作流。

工作项：

1. 增加 `qx data list/search/download/validate/repair/register` 命令。
2. 下载任务按 instrument/time range/tier 分片，支持断点续传、补洞和内容寻址缓存。
3. 每个数据集生成质量报告：时间连续性、重复、乱序、缺失、异常值、时区、公司行为覆盖和可用档位。
4. 把主源/备源、provider version、schema version 和内容 hash 写入 manifest；备源只做回填/交叉校验，不能静默替换主源。
5. 对 Bar、Tick、L2 order book 和 A 股公司行为分别维护 schema compatibility matrix。

验收标准：

- 用户只需提供数据集 ID 就能复跑，路径变化不影响输入身份。
- 缺口和质量问题在回测前显示；无法满足策略要求时拒绝运行并点名缺口。
- 数据修复前后有版本差异和 lineage，不覆盖原始数据。

### P4：适配器与策略生态

目标：降低接入新 Venue/Provider/语言的边际成本。

工作项：

1. 把 adapter 分为 Official、Community、Experimental 三层，维护人、支持产品、数据/交易能力和证据状态公开可查。
2. 建立 connector conformance suite：市场数据解析、订单类型、精度/规格、重复回报、断线、限流、对账、时钟和错误分类。
3. `qx-plugin` 继续负责静态 manifest/签名/依赖求解；如未来需要运行时插件，另设 sandbox、ABI 兼容和生命周期契约，不能把当前静态插件误称为热插拔。
4. 发布 Python wheel、C++ SDK 和 Rust crate 的版本兼容表，自动生成 CHANGELOG 片段和迁移提示。
5. 将 `StrategyContractInput` 也生成正式 JSON Schema，并让 Rust/Python/C++ 从 schema 或 golden fixtures 校验；补齐 C++ 的 `margin_mode`、`position_mode`、`leverage` 表达能力，或明确记录由运行时继承的语义。

验收标准：

- 新增一个 adapter 的最小闭环变成“实现端口 + conformance + capability manifest + example + evidence”，不需要修改多处隐式白名单。
- schema、Rust、Python、C++ 字段漂移在 CI 中直接失败。

### P5：架构解耦和性能

目标：在保持单一事实流的前提下，降低编译和演进成本。

工作项：

1. 从 `qx-cli` 抽出 application service：`BacktestService`、`PaperService`、`LiveService`、`ReportService`、`DataService`；CLI 只负责参数解析、调用和格式化。
2. 将 `qx-runtime` 拆成配置/策略、管线/归约、监督/生命周期三个内部边界；先保持 crate 不变，稳定后再拆 crate。
3. 将 `qx-storage` 的 EventLog、Outbox/Queue、Snapshot/Audit、backend adapter 分成清晰模块和 trait；避免一个后端变更触及所有存储域。
4. 为 EventLog 建立 segment index、sequence index 和增量 replay checkpoint，避免每次从头读取、反序列化和计算全量前缀。
5. 对 `LiveEventPipeline` 减少整管线 clone：按不可变快照 + 可验证 staged mutation 分离，保留冲突重试和原子提交语义。
6. 增加基准套件：Bar 百万帧、L1/L2 深度、策略 IPC、Arrow 转换、EventLog append/replay、Outbox 投递、API projection。

验收标准：

- 任何解耦都不能产生第二套 Ledger、RiskGate、订单归约或收益算法。
- 基准报告同时给出吞吐、p95/p99 延迟、内存、产物 hash 和重放结果。
- 增量 replay 与全量 replay 在事件、Ledger、summary 和 hash 上一致。

## 8. 具体优先级与依赖关系

```text
P0 真实 evidence / 生产边界
 ├─→ P4 connector conformance
 └─→ P5 运行时/存储性能

P1 首跑 / 报告
 ├─→ P2 ExperimentSpec / compare
 └─→ P3 data CLI / manifest

P2 + P3
 └─→ P4 策略和数据生态
```

建议首个季度只承诺以下最小闭环：

1. `project init → doctor → backtest → report html`。
2. 一个 Venue 的测试网 evidence 和可重放恢复证据。
3. `data download/validate/register` 的最小版本。
4. `ExperimentSpec` + 网格搜索 + compare 的第一版。
5. Strategy 输入 schema 和 adapter conformance 的第一版。

暂缓：跨节点 HA、WASM 策略、完整桌面 GUI、云端多租户、覆盖所有交易所、复杂指标库。它们都应在 P0/P1/P2 的基础身份和证据稳定后再立项。

## 9. 风险、取舍与非目标

### 9.1 主要风险

- **体验层重复计算**：HTML/compare/optimizer 如果重新实现收益，会破坏当前最重要的单一事实流；所有新读面只能消费已验证产物。
- **适配器数量膨胀**：没有 capability/conformance 就会出现“能连上但订单语义不完整”；必须先声明能力再放行功能。
- **研究加速器语义漂移**：vectorized 路径不能假装等价于 L2 撮合；必须声明数据档位、延迟、成交和成本模型。
- **证据数据泄露**：evidence bundle 不能保存密钥、完整订单敏感字段或用户隐私；只保存脱敏配置摘要和事实摘要。
- **解耦过度**：不要为每个小模块拆 crate；先用 application service 和 trait 把依赖方向固定，再根据编译/协作成本决定拆包。

### 9.2 非目标

- 不把牵星改造成只面向加密货币的 Bot；保留 A 股、多资产、衍生品和数据档位模型。
- 不牺牲 fail-closed 和可复核性来追求更多“默认能跑”的场景。
- 不把 Paper 宣称成与深度回测完全同一台撮合机；产品报告必须显示实际成交模型。
- 不用 stars、连接器数量或历史版本数量代替自身正确性证据。

## 10. 追踪指标

| 类别 | 指标 | 目标方向 |
| --- | --- | --- |
| 首跑 | 从安装到首个 verified report 的步骤数和时间 | 下降；错误可定位 |
| 正确性 | verified replay、input provenance、未知回报待对账覆盖率 | 保持 100% 门禁 |
| 产品 | 有图表/HTML 的运行占比、报告打开成功率 | 提升 |
| 研究 | 每个实验是否具备 train/test、成本、seed、code/data identity | 100% 完整 |
| 数据 | 数据集可复跑率、质量问题前置拦截率、补洞成功率 | 提升 |
| 交易 | sandbox evidence 数、重复回报/断线/恢复演练覆盖率 | 从 0 开始建立真实证据 |
| 工程 | crate 编译热点、EventLog replay p95、adapter conformance 通过率 | 可观测并持续下降/提升 |

## 11. 参考来源

以下链接均为项目官方 GitHub 仓库或官方文档，竞品结论应随其版本变化定期复核：

- [NautilusTrader README](https://github.com/nautechsystems/nautilus_trader) 与 [Architecture](https://github.com/nautechsystems/nautilus_trader/blob/develop/docs/concepts/architecture.md)
- [QuantConnect LEAN README](https://github.com/QuantConnect/Lean)
- [Hummingbot README](https://github.com/hummingbot/hummingbot) 与 [官方文档](https://hummingbot.org/docs/)
- [Freqtrade README](https://github.com/freqtrade/freqtrade)
- [Jesse README](https://github.com/jesse-ai/jesse)
- [vectorbt README](https://github.com/polakowo/vectorbt)
- [Backtrader README](https://github.com/mementum/backtrader)
- [QUANTAXIS README](https://github.com/yutiansut/QUANTAXIS) 与 [架构最佳实践](https://github.com/yutiansut/QUANTAXIS/blob/master/doc/development/best-practices.md)
- [QuantDinger README](https://github.com/OpenByteInc/QuantDinger) 与 [QuantDinger v5 文档](https://github.com/OpenByteInc/QuantDinger/blob/main/docs/README.md)
- [quantdigger README](https://github.com/QuantFans/quantdigger)（仓库已声明停止维护）
- [Microsoft Qlib README](https://github.com/microsoft/qlib)
- [VeighNa/vn.py README](https://github.com/vnpy/vnpy) 与 [English README](https://github.com/vnpy/vnpy/blob/master/README_ENG.md)
- [RQAlpha README](https://github.com/ricequant/rqalpha)
- [FinRL README](https://github.com/AI4Finance-Foundation/FinRL)

## 12. 最终决策

牵星不需要先复制竞品的全部功能，而应按以下顺序构建壁垒：

1. **保持内核正确性优势**：单一 EventLog/Ledger/Replay/Risk/Execution 事实流不变。
2. **先补产品最短板**：quickstart、HTML/SVG 报告、数据 CLI、策略模板。
3. **再补研究闭环**：ExperimentSpec、优化、walk-forward、compare。
4. **同步补真实证据**：测试网、恢复、对账、生产发布闸门。
5. **最后扩生态和性能**：adapter conformance、schema registry、应用解耦、增量 replay 和批量研究加速。

这样可以同时吸收 Nautilus/LEAN 的架构成熟度、Hummingbot 的连接器生态、Freqtrade/Jesse 的用户体验和 vectorbt 的研究效率，又不会丢掉牵星最稀缺的确定性与可复核性。

## 13. 工业级目标：从“内核正确”到“场景可用”

### 13.1 产品目标

牵星下一阶段的目标不是做成所有用户都能直接下单的黑盒平台，而是建设一个分级、可验证、可扩展的量化交易操作系统：

```text
新手：安装 → 创建项目 → 选择市场模板 → 生成策略 → 回测 → 看报告
研究员：数据集 → 因子/模型 → 实验 → walk-forward → 组合/风险 → Paper
交易员：账户/行情 → 策略运行 → 风控 → 执行 → 对账 → 告警/人工处置
运维：发布锁 → 健康检查 → 灰度/回滚 → 事件审计 → evidence → 版本升级
```

所有路径最终都必须落到同一套领域事实：

```text
Input/Data Identity
  → Strategy Decision
  → Risk Decision
  → Order Intent
  → Order/Execution Facts
  → Ledger/Portfolio
  → Projection/Report/Audit
```

产品层可以有 CLI、Web、Notebook、API、MCP 等多个入口，但不能因为入口不同而产生第二套订单、资金、收益或风险语义。

### 13.2 支持等级

将“支持”拆成用户可以理解、工程可以验收的五级，而不是一个模糊的 supported 标记：

| 等级 | 名称 | 用户可以做什么 | 必须具备的证据 |
| --- | --- | --- | --- |
| L0 | Schema/Research | 读取、校验、生成该市场数据和策略产物 | schema、样例、数据质量报告 |
| L1 | Deterministic Backtest | 在本地复现回测，获得 verified report | 输入 fingerprint、撮合/成本模型、ReplayVerifier |
| L2 | Paper/Simulated | 用实时或回放行情做虚拟成交 | Paper 规则、订单事实、恢复和指标 |
| L3 | Sandbox/Testnet | 连接真实测试环境，验证提交/回报/对账 | 脱敏 `EvidenceBundle`、故障演练、sandbox 账户记录 |
| L4 | Controlled Production | 在审批、限额和回滚条件下运行实盘 | production approval、发布锁、值班/告警、回滚演练 |

任何市场、Venue、订单类型或策略语言都必须单独登记等级。L1 通过不代表 L3，L3 通过也不自动代表 L4。

### 13.3 主要场景覆盖矩阵

| 场景 | 目标用户 | 第一阶段必须支持 | 后续增强 | 当前牵星缺口 |
| --- | --- | --- | --- | --- |
| A 股股票/ETF | 国内个人、研究员、量化团队 | 日线/分钟 Bar、T+1、整手、涨跌停、停牌、印花税/佣金、复权与 PIT | Level-2、北向/资金流、券商柜台、组合调仓 | 回测规则已有较强基础，数据源和券商证据不足 |
| 可转债 | A 股套利/轮动研究者 | 转股、赎回/回售、停牌、涨跌停、转股价调整、交易费用 | 转股套利、多腿组合、公司行为日历 | Ledger/CorporateAction 需补专门场景和完整数据供应 |
| 国内期货 | CTA、套利、机构 | 夜盘/交易日历、合约乘数、tick、保证金、手续费、交割/换月、套保/投机 | CTP、组合保证金、期权组合 | 产品规格和保证金已有内核，CTP/真实柜台未完成 |
| 国内期权 | 期权/套利研究者 | 合约链、到期、行权/履约、保证金、希腊值输入、组合腿、指派 | 组合保证金、波动率曲面、自动行权 | 期权完整生命周期和连接器尚需专门建模 |
| 国际股票/ETF | 全球资产配置团队 | 时区/交易日历、多币种、FX 转换、分红拆股、限价/止损、券商订单状态 | IB 等 broker、公司行动自动同步、税费模型 | 多币种 FX 估值和国际 broker conformance 不足 |
| FX/CFD/期货 | 宏观/趋势/套利团队 | pip/tick、合约乘数、隔夜利息、保证金、对冲模式、时区 | 多流动性源、执行算法、滑点校准 | 统一产品模型可复用，真实 Venue/费用/结算待验证 |
| 加密现货 | 加密研究和做市团队 | CEX REST/WS、maker/taker、精度、限频、部分成交、余额对账、Paper | 多交易所路由、做市、订单簿增量、WebSocket 热恢复 | Binance/CCXT 有基础，sandbox evidence 和多 Venue 一致性不足 |
| 永续/交割合约 | 加密衍生品团队 | funding、reduce-only、post-only、isolated/cross、杠杆、强平、结算币 | 组合保证金、跨交易所对冲、风险限额同步 | 内核有衍生品语义，外部回报和压力场景证据不足 |
| 多策略/多账户 | 团队和资管 | account/portfolio/strategy 隔离、资金分配、净额、归因、权限 | 母子账户、组合风险、跨账户限额 | 事实模型已有基础，产品配置和运维读面需简化 |
| 跨 Venue 套利 | 专业量化团队 | 多腿 barrier、部分成交、补偿、对账、风险停止 | 延迟校准、智能路由、跨 Venue 资金调度 | 多腿模型已有，真实双 Venue evidence 缺失 |
| 研究/ML/RL | 研究员和高校 | dataset/feature/model/target artifact 与回测绑定 | Qlib/FinRL adapter、模型服务、在线滚动 | `qx-factor` 仍偏工件绑定，缺完整实验工作流 |

### 13.4 明确产品边界

第一期工业级目标是**标准实时交易和研究平台**，不是交易所机房内的超低延迟 HFT 基础设施。建议把性能目标拆为：

- `Research`：批量回测、参数实验、数据处理吞吐优先。
- `Realtime`：稳定的行情到策略到风控到订单链路，支持常规量化和做市前置研究。
- `LowLatency Extension`：未来可通过专用 C++/Rust adapter、固定内存、旁路存储和内核线程模型单独立项。

在没有专门的时钟同步、内核旁路、硬件网络、撮合回放和延迟证据前，不能将标准实时链路包装成 HFT。

## 14. 工业级目标架构

### 14.1 六平面架构

```text
┌─────────────────────────────────────────────────────────────┐
│ Experience Plane                                             │
│ CLI / Web / Notebook / API / MCP / Report                   │
├─────────────────────────────────────────────────────────────┤
│ Research Plane                                               │
│ Dataset / Feature / Model / Experiment / Optimize / Compare │
├─────────────────────────────────────────────────────────────┤
│ Trading Plane                                                │
│ Market Data / Strategy / Risk / OMS / Execution / Reconcile │
├─────────────────────────────────────────────────────────────┤
│ Accounting Plane                                             │
│ EventLog / Ledger / Portfolio / Snapshot / Attribution       │
├─────────────────────────────────────────────────────────────┤
│ Control & Operations Plane                                   │
│ Auth / Policy / Scheduler / Audit / Health / Alert / Release │
├─────────────────────────────────────────────────────────────┤
│ Integration & Infrastructure Plane                           │
│ Venue / Provider / Storage / Queue / Schema / Evidence       │
└─────────────────────────────────────────────────────────────┘
```

建议将现有 crate 映射为：

| 目标平面 | 现有落点 | 重构原则 |
| --- | --- | --- |
| Experience | `qx-cli`、`qx-api`、未来 Web/report | 入口薄、只调用 application service，不自行计算收益或风险 |
| Research | `qx-data`、`qx-guanxing`、`qx-factor`、`qx-datastruct`、新增 experiment | 研究工件必须带 data/model/code identity，可转为策略输入但不能直接写 Ledger |
| Trading | `qx-strategy`、`qx-risk`、`qx-zhenlu`、`qx-xingban`、`qx-execution`、`qx-adapter` | 所有订单统一经过 intent → risk → OMS → execution facts |
| Accounting | `qx-core`、`qx-protocol`、`qx-storage` | EventLog 是事实源，Ledger/Portfolio/Report 是投影，禁止旁路记账 |
| Control/Ops | `qx-control`、`qx-scheduler`、`qx-runtime`、`qx-orchestrator`、`qx-api` | 命令、权限、生命周期、审计和健康状态解耦 |
| Integration/Infra | `qx-provider`、`qx-storage`、`qx-plugin`、`deploy`、`maturity` | 能力先声明、适配器先 conformance、发布先 evidence |

### 14.2 单机与分布式两种拓扑

#### Single-node Research/Paper

适合个人、新手、策略开发和本地 Paper：

```text
qx-cli / qx-agent
  ├─ strategy worker
  ├─ paper/execution worker
  ├─ local scheduler
  ├─ SQLite or Files
  ├─ local eventlog/outbox
  └─ report + local API
```

要求：安装依赖少、默认不联网下单、项目目录自包含、失败可解释、删除项目即可清理，不要求用户先部署 PostgreSQL/NATS/Kafka。

#### Distributed Production

适合团队和生产部署：

```text
Web/API Gateway (stateless, mTLS/OIDC)
          │ commands/queries
Control Service ── Audit/Policy DB
          │
Scheduler ── Job Queue ── Strategy Workers
          │                         │ intents
Market Data Workers ───────────────┘
          │                         ▼
     Event/Command Router ── Risk/OMS/Execution Workers
                                      │
                           Venue Adapters / User Streams
                                      │
               PostgreSQL EventLog + Outbox + Snapshots
                                      │
                   Projections / Reports / Metrics / Alerts
```

生产拓扑必须明确：谁拥有订单提交权、谁拥有账户事件日志、谁可以执行控制命令、谁可以读取密钥、谁负责对账、谁负责最终人工处置。API 不能直接成为长生命周期交易循环的宿主。

### 14.3 领域边界和依赖规则

建立以下不可破坏的依赖规则：

1. `qx-core` 不依赖 provider、adapter、API、数据库和具体策略语言。
2. `qx-data` 负责数据身份、质量和可见性，不负责下单和资金记账。
3. `qx-strategy` 只产生 decision/intent，不产生 Accepted/Fill，不读凭证。
4. `qx-risk` 只裁决风险，不写 Venue，不拥有 Ledger；风险事实由执行应用落入 EventLog。
5. `qx-xingban` 只实现研究/回测撮合和成本模型；Paper 成交模型必须显式标注，不与深度撮合混淆。
6. `qx-execution` 是外部回报到内部执行事实的唯一转换层。
7. `qx-core::EventLog` 是事实源；API、报告、快照和指标都是投影。
8. `qx-control` 只接受带 identity、permission、reason、request_id 的命令；执行者由应用层提供。
9. `qx-storage` 提供持久化端口和事务边界，不把具体业务决策藏在后端实现中。
10. `qx-cli` 不直接拼接订单、不直接写 Ledger、不实现第二套 backtest/reconcile 算法。

用 `tools/check_architecture.py` 继续维护上述规则，并为每条规则增加“反向变异”验证：删除调用点、绕过端口或增加第二定义时，门禁必须变红。

## 15. 核心场景的详细领域设计

### 15.1 统一身份与市场规格

统一身份建议固定为：

```text
Tenant/Organization
  → Account
    → Portfolio
      → Strategy
        → Signal/Intent
          → Instrument
            → Product/Contract
              → Venue/Market
                → DataSource
```

`InstrumentId` 不能承担产品规格。应由 `TradingInstrumentSpec` 明确表达：

- 产品类型：equity、etf、bond、convertible_bond、future、option、perpetual、spot、fx、cfd。
- 计价/结算：base、quote、settlement、margin currency、contract multiplier。
- 精度：price tick、quantity step、lot size、min/max quantity、notional limits。
- 交易：order types、time-in-force、post-only、reduce-only、position mode、leverage。
- 账户：cash/margin、cross/isolated、hedge/one-way、short availability。
- 生命周期：listed、trading sessions、expiry、delivery、exercise、delisting。
- 费用：maker/taker、commission、stamp duty、transfer fee、funding、borrow/overnight。

规格必须带 `spec_version` 和 `source`。运行时使用的规格要冻结到 RunManifest/Order/Fill/Reconcile 事实中，不能在恢复时重新从远端猜测。

### 15.2 A 股、可转债和国内期货/期权

#### A 股/ETF/可转债

必须拆成可组合规则，而不是一个 `ashare=true` 开关：

```text
TradingCalendar
 + Session/auction/holiday
 + T+1 settlement
 + lot size / odd lot close
 + daily price limit / suspension
 + corporate actions / PIT
 + fees and stamp duty
 + cash/position availability
 + northbound or broker-specific constraints
```

可转债另需：转股价和除权调整、转股/回售/赎回、债券面值与最小交易单位、停牌/强赎状态，以及股票与转债两腿的公司行为关联。

验收重点：同一数据在不同 `as_of` 不泄漏未来公司行为；买入后当日不可卖出；涨跌停锚点可解释；整手、费用和资金约束进入重放事实；公司行为发生时持仓、成本和可转数量可重放。

#### 国内期货

国内期货应支持交易所夜盘和自然日不同步的交易日历，至少包含：

- 合约乘数、最小变动、交易单位、保证金比例和手续费规则的版本化。
- 开平仓、平今、套保/投机、双向持仓、强平、涨跌停和连续合约映射。
- 日内/隔夜结算、浮盈浮亏、保证金占用、可用资金和追加保证金。
- 到期换月和主连研究数据与真实合约执行身份分离。
- CTP/仿真柜台的登录、订阅、订单状态、成交回报、断线重连和对账。

研究连续合约只能生成研究信号，不能直接提交连续合约订单；订单必须落到具体可交易合约并把映射证据写入 RunManifest。

#### 国内期权

期权不能只复用期货的线性保证金模型，应增加：

- option contract identity：标的、行权价、到期日、认购/认沽、行权方式。
- option chain snapshot 和 volatility surface 的时间点绑定。
- 买方权利金、卖方保证金、组合保证金和风险限额。
- 行权、指派、到期、自动行权和现金/实物交割事件。
- spread/straddle/covered call 等多腿组合的腿级成交与组级风险。

第一阶段可以把 Greeks 作为只读观测和风险输入，不把模型估值伪装成真实成交事实；模型来源、参数和版本必须随风险报告保存。

### 15.3 国际股票、FX 和加密衍生品

#### 国际股票/ETF/FX

- 每个 Venue 独立保存 timezone、session、holiday、early close 和 daylight-saving 规则。
- 现金、持仓、费用和收益按账户本位币记账，FX conversion 使用带时间点和来源的报价。
- 分红、拆股、合并、ADR ratio、symbol change 等公司行动作为事实事件。
- IB 等 broker adapter 应将 broker order id、client id、execution id、commission report 和 account snapshot 统一映射，并把“远端已成交、本地未知”转成对账问题而不是新订单。

#### 加密现货/永续/交割合约

- 交易所 symbol、contract、settlement asset、margin asset 和显示名称分开。
- REST/WS 连接器分别处理限频、时间戳、listen key、sequence gap、snapshot+delta、重连和回放。
- 将 maker/taker、funding、borrow、liquidation、insurance、ADL 等费用和风险事件显式化。
- 对 `reduce_only`、`post_only`、`position_side`、`margin_mode`、`leverage` 做 Venue capability 校验；不支持就拒绝，不转换成近似订单。
- 用测试网和沙盒记录验证精度、部分成交、撤单竞态、重复 trade、订单状态未知、余额快照漂移和资金费。

### 15.4 多策略、多账户和跨 Venue

统一处理层次：

```text
Signal → StrategyTarget → PortfolioTarget → AccountAllocation
       → Netting/ConflictPolicy → OrderIntent[]
       → Account/Portfolio Risk → Venue Risk → OMS
```

必须区分：

- 策略想要的目标仓位。
- 组合净额后的目标仓位。
- 账户受限后的可执行目标。
- Venue 精度、余额、保证金和订单类型过滤后的最终意图。
- 实际 Accepted/Fill/Rejected/Unknown 事实。

跨 Venue 套利要有组级状态机：`Planned → LegSubmitted → PartiallyFilled → Hedged/Unhedged → Reconciled → Closed`。任何一腿未知或对账未完成时，策略不得自动重复提交整组；补偿单必须带原组 identity、原因和审批策略。

## 16. 用户易上手设计

### 16.1 三种入口，不让复杂性泄漏给新手

| 入口 | 目标用户 | 默认体验 | 高级能力如何进入 |
| --- | --- | --- | --- |
| `quickstart` | 新手 | 内置数据、MACD/均线模板、一次命令、HTML 报告 | `--profile`、项目文件和 config explain |
| `project` | 研究员 | 项目 manifest、数据集、策略、实验和 report | Python/Rust/C++ strategy SDK |
| `run`/API | 交易员/运维 | Paper/Live 状态、health、audit、reconcile | 权限、审批、发布锁、mTLS、operator API |

### 16.2 推荐的首跑路径

```bash
qx-cli init my-qx --profile crypto-paper --strategy macd
qx-cli doctor my-qx
qx-cli data validate my-qx/datasets/demo
qx-cli backtest my-qx
qx-cli report my-qx --html
qx-cli paper-check my-qx
```

A 股、国内期货、国际股票、加密和多腿场景都应有对应 profile，但 profile 只生成自包含项目，不代表它已经具备真实 Venue 证据：

```text
ashare-research      A股研究/复权/PIT/交易规则
ashare-paper         A股 Paper，不连接券商
cn-futures-research  国内期货合约/保证金/夜盘
cn-options-research  国内期权链/到期/组合风险
global-equity-paper  国际股票/ETF/多币种
fx-paper             FX/CFD 费用与保证金
crypto-paper         加密现货/永续 Paper
crypto-testnet       加密测试网，必须额外提供凭据和 evidence
multi-venue-arb      多 Venue 多腿，只默认 Paper
```

### 16.3 每个入口的输出规范

所有命令同时提供 human 和 JSON 两种输出，JSON 最少包含：

```json
{
  "ok": true,
  "command": "backtest",
  "run_id": "...",
  "build_identity": "...",
  "config_fingerprint": "...",
  "input_fingerprint": "...",
  "capability_level": "L1",
  "warnings": [],
  "artifacts": [],
  "next_actions": []
}
```

错误必须包含 `code`、`stage`、`entity`、`reason`、`remediation` 和 `safe_to_retry`；不得把网络超时、未知成交、配置不匹配和用户参数错误都压成一条“运行失败”。

## 17. 工业级非功能目标与验收指标

以下是目标值，最终阈值要以基准环境和场景 profile 冻结，不能把目标值当成当前实现现状。

| 维度 | Research/Single-node 目标 | Production/Distributed 目标 | 验收方式 |
| --- | --- | --- | --- |
| 正确性 | 同输入、同版本、同 seed 结果 hash 相同 | 事实重放与投影一致，未知态不自动补单 | golden replay、故障注入、变异测试 |
| 数据 | 关键字段质量问题 100% 在回测前暴露 | 主备切换有 lineage，不静默改变 fingerprint | dataset quality suite |
| 可用性 | 项目目录可独立运行和恢复 | API/worker 可独立重启，单 worker 失败不丢事实 | restart/kill/partition 演练 |
| 持久化 | 本地崩溃后不产生半条事实 | PostgreSQL 事务 + outbox，RPO=0（已提交事实） | fsync/DB failure tests |
| 恢复 | RTO 目标 < 5 分钟 | RTO 目标 < 1 分钟，订单未知优先对账 | timed recovery evidence |
| 实时性 | 研究吞吐按 bars/s、events/s 报告 | 标准实时链路 p99 目标 < 100ms；低延迟另立项 | benchmark + live timestamp |
| 安全 | 默认无凭据、无下单 | mTLS/OIDC、最小权限、密钥不进日志、发布锁 | secret scan、ACL、security CI |
| 可观测 | 每次运行有 run_id、hash、报告和诊断 | logs/metrics/traces/audit 四类关联同一 correlation_id | Prometheus/OpenTelemetry/审计查询 |
| 兼容 | 旧产物可明确升级或拒绝 | schema/ABI/API 有兼容矩阵和迁移脚本 | golden fixtures + upgrade tests |
| 性能 | EventLog 增量 replay 不随全量日志线性恶化 | 热路径无整管线 clone，存储/投影异步但事实提交有边界 | criterion/负载/内存基准 |

建议将这些目标写入 `maturity/targets.yaml`，以 profile、场景、硬件、版本和证据路径为键；不允许把“所有场景一个数字”作为工业级性能结论。

## 18. 详细重构工作包

### M0：基线冻结与领域契约（2 周）

涉及：`qx-core`、`qx-protocol`、`schemas/`、`maturity/`、`tools/check_architecture.py`。

- 冻结 Instrument/Product/Venue/DataSource/Account/Portfolio/Strategy 的 identity 规则。
- 冻结 `TradingInstrumentSpec`、OrderIntent、ExecutionFact、AccountSnapshot、RunManifest v1。
- 为 A 股、国内期货、国内期权、国际股票、FX、加密现货/永续各提供最小 fixture。
- 生成 StrategyContractInput/Output 双向 JSON Schema；删除“Rust 有结构、输入方向靠文档”的缺口。
- 增加 schema compatibility、capability level 和 evidence 状态门禁。

完成标准：

- Rust/Python/C++ 对同一组账户/订单/策略 fixture 互读互拒。
- 未来版本、未知字段、错误身份和缺失精度都有稳定机器错误码。
- 领域契约变更必须同时更新 schema、fixtures、迁移说明和 golden replay。

### M1：项目与首跑产品（2–3 周）

涉及：`qx-cli`、新增 `qx-application`（初期可作为 `qx-cli` 内部模块）、`deploy/`、`docs/`。

- 引入 `ProjectManifest` 和 profile registry。
- 统一 `init/doctor/data/backtest/report/paper-check/status` 参数形状。
- 增加 `strategy new`、`data validate`、`report --html/--svg`。
- 每个 profile 生成自包含数据、规格、成本、策略和 README。
- 统一 human/JSON output、error code、next actions 和 artifact links。

完成标准：

- 新用户不安装数据库即可完成 research/paper 首跑。
- 任一失败点可复制其命令重跑，不打印无关入口墙。
- 生成的报告能展示输入、模型、风险、撮合、收益和可信度，而不是只显示一个收益率。

### M2：数据平面和场景规则（4–6 周）

涉及：`qx-data`、`qx-guanxing`、`qx-provider`、`qx-datastruct`、`qx-factor`、`qx-xingban`。

- Dataset catalog v2：内容寻址、分片、补洞、质量报告、主备 lineage、PIT window。
- Trading calendar registry：交易所、时区、夜盘、临时休市、早收市和版本。
- Corporate action registry：分红、拆股、转股、赎回、回售、期权行权/指派。
- Market spec registry：股票/ETF/可转债/期货/期权/FX/crypto 的规格和费用。
- 数据档位到撮合模型的自动准入：Bar/L1/L2/L3 能力不能由用户字符串绕过。

完成标准：

- 每个 profile 能用一份 dataset manifest 完成校验、回测和复跑。
- A 股 PIT、T+1、涨跌停；国内期货夜盘/乘数/保证金；期权到期/组合；加密 funding/精度均有行为用例。
- 不同 provider 的同一数据可对比但不能覆盖主源身份。

### M3：研究与实验系统（4–6 周）

涉及：`qx-factor`、新增 `qx-experiment`、`qx-cli`、报告层、Python research adapter。

- `ExperimentSpec`、`RunRecord`、`ParameterSpace`、`SplitPlan`、`MetricSet`、`Baseline`。
- 网格/随机/TPE adapter；优化任务经 scheduler 执行，结果只落实验记录。
- walk-forward、blind holdout、成本压力、bootstrap、PBO/Deflated Sharpe 等研究诊断。
- 比较页面/命令只读取 verified artifact，不重新计算资金曲线。
- Qlib/FinRL/Notebook 输出转换为 Dataset/Feature/Prediction/PortfolioTarget artifact。

完成标准：

- 实验可以暂停、恢复、取消、重跑；同一 identity 不重复执行或明确标记复用。
- 训练集、验证集、测试集和最终 holdout 不能重叠，as-of 和公司行为检查有效。
- ML/RL 结果进入执行前必须经过策略输入校验、风险门和订单归约。

### M4：交易与适配器工业化（6–8 周）

涉及：`qx-adapter`、`qx-execution`、`qx-runtime`、`qx-orchestrator`、`maturity/evidence`。

- Adapter SDK：MarketDataClient、ExecutionClient、Reconciler、InstrumentProvider、Clock/RateLimit、Capability。
- Official/Community/Experimental 分级和 conformance suite。
- 目标适配器分批：Binance/OKX/CCXT → IB/国际 broker → CTP 国内期货 → 国内券商（XTP/QMT/TORA 等需供应商条件）。
- 每个适配器提供 public data、private stream、submit/cancel、reconcile、sandbox、failure injection 六组用例。
- 把远端未知态、重复回报、sequence gap、部分成交、撤单竞态和余额漂移纳入统一错误 taxonomy。

完成标准：

- 新 Venue 不需要在多个 CLI 分支增加隐式白名单；通过 capability manifest 自动生成准入。
- 至少 Binance/CCXT 一个国际 Venue 和一个国内期货/券商类 Venue 完成 L3 evidence，才可宣称“国内外接入基线”。
- 任何真实下单入口都有配置指纹、权限、限额、kill switch、审计和对账出口。

### M5：运行时、存储和运维（4–6 周）

涉及：`qx-runtime`、`qx-storage`、`qx-api`、`qx-control`、`qx-scheduler`、`deploy/`、`.github/workflows/`。

- 从 `qx-cli` 抽取 application service，API 不持有长循环。
- `LiveEventPipeline` 改为 append/reduce/projection 明确阶段，消除全量 clone 和全量 replay 热点。
- EventLog segment/index/checkpoint/retention 设计；明确事实、投影、审计和 dead-letter 的保留周期。
- PostgreSQL transaction + outbox + fencing + consumer checkpoint 的生产演练。
- OpenAPI、request/correlation ID、Prometheus、traces、告警规则和 runbook。
- 单机/分布式 Compose 配方、非 root、只读根文件系统、资源限制、密钥挂载和备份恢复。

完成标准：

- API、scheduler、strategy、execution、reconcile 可独立停止和恢复。
- kill -9、数据库断连、消息重复、网络分区、Venue 返回未知、磁盘满等场景有自动化 evidence。
- 生产健康状态区分 liveness、readiness、dependency health、trading safety 和 reconciliation status。

### M6：生态、发布和商业级治理（持续）

涉及：`qx-plugin`、SDK、`schemas/`、发布 workflow、文档站和社区。

- SDK 版本兼容矩阵：Rust、Python wheel、C++ ABI、Strategy API、Account Snapshot、Event schema。
- 插件/适配器签名、SBOM、SLSA provenance、漏洞扫描、license 清单和发布审批。
- 中文/英文文档、场景教程、失败案例、迁移指南、示例数据和社区 adapter registry。
- 变更分级：领域事实、wire schema、策略 API、配置、report、实验和 adapter 各有兼容级别。
- 把每次生产演练、性能基准和 sandbox 验收纳入版本发布证据，而不是散落在日志目录。

完成标准：

- 任一发布物可以追溯到源码、构建、schema、依赖、测试、性能和安全证据。
- 用户升级时能先执行 migration/doctor，发现不兼容时阻止启动而不是损坏旧账。
- 社区适配器可以在不修改内核的前提下开发和验证。

## 19. 重构实施纪律

### 19.1 一次只改变一个事实边界

每个工作包必须明确：

```text
旧边界 → 新边界 → 数据迁移 → 兼容窗口 → 反向验证 → 回滚方式
```

不允许在同一提交中同时改变账户身份、订单状态、存储格式、API 路由和 Venue 语义；否则测试通过也无法定位结果变化来源。

### 19.2 迁移顺序

推荐顺序：

1. 先加新 schema/读兼容和 golden fixtures。
2. 再增加新 writer/adapter/service，但默认不启用。
3. 用双读或 shadow run 比较旧/新结果 hash、事件数量、Ledger 和投影。
4. 通过 feature/profile 切换流量，保留回滚开关。
5. 迁移完成后再删除旧路径，并增加“不允许旧路径复活”的架构门禁。

### 19.3 每个能力的 Definition of Done

一个功能只有同时满足以下条件才能标记完成：

- 领域模型、schema、配置、CLI/API、文档和示例齐全。
- 至少一个正向行为用例、一个错误/拒绝用例、一个重启/重复用例。
- 产物中有 input/code/config/model/cost/risk identity。
- 可以在 Paper 中跑通；需要外部连接时有 sandbox evidence。
- 有指标、日志、告警、runbook 和恢复步骤。
- 有性能基线、资源边界和安全检查。
- `maturity/capabilities.yaml` 的等级和 evidence 路径与真实状态一致。

## 20. 工业级最终决策

牵星的工业化路线确定为：

1. **核心不变**：继续坚持 Rust 确定性内核、定点数、事件溯源、唯一 Ledger、ReplayVerifier、数据档位门禁和 fail-closed。
2. **场景扩展**：以 A 股/可转债、国内期货/期权、国际股票/FX、加密现货/衍生品、多账户/多策略为首批 profile；每个场景按 L0–L4 逐级交付。
3. **用户体验前置**：先完成 project/profile/quickstart/strategy template/data validate/report HTML，让新用户十分钟完成安全首跑。
4. **研究体系独立**：用 Dataset/Feature/Model/Experiment/RunRecord 建立 Qlib/FinRL/QuantDinger 风格的研究层，研究产物只能以受控 artifact 进入交易层。
5. **交易体系标准化**：用 Adapter SDK、Capability Manifest、Conformance Suite 和 EvidenceBundle 吸收 vn.py/QUANTAXIS/Hummingbot 的生态优势，但不放宽订单、对账和权限门禁。
6. **运行时工业化**：将 API、调度、worker、执行、对账、存储和观测解耦；单机默认轻量，生产显式 distributed，生产拓扑必须可恢复、可审计、可回滚。
7. **发布可证明**：每个版本同时交付二进制、wheel、SDK、schema、SBOM、性能、安全、sandbox/production evidence 和迁移说明。

最终产品应达到的体验是：**新手不需要理解 23 个 crate 就能开始；研究员可以复现、比较和验证实验；交易员可以安全运行和人工接管；运维可以观测、恢复和回滚；审计可以从结果追溯到输入、代码、规则、订单和外部回报。**
