# Qianxing 0.5.0 发布候选范围（未发布）

状态（2026-11-27）：这是隔离候选树中的版本准备，不是正式发行。`0.5.0` 尚无 Git tag、PyPI 版本或 crates.io 包；候选树包含尚未合并到 `main` 的集成改动，验收源码提交为 `0ebd503e`。本页随后仅补充验证记录。这里的本机验收不等同于干净 tag 的发布证据；正式构建必须在获准合并后的精确 tag 上重新运行。

## 版本边界

候选包含对公开 Rust 枚举 `qx_core::EventKind::Rejected` 的源码不兼容变更：新增 `reference_price_exact: Option<ExactDecimal>` 与 `order_snapshot: Option<Box<Order>>`。因此采用 `0.5.0`，不作为 `0.4.x` 补丁发布。旧事件 JSON 缺少这两个字段时，新版本以 `None` 读取；当任一字段有值时，它参与事件摘要，旧运行时不能验证这种新事件身份。

## 面向用户的候选范围

- Python/Rust 首跑继续围绕安装、doctor、quickstart、报告与 EvidenceBundle；样例行情明确是合成数据。
- Python wheel 可携带匹配版本的 `qx-cli`，本机隔离安装后，`qianxing quickstart sample-project --format html` 从一条命令完成回测和报告生成。
- 回测拒单报告新增逐订单详情与省略计数；历史摘要仍可读。
- Rust SDK 保持单一 `qx-sdk` facade；本地归档闭包包含 14 个 crate，并通过独立消费者 quickstart。
- 连接器范围仍按产品和操作标注。匿名公开行情不是私有账户、测试网或实盘订单验收；Paper 结果也不是交易所成交证明。

## 本机候选验证

- Windows 候选 HEAD `0ebd503e`：`cargo test --workspace --all-targets --locked --offline -j 2` 通过；workspace 单元/集成测试及 Criterion 基准冒烟目标均完成，无失败。
- Python：以候选原生扩展运行 `python -m unittest discover -s python/tests -q`，687 项运行、7 项跳过、无失败。执行时从测试子进程清理不完整的 `GIT_CONFIG_*` 注入变量；原始环境下唯一失败的隔离 `git init` 用例在清理后通过。
- 前后端契约：Console 全部 Node 测试 25/25 通过；`cargo test --locked --offline -p qx-api --all-targets -j 2` 通过，覆盖 130 项 API 单元测试和全部集成测试。旧部署观察缺少 `trading_enabled`/`freshest_age_ms` 时分别显示“未评估”/“—”；显式 null 不显示成文本 `undefined`，明确 `false` 仍保持为 false。
- 该状态兼容性验收已单独登记为 R18-AC02，并绑定[运行日志](../maturity/acceptance-artifacts/7afe03aa-1755-48ef-9198-7e056067609c.log)和[源码清单](../maturity/acceptance-artifacts/7afe03aa-1755-48ef-9198-7e056067609c.source-tree.json)。它只验旧字段的 UI 呈现，R18 仍等待权威部署/venue 授权来源。
- 读模型摘要已修复数组响应计数和 HTTP 错误优先级，验收记为 R18-AC03，见[运行日志](../maturity/acceptance-artifacts/8e5f0e55-9ad9-43fe-a422-ba60a7a9aacc.log)及[源码清单](../maturity/acceptance-artifacts/8e5f0e55-9ad9-43fe-a422-ba60a7a9aacc.source-tree.json)。
- `python tools/check_architecture.py --fast` 在候选 HEAD 通过：4 个分片完整覆盖 378 项具名检查且无重复；策略重构台账 26 个验收场景、382 条 artifact-bound 运行记录有效；Markdown 链接校验覆盖 106 个文件及 4,960 个路径引用。
- `python tools/check_rust_sdk_packages.py qx-sdk --offline --allow-dirty` 验证 14 个归档包，并从这些归档编译、运行独立 quickstart；结果 `Verified: true`。
- Python wheel 在隔离环境离线安装；内置 renderer 报告版本、平台和 schema 匹配。样例 quickstart 产生 HTML 与 EvidenceBundle，`result_recomputed=false`，所以这里只证明输入身份及产物完整性，没有声称重新计算回测结果。

Windows workspace 验收之外，已用本地 Podman 启动 Linux Rust 1.98.1 与 PostgreSQL 16 容器：存储层 34 项常规测试和 11 项 PostgreSQL 忽略集成用例通过；`qx-cli` 的 R12 Unknown 跨进程恢复用例在 0.5.0 候选树通过（147.82 秒），覆盖 file、SQLite、PostgreSQL，并将证据绑定到源码清单。该恢复用例使用 fixture venue，没有连接交易所或提交订单。证据见 [`R12-AC01 运行日志`](../maturity/acceptance-artifacts/058f92cc-8ef6-456e-bd7f-d2a47bb15a81.log) 和[源码清单](../maturity/acceptance-artifacts/058f92cc-8ef6-456e-bd7f-d2a47bb15a81.source-tree.json)。

另有 PR #39 当前 head 的[托管 CI run #45885805476](https://github.com/coeasy/qianxing/actions/runs/45885805476) 全部通过，其中 Linux/PostgreSQL job 覆盖 Paper 部署生命周期、Python checkpoint supervisor 重启、两类 4,096 项队列容量和六种 worker 崩溃边界。Binance testnet job 的离线 fail-closed 检查通过，但带凭据的完整 testnet 步骤被跳过。该 run 是 PR head 证据，不是最终 0.5.0 tag，也不能证明真实账户/下单验收。

本机 Podman 结果不是最终 tag 的 hosted Linux/PostgreSQL CI 证据；PR #39 hosted job 也没有覆盖最终候选的完整 workspace Linux 构建。`0.5.0` crates.io registry consumer 和带凭据的交易所 testnet/实盘测试仍未执行。

## 真实公共行情 quickstart

- 已合入候选源码的 PR #50 提交 `fad830e2` 在 [GitHub Actions run #45765954411](https://github.com/coeasy/qianxing/actions/runs/45765954411) 完成：Linux wheel 构建成功，Binance Spot、Bitget swap、OKX swap 三个真实公共行情 quickstart 均成功；每条链路使用 180 天、15 分钟 K 线。该结果验证的是此提交的 hosted Linux 路径，不是最终 `v0.5.0` tag 资产验收。
- 候选 wheel 在本机通过 Binance Spot 真实公共 API 拉取 17,280 根 15 分钟 K 线，缺失区间为 0，并生成 HTML 报告。该运行未加载账户凭据、未调用私有账户接口、未提交订单；EvidenceBundle 复核 `result_recomputed=false`，仅证明输入身份和产物完整性。
- 本机到 OKX/Bitget 的请求受当前网络出口限制：OKX 域名解析到不可路由的链路本地地址，Bitget 连接被重置，Linux 容器内 OKX 请求超时。因此本机没有通过这两家验收；上述 hosted Linux 结果是对应连接器公共行情路径的验收证据，不应外推为本机网络或私有交易能力。

## 候选 HEAD `de50bce3` 后续验收（2026-11-27）

- 精确候选 `de50bce3822b8d89ca15f307559784e6cad08a4e` 的 Windows wheel 已隔离构建并安装；wheel 内置 CLI 版本身份与候选匹配，Python quickstart、报告与 EvidenceBundle smoke 通过，`pip check` 无依赖冲突。
- 同一候选的 Linux CLI 在本地 Podman 环境完成 init、doctor、backtest、report、EvidenceBundle verify 和 status 六步 quickstart。Linux abi3 wheel 加载原生扩展后，`pip check`、Python 样例 quickstart 和本地 Paper quickstart 均通过；Paper 运行产生 1 个订单和 4 条 ledger 记录，仅使用本地合成 L1 行情，没有网络请求、凭据或实盘订单。
- 本地 Linux wheel 是从无 `.git` 元数据的源码归档构建的，因此内置 build identity 为 `unknown`，只作为 smoke 测试产物；不能作为正式发布资产。测试 wheel SHA-256：`DA43C360118E5E8A7B9B235E1125B7F56743A24D28676A2A86965E51F8D48940`。
- GitHub Actions 的真实行情成功证据仍来自 PR head，而不是精确候选 HEAD：PR #49 (`121ef78e`) 的 OKX/Bitget 与 PR #53 (`ff07316c`) 的 OKX/Bitget 任务都运行了安装后的 Python wheel、真实公开数据回测、报告渲染，并上传项目、行情 lineage、报告和日志。PR #50 (`fad830e2`) 的 OKX/Bitget/Binance Spot 任务也已成功；其 180 天、15 分钟配置断言 17,280 根 K 线且缺失区间为 0。
- PR #51 与 #52 的 Binance testnet 工作流仅通过始终运行的离线 fail-closed 检查；需要凭据的完整 testnet 验收步骤被跳过。它们没有运行 OKX/Bitget 公开行情矩阵，也没有证明真实测试网下单。候选包含这些提交，但以上 hosted job 不等同于 `de50bce3` 的 hosted 运行。
- 本机网络到 OKX/Bitget 仍分别遇到超时与连接重置。公共行情可信证据来自 GitHub hosted Linux jobs；私有账户、testnet 下单和实盘下单没有通过这些证据验收。

## 正式发布前必须满足

1. 按依赖顺序合并候选 PR，并在最终 `main` 提交上重新跑 CI；PR #43 的源码身份与 GitHub/PyPI wheel 字节一致性修复必须纳入。
2. 从干净的精确 `v0.5.0` tag 生成资产，复核 CLI/wheel 构建身份、checksums、SBOM、provenance，确认 GitHub Release 与 PyPI wheel 文件名和字节一致。
3. 从该 tag 顺序发布 crates.io 闭包，再由独立 Cargo home 从 crates.io 安装并运行 SDK consumer；本地 archive patch 验收不能替代注册表验收。
4. Linux/PostgreSQL 与跨平台 wheel/CLI 验收以最终 tag 的 hosted CI 为准。私有账户、testnet 和生产下单不在此 Backtest/Paper 候选的验收声明内。

已有 `v0.4.2` Release、PyPI wheel 和 crates.io 包均不可原位替换；如果正式候选遇到差异或发布失败，应修复后另建新版本，不移动旧 tag。

## crates.io 发布顺序预检

候选上的 `python tools/cargo_publish_plan.py qx-sdk` 已解析出 14 个 crate 的保守发布顺序：`qx-core` → `qx-control` → `qx-guanxing` → `qx-datastruct` → `qx-data` → `qx-provider` → `qx-risk` → `qx-spec` → `qx-strategy` → `qx-zhenlu` → `qx-xingban` → `qx-app` → `qx-execution` → `qx-sdk`。当前 29 个 `qx-*` workspace crate 版本均为 `0.5.0`。该计划会计入可选及目标平台依赖，但不查询 crates.io 上的名称/版本可用性；发布前仍须对每个版本做注册表预检并按顺序发布。

已于 2026-11-26 UTC 查询官方 [crates.io sparse index](https://index.crates.io/config.json)：上述 14 个 crate 均已登记，当前最新版本均为 `0.4.2`，尚无 `0.5.0` 记录。原始响应摘要保存在[注册表预检证据](../maturity/evidence/crates-io-v0.5.0-availability-2026-11-27.json)。这证明检查时目标版本尚未发布；实际发布前仍需立即复查，不能视为版本预留或上传权限证明。

## 当前候选树复核（2026-11-27）

- 候选分支 `codex/release-0.5.0-prep` 的代码集成点为 `6b269726`，已包含开放 PR #37、#38、#39、#42–#47、#49–#53 的当前 head；随后只追加了本节验收状态文档。复核时 14 个 PR 均仍为 open、GitHub 标记可合并，关联 CI/公共行情 workflow 均为 success；依赖链 #37 → #38 → #39 已按顺序集成到本地候选树。该操作没有合并到 `main`。
- 此次刷新带入 PR #39 的最终 Windows fixture 清理回归，以及 PR #42 的最新性能证据比较门禁。回归结果：性能基线、测量输入与 Rust benchmark runner 共 39 项通过；架构快检四片覆盖 378 项、无重复；能力目录 `--check` 通过；`git diff --check` 通过。
- 对应托管检查为 PR #37/#38/#39 的 CI run #916/#917/#941，PR #42 的 CI run #942，PR #43/#44 的 CI 与公共 CCXT run #919/#163、#907/#162，PR #45–#47 的 CI run #908/#909/#930，PR #49/#50 的 CI 与公共行情 run #901/#158、#903/#160，PR #51–#53 的 CI 与公共行情 run #928/#932/#938/#169；均成功。凭据保护的 Binance testnet 子步骤依然是 skipped，不构成测试网下单验收。
- 候选源代码快照 `f2a657c5` 上的本地 Podman/Linux 实验通过 25 项服务与恢复检查：PostgreSQL/NATS 被忽略契约 13 项、NATS 有界等待 8 项、R12 worker 跨进程 Unknown 恢复 1 项、R13 cursor/blob/runtime-cache 3 项。R12/R13 运行输入文件与当前候选之间无差异；验收记录仍如实绑定到其实际运行的 `f2a657c5`。首次 R12 因测试夹具目录只读失败，调整容器挂载后复跑通过，首次失败和最终通过均保留在台账。
- 当前验收账本校验通过：27 个场景、394 条 artifact-bound 运行记录。该状态只覆盖本机候选验证与开放 PR 各自 head 的托管检查，不等同于最终集成提交或精确 `v0.5.0` tag 的 hosted CI。`main` 合并、最终 tag 构建、PyPI/GitHub wheel 字节一致性、crates.io 0.5.0 registry consumer 仍是正式发布门禁。
