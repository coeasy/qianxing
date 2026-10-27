# Crypto live connector: local read-only sandbox smoke

This smoke checks that a sandbox key can authenticate and read balances; for derivative configs it also reads positions, position mode, margin mode, and leverage. It never places/cancels orders or changes leverage/position mode. A successful smoke proves private read connectivity and derivative-setting visibility only, not order acceptance or production readiness.

## 快速运行

先跑完全离线的本地验收（不需要密钥、不访问交易所）：

```powershell
Set-Location D:\aex_work\qianxing
.\scripts\crypto-live-offline-check.ps1
```

这条脚本会跑 Rust/Python 离线合同、格式和架构检查；小于 `1e-9` 的 CCXT 行情事件、精确组合 FX 估值、原始手续费与持仓防 float 伪精度、Binance Spot `MAX_POSITION` 也包含在本地合同中。Binance/OKX/Bitget 的标准化持仓数值若已被 CCXT 转成 float，连接器会从原始 `info` 恢复数量、价格、盈亏及可恢复的保证金；关键字段缺原文时拒绝继续参与精确风控。脚本不会读取环境变量中的密钥，也不会连接交易所。

### 真实下单配置的能力预检

Binance/CCXT 的一次性提交、常驻执行队列和多腿恢复现在会先把订单、冻结市场规格和配置映射到能力目录，再决定是否创建连接器。旧的 CCXT 执行配置需要补上明确账户体系：Binance Spot 用 `account_model: "spot"`，Binance USDⓈ-M 用 `"usd_m_futures"`，OKX 用 `"unified"`，Bitget Classic/UTA 分别用 `"classic"`/`"uta"`；Bitget 还必须显式设置 `options.uta` 为 `false`/`true`。Bitget `sandbox` 配置必须是布尔值；sandbox 模式无法由所用 CCXT adapter 启用时，Python 连接器会在发起网络请求前明确失败；Rust 配置预检在创建连接器前完成，但 adapter 的 `set_sandbox_mode` 能力检查发生在 Python 读取本地环境变量之后。

该预检只证明配置形状落在已登记范围和代码成熟度地板内。它不会联网确认账户实际为所声明账户模式，也不会生成 `sandbox_tested` 或 `production_approved` 证据；生产档位在成熟度审批前保持关闭。真实沙盒下单、成交、撤单、对账和重启恢复仍需要操作者使用自己的 Testnet/Demo 账户单独验收。

只重跑 CCXT 现货/合约请求合同（包括 Binance `MAX_POSITION` 跨交易对挂单合并拦截）：

```powershell
Set-Location D:\aex_work\qianxing\python\tests
python -m unittest test_ccxt test_ccxt_worker
```

这些用例使用 fake exchange，不需要设置 API 环境变量；完整 Rust/Python 离线验收仍使用上面的仓库脚本。

要检查一次性 SubmitOrder 控制链但不访问交易所，可用仓库内默认 `dry_run: true` 的命令样例：

```powershell
cargo build -p qx-cli
.\target\debug\qx-cli.exe ccxt-submit-order `
  .\deploy\qianxing.runtime.ccxt.example.json `
  ccxt-execution-main `
  ..\python\examples\ccxt-okx-swap.example.json `
  .\deploy\qianxing.submit-order.ccxt-derivatives.example.json
```

它只验证配置与审计状态机，输出 `DRY_RUN_VALIDATED`，不会读取账户、建立交易所连接或提交委托。需要沙盒只读认证时，再按下方说明设置环境变量并传入 `-ProbeConfig`。

需要验证沙盒账户只读连通时，在同一个 PowerShell 窗口设置本地环境变量，再运行对应配置。下面以 OKX Swap Demo 为例：

```powershell
$env:QX_CCXT_OKX_API_KEY = '<你的 Demo API Key>'
$env:QX_CCXT_OKX_SECRET = '<你的 Demo Secret>'
$env:QX_CCXT_OKX_PASSPHRASE = '<你的 Demo Passphrase>'
Set-Location D:\aex_work\qianxing\python
python -m qianxing_ccxt.private_probe --config .\examples\ccxt-okx-swap.example.json
```

Binance、OKX、Bitget 都走公共 CCXT 实盘链路时，按对应沙盒账号设置本地进程环境变量并替换探针配置即可；每次只探测一个交易所：

```powershell
# Binance Spot Testnet
$env:QX_CCXT_BINANCE_API_KEY = '<Testnet Key>'
$env:QX_CCXT_BINANCE_SECRET = '<Testnet Secret>'
Set-Location D:\aex_work\qianxing
.\scripts\crypto-live-offline-check.ps1 -ProbeConfig .\python\examples\ccxt-binance-spot.example.json

# OKX Demo：使用 QX_CCXT_OKX_API_KEY / SECRET / PASSPHRASE，配置换成 ccxt-okx-swap.example.json
# Bitget Demo：使用 QX_CCXT_BITGET_API_KEY / SECRET / PASSPHRASE，配置换成 ccxt-bitget-uta-perpetual.example.json
```

若只验收现货，可以分别选 `ccxt-okx-spot.example.json`、`ccxt-bitget-uta-spot.example.json`。探针只读余额、仓位和合约设置；它不会切换双向持仓、修改杠杆或发单。后续在本地做沙盒下单时可用 `qx-cli ccxt-submit-order <runtime.json> <worker-id> <ccxt-config.json> <command.json>`，命令样例默认 `dry_run: true`；把凭据留在本机环境变量中，并且只在 Demo/Testnet 配置上由操作者显式切到 `dry_run: false`。本轮没有运行账户探针或订单命令。

该探针只读取余额、仓位和合约设置，不会提交订单。CCXT 现货/合约和原生 Binance Spot 的精确金额已贯通请求、风控规格、成交/手续费及余额快照；Paper 撮合仍拒绝超过 9 位的订单金额。只读探针只证明账户读取连通，不证明真实委托被交易所接受。

原生 Binance Spot Testnet 可以在本机 PowerShell 环境变量中放测试凭据，再用单次对账 worker 读取测试账户和远端订单；这条命令不提交或撤销订单：

```powershell
$env:QX_BINANCE_TESTNET_API_KEY = '<Testnet Key>'
$env:QX_BINANCE_TESTNET_API_SECRET = '<Testnet Secret>'
cargo run -p qx-cli -- binance-worker `
  deploy/qianxing.runtime.binance-testnet.example.json `
  reconciler-main `
  --once
```

离线实现检查仍用 `scripts/crypto-live-offline-check.ps1`。后续要在 Testnet 验收真实订单，可走 `binance-submit-order`，把 `dry_run` 明确改为 `false`，并确认订单精度与该币对当时的 Binance `exchangeInfo` 规则一致；运行前需准备单独的测试命令文件与冻结精确市场规格。本轮只运行本地 fake-transport 合同，没有加载账户凭据、连接网络或发单。

Bitget UTA 现货只读探针也有独立配置；把下面环境变量值留在本机 PowerShell 会话中即可验证 Demo Key 认证和余额读取：

```powershell
$env:QX_CCXT_BITGET_API_KEY = '<你的 Bitget Demo API Key>'
$env:QX_CCXT_BITGET_SECRET = '<你的 Bitget Demo Secret>'
$env:QX_CCXT_BITGET_PASSPHRASE = '<你的 Bitget Demo Passphrase>'
Set-Location D:\aex_work\qianxing\python
python -m qianxing_ccxt.private_probe --config .\examples\ccxt-bitget-uta-spot.example.json
```

执行 Bitget UTA Spot 市价买单时，Bitget 规定 `qty` 使用计价币金额；运行时会按此语义发送精确预算并计入风险预留，不会按 PEPE 等基础币的数量精度截断。详见 [Bitget UTA 下单接口](https://www.bitget.com/docs/uta/trade/Place-Order)。探针本身仍然只读。要验收真实沙盒订单，需单独使用 `ccxt-submit-order` 的 `dry_run=false` 命令文件；请只填 Demo/Testnet 环境变量、先用极小预算，并在本轮离线检查通过后再自行运行。当前轮没有运行这条发单命令。

## Python/external Strategy API 精确意图

`Strategy API v1` 的多语言 `intents[]` 现在可用十进制字符串表达小于 `1e-9` 的数量与限价。旧 raw 字段保留兼容：精确数量时把 `qty_raw` 设为 `0`，并只使用 `qty_exact`/`limit_price_exact`。市场规格必须包含精确 step/tick；运行时会先验证冻结规格，再由实盘账户级快照风控做最终准入。

```python
StrategyIntent(
    intent_id=1,
    instrument="PEPEUSDT.BINANCE",
    side="buy",
    qty_raw=0,
    qty_exact="1e-10",
    limit_price_exact="0.00000005",
)
```

可直接参考 [strategy_exact_amount.py](../python/examples/strategy_exact_amount.py)，在 runtime config 中将 `strategy.python_module` 配为 `examples.strategy_exact_amount`，并为标的提供相匹配的冻结精确 step/tick。Python Worker 和外部 JSON Worker 可返回这类 live intent。Bar 回测也支持 `qty_exact` / `limit_price_exact` 策略订单意图，但必须在回测规格中提供带 `exact_rules` 的冻结 `market_spec`；精确成交数量和价格会写入 Fill/EventLog。

跨语言实盘策略也可以用 `positions_exact` 读取精确当前仓位，再以 `target_qty: 0` + `target_qty_exact` 表达单向目标仓位；运行时按账簿精确仓位计算买卖差额并复用精确 market-spec 校验。示例见 [strategy_exact_target.py](../python/examples/strategy_exact_target.py)。这一路径限单向仓位；合约双向模式的目标需要拆成显式 long/short intents，避免净仓目标掩盖两条独立持仓腿。`target_qty_exact` 目标仓位目前仍不支持 Bar 回测；请在回测策略中发出明确 side 和精确数量的 intents。

Bitget Classic 与 UTA 现货市价买单使用计价币预算；可用 `quote_budget_exact` 表示最多花费的金额，不能把它填入基础币数量。参考 [strategy_bitget_quote_budget.py](../python/examples/strategy_bitget_quote_budget.py)，并将策略标的与冻结市场规格配置为 Bitget 现货标的。该字段只允许现货市价买单，限价单、卖单和合约单都会拒绝；风控会按预算金额检查账户可用计价币、最小/最大名义金额并预留未花完的预算。成交记录使用交易所返回的精确计价币成本。Bitget UTA 通过 UTA Spot `qty` 字段发送精确预算，避免将金额按小币种的基础币数量精度截断。

## Prepare

Use a Python environment with the project native package installed, then install the optional CCXT dependency:

```powershell
Set-Location D:\aex_work\qianxing\python
python -m pip install -e ".[ccxt]"
```

From the repository root, run the offline Rust/Python contracts with one command:

```powershell
.\scripts\crypto-live-offline-check.ps1
```

After setting the sandbox variables below, optionally include a read-only private-account probe:

```powershell
.\scripts\crypto-live-offline-check.ps1 -ProbeConfig .\python\examples\ccxt-okx-swap.example.json
```

Set the exchange's `QX_CCXT_*` environment variables in the same PowerShell window first. Probe paths are relative to the repository root. Choose one of the other `python/examples/ccxt-*.example.json` files for Binance Spot/USDⓈ-M, OKX Spot, or Bitget Spot/Classic/UTA. The optional probe only reads balances and derivative settings; it does not place/cancel orders or change account configuration. It also does not certify order acceptance.

The REST connector is pinned to CCXT 4.5.89 and refuses to initialize against another version until that version passes the request-shape contract. Before using keys, run the local no-network contract tests (the request builders are exercised without calling exchange endpoints):

```powershell
python -m unittest discover -s tests -p 'test_ccxt*.py' -q
python -m unittest discover -s tests -p 'test_packaging_contract.py' -q
```

Create sandbox/demo credentials in each exchange's own account. Set the values only in your local PowerShell session or secret manager; keep the JSON examples unchanged because they contain environment-variable names, not keys.

```powershell
$env:QX_CCXT_BINANCE_API_KEY = '<your sandbox key>'
$env:QX_CCXT_BINANCE_SECRET = '<your sandbox secret>'
$env:QX_CCXT_OKX_API_KEY = '<your demo key>'
$env:QX_CCXT_OKX_SECRET = '<your demo secret>'
$env:QX_CCXT_OKX_PASSPHRASE = '<your demo passphrase>'
$env:QX_CCXT_BITGET_API_KEY = '<your sandbox key>'
$env:QX_CCXT_BITGET_SECRET = '<your sandbox secret>'
$env:QX_CCXT_BITGET_PASSPHRASE = '<your sandbox passphrase>'
```

## 多账户、多标的的配置方式

每个真实交易所子账户使用独立的 `account_id` 和 API Key 环境变量名；同一真实账户的现货与合约 Worker 使用同一个 `account_id`，以便账户事件、费用和持仓落在同一账户账本。需要在组合只读接口中汇总的账户可配置相同 `portfolio_id`；该分组只做逐币种资产/权益汇总与可选 FX 折算，不合并账户保证金或购买力。

```json
"accounts": [
  { "id": "binance-spot-sandbox", "settlement_currency": "USDT", "portfolio_id": "crypto-sandbox" },
  { "id": "okx-demo-swap", "settlement_currency": "USDT", "portfolio_id": "crypto-sandbox" },
  { "id": "bitget-uta-sandbox", "settlement_currency": "USDT", "portfolio_id": "crypto-sandbox" }
]
```

为同一交易所的第二个子账户复制对应 `python/examples/ccxt-*.example.json`，把 `account_id` 与 `credential_env` 改成该子账户的唯一 ID/环境变量名。运行时每个私有 Worker 的 `account_id`、`venue_id`、CCXT 配置里的 `exchange_id` 必须对应；同一 Worker 的 `symbols`、CCXT 配置 `instruments` 和冻结规格集合都要覆盖它负责的标的。多标的执行使用 `qianxing_market_spec_set`，按完整 `InstrumentId` 为每个市场分别保存规格，不能把一个币种的 step/tick 复用给其他币种。

规格应从对应 Sandbox 可公开读取的市场元数据逐标的生成，再审核后写入集合；这一步只读公开市场规则，不需要账户密钥，也不会下单：

```powershell
Set-Location D:\aex_work\qianxing
New-Item -ItemType Directory -Force .\data\crypto-sandbox | Out-Null
cargo run -p qx-cli -- ccxt-market-spec `
  .\python\examples\ccxt-binance-spot.example.json `
  BTC/USDT.BINANCE `
  .\data\crypto-sandbox\binance-btc-spot.json
```

按相同方式为每个标的生成单市场规格，并把每份输出 JSON 对象按对应 `InstrumentId` 放入 `markets` map；同时让 CCXT 配置的 `instruments` 覆盖这些标的。不同产品类型、杠杆档位或账户设置应使用匹配的 CCXT 配置。`docs/examples/crypto-market-spec-set-v1.example.json` 展示集合结构，但其中的数值仅供阅读，不能直接作为交易所当前规则使用。完成集合后先运行 `cargo run -p qx-cli -- runtime-check <runtime.json>`；该命令只校验配置/文件引用，不连接交易所。随后逐个运行下方只读私有探针，再由操作者在 Sandbox 验证订单生命周期。

## Run one venue at a time

## Binance / OKX / Bitget 多市场合约样例

仓库提供两份可离线校验的双交易所样例：

- [qianxing.runtime.multi-venue-arbitrage.example.json](../deploy/qianxing.runtime.multi-venue-arbitrage.example.json) 使用 Rust 内置跨交易所价差策略，Binance USDⓈ-M 与 OKX Swap 分别使用独立账户、行情、执行、对账和裸腿恢复 Worker。两腿是 USDT 线性永续，逐仓、单向持仓、2 倍杠杆；目标名义金额为 100 USDT，按每家冻结规格分别换算成交易所数量。
- [qianxing.runtime.multi-venue-contract.example.json](../deploy/qianxing.runtime.multi-venue-contract.example.json) 使用 Python Strategy API，输入同时包含 OKX 主 Bar 和 Binance `market_bars`，根据两边对齐价格计算基差；每条意图通过 `account_id` 路由到独立账户，带精确合约数量、单向方向、逐仓和 2 倍杠杆。缺任意一边已对账的精确持仓时不发出意图。

这两份运行时配置都保持 `live_enabled: false`，CCXT 端点配置为 Sandbox/Demo。静态 Bar 与冻结精度文件只供离线拓扑验收和策略接口示例使用，不是交易所当前市场规则。实际 Demo 运行前，按 Sandbox 最新公开市场元数据重新生成并审核 Binance、OKX（以及替换为 Bitget 时的 Bitget）精度；不要把样例 step、tick、合约面值直接当作当前值。

先做不连交易所的配置验收：

```powershell
Set-Location D:\aex_work\qianxing
cargo run --release -p qx-cli -- runtime-check .\deploy\qianxing.runtime.multi-venue-arbitrage.example.json
cargo run --release -p qx-cli -- runtime-check .\deploy\qianxing.runtime.multi-venue-contract.example.json
```

未在本机设置凭据变量时，`runtime-check` 可提示 CCXT 凭据尚不可用，但仍会完成配置、拓扑、精度文件和数据文件引用检查；它不会读取凭据值、建立网络连接或发单。

后续只在本机 PowerShell 会话设置各自的 Sandbox/Demo 凭据。OKX 的 passphrase 只放在 OKX CCXT 配置引用的环境变量里：

```powershell
$env:QX_CCXT_BINANCE_API_KEY = '<Binance USD-M Testnet Key>'
$env:QX_CCXT_BINANCE_SECRET = '<Binance USD-M Testnet Secret>'
$env:QX_CCXT_OKX_API_KEY = '<OKX Demo API Key>'
$env:QX_CCXT_OKX_SECRET = '<OKX Demo Secret>'
$env:QX_CCXT_OKX_PASSPHRASE = '<OKX Demo Passphrase>'
```

Bitget UTA 永续对应 [ccxt-bitget-uta-perpetual.example.json](../python/examples/ccxt-bitget-uta-perpetual.example.json)，变量为 `QX_CCXT_BITGET_API_KEY`、`QX_CCXT_BITGET_SECRET`、`QX_CCXT_BITGET_PASSPHRASE`。增加 Bitget 执行腿时，要同时更新该 Worker 的 `account_id`/`venue_id`/`symbols`/`endpoint`、CCXT 配置的 `account_id`/`instruments`、账户表和精度集合中的完整 `InstrumentId`；只替换 endpoint 不会自动转换交易所合约面值或精度。

本机 Sandbox/Demo 运行时，先复制运行时 JSON 到本地文件，将本地副本的 `strategy.live_enabled` 改为 `true`，再执行 `runtime-check`。通过后才启动：

```powershell
cargo run --release -p qx-cli -- supervise .\deploy\qianxing.runtime.multi-venue-arbitrage.local.json
```

该命令会持续读取行情并按策略信号提交 Sandbox/Demo 委托；首次运行只用 Sandbox/Demo Key，并确认账户是可清空的小额测试账户。策略样例使用 `one_way` + `position_side: net`；要测试双向持仓，需让交易所账户预先处于 Hedge 模式，并为每条意图显式选择 `position_mode: hedge` 与 `position_side: long/short`。代码支持这类参数，但本地离线验收不证明具体 Sandbox 当前已接受杠杆、持仓模式或委托参数。

`ccxt-fetch-ohlcv` 可分别取回每家交易所的历史 Bar 做离线研究；实时拓扑则为各交易所配置独立 MarketData Worker。自定义 Python/JSONL 策略通过输入的 `bars` 读取主市场，通过 `market_bars[完整 InstrumentId]` 读取配置的对冲市场，运行时会拒绝未来时间和两边最新 Bar 未对齐的输入。多腿策略仍须把两家账户的保证金和风险分别计算，跨交易所没有原子成交保证。

```powershell
python -m qianxing_ccxt.private_probe --config .\examples\ccxt-binance-spot.example.json
python -m qianxing_ccxt.private_probe --config .\examples\ccxt-binance-usdm.example.json
python -m qianxing_ccxt.private_probe --config .\examples\ccxt-okx-spot.example.json
python -m qianxing_ccxt.private_probe --config .\examples\ccxt-okx-swap.example.json
python -m qianxing_ccxt.private_probe --config .\examples\ccxt-bitget-spot.example.json
python -m qianxing_ccxt.private_probe --config .\examples\ccxt-bitget-uta-spot.example.json
python -m qianxing_ccxt.private_probe --config .\examples\ccxt-bitget-perpetual.example.json
python -m qianxing_ccxt.private_probe --config .\examples\ccxt-bitget-uta-perpetual.example.json
```

The Bitget `spot` and `perpetual` examples use Classic demo mode; the `uta-spot` and `uta-perpetual` examples explicitly enable Bitget UTA. Run the example matching the demo account mode. Bitget demo REST requests require the `PAPTRADING=1` header; CCXT's sandbox mode supplies it. See [Bitget Demo Trading REST API](https://www.bitget.com/docs/classic/demo-trading/rest-api).

Each output reports connection status and asset/position counts. For configured derivative instruments it also prints the observed position mode, margin mode, and long/short leverage so the operator can compare them with the strategy policy. It never prints balances, positions, API keys, or secrets. `sandbox=true` is required; the probe exits before connecting if it is false. Remove the process-local environment variables when finished:

```powershell
Remove-Item Env:QX_CCXT_BINANCE_API_KEY, Env:QX_CCXT_BINANCE_SECRET -ErrorAction SilentlyContinue
Remove-Item Env:QX_CCXT_OKX_API_KEY, Env:QX_CCXT_OKX_SECRET, Env:QX_CCXT_OKX_PASSPHRASE -ErrorAction SilentlyContinue
Remove-Item Env:QX_CCXT_BITGET_API_KEY, Env:QX_CCXT_BITGET_SECRET, Env:QX_CCXT_BITGET_PASSPHRASE -ErrorAction SilentlyContinue
```

For explicit multi-account read-only portfolio aggregation, configure the same `portfolio_id` on each account in the runtime account registry, start the corresponding account workers, and call `GET /account/portfolio?portfolio_id=<id>`. The response preserves each account's native cash breakdown, portfolio cash by currency, exchange-reported unrealized PnL by settlement currency, and `equity_by_currency_decimal` when every open derivative position has a PnL and settlement currency and every open spot position is already represented by base-currency cash. `cash_by_currency_decimal` and `equity_by_currency_decimal` preserve exact decimal values; `*_raw_exact` remain strings containing legacy 1e9-scale raw integers. Add `&reporting_currency=USDT` to request indicative cash/equity equivalents using fresh, configured Spot market specs and the latest MarketData EventLogs. Bid/ask and FX path rates retain up to 18 decimal places, including prices below `1e-9`; older events without exact quote fields use their legacy 1e9 values. Routes are limited to quotes no older than 30 seconds, and missing or ambiguous FX paths leave the equivalent null. Derivative balances use exchange wallet-cash fields where available, separately from equity/UPnL; ambiguous Bitget Classic equity without an unrealized-PnL component fails closed. This read model does not net account margin or provide joint buying power: scalar `equity_raw` stays null and `margin_netting` stays false.

An execution worker that trades multiple instruments must use a frozen market spec set rather than reuse one single-market spec for every symbol. The set format is documented in [`worker-market-spec-set-v1.json`](../schemas/worker-market-spec-set-v1.json), and [`crypto-market-spec-set-v1.example.json`](examples/crypto-market-spec-set-v1.example.json) shows two symbols keyed by their complete `InstrumentId` (symbol plus venue). Every entry is checked against its key, including entries not selected by a particular worker, so multiple workers may share one set without an invalid extra entry escaping startup preflight. This validation also runs when `symbols` is empty (the worker is unrestricted); empty, malformed, or identity-mismatched sets are rejected. The example tick sizes and contract sizes are illustrative; replace them with the current CCXT market metadata or a reviewed frozen product spec before enabling execution. A missing worker symbol, malformed entry, or mismatched identity fails runtime market-spec validation.

Example after the local API is running (replace the port and portfolio ID with the runtime configuration):

```powershell
$apiBase = 'http://127.0.0.1:8080'
Invoke-RestMethod "$apiBase/account/portfolio?portfolio_id=main&reporting_currency=USDT"
```

For exact raw integers in JavaScript or other clients with limited integer precision, consume the `*_raw_exact` fields as decimal strings, including cash, PnL, and equity maps.

Paper account risk valuation can resolve a required currency through the shortest configured Spot FX path (for example, `BNB -> BTC -> USDT`); equally short paths with different rates fail closed. The live portfolio read model reports native-currency equity and can convert it through configured Spot FX paths; it still does not provide shared cross-account buying power.

## Boundaries

- Do not use production credentials with this smoke. It requires sandbox mode and is read-only by construction.
- The read-only probe does not prove market-specific order precision, order submission, cancel/replace, conditional orders, fee settlement, hedge-mode setup, leverage changes, or recovery after ambiguous submissions. Those paths have local fake-worker contracts but still need venue sandbox acceptance before release.
- Before a contract order, the execution adapter reads the exchange position mode, margin mode, and long/short leverage and rejects any mismatch with the strategy policy. It passes the verified mode on OKX and Bitget Classic; Bitget UTA verifies its account/market setting without sending the Classic-only `marginMode` field. Binance futures uses the account's already configured margin mode. It does not change account modes or leverage as an order side effect. Configure the sandbox account first; the read-only smoke above does not alter these settings.
- The CCXT live order path and native Binance Spot path carry exact decimal quantity, price, fills, fees, and balance snapshots through market rules, risk, order recovery, ledger and account reports. Exact market orders preserve ticker decimals separately from the 1e-9 compatibility projection, require a timestamp no more than 30 seconds old, and store the selected ask as risk/reservation metadata; it is never sent as a limit price. Exact limit orders use their limit as the risk reference. Bitget Spot market buys use quote-budget sizing in both Classic and UTA modes; native Binance Spot market buys use `quoteOrderQty`. Paper matching still rejects amounts above 9 decimal places.
- Inactive markets are rejected before submission. Price/amount min/max and notional min/max are checked when CCXT exposes them. For non-reduce-only contract orders, the worker reads same-market positions, regular open orders, and CCXT leverage tiers; linear quote-notional tiers also use a reference ticker, while OKX contract-count tiers are compared in contracts and Bitget inverse tiers use contract face value without a price lookup. Bitget Hedge tiers use the larger directional exposure, including that side's ordinary opening orders and the candidate order; other venues and One-way remain conservatively gross-summed. The estimate rounds exposure upward and rejects a requested leverage above the selected tier. Binance uses its batch tier endpoint; OKX and Bitget use their single-market endpoint. Missing/ambiguous tiers fail closed; a non-zero linear position without mark price blocks quote-notional tier checks. This is a preflight, not an account reservation: hidden conditional orders, concurrent external orders, and state changes between reads and submission may be absent, and the venue remains authoritative. OKX public tiers may not reflect special account tiers. CCXT venues still lack a complete native matrix for percent-price bands, average-price windows, account-level limits, and filters CCXT does not expose.
- The native Binance Spot route compares current public `exchangeInfo` status, base/quote, exact `PRICE_FILTER.tickSize`, `LOT_SIZE.stepSize/minQty`, and minimum notional with the frozen spec before the order write. It also checks exact `MARKET_LOT_SIZE`, static notional bounds, `PERCENT_PRICE(_BY_SIDE)`, and market `MIN_NOTIONAL`/`NOTIONAL`. Dynamic filters use `/api/v3/referencePrice` when available; if Binance reports no reference price, the adapter falls back to last price for a zero-minute window or `/api/v3/avgPrice` only when its returned window exactly matches `avgPriceMins`. Metadata is cached for 60 seconds and exact reference prices for 1 second; these are public requests without API credentials. Account-level limits, remaining Binance filters, and venue-side acceptance remain outside this local preflight.
- Immediately before shared Live risk submission, CCXT refreshes market metadata when the worker cache is older than 60 seconds, then compares the frozen risk spec with the refreshed market for product, currencies, contract size, price tick, amount step, and any min amount/notional/max leverage/maintenance rate that CCXT exposes. When `exact_rules` is frozen, its decimal contract size, tick, lot, minimum quantity and minimum notional are compared too, including changes hidden by the legacy 1e-9 grid. Known mismatch or missing mandatory precision metadata is a deterministic pre-submit rejection; it does not disconnect the Worker or become an unknown order. This catches exposed core spec drift, but does not refresh the frozen file or prove fields that the venue/CCXT does not expose.
- Order amount follows the product's native unit: ordinary Spot orders use base-asset quantity, Bitget Spot market buys use quote-currency budget, native Binance Spot market buys use `quoteOrderQty`, and CCXT derivatives use contract counts with `contractSize` applied during notional and PnL accounting. Rust marks every CCXT request with `base`, `quote`, or `contracts`; the JSONL worker requires the field and checks it against the resolved market before making an exchange call. OKX Spot market buys explicitly set `tgtCcy=base_ccy`.
- Market orders still have no configured maximum-slippage collar. The pre-trade risk reference uses a fresh ticker, but that reference does not cap the venue execution price. Prefer limit orders where a hard price bound is required.
- A CCXT cancel acknowledgement is no longer treated as a terminal cancel fact: the adapter fetches the authoritative order afterward and routes any new partial fill through the same reconciliation path before emitting cancellation.
- After a process restart, if an order gained fills before its first reconciliation, CCXT rebuilds the previous cumulative cost only when order-filtered trade history covers the remote filled quantity and the durable local filled quantity is an exact trade boundary. Missing/incomplete history, missing trade timestamps, or a boundary inside a trade fails closed into reconciliation. Local fake-worker tests cover exact partial-fill recovery and idempotent replay; sandbox trade history has not been exercised.
- Signed fees/rebates are preserved. CCXT itemized `fees[]` and Binance `commissionAsset` are carried into per-currency ledger rows; mixed fee currencies are never added together. The account snapshot keeps `fees_raw` null when charges span currencies and reports exact raw totals in `fees_by_currency_raw` and each fill's fee rows. Missing currency on a nonzero charge fails closed into reconciliation. Trade history is requested with the venue-specific order filter (`orderId` for Binance/Bitget, `ordId` for OKX), and every returned row must identify its order before it can contribute fees or recovered cost basis.
- CCXT order-level cumulative fees are diffed against the last observed total per currency, while trade-level fees are deduplicated by trade ID; the contributing IDs are persisted with each fee row and restored from the EventLog after restart. If fee data arrives after the quantity stops changing, the adapter emits a first-class fee-adjustment fact and the runtime books the currency-specific debit or rebate without fabricating a zero-quantity fill. The baseline is rebuilt from durable fill/adjustment facts after restart. Local tests cover fee reconciliation and replay; no live or sandbox exchange has been used for validation.
- The native Binance adapter is Spot. Binance derivatives, OKX, and Bitget execution use the CCXT process adapter and are limited to the declared `Market`/`Limit` capability surface.
- Multi-leg orders can route each leg to its configured account and venue. Confirmed partial fills are durably compensated by the matching account/venue worker and the group closes only after every source leg is terminal and its confirmed exposure is hedged. This does not combine collateral or buying power, and exchanges do not provide cross-venue atomic orders; each account keeps its own margin and liquidation boundary.
- The current smoke proves only private read access. It does not certify order acceptance, every exchange-specific filter, or production execution readiness. No account or order calls were made while preparing this code change.
