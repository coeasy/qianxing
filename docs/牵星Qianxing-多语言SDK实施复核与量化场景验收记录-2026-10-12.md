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

## 5. 2026-10-13 继续复核：门禁与核心 SDK 用例

- 重新运行 `python tools/quant_scenario_probe.py`：31 项本地场景全部执行通过，0 项缺失；1 项多交易所 Live 场景因 `ccxt` 运行依赖与 testnet 凭据缺失保持 blocked。结果更新到 `maturity/quant_scenarios.yaml`。场景包括十种经典策略、多语言策略、Bar/深度/双腿回测、参数对比、Paper 算法执行、风险平价与资金规划等。
- `python tools/check_architecture.py`：1096 项全部通过。
- `cargo test --offline -p qx-app -p qx-sdk`：qx-app 83 项单元测试、2 项外部消费者测试；Rust SDK 5 项单元测试、1 项资金规划消费者、10 项独立消费者、2 项 Provider 消费者测试全部通过。
- `cargo test --offline -p qx-api`：58 项单元测试及 API 集成测试全部通过。首次未设置临时目录时，4 项依赖文件系统的测试受 Windows 默认 Temp 目录拒绝访问影响；将 `TEMP`/`TMP` 指到仓库 `target/test-temp` 后全量重跑通过。该限制属于本机测试环境，未伪报为产品行为缺陷。
- 本轮重新核对确认一个未闭环的主链：仓内 Runtime/CLI 能运行多策略配置，但 Rust/Python/C++ SDK 与 HTTP/Web 稳定应用契约没有同一多策略组合回测用例；现有 `qx-app` Bar `BacktestSpec` 仍是单策略运行。组合资金分配也尚未与策略调度、共享组合风险、组合账簿、Paper/Live 订单执行连接。因此“SDK 多策略组合实盘/回测全部贯通”仍为未完成。
- 本轮没有引入第二套撮合/账簿算法，也没有将 Runtime/CLI 的场景执行误记成 SDK 接入。下一阶段应先把组合调度与共享账簿能力收敛成有明确所有权的 Rust 应用用例/领域端口，再逐一接入各语言和 API，并以跨语言相同输入、相同结果与事件/账簿核对作为退出门。
