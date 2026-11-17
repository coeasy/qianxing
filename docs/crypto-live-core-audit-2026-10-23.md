# 牵星数字货币实盘核心链路深度复核（2026-10-23）

## 当前结论（截至 2026-10-24 工作区）

Binance、OKX、Bitget 的主要现货与合约 Market/Limit 执行链已经贯通。CCXT 路径支持精确订单量/价格、合约双向参数、杠杆档位预检、账户级资金预留、逐笔成交与多币种费用恢复；手续费归一化为 float 时会用原始 `info` 十进制字段校验恢复，缺失或不匹配则拒绝伪精确记账。Binance Spot 的 CCXT 与原生适配路径都在发单前检查 `MAX_POSITION`，按 base asset 汇总 free/locked 余额及全账户 open BUY 剩余量。该过滤器启用时 quote-budget 买单 fail-closed。精确账簿与组合读取支持多币种现金/盈亏和可选报告币折算。多账户可按账户隔离路由、恢复和补偿。

因此，三家交易所的**受支持订单路径已具备本地验证基础**，可以用本机环境变量继续做 Demo/Testnet 只读连通及后续沙盒订单验收；但仍不能认定所有实盘能力全部实现或达到生产发布条件。

### 当前未闭合的能力与发布边界

- 多账户之间不共享保证金、购买力或强平风险，跨账户/跨交易所组合提交也不具原子性；现有“联合”指账户隔离的路由、协调和补偿。
- 精确金额已贯穿支持的 CCXT 和原生 Binance Spot 路径，但 Paper 撮合、`target_qty` 调仓以及部分内置/C ABI/控制入口仍有 1e-9 raw 刻度限制，不能据此宣称全策略入口均支持任意小币种精度。
- Venue 执行契约目前以 Market/Limit、查询、撤单和对账为主；条件单、止损止盈、OCO、追踪单、改单/替换及硬滑点保护未完整实现。
- 市场规则和杠杆档位存在交易所/账户等级差异与读取后到下单前的竞态；本地 fail-closed 预检不等于交易所资金预留或受理保证。
- Binance 原生与 CCXT Spot 路径均对读取到的全部公开 openOrders 做 `MAX_POSITION` 本地检查；其他客户端在读取后并发新增订单的窗口仍无法由本地预检消除，Binance 端过滤仍是最终判定。
- 本轮只运行离线/fake 契约、架构检查及 workspace 编译；没有读取密钥、请求真实账户或提交订单。Demo/Testnet 的私有 API、订单受理、部分成交、撤单、断线恢复仍需用户随后用本机沙盒凭据验收，因此当前工作区不是已发布版本，也不应标记为生产就绪。

快捷命令和三家交易所的环境变量示例见 [`crypto-live-readonly-smoke.md`](crypto-live-readonly-smoke.md)。

### 首次 2026-10-24 复核记录（由下方后续补充更新）

- 本次针对现有代码重新运行 `scripts/crypto-live-offline-check.ps1`：11 个相关 Rust crate 库测试共 706 项通过，格式检查通过，架构快速门禁 4 片覆盖 225 个唯一检查通过，CCXT Python 请求/配置用例 63 项（62 通过、1 跳过），Python 打包契约 6/6 通过。另行运行 `cargo check --workspace --all-targets` 与 `git diff --check` 均通过；Git 仅提示工作树的 LF/CRLF 转换信息。
- 没有读取 API 凭据，没有请求交易所账户，也没有提交或撤销订单。以上证据证明本地代码和隔离契约的状态，不证明供应商受理、真实账户模式或沙盒成交行为。
- 精度结论由核心数值类型、事件/账簿 raw 字段和账户快照协议共同确认：`Price`、`Quantity`、`Money` 都按 `1e9` 存储；现有协议把 raw 数值作为整数持久化并纳入摘要/哈希。要完整支持更细小数，必须同步升级内核算术、成交/账簿、事件兼容、快照 schema 和 Python/CCXT 编码；局部把比例常量改为 `1e18` 会重新解释旧 raw 值，不能安全上线。
- 精度能力更新：已加入带 `coefficient + scale` 的 `qx_core::ExactDecimal`，并把可选精确金额载荷接到 `Order`、`Fill`、事件摘要、OMS 成交累计，以及协议账户快照的订单/成交行和 state hash。账簿支持精确现货、线性/反向永续与期货成交，逐币种费用、合同乘数、已实现盈亏和 entry 重放；精确保证金/期权路径仍不支持。执行风控、Venue、在途资金预留和 CCXT Worker 订单/成交 wire 还没有消费精确金额，精确订单仍在提交入口被拒。**所以第 10 位及之后仍不能用于实盘委托，本项 P1 未关闭**。
- 快照订单表现已消费 `exact_amounts` sidecar：对 exact order 显示精确数量/已成交量，并兼容不带该字段的旧快照；前端用 `BigInt` 按系数和 scale 格式化，不经过 IEEE-754。该显示改进不扩展风控、Venue、账簿或 Worker wire 的精度能力。
- 同步成交事实校验改为用精确十进制比较成交量、剩余量和限价方向；买单成交不得高于限价，卖单不得低于限价。此处只验证订单回报的一致性，不替代下单前精度/资金风控，也不关闭 9 位账簿上限。
- 新增 `scripts/crypto-live-offline-check.ps1`，从仓库根目录一条命令运行 Rust 精度/执行契约、架构快速门禁和 Python CCXT/包装契约；可选 `-ProbeConfig` 只读检查沙盒余额/合约设置。脚本不会下单、撤单或切换账户模式，MSVC 临时目录放入被忽略的 `target/qx-test-temp`。
- 本轮此前运行的 `cargo check --workspace --all-targets` 通过；最新离线脚本对 11 个相关 Rust crate 库测试共 690 项全部通过。新增检查覆盖亚纳米订单/成交模型、事件摘要、OMS 累计，以及未接精确算术时风控、Venue 和余额预留必须 fail-closed。没有运行外部账户或交易所请求，没有提交/撤销订单；这些测试只证明本地代码路径。
- 与供应商现行公开契约对照后，交易所差异仍是实质能力边界：Binance Spot 的百分比价格与部分名义额过滤器会优先使用非空 reference price，再回退到规定均价窗口；OKX 每笔订单带交易模式，现货市价单数量币种可配置且合约单位为张；Bitget Classic hedge 通过 `side + tradeSide` 表达开平仓，其现货市价买使用 quote 币预算。Binance Spot 动态过滤器现已纳入本地预检，但交易所级过滤器覆盖仍未完整。参考：[Binance Spot filters](https://github.com/binance/binance-spot-api-docs/blob/master/filters.md)、[Binance Spot API changelog](https://github.com/binance/binance-spot-api-docs/blob/master/CHANGELOG.md)、[OKX API V5](https://my.okx.com/docs-v5/en/?language=python)、[Bitget Classic place order](https://www.bitget.com/api-doc/classic/contract/trade/Place-Order)、[Bitget Spot trade](https://www.bitget.com/zh-CN/docs/catalog/classic-spot-trade/classic-spot-trade)。

本轮修改范围包括精确小数模型、OMS 归约、精确现货账簿和未接通精确执行链前的拒绝闸门，并复核相关执行路径；没有加载 API 密钥、连接交易所、读取账户、提交/撤销订单。Windows 本机构建需将 Cargo debug info 设为 0，避免 MSVC C 编译器自身的 D8050 调试记录错误。CCXT 执行依赖现已固定为 4.5.89，并在运行时拒绝未核验版本；后续版本升级须先跑离线参数契约矩阵。

### 2026-10-24 复核补充

- 合约开仓补上杠杆档位预检。锁定版 CCXT 4.5.89 的能力表显示 Binance 有 `fetchLeverageTiers`，OKX/Bitget 则只有 `fetchMarketLeverageTiers`；现在按能力选择对应方法。Rust→Worker 委托意图携带目标杠杆、保证金模式和 reduce-only；Worker 拒绝缺少杠杆/保证金模式的合约请求。非 reduce-only 下单前，适配器读取同标的仓位、未结普通委托和适用 tiers；线性 quote-notional tiers 额外读取 ticker，保守求和当前仓位、普通挂单剩余量和候选订单名义额并向上取整。OKX tiers 以合约张数为边界，直接合计 contracts；Binance inverse tiers 按 contracts；Bitget inverse tiers 按公开档位的 position value 与 CCXT 合约面值计算 quote notional，不依赖线性价格公式。适用档位缺失/不唯一、线性仓位缺 mark price、档位缺 max leverage 或策略杠杆超过档位上限时 fail-closed。reduce-only 关闭开仓档位检查，避免阻断减仓。此检查不是资金预留：并发 worker、交易所外部/条件订单、价格和档位变化及读写竞态仍由交易所最终裁决；名义价值型开仓会增加 ticker（反向合约除外）、仓位/普通挂单/tiers 读取调用，需关注限频和延迟。OKX 特殊账户有效档位与公开 tiers 可能不同，仍需供应商验收。
- 杠杆档位修复的本地证据：隔离临时目录加载 CCXT 4.5.89 后，Python `test_ccxt*.py` 62/62 通过；额外读取本地安装包的能力表确认 Binance 为 batch=True/market=emulated，OKX/Bitget 为 batch=False/market=True。假交易所覆盖方法路径选择、OKX contract-unit 阈值、Bitget inverse face-value 阈值、合计持仓/挂单跨档拒单、reduce-only 跳过，以及预检拒绝不进入未知委托对账。Rust CCXT 定向测试 25/25、qx-service 单测 47/47、Python 全量 unittest 260 项通过/2 项跳过、架构门禁 225/225、`cargo fmt --all -- --check` 通过。只读探针 `--help` 入口也通过；全程未访问交易所网络/账户或发单。锁定版接口见 [CCXT 4.5.89 Binance](https://github.com/ccxt/ccxt/blob/v4.5.89/python/ccxt/binance.py)、[OKX](https://github.com/ccxt/ccxt/blob/v4.5.89/python/ccxt/okx.py)、[Bitget](https://github.com/ccxt/ccxt/blob/v4.5.89/python/ccxt/bitget.py)。

## 2026-10-24 Bitget Hedge 杠杆档位按方向计算

- Bitget Hedge 的持仓档位按多空两侧中较大的一侧计算，侧向规模还会包含该侧未成交开仓委托。预检现按 CCXT 4.5.89 的 `hedged=true` 买单→long、卖单→short 映射，分别累计对应方向的持仓、普通开仓挂单和候选单，再以两侧较大值选择档位；One-way 与其他 venue 仍沿用同标的保守总敞口计算。若现有方向无法识别，预检拒绝继续，避免低估。规则与参数映射：[Bitget 仓位档位](https://www.bitget.com/futures/introduction/position-tier)、[Bitget classic 仓位档位接口](https://www.bitget.com/zh-CN/docs/catalog/classic-contract-position/classic-contract-position)、[CCXT Bitget 4.5.89](https://github.com/ccxt/ccxt/blob/v4.5.89/python/ccxt/bitget.py)。
- 离线回归覆盖 Hedge 方向分别累计（含同方向开仓挂单）、与 One-way gross 口径的差异，以及 JSONL worker 从请求参数到预检的贯通；`python -m unittest discover -s python/tests -p 'test_ccxt*.py' -q`：57 项通过、1 项跳过。此变更没有连接账户或发送订单。它仍是下单前估算，不是资金预留；真实供应商验收和跨账户购买力仍未完成。

## 2026-10-24 精确十进制核算基础扩展

- `ExactDecimal` 新增无浮点定尺度乘除、显式向零/远离零/向下/向上/银行家舍入，以及十进制精确步长整除检查。`TradingInstrumentSpec` 的名义额、报价名义额、初始/维持保证金、未实现盈亏和资金费已使用这套十进制运算：风险名义额/保证金向上保守取整，交易所最小报价金额向零取整，PnL 向负无穷取整以避免低估损失或高估收益；中间乘积由十进制字符串计算，避免先用 i128 中间乘法溢出。
- 本地回归覆盖正负数方向舍入、half-even、10^-10 步长整除、低于 raw 网格的风险金额，以及低于 raw 网格的未实现损失。完整 `scripts/crypto-live-offline-check.ps1` 通过：相关 Rust 库测试、格式、架构 225 项、CCXT Python 57 通过/1 跳过、包装契约 6/6。首次直接编译遇到本机 MSVC D8050，使用仓库脚本设置的构建环境后全套通过。
- **此项仍未打开超过 9 位的实盘金额。**内部 Ledger 已能精确累计现货 entry，但对外风险、账户读模型、在途单预留、衍生品结算和 Worker wire 仍以 1e9 raw 为主；执行入口仍对 exact order fail-closed。下一步需把精确订单、余额/仓位快照、资金预留、成交费用和恢复贯穿三家适配器及读模型，并维持旧 schema/hash 兼容。
- Live CCXT 冻结规格漂移检查：统一 Live 风控提交入口在静态风控后、Venue 下单前调用适配器规格核验；CCXT Worker 的 market 元数据缓存超过 60 秒会刷新，再将冻结规格与当前元数据比较产品/币种/结算币种、合约乘数、tick/lot，以及交易所实际暴露的最小量、名义额、最大杠杆和维持保证金率。字段缺失或不同会作为确定的下单前拒绝返回，不进入未知委托对账。Rust→Worker 协议与 Python 假交易所覆盖命中、超时刷新和精度漂移；尚不能核验交易所没有暴露的字段，也不取代完整过滤器矩阵和冻结规格更新/到期机制。
- 原生 Binance Spot 规格与订单规则检查：同一共享入口要求 Spot Venue 读取公共 `exchangeInfo`，比对交易状态、base/quote、`PRICE_FILTER.tickSize`、`LOT_SIZE.stepSize/minQty` 与 `MIN_NOTIONAL`/`NOTIONAL.minNotional`；订单级 hook 按 Market/Limit 检查 `MARKET_LOT_SIZE`、静态名义额、`PERCENT_PRICE`、`PERCENT_PRICE_BY_SIDE` 和市价 `MIN_NOTIONAL`/`NOTIONAL`。动态过滤先请求 `/api/v3/referencePrice`；reference price 不存在时才回退到 last price 或 `/api/v3/avgPrice`，并要求回退均价窗口与 filter 的 `avgPriceMins` 完全相同。缺数据、窗口不匹配、请求失败或计算超界均在下单 POST 前 fail-closed。规格缓存 60 秒、reference price 缓存 1 秒；这些都是无凭据公共 GET。fake transport 覆盖动态上下界、按买卖方向使用不同倍数、市价最低名义额和均价窗口不匹配。仍未覆盖委托数/账户级限制、其他 Binance filters 及读取规则到下单之间的竞态。
- 只读 sandbox 快捷样例补齐 Spot：现提供 Binance Spot/USDⓈ-M、OKX Spot/Swap、Bitget Spot/Classic 永续/UTA 永续七份独立配置。每份都要求 `sandbox=true`，凭据仅从 `credential_env` 指定的本地环境变量取值；配置契约用例验证 venue、市场类型、变量引用和标的 venue 后缀。新加的 OKX/Bitget Spot 探针在变量为空时已验证会在创建 CCXT 客户端前退出。OKX Spot 市价买明确传 base currency sizing；Bitget Spot 市价买在价格查询前拒绝，避免当前 base 数量意图被误解释为 quote 预算。该证据只证明离线参数/配置边界，不证明交易所私有 API 实际可连通或受理委托。
- CCXT TICK_SIZE 现在按有理数精确计算交易所 tick 与 1e-9 核心定点网格的交集，例如 1e-10 和 3e-10 分别映射为 raw step 1 和 3；最小过滤限制向上取整、最大限制向下取整。复核同时发现并修复市价 `MARKET_LOT_SIZE.maxQty` 子纳米上限被 `or` 真值回退丢弃的缺陷：raw 0 作为有效的保守最大值保留，正数订单在请求行情/提交订单之前 fail-closed。此改动没有扩展 core 的 9 位表达能力，超过 9 位的实际成交、持仓或账单仍无法无损入账。
- Live Spot Sell 的账户余额预检此前使用 quote/settlement currency，账户只有 BTC 等 base 资产而无 quote 现金时会被误拒；同时可卖量仅按内部账簿持仓计算，未以交易所 `free - borrowed` base 余额设上限。现在 Live 卖出按 base 余额读取，数量须同时不超过账簿未平仓数量和交易所可用 base 数量，并扣除余额快照之后新挂且未终结的同标的卖单；快照前订单已由交易所 free 余额反映，避免重复扣减。离线行为用例覆盖无 quote 现金可卖与交易所余额较少时拒绝超卖，Live 风控 10/10；Rust fmt、架构 `--fast` 四分片聚合 225 项和 `git diff --check` 通过。
- 使用仓库内锁定版 CCXT 4.5.89 虚拟环境重跑全部 CCXT 相关离线 Python 用例，现为 54/54（含真实 OKX Spot `create_order_request` 的 base 数量/九位精度参数、Bitget Spot 市价买早拒绝、超细 tick 精确步长映射及市价数量上限 fail-closed）；用例只调用本地 formatter/request builder/fake exchange，没有访问 API。
- 数量单位已经按代码与 CCXT 合同核实：现货下单量是 base asset，合约下单量是 contract count，`contractSize` 用于换算名义额/盈亏；`Quantity` 类型本身没有单位标签，仍是 P1 风险。之前 smoke 文档中“统一订单数量总是 base units”的表述已更正。
- Rust→CCXT JSONL 下单意图现在必须带 `amount_unit`：现货为 `base`，衍生品为 `contracts`；Worker 拒绝缺失/未知单位，Python 在调用交易所前将其与已解析的 CCXT 市场类型核对。Rust 请求由账户市场设置中的 `contract` 字段决定，并有 spot 与 contract 请求测试。它能挡住进程边界及市场绑定错误，仍不能替代在领域订单/策略 API 中携带强类型单位；非 CCXT 原生 Binance Spot 路径也不经过该字段。
- 对照 Bitget 2026 年 UTA v3 官方迁移契约，发现此前把 Classic v2 的 `marginMode` 一并附加到 UTA v3 请求。现在 `fetch_contract_settings` 显式回传 `uta`，Rust 只在 Classic 路径发送 `marginMode`；UTA 路径继续读取并核对账户/标的的实际保证金模式，但下单 JSON 不带 Classic 的 `marginMode`。官方迁移指南要求 v3 删除 `marginMode`/`marginCoin`，使用 `category`、`qty`、`posSide`；已锁定 CCXT 4.5.89 的纯本地请求构造器和 Rust→Worker 测试均覆盖此差异。依据：[Bitget UTA 迁移指南](https://www.bitget.com/docs/classic/uta-api-upgrade-guide)、[Bitget UTA 下单参数](https://www.bitget.com/legacy-docs/uta/trade/Place-Order)。
- 三家官方 API 仍有明显不同：Binance 合约订单/过滤器使用产品专属规则；OKX 每笔订单带 `tdMode`，Hedge 合约单带 `posSide`，Spot 市价买默认使用 quote 币数量；Bitget Classic Hedge 用 `side` + `tradeSide`，UTA v3 用 `side` + `posSide` 并且采用不同请求字段。牵星将它们归一为 Market/Limit 与统一订单/事件账本，交易所专属条件单、OCO、追踪单、改单以及 Bitget Spot 市价买 quote-budget 意图仍未形成统一领域契约。参考：[Binance 过滤器](https://developers.binance.com/en/docs/products/derivatives-trading-options/common-definition)、[OKX Trade API](https://my.okx.com/docs-v5/en/?language=python)、[Bitget Classic 下单](https://www.bitget.com/api-doc/classic/contract/trade/Place-Order)、[Bitget UTA 下单](https://www.bitget.com/legacy-docs/uta/trade/Place-Order)。
- CCXT REST 依赖从开放的 `>=4.4.0` 收敛到 `4.5.89`，客户端运行时拒绝未核验版本；离线请求形状用例直接调用真实 CCXT Binance USDⓈ-M、OKX Swap、Bitget Classic/UTA 请求构造函数，不发送 HTTP 请求，并覆盖 8/9 位小币价格/数量格式化及 UTA v3 不发送 Classic `marginMode`。隔离安装的 4.5.89 环境现通过 CCXT 定向用例 47 项；本轮未重跑包装契约或全量 Python 套件。
- 多币种核算回归补上真实算例：FX resolver 可跨 venue 走两跳现货路径；缺失路径、等长但汇率冲突的路径都 fail-closed。显式 `fees[]` 按报告币种分别入账，不把 BNB 与 USDT 的 raw 相加；`fees_raw` 混币时留空，原币种总额保留在 `fees_by_currency_raw`。单标量旧 Fill 兼容路径仍单独使用其成交价折算/拒绝规则。
- 多账户多腿恢复现在按 `account_id + venue_id` 逐 worker 隔离补偿，逐腿持久化已确认的补偿数量和有限重试序号，确定性 client ID 因而可在部分成交、重启后续跑；完成条件还要求每条原始腿都已进入终态且已确认敞口全部对冲。全量进度已写盘但组状态提交前崩溃时，空补偿目标的恢复轮会在 claim 下完成状态收敛，不重复下单。跨账户组的共享抵押品、购买力和原子提交仍未实现。
- 快照 v2 的多币种费用映射改为复用统一 JSON map encoder；Schema 补全 `equity_raw`/PnL/费用等有生产者字段与 `margin_raw`/`frozen_raw`/`funding_raw` 无生产者字段的 null 语义。Windows 受限环境下 qx-service 与 qx-protocol 的文件测试 scratch 改到仓库 `target`。
- 已修复多标的执行规格断链：单 worker 可将 `instrument_spec_path` 指向 `qianxing_market_spec_set` v1，按完整 `InstrumentId` 为每个市场选独立冻结规格；旧单规格格式保持兼容。缺项、错误键、错误版本或与规格内 instrument/symbol/exchange 不一致均拒绝；共享集合中的未选中条目也会在启动预检时逐项验证，`symbols` 为空的 unrestricted worker 也不会跳过集合完整性预检。Schema 与双永续示例分别见 `schemas/worker-market-spec-set-v1.json`、`docs/examples/crypto-market-spec-set-v1.example.json`。这解决多标的 worker 的规格选择问题，不提供跨账户联合保证金/下单事务。
- 本次增量验证通过：`cargo test -p qx-core --test ledger` 26/26、`cargo test -p qx-service` 46/46、`cargo test -p qx-protocol --all-targets` 30/30、`cargo test -p qx-adapter ccxt_` 23/23、隔离 CCXT 4.5.89 的 `test_ccxt*.py` 47/47、`cargo fmt --all -- --check` 与 `git diff --check`。此前 Python 全量 `unittest` 235 项/3 项跳过早于本轮新增测试，未宣称为本次全量结果。
- 对齐持仓方向、费用/权益单币种旧判据、schema null 描述、手续费映射序列化和生命周期参数形状后，`python tools/check_architecture.py --fast` 四分片聚合退出码为 0。期间确认完整 CCXT 恢复路径由 `ccxt_reconcile_host` 和 `ccxt_user_stream_host` 调用 `restore_order_with_fee_state`，从 EventLog 恢复 remote order id、按币种手续费基线及成交去重 ID；两个便捷包装入口由测试/外部公开面保留。
- 这些验证全部离线执行；没有访问任何账户，也没有提交或撤销订单。因此三家交易所的真实账户模式/请求受理、沙盒成交/撤单竞态和供应商市场过滤器仍未验收。

## 当前实现边界

| 能力 | 当前实现 | 结论 |
|---|---|---|
| Binance 现货 | 原生 Binance Spot 适配器；Market/Limit、账户余额、订单查询/撤单和恢复；Live 下单前核验公共 `exchangeInfo`、静态过滤器、动态百分比价格带和市价名义额 | 已覆盖本次核验的常用过滤器；仍缺账户级/委托数等其他规则、供应商受理证据和规则读取到提交之间的竞态处理 |
| Binance 合约 | 通过 CCXT worker 接入；当前示例覆盖 USDⓈ-M；单向/双向、reduce-only、杠杆/保证金模式及合计敞口杠杆档位预检 | 参数路径已实现；COIN-M、交割合约等未以本项目运行样例证明；本地预检不是交易所资金预留 |
| OKX 现货/永续 | 通过 CCXT worker；双向仓位参数、杠杆/模式预检；合约开仓读单市场公开 tiers 并按 contracts 比较；现货市价买明确使用 base 数量 | 常规 Market/Limit 链路已实现；公开 tiers 不一定反映账户特殊档位；真实账户和 sandbox 未验收 |
| Bitget Classic/UTA | 有独立配置样例；CCXT 映射双向开平仓参数；UTA 通过 `options.uta` 选择；线性和反向合约开仓使用单市场 tiers，反向档位按合约面值估算 quote notional；Hedge 档位按较大方向分别累计仓位、同向普通开仓挂单及候选单；UTA/Classic 下单字段分离 | Classic 合约需产品类型、`marginMode`、`marginCoin`；UTA v3 改用 `category`、`qty`、`posSide` 并移除 Classic 的 `marginMode`/`marginCoin`。两种模式的账户/持仓设置层级、资产及成交接口都不同；均未通过真实供应商验收 |
| 多账户 | 账户注册表、隔离 worker/事件账簿、带账户/交易所路由的多腿组与逐腿补偿恢复；可按 `portfolio_id` 汇总只读现金 | 确认的部分成功敞口可跨账户/交易所恢复；账户保证金与购买力不共享，组提交不具跨所原子性 |
| 多标的与多策略 | worker 可配置多个 instrument；运行配置支持多个策略并绑定账户、交易所和 instrument | 配置及行情/策略路径存在；没有由此自动获得跨账户风险协调 |
| 现货、线性/反向合约 | 领域规格含 Spot、Perpetual、Future，以及线性/反向合约核算字段 | 覆盖有边界；Spot Margin 和 Option 的完整执行语义没有闭环 |
| 双向持仓 | Long/Short 方向校验；CCXT 参数映射为 Binance `positionSide`、OKX `posSide`、Bitget Classic `tradeSide`/UTA `posSide` | 锁定版 CCXT 4.5.89 纯本地请求构造器核对通过；未发单验证 |
| 逐仓/全仓 | 下单前读取实际模式、杠杆并拒绝与策略不符的账户；OKX 和 Bitget Classic 明确传 margin/trade mode；Bitget UTA 读取账户模式但不误带 Classic `marginMode`；Binance 使用账户既有模式 | Rust 与 Python 定向测试通过；供应商真实受理未验收 |
| 小币种精度 | 不使用浮点静默舍入；CCXT `TICK_SIZE` 以有理数求与 1e-9 core 网格的精确交集，formatter 必须保值；上下限按不放宽交易所约束的方向映射 | 交集中的小数位可用，不在交集或需要超过 9 位的交易金额仍拒绝；全币种/成交精度覆盖尚未完成 |
| 多币种账务 | 费用/返佣按币种记账，资金费等现金流进入事件账本；组合端按币种合计净钱包现金和带结算币种的交易所持仓浮盈，可选新鲜现货中间价折算 | 原币种净值读模型已贯通；跨账户保证金/购买力仍未实现 |
| 委托类型 | Venue 能力门控为 Market/Limit；有查询、撤单和对账 | 止损/止盈、条件单、OCO、追踪单、改单/替换不在当前执行契约内 |

### 三家交易所的能力差异与项目覆盖

| 交易所 | 当前代码覆盖 | 交易所语义中必须区别处理的部分 | 仍未达到的目标 |
|---|---|---|---|
| Binance | 原生 Spot；USDⓈ-M Futures 经 CCXT | 期货 Hedge 要求 `positionSide`，且不能发送 `reduceOnly`；现货与期货的 lot/filter 不同 | COIN-M/交割路径缺少本项目样例证明；不支持 Binance 跨子账户联合额度；杠杆档位只是下单前估算，非全产品完整过滤 |
| OKX | CCXT Spot / Swap | 每笔交易指定 `tdMode`；双向仓位需按 long/short 组合 side/`posSide`；衍生品数量单位是 contract | 真实 demo 请求未验收；当前订单契约只有 Market/Limit，未覆盖条件单、改单、组合保证金净额 |
| Bitget | CCXT Classic/UTA 有分开的配置路径和探针 | Classic Hedge 用 `side` + `tradeSide=open/close`；UTA 与 Classic 账户/请求形状不能混用；现货市价买使用 quote 预算 | 两种账户模式都缺供应商订单验收；当前 Bitget Spot 市价买明确拒绝；不同账户间无统一执行/保证金协调 |

三家的 Market/Limit 主干在代码中共用 Qianxing 的订单状态、回报归约和待对账语义，供应商字段映射则由 Binance 适配器或 CCXT 各自负责。该结构可以支撑单账户、单产品的受控验收，不代表账户间原子联合交易。CCXT 文档也明确区分 precision 与 limits，并规定合约订单量是 contracts、要按 `contractSize` 换算；这与本次核对的项目数量单位一致。

## 深查补充：代码能接通不等于完整交易能力

### 已落地的执行骨架

- Binance Spot 原生路径和三家交易所的 CCXT 路径都能从本地提交意图进入远端订单，再把查询/流回报归约到统一订单、成交和 EventLog。请求超时、断流和不确定的撤单结果会进入待对账，不自动重发订单。
- CCXT 订单预检会读取合约账户的持仓模式、保证金模式和 long/short 杠杆；策略声明与实际账户设置不符时拒单。Hedge 订单还校验 position side、买卖方向与 reduce-only 意图的一致性。
- 订单数量与价格在当前固定精度范围内执行步长/tick 精确校验；格式化会改变数值时拒绝。手续费保留原币种，成交流按订单归属过滤，避免把其他订单的手续费计入当前订单。

这些实现证明了可继续验收的工程骨架，不等于三家交易所所有账户类型、产品和市场规则都已经覆盖。

### 本轮余额与组合估值修正

- `AccountBalance` 增加可选的净钱包现金观察值。CCXT `free/used/total` 仍用于可用资金和保证金观察，但不能再不加区分地当作现金：Binance 合约从钱包余额字段取值，OKX 使用 `cashBal` 并扣除报告的借贷负债，Bitget Classic 从 `accountEquity` 剥离 `unrealizedPL`，UTA 使用单币 `balance`，不把含浮盈的 `equity` 计成现金。Bitget Classic 若只给权益而没有可剥离的浮盈就拒绝发布该轮余额，避免将权益误报成现金。
- 交易所持仓的 `settlement_currency` 从 CCXT 市场规格进入持久化观察和账户快照。`GET /account/portfolio` 按币种累加净钱包现金与未实现盈亏，并给出 `equity_by_currency_raw`；缺少持仓盈亏/结算币种或无法判断现货仓位是否已计入余额时，整组净值留空。按 reporting currency 折算仍依赖不超过 30 秒的新鲜现货报价。
- 可选字段缺席时不改变旧事件/旧账户快照的稳定 JSON 与哈希语义；增量字段有值时才进入事件/快照摘要。

### 仍会阻断“全支持”的具体缺口

| 优先级 | 发现 | 影响 |
|---|---|---|
| P1 | `Price`、`Quantity`、`Money` 共用 `SCALE=1e9`，有效小数最多 9 位 | 任何非零的第 10 位小数会被拒绝；不能承诺覆盖所有低价币的 tick、lot、余额和手续费精度。直接提高全局 scale 会改变所有持久化 raw 值、事件哈希和协议语义，必须单独做版本迁移。 |
| P1 | 多账户组执行不共享抵押品/购买力，也不具跨所原子性 | 已有按账户/交易所路由的多腿提交屏障和可恢复部分成功补偿；但没有跨账户资金预留、统一风险限额、原子提交或跨所事务回滚，不能将账户净值合并成联合购买力。 |
| P1 | Live 风控使用冻结 `instrument_spec_path`；CCXT 与 Binance Spot 下单前都比对各自当前暴露的核心元数据，但仍无自动生成、完整过滤器覆盖或冻结规格自动更新策略 | 可检测到可见的产品、币种、乘数、tick/lot 等漂移并拒单；交易所未暴露/适配器未规范化的字段、条件单限制、账户等级与动态限制仍可能漏检，冻结规格也没有自动失效/刷新机制。 |
| P1 | `TradingInstrumentSpec` 暴露 `contract_size`，但统一 `Order.qty` 没有领域级数量单位标签 | Rust→CCXT Worker 协议现显式传 `base`/`contracts` 并校验市场种类，拦截适配器边界上的单位错配；`Quantity` 与策略/配置字段自身仍不带单位，使用 base 数量构造合约张数时可能造成数量级错误。原生 Binance Spot 也有单独下单实现。 |
| P1 | 交易规则校验不完整 | CCXT 会拒绝 inactive 市场并检查可见的价格/数量/名义额上下限；Binance Spot 已检查 raw `PRICE_FILTER`、`MARKET_LOT_SIZE`、`PERCENT_PRICE(_BY_SIDE)` 和市价 `MIN_NOTIONAL`/`NOTIONAL`，reference price 缺失时只接受窗口精确匹配的 avgPrice 回退。合约开仓新增同标的仓位、普通未结单和候选订单的杠杆阶梯检查，但该读取不是资金预留；Bitget Hedge 以较大方向（含同方向普通开仓挂单）计算，其他 venue/One-way 仍保守累计 gross；OKX 公开 tiers 可能不反映特殊账户有效档位，遗漏的条件订单与读写竞态仍可能使远端拒单。账户等级限额及 Binance/OKX/Bitget 全产品过滤器矩阵仍未覆盖。Bitget 反向合约 tiers 按文档“价值”边界和 CCXT `contractSize` 做面值预检，尚无供应商验收。 |
| P1 | CCXT 交易参数映射随依赖版本变化 | 已将 REST 交易适配固定并运行时锁定到 4.5.89；升级版本必须验证 Binance、OKX、Bitget Classic/UTA 的离线请求形状。CCXT Pro 仍是独立可选实时流能力，不参与 REST 下单路径。 |
| P1 | 委托契约只支持 Market/Limit | 无条件单、止损止盈、OCO、追踪止损、改单/替换和最大滑点保护；市价单没有硬价格保护。 |
| P2 | Bitget Spot 市价买被明确拒绝 | 因现有订单数量按 base 资产表达，而 Bitget 现货市价买以 quote 金额表达；需新增显式 quote-budget 订单意图后才能安全开放。 |
| P2 | Spot Margin、Options 没有完整执行/清算语义 | 借币、还款、利息和强平，以及期权身份、行权和结算都不应被 `CCXT 能加载市场` 推断为已支持。 |
| P2 | 当前本地快捷脚本是只读私有连接探针 | 它能检查 sandbox key、余额/持仓读取和账户模式；不会证明真实委托、成交手续费、双向开平仓、撤单竞态、部分成交恢复或供应商限额。 |

交易所接口文档与代码边界一致：Binance Futures Hedge 需要 positionSide，且 Hedge 模式不接受 reduceOnly；OKX 要在每笔合约单指定 `tdMode`，并按场景提供 `posSide`；Bitget Hedge 平仓使用 side + `tradeSide=close`，reduce-only 主要适用于 One-way。Bitget UTA 与 Classic 也不能当作同一账户模式。CCXT 明确区分 precision 与 limits，并说明合约 amount 是 contracts，需结合 `contractSize` 换算。上述映射在当前依赖本机源码中已核对，供应商端仍未验收。

### 发布判断与后续门槛

当前可以称为“部分实盘执行骨架已实现，等待受控 sandbox 验收”，不能称为“Binance/OKX/Bitget 的数字货币实盘链路全部实现”或“多账户联合交易已支持”。发布前至少要完成：

1. 先定义并迁移超过 9 位的精度表示；同时把 base 数量、合约张数、结算币种等单位显式写入产品/订单契约。
2. 固定经 request-shape 验证的 CCXT 版本范围；离线覆盖 Binance Spot/USDⓈ-M、OKX Spot/Swap、Bitget Classic/UTA 的账户模式和开平仓组合。
3. 补齐各交易所的价格、数量、名义额和状态过滤器，并对失效/暂停市场本地拒单。
4. 多腿部分成功补偿已实现，但“联合账户资金池/跨账户购买力”仍需独立实现并验收；只读 portfolio 不能作为替代，多个交易所也不提供原子事务保证。
5. 先运行 `docs/crypto-live-readonly-smoke.md` 的本地环境变量只读探针，再由操作者明确选择 sandbox 订单验收。开发环境不得放生产 Key，也不得把只读探针报告成订单链路通过。

## 本轮找到并处理的问题

复核发现 Bitget UTA v3 与 Classic v2 的订单字段不同：此前统一携带 Classic 的 `marginMode` 会污染 UTA 请求。现在 worker 回传并校验账户模式，OKX 与 Bitget Classic 继续传显式保证金模式；Bitget UTA 只校验已读取的保证金设置，不发送 Classic 专属字段。该差异有锁定 CCXT 4.5.89 的本地请求构造器测试覆盖；尚未通过 sandbox order 请求验证。

同时发现逐笔成交查询把统一参数名 `order` 直接传给三家交易所。锁定的 CCXT 4.5.89 中，Binance/Bitget 过滤键是 `orderId`、OKX 是 `ordId`，原字段可能被忽略或被交易所拒绝，进而影响手续费归属和重启后的成本恢复。现在 Rust worker 发送独立 `order_id` 意图，Python connector 按交易所映射查询参数、请求分页，并再次校验每条成交的归属订单；缺订单号的响应 fail-closed。离线请求契约已覆盖参数构造，但仍需后续 sandbox 成交历史验收。

锁定的 CCXT 4.5.89 本地请求构造器用例验证了 Binance USDⓈ-M 的 `positionSide` 与 hedge 平仓字段、OKX Swap 的 `posSide`、Bitget Classic 的 `tradeSide=close`，以及 Bitget UTA v3 不携带 Classic `marginMode`。这证明当前依赖版本下的参数映射形状，不证明交易所账户的实际模式、所有产品和供应商受理结果都正确。

## 发布阻断项

### P1：定点数只支持 9 位小数

核心 `SCALE=1e9`。更细的数量、价格、余额或手续费不能无损表达时会 fail-closed，避免错误下单，但这也意味着部分低价币、超细 lot、极小手续费无法交易或准确核算。仅提高 SCALE 会影响历史事件、快照、协议哈希和所有现金/风险计算，不能作为局部改动；应设计带小数位尺度的十进制值及明确的存储/协议版本迁移。

### P1：多账户组合保证金和购买力未合并

`GET /account/portfolio` 按币种汇总净钱包现金与有结算币种标签的交易所持仓浮盈，提供 `equity_by_currency_raw`；请求 reporting currency 且报价新鲜、路线完整时可额外得出指示性 `equity_equivalent_raw`。老快照或缺少 PnL/结算币种时会明确返回不完整状态。单标量 `equity_raw` 仍为空，`margin_netting=false`；账户保证金和交易所风险限额没有跨账户统一净额语义，因此不能作为联合购买力或跨账户风控依据。

### P1：多账户联合交易没有共享购买力和跨所原子保证

订单组能把各腿路由给对应账户/交易所 worker，并在部分成交后按路由恢复补偿；它不能同时预留多账户保证金，也不能让不同交易所的下单成为原子操作。各交易所仍拥有独立保证金、强平和 API 失败边界。组合净值展示不能用作共享购买力或统一账户级风险限额。

### P1：精度和交易规则覆盖不完整

超过 9 位精度会拒绝；CCXT 路径已增加 inactive 市场拒单，并在规格可用时校验价格/数量上下限、名义金额上下限。Binance Spot 额外解析 `PRICE_FILTER`、`MARKET_LOT_SIZE`、`PERCENT_PRICE(_BY_SIDE)` 和 `MIN_NOTIONAL`/`NOTIONAL`；对动态规则按交易所优先级读取 reference price，缺失时只使用窗口相符的均价或 last price。合约非 reduce-only 开仓会读取当前同标的持仓、普通未结单与杠杆档位，按交易所档位单位（合约张数或名义价值）保守预检最大杠杆；异步状态变化和交易所限额裁决仍不能本地保证。OKX/Bitget 仍依赖 CCXT 暴露的 precision/limits，账户级限制和全部产品过滤器尚无完整适配器矩阵。

### P1：订单能力仍是最小执行面

止损/止盈、OCO、条件单、追踪止损、改单/替换、最大滑点限价保护尚未纳入当前 Venue 契约。市价单只有参考行情与名义额校验，没有成交价格上限。Bitget 现货市价买按 quote 预算表达，而牵星订单契约是 base 数量，因此当前明确拒绝该组合。

### P1：CCXT 版本兼容性没有锁定

`python/pyproject.toml` 和客户端运行时现固定到 CCXT 4.5.89；升级版本时仍需通过离线 request-shape 契约覆盖 Binance、OKX、Bitget Classic/UTA 的开仓、平仓、reduce-only、现货买卖和逐仓/全仓。这个版本锁只约束代码映射，不替代沙盒供应商验收。

### P2：非线性产品和账户模式尚未完整纳入

核心类型列有 Option，但行权价、到期、认购/认沽等身份与执行结算链路未建模。Spot Margin 的借币、还款、利息及清算规则也未接入当前实盘下单策略。当前应把产品状态标成 unsupported，而不是从 CCXT 能加载市场推断牵星能安全交易。

### P2：供应商事实仍缺沙盒证据

本地只读脚本可以检查 sandbox/demo 私有读权限、余额、持仓模式、保证金模式和杠杆。它不提交订单；因此订单受理、双向开平仓、最小数量边界、真实手续费回报、部分成交恢复和交易所撤单竞态仍未验收。当前请求明确要求开发阶段不验证实盘，故不能给出“真实交易链路已验收”的结论。

## 建议的收敛顺序

1. 固定 CCXT 版本，并添加离线订单请求映射契约；先完整覆盖 Binance USDⓈ-M、OKX swap、Bitget Classic/UTA 的双向开仓/平仓和 cross/isolated。
2. 把产品数量单位显式放入 instrument spec/订单契约（spot base amount、linear/inverse contract count），并核对市价限额、最小名义额和每个产品的 CCXT `contractSize`。
3. 定义独立于交易所账户的组合风险模型：现金、持仓浮盈、初始/维持保证金、借贷和可用额度均带币种、来源、时间戳及缺失状态；在此基础上才计算组合净值。
4. 若需要多账户联合执行，新增账户级资金预留和跨所 saga/对账，不复用只读 `portfolio_id` 汇总。
5. 评估十进制定点升级及持久化/协议迁移；完成前对超过 9 位精度的市场保持明确拒绝。
6. 仅在代码契约通过后，按用户授权安排 sandbox 订单验收；生产 API key 和真实订单不应用于开发验证。

## 参考资料

- [Binance Spot Filters](https://github.com/binance/binance-spot-api-docs/blob/master/filters.md)：Spot 的 `PRICE_FILTER`、`LOT_SIZE`、`MARKET_LOT_SIZE`、名义额与动态价格过滤器定义；Spot 刻度公式与部分期货/期权文档表述不同。
- [Binance USDⓈ-M New Order](https://developers.binance.com/docs/derivatives/usds-margined-futures/trade/rest-api/New-Order)：hedge mode 必须提供 `positionSide`，且不能发送 `reduceOnly`。
- [Binance 合约过滤器定义](https://developers.binance.com/en/docs/products/derivatives-trading-options/common-definition)：价格 tick、LOT_SIZE 等交易规则。
- [OKX API V5](https://my.okx.com/docs-v5/en/?language=python)：`posSide`、双向持仓 side 组合及衍生品 `lotSz` 为合约张数。
- [Bitget Classic Place Order](https://www.bitget.com/api-doc/classic/contract/trade/Place-Order)：双向仓位需要 side + tradeSide，hedge 下的 reduce-only 语义与 one-way 不同。
- [Bitget Contract Config](https://www.bitget.com/docs/catalog/classic-contract-market/classic-contract-market)：合约数量步长、最小量、价格位数和最大市价单量。
- [CCXT Manual](https://github.com/ccxt/ccxt/wiki/manual)：precision 与 limits 不同；合约数量还受 `contractSize` 单位影响。
- [Binance USDⓈ-M Futures Account Balance V3](https://developers.binance.com/en/docs/catalog/core-trading-derivatives-trading-usd-s-m-futures/api/rest-api/account)：余额响应区分 wallet balance、margin balance 与 available balance。
- [OKX Account Balance](https://my.okx.com/docs-v5/en/#rest-api-account-get-balance)：账户细项区分 `cashBal`、`eq`、`upl` 和负债字段。
- [Bitget Classic Futures Account](https://www.bitget.com/docs/catalog/classic-contract-account/classic-contract-account)：账户权益字段包含未实现盈亏；与可转出金额不是同一口径。
- [Bitget UTA Account Assets](https://www.bitget.com/legacy-docs/uta/account/Get-Account)：UTA 分开返回每币种 balance、equity、available、locked 和 debt。

## 2026-10-24 当前工作树复核记录

- 重新运行 `scripts/crypto-live-offline-check.ps1`：Rust 格式检查、交易相关 Rust 库契约通过；架构门禁 4 片覆盖 225 项且无重复；CCXT Python 用例 55 项通过、1 项跳过；包装契约 6 项通过。
- `cargo check --workspace --all-targets` 通过；`qx-core` 全部 92 项库测试通过，`qx-zhenlu` 全部 45 项库测试通过。新增的高精度系数宽中间运算及 OMS 累计回归用例通过。
- 没有读取凭据、连接供应商、访问账户、改变账户模式或提交/撤销订单。以上仅证明本地编译与离线契约，不能证明交易所接受委托或沙盒订单生命周期正确。
- 最终能力判断不变：当前是可离线验证的受控执行骨架，不是完整实盘能力。发布阻断仍包括 9 位固定精度的账簿/风控边界、未建模的共享账户购买力、条件订单与硬滑点保护、完整产品过滤器、Spot Margin/Options，以及缺失的供应商订单验收。

## 2026-10-24 Binance Spot 动态过滤器补齐

- Binance Spot 的本地订单预检现读取并校验 `PERCENT_PRICE`、`PERCENT_PRICE_BY_SIDE`，以及 `MIN_NOTIONAL`/`NOTIONAL` 的限价/市价最小与最大名义额规则。动态规则优先使用 `/api/v3/referencePrice`；交易对没有 reference price 时才按 filter window 回退到 `/api/v3/avgPrice` 或 last price。avgPrice 响应窗口与 `avgPriceMins` 不一致时拒绝提交。
- 离线 fake transport 回归覆盖买卖方向不同价格带、市价订单最低名义额、reference price 缺失后的精确窗口回退，以及不匹配窗口 fail-closed。Binance Spot 定向用例 27/27、`qx-adapter` 全库 77/77 通过；完整离线脚本也通过，架构 225 项、CCXT Python 55 项（1 项跳过）、包装契约 6 项通过，`cargo check --workspace --all-targets` 通过。无交易所网络请求、账户读取或订单提交。

## 2026-10-24 Bitget 反向合约档位

- Bitget `/api/v2/mix/market/query-position-lever` 文档将 `startUnit/endUnit` 定义为档位价值上下限；CCXT `fetch_market_leverage_tiers` 将其映射到 `minNotional/maxNotional`。Bitget inverse market 的 `contractSize` 用作每张合约面值，因此 `contracts × contractSize` 可用于 quote-notional 档位选择，无需参考价。证据：[Bitget 获取仓位档位梯度](https://www.bitget.com/zh-CN/docs/catalog/classic-contract-position/classic-contract-position)、[CCXT Bitget 4.5.89](https://github.com/ccxt/ccxt/blob/v4.5.89/python/ccxt/bitget.py)。
- 假交易所测试覆盖反向档位面值累计跨档拒单，并确保不调用下单；`python -m unittest discover -s python/tests -p test_ccxt.py -v` 37 项通过。该预检仍不替代交易所实际风控、档位变更和账户特殊配置的供应商验收。

## 2026-10-24 CCXT 精确市场规则进入冻结规格

- CCXT `resolve_market` 现在保留 exact decimal 字符串形式的合约乘数、价格 tick、数量 step、最小数量和最小名义额；Python 不对这些值做浮点运算。Rust 冻结规格解析器会把它们解析为 `ExactTradingRules`，提交前校验把冻结规则随请求发给 Worker，并将当前市场规则与冻结值逐字段比较。即使 1e-9 legacy raw 映射相同，1e-10 与 1e-9 的 step 变化也会被识别为漂移。
- 适用范围：CCXT 的 `TICK_SIZE` 与 `DECIMAL_PLACES` 规则可表示且小数位不超过 18 时生成 sidecar；`SIGNIFICANT_DIGITS` 没有固定 step，sidecar 不会伪造。旧冻结规格仍兼容，只比较旧 raw 字段。Worker 仍按旧 raw 量编码订单，风险预留、成交/现金/持仓/费用账簿也仍以 1e9 为主，因此本项补齐了规则冻结和 fail-closed 漂移检查，**不等于开放 10 位以上实盘下单**。
- 新增离线回归覆盖亚纳米 tick/step 冻结、隐藏于相同 legacy raw 值下的规则漂移、Rust ExactDecimal 到 Worker 的 coefficient/scale 请求格式，以及旧规格兼容。CCXT Python 单测 39/39、Worker 单测 17/17、`qx-app` 精确规格定向用例和 `qx-adapter` 漂移定向用例通过；随后完整 `scripts/crypto-live-offline-check.ps1` 通过：相关 Rust 库测试、架构 225 项、CCXT Python 58 项通过/1 项跳过、打包契约 6 项。Rust 格式检查也通过。首次 Rust 编译遇到本机 MSVC `D8050`，将 Cargo dev debug info 设为 0 后 Rust 测试通过。没有读取凭据、请求交易所或提交订单。

## 2026-10-24 高精度市场规则复核补充

- `TradingInstrumentSpec` 现在可选携带 `ExactTradingRules`（精确合约乘数、价格 tick、数量 step、最小数量和最小名义额），核心层能用原始十进制步长校验下单/成交并保守计算名义风险。旧 1e9 接口仍拒绝不对齐输入，不会把精确值静默截断。
- 这只是精度迁移基础，不是实盘精度发布能力：CCXT 市场解析仍生成 scale=9 raw 规格并另带 exact rules；订单提交入口仍拒绝含 `exact_amounts` 的订单，风险、在途预留、Worker wire 和账户读模型还未贯通。Ledger 精确现货和衍生品 entry 的实现状态已在文末 2026-10-24 补充更新；任何依赖第 10 位及之后数量/价格/费用的实际委托仍必须保持拒绝。此项 P1 继续开放。
- 本轮离线验证：`cargo test --workspace --no-run` 编译全部 workspace 测试目标通过；`scripts/crypto-live-offline-check.ps1` 全部通过（Rust 相关库测试、架构 225 项、CCXT Python 57 项通过/1 项跳过、打包契约 6 项）；新增精确规格序列化与亚纳米步长定向回归通过，15 份包含 `TradingInstrumentSpec` 的 JSON schema 均含 exact rules 定义。开发检查没有访问交易所账户或提交订单。
- 交易所支持结论不变：Binance、OKX、Bitget 的 Market/Limit 与部分 Hedge 映射已有离线请求/归约覆盖，但账户之间没有共享购买力或跨所原子执行；单币种之外的组合净值主要是按币种展示/报价折算，不能用作联合风控；条件单、OCO、跟踪止损、改单和硬滑点保护仍不在完整执行契约中。供应商 sandbox 订单生命周期未验收。

## 2026-10-24 本轮执行链精度补充

- 新增 `ExactFillFee` 及精确现货账簿路径：现金腿按 quote currency 记录，费用保留各自原币种，持仓数量、均价和已实现盈亏按 ExactDecimal 累计；精确 entry 重放后与原账簿一致。没有把 raw 兼容投影作为计算依据。
- 精确现金余额查询现在会合并 exact 与旧 raw 币种，避免账户任一资产进入精确记账后遗漏仍由旧 entry 记录的其他币种。Account Snapshot v2 的 Fill 行也保留精确费用金额、币种和 trade IDs，并把这些字段纳入 state hash；v1 明确拒绝精确费用。
- 定向回归 `exact_spot_fills_keep_sub_nano_cash_fees_position_and_replay` 通过；`cargo check --workspace --all-targets` 通过。直接运行多 crate 测试时，Windows 默认 `%TEMP%` 下 7 个 `qx-core::file_lock` 用例因临时目录创建权限拒绝失败；改用仓库脚本管理的 `target/qx-test-temp` 重跑后，格式、相关 Rust 库测试、架构 225 项、CCXT Python 58 项通过/1 项跳过、包装契约 6 项全部通过。
- **该账簿代码尚未成为可用的实时委托链路**：`qx-execution::ExecutionGateway::validate_order` 对精确订单仍明确拒绝；`qx-risk::OrderRiskContext` 仍只认 raw；CCXT 提交和同步依然按 `*_raw` 字段读写，未产出 `ExactFillAmounts`/`ExactFillFee`；账户组合 API 也尚未暴露上述 exact ledger 状态。因此本轮仍不能声称超过 9 位精度的 Binance/OKX/Bitget 实盘交易已支持。
- 交易所映射对照以锁定 CCXT 4.5.89 和官方文档为边界：Binance hedge 需要 positionSide，且合约 hedge 订单 reduceOnly 受限；OKX 双向模式要求按 long/short 填 posSide、委托必须给 tdMode，现货/合约数量单位不同；Bitget Classic hedge 用 side+tradeSide，Bitget UTA 跨现货/保证金/合约使用 category 与账户模式字段，现货市价买入的 qty 为 quote coin。代码当前有部分映射和离线请求形状测试，但不能据此推断所有订单种类、过滤器和账户路径已验收。参考：[Binance Spot filters](https://github.com/binance/binance-spot-api-docs/blob/master/filters.md)、[Binance USDⓈ-M New Order](https://developers.binance.com/docs/derivatives/usds-margined-futures/trade/rest-api/New-Order)、[OKX API V5](https://my.okx.com/docs-v5/en/?language=python)、[Bitget Order Management](https://www.bitget.com/docs/catalog/trading/order-management)、[Bitget Classic Place Order](https://www.bitget.com/api-doc/classic/contract/trade/Place-Order)。

## 2026-10-24 精确衍生品账簿补充

- `TradingInstrumentSpec::unrealized_pnl_exact` 现按原始十进制数量、价格和合约面值计算线性/反向合约 PnL；正收益向下取整、损失向负无穷取整。
- `Ledger::apply_fill_with_spec` 现支持精确 Perpetual/Future 成交：按多空分仓累计数量与均价，记录精确合约乘数和开仓杠杆；平仓时按合约类型计算已实现 PnL；exact fees 按原币种扣账。逐仓/全仓仍由账户风控与保证金路径承担，Spot Margin 和 Option 精确成交继续 fail-closed。
- 每个精确衍生持仓 entry 将合约乘数和 inverse 属性写入 `ExactLedgerAmounts` 并纳入事件摘要；字段可选，缺失时沿用旧 entry 和旧摘要语义。inverse 加仓使用调和平均成本，避免把固定报价面值反向合约错按线性价格加权。回归覆盖 0.001 base/contract 的 Hedge Long 与 Short 开平仓、子纳米费用，以及 inverse `1@100 + 1@300 => 平均价 150` 后在 200 平仓得到 0.166666666666666666 BTC，并验证现金、持仓、盈亏重放一致。
- 验证（2026-10-24 最终轮）：`cargo test -p qx-core` 150 项通过、1 项忽略；`cargo check --workspace --all-targets` 通过；`scripts/crypto-live-offline-check.ps1` 的 Rust 库测试 689 项通过、架构 225 项通过、CCXT 请求/配置 58 项通过/1 项跳过、Python 打包契约 6 项通过；`cargo fmt --all -- --check` 与 `git diff --check` 通过。本机未连接供应商、未读取 API 凭据、未提交订单。精确衍生账簿尚未接入实时 CCXT 响应和风险预留，因此**这不开放精确合约实盘委托**。

## 2026-10-24 精确订单风控底座

- `TradingInstrumentSpec::initial_margin_exact` 按精确合约名义额和杠杆向上保守舍入。`qx-risk::OrderRiskContext` 增加可选 `ExactOrderRiskFacts`，精确订单可以在提供完整精确持仓、余额、参考价及保证金投影时校验 quantity step、price tick、名义额、账户上限、reduce-only、最大数量/禁止卖空规则；精确 position/margin 与兼容 raw 投影不一致时拒单，未能投影到 1e9 的决策输出保持 `None`，不会伪报成 0。
- `qx-zhenlu::RiskContext` 能承载精确事实并转交统一风控；当前 Live `risk_host` 仍将 `exact` 留空，Live 提交入口和 `ExecutionGateway` 仍要求 legacy amounts，在途订单预留也仍拒绝精确订单。**所以本次只打通了风控计算底座，没有启用精确实盘委托；后续必须补精确账户/在途快照并接通 Worker wire、回报归约和订单账簿。**
- 离线回归覆盖 10 位数量步长、8 位价格 tick、低于 1e-9 的名义额与保证金、账户限额、余额不足、reduce-only/NoShort 持仓，以及精确/兼容持仓不一致时拒绝。`cargo test -p qx-risk` 12/12、`cargo test -p qx-zhenlu --test risk_projection` 6/6、`cargo test -p contract-tests --test risk_parity` 5/5 通过；`cargo check --workspace --all-targets` 已通过。随后完整 `scripts/crypto-live-offline-check.ps1` 通过：Rust 库用例 690 项通过、架构检查 225 项通过、CCXT 离线契约 58 项通过/1 项跳过、打包契约 6 项通过；`cargo fmt --all -- --check` 与 `git diff --check` 通过。未连接交易所或账户，未读取凭据，未提交订单。
- 发布判断仍为**未满足实盘发布条件**：Live 提交端仍要求旧 1e9 金额；精确账户余额/持仓快照、精确在途资金预留、精确 Worker 提交与成交回报归约尚未串通。精确风险底座只证明本地计算和拒单规则，不证明完整实时链路。

## 2026-10-24 精确账户事实与在途敞口补充

- CCXT 余额快照现在将 free/used/debt(or borrowed)/wallet cash 的原始十进制字符串解析为 `ExactDecimal` sidecar，并与 legacy `1e9` raw 投影并存。位置快照同样保留精确合约数量、开仓/标记/强平价、未实现盈亏和初始/维持保证金；兼容字段按向零投影，raw 与 exact 不一致时拒绝快照。余额/持仓 sidecar 已进入账户事件摘要，避免 raw 投影相同但精确事实不同的事件被误判为相同。
- 方向校验前只跳过 raw 与 exact 都为零的真实空仓；raw 因低精度投影为零但 exact 非零的持仓仍进入方向闸门，必须明确 long/short。新增离线回归覆盖亚纳米非零持仓缺 side、余额 raw/exact 冲突、持仓盈亏 raw/exact 冲突。`qx-service` 本轮测试 52/52 通过。
- `qx-risk` 新增跨 legacy/exact 在途订单的精确总敞口计算入口：精确剩余量参与估值，限价与参考价取较高名义额，市价单缺精确参考价时拒绝。该工具当前尚未接入 Live 下单路径；`risk_host` 仍不给 Live 风控传入 exact 事实，精确下单、资金预留与 Worker wire 依然 fail-closed。
- 本轮完整 `scripts/crypto-live-offline-check.ps1` 通过：Rust 相关 crate 测试 695 项通过、架构 225 项通过、CCXT Python 60 项（59 通过/1 跳过）、打包契约 6 项通过；之后针对新增三条反例重新运行 `qx-service` 52/52，`cargo fmt --all -- --check` 与 `cargo check --workspace --all-targets` 通过。架构快门禁 4 片完整覆盖 225 项且无重复。构建使用仓库脚本设置的隔离临时目录及 Cargo debug info=0；未读取 API 凭据、未连接账户/供应商、未提交或撤销订单。
- 发布结论不变：三家交易所的部分行情/委托/账户事实映射和离线参数契约存在，但**数字货币实盘核心链路尚未全部实现**。精确 Live 风控接线、精确在途资金 reservation、CCXT exact 委托/成交费用 wire、跨账户联合购买力与净值、账户模式和三家供应商订单生命周期验收仍是发布阻断。现阶段只可用本地环境变量运行只读沙盒探针；不能据此宣称已经验收真实交易能力。

## 2026-10-24 精确订单适配器副作用闸门

- 在 Binance Spot 和 CCXT Venue 的 `submit` 边界增加 `Order::require_legacy_amounts` 检查。即使调用方绕过 `ExecutionGateway` 直接调用适配器，精确订单也会在 HTTP/RPC 前明确拒绝，不会把兼容 raw 数量/价格零值继续发给交易所。此前只在统一执行网关设置拒绝，直接适配器入口没有同等保护。
- 新增假 HTTP transport 与假 CCXT RPC 回归：两条旁路提交均返回精确金额尚未贯通的业务拒绝，且请求计数为零。`cargo test -p qx-adapter --lib` 79/79、`cargo check --workspace --all-targets`、`cargo fmt --all -- --check` 通过；架构门禁 4 片覆盖 225 项且无重复。没有交易所请求或订单副作用。
- 该修改只加固未完成链路的拒绝边界，不代表精确委托已支持。仍需把 exact order 风控、在途余额预留、Binance/CCXT 数量价格 wire、精确成交/费用归约与重放全程接通并逐端验证后，才能移除此拒绝。

## 2026-10-24 精确 Live 风险事实接线准备

- `worker_risk_context` 开始组装同一风控快照所需的精确事实：从余额 sidecar 读取 free/borrowed，扣除仍以 legacy raw 计算的 Live 在途资金预留；从精确账簿读取净仓、Long、Short 数量；精确计算持仓和在途订单的 gross notional，并保留精确限额。
- 精确余额与仓位投影必须分别与当前 legacy 风控上下文一致；方向不合法、算术溢出或投影不一致时不生成 exact 风控事实。新回归验证了高精度可用余额、微量仓位与在途敞口的合并，以及 raw/exact 仓位冲突时 fail-closed。
- 该 builder **仍不构成可运行的精确下单链路**：Live 入口在 `execute_submit_order_with_worker_risk` 前置拒绝精确订单，因此生产提交不会走到这段 builder；`ExecutionGateway::validate_order`、Binance/CCXT adapter 和 legacy lifecycle 也继续拒绝。此项目前是后续贯通所需的风险上下文底座，不能移除现有闸门，也不能宣称 9 位以上精度已可交易。
- 本轮验证：`cargo test -p qx-service --lib` 54/54、完整 `scripts/crypto-live-offline-check.ps1` 通过（架构 4 片共 225 项、CCXT 离线契约 60 项/1 项跳过、Python 打包契约 6 项）、`cargo check --workspace --all-targets` 与 `cargo fmt --all -- --check` 通过。没有读取密钥、连接供应商、读取账户或提交订单。

## 2026-10-24 CCXT 精确订单边界推进

- Rust→CCXT 下单 JSON 现在可携带规范化 `coefficient + scale` 数量/价格字段，旧 `amount_raw`/`price_raw` 对精确订单明确置空，避免 Worker 把两个精度口径混用。Worker 精确小数解码只接受规范字符串或规范 coefficient/scale 对象；精确 Spot 限价格式化使用十进制字符串传给 CCXT，并按 exact tick/lot/minimum quantity/minimum notional 校验。依赖 1e-9 raw 兼容投影的字段不作为精确数量或价格来源。
- CCXT 订单/逐笔成交归一化现在另外输出精确 amount、filled、remaining、price、average、cost 和按币种汇总的 exact fees。legacy raw 字段仅作向零投影；本地回归覆盖 1e-10 PEPE 数量/价格及 1e-18 手续费的请求和回报保留。
- **该项没有开放精确实盘委托。**`CcxtVenue::submit`、统一执行网关和 Live 服务入口仍拒绝精确订单；本轮另已让 Rust `sync_order` 在 exact order 分支消费 exact 累计数量/逐笔成交/费用，并输出精确 `Fill`。精确合约订单在 Worker 侧继续拒绝，因为精确数量的持仓/挂单杠杆档位预检尚无实现；Bitget Spot 市价买仍需 quote-budget 意图。下一步仍须贯通 exact 资金预留、exact 账户快照、reconcile snapshot 与订单生命周期后，才能考虑移除任何提交拒绝。
- 本轮离线验证：Python CCXT 请求/Worker 用例 63 项（62 通过、1 跳过）、`qx-adapter` 库 79/79、完整 `scripts/crypto-live-offline-check.ps1` 通过（架构 225 项、包装契约 6/6）、`cargo check --workspace --all-targets` 及 Rust 格式检查通过。全部使用本地 fake/离线边界，未读取密钥、连接账户或提交订单。

## 2026-10-24 精确回报归约与最终阻断复核

- 修复精确回报接缝：成交规格检查之前只调用 `validate_fill(raw_qty, raw_price)`，对小于 1e-9 的精确成交会把数量/价格当作零并转对账。现按 `ExactFillAmounts` 直接校验原始小数的 step/tick；旧订单仍走原 raw 检查。提交同步回包与用户流/对账归约共用该分支。
- 修复成交幂等键：原执行层和 Runtime 去重键都只纳入 raw 数量/价格/费用。多笔 sub-nano 成交的 raw 投影全为零时，同一订单同一时间戳的不同成交可能被吞成重放。现在精确身份后缀包含 exact quantity、price、费用币种/数值与 trade IDs；旧精度事件键保持兼容。回归覆盖相同时间戳、同一 raw 投影、不同成交 ID 的两笔 PEPE 精确成交，确认两笔均入账并在 EventLog 重启后保留。
- CCXT 费用恢复现可从持久 Filled 事件重建按币种精确的累计费用与 trade ID；Rust CCXT exact sync 按远端精确订单量读取订单逐笔成交，核对累计数量、持久已见成交和按币种费用基线，逐笔输出带精确数量/价格/费用/trade ID 的 Fill。缺 trade history、金额/费用不一致、手续费在已记成交上追溯变化时转人工对账，不把 raw 投影作为成交依据。fake RPC 回归覆盖两轮部分成交与完成同步、1e-10 数量和 1e-18 费用。`cost_exact`/`average_exact` 不用于反推各笔成交价，成交价格取逐笔 trade 精确价格。
- 本轮深查再次确认：`Order::require_legacy_amounts` 仍在 Live 风控/执行入口、`ExecutionGateway`、Binance Spot 和 CCXT 下单边界拒绝 exact order；`VenueOrderSnapshot` 和重连 reconcile 的 filled 仍只有 raw `Quantity`；账户 `AccountBalance::net_cash_exact` 虽有精确 sidecar，账户快照与 `/account/balances`/`/account/portfolio` 目前聚合的是 raw 1e-9 现金，组合字段里的 `*_raw_exact` 是 raw 整数的字符串而不是 exact decimal 余额。故现货小币种精度不能只以 Ledger 或 Python CCXT formatter 已支持来判定端到端可用。
- 多账户执行已有账户隔离账簿、跨账户/跨 venue 多腿补偿状态机；没有共享保证金/可用购买力、原子组合提交，也没有对各账户余额时点与 FX/保证金来源一致性的联合风险闸门。因此“编排多账户多策略多标的”与“共享资金池的联合交易账户”是不同能力，后者未实现。
- 最新本地证据：exact fee 恢复、逐笔 exact sync 和 sub-nano 幂等用例通过；`scripts/crypto-live-offline-check.ps1` 在 exact sync 合并后完整通过：11 个 Rust crate 库测试共 706 项、格式检查、4 片 225 个架构断言、CCXT Python 63 项（62 通过、1 跳过）和包装契约 6/6。随后 `cargo check --workspace --all-targets` 与 `git diff --check` 通过。所有验证继续使用本地 fake/离线边界，没有触碰供应商账户或发送订单。
- **结论不变：数字货币实盘交易核心逻辑尚未全部实现，当前不可宣称支持小币种高精度实盘下单、精确账户/组合估值或跨账户共享购买力。** Binance/OKX/Bitget 的 Market/Limit 路径、部分合约 Hedge 参数、账户快照与对账骨架有离线契约覆盖，但远端同步、精确快照/预留、完整生命周期和真实供应商验收仍阻断发布。当前没有访问凭据/账户、连接供应商或产生订单副作用。

## 2026-10-24 CCXT 精确现货与合约订单路径接通

- 前述审计记录描述当时的阻断状态。此后已移除 CCXT 的 exact-order 总闸门：ExactDecimal 数量/限价经统一风控、冻结精确规格与步长校验、精确在途预留、JSONL 文本 wire、逐笔成交/多币种费用恢复、EventLog/账簿与 v3 快照；Paper 与原生 Binance Spot adapter 仍按能力声明拒绝 exact order。
- 本轮将合约杠杆档位预检改为 `Fraction` 精确十进制核算：候选订单、持仓、普通未结单、合约面值、档位上下界均不再投影到 1e-9 后判定；Binance/OKX/Bitget 各自沿用配置好的市场/账户模式及杠杆核验，Bitget Hedge 按方向最大敞口匹配档位。档位/仓位/方向/参考价不完整时仍拒绝开仓。
- 账户组合只读接口对精确现金和结算币未实现盈亏按币种聚合，提供十进制字符串字段和可选报告币折算。多个账户仍各自持有保证金、买卖能力与强平边界；没有共享资金池、跨账户净保证金或交易所原子组合下单。
- 余留边界：原生 Binance Spot 与 Paper 订单链不接收超过 9 位的金额；当时精确市价单参考价受 1e-9 行情投影限制（见文末 2026-10-24 后续修复）；Bitget Spot 市价买单需要 quote-budget 订单意图；策略目标/部分控制输入仍以旧 raw 口径表达。CCXT 外部实盘链路没有用真实供应商凭据验收。
- 验收证据：针对 exact derivative worker 的 sub-nano 下单/档位预检、精确 Spot、现有合约开仓档位和 Bitget Hedge 档位用例通过；整套离线脚本最终结果见本轮后续记录。全程未使用 API key、未连接交易所、未读账户、未下单。

## 2026-10-24 最终离线验收记录

上一节列出的 exact CCXT 路径与限制，经本轮完整离线脚本再次验收。`scripts/crypto-live-offline-check.ps1` 通过 Rust 格式检查、脚本选定的 Rust 交易合同测试、全量四片架构门禁（225 个具名检查）、CCXT Python 请求/配置/Worker 测试（63 项，62 通过、1 跳过）及 Python 包装合同（6/6）。架构检查最初发现一条静态断言仍按旧函数位置和单行格式匹配；已修复断言以检查实际 `pending_order_reserve_exact` 精确资金币种隔离与 policy 校验，相关分片 333 项全通过，随后完整脚本全通过。

无需账户凭据的快捷复现：

```powershell
Set-Location D:\aex_work\qianxing
.\scripts\crypto-live-offline-check.ps1
```

需要验证本机 sandbox 私有 API 环境时，先在当前 PowerShell 进程设置 `QX_CCXT_*` 环境变量，再按交易所选择 `python/examples/ccxt-*.example.json` 并运行 `python -m qianxing_ccxt.private_probe --config <配置路径>`；此探针只读余额与合约模式，不下单或修改账户设置。更完整步骤见 [`crypto-live-readonly-smoke.md`](crypto-live-readonly-smoke.md)。本轮没有加载真实密钥，没有连接账户，也没有提交或撤销订单。

此记录更新上文较早的阻断结论：CCXT Binance/OKX/Bitget 的精确订单、合约档位预检、精确费用/成交恢复及多币种组合只读字段现已接通并通过本地假 worker/离线合同。**这不等于完整实盘发布验收**：sandbox 实际订单生命周期尚未验证；原生 Binance Spot 与 Paper 的高精度订单、Bitget Spot quote-budget 市价买单、策略及控制端高精度输入、跨账户资金池与保证金净额仍是明确边界。多账户以账户隔离方式协同，不共享 collateral；任何缺失市场规格、账户模式、杠杆档位、FX 路径或可证明风控依据的路径均应拒绝或进入对账，不得降精度放行。

## 2026-10-24 Python/外部策略精确订单意图贯通

策略意图此前只有 `qty_raw`/`limit_price_raw`，使 Python/外部语言策略无法把小于 `1e-9` 的数量和限价送到已支持精确金额的 CCXT 执行链。本轮扩展 Strategy API v1：raw 字段保持兼容；精确订单以 `qty_raw: 0` 加 `qty_exact` 十进制字符串表达，可选精确限价使用 `limit_price_exact`。Rust、JSON Schema 与 Python SDK 字段集合保持同步；跨语言校验拒绝 raw/exact 混用、无效/非正值和超出 18 位支持尺度的值。

`qx-app` 按冻结 `ExactTradingRules` 校验精确步长/刻度后构造 `Order.exact_amounts`；CLI 对 exact intent 不使用缺少权威实盘余额/持仓的早期 raw 风控快照，订单继续进入提交前统一账户级 exact 风控与 worker 能力门。Bar 回测在收到 exact intent 时明确拒绝，避免进入只消费 raw Quantity 的撮合器。

验证证据：Python `test_strategy_contract.py` 15/15（包含可运行小币种示例）；Rust `qx-app` 精确策略意图用例 2/2；`qx-runtime` 策略契约用例 6/6、Schema 合同 4/4；`qx-cli` exact intent 构造回归 1/1；Rust 格式检查与策略三侧 schema/字段架构判据通过。随后完整 `scripts/crypto-live-offline-check.ps1` 复跑通过：Rust crypto live 合同、4 片架构 225 项、CCXT Python 63 项（62 通过、1 跳过）及 Python 包装合同 6/6。未连接账户、未读环境凭据、未下单。未完成项收窄为：target_qty 调仓、内置/C ABI 策略数量与部分控制输入仍使用 raw 刻度；sandbox 实际订单 lifecycle 尚未验收。精确市价参考价的后续接通记录见本节下一节。

## 2026-10-24 亚纳米市价参考价进入订单风险事实

- CCXT ticker 的 bid/ask 原文以规范十进制字符串输出，旧 1e-9 字段仅作兼容投影。精确市价单要求报价为正、双边顺序有效、交易所时间戳不晚于本地且不旧于 30 秒；用 ask 作为保守风险参考价。缺失/超尺度精确报价或过期行情 fail-closed。
- 精确参考价随精确订单事实持久化，但 worker 订单 wire 仍只发送真实 `limit_price`；risk-only 参考价不会被误当成限价。合约档位预检、Live 精确风险和后续未成交订单的精确名义额/资金预留均复用精确值。
- 回归：Python 小数 ticker 测试、Rust 市价订单参考价序列化、qx-risk 无旧 mark 的亚纳米在途名义额、qx-service 30 秒新鲜度测试通过。随后完整 `scripts/crypto-live-offline-check.ps1` 通过（架构 225 项、CCXT Python 64 项/1 项跳过、包装合同 6/6）；`cargo check --workspace --all-targets -j 1`、Rust 格式及 `git diff --check` 通过。未连接交易所账户或提交订单。
- 仍未完成的代码能力边界：Python/外部策略以外的 target_qty、内置/C ABI 数量和部分控制输入仍是 legacy raw；Bitget Spot 市价买需要 quote-budget 意图；原生 Binance Spot 与 Paper 不支持超 9 位金额；多账户可隔离执行与按币种聚合，但没有共享保证金/购买力池。真实沙盒订单生命周期仍按当前要求不做验证。

## 2026-10-24 Bitget Spot quote-budget 后续实现

本节更新上方较早的 Bitget Spot 阻断结论：`quote_budget_exact` 已从 Python/Rust 策略意图贯通至 ExactOrder、账户风控、quote 币余额和在途预留、Bitget Classic/UTA 市价单请求、精确成交成本、预算超额拒绝及多币种账簿。Bitget UTA 的 `qty` 明确按 quote 币预算构造，避免复用其按 base 数量精度格式化的通用 CCXT builder；Bitget 官方 UTA Place Order 文档说明市价买 `qty` 单位为 quote coin。新增 UTA Spot 只读配置和 `strategy_bitget_quote_budget.py` 示例。

离线验收：`scripts/crypto-live-offline-check.ps1` 全部通过，包含 225 项架构检查、Rust 离线交易/账簿/策略检查、CCXT Python 68 项（67 通过、1 项因本机 CCXT 4.5.78 与项目支持锁定 4.5.89 不同而跳过）、Strategy API 17 项和打包契约 6 项；`cargo check --workspace --all-targets -j 1` 通过。未加载密钥、连接账户或发单。用户可先运行该脚本，再按 [`crypto-live-readonly-smoke.md`](crypto-live-readonly-smoke.md) 设置本地 Bitget Demo 环境变量执行只读私有探针；探针不代表订单验收。

实现该 sizing 缺口后，仍不能宣称完整生产发布：sandbox 真实下单生命周期尚未验证；多账户按账户分离资金和强平边界，不提供共享保证金或跨所原子组合下单；Binance 原生 Spot 与 Paper 仍拒绝超 9 位金额；策略 `target_qty`、内置/C ABI 和部分控制输入仍用 legacy raw；条件单、改单和硬滑点保护不在当前完整执行契约内。

## 2026-10-24 实盘核心目标复核更新

本节更新上文较早阶段的阻断清单，依据当前工作区代码和最新离线验收结果：

- Binance、OKX、Bitget 的 CCXT Spot/Perpetual/期货配置走同一 Rust→JSONL Worker→CCXT 执行链；原生 Binance Spot 也有独立适配器。Execution Worker 可用按完整 `InstrumentId` 键控的 market-spec set 覆盖多标的。多账户通过每个 `(account_id, venue_id)` 独立 EventLog、余额/持仓快照和 worker 路由运行；跨账户多腿有持久补偿流程，不共享保证金池或购买力。
- CCXT 三家和原生 Binance Spot 的精确订单数量、限价、成交、逐币种手续费、恢复和风险预留已接入支持路径；精确 Spot/线性/反向合约账簿和费用币种分录可重放。Python/JSON Strategy API 也支持精确 intents 与单向 `target_qty_exact`；Hedge 策略需用分开的 long/short intents。小于 `1e-9` 的行情参考、币种余额/仓位与 FX 换算有精确十进制通路。
- 合约订单会读取当前 position mode、margin mode 和 long/short leverage；按 Binance/OKX/Bitget 参数约定构造 Hedge/One-way 与 reduce-only 请求，并对持仓、普通未结委托、合约乘数及杠杆档位做开仓前检查。运行时不在下单时更改账户级模式/杠杆，必须先在 Sandbox 账户配置并经只读探针核对。
- 组合只读视图按币种保留 cash、PnL、equity，可用新鲜、无歧义的现货 FX 路径折算报告币；它不跨账户净额保证金。Bitget Spot market buy 由 quote-budget 意图承载，Binance Spot market buy 使用 `quoteOrderQty`，OKX Spot market buy 指定 `tgtCcy=base_ccy`。有符号、多币种 CCXT 手续费及费用晚到调整会进入 EventLog 和恢复流程。
- 当前明确能力边界：Market/Limit 为执行类型；条件单、OCO、追踪止损、改单/替换和硬滑点保护不在本目标实现内。Paper 撮合、内置/C ABI 策略及部分 legacy 控制入口仍使用 1e-9 raw 数量刻度。杠杆档位与市场过滤器检查是提交前估算，不是对交易所余额的并发保留；未知账户配置/过滤器或竞态由 fail-closed 和交易所最终校验兜底。Sandbox 私有 API/订单生命周期没有在开发阶段运行。
- 本机复验 `scripts/crypto-live-offline-check.ps1` 通过：Rust 格式、选定适配器/服务/风控/账簿/策略合同、架构 225 个具名检查、CCXT Python 72 项（71 通过、1 项因本机 CCXT 版本 4.5.78 与锁定版 4.5.89 不同跳过）、Strategy API 19/19、打包合同 6/6；`cargo check --workspace --all-targets -j 1`、精确目标 CLI 6/6、Runtime strategy contract 7/7、`git diff --check` 通过。没有读取 API 密钥、访问私有账户、发送或撤销订单。
- 本机无密钥起步命令仍为 `scripts/crypto-live-offline-check.ps1`。环境变量、Sandbox 只读探针、多账户 portfolio 和多标的精确 MarketSpec set 的配置方法集中记录在 [`crypto-live-readonly-smoke.md`](crypto-live-readonly-smoke.md)。`runtime-check` 对 `qianxing.runtime.ccxt.example.json` 与 `qianxing.runtime.multi-venue-arbitrage.example.json` 均返回 `ok=true`、`network_accessed=false`、`orders_sent=false`；只读探针不作为订单验收证据。
