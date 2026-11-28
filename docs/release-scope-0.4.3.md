# Qianxing v0.4.3 发布候选范围

状态：**候选准备中，未创建 tag、未发布安装包。**

本候选将已合入 `main`、但不包含在 v0.4.2 发行件中的回测 / Paper 改进交付到 Python、Rust、CLI、C++ SDK 与 Web Console 安装渠道。它继续限定为研究、回测和本地 Paper 产品版本。

## 候选功能

- **更短的首跑路径：** Rust CLI 和 Python wheel 支持本地 Paper 首跑与版本化机器结果合同；内置策略目录和 `strategy init` 支持 JSON 输出及信号参数配置；策略目录和生成的 README 共用研究范围与限制说明。
- **更快的集成：** Rust SDK README quickstart 纳入编译验收；Python 运行结果通过结构化合同传递，不再解析人类日志；Console 支持教学样例首跑和显式生成公开行情 Provider 配置。
- **更完整的只读行情研究：** CLI/Python 首跑覆盖 OKX、Bitget、Binance 永续及 Binance Spot 公共 K 线，并生成可复核回测报告；这些路径不读取私有凭据、不提交订单。
- **回测与运行时修正：** 修复 VWAP 与 Keltner ATR 的计算边界；补充策略信号、输入身份与快照校验；改进订单恢复索引及 PostgreSQL EventLog 尾部读取；Bitget OCO 的双腿成交和子单身份缺失会保留人工对账状态并给出诊断。
- **发布一致性：** PR #43 已加入未来发布的来源身份检查和 Release/PyPI wheel 字节比较。本候选只有在 tag 流水线通过该检查后，才能将跨渠道构件一致性列为已验收。

## 必须通过的候选验收

1. PR CI、回测与 Paper 验收在同一候选提交通过；安装包的 CLI、原生扩展、Python 元数据、Rust crate、C++ SDK 和冻结版本一致。
2. Windows、Linux、macOS 构建与 clean-install 通过；Rust SDK 14-crate closure 可从 crates.io 干净消费；Python wheel 内 CLI 的 build commit 等于不可变 release tag。
3. OKX、Bitget 和 Binance Spot 公共行情验收通过，报告包含数据来源、窗口、数据质量和结果身份。HTTP 451 或其他地区拒绝必须如实记为受限，不得绕过端点或计作成功。
4. PyPI 发布前后校验 Release 草稿附件与 PyPI 上传的三平台 wheel 文件名及字节完全一致；校验失败时不得发布 GitHub Release。
5. 保存 SHA-256 清单、SBOM、release identity 和 provenance；Rust crate 依赖闭包逐包校验 registry checksum 和独立 consumer quickstart。

## 明确不在本候选范围

- 真实交易所私有账户连接、testnet 下单或 production 下单批准。
- 连接器订单类型、杠杆和持仓模式的全量 sandbox conformance。
- 将未测量的九个性能 profile 宣称为达标，或将 Paper 成交描述为交易所撮合结果。

候选验证失败、缺少测试网凭据或外部市场不可达时，保持对应成熟度为未验收；不以源码存在或公开行情成功替代证据。
