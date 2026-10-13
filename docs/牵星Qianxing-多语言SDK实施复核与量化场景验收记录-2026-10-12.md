# 牵星 Qianxing 多语言 SDK 实施复核与量化场景验收记录

> 日期：2026-10-12
> 对照方案：[牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11.md](牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11.md)
> 说明：本记录区分仓内可重复验证的能力、尚未完成的产品链路和依赖外部环境的实盘认证；本记录不将 Paper、mock 或本地测试等同于实盘认证。

## 1. 本轮改进

- Paper 算法父单现在由 API 受理并写入共享 checkpoint；现有 Paper Execution worker 分页发现父单，恢复 OMS/Paper Venue 状态后继续按新行情推进子单。父单查询从 checkpoint 和账户 EventLog 汇总状态、子单、成交量与剩余量。
- 父单 admission 检查启用的账户、Paper Venue、Execution worker 和完整标的身份；使用跨进程 admission 锁检查 EventLog 与活跃父单的子单 ID 范围，拒绝冲突。损坏或不可读的持久状态显式失败，不当作不存在。
- Paper Venue 支持从账户 EventLog 恢复订单和下一个 venue order ID，避免 worker 重启后复用模拟 venue 身份。行情消费游标持久化在撮合前，以阻止重启后重放同一条可能已部分成交的旧行情；崩溃恰落在该边界时可能漏掉一次 Paper 模拟成交，因此此语义不适用于认定 Live 交易事实。
- Python 与 C++ 远端 AppClient 增加 Paper 算法执行 start/get/cancel/list 四个方法，复用同一 API 端点；Python 不复制拆单、风控、订单状态或账簿规则。
- 场景探针此前有固定 Run ID 导致重跑与旧产物冲突的问题，也会用旧的静态 `missing` 项覆盖新算法执行证据。本轮改为唯一的做市 Run 身份，并把算法执行与 Python 十大策略通过真实测试生成证据；`quant_scenario_probe.py` 对外部命令使用显式 runner，失败时保留诊断输出。

## 2. 十大经典策略与共享内核

以下十种单腿策略已有 Rust 内核实现，并由 Python 原生 SDK 的回测用例逐个执行；每种策略都要求至少一笔成交且产生 16 位结果摘要。策略信号、撮合、风控、账簿和结果仍由 Rust 执行。

| 场景 | 共享策略 kind |
|---|---|
| 简单均线趋势 | `sma_cross` |
| 指数均线趋势 | `ema_cross` |
| 趋势与动量 | `macd` |
| 超买超卖反转 | `rsi` |
| 布林带回归 | `bollinger` |
| Donchian 通道突破 | `donchian_breakout` |
| 区间动量 | `momentum` |
| 均值偏离回归 | `mean_reversion` |
| 网格信号 | `grid` |
| ATR 趋势突破 | `atr_trend` |

Rust 内核当前另有 Keltner 趋势、VWAP 回归、波动率突破、配对套利、基差套利、跨交易所套利和现货/期货套利，共 17 个内置 kind。每种策略的有效参数旋钮通过单一清单登记，并由行为测试验证：清单内参数能改变策略时间线，清单外参数不能改变结果。内置策略目录与 17 项行为覆盖由 `crates/qx-strategy` 测试守护。

Python 十策略证据：

- 执行命令：`python -m unittest python.tests.test_app_use_cases.AppUseCaseTests.test_ten_classic_builtin_strategies_run_through_python_rust_kernel`
- 测试文件：[python/tests/test_app_use_cases.py](../python/tests/test_app_use_cases.py)
- Rust 策略目录：[crates/qx-strategy/src/builtin/config.rs](../crates/qx-strategy/src/builtin/config.rs)
- Python Rust bridge：[crates/qx-python/src/lib.rs](../crates/qx-python/src/lib.rs)
- 本机 Python 3.13 Windows `cp310-abi3-win_amd64` wheel 已构建、元数据检查通过，并在独立 target 目录安装后实际运行 Bar 回测（1 笔成交）。此结果只证明当前 Windows wheel，不代表其它 Python 版本/操作系统/架构。
- Rust Git SDK 消费者固定到已推送提交 `e9fa6ab23aa3f7a7dafbe50265674b8ffd24954d`，在 workspace 外构建并运行数据校验、回测和结果复核（1 笔成交），并编译调用 Paper start/get/cancel/list 与事件游标读取；见 `tools/check_rust_sdk_git_consumer.py`。
- Rust SDK `cargo test -p qx-sdk --offline` 的 4 个单元测试和 9 个独立消费者测试全部通过。Rust/Python/C++ 远端客户端已接 `/events/live` 序号游标，Python GET 行为与 Async 委托测试通过；C++ smoke 源码覆盖有/无游标，当前环境无 C++ 编译器。

## 3. 本轮场景执行结果

场景登记由 `python tools/quant_scenario_probe.py` 重新生成，覆盖 31 项本地可执行场景：经典策略、套利、盘口撮合、做市、多语言策略、参数网格、仓位 sizing、止损止盈、样本外走查、基准对照、风险平价、算法单执行和 Rust Git SDK 仓外消费者等。当前全部可本地执行项均有正向数值或专项测试证据，见 [maturity/quant_scenarios.yaml](../maturity/quant_scenarios.yaml)。

算法单闭环专项验证：

- 测试：`cargo test --offline -p qx-cli paper_worker_resumes_twap_children_across_worker_restarts_and_fills_from_new_quotes -- --test-threads=1`
- API start 创建 1 个父单；worker 经 4 次进程/单次执行推进后生成 2 个子单；新行情产生总计 `4,000,000,000` raw 成交量；API get 返回 `completed` 且剩余量为 0。
- 无新行情重复运行 worker 时不重放旧报价；跨 worker 重启不会重复创建子单或 venue order ID。
- API 路由读写权限由 `crates/qx-api/src/algo_execution_tests.rs` 验证；Python 方法由 Python app 用例验证。C++ smoke 源码检查远端方法到路径的映射，但本机没有可用 C++ 编译器，尚未获得 C++ 编译证据。

## 4. 当前状态与未通过的发布条件

| 能力 | 当前状态 | 尚需验证或补齐 |
|---|---|---|
| Rust 确定性 Bar/深度/多腿回测与参数实验 | 本地代码和场景证据通过 | 全平台安装后外部消费者验证仍需 CI 产物 |
| Python 原生 SDK 十大经典策略 | 本地 Rust 扩展测试通过；Windows abi3 wheel 安装后 smoke 通过 | CPython/操作系统/架构完整 wheel 矩阵 |
| Python/Rust/C++ 远端 Paper 算法单操作 | 三种语言均声明 start/get/cancel/list 入口；Rust 方法和序列化/错误映射通过本地 SDK 测试 | C++ 编译与真实服务端消费者 E2E；取消/分页端到端场景还需补充 |
| 多交易所、多数据源 | 仓内有 Provider/Venue 接口与多条已执行研究场景 | 每个交易所、市场、数据源的能力仍须独立验证；CCXT 覆盖不能代表所有交易所 |
| 多账户与多策略并行交易 | 有账户日志隔离及多策略运行机制 | 组合级风险聚合、竞争资金调度及完整 Web 操作流程仍未形成统一 E2E 证据 |
| Live 下单、查询、撤单与对账 | 适配器实现存在，但未认证 | 当前无 CCXT 运行依赖和交易所 testnet 凭据；不得标记为已通过或 GA |
| C++ SDK 发布 | 远端契约与 smoke 源码已更新 | 本机没有 C++ 工具链；CMake 安装包及独立消费者尚未构建验证 |
| Rust SDK 发布 | `qx-sdk` 进程内门面、Paper HTTP 生命周期 facade 与 workspace 独立消费者测试存在；Git 固定提交仓外消费已验收 | crates.io 内部 crate registry/发布顺序及安装验收未完成 |

因此本轮完成了共享 Paper 算法单链路、Python 十策略验证和场景证据修复，但目前证据不足以将整套多语言 SDK 或 Live 交易宣布为发布就绪。应继续保留上述阻塞项及 capability matrix 的限制状态。

## 6. 2026-10-13 继续实现：SDK 多策略研究候选选择

- `RunExperimentSpec.strategy_kinds` 已进入共享 Rust `qx-app` 应用用例，可在同一份数据和基础策略参数上选择多种单腿内置策略，并由原有 Rust 回测内核执行、汇总和比较；每个候选返回策略 kind，排序和实验指纹按规范化 kind 顺序保持确定性。
- 选择器支持不带数值参数维度的策略快速比较，并与现有参数维度及 `split_plan` 组合；候选总数受 256 上限约束。OOS 每折仅以训练窗收益选出策略 kind 和数值参数，再在测试窗评估；报告记录 `selected_strategy_kind`。重复 kind、双腿策略仍显式拒绝。
- Rust SDK 独立消费者与 API `/app/run-experiment` 路由有覆盖；Python 类型化 SDK、原生 Rust 扩展、远端 AppClient 都序列化相同字段；C++ HTTP 客户端以既有通用 JSON 载荷转发同一契约。Python 全量单元测试 124 项通过（1 项因环境跳过）；专项验证十种经典策略均返回成功候选并进入统一比较。
- `cargo fmt --all -- --check`、`git diff --check` 通过；Rust `qx-app`/`qx-sdk`/`qx-api` 本地套件和 Python 全量 124 项（1 项跳过）通过。推送后固定到新提交的 Rust Git 仓外消费者实际构建并运行成功（1 笔成交，数据复核成功，Paper 远端 start/get/cancel/list 及事件游标均调用通过）。量化场景探针 31 项 executed、0 项缺失、0 项未执行；真实交易所 Live 场景仍因没有 `ccxt` 与 testnet 凭据 blocked。
- 最终提交 `ee6e038` 的 Windows `cp310-abi3-win_amd64` wheel 已通过 `tools/check_python_wheel.py`，并在隔离 Python 3.13 环境离线安装；安装后从 SDK 调用共享 Rust 内核完成十种经典策略比较，10 个候选全部成功。wheel SHA-256：`a1b09eda3af487bd0887e187d5b13f9719c23f129af93123a089ff002b7742a7`。
- 该选择器解决的是同一数据集上的独立研究候选批量运行，不代表多策略共享资金下的组合调度，也不构成 Python/Rust/C++ 同时具备本地原生 API 的证明。多账户统一资金竞争、组合风险/账簿、组合回测与 Paper/Live 工作流、真实交易所认证仍未整体贯通；本机没有 C++ 编译器，C++ 尚无编译验收证据。

## 7. 2026-10-13 继续改造：Python wheel 使用稳定 ABI 构建器

- 将 Python 发布工程切换到 PEP 517 Maturin mixed-project 后端，明确 Rust manifest、Python 源目录、原生模块名和需打包的 Python namespace；构建时由同一后端编译 Rust 扩展并产出 PyO3 `abi3-py310` wheel，避免 setuptools 将 `.pyd` 当成普通资源后标为 `py3-none-any`，也避免复用工作区中的过期扩展文件。
- 简化 Windows/POSIX wheel 构建脚本为构建前端检查与 PEP 517 构建；CI 和 Release 工作流显式安装 maturin。架构审计与 Rust 文档用例覆盖两个脚本的 pip/maturin 前置检查、后端及模块契约。
- 标准 Windows 构建脚本产出 `qianxing-0.1.0-cp310-abi3-win_amd64.whl`，稳定 ABI 标签/原生 `.pyd` 检查通过；隔离 Python 3.13 环境离线安装后执行十策略快速比较，10/10 成功。最终推送提交 `ee6e038` 对应 SHA-256：`a1b09eda3af487bd0887e187d5b13f9719c23f129af93123a089ff002b7742a7`。
- 该证据只覆盖当前 Windows x64 wheel。Linux/macOS 构建依赖 CI，C++ 本地编译仍缺工具链；多策略共享账户/组合风险账簿、前端工作流和真实交易所 testnet 依旧未形成完整发布验收。

## 5. 2026-10-13 继续复核：门禁与核心 SDK 用例

- 重新运行 `python tools/quant_scenario_probe.py`：31 项本地场景全部执行通过，0 项缺失；1 项多交易所 Live 场景因 `ccxt` 运行依赖与 testnet 凭据缺失保持 blocked。结果更新到 `maturity/quant_scenarios.yaml`。场景包括十种经典策略、多语言策略、Bar/深度/双腿回测、参数对比、Paper 算法执行、风险平价与资金规划等。
- `python tools/check_architecture.py`：1096 项全部通过。
- `cargo test --offline -p qx-app -p qx-sdk`：qx-app 83 项单元测试、2 项外部消费者测试；Rust SDK 5 项单元测试、1 项资金规划消费者、10 项独立消费者、2 项 Provider 消费者测试全部通过。
- `cargo test --offline -p qx-api`：58 项单元测试及 API 集成测试全部通过。首次未设置临时目录时，4 项依赖文件系统的测试受 Windows 默认 Temp 目录拒绝访问影响；将 `TEMP`/`TMP` 指到仓库 `target/test-temp` 后全量重跑通过。该限制属于本机测试环境，未伪报为产品行为缺陷。
- 本轮重新核对确认一个未闭环的主链：仓内 Runtime/CLI 能运行多策略配置，但 Rust/Python/C++ SDK 与 HTTP/Web 稳定应用契约没有同一多策略组合回测用例；现有 `qx-app` Bar `BacktestSpec` 仍是单策略运行。组合资金分配也尚未与策略调度、共享组合风险、组合账簿、Paper/Live 订单执行连接。因此“SDK 多策略组合实盘/回测全部贯通”仍为未完成。
- 本轮没有引入第二套撮合/账簿算法，也没有将 Runtime/CLI 的场景执行误记成 SDK 接入。下一阶段应先把组合调度与共享账簿能力收敛成有明确所有权的 Rust 应用用例/领域端口，再逐一接入各语言和 API，并以跨语言相同输入、相同结果与事件/账簿核对作为退出门。


## 8. 2026-10-13 增量复核：策略选择与 OOS

此前策略 kind 快速选择无法与 `split_plan` 组合，导致十种策略无法在训练窗中统一选优并进入样本外评估。本轮删除该互斥限制：候选策略/参数都由共享 Rust 执行，按每折训练窗收益挑选，再报告测试窗收益和所选策略 kind。Python 旧报告缺少可选字段时仍可解析。

验证覆盖 Rust qx-app、Rust SDK、HTTP API、Python 原生扩展与 Python 类型解码；API/Python 用例均检查折报告 kind 与候选 ordinal 关联。Rust/Python 追加 walk-forward 逐折验证：90 根 Bar 的 Rust 用例产生 7 折，Python 十策略样例产生 2 折，逐折选择 kind 均与候选序号相符。该修复只补研究实验选择与 OOS 接线，不代表多策略共享资本、组合级风险/账簿或 Paper/Live 多策略闭环已经完成；总体发布门槛仍未满足。

## 9. 2026-10-13 继续实现：组合成交归属路由

- `PortfolioBarStrategy` 之前把每笔成交广播给全部子策略，要求每个自定义策略自行过滤 `strategy_id`。这会让无关策略观察或处理别人的成交，并把归属一致性分散到插件实现中。本轮将成交回调改为按订单 `strategy_id` 精确路由；缺失或未知归属转为对账错误。提交时由组合调度器补齐缺失策略 ID，并拒绝与当前调度槽冲突的 ID。
- 单策略回测也复用组合适配器。首次全量三入口回归发现，该适配器会把单策略保护性退出的合成 ID 当作未知策略。本轮为唯一子策略保留专用回调路由；真正多策略的账户净持仓保护退出仍在规格校验阶段拒绝，避免把未能归属的退出成交写进某个策略虚拟持仓。
- 新增组合回调隔离、自动归属和错配拒绝用例。`cargo test --offline --workspace -- --test-threads=1` 全部通过；`cargo test --offline -p qx-cli --test app_three_entrypoints -- --test-threads=1` 的 16 项 CLI/HTTP/Python 契约均通过；`python -m unittest python.tests.test_sdk_layering python.tests.test_app_use_cases` 37 项通过；架构门禁 1096 项通过。
- 使用 Maturin 重新构建 `cp310-abi3-win_amd64` wheel，稳定 ABI/扩展检查通过，并在隔离虚拟环境离线安装、导入 Python SDK 与 Rust 原生扩展成功；当前构建 SHA-256：`7aeb0e011c0d811df45945aa41d95483452b4bbf552027f6d80d21a812ac21ea`。该 wheel 仅证明当前 Windows x64 包；没有据此推断其他平台或 C++ 包已验证。
- `reserved_capital_raw` 已升级为实际策略额度：按策略已成交持仓和未成交挂单的确定性顺序计算峰值单向暴露，现货以可见价格名义额、衍生品以产品初始保证金核算；单笔意图超额会拒绝，跳空后成交价使额度超限则在账簿入账前失败关闭。策略组合拒绝 Hedge 持仓模式；手续费与总账户现金仍由共享账户资金闸门约束。
- 新增固定数量订单超预算拒绝、未成交限价单持续占用预算、成交跳空超预算失败关闭用例。每笔待成交订单独立保存参考价和杠杆，预算校验按确定性接受顺序逐笔计算，避免用后一笔低限价低估前面高限价挂单的需求。现有 Tick/OrderBook、多腿/多市场、多账户组合账簿与策略级 PnL/费用/回撤归因、Paper/Live 多策略流程和 Web/桌面组合配置仍未闭环。Live 仍缺外部 testnet 认证，C++ 本机缺编译工具链。本轮不满足整体发布门槛。

## 10. 2026-10-13 推送与发布包复核

- 策略额度保护及 Windows 场景探针环境变量修复已随提交 `8fe49f1` 推送到 `main`。
- 验证：`cargo test --offline -p qx-xingban --lib -- --test-threads=1`（98 passed）；`python tools/check_architecture.py`（1096 项通过）；`python tools/quant_scenario_probe.py`（31 executed、0 missing、0 declared_not_executed，1 个 Live 场景因缺少真实交易所凭据/testnet 被 blocked）；`git diff --check` 通过。
- 推送后重建 Windows `qianxing-0.1.0-cp310-abi3-win_amd64.whl`，wheel 检查通过，在隔离 Python 3.13 环境离线安装并成功导入 `qianxing`、`qianxing_bridge` 和 Rust 原生扩展。SHA-256：`785be855c1a6b9a779fae15d2e89a0dab50e43085215523849e256ac685ca1c7`。
- 本轮不证明全能力发布就绪：多账户资金隔离/组合风险与策略归因、Tick/OrderBook/多腿/多市场组合调度接入统一应用 SDK、Paper/Live 组合流程、Web/桌面组合操作、C++ 独立安装消费者、Linux/macOS wheel 矩阵和 Live testnet conformance 仍未完成。

## 11. 2026-10-13 继续实现：Tick/OrderBook 同账户策略组合

- 将 Bar 已有的组合预算账本扩展为 Tick 与 OrderBook 引擎可复用的策略额度账本。L1 Tick 仍适配到共享订单簿撮合器；L2 直接使用共享簿撮合器；两种路径都继续经过同一风控、OMS、账户 Ledger 和 ReplayVerifier，没有增加另一套撮合或记账实现。
- `DepthBacktestSpec.portfolio` 支持主策略和附属单腿内置策略按优先级运行。共享组合适配器把子策略本地 client ID 映射成组合全局 ID，提供各策略的虚拟持仓与归属成交回调，并在接受挂单时冻结预算、撤单时释放预算、按真实成交价在记账前复核额度。缺失/冲突所有权、超预算与不支持的持仓模式均失败关闭；摘要和 fills.csv 写出策略归属。
- Rust qx-app 用例验证 L1/L2 均通过共享 Tick/OrderBook 引擎，Rust 内核用例覆盖组合订单隔离、预算超额拒绝和订单归属回调；Python 类型 SDK 新增深度组合规格与产物解码验证。跨入口用例验证 CLI、HTTP、Python 对既有单策略深度回测及参数实验返回一致哈希；该轮也实际发现虚拟环境扩展陈旧造成 20/24 个事件的差异，重建并更新 Windows `cp310-abi3` wheel 后，两条跨入口契约均通过。
- 验证：`cargo test --offline --workspace -- --test-threads=1` 全部通过（含跨入口、CLI、HTTP、Rust SDK 与 PyO3 native 契约）；`cargo test --offline -p qx-xingban --lib -- --test-threads=1`（100 passed）、`cargo test --offline -p qx-app --lib -- --test-threads=1`（89 passed）、深度 SDK 跨入口契约（3 passed，新增组合规格 CLI/HTTP/Python 同哈希与逐策略成交归因）；Python 全量 125 项通过、1 项环境跳过；策略 crate 27 项通过；`cargo fmt --all --check`、`git diff --check` 通过。架构不变量门禁 1096 项通过。首次 Python 全量测试遇到一次 Windows 临时文件句柄清理竞态，专项重跑与全量重跑均通过。严格 Clippy 尚未全仓通过：存在已有的 `qx-xingban::market_making` 测试模块位置、`qx-runtime::algo_checkpoint` `map_or`、`qx-app::experiment_split` 类型复杂度及组合构建器参数数目告警；本轮没有放宽这些告警门禁。
- 量化场景探针 32 项已执行、0 missing、0 declared_not_executed；新增 `depth_strategy_portfolio` 场景有 2 个策略分别产生 4 笔归属成交。唯一 blocked 场景仍为 CCXT/Live：环境缺少 CCXT 运行依赖与交易所 testnet 凭据。
- 本轮支持边界是单账户、单市场、单标的、内置单腿策略的 Bar/L1/L2 组合回测；仍未实现多腿/多市场组合调度、跨账户组合账簿/净额、策略级权益/费用/回撤及聚合组合风控、Paper/Live 多策略工作流、Web/桌面组合配置。真实交易所认证、C++ 构建与 Linux/macOS 消费者安装也未由本轮验证，因此整体方案仍未达到全能力发布门槛。

## 12. 2026-10-13 继续实现：深度数据策略选择与参数比较

- 将 Bar 实验已有的 `strategy_kinds` 维度接入共享 Rust Tick/OrderBook 实验用例。候选按规范化的策略名和参数网格顺序稳定展开，策略和参数总候选数统一受 256 上限约束；双腿策略被明确导向双数据集多腿用例，做市网格不与内置信号策略选择混用。候选报告新增可选 `strategy_kind`，不带该维度的旧规格序列化与指纹保持不变。
- Python typed SDK 增加策略 kind 输入和结果解码。CLI、HTTP API、Rust 本地 SDK、Python 原生扩展/高层 SDK 均调用同一 `qx-app` 用例；C++ HTTP 门面测试用真实规格验证请求原样传递。没有新增撮合、风险或账簿算法。
- 新增十种经典策略 L2 行为用例、策略×参数笛卡尔积用例、重复/双腿选择拒绝与确定性重跑；CLI/HTTP/Python 同规格跨入口对照结果哈希、排名和候选标识；Rust 独立 SDK 消费者直接执行十种策略。Python 类型用例也覆盖十策略快速选择。
- 场景探针新增 `depth_strategy_selection` 并由真实 CLI 执行生成场景登记：L2 十种策略得到 10/10 成功候选、合计 40 笔成交、稳定顺序及逐候选结果哈希。当前场景登记为 33 项 executed、0 missing、0 declared_not_executed、1 项 Live 因 CCXT/testnet 凭据 blocked。
- 最终验证：`cargo test --offline --workspace -- --test-threads=1` 全工作区通过；`cargo test --offline -p qx-cli --test app_depth_entrypoints -- --test-threads=1`（4 项通过）；Rust SDK 独立消费者（12 项通过）；Python 全套测试（125 项通过）；架构不变量门禁（1096 项通过）；格式检查与 `git diff --check` 通过。推送 `8e8d7d9` 后重建 Windows x64 `cp310-abi3` wheel，稳定 ABI 检查通过，并在隔离虚拟环境无依赖离线重装、导入原生扩展并确认深度实验入口存在。SHA-256：`B2A2A3F36F7E5130AFDCBFB75CD249D49DC54817304708AB1BFE3790496962CB`。
- Windows 本机未提供 C++ 编译器，因此 C++ 消费者只完成请求契约更新，未声称编译通过。其他平台 wheel、真实交易所 testnet、跨账户组合账簿/风险归因、多腿/多市场组合调度、Paper/Live 组合工作流及 Web/桌面组合端到端流程仍未通过验收；本轮没有把局部策略候选接通夸大成整体方案达到发布门槛。

## 13. 2026-10-13 继续实现：多腿套利策略选择

- 扩展 `RunMultiLegExperimentSpec`：可将四种需要 reference leg 的内置套利策略与公共数值参数网格按确定顺序组合，最多 256 个候选。非双腿策略与重复 kind 在启动前拒绝；候选报告记录策略 kind；不指定策略维度时保留既有规格 JSON 和指纹形状。全部候选继续逐个进入已有双腿回测用例，没有新增执行或收益计算内核。
- 接入 Python typed SDK 规格及候选结果解码、Rust SDK 本地 façade，并验证 CLI/HTTP/Python 对同一双数据集候选给出一致顺序、组合收益和双腿哈希。C++ HTTP 消费者测试也携带多腿策略候选规格并断言原样透传；当前机器没有 C++ 编译器，不能以源码断言替代编译证明。
- 新增全部四类套利策略选择、排序规范化、重复/单腿 kind 拒绝、旧规格编码兼容及策略×参数笛卡尔积测试。量化场景探针新加 `multi_leg_strategy_selection`：4/4 候选成功，双腿合计 16 笔成交，并记录每个候选的主腿/参考腿结果哈希。量化场景登记现为 34 executed、0 missing、0 declared_not_executed、1 blocked（真实交易所 Live 缺依赖与 testnet 凭据）。
- 最终回归：`cargo test --offline --workspace -- --test-threads=1` 通过；Rust qx-app 多腿实验 4 项、CLI/HTTP/Python 多腿策略选择对照 1 项、Rust SDK 独立消费者 1 项通过；Python 全量 125 项通过；架构门禁 1096 项通过；Rust 格式、场景脚本语法与 diff whitespace 检查通过。推送 `c279947` 后重建 Windows x64 `cp310-abi3` wheel，检查器通过；隔离环境离线重装后导入原生扩展并确认多腿实验入口存在。SHA-256：`7DF26B1098D7571CF5ACF638A9E5FB444D02EBCE3B5B2F7A3D188A442CA1859F`。
- 本轮接通的是逐个多腿研究候选选择，不是并行多腿组合资金池。多标的/多市场同一组合调度、跨账户共享资金与净额、策略级 PnL/费用/回撤及组合风险限额、Paper/Live 多策略执行、Web/桌面工作流仍未实现或未验收；整体多语言全能力方案因此仍未达到全量发布门槛。

## 14. 2026-10-13 继续复核：C++ Run 路径段隔离

- 检查 C++ HTTP façade 时发现 `fill_run_id` 将调用方字符串直接拼到 URL 路径中。合法 Run ID 虽受服务端格式校验，但非法值中的 `/`、`?`、`#` 会先改变客户端请求路径，可能触发错误端点或查询参数注入，给跨语言 SDK 留下不一致的失败行为。
- C++ 路径构造现按 RFC 3986 对非 unreserved 字节做百分号编码；合法 ID 字符保持原样。新增 smoke 断言，覆盖 `../other?admin=true#fragment` 被限制为单一路径段的情况。
- 首次发现新增断言虽在 smoke 源文件中，CI 只构建未执行该二进制，属于孤儿测试。本轮已把 `qianxing_app_client_smoke` 加入 Linux/Windows/macOS C++ CI 执行矩阵。本机没有可用的 C++ 编译器或 CMake，远端 CI 尚未完成前不声称修复已编译/运行通过。
- 本修复不改变总体能力状态：跨账户/多市场组合账簿与聚合风险、Paper/Live 多策略流程、Web/桌面 E2E、独立 C++ 安装消费者、Linux/macOS Python wheel 以及真实交易所 testnet conformance 仍未通过验收；全量发布门槛仍未达到。

## 15. 2026-10-13 继续实现：Paper 多策略验收跑全 worker

- 复核 `run_paper_pipeline_once` 时发现它只执行配置中的第一个 Strategy worker，因此同账户多策略拓扑可能只运行一个策略，却被这个端到端入口报告为通过。现已改为执行每一个启用的 Strategy worker；每个本轮新增订单都必须有对应 `SubmitOrder Executed` 审计记录，避免只检查第一张订单导致漏验。
- Paper E2E 测试拆成独立测试模块，并改成两个 strategy worker 和两个 scheduler job，共享同一 Paper Execution 与账户 Ledger。实际结果为两条策略均有成交（orders=2）、共享账簿有 7 条新增分录；同日重复运行新增订单为 0，且每笔成交保留自己的策略归属。
- `tools/quant_scenario_probe.py` 新增 `paper_multi_strategy`，本地完整探针现记录 35 项 executed、0 项 declared_not_executed、0 项 missing、1 项 blocked（Live 缺真实 testnet 前置）。这只验证同一账户、同一市场的 CLI Paper 多策略主链；它不证明多账户/多市场组合，也不替代 SDK/API/Web 的多策略操作验收。
- 本轮专项测试和 `cargo test --offline -p qx-cli -- --test-threads=1` 全套通过；架构不变量门禁 1096 项通过；场景探针 35 executed、0 declared_not_executed、0 missing、1 blocked；`cargo fmt --all -- --check`、Python 探针语法、能力/场景 YAML 解析及 `git diff --check` 通过。后续仍须对 Rust/Python/C++ SDK 和 HTTP/Web 使用同一多策略 Paper 规格完成跨入口相同账簿/事件核对，再扩展多市场/跨账户组合风险与归因。

## 16. 2026-10-13 复核：场景状态与三轮主链验收

- 重跑 `python tools/quant_scenario_probe.py` 后，登记面从 35 项更新为 **36 项 executed、0 项 declared_not_executed、0 项 missing、1 项 blocked**。新增/确认项覆盖 10 种经典单腿策略、多腿套利、跨账户独立回测、Tick/OrderBook、盘口做市、多策略额度、参数搜索、仓位 sizing、止损止盈、OOS/WFA、基准、组合再平衡、Paper 多策略及算法单。探针未联网、未使用凭据、未提交真实订单；唯一 blocked 为需要外部 CCXT/testnet 的 Live 验收。
- 三轮检查按“场景与架构基线 → 全量行为测试 → 最终静态/质量门禁”执行。架构门禁两次各 1096 项通过；第二轮 `cargo test --workspace --offline -- --test-threads=1` 全工作区与 doc-tests 通过，Python 全量 127 项通过；第三轮 `cargo clippy --workspace --all-targets --offline -- -D warnings` 通过。没有因零失败就把外部验收记成通过。
- 核心量化研究与 Paper 场景当前有可重跑的执行证据，但**全方案尚未达到发布门槛**：ProviderRegistry 尚未统一驱动所有 CLI/HTTP 和实时 Paper/Live 数据入口；Provider 的 `required_auth_scope` 是声明能力筛选，不能替代真实凭据授权；跨账户/跨市场组合风险、共享资金/账簿归因和多策略 Paper/Live 操作链仍未形成统一跨语言 E2E；真实交易所 Live testnet 与全订单状态/对账认证仍 blocked。C++ 可用 CI 矩阵验证源码/构建，但本机没有编译器，且不能据此宣称所有外部安装消费环境已验证。
- 本轮只更新由探针生成的 `maturity/quant_scenarios.yaml` 和本复核记录，没有修改交易/回测实现；因此本轮验证证明的是当前提交代码的状态与场景覆盖，不将文档更新包装成新增功能。下一实现应优先收敛 Provider→冻结数据→共享应用回测的统一用例，再接入 Rust/Python/C++、CLI/API/实时 worker，并补跨账户组合风险及账簿主链；每段都必须有同输入的跨入口结果/事件核对。

## 17. 2026-10-13 继续实现：双腿回测接入 Rust/Python 策略回调

- 新增 Rust 应用层和 `qx-sdk::Client` 同步自定义策略入口；`MultiLegBacktestSpec::start_with_strategy` 提供可取消 RunHandle。Python 原生 SDK 增加本地同步及句柄式异步回调入口；调用仍进入既有两腿 Bar 撮合、回放与归因，不在绑定层复制撮合。
- 策略 ID、版本与源码 SHA-256 校验并纳入 run 配置指纹及归因产物；同一 run_id 用不同源码身份重跑会返回 Conflict。策略决策按 Rust StrategyDecision 契约校验；意图标的超出两腿或携带当前目标仓位模拟器不能表达的订单限制时 fail-closed；取消调用 on_stop。
- 新增 qx-app 行为测试、Rust SDK 独立消费者测试和 Python 原生扩展端到端用例，覆盖双腿都产生成交、可取消句柄、身份改动冲突与安装形态扩展入口；语言能力映射、用例登记、README 已同步。
- 已知边界：此接口先按两个标的的 Bar 事件产生目标仓位，再交由两条独立 Rust 账簿撮合；策略回调暂不接收真实成交/订单更新，跨腿组合资金、保证金净额和实时风控仍未合并成单一组合账簿。C++ 本地回调、HTTP/Web 的用户策略上传/执行也未接通。因此这项接入不等于多腿策略实盘闭环或全部多语言 SDK 达到发布门槛。

## 18. 2026-10-13 继续实现：自定义策略进入双腿参数实验

- 为 Rust `qx-app` 与 `qx-sdk::Client` 增加双腿参数实验策略工厂入口；同步和 RunHandle 路径都复用同一实验展开、Rust 双腿回测、结果排名和报告产物。
- 每个候选均将已应用参数的 `MultiLegBacktestSpec` 传给 factory，要求调用方为该候选创建新策略实例。策略 ID/version/source SHA-256 加入实验指纹，并沿用到每个候选 run 身份；同一 experiment_id 改源码会在候选重跑前 Conflict。自定义 factory 与内置 `strategy_kinds` 互斥，候选总数仍受 256 上限约束。
- Python 原生扩展与高层 `qianxing` 增加同步及可取消句柄入口；factory 收到每个候选的 JSON 规格字典。测试验证数量参数分别作用于候选、每个候选双腿都有成交、Rust/Python RunHandle 成功、源码变更冲突及既有内置策略选择回归。
- 验证：Rust qx-app 多腿实验 5 项通过；Rust SDK workspace 外 consumer factory 用例通过；Python 原生扩展多腿同步/RunHandle 回归通过；`cargo check -p qx-app -p qx-sdk -p qx-python --offline`、Rust 格式、Python 语法和 diff whitespace 检查通过。首次 Rust 测试使用系统 TEMP 遇 Windows 目录权限失败；将 TEMP/TMP 指向 `target/test-temp` 后专项用例全通过。
- 边界：策略 factory 仅在本地 Rust/Python SDK 运行，不能序列化为 HTTP/C++ 远端策略；仍不支持策略组合 callback。双腿回测仍按各腿独立账簿执行，策略回调不接收成交回报；跨腿净额资金、实时风控、Paper/Live 多策略交易和真实 venue testnet 仍未闭环。

## 19. 2026-10-13 继续实现：自定义主策略进入同账户策略组合

- 解除 `StrategyArtifactIdentity` 对 Bar `portfolio` 的无差别拒绝。Rust/Python 自定义 callback 可作为组合主策略；附属槽继续由内置 Rust 策略实现。组合预算、优先级调度、风险、OMS、撮合、共享账户账簿、策略成交归属与 ReplayVerifier 均沿用既有 Rust 实现，绑定层不复制交易逻辑。策略源码身份仍进入 Run 配置摘要。
- 不允许 callback 使用非默认内置信号/仓位参数，避免这些参数看似配置成功、实际由 callback 忽略。该路径支持一个自定义主策略加内置附属策略；callback-to-callback 组合、远程 callback 序列化和 Tick/OrderBook 自定义组合暂未实现。
- 新增 Rust qx-app 与 Python 测试：自定义主策略和内置附属策略都产生带 owner 的成交；同步与 RunHandle 路径均通过；成交产物可复核且共享事件/账簿重放通过。场景探针新增 `python_custom_strategy_portfolio` 并实际执行，登记现为 37 项 executed、0 declared_not_executed、0 missing、1 blocked（Live 需真实 CCXT/testnet 前置）。
- 验证：Rust `qx-app`/`qx-sdk`/`qx-python` 测试通过；Python 全量 131 项通过；严格 Clippy 通过；格式检查通过；架构不变量门禁 1096 项通过；完整量化场景探针通过 37 个场景。构建测试扩展时发现源码目录内原有忽略的 `.pyd` 遮蔽新 DLL，已用本轮构建的扩展覆盖本地测试产物后复跑通过；该二进制未纳入提交。
- 方案仍未达到全量发布门槛：组合内尚不支持多个 caller callback、跨市场/跨账户共享资金和组合净额/风险归因；Paper/Live 组合及 Web/桌面 E2E、C++ 默认 transport/独立消费者、持久化订单事件游标恢复、跨平台 wheel 与真实交易所 testnet conformance 仍待实现或验证。缺少外部凭据的 Live 能力保持 blocked/unverified。

## 20. 2026-10-13 继续实现：Rust/Python 多自定义策略组合回测

- 在既有 Bar 同账户组合调度/账簿上增加 caller-defined callback 槽路由。Rust 应用层、Rust SDK、PyO3 原生扩展与 Python 高层 SDK 均可在一个 portfolio 里运行多个自定义策略回调；Rust 仍负责统一的策略优先级、预算与风控、OMS、撮合、账簿、成交归属和回放校验。
- 每个注入槽使用独立 `strategy_id`、版本与源码 SHA-256；身份集合以稳定顺序纳入运行指纹，拒绝重复槽与无组合时注入多个回调。同一 run_id 的策略源码/版本变更产生冲突，绑定列表顺序不影响确定性结果。同步、RunHandle 与 AsyncCoreClient 路径共享同一 Rust 执行实现。
- 测试覆盖 Rust 应用、外部 Rust SDK consumer、Python 同步/句柄/AsyncCoreClient、两个自定义 callback 的策略成交归属、ReplayVerifier、绑定顺序稳定性与源码身份冲突。Python 全量 131 项通过；Rust qx-app/qx-sdk/qx-python 全量测试与严格 Clippy 通过；格式和 diff whitespace 检查通过；架构不变量门禁 1096 项全部通过。量化场景探针为 37 executed、0 declared_not_executed、0 missing、1 blocked（Live 需外部 CCXT/testnet 前置）。
- 能力范围仍限于同一账户、同一市场的 Bar 回测。Tick/OrderBook 与多腿策略回调组合、跨市场/跨账户共享资本和风险净额、组合级 PnL/费用/回撤归因、Paper/Live 多回调工作流、HTTP/Web callback 托管、C++ 等价绑定和独立安装消费、断线后的持久订单/事件游标恢复及真实 venue testnet conformance 未由本轮实现或验证。总体全能力发布门槛仍未满足。

## 21. 2026-10-13 继续实现：Tick/OrderBook 多自定义策略组合

- 把多个 caller-defined Rust/Python callback 槽接入既有同账户 Tick/OrderBook 组合账簿。Rust `qx-app` 与公开 `qx-sdk::Client` 提供同步/RunHandle 入口；Python PyO3、高层 `qianxing`、`core` 导出与 `AsyncCoreClient` 同步映射到同一用例。槽位可与 Rust 内置策略混用，优先级、预算、风险、OMS、撮合、成交归属和重放仍由原 Rust 组合内核执行。
- 保持事件契约：L1 callback 收到 `Tick`，L2 callback 收到 `OrderBook`。每个 callback identity 的 strategy ID、版本、SHA-256 绑定配置指纹；输入顺序不影响摘要，同一 `run_id` 改源码返回 Conflict。重复、未声明槽与无 portfolio 的多 callback 请求拒绝执行。
- 新增应用层、Rust 外部 consumer 与 Python 原生 SDK 行为测试，覆盖 L1/L2 的两个自定义 callback 都产生归属成交、共享账簿重放、身份顺序确定性、源码变化冲突、同步/RunHandle 与 async RunHandle。该轮 Rust `qx-app` 111 项及 Rust SDK 独立消费者 20 项通过；Python 全量 131 项通过；严格 Clippy、格式、Python 语法和 diff whitespace 检查通过；架构不变量门禁 1096 项通过。量化场景探针最终结果为 37 项 executed、0 declared_not_executed、0 missing、1 blocked；唯一 blocked 为需要真实 CCXT/testnet 前置的 Live 验收。
- 已知边界：多回调深度组合只在本地 Rust/Python SDK 接通；远端 callback 执行、C++ callback 槽与 HTTP/Web 配置/操作暂未支持。仍不实现跨市场/跨账户统一资本净额与组合级策略归因，也不代表 Paper/Live 多回调订单工作流或真实交易所 testnet 已认证。总体多语言全能力发布门槛仍未满足。

## 22. 2026-10-13 Tick/OrderBook SDK callback 提交与安装验证

- 提交 `d71750f3` 已推送到 `origin/main`。将同账户 Tick/OrderBook 组合中的自定义策略 callback 从单槽扩展到多槽，Rust/Python callback 可与内置策略组合；L1 保持 Tick 事件语义、L2 保持 OrderBook 事件语义，所有策略继续走原共享 Rust 风控、OMS、撮合、账簿与 ReplayVerifier。callback source identity 按稳定顺序绑定到运行指纹。
- 入口覆盖 Rust `qx-app` / `qx-sdk` 同步与可取消 RunHandle、PyO3、`qianxing` 高层 API、`core` 导出及 `AsyncCoreClient` 的 run/start。新增应用用例、独立 Rust SDK 消费者和 Python 同步/异步测试覆盖成交归属、事件类型、回放、确定性身份排序和源码变更冲突。
- 验证：`cargo test --offline -p qx-app -p qx-sdk -p qx-python -- --test-threads=1` 通过；Python 全量 131 项通过；`cargo clippy --offline -p qx-app -p qx-sdk -p qx-python --all-targets -- -D warnings` 通过；格式、编译和 diff whitespace 检查通过；架构不变量 1096 项通过；量化场景 37 项 executed、0 missing、0 declared_not_executed，1 项 Live 因外部 CCXT/testnet 凭据 blocked。
- Windows x64 通用 `cp310-abi3` wheel 已构建并隔离离线安装；wheel 导入检查确认 `run_depth_backtest_with_strategy_slots`、`start_depth_backtest_with_strategy_slots` 和 Rust 原生扩展可用。SHA-256：`5faa1b447b18870dd8b85df3f60c0f1abdc33bcb0fbbe5f98d4100c99b93b2f6`。首次链接因系统 Temp 目录写入错误失败，改用仓库构建临时目录后构建成功。
- 更新了能力矩阵中过时的 Tick/OrderBook“仅内置策略”描述。当前仍不满足整体发布门槛：远端 callback、可恢复逐事件/订单日志、跨账户/跨市场共享组合账簿与风险归因、多腿组合调度、Paper/Live callback 工作流、C++ callback 本地入口、Linux/macOS 安装矩阵及真实 venue testnet 认证仍未完成。已验证的 Windows wheel 安装仅证明该平台的打包入口，不代表全平台或 Live 可发布。

## 23. 2026-10-13 多腿 callback 账户上下文校正

- 复核 Rust/Python 多腿 callback 时发现 `StrategyContext.cash` 与 `available_margin_raw` 对所有事件都固定使用主腿本金，导致处理参考腿行情时策略读取到错误的账户快照。现在每次事件按事件所属标的装载该腿的初始现金和可用保证金；现有多腿撮合仍使用逐腿独立本金和账簿，未冒称跨腿共享资金池。
- `MultiLegBacktestSpec` 启动前增加两腿初始现金合计的 checked-add 校验，拒绝超出 `i128` 的账户总额；避免把溢出延迟到报告汇总阶段。
- 新增行为用例验证主/参考腿事件分别读到各自的现金快照，并验证本金合计溢出在规格校验阶段失败。`cargo test --offline -p qx-app --lib -- --test-threads=1` 113 项通过，`cargo clippy --offline -p qx-app --all-targets -- -D warnings`、Python 全量 131 项、格式与 diff whitespace 检查通过；架构门禁 1096 项通过，量化场景 37 项 executed、1 项真实 Live 前置 blocked。
- 基于此 Rust 核心重建 Windows x64 `cp310-abi3` wheel，并隔离离线安装、导入多腿 callback/深度 callback API 与原生扩展。wheel SHA-256：`a54fb261d7921d06f3b7ff27b0e623ebfda085f21913c85f1b82aa22a7a3d7e9`。
- 这修复了多腿策略上下文的一致性问题，但没有实现共享多腿账簿、资金竞争、跨腿净额或成交回报驱动的策略回调；这些仍是多语言核心发布门槛中的未完成项。

## 24. 2026-10-13 Windows C++ SDK 安装与测试链路

- 发现 `cpp/CMakeLists.txt` 构建了两个 C++ smoke 程序但未注册 CTest；首次 `ctest` 结果为 `No tests were found!`，且 Release 的 `NDEBUG` 会使程序内 `assert` 检查失效。现已启用 CTest、注册 `qianxing_ring_smoke` / `qianxing_app_client_smoke`，并仅对这两个测试目标在 Release 下显式启用断言。
- 使用 MSVC 19.44 / Visual Studio 2022、Windows SDK 10.0.26100 实际构建 C++ SDK 与策略 DLL，Release 下 CTest 2/2 通过；安装到 staging 后，独立 `cpp/tests/consumer` 通过 `find_package(QianxingStrategySDK 0.1 CONFIG REQUIRED)` 编译并运行成功。
- 下游 consumer 也注册为 CTest，`.github/workflows/ci.yml` 的 Windows/Linux/macOS C++ job 与 release workflow 都会运行 SDK smoke 和安装 consumer 测试，避免只编译不执行。
- 能力矩阵先记录 Windows x64 本机安装消费者证据；三平台 CI 后续结果见 §25。C++ 默认 HTTP transport（TLS/连接复用/deadline/重试/事件流/取消）仍未实现，远端 SDK 功能不等于本地交易运行时。

## 25. 2026-10-13 C++ SDK 三平台 CI 复核

- GitHub Actions CI run `38363403100`（commit `4f85cd9e`）的 C++ SDK job 已在 `ubuntu-latest`、`macos-latest`、`windows-latest` 全部 `completed/success`。三平台均完成 SDK build、Release CTest、CMake 安装 consumer、ring/app-client smoke、Rust 宿主真实加载 C ABI 插件、JSONL worker 与 shared-memory JSON/columnar worker 验收。随后该 run 的 25 个 job 全部 `completed/success`，失败 job 为 0。
- 据此将 C++ 能力矩阵更新为三平台 x64 CI 有验证证据；未推断其它 CPU 架构通过。全仓 CI 已通过该提交门禁，但这不代表尚未覆盖的组合交易、事件持久化、其它 wheel 架构及外部 testnet 发布门槛整体通过。
- 仍待完成：C++ 默认 HTTP transport、其它架构支持；以及多腿共享账簿、跨市场/账户风险聚合、Paper/Live 多策略链、持久事件日志等全局缺口。

## 26. 2026-10-13 多腿回测账户账簿合并投影

- 为带 `account_id` 的应用层多腿 Bar 回测增加确定性账户账簿投影：按事件时间、源账簿 entry 序号及主/参考腿稳定次序合并源账簿事实，重编号后通过 `qx_core::Ledger::apply_entry` 完整重放；核对合并前后的各币种现金、各腿持仓数量与已实现盈亏，并在归因产物输出初始资金、腿级资金预留、账户现金、持仓、已实现盈亏、entry 数和回放校验标志。
- 产物显式标注 `allocation_policy=fixed_per_leg_reservation`、`cross_leg_buying_power=false`。这补上了可审计的单账户合并账簿视图，不能解释成撮合期动态共用资金池：两腿仍由独立 BacktestEngine 按固定初始本金撮合，不具备跨腿资金竞争、净额保证金或组合级实时风险。
- 回归用例验证同账户合并报告包含两腿持仓、初始现金合计正确且策略回放幂等；`cargo test --offline -p qx-app --lib -- --test-threads=1` 113/113 通过，严格 Clippy、格式检查、`git diff --check` 和架构不变量 1096 项均通过。架构门禁首轮捕获主用例文件超出行数预算 9 行；将账户账簿装配抽到 `portfolio_ledger.rs` 后复跑全部通过。
- 由最终 Rust 源码重建 Windows x64 `cp310-abi3` wheel；`tools/check_python_wheel.py` 通过，隔离虚拟环境离线重装后 `qianxing` 与 `qianxing_bridge._qianxing_native` 均导入成功。SHA-256：`992dc39f11d7169c060764d0cdea91c4c749fb5ee97496c56496a68a381a92b6`。这只证明当前 Windows x64 wheel。

## 27. 2026-10-13 单腿 Bar 自定义策略参数实验接通 Rust/Python SDK

- 复核发现内置策略的 Bar 参数网格和 Python 单次 callback 回测都已接入 Rust，但两者之间缺少“每个参数候选创建一份 Python/Rust 自定义策略”的入口。本轮将同步执行收为 `RunExperimentSpec::run_with_strategy_factory`，保留 `start_with_strategy_factory` 可取消句柄；factory 收到 Rust 已应用参数的候选 `BacktestSpec`，每个候选仍经同一 Rust Bar 引擎、风险/OMS/撮合/账簿、回放和结果比较，不在 Python 侧复制回测逻辑。
- Python PyO3、bridge、`qianxing` 高层、`experiments`/`core` 导出和 `AsyncCoreClient` 接入同步、RunHandle、async run/start。每个候选的自定义策略由 factory 新建；Rust 校验主策略身份、版本与小写 SHA-256，把策略身份加入实验指纹，来源身份变化时同一 `experiment_id` 返回 Conflict；自定义 factory 与 `strategy_kinds` 互斥。修复测试过程中发现的 native.py 转发缺失，并把入口纳入跨语言接口登记和能力矩阵。
- Rust `qx-app`/`qx-sdk`/`qx-python` 单元及消费者测试共 146 项通过；Rust 严格 Clippy 通过；Python 全量 132 项通过；架构不变量门禁 1096 项通过；Python wheel 格式检查通过。Windows x64 `cp310-abi3` wheel 在隔离虚拟环境强制重装后，实测自定义策略参数候选均成交、同步结果成功、可取消句柄成功。wheel SHA-256：`1be28f290a7da5f5051f61aad3f1f7f9c97c30e81737544c4ed9f0249763056c`。
- 本地 Rust/Python SDK 的 Bar 参数实验链本轮已补通，但远端 HTTP/C++ callback 托管仍不支持；此实现不证明跨市场/跨账户共享资金与组合级风险、多腿同一组合调度、Paper/Live callback 策略运行、Web/桌面端到端、Linux/macOS wheel，亦不等于真实交易所 testnet 认证。整体方案仍未达到全能力发布条件。

## 28. 2026-10-13 Tick/OrderBook 自定义策略参数实验接通 Rust/Python SDK

- 新增 `RunDepthExperimentSpec::run_with_strategy_factory` 与 `start_with_strategy_factory`。每个参数候选将已应用参数的 `DepthBacktestSpec` 交给调用方工厂，并创建全新策略实例；候选继续使用同一 Rust Tick/OrderBook 回测、风险/OMS/撮合/账簿、回放与排名链路。策略 ID/version/SHA-256 绑定实验指纹，同一 experiment_id 换源码会冲突；自定义工厂与内置 `strategy_kinds` 互斥。RunHandle 仍支持候选间和引擎内协作取消，不输出部分报告。
- Python PyO3、bridge、`qianxing`、`experiments`/`core` 导出及 `AsyncCoreClient` 增加同步、句柄和 asyncio run/start 入口。新用例实测两个不同 quantity 候选各自通过 factory 创建策略并成交；同步、RunHandle、AsyncCoreClient 均得到两个成功候选。
- 验证：Rust `qx-app`、`qx-sdk` 独立消费者（含新工厂用例）和 `qx-python` 共 147 项通过；严格 Clippy 通过；Python 全量 133 项通过；`tools/check_architecture.py` 1096 项通过；wheel 检查、隔离环境安装与 native API 导入通过。Windows x64 `cp310-abi3` wheel 大小 1,817,054 字节，SHA-256 `8ca9fedbe370093fab5c69e7ce6e257977a4d5e9f5b770c206a4fbcaae17b1b3`。量化场景探针 37 项通过、0 项缺失、1 项 Live 外部前置 blocked。Windows 系统 TEMP 对 Rust 用例目录有拒绝访问，重定向至 `target/test-tmp` 后全套测试通过。
- wheel 发行配置排除本地 `.venv`；构建时还将仓库忽略的虚拟环境临时移出源码目录并在 finally 路径恢复，避免发行包扫描开发环境。该临时动作已恢复，`python/.venv` 仍在。
- 本轮补通的是本地 Rust/Python SDK 的 Tick/OrderBook 多参数回调实验；HTTP/C++ callback 托管、跨市场/跨账户统一资本与风险、组合级多腿调度、Paper/Live callback 策略工作流、Web/桌面 E2E、Linux/macOS wheel 与真实 venue testnet 仍未通过验收。因此整体方案仍未达到全能力发布门槛。

## 29. 2026-10-13 补登记：Python 自定义深度参数实验场景证据

- 对照 §28 的已测功能与场景探针后发现，量化场景矩阵未单独执行 Python 自定义 Tick/OrderBook 参数工厂；只记录 Rust 内置深度策略选择会漏掉 Python callback 和 async/RunHandle 链路。现为 `tools/quant_scenario_probe.py` 新增 `python_custom_depth_parameter_experiment` 场景，复用同一 Python 原生 SDK 行为测试并从测试输出提取实际观测。
- 全量 `python tools/quant_scenario_probe.py` 重跑结果：38 项 executed、0 declared_not_executed、0 missing、1 项 blocked。新增场景实测 2 个候选均成功、总成交 2 笔，覆盖每候选新建 factory、RunHandle 与 asyncio 入口。Live testnet 场景继续保持 blocked，不因本地 Paper/回测通过而升级状态。
- 该更新补足验收登记与可重复执行证据，不新增交易能力；远端 C++/HTTP callback、多市场/跨账户组合风险、完整 Paper/Live 策略工作流、跨平台 Python wheel 和 testnet 认证仍是全量发布的未完成项。

## 30. 2026-10-13 修复：Paper 跨币种账簿现金腿与账户估值

- 发现规格驱动的现货成交仍沿用账户报告币/worker 账簿币记现金腿，导致 EUR 报价市场的买入被记成 USDT/USDC；跨币种 FX 估值即使有正确报价也无法恢复真实现金余额。现货成交现按规格报价币记账，衍生品仍按结算币记账；报价币代码在规格成交路径规范化为大写。
- Paper 风控从同账户显式配置的 Spot 市场规格读取直接或反向 FX 标记价，将现金及持仓权益转换到报告币。找不到正标价、规格与标的无法对应、报价冲突或所需 FX 缺失时失败关闭；零额币种不再制造无意义的 FX 依赖。该实现目前只支持唯一直接/反向汇率，不做多跳三角换汇。
- 新增 EUR 报价现货成交与 EUR/USDT FX 标价的 Paper 回归，断言现金账簿币种、市场估值和风险可用权益；增加规格币种大小写归一化回归。
- 验证：`cargo test --offline -p qx-core` 通过；`cargo test --offline -p qx-cli` 全量 396 个单元测试及应用集成测试通过；`cargo test --offline -p qx-app -p qx-sdk` 通过（122 个应用单元、22 个独立 SDK 消费者及 Provider/多账户消费者）；`cargo test --offline -p qx-api` 通过（65 个单元及全部集成测试）；Python `unittest discover` 通过 136 项、跳过 1 项。跨币种 Paper 专项回归在币种大小写规范化后再次通过。
- 本修复不关闭全局发布缺口：共享账户下跨市场 Bar 同步回测与统一撮合/风险/账簿仍未形成 SDK 主用例；多腿入口仍是各腿独立模拟，不可描述成同步组合撮合。C++ 本地没有可用 CMake/C++ 编译器，安装包/独立消费者缺少本机验收；Live 真实交易所仍缺 CCXT 运行环境与 testnet 凭据。故整套方案仍未达到全能力发布门槛。

## 31. 2026-10-13 补登记：Paper 跨币种现货估值场景

- 将 §30 修复接入 `tools/quant_scenario_probe.py`，探针通过实际运行 `paper_cross_currency_spot_uses_quote_cash_and_configured_fx_market` 回归测试，核对 EUR 报价现货现金腿、ETH/EUR 持仓和 EUR/USDT 兑换后的账户权益。
- 重跑探针结果：40 项 executed、0 declared_not_executed、0 missing、1 项 blocked。新增场景观测为 11,122 USDT；唯一 blocked 仍是缺少 CCXT 运行依赖和交易所 testnet 凭据的 Live 验收，不将本地测试冒充实盘认证。
- 这是验收矩阵补证，不关闭同步多市场共享账簿/撮合、多腿联合执行、远端回调托管、跨平台安装矩阵等既有缺口；整体方案仍未达到全能力发布条件。

## 32. 2026-10-13 修复：回测账户币种与市场现金币种前置校验

- 工作区全量回归发现六个衍生品测试使用默认 USD 账户，却交易 USDT 结算合约；共享账簿在首次成交时才因币种不符拒绝，配置错误出现得过晚。将 Bar 与 OrderBook 回测入口统一为先规范化账户币种，再按产品校验现金币种（现货报价币、衍生品结算币），并在调用策略前拒绝不匹配；Bar 路径同时把产品规格标的校验与规格验证收敛到同一前置闸门。
- 修正衍生品测试配置为 USDT，并保留一条小写币种回归，确认归一化后盈亏更新权益但不会错误改写初始现金；新增错误币种回归确认策略 callback 尚未运行即失败。为遵守单文件行数棘轮，将验证逻辑与用例移入模块文件。
- 验证：`cargo test --offline -p qx-xingban --lib` 103 项通过；最终版 `cargo test --offline --workspace` 全部通过；`cargo fmt --all -- --check` 通过；`python tools/check_architecture.py` 1096 项通过；全量量化场景探针 40 项 executed、0 项未执行、0 missing、1 项 Live testnet blocked；`git diff --check` 通过。
- 该修复强化已有单市场回测账簿一致性，不实现跨市场共享账户资金/统一撮合风险、多腿联合组合调度、远端 callback、Web/桌面 E2E、跨平台 wheel 矩阵或真实 venue testnet 认证；全能力发布条件仍未满足。

## 33. 2026-10-13 修复：异步 Run 事件拒绝超前游标

- 复核发现 `/app/runs/{run_id}/events?after_seq=N` 对大于最新事件序号的 N 静默返回空数组，客户端无法区分“暂无新事件”和错误/失效游标。Run 事件读取现在按已恢复的最新序号校验，超前游标返回 400 `InvalidInput`；事件 JSON 序列化错误也不再伪装成空数组。
- 扩展异步多腿 Run 的持久化重启回归：注册表重开后，`after_seq=1` 仍取得终态事件，`after_seq=3`（最新序号为 2）被拒绝；正常游标和现有 HTTP 读取契约保持不变。
- 验证：`cargo test --offline -p qx-api` 全部单元、集成和文档测试通过（65 项单元测试）；`cargo fmt --all -- --check` 通过；架构不变量门禁 1096 项通过。
- 这补齐的是超前游标校验，不是计划 §6.4 的完整事件日志：尚无逐事件/订单成交持久追加、发生时间与因果 ID；当前生命周期事件不做裁剪，因此尚未实现“游标过期→要求取快照”的恢复协议。整体方案仍未达到全能力发布条件。

## 34. 2026-10-13 复核：多腿回测不是成交反馈驱动的联合组合引擎

- 顺着 Python/Rust 自定义策略入口复核 `run_multi_leg_backtest`：当前先按完整 Bar 历史依次调用策略，并在发出 intent 时立即改写 `StrategyContext.positions`；随后才用 `run_leg` 分别执行主腿和参考腿。两条腿的结果账簿最后才按时间合并用于归因。`portfolio_ledger::merge_account_ledger` 明确记载这只是投影，撮合期间仍是固定腿级资金预留且没有跨腿 buying power。
- 因而当前入口能证明双腿数据输入、策略 callback、双腿各自回测、确定性归因和可重放账簿；它不能证明“成交/拒单/部分成交 → 策略回报 → 更新组合状态 → 下一腿决策”的联动，也不能提供组合级资金竞争/风险净额。若一笔意图在后续腿级引擎被风控拒绝，之前预扫描中的策略状态已经按意图乐观更新；现状没有一个统一的时间顺序引擎来向策略回传该拒单。
- 这不是本轮通过改文案即可关闭的能力。下一实现切片需在 Rust kernel 增加按 `(timestamp, deterministic instrument priority, event priority)` 驱动的多标的 Bar session，把各腿的 market event、策略决策、风险/订单状态、成交回报、共享账户 Ledger 和下一次 callback 置于同一个有序循环；Rust/Python SDK 再调用该用例，并用拒单/部分成交、资金竞争、同刻双市场事件、确定性重跑和 replay 对账证明闭环。完成前，既有 multi-leg callbacks 只标为本地研究能力，不能等同同步组合交易或组合共享账簿。

## 35. 2026-10-13 修复：Python/Rust Bar 策略成交回报闭环

- 复核统一 `qx_strategy::Strategy::on_order_update` 后确认：接口可返回 `StrategyDecision`，C ABI 与应用策略槽都能透传，但 Bar 回测成交完成后没有调用点；Python `_PythonBacktestCallback` 也没有订单更新 handler。因此回测策略只能在行情事件上决策，成交/部分成交不能影响其下一次状态决策。
- Bar `NativeBarStrategy` 现在只在已归属的策略订单完成账簿及 replay 写入后发送 `{schema_version, event:"fill", order, fill}` JSON 事实，并先更新策略持仓、时间、现金和可用保证金快照。回调返回的决策暂存至下一决策边界，再走原订单数量/账户/市场校验、RiskGate、OMS、撮合和账簿；不在成交回调内旁路执行。Python bridge 新增可选 `on_order_update(context, update)`，缺少该方法的旧策略仍保持兼容。
- 新增 Rust 行为用例验证成交回调看到已更新持仓、返回的平仓意图在下一 Bar 成交且报告 replay 可验证；新增 Python bridge 序列化用例及真实 PyO3 扩展端到端用例，确认 Python callback 的成交决策经过 Rust 风控/OMS 并最终完成平仓。能力探针新增 `python_fill_feedback_loop`。
- 验证：全工作区 `cargo test --offline --workspace` 在将 Windows 临时目录重定向到仓库 `target/test-tmp` 后通过；回调实现拆模块后 `cargo test --offline -p qx-xingban --lib` 103/103 通过、`cargo build --offline -p qx-python` 成功；Python 全量 139 项通过；回调 Python→Rust 专项 E2E 通过；最终架构不变量门禁 1096 项通过；量化场景探针 41 executed、0 declared_not_executed、0 missing、1 blocked；`cargo fmt --all -- --check` 与 `git diff --check` 通过。成交反馈前会刷新账簿确认后的现金与现货可用资金，供策略下一次决策读取。
- 本修复只关闭统一 Bar 策略的 fill callback 断链。Tick/OrderBook adapter 尚未统一透传 `on_order_update`；拒单/撤单等非成交状态也尚未形成通用回调；多腿执行仍是先生成意图、后分腿模拟和账簿投影，没有共享资本竞争、统一时间线或拒单/部分成交反馈；Paper/Live 的 Python callback 策略与真实交易所 testnet 仍未端到端认证。整体多语言 SDK 方案仍未达到全能力发布门槛。
