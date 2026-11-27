# Qianxing 0.5.0 发布候选范围（未发布）

状态（2026-11-27）：这是隔离候选树中的版本准备，不是正式发行。`0.5.0` 尚无 Git tag、PyPI 版本或 crates.io 包；候选分支 `codex/release-0.5.0-prep` 已推送但仍未合并到 `main`。Rust 包闭包验收所用的候选源码提交为 `979c8346d25b89bbfe3fa2253f454cdb6dbddae4`；后续仅补充文档和验收证据。本机验收不等同于干净 tag 的发布证据；正式构建必须在获准合并后的精确 tag 上重新运行。

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
- 与主工作树的 `codex/release-sdk-closure` 分支逐项对照后，没有整支合入：它相对当前 `origin/main` 落后 26 个提交、另有 18 个分支提交，直接合并会在多个已演进文件产生冲突。候选已包含这些分支所做的关键改进的当前实现：控制审计分页、Python 3.10 超时/取消清理、无变化 Run 轮询跳过快照写入，以及拆分后的 Binance Spot 认证、用户流、行情和订单模块；候选同时保留当前 main 与 PR 的后续改进。
- 候选源代码快照 `f2a657c5` 上的本地 Podman/Linux 实验通过 25 项服务与恢复检查：PostgreSQL/NATS 被忽略契约 13 项、NATS 有界等待 8 项、R12 worker 跨进程 Unknown 恢复 1 项、R13 cursor/blob/runtime-cache 3 项。R12/R13 运行输入文件与当前候选之间无差异；验收记录仍如实绑定到其实际运行的 `f2a657c5`。首次 R12 因测试夹具目录只读失败，调整容器挂载后复跑通过，首次失败和最终通过均保留在台账。
- 候选源码快照 `5eed760e` 上再次运行 Rust 发布包检查：14 个 `qx-sdk` 闭包 crate 均成功打包和独立编译，并从归档包构建/运行 Rust quickstart，70 根示例 Bar、1 笔成交、`Verified: true`。结果记入 [R01-AC01 日志](../maturity/acceptance-artifacts/274ddb07-6228-4159-a08d-71f12f8a9d8a.log)与[源码清单](../maturity/acceptance-artifacts/274ddb07-6228-4159-a08d-71f12f8a9d8a.source-tree.json)；这仍是本地离线归档消费者，不是 crates.io registry install 或发布。
- 当前验收账本校验通过：27 个场景、395 条 artifact-bound 运行记录。该状态只覆盖本机候选验证与开放 PR 各自 head 的托管检查，不等同于最终集成提交或精确 `v0.5.0` tag 的 hosted CI。`main` 合并、最终 tag 构建、PyPI/GitHub wheel 字节一致性、crates.io 0.5.0 registry consumer 仍是正式发布门禁。

## 最新候选补充验收（2026-11-27 04:14 UTC）

- 当前本机候选分支 `codex/release-0.5.0-prep` HEAD 为 `dbae3a0a`；本轮实际编译和安装的代码身份是候选集成提交 `2d5c5df9d83cc86b24ad100912d50d44c4d44ac7`。后续提交仅增加 Linux 验收台账和本说明，不改运行时代码。该候选分支仍只在本机；远端 `main` 仍为 `643f1293d006915c931adb9fd1fd61f91a73d422`。
- Debian 13 x86_64 Box 上的全新 Python 3.13 虚拟环境安装了 `qianxing==0.5.0` Linux abi3 wheel（SHA-256 `013a87abbe8341fda12c8908f548d811bff262c13f907731e14e991af75c6370`）及 CCXT 4.5.89。直接匿名请求 OKX Swap、Bitget Swap 和 Binance Spot 公开行情，各运行 180 天/15 分钟回测：均有 17,280 根 Bar、缺口 0、HTML 和三张 SVG、EvidenceBundle 六个工件核验通过及输入身份复算通过。OKX 1,007 fills/0 拒单，hash `b86e83b72a23b722`；Bitget 1,013/0，hash `7fc53ea312b86101`；Binance Spot 978 fills/30 拒单，30 笔均为最小名义金额限制且通过项目的现货容忍规则，hash `e22d1886eea39f4a`。未加载密钥、未调用私有 API、未提交订单；`result_recomputed=false`，因此没有声称独立复算策略结果。原始行情与报告留在 Box。完整验收及源码清单见 [R01-AC01 记录](../maturity/acceptance-artifacts/622eabcb-7d42-4b48-ab80-312758a42d53.log) 和[源码清单](../maturity/acceptance-artifacts/622eabcb-7d42-4b48-ab80-312758a42d53.source-tree.json)。
- 当前开放的 #37–39、#42–47、#49–53 共 14 个 PR 均有成功的最新 CI/公共行情工作流，评审线程为 0；#37→#38→#39 有依赖顺序。#41 和 #48 已合并。尚未合并的 14 个 PR 仍需项目所有者授权后才能进入 main；#53 的公开 Rust API 破坏性变更限定在 0.5.0 版本边界内。
- 2026-11-27 04:14 UTC 对 Git tag `v0.5.0`、PyPI `qianxing==0.5.0`、crates.io `qx-sdk 0.5.0` 的官方查询均返回 404；因此没有创建或发布 0.5.0。合并授权完成后还需在最终 main/tag 上重跑托管发布门禁与三平台字节一致性核验。
- 新增 Box 验收后，策略重构台账为 27 个场景、397 条 artifact-bound 运行记录；R01–R23 校验和 `git diff --check` 均通过。这些本机结果不取代最终 tag 的 CI、注册表消费者或任何私有交易所验收。

## Linux workspace 全量测试补充（2026-11-27）

- 在 Debian 13 x86_64 Linux Box 上使用 Rust/Cargo 1.99.0、Python 3.13.5，构建 `qx-python` 与 `qx-cli` 后运行 `cargo test --workspace --locked -j 2`：2,961 项通过、0 失败、2 项忽略。为控制 Box 磁盘占用，构建关闭了调试符号与增量缓存；这不改变测试判据。该运行来自集成候选代码提交 `2d5c5df9`，并包含下列测试兼容改动。
- 这次运行发现新版 Cargo 只在测试进程运行时提供 `CARGO_BIN_EXE_qx-cli`；测试中 157 处编译时 `env!` 会令 workspace 无法编译。现改为运行时读取，单元测试辅助函数保留旧 Cargo 的编译时回退；兼容修复提交 `93835674` 已推送到候选分支。此改动仅调整测试可执行文件定位，不改变产品运行逻辑。
- 同一候选树的格式检查、架构快检（4 片共 378 项）、能力目录校验和策略重构台账校验均通过；台账为 27 个验收场景、397 条 artifact-bound 运行记录。Box job `f271e965` 是本机运行记录，不是最终 tag 的托管 CI。
- 复核时开放 PR #37–39、#42–47、#49–53 的 GitHub 检查均为成功且无未解决评审线程；#38 以 #37 为基础、#39 以 #38 为基础。它们仍未合并到 `main`；候选分支上的集成测试不能代替合并后的 `main` 与精确 tag 托管验收。

## Linux Python wheel 集成套件（2026-11-27）

- 在 Debian 13 x86_64 Box 的全新 Git 克隆中，针对候选提交 `aecffc3db5d2a77c808dbd87f91b741179172bd4` 运行 `python -m unittest discover -s python/tests -q`：708 项运行、5 项跳过、0 项失败，用时 90.857 秒。Python 源码包加载自干净候选树，原生扩展来自已安装的 `qianxing==0.5.0` manylinux abi3 wheel；Python 为 3.13.5。wheel SHA-256 为 `013a87abbe8341fda12c8908f548d811bff262c13f907731e14e991af75c6370`，扩展 SHA-256 为 `cc690f1dba60f55a152dba1b260bd06f2ef233e5bdc75cb262082e9961dafec3`。
- 本运行是 Python 包与 wheel 原生扩展的 CI 对照套件；测试内的隔离测试替身不构成交易所接口验收。OKX、Bitget 和 Binance 的真实公开行情验收见上方 R01 记录；本次没有加载账户凭据、调用私有接口或提交订单。
- 运行日志和候选源码清单分别见[验收日志](../maturity/acceptance-artifacts/99c6a160-db09-4e32-af63-d128d7e31764.log)与[源码清单](../maturity/acceptance-artifacts/99c6a160-db09-4e32-af63-d128d7e31764.source-tree.json)。Box job `67ab11af` 属于本机验收，仍需在获准合并后的精确 tag 上重跑托管发布门禁。

## Linux wheel 安装与首跑闭环（2026-11-27）

- Debian 13 x86_64 的全新 Python 3.13.5 虚拟环境从本地 wheel 离线安装 `qianxing==0.5.0`，`pip check` 无依赖错误。运行 `doctor` 时，原生扩展为 `matched`，wheel 内置 renderer 为 `bundled/ready`；`doctor.cli=unavailable` 只代表没有单独安装到 PATH 的 Rust CLI，不影响 Python wheel 的 quickstart。该解释已补入[首跑指南](first-run.md)。
- 一条 `qianxing quickstart <新目录>` 完成 600 根样例 Bar 预检、回测、HTML 报告和 EvidenceBundle 校验；项目运行 `succeeded`，产生 1 笔样例成交，输入身份和产物摘要校验通过。`result_recomputed=false` 被如实保留：验收确认输入身份与工件完整性，没有重跑策略复算结果。
- 同一 wheel 的 `tools/python_wheel_smoke.py` 在干净源码 worktree `2d5c5df9d83cc86b24ad100912d50d44c4d44ac7` 上通过：80 根样例首跑耗时 0.573 秒，生成 66,955 字节 HTML；wheel 的内置 renderer 与版本、schema 和 Linux 平台匹配。附加 smoke 覆盖 17 个内置策略、基础 Bar/A 股回测、多账户与深度入口。
- 两次验收使用的 wheel SHA-256 均为 `013a87abbe8341fda12c8908f548d811bff262c13f907731e14e991af75c6370`；其构建身份为 `2d5c5df`，所以这些是候选 wheel 的 Linux 安装证据，不是最终 tag 资产证明。完整首跑日志见[安装与 quickstart 日志](../maturity/acceptance-artifacts/9d503f32-17e5-4f89-805e-2759dfd4a628.log)，wheel smoke 见[集成 smoke 日志](../maturity/acceptance-artifacts/4c8a9e17-46c2-47f5-aad1-89d15200e4c2.log)；两者都绑定到该构建提交的[源码清单](../maturity/acceptance-artifacts/622eabcb-7d42-4b48-ab80-312758a42d53.source-tree.json)。这些样例验收使用本地样例数据，不连接交易所；真实公开行情结果见前述 R01 记录。

## 发布门禁现状（2026-11-27 05:53 UTC）

- 远端候选分支 `codex/release-0.5.0-prep` 已推送，仍未合并；该次 Rust 包闭包验收的源码提交为 `979c8346d25b89bbfe3fa2253f454cdb6dbddae4`。远端 `main` 为 `643f1293d006915c931adb9fd1fd61f91a73d422`。
- 05:53 UTC 重新读取 PR 元数据：#37–39、#42–47、#49–53 共 14 个均为 open、mergeable，未解决评审线程均为 0。CI 成功：#37 [916](https://github.com/coeasy/qianxing/actions/runs/45778544956)、#38 [917](https://github.com/coeasy/qianxing/actions/runs/45778564767)、#39 [941](https://github.com/coeasy/qianxing/actions/runs/45885805476)、#42 [943](https://github.com/coeasy/qianxing/actions/runs/45944520263)；#43 [CI 919](https://github.com/coeasy/qianxing/actions/runs/45787126102) / [公共行情 163](https://github.com/coeasy/qianxing/actions/runs/45787128184)，#44 [CI 907](https://github.com/coeasy/qianxing/actions/runs/45774072840) / [公共行情 162](https://github.com/coeasy/qianxing/actions/runs/45774070953)；#45 [908](https://github.com/coeasy/qianxing/actions/runs/45774421407)、#46 [909](https://github.com/coeasy/qianxing/actions/runs/45774432608)、#47 [930](https://github.com/coeasy/qianxing/actions/runs/45831113518)；#49 [CI 901](https://github.com/coeasy/qianxing/actions/runs/45755956028) / [公共行情 158](https://github.com/coeasy/qianxing/actions/runs/45755955967)，#50 [CI 903](https://github.com/coeasy/qianxing/actions/runs/45765954405) / [公共行情 160](https://github.com/coeasy/qianxing/actions/runs/45765954411)；#51 [928](https://github.com/coeasy/qianxing/actions/runs/45815896330)、#52 [932](https://github.com/coeasy/qianxing/actions/runs/45845719385)、#53 [CI 938](https://github.com/coeasy/qianxing/actions/runs/45871076944) / [公共行情 169](https://github.com/coeasy/qianxing/actions/runs/45871076927)。#37→#38→#39 按依赖顺序处理；PR #48 已合并。上述 CI 属于各自 PR head，并非同一集成提交。其余 PR 的合并仍待项目所有者授权。
- 在干净候选提交 `979c8346d25b89bbfe3fa2253f454cdb6dbddae4` 上，R01-AC01 [Rust 归档闭包验收日志](../maturity/acceptance-artifacts/d4fb3567-805b-4a53-88d7-44d3b7d0e9aa.log)与[源码清单](../maturity/acceptance-artifacts/d4fb3567-805b-4a53-88d7-44d3b7d0e9aa.source-tree.json)记录了 14 个 crates 归档包的离线打包和独立编译，以及仅使用这些归档依赖的 Rust quickstart（70 根示例 Bar、1 笔成交、`Verified: true`）。该检查没有从 crates.io 安装或发布。
- 05:53 UTC 核验 GitHub `v0.5.0` tag 仍不存在；官方 [PyPI qianxing 0.5.0 查询](https://pypi.org/pypi/qianxing/0.5.0/json)与 [crates.io qx-sdk 0.5.0 查询](https://crates.io/api/v1/crates/qx-sdk/0.5.0)均返回 HTTP 404。没有创建 tag 或发布包；这些状态不是版本预留。创建 0.5.0 发布仍需明确授权，且正式包必须在最终 tag 上重跑构建、字节一致性与 crates.io 注册表消费者验收。
- PR 工作流的 Binance testnet 凭据步骤被跳过，因此当前证据只支持回测、Paper、公共行情和已执行的本地/托管合同检查，不证明私有测试网或实盘下单。
