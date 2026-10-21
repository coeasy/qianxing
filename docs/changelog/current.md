# Recent entries

### 0.2.0 版本对齐与退役门禁说明清理（2026-10-21）

把当前 workspace、Rust 锁文件、Python 包、CMake SDK、成熟度发布记录与安装文档统一到 0.2.0。删除活动 Rust 源码和检查器注释中残留的历史行数棘轮/用例地板说法；历史变更记录保留为审计背景，不参与当前验收。架构快速分片复核发现版本动态检查漏导入 `tomllib`，改为直接读取 `[workspace.package]` 的版本声明后，四片完整聚合通过。

- 验证：`cargo metadata --offline` 成功；29 个 workspace 包版本均为 0.2.0；Rust workspace 165 个测试目标合计 1,941 passed、0 failed、2 ignored；默认与无 backend feature 的全目标严格 Clippy `-D warnings`、feature-off `cargo check`、Rust fmt、Python 199/199、架构 `--fast` 四片完整覆盖 221 个具名检查且无重复、成熟度生成检查、Markdown 链接和 `git diff --check` 均通过。
- 本机 Release CLI 输出 `qianxing 0.2.0 (build 343d405-dirty, target x86_64-pc-windows-msvc, profile release)`，SHA-256 `D737F1EAE554B2169643C39FC0767B4BFC3DA7E921C31E33EAAD52616334C9D7`。用该二进制重新执行 Crypto/A 股确定性验收，result_hash 为 `26fdd6b52d020700` / `6be034a4cb0760fa`，并通过 Paper E2E（提交 1 单、成交 1 单、账簿 4 条）；三份本机验收记录已刷新。工作树仍有未提交改动，该二进制不是干净发布候选，性能证据仍需与干净候选二进制重新绑定。
- 干净提交 `5db568f2` 的 Release CLI 输出 `qianxing 0.2.0 (build 5db568f, target x86_64-pc-windows-msvc, profile release)`，SHA-256 `044DC5647DE5B9791B55F1E95CA2D4E018A9E49CF10C13CB55E18A27221F57BF`；同二进制执行 Crypto/A 股与 Paper dry-run 均通过，结果 hash 不变，Paper 为 1 单提交、1 单成交、4 条账簿事实。该干净构建取代上一条 dirty 二进制的本机候选核验；远端矩阵尚待 PR CI。

### 0.1.0 本机全量回归与历史快照门禁清理记录（2026-10-21）

移除以历史文件/测试数量、固定源码行数及静态回测哈希为依据的验收条件，保留实际架构接线、行为语义、产物完整性与重放一致性验证。全量回归发现并修正跨服务迁移后的调用点核对、现货无库存卖出归因假设，以及本地 Python 原生扩展陈旧导致的 CLI/HTTP/Python 哈希差异。用当前 Release 重建 Python stable-ABI wheel，并刷新 Crypto/A 股及 Paper 验收证据。

- 验证：Rust workspace 165 个测试目标全部通过；架构检查 221 项完整聚合；全目标严格 Clippy、生产代码 unwrap/expect lint、无 backend feature Clippy、Rust fmt、Console JS 语法、Python 199/199、成熟度生成校验及 Markdown 链接均通过。Windows Python 3.13 wheel `qianxing-0.1.0-cp310-abi3-win_amd64.whl` 构建、tag/内容检查及隔离目录原生导入 smoke 通过，SHA-256 `0101a934fa9e2070a6d3b275db1bebca90cb0707525b01f3bb5949a067e6dd95`。
- 后续针对移除历史门禁后调整的 feature 条件断言复验：默认、SQLite、PostgreSQL、NATS、PostgreSQL+NATS 五种配置下 qx-cli 全测试均通过；各配置 9 个测试目标，默认 `cli_behavior_parity` 32/32、`project_commands` 45/45。严格 Clippy、check 通过。测试构建使用隔离 target 与仓库临时目录；MSVC 隔离构建关闭调试符号，并为 PostgreSQL 组合增加 rustc 线程栈以完成大型 qx-cli crate 编译。
- 候选 `qx-cli` Release SHA-256：`79324835b0e912665a26cac72558e0eb873805e9bd83fc434ba8396a6afb672e`。以此二进制重验 Crypto/A 股样例回测（result_hash `26fdd6b52d020700` / `6be034a4cb0760fa`）、A 股规则 1k/8k/64k profile（P50 281.050/309.601/547.507 ms）及 Crypto Paper 三轮 E2E（P50/P95 2672.090/2716.694 ms）；两项 profile 证据均绑定该二进制 SHA。
- 尚未声称生产批准：Linux/macOS 与 Python 3.10–3.12 矩阵、PostgreSQL/NATS 服务容器、Linux/macOS C++ SDK 矩阵未在本机执行；Windows C++ SDK 构建、CTest、安装包消费者、JSONL、Rust C ABI 插件与两种共享内存 Worker 协议已通过本机复核。Rust SDK Git 消费者需要已发布候选提交，Binance testnet 需要外部凭据。`ashare-research`、`crypto-paper`、`multi-venue-arb` profile 仍按覆盖范围保持 `unmeasured`。
- Windows C++ Worker 首次运行遇到系统临时目录 ACL 拒绝；切换到仓库隔离临时根后 shared-memory JSON/columnar 均通过。QXRB 文件打开错误现在包含失败路径及 Win32 错误码，便于区分路径/权限问题。

### A 股规则回归证据与 SDK 测试目录隔离（2026-10-21）

将 A 股规则合同与完整回测生命周期纳入量化场景探针；以当前源码重建 Release 后复测 1k/8k/64k 分钟 Bar、PIT 公司行为与确定性哈希。Rust SDK 测试临时目录改用进程号加纳秒时间戳，避免 PID 复用读取旧产物而出现假冲突。

- 验证：A 股规则合同 33/33、生命周期集成 8/8、Rust SDK 两个独立消费者目标 28/28 与 2/2；量化场景 81 项 executed、0 未执行、0 缺失、2 项外部阻塞。原吞吐证据采用 weekday-only 合成日历，已由 WP-7.89 的沪深公开日历负载补充；合成价格与全部规则分支未纳入吞吐负载，`ashare-research` 状态保持 `unmeasured`。

### 多 Venue Paper 补偿恢复场景证据贯通（2026-10-21）

扩展 Paper 多腿恢复用例，验证 BINANCE 部分成交、OKX 另一腿取消后的跨 Venue 补偿闭环，并继续检查冻结规格对越界补偿回报的拒绝。修正量化探针读取 L2 多策略版本化摘要的路径；将依赖远端已发布提交的 Rust SDK Git 消费者从无网络探针中移出，登记为候选提交发布后的单独验收。

- 验证：恢复用例 2/2；量化场景 79 项执行通过、2 项保留外部阻塞；架构 `--fast` 221 项检查完整聚合通过。`multi-venue-arb` 性能 profile 仍为 `unmeasured`。

### 修复 qx-app 全目标回归的陈旧定资与结果哈希断言（2026-10-21）

qx-app 回归发现手续费预留向上取整后仍断言旧 raw 金额，以及 OrderBook/独立多 builtin 当前稳定结果与旧哈希不符。根据当前精确定资和回测引擎输出更新固定期望，并移除声称仍等于 legacy hash 的过时测试名；保留每次重放的确定性比较。

- 定向验证：qx-app 32 个测试目标 200/200 通过；qx-app/qx-api/qx-sdk 格式检查与全目标严格 Clippy 通过。

### 当前源码 Paper 执行与账簿闭环复验（2026-10-21）

使用刚重建的 qx-cli 验证服务层风险准入、执行 worker 和一次性订单宿主迁移后的 Paper 交易闭环。

- 定向验证：`paper_acceptance.py` 通过；提交 1 单、成交 1 单、账簿事实 4 条；重跑不重复下单，审计链到 Executed，独立目录结果一致。该结果不代表真实 Venue 网络验收。

### 当前源码 A 股/币圈与 API/SDK/Console 链路回归（2026-10-21）

重建当前 qx-cli 后复跑 A 股/币圈双目录确定性验收；验证 qx-api、Rust/Python SDK 和 Console 新增异步研究路由。新菜单与 qx-api RunHandle 启动路由的双向接线由架构检查核对。

- 定向验证：A 股/币圈 `result_hash` 为 `6be034a4cb0760fa` / `26fdd6b52d020700`；qx-api 122 项、Rust qx-sdk 44 项、Python SDK/provider/native E2E 49 项通过；控制台脚本语法通过；API/Console/Rust SDK 接线检查 84 项通过。本轮不是开发完成后的三轮终态全链路审查。

### Rust 核心与回测引擎定向回归（2026-10-21）

对当前工作树的 qx-core 账簿/估值和 qx-xingban A 股/组合/OrderBook 改动执行 crate 全目标回归；将仓库内隔离测试目录 `.tmp-test/` 加入忽略规则，保留本机数据不删。

- 定向验证：qx-core 80/80；qx-xingban 215 passed、0 failed、1 ignored；两 crate 格式检查与全目标严格 Clippy 通过。账簿现金查询 Criterion 的 indexed 1k/8k/64k 中位数为 104.8/131.1/181.6 ns；64k entry-scan 对照约 323.0 µs。1k 相对同机 Criterion base 点估计 +7.8%，未超过台账 10% 阻断阈值；单组读数不代表完整 profile。
- 候选卫生：将 `.tmp-test/`、CLI runtime store 新生成 runs/队列及 deploy runtime store 新生成 runs/snapshots 加入忽略规则；文件保留，原有跟踪基线保留。剩余 130 个未跟踪资产待审并纳入合适提交。

### 修复 feature-off CI 工作流并覆盖服务层（2026-10-21）

修复 CI feature-off 两条多行命令的 YAML 缩进错误，确保工作流可解析；将新迁入的 `qx-service` 纳入无 backend feature 的全目标检查与严格 Clippy。

- 定向验证：工作流 YAML 解析成功（12 个 job）；feature-off `cargo check --all-targets` 与 Clippy `-D warnings` 通过；架构门禁 `--fast` 四分片完整聚合通过；`git diff --check` 通过。

### Worker market spec 启动预检归服务层（2026-10-21）

将运行时配置中的 worker spec 路径检查、声明标的绑定和逐标的冻结规格校验迁入 `qx-service::worker_market_spec`。runtime-check 保留错误汇总与呈现，Binance/CCXT/Paper worker 在进入服务循环前仍共用此 fail-closed 预检。

- 定向验证：runtime market-spec 预检 1/1；runtime fail-closed/规格单读者架构合同 17/17；qx-service 28/28；qx-service/qx-cli/qx-runtime 严格 Clippy 与格式检查通过。

### Paper/live 账户风险上下文归服务层（2026-10-21）

将 Paper/live 的账户余额准入、可用资金计算、现货库存冻结、衍生品持仓腿与在途 gross exposure 计算、以及 `RiskContext`/`OrderRiskPosition` 构造迁入 `qx-service::risk_host`。CLI 的 worker risk 模块现仅适配既有调用接口，Paper/ Binance/CCXT 共用服务层的风险判断和预留逻辑。

- 定向验证：Paper 保证金估值 7/7、Live 预留 9/9、qx-service 28/28；相关估值/准入/错误传播架构合同 21/21；qx-service/qx-cli/qx-runtime 全目标严格 Clippy 与格式检查通过。

### Live 在途订单规格选择归服务层（2026-10-21）

将 Live 余额快照后的在途订单冻结 market spec 选择迁入 `qx-service::risk_host`。服务层按账户、Venue、instrument 与启用 Execution worker 解析候选规格，拒绝冲突或缺失规格；CLI 仅把服务结果接入资金预留计算。

- 定向验证：Live 在途预留 9/9；Paper/live 准入与估值架构合同 11/11；qx-service/qx-cli/qx-runtime 全目标严格 Clippy 与格式检查通过。

### Paper 多市场账户规格与 FX 快照归服务层（2026-10-21）

将 Paper 账户的多市场冻结 market spec 汇总、持仓/在途订单标的解析、所需 FX 币种推导及直接/反向 Spot FX 一致性校验迁入 `qx-service::risk_host`。worker 规格身份匹配由服务层负责并调用统一 market-spec loader；CLI 保留风险上下文参数适配。

- 定向验证：Paper 保证金估值 7/7；估值与 Paper/live 准入架构合同 11/11；qx-service 28/28；qx-service/qx-cli/qx-runtime 全目标严格 Clippy 与格式检查通过。

### Worker market spec 加载归服务层（2026-10-21）

将 worker 冻结 market spec 的文件读取、相对路径解析与 qx-app 规格校验迁入 `qx-service::worker_market_spec`。风控与 runtime-check 继续走同一个加载器，保留错误诊断和 fail-closed 行为；CLI 只提供既有调用点的服务重导出。

- 定向验证：runtime market-spec 预检 1/1；规格单读者/runtime fail-closed 架构合同 17/17；qx-service 测试 28/28；服务、CLI、runtime 全目标严格 Clippy 与格式检查通过。

### Paper 在途资金预留归服务层（2026-10-21）

将 Paper 账户未终结订单的现货购买力、衍生品保证金及跨币种预留计算移入 `qx-service::risk_host`。CLI 继续负责从运行时配置、冻结规格、EventLog 和账簿组装快照输入；服务层单点执行金额计算与 fail-closed 校验，行为合同确认不存在旧实现复活。

- 定向验证：Paper 保证金估值 7/7、Paper 提交终态 5/5、Live 预留 9/9、qx-service 28/28；Paper/live 架构合同 2/2，相关 crate 严格 Clippy 与格式检查通过。

### 风控一致性合同切换到完整上下文（2026-10-21）

删除把 `RiskGate::check_with_spec` 历史缺口当成当前生产合同的 `known_gap_account_level_margin_is_invisible_to_legacy_gate` 用例。三入口一致性现在都通过完整 `OrderRiskContext` 比较；新增保证金不足的拒绝路径，确保 RiskEngine、RiskContext 门面与 RiskGate 对账户保证金给出一致决定。成熟度遗留项改为准确指向仍未关闭的 Live 跨币种抵押品与跨市场持仓估值。

- 定向验证：`risk_parity` 集成用例 5/5；架构 `--fast` 四片聚合覆盖 221 项且无重复；成熟度报告一致、YAML、Rust 格式和差异检查通过。

### A 股配置日历闭环到订单拒绝（2026-10-21）

扩展 A 股交易日历回归：日历明确排除的普通工作日不得提交买单，错误应说明当前 bar 不在可交易日或停牌；周末和午休时段也应判为非交易时间。此项验证配置日历拒绝语义，不代表仓库已有交易所官方完整日历数据，也不改变 A 股 profile 仍未测的状态。

- 定向验证：qx-xingban `calendar_json_controls_daily_and_intraday_trading_windows` 1/1；格式及差异检查通过。

### 启用 A 股规则时拒绝无界日历与全天时段（2026-10-21）

共享应用绑定器此前允许 `enabled=true` 的 A 股规则快照在交易日列表和 session 均为空时通过，底层会把任意日期和时刻视为可交易。现在回测必须提供有界 `trading_days`+`sessions`，或明确列出 `trading_timestamps`；缺输入在应用入口报错。同步说明部署配置要求，并以架构合同覆盖 fail-closed 绑定。

- 定向验证：绑定器缺日历/缺时段/有效配置用例 1/1；qx-cli A 股/回测合同 50/50；qx-app 全目标严格 Clippy、格式/差异检查及架构 `--fast` 221 项通过。

### 移除历史无效验收门禁并归拢账户准入服务边界（2026-10-21）

复核并清除仅依赖历史数量/规模快照的验收门禁：当前执行路径不再引用架构判据总量/标签快照、测试数量地板、全仓单文件行数棘轮、账簿/报告/API 读面固定行数上限、参数豁免总数上限或旧 `validate_core.py`；相应快照与预算文件不再作为仓库资产。架构检查保留模块归属、唯一实现、真实消费者、行为合同及活文档引用可达性，历史 changelog/归档不再被当作当前引用门禁。模式版本迁移图只覆盖需要读侧显式拒绝/迁移的契约，检查改为其 schema_id 必须属于模式登记册，不再错误要求覆盖所有 DTO schema。另将 Paper/live 共用的账户准入规则迁入 `qx-service::risk_host`。

- 验证：四分片架构门禁全绿并成功聚合；成熟度报告一致性、架构/生成器脚本语法、`git diff --check` 通过；NATS consumer 管道预算行为用例 1/1。活动 tools/maturity/CI/build 路径对退役门禁的检索无结果。

### API/Console/SDK 研究主链路复核与 Windows 隔离用例修复（2026-10-21）

Console 研究菜单现已接通 Bar 后台回测、多账户/多市场任务与参数实验；API 比较回测通过共享 Rust 应用用例稳定排名，并拒绝混合输入血缘。Python 与 Rust SDK 新增/覆盖多市场数据清单、订单簿队列配置、多腿自定义策略工厂和 RunHandle 消费链。将 qx-api RunRegistry/共享限流测试的 scratch 目录从 Windows 系统 Temp 移到仓库 `target/` 隔离目录，修复权限受限环境下的伪失败。

- 定向验证：API/Console/SDK 结构合同 105/105、API 路由合同 8/8；qx-api/qx-sdk 包测试、全目标严格 Clippy 和格式检查通过；Python SDK 分层 38/38、应用用例 37/37、Provider 9/9、新增 backtest DTO/验收用例 5/5。

### Live 下单拒绝过期交易所余额快照（2026-10-21）

Binance/CCXT Live 下单和价差恢复现在校验账户/Venue 最新余额快照时间；快照超过 60 秒或时间戳晚于准入时钟时 fail-closed。最终下单在账户锁内刷新 EventLog 后取当前时钟，锁等待与刷新耗时计入快照年龄。Paper 风控不依赖该交易所快照。

- 定向验证：Live 余额/在途预留 9/9、价差恢复 4/4、Live 风控架构专项 1/1、qx-cli 全目标严格 Clippy 与格式检查通过。恢复用例改用仓库隔离目录后在 Windows runner 通过。

### 快速回测拒绝显式非法的可选 market spec（2026-10-21）

快速回测 manifest 现在要求显式 schema v1 并拒绝未知字段。`market_spec` 字段缺省时仍允许使用运行时配置；若显式为空或类型错误，则在解析时失败，不再静默降级为未配置。

- 定向验证：qx-app 快速回测用例 3/3、CLI manifest/多 venue/A 股用例 5/5、全目标严格 Clippy、格式检查和架构专项 4/4 通过。重编译 CLI 后 A 股/币圈双目录确定性与 replay 验收通过，hash 分别为 `6be034a4cb0760fa` / `26fdd6b52d020700`；A 股报告产品用例 1/1 通过。

### Live 现货按报价币余额快照准入（2026-10-21）

Live 现货买单现在直接使用交易所余额快照中的报价币 `free - borrowed` 购买力，即使账簿报告币种不同也不做汇率换算；快照后同报价币现货在途订单按冻结规格预留，其它报价币现货订单使用独立现金桶，不互相占用。衍生品遇到异结算币在途订单仍 fail-closed，跨币种抵押品估值仍未开放。

- 定向验证：Live 余额/预留行为 8/8；`qx-cli` 全目标严格 Clippy、`live_balance_order_reservation_check()`、workspace 格式和成熟度报告一致性检查通过。

### 为 Rust SDK 注册表发布补齐内部 crate 版本约束（2026-10-21）

工作区内可发布 crate 的所有本地 path 依赖现在同时声明 `0.1.0` registry 版本要求；架构检查逐 crate 校验生产、构建和开发依赖。`qx-sdk` 与其应用层依赖 `qx-app` 增补 repository/homepage/documentation 元数据。使用临时本地 patch 映射解析未上架的工作区依赖后，Cargo 对 `qx-sdk` crate 包默认校验通过；真实注册表发布仍需先按依赖顺序发布内部 crate。

- 定向验证：`qx-sdk` 单测 13/13、独立消费者/集成用例 31/31；`cargo check -p qx-sdk`、严格 Clippy 和本地 patch 环境下 `cargo package -p qx-sdk` 默认校验通过；`registry_dependency_version_check()`、workspace 格式与目标差异检查通过。

### 移除构建脚本的历史步骤数量门槛（2026-10-21）

删除架构检查中固定要求构建脚本恰有 8 个编号步骤和 7 条 Cargo 命令的快照阈值。保留有效合同：Windows 与 Unix 脚本步骤名称/顺序一致、编号连续且分母自洽，Cargo 命令集合一致；解释器交接与门禁执行顺序继续检查。

- 定向验证：`build_script_parity_check()` 通过；`cargo fmt --all -- --check` 与差异空白检查通过。

### Live 同币种跨市场在途订单预留（2026-10-21）

Live 余额快照后的同账户、同 Venue 在途订单现在按其冻结 market spec 计算剩余名义额或初始保证金，因而支持同币种跨市场共享余额预留。市场规格缺失/冲突，或预留资金币种与余额资产不一致时仍 fail-closed；Live 跨币种抵押品与跨市场持仓统一估值仍未关闭。

- 定向验证：Live 在途预留行为 6/6；`cargo clippy --offline -j 1 -p qx-cli --all-targets -- -D warnings`、`cargo fmt --all -- --check`、`live_balance_order_reservation_check()` 与成熟度报告一致性检查通过。

# Changelog

### 0.2.1 多语言策略样例与安装包（2026-10-21）

- 新增 Python 动量、SMA 与均值回归策略实例，整理 Rust、Python、C++ 策略入口和跨语言回测说明。
- Rust workspace、Python wheel、C++ CMake SDK 与发布基线统一为 0.2.1。
- 发布流水线为 Linux、Windows、macOS 构建 CLI 二进制和 Python wheel，并构建 C++ SDK 包。

### 复核并固定交易所用户流空闲节奏（2026-10-21）

清除能力台账中已过期的“用户流空闲可能忙轮询”限制：CCXT 的秒回 idle 有至少 100ms 本地让步，Binance 用户流 idle 来自有界 WebSocket 读窗，分片与控制帧循环有明确上限。新增架构合同核对两种节奏来源及行为回归，并更正 CCXT 对本地节流的旧注释。

- 定向验证：CCXT 空闲节流用例、Binance WebSocket 超时用例和 worker 停机结构合同均通过；新增架构合同通过。前后端/SDK 五组架构检查通过，Python SDK 分层/DTO 用例 38/38；`qx-api` 86 个单测 + 36 个集成用例、`qx-sdk` 13 个单测 + 31 个独立消费者用例、C++ SDK 两个 smoke 均通过；`qx-core`/`qx-xingban` 回归与严格 Clippy 通过，覆盖事件账簿估值、A 股 T+1/库存、无前视和风险准入；A 股/币圈双目录回测验收与 ReplayVerifier 均通过，hash 分别为 `6be034a4cb0760fa` / `26fdd6b52d020700`。成熟度报告与格式检查通过。

### Paper 跨币种现货候选资金投影闭环（2026-10-21）

Paper 现货新单按报价币现金建立购买力，报价币可不同于账户账簿报告币种；同币种在途买单先按剩余名义额预留，再通过已配置的 Spot FX 汇率统一换算，缺失或冲突汇率时 fail-closed。Live 对未经统一估值的异币种现货单继续拒绝。新增 EUR 报价的候选单/在途单行为回归，并修正 Paper 账户准入架构检查中已迁移的锁调用与旧结算币购买力判据。

- 定向验证：跨币种 Paper 现货用例 1/1；`paper_account_admission_serialization_check()` 通过；成熟度报告生成一致、workspace 格式检查与目标差异检查通过。

### 持仓开仓杠杆进入账簿重放与 Paper 保证金估值（2026-10-21）

衍生成交的 `TradePosition` 账簿事实现持久化开仓杠杆，账簿重放会恢复按持仓数量加权的倒数杠杆比例；Paper 估值与 OrderBook 组合准入据此扣除标记价下的初始保证金。EventLog 摘要纳入新增杠杆事实，同时不改变缺少该字段的旧事件摘要。旧杠杆持仓在估值/准入时 fail-closed，避免用新订单杠杆伪造历史仓位保证金。

- 定向验证：`qx-core` 80 项单测、45 项集成通过（另有 1 项伸缩量具忽略）；qx-xingban 全 crate 146 项单测与 22 项集成通过；Paper 保证金 7/7、OrderBook 杠杆投影/旧仓拒绝 3/3、EventLog 后端切换 6/6、分段日志恢复 10/10、SQLite EventLog 9/9 通过。新增架构检查确保成交事实、EventLog 摘要、加仓比例归并、Paper 与 OrderBook 保证金投影和未知历史杠杆拒绝逻辑连通；格式及 qx-core/qx-risk/qx-runtime/qx-xingban/qx-cli 全目标严格 Clippy 通过。A 股/币圈确定性回测验收 hash 未变化，分别为 `6be034a4cb0760fa` / `26fdd6b52d020700`。
- 跨币种现货候选资金投影、Live 跨市场资金统一估值与完整发布矩阵仍未关闭。

### 清除过期的测试数量验收口径（2026-10-21）

修正架构门禁脚本概述中残留的“用例条数只增不减”说法。当前门禁只检查测试模块挂载、旧单文件不复活和共享夹具唯一性；测试数量通过实际测试目标执行验证，不设历史数量地板。历史变更记录中的旧计数仍作为历史记录保留。

### 移除历史诊断输出数量棘轮（2026-10-21）

删除 `process_diagnostics_check` 中固定未标记输出上限与 crate 名册一致性两项历史数量门槛。它们只约束旧快照数字，且成熟度记录与实际门槛已不一致；保留“引入统一日志框架时必须一次完成标签迁移”的架构合同。同步更新能力记录并重生成 `maturity/unverified.json`。

### Live 余额快照与在途订单预留（2026-10-21）

Live 下单入口现与 Paper 一样按账户锁串行化，并在锁内刷新 EventLog 后执行风控到 Venue 提交。风控按最近交易所余额快照的事件序号，预留快照后本地提交、尚未终结的同市场现货买单名义额或衍生品保证金；快照已覆盖的订单不会重复预留。余额快照后出现成交/现金流，或存在无法由当前 worker 认证的跨市场在途订单时，拒绝新单直到新快照抵达。

- 新增行为回归覆盖同市场预留、避免对旧订单双重预留、跨市场无法估值时 fail-closed；Live 提交链架构检查验证 Binance/CCXT 的调用根、账户锁、刷新、风控顺序。
- 定向验证：Live 行为 5/5；Paper 在途订单 1/1、Paper 提交终态 5/5；qx-risk/qx-cli 全目标 Clippy `-D warnings` 与格式检查通过。
- 未实现 Live 多市场账户资金统一估值、跨币种现货候选资金投影及持仓开仓杠杆持久化；这些作为发布前风控残余继续保留。

### Paper 账户购买力串行与现货预留修复（2026-10-21）

最终提交风控复核发现 Paper 同账户多个 worker 可能同时基于各自打开的旧 EventLog 快照判断购买力；现新增账户级文件锁，覆盖风控到撮合落账，并在锁内刷新 EventLog。现货买入购买力只按报价币现金计算，持有资产市值不再冒充可重复消费的现金；现货卖出由账户库存/在途卖量约束，不再要求报价币保证金。OMS 在途现货买单按剩余名义额预留，在途衍生品保证金继续按组合/FX 估值；跨币种现货候选单目前 fail-closed。

- 新增旧快照提交回归：两个 pipeline 都在第一笔成交前打开，第二次提交在锁内刷新后因可用资金不足拒绝。
- 定向验证：`qx-risk` 9/9，Paper 保证金 7/7，Paper 提交终态 5/5，策略订单身份 4/4；qx-risk/qx-cli 全目标严格 Clippy 与目标文件差异检查通过；A 股/现货回测准入 11/11、现货库存集成 2/2、三入口风控 parity 5/5。
- A 股和币圈示例回测验收均通过双次同目录字节一致、独立目录产物一致与 ReplayVerifier：A 股 `6be034a4cb0760fa`，币圈 `26fdd6b52d020700`。
- 新增 `paper_account_admission_serialization_check`，仅检查锁→EventLog 刷新→风控→撮合顺序与具名行为合同，不以检查数量作为质量门槛。
- 同轮架构检查复核发现 `risk_spec_fail_closed_check` 还钉着迁移前的 `check_with_spec` 调用。现改为验证 Bar、单市场和多市场 OrderBook 全部走 `check_with_context` 并进入统一 `RuleSet`，保留缺规格 fail-closed 与三入口 parity 合同；定向 6/6 通过。
- Live 已按最新交易所余额快照的 EventLog 序号预留其后的同市场未终结订单，并在 Binance/CCXT 提交中用账户锁串行刷新与风控；快照后成交/现金流、跨市场在途订单会 fail-closed，等待下一次余额同步。持仓开仓杠杆持久化、跨币种现货候选资金投影、Live 跨市场共享资金统一估值与最终三轮全链路验收仍未关闭。

### 订单簿衍生品组合保证金准入修复（2026-10-21）

移除按单笔初始保证金与账户标记权益对比的衍生品旁路，改由统一 `OrderRiskContext` 对已结算持仓、未终结在途订单和候选订单的投影初始保证金进行组合判定；现货现金/库存预留与既有撮合语义不变。当前持仓保证金仍按候选订单杠杆估算，Ledger 尚未保存持仓杠杆元数据，因此该口径不能替代交易所 free-margin 快照。

- 新增组合保证金单测和“双订单合计超限”订单簿集成回归，验证第二笔订单被规范风险门拒绝。
- 定向验证：两项新增回归通过，既有衍生品订单簿成交/账簿语义回归通过，`qx-xingban` 全目标 Clippy `-D warnings` 通过。
- 派发链抽查确认策略订单经 Binance、CCXT、Paper 最终 worker 风控；当时发现 Paper 可用资金未预留 OMS 在途订单，后续已由本文件顶部的账户串行与购买力修复收敛。Live 本地在途单与交易所余额同步仍待审计。
- 该轮不代表发布矩阵完成；全量架构门禁与工作区发布矩阵仍待开发收敛后执行。

### 能力可执行性深检：修一条真缺陷 + 落一份真执行的观测记录（2026-10-11 第 11 轮）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§2/§4.2/§9

这一轮不再看"声明"，而是**把命令敲下去**：逐条执行多语言 SDK 点名的核心能力，看哪些真跑得起来、
哪些跑不了、跑不了的卡在哪。执行过程中撞到一条真缺陷并修掉。

- **立案（真缺陷）**：`qx-cli paper-e2e` 在仓库根第二次跑就死——
  `Paper 主链路验收失败: 加载 Scheduler 状态失败: Io("missing field `input_refs`" at line 1 column 427")`。
  根因不是状态文件坏了，而是 **`JobSpec` 是持久化类型**（`JsonStateStore::save_scheduler_at`
  写进 `scheduler.json`、`load_scheduler_at` 下一次启动读回），而 `input_refs` / `output_refs` /
  `permission_scope` 三格被按**必填**加进来——于是任何在本构建之前写下的状态文件都读不回来。
  更刺眼的是这三格**零生产读者**（`JobSpec::validate` 一格都不看）：`必填` 这个选择唯一能产生的
  可观测效果就是让旧文件读不回来。**升级路径从来没有被跑过**——所有 `deploy/qianxing.scheduler.*.json`
  模板在加字段时同步改过，只有仓库里那份陈旧的 `data/qianxing-paper/scheduler.json` 会暴露它。
- **修法（具名缺省，而不是拒绝）**：按 `maturity/artifact_migration.yaml` 的口径——"显式迁移或
  具名拒绝"——这里走第三条同样成立的路径：**旧文件本来就合法**，那就不该拒绝。给缺键一个具名缺省：
  `Vec` 两格空表；`permission_scope` 用 `"legacy-undeclared"` 而**不是空串**（空串会把"写于本字段
  存在之前"与"显式声明了空权限"读成同一件事），与 V13 R9「金额字段缺键不许静默变 0」同一条纪律。
  `#[serde(default)]` 只改反序列化，**新写的状态文件逐字节不变**，已 bless 的 `config_fingerprint`
  与 RunManifest 哈希不会因为这次修复集体失真。
- **读失败改成具名拒绝**：`load_scheduler_state` 的 `map_err` 现在点名状态文件路径并给出唯一处置
  （"若该文件由更早的构建写出，按迁移口径删除后重建，不要就地改写"），不再只报一句 `Io(missing field ...)`。
  实测：把状态文件里 `timeout_seconds` 改成一个字符串，现在报的是
  `...（状态文件 data/qianxing-paper\scheduler.json；若该文件由更早的构建写出，按迁移口径删除后重建，不要就地改写）` 并以 2 退出。
- **三条行为用例**（`crates/qx-scheduler/tests/persisted_state_upgrade.rs`）：把三格从序列化结果里删掉
  再反序列化必须成功且落到具名缺省；删 `job_id` / `idempotency_key` / `timeout_seconds` 这些**真必填**格
  仍要拒绝（否则"容忍缺键"会退化成"什么都能读"）；`Scheduler::from_json` 读一份缺那三格的**整份状态**
  必须成功——最后这条最贴近事故现场。
- **新增 `persisted_state_upgrade_path_check` 三颗**：① 这型真是持久化的（写侧与读侧都在盘）；
  ② 每一格零读者声明格都容忍缺键（`#[serde(default...)]` 或 `Option<...>`）；③ 读失败是具名拒绝。
  **恒发三颗、不早退**。
- **新增 `tools/capability_execution_probe.py` + 仓库资产 `maturity/capability_execution.yaml`**
  （真执行，不是手写）：八条不需要凭据、不访问网络的路径被**真的跑过**并记下带数字的观测——
  单腿回测（`fills=1` + 稳定 `result_hash`）、多市场双腿（`groups=3` + turnover/fees）、
  订单簿深度撮合（L2 内核 + RunManifest）、**跨语言多策略**（Rust 撮合内核 + 两个 Python 策略
  worker 各跑各的，实测 `strategy-momentum` 与 `strategy-mean-reversion` 各 `fills=3`）、
  **多数据源批量对比**（`fast-backtest`：`jobs=2 completed=2`，BINANCE 与 OKX 两套数据/两个市场
  各跑一次并各落四件产物）、内置策略目录（17 条）、事件与控制面（`ecosystem`）、
  多策略运行拓扑（`strategies=2 workers=3`）。
  `delegated` 段指向 `maturity/paper_acceptance.yaml` 与 `maturity/backtest_acceptance.yaml`
  ——**同一件事不许有两份说法**。`blocked` 段如实登记两条**跑不了**的：多交易所实盘（`ccxt` 未装入
  `python/.venv`、无凭据、testnet 记录 `outcome=skipped`）与一等多账户（账户只在策略级建模，
  `RuntimeConfig` 没有 `accounts` 段）——每条都写明卡在哪条前置上。
- **新增 `capability_execution_check` 六颗**守这份记录：记录自述齐全；每条 `executed` 的**子命令词**
  真在生成器里（记录是跑出来的，不是手写的）；每条 `anchor` 在盘；判定落在闭集里、`blocked` 写明前置、
  `delegated` 指向在盘记录；每条 `executed` 都记了**带数字**的观测值（跑出 0 笔成交的"通过"是空跑）；
  能力名唯一且 evidence 在盘。**恒发六颗、不早退**。
- **⑤ 当场抓到我自己记录里的一处弱点**：`config validate` 只说"通过"、不带任何数，于是那条 `executed`
  的观测值是空的——判据立刻把它判红，探针改为直接数模板里真声明了几个策略/worker，观测值才变成
  `strategies=2 workers=3`。判据先咬了自己一口，这是它有用的证明。
- **同轮把三条行为用例从 `qx-scheduler/src/lib.rs` 搬到 `crates/qx-scheduler/tests/`**：搬进去会让
  `lib.rs` 从 1464 行涨到 1527 行，越过 `maturity/line_budgets.yaml` 的登记值——而那份预算**只降不升**。
  搬到集成测试目录后 `lib.rs` 回到 1464 行（= 预算），用例照跑。
- **读数**：门禁 **1080 条 / 地板 1020 / 0 FAIL**；`qx-scheduler` 21 条用例全绿；
  `persisted_state_upgrade_path_check` 变异 **3/3 KILL**（零附带、逐字节还原）。

### Python SDK 接口分层：把 §6.2 的八个子层从方案变成真包（2026-10-11 第 10 轮）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§6.2

方案 §6.2 明写「接口分层：`qianxing.core` 本地 Rust 用例；`qianxing.client` 远端 API；
`qianxing.data` provider/dataset；`qianxing.strategy` 策略 contract/worker；`qianxing.paper` 与
`qianxing.live` 明确依赖服务端执行；`qianxing.experiments` 参数对比；`qianxing.reports` 报告与
Evidence。顶层命名空间稳定导出常用接口，optional dependencies 不改变核心语义」。

- **立案（真实缺口）**：此前这一条**只写在方案里**。`qianxing` 是一个平铺的包
  （`app.py` / `cli.py` / `strategy.py` / `ashare.py` / `ccxt.py`），八个子层一个都不存在。
  集成方拿到的是 63 个名字的一整面墙——"我到底需不需要一台服务"这个问题在 import 那一行
  没有答案；而 `AppClient`（要服务）与 `run_backtest`（不要服务）就并排躺在同一个名单里。
- **新增七个子层包**（`qianxing.core` / `.client` / `.data` / `.paper` / `.live` /
  `.experiments` / `.reports`）；`qianxing.strategy` 本来就在盘（它是 `qianxing_strategy`
  这个独立发行包的视图），本轮把它并入同一套口径核。
- **每一层只回答一个问题**——"要不要一台服务"：`core` / `data` / `strategy` / `experiments` /
  `reports` 在进程内，`client` / `paper` / `live` 要一台 `qx-api`。分层不是装饰：
  `core` **刻意不导出** `AppClient` / `RemoteRunHandle` / `PaperClient` / `LiveClient`，
  `client` **刻意不导出**本地自由函数——想在进程内跑用例的人不该被要求先起一个服务。
- **新增 `python_sdk_layering_check` 十颗**：① 八个子层都有落点（包或单文件模块）且每层
  `__all__` 非空；② 每层导出的名字都在该层自己的 import 语句里有来源；③ 每层导出的名字都
  取自顶层稳定名单（或在按层点名的子命名空间名册里）；④ 名册不烂——名册里的名字真在那一层里；
  ⑤ 顶层 `qianxing.__all__` 与 `qianxing/app.py` 的公开类/函数集合**双向相等**；
  ⑥ `core` 里没有远端面、`client` 里没有本地自由函数且摆出 `AppClient`/`RemoteRunHandle`；
  ⑦ `paper` 只摆 `PaperClient`、`live` 只摆 `LiveClient`，两者都摆共用的 `TradeWorkflowClient`；
  ⑧ 核心层的 import 语句不点名可选适配器；⑨ 分层用例在盘；⑩ 六条具名证据一条不少。
  这一族**恒发十条、不早退**。
- **⑤ 是这一族里最硬的一颗**：顶层稳定名单与 `app.py` 的公开面**双向**相等（实测 63 == 63）——
  多导出 = 顶层比实现宽（承诺了不存在的东西），少导出 = 实现里有公开面没人认领（孤儿）。
  两个方向都是本仓反复栽过的那两类。
- **⑧ 只读 import 语句、不读散文**：`qianxing/data/__init__.py` 的文档里点名
  "适配器在 `qianxing.ccxt`" 是**说明**，不是依赖。按散文判会把说明读成依赖。
- **缺口如实登记**：§6.2 还有三件没落——同步 API 与 `asyncio` API 共享同一 contract、
  `RunHandle.events`、CPython 3.10+ × OS/arch 支持矩阵与不带 Rust 工具链的干净环境安装冒烟。
  它们写进 `maturity/capability-matrix.yaml` 的 `python_sdk` 行 limitations，不假装已经有了。
  其中 `RunHandle.events` 与 Rust 门面那一格同源：`qx-app` 没有事件日志，远程事件游标住在
  `qx-api` 的 HTTP 面——门面不凭空长出一个没有生产端的事件流。
- **同轮自己踩到一次**：② 的第一版解析器只按行切 import，括号式跨行 import
  （`from qianxing.app import (\n … \n)`）只切到第一行的 `(`，于是把 `core` / `experiments` /
  `reports` 三层的整份 `__all__` 都误报成"没有来源"（44 条假红）。改成**括号式整块先取走**再按
  逗号切，两侧都对上了。
- **门禁 1061 → 1071**（`GATE_CHECK_FLOOR` 1001 → 1011），`--snapshot` 已刷新；
  **10 枪变异全部 KILL、零附带红**（`.task-tmp/mutate_py_layering.py`，逐枪按文件换行符拼锚点、
  逐字节还原）。其中 ③④ 两枪改的是门禁自己的名册常量（名册只有门禁读得到），
  ⑨ 一枪必然连带 ⑩（文件不在盘时那六条确实也不在），已注明是同一次变异的两条真话。
  另新增 `python/tests/test_sdk_layering.py` 十一条行为用例（含一条**子进程**用例：
  核心层 import 不得顺带拉起 `ccxt` / `qianxing_ccxt` / `qianxing_ashare`）。

### 第三方 Rust 门面 `qx-sdk`：把 §6.1 的「稳定边界」从零建出来（2026-10-11 第 9 轮）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§6.1 / §7 阶段 0 / §7 阶段 4

方案 §6.1 要的是一个**给仓库外调用方**的稳定边界：`Client/Engine`、capabilities、spec 构造校验、
sync/async run、`RunHandle`、events/cursors、cancel/wait/result、report/verify、Provider/Venue 扩展端口、
统一错误，外加 semver、迁移指南、examples 与独立消费者构建。此前这一格**完全不存在**——
`maturity/capability-matrix.yaml` 的 `rust_sdk` 行自己写着「当前交付是 qx-cli 运行程序」。

- **立案（真实缺口）**：`qx-app` 是按用例组织、为 CLI / HTTP / Python 三个**仓内**门面服务的应用层，
  不是给仓库外调用方用的边界。集成方只能自己挑 `qx-app` 加若干领域 crate 拼，拼错了没人拦；
  §6.1 要的 semver 承诺、迁移指南、examples、独立消费者构建一样都没有。
- **新增 `crates/qx-sdk`**（`Cargo.toml` + `src/lib.rs` + `src/client.rs` + `tests/independent_consumer.rs`
  + `examples/quickstart.rs`），并登记进 workspace members。门面**只**依赖 `qx-app` 加 §6.1 点名的
  两个端口 crate（`qx-provider` / `qx-execution`），外加 `qx-strategy`（`BuiltinStrategyKind` 是
  `BuiltinStrategySpec::new` 的**参数类型**，而 `qx-app` 没有重导出它——这不是"拼接领域 crate"，
  是补全 `qx-app` 自己的输入类型；`qx_sdk_facade_check` ③ 把这个集合钉成**恰好**相等）。
- **门面里没有任何业务逻辑**：每个 `pub fn` 的函数体不超过 4 行（非空非注释行），
  只做重导出与转发；用例逻辑全部仍住在 `qx-app`。第 ② 颗判据守着这条。
- **新增 `docs/qx-sdk-migration.md`**：从"直接用 `qx-app`"迁到门面的迁移指南，含 §6.1 逐项覆盖表
  与一节**已知缺口**（events/cursors、发布验收、能力预检、Live）。
- **新增 `qx_sdk_facade_check` 十颗**：① 门面 crate 在盘、进 workspace members、包名 `qx-sdk`、
  版本取自 workspace；② 门面里没有业务逻辑（函数体 ≤ 4 行）；③ 内部依赖**恰好**是那四个，
  且 `qx-app` 的被依赖集合点名了它（与 `LAYER_SOLE_DEPENDENTS` 同一把尺）；④ §6.1 每项能力
  要么有名册实物、要么在缺口表里写明理由；⑤ 名册里的**门面符号**真出现在门面源码里；
  ⑥ 名册里的**源符号**真在源 crate 的公开面里（凭空写一个名字 = 断链）；⑦ §6.1「async run」
  在 `qx-app` 里有六条 `*Spec::start` 实物；⑧ 独立消费者在盘、只说 `qx_sdk::` 一种前缀；
  ⑨ examples 与迁移指南在盘且都被门面 crate 文档点名；⑩ 每条能力缺口都在能力矩阵的
  `rust_sdk` 行如实登记。这一族**恒发十条、不早退**。
- **⑤ 与 ⑥ 两个方向刻意都核**：只做 ⑤ 会放过「文档说有、代码没有」，只做 ⑥ 会放过
  「凭空声明一个源符号」。⑥ 的源 crate 公开面用**构造性取法**（`pub use` 按 `{...}` 展开 +
  `pub fn/const/struct/enum/trait/type/static` 声明名，先剥 `#[cfg(test)]` 与注释）。
- **缺口如实登记，不造一个假的**：§6.1 点名的 events/cursors 在 `qx-app` 里**没有**对应物
  （远程 Run 的事件游标住在 `qx-api` 的 HTTP 面）。门面**不**凭空长出一个没有生产端的事件流——
  那正是本仓反复栽过的"声明替不存在的代码作保"；改成空名册 + 缺口表，并由 ⑩ 要求同一个令牌
  出现在能力矩阵的 `rust_sdk` limitations 里（两张登记面不许各说各话）。
- **同轮顺手补一条真缺陷**：`qx-app` 的 `DEFAULT_WAIT_TIMEOUT`（`wait()` 的有限缺省，方案 §9）
  是私有 `mod run_handle` 里的 `pub const`、**从未重导出**，所以仓库外根本够不着它。
  加进 `pub use run_handle::{...}` 之后门面才能把它摆到公开面上——`qx-app/src/lib.rs` 一行改动，
  但那是「门面暴露了 qx-app 自己的洞」而不是门面自己造的东西。
- **doctest 在沙箱起不来**（`rustdoc --test` → `os error 231 ERROR_PIPE_BUSY`，且全仓此前
  没有任何一份可运行的文档示例）。示例因此放进 `examples/quickstart.rs`——`cargo test --workspace`
  会**编译**它，证据强度比 doctest 高而不是低；`client.rs` 里那个块标成 ```text``` 并写明原因。
- **门禁 1051 → 1061**（`GATE_CHECK_FLOOR` 991 → 1001），`--snapshot` 已刷新；
  **10 枪变异全部 KILL、零附带红**（`.task-tmp/mutate_sdk_facade.py`，逐枪按文件换行符拼锚点、
  逐字节还原）。其中 ④⑤⑥ 三枪刻意改的是 `tools/check_architecture.py` **自己的常量**——
  这三颗完全由那份常量算出，改数据文件够不着它们；三枪都逐字节还原。

### 主链路调用图 + 孤儿/重复逻辑清单：把 §5.1/§5.2/§5.4 的三段箭头钉在真实符号上（2026-10-11 第 8 轮）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§5.1 / §5.2 / §5.4 / §7 阶段 0 / §13.2

方案 §7 阶段 0 的交付清单里，「三条主链路调用图」与「能力矩阵」并列；§13.2 又明写「完成阶段 0，
并将能力矩阵和**接口调用图**纳入仓库版本控制」。能力矩阵在第 7 轮落地，本轮补调用图这一半。

- **立案（真实缺口）**：方案 §5.1 / §5.2 / §5.4 用箭头写了三条主链（数据→研究/回测、
  Paper/Live 订单闭环、多语言策略），但那三段文字**没有任何东西把每一段钉在真实符号上**。
  符号改名、文件搬家、用例删掉，三段箭头照旧读起来很完整——「文档替不存在的代码作保」
  正是本仓反复栽过的那一类。
- **新增 `maturity/chain-map.yaml`**：三条链 × 19 个 step，每步点名两件事——`anchor`
  = `<文件> <符号>`（符号必须真在该文件里声明：Rust 认 fn/const/struct/enum/static/type/trait，
  `.py` 认 class/def）与 `case` = `<文件> <用例>`（用例必须真在该文件里，**读原文**）；
  每条链另有 `entry`/`terminal`/`evidence`/`note`。`orphans` 段收**已确认**的孤儿。
- **新增 `chain_map_check` 十一颗**：① 调用图在盘、自述 kind 与 schema_version 正确、三段非空；
  ② `chains` 名册 == 方案点名的三条主链；③ 每条 step 的 `chain` 都是声明过的链；
  ④ 每条链内 step 的 `order` 恰好是 1..N；⑤ anchor 文件在盘且符号真在其中声明；
  ⑥ `case` 用例真在那个文件里；⑦ 每条链的 `entry`/`terminal` 都是链内真实存在的 step 名；
  ⑧ 每条链的 `evidence` 逐条在盘；⑨ **声明不是装饰**——anchor 符号必须在**它自己文件之外**
  的生产文本里出现过；⑩ 每条 orphan 的 anchor 能解析、`kind` 与 `disposition` 落在闭集里；
  ⑪ **孤儿记录必须是真的**——`no_production_consumer` 在外面真找不到读者，且没有链上 step
  被同时记成孤儿。这一族**恒发十一条、不早退**。
- **⑨ 与 ⑪ 两个方向刻意相反**：只做 ⑨ 会放过「把不存在的调用画进图里」，只做 ⑪ 会放过
  「把活着的东西记成孤儿」。两向都核才闭合。
- **本轮 ⑨ 当场抓到一条真缺陷**：`data_fetch` 原先锚的 `InMemoryProvider` 在它自己文件之外
  **零读者**——`InMemoryProvider::new` 的两个调用点都在 `crates/qx-provider/src/lib.rs` 的
  `#[cfg(test)]` 里。它之所以一直没被零读者判据抓出来，是因为裸词计数把
  `pub struct InMemoryProvider` 与 `impl … for InMemoryProvider` 这两行**声明本身**当成了读者。
  改锚到真有下游的 `JsonBarFrameProvider`（消费方在 `qx-cli` 与 `qx-data::ingestion`），
  并把 `InMemoryProvider` 如实登记进 `orphans` 段（`kind: no_production_consumer` /
  `disposition: retained_public_api`）。同轮 `python_worker` 一步从只在自身文件内被调的
  `serve_framed` 改锚到 `python/qianxing_bridge/strategy.py StrategyInput`。
- **同轮自作自受一次**：第一版把「`段:` 下若干 `- name: X` 记录」的解析在 `_matrix_sections`
  与 `_chain_sections` 里各写了一份，`merge_duplicate_block_check` 当场打出 9 行逐字重复窗口
  （`17737==18069`）。收成**单一份** `_parse_named_records(path, wanted)`，两张登记面共用。
- **门禁 1040 → 1051**（`GATE_CHECK_FLOOR` 980 → 991），`--snapshot` 已刷新；
  **11 枪变异全部 KILL**（`.task-tmp/mutate_chain_map.py`，逐枪按文件换行符拼锚点、逐字节还原）。
  其中 ② `chain-renamed` 一枪刻意记下 ②③⑦ 三条同红——只改一行 `name:` 时它们是同一次变异的
  三条真话，不是噪声。

### 能力矩阵：方案 §2 的基线复核产物落地，五态缺口替代布尔位（2026-10-11 第 7 轮）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§2 / §4.2 / §13.2

方案 §2 明写「基线复核产物必须是 `capability-matrix.yaml`、语言/API 契约映射表、依赖图和问题清单。
每个缺口标记为 `implemented-and-verified`、`implemented-unverified`、`partial`、`unsupported` 或
`blocked`，**不得用布尔值 `supported=true` 掩盖限制**」；§13.2 又要求「完成阶段 0，并将能力矩阵和
接口调用图纳入仓库版本控制」。语言/API 契约映射表在第 5 轮已落地，本轮补上缺的那一半。

- **立案（真实缺口）**：`maturity/capabilities.yaml` 是**另一条轴**——它记的是内部子系统的四档
  布尔阶梯（implementation / code_tested / sandbox_tested / production_approved），回答"这套代码本身
  走到哪一档了"。方案 §2 要的是那张基线表里每个领域/Provider/Venue 的**缺口状态**。此前：
  - 方案 §2 的八个领域（Rust 内核 / Python / C++ / Rust SDK / 多数据源与交易所 / 多账户多策略 /
    Web-API / 发布）**没有一个被逐条评过档**；
  - 三个真实存在的 `impl Venue for`（`PaperVenue` / `BinanceSpotVenue` / `CcxtProcessVenue`）
    没有任何地方记着它们的限制落在哪一格；
  - 而布尔位恰恰是最容易把「未验证」读成「已支持」的写法——`sandbox: true` 分不出
    "沙盒验过""没验过""只验了一半"。
- **新增 `maturity/capability-matrix.yaml`**：
  - `levels` = 五态闭集（顺序即从强到弱）；`capability_fields` = 方案 §4.2 的十四格 venue 字段名册
    （market/history/realtime/frequency/depth/order_type/account_model/fee/cancel/query/idempotency/
    rate_limit/sandbox/precision）；`provider_fields` = 数据面六格（订单类字段对 Provider 不成立，
    硬套会逼出一堆无意义的档位）。
  - `domains`：方案 §2 那张表的八个领域，每条带 `level` + `limitations` + `evidence`（真实在盘路径）
    + `next_gate`。
  - `venues`：三个真实 venue 逐格评档，`impl` 指向含该 `impl Venue for` 的文件，`kind` 区分
    `internal_simulation` 与 `external_venue`。PaperVenue 的 realtime/depth/rate_limit/sandbox 四格
    老实写 `unsupported`（它没有实时行情源、没有订单簿深度、没有外部限流）；两个外部 venue 全部
    `partial` / `implemented-unverified`，`sandbox` 一律 `unsupported`——因为 testnet 记录
    `outcome` 是 `skipped`（缺凭据），没有一格够格进 `verified`。
  - `providers`：三个真实 `impl DataProvider for`（InMemoryProvider / JsonBarFrameProvider /
    DemoProvider），`DemoProvider` 如实标成"生态冒烟用的演示 Provider，不是生产数据源"。
- **新判据族 `capability_matrix_check`（十二颗）**：① 矩阵在盘、kind 与 schema_version 正确、三段非空；
  ② `levels` 恰是五态且顺序一致；③ 全表没有把能力写成布尔值的地方（**判据自带反例自证**：喂它
  `supported: true` 必须报，喂它 `market: partial` 必须不报——一条从不发声的空判据靠不住）；
  ④ `domains` 名册 == 方案 §2 的八个领域；⑤ 每个领域档位落在闭集里；⑥ 每个领域都写了至少一条限制
  （「不得掩盖限制」的可核验形态）；⑦ 每个领域证据里至少一条仓库路径且每条都在盘；⑧ 每个领域都写了
  下一个闸门；⑨ **venue 名册 == 生产代码里真实的 `impl Venue for X`**，且 `impl` 就是含该实现的文件
  （多一个 = 凭空声明一个不存在的 venue；少一个 = 有 venue 活着却没人给它评档）；⑩ 每个 venue 逐格
  给齐十四格且档位合法、`verified_version` 非空；⑪ provider 名册 == 真实的 `impl DataProvider for X`
  且逐格给齐数据面六格；⑫ 外部 venue 声称 `sandbox`/`order_type` 已验时必须有 outcome=pass 的
  testnet 记录（**与 `capabilities_check` 同一把尺：没有证据不得翻真**）。这一族**恒发十二条、不早退**。
- **历史记录（当时）**：门禁 1028 → 1040 条、地板 968 → 980；当时曾使用 `maturity/gate_snapshot.json`。该数量快照门禁已删除，这些读数不再代表当前验收口径。
- **变异取证 13 发全 KILL**（`.task-tmp/mutate_capability_matrix.py`，按字节还原，每发都断言
  锚点唯一）。覆盖十二颗判据里的每一颗：整份登记面消失 / 五态重排 / 能力写成 `supported: true` /
  领域改名 / 档位写成闭集外的词 / 限制清空 / 证据指向不存在的文件 / `next_gate` 清空 /
  venue 名册凭空多一个 / venue 的 `impl` 指错文件 / venue 少给一格 / provider 档位非法 /
  外部 venue 无凭据却把 `sandbox` 翻成已验。
  - **首轮 `matrix-file-missing` 是 MISS，抓到的是我自己期望集写漏了**：登记面整份消失时，
    `levels` 名册也读成空表，② 跟着红。那条红是同一次变异的**真话**（不是假阳），已列进期望集，
    复跑 6/6 KILL。与第 5 轮 `contract-file-missing` 连带打红快照那颗同型——**「别的判据跟着红」
    要逐条判它是真话还是噪声，不能一律当噪声抹掉。**

### 等待默认有限：两条腿的 `wait()` 不再一个无界一个 30 s（2026-10-11 第 6 轮）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§9

方案 §9 的原话是「`wait()` 默认有限时；无限等待仅由调用方显式请求并可取消」。这条纪律此前一条判据
都没有，而同一个 SDK 的两条腿对「等多久」给了两个相反的答案。

- **立案（真实缺陷，不是理论风险）**：
  - Python 原生句柄 `RunHandle.wait(timeout_ms=None)` 的缺省是 `None`，一路透到 `qx-python` 的
    `loop { … }` 上就是**一直等**——调用方一个参数都不写就拿到了无限等待，而远端句柄
    `RemoteRunHandle.wait(timeout=30.0)` 的缺省是 30 s。
  - Rust 核心 `qx_app::RunHandle::wait()` **连超时参数都没有**，直接 `receiver.recv()` 堵到底。
    调用方不仅默认拿到无限等待，而且**拿不到「最多等多久」这个选项**。
- **修复**：
  - `crates/qx-app/src/run_handle.rs`：新增 `pub const DEFAULT_WAIT_TIMEOUT: Duration = 30 s`
    （与远端句柄同值），`wait()` 改走新的私有 `wait_within(timeout)`；有界等待用 `recv_timeout`，
    并且**超时单独一支且不置 FAILED**——到点没结果是「还没好」，不是「坏了」；把超时并进断连那一支
    会让一次正常的到点返回把还在跑的运行记成失败。想真正无界等待的调用方自己写 `status()` 循环
    （缺省不是无界，不等于不可取消：`cancel()` 仍然收得回来）。
  - `python/qianxing/app.py`：新增 `DEFAULT_RUN_WAIT_MS = 30_000`，`RunHandle.wait` 的缺省从
    `None` 改成它；`None` 保留为**显式**的无限等待。
- **新判据族 `bounded_wait_default_check`（五颗）**：① 两条腿的有限缺省常量都在盘且是有限正值，
  且 Rust 那个不超过 60 s（「有限」不能靠把常量写成十年）；② Python 两条腿的 `wait` 缺省都不是
  `None`；③ Rust `wait()` 走有限缺省常量而不是裸 `receiver.recv()`；④ 有界等待用 `recv_timeout`
  且超时单独一支不置 FAILED；⑤ 有界等待有行为用例在盘。
- **门禁 1023 → 1028 条，地板 963 → 968**；`--snapshot` 已刷新。
- **变异取证 5 发全 KILL**（`.task-tmp/mutate_bounded_wait.py`，按字节还原）。**这一轮变异抓出了
  我自己写的一条假判据**：② 第一版比的是 `matched.group(1)`（参数名）而不是 `group(2)`（缺省值），
  于是把缺省改回 `None` 时它照印全绿——`python-default-back-to-none` 那一发当场打出
  `rc=0 caught=0/1`。修掉之后五发全杀。另一发 `rust-wait-bypasses-constant` 除目标红之外还
  连带打红「零读者」那颗：绕过常量会让它没人取用——那条红是同一次变异的真话，已列进期望集。

### 语言/API 契约映射表：11 个用例在五种语言面里逐一对应（2026-10-11 第 5 轮）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§2 / §8

方案 §2 把「语言/API 契约映射表」列为基线复核产物之一，§8 又要求「跨语言同一 Run」。本轮把这张表
做出来并给它装上牙齿——它此前**根本不存在**，而缺的正是最容易被读成"已经齐了"的那一格。

- **立案（真实缺口，不是理论风险）**：`maturity/app_use_cases.yaml` 已经把 11 个用例与 Rust 的
  `cases/` 目录、`pub use`、`cases/mod.rs` 的九项文档表三处对上了，但**一个字都没提另外三种语言面**。
  也就是说：新增（或删掉）一个用例时，Rust 侧三处齐全、门禁全绿，而 Python 进程内扩展、Python 远端
  HTTP 门面、C++ 远端 HTTP 门面可以静默地少一个入口或多一个孤儿入口——集成方要到运行时才知道
  「这个方法不存在」。这与上一轮立案的「声明表不是装饰」是同一族：**登记面只覆盖了一条腿**。
- **新登记面 `maturity/language_api_contract.yaml`**：11 行，每行五格——`rust` / `python_native` /
  `python_remote` / `cpp_remote` / `http`。两个显式名册回答"这些公开方法为什么不是用例入口"：
  `python_native_helpers: [doctor]`（安装自检）与 `cpp_lifecycle_methods: [run_status, cancel_run]`
  （run 生命周期的读/写面）。写成名册而不是「排除 `run_` 开头的」这类规则，是因为规则会把将来新加的
  同形状方法一起放行，名册则要求有人为每一条回答一次。
- **新判据族 `language_api_contract_check`（九颗）**：① 映射表在盘、自述 kind 正确、每行五格齐且用例名
  不重复；② 映射表用例集合 == `app_use_cases.yaml` 的用例集合（两张登记面不得各说各话）；③ 每格点名的
  Rust 入口都是 `cases/<use_case>.rs` 里的 `pub fn`；④ 每格点名的 Python 入口在盘（`python_native` 看
  模块级 `def`，`python_remote` 看 `AppClient` / `TradeWorkflowClient` 的公开方法）；⑤ 每格点名的 C++
  入口都是 `AppClient` 的公开方法；⑥ 每格点名的 `http` 路由都在两张客户端路由声明面里（用例 → 路由 →
  声明面，三段接得上）；⑦⑧⑨ **反向普查**三条腿——客户端公开面里没有未被任何用例认领的入口。反向那三颗
  是本族最贵的一格：正向只核「声明的东西在盘」，一个多出来的 `AppResponse frobnicate(...)` 照样绿。
  这一族**恒发九条、不早退**（缺文件时后八条按空文本判，自然落空）——早退会让一次缺文件把总条数打下来，
  快照漂移判据跟着红，第二条红盖住第一条。
- **门禁 1014 → 1023 条，地板 954 → 963**；`--snapshot` 已刷新。
- **变异取证 5 发全 KILL**（`.task-tmp/mutate_language_contract.py`，全部按字节还原）：
  `contract-row-removed` 4 红（用例集合不齐 + 三条腿各自出现未认领入口）/ `cpp-extra-method` 1 红
  （只有反向普查那一颗）/ `http-route-wrong` 1 红（路由对不上声明面）/ `python-remote-bogus-entry` 1 红
  （点名的入口不在盘）/ `contract-file-missing` 6 红（含 `labels_sha256` 那颗——去掉一份 `maturity/*.yaml`
  会改登记面份数，而份数写在另一颗判据的 label 里，那条红是同一次变异的真话）。

### 远端客户端「声明表不是装饰」+ C++ 最小消费者工程补牙齿（2026-10-11 第 4 轮）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§6.3 / §11

上一轮把 C++/Python 两条客户端腿的路由**声明面**接到 qx-api 分派表上（`client_route_wiring_check`
七颗）。本轮接着问那七颗没问的问题：**声明表里的东西是不是真的在驱动请求**。

- **立案（真实缺陷，不是理论风险）**：`cpp/include/qianxing_app.hpp` 里 `routes::kRunStatusTemplate`
  （`/app/runs/{run_id}`）与 `routes::kRunCancelTemplate`（`/app/runs/{run_id}/cancel`）被列进了
  `kDeclaredRoutes`、也被门禁逐条核对过，但**没有任何代码用它们**：真路径由另一对
  `kRunsPrefix`（`/app/runs/`）与 `kRunCancelSuffix`（`/cancel`）拼出来。于是门禁比的是模板（对的）、
  请求发的是前缀——把 `kRunsPrefix` 改成 `/app/run/`，门禁照印全绿，`run_status` / `cancel_run`
  却会 404。「声明表不是装饰」这句承诺恰好在两个 run 生命周期入口上落空。
- **修复**：新增 `routes::fill_run_id(template, run_id)`，`run_path` / `run_cancel_path` 改成消费
  **声明的模板**（Python 侧本来就是 `.format(run_id=...)`，两条腿从此同一构造方式），删掉那对
  前缀/后缀常量。`cl -std:c++17 -W4` 编译零告警，`app_client_smoke` 运行退出码 0——它断言的是
  具体路径 `/app/runs/run-1` 与 `/app/runs/run-1/cancel`，所以这条修复有端到端证据而不是只靠读代码。
- **`client_route_wiring_check` 七颗 -> 九颗**：
  - ⑧ 声明面里每条路由都被客户端代码**真的取用**——C++ 的 `kX` 名字必须出现在 `kDeclaredRoutes`
    数组与它自己的 `inline constexpr` 定义**之外**，Python 的键必须被 `APP_ROUTE_TEMPLATES["key"]`
    字面量取用。匹配前先剥注释，否则「退回旧写法 + 注释里留一句锚点」照样绿。
  - ⑨ 反向普查：客户端源码里不得出现声明面**之外**的 `/app/...` 路径字面量。刻意只做「多出」这一向——
    声明面的字面量按构造就在源码里，「声明的字面量找不到」那一向永远不可能为真，写上去只是凑一个
    不可达分支（本仓口径：判据的每一向都要能真的红）。
- **Python 侧同族修复**：`TradeWorkflowClient.submit` 原来用
  `APP_ROUTE_TEMPLATES[f"{self.workflow}_commands"]` 动态拼键——「工作流名」与「路由键名」被一条
  没写在任何地方的命名约定绑在一起，两个键在源码里没有字面量读者（门禁核不出它们到底有没有被用上），
  键改名只会拿到运行时 `KeyError`。改成两个字面量分支。
- **新增 `cpp_consumer_example_check`（三颗）**：`cpp/tests/consumer/` 是方案 §6.3 点名的**下游独立工程**
  （`find_package(QianxingStrategySDK)` 消费 `cmake --install` 出来的导出 target），此前全仓
  `grep tests/consumer tools/check_architecture.py` **零命中**——工程被删、CMake 退化成相对路径 include、
  示例里另抄一份路径，门禁一条都不红。三颗分别钉：① 工程两件在盘；② CMakeLists 真走
  `find_package` + `add_executable` + `target_link_libraries`（剥 `#` 注释后判；相对 include 蹭进仓库
  测不出「workspace 外消费」这条退出门）；③ 示例源码取用 `qianxing::routes::k*` 且不另抄 `/app/...` 字面量。
- **同轮修掉示例里的坏示范**：`cpp/tests/consumer/main.cpp` 硬写 `"/app/compare-runs"`，改成
  `qianxing::routes::kCompareRuns`。示例是使用者照抄的模板，硬写一份等于把「第二份声明面」教给下游，
  而 header 里那份常量就摆在那儿。
- **这一族恒发三条**：`cpp_consumer_example_check` 刻意**不早退**——第一版在「文件缺失」时 `return`，
  变异实测发现「把 `main.cpp` 挪走」会同时把总条数从 1014 打到 1012，快照漂移判据跟着红，
  红出来的第二条会盖住真正的那条。缺文件时后两条按空文本判，自然落空，条数不随盘上状态变。
- **变异取证（七枪全杀，还原后按字节断言与 pristine 一致）**：C++ 声明面摘掉一条的消费（`run_path`
  不再用模板）/ 只在注释里留锚点 / Python 键不再被取用 / 方法体另抄一份 `/app/backtst` /
  消费者 `main.cpp` 挪走 / CMakeLists 摘掉 `find_package`（真摘 + 只注释掉各一枪）/ 示例改回硬写字面量。
  脚本：`.task-tmp/mutate_client_declaration.py`、`.task-tmp/mutate_consumer_example.py`。
- **端到端验证**：`cl -std:c++17 -W4` 编译 C++ 冒烟零告警、运行退出码 0；`cmake --install` 出包后
  下游消费者 `find_package` 配置 → 编译 → 运行退出码 0；Python 10 份测试全过；
  `cargo test --workspace --offline` **1340 passed / 0 failed**（`os error 231` 噪声 0 条）。
- `GATE_CHECK_FLOOR` 949 -> 951 -> 954；门禁实测 **1009 -> 1011 -> 1014**。

### 多语言 SDK 契约映射（2026-10-11）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§2「语言/API 契约映射表」

本轮是「前后端贯通」审计的第一条：把远端客户端的**路由接线**从「各自写各的路径字面量」收成
「单一声明面 + 门禁逐条对账」。

- **立案**：`web_console_check` 只核控制台（`web/console/app.js`）点名的路径都在 qx-api 路由表里。
  远端客户端的另外两条腿——C++ 头文件 `cpp/include/qianxing_app.hpp` 与 Python
  `python/qianxing/app.py`——**此前没有任何判据核过**：客户端把 `/app/backtst` 拼错、或后端把某个
  路由改名，两边都不红，集成方要到运行时才拿到 404。这正是方案 §11 点名的「多语言契约漂移」风险，
  而门禁里没有一颗牙齿。
- **两条腿各自收成单一声明面**：C++ 新增 `namespace routes`（20 个 `inline constexpr std::string_view`
  + `kDeclaredRoutes` 数组），Python 新增 `APP_ROUTE_TEMPLATES` 字典；方法体不再各抄一份字面量，
  一律走声明面（`run_path`/`run_cancel_path` 拼接 `{run_id}` 模板）。
- **新增门禁 `client_route_wiring_check`（七颗）**：① 两份声明面在盘；② 两份声明面非空；③ 客户端声称的
  每个路由都能在 qx-api 分派表里找到（正向断链）；④ C++ 与 Python 两条腿逐条相等（跨语言契约漂移）；
  ⑤ qx-api 的每个 `/app/*` 入口都被客户端声明（反向：没有客户端能调的端点就是孤儿）；⑥ 每个路由字面量
  在客户端源码里只有一处定义（声明表不是装饰）；⑦ 每个路由都出现在对应语言的冒烟用例里（声明了却
  没人跑过的路由是孤儿）。`GATE_CHECK_FLOOR` 936 → 943，门禁实测 **996 → 1003**。
- **同轮修掉一处真实覆盖缺口**：`cpp/tests/app_client_smoke.cpp` 此前只跑 16/20 条路由，
  `validate_dataset` / `run_backtest` / `verify_run` / `compare_runs` 四个方法从没被运行过——补齐四条断言。
- **变异取证（四枪全杀，还原后按字节断言与 pristine 一致）**：Python 路由表拼错路径 / C++ 声明表摘掉一条 /
  方法体另抄一份字面量 / C++ 冒烟用例删掉一条路由断言 —— 各只红对应那一颗。
- **两条口径的落地校验**：C++ 头文件在 MSVC 默认代码页（936）下必须 **ASCII-only**（新增 CJK 注释会让
  MSVC 把多字节序列读成标识符、整份头文件编译失败），故本轮新增注释一律英文；`cl -std:c++17 -W4` 编译
  并运行 `app_client_smoke` 退出码 0；Python `python/tests/test_app_use_cases.py` 14 条全过。

同轮第二件（跨语言二进制契约）：新增门禁 `ring_layout_cross_language_check`（六颗），把 **QXRB 共享内存
环布局**在 Rust / Python / C++ 三处手写镜像之间逐格对照——此前全仓 `grep QXRB tools/check_architecture.py`
零命中，三处没有编译期耦合、也没有任何静态判据：任一处偏移或魔数漂移，读出来的就是错位内存或
header mismatch，而门禁照绿。六颗分别钉：三处实现在盘 / 九格常量都取得到 / Rust 与 Python 逐格相等
（MAGIC、VERSION、HEADER_BYTES、SLOT_HEADER_BYTES、WRITE_SEQ_OFFSET、READ_SEQ_OFFSET、
SLOT_COMMIT_OFFSET、SLOT_LEN_OFFSET、SLOT_CRC_OFFSET）/ C++ 的 `kHeaderBytes`/`kSlotHeaderBytes`
等于 Rust 的对应常量 / C++ 头文件的魔数与 header·seq·slot 偏移逐条对齐 Rust（子串指纹）/ 环布局有跨
语言运行时用例在盘。三发变异全杀（Python `HEADER_BYTES` 漂移 / C++ `kSlotHeaderBytes` 漂移 /
C++ header 校验偏移换错）。`GATE_CHECK_FLOOR` 943 → 949，门禁实测 **1003 → 1009**。

### V13 R31（2026-10-10）· qx-api 读面支持按账户过滤——控制台多账户作用域的后端前置

R30 的审计把控制台的多账户作用域判成「后端读面缺前置」而暂缓（另立任务）。本轮只做那一半：
`qx-api` 的读面先能按账户过滤，前端的账户选择器仍作为独立任务跟进（#26），本轮不碰 `web/console/`。

- **`/account/ledger` 与 `/reconcile/reports` 从「四条整体现读端点一律不收窄」里拆出来，立成一条按账户过滤的通道**：
  读的还是整份现读模型，但底层类型本来就带账户归属列——`LedgerEntry` 有 `account_id` 一列、
  `ReconcileReportSnapshot` 两列都有（`account_id` + `venue_id`）——所以 `?account_id=` 真的能过滤。
  账簿那一格只认一把键不是取巧：`LedgerEntry` 没有 `venue_id` 字段，给它配第二把收窄键只会让调用方
  以为按 venue 读过一遍。反过来 `/scheduler/runs` 与 `/control/audit` 的 `JobRun` / `AuditRecord`
  没有账户归属列，只能继续回 `400` 而不是照常 `200`。
- **过滤读面不回 `404 account_projection_not_found`**：键形状合法但过滤不出条目时回 `200 []`。
  两族口径的差别是刻意的——投影是 `(account_id, venue_id)` 键控的一份独立副本，没有那份副本就是
  「这个账户不在这份部署里」；账簿与对账报告是整本台账加一轮对账结果，过滤不出条目只说明这一轮没有事实。
  把「这一轮没有事实」读成「这个账户不存在」，正是 #191 那类读侧误读。
- **收紧错误出口而不是合并**：两条过滤臂都保住 `400`（键形状错）/ `503`（现读模型取不到）的分工——
  键拼错不会被读成后端故障，后端故障也不会被读成「你没说清」。
- **读面分类收成一个单点 `crates/qx-api/src/read_scope.rs`**：三张路由名单
  （`PROJECTION_SCOPED_ROUTES` 七条 / `MODEL_FILTER_ROUTES` 两条 / `KEYLESS_READ_ROUTES` 两条）
  与五张查询键名单都住在这里。`admission::refused_query_param` 只借名单、不再自持一份 `*_PARAMS` 常量
  ——此前名单散在 `admission.rs` 与 `lib.rs` 两侧，挪走一半就会让准入侧与分派侧各说各话。
- **新增门禁 `read_face_scope_check`（十颗）**：模块在盘且低于行数门槛并挂载 / 三张名单条目数自洽、
  两两不相交、每条都有活的分派臂 / 查询键名单只有 `read_scope` 一份而 `admission` 不再自持 /
  `accepted_query_params` 是唯一取名册出口 / **收窄键名单与底层类型的归属列逐格一致** /
  两条过滤臂真在结果集上过滤且不借用投影那族的 `missing_projection_response` /
  空结果回 `200` 而非 `404 account_projection_not_found` / `400`/`503` 分工不合并 /
  端点表只在真读收窄键的入口承诺查询串 / 九条行为用例在册。
  立项动机：`api_surface_doc_check` 只比路由集合、`api_endpoint_table_routes.rs` 只比表与分派是否平，
  **没有任何一颗判据看「这条臂有没有真读 `query`」**——于是 `/account/ledger` 可以既在表里写 `[?account_id=]`
  又一行过滤都没有，读者拿到的是默认账户那份流水而被当成了自己点名的账户。
- **九条行为用例**（`crates/qx-cli/tests/unit/api_read_route_query_contract.rs`）：两条过滤臂按账户过滤且
  venue 单独成键 / 空结果口径是 `200 []` 而不是 `404 account_projection_not_found` / 名单外的键、
  空串键、纯空白键一律 `400` 并点名被拒那把 / 两条无键端点继续 `400` / 七条带键读面自己的键契约没被
  这次改动带歪 / `snapshot/diff` 只返回文档承诺的那几个非 200 码 / 三张名单两两不相交且每条都有活分派臂 /
  端点表不再为这两条端点承诺 404。
- **接口文档同步**：`deploy/README.md` 两张端点表按新口径重写（过滤端点写出各自的收窄键、无键端点
  不再写 `[?… ]`），并修掉一处**行号死引用**——原文写「`admission.rs:413` 那句 `else { return None }`」，
  该句在上一轮重构后已不存在、行号也对不上，改指 `accepted_query_params(route)?` 这个稳定锚点。

### V13 R30（2026-10-10）· 一次性提交入口的多账户拓扑判据收口 + 第二交易所验收链的门禁牙齿

R29 之后做三轮发布前审计（主体链路连通性 / 孤儿逻辑 / 死循环 / 前后端贯通），两轮都按「先修干净全部问题、再进下一轮」推进。

- **三条一次性提交入口共用同一份账户/交易所拓扑判据（V13 R28 的隔离口径在一次性入口不能断）**：
  常驻 worker 循环一直有 `*_submit_matches_worker` 守「命令归谁」，但**一次性提交入口**这半边有三格缺口——
  ① CCXT 一次性入口（`run_ccxt_submit_order`，R28 新增的那条）显式点名 worker 却从不做拓扑比对，
  订单的 `account_id` 或 `instrument` venue 与点名的 worker 对不上时会把订单事实写进另一台 worker 的
  EventLog；② Paper 一次性入口没有 `worker_id` 参数、按「第一台启用的 Paper Execution worker」挑选，
  多账户共享一份控制面时这条退化成「取第一台」，订单会落进别的账户账本；③ Binance 一次性入口把同一条
  规则**就地再写了一遍**（`account_id != expected_account || !is_binance()`），是同一规则的第二份实现，
  改口径时漏改一侧就会一边拦、一边放行。三处现在都改走同一条判据：Paper 按 `paper_submit_matches_worker`
  挑出与订单账户一致的 worker、挑不出就 fail-closed（不退回取第一台），Binance/CCXT 在动作函数之前比对
  并落成终态 `Failed`。Binance 那份就地实现随之删除，规则只剩一份。
- **新增门禁 `submit_topology_guard_check`**（`SUBMIT_TOPOLOGY_GUARDS` / `SUBMIT_TOPOLOGY_ORDER` 两张表）：
  三条入口的拓扑判据必须落在动作函数被调用**之前**——把比对删掉、挪进动作函数、或 Paper 退回取第一台
  都会红。三处反向变异（删 CCXT 判据 / 删 Binance 判据 / Paper 换成无差别匹配）逐处打红后字节还原。
- **补 `ccxt-submit-order` 的行为用例**：`binance_submit_rejects_order_from_another_account_as_terminal_failure`
  与 `ccxt_submit_rejects_order_from_another_account_as_terminal_failure` 各断四格——错误文案点名「拓扑不一致」、
  审计留下 Accepted + 终态 `Failed` 两条、终态原因码带出拓扑不一致、EventLog 无订单事实且账簿为空；
  `paper_submit_rejects_order_without_a_matching_worker_as_terminal_failure` 断 Paper 挑不出匹配 worker 时
  既不给订单入账、也不写初始资金。
- **门禁为 `--venue okx` 这条链补两组牙齿**（`external_acceptance_check` 内）：此前 `ACCEPTANCE_CONSTANT`
  只钉 Binance 那五格常量，OKX 的八格常量（`OKX_BASE_CONFIG` / `OKX_CCXT_CONFIG` /
  `OKX_EXECUTION_WORKER` / `OKX_RECONCILE_WORKER` / `OKX_KEY_ENV` / `OKX_SECRET_ENV` / `OKX_PASS_ENV`）
  不在名单里——`--venue okx` 整条链路可以断掉而门禁不响，「第二交易所已通」就没有支点。新增四颗判据：
  常数组齐备且 `choices` 里真有 `okx`（不复制脚本而是参数化）、两份 okx 配置文件都指向仓库真文件、
  okx 验收脚本点名的 worker 在 okx 验收配置里存在且已启用、CCXT 配置的凭据引用与脚本检查的环境变量
  同名（同 Binance 那组口径，否则脚本会以为凭据齐了）。两处反向变异（删掉 `OKX_PASS_ENV` / 从 choices
  里摘掉 `okx`）都当场打红。
- **接口文档同步**：`README.md` 命令清单与 `deploy/README.md` 的「订单提交」一节补上 `ccxt-submit-order`
  的完整参数顺序（`runtime.json <worker-id> <ccxt-config.json> <command.json>`，比 binance 多一个 CCXT
  配置路径）与它和三条入口共用的拓扑口径；`docs/外部链路验收执行方案-V1.md` 修掉那条过期的三参数签名
  并按仓库惯例加「回写批注」而不是删历史；`docs/外部链路验收-凭据与测试信息清单.md` 的收尾清单按实测结果更新。
- **CLI 表面四面对齐已核**：`ccxt-submit-order` 在 clap 命令枚举、`cli.rs` 派发臂、`cli_help.rs` 帮助文本、
  `tests/command_surface.rs` 的 `CLI_COMMANDS`（46 条）四处一致，且 `--help` 实跑通过。
- **验证**：`tools/check_architecture.py` **940 项全绿**（935 → 940，`GATE_CHECK_FLOOR` 882 不动）；
  `cargo test -p qx-cli --offline` **457 passed / 0 failed**（R29 的 453 → 457，新增 4 条拓扑用例）；
  `cargo test --workspace --offline` **1231 passed / 0 failed**；`cargo clippy -p qx-cli --all-targets` 零告警；
  本轮改动的 9 个 Rust 文件逐个 `rustfmt --check --config skip_children=true` 全干净。
- **仍挂账的两笔（如实登记，不假装完成）**：① 诊断债 `UNLABELED_DIAGNOSTIC_CEILING` 仍在 55——剩余站点里
  有若干（`main.rs` 的 `rejected`/`acked`、JSON 直出站点）被测试逐字断言，加标签会当场红，要连带改测试才能收口；
  ② 控制台多账户作用域（四条读面加 `account_id`/`venue_id` 选择）需后端 `qx-api` 读面先支持按账户过滤，
  另立任务。

### V13 R29（2026-10-09 续）· 诊断债偿还（棘轮下探 100 → 55）

承接 R28 把诊断公共组件面钉成的「写下来的决定 + 有上界的欠账」，本轮开始偿还这笔债，并证明棘轮确实只降不升（不是躺着不动的允许清单）。

- **`cli.rs` 整段错误/诊断输出收到 `[qx-cli · CLI]` 标签下（45 处）**：outbox/event-consumer/DLQ 中继失败、init/doctor/config 入口失败、回测各子命令失败、supervise/scheduler/各 worker 启动失败、binance/probe/submit 失败等内部错误报告统一带组件前缀，运维可按组件过滤。这些站点的 CLI 用例只断言退出码与 stderr 关键词、不逐字断言文案，加前缀零风险。
- **门禁 `UNLABELED_DIAGNOSTIC_CEILING` 从 100 下探到 55**（`process_diagnostics_check` 三颗仍全绿：`qx-cli` 实测 55 ≯ 55）。`_diagnostic_site_census()` 实测：`qx-cli` 未带标签站点 **100 → 55**。
- **故意留在债里的（逐字被测试断言，迁了会红）**：`main.rs` 的 `rejected`/`acked`（`process_recovery.rs` 的 `assert_eq!(stale, "rejected")` / `assert_eq!(acked, "acked")` 逐字匹配）、若干 JSON 直出站点（`config.to_json()` / `plan canonical_json` / `data_validate` / `report` 的 `println!("{}", …)`）。
- **验证**：`cargo test -p qx-cli --offline` **453 passed / 0 failed**（cli.rs 改动零回归）；反向变异——把上界临时改 55 → 54，门禁在 `只降不升` 那颗当场打红（越界 `{'qx-cli': 55}` / 登记 `{'qx-cli': 54}`），还原后复绿。
- 计划文档 `docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md` 新增 **§18.6** 记录本轮；§18.4 的欠账读数同步更新为 55。`maturity/capabilities.yaml` 的 `process_diagnostics_have_no_framework_single_source` 限制项同口径更新。门禁仍 **935 全绿**，`GATE_CHECK_FLOOR` 882。

### V13 R28（2026-10-09）· 全量验收与深度改进（多账户并行隔离、CI 特性全关腿、诊断面棘轮）

**口径**：承接用户对 `docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md` 的八问——「是否已按方案全部修复 / 全部优化改进 /
主体链路是否全部贯通 / 多种运行方式是否全部可行 / 多账户·多资产·多策略·多运行模式同时并行是否可行 / 易用性与快速执行是否到位 /
日志与报错等公共组件是否统一封装 / 继续深度改进」。逐问的答复与实测证据写进该文档新增的 **§18**；本条目只记**改了什么**。
每一处都按仓库纪律配齐**门禁牙齿 + 反向变异验证 + 行为用例**，`tools/check_architecture.py` 从 **922 → 935** 条判据
（`GATE_CHECK_FLOOR` 869 → **882**），全绿。

- **① 多账户 / 多 Venue 并行运行的隔离，从「名字拼得对」变成「事实不串账」**：此前这条不变量只有命名层用例
  （`account_event_log_name` 拼得对）与读侧去重用例（同一账户不分裂），**没有一条用例真把两个账户的 worker 各跑一遍再翻开
  两本账看有没有串账**——命名对不等于事实不串。新增 `crates/qx-cli/tests/unit/parallel_run_isolation.rs` 四条行为用例：
  (a) 两个账户在同一 venue 上各跑一次**真实** `run_paper_execution_worker`，两本账互查「对方账户现金为 0」且不落
  `paper-events.json`；(b) 一条 `SubmitOrder` 命令只按 `order.account_id == worker.account_id` 分派——先跑第二账户的 worker
  拿到空订单簿，再跑主账户才成交；(c) 每个配置的账户各拿到自己那份投影源；(d) **真并发**：`Barrier::new(2)` 卡住两条线程，
  同时跑两个账户的真实执行 worker，两本账仍各自独立。新判据 `parallel_run_isolation_check` **七颗**（用例在册 / 第二账户日志名
  从唯一构造点 `account_event_log_name` 派生且用例里无字面量日志名 / 写侧两账户各跑一次真实入口 / 写侧跨账户零现金互查 /
  并发用例真开两条线程 + `Barrier::new(2)` / `paper_submit_matches_worker` 拿订单账户比对 worker 声明账户 /
  `seed_paper_initial_cash` 按 worker 声明的账户入账而非硬编码 `main`）；8 处变异全红，逐处字节还原。
- **② CI 补上「特性全关」的另一半腿**（收口 §17.11 登记的覆盖面缺口）：`feature-matrix` 只 lint「特性开着」的组合，
  `#[cfg(not(feature = "sqlite"))]` 那半边在 workspace 构建里从不被 clippy 看到（`qx-cli` 的 `default = ["sqlite"]` 经转发
  把 `qx-runtime/sqlite` 一并打开）。`.github/workflows/ci.yml` 新增 `feature-off` job，按
  `--no-default-features --all-targets` 对四个声明了非默认特性的 crate 跑 clippy（带 `-D warnings`）与 check。
  `ci_feature_matrix_check` 从 4 颗扩到 **6 颗**：`feature-off` 腿在盘且 clippy 步骤**单独**带齐三个旗标（变异 ② 实测踩到——
  原先在整段 job 文本里搜旗标，旁边 `cargo check` 步骤的同名旗标会替它满足）、有 `cfg(not(feature = …))` 分支的 crate
  都在腿上、腿点名的 `-p` 集合恰好等于声明了非默认特性的每个 workspace crate；5 处变异全红。**这条腿当场见效**：
  它抓出了本轮新写的 `parallel_run_isolation.rs` 里一处 `needless_borrows_for_generic_args`。
- **③ 公共组件面：报错已统一封装，诊断这一格落成「写下来的决定 + 有上界的欠账」**：`qx-core::error` 的 `ErrorCode` 闭集 /
  `Retryability` / `ErrorContract` / `QxError::contract()` 唯一映射表已由 `error_code_contract_check` 八颗钉住，不需要动。
  缺的是**日志**：全仓 `crates/*/Cargo.toml` 没有任何 `tracing` / `log` / `env_logger` / `slog` / `fern` / `log4rs` 依赖，
  245 处诊断（`_production_sources()` 口径）直接走 `println!` / `eprintln!`，靠一条**非正式**的 `[组件 · 子域]` 前缀约定；
  `qx-api`（8 处）与 `qx-orchestrator`（2 处）已全量带标签，`qx-cli` 还有 **100** 处未带。本轮**刻意不**一次迁移完 100 处
  （那是一次横跨 45 个 CLI 入口的纯文案改动，收益只有过滤一致性，代价是 100 处 diff 与随之而来的用例文案漂移），
  改为新增 `process_diagnostics_check` **三颗**把它钉成不可回退的欠账：① 一旦引入诊断框架，未带标签站点必须为 0
  （半迁移会在既有约定之外多出**第三种**约定）；② 未带标签站点数只降不升（`UNLABELED_DIAGNOSTIC_CEILING` 棘轮，
  表外 crate 一律要求 0）；③ 棘轮表是**活名册**（归零的 crate 必须从表里删掉、新增未标签站点的 crate 必须登记）。
  4 处变异全红。同轮把这条决定登记进 `maturity/capabilities.yaml`（`storage_consistency_contract` 下的
  `process_diagnostics_have_no_framework_single_source`，与该块既有的进程日志保留策略、写侧超时文案两条 limitation 同族）。
- **文档面**：方案文档新增 §18（八问逐条回答 + 三件落地 + 主链/运行方式实测证据表 + 仍未关闭清单 + 本轮读数），
  附录 B/C 与收尾注记按 2026-10-09 现状回写；同时**修正本文件与方案文档里能力块计数的一处口误**——
  此前写作「27 条能力块」，实测是 **25 条**（`maturity/capabilities.yaml` 的 `capabilities:` 段下 25 个两格条目，
  与附录 C 表格行数、§3.13 的「3 个 profile + 25 条能力」逐一对上）。

**本轮实测读数**：`tools/check_architecture.py` **935 项 PASS / 0 FAIL / 150 个 `*_check()`**；
`cargo test --workspace --offline` **exit 0 / 1227 passed / 0 failed / 1 ignored（122 个 target）**；`cargo test -p qx-cli --offline` **453 passed / 0 failed**；
`cargo clippy --workspace --all-targets -- -D warnings` **0 告警**；`--no-default-features --all-targets` 四条腿全绿；
反向变异 **17 枪全红**（并行隔离 8 + CI 腿 5 + 诊断面 4），逐枪字节还原。三条主链与运行方式现跑证据见方案文档 §18.3。

### V13 R27（2026-10-09）· 架构梳理稿的 P0–P2 改进项全量落地（写面稳态成本、保留策略、锁中毒、机读快照）

**口径**：承接用户「`docs/牵星qianxing_架构梳理与缺点分析.md` 文档梳理出来的改进项，全面优化改进存在的问题」，
按该文档 §7 的 P0–P2 优先级表逐条落地。每一处都按仓库纪律配齐**门禁牙齿 + 反向变异验证 + 行为用例**，
`tools/check_architecture.py` 从 **895 → 922** 条判据（`GATE_CHECK_FLOOR` 843 → **869**），全绿。

- **P0-1 账户级金额字段接真实生产者**（此前 `unrealized_pnl_raw` / `realized_pnl_raw` 恒发布 `null`）：
  `Ledger::realized_pnl_for` / `Ledger::unrealized_pnl_for`（`crates/qx-core/src/ledger/query.rs`）成为两个算点，
  `qx-cli` 的账户快照投影接上它们。缺标记价即 `None`（不是 0）、累计全走 `checked_add`——`margin_raw` /
  `frozen_raw` / `funding_raw` 三格**没有生产者**（保证金要保证金模型、资金费要资金费台账），按零读者纪律
  如实保留在 `SNAPSHOT_UNCOMPUTED_FIELDS` 并由 `snapshot_money_honesty_check` 钉住。四处口径同步改口：
  `schemas/account-snapshot-v1.json` 的 description、`maturity/capabilities.yaml` 的两条 limitation 删除、
  `deploy/README.md` 的「有算点」分组、`api_snapshot_money_fields.rs` 的逐字段点名。新判据
  `account_pnl_producer_check` 三颗；5 处变异全红。
- **P0-2(a) 写面稳态追加不再整份拷贝状态**：`LiveEventPipeline::ingest_once` /
  `register_order_with_correlation_once` 原先每次追加都先 `self.clone()` 整份状态（O(日志长度)）。
  现在拆成「薄包装（失败即回滚）+ `*_commit`（就地提交）」，回滚走 `rebuild_from_store(true)`——
  store 只在 `persist` 成功时才写，所以失败时它仍是旧账本，重放即回滚。`force` 形参是必须的：
  `append_at_engine` 会在 `append_checked` 之前就 `alloc_seq` 推过 `next_seq`，而 `events` 一条没变，
  常规重建的"日志逐条相等就跳过"短路会让这一格漏回滚（这条正是变异 ③ 抓出来的）。四个方法从顶格文件
  `pipeline.rs` 切进新子模块 `crates/qx-runtime/src/pipeline/commit.rs`（同 crate inherent impl，子模块可见
  父模块私有项），`pipeline.rs` **2480 → 2160 行**，顺带把文档 §7 的 P3「大文件拆分顺路做」也做掉。
  新判据 `pipeline_commit_rollback_check` 四颗；新行为用例
  `failed_ingest_rolls_back_the_appended_fact_and_the_seq_cursor`（未知订单的 `Accepted` 在**追加之后**才失败，
  断事件数与 `next_seq` 都收回、且回滚后仍可继续）；8 处变异全红。`seen_fills` 台账 census 跟着写面搬家。
- **P0-2(b) EventLog 的保留/归档策略从「没有策略」改成「写下来的决定」**：这本日志是账簿与全部内存索引的
  **重放源**，所以策略是①**册内保留被禁止**（`EventLog` 公开面不提供也不得新增任何截断/淘汰/压缩入口——
  活日志变短会让账簿重放与 `refresh_latest` 的前缀校验同时失去参照物）；②**轮转是运维杠杆**（换一本账靠换
  `log_name`，旧日志原样留着）；③**归档粒度是封段**（分段后端只有尾段可增长、满段永远不可变，段清单带摘要
  校验）。正文写在写面模块文档（`pipeline.rs`），`EventLog` 的类型文档留一行指针（`sourcing.rs` 顶格在行数
  棘轮上，加行会红）。新判据 `event_log_retention_policy_check` 三颗；登记面两条 limitation 的旧读数
  （"`ingest_once` 每次追加仍先 `self.clone()`"）按"部分收敛"改口；8 处变异全红。
- **P1-5 名义额规则缺规格 fail-closed**：`legacy_spot_spec` 删净，`MaxNotionalRule` 体点名缺规格消息且不再
  合成临时规格，`RiskGate::check_with_spec` 在盘且两条回测路径都传规格。新判据 `risk_spec_fail_closed_check`
  六颗；8 处变异全红。**收口时才发现 parity 面被这次改动打红**（`cargo test --workspace` 里
  `contract-tests/tests/risk_parity.rs` 5 条挂 4 条）：三入口一致性用例的 (c) 腿原先走**无规格**的
  `check_with_price`，fail-closed 之后它把"缺规格"当成一条业务拒绝，于是 (c) 与 (a)/(b) 的 violations
  集合再也对不上。修法不是放宽判据而是**把 (c) 腿换成带规格的 `check_with_spec`**——这正是两条回测生产
  路径今天调用的形状，用例因此比改前更贴近生产。另加一颗判据（第 ⑥ 颗）钉住 `assert_gate_matches_canonical`
  的**函数体**必须出现 `check_with_spec(` 与规格实参，3 处变异全红（含"只在注释里写锚点"那一处，证明
  判据读代码不读散文）。reduce-only 用例同时补一条反向断言：**无规格入口只能多报"缺规格"，绝不能少报
  reduce-only 那条**——fail-closed 不得波及与规格无关的不变式。
- **历史记录（当时）**：P2-6 曾让 `--snapshot` 同时写行数棘轮与门禁数量/label 摘要，并要求实测值等于快照。
  后续复核已确认固定数量、标签摘要与静态源码行数都不代表质量；数量快照验收及 `maturity/line_budgets.yaml` 行数棘轮均已删除。
  README 两处写死的条数与地板改成引用文件名。**落地时踩到两个自己造的坑**：① `gate_snapshot_check` 在
  "快照缺失"与"快照在盘"两条路径上各发**不同条数**的判据，于是生成轮记下 N、复核轮算成 N+1，那条相等判据
  永远不可能成立（修法：恒发三条）；② 改了地板常量之后旧快照必然与常量不等，而那颗判据跑在写快照之前——
  生成本身被自己挡住（修法：生成轮把盘上那份当"不在盘"处理）。新判据 `gate_snapshot_check` 三颗 + 收尾一颗；
  4 处变异全红（其中一处走完整门禁）。
- **P2-7 发布版本单源 + CI 显式钉解释器**：wheel 版本 == workspace 版本；`rust-core` 作业设 `QX_PYTHON`。
  新判据 `release_version_single_source_check` 两颗。
- **P2-8 `qx-api` HTTP 路径上的 `.expect(` 改成显式 503**：19 处 `api state mutex poisoned` 全部走新的
  `crates/qx-api/src/state_lock.rs`（取锁收成可失败入口 `lock_state`，读面错误收口 `read_error_response`
  把中毒判成 **503**、其余 400），`QueryPort` 九个读法一并改回 `Result`。新判据 `api_lock_fail_closed_check`
  五颗；新用例 `crates/qx-api/tests/api_lock_fail_closed.rs`（真中毒 + 13 条读面断言 503 + 对照组）；
  5 处变异全红。
- **P1-3 / P1-4 复核结论：已在册，不重复实现**。权益 8 入口收敛成 `ValuationContext`/`ValuationResult`
  与 `qx-execution::RiskVerdict` 改名都是 V13 R23 已落地的（分别由 `valuation_single_source_check` 与
  源码注释点名 `qx_risk::RiskDecision` 守着）；`python/qianxing_ashare` 与 Rust 规则的跨语言对照已由
  `ashare_cross_language_contract_check`（14 颗）与三份夹具承载。按「不要图省事重做」处理，只做复核。
- **行数棘轮**：`--snapshot` 重生成 `maturity/line_budgets.yaml`，除本轮真实下降的
  `pipeline.rs`（2480 → 2160）外，还同步了几处**非本轮**的既有下降（`qx-scheduler/src/lib.rs` 1560 → 1464、
  `ccxt.rs` 1163 → 1160、`qx-api/src/lib.rs` 2969 → 2967、`event_pipeline.rs` 798 → 787、
  `strategy_host.rs` 915 → 908、`api_snapshot_money_fields.rs` 518 → 517），并移除了已不再超 500 行的
  `crates/qx-cli/src/config_commands.rs: 539`。棘轮只降不升，记录实际值是正确的收紧方向。

### V13 R26（2026-10-09）· 文档面产品化：README 重订、使用文档口径重钉、七份规划稿归档

**口径**：承接用户「更新 readme 说明文档，向产品说明文档一样，同时更新项目的使用文档，删掉旧版本无效的历史
文档信息，全部更新之后，提交最新变化」。这一遍**不动运行面代码**，改的是「文档说的数」与「代码里的数」之间
的那点差——而那点差是可数的。

- **README 产品化**：根 `README.md` 684 → **551 行**。新增「交付面一览」（八格，每格点名「谁在核对这个数」：
  CLI 入口名 46 / 内置策略 17 / HTTP 路由 18 / WebSocket 事件流 1 / `deploy/` 顶层模板 53 / workspace 成员 25 /
  架构判据与用例地板）与「看结果：离线报告、静态页面与同源 BFF 控制台」；把只在本机可复现的逐轮读数
  （`all` 链现场、产物身份那段完整措辞、第二棵源码树实测）摘进 `docs/archive/README-历史实测快照-2026-10-09.md`，
  正文只留**行为**与它的核对者。`demo` 段按 2026-10-09 本机重抓：退出码 0、`digest=915bfad3b6872a36`。
- **使用文档口径重钉**（`deploy/README.md` 八处，全部单行改，行数一根手指都没动）：HTTP 入口从「17 条」改成
  **18 条（17 条 `GET` 读面 + 1 条 `POST` 写面）**——`/schema/contract-matrix` 上线后两张端点表各含 18 条路由，
  文档、门禁与用例却还停在 17；认证豁免名册补 `/schema/contract-matrix`（判定体 `crates/qx-api/src/lib.rs:1498`
  的 `matches!` 里就是这四条）；「不判查询串的全局出口」四条 → **五条**，覆盖面算术随之改成 12 条读面入口 +
  5 条明写边界 = 17 条 GET；那处指向 `else { return None }` 的引用从 `crates/qx-api/src/admission.rs:404`
  改成 **:413**（404 是函数签名行，那句 `else {` 起于 413、`return None` 落在 414——行号能落地但落点说错了话，这类指针门禁抓不到，
  靠人核）；`GET /control/audit` 那格的「上面那四条」改成**三条**（它上面那行只摊了三条 keyless 入口）；
  带 `environment` 的 runtime 模板 18 → **19 份**（paper 15 / sandbox 2 / testnet 1 / production 1）。
- **文档契约用例跟着改口**（两份文件 8 处单行）：`api_endpoint_table_routes.rs` 四道地板 17 → **18**（分派集合、
  两张表各自的路由解析、语义表行数）加两道失败详情里的「17 条 / 17 行」；`api_response_field_doc.rs` 的
  `rows.len() >= 14` → **`>= 18`** 与详情里的「17 条路由」。收紧是加性的：原口径下只有整行被删才红，
  现在表被读窄也红。定向复跑 `cargo test -p qx-cli --bin qx-cli -- api_` → **42 passed / 0 failed**。
- **登记面**：`maturity/capabilities.yaml` 三处活口径（两条证据里的 `deploy/` 顶层 52 → **53** 份、
  `api_surface_doc_check` 那条的 17 条路由 → **18** 条）。**带日期的当轮读数一律不改**——CHANGELOG、
  `docs/archive/**`、门禁注释里的历史条目、V13 §1 与 2026-10-06 架构稿 §14 那批写着「本轮实测」的基线，
  它们记录的是当时成立的事实，追改等于销毁证据。
- **七份规划稿归档**：四份 2026-10-03 规划（架构审计 / 量化回测 Web 平台 / 桌面与 Web 客户端接入 / 与 tauron
  一体化桌面）、一份 2026-10-04 合订本、一份 2026-10-08 综合路线图、一份 2026-10-08 发布审计（V13 R22）
  `git mv` 进 `docs/archive/`。每份只在**文末**追加 14 行「归档状态」节——追加在标题下会让活文档里那些按行号
  取证的 `path:NN` 整体下移。路径提及同步 11 处：活文档 6 处（`CHANGELOG.md` 1 / V13 3 / 2026-10-06 架构稿 2）
  与同批互指 5 处。归档理由逐份写在自己的节里，其中 tauron 那一份靠**全仓扫描**立据：`crates/`、`web/`、
  `python/`、`deploy/`、`tools/`、`schemas/`、`.github/` 七处目录里 `tauron` 与 `tauri` 两个关键字各 **0 命中**。
  `docs/archive/README.md` 索引 33 → **48 行 / 8,519 字节**，十条在册，读法纪律里那处「README 的『当前状态』」
  指针同步改成「交付面一览」与「版本与现状」。
- **引用可达性重钉**：`LIVE_DOC_CITATION_FLOOR` **195 → 272**（终字节实测 272 条 `path:NN`，落在 30 份活 .md 上，
  落空 **0** 条；上一档 195 是 R1-G 在那七份还住在 `docs/` 时量的，中途 270 是这条 CHANGELOG 入库前测的那一档）。存档侧落空 9 条 = 天花板 9，**未升**，
  `ARCHIVE_DEAD_CITATION_CEILING` 不动；`GATE_CHECK_FLOOR` 与 `WORKSPACE_TEST_FLOOR` 也不动（本轮没加判据）。
- **环境腿的一次假红**：`cargo test --workspace --no-fail-fast` 首跑在默认 `%TEMP%` 上 **22 颗用例红（121 段 / 1196 passed / 22 failed）**，
  正文只有两种、同一族：**13 颗是「SQLite 打不开库文件」、9 颗是 `Os { code: 5, kind: PermissionDenied }`**——前者全在 `qx-storage`（按源文件落点：**`sqlite_event_log.rs` 7 颗 + `sqlite.rs` 5 颗 + `snapshot_rows_persist.rs` 1 颗**），
  文案是 `Io("SQLite: unable to open database file: …\\Temp\\qianxing-*.sqlite")`；后者散在
  **`qx-strategy` 6 颗、`qx-adapter` 2 颗、`qx-cli` 1 颗**，文案统一含 `code: 5, kind: PermissionDenied`（`Io(Os {...})` 与裸 `Os {...}` 两种写法都
  指向同一件事：`%TEMP%` 拒绝写入）。把 `TMP`/`TEMP` 指到仓内可写目录后整树复跑 → **121 段 / 1218 passed / 0 failed / 1 ignored，`TEST_RC=0`**；
  这一族用例都在 `%TEMP%` 下建文件，与被改的文档面无关。
- **验证**（全部本轮现跑）：`python -X utf8 tools/check_architecture.py` → **895 项 `[PASS]` / 0 项 `[FAIL]` / rc 0**；
  `cargo test --workspace --no-fail-fast` → **121 段 `test result:` / 1218 passed / 0 failed / 1 ignored**；`cargo clippy --workspace --all-targets -- -D warnings` →
  **rc 0，诊断行 0 条**；`cargo fmt --all --check` rc 0；`python -X utf8 -m unittest discover -s python/tests` →
  `Ran 68 tests / OK`；`git diff --check` 无空白错误。
- **本轮没做**：`docs/牵星qianxing_架构梳理与缺点分析.md`（2026-10-09 由仓库外的动作新建，391 行，
  0 处 `path:NN` 引用）留在未跟踪状态，不代它决定是否入库；§17.11 那两条 `--no-default-features` 的 clippy 腿
  仍在 CI 之外（既有登记，未动 `.github/workflows/ci.yml`）。


### V13 R25（2026-10-09）· 两轮复扫：字段级接线、游标口径与登记面自身的可机读性

**口径**：承接用户「继续检查还有哪些逻辑存在潜在问题，主体流程全部联通，核心链路不存在断链，逻辑连贯
不存在孤儿逻辑，不存在死循环，前后端全部贯通。至少检查 2 轮，每遍修复全部问题之后再执行下一轮」。
本轮把**前端 ⇔ 后端**这条链从"路径级全绿"下压到**字段级**，并把二轮复扫抓到的游标口径、登记面机读性、
死信台账增长三件事一并收口。全部加性：门禁 **853 → 895 全绿**（`[PASS]` 895 行 / `[FAIL]` 0 行 / rc 0），
`GATE_CHECK_FLOOR` **801 → 843**，`WORKSPACE_TEST_FLOOR` **1083 → 1233**，未删任何既有判据。

- **第一轮（前端字段级断链，全部修复）**：`web/console/app.js` 七格取数名册按后端线格式逐字段重订
  （快照身份回到 `header` 而不是顶层、`PositionSnapshot` 那三列名册外的幻影键删掉、`event_seq` 回到
  envelope）、定点金额走唯一标度出口（`RAW_DECIMALS = 9` ⇔ `qx-protocol` 的 `SCALE = 1_000_000_000`）、
  四个时间列走唯一毫秒出口、WS 帧按 `qx-api/src/ws.rs` 那份词表分派、写面补齐 `X-QX-CSRF` 双提交并把
  BFF 的三种拒绝如实转述、非 200 不再静默。`index.html` 三张表的列定义与 `<th>` 数逐格对齐。
- **第一轮的两条后端真缺陷**：① `crates/qx-api/src/lib.rs` 的审计身份覆盖原先住在 `match &self.policy`
  的 `Some(policy)` 臂里，`api.operators: {}`（出厂模板与 `--init` 的默认形状）这条路上**永远走不到**，
  页面表单里自称的 `operator_id` 原样进审计——现以认证边界为准，并新建
  `crates/qx-api/tests/console_operator_identity.rs` 走公开面 `ConsoleFront::handle` 做常驻反例（刻意用
  **没有**名册的 `ApiService::new`，`src/console.rs` 里既有的用例全都配了名册，恰好是缺陷藏得最深的那一侧）；
  ② `qx-orchestrator` 在 `spawn` 成功而 `stdin.take()` 落空时把子进程丢掉，现先挂进 `children` 台账，
  由唯一那处 `stop_managed_children` 按预算轮询 `try_wait` 收摊。另修 `qx-core/src/clock.rs` 的 `Ts`
  文档口径（**epoch 毫秒**，不是纳秒——当成纳秒的下游会把 2026 年渲染成 1970 年），以及
  `qx-runtime/src/pipeline.rs` 在非 `sqlite` 特性下的 `unused_variables`（关掉默认特性的那条构建腿会在
  依赖 crate 里冒出诊断，`-D warnings` 当场红）。
- **第二轮（复扫新抓到的四类）**：① 页面与 `qx-api` **并存两套游标约定**——后端 `after` 是"最后看到的
  seq"、回 `seq > after`，页面原先每追平一次就写 `seq + 1`，下一轮必然吃一发 409 `cursor_ahead`；现收成
  一个 `noteCursor(seq)` 出口，"没有基线"用 `null` 且**不带** `after`，WS 重定基走同一个出口；② 名册里
  留着一条**没有任何读者**的路径，而 WebSocket 通道名是第二份字面量；③ `maturity/` 的 8 份登记文件里
  **曾有 2 份**（`capabilities.yaml`、`http_surface.yaml`）对标准 YAML 解析器是 `ScannerError`——机读面
  只对门禁可读，对编辑器 / CI lint / 下一个工具是坏的，38 + 3 行标量加引号后**内容一字不改**（与
  `.tmp-audit/backup_*.yaml` 逐行对照）；④ 死信台账与已处理幂等台账在三条后端上只追加、无保留期。
- **牙齿**（全部加性）：`web_console_field_wiring_check` 新建 **三十一颗** → 二轮 **三十四颗**；
  `capabilities_check` 三颗 → **四颗**（自带四条自证：坏形态放得进、引号/块标量/`- key: value` 豁免得了）；
  `bounded_growth_and_reap_check` 九颗 → **十颗**；`console_front_check` 十七颗 → **十九颗**；
  `web_console_check` 九颗 → **十一颗**；`resource_lifecycle_and_lock_reentrancy_check` **+两颗**。
  `WORKSPACE_TEST_FLOOR` 1083 → **1233**：地板自 R12/R13 起一直停在 1083，而磁盘在册实测已到 1233——
  12.2% 的余量可以让一整族用例消失而门禁照印全绿，正是这颗地板立起来要抓的那种失效。
- **反向变异 35 枪全红**（KILLED 35 / COLLATERAL 0 / EQUIVALENT 0 / BAD 0，十六份被改文件逐份 sha256
  比对回原字节）：二轮 14 枪覆盖五颗新牙齿，一轮 16 枪里有 **三支是空枪**（F01/F14/R02 的锚点住在修复前
  的写法上，轮末字节上 `ANCHOR_BAD hits=0`——空枪不等于牙齿抓不到），重订锚点复打成 E01–E04 全红；
  F05 那一枪其实早已打红，只是点名子串写的是常量名而判据文案是中文，订正后复打。cargo 侧 I01：摘掉服务端
  身份覆盖 → 常驻反例红（`failed=1`），还原后同一条用例在还原字节上绿（`passed=1`）。脚本在
  `.tmp-audit/battery_r25*.py`、`.tmp-audit/refire_r25c.py`、`.tmp-audit/mutate_identity.py`。
- **本轮实测（收尾验证，读数全部由日志派生）**：`cargo fmt --all --check` rc 0；`cargo clippy --workspace
  --all-targets -- -D warnings` rc 0 / **诊断 0 行**，另加两条**关掉默认特性**的腿
  （`-p qx-runtime` / `-p qx-storage --no-default-features --all-targets`）各 rc 0 / 诊断 0 行；
  `cargo test --workspace --no-fail-fast` **121 段 `test result:` 行、1218 passed / 0 failed / 1 ignored**；
  `cargo test -p qx-cli --no-default-features --features sqlite` 19 段 **448 passed / 0 failed**；
  `python -m unittest discover -s python/tests` **`Ran 68 tests / OK`**；`python tools/check_architecture.py`
  **895 项全绿**；`git diff --check` rc 0。
- **文档回写**（同一轮内、按现读实测）：两份主文档与 README 的门禁条数、`GATE_CHECK_FLOOR`、
  `WORKSPACE_TEST_FLOOR`、门禁脚本规模（14,729 行 / **141** 个 `*_check()` / **814** 处 `check(...)`）、
  能力矩阵（**3 个 profile + 25 条能力**，`implementation` / `code_tested` 各 **21**）、CI job 数（**14**）、
  行数棘轮（**46** 份）、`deploy/` 顶层模板份数（**52 → 53**，逐处区分"今天的读数"与"那一轮的读数"）、
  workspace 成员数（23 → **25**：24 个 `crates/qx-*` + `crates/contract-tests`，架构图与分层表同步）
  一并回写；`crates/qx-cli/build.rs` 那句注释不再硬编份数（改由 glob 决定）。新增主文档 §17.11 记录这两轮。
- **按实测留在册、本轮不修的三条**（写进 `maturity/capabilities.yaml` 的 limitations，不是"发现而未处置"）：
  `idle_reply_windows_have_no_local_pacer__the_cadence_is_the_childs_cooperation`、
  `venue_adapter_capability_and_health_surface_has_no_production_reader`、
  `consumer_state_processed_ids_and_dead_letters_grow_without_retention`。另登记控制台**多账户作用域**
  【设计意图，未排期】：页面四条读面都不带 `account_id`/`venue_id`，走无键分支读全局投影，按账户投影部署时
  页面读不到那些账户，且无法区分"这个账户没账"与"没有全局投影"——现状是单 operator 单账户。
- **诚实登记的一处环境读数**：本机 `%TEMP%` 在这一轮里对 Rust 进程不可写（`std::fs::write` 回
  `Os { code: 5, PermissionDenied }`，同一目录 Python 写得进），三颗要写临时文件的用例因此红过；把
  `TEMP`/`TMP` 指到仓内可写目录后同一条链 0 failed。这是**机器状态不是代码**，Linux CI 不受影响，登记在此
  是为了下一轮不再去追这条。
- **仍未关闭**：`qx-cli serve` 自动挂载 console；桌面 Host、多机反代 / 第三方对外暴露（M4'）；
  Paper/sandbox/production 外部验收（M5'）。本轮不创建 tag、不触发 Release。


### V13 R24（2026-10-09）· 控制台易用性三件：令牌缺失自动生成 + `--init` 脚手架 + `--generate-token`

**口径**：承接用户「是否可以在本地启动 web 服务？易用性是否可以继续提升？」与「没有配置
`QX_CONSOLE_BOOTSTRAP_TOKEN` 时自动生成并提示、已配置则按配置启动」。本轮把这条链做成一等能力，并
**不改**安全口径：令牌仍优先来自环境变量、仍不进命令行/配置文件/日志；自动生成的只是**一次性兜底**。
全部加性：门禁 **848 → 853 全绿**，`GATE_CHECK_FLOOR` **796 → 801**，未删任何既有判据。

- **令牌兜底（`console_serve.rs`）**：`std::env::var(&console.bootstrap_token_env)` 仍是首选来源；
  缺失或为空时 `generate_bootstrap_token()` 临时生成一枚（96 个十六进制字符），并当场打印
  「已临时生成一次性引导令牌（进程退出即失效，重启换新）」。熵来自标准库 `RandomState`（进程内首次
  使用时由 OS 熵源随机化）——**不引入 RNG 依赖**，与 `console.rs` 用引导令牌经 SHA-1 派生会话 id 同一取舍。
  这枚令牌随进程生灭、不落盘，只服务本机回环控制台的首次引导，不替代运维显式配置的长期秘密。
- **`qx-cli console --generate-token`**：只打印一枚令牌与可直接粘贴的 `export` 行后退出（不启动服务）。
  刻意不写成裸输出：启动横幅走 stdout，`$(…)` 会把横幅一起吞进去。
- **`qx-cli console --init <path>`**：写出一份与部署模板**同形**的就绪配置（绑回环、令牌只给环境变量名、
  正文无令牌字面量字段），**拒绝覆盖**已有文件（静默改写等于替别人签字），并打印下一步。
- **门禁牙齿（新建 `console_usability_check`，五颗）**：令牌兜底在盘且环境变量仍是首选；生成令牌有行为
  用例在盘（长度下限 + 十六进制 + 两次不同）；`--generate-token` 声明且被读到；`--init` 声明且被读到、
  且对已存在文件拒绝覆盖；`--init` 模板与部署模板同形（当 JSON 解析后逐格核对）。`GATE_CHECK_FLOOR` **796 → 801**。
- **反向变异已实测（五处，各自单独红）**：改名生成函数 / 改测试名 / 改旗标声明 / 抹掉覆盖拒绝 / 模板绑
  `0.0.0.0`。脚本 `.audit/console_usability_mutation.py` 跑完逐字节还原，**5/5 全部打红**。
- **文档**：`deploy/README.md` 补 bash + PowerShell 双语起步、令牌兜底说明、两条旗标，以及
  「作为独立服务运行」小节（systemd unit + NSSM/Windows 服务），并诚实说清"独立服务 ≠ 对第三方开放"：
  跨主机仍需 TLS/mTLS + 独立反代 + operator 身份 + 显式 CORS（**M4' 未关闭**）。
- **仍未关闭（诚实登记）**：`serve` 自动挂载 console（受 `strategy_contract.rs` 行数棘轮与 supervisor
  生命周期复杂度约束，本轮未做）；桌面 Host；多机反代 / 第三方对外暴露（M4'）；Paper/sandbox/production
  外部验收（M5'）。本轮不创建 tag、不触发 Release。


### M4'/M5' 续（2026-10-09）· 同源 BFF 控制台（`qx-cli console`）+ Paper 轨验收

**口径**：用户确认「两条并行全量推进」。本轮一次做完两件事——**同源 BFF / CSRF / 权限·会话模型**
（三件事是**同一条链**，BFF 是后两者的载体），以及 **Paper 轨**（M5' 里不需要任何交易所凭据就能完整
验收的那条子项，与上一轮的回测轨同源）。**桌面 Host** 与 **sandbox/production 外部验收**仍未做，
本轮不创建 tag、不触发 GitHub Release。全部加性：门禁 **824 → 848 全绿**，
`GATE_CHECK_FLOOR` **772 → 796**，未删任何既有判据。

- **同源 BFF 本体**（`crates/qx-api/src/console.rs`）：`ConsoleFront` 把静态三件与 API 放在**同一个源**上，
  浏览器不再需要 CORS allowlist（同源请求根本不发 `Origin` 预检），会话与 CSRF 留在服务端。
  - **引导**：运维带 `?token=<bootstrap>` 打开一次页面，服务端校验后签发两枚 cookie——会话 cookie
    （`HttpOnly` + `SameSite=Strict`，页面脚本读不到）与一枚**非 HttpOnly** 的 CSRF cookie（页面读出来
    放进 `X-QX-CSRF` 头，这就是双提交模式）。此后地址栏里不再有凭据。
  - **会话表**：上限 64 条，超出按**签发顺序**退最老的一条（不是按字典序挑一个）；到期即失效。
    会话 id 与 CSRF 由引导令牌经 SHA-1 派生的伪随机函数给出——秘密只有一个来源，轮换因此可解释，
    也不必为这一层引入 RNG 依赖。
  - **两道独立的锁**：非 GET 必须带 `X-QX-CSRF` 且与会话记住的那一枚**逐字符相等**（空头与不等都 403），
    并且过 `origin_matches_host`（带 `Origin` 的请求必须与 `Host` 同源）。任一道都不许放宽成"有头就过"。
  - **身份注入**：`handle_inner` 收到的是**会话里那份 operator**；命令体里的 `operator_id` 不被采信
    （它是审计字段，不是认证）。
  - **回环边界**：`console_bind_is_loopback` 是唯一判据，只允许 `127.0.0.1` / `::1`。这一层没有 TLS、
    没有 mTLS、没有运维审批，绑到外部接口等于把控制面交给任何能连上的人。
  - **连接预算复用** API 那一枚（`DEFAULT_MAX_CONCURRENT_CONNECTIONS`），额度随会话线程一起归还。
- **`qx-cli console [runtime.json]`**（`console_serve.rs` + `console_args.rs`）：读配置 → 过回环判据 →
  从 `api.console.bootstrap_token_env` 点名的环境变量取令牌（**不走命令行、不写进 JSON、不回显进日志**：
  命令行会进 shell 历史、配置文件会进版本库）→ 绑监听 → 印**一次**入口 URL。静态目录按
  「配置所在目录 → 可执行文件同级/上一级 → 构建期源码树」三格找，一格都不中就报错点名找过哪里——
  静默回落一个空目录会让控制台在浏览器里 404，而服务端看起来一切正常。
- **配置面**：`api.console` 段（`bind` / `static_dir` / `operator` / `bootstrap_token_env` /
  `session_ttl_seconds`），边界校验独立成 `crates/qx-runtime/src/runtime_config/console_validation.rs`
  的 `console_boundary`，由 `topology_validation.rs` **fail-closed** 调用：回环、三格非空、TTL 非 0、
  以及 operator 必须在 `api.operators` 里。新模板 `deploy/qianxing.runtime.console.example.json`
  （明文 + 回环）进 `deploy_template_coverage` 的 COVERAGE 表。
- **门禁牙齿（新建 `console_front_check`，十七颗）**：模块在盘与挂载、六个公开常量、`ConsoleConfig::new`
  是唯一构造入口且四类误配当场拒绝、会话 cookie `HttpOnly`+`SameSite=Strict`、CSRF cookie 刻意非
  `HttpOnly`、非 GET 的 CSRF 与同源两道锁、身份由服务端注入、回环单源、令牌只从环境变量、
  `api.console` 段 fail-closed、CLI 派发接线、模板形状、行为用例在盘、**发布身份的产品能力声明由在盘
  代码背书**。`GATE_CHECK_FLOOR` **772 → 789**。
- **Paper 轨（M5' 的本地可验收子项，`tools/paper_acceptance.py` + `maturity/paper_acceptance.yaml`）**：
  跑仓库自己的 Paper venue（进程内撮合），主链 `scheduler -> strategy -> paper-execution -> ledger`
  在两个独立目录各跑一遍 + 同腿重跑，要求主链四段事实面相等、重跑不重复下单、无遗留待执行命令、
  审计链校验通过且走到 `Executed`；子进程环境**主动摘掉** `QX_BINANCE_*` / `QX_CCXT_*` / `QX_OKX_*` /
  `QX_CONSOLE_*`，所以「不需要凭据」是构造出来的，不是碰巧没配。实测
  `OrderSubmitted=1 / Filled=1 / LedgerApplied=4`，`independent_dirs: fact_surface_equal`。
  新建 `paper_track_check` **七颗**：记录六格自述齐全、脚本真有四件比对、主链四段且计数全为正
  （0 笔成交的"通过"是空跑）、记录正文不出现任何 venue 名称且实盘两档仍全 false、终态退场与幂等、
  默认跑仓库那份 paper 模板并摘凭据环境变量。`GATE_CHECK_FLOOR` **789 → 796**。
- **门禁合计**：**824 → 848 项全绿**，`GATE_CHECK_FLOOR` **772 → 796**。
- **反向变异已实测（二十四处，各自单独红）**：console 十七处（`console.rs` 改名 / 摘 `mod` 挂载 /
  资源表降成私有 / 抹掉一条拒绝文案 / 摘会话 cookie 的 `HttpOnly` / 给 CSRF cookie 加上 `HttpOnly` /
  CSRF 校验放宽成"有头就过" / 摘同源校验 / 身份注入改成 `None` / 装配处不过回环判据 /
  配置里出现令牌字面量字段 / 拓扑校验不再调边界校验 / 摘 CLI 派发臂 / 模板改名 / 模板绑 `0.0.0.0` /
  抹掉真套接字端到端用例 / 身份把 `product_csrf` 说成 `False`）+ Paper 七处（结论改 `skipped` /
  抹掉主链比对函数 / 成交数改 0 / 正文写进 venue 名 / 遗留一条待执行命令 / 悄悄换掉默认配置 / 能力档把 Paper 轨指错）。
  脚本 `.audit/console_mutation.py` 与 `.audit/paper_mutation.py` 跑完逐字节还原，24/24 全部打红。
- **顺带修正一处身份含糊**：`release-identity.json` 原用 `csrf_supported=false` /
  `session_permissions_supported=false` / `desktop_host_supported=false` 三个字段，容易被读成
  「**产品**没有 BFF」。现拆成 `package_scope=static-assets-only`（归档自己是什么）与 `product_*` 四格
  （产品现在有什么，`product_same_origin_bff`/`product_csrf`/`product_server_side_session` 为 `true`，
  `product_desktop_host` 仍为 `false`）。**归档仍不是 BFF**；变的是身份不再少报产品能力。
- **阈值漂移同步**（改数量类判据的既定纪律）：新模板使「仓库明文 runtime 模板」由 **17 → 18** 份，
  `api_transport_auth_boundary_doc.rs` 的阈值与模块文档、`deploy/README.md` 的份数说明一并改口；
  `docs/自研量化框架重构方案-V13.md` 那段是 #244 立案时的读数，按仓库惯例**保留不改**并加回写批注。
- **收尾验证**：`cargo check -p qx-cli --all-targets` 零错零警；
  `cargo clippy -p qx-api -p qx-runtime -p qx-cli --all-targets` **零告警**；
  `cargo test -p qx-api` 全绿（lib 48 条，含 console 13 条）；`cargo test -p qx-runtime` 默认特性 57 条 /
  `--features sqlite` 59 条全绿；**`cargo test --workspace --no-fail-fast` 120 个测试目标 / 1214 passed /
  0 failed / 1 ignored**（`WORKSPACE_TEST_FLOOR = 1083`）；`python -m unittest discover -s python/tests`
  用仓库 venv 跑 **68 条 OK**（托管 python 缺 `tzdata`，那 6 条 `test_ashare` 报错是环境噪声）；
  触碰过的 16 个 Rust 文件逐个 `rustfmt --check --config skip_children=true` 干净；
  `python tools/check_architecture.py` **848 项全绿**；`git diff --check` 干净。
- **仍未落地（诚实边界）**：**M4' 未关闭的部分**是桌面 Host 与多机反代形态（那属于部署件，不是代码）；
  **M5' 的 Paper/sandbox/production 外部验收**仍未进行；本轮没有创建 tag，也没有触发 GitHub Release。


### M4'/M5' 续（2026-10-08）· Web 控制台 local-only 边界硬化 + 发布身份状态字段

**口径**：继续把「静态 Web 控制台发布包」的边界说清，并把能本地验证的部分钉进门禁；仍不冒充完整 M4'/M5'。本轮不新增同源 BFF、CSRF、权限会话、桌面 Host，也不触发 GitHub Release。

- **浏览器侧 local-only 入口**：`web/console/app.js` 的连接入口只允许 `http://127.0.0.1` / `http://localhost`；提交 `POST /control/commands` 前再次检查。非本机地址会在健康面板显示「已拒绝」并说明原因：没有同源 BFF、CSRF token 或服务端会话时，静态包不能作为远端控制面。`web/console/index.html` 同步写明这只是本机控制台边界，不是认证授权。
- **发布身份机器可读化**：`tools/package_web_console.py` 的 `release-identity.json` 现在明确登记 `distribution_boundary=local-only`、`requires_same_origin_bff=true`、`csrf_supported=false`、`session_permissions_supported=false`、`desktop_host_supported=false`、`sandbox_accepted=false`、`production_accepted=false`，避免把静态包误读成生产控制台。
- **门禁牙齿**：`release_supply_chain_check` 扩展发布包身份字段判据；`web_console_check` 新增两颗 local-only 判据（连接入口只允许回环、提交前再次检查并给出拒绝路径说明）。`GATE_CHECK_FLOOR` **767 → 772**，实际门禁 **822 → 824 项全绿**。
- **验证**：`python tools/check_architecture.py` 全绿（824 项）；`python -m unittest discover -s python/tests -p test_web_console_package.py -v` 3 条通过；`node --check web/console/app.js` 通过；`git diff --check` 通过。
- **反向变异已实测**：移除 `localhost` 回环白名单会红「连接入口只允许本机回环」；移除提交前二次检查会红「命令提交前再次检查本机回环边界」。还原后复绿。
- **仍未落地（诚实边界）**：同源 BFF、CSRF、权限会话、桌面 Host、Paper/sandbox/production 外部验收仍未完成；本轮没有创建 tag，也没有触发 GitHub Release。


### V13 R23 续（2026-10-08）· 阶段二 P0/P1 逐项落地（native 信任门 / dev 环迁出 / EventLog 单事务批量追加 / 游标增量 refresh / 估值单点 / 错误码五元契约）

**口径**：按「先牙齿后功能」把 `docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md` 阶段二里**可当场关闭**的六项逐条落地，每项都带自己的门禁牙齿 + 反向变异实测。全部加性：门禁 **759 → 793 全绿**，`GATE_CHECK_FLOOR` **703 → 737**，未删任何既有判据。方案文档 §17.2 表格中已关闭的行标 ✅，新增 §17.4 逐项明细。

**续（同日）**：随后回答「P0-1 是否可以先做回测、不做实盘」——**可以，而且那才是本仓的验收边界**。P0-1 的卡点从来是真实交易所凭据（非代码），而 `sandbox_tested` / `production_approved` 两档**只对需要外部 venue 的能力有意义**；本仓主用法是回测 + Paper 闭环，一条凭据都不用。于是把「回测轨」做成**不需要任何凭据就能完整验收**的一条轨：新增 `tools/backtest_acceptance.py` → `maturity/backtest_acceptance.yaml`（**仓库资产**，与未跟踪的实盘证据不同）+ `crates/qx-cli/tests/backtest_acceptance_determinism.rs`（两条行为用例）+ 六颗判据 `backtest_track_check`。门禁 **793 → 799 全绿**，`GATE_CHECK_FLOOR` **737 → 743**。详见下面 P0-1 那一行。

- **P0-3 / DD-4 · in-process 原生策略信任门**（`crates/qx-cli/src/native_trust.rs`）：`admit_in_process_c_abi` + `target_triple_matches`；`load_c_abi_strategy` 在 `dlopen` **之前**先过信任门；配置面暴露 `c_abi_trusted_native` / `c_abi_target_triple`，且开关**缺省为 `false`（默认拒绝）**。牙齿 `native_trust_check` 5 颗（三件在盘且两个判定都非测试专用、`dlopen` 前先过门、配置面暴露、缺省 false）。**759 → 764**，floor **703 → 708**。
- **P1-12 / §8 WP-22 · dev 依赖环迁出**：新建 `crates/contract-tests`（纯测试宿主），把两条压在正常边上的 dev 环（`qx-execution --dev--> qx-runtime`、`qx-risk --dev--> qx-zhenlu`）连同它们的 **5 份跨 crate 契约用例（10 条）**搬出生产 crate。牙齿 `dev_dependency_cycle_check` 3 颗：两条已知环不许回来；全仓不得存在「A --dev/build--> B 且 B --normal--> A」这种二点环（`cargo tree` 默认视图看不见 dev 边，新写的 dev 边踩到任一正常边当场红）；搬家不是丢用例——迁走的五份用例仍在 `crates/contract-tests/tests/` 且该 crate 在 workspace members 里。`EXECUTION_TEST_FLOOR` **25 → 15**（本地板只覆盖 `src/tests` 的 15 条，全仓地板把新 crate 一并计入、总数不变）。**764 → 767**，floor **708 → 711**。
- **P1-1 / DD-2 / §7 M2 · EventLog 写侧单事务批量追加**：`EventLog::append_batch(&mut self, batch: &[Event])`（`append_checked` 委托它，逐条校验规则单源）+ `digest_of_prefix()`（全量摘要与前缀摘要同源，增量核对才成立）+ `SqliteEventLogStore::append_batch`（单事务里走增量路径，**不再 `load_event_log` 读回整条历史**——此前写放大随 N 线性增长，G4）+ 生产写面的 SQLite 分支真的调它。牙齿 `event_log_append_batch_check` 6 颗；原子性（Kernel）与增量性（SQLite）各有一条行为用例在盘。**767 → 773**，floor **711 → 717**。
- **P1-2 / §7 M2 · 游标增量 refresh（G5 收敛）**：`SqliteEventLogStore::read_since`（`WHERE name = ?1 AND seq > ?2 ORDER BY position ASC`，只读 manifest 校验前缀，O(1)）+ `RuntimeEventStore::read_since` 委派 SQLite、其余后端**显式**回落 `Ok(None)` + `LiveEventPipeline::refresh_latest` 三支（空尾部 no-op / 前缀一致只接尾部 / 前缀不符或后端无行级读则整份重建）+ `apply_tail` 走 P1-1 的 `append_batch` 且与整份重建共用 `apply_index_event`（归约单源）。牙齿 `pipeline_cursor_refresh_check` 6 颗。**773 → 779**，floor **717 → 723**。
  - **方法论要点**：增量与整份重建的**终态完全一致**，纯行为断言（订单状态、账簿条数）抓不到退化；牙齿因此同时钉住两个**内部计数器** `tail_appends` / `rebuilds`（不进运维快照 `PipelineMetricsSnapshot`，只由本 crate 用例读——仓库既有的「没有生产读者但被回归用例钉住」那一类）。**反向变异实测**：把 `Some(tail) => apply_tail` 改成 `rebuild_from_store`，门禁红「refresh_latest 分支不全」+ 用例红（`tail_appends` 0≠1）；还原复绿。
- **P1-6 / WP-19 · 估值单点**：`crates/qx-core/src/valuation.rs` 的 `ValuationContext` / `ValuationResult`；`Ledger::valuate` **单点派发**三把参数化尺子（现货按乘数 / 衍生按合约规格 / 跨币种再叠 FX），`MarginState::valuate` 共用结果形状（`available` 关系单源）；`qx-xingban` 回测两处手工派发（`equity_for`、订单簿回测）与 `qx-cli` paper 保证金派发全部改走它。牙齿 `valuation_single_source_check` 6 颗，含一条**生产源码全文扫描**：三把参数化尺子只能出现在定义点（`ledger/query.rs`）与估值单点——取 `production_text` 剥掉注释与测试项，否则"文档里提一句 / 用例里直接调原语"会让"还有谁在直接派发"数不准。**779 → 785**，floor **723 → 729**。**反向变异实测**：还原回测里的手工派发 → 红 2 条；还原 `MarginState::equity` 的内联算式 → 红 1 条。
- **P1-11 / DD-5 · 错误码五元契约**（`crates/qx-core/src/error.rs`）：`ErrorCode` **闭集**（8 码，`ALL` 可遍历，`as_str`/`parse`/`Display`/Serde）+ `Retryability` 四档（`Never`/`Allowed`/`AfterReconcile`/`AfterBackoff`，刻意**不是布尔**）+ `ErrorContract` 五格（`code`/`retryability`/`reconcile_required`/`safe_to_retry`/`user_message`）+ `QxError::contract()` **全仓唯一**映射表（`code()` 从它派生，自身不再 `match self`）。消费侧三处接上：`usage_errors::qx_context` 按 `reconcile_required` / `allows_retry` / `may_retry_eventually` 三档提示；`runtime_wiring/pipeline_storage.rs` 四处 `QxError`→字符串边界不再摊成 `{error:?}` 自由文本；`qx-runtime/src/pipeline.rs` 两处热路径重试循环改按 `retryability` 分支。牙齿 `error_code_contract_check` 8 颗。**785 → 793**，floor **729 → 737**。
  - **反向变异实测（三处，各自单独红）**：① `code()` 不再从 `contract()` 派生 → 红「唯一映射表 / 两份口径」；② `pipeline_storage.rs` 退回 `format!("…: {error:?}")` → 红「只有 3 处走 qx_context / 仍有 {error:?}」；③ 重试循环退回 `matches!(error, QxError::Transient(_))` → 红「重试循环没有消费契约」。还原后复绿 **793**。
  - **刻意不纳入**：HTTP/工作流层面的状态串（`qx-api` 的 status、`qx-control` 的 `CommandStatus`）。它们是各自的读面，不是"错误分类"；塞进 `ErrorCode` 会把闭集撑成开放集，"一型一码"那条判据当场数不准。
- **本轮另修两处真实缺陷**：
  - `crates/qx-runtime/src/pipeline.rs` 的 sqlite 游标用例缺 `#[cfg(feature = "sqlite")]` 门 —— 默认特性下 **`cargo test -p qx-runtime` 编译不过**（`open_sqlite` 不存在）。
  - `crates/qx-cli/tests/unit/worker_pipe_failure_diagnostics.rs::live_but_non_draining_worker_write_is_bounded_not_hanging` 断言的是**已不存在的文案**（`输入超时` / `worker 存活但不接收输入`）——R17 把三处子进程 stdin 写入统一到 `qx-adapter::io_budget::write_all_within` 之后，写侧超时只报「未在 N ms 预算内写完」，这条**回归面从那时起就是永久红的**（全仓 grep 该文案 0 命中）。改按新口径断言（走预算通道 + 交代 `程序=`/进程状态），保留原有的「耗时 < 5× timeout」有界性断言。
  - 顺带修掉 P0-3 引入的 `clippy::field_reassign_with_default`（`native_trust.rs` 的测试改用结构体字面量 + `..Default::default()`，保住那条 fail-closed 梯子的可读性）。
- **收尾验证**：`cargo fmt --all --check` rc 0；`cargo clippy -p qx-core -p qx-runtime -p qx-cli --all-targets` **零告警**；`cargo test -p qx-core` 全绿（60 + 2 + 4 + 25 + 6）；`cargo test -p qx-runtime`（默认特性 56 条、`--features sqlite` 58 条）全绿；`cargo test -p qx-cli --no-fail-fast` **441 passed / 0 failed**（bin 单元 363 + 16 个集成目标 78，含本轮新增的 `error_code_contract` 三条）；**`cargo test --workspace --no-fail-fast` 118 个测试目标 / 1193 passed / 0 failed / 1 ignored**（`WORKSPACE_TEST_FLOOR = 1083`）。行数棘轮：`crates/qx-runtime/src/pipeline.rs` 停在预算线上 **2480/2480**（靠把新增的说明性注释并句腾出行数，不升预算）。
- **P0-1 / §6.5 · 回测轨做成「无凭据可完整验收」的一条轨**：实盘轨的 `sandbox_tested` / `production_approved` 只能由 `maturity/evidence/` 下 `outcome=passed` 的记录翻真，而那份 testnet 记录是 `outcome:"skipped"`（缺 `QX_BINANCE_TESTNET_API_KEY` / `_SECRET`）——卡点是**凭据不是代码**。关键是那两档**只对需要外部 venue 的能力有意义**：本仓主用法是回测 + Paper 闭环，因此新增 **`maturity/capabilities.yaml` 的 `backtest_only` 档**（`credentials_required: false` / `external_venues: none` / `approval_scope` 明写那两档**不适用**）并设为 `default_profile`；那条轨的验收边界是 **`maturity/backtest_acceptance.yaml`**（**仓库资产**，与未跟踪的实盘证据不同），由 **`tools/backtest_acceptance.py`** 生成——两个独立目录各跑 `quickstart` + **同目录两轮 `backtest`**，要求：① 同目录重跑**逐格相等**（含产物文件名后缀与 `config_fingerprint`）；② 跨目录只要求 `result_hash` / `data_fingerprint` / `replay_verdict` 与**归一化后**的产物内容相等，`equity.csv` / `fills.csv` **逐字节相等**；③ `replay_verdict: verified`；④ `network_accessed` / `orders_sent` 双 `false`；⑤ 子进程环境**摘掉** `QX_BINANCE_*` / `QX_CCXT_*` / `QX_OKX_*`。归一化只抹三样：**绝对路径**（三种形态）、内容寻址的 `config_hash`、**由它派生的产物文件名后缀**——这正是 §46「artifact digest vs result_hash」那条区分：`result` 必须与路径无关，`artifact identity` 不必。实测 `result_hash=26fdd6b52d020700` 跨目录稳定。牙齿 `backtest_track_check` **6 颗**：① 默认档是 `backtest_only` 且声明「不需要凭据 / 无外部 venue / 那两档不适用」；② 记录在盘且六格自述齐全；③ 记录由在盘脚本生成、且脚本真有 `compare_reruns` / `compare_independent` / `one_leg` 三件事；④ `result_hash` 是 16 位十六进制且四类产物摘要齐全；⑤ 记录正文**不出现任何 venue 名称**（先剥 `#` 注释再判，免得「实盘证据在哪」那句散文被当成越界声明），且实盘两档仍全 `false`；⑥ 行为用例在盘。**793 → 799**，floor **737 → 743**。刻意**不做**「记录是否过时」判据——`generated_at_unix` 每跑一次都变，拿它当判据只会逼人写死时间戳。
  - 顺带修一处**文档失实**：`maturity/evidence/README.md` 声称「`maturity/evidence/**` 不进版本控制」，而 `git ls-files maturity/` 明明列得到 `README.md` 与 `testnet/20260920T005856Z-dryrun/` 下两份记录。README 改口到实测现状，并新增「两条轨别混读」的开篇说明。
- **仍未落地（诚实登记）**：**M4' 完整生产化**（桌面 Host/本地代理；Web 同源 BFF/CSRF/权限会话等）与 **M5' 外部验收**（Paper/sandbox/production）；本轮只补了静态 Web 控制台可校验发布包，不冒充完整 M4'/M5'。**P0-1 的实盘轨**仍待真实交易所凭据（非代码），其**回测轨已完整验收**。M1（`qx-core::contract` 稳定契约单点）/ M3'（控制面控制台）/ P2-2（开源 HTTP 库技术可行、当前暂不迁库，见 §17.6）本轮均已关闭。


- **§6.4 P2-2 · 自研 HTTP/WS 面评估：保持自研，缺口钉进门禁**：用户问「不要自研 HTTP、直接上开源成熟库是否可行」。结论：**技术上可行，但当前不建议迁库；自研面缺口尚未证明不可接受**。先落地 `maturity/http_surface.yaml`（自研 HTTP/WS 面逐格覆盖表，21 格：13 covered / 4 partial / 3 gap / 1 by_design），每格写清「由哪个符号负责、哪条用例守着、覆盖到什么程度」——缺口是数出来的，不是估出来的。配套 `tools/check_architecture.py` 新增 `http_surface_check`（七颗）：`self_built_file` 全部在盘、每行 `anchor` 符号真在登记文件里声明、每行 `case` 用例真在测试文件里、`verdict.decision=keep_self_built`、`accepted_gaps` 恰好等于「非 covered 行」、`migration_trigger` 在场。门禁 **813 → 819 全绿**，`GATE_CHECK_FLOOR` **757 → 764**。
  - **判定口径**：三格 gap（chunked 请求体 / 重复 Content-Length 该拒 / ping/pong 保活）全部落在「本仓不用的 HTTP 特性」上，且 fail closed；四格 partial 靠间接用例覆盖，已在表内逐格登记欠账。按 §9 排期原则 3「零新依赖」，先登记、后评估；迁库触发条件（`migration_trigger`）三条：需要本仓读面真正要用的 HTTP 特性 / 需要把 API 暴露给不受控客户端 / WS 需要双向载荷。
  - 顺带修一处**文档失实**：覆盖表 `reason` 原写「20 格 / 2 格 gap」，实际 21 格 / 3 格 gap（漏数 `ws_ping_pong`），已改口。
  - `maturity/capabilities.yaml` 新增能力 `qx_api_self_built_http_ws`（impl/code_tested 双 true，limitations 登记 keep-self-built 决策与 accepted_gaps），把「不自研改迁」的判定也接进门禁能核的登记面。
- **M4'/M5' 续 · Web 控制台版本化发布包**：新增 `tools/package_web_console.py`，只打包 `index.html`/`app.js`/`styles.css`，校验 HTML 只引用包内资源、拒绝外链/漏打资源；归档内含版本、完整 commit、Schema Registry 版本与逐文件 SHA256 的 `release-identity.json`，固定 gzip/tar 元数据，重跑逐字节一致。`python/tests/test_web_console_package.py` 三条用例覆盖可复现/身份与摘要、外链或漏打资源 fail closed、版本与提交身份校验。`.github/workflows/release.yml` 新增 `web-console` job：生成归档、attest provenance、上传 artifact；Release 依赖此 job，统一的 `SHA256SUMS` 覆盖该归档。`release_supply_chain_check` 新增三颗判据；`GATE_CHECK_FLOOR` **764 → 767**，实际门禁 **819 → 822 项全绿**（本轮复跑通过）。`deploy/README.md` 增本地重建办法并明确安全边界：静态包不是同源 BFF，没有会话/CSRF/权限代理，不可单独暴露公网或充作生产交易控制台。**只关闭了静态交付/打包子项，M4' 同源 BFF/CSRF 与 M5' 外部 Paper/sandbox/production 验收仍未完成**；未触发 GitHub Release。

### V13 R23 收口（2026-10-08）· 方案落地度核验 + 阶段四 M1'/M2' 只读 Web 控制台 + 文档口径回写

**口径**：用户问「是否已按新版方案（`docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md`）全部落地」。逐条把方案的每个工作包对到实际代码/门禁，结论是**未全部落地**，并按「先牙齿后功能」推进了可当场关闭的最高价值一项（阶段四只读控制台）。全部加性：门禁 **754 → 759 全绿**，未删任何既有判据。

- **方案落地度核验（结论：未全部落地）**：
  - **已落地**：**阶段一 15 族**（fam01–fam15 + fam16 手续费币种）全部有判据（`file_lock_single_source_check` / `io_budget_and_venue_cache_check` / `scheduler_owner_routing_check` / `environment_production_single_source_check` / `ops_read_surface_check` / `ci_feature_matrix_check` / `kernel_timeline_check` / `fee_settlement_currency_check` / `cli_surface_coverage_check` / `bounded_growth_and_reap_check` / `control_plane_honesty_check` / `storage_retry_check` / `strategy_input_schema_check` / `paper_check_delta_honesty_check` / `dead_public_entry_check`）；**M0 基线冻结**（`baseline_freeze_check` + `maturity/baseline.yaml`）；**G6 性能基线**（`performance_baseline_check` + `benchmarks/run_baseline.py` + `results/baseline.json`）；**P2-4 发布供应链**（`release_supply_chain_check` + `.github/workflows/release.yml`，SBOM/provenance/SHA256）；**M0 证据格式**（`maturity/evidence/README.md` + `evidence/testnet/…-dryrun`）。
  - **未落地（诚实登记）**：**P0-1 外部证据**（`sandbox_tested:true` = 0、`production_approved:true` = 0，唯一 testnet 记录是 dry-run——卡在真实交易所凭据，不是代码）；**P0-2** 文件后端单事务（无 `append_batch`）；**P0-3** native 信任根（有 Ed25519 签名校验，无 `trusted_native` profile）；**P1-1/P1-2**（无 `append_batch` / Pipeline 仍 clone 整体状态）；**P1-6** 无 `ValuationContext`；**P1-11** 错误码仍是 `Option<String>`；**P1-12** dev 依赖环仍在（`qx-adapter`/`qx-execution`/`qx-risk`，Cargo 合法但按方案要迁 contract-tests）；**M1** 无 `qx-contract` crate；**阶段四产品轨** 此前无任何前端（本轮补 M1'/M2'）；**P2-2** 仍自研 HTTP。
- **阶段四 M1'/M2' 落地：只读 Web 控制台**（`web/console/{index.html,app.js,styles.css}`）：消费 `qx-api` 的 `GET` 读面（`/health`、`/ready`、`/account/snapshot`、`/account/balances`、`/account/orders`、`/account/positions`、`/account/ledger`、`/reconcile/reports`、`/scheduler/runs`、`/control/audit`、`/events?after=`）与 WebSocket 增量；**不发送订单、不调用 `POST /control/commands`**。未算出的钱如实显示为 `—`（`null`），不折成 0。页面顶部印出 CORS 要求（`api.cors_allowed_origins`）与 `qx-cli serve` 启动入口。对应路线图 §18「不应把 HTTP/WS API 自动称为前后端贯通」——贯通要有一条**可核对的接线表**。
- **门禁牙齿（5 颗，新建 `web_console_check`）**：控制台三件在盘；`app.js` 点名的每个 API 路径都在 `qx-api` 路由表里（前端引用后端不存在的端点当场红）；页面写明 `cors_allowed_origins` 要求；写明 `qx-cli serve` 入口；控制台代码不得出现 `POST` 或 `/control/commands`（先剥注释再判，与门禁别处同口径）。`GATE_CHECK_FLOOR` **698 → 703**。
- **反向变异已实测**：往 `app.js` 的接线表塞一个 `/nonexistent/endpoint`，门禁当场红「控制台引用了后端不存在的路径」；还原后复绿 **759**。
- **端到端实测**（`quickstart` 出配置 → `qx-cli serve` 起服务 → curl）：控制台点名的 12 条端点全部按语义应答（`/health` 200、`/account/balances|orders|positions|ledger` 200、`/reconcile/reports|scheduler/runs|control/audit` 200、`/account/snapshot` 404 无快照、`/ready` 503 未就绪、`/events?after=0` 409 需先取快照、`/metrics` 200——404/409/503 是语义分支不是"路由不存在"）；配了 `cors_allowed_origins:["http://127.0.0.1:5173"]` 后，预检 `OPTIONS` 回 **204**、`GET` 带 `Vary: Origin` + `Access-Control-Allow-Origin: http://127.0.0.1:5173`。
- **文档口径回写（消除文档-代码漂移）**：方案文档 §3.13/§4.1/§4.2/附录 B/附录 C 的读数从成文时的「662 门禁 / 103 个 `*_check()` / 572 处断言 / `GATE_CHECK_FLOOR=633` / 20 条能力 17/17/0/0 / 44 份超线 / CI 7 job」回写到实测现状「**754（本轮 759）门禁 / 127 个 `*_check()` / 678 处 `check(...)` / floor 698（本轮 703）/ 24 条能力 20/20/0/0 / 45 份超线 / CI 12 job**」；§12.2「阶段一 15 族中的 14 族未落地」加 2026-10-08 回写批注（已被 R8–R22 推翻）；§12–§16 的历史读数保留不回改。`deploy/README.md` 增「只读 Web 控制台」一节，`README.md` 模块表增 `web/console/` 一行。
- **收尾**：门禁 **759 项全绿**；本轮改动为纯加性（新增 `web/` 目录、`web_console_check`、文档回写），未触碰任何 Rust 源与既有判据。


### V13 R22 收口（2026-10-08）· 第 22 轮三扫 · 门禁 `TEST_PATH` 的单文件测试盲区（#222）+ 发布条件复核

**口径**：本轮按 `docs/archive/qianxing_综合优化改进系统方案_竞品架构实现路线图_2026-10-08.md` 的 §43（孤儿）/ §44（死配置键）/ §45（配置不生效）/ §46（确定性）/ §47（重放）/ §67（验收）逐条过一遍。三扫结论：**A 连通性与前后端贯通 0 颗、C 终止性与无界 0 颗**；B 孤儿面命中一条**门禁自身的盲区**（`#222`，V13 R2 第十六遍立案、本遍落地）。全部加性：门禁 **753 → 754 全绿**，未删任何既有判据。

- **`#222` 落地：`TEST_PATH` 认不出单文件形态的 `src/tests.rs`**。`tools/check_architecture.py` 的 `TEST_PATH` 只认目录式 `src/tests/`（含 `tests/mod.rs`）与 `_tests.rs`，不认单文件式 `src/tests.rs`——而 V12 R2 为绕开行数棘轮，把八个 crate 的库内用例搬成了后者（`#[cfg(test)] mod tests;` 挂进来）。于是「唯一读者写在 `src/tests.rs` 里」的 `pub` 项被当成有生产读者，**零读者判据在它们面前是瞎的**；同一个过滤器还罩着 `PUBLIC_ENTRY_FAMILIES`（能力族入口清点）与枚举变体生产者两条判据，所以这是一条会同时松开三处前提的盲区。
  - **修法（两种形态定成一个口径）**：`TEST_PATH` 补 `(?:^|/)tests\.rs$`，目录式 `src/tests/` 与单文件式 `src/tests.rs` 都认得。
  - **逐模块清点（放宽后放出的 8 条零读者）**：`qx-datastruct::{close_at, from_view, resample_with_manifest, select_time_with_manifest}`、`qx-protocol::{from_wire_json, to_wire_json}`、`qx-xingban::{corporate_action_supported_by_ledger, corporate_actions_from_data}`——共同形状是**生产链路走 JSON/字符串入口**（`BarFrame::from_json`、`apply_corporate_actions_json`），这些类型化的程序化入口只有库内用例读者；按「保留 + 登记 + 双向钉住」逐条进 `PUBLIC_SURFACE_ALLOWLIST` 并写明理由（含 `qx-protocol` 两条的跨语言 wire 契约、`qx-datastruct` 两条的 Python 桥镜像实现、`qx-xingban` 两条对应的 `ashare_corporate_action_ledger` 台账）。
  - **与立案时的 10 条对账**：`#222` 立案记的是 10 条，本遍实测 8 条。差额两条已各自销案——`qx-protocol::to_qifi` 在 `crates/qx-cli/src/ecosystem_smoke.rs:208` 长出生产读者；`qx-runtime::to_json_for` 已改成 `#[cfg(test)] pub(crate) fn` 并搬到 `strategy_contract/contract.rs`（被 `_strip_cfg_test_items` 整项剥离），故不再在列。
  - **门禁牙齿（1 颗，住在 `zero_reference_public_surface_check`）**：`TEST_PATH` 必须同时认得目录式与单文件式，且不得把 `contest.rs`/`attestation.rs` 这类含 test 词干的生产文件误判。`GATE_CHECK_FLOOR` **697 → 698**。
  - **反向变异已实测**：把 `TEST_PATH` 改回旧写法（去掉 `(?:^|/)tests\.rs$`），门禁当场红 **2 条**——新牙齿（「TEST_PATH 认得测试模块的两种形态」）与「允许清单里的条目仍然没有生产读者」（8 条条目变过期）；还原后复绿 **754**。
- **发布条件复核（§67 逐条 + §46 实测）**：
  - **门禁**：753 → **754 项全绿**；**`cargo fmt --all --check`** rc 0；**`cargo clippy --workspace --all-targets -- -D warnings`** rc 0。
  - **`cargo test --workspace`**：114 个测试目标 / **1169 passed / 3 failed / 1 ignored**，3 条全是**环境相关**——2 条 `rust_invokes_python_*`（`QX_PYTHON` 未设置、回落 PATH `python` 被沙箱拒绝，`os error 5`）与 1 条已知 `os error 231` 管道噪声；把 `QX_PYTHON` 指到 `python/.venv/Scripts/python.exe` 后前两条当场转绿。
  - **§46 确定性实测**：同输入两次 `quickstart`，`result_hash=26fdd6b52d020700`、`input_data_hash`、`replay.log_digest`、全部 metrics、`fills.csv`、`equity.csv` **逐字节相等**；只有路径派生的 run_id 与 manifest 路径不同（正是 §46「artifact digest vs result_hash」那条区分）。`config fingerprint` 实测**内容寻址**：不同目录下逐字节相同的配置得同一指纹（`417a32e4…`）。
  - **§45 配置不生效**：`builtin_fast_window/slow_window/period/threshold_bps` 对 MACD 恒不生效（实测改这四项 `result_hash` 不变）——这是**已登记**的 limitation（`declared_unused_knobs_do_not_fail_the_run`，V12 #102 §14.5），且系统在 stdout 与摘要 `signal.declared_unused` 两处如实报告。
  - **§47 重放**：`ReplayVerifier::verify` 在写任何工件之前跑（`replay_kernel_check` 看守），不是独立 `replay` 命令但满足意图。
  - **§43 UNFED / §44 死配置键 / 终止性**：`RuntimeConfig` 全字段有生产读者（§44）；生产代码无 `while true`，`loop {` 均有出口或预算（`*_retry_*` / `stop_aware_worker_tick_sleep_check` / `api_accept_loop_exit_check` / `bounded_growth_and_reap_check` 覆盖，§43 无界方向 0 颗）。
- **环境发现（写给下一位跑测试的人）**：本沙箱里 `%TEMP%` 对**被测子进程**不可写，`cargo test -p qx-cli` 会有一片 `os error 5`（拒绝访问）**假失败**（实测 70 条，全部停在 `crates/qx-cli/tests/unit/mod.rs:54` 的 `create_dir_all`）；把 `TEMP`/`TMP` 指到工作区内的目录（`export TEMP=$PWD/.tmp-audit/tmp TMP=$PWD/.tmp-audit/tmp`）后降到 3 条已知噪声。**收尾跑测试必须先设 `TEMP`**，否则会把环境问题误读成回归。


### 报告导出安全与落点增强（2026-10-06）· 易用性 P3 收口

- **HTML 文本转义**：报告会显示摘要中的策略名、标的名、输入路径等用户数据；这些文本现统一转义 `& < > \" '`，避免被当作 HTML 标签或资源属性解析。新增恶意标签与带 URL 文本夹具，断言产物只呈现文本，不产生活标签。
- **完整图表导出**：除自包含 HTML 外，默认并列写 `<stem>.equity.svg` / `<stem>.fills.svg` / `<stem>.monthly.svg`；自选 HTML 落点 `--output <path>` / `-o <path>` 会为 HTML 与三张 SVG 共用输出前缀，并自动创建父目录。输出仅接受 `.html`/`.htm`；不带 `--html` 的 `--output` 会 fail-closed。`run report` 统一入口同样支持这些参数。
- **模块边界**：把摘要校验后的报告命令处理抽到 `report_command.rs`，让 `config_commands.rs` 回到预算内；报告落盘仍只有一个生产调用点，门禁同步跟踪该唯一出口。
- **机读输出纪律**：`--json --html` 不再把 `[Report]` 人类提示混进 stdout；JSON 的 `generated_artifacts` 列出 HTML 与 SVG 的实际路径，stdout 可直接解析。
- **本轮验证**：架构门禁 682 项全绿；`cargo fmt -p qx-cli --check` 与 `cargo clippy -p qx-cli --all-targets --offline -- -D warnings` 均通过；报告用例 10 条通过（含 JSON stdout 解析、直接 CLI 参数解析、自定义输出与 HTML 转义）；统一 `run report` 导出路径用例通过。全量 qx-cli 为 323 passed / 4 failed，4 条均是已知 Windows 沙箱 `os error 231` 子进程管道限制；无其他失败。


### 结果可读性层落地（2026-10-06）· 竞品对比 §6 P3「看不见」补上

**口径**：`docs/竞品对比与易用性改进优化计划-V1.md` §1 把最大的产品差距概括为三个词，第一个就是**「看不见」**——竞品默认交付一张标注买卖点的图或一份交互式 HTML，而我们只落 `*.equity.csv` / `*.fills.csv` / `summary.json`，把「读结果」整段留给了用户（`plot_/chart/.html` 在 `crates` 下 0 命中）。这一轮把那一格补上：`qx-cli report --html` 产出一份自包含 HTML 报告。全部加性：门禁 **676 → 682 全绿**，未删任何既有判据。

- **无依赖 SVG 生成器 `crates/qx-cli/src/report_svg.rs`**：三张图——净值曲线 + 最大回撤区间、成交点（▲ 买红 / ▼ 卖绿，中国市场口径）、月度收益热力表。为保住「单 exe + 运行期零依赖」这条相对 Python 栈的真实优势（竞品对比 §4.3 第 4 条），**不引任何绘图 crate**：只做「定点数 → 坐标 → 拼字符串」。内联 SVG **不带 `xmlns`**——HTML5 内联不需要它，带它就等于往产物里塞一个外链前缀字样。空序列画「无数据」占位而不是空白；0 成交**不画任何三角**（假买卖点比没有图更糟）。
- **自包含 HTML 报告 `crates/qx-cli/src/report_html.rs`**：指标卡 + 三张内嵌 SVG + 产物身份表 + 「未连接真实交易所」声明。两条纪律：① **不新增第二个指标算式来源**——卡片每个数都从 `summary.json` 已有格子读，曲线/成交点只从两份 csv 派生，月度收益由净值序列按自然月归并；② **确定性**——不用墙钟、不用随机、不遍历 HashMap，同输入两次生成**逐字节相等**（用例钉住）。产物无任何外链（无脚本/图片/外链样式），可离线双击打开。
- **命令面 `report --html`**：`cli_args.rs` 的 `report` 新增 `--html` 旗标（与 `--json` 正交，两者可同时给）；`config_commands.rs` 的 `run_report` 在**复核摘要之后**调用渲染，落 `<stem>.report.html`；统一入口 `run report --html` 与直接入口**同形状**（先摘出旗标再走通用参数读法）。`cli_help.rs` 同步印出形状。
  - **行数棘轮**：`cli.rs`(673)、`cli_args.rs`(513)、`config_commands.rs`(539) 都在预算顶。三个文件都做了**净零/净负**的行数腾挪（压缩既有注释、一处 `if empty {1} else {len}` → `len().max(1)`），没有动 `line_budgets.yaml` 的任何一个数。
- **门禁牙齿 `report_readability_check()`（5 颗）**：① 两个模块在盘；② 在单文件行数门槛内；③ 在 crate 根 `mod` + `pub(crate) use` 成对挂载；④ 模板（去注释行后）**无外部资源 token**（命名空间声明 / 外链前缀 / `<script` / `<img` / `<link ` / `<iframe` 出现即红）；⑤ 保留「未连接真实交易所」声明 + 落盘调用点唯一。**反向变异已实测**：注释掉 `mod report_svg;` 判据当场变红并点名 `缺配对 ['report_svg']`，随后还原复绿 682。
- **用例 `crates/qx-cli/tests/unit/report_html.rs`（7 条）**：同输入两次逐字节相等 / 无外链 token / 0 成交不画买卖点且每笔成交一个标记 / 卡片数值等于 summary 字段 / 坏 CSV 当场拒绝（走 `write_report_html` 真实落盘路径）/ epoch 毫秒到自然月映射。
- **顺带修一处显示口径**：回撤是「幅度」不是「方向」，卡片不带正号、为 0 时不染绿（否则「没回撤」看着像跌）。
- **本轮实测**：门禁 **682 项全绿 / 0 红**；`cargo fmt -p qx-cli --check` rc 0；`cargo clippy -p qx-cli --all-targets -- -D warnings` rc 0；`crates/qx-cli/tests/unit/report_html.rs` **7 passed**；真机跑通 `qx-cli report <summary> --html`（v1 与 v4 摘要各跑一次，生成 ~8 KB HTML，`http` 出现 0 次）。`deploy/data/qianxing-ashare/runs/` 下的示例产物集同步补一份 `.report.html`，与同一次运行的 `.summary.json`/`.equity.csv`/`.fills.csv` 并列。
- **未落地（诚实边界，已进 capabilities limitations）**：Sharpe/Sortino/Calmar/胜率/盈亏比尚未进摘要，故报告里也没有（不新增第二个算式来源，补它们要先在内核定唯一算式）；`--html` 是显式旗标，`quickstart` 首跑链默认不产报告（并入会改动既有产物份数不变量）；Grafana dashboard JSON 本轮未做（deploy 顶层 JSON 有登记表判据，新增模板要同步登记）。


### 契约层与登记层全量落地（2026-10-06）· 规划 §13.2 / §14.3 / §17 / §18 M0 / §6.2

**口径**：上一轮（`f424b6e`）落地了 §6.2 的七类地基对象；这一轮把它们**钉住**——用真实契约、真实场景、真实依赖规则和真实目标台账。范围是规划里**不需要外部凭据**的全部可本地落地项（M0 剩余 + 各阶段依赖的登记层）。全部加性：门禁 **667 → 676 全绿**，未删任何既有判据。

- **策略契约补齐输入方向（§18 M0 / §4.2 第 8 条）**：新增 `schemas/strategy-api-input-v1.json`，与既有 output schema 合成双向机器可读定义，消除「Rust 有结构、输入方向靠文档」这个点名缺口。`crates/qx-runtime/tests/strategy_contract_input_schema.rs` 五条用例把它与 `StrategyContractInput`/`StrategyContractBars` 逐键对齐：键全集、required（**删键看 serde 认不认**）、`additionalProperties`、定点值只走 JSON 整数、版本常量同源。
  - **用例当场抓出一处真实漂移**：我最初把 `available_margin_raw` 写进 `required`，但它是 `Option<i128>`——serde 对 `Option` 字段的缺省语义就是 `None`，删掉这个键 serde 照样认。schema 已按实测口径改回，描述里写明「这一条不是照抄字段表，而是被删键实验钉住的」。
- **七场景最小 fixture（§18 M0）**：`maturity/fixtures/scenarios/` 下 A 股 / 国内期货 / 国内期权 / 国际股票 / FX / 加密现货 / 加密永续 各一份，每份四块身份（`instrument_spec` / `dataset` / `experiment` / `run`）。`crates/qx-spec/tests/scenario_fixtures.rs` 七条用例把四块分别读进 `TradingInstrumentSpec` / `DatasetManifestV2` / `ExperimentSpec` / `RunRecord` 并逐块校验，另钉住五种产品类型都有样板。
- **补 `TradingProduct::Option`（§15.1）**：规划的产品类型表把 `option` 单列一格，而枚举里没有。已补上（刻意不计入 `is_derivative()`：衍生品那条分支要求 `linear`/`inverse` 恰好指定一个，而期权两者都为 false 才对），市场规格解析器同步接受 `option`/`options`。合约身份真正缺的几格（行权价/到期日/认购认沽/行权方式/组合保证金）已登记为 limitation。
- **`maturity/targets.yaml`（§17）**：工业级非功能目标台账，九个场景 profile × 十个维度，以 profile / 场景 / 硬件 / 版本 / 证据路径为键。牙齿是**诚实规则**：`measurement_status: measured` 必须有真实存在的证据路径，`unmeasured` 必须恰为 `unavailable`——「目标值不等于当前实现现状」从一句话变成会红的判据。
- **`maturity/levels.yaml`（§13.2）**：能力等级登记册，markets / venues / order_types / strategy_languages 四类主体各自登记 L0–L4。牙齿：`capabilities.yaml` 里 `sandbox_tested: true` 的条数为 0 时，**任何主体都不许声明 L3+**——「先把等级调高」这种改字行为当场变红。当前最高只到 L2。
- **`maturity/schema-registry.json`（§6.2 / M6）**：模式登记册实例，覆盖 `schemas/` 下全部 10 份契约，每份写清 id / 版本 / 生产方 / 消费方 / 兼容级别 / golden fixture；判据核对「无重复登记、无漏登记、指针全部落地」。
- **`layer_dependency_check`（§14.3）**：把文档里的**十条领域依赖规则**变成会红的判据——逐 crate 解析 `Cargo.toml` 内部依赖，六条禁止边 + 三条反向规则（`qx-adapter` 只能被 `qx-execution`/`qx-cli` 依赖 = 唯一转换层；`qx-api` 只能被 `qx-cli` 依赖 = 投影面；`qx-cli` 不得被任何 crate 反向依赖 = 叶子）。规则以**当前真实依赖图为基线**，不是「希望如此」。
  - **反向变异已实测**：临时给 `qx-data` 加一条 `qx-api` 依赖，判据当场变红并**同时点名 §14.3-2 与 §14.3-7**，随后已还原（`git diff` 为空）。
- **本轮实测**：`python tools/check_architecture.py` **676 项全绿 / 0 红**；`cargo fmt --check` rc 0；`clippy -p qx-core -p qx-cli -p qx-runtime -p qx-spec -p qx-data --all-targets -- -D warnings` **rc 0**；`qx-spec` **10+7 passed**、`qx-runtime` 输入 schema **5 passed**、`qx-core` **70 passed**、`qx-data` **30 passed**、`qx-cli --bin qx-cli` **311 passed / 4 failed**（4 条全是既有沙箱 `os error 231` 管道噪声，非代码面）。
- **未落地（诚实边界）**：规划的 P0（真实 testnet evidence）、P4（adapter conformance / SDK 兼容矩阵）、P5（application service 抽取 / EventLog 增量 replay / 基准套件）与 M2–M6 的功能面仍需外部凭据与多周工作量，本轮不碰；`qx plan` 用户级读入命令仍受 `cli.rs`/`cli_args.rs` 行数棘轮限制（上一轮已登记）。


### 地基规格对象落地（2026-10-06）· 规划 §6.2 七类「统一身份」声明式文档

**口径**：按 `docs/qianxing-项目结构与GitHub竞品对比及优化方案-2026-10-06.md` §6.2 / §7 落地七类地基对象，本轮只做**声明式文档 + 门禁牙齿**，不接实盘功能（功能面留后续轮次）。全部改动加性：门禁 **662 → 667 全绿**，未删任何既有判据。

- **新增第 24 个 crate `qx-spec`**：`ProjectManifest` / `ExperimentSpec` / `RunRecord` / `CapabilityManifest` / `EvidenceBundle` / `SchemaRegistry` 六类 + `qx-data::catalog_v2::DatasetManifestV2`（第七类，与既有 `DatasetManifest` 同 crate 以便复用 v1 解析）。统一走 `qx_spec::describe(kind, payload)` 这**唯一读入漏斗**，`FoundationKind` 七变体 `ALL` 数组驱动，`from_json` 一律 `deny_unknown_fields` 严格解析。
- **对象层硬约束（不只是字段校验）**：`RunRecord` 的 `status=completed` 必须同时带 `finished_at_ms` + `replay_verdict=verified` + 非空 `artifact_refs`（复现不了就不许标完成）；`CapabilityManifest` 声明 `sandbox_tested` 就必须有 `evidence`、`production_approved` 必须先 `sandbox_tested`、`level>=L3` 必须有沙盒证据、`level>=L4` 必须已生产批准；`EvidenceBundle` 的 `L4` 声明网络必须是 `mainnet`、`L3` 不得是 `mainnet`。这四条把「没拿到外部证据不得声明已通过」写成了**类型层拒绝**，而不是文档里的口头约定。
- **7 份严格 JSON Schema**（`schemas/*-v1.json`）：每份 `additionalProperties:false` + 版本键 `{"const": 1}`，与 Rust 侧 `*_SCHEMA_VERSION` 常量同源。
- **门禁牙齿 `foundation_specs_check()`（5 颗）**：① 七份 Schema 与 Rust 版本常量逐条一致；② 七类对象各只有一处 `pub struct` 定义（同名概念不得出现第二份）；③ `qx-spec` 是 workspace 成员且被 `qx-cli` 依赖（地基对象必须有生产读入者）；④ `describe` 派发臂覆盖七类且不重不漏；⑤ `capability.rs` 的四处证据闸门字面量必须仍在（防止有人把硬约束改软）。`GATE_CHECK_FLOOR` 仍为 633（667 实测已覆盖）。
- **生产读入者**：`qx-cli` 新增 `plan_commands.rs::verify_foundation_specs()`，由既有 `ecosystem` 自检驱动——七类各给一份最小合法夹具必须读入、再删掉版本号必须被拒。实测 `qx-cli ecosystem` 输出 `[规格 · qx-spec] 7 类规格读入/拒绝通过 ✓`，rc 0。
- **登记 limitation（诚实边界）**：① `foundation_objects_have_no_cli_readout_command`——规划里的 `qx plan` 只读命令**本轮未做**：`cli.rs`(679>673) 与 `cli_args.rs`(523>513) 已顶到 `maturity/line_budgets.yaml` 的**只降不升**棘轮，加一条新命令就会红。已改为走 `ecosystem` 自检接入，命令面留后续轮次（需先给这两个文件减行）。② `run_record_and_capability_not_yet_wired_to_backtest_artifacts`——两类对象目前只有声明与校验，回测产物尚未真正写入 `RunRecord`。
- **顺带修一处既有 clippy 红**：`crates/qx-cli/tests/unit/scheduler_dispatch_support.rs:220` 的 `&[accepted.clone()]` 触发 `clippy::cloned_ref_to_slice_refs`（`clippy --all-targets -- -D warnings` 下即 error，CI 会红）。改为 `std::slice::from_ref(&accepted)`，**净增 0 行**、行为不变。该告警在 `9d46305` 基线提交上就存在（rustc 1.98.1 的 lint 版本漂移），非本轮引入。
- **本轮实测**：`python tools/check_architecture.py` **667 项全绿 / 0 红**；`cargo clippy -p qx-spec -p qx-cli -p qx-data --all-targets -- -D warnings` **rc 0**；按「告警行号 ∩ 本次新增行区间」求交集，新增 **1,673 行里 0 条 clippy 告警**（唯一告警是上面那条既有项）；`cargo test -p qx-spec` **10 passed**；`cargo test -p qx-cli --bin qx-cli` **311 passed / 4 failed**，4 条全是既有沙箱 `os error 231`（`ERROR_PIPE_BUSY`，子进程管道环境噪声，与本轮改动无关，历史轮次同基线）；`qx-cli ecosystem` rc 0。


### 发布审计第二轮（2026-10-06）· 换角度 A/B/C · API 前后端 / 配置键 / 错误路径与状态机

**口径**：同第一轮（主体流程全联通、无断链、无孤儿逻辑、无死循环、前后端贯通），但换三个**不同**的扫描角度，避免与第一轮重复。详见 docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md 第 14 节。

- **第 A 轮 · API 前后端 + trait 方法零读者（1 条新登记）**：17 条路由与 deploy/README.md 端点表 17 个 token 逐一相等（门禁 `api_endpoint_table_routes` 双侧钉住）。全量扫 98 个 trait 方法，7 个无生产调用点——5 个 `JobQueueBackend` 方法已登记；新登记 `outbox_and_consumer_projection_contract_methods_have_no_production_caller`：`OutboxStore::append_outbox`（生产走各后端固有 `append`）与 `TransactionalConsumerStateStore::load_projection`（唯一消费者是契约用例），与已登记的 job_queue 那条同形。静态门禁的 `zero_reference_public_surface_check` 只扫 `pub fn`/`pub const`，trait 方法对它不可见（已登记盲区）。
- **第 B 轮 · 配置键接线（零发现）**：`runtime_config/schema.rs` 的 63 个字段全部有生产引用；deploy 下 18 份 runtime 模板的全部键都能对应到生产结构体字段（`ops` 是 map 键、`_comment` 由 `strip_config_comments` 剥离）；`deploy_template_coverage` 动态枚举 18==18，无漏检模板。
- **第 C 轮 · 错误路径 / fail-closed / 状态机（1 条新登记）**：`OrderStatus` 11 变体全部从 `PendingSubmit` 可达、7 个非终态全部有出边、无死锁状态；两处 `unreachable!`（`qx-runtime/src/lib.rs` 的 MarketQuote 臂、`qx-cli/src/strategy_host.rs` 的共享内存臂）经核实确实不可达；160 处 `let _ = ` 全为尽力而为语义；`StrategyContract{Input,Intent,Output}` 与 Python 侧逐字段相等。新登记 `legacy_no_spec_notional_fallback_synthesizes_a_spot_spec_instead_of_failing_closed`：`MaxNotionalRule::check` 缺 `instrument_spec` 时合成「现货 1:1」规格而非 fail-closed（源码两处 TODO 同源）。
- **本轮实测**：`python tools/check_architecture.py` **662 项全绿**（改动仍全部加性）。


### 发布审计三轮（2026-10-06）· 连通性 / 孤儿逻辑 / 终止性

**口径**：主体流程全部联通、核心链路无断链、无孤儿逻辑、无死循环、前后端贯通；每轮把当轮发现全部处理完（修掉，或按仓库先例登记为 capabilities.yaml limitation）才进下一轮。详见 docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md 第 13 节。

- **第 1 轮 · 连通性 / 断链（2 处已修）**：核心事件链（EventLog → pipeline → Ledger/Oms）完全连通，无断链。命中 2 处 README 过度声明并按代码事实改口：`qx-genglu` 行去掉「绩效指标、归因」（分别在 qx-xingban / qx-cli / qx-runtime），`qx-control` 行去掉「事件订阅游标」（在 qx-api）。`EventKind::Settle` 写侧「孤儿」经核实**不是断链**：它是派发循环写的结算标记事件，运行期 pipeline 刻意按 no-op 匹配，状态变更由同段落账簿分录单独承载。
- **第 2 轮 · 孤儿逻辑（3 条新登记 + 1 颗新判据）**：字段级零读者是仓库已登记的已知缺口（#174，静态门禁只扫类型与 pub fn/pub const，看不见结构体字段）。按 #118/#170/#171 先例「保留不删 + 登记 limitation + 双向钉住」，新登记 `risk_snapshot_carries_fields_no_decision_reads`（canonical_order_risk_decision）、`backtest_report_max_drawdown_raw_has_no_reader`（local_backtest）、`job_spec_declaration_fields_have_no_production_reader`（paper_execution）三条 limitation；新增双向判据 `zero_reader_struct_fields_stay_registered_in_capabilities`（crates/qx-cli/tests/unit/zero_reader_fields.rs）；在 qx-risk / qx-xingban / qx-scheduler / qx-data 的相应字段留下「零生产读者」路标（受行数预算只降不升约束，qx-scheduler/qx-xingban 用行尾注释、净增 0 行）。
- **第 3 轮 · 终止性 / 死循环 / 无界增长（2 条新登记）**：全部循环与阻塞调用有界，未发现死循环。已登记：EventLog 无保留策略、审计链无轮转、进程日志无轮转、内存投影共享 EventLog 保留缺口、成交去重台账刻意无界（由 bounded_growth_and_reap_check 守「只增」）。新登记 `scheduler_run_history_grows_without_retention`（paper_execution）：Scheduler.{runs, completed} 只增不减、落盘重启不缩小、dispatch 每 tick 遍历 runs() 判超时；`consumer_state_processed_ids_and_dead_letters_grow_without_retention`（storage_consistency_contract）：文件消费者 processed_event_ids / dead_letters 只增不减，SQLite/Postgres 同形状。
- **本轮实测**：`python tools/check_architecture.py` **662 项全绿**（三轮改动全部加性、未增删判据数量）；`cargo fmt` 改动过的 5 个 crate `--check` 无差异；新判据 `zero_reader_fields` 模块 **4/4 通过**；工作区用例与首轮同基线（少数失败为沙箱 os error 231 子进程环境噪声，非代码面）。


### V13 R14 + R15 + R16 收口（2026-10-04）· 第 17-19 轮三扫 · 数值精度 / 时间与时钟 / 序列化与 schema 演进

**三扫新角度（不重复 R8-R13 已查的面）**：R14 数值与精度面（u64 溢出回绕、除零、f64 舍入方向、跨语言同一数值的表示一致性）、R15 时间与时钟面（墙钟/单调时钟混用、ms/s 单位混淆、跨进程时间戳比较、审计链 ts 单调性、NTP 回拨行为）、R16 序列化与 schema 演进面（JSON 数值精度、字段名漂移与别名、缺失字段的默认值语义与 fail-closed 一致性、可选字段与空值的区分）。

- **R14 启动器闸门不对称（已修）**：`deploy/start-qianxing.sh` 在 `supervise` 之前没有 `runtime-check` 前置闸门，而 `start-qianxing.ps1` 有。`supervise` 只走 `plan_workers` 的拓扑校验，**不检查配置引用的文件是否存在**（数据集 bundle、研究快照、秘密文件的存在性只在 runtime-check / live-check 里查）——坏配置时 7 个子进程被拉起又在启动阶段各自失败，监督器按 fail-fast 把其余的停掉、process-logs 里落下日志，而不是在任何子进程起来之前就拒绝。Linux/macOS 与 Windows 走的是两条不同的失败路径。已在 `.sh` 补上同一道闸门。
- **R14 闸门退出码陷阱（已修）**：闸门失败时用 `if ! cmd; then echo ...; exit "$?"; fi`，此刻 `$?` 拿的是上一条 `echo` 的状态（**0**），调用方会以为闸门通过了：部署平台按 0 退出继续走下一步，而实际上一个子进程都没起来。已固定为 `exit 2`，与本脚本另外两处拒绝（配置缺失、可执行文件缺失）同码。
- **R16 三处廉价修法（已修）**：① Python `StrategyInput.from_dict` 补 `_reject_unknown_keys`（同文件的 `StrategyIntent` / `StrategyOutput` 都有，只有 Input 漏了：策略作者把 `positions` 拼成 `positionz` 时输入侧静默拿到空 dict，策略可能以为账户是空的而误触发）；Rust 侧 `StrategyContractInput` 同步加 `#[serde(deny_unknown_fields)]` 保持双侧对称（该结构在 Rust 侧只被构造、不从不可信输入反序列化，加注解无害且为将来防护）。② `FactorReport::validate` 补 `missing_policy` 的 `reject/skip/zero` 词表校验（`FactorAnalysisConfig::validate` 本来就有；报告能被 `from_json` 反序列化，只有空串校验时手改一份 JSON 能放行到下游 `resolve_missing` 才报「missing_policy 非法」，报错落点离改错的地方很远）。③ outbox `attempts` 读回改用 `parse_sqlite_u32` / `parse_u32` 而不是 `... as u32`：attempts 以 TEXT 落盘，u64→u32 是真截断，手工改库写成超过 `u32::MAX` 会回绕成小值、被误判成「仍在正常重试」而绕开死信判定。**注意 `schema_version` 分两格**：sqlite 侧以 i64 落盘、`i64 as u32` 是真截断（已修）；postgres 侧以 `INTEGER`(i32) 落盘，`i32 as u32` 是 32 位↔32 位按位重解释，`((x as i32) as u32) == x` 恒成立、无损，保留原样。
- **R14 数值与精度（0 真缺陷，1 处登记）**：台账记账（`apply_fill_with_multiplier_inner`）与撮合引擎、`numeric.rs`/`trading.rs`/`ledger`/Binance/CCXT 适配器全用 `checked_mul`/`checked_div`，外部数值入口全部过精度与范围验证。**登记**：`fee.rs` 的 `notional()`/`bp_amount()` 用 `saturating_mul` + 裸 `/`，与全库 checked 风格不一致；同族还有 `multi_leg.rs` 单位成本（注释自己写明「不能夹到 i64::MAX」却用了 `saturating_mul`）、`orderbook_backtest.rs` 预检（裸 `*`/`/`）、`ashare/trading.rs` 除权除息折算。全部当前不可达（bp 限 0..=10000、触发饱和需 `net_cost_raw > 1.7e34`、除零已有显式守卫），但收敛要改函数签名牵连三处调用方。`multi_leg_cost_bps` 的除零守卫本轮专门核实存在（`turnover_raw <= 0` 返回 `None`）。
- **R15 时间与时钟（0 真缺陷，1 处登记）**：全部 `*_ms` 字段统一 `Duration::from_millis`（`lease_clock` 正确 `/1000`）；111 处 `SystemTime::now()` 全部用于 epoch 时间戳值或临时目录命名，elapsed/deadline 统一 `Instant::now()`（NTP 回拨不会让超时提前或无限延长）；跨进程比较路径均为注入的单调时钟，CCXT 子进程的 `event_ts`（远端时钟）与 `received_ts`（父进程时钟）分离存储。**登记**：三家审计 store 的 `append` 均不保证 `ts` 单调不减，`validate_audit_chain` 与 `ControlPlane::from_json` 也不校；`retire_overflow_with` 按终态记录的 `(ts, command_id)` 升序退场，ts 倒退时退场顺序会偏向较新的命令——被退场的那条已终态、账目不错，但保留窗口会留下「较新的被留、较老的被退」。触发窗口很小（两次 `transact` 之间赶上一次 NTP 修正），hash 链与 sequence 完好。拒绝式修法（`Conflict`）会把一次 NTP 回拨变成控制面永久卡死；钳制式修法要在 `audit_entry_hash` 之前做且需处理既有非单调前缀，两种都不是改字能收口的。
- **R16 序列化与 schema（0 真缺陷，2 处登记）**：账户快照 schema_version 双读校验、8 个钱字段全 `Option<i128>` 无 default、`SnapshotHeader` 全 required、运行时配置全 `deny_unknown_fields`、Binance 全程 `String` 定点、CCXT `raw_i128_value` 显式拒 f64、A 股公司行为手工逐字段抽取（绕开 struct 默认值）、C ABI 5 个 struct 字段顺序与 C++ 头文件逐一比对一致、Python 与 Rust 的 StrategyInput/Intent/Output 字段名逐一对齐——**均已闭合**。**登记**：① serde_json 全仓未开 `arbitrary_precision`，`*_raw: i128` 遇 > u64::MAX 的 JSON 整数经 f64 中转丢约 1e6 量级精度；今天不可达（价格经 `Money::from_dec` 限 ≤9 位精度，qty 1e20 × price <1e9 → notional 1e29 远超任何合理的 `MaxNotionalRule` 上限；A 股实际值域 < 1e15），但 `validate_order` 无上界检查；收敛要么开 feature（`serde_json::Number` 内部表示从 u64/f64 换成 `Rc<str>`，波及全仓 `as_u64()`/`as_f64()` 形态与编译），要么逐字段加范围断言，两者都不是改字能收口的。② `DatasetComponentManifest.format` 的 `skip_serializing_if` 让 round-trip 字节漂移（值语义不丢，只影响血缘/指纹的字节级一致性）。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 636 / [FAIL] 0**（`GATE_CHECK_FLOOR` 629 → 633，新增 `launcher_pregate_check` 两颗 + `schema_defense_consistency_check` 四颗）；`cargo test --workspace --all-targets --no-fail-fast` rc 0 / **0 failed**；`python -m pytest python\tests -q` **64 passed**；`qx-storage --no-default-features --features sqlite,postgres,nats` **73 passed** rc 0；行数棘轮按 `--snapshot` 重登记（44 个超 500 行文件）。
- **工业级差距复核（结论不变：未达工业级）**：P0-1 sandbox evidence（`sandbox_tested: true` 与 `production_approved: true` 各命中 **0** 次）、P0-2 文件后端非事务性、P0-3 C ABI 信任根不锚定——本轮**没有**新动 P0 项，全部维持「未排期」。本轮修的都是启动器行为不对称与防御口径不一致，不触及 P0 的信任边界。


### V13 R12 + R13 收口（2026-10-04）· 第 15/16 轮 · Postgres 池自死锁、子进程回收、环文件泄漏

- **R13-A 修 PostgreSQL 控制面事务在 `pool_size = 1` 时当场自死锁（真缺陷，A 级）**：`PostgresControlStore::transact_control`（`crates/qx-storage/src/postgres.rs`）先 `lock_client()` 拿到 `clients[index]` 的 `MutexGuard`，事务提交之后在**同一个作用域内**调 `sync_control(&plane)`；`sync_control` 内部走 `read()` 会再取一次 `lock_client()`。池索引是 `next_client.fetch_add(1) % clients.len()`，池容量为 1 时两次都落到 `clients[0]`，而 `std::sync::Mutex` **不可重入**——同一线程重入就是当场自死锁。更关键的是：**事务已经提交成功**，但函数永远回不了调用方，`api_service.rs` 的 `with_control_submitter`（API 受理控制命令）与 worker 回写终态两条生产路径全停，控制面完全瘫痪。`postgres_pool_size` 的合法范围本就含 1（`topology_validation.rs` 校验 1..=128），不是假设中的边界值。已改为 `sync_control` 之前显式 `drop(client)`（`commit` 已经消费掉 transaction，要放的是它借用的那个连接锁），补 `#[ignore]` 回归用例 `postgres_control_transaction_completes_with_a_single_connection`（需挂 `QX_TEST_POSTGRES_DSN`，已在 postgres feature 下确认编译进用例名册）。
- **R12-C 修 CCXT 子进程 spawn 之后管道取失败不回收子进程（防御不一致）**：`CcxtProcessClient::spawn` 用 `child.stdin.take().ok_or_else(...)?` 取管道，失败直接 `?` 返回而子进程还在跑。`Stdio::piped()` 之后这两个 `Some` 是不变式、这条路径今天走不到，但上游一旦把 worker 改成无管道模式就会留下一个无人回收的孤儿 Python 进程——而 `strategy_host.rs` 的同一处早已经有 kill+wait。已改成先 `kill()` + `wait()` 再返回错误。注意 `call()` 里那次 `take()` 是「取用完就 `Some(handle)` 还给自己」的恢复式取用，语义不同、不在此列，门禁判据只读 `spawn` 的函数体。
- **R12-B 修共享环临时文件在设置失败路径上泄漏**：`.input`/`.output` 是 `memmap2` 打开的 mmap 文件（默认 64 MiB 一个、一对 128 MiB）。正常路径由 `PythonStrategyClient::drop` 回收，但 `SharedRingWriter::create` 与 `SharedRingReader::open` 任意一步失败时 `ring_paths` 还没被赋值，那条 `Drop` 不触发，`%TEMP%` 里就永久留下一对几十 MiB 的孤儿文件。已新增 RAII 守卫 `RingFileGuard`：从路径生成那一刻接管清理责任，成功时 `disown()` 把责任交还给客户端的 `Drop`（`remove_file` 对已删除文件幂等）。
- **R13-C 修 stderr 泵锁中毒静默返回空诊断**：`strategy_host.rs` 的 `diagnostics()` 在锁中毒时 `return String::new()`，`death_note()` 因此报「worker 无 stderr 输出」，运维会去看一个根本没坏的方向。已改为返回 `"; stderr=（stderr 泵锁中毒，尾部不可读）"`——诊断面自己交代自己坏了。
- **R12/R13 其余（0 处已修、4 处登记）**：① `process-logs/*.out.log`/`.err.log` 无轮转 / 无大小上限（与已登记的 EventLog/audit.json 无保留策略不同目录、不同形态，那条是不可丢的交易账本、这条是可丢的运行日志，处置策略本就该一起定）；② 写侧超时错误把 `Timeout`（handler 活着但不读 stdin）与 `Disconnected`（写线程自己死了）混成同一句「存活但不接收输入」，改字没有回归证据钉住；③ spread 恢复块与 scheduler 轮询只在循环头检查停机令牌，Ctrl+C 落在长恢复块内部要等整批跑完才生效（账目不错，错的是停机响应时间）；④ 全仓 25 处 `.expect("...poisoned")` 今天不可达（R10 已核持锁段内只返回 `Result`），但任何人往持锁段里加一个 `unwrap()` 这个面就从理论变成实路，且门禁没有一颗判据守「持锁段内不得有 panic 调用」。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 630 / [FAIL] 0**（`GATE_CHECK_FLOOR` 627 → 630，新增 `resource_lifecycle_and_lock_reentrancy_check` 三颗）；`WORKSPACE_TEST_FLOOR` 1082 → 1083（1 条 Postgres 池容量为 1 的回归用例，挂 `#[ignore]`）；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0 / **1068 passed / 0 failed / 1 ignored**（新增用例在默认 feature 集下不编译，postgres feature 下已在用例名册中确认）；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**；全 feature 矩阵两组全绿：`qx-storage --no-default-features --features sqlite,postgres,nats` **73 passed**、`qx-cli --no-default-features --features postgres,nats` **389 passed**，全 rc 0。行数棘轮按 `--snapshot` 重登记（44 个超 500 行文件）。
- **工业级差距复核（结论不变：未达工业级）**：P0-1 sandbox evidence（`sandbox_tested: true` 与 `production_approved: true` 各命中 **0** 次）、P0-2 文件后端非事务性、P0-3 C ABI 信任根不锚定——本轮**没有**新动 P0 项，全部维持「未排期」。本轮修的都是**可达的真缺陷**（Postgres 池自死锁在合法配置下必然复现），但不触及 P0 的信任边界。


### V13 R11 收口（2026-10-04）· 第 14 轮三扫 · WS 假 Close 断连、退出码丢失、C ABI 枚举 UB

- **R11-A 修 WebSocket 按整块缓冲逐字节扫 Close 帧（真缺陷）**：`crates/qx-api/src/ws.rs` 的读循环用 `client_buffer[..size].iter().any(|byte| (*byte & 0x0f) == 0x8)` 判 Close。但 Close 帧的 opcode 只出现在**帧起点**，这个表达式扫的是缓冲区里的每一个字节——256 个字节值里有 **16 个**（`0x08/0x18/…/0xF8`）会命中，二进制行情载荷里这类字节很常见，**客户端发一个完全正常的 text/binary 帧就把服务端静默断连**，而且 `return` 先于 `advanced = true` 执行，空闲计数也不推进。已改成 `WsCloseScanner`：按 RFC 6455 的帧头（2 字节基础头 + 126/127 的扩展长度 + 掩码键）跨读累积，帧没攒够就等下一次读，只有读到完整帧头才看 opcode，非 Close 帧整体跳过（本通道只推送事件、不消费客户端载荷，所以不解析载荷内容）。补 6 条回归用例，其中一条专门用 `[0x81, 0x03, 0x18, 0x00, 0x01]` 复现——`0x18`/`0x68`（'h'）都会命中旧判据。跨读累积同时引入了一个旧实现不存在的内存面：客户端可以每次只给一两字节、故意不完成帧头，让累积缓冲无限长大，所以另设 `WS_CLOSE_PENDING_CAP`（1 MiB）上界，越界即清空缓冲并按断连处理（fail-closed），补一条专门的溢出用例。
- **R11-A 修编排器子进程退出码被字符串化（真缺陷）**：`qx-orchestrator` 的 `ManagedProcess::poll_exit` 把 `status.code()` 转成 `String` 后传下去，父进程只能一律 `exit(2)`——panic 的 **101**、OOM 的 **137**、干净退出的 **0** 在监控里长得一样，故障分类的工作量被推回读日志。已把返回类型改成 `ProcessExit { raw_code: Option<i32>, label: String }`，`SupervisorStop::WorkerExited` 携带 `code: Option<i32>`，`supervise_workers` 返回新的 `SuperviseFailure`（`Usage` / `WorkerExit { raw_code, detail }`），`cli.rs` 把非零原始码原样上报、被信号杀死（`None`）走 **128**（POSIX 信号退出约定）。`SuperviseFailure::append` 保证收尾未收干净时原始码不被抹掉。补回归用例 `worker_exit_carries_the_raw_exit_code`。
- **R11-D 修 C ABI 的 `side` 是 `#[repr(C)]` 枚举导致宿主 UB（真缺陷）**：插件把 `side` 写进**宿主**内存。`QxOrderSide` 只有 `Buy = 1` / `Sell = 2` 两个判别值，而 Rust 读取一个不在已声明判别值里的 `#[repr(C)]` 枚举值**本身就是未定义行为**——`match` 里不存在可达的拒绝臂，一个签名过的插件写 `3` 就能在宿主上触发 UB。已改为 `QX_ORDER_SIDE_BUY/SELL: u32` 常量加显式拒绝（`"C ABI intent side 非法: {other}"`）。**布局不变**（都是 4 字节），既有插件无需重编译；C++ 侧本来就有 0 个消费点（`cpp/` 下 `qx_order_side` 只出现在头文件的声明与字段里），CI 不受影响。门禁的 `c_abi_header_check` 类型映射表补登记 `qx_order_side -> u32`（枚举集合比对自动收敛为只剩 `QxMarketEventKind`）。补回归用例 `decode_decision_rejects_an_invalid_c_side_value`。
- **R11-A 其余**：CLI 47 个长旗标全部有消费点、无 `ident: _` 丢弃；help 37 条入口 ≡ clap 37 个变体 ≡ `cli.rs` 37 条派发分支；配置 schema 全部字段（含 `WorkerConfig` 13 个、`StrategyRuntimeConfig` 39 个）均有生产读点；API 17 条路由覆盖完整（`KEYLESS_READ_ROUTES` 4 + `PROJECTION_SCOPED_ROUTES` 7 + 4 条全局出口 + `/account/snapshot/diff` + 1 条 POST）；WS 7 种下发帧类型与文档逐一对应；`paper-e2e`/`paper-check` 写入 `data_dir` 属验收链而非冒烟测试（同日重跑零新增已由 `paper_worker.rs` 显式处理）。**均为已验证连通，无新缺陷。**
- **R11-C 信任边界（0 处已修，5 处登记）**：A 级 3 处——① `decode_decision` 的 `slice::from_raw_parts(decision.intents, decision.intents_len)` 信任插件给的长度（只有 `MAX_PLUGIN_INTENTS = 100_000` 与「len>0 但指针为空」两道校验，没有由分配大小反推长度），插件只分配 1 个 intent 而声明 99_999 就越过分配边界读相邻堆内存；② `read_required_string` 用 `CStr::from_ptr` 读到 NUL 为止，不含终止符的指针会一路扫到页边界；③ `InstrumentId::parse` 的 symbol 段不做词表校验（`"../../etc/passwd.BINANCE"` 可解析）——**已核实今天不是活的穿越面**：`account_event_log_name` 只接收 account_id/venue_id，全仓 `crates/` 下无任何以 instrument 为路径片段的 `join` 调用。前两条的共同点是「这条 ABI 没有长度协商」，收敛要改契约（加容量字段或改迭代协议）；第三条收敛要么在 `parse` 加词表（影响历史产物形态）要么在每个把它当文件名的落点净化。另登记 3 条：`ControlCommand`/`Order` 未开 `deny_unknown_fields`（拼错字段被静默丢弃，Binance 侧有 `ReconcileRequired` 兜底但订单数量不会）、明文 API 下 `dry_run`/`permission` 由客户端控制（生产已被 `topology_validation` 拦死，剩 paper/sandbox 本地）、CCXT 子进程退出码不区分崩溃与被 kill（stderr 走 `inherit`，崩溃栈可见但无机器可读区分位）。五条全部按未排期登记，理由是每条的收敛都要改对外契约或砍掉现有验收能力。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 627 / [FAIL] 0**（`GATE_CHECK_FLOOR` 624 → 627，新增 `untrusted_input_boundaries_check` 三颗）；`WORKSPACE_TEST_FLOOR` 1073 → 1082（8 条回归用例：C ABI 非法 side 1 + WebSocket 帧扫描器 6 + 子进程退出码 1）；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0 / **1068 passed / 0 failed / 1 ignored**（R10 基线 1060 + 本轮 8 条）；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**；全 feature 矩阵两组全绿：`qx-storage --no-default-features --features sqlite,postgres,nats` **73 passed**、`qx-cli --no-default-features --features postgres,nats` **389 passed**，全 rc 0。行数棘轮按 `--snapshot` 重登记。
- **工业级差距复核（结论不变：未达工业级）**：P0-1 sandbox evidence（`capabilities.yaml` 里 `sandbox_tested: true` 与 `production_approved: true` 各命中 **0** 次）、P0-2 文件后端非事务性、P0-3 C ABI 信任根（本轮修的是宿主侧的 UB 与退出码，信任根仍不锚定）——本轮**没有**新动 P0 项，全部维持「未排期」。


### V13 R10 收口（2026-10-04）· 第 13 轮三扫 · 实盘去重键归零、恢复 worker 静默收窄、panic 丢诊断、确定性复核

- **R10-C 修三处实盘/恢复路径的静默降级（真缺陷）**：① **`binance.rs:1023` 成交回报的 `trade_id` 缺字段时 `unwrap_or(0)`**——`trade_id` 是 `seen_fill_keys` 去重键的一部分（`ingest_fill_wire` 的 `key = (client_order_id, trade_id, qty, price, fee)`），归零会让两笔都没有 `t` 的成交互相误去重、**漏计一笔且无人告警**；同一段代码对 `orderId`（字段 `i`）就是 `.ok_or_else(...)`，而 CCXT 侧（`ccxt.rs:548`）对同一情形本来就 fail-closed——两边口径不该不一致。已改为 `ReconcileRequired("Binance 回报缺少 trade_id(t)")`，补回归用例 `user_event_without_trade_id_requires_reconcile_instead_of_zeroing_it`。② **两个多腿恢复 worker 的 `venue_id` 缺省静默冒充**（`worker_entry.rs:136` → `"BINANCE"`、`:373` → `"ccxt"`）——`pending_spread_recovery_groups`（`spread.rs:230`）里**空 venue_id 是有意的通配符**，而默认值产出的是非空字符串，等于把通配符路径变成不可达：配置遗漏时 worker 会静默只扫一个 venue、**漏掉其余 venue 的裸腿**，而敞口正是"停止扫描等于永久晾着"的那种。已抽 `recovery_worker_venue_id()` 统一为启动即拒（点名 worker id 与角色），补回归用例 `recovery_worker_requires_an_explicit_venue_id`。③ **`worker_shutdown.rs:38` 的 `map_err(|_| "{label} panic")` 丢掉 JoinError 的 Debug 输出**——运维拿到的是"线程挂了"而看不到挂在哪里，等于把故障定位的工作量推回现场。已改为 `format!("{label} panic: {error:?}")`。门禁新增 `live_path_fail_closed_check` 三颗判据把这三处钉住。
- **R10-C 其余分级（不改，登记）**：A 级（生产可达 panic）**0 处**——6 处 `unwrap`/`expect`/`unreachable` 都依赖当前不变式成立（Binance 订单表 guard、strategy transport 枚举、API 快照刚置 Some 等），属"脆弱安全"而非活缺陷；C 级（错误上下文不足）7 处中已修最重的 1 处，余下 6 处（`event_pipeline.rs:424` 的 `.stderr(Stdio::null())` 丢弃 consumer 子进程 stderr、`runtime_check.rs:387` 的 JSON 报错缺文件路径、`readiness.rs:133` 的 `is_err()` 后只剩 `"control_store_unavailable"`、三处 `PoisonError` 消息笼统）属诊断增强，不改行为。
- **R10-A 跨重启幂等性（0 真缺陷）**：5 条崩溃/重启轴逐一取证——① EventLog 去重**不是独立已见集合**：`dedup_key` 持久化在事件元数据里随日志落盘，`seen_fills` 不直接落盘但由 `rebuild_runtime_indexes` 从 `Filled` 事件重建（`pipeline.rs:1177-1246`），不存在"事件已落盘、去重表未落盘"的中间态；`persist()` 是 `ingest_once` 最后一步，未落盘前崩溃＝整笔丢失、无重复。② outbox `event_id = {log_name}:{seq}` 确定性稳定；claim→publish→ack 顺序下若 publish 成功而 ack 前崩溃则重投，消费端按 `event_id` 幂等（`ConsumerEngine::consume` 第一步就是 `is_processed`）。③ 三后端 `transact_control` 都是"先提交快照、后补链"，且 `update(&mut plane)` 返回 Err 或 panic 都不会持久化——`validate` 在 mutation 之前，validate 失败则 mutation 不执行，不存在"Err 但 plane 已部分改"。④ 分段 EventLog 后端的崩溃窗口比单文件多一层（segment 已写、manifest 未更新），但 `read_if_exists` 按 digest/event_count **拒绝读取**（fail-closed），不是静默修复。⑤ JobQueue 租约恢复走 claim 路径的隐式接管（`expires_ts <= now` 则递增 fencing_token 接管），墙钟秒——回拨会延迟过期但不会永久不回收。SQLite/Postgres 的 EventLog+Outbox 在同一事务内，无 crash gap。
- **R10-B 无界增长（1 处新增登记）**：内存侧 14 个只增不减字段、磁盘侧 2 个只增不减文件，全部有淘汰调用计数的实测支撑。**新增登记** `audit_chain_has_no_retention_or_compaction_trigger`——持久化 `audit.json`（`AuditFileStore::append_unlocked`）只追加不退场（remove/retain/drain/truncate/pop/clear/delete/rotate/retention/compaction/archive/purge/prune **全词 0 处淘汰调用**），增长驱动量是控制命令数；内存侧 `ControlPlane.audit` 有 4096 的 `FINALIZED_COMMAND_RETENTION` 上限，持久化侧没有。**刻意不修**：内存上限省的是进程内存，持久化哈希链一旦轮转就会让"链落后自愈"与"前缀内容分叉检测"都失去参照物，轮转的前提是先定"轮转后从哪一条继续接哈希"。另登记 `event_log_in_memory_projections_share_its_retention_gap`：`Ledger.entries`、`Oms.orders`、`ApiState.events`、`ApiAccountProjection.events` 四个字段都是 EventLog 的内存投影，上界等于 EventLog 长度，不是独立风险面——EventLog 的保留策略定了它们跟着定。已验证有界 18+ 处（SnapshotHistory 1024、ApiEventBus 4096、ControlPlane 三表 4096、Outbox ack 删除、Reconcile 报告覆写）；**句柄泄漏 0 处**（ConnectionGuard 的 Drop 归还额度、SQLite 短生命周期连接、文件全部 open→write→close）。
- **R10-D 确定性与时钟面（0 缺陷）**：生产墙钟唯一入口是 `runtime_wiring.rs:289` 的 `runtime_timestamp_ms()`，全仓 194 处 `SystemTime::now` 里只有它在生产路径——读者是审计元数据 `ts`、API 请求戳、metrics 渲染、租约时钟（`lease_clock` 截断到秒）、实时行情的 `received_ts`、策略心跳，**无一路写进重放产物**；回测路径里那两处是临时文件名。EventLog 事件的 `ts` 来自事件自身（`ingest` 传入），不是墙钟。`qx-api/src/lib.rs:2042` 明确要求 `serve` 按连接读取墙钟而非调用方预取，`runtime_api_worker_identity.rs:168-174` 的用例点名钉住。HashMap 在架构规则层面被排除于顺序敏感迭代（`qx-core/src/lib.rs:9`：「HashMap 只用于查找，遍历一律排序」；`oms.rs:11`：「使用 BTreeMap 而非 HashMap，保证快照和回放的遍历顺序确定」）——全仓 `HashMap|HashSet|IndexMap` 仅 **3** 处命中，全部是那条规则本身、一处测试 `.collect::<HashSet<_>>()`、一处纯去重用途；抽查的 5 个 `.insert()` 落点（Binance 订单映射、factor artifact/report 表、rebalance targets）**全在 BTreeMap 上**。`sourcing.rs:13` 另有明写不用 `DefaultHasher` 的理由（跨版本不稳定会破坏"两次运行哈希一致"）。
- **R10-A/B 顺带核掉的 CI 覆盖缺口**：`qx-storage` 的 `default = []`，postgres 与 nats 后端**不在默认 `cargo test --workspace` 编译范围内**——本轮改动碰过 `postgres.rs`，若不单独验就会漏。已按 `.github/workflows/ci.yml` 的 matrix 四组补齐：`qx-storage --features sqlite,postgres,nats` **73 passed / 0 failed**、`qx-cli --no-default-features --features postgres,nats` **389 passed**、`qx-cli --features sqlite` **389 passed**，全 rc 0。CI 本身覆盖是完整的，缺口只在我此前的本地验证口径。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 624 / [FAIL] 0**（`GATE_CHECK_FLOOR` 621 → 624，新增 `live_path_fail_closed_check` 三颗）；`WORKSPACE_TEST_FLOOR` 1071 → 1073（两条回归用例）；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0 / **1060 passed / 0 failed / 1 ignored**；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**；全 feature 矩阵三组全绿（见上条）。行数棘轮按 `--snapshot` 重登记。
- **发布件按终树重打**：本轮改动横跨 `qx-adapter` / `qx-cli` / `tools` / `maturity` / 文档五个面，`qx-cli.exe` 与 wheel 里的 `.pyd` 都因此过期。`cargo build --release` 得 `qx-cli.exe` **12,560,384 字节**，`crates/**/*.rs` 比它新的 **0** 份；`tools/build_python_wheel.ps1` 得 **219,447 字节 / 17 条目**，sha256 `e98ec3102ff2c950e6f41711def218a7c0fb90c28e3b942dd40e605fe84545b9`，内嵌 `.pyd` md5 `6fb9285471edd527a8438d7a9ab89ee0` **≡** `target/release/_qianxing_native.dll` md5，**12 份 `.py` 与仓库逐字节不一致 0 份**；release `qx-cli` 三发 `all` / `ecosystem` / `runtime-check deploy\qianxing.runtime.example.json` 各 rc 0；把这份 wheel 装进**另一个**干净 py3.12 venv（`--no-deps --no-index`），`qianxing_bridge`/`qianxing_strategy`/`qianxing_ashare`/`qianxing_ccxt` 四个包 import 全 OK、`native.available() -> True`、`normalize_instrument('sz.000001') -> '000001.SZSE'`。
- **工业级差距复核（结论不变：未达工业级）**：P0-1 sandbox evidence（`capabilities.yaml` 里 `sandbox_tested: true` 与 `production_approved: true` 各命中 **0** 次）、P0-2 文件后端非事务性（`audit.json` 的无保留正是这条的延伸）、P0-3 C ABI 信任根——本轮**没有**新动 P0 项，全部维持「未排期」。


### V13 R9 收口（2026-10-04）· 第 12 轮三扫 · 审计链并发误报、WS 谎报路由、投影键分裂、策略超时错配

- **R9-A 修 `sync_control` 在 SQLite/Postgres 上的并发误报（真缺陷）**：三个后端的 `sync_control` 原先用 `existing.len() > records.len() || 前缀 zip 不等` 判分叉。但 **file 后端的整段 `transact_control` 都在同一把 `.control-plane.write.lock` 内**（读→改→存快照→补链一步到位），而 **SQLite/Postgres 的 `sync_control` 跑在控制面事务 `commit()` 之后**（`sqlite.rs:1084 → :1088`、`postgres.rs:1500 → :1507`）。`PRAGMA busy_timeout = 5000` 让两进程的写事务在提交时排队，于是进程 B 的事务必然读到 A 已提交的状态、它的 plane 是 A 的超集；B 补链在前、A 补链在后时，A 手里的 `existing`（含 B 追加的记录）就比 A 的 `records` 长——前缀逐条一致，旧判据却报 `Conflict`。**后果不是数据错，而是"控制面状态已经提交成功、`transact_control` 却返回 Err"**，调用方按失败重试会撞上重复请求。已把三后端改成只比公共前缀（`common = existing.len().min(records.len())`，前缀内容对不上才 `Conflict`，链更长按幂等空操作收口）。**篡改检测没被放宽**：`a_chain_ahead_of_the_snapshot_fails_the_transaction_inst_of_rewriting_history` 仍绿——它守的是同位内容分叉（`FORGED(99)` 落在 index 1，公共前缀内），不是长度。补回归用例 `audit_sync_control_treats_a_longer_chain_as_idempotent_not_conflict` 钉住三支语义（链更长＝幂等、前缀分叉＝仍拒、链更短＝正常追加）。`file/state.rs` 那段主张"链超前必须永久失败"的注释按新口径改写。**顺带核掉 R9-D2 另报的三条**：三后端 `sync_control` 前缀检查本来就逐字等价、哈希链共用同一份 `audit_entry_hash`（fnv1a）、`append` 幂等判定语义等价、空存储 `read` 三端一致；`sqlite.rs` 的 sequence 用 `max(sequence)+1` 而 file/postgres 用 `entries.len()`，稠密链等价，而 `validate_audit_chain` 读时强制 `entry.sequence == index` 直接拒掉 gap 链，实际不可达，按已验证一致登记。
- **R9-B 修 WS 查询键拒绝的谎报路由**：`crates/qx-api/src/ws.rs:77` 硬写 `"/events/live 不接受查询参数 {name}"`。但 **WS 通道不占路由表**（`deploy/README.md:299`："任何路径带 `Upgrade: websocket` 即在 HTTP 分派前转交"），客户端连 `ws://host/events?foo=bar` 收到的正文点名的是 `/events/live`——运维照那句话去查会查错地方。已取出请求真实 target（`split_target` 的 path 段）按实际路由点名，与 HTTP 侧 `lib.rs:1507` 的 `format!("{route} 不接受查询参数 {name}")` 同口径。同轮核掉 R9-B 其余四条：12 个文档登记码名与代码产出点逐一相等、`KEYLESS_READ_ROUTES`(4) 与 `PROJECTION_SCOPED_ROUTES`(7) 分派正确、OPTIONS 预检三态正确区分 `NotConfigured`/`Rejected`、`refuse_connection` 在 TLS 包装前判预算。**未修的登记面**：`/events` 的 500 分支文档没登记而 `/events/live` 登记了（两条都不可达）、`ApiProjectionKey::validate()` 的"两把都传但一个为空"分支文档没点名、`ControlError` 的 `{:?}` 复合串与 `serde_json` 原始错误串两处客户端无法精确匹配——都属契约文档补写，本轮按登记面处理。
- **R9-C 修投影键归一化不一致导致的同账户分裂**：`ApiProjectionKey::new` 原先不 trim。`market_bridges.rs:100-115` 的 `configured_account_event_logs` 返回**原始** `worker.account_id`/`venue_id`，而 `api_service.rs:343-344` 对快照 header 做 trim、`account_event_log_name` 内部也 trim——配置里写 `" main "` 这种带空白的账户，事件投影落到 `(" main ", venue)`、快照落到 `("main", venue)`，**同一账户在 `state.projections` 分裂成两格**，且两格都在表里、`missing_projection_response` 都返回 `None`，运维侧看不到告警。已在构造器统一 trim，三条路径（投影桥 / 查询串 / 快照 header）自动同一口径。**登记未修**：`/reconcile/reports` 把 `data_dir/reconcile/*.json` 全量返回（多账户下每个 worker 一份），而 `/account/ledger` 只读默认账户——同属 `KEYLESS_READ_ROUTES`，一给全部一给默认，已在 `capabilities.yaml` 登记为 `keyless_read_routes_do_not_scope_by_account`。
- **R9-D 修策略冷启动分支忽略 `python_timeout_ms`**：`strategy_host.rs:613` 的冷启动 helper 直接传写死的 `PYTHON_STRATEGY_TIMEOUT_MS = 2000`，而配置侧 `strategy_validation.rs:218-219` 允许 `python_timeout_ms ∈ [1, 60000]`。预热分支（`workers.rs:131/149`、`strategy_contract.rs:45/60`）都读配置，只有冷启动那条读死值——用户配 30s 在首轮被 2s 掐死。已把超时改为参数并接上 `config.strategy.python_timeout_ms`；那份常量只剩测试在用，改标 `#[cfg(test)]`，避免"常量从未使用"的告警（`cargo clippy -D warnings` 会红）。同轮修 `live_check.rs` 的本机回环判定：字符串前缀 `"127."` 覆盖不到 `[::1]:port`，改成 `SocketAddr::parse().is_ok_and(|b| b.ip().is_loopback())`，与 `topology_validation.rs:54` 的 `api_bind.ip().is_loopback()` 同口径，`localhost` 单列保留。
- **R9-E 类型面孤儿**：门禁三层零读者判据（`pub fn`/`pub const`/`pub(crate) fn`）**完全不索引类型**。独立脚本扫全仓 467 个 `pub struct`/`enum`/`type`：433 个有生产读者，`materialize` 家族的 5 个类型全部只被 `#[cfg(test)]` 消费（`materializer.rs` 的 `#[cfg(test)] mod tests` 在第 287 行，全部调用点在其后），与 R4-C 的 #119 断链登记一致。唯一真孤儿是 `qx-data::DataSchemaVersion`（结构体 + `impl Default`，全仓零引用；同文件 `DATA_SCHEMA_VERSION` 常量被 `provider.rs:25` 的 `BAR_FRAME_SCHEMA_VERSION` 使用，保留），已删除并同步 `lib.rs` 的 `pub use`。`InMemoryProvider`/`MemoryDataStorage`/`TcpHttpTransport` 三个测试专用类型按名字补登（`capabilities.yaml`）。`FactorEvaluator` trait 的两个实现者（`MomentumEvaluator`、`BadEvaluator`）都在 `#[cfg(test)]` 之后，属 #119 断链家族，不动。
- **R9-F 工业级差距复核（结论不变：未达工业级）**：R9-E 重扫终止性——11 处新增 `loop`/`while` 全部有界或属设计意图的常驻 supervisor（`shutdown.rs:65`、`supervisor_stop.rs:39` 等停机信号是本职）；无跨 crate static Mutex、无嵌套锁、无无超时 `Condvar::wait`；Python 三条 serve 路径都有父进程探测或 EOF 出口。14 处 `== "production"` 判定全部一致（都用 `eq_ignore_ascii_case`，且 environment 已由 `ENVIRONMENT_VOCAB` 收口），`scheduler.rs:102` 的 `dry_run` 判定是另一条独立轴不冲突。**0 项会导致进程永挂或真实资源泄漏**。硬缺口仍是 P0-1 sandbox evidence（`capabilities.yaml` 里 `sandbox_tested: true` 与 `production_approved: true` 各命中 **0** 次）、P0-2 文件后端事务语义、P0-3 C ABI 信任根——本轮**没有**新动 P0 项，全部维持「未排期」。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 621 / [FAIL] 0**（`GATE_CHECK_FLOOR` 619 → 621，新增 `audit_sync_control_prefix_check` 与 `websocket_refused_param_route_check` 两颗）；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0 / **1058 passed / 0 failed / 1 ignored**（R7 基线 1057 + 本轮 1 颗回归用例）；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**；`python tools/validate_core.py` 全部自校验通过。行数棘轮按 `--snapshot` 重登记。
- **发布件按终树重打**：本轮改动横跨 `qx-api` / `qx-cli` / `qx-storage` / `qx-data` / `tools` / `maturity` / 文档六个面，`qx-cli.exe` 与 wheel 里的 `.pyd` 都因此过期。`cargo build --release` 得 `qx-cli.exe` **12,557,824 字节**，`crates/**/*.rs` 比它新的 **0** 份；`tools/build_python_wheel.ps1` 得 **219,449 字节 / 17 条目**，sha256 `c36d25b700b049aa38c8de73109eb65fd6ca7c349005b28795f624f19d8650ec`，内嵌 `.pyd` md5 `55ced3bad29541d5cc1a89f5ce4805ed` **≡** `target/release/_qianxing_native.dll` md5，**12 份 `.py` 与仓库逐字节不一致 0 份**；release `qx-cli` 三发 `all` / `ecosystem` / `runtime-check deploy\qianxing.runtime.example.json` 各 rc 0；把这份 wheel 装进**另一个**干净 py3.12 venv（`--no-deps --no-index`），`qianxing_bridge`/`qianxing_strategy`/`qianxing_ashare`/`qianxing_ccxt` 四个包 import 全 OK、`native.available() -> True`。



### V13 R8 收口（2026-10-04）· 第 11 轮三扫 · 零读者判据第三层盲区、补单闸门漏档、超时误报、控制面口径

- **R8-A 补上零读者判据的第三层盲区**：`PUB_SURFACE_DEF` 只认 `pub fn` / `pub const`，全仓 **500 个 `pub(crate) fn` 整层逃过**——本轮之前没有任何一轮数过它们。新增 `zero_reference_pub_crate_surface_check`（含元判据共 3 颗，**616 → 619**）。`pub(crate)` 天生只在本 crate 可见，所以读者**按本 crate 计数是构造性正确的**，不需要像 `pub fn` 那样为跨 crate 的假活口付代价值得。那种匹配正是 `qx-storage::after` 活到本轮的原因：`AuditFileStore::after` 自己零读者（唯一调用点是 `#[cfg(test)]` 里的 `assert_eq!(store.after(0)…)`），却被 `qx-api` 事件游标里 **26 处**无关的 `after` 参数保活——`crates/qx-api/src/admission.rs:395` 的 `["account_id","venue_id","after"]`、`event_cursor.rs` 的参数与局部变量、`lib.rs` 十余处 `let after = match parse_after_cursor(query)`、`ws.rs` 与 `read_after(&self, after)`。`_production_lines` 其实**已经**剔掉注释行（`return [line for line in _strip_cfg_test_items(text) if not line.lstrip().startswith("//")]`），所以之前怀疑的「英文注释给 `after` 加读者」是错的——真正的原因跨了 crate。同轮把四个 AuditStore 镜像方法删掉：`AuditFileStore::root`（全仓零引用）、`AuditFileStore::after`（测试断言改用 `read()` + `filter(entry.sequence > 0)` 表达同一件事）、`SqliteAuditStore::path`（全仓零引用，`SqliteAuditStore` 只有 `::new` 与 `.sync_control` 两个生产入口）、`PostgresAuditStore::after`。**保留 `append` / `read`**：`read` 被 `append_unlocked`（`lib.rs:1451`）与 `sync_control`（`lib.rs:1473`、`postgres.rs:1324`、`sqlite.rs:1442`）在生产路径**内部**调用，`append` 是 `PostgresAuditStore::sync_control` 的生产入口（`postgres.rs:1338`），两者都不是孤儿——上一轮把「仅测试读者」误写成「零读者」，这一轮按源码内部调用点重判。唯一的新孤儿是 `qx-cli::for_test`：`#[cfg(all(test, feature = "nats"))]` 的复合 cfg 条件 `_strip_cfg_test_items` 剥离不掉（它只认恰好等于 `#[cfg(test)]` 的标记行），于是这个 test+nats 门控构造器看起来像一条生产定义；`EventConsumerHandler` 的三个字段是本模块私有，`tests/` 作为兄弟模块拿不到写权限，只能由定义模块开这个入口，已带理由登记。
- **R8-B 修 `submit()` 补单闸门漏了一档**：`crates/qx-execution/src/lib.rs:428` 的 `submit()` 对已存在的同 `client_id` 订单只挡两类——`is_terminal()` 或 `Accepted|Working|PartiallyFilled|Filled`（已确认，回 `ALREADY_APPLIED`）与 `Submitted|Unknown`（已出网但未确认，转 `ReconcileRequired` 并 Err）。`OrderStatus` 共 11 个变体，**`CancelPending` 掉出两个分支后直接落到 `venue.submit_order`**。而迁移表（`qx-core/src/order.rs:18-79`）里 `CancelPending` 只能走到 `Cancelled|Filled|Unknown`，`Submitted` **非法**——所以对一条撤单在途的订单补发等于用同一个 `client_order_id` 再下一单，这正是闸门本该挡住的那类事故。已并入 reconcile 闸门并改写错误信息（带状态名，原来那句「已离开本地但未有 Accepted 事实」对 `CancelPending` 并不成立）。**`PendingSubmit` 刻意放行**：已核对 `mark_reconcile`（`lib.rs:600`）只 append 一条 `ReconcileRequired` 事件、不写 status；而 `submit()` 里 `venue.submit_order` 的 Err 分支（`lib.rs:494-499`）经 `mark_reconcile` 转 `Unknown`、空回包（`lib.rs:489-492`）也 `mark_reconcile`，所以订单会停在 `PendingSubmit` 当且仅当**远端从未收到**（注册后、出网前失败，或跨重启恢复），补发是恢复动作而非重复下单。审计里那条「PendingSubmit 二次调用会重复记账」的结论不成立（`Accepted` 属 `semantic_replay`，EventLog 按 `correlation_id` 去重），但 `CancelPending` 那半是真的，已修。
- **R8-C 修策略宿主共享输入 ring 的超时误报**：实现现位于 `crates/qx-orchestrator/src/strategy_worker.rs:161` 的输入写入循环——`Ok` break、`Err(Full) if Instant::now() < deadline` 睡 1ms、`Full` 超时单独报错。到点仍是 `Full` 时，旧实现落入 catch-all，运维读到的错误是「ring 满/写入失败」，方向被引向共享内存实现本身；实际是 worker 停止消费输入——与紧接着的输出侧 `Err(Empty)`（`strategy_worker.rs:181` 起，报「worker 响应超时」并 `child.kill()` + `death_note`）**是同一种死法**。已改报「worker 输入写入超时 timeout_ms=…：共享输入 ring 满，worker 已停止消费」，并照输出侧补上 `child.kill()` 与 `death_note` 诊断，避免留下不再消费输入的 worker 让下一次请求再卡满整个预算。
- **R8-D 更正两处对控制面审计的失实口径**：`/metrics` 的 `qx_control_retired_*` 与 `/control/audit` 读的都是 `ApiState.control` 这份**进程内副本**——装配期从持久化控制面载入（`api_service.rs:22`），之后只有本进程 `submit_command` 走 `store.transact` 成功时才回写（`lib.rs:1800`）。worker 进程经 `control_store.transact(execute)` 落盘的命令终态与随之发生的退场**不回流**，所以在本进程下一次 `submit_command` 之前这两条计数都是**滞后上界而非实时值**。原注释写「这两条计数因此挂在每次 `/metrics` 的现读上」，`QueryPort` 那段也只说「`/control/audit` 走 `control_audit()`」——两处都已按实改写，实时化归入 P0 的 QueryService 拆分那层（登记项，本轮不动）。`/control/audit` 与它的三个兄弟端点（`/scheduler/runs`、`/account/ledger`、`/reconcile/reports`）错误口径不齐那半**未修**：`control_audit()` 直读内存不可能失败，要加 503 就必须改从 store 现读，那正是上一条的同一个改造，本轮不做半截。
- **R8-E 工业级差距复核（结论不变：未达工业级）**：硬缺口仍是 P0-1 sandbox evidence（全仓唯一一份 testnet 证据 `outcome:"skipped"`、`credentials_present:false`、`send_orders:false`；`capabilities.yaml` 里 `sandbox_tested: true` 与 `production_approved: true` 各命中 **0** 次）、P0-2 文件后端事务语义（EventLog/Outbox/队列在文件后端非单事务，`append_batch|BEGIN|COMMIT` 在 `crates/qx-storage/src` 只命中 sqlite.rs 两处且都不是这三条链）、P0-3 C ABI 信任根（`c_api.rs:645-865` 有 sha256 + 可选 Ed25519 `load_verified`，但 `ed25519_public_key` 是部署配置字段而非锚定的信任根）。`data_binding.rs` 整模块未接线那一条经核对**已在盘上登记**（允许清单 #53 + 计划 P1-7），本轮不删不接。
- **发布件按终树重打**：本轮改动横跨 `qx-storage` / `qx-execution` / `qx-cli` / `qx-api` 四个 crate，`qx-cli.exe` 与 wheel 里的 `.pyd` 都因此过期。`cargo build --release` 得 `qx-cli.exe` **12,555,264 字节**，`crates/**/*.rs` 比它新的 **0** 份；`tools/build_python_wheel.ps1` 得 **219,447 字节 / 17 条目**，sha256 `b299ecfe…`，内嵌 `.pyd` md5 `8618a4b524df2c4d899fad0fe2ffc459` **≡** `target/release/_qianxing_native.dll` md5，**12 份 `.py` 与仓库逐字节不一致 0 份**；release `qx-cli` 三发 `all` / `ecosystem` / `runtime-check deploy\qianxing.runtime.example.json` 各 rc 0；把这份 wheel 装进**另一个**干净临时 venv（`--no-deps --no-index --find-links dist`），`qianxing_bridge`/`qianxing_strategy`/`qianxing_ashare`/`qianxing_ccxt` 四个包 import 全 OK、`native.available() -> True`、`normalize_instrument('sz.000001') -> '000001.SZSE'`。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 619 / [FAIL] 0**；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0 / 87 段 / **1057 passed / 0 failed / 1 ignored**（与 R7 基线同数）；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**；`python tools/validate_core.py` 全部自校验通过。行数棘轮按 `--snapshot` 重登记。R8-A 删除 4 个方法净减约 25 行生产代码，门禁判据与注释净增约 90 行。


### V13 R7 收口（2026-10-04）· 第 10 轮三扫 · R6-A 的语义漏洞、零读者判据盲区里的三处零多态消费者



- **R7-A 修掉 R6-A 自己留下的语义漏洞**：R6-A 把两条投影失败都改成 `pipelines.remove(&pipeline_key)` + `continue`，注释写「不能留着继续轮询每 250ms 重复同一行错误日志」。但外层是 `while !stop { for in sources { … } thread::sleep(250ms) }`——下一轮 `while` 到这个 key，`pipelines.entry(...)` 是 Vacant，于是 `storage.open_read_only` 重开同一本账本 → `refresh()` → 再次投给 API。`project_event_log` 的失败是内容漂移或序号缺口，**不会自愈**，所以同一行 `eprintln!` 每 250 ms 复发一次，正好是我自己注释里说不会发生的那件事。新增 `permanently_retired: BTreeSet<(String,String)>`：两条投影失败各记一次、循环顶部跳过，退场成为终局。**`refresh()` 失败那一路刻意不入名单**——读盘失败可能是瞬时 IO，下一轮重开重试才正确，而投影失败是账本内容本身冲突，重试只会无限刷屏。顺带把残余的分叉窗口说明白：按账户投影成功、全局投影失败那一支，`state.projections[key]` 已推进一格而 `state.events` 停在原地，两个读面就此冻结在该点并由那行错误日志留证，不再继续发散。锁顺序复核过：`project_event_log` 与 `project_account_event_log` 各自只做一次 `self.state.lock()`，`ApiState` 内无二级锁，无死锁风险。**门禁新增第 616 颗判据**（615 → 616）把这三点钉住：永久退场名单定义恰 1 处、入名单点恰 2 处（两条投影失败各一）、循环跳过点恰 1 处，并按源码偏移量守「按账户投影调用点先于全局投影调用点」（R6-A 的提交顺序本身）。这颗判据补的正是它自己暴露的那个问题——R6-A 的注释与代码矛盾这件事，靠读代码才看得见。
- **R7-B 清掉零读者判据盲区里的三处零多态消费者**：这一轮查的是 `PUB_SURFACE_DEF` 的盲区——它只数 `pub fn` / `pub const`，**不认 trait 定义与 trait 方法**，所以这三处恒绿，前九轮每轮都绿过：①`qx-zhenlu` 的 `VenueAdapter` trait（`capabilities()` 返回 `ConnectorCapabilities`，全仓 `\.capabilities\(\)` 命中 **0** 条，连测试都没有；`health()` 返回 `AdapterHealth`，仅 `binance.rs` 三个 `#[cfg(test)]` 调用点）——trait 与只由它读的两个类型 `ConnectorCapabilities`/`AdapterHealth` 一并删除（后者若留下，删掉方法后就成了无读者的 `pub struct`），`capabilities()` 直接删、`health()` 的三个测试调用点改成直接读同模块的私有字段 `state` / `last_event_ts`。②`qx-execution` 的 `ReconcilePort` trait——全仓无 `dyn ReconcilePort`、无 `where T: ReconcilePort` 约束，三个生产调用点（`binance_reconcile.rs`、`ccxt_execution.rs`、`ccxt_reconcile_worker.rs`）都拿具体类型 `EventLogReconcilePort` 直接调，`reconcile_port_contract.rs` 那份「契约测试」也是直接调具体类型。删 trait，`require_reconcile` 改成该类型的固有 **`pub`** 方法——原来方法本身没有 `pub`，靠 `use ... ::ReconcilePort` 把 trait 拉进作用域才可见，trait 一删必须显式补 `pub`，否则三个跨 crate 调用点直接 `E0624`。三处 `ReconcilePort` 的 doc 引用（`lib.rs`、`reconcile_port_contract.rs` 文件头、`qx-genglu` 的 `VerdictAction` 注释）同轮改口。
- **R7-B 的裁定边界**：`JobQueueBackend` 按同一把尺子**保留**。它的五个 trait 方法（`enqueue_job`/`available_jobs`/`claim_job`/`ack_job_at`/`recover_expired_leases`）在生产链零调用者——生产队列面是 `ConfiguredJobQueue` 枚举，按 `match` 臂调各后端类型的**固有**方法（`enqueue`/`available`/`claim`/`ack_at`/`recover_expired`），两套接口并行存在、连名字都不同。但它唯一的多态消费者是 `crates/qx-storage/tests/queue_backend_semantics.rs` 的 `assert_job_queue_semantics(queue: &dyn JobQueueBackend)`，靠**同一份**断言代码逐个跑 file / SQLite / Postgres 三个后端——这是后端一致性 harness，不是死面，删掉等于删证据。改为在 `storage_consistency_contract.limitations` 登记为 `job_queue_backend_trait_methods_have_no_production_caller`，写明收敛方向（先让两套接口收敛成一份，或先定 `ConfiguredJobQueue` 为什么必须持有构造器而不是裸 trait 对象）。同一轮补 `AuditFileStore::after()` 这类「同名兄弟遮蔽」的失配：门禁按名字找读者，`PostgresAuditStore::after` 有生产读者就把 `AuditFileStore::after`（仅 `lib.rs:2070` 一处用例读者）判成活面——与 `latest_quote` / `query_command` 那次是同一个盲区家族。
- **R7-C 能力矩阵与家族登记同步**：fam02「API 读模型」加续修注——它名下的 6 格里今天已有 4 格在盘（**事件按账户键**由 R5-A 接通、**快照历史有界** `MAX_SNAPSHOT_HISTORY = 1_024`、**控制面现读** `submit_command` 整体换 `state.control`、**停机令牌与连接上界** `session_shutdown` / `connection_budget`），唯一未落的是 QueryService 拆分那层「API 自己那份查询读模型 `EventLog` 无淘汰水位」，R4 那段的技术理由不变（把窗口外的旧事件剪掉会让一次合法的重叠投影查不到而被报成内容漂移）。R6-A2 的第三本台账登记、R6-B 的六处公共面删除也一并补进对应条目。
- **R7-D 口径收口**：终版文档 §1.1 门禁行与 §1.3 标题从 615/六轮改到 616/七轮，§8 一句话结论补第 10 轮段落并把「九轮」改「十轮」。
- **发布件按终树重打**：本轮改动横跨 `qx-cli` / `qx-api` / `qx-storage` / `qx-execution` / `qx-zhenlu` / `qx-adapter` / `qx-genglu` 七个 crate，`qx-cli.exe` 与 wheel 里的 `.pyd` 都因此过期。`cargo build --release` 得 `qx-cli.exe` **12,582,400 字节**，`Get-ChildItem crates -Recurse -Filter '*.rs' | Where-Object LastWriteTime -gt exe` 得 **0** 份；`tools/build_python_wheel.ps1` 得 **219,447 字节 / 17 条目**，sha256 `5ed022fe…`，内嵌 `.pyd` md5 `497dcb08bd04e84cbb6154a5b9380e85` **≡** `target/release/_qianxing_native.dll` md5，**12 份 `.py` 与仓库逐字节不一致 0 份**；release `qx-cli` 三发 `all` / `ecosystem` / `runtime-check deploy\qianxing.runtime.example.json` 各 rc 0；把这份 wheel 装进**另一个**干净临时 venv（`--offline --no-deps --no-index`），`qianxing_bridge`/`qianxing_strategy`/`qianxing_ashare`/`qianxing_ccxt` 四个包 import 全 OK、`native.available() -> True`、`normalize_instrument('sz.000001') -> '000001.SZSE'`。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 616 / [FAIL] 0**；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0（**87 段 / 1057 passed / 0 failed / 1 ignored**）；`python -m unittest discover -s python/tests -q` **64 passed / OK**；行数棘轮按 `--snapshot` 重登记。R7-B 的三处删除净减约 40 行生产代码，R7-A 净增约 25 行（含永久退场名单与门禁判据）。**发布件因此过期**，需按同一套流程重打 `qx-cli.exe` 与 wheel。


### V13 R6 收口（2026-10-04）· 第 6 轮三扫 · R5 自身缺陷、第三本去重台账、六处零读者公共面

- **R6-A 收口 R5-A 自己的提交顺序缺陷**：`spawn_api_projection_bridge` 里 `project_account_event_log` 与 `project_event_log` 各自在内部加锁改状态、返回值只是结果，原写法是**两条都提交完才开始看错误**。于是默认账户的按账户投影失败（`project_event_log` 的内容漂移 / 序号缺口这类不会自愈的冲突）时，全局那格已经被写过——同一个账户的 `/events`（读 `state.events`）与 `/events?account_id=&venue_id=`（读 `projections[key].events`）从此各讲一份，而这恰是 R5-A 的 CHANGELOG 声称已经关掉的那个形状。改成先提交并当场判定按账户那条、成功后才写全局那格。连带修掉我自己在这轮引入的第二个问题：全局投影失败原先只 `eprintln!` 不摘管道、不 `continue`，而 `project_event_log` 的失败不会自愈，于是同一条错误每 250 ms 复刷一次（约 4 行/秒），且 `/ready` 的 `projection_readiness()` 只扫 `state.projections`、永远看不见全局那格，进程照报 200。现在两条失败都摘管道退场。
- **R6-A2 补上第三本只增不减的成交幂等台账**：R5-B 的正则 `seen_(?:fill_keys|trade_ids)\.` 不认 `seen_fills`，而且判据只读 `BINANCE_VENUE_FILE`/`CCXT_PUMP_FILE` 两个文件——`crates/qx-runtime/src/pipeline.rs` 的 `LiveEventPipeline.seen_fills: BTreeSet<(u64,u64,i128,i128,i128,String)>`（键 `(order_id, ts, qty, price, fee, venue_order_id)`，比 Binance 那本多一栏 venue 订单号）整本不在范围内。它只增：唯一读点 `.contains`、三处 `insert`（两条 ingest 路径各一、`rebuild_runtime_indexes` 重启时把整本 EventLog 的 `Filled` 全插回来一处）、全词无 `remove`/`retain`/`drain`/`pop`/`clear`/`truncate`；上界是进程一生 ingest 过的成交数，`LiveEventPipeline` 还是 `#[derive(Clone)]`，克隆一次多复制一整本。**淘汰是否安全：不安全**——它挡住 user stream / reconcile / Outbox 补投影三条路径重投同一笔 fill，去掉任何一条已见键同一笔成交就被 `oms.apply_fill` 记两次账，与 R5 注释写的同一个失败方式。所以该登记 + 扩门禁，不是加水位：正则扩成 `seen_(?:fill_keys|trade_ids|fills)\.`，pipeline.rs 纳入同一颗判据（Binance 1 处 `insert` / CCXT 1 处 `entry` / 运行期 3 处 `insert`），三处字段注释都写明「只增不减的成交幂等台账」，capabilities.yaml 的 `binance_direct.limitations` 从两本改三本。门禁条数不变（615）。
- **R6-B 清掉六处零调用者的公共面**：①`JobQueueBackend::ack_job` 及三个实现（sqlite / postgres / file-jobs）——被带 fencing token 的 `ack_job_at` 取代，全仓 `.ack_job(` 命中 0 条，留着会让读者以为无 token 的 ack 仍在契约里；`ack_at` 那条在用（`tests/queue_backend_semantics.rs` 108/115/119），且 `FileJobQueue::ack` 固有方法仍有用例读者（`lib.rs` 2061 附近的接管测试）。②`AuditStore` 整个 trait 与三份实现——四个方法（`append_record`/`read_entries`/`query_command_entries`/`entries_after`）零调用者，全仓无 `dyn AuditStore`、无 `impl Trait: AuditStore`，生产读审计走固有方法 `AuditFileStore::append` / `append_unlocked`；那份测试改用 `read()` + 内联 filter。③`MarketDataPort` 与 `LiveEventPipeline` 的实现——无 `dyn MarketDataPort`、无 `latest_quote(&dyn)` 调用点，生产用的是同名固有方法 `latest_quote_with_depth`（`spread.rs`、`venue_runtime/paper_submit.rs` 在读）。④`ControlPort` 与 `ApiService` 的实现——无 `dyn ControlPort`、也没有 `control_port()` 访问器（对比 `query_port()` 定义在用），与 `QueryPort` 那条在用路径不对称。⑤`QueryPort::events_after` 与实现——全仓 `events_after` 的其余命中全是另一个函数 `events_after_cursor`（`event_cursor.rs`，`/events`、`/events/live` 走它），trait 方法零调用者。⑥`DataStorage::load_range` 与 17 行默认实现——唯一读点是自己那个测试，用例改成只断言正序。
- **R6-B 的连带孤儿**：删完立刻被零读者判据反查出两条**新**孤儿——`qx-runtime::latest_quote`（唯一读者是刚删掉的 `MarketDataPort::latest_quote` 实现，测试 `paper_bridge_and_bundles.rs` 的那一处断言与紧随其后的 `latest_quote_with_depth` 断言重复，改成对 depth 版补 `ts`/`bid`/`ask` 三个断言）与 `qx-storage::query_command`（文件版与 Postgres 版，唯一读者是刚删掉的 `AuditStore::query_command_entries`）。门禁条数不变（615），但「删了公共面又长出零读者」这条链路被判据当场拦下来，这正是判据该做的事。
- **R6-C 更正一条不存在的节奏**：`crates/qx-api/src/snapshot_history.rs` 头注释原先写「读模型每 250 ms 重装一次账户快照」，于是 1_024 那份数被描述成「约 4 分钟的基线可回看窗口」。事实是投影桥只喂事件读模型（`project_event_log`/`project_account_event_log` 只写 `events`+`event_bus`，**不碰** `snapshot`/`snapshot_history`），快照只在 `build_configured_api_service` 装配时按域装载一次（全仓 `publish_snapshot` 的生产命中只有 `api_service.rs` 两处 boot 调用）。按真实契约改写：首次加载走快照、增量走事件、读侧自己拿 `/events` 做 reducer；1_024 从「4 分钟窗口」更正为内存天花板，窗口实际多长取决于本进程换过几次摘要。
- **R6-D 口径收口**：终版文档 §8 摘要停在 614 且把「第 4/5/6 轮」和「R4」两套编号混着数，§1.1 与 §1.3 已经是 615；统一到 615 与 R1…R6 一套编号，并补上 R6 的四颗。同轮更正 §1.3「三轮共同结论」里「ccxt/binance insert-only map 无界」仍在待办那格的旧说法（R5/R6-A 已补牙齿），以及投影桥那段注释只提 `project_event_log` 漏了 `project_account_event_log`。
- **发布件按终树重打**：本轮改动横跨 `qx-api` / `qx-storage` / `qx-runtime` / `qx-data` / `qx-execution` 五个 crate，`qx-cli.exe` 与 wheel 里的 `.pyd` 都因此过期。`cargo build --release` 得 `qx-cli.exe` 12,581,376 字节，`Get-ChildItem crates -Recurse -Filter '*.rs' | Where-Object LastWriteTime -gt exe` 得 **0** 份；`tools/build_python_wheel.ps1` 得 **219,448 字节 / 17 条目**，sha256 `79fac3b2…`，内嵌 `.pyd` md5 `1e2f3ab8611ee6289c8cbb212a435536` **≡** `target/release/_qianxing_native.dll` md5，**12 份 `.py` 与仓库逐字节不一致 0 份**；release `qx-cli` 三发 `all` / `ecosystem` / `runtime-check deploy\qianxing.runtime.example.json` 各 rc 0；把这份 wheel 装进**另一个**干净临时 venv（`--offline --no-deps --no-index`，补 `tzdata`），`qianxing_bridge`/`qianxing_strategy`/`qianxing_ashare`/`qianxing_ccxt` 四个包 import 全 OK、`native.available() -> True`、`normalize_instrument('sz.000001') -> '000001.SZSE'`。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 615 / [FAIL] 0**（含扩到三本台账的那颗「成交幂等台账只增不减」）；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0 / 87 段 / **1057 passed / 0 failed / 1 ignored**；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**。行数棘轮按 `--snapshot` 重登记（`pipeline.rs` 2332→2338 注释、`snapshot_history.rs` 注释、四处 orphan 删除使 4 个文件下探）。行数变化为增的为**注释与判据正则**，删除均为零读者公共面，无一改动生产语义。尚未 `git add`、未推送。


### V13 R4/R5 收口（2026-10-04）· 第 4、5 轮三扫 · 断链与门禁盲区收口 · 发布件按终树重打

- **R4-A「无预算等待」族第三处**：`crates/qx-adapter/src/ccxt.rs` 的 `CcxtProcessClient::call` 原先直接 `self.stdin.write_all(payload)` 再等读侧——一旦 CCXT worker 存活但不再从 stdin 取字节，超出 OS 匿名管道缓冲（~64KB）的那段写入永久阻塞，`timeout_ms` 只守读侧，本进程就此卡死且连关停都读不到。修法与 #281（策略宿主）、#282（event consumer handler）同族：`stdin` 降为 `Option<ChildStdin>`，写 `write_all`+`flush` 交给 spawned 线程，主路径按 `write_done.recv_timeout(Duration::from_millis(self.timeout_ms))` 收口，超时即杀 worker 并回「CCXT Worker 输入超时」诊断。新增门禁 `ccxt_pipe_write_budget_check` 三条（写入被交给线程且有界等待、写侧预算取自 `self.timeout_ms`、超时诊断与句柄回收在位）。
- **R4-C 两处门禁盲区**：`PUB_SURFACE_DEF` 旧式 `pub (?:async )?(?:fn|const) (\w+)` 在 `pub const fn` 上把捕获组落到 `fn` 本身——全仓 23 处 `pub const fn` 因此全部映射成 `crate::fn`，而 `fn` 一词到处都是，「零读者」永远数不出来；`pub use` 重导出行曾被当成生产读者，而那行只是把名字摆到 crate 根、没有任何调用。两处修好后立刻放出 `qx-data::load_bar_batch`（批量装载逐条校验 provider 不得返回越界行，能力在盘但生产走 `ingest_bars`）与 `qx-core::default_maker_taker`（生产直接读 `DEFAULT_MAKER_BP`/`DEFAULT_TAKER_BP` 装配，只有用例在调构造器），各带理由登记进允许清单；新增一颗元判据把两处盲区钉住。同时复核 479 个 `pub struct/enum/trait/type` 与 82 条 `pub use` 重导出，除 `load_bar_batch` 外全部有生产引用，允许清单既有 27 条零读者登记无一长出读者。
- **R5-A 接通无键事件读面这条断链**：`GET /events`、`GET /events/live`、不带 `account_id`/`venue_id` 的 WebSocket 三条读面读的是那格全局兼容投影，而 `spawn_api_projection_bridge` 只喂按账户投影，于是生产上这三条读面恒为空数组。修法与快照那条判据同源：桥按 `default_account_event_log` 认出默认账户，把它的那本 EventLog 同时投影到全局兼容读模型，其余账户仍只进自己的投影，多账户部署下两个读面不会各讲一个账户。投影桥函数原有的文档注释（「调用 API 的 `project_event_log`」）此前与代码不符，本次一并落到真实调用。
- **R5-B 给成交幂等台账补牙齿**：Binance `seen_fill_keys` 与 CCXT `seen_trade_ids` 是**只增不减**的去重台账——user stream 与 `trades` 查询都会把同一笔成交重投，淘汰任何一条已见键就等于允许同一笔 fill 被 trace 两次（重复记账），这是本仓库唯一不接受的失败方式。它们刻意不设水位，所以新判据守的是「只增」而不是「有界」：入账点各恰一处、两个字段全词无 `remove`/`retain`/`drain`/`pop`/`clear`/`truncate`、两处字段注释都写明「只增不减的成交幂等台账」，并登记进 capabilities.yaml 的 `binance_direct.limitations`。同期把 `QueryPort` 的注释改诚实：它原先谎称「网络协议只依赖这些只读方法」，而 `/account/orders|positions|balances` 三个端点实际先走 `snapshot_for_query` 做身份解析再取分表，从不走 `account_orders()` 那三个**无身份的默认账户视图**——那三个方法只有用例读者，接到带身份的端点会在多账户部署下静默串账，因此注释写明它们需要身份感知变体才能上生产。
- **门禁 610 → 615（净增五颗）**：R4-A 三颗、R4-C 一颗元判据、R5-B 一颗。行数棘轮按 `--snapshot` 逃生门重登记（本轮涨的是 `qx-adapter/src/binance.rs` 2121→2127、`qx-adapter/src/ccxt.rs` 1174→1180、`crates/qx-api/src/lib.rs` 2979→2990，均为注释与写侧预算）。
- **发布件按终树重打（台账 #251，本轮落地）**：此前 `target/release/qx-cli.exe`（18:40）早于 R4-A 的 `contract.rs`（19:44）与本轮 R5 的 `market_bridges.rs`（22:08）、`binance.rs`/`ccxt.rs`（22:16），而 `dist/…whl`（18:22）里嵌的 `qianxing_bridge/strategy.py` 只有 14504 字节、仓库 15344 字节——**发布物仍是改前那份代码**。本轮：`cargo build --release` 后 `qx-cli.exe` 12,581,376 字节，`Get-ChildItem crates -Recurse -Filter '*.rs' | Where-Object LastWriteTime -gt exe` 得 **0** 份；`python\.venv` 不存在，按 `uv venv python\.venv --python 3.12 --clear` + `uv pip install pip tzdata` 补齐前置后跑 `tools/build_python_wheel.ps1`，得 **219,448 字节 / 17 条目**，`.pyd` md5 `dc8b48c61576334274ff1ba50811f92a` **≡** `target/release/_qianxing_native.dll` md5，**12 份 `.py` 与仓库逐字节不一致 0 份**；再把这份 wheel 装进**另一个**干净临时 venv（`--offline --no-deps --no-index`，补 `tzdata`），`qianxing_bridge`/`qianxing_strategy`/`qianxing_ashare`/`qianxing_ccxt` 四个包 import 全 OK、`native.available() -> True`、`normalize_instrument('sz.000001') -> '000001.SZSE'`。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 615 / [FAIL] 0**；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets` rc 0 / 87 段 / **1057 passed / 0 failed / 1 ignored**；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**；release `qx-cli` 三发 `all` / `ecosystem` / `runtime-check deploy\qianxing.runtime.example.json` 各 rc 0。尚未 `git add`、未推送。


### V13 R7 收口（2026-10-04）· 第 7 轮三扫 · 台账 #284 · R7-a —— 策略输出契约的六臂折叠诊断把「漏写 schema_version」念成「身份不一致」

- **这颗不是扫出来的，是 R6 那次干净 venv 冒烟撞出来的**：策略作者按 `schemas/strategy_api_v1.md` 返回一个 dict 而漏写 `schema_version`，两侧宿主都回同一句折叠诊断（Rust `StrategyContractOutput 与输入身份、标的或有效期不一致`、Python `strategy output identity or expiry does not match input`），于是他去查 `request_id`——那一格本来就没写错，永远查不出问题。跨语言分叉最常见的长相恰好是「worker 侧 Python 已放行、宿主侧 Rust 才挡下」，而那条链上第一格就是版本号；一句折叠话术把七种成因压成一条查不到的线索。
- **落地（两侧同一口径，拒绝集一字未动）**：`crates/qx-strategy/src/contract.rs:286` 的 `validate_for` 拆成八句各说各话的诊断、首臂点 `schema_version`；`python/qianxing_bridge/strategy.py:290` 同样八句 `ValueError`，空标的走自己那一臂；解码入口 `crates/qx-strategy/src/contract.rs:353` 的 `from_json_for` 与 `python/qianxing_bridge/strategy.py:337` 的 `from_dict` 各过一次 `validate_for`，Python 缺键读 `0` 而不采纳自家默认版本。牙齿按「折叠优先」走：Rust 侧把 `crates/qx-runtime/src/strategy_contract/tests.rs:54` 那颗既有用例扩成六臂点名表并新增一颗解码入口用例，Python 侧折进既有那颗 `python/tests/test_strategy_contract.py:41`。
- **三条诚实话**：「两侧过期极性不一致」这颗**从来没有存在过**（HEAD 两侧都是 `!= 0`），本轮差点把一句注释当缺陷——它现在被钉成不变量，是钉子不是修复；解码入口那次 `validate_for` 在 HEAD 也已在场，同样是钉子；台账里那句「+0 颗 `#[test]`」**半假**——Python 侧真折叠（`Ran 64` 保持），Rust 侧净增一颗（承担用例文件 HEAD 4 颗 / 工作树 5 颗，全仓磁盘在册 1070 → 1071），地板随之 1070 → 1071。
- **门禁 602 → 610（净增八颗运行时判据）**：轮函数 `strategy_output_arm_diagnostics_check()` 两侧都从源码现取诊断句集合，不写第二份手工清单——把某臂抄回折叠句、删一臂、首臂换人、两侧极性式子分叉、解码入口少一次校验、承担用例改名六种动作全部当场红；`tools/check_architecture.py:542` 的 `GATE_CHECK_FLOOR` 注释里那半句 `602 -> 610` 就是本轮。`_python_scope()` 取不到唯一锚点时返回空串让下游判 FAIL（方向对且不 `die`），剥 `#` 注释让「散文既不替代码定罪也不替代码作证」由 C1/C2 两颗对照枪实测。
- **变异取证 14 枪 + 4 枪重放**：主轮档位现读 `TALLY {'fired': 14, 'killed': 12, 'equivalent': 0, 'masked': 0, 'noconclusion': 0, 'control_green': 2, 'control_bad': 0, 'restore_ok': 14, 'collateral': 0}`、`IDENTITY fired=14 tiers_sum=14 equal=True restore_ok=14`，BASELINE 与 FINAL pristine 同为 `PASS=610 FAIL=0 八颗在场=8/8`，四份注入文件按 sha16 逐颗比回（`contract.rs=a55080f30dfe5b25` / `strategy.py=e1a3c0d255d0436f` / `tests.rs=b9e40c31c37fb113` / `check_architecture.py=7793ea3c13e54f43`）；形状枪 S1/S2 各读三颗 FAIL（`PASS=607 FAIL=3`）证明锚点取空的方向是红不是静默通过；因主轮开火后 `tests.rs` 又被本轮自己的 clippy 修复改过字节，G1/G2/G8/G10 四颗一律在终字节上重放击杀（4）。**最有价值的一格是 G7**：门禁红而 `Ran=64` 套件仍绿——「两侧同一个式子」这条判据眼下没有用例替它作保、它是唯一读者，按规矩登记为下一轮的活（给极性分叉补一颗常驻反例）。
- **本轮实测（九腿整跑在终树上，读数全部由日志派生，文档里没有一个手抄数字）**：树摘要九腿前后同为 `142085caa8745541`、rc 全 0、无 host-locked 腿；`fmt --check` rc 0（`Diff in` 0 行）、`clippy --workspace --all-targets -D warnings` rc 0（诊断 0 行，本轮新写的六臂表先被 `type_complexity` 抓过一次）；`cargo test --workspace` rc 0 / 107 段 / 1057 passed / 0 failed / 1 ignored（86 段 Running + 21 段 Doc-tests），`--all-targets` rc 0 / 87 段 / 1057 passed，`--doc` rc 0 / 21 段 / 0 passed；`--list` rc 0 / 1058 行 / 1056 唯一名，恒等式 1057 + 1 = 1058、1071 − 1069 = 1058 − 1056 = 13（`nats` 10 + `postgres` 3）；Python `Ran 64 / OK`（rc 0，本轮这遍 0 skip）；`validate_core.py` rc 0；门禁 rc 0、**[PASS] 610 / [FAIL] 0**。引用普查（落笔后重跑）：活文档 @CENSUS_LIVE_TOTAL@ 条 `path:NN`、@CENSUS_LIVE_DEAD@ 条落空，存档 @CENSUS_ARCHIVE_TOTAL@ 条、@CENSUS_ARCHIVE_DEAD@ 条落空（天花板 9，只降不升）。**Python 的 skip 口径本轮纠正**：此前 README 与终版计划写的 `OK (skipped=2)` 是「没构建 `_qianxing_native.pyd` / 没装 `pyarrow`」那台机器的读数，文档现在写「读数 + 口径」两句而不是把 2 换成 0 了事。
- **发布件重建义务（台账 #251，本轮未做，按依赖图与字面量普查现读定义务）**：`cargo tree -p qx-python -e normal` 不含 `qx-runtime`，而 `qx-cli` 直接依赖它——本轮改动落在 CLI 二进制与 wheel 的 Python 载荷上。wheel 侧 12 份 `.py` 里与仓库逐字节不符的恰好 1 份，正是 `qianxing_bridge/strategy.py`（wheel 内 14504 字节 vs 仓库 15344 字节，折叠句命中 1 次、逐臂句 0 次）——**wheel 内嵌的仍是改前那份代码**；`target/release/qx-cli.exe` 按字面量面已带八句（各 ≥ 1 次、旧折叠字面量 0 次）但字符串普查不担保产物字节；wheel 内 `.pyd` = `f88cd81076318c342659a35b3afb6b9d` 而 `target/release/_qianxing_native.dll` = `1b971292e74a2bbe2fd898b4a2c7d52c`，重打包时「dll → 包内 `.pyd`」那一拷贝步必须一起走。详见 `docs/自研量化框架审计与重构方案-V13.md` §9.54。**尚未 `git add`、未推送**：Rust 一侧、Python 一侧、门禁八颗、capabilities 那一条证据行与五份文档的活面全部仍在未暂存状态。


### V13 R5/R6 收口（2026-10-04）· 第 5、6 轮三扫 · 发布条件收敛

- **R5 落地的四颗**：`/ws` 的升级分支补上浏览器准入（此前 R1-B 那套 CORS 名单 / 预检 403 / 在途预算 503 / 握手 key 只长在 HTTP 那一臂上，跨源网页可带着凭据起一根有状态 WS——CSWSH），跨源·身份·握手 key 三种判定全部排到写 `101` 之前，握手 key 只读一次、缺失回 400 `missing_websocket_key`，`101` 带的 `Sec-WebSocket-Accept` 取自准入算出的那份会话，五条出口带同一份跨源头；`backtest book` 的 `--latency-snapshots` 越过「永远等不到成熟」那条线时从退 0 落一份空成交表变成本场拒绝并点名可用上限 `帧内快照数 − 2`；三条子进程管道逐行读补 16 MiB 行长闸（`read_capped_line` / `read_capped_worker_line`，越界即中止并点名，不截断——半行 JSON 交给解码器是更难归因的失败）；`qx outbox-relay` 三个出口都把 `parked` 念进摘要行，"这一页搬完了"不再被读成"中继干净"。
- **R6 的结论按两半写**：三扫在第 5 轮终树上**没有再发现可修的运行面缺陷（0 颗）**；剩下的是"有意边界"与"文档没跟上代码"两类。四条全局出口（`/health`、`/ready`、`/metrics`、`/schema/account-snapshot-v1`）不判查询串，裁定维持——它们是探针与网关的落点，判键等于给存活探测加一条 400 路径；接口文档现在明写这一支与"12 条读面入口 + WS 那一臂"的覆盖面。独立策略进程那段此前没写行长闸，现补齐两枚 16 MiB 常量与门禁的 1/2/0 调用形状判据。能力矩阵一处漂移重钉（provider 那四格字段挪成独立条目 `provider_capability_registry` 之后，README 六格按现读改成 **23 / 21 / 386 / 357 / 120 / 697**，并把"以仓库内路径开头"的口径点名为八个前缀——漏 `.github` 会把 357 读成 349）。
- **变异取证 16 枪**：现读 `fired=16 killed=14 equivalent=0 noconclusion=0 control_green=2 control_bad=0`，锚点普查 16、`restore=OK` 16（`ws.rs` / `admission.rs` / `lib.rs` 三份逐 sha 比回）。G1~G8 是 R5 的八条判据（其中 G4 是"行为等价、形状不等价"那一类——用例全绿而门禁 `601/1` 红；G5 额外被 clippy 抓住 `field 'accept' is never read`），H1~H6 是读面名册与 WS 查询串臂，C0/C1 两颗对照枪三腿全绿。
- **本轮自己的三处工具缺陷如实记**：取证脚本的三档恒等式漏算对照枪那一档，`14 != 16` 触发 exit 5，改五档后**从同一份日志重导**（没重开枪）；九腿驱动的 `--list` 分类器把 1057 行读成 289（`split(":")[0]` 撞上测试名里的 `::`），改成按行尾 `: test` 收口并同时报行数与唯一名数；本轮第一枪红在 clippy 的 `useless_format`，是本轮自己写的用例被抓，拆成 `&str` 字面量后 8 行全绿。
- **本轮实测（九腿整跑在终树上，读数全部由日志派生）**：树摘要九腿前后同为一个哈希、无 host-locked 腿、rc 全 0；`fmt --check` 干净；`clippy --workspace --all-targets -D warnings` 诊断 0 行；`cargo test --workspace` 107 段 / 1056 passed / 0 failed / 1 ignored，`--all-targets` 87 段同数，`--doc` 21 段 / 0 passed；`--list` 编译出 1057 行 / 1055 个唯一名（唯一那对重名是两份后端契约文件各写一遍的 `user_stream_runner_reconnects_*`）；磁盘在册 `#[test]` 1070、差 13 颗（nats 10 + postgres 3）；Python 腿 Ran 64 / OK；`validate_core.py` rc 0；门禁 rc 0、**[PASS] 602 / [FAIL] 0**，`GATE_CHECK_FLOOR` 不动——两轮 `#[test]` 净增 0、判据净增 0，四颗落地全部折叠进既有用例与既有判据。行数棘轮按 `--snapshot` 逃生门重登记（涨的三格是 `qx-adapter/src/lib.rs` 681→728、`qx-adapter/src/ccxt.rs` 1139→1144、`qx-cli/src/strategy_host.rs` 846→856）。详见 `docs/自研量化框架审计与重构方案-V13.md` §9.52 与 §9.53。尚未 `git add`、未推送。


### V13 R4 收口（2026-10-04）· 第 4 轮三扫 · API 与前后端贯通专项

- **落地的七颗**：CLI 位置参 `outbox-relay … 0` 不再"什么都不搬却退 0 报健康"（`parse_relay_limit` 挡 `1..=10000`，与配置孪生 `relay_batch_size` 同域）；缺 `Sec-WebSocket-Key` 的判定从 `serve_websocket` 提到 `admit_websocket`，握手之前就能回 400 `missing_websocket_key` 而不是掐断套接字；`schemas/strategy_api_v1.md` 那句"Python 侧不带 margin/position/leverage"按现读改口（Python 三格都带，只有 C ABI 头只带 `position_side`）；QIFI 信封在烟测链上第一次真的往返一遍（此前文档替它作保、代码零调用）；`JsonStateEnvelope` 从外销 `pub trait` 收为 `pub(crate)`；策略契约的 `uniqueItems` 折进既有用例拿到运行牙齿；两条 HTTP 读链补 16 MiB 总量界（`set_read_timeout` 只卡单个读窗口，慢速滴水上游可无限续期）。
- **登记的一颗**：API 事件读模型无淘汰（fam02）。本轮把"为什么不落"从裁定升级成技术理由——给 `Vec` 加长度上限写不出来：`project_event_log` 比较重叠前缀，尾部截断会把合法新事件判成「事件内容漂移」。需要的形状是 `retained_from_seq` 水位线，属策略决策。`ApiEventBus::publish` 那一半在 4096 处已淘汰。
- **按实测销案的两颗（假阳性，写明是为了下一轮别再捡）**：Binance `allOrders` 翻页 `loop` 有三条可点出口（页不满 / 游标未前进 / `checked_add` 溢出），人为页数上界会把"还没看完的资金事实"当"已核对"；Postgres 的 `Mutex<Client>` 全文件只有两处 `.lock()` 且都把毒锁映射成错误，锁内受 `statement_timeout=30s / lock_timeout=5s / idle_in_transaction=60s / connect_timeout` 约束。
- **变异取证 3 枪**：2 枪 KILLED、1 枪**无结论**。第一枪想把去重判据拆掉，写成 `insert(..) && false` 触发的是 `error[E0282]`（`BTreeSet` 推不出元素类型），整 crate 编译失败、`test result:` 没印出来——分类器读成"SURVIVED"是错的，那是断管道不是存活；重放成保留类型锚点后 KILLED。第二枪拆掉 HTTP 总量闸：KILLED。
- **本轮实测（10 腿电池，全部读数由日志派生）**：`cargo fmt --check` rc 0；`clippy --workspace --all-targets -D warnings` rc 0、诊断 0 行；`cargo test --workspace` rc 0、107 段 / 1056 passed / 0 failed / 1 ignored；`--all-targets` 87 段 / 1056 passed；`--doc` 21 段 / 0 passed（本仓无 doctest）；`--list` 编译出 1057 条；磁盘在册 `#[test]` 1070 颗、地板 1070，差 13 颗逐名可点；`python -m unittest discover -s python/tests` Ran 64（skipped=2）；门禁 rc 0、[PASS] 602 / [FAIL] 0，**本轮未新增判据**故 `GATE_CHECK_FLOOR` 不动。
- **两处口径随本轮改掉**：README 里「不带 `QX_PYTHON` 时那 2 条 Python 桥用例必红」按代码事实改口——解析是"`QX_PYTHON` 非空才用、否则回落 PATH `python`"，决定红不红的是回落点是不是可用解释器（本轮 `QX_PYTHON` 未设置、PATH 解析到 CPython 3.13.13，两条桥用例照跑照绿）；README 那句「`python/.venv` 本轮它在场」也已按事实改成"第 4 轮这遍它不在盘上，数字是用 PATH 上的解释器跑的"——这条风险第二次落地，正好说明 `build.bat`/`build.sh` 从 `[0/9]` 步探测解释器而不是信任 venv 是对的。
- **第三处口径纠正是本轮自己造的**（B 扫产物，记为 R4-D）：静态「门后」census 是**扫描器形状的读数**，四种形状本轮各现读一遍——三段齐 + 作用域按**花括号深度**收口 = 36 颗（sqlite 23 / nats 10 / postgres 3）；作用域只收到**列 0 的 `}`** = 44 颗（sqlite 31），**过计**（内联 `mod tests { … }` 里被门住那项的右括号落在列 4，门清不掉、漏到同块后面的用例上）；少认「**声明那个模块的行上的门**」那一段 = 30 / 38 颗，**少报**。判据取编译器真相：`36 − 23 = 13` 与「磁盘在册 − `--list` 编译出」同数，sqlite 那 23 颗逐名都在 `--list` 里、nats/postgres 那 13 颗一条都不在。本轮中途把 README 与终版计划 §1.1 写成过计那一格（44 / 31），已改回并把四种形状写进文档。
- **行数预算**：`crates/qx-cli/src/cli_args.rs` 513 行、`crates/qx-adapter/src/lib.rs` 697 行两颗越过 500 阈值，按该文件自订的逃生门 `--snapshot` 重登记，并核对预算名册全部 44 格与磁盘行数同数；`maturity/capabilities.yaml` 与 `crates/qx-storage/src/lib.rs` 的改动都是净零行数。尚未 `git add`、未推送。

### V13 R1 收口（2026-10-04）· 台账 #283 · 合流 `c07ad22` 回退面登记

- **登记（不重落）**：合流 `c07ad22` 按 `-s ours` 保留上游发布线，丢掉了本地 V11/V12/V13-R1 审计线的 135 份文件与 39 颗门禁判据。按用户裁定「以上游发布线为准（现状）」，本节把它们按 15 族逐名登记为**已回退，未排期**，口径不是「已修复」；名册写全在 V13 §9.48，取证目录不进仓库也不影响可读。
- **缺陷驱动落地的三颗**：控制面终态退场 + `RetirementSummary` 两条计数接上 `/metrics`；删掉零构造者的 `CommandStatus::Rejected`，终态词表收到 `is_final()` 一处；给 8 颗没有执行者的 `nats` 门后 `qx-storage` 用例在 CI 里补了一条非 ignore 的腿（本机实跑 8 passed / 1 ignored）。门禁新增 `command_status_vocabulary_single_source_check`，`GATE_CHECK_FLOOR` 515 → 602，`WORKSPACE_TEST_FLOOR` 866 → 1069。
- **`#152` 落地**：新增 `doc_citation_reachability_check`，活文档里每一处 `path:NN` 都要文件在盘上、行号不越界；`docs/archive/` 只降不升。
- **`#144` 关掉**：磁盘在册 `#[test]` 1069 颗 vs `--list` 编译 1056 颗，差 13 颗逐名点出（nats 10 + postgres 3）；README 里那条 `866 − 858 = 8` 的假恒等式删掉。同时纠正本轮自己的一处口径：静态 `#[cfg]` 扫描标出 36 颗「门后」，但 `qx-cli` 的 `default = ["sqlite"]` 让特性归一打开了 `qx-storage/sqlite`，那 23 颗其实照常执行。
- **本轮实测（7 腿电池，digests 稳定、无 host-locked 腿）**：`cargo fmt --check` 干净；`clippy --workspace --all-targets -D warnings` 诊断 0 行；`cargo test --workspace --all-targets` 87 段 / 1055 passed / 0 failed / 1 ignored；`cargo test --workspace` 107 段 / 同样 1055 passed；`python -m unittest discover -s python/tests` 64 条 OK；门禁 602 项全绿。
- **在册未修**：`docs/SECURITY.md` 已回退（全仓现读 35 处命中里 README 占 0 处，所以**没有**悬空指针——本条第一版写的「README 指向它的是悬空指针」是没现读就写下的假断言，已在 V13 §9.48 ② 改口）；新补的那条 NATS 腿只由 ci.yml 的文本存在性保证，门禁里没有判据核它（看守它的那颗在回退面里）；`cap_ledger_reading_check` 在回退面里，README 台账六格本轮按同一配方手工重钉（22 / 20 / 384 / 355 / 114 / 681）但没有牙齿。

### 量化场景探针与 Python/Rust SDK 结果契约修复（2026-10-19）

- 修复 qx-cli 回测装配构造改为 `Result` 后未更新的测试调用，以及 `AccountSnapshot::to_json()` 新错误类型导致的旧断言编译失败；多市场深度 Python outcome 补充可选 `execution_models` 字段，兼容既有无该字段响应。
- 探针改为每份冻结 BarFrame 使用隔离的数据注册表，并对齐当前 CLI 的 `[Strategy · Backtest]` 输出行，避免旧 manifest 冲突和过时正则产生假失败。
- `cargo test --offline -p qx-cli --no-run -j 1`、qx-python 原生扩展 wheel 构建、`cargo fmt --all -- --check` 通过；完整探针 **79 executed / 0 declared_not_executed / 0 missing / 1 blocked**（实盘外部前置）。

### 当前 Release A 股规则吞吐基线（2026-10-19）

- 以当前工作树重建 `target/release/qx-cli.exe`，同一 Ryzen 7 8700G/Windows 11 环境跑 1k/8k/64k 合成分钟 Bar，各 512 条 PIT 现金分红、5 次重复，绑定 A 股交易规则、费用与合成交易日历；P50/P95 分别为 304.459/313.926ms、350.769/353.258ms、616.956/672.753ms。每档重复运行 hash 稳定，单动作控制组 hash 不同。
- 证据 `maturity/evidence/ashare-rules-backtest-release-current-2026-10-19.json`。样本只用于当前终树吞吐基线，不是完整真实 A 股 profile；不改变 `maturity/targets.yaml` 九档 `unmeasured / unavailable` 状态。

### D16 全仓生产 unwrap/expect 清零复核（2026-10-19）

- workspace 全部生产 lib/bin 通过 Clippy `-D clippy::unwrap_used -D clippy::expect_used`；仓内 3 个 `build.rs` 逐文件正则检索无解包调用。该命令与构建脚本扫描已加入 CI，架构门禁同步核验其持续存在；门禁快照更新为 **1,200/1,200**。旧记录的 73 个命中及 qx-core 静态矩阵序列化 `expect` 已过期，当前生产闭集无残余项。D16 收口。

### D21 模块地图英文职能索引（2026-10-19）

- 为 26 个 crate/集成面补充一行英文职能说明，并在架构门禁中核对索引条目完整、唯一且为英文文本；四角色 CLI 指南和双语术语已有行为用例。门禁快照 **1,201/1,201** 通过。Rust 行为测试本轮未运行。

### D10 JetStream Outbox 发布复用稳定消息身份（2026-10-19）

- NATS publisher 将 Outbox `event_id` 作为 JetStream `Nats-Msg-Id`，使 ack 丢失后的 relay 重试能利用 stream 配置的有限去重窗口。EventLog 投影现以稳定 `log_name` 分区并写入源 `seq`；但 EventLog 尚无 append 顺序上的 seq 严格递增承诺，consumer checkpoint 接入及旧分区状态迁移仍未完成，因此不裁剪 `processed_ids`。qx-storage NATS feature 离线编译、全仓格式检查通过，架构门禁 **1,203/1,203**；未运行行为测试或连接 NATS 验收。

### 订单簿衍生品策略保证金快照（2026-10-21）

- OrderBook 回测的策略回调现在接收标记权益扣除已开仓保证金、在途订单保证金和预估费用后的模拟可用保证金；缺市场价时仍返回缺席，缺在途单风险策略时拒绝估算。该值不代表交易所 free-margin 快照。
- 定向复核：订单簿回测单测 24/24，`qx-xingban` 全目标严格 Clippy、估值单源与风险规格架构合同通过；未运行全量架构门禁或发布矩阵。

### Runtime 冻结 market spec 预检（2026-10-21）

- `config validate`/`runtime-check` 对启用 worker 已声明的标的，现在会通过唯一 market-spec 加载器校验文件内容和规格，而不止检查路径存在；进程监督器及独立 Binance、CCXT、Paper Execution/SpreadRecovery 启动路径也会在启动 worker 前执行该预检。未声明静态标的的 worker 仍在订单准入时按实际订单标的检查。
- 定向复核：预检有效/非法规格用例、deploy 模板覆盖 3/3、qx-cli all-targets 严格 Clippy 及运行时配置/路径/market-spec 架构合同通过；全量发布矩阵未运行。


### WP-7.68 · 清理成熟度台账中的历史门禁数量地板

成熟度能力台账仍残留历史 `GATE_CHECK_FLOOR` 与门禁总量递增描述，容易被误读为当前验收要求。已从当前能力台账移除这些过时数量阈值与快照措辞；活代码、CI、本地构建和成熟度工具中均无相关门禁或旧入口引用。保留具名行为/结构合同、实际测试执行及基准回归门禁，不改 A 股/币圈/Paper 验收链，也不删归档历史记录。

**定向验证**：当前执行路径检索无 `GATE_CHECK_FLOOR`、`WORKSPACE_TEST_FLOOR`、旧快照/行数预算和 `validate_core.py` 引用；完整架构及发布矩阵未运行。

### WP-7.69 · 运行时冻结规格与双市场回测定向复核

按当前工作树重跑运行时 market spec 预检回归，覆盖合法规格启动放行、非法精度配置拒绝及 runtime reference 诊断；对应架构合同检查运行时配置 fail-closed、market spec 单读者与路径所有权，共 18 项通过。A 股和币圈 CLI 确定性验收重新生成通过，hash 与已登记基线一致：`6be034a4cb0760fa` / `26fdd6b52d020700`。另重跑 A 股报告产品进程用例，验证多文档导出、便携证据重算以及同一输入比较，1/1 通过。

**定向验证**：`runtime_reference_preflight_parses_market_specs_for_declared_worker_symbols` 1/1；三组架构合同 18/18；A 股与币圈回测验收通过；`ashare_runs_export_documents_and_compare_on_the_same_input` 1/1。完整 workspace/全量架构门禁与三轮全链路审查仍待开发收敛后执行。

### WP-7.70 · Paper 本地执行闭环验收复跑

当前 qx-cli 二进制通过 Paper 端到端验收：提交 1 单、成交 1 单、生成 4 条账簿事实，重跑不重复下单，证据链可到 Executed。默认 Windows 临时目录的 Scheduler 状态文件遇到权限拒绝，改到事先确认不存在的 `target/qx-paper-acceptance-wp769-20261021` 隔离根后验收通过；该失败属于临时目录访问问题，未修改业务规则。

**定向验证**：`paper_acceptance.py` 退出码 0；严格 Clippy、A 股/币圈验收、A 股报告产品与运行时规格预检证据见 WP-7.69。本地外部 Venue/sandbox 尚未验收；全量 workspace/全量架构门禁与至少三轮全链路审查仍未完成。

### WP-7.71 · API/Console/SDK 当前树跨层复核

针对当前 API、控制台与 SDK 工作树改动重跑真实行为与结构合同：Python SDK 分层、应用用例、Provider 与新增回测 DTO/原生规格用例共 89/89；API 路由、Console 接线、研究菜单与 RunHandle、Rust/Python/C++ API 合同及 SDK 导出/认领合同共 105 项通过。`cargo test --offline -j 1 -q -p qx-api -p qx-sdk` 全部目标通过，qx-api 86 个单测和各 API/SDK 集成目标零失败；两个 crate 全目标严格 Clippy `-D warnings` 通过。重复测试没有发现新的断链或孤儿公开入口。

**边界/下一步**：WP-3 服务宿主目标仍未完全关闭，CLI `worker_entry.rs` 与 `venue_runtime/` 仍持有 venue 执行实现；该剩余项不能由这轮 API/SDK 用例替代。开发收敛后仍需完成终态三轮全链路发布复核与全量发布矩阵。

### WP-7.72 · Paper 验收目录隔离与非破坏性修复

Paper 验收脚本原先默认使用固定系统临时路径，且若目录存在就递归删除；这既与保留证据便于复核的说明不符，也在 Windows ACL 环境失败。现默认在仓库 `target/` 创建唯一临时目录；显式 `--root` 必须是不存在的新路径，已有目录会 fail closed 且保留原内容。架构 `paper_track_check()` 新增该路径安全合同。

**定向验证**：Paper 轨架构合同 8/8；Python 脚本语法检查通过；已有目录拒绝用例确认原目录 36 个文件未变；不传 `--root` 的完整 Paper 验收通过，OrderSubmitted=1、Filled=1、LedgerApplied=4，并写入当前成熟度记录。全量架构门禁与发布矩阵未运行。

### WP-7.73 · 服务层测试夹具 Windows 临时目录修复

`qx-service::backtest_bindings::builtin_snapshot_normalizes_strategy_identity_and_portable_frame_path` 曾将临时 BarFrame 写入系统 temp，并在当前 Windows ACL 下以 PermissionDenied 失败。夹具现改用仓库 `target/qx-service-tests` 下带进程号、纳秒时间戳及原子序号的新目录，并在断言后清理；不改生产路径或业务逻辑。

**定向验证**：`cargo test --offline -j 1 -q -p qx-service` 28/28；`cargo clippy --offline -j 1 -p qx-service --all-targets -- -D warnings`、`cargo fmt --check -p qx-service`、Paper 验收族架构合同、Python 脚本语法与成熟度生成一致性通过。WP-3 的 venue 领域 runner 迁移和开发完成后的全链路/发布矩阵仍未关闭。

### WP-3.41 · Worker pipeline 指标报告器迁入 qx-service

`PipelineMetricsReporter` 原由 qx-cli 持有，但其生命周期口径、worker-metrics 文件发布和 Prometheus counter 聚合都属于服务宿主职责。现将报告器迁入 `qx-service::pipeline_metrics_report`，依赖服务层唯一的 worker metrics IO；qx-cli 删除旧实现，仅重导出类型并继续把各 worker pipeline 观测交给它。同步更新 runtime 的生产出口说明、Grafana/字段文档及 pipeline 打开站点豁免文档；`health_snapshot_knob_check()` 新增唯一所有权判据，拒绝 CLI 旧实现复活。

迁移复核时 `paper_worker_publishes_pipeline_counters_to_its_metrics_file` 暴露一条旧断言：该夹具的四次 ingest 对应四次 refresh，订单准入在账户锁内另有一次显式刷新以追平锁等待期间的 EventLog，因此总刷新应为五次。行为逻辑正确，已将断言更新为 4 + 1，并在用例说明其新鲜度用途；ingest 仍精确为 4。

**定向验证**：`pipeline_metrics_surface` 7/7；pipeline 打开站点账本 2/2；`health_snapshot_knob_check()` 2/2；qx-service 28/28；qx-service/qx-cli/qx-runtime 全目标严格 Clippy 与格式检查通过。旧 CLI 报告器路径仅留在架构合同中用于确认其不存在。WP-3 其它 venue 领域 runner 所有权与终态三轮发布审查仍开放。

### WP-7.89 · A 股性能夹具改用沪深公开年度休市日历

将 A 股吞吐负载过去自动生成的 weekday-only 日期替换为版本化的沪深 2024-2025 交易日历夹具（485 个开市日），其节假日安排来自上交所、深交所当年年度通知；超过夹具日期范围会直接报错。性能证据分别绑定源夹具和实际传入 Rust 回测的裁剪日历摘要，保留合成分钟行情、未覆盖公告外临时休市与未覆盖完整规则分支的限制。

**定向验证**：夹具检查 3/3；以当前 Release 重建 qx-cli 后完成 1k/8k/64k 三档、每档 3 次与 4 条 PIT 公司行为的回测，P50 为 274.290/305.733/551.723ms，4 条动作相对单动作控制均改变结果哈希，A 股规则/成本绑定均被观察到。二进制 SHA-256 为 `af7801132b07c2c75d56a0e7490da42ccab8bf5c9acaace9ddfc3ddb5578ba0b`，证据：`maturity/evidence/ashare-rules-backtest-2026-10-21-calendar-release.json`。此吞吐证据不覆盖完整规则合同或 Paper 生命周期，`ashare-research` 仍保持 `unmeasured`；最终三轮全链路审查和全量发布矩阵继续开放。

### WP-4.14 · 最新 Release 重验币圈回测与 Crypto Paper 闭环

使用同一当前 Release 二进制重跑 Crypto 与 A 股仓内回测验收，结果哈希分别为 `26fdd6b52d020700` 和 `6be034a4cb0760fa`；两条路径都验证同目录重跑字节一致、独立目录产物一致及无凭据运行，并刷新两份成熟度验收记录。Crypto Paper E2E 以同一二进制重复 3 次，验证 scheduler→strategy→Paper execution→Ledger/EventLog/audit、幂等重跑及独立根一致；P50/P95 为 2862.415/2866.430ms，二进制 SHA-256 `af7801132b07c2c75d56a0e7490da42ccab8bf5c9acaace9ddfc3ddb5578ba0b`，证据：`maturity/evidence/crypto-paper-e2e-2026-10-21-current-release.json`。

此 E2E 不覆盖实时 funding Provider、8 小时 funding 调度和外部数据库，故 `crypto-paper` 仍为 `unmeasured`；`crypto-testnet` 还需要真实凭据/环境。完整三轮全链路审查及最终全量发布矩阵仍开放。

### 发布审查第 1 轮 · 回测至 API、Console 与 SDK 主链路

第一轮沿 Crypto/A 股样例回测、qx-app/共享撮合账簿、qx-api/Console、Rust SDK 与 Python SDK 顺链核验。最新 Release 上两市场回测验收、Paper worker E2E、qx-cli 回测合同 50 项、qx-api/qx-sdk 全目标测试与 Python SDK 测试均通过；Console API 的页面/路由/认证/跨源/关闭行为包含在 qx-api 实测中，控制台 JS 通过 Node 语法检查。没有发现策略到共享回测用例、RunHandle/API 或 SDK facade 的新断链。首次 CLI 合同测试受 Windows 系统临时目录 ACL 阻断，切换到仓库隔离临时目录后同一组 50 项通过。

**证据**：回测合同 50/50；qx-api 86 单测与全部集成目标通过；qx-sdk 全部 44 个测试通过（含 31 个独立消费者用例）；Python 测试 199/199；Node `--check` 与对应差异检查通过。此轮没有替代运行时无限等待审计、全量架构门禁、全 workspace 测试或完整 Release 矩阵；尚有至少两轮全链路审查与最终发布矩阵待完成。

### 发布审查第 2 轮 · Rust 核心账簿、风控、OMS 与撮合

逐条复核账簿开/平/反手的杠杆记录及保证金估值、统一 RiskGate、Bar/OrderBook 现货库存与衍生品保证金预留、OMS/Paper venue 状态与取消边界。发现跨 Bar 卖单预留已有集成回归，但缺少同 Bar 多笔卖单覆盖；补入 BacktestEngine 合同，验证第一笔在途卖单预留库存后第二笔被拒、成交后仓位不为负。代码审查未发现资金预留双计/漏计、借余额放行或会阻塞退出的循环路径。

**定向验证**：`cargo test --offline -j1 -p qx-core -p qx-risk -p qx-zhenlu -p qx-xingban --no-fail-fast` 全部目标通过；`cash_spot_sell_inventory` 3/3；四 crate 全目标严格 Clippy `-D warnings` 与 workspace Rust 格式检查通过。此轮未审 API 生产部署、进程异常恢复及全部 venue 外部对账；发布审查第 3 轮和全量发布矩阵仍待完成。

### 发布审查第 3 轮 · Worker 停机、API/Console 接线与多语言 SDK

沿服务宿主、RuntimeSupervisor、HTTP/Console accept loop、API 投影桥、同步/异步 RunHandle 与 Python/Rust SDK 回测入口检查等待与终态。补入 ConsoleFront 无连接停机回归，验证它在空闲 accept 轮询时会响应停止令牌并返回；原 API 停机、Worker 长间隔取消、投影桥停止合同已一并复测。Console 前端声明路由与 qx-api 路由双向对账，异步研究入口无后端孤儿启动路由，SDK facade 与 Python 分层公开面也通过定向检查。

**定向验证**：新增 Console 停机用例 1/1；qx-api/qx-runtime/qx-service 全目标测试与严格 Clippy 通过；前端/API/SDK/Worker 等六组架构合同共 74 项、0 失败；qx-sdk 44 项、qx-cli 回测合同 50/50、Python SDK 199/199 通过。三轮审查中的定向改动均已通过对应回归。全 workspace / 全量架构门禁 / Release 矩阵仍未执行；版本候选需待最终全量门禁并修复其发现项后再定。
