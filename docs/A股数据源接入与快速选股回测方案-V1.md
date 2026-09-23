# 牵星 A 股数据源接入与快速选股回测方案 V1

更新时间：2026-09-14

## 1. 结论

A 股数据可以快速接入现有框架，但必须把“数据源接入”和“交易规则仿真”分开治理：

```text
AkShare / Baostock / easy_tdx
        ↓
统一字段、交易所代码、时间、复权、质量校验
        ↓
BarFrame + AshareManifest（可审计快照）
        ↓
快速选股 / 候选池
        ↓
fast-backtest 并行回测
        ↓
A 股专用撮合规则（T+1、涨跌停、100 股一手、费用、停牌）
```

本版本已经落地数据接入、快速筛选和 A 股规则化 Bar 回测；真实券商柜台仍必须经过具体券商协议和模拟盘验收，不能把公共行情源当成交易接口。

## 2. 本次已实现

新增 Python 包 `qianxing_ashare`：

- `AkShareProvider`：日线使用 `stock_zh_a_hist`，分钟线在当前依赖提供接口时使用 `stock_zh_a_minute`。
- `BaoStockProvider`：支持日、周、月和 5/15/30/60 分钟查询，并正确处理登录、查询错误和退出。
- `EasyTdxProvider`：对 `UnifiedTdxClient`/`TdxClient` 做薄适配，支持注入已有 client，便于复用连接和离线测试。
- 统一代码：`sh.600000`、`600000.SSE`、`600000` 等输入归一化为 `600000.SSE`；深市和北交所分别归一化为 `SZSE`、`BJSE`。
- 统一字段：中文/英文 DataFrame 或 row mapping 都转换为定点整数 `BarFrame`，价格和成交量使用 `1e9` scale。
- 统一质量规则：时间排序、重复时间戳保留最后一条、时间范围过滤、空值/非有限值/负成交量拒绝。
- `AshareManifest`：保存 provider、版本、标的、频率、复权、日期范围、行数、BarFrame digest 和接收时间。
- `fetch_corporate_actions`：统一获取 AkShare 分红/送转、Baostock `query_dividend_data` 和 easy_tdx XDXR，输出 `AshareCorporateAction` 与独立 manifest；保留原始字段，支持 `as_of` PIT 过滤。
- `reconcile_corporate_actions`：按标的、除权日和事件类型合并多源事件；不覆盖冲突，而是返回可审计的 conflict 列表，研究和回测必须在冲突清零后继续。
- `screen_bar_frames` 和命令行 `screen`：按区间收益、最大回撤和平均成交量做第一轮候选筛选，结果可用于生成 `fast-backtest jobs`。
- Rust 回测规则：交易日/停牌时间戳、T+1 可卖仓位、买入整手、卖出零股、涨跌停封板方向阻断和 A 股费用模型。
- A 股规则快照通过 runtime 的 `ashare_rules_path` 绑定到回测，规则参数、日历数量和停牌数量进入结果模型指纹。
- 所有数据源依赖都是 optional extra；没有安装 A 股依赖时，不影响现有 CCXT 和 Rust 离线链路。

安装方式：

```powershell
pip install -e python
pip install -e "python[a-share-akshare]"     # 通用首选
pip install -e "python[a-share-baostock]"    # 日线/分钟线备用
pip install -e "python[a-share-easy-tdx]"    # 通达信在线或本地数据
```

示例：

```powershell
python -m qianxing_ashare fetch `
  --provider akshare --code 000001 --start 20240101 --end 20241231 `
  --frequency daily --adjustment qfq `
  --output data/ashare/000001.SZSE.json

python -m qianxing_ashare actions `
  --provider akshare --code 000001 --start 20200101 --end 20241231 `
  --as-of 2024-06-01T00:00:00+08:00 `
  --output data/ashare/000001.SZSE.actions.json

python -m qianxing_ashare screen `
  --bars-dir data/ashare --min-return-bps 500 --limit 50 `
  --output data/ashare/screen.json

python -m qianxing_ashare screen `
  --bars-dir data/ashare --min-return-bps 500 --limit 50 `
  --output data/ashare/screen.json `
  --backtest-manifest data/ashare/fast-backtest.json `
  --runtime deploy/qianxing.runtime.ashare.example.json `
  --market-spec deploy/qianxing.ashare.spot.spec.json
```

生成的 `BarFrame` 可直接作为现有 `strategy backtest`（或 `backtest builtin`）与 `fast-backtest` 的 `bars` 输入；回测前必须另行提供产品规格和 A 股规则配置。

## 3. 数据源选择

| 数据源 | 默认定位 | 优点 | 风险与边界 |
|---|---|---|---|
| AkShare | 首选研究数据源 | 接口覆盖广、字段接近 pandas、适合快速选股 | 上游接口和限流会变化，必须冻结快照，不能把在线响应当作回测数据 |
| Baostock | 稳定备用源 | 有交易日历、日/周/月和分钟接口，适合批量历史数据 | 字段和复权标记有自己的约定，必须经过统一层，不能直接喂回测 |
| easy_tdx | 实时/本地 TDX 补充源 | 可走通达信协议或本地文件，适合行情补充 | 服务器、字段和复权口径依赖客户端环境；本项目只做薄适配，不复制其回测引擎 |

`auto` 只按“依赖是否安装”选择 AkShare → Baostock → easy_tdx，不会在请求失败后静默切换数据源。切换数据源会改变复权、缺失值和交易日语义，必须重新生成 manifest 并重新回测。

## 4. 必须补齐的 A 股交易规则

这些规则不能由通用 CCXT 产品规格代替：

1. T+1：当日买入股票当日不可卖出；当日卖出资金可用性和可取性分离。
2. 整手和零股：买入通常按 100 股整数倍，卖出允许处理不足一手的零股；不同板块、退市整理期和 ETF 需要独立规则。
3. 涨跌停：主板、创业板、科创板、北交所、ST 和新股的涨跌幅限制不同；封板时不能假设必然成交。
4. 停牌和无成交：停牌日不能成交，不能用前收盘价伪造成交量；复牌首日要保留规则标签。
5. 费用：佣金最低收费、印花税、过户费和卖出方向差异必须按费用版本冻结。
6. 复权和公司行为：前复权适合研究展示，交易仿真必须保留原始价格、除权除息事件和现金/股份变动。
7. 交易日历和时段：集合竞价、连续竞价、午休、收盘竞价要进入撮合时钟；不能用 UTC 日界线代替交易日。
8. 生存者偏差和 PIT：股票池、停牌、退市、行业和财务数据都要带 `as_of`，选股不能看到公告日之后才发布的数据。

## 5. 重构后的实现边界

### Data Plane

- `qianxing_ashare` 负责外部数据源、字段映射、规范化和快照。
- BarFrame 只保存可回测的标准 K 线；原始响应、请求参数和 provider 版本保存到数据血缘目录。
- 后续增加 `CorporateActionFrame`、`TradingCalendarFrame`、`SuspensionFrame`、`LimitRuleFrame`，不要把这些信息塞进 OHLCV 的隐含值。

### Research Plane

- `screen` 只做候选池初筛，不产生交易订单。
- 因子计算必须使用 `as_of` 和 point-in-time 数据；每次选股输出 candidate manifest、参数、输入摘要和结果摘要。
- 候选池输出标的列表后，由 `fast-backtest manifest.json` 并行运行策略、费率和规则版本组合。
- 公司行为必须先按来源、公告时间和生效时间完成 PIT 过滤，再转成 Rust 规则快照；不能把前复权 K 线当作交易成交价。

### Simulation Plane

- Rust `BacktestEngine` 继续作为唯一撮合、Ledger、风险和报告归约入口。
- `AshareRuleConfig` 已组合交易日/停牌、T+1、整手、涨跌停和费用规则；后续数据服务只需要生成规则快照，不需要改撮合器。
- 撮合前顺序固定为：交易时段 → 停牌/涨跌停 → T+1 可卖仓 → 整手/零股 → 费用 → Fill/Ledger。
- 每个回测报告必须绑定 A 股规则版本、市场规格、数据 manifest、策略版本和随机种子。

### Live/Paper Plane

- 本期 A 股先支持数据研究和纸面回测，不宣称已经完成券商实盘交易。
- 后续接入券商/柜台时，必须沿用现有 OMS、RiskGate、EventLog、Reconcile，不允许数据源模块直接下单。
- 密钥配置只进入凭据管理边界；AkShare/Baostock/easy_tdx 公共数据不等于券商交易授权。

## 6. 交付路线

### P0：当前版本

- 三个可选数据源统一成 BarFrame。
- 数据清洗、digest、manifest、离线单测。
- 快速初筛和现有并行回测入口打通。

### P1：A 股规则仿真（核心已落地）

- 交易日/停牌时间戳、T+1、100 股一手、停牌、涨跌停已进入规则快照和回测撮合。
- 佣金/印花税/过户费已按方向和版本参数化。
- 现金分红登记/支付、送股和转增事件已经进入 Ledger，并可通过事件日志重放；配股登记日授予、认购期扣减、剩余权利截止日失效、增发认购、回购要约和可转债转股在显式数量、价格、目标标的齐全时也进入 Ledger 并可重放。发行人事件、可转债发行/回售/赎回/利息周期仍保持 fail-closed。
- A 股专用回测验收矩阵仍需用真实历史样本覆盖主板、ST、创业板、科创板、北交所、ETF、停牌和除权除息。

### P2：研究工业化

- Parquet/Arrow 分区缓存、增量更新、断点续传和缓存命中校验。
- 交易日历驱动的批量选股，避免逐股票重复网络请求。
- PIT 财务/公告/行业数据、股票池版本和生存者偏差审计。
- 多进程/列式指标计算、参数网格和 walk-forward 批量回测。

### P3：A 股实盘适配

- 先接券商模拟柜台，再接真实柜台；每家券商独立 capability、订单状态映射和对账适配。
- 实盘只允许白名单标的、限额和人工熔断；先做行情和账户只读验收，再做小额订单验收。

## 7. 当前明确不应误用的地方

- `qianxing_ashare` 的 `screen` 是轻量技术指标初筛，不是完整选股平台，也不包含财务 PIT。
- 未配置 `ashare_rules_path` 时，普通 `spot` MarketSpec 只解决价格精度、数量步长和账户记账，不自动提供 T+1、涨跌停和 A 股税费。
- easy_tdx 的在线 K 线可能只返回最近窗口；要做可复现回测，必须检查时间覆盖并落盘后再回测。
- 复权数据用于信号研究时，成交价格和现金流仍需明确使用哪套价格口径，不能把 qfq K 线直接当成交明细。
