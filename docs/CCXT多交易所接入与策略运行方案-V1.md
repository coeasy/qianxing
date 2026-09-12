# 牵星公共 CCXT 多交易所接入与策略运行方案 V1

更新时间：2026-09-11

## 1. 结论

交易所连接层统一优先使用公共 CCXT，不再为 Binance、OKX、Bybit 等交易所重复实现签名、REST 路径、限频和订单协议。

- `ccxt`：统一 REST 公共行情、历史 K 线、市场元数据、账户、下单、撤单和订单查询。
- `ccxt.pro`：可选实时 WebSocket `watch_*` 能力；没有 Pro 授权或某交易所不支持时，使用 REST 轮询和对账，不伪装成实时流。
- Rust 核心：继续负责 Instrument、OrderIntent、风控、订单状态、事件事实、Ledger、回测撮合和恢复语义，不依赖 CCXT。
- Python CCXT 边界：负责交易所调用、symbol/id 映射、十进制定点转换和 CCXT 错误分类，通过稳定的 JSON/Arrow 数据进入研究、回测和运行时。

官方 CCXT 的统一 API 只覆盖交易所共同能力，市场 `symbol` 与交易所原始 `id` 必须通过 `markets` 映射，不能把字符串直接当成牵星 `InstrumentId`。不同交易所特有能力必须通过显式 `params` 和 capability 检查进入，不得破坏统一模型。

## 2. 当前已落地

代码入口：`python/qianxing_ccxt/__init__.py`

- `CcxtConfig`：exchange_id、sandbox、凭据、defaultType、限频和超时。
- `CcxtExchangeClient`：惰性加载公共 `ccxt`，按 exchange_id 实例化交易所。
- `resolve_market`：支持 `BTCUSDT.BINANCE`、`BTC/USDT.BINANCE` 和交易所原始 market id 映射到 CCXT symbol。
- 市场下单前校验：统一检查 price tick、qty step、最小/最大数量和市场最大杠杆，不能把交易所精度错误推迟到不可重试的实盘副作用之后。
- `fetch_ohlcv`：时间范围、分页、去重、排序、空结果和定点整数转换。
- `fetch_ticker`：统一 L1 ticker 并转换为牵星定点数。
- `create_order/fetch_order/cancel_order/fetch_balance`：统一 REST 交易入口。
- 合约能力：`set_margin_mode`、`set_leverage`、`set_position_mode`、`fetch_positions`、`fetch_funding_rate`、`fetch_open_orders`；统一保留 linear/inverse、contractSize、settle、expiry、marginMode、hedged、liquidationPrice 等信息。
- CCXT Reconcile：`fetch_positions` 归一化为带多空方向、均价、标记价、强平价、未实现盈亏、初始/维持保证金和杠杆的 `AccountPositionSnapshot`；余额快照保留借贷负债，订单/成交对账保留手续费；`fetch_funding_rate` 归一化为可重放的 `FundingRateSnapshot`，但费率观察不会直接改 Ledger。
- CCXT Cashflow：优先使用公共 `fetch_ledger`，将 funding/interest/settlement/transfer 账单归一化为带 `external_id` 的 `AccountCashflow`；不支持时显式尝试 `fetch_funding_history`，进入 EventLog 后生成 `LedgerApplied`，按账单身份幂等，不把余额快照或费率观察当成现金结算。
- `CcxtErrorClass`：限频、网络重试、认证、交易所错误、参数错误、未支持能力和未知错误分类。
- `watch_ohlcv/watch_ticker/watch_orders/watch_my_trades/watch_balance/watch_positions`：可选公共 CCXT Pro 流，缺失时显式返回 Unsupported；JSONL Worker 会复用同一个 Pro exchange，并输出带 stream/exchange_id/received_ts/events 的稳定事件封装。
- `python -m qianxing_ccxt.worker --config ...`：JSONL 进程边界，支持 `load_markets`、`fetch_ohlcv`、`fetch_ticker`、`create_order`、`fetch_order`、`fetch_my_trades`、`cancel_order`、`fetch_balance`、`fetch_ledger`、`fetch_funding_history`、`fetch_leverage_tiers` 和可选 `watch_*` 流；秘密只从环境变量读取，Pro stream 对网络/限频错误按配置有限重连并指数退避，对认证、参数和不支持错误稳定失败。
- CCXT `UserStream` worker 已接入 `watch_orders`：流只提供 remote order 唤醒和身份，Rust 再用 `fetch_order` 补齐成交/费用，之后通过 `CcxtProcessVenue::sync_order` 进入统一 Runtime/EventLog/Ledger；未知 remote order 交给 Reconcile，不自动注册或补单。
- `qx-cli ccxt-worker runtime.json worker-id ccxt-config.json`：把已审计 SubmitOrder 接入 Rust Control/Queue/EventLog/ExecutionService。
- CCXT Python worker 启动时会清空父进程环境，只保留 Python/Windows 运行所需基础变量和 `credential_env` 声明的变量；不相关的交易所凭证不会跨进程继承。凭证值仍只从环境变量读取，不进入 Rust 日志或配置摘要。
- Rust `CcxtProcessClient` 按配置读取 `timeout_ms`，通过独立响应读取线程和有界等待避免交易线程永久阻塞；超时、worker 退出和通道断开都按“提交结果未知”处理，不自动重试下单。
- ExecutionService 接收到冻结 `TradingInstrumentSpec` 时，Paper/CCXT 的成交统一使用产品规格归约：Spot 走现金成交，Margin/Perpetual/Future 走持仓、已实现 PnL、手续费和资金结算语义；规格生成的 LedgerApplied 事实可在重启后直接重放。
- `qx-cli ccxt-fetch-ohlcv ccxt-config.json instrument start_ms end_ms output.json [timeframe]`：下载一次 OHLCV 并冻结为回测输入快照。
- `qx-cli ccxt-market-spec ccxt-config.json instrument market.json`：下载并冻结 CCXT 市场元数据与可用 `leverage_tiers`，作为现货/保证金/永续/交割合约回测的产品规格和阶梯保证金输入。
- `qx-cli ccxt-backtest bar-frame.json [fast] [slow] [market.json]`：只读取本地 BarFrame；传入 market.json 时按合约乘数、线性/反向合约和产品类型计算成交、持仓和未实现盈亏。
- Strategy 运行配置支持 `product`、`margin_mode`、`position_mode`、`leverage` 和 `allow_short`；现货默认 Cash/1x/NoShort，永续/期货可显式生成 Cross/Isolated、OneWay/Hedge 和空头订单 Policy。
- Runtime 支持 `strategies[]` 多策略实例；每个实例通过 `id` 绑定同名 Strategy worker，JobSpec.owner 决定任务归属，避免多个策略抢占或重复处理同一任务。
- Strategy 实例可配置 `python_module`，或配置 `external_executable/external_args/external_env` 接入 Rust/C++ 独立策略进程；均由 Rust Strategy Worker 通过版本化 JSONL 契约调用。输入包含 PIT 研究目标、账户/持仓/现金、可用保证金和风险状态，输出必须回传相同的 `request_id`、`strategy_id`、`instrument`，不能绕过 Rust 风控与控制面。未配置时继续使用 Rust 内置策略兼容路径。
- 假交易所测试覆盖多交易所通用逻辑，不访问真实账户和网络。

## 3. 目标数据和交易链路

### 3.1 回测

```text
CCXT fetch_ohlcv
  → BarFrame / Arrow 快照
  → DataQuery / QualityGate / PIT
  → Factor / Candidate / Strategy
  → Signal → PortfolioTarget → RiskGate
  → Xingban 撮合模型
  → Ledger / RunManifest / 可重放摘要
```

回测必须保存 `exchange_id`、CCXT symbol、market id、timeframe、查询边界、接收时间和数据摘要；回测不可在策略运行时直接请求交易所。

### 3.2 实盘

```text
Strategy
  → OrderIntent
  → Rust Control/OMS
  → CCXT Execution Worker
  → create_order
  → fetch_order / watch_orders
  → RuntimeEvent
  → EventLog → Ledger → Reconcile
```

未知下单结果禁止自动补单，先用 `fetch_order`、`fetch_open_orders` 和余额/成交对账确认。CCXT 的统一状态只作为输入，终态转换仍由牵星订单状态机决定。

## 4. 多交易所配置约束

每个账户/执行路由必须明确：

```json
{
  "exchange_id": "okx",
  "account_id": "main",
  "sandbox": true,
  "default_type": "spot",
  "instrument": "BTC/USDT.OKX",
  "credential_env": {
    "api_key": "QX_OKX_API_KEY",
    "secret": "QX_OKX_API_SECRET",
    "password": "QX_OKX_API_PASSWORD"
  },
  "params": {}
}
```

禁止：

- 直接把 `BTCUSDT`、`BTC/USDT` 当成跨交易所通用 InstrumentId。
- 把 Binance 的 `recvWindow`、OKX 的 `tdMode` 等特有参数写入核心订单模型。
- 在没有 `watch_*` 能力时宣称具备实时用户流。
- 用 CCXT 返回的订单状态直接修改 Ledger。

## 5. 多策略支持

策略统一只输出 `Signal` 或 `PortfolioTarget`，不能直接调用 CCXT：

- SMA/EMA、动量、均值回归、突破、网格、套利等策略实现为独立 Strategy 插件。
- `SignalMerger` 按 strategy_id、signal_id、priority 和过期时间确定性合并。
- Portfolio 计算目标仓位与当前持仓的差额。
- RiskGate 统一检查资金、数量、价格、NoShort、交易所能力和账户限额。
- 多策略共享行情和账户快照，但订单必须保留 strategy_id、signal_id、intent_id 归因。

## 6. 后续实现顺序

1. 将现有 `qianxing_ccxt.worker` 接入独立 MarketData/Execution/Reconcile Worker，使用稳定 JSON 命令和事实文件与 Rust 运行时通信；当前已接入 CCXT ticker MarketData、Execution SubmitOrder、订单状态/余额 Reconcile、OHLCV 快照和 MarketSpec 下载入口。
2. 将 CCXT 市场缓存、SymbolMapper 持久化、精度/杠杆/维持保证金分层和 capability 快照继续补齐；当前 MarketSpec 已能驱动统一产品规格，但缺省精度字段仍要求部署侧覆盖。
3. 完成 REST 轮询式实盘闭环的成交明细、持仓快照、资金费观察、资金账单/利息、交割和分层强平对账；Cashflow 已进入 EventLog/Ledger，仍需按交易所真实字段和账单时间窗口做外部验收。
4. 已完成 CCXT Pro `watch_*` JSONL、有限重连/退避和 UserStream 代码接入；仍需在真实 CCXT Pro/交易所环境中验证断点、重复回报、账单字段和限频矩阵。
5. 为 Binance、OKX、Bybit 各完成 sandbox 注入式测试，再进行真实账户验收。
6. 将 CCXT 历史数据快照接入回测 CLI，并实现多策略同输入确定性回放；当前已接入 Rust BacktestEngine 和多策略 JobQueue 隔离，后续补充策略插件化输入与组合级归因。

当前公共 CCXT 代码链路、产品规格风控和规格化成交归约已经完成；第 3、4、5 项的真实交易所字段、Pro 重连和 sandbox/实盘验收仍属于外部验收，不能把本地协议测试当成真实账户验收。
