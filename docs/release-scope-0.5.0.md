# Qianxing 0.5.0 发布候选范围（未发布）

状态（2026-11-27）：这是隔离候选树中的版本准备，不是正式发行。`0.5.0` 尚无 Git tag、PyPI 版本或 crates.io 包；候选包含尚未合并的集成 PR，且工作树为 dirty。不要把本页的本机验收结果当成干净 tag 的发布证据。正式构建必须在获准合并后的精确 tag 上重新运行。

## 版本边界

候选包含对公开 Rust 枚举 `qx_core::EventKind::Rejected` 的源码不兼容变更：新增 `reference_price_exact: Option<ExactDecimal>` 与 `order_snapshot: Option<Box<Order>>`。因此采用 `0.5.0`，不作为 `0.4.x` 补丁发布。旧事件 JSON 缺少这两个字段时，新版本以 `None` 读取；当任一字段有值时，它参与事件摘要，旧运行时不能验证这种新事件身份。

## 面向用户的候选范围

- Python/Rust 首跑继续围绕安装、doctor、quickstart、报告与 EvidenceBundle；样例行情明确是合成数据。
- Python wheel 可携带匹配版本的 `qx-cli`，本机隔离安装后，`qianxing quickstart sample-project --format html` 从一条命令完成回测和报告生成。
- 回测拒单报告新增逐订单详情与省略计数；历史摘要仍可读。
- Rust SDK 保持单一 `qx-sdk` facade；本地归档闭包包含 14 个 crate，并通过独立消费者 quickstart。
- 连接器范围仍按产品和操作标注。匿名公开行情不是私有账户、测试网或实盘订单验收；Paper 结果也不是交易所成交证明。

## 本机候选验证

- Windows：`cargo test --workspace --all-targets --locked --offline` 通过。
- Python：从仓库根目录以候选原生扩展运行 `python -m unittest discover -s python/tests -q`，687 项运行、7 项跳过、无失败。
- `python tools/check_architecture.py` 通过。
- `python tools/check_rust_sdk_packages.py qx-sdk --offline --allow-dirty` 验证 14 个归档包，并从这些归档编译、运行独立 quickstart；结果 `Verified: true`。
- Python wheel 在隔离环境离线安装；内置 renderer 报告版本、平台和 schema 匹配。样例 quickstart 产生 HTML 与 EvidenceBundle，`result_recomputed=false`，所以这里只证明输入身份及产物完整性，没有声称重新计算回测结果。

Windows workspace 验收之外，已用本地 Podman 启动 Linux Rust 1.98.1 与 PostgreSQL 16 容器：存储层 34 项常规测试和 11 项 PostgreSQL 忽略集成用例通过；`qx-cli` 的 R12 Unknown 跨进程恢复用例在 0.5.0 候选树通过（147.82 秒），覆盖 file、SQLite、PostgreSQL，并将证据绑定到源码清单。该恢复用例使用 fixture venue，没有连接交易所或提交订单。证据见 [`R12-AC01 运行日志`](../maturity/acceptance-artifacts/058f92cc-8ef6-456e-bd7f-d2a47bb15a81.log) 和[源码清单](../maturity/acceptance-artifacts/058f92cc-8ef6-456e-bd7f-d2a47bb15a81.source-tree.json)。

这不是最终 tag 的 hosted Linux/PostgreSQL CI 证据，也没有覆盖整个 workspace 的 Linux 构建；外部 crates.io consumer、带凭据的交易所 testnet/实盘测试仍未执行。

## 真实公共行情 quickstart

- 已合入候选源码的 PR #50 提交 `fad830e2` 在 [GitHub Actions run #45765954411](https://github.com/coeasy/qianxing/actions/runs/45765954411) 完成：Linux wheel 构建成功，Binance Spot、Bitget swap、OKX swap 三个真实公共行情 quickstart 均成功；每条链路使用 180 天、15 分钟 K 线。该结果验证的是此提交的 hosted Linux 路径，不是最终 `v0.5.0` tag 资产验收。
- 候选 wheel 在本机通过 Binance Spot 真实公共 API 拉取 17,280 根 15 分钟 K 线，缺失区间为 0，并生成 HTML 报告。该运行未加载账户凭据、未调用私有账户接口、未提交订单；EvidenceBundle 复核 `result_recomputed=false`，仅证明输入身份和产物完整性。
- 本机到 OKX/Bitget 的请求受当前网络出口限制：OKX 域名解析到不可路由的链路本地地址，Bitget 连接被重置，Linux 容器内 OKX 请求超时。因此本机没有通过这两家验收；上述 hosted Linux 结果是对应连接器公共行情路径的验收证据，不应外推为本机网络或私有交易能力。

## 正式发布前必须满足

1. 按依赖顺序合并候选 PR，并在最终 `main` 提交上重新跑 CI；PR #43 的源码身份与 GitHub/PyPI wheel 字节一致性修复必须纳入。
2. 从干净的精确 `v0.5.0` tag 生成资产，复核 CLI/wheel 构建身份、checksums、SBOM、provenance，确认 GitHub Release 与 PyPI wheel 文件名和字节一致。
3. 从该 tag 顺序发布 crates.io 闭包，再由独立 Cargo home 从 crates.io 安装并运行 SDK consumer；本地 archive patch 验收不能替代注册表验收。
4. Linux/PostgreSQL 与跨平台 wheel/CLI 验收以最终 tag 的 hosted CI 为准。私有账户、testnet 和生产下单不在此 Backtest/Paper 候选的验收声明内。

已有 `v0.4.2` Release、PyPI wheel 和 crates.io 包均不可原位替换；如果正式候选遇到差异或发布失败，应修复后另建新版本，不移动旧 tag。

## crates.io 发布顺序预检

候选上的 `python tools/cargo_publish_plan.py qx-sdk` 已解析出 14 个 crate 的保守发布顺序：`qx-core` → `qx-control` → `qx-guanxing` → `qx-datastruct` → `qx-data` → `qx-provider` → `qx-risk` → `qx-spec` → `qx-strategy` → `qx-zhenlu` → `qx-xingban` → `qx-app` → `qx-execution` → `qx-sdk`。当前 29 个 `qx-*` workspace crate 版本均为 `0.5.0`。该计划会计入可选及目标平台依赖，但不查询 crates.io 上的名称/版本可用性；发布前仍须对每个版本做注册表预检并按顺序发布。

已于 2026-11-26 UTC 查询官方 [crates.io sparse index](https://index.crates.io/config.json)：上述 14 个 crate 均已登记，当前最新版本均为 `0.4.2`，尚无 `0.5.0` 记录。原始响应摘要保存在[注册表预检证据](../maturity/evidence/crates-io-v0.5.0-availability-2026-11-27.json)。这证明检查时目标版本尚未发布；实际发布前仍需立即复查，不能视为版本预留或上传权限证明。
