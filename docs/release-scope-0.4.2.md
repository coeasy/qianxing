# Qianxing 0.4.2 发布范围

v0.4.2 回测 / Paper 正式版已发布。Python 使用 [PyPI 的 qianxing 0.4.2](https://pypi.org/project/qianxing/0.4.2/)，Rust 工程使用 [crates.io 的 qx-sdk 0.4.2](https://crates.io/crates/qx-sdk/0.4.2)，CLI 与 Web Console 从 [GitHub Release v0.4.2](https://github.com/coeasy/qianxing/releases/tag/v0.4.2) 下载。 [正式发布运行](https://github.com/coeasy/qianxing/actions/runs/45554760818)

## 面向用户的新增体验

- PyPI wheel 提供 `qianxing quickstart --profile ccxt`，从 Python 入口完成匿名公开行情读取、回测和 HTML 报告。wheel 调用同版本自带的 `qx-cli`，并使用当前 Python 解释器启动行情 worker；用户无需单独安装 Rust 工具链或 CLI。
- CCXT quickstart 默认以 1,000 USDT 固定名义金额回测（默认 100,000 USDT 研究账户的 1%），并可通过 `backtest ccxt-builtin --target-notional` 精确调整；避免把高价 BTC 的默认整币数量当成安全仓位。
- `qianxing[ccxt]` 是可选安装项。流程只读取公开行情，不读取账户凭据、不启用执行 worker、不发送订单。无网络的离线样例仍可通过 `qianxing quickstart <目录>` 完成。
- Rust SDK 继续通过 `qx-sdk` facade 和 14 个 crates.io 包供外部工程集成。v0.4.2 的 14 包 registry 发布、逐包 checksum 与独立 consumer 验收均已完成。
- crates.io 的 `qx-sdk` 包附带包级快速接入 README：从单一依赖创建 consumer，下载有明确合成数据标识的 BarFrame，复用只读预检、回测和产物复核；未引入额外业务门面或隐式交易行为。

## 行情与回测验收范围

- 发布 profile 使用 OKX 和 Bitget 的匿名公开数据完成同一截止时间下 180 天、15 分钟行情获取、质量检查、成本冻结、回测及 HTML 报告。
- OKX 使用公开 swap 行情接口；Bitget 超出 Classic 接口保留期时通过 CCXT 4.5.89 的 UTA 历史 K 线接口分页获取，单页最多 100 根，并校验 17,280 根数据及零缺口。CI 必须在真实公开服务上完成该验收后，才能宣称 Bitget 半年链路通过。
- Binance 保留在完整公共行情验收中；若 runner 收到官方 API 的 HTTP 451 地区资格拒绝，该任务标为受限，不换端点规避，也不计为成功。
- 这些链路不验证账户私有接口、测试网下单、实盘成交或策略收益。

## 发布准入

正式发布前必须在同一个候选提交上完成版本一致性、完整 CI、Python wheel clean install / quickstart、OKX 与 Bitget 公开行情验收、Windows/Linux/macOS CLI 与 wheel 构建、C++ SDK、SBOM、校验和及来源证明。Python wheel 和内置 CLI 的身份必须匹配。

Rust SDK 14 包闭包已从不可变 `v0.4.2` tag 依赖拓扑发布；registry API 与 Ubuntu、Windows、macOS clean Cargo home consumer quickstart 均验证了 `qx-sdk 0.4.2`。Python wheel 发布、三平台构建和 OKX/Bitget 匿名行情验收见[发布运行](https://github.com/coeasy/qianxing/actions/runs/45554760818)。

## 不在本版承诺范围

- OKX、Binance 或 Bitget 的真实测试网/实盘下单与生产批准。
- Python wheel 的 CCXT 路径不是账户交易客户端；公开行情验收不得表述为订单连接器验收。
- Paper 撮合结果不等价于交易所撮合，不证明真实流动性、排队位置、市场冲击或策略盈利能力。

发布资产包括三平台 CLI 和 wheel、C++ SDK、Web Console、SBOM、release-identity.json 与 SHA256SUMS。下载文件后可对照 [SHA256SUMS](https://github.com/coeasy/qianxing/releases/download/v0.4.2/SHA256SUMS) 核验。
