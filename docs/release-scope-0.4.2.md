# Qianxing 0.4.2 正式发布范围

状态（2026-11-26）：[`v0.4.2` GitHub Release](https://github.com/coeasy/qianxing/releases/tag/v0.4.2)、[PyPI `qianxing==0.4.2`](https://pypi.org/project/qianxing/0.4.2/) 与 14 个 crates.io SDK 包均已发布。GitHub Release 含 32 个资产；PyPI Windows、Linux x86_64 和 macOS ARM64 三平台 wheel 均带 Trusted Publishing provenance。PR #41 修复 Criterion 性能门禁并加入受保护的精确 tag 续跑入口，合并后续跑成功。

发布后资产复核发现：GitHub Release 与 PyPI 的三个同名 wheel SHA-256 全部不一致。GitHub tag `v0.4.2` 与 Release `release-identity.json` 指向提交 `6e9e8629e378fe3dfc4e9b99cb3db2211c3ae56d`；PyPI Trusted Publishing provenance 记录的 workflow commit 是 `49ae18e497c1192a785cc382978a63e82b3b6948`。Windows 包内 CLI 也分别显示 tag 提交与派发提交。PR [#43](https://github.com/coeasy/qianxing/pull/43) 增加 wheel 来源校验并要求草稿 Release 与 PyPI 载荷逐字节相同；该 PR 尚未合并，不能改变已发布 v0.4.2 文件。完整的版本级证据与三个平台的 SHA-256 见 [`release-v0.4.2-publication-2026-11-25.json`](../maturity/evidence/release-v0.4.2-publication-2026-11-25.json)。

发布边界补记（2026-11-26）：Binance Spot `--market-type spot` quickstart 在现存 `v0.4.2` tag 创建后开发；该 tag 不含此入口。若合入并发布，须进入后续新版本，不可把它记作 `v0.4.2` 已有能力。

## 面向用户的新增体验

- PyPI wheel 提供 `qianxing quickstart --profile ccxt`，从 Python 入口完成匿名公开行情读取、回测和 HTML 报告。wheel 调用同版本自带的 `qx-cli`，并使用当前 Python 解释器启动行情 worker；用户无需单独安装 Rust 工具链或 CLI。
- CCXT quickstart 默认以 1,000 USDT 固定名义金额回测（默认 100,000 USDT 研究账户的 1%），并可通过 `backtest ccxt-builtin --target-notional` 精确调整；避免把高价 BTC 的默认整币数量当成安全仓位。
- `qianxing[ccxt]` 是可选安装项。流程只读取公开行情，不读取账户凭据、不启用执行 worker、不发送订单。无网络的离线样例仍可通过 `qianxing quickstart <目录>` 完成。
- Rust SDK 继续通过 `qx-sdk` facade 和 14 个 crates.io 包供外部工程集成。v0.4.2 的 14 包 registry 发布、逐包 checksum 与独立 consumer 验收均已完成。
- crates.io 的 `qx-sdk` 包将附带包级快速接入 README：从单一依赖创建 consumer，下载有明确合成数据标识的 BarFrame，复用只读预检、回测和产物复核；未引入额外业务门面或隐式交易行为。

## 行情与回测验收范围

- 发布 profile 使用 OKX 和 Bitget 的匿名公开数据完成同一截止时间下 180 天、15 分钟行情获取、质量检查、成本冻结、回测及 HTML 报告。
- OKX 使用公开 swap 行情接口；Bitget 超出 Classic 接口保留期时通过 CCXT 4.5.89 的 UTA 历史 K 线接口分页获取，单页最多 100 根，并校验 17,280 根数据及零缺口。CI 必须在真实公开服务上完成该验收后，才能宣称 Bitget 半年链路通过。
- Binance 保留在完整公共行情验收中；若 runner 收到官方 API 的 HTTP 451 地区资格拒绝，该任务标为受限，不换端点规避，也不计为成功。
- 这些链路不验证账户私有接口、测试网下单、实盘成交或策略收益。

## 发布验收结果

版本一致性、完整 CI、Python wheel clean install / quickstart、OKX 与 Bitget 公开行情验收、Windows/Linux/macOS CLI 与 wheel 构建、C++ SDK、SBOM、校验和及发布 provenance 均完成。但发布资产筛选在续跑路径中混入了不同构建来源，造成 GitHub Release 与 PyPI 的 wheel 文件不一致；因此“已发布”不等于“跨渠道构件一致”。发布文件、provenance 与差异可从 [GitHub Release](https://github.com/coeasy/qianxing/releases/tag/v0.4.2)、[PyPI 版本页](https://pypi.org/project/qianxing/0.4.2/) 及本页证据 JSON 核验。

Rust SDK 14 包闭包已从不可变 `v0.4.2` tag 依赖拓扑发布；registry API 与 Ubuntu、Windows、macOS clean Cargo home consumer quickstart 均验证了 `qx-sdk 0.4.2`。

## 不在本版承诺范围

- OKX、Binance 或 Bitget 的真实测试网/实盘下单与生产批准。
- Python wheel 的 CCXT 路径不是账户交易客户端；公开行情验收不得表述为订单连接器验收。
- Paper 撮合结果不等价于交易所撮合，不证明真实流动性、排队位置、市场冲击或策略盈利能力。

Python wheel 与 GitHub Release 已发布，但其三平台同名 wheel 不一致。修复必须使用新版本号重建并同时发布 PyPI 与 GitHub Release，先验证 wheel 文件名和字节完全一致、内置 CLI 报告提交与 tag 一致，再把版本记录为发行闭环。发布只声明本页列出的回测、Paper 与匿名公开行情范围；任何账户交易、测试网或生产下单能力仍须由各自的外部验收和成熟度证据单独证明。
