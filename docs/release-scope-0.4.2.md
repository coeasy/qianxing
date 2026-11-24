# Qianxing 0.4.2 发布范围（候选）

状态：候选准备中，尚未合并、打 tag 或上传 PyPI / crates.io。当前公开稳定版仍为 v0.4.1。

## 面向用户的新增体验

- PyPI wheel 提供 `qianxing quickstart --profile ccxt`，从 Python 入口完成匿名公开行情读取、回测和 HTML 报告。wheel 调用同版本自带的 `qx-cli`，并使用当前 Python 解释器启动行情 worker；用户无需单独安装 Rust 工具链或 CLI。
- `qianxing[ccxt]` 是可选安装项。流程只读取公开行情，不读取账户凭据、不启用执行 worker、不发送订单。无网络的离线样例仍可通过 `qianxing quickstart <目录>` 完成。
- Rust SDK 继续通过 `qx-sdk` facade 和 14 个 crates.io 包供外部工程集成。v0.4.2 的 registry 发布、逐包 checksum 与独立 consumer 验收必须完成后，才能宣称该版本可安装。

## 行情与回测验收范围

- 发布 profile 使用 OKX 和 Bitget 的匿名公开数据完成行情获取、质量检查、成本冻结、回测及 HTML 报告。
- OKX 15 分钟历史窗口可验收 180 天；Bitget Classic USDT 永续公开接口的 15 分钟窗口为 52 天。文档不得将 Bitget REST 范围写成半年。
- Binance 保留在完整公共行情验收中；若 runner 收到官方 API 的 HTTP 451 地区资格拒绝，该任务标为受限，不换端点规避，也不计为成功。
- 这些链路不验证账户私有接口、测试网下单、实盘成交或策略收益。

## 发布准入

正式发布前必须在同一个候选提交上完成版本一致性、完整 CI、Python wheel clean install / quickstart、OKX 与 Bitget 公开行情验收、Windows/Linux/macOS CLI 与 wheel 构建、C++ SDK、SBOM、校验和及来源证明。Python wheel 和内置 CLI 的身份必须匹配。

Rust SDK 14 包闭包应从不可变 `v0.4.2` tag 依赖拓扑发布；逐包 registry checksum 与 Ubuntu、Windows、macOS clean Cargo home 消费者 quickstart 均通过后，才将 `qx-sdk 0.4.2` 标记为已发布。

## 不在本版承诺范围

- OKX、Binance 或 Bitget 的真实测试网/实盘下单与生产批准。
- Python wheel 的 CCXT 路径不是账户交易客户端；公开行情验收不得表述为订单连接器验收。
- Paper 撮合结果不等价于交易所撮合，不证明真实流动性、排队位置、市场冲击或策略盈利能力。

发布后应把 GitHub Release、PyPI 版本、crates.io 工作流和独立消费者结果写回本页与成熟度证据；在此之前，全部 v0.4.2 内容均为候选说明。
