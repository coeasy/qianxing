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

### Python 安装包校验

v0.4.2 的 Windows x64、Linux x64 和 macOS ARM64 wheel 在 PyPI 与 GitHub Release 上使用相同文件名，两个渠道公布的 SHA-256 均不同。使用 pip 安装时可在 [PyPI 文件列表](https://pypi.org/project/qianxing/0.4.2/#files) 中核对摘要。手动下载 GitHub 附件时使用该 Release 附带的 [SHA256SUMS](https://github.com/coeasy/qianxing/releases/download/v0.4.2/SHA256SUMS)。

在 Windows x64 的两个独立环境中，分别安装各渠道校验过的 wheel。两者均完成离线 quickstart、生成 HTML 并通过产物核验。PyPI wheel 内置 CLI 的构建提交为 `6e9e862`，与 v0.4.2 标签一致。GitHub Release 的同名 wheel 报告 `49ae18e`。需要复现标签构建时，在 Windows 上选用 PyPI wheel。

另在 Debian 12、CPython 3.13 的隔离 Linux 容器中，按 GitHub Release API SHA-256 校验并安装 Linux x86-64 Release wheel；容器断网运行 `qianxing quickstart --rows 80 --format html`，预检 ready、80 根明确标记为 synthetic sample 的 Bar、HTML 报告生成，来源产物复核通过。报告中的 bundled CLI 标识为 `qianxing 0.4.2 (build 49ae18e, target x86_64-unknown-linux-gnu)`。这只验证 GitHub Release Linux wheel；PyPI Linux wheel 的 SHA-256 是 `3f599f612b410581c3fa4f37fc1e6124c8afcd1315a4572c03c3c1fba53dfc45`，GitHub Release Linux wheel 为 `2d46e1418b686d107660d362c64e0ee7a73a38c8f4ac88fb24fe8ac156eb50f7`，两者字节不同，不能把此次验收转述为 PyPI Linux wheel 验收。
