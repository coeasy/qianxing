# 多语言策略示例

牵星的策略层负责生成目标仓位或订单意图；成交、费用、风险检查和账本仍由同一 Rust 内核执行。
跨语言订单与回测合同使用定点数或精确十进制；Python 样例可用 `Decimal` 做精确指标计算，避免二进制浮点误差。

| 语言 / 接口 | 示例 | 策略实例 | 使用方式 |
|---|---|---|---|
| Rust 原生 SDK | `crates/qx-strategy/src/lib.rs` 的 `SmaCrossStrategy` | SMA 金叉 | `cargo run -p qx-cli -- backtest builtin sma_cross deploy/qianxing.bar-frame.example.json` |
| Python Worker 函数协议 | `python/examples/strategy_instances.py` | 动量、SMA、均值回归 | 配置 `strategy.external_executable` 指向 Python，并将该文件导出为 Worker 的 `on_event` 入口 |
| Python `BaseStrategy` 类 | `python/examples/strategy_file_sma.py`、`python/examples/strategy_file_nextstyle.py` | 事件式 SMA、Backtrader 风格预热 | `qianxing run-strategy`、`qianxing research`；本地可信策略直接运行，不要求沙箱 |
| C++17 C ABI | `cpp/examples/momentum_strategy.cpp` | Bar 事件策略插件骨架 | `cmake -S cpp -B build/cpp -DCMAKE_BUILD_TYPE=Release && cmake --build build/cpp --config Release` |

Python 示例通过 `StrategyContractInput` 接收不可变行情帧，返回带策略身份、时间戳和定点目标仓位的决策。
可以在 Worker 启动模块中将 `on_event = momentum`、`sma_cross` 或 `mean_reversion` 绑定为入口；
参数需在部署时固定，作为运行配置的一部分记录。示例没有调用交易所，也不绕过 Rust RiskGate / OMS。
模块函数策略可选导出 `on_error(request, error)`；类策略可覆盖兼容的 `on_error(context, error)`。两者继续收到原生 Python 异常。还可实现 `on_error_v1(error: StrategyErrorV1)` 接收与 Rust/C/C++ 共用的版本化 DTO；两个观察器都会运行，钩子异常不会替换原错误。DTO 包含 `stage`、策略身份、时间、标的和有界 `message`。

## Python 类策略：本地接入与研究

文件策略可继承 `qianxing_strategy.BaseStrategy`，并导出 `STRATEGY_CLASS` 或 `create_strategy(spec)`。

策略回调失败时，Rust `Strategy` 可选实现 `on_error(&StrategyErrorV1)`；C/C++ 插件可选导出 `qx_strategy_on_error_v1` 接收同一版本化 JSON。Python `BaseStrategy` 和模块函数可选实现 `on_error_v1(StrategyErrorV1)`，旧 `on_error(context, error)` / `on_error(request, error)` 接口保持兼容。Bar、Tick、原生 OrderBook/DepthBar 回测、Python 本地/Runtime 宿主及候选偏差诊断 worker 会在可确定阶段时通知；`history_requirements()` 失败使用 `history_requirements` 阶段。观察器返回错误或 panic 都不会替换原始失败。旧 C ABI 插件缺少可选符号时继续运行。
策略可按需覆盖 `on_init`、`on_start`、`history_requirements`、`on_warmup_complete`、`on_bar`、
`on_tick`、`on_order_book`、`on_timer`，以及 `on_error_v1` / `on_stop`；兼容错误钩子 `on_error` 仍接收原上下文和 Python 异常。Bar 策略也可使用
`prenext` / `nextstart` / `next`。运行宿主提供对应的权威更新输入时，还可覆盖 `on_order`、`on_trade`、
`on_account`、`on_position`。`on_error` 不能把宿主判定的失败改成继续交易。策略的 `buy`、`sell`、`short`、`cover` 与
`close_position` 只构造订单意图，最终由 Rust 风控、OMS、执行和账本链路处理。历史数据门面按
`context["as_of"]` 截断，精确 OHLCV 可用 `Decimal` 读取。

从仓库根目录运行示例：

```powershell
# 单次本地回测
qianxing run-strategy python/examples/strategy_file_sma.py python/examples/strategy-file-backtest.example.json

# 参数研究、样本外复核、候选偏差诊断和交互式 HTML 报告
qianxing research python/examples/strategy_file_sma.py python/examples/strategy-file-search.example.json --html
```

本地 `run-strategy`、`search-strategy` 和 `research` 默认在本机加载并执行策略文件；可信的单用户研究不需要
沙箱 SDK、云账号或交易所凭据。安装的 Python 包提供 `qianxing_strategy.BaseStrategy` 与 `qianxing` 命令；
`research` 的逐候选未来数据/预热诊断也接受 `STRATEGY_CLASS`，并为每个 Bar 决策提供只读且按 `as_of` 截断的历史门户。
运行自己的文件时，将示例 JSON 换成自己的版本化 BacktestSpec/RunExperimentSpec，并在配置中填写数据、市场、费用和参数域。

多租户服务或运行不可信上传策略时，应由服务器部署配置强制启用隔离。E2B 是远程持久策略 worker 的首选；
批量 `remote-research` 默认 E2B，也可显式选择 Microsandbox 或 OpenSandbox。Microsandbox 支持本机 microVM
和显式配置的 Cloud backend；自建部署也可接 OpenSandbox 服务。provider 由宿主配置，失败时拒绝运行，不回退到本机。
持久 Python 策略 worker 也支持 `llm-sandbox`，但仅连接启用 mTLS 的远程 Docker Engine，属于加固容器路径，
不是 microVM。安装 `qianxing[sandbox-llm]` 后，按 `python/README.md` 中的配置项固定镜像 digest、TLS 文件和资源限额；
`strategy.python_timeout_ms` 至少设为 `15000`。E2B 仍是托管多租户部署的推荐首选。
沙箱负责隔离策略进程，Rust 宿主继续持有交易所凭据、Gateway、风控、OMS、执行和账本；fake SDK/离线测试
不等于真实供应商或 Docker 主机的隔离、容量或生产验收。连接不可信策略前必须先验收目标服务的认证、隔离、资源执行、
故障清理和长会话恢复。

远程批量研究示例（先在宿主配置镜像 digest；Cloud 凭据只留在宿主环境）：

```powershell
# E2B 是默认 provider；先在宿主配置模板和 API Key
$env:QX_E2B_TEMPLATE = "<已配置的模板 ID>"
$env:E2B_API_KEY = "<仅宿主可见的 API Key>"
qianxing remote-research python/examples/strategy_file_sma.py python/examples/strategy-file-search.example.json --provider e2b

# Microsandbox 本机 microVM；镜像必须以 OCI SHA-256 digest 固定
$env:QX_MICROSANDBOX_IMAGE = "<repository>@sha256:<64位十六进制摘要>"
qianxing remote-research python/examples/strategy_file_sma.py python/examples/strategy-file-search.example.json --provider microsandbox

# 或者 --backend cloud，并在宿主设置 MICROSANDBOX_API_KEY / QX_MICROSANDBOX_PROFILE
```

Microsandbox 与 OpenSandbox 的远程研究输入沿用相同的策略包、数据/spec 指纹和有界结果归档合同；远程任务只收集结果，
不会把交易所密钥送进 guest。该命令适合同步 CLI/worker 调用；Python async 服务应把批任务交给独立 worker 进程。

C++ 示例展示 C ABI 生命周期和 Bar 回调契约。C ABI 当前示例只返回空意图，适合验证插件加载边界；
需要实际下单时应在 `decision->intents` 中填充经 SDK 分配器创建的订单意图，并实现配对释放回调。

统一回测产物可用 `qx-cli compare-runs` 与 `qx-cli report` 对照策略结果；相同冻结 BarFrame、费用配置、
仓位单位和运行参数是跨语言比较的前提。策略样例用于演示接口与链路，不代表投资建议或收益承诺。
