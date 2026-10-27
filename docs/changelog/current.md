# Recent entries

### v0.3.0 发布与 Windows 本地 Microsandbox 验收（2026-10-27）

服务端 Python 策略隔离仍以 E2B 为首选，Microsandbox 与 OpenSandbox 是显式可选后端；本地 Windows 验收只走 Microsandbox local。Microsandbox SDK 固定为 `0.7.11`，增加 Windows 11 / WHP 前置步骤和 `tools/microsandbox_local_smoke.py`，该命令在关闭 guest 网络的微虚机中执行 Python 并检查结果标记。Cargo、Python、C++ SDK 和成熟度基线版本统一准备为 `0.3.0`。

- `cargo test --workspace --offline -j 1` **2,578 通过、2 忽略、0 失败，178 个目标**；同时重建了匹配已测试提交的 Python 原生扩展。A 股 evidence 测试过长临时路径已修复，独立用例 **1/1**、完整集成组 **33/33** 及其后的全工作区回归通过。完整 Python 套件 **560 通过、10 跳过**、架构门禁 **228/228**、Rustfmt 通过。Windows CPython wheel clean-install smoke 记录于 [`python-wheel-release-candidate-2026-10-27.json`](../../maturity/evidence/python-wheel-release-candidate-2026-10-27.json)；Rust 回归见 [`rust-workspace-release-candidate-2026-10-27.json`](../../maturity/evidence/rust-workspace-release-candidate-2026-10-27.json)。
- Windows 重启后 `HypervisorPresent=True`、`msb doctor` 显示 WHP ready；Microsandbox SDK `0.7.11` 下运行 `python tools/microsandbox_local_smoke.py --image python:3.12-slim`，退出码 **0**，无网络 guest 返回 `QIANXING_MICROSANDBOX_LOCAL_OK`，临时 sandbox 随后销毁。证据见 [`microsandbox-local-windows-smoke-2026-10-27.json`](../../maturity/evidence/microsandbox-local-windows-smoke-2026-10-27.json)；重启前的失败尝试保留在 [`microsandbox-local-windows-attempt-2026-10-27.json`](../../maturity/evidence/microsandbox-local-windows-attempt-2026-10-27.json)。该验收只覆盖 Windows 本地 VM 与 guest 执行，不代表 Qianxing worker 在 guest 内的连通性或服务端 provider、租户隔离、容量、计费及生产适用性。
- v0.3.0 已通过 GitHub Actions 发布，包含 Windows/Linux/macOS CLI、三平台 Python wheel、C++ SDK、Web 控制台、SBOM、身份文件和校验清单，共 **15 个附件**；[GitHub Release](https://github.com/coeasy/qianxing/releases/tag/v0.3.0)、[Actions run](https://github.com/coeasy/qianxing/actions/runs/40518467601) 与机器记录见 [`github-release-v0.3.0-2026-10-27.json`](../../maturity/evidence/github-release-v0.3.0-2026-10-27.json)。仓库的 `PYPI_PUBLISH_ENABLED` 未开启，PyPI 发布 job 被跳过；Python wheel 可从 GitHub Release 下载。

### `llm-sandbox` 远程 Docker 策略 worker 接入（2026-10-27，未发布工作区）

新增可选 `QX_PYTHON_STRATEGY_EXECUTION_MODE=llm-sandbox` 持久 Python worker，使用 mTLS 连接远程 Docker Engine 与交互式 IPython 会话。普通本地策略开发和研究仍默认直接运行；服务器通过 `QX_PYTHON_STRATEGY_SANDBOX_REQUIRED=true` 可强制要求远端 provider，任何 provider 配置或运行故障都不会回退到本地。

- 宿主将 Docker 端点与 TLS 文件限制在 provider bridge；guest 只接收经过验证的策略 bundle、spec 与运行时事件。每次创建后读取 Docker inspection，确认 pinned image digest 对应的 image ID、无网络、CPU/内存/swap/pid 限额、无额外 capabilities、非 root UID、只读根目录、临时 `/tmp`/`/workspace` 和共享内存边界；容器镜像、Qianxing 版本和输入 bundle 也会核验。
- 依赖通过 `qianxing[sandbox-llm]` 可选安装。worker 需要 mTLS 远程 Docker、digest 固定且预装匹配版本 Qianxing/IPython 的镜像，`strategy.python_timeout_ms >= 15000`。E2B 仍是托管/多租户的首选，llm-sandbox 这条路径只实现远程 Docker 容器，不是 microVM。
- 验收：定向 worker 与审计用例 **10/10**；Rust sandbox execution policy **4/4**；完整 Python 回归 **560 通过、10 跳过、0 失败**；架构 `--fast` **228/228**；能力目录摘要、Python Ruff、Rustfmt 和差异检查通过。测试使用 fake SDK，尚未连接实际 Docker Engine，因此真实 mTLS、HostConfig 生效、隔离、容量/费用及生产资格仍未验收。
- Windows x64 CPython 3.13 wheel 已基于当前工作树重建并在新虚拟环境安装；包内 worker 与 `sandbox-llm` optional extra 核验通过，基础安装不加载 sandbox SDK。Wheel smoke 覆盖 **17** 个内置策略、加密与 A 股回测、**13/13** 策略选择、**4/4** 参数网格及多账户用例，运行产物核验通过。SHA-256 与平台边界记录于 [`python-wheel-llm-sandbox-provider-2026-10-27.json`](../../maturity/evidence/python-wheel-llm-sandbox-provider-2026-10-27.json)。该 wheel 仍是未提交工作树上的 **0.2.2** 临时构建，不是新发布件；Linux/macOS 与其他 Python 版本发行矩阵及真实远端 Docker 验收仍未完成。

### 独立进程 Strategy Worker 支持 durable checkpoint（2026-10-27，未发布工作区）

持久 `external_executable` 策略现在必须提供发布物 SHA-256；Rust host 会把 checkpoint 传给进程，并要求响应带回同身份、同版本的 JSON checkpoint。恢复状态与 EventLog cursor 一起提交；发布物或启动参数变化都会拒绝沿用旧状态。无状态策略也回传空 JSON state。C++ JSONL 示例已实现 `processed_requests` 状态往返，并修正 output schema 固定为 v1，适配 input schema v4-v9。

- 验收：`qx-strategy` **75/75**，qx-cli checkpoint 绑定/原子提交 **4/4**，跨进程响应 checkpoint parser **1/1**；Windows C++ JSONL Release 示例编译通过，两次独立进程启动验证状态 `4 → 5 → 6`；架构 `--fast` **228/228**，能力目录、格式、JSON 和差异检查通过。
- 边界：尚未对第三方外部语言实现做完整 conformance/fault-injection；外部进程本身不构成安全沙箱，外部提交与 checkpoint 文件也不是跨存储全局事务。

### C ABI 策略支持持久状态恢复（2026-10-27，未发布工作区）

C ABI 保持原有 vtable ABI 不变，增加可选成对的 checkpoint snapshot/restore 动态库导出。插件状态与 EventLog cursor、run marker 一起提交；重启时恢复最近提交状态，插件身份绑定其已验证 SHA-256。只实现一个导出、恢复失败、摘要变化或旧 checkpoint 无法恢复时拒绝继续消费事件。旧插件仍能运行，但不具备重启状态恢复。

- 验收：`qx-strategy` **74/74**（`TEMP/TMP` 指向工作区可写目录）；C ABI checkpoint JSON 往返测试通过；qx-cli checkpoint/cursor 定向测试 **3/3**；`cargo check -p qx-cli --bin qx-cli --offline` 通过。
- 边界：未覆盖任意外部进程的持久状态协议、跨存储全局事务或真实第三方 native 插件验收；E2B/Microsandbox/OpenSandbox 仍需真实远端环境验收。

### 远端 Python 模块函数策略支持 checkpoint 安全轮换会话（2026-10-27，未发布工作区）

E2B、Microsandbox 与 OpenSandbox 原先把所有没有 `QX_STRATEGY_SPEC_JSON` 的模块函数策略都视为不可恢复，即使新请求已携带宿主保存的 checkpoint 也拒绝在 session TTL 前轮换。现在三种 provider 统一按请求 checkpoint 决定是否安全轮换：策略已执行过时，只有当前请求携带 checkpoint 才会先创建新 session 并恢复；缺 checkpoint 仍在发送事件前失败关闭。配合 §13.41 的 `snapshot_state/restore_state` 与 `STRATEGY_STATELESS = True`，模块函数策略可跨 worker 和 sandbox session 恢复。

- 验收：E2B/Microsandbox/OpenSandbox fake SDK worker **40/40**；完整 Python 套件 **553 通过、10 跳过、0 失败**。qx-cli 完整测试首轮 **445/454 通过**，9 个失败均为命令二进制过期的新鲜度护栏；重建 `qx-cli.exe` 后失败项逐个 **9/9** 通过。架构 `--fast` **228/228**、能力目录检查和 Python 语法/差异检查均通过。真实 provider session 恢复仍未验收。证据：`maturity/evidence/strategy-lifecycle-warmup-matrix-2026-10-27.json`。
- 边界：fake SDK 覆盖创建/轮换/状态请求转发，不证明供应商服务实际续租、网络断流恢复、容量/费用、隔离或 Live 交易通过。

### Python 模块函数策略持久 checkpoint（2026-10-27，未发布工作区）

持久 Python Strategy worker 现在支持旧式 `on_event(request)` 函数策略的恢复状态：函数策略通过 `snapshot_state()` / `restore_state(state)` 保存 JSON 状态，或显式声明 `STRATEGY_STATELESS = True`。启动 bootstrap、策略状态与 EventLog cursor 继续使用一个 durable checkpoint 提交；回调失败会恢复内存状态并允许重放。未声明状态模式的函数策略在持久 worker 首次回调前失败关闭；本地 one-shot 回测/研究兼容行为不变。

- 验收：Python 基类与进程适配器回归 **68/68**；新增模块函数 checkpoint 用例 **4/4**；持久 Python module worker 租约接管定向测试 **1/1**；相关 Rust `rustfmt --check`、能力目录 `--check` 通过；架构 `--fast` **228/228**、四片无重复（113.34 秒）。机器证据已并入 `maturity/evidence/strategy-lifecycle-warmup-matrix-2026-10-27.json`。
- 边界：覆盖 Python 模块函数与现有 Python `BaseStrategy` checkpoint 路径；不代表 Rust/C ABI/外部进程统一状态恢复，也不代表真实远端沙箱或发布门通过。

### Strategy Job 多进程租约隔离与失租收口（2026-10-27，未发布工作区）

Strategy Job 队列 owner 改为进程实例级唯一值，避免多个进程复用同一配置 worker ID 后共享 lease/fencing token。策略评估、cursor/checkpoint 提交、已提交 run 恢复及失败收口前用当前 lease 时钟重新 claim；失租 worker 不会把新 owner 正在处理的 JobRun 标记为 Failed。行情过期路径在 ack 前重新确认 owner 并用当前 fencing token 收口。

- 验收：相同配置 ID 的并发进程租约隔离 **1/1**；Strategy snapshot stale/lease gate **6/6**；策略回调执行中被另一 owner 接管后，旧 worker 在 cursor/checkpoint 提交前退出且保留新 owner fencing lease **1/1**。首次 qx-cli 完整单测为 **404 通过、50 失败**，归因于 Windows 默认临时目录权限和 CLI 测试二进制过期；将 `TEMP/TMP` 指向工作区并重建 CLI 后，50 个失败用例（替换过时 ack 注入用例）逐项复跑 **50/50 通过**，完整 454 项未整体重跑。架构 `--fast` **228/228**、四片无重复（137.02 秒）；Rust 格式、架构脚本语法、差异检查通过。
- 边界：文件队列证明了进程 owner 隔离，尚未覆盖 SQLite/PostgreSQL 多节点压力、lease 与 checkpoint 文件的单事务以及崩溃注入；完整 W3 和发布门仍开放。

### 策略生命周期与更新回调跨宿主归一化金样（2026-10-27，未发布工作区）

扩展共享策略生命周期矩阵，统一 Bar 预热、订单/成交、账户/空持仓快照、精度盘口、Timer、版本化错误、stop、Python checkpoint 恢复和不支持输入拒绝场景。Rust Strategy trait 与 Python `BaseStrategy` Runtime adapter 消费适用场景；Python 的 Bar 生命周期验收从真实策略文件加载 `STRATEGY_CLASS`；checkpoint 金样核验 Bar 游标、预热状态、账户 EventLog 游标及重启去重，并注入“策略状态已变更后回调抛错”，验证失败响应不前移 checkpoint、失败状态被回滚且从最后提交点重试可复现。Rust strategy worker 将 checkpoint 和 EventLog cursor 原子提交；写盘失败保持旧内存/磁盘状态并清理临时文件，主文件提交后的备份清理失败不再误报事务未提交。未知市场事件在 Rust/Python 被拒绝，旧 C ABI 插件缺少精确盘口 v3 callback 时明示错误并拒绝降级；C ABI/C++ 动态宿主消费完整 callback trace。

- 验收：`qx-strategy` **86 通过、0 失败、1 项 C++ fixture 测试忽略**；Python 策略基类与 process adapter **64/64**；Rust checkpoint/cursor commit 与写盘失败恢复 **2/2**；Windows C++ Release 构建、CTest **3/3**、动态 C ABI 宿主 **1/1**；能力目录 `--check`、修改的 Rust 文件 `rustfmt --check`、相关 `git diff --check` 均通过；架构 `--fast` 四片 **228/228**、无重复（117.22 秒）。
- 边界：跨宿主 checkpoint 合同、Rust/C ABI 状态恢复、旧模块函数 worker、逐宿主 hook 能力清单和其余不支持 hook 拒绝语义仍开放；checkpoint 故障注入目前只覆盖 Python Runtime class worker。本轮 unsupported 金样只覆盖未知 event tag 与 exact OrderBook v3 缺失拒绝。C++ 验收基于仓库示例 DLL。真实远程沙箱服务与发布门未验收。证据：[`strategy-lifecycle-warmup-matrix-2026-10-27.json`](../../maturity/evidence/strategy-lifecycle-warmup-matrix-2026-10-27.json)。

### C++ 策略衍生品订单策略字段成熟度纠偏（2026-10-27，未发布工作区）

复核发现成熟度登记仍称 C++ 策略缺少 `margin_mode` / `position_mode` / `leverage`，但当前稳定 C ABI 已通过可选 `qx_strategy_get_order_policy_v1` 扩展按 `intent_id` 传送完整策略。移除过时 limitation，在 capability evidence 登记 C++ 样例、版本化 schema 与 Rust 动态宿主实测；扩展 `strategy_intent_three_language_check`，防止 C++ 示例、schema、Rust 解析/合并或逐字段回读断开。

- 验收：Windows C++ Release 示例与合同目标构建通过，CTest **1/1**；`qx-strategy` crate **83/83**，Rust 宿主加载该 C++ 动态库的 ignored 集成测试 **1/1**，断言策略方向、保证金模式、持仓模式及 3 倍杠杆；生成能力目录已同步；架构 `--fast` **228/228**（4 片、无重复，140.60 秒）。全生命周期跨宿主 golden matrix、真实交易所订单模式验收仍开放。机器记录见 [`cpp-strategy-order-policy-cross-language-2026-10-27.json`](../../maturity/evidence/cpp-strategy-order-policy-cross-language-2026-10-27.json)。

### Python/Rust 策略进程传递 exact 现金与可用保证金（2026-10-27，未发布工作区）

Strategy API 输入 v9 新增 `cash_exact` 与 `available_margin_exact`，以逐币种字符串保留 ledger/venue 账户事实中的小数精度；策略输入适配器按 1e-9 raw 的向零投影交叉校验。Rust `ExactDecimal::to_raw_truncated()` 为此提供显式兼容投影，原严格 `to_raw()` 语义不变。策略类可通过 `BaseStrategy.cash_balance()` / `available_margin()` 读取 exact 优先的值，native process context、Runtime class adapter 与候选诊断均保留侧车。v9 可以单独携带精确账户事实，也可与已有行情/执行/盘口字段组合，不再错误要求 L2 盘口。

同时将 artifact migration 登记更新至输入 v1-v9，并修正 T1-2 的静态检查：拒绝用例原先只扫描函数后的前 2,000 字符，会漏掉较长测试体中真实存在的错误消息断言；现在读取平衡括号定位出的完整测试函数体。

- 本地 `BaseStrategy` 仍默认直接执行，不加载 sandbox provider；服务器隔离执行继续由部署端强制选择 provider，当前持久 Python strategy worker 首选 E2B，Microsandbox/OpenSandbox 为可选实现，provider 失败不会回退到本机。
- 边界：只有账本或供应商给出 exact 来源时才能保留高精度；raw-only 来源不做精度猜测。没有连接真实沙箱服务或验证 Live 链；真实隔离、容量/成本及发布门仍开放。
- 验收：Python 策略契约/基类回归 **66/66**；Rust 输入 schema **12/12**、Runtime 合同 **8/8**、ExactDecimal 精度 **1/1**、CLI 账户上下文 **1/1**；CLI 测试目标编译、定向 `rustfmt --check`、`git diff --check` 通过；架构 `--fast` **228/228**（4 片、无重复）。机器记录见 [`strategy-api-input-v9-exact-cash-margin-2026-10-27.json`](../../maturity/evidence/strategy-api-input-v9-exact-cash-margin-2026-10-27.json)。

### C ABI/C++ 策略上下文保留 exact 持仓、现金与保证金（2026-10-27，未发布工作区）

新增可选 `qx_strategy_on_context_v2`，为 C/C++ 策略提供 1e-9 兼容 context 之外的 exact position/cash/margin sidecar。旧插件遇到无法无损表示的子纳米账户状态时在策略 callback 前失败关闭；无损输入保持兼容。Rust host 与实际 C++ 示例动态插件已用 18 位小数值对拍，策略 schema registry 升至 v16。

- 验收：完整 `qx-strategy` crate **83 项通过、0 失败、1 项 C++ fixture 测试忽略**；C ABI 精度/兼容定向用例 **1/1**；Windows C++ Release SDK contract 与示例插件构建通过，CTest **1/1**；Rust host 加载 C++ 动态插件集成 **1/1**，覆盖完整事件序列内 **16** 次 context 更新，并验证有损旧插件拒绝及 v2 context callback 出错时均不进入策略 `on_init`；schema golden 验证通过；architecture `--fast` 四片 **228/228**、无重复；触及的 4 个 Rust 文件 rustfmt 检查与 `git diff --check` 通过。
- 边界：完整 qx-strategy crate 回归和全仓 architecture/schema-registry 门禁通过；不代表跨语言全生命周期 golden、其他 C++ 金额合同、跨币种换算、真实交易所和远端沙箱已经验收或可发布。

### Runtime 策略构造失败通知模块级错误钩子（2026-10-27，未发布工作区）

Runtime `create_strategy(spec)` / `STRATEGY_CLASS` 在创建 `BaseStrategy` 实例前抛错时，现在调用可选模块级 `on_error_v1(StrategyErrorV1)`，发送 `stage=initialization`、策略身份和原始错误摘要；构造器返回非 `BaseStrategy` 时也作为初始化错误报告。错误观察器自身失败只记 stderr，不覆盖启动原错误。模块导入失败时模块尚不可用，仍无法调用模块钩子。

- 验收：`test_strategy_process_adapter.py` **28/28**；Runtime 构造失败 DTO 身份/阶段和错误观察器失败保留原错覆盖；worker 与测试文件 `py_compile` 通过。机器记录：`maturity/evidence/strategy-runtime-constructor-error-v1-2026-10-27.json`。
- 边界：仅补 Runtime class process 构造阶段错误观察；真实 E2B/Microsandbox/OpenSandbox 服务、隔离/容量/费用与完整发布门仍开放。

### Python/Rust 版本化策略错误 DTO 对齐（2026-10-27，未发布工作区）

Python 新增 `StrategyErrorV1`，与 Rust/C/C++ 共享 schema/golden，并增加 `on_error_v1(error)` 标准观察钩子。BaseStrategy 本地 callback、Runtime class worker、模块函数 worker 会在已知 callback 阶段构造同字段 DTO；现有 `on_error(context, error)` / `on_error(request, error)` 仍收到原生异常。双钩子分别调用，钩子失败不会替换策略原错误；stop 失败也会发送 `stage=stop`。

- 验收：Python DTO 与 Rust golden **1/1**；消息 UTF-8 长度、双 hook 失败及原错误保留测试通过；`test_strategy_base.py` **32/32**、`test_strategy_process_adapter.py` **25/25**、`test_strategy_error_v1.py` **3/3**；Python changed-file `py_compile` 通过。
- 边界：Python history-requirements 与独立诊断 worker 尚未提供统一错误来源；W3 全生命周期跨宿主 golden、真实沙箱/供应商验收和整体发布门仍开放。

### Rust Tick/OrderBook 策略错误通知补齐（2026-10-27，未发布工作区）

为 `qx_strategy::Strategy` 增加统一 callback-result 通知函数，并接入 `NativeTickStrategy`、`NativeOrderBookStrategy` 和 `DepthBarStrategy`。初始化、历史需求、start/warmup/stop、行情决策和成交更新失败都会报告 `StrategyErrorV1` 阶段、时间和标的；错误观察器自身返回错误时仍保留原 callback 错误。

- 验收：`cargo test -p qx-xingban --offline -j 1 --target-dir target/rust-workspace-error-v1` 全部通过：单测 **219/219**、集成测试 **24/24**；新用例覆盖 Tick 行情、原生 OrderBook/DepthBar 行情及 OrderBook 成交更新错误。
- 边界：覆盖的是基于 `qx_strategy::Strategy` 的 Rust 原生适配器；旧式任意实现的 `TickStrategy` / `OrderBookStrategy` 没有统一的 Strategy identity/context，因此不伪装为版本化 DTO。Python 错误 DTO 对齐、四宿主 golden matrix 和远程沙箱验收仍开放。

### Rust/C ABI 版本化策略错误观察合同（2026-10-27，未发布工作区）

新增 `StrategyErrorV1` 与 `strategy-error-v1` schema/registry v15。Rust `Strategy::on_error` 可选实现；C/C++ 插件可选导出 `qx_strategy_on_error_v1`，独立符号不改变 vtable 和 ABI 版本。Bar adapter 与 qx-cli C ABI 入口在策略 callback 失败时发送阶段、策略身份、时间、标的和有界消息；observer 返回失败或 Rust panic 时保留原始错误。C++ 示例插件已消费该 JSON，缺少符号的旧插件保持兼容。

- 验收：`qx-strategy` **72/72** + error golden **1/1**；`qx-xingban` 全包单测/集成项通过（216 项单测）；`qx-cli` `cargo check` 与 `cargo test --no-run` 通过；Windows C++ Release 构建与 CTest **3/3**；Rust 加载 C++ 插件并调用 error export **1/1**。
- 全量回归补充：`cargo test -j 1 --target-dir target/rust-workspace-error-v1 --workspace --offline --quiet` 退出码 **0**；唯一 ignored 项是需 C++ 共享库 fixture 的 SDK CI 用例，已由上述动态插件集成用例覆盖本次错误回调路径。五个本轮 Rust 文件的定向 rustfmt 检查通过；仓库级 rustfmt 检查报告工作区其它文件的格式差异，未在本轮全局重排。
- 边界：随后由本 changelog 的 Tick/OrderBook 补齐切片接入 Rust 原生适配器；Python 错误 DTO 对齐、W3 跨宿主 golden 仍开放。observer 不回滚策略状态，真实远端沙箱和生产验收仍未完成。

### BarFrame 单遍解码与回测输入复用复核（2026-10-27，未发布工作区）

`qx-datastruct::BarFrame::from_json` 与 `qx-data` Provider 严格合同路径改为单遍 typed decode，保留未来 schema 拒绝、版本化未知字段拒绝、legacy 扩展字段兼容及 exact OHLCV 投影校验。Provider 独立解析仍逐字段对拍原始帧；已验证的 Provider 行现在直接转成回测 Bar，单市场用例不再先构造一份重复 Bar 向量再丢弃。

- 验收：`qx-datastruct` **8/8**、`qx-data` **39/39**、`qx-app` workflow/use-cases/execution-binding **24/24**；六类市场 64k Bar 新旧结果 hash 一致。
- 性能结论：两轮同机 Release 对比未显示稳定端到端提速或 RSS 下降；相同候选二进制的独立 P50 轮间漂移 **-10.2% 到 +5.4%**。不声明性能收益、不更新产品 profile 状态。证据：`maturity/evidence/profile-market-backtest-parse-optimization-review-2026-10-27.json`。
- 验收补充：`cargo test --workspace --offline --quiet` 完整通过，唯一忽略项为需 C++ 共享库 fixture 的 SDK CI 测试；定向 Rustfmt 与 `git diff --check` 通过。
- 边界：合成 Bar CLI 代理，不覆盖 Paper/日历/公司行为/funding/Testnet/多 venue；全工作区测试通过不等于 Rust release gate、完整容量画像或真实供应商验收完成。

### 精确权益报告 reader 与 Rust 静态账本修复（2026-10-27，未发布工作区）

CLI equity reader 现在支持权益 CSV schema v2/v5 的 `equity_exact` 和多腿组合 schema v4，校验精确十进制值向零投影到 1e-9 后与兼容 `equity_raw` 一致，并复核组合腿和与声明权益一致。多市场 research report 现在绑定、复核并输出 exact equity/contribution sidecar；Bar 与 Depth EvidenceBundle 均携带 exact equity，Depth schema v2-v4 统一按事件流读取。多账户联合便携 EvidenceBundle 也映射子报告 `equity_exact_path`，支持来源目录删除后的离线复核和复算。raw-only EventReport v1 工件仍走其兼容校验器。SchemaRegistry 增补登记文件已使用的 `parallel-version` 兼容值，并同步 Rust 枚举和 JSON Schema。CCXT 重连门禁覆盖 ticker、order book、OHLCV 三条预算通道；pipeline 打开站点账本补记了策略调度行情摘要及行情/账户日志两种读取分支，文档同步说明豁免原因。

- 验收：CLI HTML 报告套件 **22/22**；多市场/多腿/EvidenceBundle 集成套件 **16/16**；exact equity 投影与多腿腿合计回归 **1/1**；pipeline 打开站点账本 **2/2**；CCXT 三通道重连预算门禁 **1/1**；多市场 raw EventReport 兼容校验器 **1/1**；schema-registry CLI 回放 **1/1**；多账户联合便携证据搬迁与复算 **1/1**；qx-cli 单元测试 **452/452**；`cargo test --workspace --quiet` **通过**。唯一忽略项是需 C++ 共享库 fixture 的 SDK CI 测试。
- 边界：完整本地代码/集成回归通过；E2B、Microsandbox、OpenSandbox 真实供应商隔离/容量与费用验收仍开放，尚未发布。

### 本地标准类策略研究与 HTML 报告打通（2026-10-27，未发布工作区）

修复 Python 安装包本地研究链路中的三处断点：原生扩展公开 Quick Research 用例入口；HTML 报告 reader 按 RunRecord 中声明的成交产物版本校验，并对 fills v2 强制要求精确数量、价格和费用列；候选偏差诊断 worker 现在接受 `STRATEGY_CLASS(BaseStrategy)`，并为每个决策时点提供受 `as_of` 限制的 `StrategyDataPortal`。因此，标准类策略可在本地无沙箱执行参数研究、候选未来数据/预热诊断并生成含逐候选结果的交互报告。

- 验收：新建 Windows x64 CPython 3.13 虚拟环境离线安装 wheel；`doctor` 显示 native extension 与应用用例均可用；安装后的 `qianxing research` 对 `STRATEGY_CLASS` 样例运行 6 个参数候选，全部成功，候选诊断 `complete`、6/6、0 失败，生成 381,495-byte HTML 报告。完整 `python/tests` **529 通过、10 跳过、0 失败**；诊断/HTML 定向套件 **32/32**。机器记录：`maturity/evidence/python-wheel-local-research-smoke-2026-10-27.json`。
- 边界：wheel 证据仅覆盖 Windows x64 / CPython 3.13 和仓库 Bar 样例数据；不表示策略收益有效，也不证明其他 OS/Python wheel、PyPI 发布或远程供应商 sandbox。E2B/Microsandbox/OpenSandbox 真实隔离、资源、成本和容量验收仍开放。

### Microsandbox 批量远程研究入口（2026-10-27，未发布工作区）

`remote-research` 除默认 E2B 和 OpenSandbox 外，新增显式 Microsandbox provider，支持 local/cloud backend、digest-pinned OCI 镜像、CPU/内存/超时限制、guest 无网络、宿主侧 API key/profile、输入/结果指纹审计及结果归档校验。同进程 Microsandbox 批任务串行化，避免 SDK 进程级 backend scope 并发串用；研究执行在同步 CLI/worker 中运行，异步服务需提交给独立 worker 进程。provider 失败会清理沙箱并返回错误，不回退到 E2B 或本地执行。同步更新多语言策略指南与架构方案。

- 验收：Microsandbox batch provider、sandbox audit 与策略文件 CLI 定向套件 **65/65**；完整 `python/tests` **526 通过、10 跳过、0 失败**；CLI `remote-research --help` 展示 provider/backend 选项；相关 Python 文件 `py_compile` 通过。
- 边界：使用 fake SDK；没有连接 Microsandbox Cloud 或目标服务器 microVM。服务端镜像/资源/网络执行、容量/费用及外部隔离尚未验收；未发布。

### Runtime `on_stop` 终态按持久策略会话触发（2026-10-27，未发布工作区）

明确 Runtime 策略会话与调度 JobRun 的边界：一个持久 Python 策略跨多个 durable JobRun 继续运行；每条 JobRun 是事件批次，不是新的启动/结束周期。Runtime clean EOF 触发一次 `on_stop(..., reason="shutdown")`，worker 级帧/流异常以 `failed` 结束；单请求解码错误返回请求错误，已解码策略回调错误调用 `on_error` 并回滚，都不结束整个会话。有限回测的 `completed/cancelled/failed` 终态合同保持不变。新增回归校验两个 JobRun 只触发一次 `on_start`，最后 EOF 只触发一次 `on_stop` 且传入最后请求上下文。

- 验收：`python -m unittest tests.test_strategy_process_adapter` **23/23**；Python `py_compile` 和 evidence JSON 解析通过。
- 边界：没有新增可在 Strategy worker 继续存活时远程停止单个策略的持久控制命令；这需要先定义未完成 JobRun 的终态/恢复语义。远程 sandbox 真实供应商、隔离和容量成本仍未验收。

### Runtime `STRATEGY_CLASS` 支持无参构造并对齐本地入口（2026-10-27，未发布工作区）

持久 Python class worker 此前只调用 `STRATEGY_CLASS(normalized_spec)`，与本地 `run-strategy` 的无参类构造能力不一致。现在 Runtime class worker 与本地 CLI 一样，支持 `STRATEGY_CLASS()` 或接收完整规范化 spec 的类；`create_strategy(spec)` 仍按显式工厂合同接收 spec，rollback 重建也复用同一构造路径。另更正 Python README 对 Runtime OrderBook 输入仍未接通的过时说明：现在由 durable Runtime EventLog 重放并通过 checkpoint/cursor 去重，精确盘口 sidecar 保留。

- 验收：`python -m unittest tests.test_strategy_process_adapter` **22/22**；改动 Python 文件 `py_compile`、相关文件 `git diff --check` 通过。
- 边界：仅是本地无参类构造与文档/Runtime worker parity；provider 真实服务验收、sandbox 越权/容量成本与交易所实盘验收仍未完成，未发布。

### Rust/C ABI 停止回调补齐默认 host_stop 与动态插件 shutdown（2026-10-27，未发布工作区）

`CAbiStrategy::on_stop()` 现在通过版本化 v2 回调发送 `host_stop`，没有 v2 导出时仍回退原 v1 callback；动态库包装器在无参 `on_stop()` 路径发送 `shutdown`，显式终态调用仍保留调用方 reason。Python Runtime worker 的干净 EOF 传 `shutdown`，管道/协议错误传 `failed`，直接关闭仍默认 `host_stop`。Rust 与 Python callback 现在共同消费策略停止上下文共享金样。没有改动 C ABI vtable 布局。架构门禁中 CCXT 市场轮询重连计数同步为三个真实通道（ticker、orderbook、OHLCV）。

- 验收：Rust 停止 reason 用例 **1/1**；`cargo test -p qx-strategy --lib -- --test-threads=1` **71/71**，Rust stop-context golden **2/2**；Python 消费 Rust 共享金样 **1/1**，`python.tests.test_strategy_process_adapter` **21/21**，覆盖干净 EOF、stdin 故障及直接关闭 reason；Python 编译通过；架构门禁四分片覆盖 **228/228**；相关文件 `git diff --check` 通过。
- 边界：Runtime worker 仍不能收到单次策略运行的 `completed/cancelled` 终态，只能判断进程 `shutdown/failed`；远程沙箱供应商与实盘交易未验收，未发布。全工作区 rustfmt 检查仍显示工作树其他模块的格式差异。

### Python `BaseStrategy` 本地接入与沙箱边界指南（2026-10-27，未发布工作区）

更新多语言策略指南，补充经典 Python 策略类的生命周期/事件 hook、Backtrader 风格 Bar 预热入口、本地回测和样本外研究命令，以及服务器显式启用 E2B、Microsandbox 或 OpenSandbox 的边界。明确本地可信策略默认直跑；策略只产生 intent，Rust 宿主持有 Gateway、风控、OMS、执行和账本；`llm-sandbox` 当前未集成。README 增加该指南入口。

- 核对：文档命令与当前 `qianxing` CLI 参数定义、策略样例路径和方案中的执行模式说明逐项比对；本次仅更新文档，没有新增或运行测试。
- 边界：没有改变策略运行时；远程 provider 的真实服务验收、隔离强度、容量/成本与发布门仍未完成。

### 历史资金费按交易所结算时间和逐事件周期验收（2026-10-27，未发布工作区）

历史 FundingRateEvent 不再要求 settlement_ts 落在 UTC 8 小时网格；该时间作为交易所实际结算时间，并可逐事件声明 funding_interval_ms（60 秒至 24 小时）限制标记价最大年龄，缺省使用 8 小时。Python SDK 另拒绝 bool、浮点数和字符串等非整数类型。常量资金费率保留默认 UTC 8 小时调度。MultiLeg Provider 兼容旧 funding_8h 查询，并接受通用 funding；payload v1 保持 rate_bps 旧格式，payload v2 要求 funding_interval_ms。Rust/Python typed event、输入/产物 Schema、schema registry v14 和多腿报告事件摘要同步对齐。

- 验收：core 周期边界与 JSON 类型拒绝 **2/2**；qx-xingban --lib funding **23/23**，含非 8 小时对齐的历史事件和 1 小时标记价过期拒绝；Provider v1/v2 到多腿运行产物 **1/1**；qx-app --test depth_backtest **10/10**、multi_market_depth_backtest **13/13**、multi_leg_backtest **12/12**；Python/Rust schema 与共享应用入口 **2/2**；相关 Rust crates cargo check --tests、Python py_compile、JSON 证据解析通过。完整记录见 `maturity/evidence/funding-event-intervals-2026-10-27.json`。
- 边界：没有自动拉取或推导各 venue 的 funding interval，也未与 Binance/OKX/Bitget 历史账单逐笔对拍；provider v2 周期由来源提供。没有真实供应商或实盘验收，未发布。

### 深度合约回测资金费率结算贯通（2026-10-27，未发布工作区）

单市场 L1/L2 与共享多市场深度回测现在接受默认 UTC 8 小时常量资金费率，或按交易所实际结算时间提供的历史费率；常量与历史序列不能混用，费率标的、范围、重复事件和标记价过期都会在错误使用时拒绝。历史事件可以逐条带入资金周期以限制标记价最大年龄。L1 Tick 复用同一账簿结算逻辑。资金费按精确持仓、精确标记价和产品合约乘数计算，以带标的、数量、价格和现金流的 exact Ledger/EventLog 事实记录；费用进入净值并参与回放哈希。单市场指标与多市场研究报告展示资金费假设/结算现金流；深度输入 schema 同步接受资金费字段，并允许多市场衍生品规格。

- 验收：精确子纳米/反向/正负方向资金费 **1/1**；`qx-xingban` funding 过滤回归 **23/23**（含历史事件、UTC 常量费率、共享多市场双标的 exact 结算、过期标记 fail-closed、Ledger/EventLog 重放）；`qx-app --test depth_backtest` **10/10**；`qx-app --test multi_market_depth_backtest` **13/13**。`cargo check -p qx-core -p qx-xingban -p qx-app --tests`、改动 Rust 文件 rustfmt、`git diff --check` 和三份深度 JSON Schema 解析通过。
- 边界：上述定向离线测试已通过；未做外部交易所资金账单逐笔对拍，也未验证不同交易所的实际结算日历/特殊产品例外；未发布。只有行情/费率事件覆盖到结算点且标记价未过期时才入账。

### Python 本地回测 callback 接收版本化终态与最终上下文（2026-10-27，未发布工作区）

`qx-python` 的 `PythonBacktestStrategy` 现在把 `StrategyStopContextV1` 传给本地 Python callback；`_PythonBacktestCallback` 校验 schema/reason，并向 `BaseStrategy.on_stop(context, reason)` 提供最终已提交上下文和 `completed`、`cancelled`、`failed` 等终态。旧 `_qx_on_stop()` 仍保持 `host_stop`，Rust 适配器遇到缺少新 contextual 方法的 callback 会回退旧入口。未完成预热时仍在调用停止钩子后明确报错。

- 验收：Python callback/策略适配套件 **49/49**；Rust/PyO3 stop-context 桥接用例 **1/1**；Rust 格式、Python 编译和 `git diff --check` 通过。
- 边界：这关闭的是本地 Bar/Tick/L2 回测到 Python `BaseStrategy` 的终态透传；Runtime worker EOF 与服务 shutdown 仍用通用 `host_stop`，远程供应商和实盘服务未验收，未发布。

### BaseStrategy 本地与 Runtime 回调统一 on_error 失败生命周期（2026-10-27，未发布工作区）

本地 Rust→Python callback 和 Runtime Python class worker 在有效请求执行或结果校验失败时，都会先调用 `BaseStrategy.on_error(context, error)`；本地 callback 再把原异常返回 Rust，Runtime worker 则回滚本次策略状态。错误上下文只使用当前请求的 as-of 快照，不带未来历史。`on_error` 自身抛错会写入 stderr 诊断，但原始失败仍保留，工厂策略仍按原合同重建以支持重试。默认实现为空，旧策略无需修改。停止 reason 的逐运行传输仍需后续接入 host shutdown 协议。

- 验收：`test_strategy_base`、`test_strategy_process_adapter`、`test_strategy_contract` **81/81**；Python `py_compile` 与改动文件 `git diff --check` 通过。
- 边界：`on_error` 目前覆盖 BaseStrategy 类在本地 callback 和 Runtime worker 的路径；模块函数型策略尚无统一错误 hook。没有远程沙箱或交易所真实服务验收，未发布。

### Bar/Tick/L2 回测终态 reason 贯通到策略（2026-10-27，未发布工作区）

Bar、Tick 和 OrderBook strategy trait 新增有默认实现的 `on_stop_with_reason`，旧实现继续只需无参 `on_stop()`。原生适配器把终态 reason 与最终 `StrategyContext` 封装为 `StrategyStopContextV1`；单市场、市场路由、共享账户组合、策略槽、L1 Tick adapter、L2/DepthBar 和共享订单簿组合均透传。成功报告/参数早停映射为 `completed`，取消返回映射为 `cancelled`，启动/执行错误映射为 `failed`。C ABI v2 可以收到具体回测终态，旧插件仍按原 v1 callback 运行。

- 验收：`cargo test -p qx-app --lib` **161/161**；`cargo test -p qx-xingban --lib` **211/211**；生命周期 helper 状态分类 **2/2**，原生 Bar adapter、market router、组合策略、Tick adapter 和 L2 DepthBar 的 reason/context 透传覆盖通过。定向 lifecycle 文件格式检查通过。
- 边界：Runtime worker shutdown 与其它服务停止路径仍传通用 `host_stop`；真实沙箱/交易所验收不在本轮。全 workspace rustfmt 检查仍被工作区其它已有格式差异挡住；离线实现，未发布。

### Runtime BaseStrategy 持久 OrderBook 输入接通（2026-10-27，未发布工作区）

Python Runtime class worker 现在可从 durable Runtime EventLog 接收并重放 L2 OrderBook；Strategy input schema v8 与 `BaseStrategy.on_order_book`、四种 worker 传输及 exact price/quantity sidecar 对齐。策略 checkpoint 保存用户 JSON 状态、事件去重游标、预热状态和行情 cursor，并在同一 Runtime 快照提交。CCXT OrderBook 策略不再因残留参考标的配置额外调度 OHLCV 读取；实时输入仍为 REST 快照轮询，不是 WebSocket 增量流。

- 验收：`cargo test -p qx-runtime --features sqlite` 共 **140** 项通过；OrderBook Runtime 输入 schema v8 JSON shape/精度回归通过；CCXT OrderBook 行情 worker 回归 **1/1**；Python Strategy Contract/Process Adapter/沙箱 worker 组合 **101/101**，策略 manifest 与本地 CLI 文件入口 **50/50**。
- 边界：这些是本地离线和 fake SDK 验收，不代表 E2B、Microsandbox 或 OpenSandbox 的真实服务隔离/容量/网络/费用验收；Runtime worker 仍为 at-least-once，不承诺 exactly-once；未发布。

### BaseStrategy 停止回调支持上下文并保留旧签名（2026-10-27，未发布工作区）

Python `BaseStrategy.on_stop` 可声明 `context` 与 `reason`，本地 callback 与 Runtime class worker 在停止时传递最后已提交的只读上下文和 `reason="host_stop"`。旧策略覆盖 `on_stop(self)` 仍能运行。

- 验收：策略 Contract、Runtime Process Adapter、BaseStrategy callback、manifest 与本地 CLI 文件入口 `python -m unittest tests.test_strategy_contract tests.test_strategy_process_adapter tests.test_strategy_base tests.test_strategy_manifest tests.test_cli_strategy_file` **128/128**；覆盖 contextual stop 和既有无参 stop 兼容。
- 边界：当前 Python host 仍传通用 `host_stop`，worker 异常/取消尚未映射成不同 stop reason，且没有统一 `on_error` 生命周期；本地离线实现，未发布。

### Rust/C ABI 策略停止上下文 v1 与兼容 v2 callback（2026-10-27，未发布工作区）

Rust `Strategy` 新增 `StrategyStopContextV1`、标准 reason enum 和兼容默认 hook；旧策略仍只需实现无参 `on_stop()`。C ABI 新增可选 `qx_strategy_on_stop_v2(handle, context_json, error, error_len)` 导出，不改变旧 vtable 布局；动态宿主优先调用 v2，未导出时回退 v1。C++ 示例插件消费版本化 context/reason，Rust Bar/Tick/OrderBook 策略 adapter 把最终只读 context 传到 contextual hook。共享 schema/fixture 登记为 stop-context v1。后续 Bar 与 Tick/L2 backtest engine 已把 completed/cancelled/failed 传到策略；Runtime worker shutdown 和其它服务 wrapper 仍使用通用 `host_stop`。

- 验收：`cargo test -p qx-strategy --lib` **70/70**；Rust stop-context golden **2/2**；Rust 原生动态 C ABI stop fallback **1/1**；Windows C++ CTest **3/3**；Rust 宿主加载 C++ 插件并消费 v2 stop context **1/1**；`cargo check -p qx-xingban --tests` 通过。v8 OrderBook 测试修正为真正的升序买盘反例后，盘口严格降序验证符合预期。
- 边界：保留旧 vtable、Rust `on_stop()` 和无 v2 导出插件兼容；Runtime worker shutdown、所有 app/wrapper 生命周期与 `on_error` 尚未统一；本机离线/动态插件验收，不代表远程策略沙箱或交易所真实服务验收，未发布。

### `remote-research` 批任务纳入统一沙箱审计（2026-10-27，未发布工作区）

E2B 与 OpenSandbox 批量研究 provider 现在使用 `python-strategy-sandbox-audit-v1` 写入宿主 JSONL：记录 sandbox 创建、就绪、策略任务输出摘要与销毁；审计绑定策略入口/依赖包/spec/data 摘要、job/provider/runtime、资源请求和网络策略。审计目录默认位于用户目录 `.qianxing/strategy-sandbox-audit/`，可通过 `QX_REMOTE_RESEARCH_AUDIT_DIR` 更换；本地路径不会上传到 guest。审计文件无法创建时会在调用 provider 之前停止。E2B 批任务要求 `QX_E2B_EXPECTED_CPU_COUNT` 与 `QX_E2B_EXPECTED_MEMORY_MB`，创建后读取资源并与声明值比较，不一致则先清理再拒绝上传策略包。

- 验收：`python/tests/test_sandbox_execution.py` 与 `test_sandbox_audit.py` **28/28**；覆盖成功/失败生命周期、E2B 模板资源配置与不匹配、审计不可写失败关闭，以及本地审计文件 schema/hash-chain。策略/沙箱组合回归 **143/143**；架构 `--fast` 四分片覆盖 **228/228**。
- 边界：OpenSandbox 写入的是发给服务端的 CPU/内存请求值，尚未证明连接服务确实执行；真实供应商隔离、资源、容量/成本与断线验收仍未完成。

### 持久策略沙箱统一审计记录 v1（2026-10-27，未发布工作区）

Rust 持久 `strategy-worker` 和 Bar `ContractBarStrategy` 在 E2B、Microsandbox 或 OpenSandbox 被部署策略选中时，为宿主执行创建独立审计 JSONL。三个 provider bridge 共用 schema v1，记录 sandbox 创建/就绪/失败/销毁和每个成功请求的宿主 worker/策略身份、数据指纹、响应摘要、策略入口/传输包/spec 摘要、Qianxing 版本、runtime 引用、CPU/内存/超时/租约限额及 deny-all 网络策略。记录由宿主路径保存，不随请求传入 guest，也不写入 API key 或原始策略载荷。每文件带序号和前序 SHA-256 链；强制审计路径不可写时在创建/继续执行前失败关闭。Schema 已加入 registry v12。

- 验收：Python 策略类、进程适配器、三家 worker、批研究审计和诊断组合 **143/143**；Rust CLI 审计路径单测 **1/1**、CLI `cargo check` 通过；Schema/registry JSON 解析、Python compileall、改动 Rust 文件格式检查、`git diff --check` 和架构 `--fast`（4 分片、228 项）通过。
- 边界：哈希链不是签名或防篡改存储；批研究补充实现见上条；真实服务隔离、资源应用、断线恢复与容量/成本仍未验收，未发布。

### Quick Research 默认要求样本外复核（2026-10-27，未发布工作区）

本地 `qianxing research` 和远程 `remote-research` 快捷流程都经 `qx-app::prepare_quick_research_spec_v1` 校验并规范化，显式要求 `split_plan`；没有测试窗时会在加载策略或运行搜索前拒绝，避免把样本内排名当作有效性结论。单市场未声明基准时自动添加 `buy-and-hold`；多市场继续使用严格 schema 和 OOS 折叠，但暂不支持跨市场基准。诊断默认最多覆盖 16 个已物化候选，其他候选在报告中保持未验证状态，仍可通过参数调整上限。一般 `search-strategy` 保留原有样本内研究能力。

- 验收：`qx-app --test quick_research` **4/4**；快捷研究两个回归文件 **35/35**；本地策略类/进程适配/策略文件与三种 sandbox worker 定向组合 **137/137**；`qx-python` 编译检查、Python 编译检查、架构 `--fast`（4 分片、228 项）通过；完整边界记录在 `maturity/evidence/quick-research-gate-2026-10-27.json`。
- 边界：单市场快捷研究会默认增加 `buy-and-hold`；严格的多市场实验 schema 目前不含跨市场基准，只提供 OOS 折叠结果，不能称为基准相对有效性结论。候选诊断仍由 Python 策略隔离回放生成；`qx-app` 继续拥有候选回测、OOS 和基准事实。本次未进行真实远程沙箱或交易所验收，也未发布。

### 费用与 A 股参考价算术溢出改为显式失败（2026-10-27，未发布工作区）

核心手续费名义额和基点金额不再通过饱和乘法生成一个看似有效的金额；手续费模型校验负费率、非法输入和溢出。单账户/多账户盘口报告的 raw 手续费与成交额、跨交易所多腿归因汇总及成本 bps 缩放改为 checked 累计。A 股除权除息现金流、股份比例、配股现金和 tick 舍入全程 checked；单点与批量昨收查询将错误传到回测，异常参考价不会继续进入成交判断。

- 验收：qx-core fee tests **10/10**；qx-xingban 与 qx-zhenlu library tests **206/206 + 58/58**；A 股批量昨收集成测试 **1/1**；Paper accounting contract **4/4**；相关 crates check、qx-xingban benches check、Rust 格式检查和架构 `--fast`（4 分片、228 项）通过。详细命令与边界记录在 `maturity/evidence/checked-financial-arithmetic-2026-10-27.json`。
- API：`FeeModel::commission`/`commission_for_side` 改为返回 `QxResult`；A 股 `previous_close` 与 `previous_close_series` 改为可返回计算错误。下游 Rust 实现需适配新签名，发布前需按兼容性策略升版。
- 边界：这是定向代码验收，不是整工作区发布门禁；未完成真实供应商验收、完整发行矩阵或发布。

### 远程策略沙箱显式限额与不可变镜像引用（2026-10-27，未发布工作区）

OpenSandbox 持久 Python strategy worker 与 `remote-research --provider opensandbox` 现在都会向 sandbox create 请求提交 CPU、内存上限，并将请求值附到 `qianxing_resource_cpu` / `qianxing_resource_memory_mib` metadata。策略 worker 可通过 `QX_OPENSANDBOX_CPUS`、`QX_OPENSANDBOX_MEMORY_MIB` 配置；批研究命令支持相同环境变量或 `--cpus` / `--memory-mib`。OpenSandbox 默认 1 CPU / 2048 MiB，Microsandbox 默认 1 CPU / 1024 MiB；二者都允许 1–64 CPU、512–65536 MiB，且超界值会在接入或创建 provider 前拒绝。OpenSandbox 与 Microsandbox 远程 worker 都拒绝可变镜像 tag，要求 `repository@sha256:<64-hex-digest>`。该校验固定镜像内容引用，不验证镜像签名或供应链来源。

- 验收：`python -m pytest -q python/tests/test_opensandbox_strategy_worker.py python/tests/test_microsandbox_strategy_worker.py python/tests/test_sandbox_execution.py` **38/38**；`cargo check -p qx-cli -j 1` 通过；`git diff --check` 与相关 Python compileall 通过。机器证据见 `maturity/evidence/opensandbox-resource-limits-2026-10-27.json`。
- 边界：离线 fake SDK 验收只证明牵星提交限额并校验镜像引用，不证明真实服务/runtime 执行资源、网络和镜像策略。没有真实服务、隔离、容量、费用验收，未发布；W9 的 provider-neutral 生命周期和策略执行审计仍待完成。

### 提交订单号绑定完整订单身份（2026-10-27，未发布工作区）

订单恢复与幂等判断现在共用 `Order::same_submission_identity`：账户、标的、方向、数量、价格、策略/信号追踪、执行策略及精确数量/报价预算/市价风险参考价必须与最初提交完全一致。生命周期状态和累计成交量不参与身份比较，因此相同订单恢复时仍可幂等识别；复用 `client_order_id` 改变 legacy 或 exact 请求会在触达 Venue 前拒绝。算法子单恢复与多腿补偿也复用该比较，补齐算法子单此前未比较精确金额的缺口。

- 验收：`cargo test -p qx-execution --lib` **39/39**；`cargo test -p contract-tests --test recovery_and_replay -- --test-threads=1` **3/3**。Windows 默认临时目录曾导致文件锁权限错误；将 `TEMP`/`TMP` 指向 `target/test-temp` 后恢复用例通过。
- 边界：本地代码及模拟 Venue/文件 EventLog 验收；未连接真实 Binance、OKX 或 Bitget，也未完成跨节点并发/外部网络断连验收；未发布。

### MultiLeg 双腿回测精确初始本金 v2 与实验、账簿和报告读回贯通（2026-10-27，未发布工作区）

`MultiLegBacktestSpec` 保留 v1 两腿 raw 本金行为，新增 v2 `primary_initial_cash_exact` / `reference_initial_cash_exact`。两腿 raw 字段分别是 exact principal 向零投影到 1e-9 的兼容值；共享账户初始资金用 exact 求和后入账，允许正本金的单腿乃至组合本金投影为 raw 0。精确本金同步传给策略启动的 exact cash/margin 快照，逐腿收益用对应 exact 本金作分母。

多腿 artifact v6 写入组合与逐腿 exact 本金，报告 reader 将 v6 与 v2 spec 绑定，重放 EventLog/账簿并验证 exact equity sidecar、raw 投影、组合收益、逐腿收益及共享账户摘要。交互研究报告和控制台账户指标也透传精确期初本金。新增 v2 输入和 v6 artifact JSON Schema；多腿参数实验 base 接受 v1/v2；Python SDK 增加 `.with_initial_cash_exact()` 十进制构造器。v1 的策略上下文、入账和 artifact 形状继续沿旧路径运行。

- 验收：`qx-app --test multi_leg_backtest` **12/12**；`qx-app --test multi_leg_experiment` **9/9**；`qx-app --test spec_contract` **17/17**；Python SDK/schema layering **39/39**；MultiLeg v1/v2 与实验 schema 通过 Draft 2020-12 校验。端到端 exact 用例覆盖双腿各 `0.0000000002`、合计 `0.0000000004`、raw 投影 0，以及实验候选保存 v6 artifact 后的报告读回。
- 边界：未发布本地离线实现；尚未完成全工作区发布矩阵、真实 sandbox/provider 验收和其他 W1 开放项。

### MultiMarket Depth 精确初始本金 v2 贯通共享账簿与报告读回（2026-10-27，未发布工作区）

多市场盘口回测新增 v2 `initial_cash_exact`，保留 v1 raw 行为；exact 金额经 Ledger 精确入金、共享现金/保证金估值、启动策略上下文和报告指标。v2 raw 权益曲线逐点从 exact 曲线向零投影，报告 reader 复核投影、初值、收益和指标。schema registry 新增 v2，深度实验输入接受 v1/v2，Python DTO 也校验精确本金与 raw 投影。

回归证明 `1.3e-9` 本金可以容纳两笔总额 `1.2e-9` 的 quote-budget 买入，并报告精确成交额和费用；`0.5e-9` 正本金可在 raw 投影为零时完成运行与报告读回。v1 原有入金/估值路径保持不变。

- 验收：`qx-xingban --lib` **200/200**；多市场深度集成 **13/13**；qx-app 深度报告/身份单测 **3/3**；Python SDK/schema 定向合同 **1/1**；qx-app/qx-sdk、qx-api、qx-cli 测试目标 `--no-run` 编译通过；三份深度 JSON Schema 通过 Draft 2020-12 校验；架构 fast 四片 **228/228**、无重复（109.76 秒）；`git diff --check` 通过。机器证据：`maturity/evidence/multi-market-depth-exact-principal-2026-10-27.json`。
- 边界：MultiLeg exact 本金、跨计价币种、全量 Python/发行矩阵、资金费率深度模型、恢复/便携导出及真实沙箱/容量验收仍开放；此为未发布离线代码验收。

### MultiMarket Bar 精确初始本金 v2 贯通并修复 walk-forward 策略快照断链（2026-10-27，未发布工作区）

`MultiMarketBacktestSpec` 新增 v2 `initial_cash_exact`，保留 v1 raw 合同；精确本金向零投影到 1e-9 的 `initial_cash_raw`，正本金可投影为 0。精确本金贯通策略额度汇总、共享账簿入金/回放、权益与收益指标、逐标的贡献和报告 reader；schema registry 新增 MultiMarket spec v2，参数实验 base 接受 v1/v2，Python DTO 同步支持。

回归覆盖 `100000.0000000005` 本金、exact 预算校验、artifact 和 exact equity sidecar 读回复核，以及 `0.0000000005` raw 投影为零的实际回测。测试扩展期间还发现 `TradeStartGate` 丢失 exact cash/margin 更新，造成 walk-forward 候选策略上下文前后不一致；现已转发完整精确快照。

- 验收：`qx-xingban --lib` **200/200**；多市场 Bar **10/10**；多市场参数/自适应/Walk-forward **9/9**；qx-app multi-market 报告与身份 **17/17**；多账户联合回测 **4/4**；Python multi-market DTO/schema 与实验 schema 定向检查通过；qx-app/qx-xingban/qx-sdk 测试目标 `--no-run` 编译通过；架构 fast 四片 **228/228**、无重复（113.14 秒）。全量 Python 套件及发行矩阵未运行，工作区全量 `cargo fmt --check` 受已有格式差异影响未通过。本地机器可读证据：`maturity/evidence/multi-market-exact-principal-2026-10-27.json`。
- 边界：只关闭 MultiMarket Bar 的本金精度链。MultiMarket Depth、MultiLeg 本金、跨计价币种预算、恢复/通用导出、完整跨语言 golden、真实 sandbox 与容量验收仍未关闭；W1 和发布门继续开放。

### 组合策略预算保留子纳米精度并修复 Depth 策略账户快照同步（2026-10-27，未发布工作区）

单市场组合主策略/附属策略与多市场策略槽现在可传 `reserved_capital_exact`，raw 字段必须等于精确值向零投影到 1e-9 后的结果。配置校验以 exact 金额求和，精确初始资金存在时与 `initial_cash_exact` 比较；Bar 和 L1/L2 订单准入、成交价复核、活动挂单/持仓敞口都使用精确预算。Python SDK、16 份公开 JSON Schema 和 provider 嵌套 schema 同步支持该字段，未填写时继续解释为原 1e-9 raw 数值。

定向 Rust 回归发现 DepthBarStrategy 初始化时收到按策略额度构造的 exact cash，随后账本更新只替换了 raw cash，导致同账户全额现金与旧 exact 余额错配；现已让策略适配器同步 exact cash 与 exact 可用保证金。验收：`qx-xingban --lib` **200/200**；组合规格、Bar portfolio、多市场、Depth 回测及 sizing 集成测试 **25/25**；新增 exact 预算单测已包含在前述 Rust 结果中；Python SDK/schema layering **39/39**；qx-app、qx-api、qx-sdk 所有测试目标 `--no-run` 编译通过，qx-app/qx-xingban 离线检查通过；架构 fast 四片 **228/228**、无重复（109.15 秒）。

- 边界：MultiMarket 账户初始本金目前仍是 raw 配置；跨计价币种预算折算、持久化恢复/通用导出、全语言 golden、供应商 sandbox 和完整容量验收仍开放。本项为本地离线验收，未发布。

### 本地策略支持 Rust ExactDecimal 持仓并完成 Windows wheel 最短旅程（2026-10-27，未发布工作区）

安装包中的 Python `BaseStrategy.position_qty/close_position` 之前只识别 Runtime worker 的十进制字符串；Rust 本地 callback 将非零 exact position 按 `{"coefficient": ..., "scale": ...}` 序列化后，用户策略持仓一旦非零便失败。本地 helper 现在同时接受两种规范 ExactDecimal 表达，拒绝浮点和其他模糊类型，并有子纳米精度 close-position 回归。修正 Python README 中示例运行目录后，使用 CPython 3.13 的干净虚拟环境离线安装 Windows abi3 wheel，运行安装包自带 CLI 完成 `BaseStrategy` 策略文件回测。

- 验收：`python/tests/test_strategy_base.py` **28/28**；wheel `qianxing-0.2.2-cp310-abi3-win_amd64.whl` 构建并在全新 venv 用 `--no-index --no-deps` 安装；`qianxing doctor` 报告 native extension 和应用用例可用；安装后的 `qianxing run-strategy` 完成 70 Bar、1 笔成交，生成 summary/manifest/equity/fills/event trace，run hash `364e0f7aaccf9904`。wheel SHA-256 与 summary SHA-256 见 `maturity/evidence/python-wheel-local-strategy-smoke-2026-10-27.json`。
- 边界：这是当前 Windows x64 / CPython 3.13 单环境的本地包验收，不代表其他 OS/Python 版本发行矩阵、PyPI 发布、可选交易所/沙箱 extras 或真实供应商验收已经完成。

### Rust/C ABI/C++ 策略生命周期补齐预热就绪回调（2026-10-26，未发布工作区）

C ABI/C++ 动态策略可选导出 `qx_strategy_history_requirements_v1` 声明 Bar 预热数量，并导出 `qx_strategy_on_warmup_complete_v1` 接收版本化就绪事件。Rust 动态宿主将两者适配到统一 `Strategy` trait；缺少声明的旧插件默认零预热，缺少就绪回调则继续运行，`qx_strategy_vtable` 布局不变。新增跨语言共享生命周期 fixture，核对 init/start、预热、就绪通知、首个可决策 Bar 和 stop 的次序，并在 C ABI 头/Rust `repr(C)`/JSON Schema/registry 中登记合同。

- 验收：Rust `qx-strategy` 单元 **68/68**（工作区临时目录）；Rust 宿主真实加载 Windows C++ 示例 DLL **1/1**；MSVC Release 构建成功，C++ ABI CTest **1/1**；Python strategy process adapter **17/17**。
- 边界：本机动态插件与共享 fixture 验证 ABI 映射，不代表第三方旧插件、独立 JSONL worker 或远端沙箱经过验收；C++ native 插件仍须按可信库策略校验并运行在合适信任边界内。

### E2B Python 策略 worker 校验沙箱实际资源上限（2026-10-26，未发布工作区）

E2B 持久策略 worker 现在要求宿主设置正整数 `QX_E2B_EXPECTED_CPU_COUNT` 与 `QX_E2B_EXPECTED_MEMORY_MB`，并在上传策略文件和启动进程前读取 E2B sandbox info，核对实际 CPU 与内存。资源字段缺失或与宿主策略不符时立即停止 sandbox；模板选择仍由服务器宿主配置控制。E2B 模板的 CPU/内存需在模板构建时确定，README 示例明确要求部署配置与模板一致。

- 验收：增加匹配资源、CPU/内存不匹配、供应商未报告资源、缺少/非法资源门控的 worker 用例；fake SDK 验证失败路径在上传策略或启动进程前清理 sandbox。
- 边界：这是代码侧资源门控，不是 E2B 真实账户/模板的隔离、配额、计费、网络或容量验收；样例的 2 CPU/2048 MB 仅供开发配置，不是生产规格。

### C ABI/C++ 策略事件 v3 保留精确盘口价量（2026-10-26，未发布工作区）

DepthFrame v2 的 Rust `MarketEvent::OrderBook` 现在通过可选 C ABI `qx_strategy_on_market_event_v3` 向动态 C/C++ 插件传递与原始盘口档位一一对应的 exact price/quantity coefficient+scale sidecar。旧 v1/v2 事件与 vtable 布局保持不变；插件缺少 v3 时，带精确盘口的事件会明确拒绝，不再静默按 1e-9 raw 投影运行。新增共享小币种盘口 fixture，由 Rust、Python `BaseStrategy`、Rust 动态宿主加载的 C++ 示例插件共同消费，并登记 `strategy-market-orderbook-v1` schema。

- 验收：Rust C ABI v3/legacy fail-closed **1/1**，Rust 市场事件 golden **2/2**，Python `test_strategy_base.py` **27/27**，Windows C++ ABI layout 合同程序通过，Rust 动态加载 C++ 插件并核对 scale 12 盘口价、scale 18 数量 **1/1**。
- 边界：这是策略事件精度跨语言传递，不代表每个 C++ 策略都已经使用精确值计算信号；完整跨语言 lifecycle/update golden matrix、远端沙箱真实供应商验收、发布包和生产实盘验收仍未关闭。

### 修复原生策略仅订阅账户快照时的更新收集断链（2026-10-26，未发布工作区）

策略 worker 判断原生 C ABI 策略是否需要扫描执行 EventLog 时，之前只看订单状态与成交回调；实现了账户/持仓快照 callback、但未实现前两类 callback 的插件不会启动更新收集，因此快照永远到不了插件。现在把 `supports_account_updates()` 纳入统一门槛；无更新 callback 的原生策略仍跳过该扫描，已有订单/成交 callback 的行为保持不变。

- 验收：新增 account-only 能力矩阵回归，覆盖 account-only、无 callback、仅订单状态、仅成交四种情形；另验证非 Python 宿主 EventLog 重放、C ABI 账户 callback，以及 v6 拒绝/v7 接受的输入版本门。定向回归合计 **7/7 通过**（qx-cli worker 3/3、EventLog 重放 1/1、qx-strategy C ABI 1/1、输入版本 2/2）。
- 边界：这是本地 EventLog→策略 worker 分发修复，不证明具体 C++ 插件、外部供应商或 Live 账户的端到端验收。

### 建立账户/持仓快照 Python、Rust、C ABI/C++ 共享金样（2026-10-26，未发布工作区）

新增统一的 `strategy-account-update-sequence-v1` fixture，覆盖合法的零起始账户 EventLog 序号、完整空余额快照和完整空持仓快照。Rust C ABI 单测、Python `BaseStrategy` Runtime class adapter 与 Rust 动态宿主加载的 C++ 示例 DLL 都消费同一事件文件；Python 验证两个快照都按期望次序到达，C++ 动态插件验证同一事实映射到可选账户更新 callback。该 fixture 已加入 `strategy-api-input-v7` schema registry。

- 验收：Python 策略 process adapter **17/17**；Rust C ABI 账户 callback **1/1**；Rust 动态加载 C++ 策略并消费同一账户/持仓金样 **1/1**。这些测试验证本机语言边界映射，不代替远端 worker 故障恢复或交易所账户流验收。

### Rust 与 C ABI/C++ 接入账户/持仓快照策略回调（2026-10-26，未发布工作区）

`StrategyAccountUpdateV1` 现在进入 Rust `Strategy::on_strategy_account_update`，回调前校验 schema、EventLog 序号、时间、账户和 venue。EventLog 序号允许从 0 开始。策略 worker 把账户快照与订单状态、fill 合并排序；`StrategyContractInput` v7 传给 Python 和外部进程，原生策略通过 typed callback 收到同一事实。C ABI 新增可选 `qx_strategy_on_account_update_v1` JSON 导出，动态宿主自动发现并调用，旧 vtable 布局不变；C++ 示例支持余额与持仓空快照。当前 worker 的快照读取 scope 为策略配置的单个 account/venue；多账户组合快照订阅仍需单独定义授权范围。账户快照只作为只读事实，不修改 Rust ledger。

- 验收：`cargo check -p qx-cli --tests` 通过；`cargo test -p qx-strategy --lib` **64/64**（将 `TEMP/TMP` 指向工作区可写临时目录后）；Rust 账户回调与账户/venue 拒绝用例 2/2、策略宿主合并顺序 2/2、非 Python host EventLog 重放 1/1；Visual Studio 2022 构建示例 DLL 和 C++ ABI 程序，CTest 3/3；Rust 加载该 DLL 并验收账户快照、持仓快照与诊断计数 1/1；架构 `--fast` 4 片 / 228 项全通过。
- 边界：旧 C ABI 插件没有可选 account callback 时保持兼容并忽略快照。完整 Rust/Python/C++ 全事件 golden matrix、多 worker 恢复与真实交易所验收仍未完成；本地策略默认无沙箱执行，远端 provider 需由部署配置显式启用。

### strategy-worker 增加宿主强制沙箱启动门（2026-10-26，未发布工作区）

新增 `QX_PYTHON_STRATEGY_SANDBOX_REQUIRED` 部署策略开关。未设置时，Python strategy-worker 仍默认本地直跑；启用后必须同时显式选择 `e2b`、`microsandbox` 或 `opensandbox`，provider 缺失、设为 `local` 或布尔值非法都会在 worker 启动/策略进程创建前拒绝。策略文件和运行 spec 不能覆盖该宿主决定，provider 故障仍失败关闭、不回退本机。Python README 的远端部署样例现在均显式启用该门。

- 验收：`cargo test -p qx-cli strategy_host::execution_policy --offline -j 1` **4/4 通过**；`cargo fmt --check -p qx-cli` 与架构 `--fast`（4 片、228 项、无重复）通过。
- 边界：本项只封住“服务器忘记设置 provider 却落入本地执行”的配置错误；不代表已完成 provider-neutral 生命周期/策略审计 schema、供应商真实服务、网络/凭据越权、容量/费用验收或实盘批准。

### 精确权益估值派发收敛与 schema 登记修复（2026-10-26，未发布工作区）

为避免子纳米回测与报告在调用侧自行挑选估值尺子，新增 `ExactValuationContext` / `Ledger::valuate_exact`，与 raw `Ledger::valuate` 分别成为唯一参数化派发点。Bar、共享多市场、OrderBook 组合回测和多市场报告复核都改走统一 exact 入口；精确标记价仍要求冻结的 market specs。架构快检的估值门禁扩为覆盖 raw/exact 两组原语，并修复遗留的 `backtest-equity-csv-v3` schema 未登记项。

- 验收：`cargo test -p qx-core --test valuation` 12/12；`cargo test -p qx-xingban --lib` 193/193；多市场 exact 报告复核 1/1；schema registry 路径检查通过。Rust 格式与差异空白检查通过。
- 边界：这轮统一估值选路和精确权益读点，不表示所有交易量、费用、资金预留与报告字段已全部贯通到 exact precision。

### Python BaseStrategy Timer 事件跨宿主金样（2026-10-26，未发布工作区）

新增 `strategy-market-timer-event-v1`，固定 tagged Python/Rust event `{ "Timer": { "name", "ts" } }`；Rust `MarketEvent` 与 Python `BaseStrategy.on_timer` 共读同一 JSON fixture。C ABI 回调验证映射后的 `kind/name/ts`，C++ SDK 合同测试覆盖对应的 enum/字段布局，并登记 schema registry。

- 验收：Rust 金样 1/1、C ABI 金样 1/1、Python `test_strategy_base.py` 26/26、Windows C++ contract CTest 1/1；C++ 示例 DLL 由 Rust 宿主加载并消费同一 fixture 1/1；schema、fixture 和 registry JSON 解析通过，相关 Rust 文件 rustfmt 与 `git diff --check` 通过。
- 验收增补：C++ 示例 DLL 已由 Rust 动态宿主加载并消费同一 Timer fixture，确认 request id、时间戳和空订单意图；`cpp_plugin_host` 的 ignored 用例在本机通过。
- 边界：该动态插件用例证明 C ABI 回调映射，不代替 E2B/Microsandbox/OpenSandbox provider 上的 Runtime worker 故障恢复、隔离和容量验收。

### Python Runtime BaseStrategy 接入 scheduler Timer hook（2026-10-26，未发布工作区）

Python `BaseStrategy` Runtime class worker 新增显式 `data_level="timer"`。实时策略 worker 按 `strategy.live_timeframe` 生成稳定时间桶指纹，每桶只排一个幂等 Strategy job；适配器使用该 job 的 `as_of` 派发一次 `on_timer(context, {"name": "scheduler", "ts": as_of})`，不依赖本机墙钟。跨桶滞后作业由现有 staleness gate 拒绝。Timer manifest 与 Runtime spec 必须匹配，且 `warmup_bars=0`。Bar 快照仍可通过 point-in-time data portal 读取；订单/成交和账户/持仓更新保持原先顺序并同一输出合并。

- 更新 Python strategy manifest v1 schema、Runtime 配置 `data_level` 校验和本地运行拒绝逻辑；本地 Bar/Tick/OrderBook backtest 不会误消费 Timer-only 策略。
- 验收：Python adapter/manifest 定向套件 44 项通过；Rust Runtime Timer 配置闸门 1/1 通过；CLI Timer 时间桶单测 2/2 通过；Timer 示例 manifest 检查通过。
- 边界：OrderBook 仍未进入持久 Runtime worker；Live EventLog 尚无深度快照事实。该更新不等于 E2B/Microsandbox/OpenSandbox 真实服务验收或发布。

### DepthFrame v2 exact Spot quote-budget and shared funding chain（2026-10-26，未发布工作区）

在精确基础币数量单链路上，现补通计价币预算市价买单：单市场和共享多市场准入均校验冻结精度规则与参考价；资金层预留预算和手续费缓冲，部分成交按实际报价成本递减并保留未完成预留；盘口 matcher 按冻结参考价确定数量目标，再逐档受盘口容量、数量步长和剩余预算约束。完整/预算碎额完成标记进入 OMS、精确 Spot 账簿与报告；未使用资金在订单终态释放。E2E 多市场报告也验证同币种账户现金只接受可覆盖预算及手续费的订单。

- 验收：DepthFrame v2 单市场回归覆盖价格跳变预算碎额、跨价位多档累计、跨快照部分成交及预留续持；`cargo test -p qx-xingban --lib --offline -j 1` 193/193、`cargo test -p qx-app --test multi_market_depth_backtest --offline -j 1` 9/9、`cargo fmt --check -p qx-xingban -p qx-app` 通过。
- 边界：盘口队列位置推断和衍生品 DepthFrame v2 撮合仍未实现；本地代码验收不等于交易所 Sandbox/实盘链路验收，也未发布新版本。

### DepthFrame v2 exact Spot 逐档撮合（2026-10-26，未发布工作区）

订单簿 matcher 现在可消费 DepthFrame v2 精确买卖盘容量，支持绑定精确 Spot 交易规则的基础币数量委托；按精确价量逐档撮合，并在成交/ExecutionAttempt 中保留精确数量、价格、报价币成本和手续费。精确买单现金成本向上取整、卖单收入向零取整；精确市场冲击按 tick 朝不利方向取整。排队估算、报价币预算委托、衍生品、混用 v1/v2 或缺少冻结精度规则仍会明确失败关闭。Strategy OrderBook 事件也保留盘口原始精度。

- 验收：精确 2e-10 数量的逐档成交、手续费/报价现金和精确账本回放测试通过；输入指纹覆盖相同 1e-9 投影下不同的精确盘口值；`qx-app` 离线编译通过。
- 边界：单笔 matcher 能精确成交不代表整条 Depth 回测已完整支持小币种；单市场与共享账户回测入口现在会显式拒绝 exact order，因为订单风控、资金/库存预留、组合核算和部分报告字段仍未精确化。该路径不作为发布能力。

**后续状态更正（2026-10-26）：**上面两条“报价币预算委托/衍生品仍未实现”和“单市场与共享账户入口拒绝 exact order”的边界，记录的是该条目写入时的阶段状态，已被后续实现取代。当前 DepthFrame v2 已贯通单市场与受限的共享多市场精确风险准入、资金/库存预留、逐档撮合、Spot quote-budget、线性/反向衍生品成交保证金复核、精确 Fill/费用/账簿/EventLog 重放、权益报告及报告读回；以当前架构方案 §13.8–§13.11 和本 changelog 后续验收条目为准。现存限制是队列位置估算、混合 Spot/衍生品组合、跨结算币种换算及 funding 深度模型仍不支持或 fail closed；本地离线/mock 证据也不等于真实交易所或沙箱供应商验收。

本次 BaseStrategy/沙箱能力复核：`test_strategy_base.py` **26/26**、`test_strategy_process_adapter.py` **16/16**、E2B 持久 worker **10/10**、Microsandbox/OpenSandbox 与远程研究 sandbox **19/19** 通过。以上为本机和 fake SDK 测试，不是连接供应商的隔离、容量、费用或实盘验收。

### DepthFrame v2 保留精确盘口源值（2026-10-26，未发布工作区）

`BookLevel` 现在可以同时携带盘口价格/数量的 `ExactDecimal` 源值与 1e-9 兼容投影。DepthFrame v2 要求每一档都提供精确值，校验投影与盘口顺序时使用精确数字，并将 coefficient/scale 纳入输入指纹；v1 仍沿用原始结构，出现精确字段会明确拒绝。Provider Depth 冻结时识别全量 exact 档位并写成 v2，混合精度会拒绝；Tick adapter 仍不接受 v2，exact Spot 的基础币订单撮合能力见上条记录。新增 `depth-frame-v2` JSON Schema 与示例并登记到 schema registry。

- 验收：`qx-xingban --lib` **183/183**、`qx-app --lib` **159/159**、Depth 集成 **10/10**、MultiMarket Depth **8/8** 通过；Provider exact v2 冻结并在执行前失败关闭用例通过；JSON Schema/示例/注册表校验及 `qx-cli plan schema-registry` 通过。
- 边界：该项只关闭精确盘口输入合同及其校验/指纹。虽然后续已接通有限的 exact Spot 撮合，队列估算与 Depth 回测完整的精确风控/资金预留尚未贯通，故仍未达到深度小币种完整支持或发布条件。

### Depth 回测收益与回撤改用精确权益曲线（2026-10-26，未发布工作区）

单市场 OrderBook/Tick Depth 和共享账户 MultiMarket Depth 报告现在同时保留 legacy raw 与 `equity_exact` 曲线，并以 exact 曲线计算收益和最大回撤。单市场 Depth 摘要包含 `metrics_exact.equity`，权益 CSV 新增 schema v5，RunRecord 校验该版本；MultiMarket Depth 产物升为 v3 并保存精确权益 sidecar，完整报告 reader 会复核 sidecar、时间轴、初始资金、收益和回撤。

- 验收：`qx-xingban --lib` **179/179**、`qx-app --lib` **158/158**、Depth 集成测试 **10/10**、MultiMarket Depth 集成测试 **8/8** 通过。
- 精度边界：DepthFrame 的盘口价格/数量与撮合器仍使用 1e-9 `Price`/`Quantity`，当前订单簿 matcher 也拒绝精确订单金额。因此不能声称小于 1e-9 的深度行情/成交已完整支持；还需贯通精确盘口容量、匹配、成交与账本。该记录只关闭估值曲线和报告指标部分。
- 此为未发布本地工作区记录，不代表供应商、部署隔离或实盘验收。

### 单市场 Bar 收益率与最大回撤基点改用精确权益（2026-10-26，未发布工作区）

Rust Bar 回测现在从 scale-aware exact equity 计算 `return_bps`、`max_drawdown_bps`，训练窗最大回撤剪枝沿用同一精度；legacy raw equity 与 `max_drawdown_raw` 保留兼容投影。新增用例覆盖 `0.0000000019` 到 `0.0000000020`：精确收益是 526 bps，按 1e-9 raw 投影会误报为 10,000 bps。

- 验收：`qx-xingban` 全套测试通过；`qx-app` 账户/规格契约测试 18/18 通过；`qx-app`/`qx-sdk` 离线编译检查通过。
- 边界：该记录对应当时已修复的单市场 Bar 指标；其后 MultiMarket Bar 也已改用 exact equity。MultiLeg、Depth 同类 raw 指标和组合预算 exact 精度仍待贯通。离线证据不代表发布或供应商验收。

### 多市场 Bar 回测收益与回撤基点使用精确权益（2026-10-26，未发布工作区）

多市场共享账户回测的 `return_bps`、`max_drawdown_bps` 现在从 `equity_exact` 计算，避免很小的精确权益经 1e-9 raw 投影后扭曲收益排名或回撤判断；不再使用的 raw 最大回撤 helper 已删除。

- 验收：子纳米权益示例收益为 526 bps、峰值回撤为 500 bps；修复 Provider Depth RunRecord 断链后，qx-app 库级回归 **156/156 通过**。首次依赖编译触发本机 MSVC D8050；设置 `CARGO_PROFILE_DEV_DEBUG=0` 后通过。
- 边界：MultiLeg 与 Depth 指标仍需复核 exact equity 接入；组合预算、跨币种资金、本金输入、恢复/通用导出和 C++ DTO 等 W1 残余仍在。

### Provider Depth RunRecord 接受 indexed equity CSV schema v3（2026-10-26，未发布工作区）

全量 qx-app 回归发现深度回测产物使用 `index,ts,equity_raw,position_raw` 权益表头，但 RunRecord 只识别 Bar equity 的 v1/v2 表头，导致 Provider Depth 回测在 RunRecord 落盘时失败。现为 indexed 深度权益 CSV 登记 schema v3，并允许 RunRecord v2 对该 equity 工件引用 schema 3；未知表头仍失败关闭。

- 新增 `backtest-equity-csv-v3.json` 和 v1/v2/v3 表头识别单测。
- 验收：修复前 qx-app 库测试为 153/154，唯一失败是 Provider Depth RunRecord schema 拒绝；修复后完整 qx-app 库测试 **156/156 通过**，包含 v1/v2/v3 表头映射、未知表头拒绝、Provider Depth 回测落盘与报告读回。

### MultiLeg 与 Depth 基点指标 exact 性复核（2026-10-26，待实施）

静态追踪发现 MultiLeg 腿级收益用 raw Position、费用、资金费率与 raw 初始资金计算；组合收益和回撤也消费 raw equity。Depth 与 MultiMarket Depth 的共享撮合报告只保存 raw equity，并从 raw Ledger valuation 计算回报和回撤，没有 exact equity sidecar 可直接接入。对 Depth 小币种精度的完整修复必须先贯通精确行情/订单簿价格、成交、账簿估值和回报曲线，再升级 DTO/schema 与读侧复核。

- 状态：MultiLeg/Depth/MultiMarket Depth 的 exact 基点链未实现；本条记录为源码复核，不是验收通过。RunRecord 已接受 indexed equity CSV schema v3，只修复产物元数据登记，不改变深度数值精度。

### 研究报告成交明细保留精确数量、价格与费用（2026-10-26，未发布工作区）

Python 单市场交互式实验报告会展示并导出成交 CSV v2 的 `qty_exact`、`price_exact` 和 `exact_fees_json`。多市场报告将 Rust Fill 的 coefficient/scale exact sidecar 投影为十进制字符串后展示/导出，同时保留原始 Fill 字段；非法或不完整精度结构会停止报告生成。该投影使用足够大的 Decimal context，覆盖 i128 系数边界而不经浮点或低精度舍入。

- Python 交互式实验报告专项测试 **16/16 通过**；本次代码变更之前的 Python 全量测试 **450 通过、10 跳过**。本条为未发布工作区记录，不构成完整发行包或 live 验收。


### Python 策略精确订单意图贯通 Bar 回测（2026-10-26，未发布工作区）

策略标准类生成的 `qty_exact` / `limit_price_exact` 订单意图现在可通过共享 Rust Bar 回测链执行。策略必须为标的提供冻结的 `market_spec.exact_rules`；Rust 按精确 step/tick 验证订单、撮合并将精确数量、价格和费用写入 Fill/EventLog。`target_qty_exact` 净仓目标仍不支持 Bar 回测，需改用方向明确的订单意图。策略诊断 CLI 校验完整数据集后，会从 BarFrame 剥离 instrument 身份元数据再传入严格的 StrategyInput 契约。

- 精确 Python callback 回测确认 `0.001` 数量在 Fill 中保留为 coefficient `1`、scale `3`；Rust 精确意图测试 **3/3 通过**，Python 全量套件 **448 通过、10 跳过**，CLI/HTTP/Python Rust 集成入口 **33/33 通过**。

### 远程策略 worker 优雅退出与 `on_stop`（2026-10-26，未发布工作区）

修复 E2B 与 Microsandbox 持久 worker 正常关闭/会话轮换时先杀 guest 的问题。现在 host 先关闭 guest stdin，让 Python Runtime worker 走 EOF/finally 并调用 `BaseStrategy.on_stop()`，随后有限等待输出流收尾；最多等待 5 秒，超时仍强制终止并销毁隔离环境。启动失败和策略请求故障仍立即 fail closed。OpenSandbox 原有显式 close 请求保持不变。

- 定向 E2B/Microsandbox fake SDK 用例 **14/14 通过**，覆盖正常 EOF 退出、session 轮换 checkpoint、provider sandbox 回收和策略 worker 故障时强制清理；未连接真实供应商服务，未运行 KVM/microVM 集成验收。

### Python Runtime BaseStrategy 接入账户与持仓快照（2026-10-26，未发布工作区）

Python `BaseStrategy` 新增 `on_account`、`on_positions_snapshot` 和 `on_position` hooks。策略 worker 从绑定账户/venue 的 EventLog 读取完整快照，以 input v7 传递有序事实；空快照会保留，持仓快照先整体回调再逐行回调。回调 context 不含可能晚于该快照的余额、持仓、保证金、风险和研究目标。快照只作观察，不修改 Rust ledger；所有策略意图仍回到宿主 Risk/OMS。

- v1-v6 输入序列化形状保持兼容；v7 可与旧执行更新、fill、checkpoint、精确 OHLCV 和 Tick 事实组合。Account/position exact sidecar 会校验与 1e-9 raw 投影一致；单批账户快照最多 10,000 条，积压按 EventLog 游标分批交付。
- 本地策略仍默认直接执行且不加载沙箱 SDK。按用户最新部署偏好，服务器持久 worker 建议首选 E2B；自有 Linux/KVM 主机可选 Microsandbox local microVM，OpenSandbox 为自托管服务备选。strategy-worker 的 provider 仍需部署者显式设定，unset 才会使用本地直跑；独立 `remote-research` 批任务默认 E2B。provider 故障不自动切换或回退本机；`llm-sandbox` 尚未集成。
- 当前是 Python Runtime class worker 实现，尚未扩展到 Rust/C ABI/C++ callback host；真实 provider 隔离、容量、费用及 live 账户链路仍需独立验收。

### Microsandbox 持久策略 worker 适配（2026-10-26，未发布工作区）

加入可选 `QX_PYTHON_STRATEGY_EXECUTION_MODE=microsandbox` 持久策略桥接。它用 Microsandbox Python SDK 创建隔离 microVM，支持 SDK local/cloud backend、预配置 image、资源限制、有限最长会话、deny-all 网络、JSONL/Framed JSON 双向流和 BaseStrategy checkpoint 续租。Cloud API key 只交给宿主桥接进程，不进入 guest。执行器依赖通过 `qianxing[sandbox-microsandbox]` 可选安装；未选择 provider 时仍由本地 Python 环境直接运行策略。

- Rust CLI 装配识别 Microsandbox 模式并校验 image/backend/凭据；API key 通过单独的宿主 provider 凭据白名单传递。超时、SDK、远程启动或响应故障均 fail closed，不转成本地运行。
- 该适配记录写入时的部署建议是 Microsandbox local backend：显式选择 Microsandbox、但未设置 `QX_MICROSANDBOX_BACKEND` 时，会在 worker 所在主机创建 microVM；Linux 需要 KVM。Microsandbox Cloud 是显式可选项，需独立配置账户/endpoint；这不改变普通本地策略默认直跑、无沙箱的行为。当前推荐顺序已按用户最新偏好调整为服务器首选 E2B，见本文件上方部署说明。
- guest 输出设置单行 16 MiB 和会话 64 KiB 上限；远程 URL 拒绝 userinfo/query 凭据。会话正常轮换先结束 guest stdin 并等待策略 `on_stop`/worker 退出，超时后再强制销毁，限制残留进程和无界服务日志。
- 当前实现依赖预装了相同 Qianxing 版本及策略依赖的 guest image，并假设目标 Microsandbox backend 支持 network-none、streaming exec 和 sandbox destroy。未连接真实 Cloud/BYOC 服务；未做外部隔离、容量、计费、恢复或交易链路验收；未运行测试。

### OpenSandbox 持久 worker fake SDK 回归（2026-10-26，未发布工作区）

新增 `python/tests/test_opensandbox_strategy_worker.py`，模拟 OpenSandbox SDK 生命周期，覆盖远端创建请求 deny-all 网络策略、API key 只留在 host connection config、guest 使用清洁环境、正常请求/文件清理/优雅销毁，以及读取失败时销毁 sandbox 并 fail closed。相关 BaseStrategy、策略文件入口、E2B、Microsandbox 和批量沙箱测试合计 115 项通过；没有连接真实 OpenSandbox 服务，因此不代表 provider 认证、网络隔离、容量或计费已验收。

### 远程研究增加显式 OpenSandbox provider（2026-10-26，未发布工作区）

`qianxing remote-research` 保持 E2B 为默认 provider，并增加显式 `--provider opensandbox` 选项。OpenSandbox 通过可选 `sandbox-opensandbox` extra 和宿主配置的 service domain/image/API key 运行同一批量研究 bundle；沙箱创建要求 deny-all 出站网络策略，完成后销毁实例并复用既有有界归档验证和报告 identity 流程。Provider 选择不自动切换，也不会回退本地策略执行。

- 更新 Python CLI、optional dependencies、README 和策略接口方案；本地 `run-strategy`、`search-strategy` 和 `research` 仍不加载沙箱 SDK。
- 边界：OpenSandbox 当前只覆盖批量研究，不覆盖持久 Runtime 策略 worker；无连接 OpenSandbox server 的运行验收、隔离/容量/费用验收。本轮未运行测试。

### Runtime 策略 worker 增加持久 OpenSandbox provider（2026-10-26，未发布工作区）

Rust `strategy-worker` 的宿主策略执行模式新增 `opensandbox`。宿主可通过独立 provider 凭据通道传递 OpenSandbox API key；远端使用持久 Python guest 进程、文件队列和同一策略 JSONL/framed JSON 协议，保持 BaseStrategy 回调状态，并在正常关闭或 session 轮换时触发 `on_stop`。远端 bundle 和 Qianxing 版本在启动前校验，沙箱创建请求 deny-all 出站网络。该条记录时 `local` 是默认策略执行模式，E2B 是首选远程 provider；后续的 Microsandbox worker 接入和当前部署选择见本文件最上方记录。provider 错误不会回退到本地执行。

- OpenSandbox 通过可选 `sandbox-opensandbox` extra 安装；需要部署端提供支持后台命令、文件操作和网络策略的服务及预装依赖的镜像。
- 边界：尚未连接真实 OpenSandbox 服务完成 SDK/API、隔离、容量、费用或实盘端到端验收；这条记录之后新增了 Microsandbox 持久策略 worker，具体以本文件最上方后续记录为准。本轮不运行测试。

### BaseStrategy 精确 Decimal 历史查询（2026-10-26，未发布工作区）

Python 策略可通过 `history_decimal()`、`current_decimal()` 和 `bar_decimal()` 用 `Decimal` 读取 OHLCV。接口优先使用来源 `exact_ohlcv` coefficient/scale；raw-only 数据则按核心 1e-9 刻度无损还原，不经过 float。`position_qty()` 的 raw 回退也改为直接构造 1e-9 Decimal，并校验 i128 边界，避免高位数量受 Python 默认 Decimal context 舍入。SMA 文件策略示例改为精确 Decimal 计算，适用于价格低于 1e-9 的小币种。

- 本地 BaseStrategy 回测与 Python Runtime class worker 共用点时历史门面，查询仍按 callback `as_of` 截断；精确格式、系数范围和 scale 会校验。
- 本轮只做源码编译和格式/差异检查，未运行测试。

### BaseStrategy 点时历史查询门面（2026-10-26，未发布工作区）

Python `BaseStrategy` 现在可用 `history(context, length=...)` 与 `current(context, field=...)` 查询 Bar。Rust 回测 callback 提供逐事件有界历史，Python Runtime class worker 从列式输入读取；两者都严格截止于本次 context 的 `as_of`，按 `instrument` 隔离，单次返回最多 10,000 根。init/start/restore 和零 Bar warmup 阶段拿到空历史，避免 class worker 在开始回放之前从完整冻结快照读取未来值。raw 字段保持 `*_raw` 刻度，来源存在时保留 `exact_ohlcv` coefficient/scale。预热仍由 `history_requirements()` 单独声明，不会因为可以读取历史而隐式允许下单。

- 将 data portal 暴露给本地 BaseStrategy 回测和 Python Runtime class worker；不向旧 duck-typed 策略 context 注入非 JSON 对象。SMA 多市场示例改为通过 `history()` 计算交叉，避免把历史 Bar 数组作为策略持久状态。
- 更新标准策略接口方案和 Python README，说明本地默认直跑、历史 PIT 边界、E2B 远程当前仅接 Bar，以及备选 provider 尚未接入。
- 验证：改动 Python 文件 `py_compile` 通过，指定文件 `git diff --check` 通过；本轮未运行测试。

### P0-A2 撤单发起事实持久化（2026-10-26，未发布工作区）

统一执行网关现在会在调用交易所撤单前，将 `CancelPending` 写入 append-only EventLog，并以 `control` 标记其来源。事件关联到稳定 CancelOrder command ID；同一命令重放幂等，不同撤单操作遇到已有在途请求会被拒绝。进程在网络请求期间退出后，恢复会看到撤单在途状态；请求超时/空响应转入 `Unknown`，状态未知时禁止再次直接撤单，必须先对账。交易所确认的迟到 `Cancelled` 仍能使 `Unknown` 收敛到终态。API 事件 scope、报告订单关联、CLI 报告事件名称与 Python 策略订单状态更新均已覆盖新事件。

- 同步更新了 Rust 执行/Runtime 合同和订单状态 EventLog 重放；无有效活动订单或未知订单不会写入新的撤单发起事实。
- 执行 EventAppender 现在返回追加/重复结果。相同 CancelOrder 命令重投即使来自持有旧 OMS 快照的 worker，也会先刷新共享 EventLog 并识别重复 `CancelPending`，在调用 Venue 前停止；不同命令竞争撤单会因当前 OMS 已处于 `CancelPending` 而拒绝追加。新增执行端口与 Runtime 共享日志用例覆盖重复投递和竞争追加。
- 新增真实子进程退出注入：子进程在 Venue `cancel_order` 调用入口退出，父进程重开 EventLog 后确认订单恢复为 `CancelPending`，再次撤单在 Venue 调用前被拒绝。它覆盖 Gateway 已持久化意图但没有拿到 Venue 响应的恢复窗口；真实网络已发送后断电仍需外部验收。
- Runtime 全量测试首跑发现旧子纳米行情用例把 `1e-10/2e-10` 的精确数量配成了非零 1e-9 raw 数量；修正测试 fixture 为正确的向零投影 `0/0`，生产校验逻辑未放宽。
- 验收：Rust workspace `cargo check --offline --workspace -j 1` 通过；`qx-execution --lib` 36/36、`qx-runtime --lib` 94/94（Windows `TEMP` 指向仓库可写测试目录以绕过系统临时目录 ACL）；单独撤单生命周期、重复命令防重发、共享 EventLog 竞争追加及子进程退出用例包含在通过结果内；Python 架构门禁四片、228 个具名检查通过且无重复；`py_compile` 与改动文件 `git diff --check` 通过。
- 范围：仍未覆盖真实网络请求已发出后的断电/响应丢失、成交/撤单乱序回报、跨 EventLog 后端并发压力及多腿全路径恢复；A2/P0 尚未整体完成。

### P0-A2 撤单在途期间部分成交归约（2026-10-26，未发布工作区）

撤单请求已持久化为 `CancelPending` 后，交易所仍可能先推送部分成交，再确认撤单。此前 Runtime OMS 与 Binance adapter 会尝试把订单迁成 `PartiallyFilled`，导致状态迁移失败；直接放开迁移又会丢失持久撤单意图，并可能在重启后再次发起撤单。

- OMS 对 raw 与 ExactDecimal 成交都继续累加数量/账务，但只要成交未完成，就保留 `CancelPending`；完整成交迁成 `Filled`，权威撤单确认迁成 `Cancelled`。没有放宽 `CancelPending -> PartiallyFilled` 状态迁移。
- Binance 用户流与 CCXT raw/exact reconciliation 在撤单等待期间接纳部分成交并保留取消状态。撤单终态后再到达的新成交仍 fail closed 并要求对账。
- EventLog 用例覆盖 `CancelPending -> partial Fill -> restart -> duplicate Fill -> Cancelled -> restart`：重复成交不重复记账，成交量与终态重放一致。
- 验收：`qx-adapter --lib` **96/96**、`qx-runtime --lib` **96/96**、`qx-zhenlu --lib` **56/56**、`qx-core` 状态迁移用例 **1/1**、`contract-tests --test recovery_and_replay` **3/3**；涉及文件 rustfmt 检查与 `git diff --check` 通过。
- 范围：这是本地适配器/OMS/EventLog 的竞态验收，不是交易所私有流时序验收；真实网络副作用后响应丢失、跨节点写入压力与多腿补偿仍未完成，A2/P0 继续开放。
- 并发补充：两个独立 SQLite EventLog worker 持有同一 Accepted 快照并同时撤单时，只有一个 worker 能持久化 `CancelPending` 并调用 Venue；最终 EventLog 仅有一条 `CancelPending` 和一条 `Cancelled`。`qx-runtime --features sqlite --lib` **99/99** 通过。该线程级共享库用例不代替跨进程/跨节点压力或真实网络验收。

### BarFrame 子纳米 OHLCV 的版本化保真切片（2026-10-26，未发布工作区）

BarFrame 最高读写契约升到 v2；raw-only 帧仍写 v1，包含来源精度的帧写 v2 `exact_ohlcv`。精确值同时进入 Rust/Python JSON 校验、列式帧摘要、数据集指纹、CCXT 行情归一化/Provider、native Rust Bar 策略事件、Bar 撮合价量与实时快照/实验折分；旧 raw 列按向零截断投影到 1e-9，精确 sidecar 是权威值。`StrategyContractBars` 的跨进程策略历史和 C ABI `QxMarketEvent` 仍只有 raw 列，未把整条多语言策略输入链宣称为无损。

- 验收：全 workspace `cargo check` 通过；Python Bridge/CCXT 定向 54 项通过；`qx-adapter` 94 项通过（含 v2 CCXT Provider 精度保留）；`qx-data` 38 项通过；`qx-datastruct` v2 合同 5 项通过；`qx-xingban` exact volume participation 用例通过。全 crate `qx-xingban` 176 项中首次运行有一条新用例因超过 Decimal 18 位上限失败，缩减到受支持精度后针对用例复验通过。
- 状态：这是 W1 的数据与 native Bar 垂直切片，不表示不同供应商数据已经都含来源精度。输入仍为 v1 的源文件无法恢复已丢失精度；跨进程 Python/C ABI 策略事件、其合同/Schema、所有历史数据供应商规范化、恢复和报告导出仍需继续闭环。

### StrategyInput v5 跨进程保留精确 OHLCV（2026-10-26，未发布工作区）

将来源精度 `exact_ohlcv` sidecar 接入 Rust/Python `StrategyContractBars` 与 StrategyInput v5 JSON Schema；raw-only 仍沿用旧契约，v5 可与 checkpoint、订单状态及成交更新共存，生产装配按实际特性选版本。共享内存列式载荷新增 v2 标记和精度元数据，v1 raw-only 载荷保持兼容。Python `BaseStrategy.on_bar` 可读取十进制字符串/系数与 scale，不经过浮点转换。

- Rust 与 Python 均校验 sidecar 行数、OHLCV 合法区间及与旧 1e-9 raw 列的一致投影；不匹配或旧版本携带精确字段会失败关闭。版本关系登记、拒绝文案和常驻用例同步到 v5。
- 验收：全 workspace `cargo check --offline` 通过；Python Strategy Contract/Process Adapter 44/44；Rust v5 JSON 往返、列式 v2 元数据与未来版本拒绝 3 项针对用例通过；变更 Rust 文件定向 `rustfmt --check` 和 `git diff --check` 通过；架构快速门禁 4 片完整覆盖 227 个具名检查、无重复、0 失败。
- 边界：此项补齐的是 Rust↔Python Bar 策略进程，不包括 C ABI `QxMarketEvent`，也不证明所有市场供应商都提供源精度；W1 仍需完成 C ABI、供应商覆盖、恢复、账簿/导出和跨货币预算等余项。

### C ABI 可选 v2 市场回调保留 exact OHLCV（2026-10-26，未发布工作区）

保留原有 `QxMarketEvent` 和 `QxStrategyVTable` 布局，新增独立导出符号 `qx_strategy_on_market_event_v2` 与版本化 envelope，承载原始 v1 事件和 exact OHLCV coefficient/scale。Rust 动态宿主探测 v2 符号；旧插件处理普通 raw 事件仍兼容，遇到带精度的 Bar 则明确拒绝降级，避免小币种价格截断后继续下单。C++ SDK 示例现实现 v2 回调并将精度值传到 Rust host。

- 验收：Rust C ABI 单测覆盖 v2 精确事件和旧 v1 拒绝；Visual Studio 2022 编译 C++ SDK 合同程序与策略 DLL；CTest 合同用例 1/1；Rust 宿主实际加载该 C++ DLL，精确 close `1005/10` 跨边界保留，动态插件集成 1/1；C ABI Rust/header 逐字段架构检查 12 项通过；随后完整架构快速门禁 4 片、227 个具名检查通过，无重复项。
- 限制：`qx-strategy` 全 crate 本轮 53/59 通过，另外 6 项 Windows 临时文件/内存映射用例报 `PermissionDenied`，与本次 C ABI 用例无关，仍需在稳定运行环境复验。W1 仍未完成全部 Provider 精度覆盖、账簿恢复/通用导出、跨币种预算与 spot/linear/inverse 全场景对拍。

### 本地 BaseStrategy 文件策略接通 Tick/L2 回测与深度参数搜索（2026-10-26，未发布工作区）

`qianxing run-strategy` 现可直接加载用户 Python 文件，并把 `DepthBacktestSpec` 的 L1 Tick/L2 OrderBook 事件送入 `BaseStrategy.on_tick` / `on_order_book`，订单决策仍由同一 Rust 深度引擎校验、撮合、记账和复算。完整策略预检校验 `data_level` 与 l1/l2 对应关系、深度策略参数及可从 `market_spec` 证明的产品/venue；数据帧本身仍由深度用例在实际运行中解析验证。`search-strategy` 也可接受 `RunDepthExperimentSpec`，每个参数候选新建 Python 策略实例并输出 Rust 排名；报告明确标记为输入区间样本内排序。新增 L1 Tick 均线类策略和本地单跑/网格 JSON 示例。

- `BaseStrategy` depth 回调不再把 `warmup_bars=0` 错误映射为 Tick/L2 的 `on_warmup_complete`；Tick/OrderBook 类 manifest 必须使用 `warmup_bars=0`，策略自行维护事件预热。Bar 路径仍保留原回调语义。
- 验收：策略文件 CLI + BaseStrategy 42/42；策略 manifest、真实 Bar/Tick CLI 与深度网格 27/27；Python 深度 SDK 集成 2/2；Rust `strategy_preflight` 7/7；架构快速门禁覆盖 227 个检查、无重复；相关 Python 文件 `py_compile` 与 `git diff --check` 通过。Tick 单跑处理 80 帧并产生 1 次成交；9 组深度参数候选全部完成。覆盖 Tick CLI 路由、策略层级 mismatch、深度参数搜索工厂、独立类实例、warmup 边界、l1/tick 与 l2/orderbook 对应关系。
- 边界：Tick/L2 完整清单当前不能声明 `asset_classes`，因为 `DepthBacktestSpec` 没有该字段；depth 研究暂不支持 train/OOS、交互 HTML 或 future-data candidate diagnostics。持久 server strategy-worker 的 class process bridge 仍只贯通 Bar；本项不代表远程 E2B Tick/L2 worker 已完成，也不代表真实 sandbox/provider 验收。

### 架构门禁漂移修复与共享队列合同补齐（2026-10-26，未发布工作区）

按本轮全仓架构扫描修复能力目录漂移：从 capability matrix、maturity records 和 crosswalk 重新生成 API/Python JSON 目录与 Markdown 能力表，生成器 `--check` 通过。队列后端共享合同用例补上非法运行身份/零租约拒绝、同一稳定 Run 的幂等重投与现有状态保留，以及相同 run_id 被不同任务载荷占用时的冲突；文件与 SQLite 后端均执行同一行为合同。

- 架构门禁的实现形状检查已跟上当前精确路径：Bar 账簿重放 helper、ExactDecimal 累计成交量上限、Paper 恢复的 `try_on_quote` 错误路径和多市场精确保证金快照。`python tools/check_architecture.py` 全量运行退出码为 0。
- 定向验收：文件队列 1/1、文件+SQLite 队列合同 2/2；PostgreSQL feature 编译通过，数据库用例因需要 `QX_TEST_POSTGRES_DSN` 标记 ignored；能力目录 API 查询 5/5；估值 12/12；`qx-xingban` 全 crate 通过（176 个单元测试及集成目标）；Paper 多腿恢复 2/2。
- 边界：全仓架构静态门禁通过不代表完整发布矩阵通过；本轮没有连接 PostgreSQL，也没有进行 E2B 真实账户、隔离、容量或费用验收。

### E2B 远端持久 Python 策略 worker 接入（2026-10-26，未发布工作区）

Rust Python Strategy worker 新增宿主环境强制的 E2B 模式：`QX_PYTHON_STRATEGY_EXECUTION_MODE=e2b` 才创建远端沙箱；不设置时保持本地。一个 Python 策略 worker 在首个事件时懒创建持久 E2B sandbox，通过 JSONL 或 Framed JSON 转发策略事件和响应；共享内存模式显式拒绝。远程启动绑定本地策略文件/静态 Python 导入、spec 与引用输入的 SHA-256，核验远端 Qianxing 包版本及策略模块初始化；策略意图仍回到宿主 Rust Risk、OMS 和执行链。E2B API key 通过独立的宿主 provider 凭据通道传给本地桥接进程，普通策略环境仍拒绝凭据变量；策略输入中的凭据字段、明显内嵌凭据源码、常见 credential 文件名/密钥文件和凭据赋值模式都会被拦截；远端命令清空模板继承环境并关闭互联网访问。BaseStrategy 会用宿主持久 checkpoint 轮换 session，策略 worker job lease 同时覆盖 E2B 启动和请求上限。E2B 配置缺失、超时、连接/模板故障不会回退本地策略执行。

- 本地默认路径与 E2B provider extra 解耦；服务器可设置模板、API key 和租约，默认 3600 秒，最短为策略请求超时加 30 秒，最长 86400 秒；远程请求超时至少为 5000 毫秒。BaseStrategy class worker 会在下一请求携带宿主持久 checkpoint 时提前轮换远端 session；模块函数 handler 没有 checkpoint，处理过首个事件后不安全轮换并会在租约临近时失败关闭。策略 job lease 会按 Python 请求预算和 E2B 启动预算扩展，避免 30 秒默认租约内另一 worker 重复领取。当前 class worker 只接 Bar manifest，Tick/盘口/定时事件未从 Rust worker 端到端贯通。
- 本轮修复并验收：E2B Python SDK 的 `background=True` 返回 `CommandHandle`；stdout/stderr 必须由 `handle.wait(on_stdout=..., on_stderr=...)` 消费，因此桥接现用独立输出读取线程接收响应。新增 10 项 fake SDK/输出协议用例，覆盖惰性创建、网络关闭/API Key 不出宿主、checkpoint 续租、无 checkpoint 拒绝轮换、超时 fail-closed、启动失败清理、上传凭据拒绝，以及多余/迟到/不完整/超限输出 fail-closed；与研究 provider 用例合计 23 项通过。
- Rust 凭据边界修复：E2B API key 原先会被通用策略子进程环境门拒绝；新增仅允许 `E2B_API_KEY` 的宿主 provider 环境通道，策略配置环境仍拒绝该变量。跨进程 Rust 用例通过；本地 Rust↔Python BaseStrategy 合同回归通过；`cargo check --offline -p qx-cli` 通过。
- 状态：持久 E2B bridge 达到源码与 fake SDK 生命周期 `code_tested`，未执行真实 E2B 账户请求。长会话断流恢复、模板资源、吞吐与 p95、费用、网络/凭据越权和 Live 订单链路验收仍未完成。Microsandbox/OpenSandbox/LLM Sandbox 尚未集成。

### 本地 BaseStrategy 默认执行与远程沙箱选型（2026-10-26，方案更新）

确认本地可信策略继续走无沙箱路径：安装 Python 包后直接使用 `BaseStrategy`、`run-strategy`、`search-strategy` 和 `research`，不增加基础安装依赖。该条方案更新记录了当时的策略类接口状态；`on_position/on_account` 已由之后的 Python Runtime input v7 条目落地，Rust/C ABI/C++ 全宿主覆盖仍待扩展。

大型服务器、多租户和用户上传策略新增远程隔离执行工作包；这条初始方案记录的供应商顺序为 E2B、Microsandbox、OpenSandbox/LLM Sandbox，当前持久 worker 推荐顺序已在本文件最上方更新。E2B 研究批任务基础依赖不加载 E2B，策略文件静态收集不在本机导入执行；打包策略、依赖和输入数据，核对策略/数据/spec SHA-256 和 Qianxing 版本，拒绝 spec 凭据字段并禁用互联网；下载报告前校验路径与归档大小。批任务之后另行加入了宿主强制的持久 Python worker bridge，Gateway、交易凭据、Rust Risk/OMS/账簿仍由宿主保留。

- 状态（该记录时间）：远程研究 provider、可选 `sandbox-e2b` extra、CLI、用户模板和说明已落代码；远程研究 fake SDK 集成验收 13/13 通过。持久 Python worker bridge 另有 10 项 fake SDK/输出协议用例通过，合计 23 项定向 Python 用例通过，Rust provider 凭据边界和本地 BaseStrategy 回归也通过（详见本文件上方 E2B worker 条目）。当时本机没有执行真实 E2B 账号/计费/容量/隔离验收，Microsandbox/OpenSandbox/LLM Sandbox provider 尚未集成；之后接入状态见本文件上方更新。
- 供应商依据：E2B 官方产品资料列出 Cloud、BYOC、Embed 与 Python SDK；Microsandbox Cloud 当前为 private beta、BYOC 为 evaluation；OpenSandbox 提供远端生命周期服务和可插拔 runtime；若 `lm-sandbox` 指 `vndee/llm-sandbox`，其公开实现支持远程 Docker Host/Kubernetes，但隔离安全由部署后端配置决定。链接见总方案 §7.3。

### 多市场 exact 权益与贡献报告（2026-10-26，未发布工作区）

共享账户多市场 Bar 报告现在保留精确组合权益，以及每个标的的 exact 现金与持仓贡献。新增的 full exact 权益 sidecar 和逐标的 `pnl_contribution_exact` 进入 RunManifest 指纹；报告读取时校验 raw/exact 时间轴、逐点贡献守恒，并通过重放账簿复核终值。Python 离线研究 HTML 与 Web Console 展示十进制 exact 字符串；旧版 raw-only 报告仍可读取。既有 raw 曲线维持 1e-9 兼容投影，子纳米金额请使用 exact 字段。

- 验收：`cargo test --offline -p qx-core valuation -j 1` 通过；`cargo test --offline -p qx-xingban -j 1` 全包通过；`cargo test --offline -p qx-app -j 1` 全包通过；Python 报告测试 15/15，覆盖 exact 守恒、sidecar 篡改拒绝和 legacy raw-only 兼容；Web Console 与生成的研究 HTML 脚本通过 Node 语法检查。
- 边界：跨计价币种策略预算、恢复/通用导出、批次 exact 比较以及高精度 Bar 源数据仍待完成；不代表交易所实盘链路通过外部验收。

### Bar 组合预算 exact 数量、多标的敞口与阶梯保证金（2026-10-26，未发布工作区）

Bar `reserved_capital_raw` 预算现在按“策略 ID + 标的”跟踪精确持仓与在途挂单，并在同计价币种下按各标的冻结规格、可见价格和保证金规则汇总资本；不同计价币种明确拒绝，不把不同标的数量直接相加。多笔同标的挂单保留最高参考价参与预算，避免后续低价挂单覆盖先前的高价敞口。订单准入、成交价复核和成交回写沿用 exact 数量/价格。带冻结规格的高精度敞口按 exact notional 核算；TieredMargin 对档位选择使用向上投影的 raw 名义额，并按选中档位计算 exact 初始保证金。旧 raw 输入仍保留兼容计算路径。

- 验收：`cargo check --offline -p qx-xingban -j 1` 通过；`cargo test --offline -p qx-xingban -j 1` 全部通过（175 个单元测试、23 个集成测试）；定向覆盖同币种多标的挂单/持仓、混合币种拒绝和同标的不同参考价保守预算。
- 边界：预算值 `reserved_capital_raw` 仍是 1e-9 精度；跨计价币种换算未支持。此条提交时逐标的 exact 归因和多市场精确报告尚未贯通，现已由下方“多市场 exact 权益与贡献报告”条目完成；跨币种预算、恢复/导出仍未完成，不能据此宣称完整多市场 exact-safe。

### Python BaseStrategy 开空与平空意图 helper（2026-10-26，未发布工作区）

`BaseStrategy` 新增 `short()` 和 `cover()`：前者构造卖出开空意图，后者构造固定 reduce-only 的买入平空意图。Hedge 模式通过显式 `position_mode="hedge"` / `position_side="short"` 指定目标腿；margin mode、杠杆、产品权限和风险仍由 Rust 核心校验，helper 不会直连 Gateway。策略文件进程适配器保留 exact quantity 与 policy，并输出版本化 StrategyOutput。

- 验收：BaseStrategy、策略 class process adapter 与 StrategyContract 定向测试 64/64 通过；覆盖 helper 的方向、精确数量、稳定 intent ID、Hedge position side、杠杆传递和 cover reduce-only。
- 边界：仅完成策略意图构造与文件 worker DTO 映射；不代表现货借币开空、所有合约市场/账户模式或外部交易所已获支持。Hedge mode 必须显式传递 position policy。

### Bar 回测精确费用与成交额进入研究报告（2026-10-26，未发布工作区）

Bar 撮合成交现在携带费用币种；单市场 `BacktestReport` 按币种汇总 exact fees，并输出保留十进制精度的 turnover。应用运行摘要、策略与多腿回测 artifacts、研究报告 schema、Python 交互式 HTML 和 Web Console 都透传/展示 `metrics_exact`。已有 `fees_raw` 与 `turnover_raw` 保持兼容，子纳米金额分析应读取 exact 字符串字段。

- 验收：子纳米成交费用与成交额回归 1/1；`qx-app` 自定义策略 artifact 链路 5/5；Python 研究 HTML 13/13；Web Console `node --check`、研究 schema JSON 解析通过；相关文件 `git diff --check` 通过（仅有仓库 LF/CRLF 转换提示）。
- 边界：精确报告目前覆盖单市场 Bar 汇总；多市场精确合并、恢复/导出与批次比较仍未完成，不能将旧 raw 字段当作精确成本口径。

### Bar 回测现货 quote-budget 市价买入贯通（2026-10-26，未发布工作区）

单市场现货 Bar 回测现在可以接收 quote-budget 市价买入，按冻结参考价换算基础币数量并向下对齐 lot step。每笔部分成交会记录精确 quote cost；lot step 留下的预算余数通过最终 Fill 完成标记收敛 OMS，策略收到终态状态，EventLog digest/replay 保留该完成事实。预算换算和成本舍入均保守处理，避免超过预算或少记已用计价币金额。

- 验收：端到端覆盖多根 Bar 下的部分成交、7 次 lot-step 成交、预算未超支、exact position/cash、OMS `Filled`、策略终态回调与 EventLog 重放；`cargo check --offline -p qx-xingban -j 1` 通过；`cargo test --offline -p qx-core -p qx-risk -p qx-zhenlu -p qx-xingban -j 1` 通过；Python StrategyContract/BaseStrategy 定向测试 50 项通过。
- 边界：仅完成单账户、单市场、现货 Bar 回测路径；多市场预算分配、合约 quote-denominated sizing、实盘交易所 request 以及跨市场精确报告/恢复仍待独立贯通和验收。

### Python BaseStrategy 本地策略易用函数与执行边界说明（2026-10-26，未发布工作区）

为快速本地开发策略，`BaseStrategy` 新增确定性 `next_intent_id()`、`buy()`、`sell()`、精确优先的 `position_qty()` 与净持仓 `close_position()` 助手。方法只构造策略意图，不直连交易所；close 自动设置 reduce-only，精确持仓优先于 1e-9 raw 投影，路由账户持仓优先匹配。

Python 用户文档说明了本地可信代码默认直跑、无需 sandbox 的路径，以及多租户/上传策略应由部署策略强制真正的 OS/容器隔离。当前 runtime worker 子进程不是安全沙箱；账户/持仓更新回调与 sandbox host 仍需独立实现，不能标成已支持。

- 验收：`test_strategy_base.py` 22/22、`test_cli_strategy_file.py` 13/13；相关文件 `git diff --check` 通过（仅有仓库 LF/CRLF 转换提示）。
- 边界：helper API 不改变订单决策协议；实际订单仍由 Rust 核心校验。`close_position` 仅对净持仓安全，合约双向持仓需要明确 position-side policy。

### P0-C Paper L1 精确撮合与跨重启补偿（2026-10-26，未发布工作区）

`PaperVenue` 现在要求 exact 订单绑定冻结 `TradingInstrumentSpec`，并按精确 L1 价格、数量和交易步长撮合，记录精确成交费用与标的结算币种。Paper 策略 replay、spread recovery、execution submit 和 smoke 调用改用可传播错误的 quote matcher；行情 sidecar 在接收边界校验正值、raw 投影及盘口顺序。

跨重启补偿验收发现，同一确定性补偿单第一次成交后，恢复逻辑把变化的 `filled/quote_filled` 累计值纳入请求身份比较，误判 client ID 冲突并把 spread group 转为 `ReconcileRequired`。现在身份比较覆盖市场、方向、数量/精确请求、限价、账户、策略 trace 和订单政策，并忽略可变成交累计字段；子纳米原腿部分成交后恢复 exact 规格、生成补偿、撮合并收敛到 `Hedged` 的端到端用例通过。

- 验收：`cargo test -p qx-zhenlu -p qx-execution -p qx-service --lib` 171/171；`cargo check -p qx-zhenlu -p qx-execution -p qx-service -p qx-cli` 通过；exact spread restart 用例单独通过；相关文件 `rustfmt`、`git diff --check` 通过。
- 边界：离线 Paper 证据不等于真实交易所验收；Bar exact 撮合、全部回测入口一致性与 exact 报告回环仍在 P0-C 范围内。

### Python BaseStrategy 策略状态 checkpoint 与游标原子恢复（2026-10-26，未发布工作区）

Python Runtime `BaseStrategy` class worker 现在支持用户策略状态快照：默认 `snapshot_state()` 保存 JSON 兼容实例属性；策略可覆盖 `snapshot_state()/restore_state()` 并通过 `on_restore(context)` 重建本地资源。输入协议升级为 Strategy Input v4，包含 schema v1 `StrategyCheckpointV1`，绑定 `strategy_id/strategy_version`，限制 JSON 对象总大小 64 KiB。adapter 的 warmup/bar/update cursors 与策略状态共同恢复。

Rust worker 将 checkpoint、EventLog 执行事实游标、策略身份摘要和已提交 run marker 写入同一原子快照。写入失败时不推进内存 cursor；身份不匹配、checkpoint 损坏或旧游标已前进却没有 checkpoint 时 fail closed。重启发现 run marker 已提交则完成 Scheduler run 并确认队列，不再重复调用 Python。恢复保持 at-least-once 语义，不宣称策略副作用与外部下单 exactly-once；订单效果已提交但 checkpoint 尚未提交的窗口仍需稳定 intent identity 或 OMS 对账。

- 定向验收：checkpoint+cursor 原子提交与身份绑定 1/1；cursor 原子替换中断后的 backup 恢复 1/1；Rust→Python class worker 四种 transport/checkpoint 端到端 1/1；Strategy Input v4 schema 9/9；Python BaseStrategy/process adapter/StrategyContract 60/60；涉及 Rust 文件 `rustfmt --check` 通过。
- 范围限制：仅 Python Runtime class worker；不覆盖 SDK callback、Rust/C ABI/C++ 策略状态恢复、多 worker 并发、底层持久介质损坏或供应商 sandbox。`qx-cli` 全量单测此前为 386/436，主要失败与本机 OS 临时目录 ACL/旧 CLI fixture 有关，尚不能标记全仓验收通过；完整架构门禁另有既存的 job queue shared-contract 检查失败。

### Strategy worker 执行事实游标跨故障恢复（2026-10-26，未发布工作区）

新增 worker 级恢复覆盖：Python 策略实际接收 EventLog 订单状态和两笔部分成交；回调失败时 cursor 不前进；cursor 持久化后模拟 ack 失败，队列保留冻结范围并在重启后重放；成功 ack 后再次启动不重复回调。测试发现并修复已追平边界：cursor 正好是 EventLog 尾序号加一时应返回无待办，只有超过尾序号加一才视为日志截断。

重启验收又发现重复 enqueue 的 run 身份比较包含了重新生成的时间戳，导致同一 `JobSpec` 和冻结范围在已有队列项时被错误拒绝。队列现在按稳定 run 身份幂等接收重复 enqueue，并保留已持久化的运行状态、重试次数和租约；真正不同的 JobSpec 或 manifest 仍拒绝。

- 验收：`qx-storage` 队列幂等回归 1/1；策略 worker 冻结范围故障注入 1/1。`qx-cli` 全量 434 项运行中 425 项通过，9 项被已有新鲜度门禁拒绝；重建 CLI 后，受影响分组 1/1、6/6、19/19 通过。此前 worker 子集为 26/26。
- 边界：证明当前 Files 队列与单 worker 的回调失败、ack 失败及重启恢复顺序；尚未证明多 worker 并发、底层存储介质损坏、供应商回报或 exactly-once。

### 策略订单状态与部分成交跨宿主共享黄金样例（2026-10-26，未发布工作区）

扩展 `strategy-execution-sequence-v1.json` 为六条更新：同一时间戳覆盖两个账户和 Binance/OKX 两个标的，一笔 BTC 订单经历两次部分成交，同一 EventLog 序号的状态更新固定排在成交更新前。Rust contract validator、qx-cli C ABI callback 排序、Python BaseStrategy callback、Python JSONL Strategy Input V3 双数组与 C++ 动态策略宿主共用该 fixture。新增冻结范围重试用例：EventLog 追加新尾部后仍按原 cursor range 返回相同的两次部分成交事实。C++ 示例策略诊断快照记录最后接收的订单状态 seq。

- 验收：Rust shared matrix 1/1；qx-cli callback sort 1/1；冻结 cursor retry 1/1；Python strategy base + contract 48/48；SDK/schema layering 39/39；CMake C++ example/typed contract 构建成功；C ABI 动态加载 C++ 插件消费六条更新 1/1；旧 C ABI fill fallback 1/1；C++ contract CTest 1/1；架构快检 227 项、四片无重复。
- 边界：覆盖的是离线排序、payload、部分成交与固定游标范围重试；策略进程仍需基于 `event_seq` 自行处理 at-least-once 重投，自动 `BaseStrategy` 进程适配、进程崩溃注入、交易所验收和发布尚未完成。

### Live strategy worker 通过 Strategy Input V3 接收 EventLog 成交更新（2026-10-26，未发布工作区）

策略 worker 现在从账户/venue EventLog 按持久序号游标重放匹配订单的状态与成交更新。成交保留源事件 `event_seq` 和成交前订单快照，并通过 Strategy Input V3 一起交给 Python/外部进程策略的 `on_event(StrategyInput)`；V3 要求存在非空 `fill_updates`，可选包含 `execution_updates`。同一 EventLog 事件的订单状态回调先于成交回调。作业携带冻结的游标范围，成功处理后推进游标，重试复用相同范围。

架构豁免台账同步更新 `execution_updates_for_ranges` 的只读 EventLog 打开点名称，避免重命名后形成未登记站点。

- 验收：执行更新 replay 5/5；状态/成交同 seq 顺序 1/1；V3 schema integration 8/8；Rust 不支持版本拒绝用例通过；Python strategy contract 28/28；架构 `--fast` 四片 227 项无重复；豁免台账定向门禁 1/1；重建当前 CLI 后 `cargo test -p qx-cli --bin qx-cli` 432/432 通过（186.43 秒）。首轮旧 binary 检查失败已由重建解决。
- 边界：文件策略仍以 JSONL `on_event(StrategyInput)` 消费数组，不会自动实例化 Python callback SDK 的 `BaseStrategy`；跨宿主 golden matrix 和真实供应商验收仍待完成。

### C ABI/C++ 策略接收带 EventLog 序号的成交（2026-10-26，未发布工作区）

C ABI 动态插件现在可以可选导出 `qx_strategy_on_fill_update_v2`，消费 `StrategyFillUpdateV2` JSON；`QxStrategyVTable` 布局保持不变。未导出新符号的旧插件继续收到原 fill-only callback。C++ 公共头文件提供 schema 版本常量，示例插件和动态加载集成用例已覆盖 `event_seq` 传递。

- 验收：`cargo test -p qx-strategy --quiet` 58 个单元测试、4 个集成测试通过；CMake 完整构建和 CTest 3/3 通过；使用生成 DLL 显式运行此前 ignored 的 C ABI 宿主 integration，1/1 通过，并核对诊断态记录 `event_seq=24`。
- 边界：策略文件 worker 尚未投递独立 fill stream；fill callback 为 JSON，不是 typed C struct。position/account/lifecycle 统一合同与全宿主 golden matrix仍待完成。

### 策略成交回调携带 EventLog 序号（2026-10-26，未发布工作区）

Bar/Depth 回测现在将 authoritative `Filled` 事件的 `event_seq` 和成交时间传给 Rust `StrategyFillUpdateV2`；Python callback SDK 增加 `BaseStrategy.on_fill_update(context, update)`。策略可用 `event_seq` 做部分成交与重投去重。未升级的 Rust/Python 策略仍收到旧 fill projection。新增 V2 schema 和 golden fixture。C ABI 与 Python/外部策略文件 worker 本轮保持各自旧 fill 合同。

- 验收：`qx-strategy` 57 个单元测试、4 个集成测试通过；`qx-xingban` 全量 162 个单元测试通过，新增订单簿序号因果断言 1/1 通过；`qx-python` 5/5；Python `test_strategy_base.py` 19/19；schema JSON、registry 和 Rust 格式检查通过。
- 边界：只覆盖 Rust Bar/Depth 回测与 Python callback SDK，不代表 C ABI/策略文件 worker、供应商验收或统一 position/account/lifecycle 更新已完成。

### Python 策略类级参数 manifest 与校验器（2026-10-26，未发布工作区）

文件策略现在可以显式导出 `STRATEGY_CLASS`，在类上声明同 schema 的 `STRATEGY_MANIFEST` 和 `validate_config(config)`。inspect、预检、默认值绑定及每个搜索候选工厂仍经过既有统一 manifest 路径。模块级/类级 manifest 冲突或重复声明 validator 会直接拒绝。仓库 SMA 示例已改用 `BaseStrategy` 子类、`on_bar` 和 intent builder，参数名与严格 Rust 策略规格字段对齐；低层 Python SDK callback API 不因此宣称具备文件策略 manifest 预检。

- 验收：manifest/类策略示例/真实 Rust 回测 26/26、策略文件 CLI 13/13、BaseStrategy 17/17；Python 全量测试 389 项通过、3 项跳过；修改的 Python 文件 `py_compile` 通过。实际 CLI `inspect-strategy` 识别完整类 manifest；本地 SMA 回测产出 70 个权益点和 1 笔成交；单市场参数搜索 6/6 候选成功并产出 OOS 结果；多市场参数搜索 8/8 候选成功、2 个 OOS fold 无候选失败；架构门禁四片 227 项、无重复。
- 边界：策略文件类入口已覆盖；直接 Python SDK 参数声明和跨语言 lifecycle golden matrix 仍开放。

### W1 全失败多账户批次便携核验（2026-10-26，未发布工作区）

补齐全账户失败时的 Evidence 行为回归：两个账户均因 BarFrame 缺失失败时，仍可打包每账户的缺失输入状态，搬迁并删除来源目录后独立验证。由于这类结果没有数据集型父 RunManifest 和源代码版本身份，`--recompute` 必须 fail closed；不会把失败清单冒充已复算的成功运行。

- 验收：`app_three_entrypoints::multi_account_entrypoints::all_failed_multi_account_package_relocates_and_verifies_without_claiming_recompute` 1/1。
- 边界：支持便携验证，不支持独立复算；后续如需复算，需先定义失败运行身份 schema。

### W1 多账户联合 Bar Evidence 搬迁复算（2026-10-26，未发布工作区）

新增 `pack-multi-account-joint-run` 与 `verify-multi-account-joint-run [--recompute]`。联合父包绑定每个账户的多市场 EvidenceBundle、父级 RunManifest/RunRecord、组合/权益/成交文件及便携 spec；报告 reader 通过路径映射读取包内子证据。复算只使用迁移后的包内行情，并要求当前构建 revision、结果和 RunManifest 与冻结证据一致。路径遍历和篡改子证据均有 fail-closed 回归。

- 验收：OKX 单标的账户与 Binance 双标的账户在封装、搬迁、删除源文件后核验和复算通过；篡改子账户 EvidenceBundle 后摘要校验拒绝。CLI 集成 1/1、证据命令回放 1/1、`project_commands` 47/47、qx-app 联合账户回归 4/4；架构门禁四片 227 项、schema/registry、CLI 清单与 Rust 格式检查通过。命令面测试将 `TEMP/TMP` 指向仓库 `target/qx-test-temp` 以绕开系统临时目录权限拒绝。
- 边界：当前证据输出 `signed: false`；仅覆盖联合账户 Bar，不代表其它 RunType、供应商沙盒验收或整体发行门已完成。

### W1 多市场 Bar Evidence 支持 1–2 个市场（2026-10-26，未发布工作区）

修正回测规格与 EvidenceBundle 合同不一致：multi-market Bar 回测原本允许 1..=32 个市场，证据构建/验证却要求至少 3 个，导致合法运行无法打包和复算。Rust validator、CLI 构建与复算、schema v3 现统一接受 1..=32；multi-barframe 保持恰好两腿，多市场 Depth 保持至少两市场。

- 验收：qx-spec 多市场证据结构用例 1/1；真实 1/2 市场 CLI 证据包搬迁、删除源目录后复算 1/1；既有 3 市场迁移复算 1/1；多市场质量/Evidence 集成组 4/4；`rustfmt --check`、`git diff --check`、schema JSON 解析及架构 `--fast`（4 片、227 项、无重复）通过。
- 边界：仅修复 multi-market Bar run Evidence 的合法范围，不关闭联合多账户/全 RunType portable-recompute 和发行门。

### W1 能力预检拒绝原因进入统一错误合同（2026-10-26，未发布工作区）

`AppError` 新增稳定、可空的 `reason_code`，执行能力和账户私有查询预检被 `require_allowed()` 转成应用错误时保留其具体拒绝原因。CLI、Python 与 HTTP 仍由同一 `AppError::to_json()` 输出；`category`、`action`、重试语义和底层 `source_code` 保持各自职责，普通错误的 `reason_code` 为 JSON `null`。

- 验收：`cargo test -p qx-app --quiet` 通过；重建当前 PyO3 扩展后，`cargo test -p qx-cli --test app_three_entrypoints --quiet` 32/32 通过。错误 JSON 固定键集、空值合同及执行 scope/账户查询操作拒绝子因均有回归覆盖。
- 复核：首次跨入口运行选中了旧 Python 扩展并因旧 RunRecord schema 失败；`cargo build --release -p qx-python --features extension-module --quiet` 后，原失败用例 1/1 通过，完整入口组复跑通过。
- 边界：本轮只补本地应用层结构化输出，没有供应商外部验收，也不提升任何能力成熟度。

### W2 Python 策略诊断快照合同（2026-10-26，未发布工作区）

Python `BaseStrategy` 现在明确提供可选 `diagnostic_state()` 钩子，返回只读诊断 JSON，不作为持久化恢复快照。策略文件诊断 worker 与 Rust/C ABI 一样，将快照限制为 64 KiB；拒绝非对象、非字符串键、非有限/非 JSON 值和超限输出，避免偏差诊断因静默截断失真。

- 验收：`python -m unittest python.tests.test_strategy_base python.tests.test_callback_strategy_diagnostics` 28/28 通过；覆盖默认空值、正常快照、NaN 和 64 KiB 上限，并复核偏差诊断 worker 回归。
- 边界：此钩子服务策略偏差诊断；实时策略自动 checkpoint/restore 仍是独立的版本化合同。

### W1 多账户 Tick/OrderBook Evidence 搬迁复算（2026-10-26，未发布工作区）

新增 `pack-multi-account-depth-run` 与 `verify-multi-account-depth-run --recompute`，覆盖独立账户 L1 Tick/L2 OrderBook 批次。每个成功账户携带便携 Depth EvidenceBundle 与质量清单；失败账户的行情和质量清单分别记录 SHA-256 或原始缺失状态。复算只读取包内数据，并在隔离目录重跑整个批次。

批次指纹现在忽略本机行情、产物和质量清单路径，输入身份由账户级数据与质量内容指纹表达。打包入口先核验历史路径敏感指纹，再将便携结果与父级 RunManifest 的 config_hash 一并规范化。便携规格重定位质量清单并清除运行配置来源路径。新增 `multi-account-depth-run-evidence-v1` schema 登记。

- 验收：成功和部分失败批次均在搬迁目录、删除源行情和质量清单后独立复算通过，各 1/1；qx-app 多账户深度集成 4/4；Bar Evidence 回归 1/1；CLI Evidence 路由 1/1；嵌套 CLI 帮助 1/1；架构 `--fast` 四片覆盖 227 项、无重复。
- 边界：仅闭合独立多账户深度批次的 portable/recompute；多账户联合、其它 RunType 和 Research 发布门仍开放。离线证据不代表交易所沙盒验收。

### W0 研究成交归因事实 owner 对齐（2026-10-26）

修正 `qx-genglu` crate 文档和 `docs/module-map.md`：研究成交归因由实现 `multi_leg_group_attributions` 的 `qx-app` 持有，`qx-cli` 只做呈现 facade。新增架构门禁 D22，核对模块地图、crate 职责说明、实现符号和 CLI facade 声明一致。

- 验收：`python tools/check_architecture.py --fast` 四片 227 个具名检查全部通过，无重复。

### W1 ExchangeGateway 路由、冻结规则快照与运行态健康投影（2026-10-26，未发布工作区）

新增 qx-service ExchangeGatewayRegistry，按 venue/account 区分交易会话，关联共享行情 worker、执行、私有用户流、对账、恢复、规则目录和账户 EventLog；profile 只绑定生成目录 identity/digest，不代替 operation-specific capability preflight。`InstrumentRulesV1` 复用冻结 market-spec resolver，校验账户执行 worker、venue 和 InstrumentId 身份并固定规则 fingerprint；真实订单能力预检现消费该 fingerprint。服务 worker 入口要求 worker 存在于注册表，避免运行拓扑中出现未绑定路由；私有 worker 还验证账户 gateway 绑定了对应端口，且目标 worker 正是该端口的登记者。静态架构门禁同步登记 Gateway 的唯一 VenueFamily 读点。`GET /ready` 的 O 层按账户投影 7 个窄端口的进程心跳 `status` 和业务探针 `business_status`；Binance/CCXT 行情、私有订单流、账户余额查询、对账轮次及长驻执行 worker 的 submit/cancel 发布独立业务探针。执行请求成功为 healthy，确定性拒绝为 degraded，结果未知/需对账/内部不变量错误为 unhealthy；尚无执行调用证据的端口保持 `unobserved`。完整对账无差异为 healthy，有差异为 degraded，轮次失败为 unhealthy；账户查询在首次有效响应前失败为 unhealthy，已有有效查询但轮次未完成为 degraded。过期证据不会显示为 healthy，profile fingerprint 仍仅供诊断。

- 验收：执行探针接入后 `cargo test -p qx-service --lib --quiet` 83/83 通过；覆盖多账户/多 venue 隔离、行情共享、必需身份缺失、规则指纹篡改和标的错配拒绝，并验证探针指标编码、Venue submit 边界写入、healthy/degraded/unhealthy 分类、fresh/stale/future timestamps、账户级 `/ready` 进程/业务状态投影、部分覆盖降级和未接线端口。
- worker 入口回归：CLI worker observability 20/20、策略 worker entries 9/9 通过。Windows 系统 TEMP ACL 拒绝仓库临时目录写入，使用仓库隔离 TEMP/TMP 复跑通过。
- 收敛验收：执行、账户查询和对账探针接线后 CLI binary 全量 430/430 通过；架构 `--fast` 四片 227/227、无重复。此前门禁发现的控制命令文档名单和两个短生命周期只读 EventLog pipeline 豁免已同步，定向用例分别 4/4、2/2 通过。
- 边界：注册表记录已配置的 worker route；`status` 只证明进程近期发布心跳。业务探针目前覆盖 Binance/CCXT 行情、私有订单流、账户查询/对账轮次以及长驻执行 worker 的 submit/cancel。one-shot 入口和未接线 connector 不发布 worker 业务探针；执行结果分类以 `Venue` API 返回契约为依据，未知结果与需对账错误会 fail closed。探针不等于供应商验收，也不是单进程持有全部窄端口的客户端。动态 profile、更多连接器覆盖和供应商验收仍未完成，Live 能力不升级为 sandbox-tested。

### 策略基础函数约定与撤单 operation 独立门控（2026-10-26，未发布工作区）

经典策略接口方案新增标准基础函数清单和参数约定：参数 schema 以 manifest/StrategySpec 为唯一来源，可选 `validate_config(config)` 只做运行前的确定性跨字段校验；Rust 保持单一 `on_event`，Python 作者按需覆盖 typed market hook。对比吸收 Freqtrade 的研究/运行回调分层、Backtrader 的 warmup/执行反馈分离、VeighNa 的事件语义、RQAlpha 的交易日历阶段和 Qlib 的执行结果反馈；不提供无权威事实源的 position/account 空回调。

本轮策略接口复核更正竞品对比文档中的旧状态描述：Python `BaseStrategy.on_order` 已由版本化订单状态更新驱动；尚缺的是统一的权威 position/account 更新合同，不应把订单状态也列作未实现项。

策略文件 CLI 现在会在基线数据预检前将 manifest 默认参数写入目标策略配置；若导出 `validate_config(config)`，会以只读参数映射先校验单次运行，再对每个搜索候选在调用工厂前重新解析默认值并校验。无效组合不会开始读取/运行回测；多市场仅对所选槽应用默认值。callback SDK 仍未提供独立 class-level manifest 元数据。

`CancelOrder` 加入 Binance 与 CCXT Execution worker 的队列派发。入口校验账户、Venue、client order ID 和可执行订单；Binance 从 EventLog 恢复订单，CCXT 另恢复远端订单号与成交费用基线后再建连接器并撤单。撤单回报继续走 OMS/EventLog；多腿组同步与补偿遵循 dedicated recovery worker 配置。API 装配只将匹配的 Binance/CCXT worker 登记为撤单路由，Paper 不会受理成无消费者的命令。

执行能力目录现在对每个具体 `execution_scope` 分别声明 `submit` 与 `cancel`；能力生成器校验 operation 名称、唯一性和非空约束。应用预检提供独立撤单评估器，API 能按 `operation=cancel` 查询，Binance/CCXT 撤单在连接器启动前必须通过 cancel scope，单有 submit 不再放行撤单。控制命令服务端授权顺序也已前置于撤单 payload 语义校验，未授权请求返回 403，不因空 payload 先泄漏 400 校验结果。

能力目录 facets 与 Web 控制台筛选现已包含 operation；用户可从 `submit`、`cancel` 或账户查询操作列表中选择，筛选条件通过统一 API 传递。workspace 包数同步为 29，并新增架构门禁将 README 数量与 Cargo 成员清单对账。

- 验收：`cargo test -p qx-control -p qx-app -p qx-api -p qx-service --lib --quiet` 通过（352 项）；架构 `--fast` 四片覆盖 227 个具名检查且无重复；`python tools/generate_capability_catalog.py --check` 确认 API/Python JSON 和 Markdown 能力目录与源清单一致；Web 控制台 JS 语法检查通过。
- 策略参数回归：manifest/预检/策略文件 CLI/BaseStrategy Python 测试分别 18/18、13/13、16/16 通过；Python 源码编译检查通过。
- 边界：本地离线代码与控制面验收不代表 Binance/CCXT sandbox 或生产撤单已验证；当前能力目录将 scope 标为 `implemented-unverified`，没有外部凭据往返证据就不升级成熟度。

### 多账户 Evidence v2 支持部分失败批次的便携复算（2026-10-26，未发布工作区）

`pack-multi-account-run` 现在以 Evidence manifest v2 逐账户登记 BarFrame 路径和 SHA-256。成功账户复用经过验证的包内数据；失败账户若原始 BarFrame 存在则携带原始字节，若输入原本缺失则在清单中保留明确的缺失状态。`verify-multi-account-run --recompute` 可在隔离目录重跑完整批次，包括已捕获输入的部分失败批次；全失败批次仍因没有父级 RunManifest 而不宣称独立复算。旧 v1 包仍可离线验真，缺少失败账户输入证据的旧部分失败包拒绝复算。

- 验收：成功双账户和“一个成功、一个缺失行情”的部分失败批次均在移动包并删除 source workspace 后复算通过，各 1/1；篡改已打包输入 SHA-256、把声明缺失的输入改为存在均被拒绝；CLI 缺输入行为回归 1/1；Schema JSON 解析、定点 rustfmt、`git diff --check` 通过；架构 `--fast` 四片覆盖 226 项、无重复，109.38 秒。因更新成熟度来源，重新生成 API/Python 能力目录与 Markdown 表。
- 边界：仍要求匹配 code revision；全失败批次没有父级 RunManifest，旧版部分失败包也缺少可重放输入，因此 fail closed。供应商沙盒、全 RunType portable/recompute 和 Research 发布门继续开放。

### 单市场深度 EvidenceBundle 复算保留质量清单（2026-10-26，未发布工作区）

单市场 L2 DepthFrame 的 EvidenceBundle 在 `--recompute` 时此前没有把已校验的质量清单传入隔离回测，RunManifest 因缺少 `dataset_quality_fingerprint` 而不一致。`rerun_depth_backtest` 现在接收 verifier 解析后的质量清单路径，并将其传给深度回测入口。回归把 DepthFrame、清单和原始产物放入 source workspace，导出并搬迁 bundle、删除 source workspace 后重新复算；本地与 portable 两条路径均通过。

- 验收：`depth_manifest_is_generated_bound_reported_and_rejects_quality_drift` 1/1；`cargo test -p qx-cli --test backtest_contracts -j 1` 50/50；多市场深度 1/1；多市场质量 3/3。Windows 复验通过工作区内隔离 TEMP/TMP 运行；定点 rustfmt、差异检查和架构 `--fast` 四片 226/226（无重复，112.69 秒）通过。
- 边界：只闭合单市场 depth-backtest 质量清单传递；其他 RunType 的便携复算和 Research 发布门保持开放。

### 多市场 Bar EvidenceBundle 搬迁后独立复算（2026-10-26，未发布工作区）

多市场 Bar 的 `instrument_equity_contributions` 已作为带摘要的 ArtifactRef 进入 EvidenceBundle，但输入身份复核仍读 summary 记录的本机绝对路径。现改为从 EvidenceBundle 工件引用定位副件，再与父级 RunManifest 的 `derived:instrument-equity-contributions` 内容身份核对；重复引用会拒绝。

- 验收：三市场 BarFrame/质量清单生成运行和证据包，移动包目录并删除整个原始 source workspace 后，`evidence verify-run --recompute` 通过；`cargo test -p qx-cli --test multi_market_quality_manifest -j 1` 3/3；受影响 Rust 文件定点格式与差异检查通过。
- 边界：只修复 multi-market Bar 的权益贡献副件定位；RunType portable/recompute 全矩阵和 Research 发布门仍开放。架构 `--fast` 四片 226/226、无重复，111.38 秒。

### 多腿回测 EvidenceBundle 搬迁后独立复算（2026-10-26，未发布工作区）

修复 `evidence export-run` 的多腿回测源目录依赖：导出包内 BarFrame 与质量清单路径现在映射回复核摘要；qx-app 用已校验的组合 RunManifest 对账两腿数据指纹、两腿结果哈希和组合结果哈希。普通本地验证仍会核验腿级摘要/RunRecord；portable 验证依赖包内输入和 RunManifest，最终 `--recompute` 会完整重跑 multi-builtin 并逐字段对拍。

- 验收：真实双腿质量清单用例在包目录搬迁、删除原始输入/质量清单/运行产物后执行 `verify-run --recompute` 通过 1/1；`artifact_quality` 14/14、qx-app 多腿定向测试 16/16、multi-leg backtest 集成 10/10；架构 `--fast` 四片 226/226、无重复，114.61 秒。受影响 Rust 文件定点 rustfmt 和差异检查通过。
- 边界：`cargo fmt --all -- --check` 仍报告工作区其它既有格式差异；只覆盖 multi-leg builtin run，不代表 RunType portable/recompute 矩阵或发布门完成。

### 多腿完整报告增加版本化 HTTP 读取入口（2026-10-26，未发布工作区）

新增 `POST /app/read-multi-leg-report`，接收 `MultiLegResearchReportReadRequestV1`，经 qx-app reader 完整重放后返回 `MultiLegResearchReportV1`。ArtifactRef 受 artifact root 约束，行情/质量输入受独立 data root 约束；Python 同步/异步和 C++ AppClient 已接线，能力声明、语言 ownership、HTTP manifest 与部署端点表已更新。

- 验收：API 用真实双腿产物对拍 qx-app DTO 并拒绝 artifact-root 越界 1/1；qx-api 全单测 118/118；API capability、Python sync endpoint matrix 1/1、Python async route 1/1、CLI route matrix、Schema 解析、route manifest、Rustfmt 和差异空白检查通过；架构 `--fast` 四片 226 项、无重复，最终复跑 108.43 秒。
- 边界：本机没有 C++ 编译器，C++ 编译与 smoke 未验收；RunType 全矩阵、portable/recompute、干净发行包和 Research 发布门仍开放。

### 多账户联合报告增加版本化 HTTP 读取入口（2026-10-26，未发布工作区）

新增 `POST /app/read-multi-account-joint-report`，接收联合回测结果并调用 qx-app 共享 reader 重放核验父级与成员产物，返回 `MultiAccountJointResearchReportV1`。读取器将 ArtifactRef 约束在配置的 artifact root 内，并登记到 API capability、路由清单、部署端点表及 Python/C++ 远端 AppClient；Python 同步/async 与 C++ client smoke 均已接线。

- 验收：qx-api 全部 117 个单测通过，联合报告 dispatcher/DTO equality/越界拒绝 1/1，qx-app 联合回测兼容与三交易所集成 4/4；能力声明 1/1；Python HTTP client 路由矩阵 1/1；CLI 静态路由矩阵 1/1；HTTP route manifest 一致；Rust 格式与差异空白检查通过；架构 `--fast` 四片覆盖 226 项、无重复，墙钟 110.33 秒。
- 边界：本机无 C++ 编译器，C++ 编译未验收；单个联合报告读口不代表 R1 全 RunType 的 reader、portable/recompute、跨入口 Schema/发行矩阵完成。

### Provider 报告客户端纳入架构入口登记（2026-10-26，未发布工作区）

补齐 `maturity/language_api_contract.yaml` 中 Python/C++ Provider Bar 与 Depth 版本化报告读取 helper 的显式登记。它们是经过 qx-app 核验的报告读入口，不是新的执行用例。之前完整架构门禁据此发现了两条“未认领公开方法”；更新登记后，分片 2/4 的 333 项全部通过，随后完整 `python tools/check_architecture.py --fast` 四片通过，覆盖 226 项、无重复，墙钟 108.33 秒。

- 范围：只修复客户端入口的机读 ownership 清单并复验总门禁；R1 其它 RunType 的共享报告 reader、portable/recompute 和发行矩阵仍未完成。

### 单市场 Bar/Depth 实验包支持搬迁和包内复算（2026-10-25，未发布工作区）

新增 `pack-experiment`、`pack-depth-experiment` 与对应的 `verify-*` portable bundle 路径。实验包携带原始输入、身份 sidecar、父级 RunRecord/RunManifest 和候选工件；包内 reader 校验后可从随包输入复算完整候选网格。

- 验收：外部目录搬迁后隐藏原始行情和旧输出，仍能校验并完成整网格复算；篡改输入 SHA-256 被拒绝。`cargo check -p qx-cli -j 1`、定向 CLI 回归、缺输入行为用例通过；`python tools/check_architecture.py --fast` 覆盖 226 项且无重复。
- 边界：证据覆盖单市场 Bar 与 L1 Depth 首批路径，不代表其它深度层级、可选清单组合或其它 RunType 已关闭。

### Provider Bar/Depth 接入统一 Web 研究报告读链（2026-10-25，未发布工作区）

`/app/research-artifacts` 现在识别 Provider Bar 与 Provider Depth 外层结果，通过 qx-app Provider reader 复核来源/输入身份，再将已核验的内层报告投影到统一研究报告 DTO。API 只允许读取本次运行产物根目录中的冻结输入和工件；越界输入路径会被拒绝。Web 研究页将 Provider Depth 按深度运行展示，并显示 Provider 身份。

- 验收：Provider Bar/Depth API 读回和越界路径拒绝用例 `2/2` 通过；Rust 定向格式检查、`node --check web/console/app.js`、定向 `git diff --check` 通过；`python tools/check_architecture.py --fast` 四片完整覆盖 226 项且无重复。
- 边界：此切片只覆盖 Provider Bar/Depth 的 API/Web 读回；其它 RunType 的 portable/recompute 与 CLI/Python/API 完整 DTO 等价仍按 R1 矩阵逐项推进。

### Provider Bar/Depth HTTP reader 返回共享版本 DTO（2026-10-25，未发布工作区）

新增只读路由 `/app/read-provider-report` 与 `/app/read-provider-depth-report`，分别返回 `ProviderBacktestReportV1` 和 `ProviderDepthBacktestReportV1`。两个路由调用与 CLI/Python 同一 qx-app reader，保持同版核心报告 DTO；HTTP adapter 另对 outcome 中所有产物和冻结输入路径执行 artifact-root 授权，并在 `/app/capabilities` 声明能力。Python 与 C++ 远端 AppClient 均增加对应同步读取方法，Python 同时提供 async 包装。

- 验收：Provider API 用例 `8/8` 通过，包含 HTTP DTO 与 qx-app 原生 reader JSON 完全相等、越权 Bar manifest/Depth 输入拒绝；CLI 部署端点表逐表对账 `1/1`、Python AppClient 用例 `40/40` 通过；架构分片 1/4 与 3/4 的定向复核分别为 `378/378`、`338/338`，完整四片复跑中；受影响 Rust 文件定向格式检查通过。
- 边界：当前只闭合 Provider Bar/Depth 的 HTTP 版本化 reader；其它 RunType、portable/recompute 及全矩阵兼容迁移仍未完成。

### 单市场 Bar/Depth 实验增加完整本机候选复算入口（2026-10-25，未发布工作区）

新增 `evidence recompute-experiment` 与 `evidence recompute-depth-experiment`。复算读取持久化原始规格，要求代码修订匹配；外部策略在缺少构建材料时失败关闭。复算前后都通过 qx-app 共享 `verify_run` / `verify_depth_run` 核验成功候选产物，并对完整报告作路径无关比较。

- 验收：`cargo check -p qx-cli -j 1` 通过；CLI 新增命令的缺失输入和无副作用回归通过；端到端测试从真实 Bar/Depth 实验产物复算全部候选 1/1 通过；架构门禁四片覆盖 226 项、无重复并通过。
- 当时边界：该入口仍依赖原始输入路径，未支持 portable bundle/离线迁移。后续单市场 Bar/Depth portable bundle 已在 2026-10-25 落地，见本 changelog 对应记录；其它 RunType 的 reader、portable/recompute 与入口一致性矩阵仍开放。

### C ABI 增加 typed order-status v2 callback（2026-10-25，未发布工作区）

为 C++ 插件增加独立可选 `qx_strategy_on_order_status_v2`，把版本化订单状态、固定宽度状态码、事件序号、订单身份、旧 raw 值和 exact coefficient/scale 作为只读 typed DTO 传递。Rust 动态宿主优先使用 v2，旧插件仍通过 v1 JSON 回调运行；原有 `qx_strategy_vtable` 布局保持不变。C++ 示例实现 v2，CMake 增加结构编译合同；跨平台 CI 插件宿主用例现在会发送一次真实 typed 状态回调。

- 本地回归加强：C++ 动态插件分别记录 v1/v2 回调次数，CI 断言 v2 命中且 v1 不误调；typed DTO 允许“原因存在但内容为空”，并拒绝不一致的 presence/pointer/length 组合。
- 验收：首次运行时 Windows 默认临时目录导致 6 个文件/内存映射测试 PermissionDenied；将本次测试进程 TEMP/TMP 指向 `target/qx-test-temp` 后，`cargo test -p qx-strategy -- --test-threads=1` 通过（单测 55/55、信号集成 4/4，C++ 动态宿主用例按设计 ignored）。Rust 测试覆盖 exact decimal、v1/v2 优先级和 present-empty reason 的 typed 映射；Rust 格式检查通过。架构门禁先发现 schema 版本检查器读错常量文件，改为读取规范 DTO 所在 `qx-strategy/src/lib.rs` 后，`python tools/check_architecture.py --fast` 通过，4 片完整覆盖 226 项且无重复（111.26 秒）。生成物 `generate_unverified.py --check`、`generate_capability_catalog.py --check` 与定向 `git diff --check` 通过。本机没有 C++ 编译器，CMake/动态插件实际构建由跨平台 CI 矩阵执行，未声称本地 C++ 构建通过。
- 边界：本机没有 C++ 编译器，CMake/CTest 和真实 C++ 动态插件回调需由既有 Ubuntu/Windows/macOS CI 矩阵验收；完整 C++ lifecycle、fill、position/account typed DTO 与供应商沙盒验收仍未完成。

### 订单与控制命令 wire DTO 严格拒绝未知字段（2026-10-25，未发布工作区）

`Order`、嵌套 `OrderPolicy`、`OrderTrace`、精确数量 DTO 与 `ControlCommand` 增加未知字段拒绝；旧订单仍可省略新增的可选字段。控制 API 在写审计前拒绝拼写错误字段。整包验收同时发现并修复多市场订单状态被广播给所有策略篮子的问题：现在按唯一策略归属分发，未知或重复归属继续要求对账。

- 验收：`qx-core`、`qx-control`、`qx-xingban`、`qx-api` 全部测试通过；qx-api 117 项单测通过，多市场同步/异步端到端与研究报告回放通过。
- 边界：策略命令的未知字段拒绝改变了错字段输入的旧宽松行为；已知旧格式缺少可选字段仍保持读取兼容。此项不构成交易所沙盒或生产验收。

### Python Strategy warmup hook 顺序合同对齐（2026-10-25，未发布工作区）

对齐 `BaseStrategy` 类说明与现有 Python README、Rust/Python Bar host 和策略接口专项方案：前 N−1 根 Bar 更新策略状态并抑制订单意图；第 N 根 Bar 可见时先调用 `on_warmup_complete`，再调用该 Bar 的策略 hook。零预热仍在 `on_start` 后通知。实现时点保持不变，避免打破已存在的首个 ready 决策语义；新增回归明确校验处理顺序与 warmup 时的 Bar 计数。

- 验收：`python -m unittest discover -s python/tests -p test_strategy_base.py` 16/16 通过。
- 边界：这只明确 Python Bar callback helper 的 warmup 合同；Rust Native、Tick/OrderBook/Live host 的统一 readiness 状态机仍未完成。

### `i128` raw JSON 字段启用任意精度整数往返（2026-10-25，未发布工作区）

Workspace `serde_json` 启用 `arbitrary_precision`，修复 `*_raw: i128` 超过 `u64::MAX` 时可能被 JSON `Value` 浮点化而改变数值的问题。回归覆盖 `u64::MAX`、大于边界的非整百万值以及 `i128::MIN/MAX`；策略订单契约另用 `qty_raw` 实际字段验证精确解码和编码。对应确定性核心精度缺口已从成熟度限制中关闭。

- 验收：精确 raw JSON 定向用例通过；`qx-core` 108 项单测及其集成用例全部通过；`cargo check --workspace --all-targets` 通过。
- 边界：这是 JSON 整数解析精度修复，不代表各交易所 API 的业务限额、合约精度档案或实盘沙盒验收已完成。

### 竞品与架构重构决策主方案、联合账户离线 HTML（2026-10-25，未发布工作区）

新增一份聚合决策主方案，集中记录 GitHub 竞品的优势和适用边界、当前模块架构、源码级风险、目标依赖边界、R0–R6 重构顺序及 Research/Live 独立发布门，并在项目文档索引中加入入口。

Python `qianxing.reports` 新增联合多交易所账户 Bar 回测离线 HTML writer。它在生成页面前调用 Rust 已核验读模型，可比较账户汇总、浏览有界权益/成交/事件样本并导出 CSV；支持逐账户请求事件页，不生成跨所全局事件次序或跨币种合并值。

- 验收：联合 HTML 用例 3/3；Python 分层/Schema 用例 42/42；`py_compile`、内嵌 JavaScript `node --check`、能力目录生成器 `--check`、架构 `--fast` 四片 226/226 和 `git diff --check` 通过。
- 范围：这是联合账户回测报告，不是多账户参数实验候选归因；不代表包发行、供应商沙盒验收或实盘批准。

### C ABI v2 增加可选衍生品订单政策扩展（2026-10-25，未发布工作区）

为 C++ 策略增加 `qx_strategy_get_order_policy_v1` 可选导出，按 intent_id 返回保证金模式、持仓模式和杠杆；没有导出该符号的既有 v2 插件维持原默认行为。Rust host 限制 JSON 长度并拒绝未知字段、非法模式、零杠杆、部分字段、未知 schema 版本、非法 UTF-8 和嵌入 NUL。扩展不改变 `qx_order_intent`、`qx_strategy_vtable` 的 ABI v2 布局。

- 新增 JSON Schema 与 golden fixture，并登记到 `maturity/schema-registry.json`；C++ 示例现在真实返回一条 derivative intent，Rust host 集成测试会检查 intent 与杠杆/保证金/持仓政策的映射。
- 验收：`cargo test -p qx-strategy` 的 53 项单测与 4 项信号集成测试通过；另用刚构建的 Windows DLL 执行原本 ignored 的 C++→Rust 动态插件宿主用例 1/1；CMake 构建及 CTest 2/2、安装后下游 SDK consumer 构建/CTest 1/1、Python 策略合同 16/16、schema registry CLI 只读回放 1/1、能力目录生成器 `--check`、架构 --fast 4 片/226 项全部通过。
- 修正发现：真实读回测试发现既有 `strategy-order-status-update-v1` 将 `compatibility` 写成 registry enum 不支持的 `additive`，导致 schema registry 不能解析；已更正为 `forward` 并把 registry_version 提升到 2，CLI 回放现已通过。

### 联合多账户父产物 envelope schema 与 RunRecord ArtifactRef 对齐（2026-10-25，未发布工作区）

明确拆分了联合组合文件的 envelope v2、嵌套 `MultiAccountJointBacktestOutcome` DTO v1 与 RunRecord v2。新 ArtifactRef 使用独立 envelope kind/schema v2，报告读侧对外层版本执行 fail-closed 核验；旧 ArtifactRef 的误标 v1 仅在摘要、字节长度、SHA-256、父 RunManifest 与结果身份全部核对后兼容，并在同身份重跑时原子升级。新增 envelope JSON Schema 并登记 schema registry / migration / app use-case 元数据。

- 验收：联合回测用例覆盖新工件类型和 schema 版本、旧 ArtifactRef 校验后升级及父报告旧格式升级；RunRecord 单测覆盖当前版本接受和未来版本拒绝；Python schema layering 核对 Rust writer/verifier 常量、schema 与 registry。
- 边界：该切片只修复联合父产物版本语义，不代表其余 RunType envelope、portable/recompute 或真实交易所验收已完成。

### 多市场 Bar 参数实验接通可复算产物身份（2026-10-25，未发布工作区）

多市场 Bar 参数实验现在为去除本地数据路径影响的规范输入生成 CanonicalRunSpec，汇总每个 BarFrame 与可选质量清单指纹，并持久化 RunManifest、RunRecord V2、不可变结果报告和原始实验规格 sidecar。相同 BarFrame 移动目录后可按规范身份复用已核验报告，内容变化仍以 Conflict 拒绝。RunRecord 对 sidecar 的 SHA-256 与字节长度进行绑定；结果读取会核验规范身份、数据组成、结果摘要和 sidecar，拒绝输入或报告篡改。既有稳定报告在身份兼容时可升级为新 envelope，不必重跑候选。

- Rust 回归覆盖网格结果、同身份复用、BarFrame 移动目录后复用、原路径数据内容变化拒绝、sidecar 篡改拒绝、旧报告升级和 RunManifest/RunRecord 读回；CLI、HTTP、Python 共享同一元数据并检查 sidecar 绑定；Python schema 与 AppUseCase 合同同步更新。
- 边界：此切片补齐多市场 Bar 参数实验的可复算输入证据；其它 RunType 的覆盖、portable/recompute 和外部供应商验收仍按架构方案逐项完成。

### 多市场 Depth 参数实验绑定原始输入规格（2026-10-25，未发布工作区）

多市场 L1/L2 参数实验现在将规范实验输入规格保存为相邻 sidecar，并在结果 `artifact_locations.experiment_input` 和 RunRecord V2 中登记其 SHA-256、字节长度及 schema 身份。复用旧的两工件实验记录时会先验证旧报告，再发布带 sidecar 的新不可变报告/记录；新旧结果 schema 均兼容。

- 回归覆盖 sidecar 文件存在、RunRecord 内容摘要/长度绑定、旧报告升级、同规格幂等重用和报告篡改拒绝。
- 边界：这里只补齐多市场 Depth 实验输入规格的可追溯绑定，不代表其它实验类型、portable/recompute 或完整发布门全部完成。

### 多腿参数实验绑定输入规格并延伸到可搬运包（2026-10-25，未发布工作区）

多腿参数实验此前虽保存规格 sidecar，但结果 RunRecord 仅绑定报告和 RunManifest，导致普通读取无法从 RunRecord 验证候选网格、策略声明和输入路径身份。现在实验报告公开 `artifact_locations.experiment_input`，RunRecord V2 记录 sidecar 内容摘要/长度；历史双工件 envelope 经验证后升级为 input-v1 不可变报告。portable bundle 使用包内 `spec.identity.json` 作为同一输入工件，离线校验沿用相同 RunRecord 逻辑。

- 更新 Rust/Python 结果 Schema 和 use-case 清单，保持旧两路径结果可读；新增 sidecar 存在性、摘要/长度和篡改拒绝检查。CLI 跨入口用例同时打包、搬迁并整网格复算 portable 包。
- 验收：qx-app 多腿实验 8/8；CLI/HTTP/Python 多腿实验与 portable 打包/搬迁/复算 1/1；Python AppUseCase 与 schema 合同 2/2；Python 原生扩展重建通过。
- 边界：本切片覆盖多腿参数实验本地产物和 portable bundle 输入规格绑定；其他 RunType 的输入矩阵、供应商验收与全局发布条件仍开放。

### 单市场 Bar/Depth 参数实验保存可复算输入规格（2026-10-25，未发布工作区）

普通 Bar 与 Tick/OrderBook 参数实验此前有候选结果和 CanonicalRunSpec，但没有持久化完整原始实验规格，无法从产物还原参数网格及其输入文件声明。现在新增版本化规格 sidecar，包含精确 `spec_json`、代码修订和可选策略源码身份，并将 sidecar 的 ArtifactId/字节长度登记在 RunRecord `experiment_input` 工件中；校验器按记录中的摘要和长度验证 sidecar 内容，并核验规格类型、实验 ID、规范 JSON 与策略身份。

- 旧 RunRecord 的两工件布局仍可读；既有报告重用时先验旧记录，再发布带 sidecar 的新不可变报告和记录。输入/输出路径不参与 sidecar 重用比较，仍由 BarFrame/DepthFrame 内容指纹约束实际数据，因此相同数据搬迁可重用、内容变化拒绝。
- 验收：qx-app Bar 参数实验 25/25、Depth 参数实验 7/7；覆盖旧报告升级、输入文件路径搬迁、sidecar 完整性损坏拒绝和 RunRecord/RunManifest 读回。
- 边界：此切片补齐两类实验的原始规格持久化与 sidecar 完整性；portable 打包、离线完整网格复算、共享实验报告 reader 及其它 RunType 矩阵仍开放，不能据此关闭 R1。

### Provider Bar/Depth 报告读取接通 CLI 与 Python（2026-10-25，未发布工作区）

CLI 新增的 `app read-provider-report`、`app read-provider-depth-report` 与 Python SDK 报告读取入口共用 qx-app 版本化报告 DTO。Bar 报告将 Provider 外层身份绑定到已验证摘要中的冻结 manifest/lineage；Depth 报告重读冻结 DepthFrame 并重新核验 query、request、source 和输入指纹。Provider Depth 结果 schema 改为引用独立 `depth-backtest-outcome-v1.json`，避免内嵌副本与 standalone outcome 漂移；Python schema 验收使用本地 registry，不访问网络。

- 验收：qx-app Provider 定向 8/8；Python Provider SDK 与 SDK/schema layering 48/48；CLI 集成 1/1，真实 Bar 和 L1 Provider outcome 分别经 CLI、Python 读取并比较完整 DTO 相等；命令清单与能力目录已重新生成；能力目录 `--check` 通过；架构 `--fast` 四片完整覆盖 226/226 项、无重复。
- 边界：测试使用内存 Provider fixture，不代表外部供应商或交易所验收；R1 其余 RunType、portable/recompute 和发行矩阵仍开放。

### 单标的 Depth 报告拆分运行身份与清单身份（2026-10-25，未发布工作区）

修复 `qx-cli report <Depth summary>` 把用户可读的 `run_id` 与离线证据契约中的 `manifest_run_id` 当成同一身份的问题。`DepthBacktestOutcome` 新增可选、向后兼容的 manifest 身份字段；qx-app 分别核验摘要运行身份和 RunManifest 身份，CLI 从真实摘要分别装配两者。旧 outcome 缺少该字段时仍按历史 `run_id` 规则验证。

- 新增深度质量清单报告回归，明确断言两个身份值不同；篡改摘要中的 manifest 身份会因独立 RunManifest 不匹配而拒绝。
- 验收：CLI 回测契约 50/50；新增篡改路径定向回归 1/1；CLI/HTTP/Python Bar 入口 30/30；深度跨入口 3/3；qx-app 深度回测 10/10；Depth outcome 旧 wire 兼容 1/1；Python SDK schema/DTO 39/39；Python→Rust 带双身份 Depth 结果端到端 1/1（先重建 PyO3 扩展）；架构 `--fast` 四片 226/226；改动 Rust 文件 `rustfmt --check` 与 `git diff --check` 通过。
- 边界：只修复单标的 Depth 报告的双身份核验；R1 的其他 RunType、portable/recompute 与整体发布门仍开放。

### 单标的 Depth 交互报告复用 qx-app 核心复核（2026-10-25，未发布工作区）

标准 `qx-cli report <Depth summary>` 现在从摘要装配稳定的 `DepthBacktestOutcome`，调用 qx-app `read_depth_run_report_v1_with_reader` 统一复核 RunRecord、RunManifest、权益/成交 sidecar 和结果摘要，再继续生成 JSON、HTML、XLSX 或 Evidence。报告要求所选摘要路径与其 `artifact_locations.summary` 一致，并在 JSON 中返回 `run_kind=depth` 与共享 `verification`。

- 验收：CLI Depth 跨入口集成 3/3；其中新增标准交互报告成功读回，以及改写 `return_bps` 后报告拒绝；定向 rustfmt 检查通过。
- 边界：统一的是单标的 Depth 核心事实验证；API 仍提供额外事件/盘口投影，所有 RunType 的交互报告、portable/recompute 和发行门仍未关闭。

### 多市场 Depth 完整研究报告统一读取链（2026-10-25，未发布工作区）

新增 qx-app 版本化 `MultiMarketDepthResearchReportV1`，在一个应用层读模型中复核多市场 L1/L2 输入、质量记录、执行假设、RunManifest、EventLog/共享账簿、权益曲线和成交 sidecar，再返回受界限约束的每市场深度预览及权益/成交/事件面板。qx-api 改为适配同一 DTO；qx-cli `app read-multi-market-depth-research-report`、交互式 `report <产物>`、Python 原生 API 与 `qianxing_bridge` 均消费同一 reader。CLI 报告投影继续展示来源、内容哈希和已验证的质量清单面板。

- 登记请求/响应 JSON Schema、语言 API helper、CLI 行为证据与能力目录；完善 CLI/Python 对同一真实双市场 Depth 运行的完整 DTO 相等回归。
- 验收：本轮 qx-app 多市场深度用例 6/6、CLI 质量清单/Evidence/XLSX 回归 1/1、CLI/Python 完整 DTO 对拍 1/1、Python extension-module build、真实双市场报告 Schema Draft 2020-12 校验及 capability catalog `--check` 均通过。新增回归确认 CLI 对用户实际选中的摘要文件做验证；将质量清单改绑到内容相同但路径不同的副本会被拒绝。架构 `--fast` 四片完整覆盖 226 项且无重复（109.68 秒）；历史 API 读回与 Python facade 验收见前序记录。
- 环境限制：默认 qx-cli 首次在本机 SQLite/MSVC C 编译步骤触发 D8050；关闭 Rust 开发调试符号后同一 CLI 跨入口测试通过。此证据不代表默认发行构建通过。
- 范围：关闭多市场共享账户 L1/L2 的核心富报告读取链，不代表所有 RunType 报告、发布矩阵、真实供应商验收或整体目标完成。

### 多市场 Bar 完整有界研究报告共享 DTO（2026-10-25，未发布工作区）

新增 qx-app 版本化 `MultiMarketResearchReportReadRequestV1` / `MultiMarketResearchReportV1`，在读取时复核运行产物、BarFrame/质量清单身份、RunManifest、EventLog 重放、共享权益与成交 sidecar、逐标的权益贡献，再返回有界权益、成交、事件和每市场 Bar 面板。API 使用分开的 artifact/data rooted reader 维持路径授权；CLI 与 Python 暴露同一核心 DTO。事件页只附加当前因果可见的 Bar 上下文，逐市场处理完成后释放完整 BarFrame。

- 报告响应最多 1,000 个权益点、200 笔成交、300 条事件、每市场 1,000 个 Bar/贡献预览，以及 100 条资金费率/保证金阶梯预览；完整资金费率和保证金序列保留总数与 SHA-256，市场规则提供精确 JSON 字符串，避免展开超长序列或丢失 i128 精度。
- 已登记请求/响应 JSON Schema、schema registry、Python native helper 和 CLI 行为证据。跨入口回归要求 CLI/Python 对同一实际运行返回相等 DTO，并验证各面板上限与因果上下文。
- 验收：四 crate no-default-features `cargo check`、qx-app 采样/摘要 2/2、qx-api 重放与市场面板 1/1、真实产物 CLI/Python DTO 对拍 1/1、Python extension-module build、新报告 JSON Schema 实例校验与 capability `--check` 均通过；架构 `--fast` 四片 226 项完整覆盖且无重复（122.83 秒）。等距采样修复了旧步长在 2,001 个点时仅返回 668 行的问题。
- 验收范围：此切片关闭多市场共享账户 Bar 的完整报告 DTO 入口，不代表其它 RunType 全覆盖、portable/recompute 矩阵、完整发行矩阵或供应商实盘验收完成。

### 标准策略函数、经典框架对标与目标仓位构造器（2026-10-25）

更新[策略接口专项方案](../牵星Qianxing-标准策略接口经典框架对比与改进方案-2026-10-25.md)，补齐策略作者所需的最小函数集、推荐生命周期顺序、回测/研究/实盘边界，以及 Freqtrade、Backtrader、VeighNa CTA、RQAlpha 和 Qlib 的可吸收优点。结论是一个必需决策入口，生命周期、warmup、执行事实、session hook 均可选；向量化信号 adapter 与事件型 live strategy 分开。Python BaseStrategy helper、warmup、fill-to-on_trade 和 intent builder 已有代码；跨 host readiness、完整订单/持仓/账户 updates、session、checkpoint 与向量化 adapter 仍待实现。BaseStrategy 回归本轮 15 项通过。

补充策略基础函数清单和参数设计：使用 manifest/StrategySpec 参数 schema 作为 CLI、Web、预检和搜索的唯一来源，`validate_config(config)` 只负责可选的跨字段纯校验；Rust 仍只要求单一 `on_event`，Python 用户覆盖实际使用的 typed hook。明确 position/account 回调仍需等待权威事实合同。

策略文件 SDK 新增 StrategyOutput.for_target_position(...) 与 for_intents(...)，自动绑定 request identity，减少手工复制字段；输出与嵌套 intent 的 JSON 读取现在拒绝把 bool、float、字符串隐式转换为协议整数/布尔字段，并验证 Rust 对应的 i128/u64/i32/u32 范围及 schema 必填键。小币种目标继续使用 exact decimal 字符串，运行时仍负责仓位差额与交易规则。

- 验收：python -m unittest python.tests.test_strategy_contract：27 项通过，覆盖 raw/exact signed target、identity-bound builder、多个 intent、JSON 整数类型强校验和旧 v1 JSON round-trip；全量架构不变量检查 1,361 项通过。
- 边界：Python 文件协议构造器不等于 callback BaseStrategy 的目标仓位 helper；后者需等权威持仓、one-way/hedge position-side 语义稳定后再加。

### 多腿完整有界研究报告共享 DTO（2026-10-25，未发布工作区）

新增 `MultiLegResearchReportV1` 与版本化读请求，统一返回运行/策略/数据身份、共享账本摘要、逐腿收益与资金费、汇总成本、权益指标、精度安全的 raw 字符串、执行费用/撮合/资金费假设、两腿 Bar 与质量身份，以及最多 1,000 个权益点、最新 200 笔成交和最多 300 条事件。市场规格以 JSON 文本携带，避免被只支持 IEEE-754 Number 的消费端先行舍入。

`qx-cli app read-multi-leg-research-report`、Python `read_multi_leg_research_report_v1` 与 qx-api 多腿报告 adapter 消费同一 qx-app DTO；原有 facts-only CLI/Python 入口继续保留。新增请求/响应 JSON Schema、schema registry、Python capability catalog 与架构命令行为证据登记。

- 验收：CLI/Python 对同一真实双腿运行完整 DTO 对拍 1/1，包含越界分页拒绝；qx-api 多腿报告回归通过；qx-app 多腿定向测试 5/5；qx-app/qx-api/qx-cli/qx-python `cargo check --no-default-features`、Python extension build、Python app use-case 40 项、SDK layering 39 项、capability catalog `--check` 和 JSON schema parse 通过。
- 边界：这里只共享多腿核心报告事实；API 仍负责 HTTP 展示投影与 Bar 因果上下文；诊断附件、其它 RunType 全面 reader/portable/recompute、完整报告发布矩阵仍开放。

### CLI 多账户 Depth 富报告接入 qx-app 报告读取与事件分页（2026-10-25，未发布工作区）

CLI `report <MultiAccountDepthBacktestResult>` 现在识别 Depth 批次，使用 `read_multi_account_report_v1` 读取已核验的父批次和子账户核心事实，再通过 qx-app 的 `read_event_report_v1_with_reader` 有界回放/分页 EventLog。Depth HTML/XLSX 报告包含 tier、snapshot 数、成交、净值、事件及账户汇总；输出路径在生成 EvidenceBundle 等副产物前校验冲突。成交和权益明细截断标记合并反映应用层与 CLI 两级采样。

- 回归：多账户 Depth CLI 富报告 JSON/HTML/XLSX 集成 1/1；Bar CLI/API/Python 共用入口回归 1/1；定向 Rust 格式检查通过；架构 `--fast` 通过，4 片覆盖 226 个具名检查且无重复（112.36 秒）。
- 边界：交互报告各 RunType 的完整 reader 收口、portable/recompute 全矩阵、容量画像、默认 feature/workspace 与发行门仍开放；本地用例不证明交易所外部验收或发布完成。

### CLI 多账户 Bar 富报告消费 qx-app 批次报告 DTO（2026-10-25，未发布工作区）

`qx-cli report <MultiAccountBacktestResult>` 现在先调用 `read_multi_account_report_v1` 统一验证父批次、逐账户核心报告和重算汇总；HTML/XLSX 适配器从该 DTO 读取账户 summary、Run 身份、数据指纹及批次汇总。删除本地批次计数检查、重复父 manifest 校验、逐账户 `verify_run` 和直接重读 summary 的身份判断。成交、事件和净值明细仍由 CLI 装配为交互报告。

- 回归：跨入口集成比较 rich report 与 CLI core DTO 的批次汇总及账户数据指纹，并篡改 equity Artifact 后确认报告拒绝；多账户 Bar 定向用例 1/1、Depth core reader 1/1、架构 `--fast` 226 项通过。
- 边界：这只统一多账户 Bar 的核心报告事实；CLI 成交/事件明细分页与其它 RunType 富报告仍需继续迁移。

### CLI 多市场 Depth 报告复用 qx-app 输入与质量事实（2026-10-25，未发布工作区）

`qx-cli report` 的多市场 Depth 分支现在按 spec 市场集合重新装配输入，复用 qx-app 的 `verify_multi_market_depth_input_v1` 和 `multi_market_depth_input_set_identity_v1` 复核 DepthFrame 身份、L1/L2 形状、质量清单、连续性统计、整体数据指纹与 RunManifest 组件。CLI 自有的质量清单重算逻辑已删除；spec 和产物中的本地输入路径必须指向同一文件。

- 回归：多市场 Bar/Depth 报告集成 2/2 通过；新增将内容完全相同的质量清单复制到另一条路径后改写产物引用，报告会因该路径未绑定 spec 而拒绝。
- 边界：只统一 CLI 多市场 Depth 输入/质量身份复核；其它 RunType 报告、portable/recompute 全矩阵、容量画像、默认 feature/workspace 与发布门仍开放。

### CLI 多市场 Bar 报告复用 qx-app 输入与质量事实（2026-10-25，未发布工作区）

`qx-cli report` 的多市场 Bar 分支现在用 qx-app 的 `validate_multi_market_quality_manifest_set_v1`、`verify_multi_market_bar_input_v1` 和 `multi_market_input_set_identity_v1` 复核市场集合、BarFrame 输入身份、质量清单内容/路径、缺口统计、数据指纹与 RunManifest 输入组件。CLI 内重复的 DatasetManifestV2/SHA/缺口计算及本地 FNV 输入组合已删除；标的冻结规格与末根收盘价使用同一个验证结果。

- 回归：多市场 Bar/Depth 报告集成 2/2 通过；新增同内容质量清单改绑到新路径后拒绝的负向用例；架构 `--fast` 通过 4 分片、226 个具名检查。
- 边界：只统一 CLI 多市场 Bar 的输入与质量事实；单市场、账户组合、多腿及全部 portable/recompute 矩阵仍需逐项盘点。

### CLI 多市场 Bar 报告统一使用 qx-app 事件与账簿事实（2026-10-25，未发布工作区）

`qx-cli report` 的多市场 Bar 分支不再自己调用 `ReplayVerifier` 重放 v1 内嵌事件，也不再以本地逻辑复核按标的权益贡献。v1 legacy 与 v2 JSONL 都从 qx-app reader 取得已复核的 ledger；CLI 通过同一 `verify_multi_market_pnl_contributions_v1` 重新核验贡献轴、共享权益守恒，并使用最终 Bar 收盘价和回放账簿复算末尾各标的估值。报告投影仍维持旧版 v1 的全事件输出和 v2 的有界事件页。

- 回归：`cargo test -p qx-cli --no-default-features --test multi_market_quality_manifest -j 1` 通过（2/2，包含深度证据回归）；多市场 Bar 用例覆盖 v1 legacy 转换、v2 流式报告，以及保持组合权益和 sidecar 指纹有效但篡改末值归因后的拒绝。
- 边界：只收敛 CLI 多市场 Bar 这条报告事实路径；其它 RunType、portable/recompute 全覆盖、完整容量画像、默认 feature/workspace 发布门和外部 venue 验收仍未完成。

### 多市场 v1 内嵌 EventLog 报告复核迁入 qx-app（2026-10-25，未发布工作区）

新增 `read_multi_market_legacy_event_report_v1`，让 qx-app 统一复核旧版多市场 Bar/Depth 产物中的内嵌 EventLog、digest、重放账簿、有限分页、成交计数与序列摘要，并建立订单到标的索引。相同订单若被不同事件关联到不同标的现在会拒绝，避免 API 原有 `collect::<BTreeMap<_, _>>()` 静默覆盖冲突归属。旧版多市场 Depth 成交 sidecar 的逐行兼容比对也迁入 `verify_multi_market_legacy_event_fill_sidecar_v1`；报告文件读取、路径授权、页面参数与 Bar 上下文仍留在 API adapter。

- 回归：qx-app `report_read::event_report_tests` 10/10、legacy Depth sidecar 校验通过、qx-api 多市场 Bar/Depth 报告与路由 6/6 通过。
- 边界：这里只迁移 legacy multi-market 事件事实；全部 RunType 的统一 reader、交互报告、portable/recompute、容量画像、全 workspace 和外部沙盒仍未验收。

### 多腿研究报告聚合事实 DTO 接入 qx-app（2026-10-25，未发布工作区）

在已有多腿账簿、成本和权益核验函数上新增 `MultiLegResearchReportFactsV1`，将请求 spec、outcome、产物声明、EventLog replay Ledger、两份 BarFrame 与按腿成交合并为一个可复用报告事实。EventLog digest/账簿对账、v4 event hash/clock 核验、订单标的索引和逐腿 Fill 提取现由 qx-app reader 统一处理；该用例还绑定账户/标的身份与冻结市场规格，并核验逐腿成交数/指纹、逐腿收益和资金费、组级费用/资金费/成交额、pending reconciliation、共享权益曲线及组合收益/回撤。API 负责授权文件读取、Bar 上下文与交互投影，已移除自己的多腿重放及 Fill 归属解析。

- 回归：qx-app `report_reader` 4/4、qx-api 多腿报告及路由 10/10、多腿回测 11/11 通过；其中新增逐腿收益产物篡改拒绝回归通过。能力目录 drift check、目标格式/差异检查及架构 `--fast` 226/226 通过。
- 边界：此 DTO 先覆盖多腿事实，不等于所有 RunType 共用完整 qx-app reader；容量画像、完整 workspace 和发布验收仍未完成。

### 多腿报告 reader 接通 CLI 与 Python（2026-10-25，未发布工作区）

新增 `qx-cli app read-multi-leg-report <MultiLegBacktestOutcome.json>` 与 Python `read_multi_leg_report_facts_v1(outcome)`。两个入口读取 qx-app 的版本化聚合报告事实；跨入口回归会写出同一 outcome、分别调用 CLI 与 PyO3 bridge，并比较返回 JSON。CLI 命令也加入 architecture behavior-coverage 映射，Python doctor catalog 登记 `report.read.multi-leg.facts.v1`。

多腿 Bar EventLog 增加 qx-app 有界分页读模型，先完整复核运行产物、BarFrame、质量清单、EventLog 与共享账本，再返回同一版本化页面 DTO。CLI、Python、API 使用共享分页和身份事实；API 保留授权后的 artifact/data 独立根目录，并只为当前页面构建可见 Bar 上下文。

- 回归：qx-app 报告回读 1/1、qx-api 多腿报告 1/1、CLI/Python native 报告一致性 1/1、Python doctor catalog 1/1；`cargo check -p qx-app -p qx-cli -p qx-python --no-default-features -j 1` 通过。
- 边界：Python 完整用例文件未跑完（环境缺少 `pytest` / `jsonschema`）；全 workspace、发行矩阵、所有 RunType 报告和外部沙盒验收仍开放。

### 多腿报告事实核验迁入 qx-app（2026-10-25，未发布工作区）

将多腿报告读侧的初始资金 checked 加总、逐腿成交数量与 FNV 摘要核对、pending-reconcile 成交额求和，以及权益曲线时间顺序/首尾权益/组合收益/最大回撤核验统一到 qx-app `multi_leg::report_reader`。API 在完成授权读取与 EventLog 重放后调用这些版本化应用事实函数，不再维护对应的第二份算法；报告字段和历史产物形状保持兼容。

- 回归：qx-app `report_reader` 单测 4/4、qx-api 多腿报告匹配回归 9/9、qx-app 多腿回测集成 11/11 通过。
- 边界：多腿事件上下文、诊断绑定及剩余产物装配仍由 API adapter 负责；全 RunType 共用 DTO、完整 portable/recompute 和容量画像继续未完成。本条不代表完整报告读链或发布门通过。

### 共享报告账务与组合指标统一到应用层（2026-10-25，未发布工作区）

多腿回测产出和报告重读现在复用 `qx-app::multi_leg::report_facts` 的共享账户账簿摘要构造器/校验器。API 在重放 EventLog 后验证产物声明中的账户 ID、初始资金、现金余额、两腿持仓、已实现盈亏、账簿行数及重放标志，并用通过复核的 canonical 摘要构建交互报告；API 不再维护第二份账簿 JSON 计算。标的腿数不等于 2 时拒绝，保持已发布字段与原始精度字符串兼容。

权益曲线最大回撤的整数运算也统一到 `qx-app::report_analysis::equity_max_drawdown_bps_v1`，多腿、多市场与多市场深度报告都调用同一实现。API 保留 JSON 行结构和时间顺序校验。溢出按满额回撤保守处理，避免不同 adapter 的指标口径漂移。

多腿报告的总费用、总资金费、总成交额和成本 bps 复算，以及分组费用、残余费用、资金费、分组成交额与待配对成交额的守恒核对，统一到 `qx-app::multi_leg::report_facts::verify_multi_leg_cost_facts_v1`。API 只解析产物声明并映射错误，不再自行计算这些账务指标。

多腿和多市场深度报告由 `qx-app::report_analysis::equity_total_return_bps_v1` 统一复算权益总收益；同模块的最大回撤函数也用于多腿、多市场和深度报告。应用层统一检查非正初始权益和整数溢出，API 仍校验权益行与报告声明。

多账户 Bar/Depth 批次生产、结果复核和 API 报告使用同一个 `WeightedReturnAccumulatorV1` 汇总成功账户的本金加权收益，确保分子/分母溢出和整数取整规则一致，全失败结果仍为 `None`。

多账户 Bar schema-v2 API 报告现在也通过产物根目录约束下的 `RootedArtifactReader` 调用 qx-app reader-aware verifier，核验父级 `RunManifest` 与成功子运行身份；Depth 保持同一边界。新增篡改父清单 `config_hash` 后 API 必须拒绝报告的回归用例。缺少父清单的历史 Bar schema-v1 结果继续按兼容路径读取。

Python SDK 新增 `read_depth_run_report_v1`，经 PyO3 调用同一个 `qx-app::read_depth_run_report_v1`，返回与 Bar 一致的版本化核心 DTO、有界权益/成交样本及验证结论；已登记到 bridge 的能力入口、报告分层与语言 API 契约。

Rust CLI 新增 `qx-cli app read-report <BacktestOutcome.json>` 与 `read-depth-report <DepthBacktestOutcome.json>`，与 API/Python 调用同一 qx-app 版本化 reader。Bar 和 L1 集成回归将同一份 outcome 交给 CLI、Python reader，比较完整 `ResearchRunReportV1` JSON，证明摘要、核验状态及精确权益/成交样本一致。

多账户 Bar/Depth 报告中成功/失败计数、成交总数、本金加权收益、声明 summary 比较和父报告子身份指纹由 `qx-app::verify_multi_account_research_facts_v1` 统一复算。API 只传入已完成子报告复核的账户事实，跨账户事件分页及交互呈现仍由 API adapter 负责。qx-app 覆盖部分成功、全失败、篡改 summary、重复账户和溢出；Bar 与 Depth API 报告回归均通过。

多账户 Bar/Depth 新增统一 `qx-app::read_multi_account_report_v1` 读模型及 reader 注入变体：父批次身份按需通过受限 `ArtifactReader` 复核，每个成功账户再使用既有 Bar/Depth reader 复核，最后返回有界版本化批次 DTO。API 在 artifact root 授权边界内调用该 reader，并将已验证子 DTO 交给现有账户明细/事件分页 adapter；CLI 提供 `app read-multi-account-report` 与 `app multi-account-depth-backtest`；Python PyO3、`qianxing_bridge.app` 与公开 `qianxing` SDK 都调用相同 qx-app 入口。

- 本切片回归：多账户 Bar/Depth qx-app 6/6、4/4，API rooted Bar/Depth 报告 2/2，Python 原始与 typed Bar/Depth SDK 2/2；CLI/Python Bar 批次完整 DTO 相等，CLI Depth 批次运行后报告读回通过。首次跨入口回归发现 bridge facade 漏包装函数，补齐后通过。能力目录与 CLI 清单已重生成且 drift check 通过；架构 fast 门禁 4 分片完整覆盖 226 项、无重复。此记录不代表所有 RunType、容量画像或发布验收完成。

Python L1 报告端到端验收暴露 Depth 参数实验先验证 RunRecord/RunManifest、后发布稳定索引的顺序缺陷。现已改为先发布不可变报告与 RunRecord，再写稳定索引，最后调用 qx-app 标准读回复核；已有结果在幂等读取时也会复核其登记身份。`cargo test -p qx-app --test depth_experiment` 7/7；Python L1/typed SDK 端到端、bridge doctor、SDK 分层合计 40 项通过；`cargo check -p qx-python`、Windows editable 扩展重建、能力目录校验及架构门禁 226 项通过。

- 验证：`cargo check -p qx-api --no-default-features` 无警告；qx-app 报告事实单测 4/4、报告分析单测 6/6、多腿回测集成 11/11、多账户 Bar/Depth 集成 6/6 与 4/4、多腿/多市场深度/多账户报告读回用例及 API 库测试 113/113 通过；多账户 Bar 报告篡改父 `RunManifest.config_hash` 的拒绝用例通过；`tools/check_architecture.py --fast` 四片完整覆盖 226 项、无重复。
- 尚未完成所有 RunType 的统一报告读用例、路径合同复核和完整容量画像；Bar/Depth 核心 DTO 已接入 API、Python 与本轮新增的 CLI `app read-*` 入口。本项仍为未发布工作区改动。
- 本轮共享读链验证补充：`cargo check -p qx-cli --no-default-features` 通过；Bar 三入口报告回归 1/1、L1 Depth 三入口报告回归 1/1 通过，均用同一 outcome 比较 CLI 与 Python 全 DTO。Depth 临时夹具固定在仓库 `target/`，避免本机 Temp ACL 阻断回归。原 `qx-cli report` 展示读链、其他 RunType 和容量画像仍未完成。
- 本轮多账户报告事实收敛：`qx-app::verify_multi_account_research_facts_v1` 统一验证 Bar/Depth 批次 summary、本金加权收益和子身份指纹，API 继续持有授权路径、子报告呈现与账户事件页。qx-app 库测试 133/133，qx-api 库测试 113/113，多账户 Bar/Depth 报告回归 2/2，`cargo check -p qx-cli --no-default-features -j 1`、能力目录 drift check 和定向 Rust 格式通过；架构门禁 226/226。全量多 RunType 报告读用例仍待完成。

### 文件策略运行前置条件统一到 Rust 应用层（2026-10-24，未发布工作区）

新增 `StrategyExecutionPreflightSpecV1` / `StrategyExecutionPreflightResultV1`。Python 文件策略的 `run-strategy`、`search-strategy` 与 `diagnose-strategy` 现在在启动前通过同一个 qx-app 入口校验规范化策略 manifest：参数类型/必填/范围/枚举、搜索空间、数据级别、产品/交易所/资产类别、显式市场规格、目标策略槽、warmup 加一次决策 Bar、样本外训练窗，以及每份目标 BarFrame 的 Rust 可用性、标的和内容指纹。多市场按选中策略槽只检查相关市场；partial manifest 明确返回未校验状态并保持兼容。删除了旧 Python 参数/搜索/市场规则执行校验副本，Python 层只加载策略元数据并计算源码身份。新增 schema v1、schema registry 记录和 Python SDK 导出。

- 验证：`cargo check -p qx-app -p qx-python`、`cargo test -p qx-app` 全量通过；策略预检集成用例 5 项通过；Python 策略预检/manifest/CLI 用例 31 项通过，未来数据诊断/app 用例/SDK 分层/打包合同 89 项通过；Windows editable PyO3 扩展重建成功；全量架构不变量门禁 1,353 项通过。
- 仍待：Rust CLI/API 对 Python 文件策略的统一入口尚未接入；多账户账户级预检、按有效时间窗计数与非 Bar 文件策略入口仍需分阶段补齐。本改动不代表 tick/orderbook 策略已能通过 Bar 回测，也不代表已发布。

### 版本化研究报告核心读模型（2026-10-24，未发布工作区）

qx-app 新增 `ResearchRunReportV1`（`research-run-report-v1`）：Bar 与深度运行共享复核结论、摘要、有界权益曲线和成交样本，CSV 金额按原始字符串保留精度；读取限制摘要 16 MiB、其他核心产物 128 MiB、最多 500 万行和 1 MiB 单行。单市场 API 先将输入与 RunRecord 引用路径约束在产物根目录，再消费 DTO，并继续做完整 EventLog 重放及 fills 逐项核对。Python 搜索 HTML CLI 也调用该 Rust 读用例验证候选身份/摘要，前端 Web 继续消费 API 原有响应外形。

- 验证：`cargo test -p qx-app`、`cargo test -p qx-api`（108 个库测试及集成目标）、Python 应用/HTML/打包/SDK 合约 95 项通过；Windows editable PyO3 扩展重建成功。真实六候选 SMA 搜索工件已由 Python CLI 所用 Rust DTO 读入并通过，样本首项为 `python-file-sma-search-0001`。
- 已明确剩余：多市场/多账户/Tick/L2 尚未消费统一 DTO；报告存储端口、跨市场独立 replay 收敛和更大容量画像仍需后续实现。此改动不构成已发布版本或外部供应商验收。

### 多市场事件页共享读取入口（2026-10-25，未发布工作区）

qx-app 新增 `MultiMarketEventReportReadRequestV1` / `MultiMarketEventReportPageV1`：请求只接收完整 `MultiMarketBacktestOutcome` 与可选页参数；reader 从 outcome 指向的产物中派生事件轨迹、RunManifest、ledger 和策略/市场映射，并核对产物内嵌 outcome、spec fingerprint、CanonicalRunSpec/RunManifest 身份，再完整流式重放后返回有界事件页。这样调用方不能拼接另一次运行的摘要、映射或账簿。分页越界会被拒绝。CLI、Python 和 API 共用此应用读模型；API 从读模型取得已验证产物/spec/manifest/账本，再独立执行数据根目录授权、输入身份核验和市场上下文投影。schema 已登记；多市场 outcome schema 的市场数和策略数下限也更正为实现允许的 1。

- 验证：qx-app 报告读模型测试 15/15；API 多市场报告/入口定向测试 6/6；`qx-python --features extension-module` 原生扩展构建通过；CLI/Python 对同一真实多市场产物比较完整 DTO 的跨入口回归 1/1 通过，并覆盖伪造 outcome 身份和分页越界的拒绝路径（`qx-cli --no-default-features`）；架构 `--fast` 通过，4 片完整覆盖 226 项具名检查且无重复。默认 CLI 构建在本机 MSVC 的 SQLite C 编译阶段失败（D8050 / c1.dll），不计为代码验收通过。
- 范围：该入口统一的是 EventLog 核心读页，不替代多市场完整报告 DTO、HTML/XLSX 展示读面，也不关闭 QX-REPORT-01 或整体发布门。

### 多市场 Depth EventLog 共享读取入口（2026-10-25，未发布工作区）

新增 `MultiMarketDepthEventReportReadRequestV1`，由 qx-app 从完整 `MultiMarketDepthBacktestOutcome` 读取并绑定产物、Depth spec fingerprint、实际执行模型、RunManifest 和 EventLog；重放校验账簿、成交数及分页范围后返回复用的版本化事件页 DTO。API 将已验证读模型用于原有 L1/L2 输入、质量清单、权益/成交 sidecar 和交互上下文投影；新增 CLI `app read-multi-market-depth-event-page` 与 Python `read_multi_market_depth_event_report_page_v1`，schema 和能力目录已登记。

- 验证：qx-app 报告读模型测试 15/15；qx-api 多市场定向测试 6/6；qx-python 原生扩展构建通过；同一真实双市场 Depth outcome 的 CLI/Python DTO 一致回归 1/1，并覆盖 outcome 身份篡改与 offset 越界拒绝；Python compileall、JSON Schema parse、能力目录 `--check` 通过，架构 `--fast` 覆盖 226 项且无重复。
- 范围：该切片统一多市场 Depth EventLog 读口，API 仍负责数据根目录授权和 Depth 输入/sidecar 验证；其余 RunType 报告、portable/recompute 矩阵与 Research 发布门仍未完成。

### 原生 Binance Spot 精确金额与小币种余额贯通（2026-10-24，未发布工作区）

原生 Binance Spot 连接器现在接受 `ExactOrderAmounts`，按 `exchangeInfo` 原始十进制字符串解析 PRICE/LOT/MARKET_LOT、NOTIONAL、价格区间和动态价格带；下单请求保留精确 quantity/price，计价币预算市价买使用 `quoteOrderQty`。成交数量、价格、quote cost 和币种手续费写入精确成交事实；account REST 快照和 `outboundAccountPosition` 也保留精确 free/locked 余额，服务对账管线将这些值写进精确账户余额事实。旧 1e9 字段保留兼容投影，不作为精确路径的过滤器输入。

- 离线验证：Binance Fake HTTP 契约覆盖亚纳米规则、订单、成交、手续费与余额，quote-budget 参数映射，以及精确成交进入多币种账簿并重启恢复；`cargo test -p qx-adapter --lib` 88/88、`cargo check -p qx-adapter -p qx-service -p qx-core -p qx-zhenlu --all-targets -j 1` 通过。未读取环境变量凭据、连接 Binance 或提交订单。
- 本机 Testnet 只读账户检查命令见 [`crypto-live-readonly-smoke.md`](../crypto-live-readonly-smoke.md)；真实 Testnet 的下单/撤单/重启恢复仍需要用户后续自行用本地凭据验收，当前不宣称供应商已验收。
- 能力边界：原生 Binance adapter 仅覆盖 Spot；Binance 合约及 OKX/Bitget Spot/合约仍由 CCXT 执行。Paper 撮合和部分 raw 策略控制输入尚未迁移精确金额；多账户之间不共享保证金、余额或原子购买力。

#### 本地复核补充：Binance Spot `MAX_POSITION` 提交前门控

原生 Binance Spot 对启用 `MAX_POSITION` 的市场，现会在委托写请求前读取账户 base 余额、全账户未成交订单和全市场 symbol/baseAsset 映射，将同一 base asset 的其他交易对未成交买量与本单合并检查。quote-budget 市价买在启用此过滤器时 fail-closed，因为成交 base 数量不能从 quote 预算静态证明。只读 account/openOrders 请求失败保持为可重试读取错误，不会误报为未知订单结果。

- Fake HTTP 回归覆盖跨交易对买单、free/locked 余额、超限拒绝、quote-budget 拒绝及无订单 POST；适配器精确规则/下单样例均为本地传输，不连接账户或交易所。
- `MAX_POSITION` 检查属于提交前观察，其他客户端并发下单仍存在读取/提交竞态，交易所最终过滤仍是最后边界；原生 Binance Adapter 仍只覆盖 Spot。

#### CCXT Binance Spot `MAX_POSITION` 预检补齐

CCXT 的 Binance Spot 现货路径也会在提交前识别市场 `MAX_POSITION` 过滤器，并以原始十进制字段合并账户 free/locked 余额、其他现货交易对的未成交 BUY 剩余量和候选订单。缺少原始过滤器/账户快照、委托 side/base asset 或精确数量时 fail-closed；quote-budget 买单无法证明基础币数量时拒绝。检查已放入 CCXT client 与 JSONL worker 的提交流程，直接 client 下单也执行同一预检。外部并发仍可能发生在预检和写入之间。

- Fake CCXT 覆盖 PEPE/USDT 候选买单与 PEPE/BTC 在途买单合并超限、缺少方向的委托快照 fail-closed，确认在 `create_order` 前拒绝；未连接 Binance、未读取凭据、未发送订单。
- 本地快捷验收：`python -m unittest test_ccxt test_ccxt_worker`（从 `python/tests` 目录执行，68/68）；本次仓库全量离线脚本通过，架构门禁 225/225，CCXT 合同 70 项（69 通过、1 项因本机 CCXT 版本低于锁定版本跳过），Python Strategy API 17/17、打包合同 6/6。完整快捷入口见 [`crypto-live-readonly-smoke.md`](../crypto-live-readonly-smoke.md)。

#### CCXT 手续费防 float 伪精度

订单或成交的 CCXT 标准化手续费若是 Python `float`，现在按币种从原始 `info` 中读取 Binance `commission`/`fills`、OKX `fee`/`feeCcy`、Bitget `feeDetail` 等十进制来源，并校验其 float 投影与 CCXT 返回值相符后才生成 exact fee。原始费用缺失、无法精确解析或投影不匹配时 fail-closed，不再把已舍入的 float 转成看似精确的十进制文本；符号沿用 CCXT 归一化费用语义，费用仍按币种分别记账。

- Fake 回归覆盖十进制手续费被标准化为 float 后从原始字符串恢复，以及没有原始依据时拒绝记账。没有访问交易所或账户。
- CCXT Python 快捷合同现为 `python -m unittest test_ccxt test_ccxt_worker`（从 `python/tests` 目录执行）；仓库级离线命令仍见 [`crypto-live-readonly-smoke.md`](../crypto-live-readonly-smoke.md)。

### CCXT 精确行情事件与多币种组合估值闭环（2026-10-24，未发布工作区）

CCXT 的十进制 bid/ask 现在从 Python ticker 响应经过 Rust 行情 worker、RuntimeEventEnvelope、EventLog 与重启恢复保留到 18 位；合法的亚纳米价格即使旧 `Price` 投影为 0，也不会被误判成无效行情。执行前风控 ticker 也写入精确双边报价。组合 FX 估值使用精确报价构建最短路径，逆向汇率、多跳汇率和同长度冲突校验均以十进制计算，portfolio cash/equity 的报告币估值不再先降到 1e-9。

- 本地验收：`scripts/crypto-live-offline-check.ps1` 全通过（架构 225 项；Rust CCXT/风控/账簿/运行时/API 契约；CCXT Python 68 项，67 通过、1 项因本机 CCXT 4.5.78 未达到 4.5.89 锁定版本而跳过；Strategy API 17/17；打包契约 6/6）；`cargo check --workspace --all-targets -j 1` 与 `cargo fmt --all -- --check` 通过。覆盖亚纳米行情持久化恢复、亚纳米 FX 估值与等长冲突 fail-closed。没有读取凭据、连接账户或发送订单。
- 本机快速命令与 Binance/OKX/Bitget 沙盒环境变量示例见 [`crypto-live-readonly-smoke.md`](../crypto-live-readonly-smoke.md)。
- 支持边界仍然明确：CCXT 负责 Binance/OKX/Bitget 的现货与合约精确金额交易；原生 Binance Spot 精确委托/成交/余额路径见本节上方新条目，Paper 撮合仍拒绝超过 9 位金额。多账户 portfolio 用于对账/估值和账户路由，不共享保证金或跨交易所原子购买力；真实 Testnet/Demo 下单生命周期仍需用户后续本机验证。

### Bitget Spot 市价买 quote-budget 贯通下单链路（2026-10-24，未发布工作区）

新增 `quote_budget_exact`，将 Bitget Spot 市价买的计价币预算与基础币数量明确区分。Strategy API、Rust ExactOrder、订单状态机、账户级风险与可用余额检查、精确在途资金预留、CCXT Classic/UTA request、成交成本恢复和多币种账簿已贯通；未花完的预算继续作为 quote 资金预留，实际成交按交易所回报的精确 quote cost 记账。新增 Python 策略实例与 UTA Spot 只读配置样例。UTA 按 Bitget UTA 官方 `qty` 语义直接构造 `SPOT` 市价委托，防止 CCXT UTA builder 把 quote 预算套到基础币数量刻度。

- 验证：完整 `scripts/crypto-live-offline-check.ps1` 通过；架构 225 项，Rust 离线交易合约和精确账簿/策略意图集成用例通过，CCXT Python 68 项（67 通过、1 项因本机为 CCXT 4.5.78 而支持契约锁定 4.5.89 跳过），Strategy API 17/17、Python 打包合同 6/6 通过；`cargo check --workspace --all-targets -j 1` 与 Rust 格式检查通过。没有加载凭据、连接交易所或提交订单。
- 运行入口与环境变量示例见 [`crypto-live-readonly-smoke.md`](../crypto-live-readonly-smoke.md)。私有探针只读；真实 sandbox 订单接受/成交/撤单仍需后续本地 Demo/Testnet 凭据验收。
- 剩余边界：跨账户仍不共享 collateral/购买力且跨所无原子组合提交；Binance 原生 Spot 与 Paper 仍拒绝超过 9 位金额；策略 `target_qty`、内置/C ABI 与部分控制输入仍有旧 raw 刻度路径；当前执行能力不包含完整条件单、改单和硬滑点保护。

### 亚纳米市价单参考价贯通风控与在途预留（2026-10-24，未发布工作区）

CCXT ticker 现在同时保留原始十进制 bid/ask 和 1e-9 兼容投影。精确市价单只接受正值、时间戳不晚于本地且不超过 30 秒的行情；用 ask 作为精确风险参考价并写入订单事实。风控、合约杠杆档位校验和后续订单的精确在途名义额/资金预留继续使用这份精确值。该字段只用于风控，不会作为委托价格传给交易所。

- 验证：CCXT Python ticker 契约 42/42；`qx-core` 精确订单序列化、`qx-risk` 亚纳米在途估值、`qx-service` 新鲜度/精确价格解析、`qx-adapter` 市价 wire 均通过。随后完整离线脚本通过：架构 225 项、CCXT Python 64 项（63 通过、1 跳过）、打包合同 6/6；`cargo check --workspace --all-targets -j 1`、Rust 格式与差异检查通过。未连接账户或提交订单。
- 剩余：策略 target_qty/内置/C ABI 与部分控制输入仍为 raw 刻度；Bitget Spot 市价买需要 quote-budget 意图；原生 Binance Spot 与 Paper 对超过 9 位金额保持拒绝；sandbox 订单生命周期未验收。

### Python/外部策略精确订单意图到达 CCXT 实盘构建入口（2026-10-24，未发布工作区）

Strategy API v1 的 `intents[]` 新增 `qty_exact`/`limit_price_exact` 十进制字符串；旧 raw 字段仍可用。Rust 契约与 Python SDK 拒绝 raw/exact 混用和超支持精度，`qx-app` 依据冻结 exact step/tick 构造 `ExactOrderAmounts`，CLI 实盘策略构建器将订单交给提交前账户级精确风控。Bar 回测暂不支持 exact amount，遇到时明确拒绝。

- 验证：Python Strategy API/示例 15/15；`qx-app` 精确意图/冻结规格 2/2；`qx-runtime` 策略契约 6/6、Schema 合同 4/4；`qx-cli` 策略精确意图构建 1/1；Rust 格式、差异检查及 Rust/Python/Schema 六项字段/精确路径架构判据通过。完整 `crypto-live-offline-check.ps1` 复跑也通过（架构 225 项、CCXT Python 63 项含 1 跳过、打包合同 6/6）。无账户连接或订单操作。
- 剩余：内置/C ABI 策略、target_qty 调仓与部分控制输入仍为旧刻度；原生 Binance Spot 与 Paper 对高于 9 位金额仍 fail-closed；sandbox 真实订单 lifecycle 尚未验收。

### CCXT 精确合约订单贯通杠杆档位预检（2026-10-24，未发布工作区）

CCXT exact-order 能力现在覆盖 Spot 与合约：合约 Worker 不再因 `amount_exact` 拒单，而是先核验账户持仓模式/保证金模式/杠杆，再按精确数量、限价、仓位、普通未结订单和精确档位边界计算同标的敞口；Bitget Hedge 继续按多空方向最大敞口选档。各币种订单/成交/手续费继续走精确 wire、同步和账簿回放。能力声明只对 CCXT 开放 exact order；原生 Binance Spot 与 Paper 保持拒绝高于 9 位金额的订单。

- 组合只读读面提供 `cash_by_currency_decimal`、`equity_by_currency_decimal` 和精确报告币估值；它不把多个账户净成共享保证金或统一强平边界。
- Python/外部语言 `Strategy API v1 intents[]` 新增 `qty_exact` 与 `limit_price_exact` 字符串，Rust 按冻结规格生成精确订单，后续进入账户级实盘风控；target_qty 调仓语义、内置/C ABI 策略输入和部分控制命令仍是 raw 刻度。
- 保留的拒绝边界：精确市价单需要可由当前市场行情风险输入表达的参考价；Bitget 现货市价买单需要 quote-budget 意图；缺失/歧义交易所档位、仓位方向或行情继续 fail-closed。sandbox 真实委托验收仍未执行。
- 验证：`scripts/crypto-live-offline-check.ps1` 全部通过（Rust 格式与选定实盘合同测试、架构 225 项、CCXT Python 63 项含 1 项跳过、Python 包装合同 6/6）；全程未读取凭据、连接账户或发送订单。`sandbox_tested` 与 `production_approved` 维持 false。具体剩余发布边界及本机快捷命令见文末“Crypto 实盘链路收敛记录”。

### CCXT 精确市场规则进入冻结规格与漂移校验（2026-10-24，未发布工作区）

CCXT 市场解析现在把精确合约乘数、价格刻度、数量步长、最小量和最小名义额作为十进制字符串保留，并写入 `TradingInstrumentSpec.exact_rules`。Rust→Worker 的下单前规格校验携带 normalized coefficient/scale，Worker 与刷新后的 CCXT metadata 做精确比较；旧 `1e-9` raw 映射相同的 `1e-9`/`1e-10` 步长变化也能拒绝。`TICK_SIZE`/`DECIMAL_PLACES` 支持 18 位以内的可表达十进制；没有固定步长的 `SIGNIFICANT_DIGITS` 不会被猜成固定精度。

- 验证：CCXT Python 用例 39/39、JSONL Worker 用例 17/17；`qx-app` 冻结规格解析定向用例与 `qx-adapter` Worker 请求/漂移定向用例通过，完整 `scripts/crypto-live-offline-check.ps1` 通过（相关 Rust 库、架构 225 项、CCXT Python 58 项通过/1 项跳过、打包契约 6 项），`cargo fmt --all -- --check` 通过。Windows Rust 编译需要 `CARGO_PROFILE_DEV_DEBUG=0` 避开 MSVC D8050。
- 限制：这是精确市场规则的冻存和下单前漂移检查。订单金额编码、可用资金预留、风险、成交/手续费、现金/持仓账簿和恢复仍未全部迁移到精确十进制，真实下单能力仍以 9 位 raw 为界；没有连接账户或提交订单。

### 为小币种精度迁移引入带尺度的十进制内核值（2026-10-24，未发布工作区）

`qx-core` 新增 `ExactDecimal`，显式序列化 coefficient 与 scale，支持无浮点十进制/科学计数法解析、18 位以内精确加减乘、规范化格式化，以及无损转换到固定 scale。旧 `Price`/`Quantity`/`Money` 和 `*_raw` 保持 `1e9` 语义；`parse_decimal_exact` 现在经该类型完成解析后再做精确降尺度。可选精确数量/价格载荷现已接入 `Order`、`Fill`、事件摘要、OMS 成交累计，以及账户快照订单/成交行和 state hash；尚未接入精确风险、交易所请求编码、现金/持仓/费用账簿和 Python Worker。未接通的执行/记账入口会明确拒绝精确金额，**所以这只是迁移基础，不代表小币种高精度实盘链路已经打通，实际交易能力仍为 9 位**。

- 离线验证：相关库测试合计 447 项通过（qx-core 74、qx-risk 90、qx-adapter 29、qx-zhenlu 11、qx-xingban 152、qx-execution 44、qx-service 47）；随后 `qx-protocol`/`qx-storage`/`qx-api --all-targets` 全部通过（含 102 个协议库用例、快照集成用例）。`cargo check --workspace --all-targets` 和格式检查通过。覆盖精确订单/成交 JSON、Event digest、OMS 亚纳米累计、快照行/hash 和精度不受支持时的拒绝；未访问账户或发送订单。

### 原生 Binance Spot 下单前比对当前交易规则（2026-10-24，未发布工作区）

原生 Binance Spot Venue 现在会在共享 Live 风控提交阶段读取公共 `exchangeInfo`，并将当前市场状态、base/quote、`PRICE_FILTER.tickSize`、`LOT_SIZE.stepSize/minQty` 与 `MIN_NOTIONAL`/`NOTIONAL.minNotional` 和冻结规格精确比较。另在具体订单提交前按订单类型检查 `MARKET_LOT_SIZE`，并校验实时 `PRICE_FILTER` 价格区间与 `LOT_SIZE`/`MARKET_LOT_SIZE` 数量上下限。规则只缓存 60 秒；公共读取不附带 API 凭据或签名。市场停用、规格漂移、订单过滤器不符、元数据缺失或精度超过核心表示范围时，在下单写请求前 fail-closed；公共读取的网络/服务端错误不会误置成订单结果未知，也不会触发订单恢复状态。

- 离线验证：fake HTTP transport 覆盖规则匹配、60 秒缓存命中/过期刷新、数量 step 漂移拒绝和市价单 `MARKET_LOT_SIZE` 拒绝，并断言请求是无认证公共 GET、违规时没有 POST。`cargo test -p qx-adapter` 84/84、`cargo test -p qx-service --lib` 47/47、架构门禁 225/225、`cargo fmt --all -- --check` 和 `git diff --check` 通过。没有访问交易所/账户或发送订单。
- 限制：尚未覆盖百分比价格带、动态均价名义额、最大委托数和账户级限制；不验证交易所实际受理，也不能消除读取到发单之间的规则变化竞态。

### Live CCXT 下单前核验冻结规格并刷新市场元数据（2026-10-24，未发布工作区）

共享 Live 风控提交入口在静态准入通过后、Venue 写请求前要求适配器核验产品规格。CCXT Worker 对超过 60 秒的 market cache 先刷新，再比较冻结规格与 CCXT 当前 metadata 的产品、币种/结算币、合约乘数、价格 tick、数量 step 和交易所可见的数量/名义额/杠杆/维持保证金字段。差异或缺少强制精度数据时以确定的预提交拒绝返回，不断开 Worker，也不进入未知委托恢复。

- 限制：仅校验 CCXT/交易所暴露并可规范化的字段；冻结文件不自动改写，完整动态过滤器、条件单和账户级规则仍需补充。检查到交易所写请求之间仍有市场规则变化竞态。
- 离线验证：锁定 CCXT 4.5.89 的 `test_ccxt*.py` 62/62、Rust CCXT 25/25、qx-service 单测 47/47、Python 全量 unittest 260 项通过/2 项跳过、架构门禁 225/225、`cargo fmt --all -- --check` 与 `git diff --check` 通过；ruff 致命/未定义名称检查通过。fake exchange 覆盖一致接受、过期缓存强制刷新、lot 变化拒绝且没有写请求；Rust 适配器拒绝合同保证连接保持。未连接交易所或读取账户。

### CCXT 合约开仓按实时杠杆档位预检合计敞口（2026-10-24，未发布工作区）

Rust 下单意图现在显式携带目标杠杆、保证金模式与 reduce-only 属性；JSONL Worker 拒绝缺少杠杆/保证金模式的合约下单请求。CCXT 按能力调用档位接口：Binance 使用账户私有批量 tiers，OKX/Bitget 使用单市场 tiers；OKX tier size 按合约张数比较，Binance 线性和已声明覆盖的 Bitget 线性使用名义价值。非 reduce-only 合约单会计入同标的仓位、未结普通订单与本次订单，名义价值型档位按向上取整计算。档位不唯一、仓位缺少 mark price、杠杆上限缺失或策略杠杆高于档位上限时，在 create_order 前拒绝；reduce-only 平仓不受开仓档位检查阻挡。Bitget 反向合约档位单位未核验，当前明确拒绝。

- 离线验证：隔离目录加载锁定版 CCXT 4.5.89 后，`test_ccxt*.py` 61/61 通过；fake-worker/connector 用例覆盖 Binance batch tiers、OKX/Bitget market tiers、OKX contracts 阈值、Bitget inverse fail-closed、已有仓位与挂单跨档拒单、reduce-only 不查询开仓档位，以及已知预检拒绝不进入未知委托对账。Python 全量 unittest 259 项通过/2 项跳过；Rust CCXT 定向用例 24/24、fmt 检查及架构门禁 225/225 通过。只读探针 `--help` 可启动。没有连接账户或发送订单。
- 限制：这是同一市场的保守前置检查，不是交易所资金预留。反向仓位按绝对敞口累加，可能保守拒绝；条件单/其他客户端在检查后新增的风险、交易所档位变化和读写竞态仍由交易所最终裁决。OKX 公开 tiers 可能与特殊账户有效档位不同；Bitget 反向合约暂不放行。完整动态过滤器矩阵和跨账户购买力仍未实现。

#### 2026-10-24 补充：Bitget 反向档位面值

Bitget 仓位档位文档将档位边界定义为价值；CCXT 单市场 tiers 将边界映射为名义价值。反向合约按 `contracts × contractSize` 计算档位价值，不再套用线性合约“数量 × 价格”公式，也不需要额外 ticker。假交易所覆盖累计面值跨档拒单并断言没有下单请求；`test_ccxt.py` 37 项通过。此前本节的“暂不放行”限制已由这项本地实现替代；供应商规则变化和真实受理仍未验收。

#### 2026-10-24 补充：Bitget Hedge 档位按方向累计

Bitget Hedge 的档位规模按 long/short 两侧中较大的一侧计算，并包含同方向普通开仓挂单。预检现在读取 CCXT Hedge 侧向映射，分别累计持仓、同侧挂单与候选单；其余 venue 和 One-way 沿用保守 gross 口径。Fake exchange 与 JSONL worker 回归覆盖方向差异；`python -m unittest discover -s python/tests -p 'test_ccxt*.py' -q` 57 项通过、1 项跳过。官方规则：[Bitget 仓位档位](https://www.bitget.com/futures/introduction/position-tier)，参数映射：[CCXT Bitget 4.5.89](https://github.com/ccxt/ccxt/blob/v4.5.89/python/ccxt/bitget.py)。本地预检不构成资金预留或真实供应商验收。

### 精确十进制的定尺度风险运算（2026-10-24，未发布工作区）

扩展 `ExactDecimal` 为十进制字符串中间运算，提供定尺度乘除、显式方向舍入和精确 step 整除检查；现有 `TradingInstrumentSpec` 的 raw 名义额、报价成本、初始/维持保证金、未实现盈亏、资金费和订单 tick/step 校验改由该运算执行。风险敞口及保证金向上取整，最低名义额判断向零取整，PnL 向负无穷取整以保护账户权益口径。没有修改 `SCALE=1e9` 的历史语义，且交易规格、订单风险仓位、现金/持仓/费用账簿和 Worker wire 仍未消费高精度 sidecar，所以**超过 9 位精度仍未端到端支持**。

- 验证：`scripts/crypto-live-offline-check.ps1` 全部通过，包括 Rust 相关 crate 库测试与格式检查、架构门禁 225 项、CCXT Python 57 项通过/1 项跳过、包装契约 6 项通过。新增用例覆盖舍入方向、half-even、亚纳米步长、低于 legacy 金额网格的风险计算和未实现损失。全程未读取凭据、连接账户或发送订单。

### Live 现货卖出按 base 余额和账簿双重限额（2026-10-24，未发布工作区）

修复 Live Spot Sell 错用报价/结算币余额导致“有现货、无 quote 现金”也被拒单的问题。卖出风控改读该账户/交易所的 base 币 `free - borrowed`，并将可卖量限制为内部账簿持仓与交易所可用 base 余额两者较小值；对最新余额快照后提交、尚未反映到交易所余额的在途卖单再扣数量。新买入与永续/交割品种的资金币种判据保持原口径。

- 离线验证：Live 风控模块 10/10，覆盖无 USDT 现金但有 BTC 可卖、交易所 base 余额低于账簿持仓时拒绝超卖；Rust fmt、架构 `--fast` 四分片聚合 225 项、`git diff --check` 通过。未连接交易所/账户，也未发送订单。

### CCXT 超细交易刻度映射与市价单上限 fail-closed（2026-10-24，未发布工作区）

CCXT `TICK_SIZE` 精度现在按有理数计算交易所刻度与牵星 1e-9 定点网格的交集步长：例如 1e-10 可映射为 raw step 1，3e-10 映射为 raw step 3；只有处于双方刻度交集的值才允许下单。价格/数量/名义额最小限制向上取整，最大限制向下取整。另修复市价 `MARKET_LOT_SIZE.maxQty` 小于 1e-9 时被 Python 真值回退逻辑丢弃的问题；此时限制保留为 raw 0，所有正数量会在行情或写请求前拒绝。

- 离线验证：假交易所 `test_ccxt.py` 32/32，隔离 CCXT 4.5.89 的 `test_ccxt*.py` 54/54，仓库根目录 Python 全量 245 项通过/3 项跳过；架构 `--fast` 四分片完整聚合 225 项通过，Python compileall 与 `git diff --check` 通过。交易金额仍受核心最多 9 位小数限制；这项改动只接受可以精确落在现有定点网格上的值。未连接交易所/账户，也未发送订单。

### 三家交易所只读沙盒样例补齐现货覆盖（2026-10-24，未发布工作区）

新增 OKX Spot 与 Bitget Spot 的私有只读探针配置，现有 Binance Spot/USDⓈ-M、OKX Swap、Bitget Classic/UTA 合约样例继续保留。所有配置都强制 `sandbox=true`，密钥只通过进程环境变量读取；配置契约测试检查交易所、市场类型、凭据变量引用和标的 venue 后缀。交易参数回归补上 OKX Spot 市价买显式 `tgtCcy=base_ccy`，并确保 Bitget Spot 市价买在行情/委托调用前 fail-closed，避免把 base 数量误发成 quote 预算。探针仍只读取账户摘要，不发送订单。Bitget Demo REST 的 `PAPTRADING=1` 要求见[官方说明](https://www.bitget.com/docs/classic/demo-trading/rest-api)。

- 离线验证：锁定 CCXT 4.5.89 的 `test_ccxt*.py` 50/50、沙盒样例/包装契约 6/6；OKX Spot 市价买的真实 CCXT 请求构造器保留 9 位 base 数量并生成 `tgtCcy=base_ccy`；Bitget 市价买回归断言行情与委托函数均未调用。两个新 Spot 样例在密钥为空时均于连接前退出，没有发起网络请求。架构 `--fast` 四片完整聚合 225 项唯一检查通过，Rust 格式与差异检查通过。交易所账户/订单链路仍未验收。

### 多账户多腿恢复状态闭环加固（2026-10-24，未发布工作区）

联合补偿恢复现在按账户与交易所限定每轮可提交的腿，逐腿持久化已确认补偿量和最多三次的确定性重试身份；部分成交、重启后可续跑，不会从头重复补偿。`Hedged` 需要所有原始腿均为终态且每条确认敞口都已补齐。若所有补偿量已写盘但进程在组状态提交前退出，恢复轮会在 claim 下补完状态而不重复下单。Paper/实盘扫描会拾取这种待收敛状态。旧分组缺少整张进度表时仍恢复为空表；一条已存在的进度记录若缺少补偿数量则拒绝读取，避免把损坏进度误认成零。

- 离线验证：`qx-execution` 29/29、`qx-zhenlu` 多腿组用例 7/7、`qx-service` 47/47、CLI Paper 多腿恢复 2/2；`cargo check -p qx-service --all-targets -j 1`、格式检查与架构 `--fast`（四片完整聚合、225 项唯一检查）通过。未访问账户或发送订单。跨账户共享购买力/保证金、交易所原子提交和超过 9 位的小数精度仍不属于本次实现。

### 小币种九位精度请求契约复核（2026-10-24，未发布工作区）

将牵星 connector 的低价现货精度回归提升到数量与价格均为 1e-9，并用锁定版 CCXT 4.5.89 的 Binance Spot、Binance USDⓈ-M、OKX Swap、Bitget Classic/UTA 格式器逐一核对九位小数不会被改写。超出牵星当前九位内部定点范围的非零精度仍 fail-closed；这项回归确认了现有支持上界，没有改变该上界或历史 raw 数值语义。

- 离线验证：`python/tests/test_ccxt.py` 26/26、请求形状用例 6/6；`test_ccxt*.py` 合计 47/47。没有连接交易所、账户或发送订单。

### 多币种费用、FX 换算与快照编码补强（2026-10-24，未发布工作区）

显式成交 `fees[]` 以原币种分别入账；组合快照不跨币种合计费用，混合币种时 `fees_raw` 留空并提供 `fees_by_currency_raw`。Paper/组合 FX resolver 的跨交易所两跳路径有回归用例，缺路径或等长路径汇率冲突时拒绝估值。快照 v2 的手续费币种映射复用统一 JSON map encoder，Schema 补齐有/无生产者字段的 null 语义。

- 离线验证：ledger 26/26、service 46/46、protocol 所有目标 30/30、CCXT Rust 23/23、隔离 CCXT 4.5.89 Python 46/46；架构快速四分片聚合通过。未连接账户或发送订单。

### Bitget UTA v3 下单字段与 Classic 分离（2026-10-24，未发布工作区）

对照 Bitget UTA v3 当前官方接口后修正合约下单参数：UTA v3 订单不携带 Classic v2 的 `marginMode`，保证金模式仍由已读取的账户/标的配置校验；Classic v2 和 OKX 则继续在写请求中明确模式。Bitget 设置探针把实际账户路径标记为 `uta`，缺失标记会拒绝提交。

- 离线验证：隔离 CCXT 4.5.89 的 `test_ccxt*.py` 46/46，`cargo test -p qx-adapter ccxt_` 23/23；测试包含 Bitget UTA v3 请求字段以及 Rust 不泄漏 Classic `marginMode`。没有连接账户或发送订单。

### CCXT 下单数量单位边界校验（2026-10-24，未发布工作区）

Rust 下单进程协议现在显式传递 `amount_unit`：由交易所市场设置判定现货 `base` 或合约 `contracts`。Python JSONL worker 对缺失/未知单位拒单，适配器再对照已解析市场类型校验，避免 spot base 数量与 derivative contract count 在 Rust/Python 边界静默互换。领域 `Quantity` 和策略输入还没有单位类型，原生 Binance Spot 不经过此 CCXT 协议。

- 离线验证：隔离 CCXT 4.5.89 下 CCXT Python 定向用例 46/46 通过；`cargo test -p qx-adapter ccxt_` 23/23 通过。未连接交易所或账户。

### CCXT 交易适配依赖版本收敛（2026-10-24，未发布工作区）

将 `qianxing[ccxt]` 与 `[ccxt-pro]` 中的公共 CCXT 固定到 4.5.89。交易适配器在真正创建 CCXT 客户端时核对 `ccxt.__version__`，未核验版本 fail-closed 并提示安装项目 extra；离线包装契约检查钉住 extras 与运行时版本必须一致。此变更避免任意上游版本悄然改变 Binance/OKX/Bitget 参数映射；仍需在每次有意升级时跑离线请求形状矩阵，并另行进行 sandbox 验收。

- 离线验证：隔离安装 CCXT 4.5.89 后 `test_ccxt*.py` 46/46、包装契约 5/5；新增真实 CCXT 请求构造器断言 Binance USDⓈ-M/OKX Swap/Bitget Classic/UTA 双向平多参数，以及 8 位小数数量/价格保真。用例不连接任何交易所；sandbox 委托尚未验收。

### 实盘 worker 多市场冻结规格映射（2026-10-24，未发布工作区）

修复一个执行链断点：`WorkerConfig.symbols` 可声明多个标的，但原 `instrument_spec_path` 加载器只能将一个标的绑定到单份规格，导致同一 worker 的多标的精度、乘数和保证金无法各自预检。现在可配置 `qianxing_market_spec_set` v1，以完整 `InstrumentId` 为键选择独立规格；旧单市场文件继续读取。运行时启动预检逐个验证 worker 声明的标的，并验证共享集合内所有规格条目；即使 worker 的 `symbols` 为空也会校验集合本身。规格缺失、非法键、条目身份错配和未知版本均 fail-closed。新增双标的永续示例和 JSON Schema。

- 离线验证：`qx-service::worker_market_spec::tests` 5/5 通过；CLI runtime market spec preflight 1/1 通过；runtime 配置、worker spec loader、单源规格读取相关架构子门禁通过。未连接交易所或账户。

### 0.2.2 多交易所联合回测与精度、发布链路收敛（2026-10-23）

优先打通 OKX、Binance、Bitget 的多标的回测链：支持单交易所单账户、多市场共享账户以及多交易所多账户联合回测；现货与带杠杆合约使用统一策略/撮合/账本内核，逐市场绑定合约乘数、保证金档位、资金费率、价格 tick、数量 step、最小数量和最小名义价值。小数位与交易所规则不足时拒绝猜测，按固定点精度检查订单。

完善自适应参数搜索、折内隔离、可验证检查点、未来数据/预热诊断和交互式研究对比报告；Rust API、CLI、Python 原生 SDK、C++ SDK 和 Web 研究台共用版本化回测契约。现有 `VenuePort`/`VenueRouterPort` 与 `Strategy` 接口承担交易所网关及策略契约，不再新增平行抽象。实盘路径已补齐代码和风控接线，本版本未发送实盘或 Testnet 交易订单。

- 验证：Rust workspace 168 个测试目标 2,056 passed、0 failed、2 ignored；全目标严格 Clippy、Rust fmt、Python 231 passed/2 skipped、研究对比页 8 passed、架构分片完整覆盖 225 项且无重复、供应商验收离线门禁 6 passed；C++ SDK 两项冒烟与安装后独立消费者测试通过。
- 发布版本字段同步到 `0.2.2`，覆盖 Cargo workspace/lock、Python 包、CMake SDK、成熟度发布基线/目标及安装示例。真实交易验收仍需各交易所账户权限与密钥；公共行情和本地模拟不会被描述成下单验收。

### 0.2.0 版本对齐与退役门禁说明清理（2026-10-21）

把当前 workspace、Rust 锁文件、Python 包、CMake SDK、成熟度发布记录与安装文档统一到 0.2.0。删除活动 Rust 源码和检查器注释中残留的历史行数棘轮/用例地板说法；历史变更记录保留为审计背景，不参与当前验收。架构快速分片复核发现版本动态检查漏导入 `tomllib`，改为直接读取 `[workspace.package]` 的版本声明后，四片完整聚合通过。

- 验证：`cargo metadata --offline` 成功；29 个 workspace 包版本均为 0.2.0；Rust workspace 165 个测试目标合计 1,941 passed、0 failed、2 ignored；默认与无 backend feature 的全目标严格 Clippy `-D warnings`、feature-off `cargo check`、Rust fmt、Python 199/199、架构 `--fast` 四片完整覆盖 221 个具名检查且无重复、成熟度生成检查、Markdown 链接和 `git diff --check` 均通过。
- 本机 Release CLI 输出 `qianxing 0.2.0 (build 343d405-dirty, target x86_64-pc-windows-msvc, profile release)`，SHA-256 `D737F1EAE554B2169643C39FC0767B4BFC3DA7E921C31E33EAAD52616334C9D7`。用该二进制重新执行 Crypto/A 股确定性验收，result_hash 为 `26fdd6b52d020700` / `6be034a4cb0760fa`，并通过 Paper E2E（提交 1 单、成交 1 单、账簿 4 条）；三份本机验收记录已刷新。工作树仍有未提交改动，该二进制不是干净发布候选，性能证据仍需与干净候选二进制重新绑定。
- 干净提交 `5db568f2` 的 Release CLI 输出 `qianxing 0.2.0 (build 5db568f, target x86_64-pc-windows-msvc, profile release)`，SHA-256 `044DC5647DE5B9791B55F1E95CA2D4E018A9E49CF10C13CB55E18A27221F57BF`；同二进制执行 Crypto/A 股与 Paper dry-run 均通过，结果 hash 不变，Paper 为 1 单提交、1 单成交、4 条账簿事实。该干净构建取代上一条 dirty 二进制的本机候选核验；远端矩阵尚待 PR CI。

### 0.1.0 本机全量回归与历史快照门禁清理记录（2026-10-21）

移除以历史文件/测试数量、固定源码行数及静态回测哈希为依据的验收条件，保留实际架构接线、行为语义、产物完整性与重放一致性验证。全量回归发现并修正跨服务迁移后的调用点核对、现货无库存卖出归因假设，以及本地 Python 原生扩展陈旧导致的 CLI/HTTP/Python 哈希差异。用当前 Release 重建 Python stable-ABI wheel，并刷新 Crypto/A 股及 Paper 验收证据。

- 验证：Rust workspace 165 个测试目标全部通过；架构检查 221 项完整聚合；全目标严格 Clippy、生产代码 unwrap/expect lint、无 backend feature Clippy、Rust fmt、Console JS 语法、Python 199/199、成熟度生成校验及 Markdown 链接均通过。Windows Python 3.13 wheel `qianxing-0.1.0-cp310-abi3-win_amd64.whl` 构建、tag/内容检查及隔离目录原生导入 smoke 通过，SHA-256 `0101a934fa9e2070a6d3b275db1bebca90cb0707525b01f3bb5949a067e6dd95`。
- 后续针对移除历史门禁后调整的 feature 条件断言复验：默认、SQLite、PostgreSQL、NATS、PostgreSQL+NATS 五种配置下 qx-cli 全测试均通过；各配置 9 个测试目标，默认 `cli_behavior_parity` 32/32、`project_commands` 45/45。严格 Clippy、check 通过。测试构建使用隔离 target 与仓库临时目录；MSVC 隔离构建关闭调试符号，并为 PostgreSQL 组合增加 rustc 线程栈以完成大型 qx-cli crate 编译。
- 候选 `qx-cli` Release SHA-256：`79324835b0e912665a26cac72558e0eb873805e9bd83fc434ba8396a6afb672e`。以此二进制重验 Crypto/A 股样例回测（result_hash `26fdd6b52d020700` / `6be034a4cb0760fa`）、A 股规则 1k/8k/64k profile（P50 281.050/309.601/547.507 ms）及 Crypto Paper 三轮 E2E（P50/P95 2672.090/2716.694 ms）；两项 profile 证据均绑定该二进制 SHA。
- 尚未声称生产批准：Linux/macOS 与 Python 3.10–3.12 矩阵、PostgreSQL/NATS 服务容器、Linux/macOS C++ SDK 矩阵未在本机执行；Windows C++ SDK 构建、CTest、安装包消费者、JSONL、Rust C ABI 插件与两种共享内存 Worker 协议已通过本机复核。Rust SDK Git 消费者需要已发布候选提交，Binance testnet 需要外部凭据。`ashare-research`、`crypto-paper`、`multi-venue-arb` profile 仍按覆盖范围保持 `unmeasured`。
- Windows C++ Worker 首次运行遇到系统临时目录 ACL 拒绝；切换到仓库隔离临时根后 shared-memory JSON/columnar 均通过。QXRB 文件打开错误现在包含失败路径及 Win32 错误码，便于区分路径/权限问题。

### A 股规则回归证据与 SDK 测试目录隔离（2026-10-21）

将 A 股规则合同与完整回测生命周期纳入量化场景探针；以当前源码重建 Release 后复测 1k/8k/64k 分钟 Bar、PIT 公司行为与确定性哈希。Rust SDK 测试临时目录改用进程号加纳秒时间戳，避免 PID 复用读取旧产物而出现假冲突。

- 验证：A 股规则合同 33/33、生命周期集成 8/8、Rust SDK 两个独立消费者目标 28/28 与 2/2；量化场景 81 项 executed、0 未执行、0 缺失、2 项外部阻塞。原吞吐证据采用 weekday-only 合成日历，已由 WP-7.89 的沪深公开日历负载补充；合成价格与全部规则分支未纳入吞吐负载，`ashare-research` 状态保持 `unmeasured`。

### 多 Venue Paper 补偿恢复场景证据贯通（2026-10-21）

扩展 Paper 多腿恢复用例，验证 BINANCE 部分成交、OKX 另一腿取消后的跨 Venue 补偿闭环，并继续检查冻结规格对越界补偿回报的拒绝。修正量化探针读取 L2 多策略版本化摘要的路径；将依赖远端已发布提交的 Rust SDK Git 消费者从无网络探针中移出，登记为候选提交发布后的单独验收。

- 验证：恢复用例 2/2；量化场景 79 项执行通过、2 项保留外部阻塞；架构 `--fast` 221 项检查完整聚合通过。`multi-venue-arb` 性能 profile 仍为 `unmeasured`。

### 修复 qx-app 全目标回归的陈旧定资与结果哈希断言（2026-10-21）

qx-app 回归发现手续费预留向上取整后仍断言旧 raw 金额，以及 OrderBook/独立多 builtin 当前稳定结果与旧哈希不符。根据当前精确定资和回测引擎输出更新固定期望，并移除声称仍等于 legacy hash 的过时测试名；保留每次重放的确定性比较。

- 定向验证：qx-app 32 个测试目标 200/200 通过；qx-app/qx-api/qx-sdk 格式检查与全目标严格 Clippy 通过。

### 当前源码 Paper 执行与账簿闭环复验（2026-10-21）

使用刚重建的 qx-cli 验证服务层风险准入、执行 worker 和一次性订单宿主迁移后的 Paper 交易闭环。

- 定向验证：`paper_acceptance.py` 通过；提交 1 单、成交 1 单、账簿事实 4 条；重跑不重复下单，审计链到 Executed，独立目录结果一致。该结果不代表真实 Venue 网络验收。

### 当前源码 A 股/币圈与 API/SDK/Console 链路回归（2026-10-21）

重建当前 qx-cli 后复跑 A 股/币圈双目录确定性验收；验证 qx-api、Rust/Python SDK 和 Console 新增异步研究路由。新菜单与 qx-api RunHandle 启动路由的双向接线由架构检查核对。

- 定向验证：A 股/币圈 `result_hash` 为 `6be034a4cb0760fa` / `26fdd6b52d020700`；qx-api 122 项、Rust qx-sdk 44 项、Python SDK/provider/native E2E 49 项通过；控制台脚本语法通过；API/Console/Rust SDK 接线检查 84 项通过。本轮不是开发完成后的三轮终态全链路审查。

### Rust 核心与回测引擎定向回归（2026-10-21）

对当前工作树的 qx-core 账簿/估值和 qx-xingban A 股/组合/OrderBook 改动执行 crate 全目标回归；将仓库内隔离测试目录 `.tmp-test/` 加入忽略规则，保留本机数据不删。

- 定向验证：qx-core 80/80；qx-xingban 215 passed、0 failed、1 ignored；两 crate 格式检查与全目标严格 Clippy 通过。账簿现金查询 Criterion 的 indexed 1k/8k/64k 中位数为 104.8/131.1/181.6 ns；64k entry-scan 对照约 323.0 µs。1k 相对同机 Criterion base 点估计 +7.8%，未超过台账 10% 阻断阈值；单组读数不代表完整 profile。
- 候选卫生：将 `.tmp-test/`、CLI runtime store 新生成 runs/队列及 deploy runtime store 新生成 runs/snapshots 加入忽略规则；文件保留，原有跟踪基线保留。剩余 130 个未跟踪资产待审并纳入合适提交。

### 修复 feature-off CI 工作流并覆盖服务层（2026-10-21）

修复 CI feature-off 两条多行命令的 YAML 缩进错误，确保工作流可解析；将新迁入的 `qx-service` 纳入无 backend feature 的全目标检查与严格 Clippy。

- 定向验证：工作流 YAML 解析成功（12 个 job）；feature-off `cargo check --all-targets` 与 Clippy `-D warnings` 通过；架构门禁 `--fast` 四分片完整聚合通过；`git diff --check` 通过。

### Worker market spec 启动预检归服务层（2026-10-21）

将运行时配置中的 worker spec 路径检查、声明标的绑定和逐标的冻结规格校验迁入 `qx-service::worker_market_spec`。runtime-check 保留错误汇总与呈现，Binance/CCXT/Paper worker 在进入服务循环前仍共用此 fail-closed 预检。

- 定向验证：runtime market-spec 预检 1/1；runtime fail-closed/规格单读者架构合同 17/17；qx-service 28/28；qx-service/qx-cli/qx-runtime 严格 Clippy 与格式检查通过。

### Paper/live 账户风险上下文归服务层（2026-10-21）

将 Paper/live 的账户余额准入、可用资金计算、现货库存冻结、衍生品持仓腿与在途 gross exposure 计算、以及 `RiskContext`/`OrderRiskPosition` 构造迁入 `qx-service::risk_host`。CLI 的 worker risk 模块现仅适配既有调用接口，Paper/ Binance/CCXT 共用服务层的风险判断和预留逻辑。

- 定向验证：Paper 保证金估值 7/7、Live 预留 9/9、qx-service 28/28；相关估值/准入/错误传播架构合同 21/21；qx-service/qx-cli/qx-runtime 全目标严格 Clippy 与格式检查通过。

### Live 在途订单规格选择归服务层（2026-10-21）

将 Live 余额快照后的在途订单冻结 market spec 选择迁入 `qx-service::risk_host`。服务层按账户、Venue、instrument 与启用 Execution worker 解析候选规格，拒绝冲突或缺失规格；CLI 仅把服务结果接入资金预留计算。

- 定向验证：Live 在途预留 9/9；Paper/live 准入与估值架构合同 11/11；qx-service/qx-cli/qx-runtime 全目标严格 Clippy 与格式检查通过。

### Paper 多市场账户规格与 FX 快照归服务层（2026-10-21）

将 Paper 账户的多市场冻结 market spec 汇总、持仓/在途订单标的解析、所需 FX 币种推导及直接/反向 Spot FX 一致性校验迁入 `qx-service::risk_host`。worker 规格身份匹配由服务层负责并调用统一 market-spec loader；CLI 保留风险上下文参数适配。

- 定向验证：Paper 保证金估值 7/7；估值与 Paper/live 准入架构合同 11/11；qx-service 28/28；qx-service/qx-cli/qx-runtime 全目标严格 Clippy 与格式检查通过。

### Worker market spec 加载归服务层（2026-10-21）

将 worker 冻结 market spec 的文件读取、相对路径解析与 qx-app 规格校验迁入 `qx-service::worker_market_spec`。风控与 runtime-check 继续走同一个加载器，保留错误诊断和 fail-closed 行为；CLI 只提供既有调用点的服务重导出。

- 定向验证：runtime market-spec 预检 1/1；规格单读者/runtime fail-closed 架构合同 17/17；qx-service 测试 28/28；服务、CLI、runtime 全目标严格 Clippy 与格式检查通过。

### Paper 在途资金预留归服务层（2026-10-21）

将 Paper 账户未终结订单的现货购买力、衍生品保证金及跨币种预留计算移入 `qx-service::risk_host`。CLI 继续负责从运行时配置、冻结规格、EventLog 和账簿组装快照输入；服务层单点执行金额计算与 fail-closed 校验，行为合同确认不存在旧实现复活。

- 定向验证：Paper 保证金估值 7/7、Paper 提交终态 5/5、Live 预留 9/9、qx-service 28/28；Paper/live 架构合同 2/2，相关 crate 严格 Clippy 与格式检查通过。

### 风控一致性合同切换到完整上下文（2026-10-21）

删除把 `RiskGate::check_with_spec` 历史缺口当成当前生产合同的 `known_gap_account_level_margin_is_invisible_to_legacy_gate` 用例。三入口一致性现在都通过完整 `OrderRiskContext` 比较；新增保证金不足的拒绝路径，确保 RiskEngine、RiskContext 门面与 RiskGate 对账户保证金给出一致决定。成熟度遗留项改为准确指向仍未关闭的 Live 跨币种抵押品与跨市场持仓估值。

- 定向验证：`risk_parity` 集成用例 5/5；架构 `--fast` 四片聚合覆盖 221 项且无重复；成熟度报告一致、YAML、Rust 格式和差异检查通过。

### A 股配置日历闭环到订单拒绝（2026-10-21）

扩展 A 股交易日历回归：日历明确排除的普通工作日不得提交买单，错误应说明当前 bar 不在可交易日或停牌；周末和午休时段也应判为非交易时间。此项验证配置日历拒绝语义，不代表仓库已有交易所官方完整日历数据，也不改变 A 股 profile 仍未测的状态。

- 定向验证：qx-xingban `calendar_json_controls_daily_and_intraday_trading_windows` 1/1；格式及差异检查通过。

### 启用 A 股规则时拒绝无界日历与全天时段（2026-10-21）

共享应用绑定器此前允许 `enabled=true` 的 A 股规则快照在交易日列表和 session 均为空时通过，底层会把任意日期和时刻视为可交易。现在回测必须提供有界 `trading_days`+`sessions`，或明确列出 `trading_timestamps`；缺输入在应用入口报错。同步说明部署配置要求，并以架构合同覆盖 fail-closed 绑定。

- 定向验证：绑定器缺日历/缺时段/有效配置用例 1/1；qx-cli A 股/回测合同 50/50；qx-app 全目标严格 Clippy、格式/差异检查及架构 `--fast` 221 项通过。

### 移除历史无效验收门禁并归拢账户准入服务边界（2026-10-21）

复核并清除仅依赖历史数量/规模快照的验收门禁：当前执行路径不再引用架构判据总量/标签快照、测试数量地板、全仓单文件行数棘轮、账簿/报告/API 读面固定行数上限、参数豁免总数上限或旧 `validate_core.py`；相应快照与预算文件不再作为仓库资产。架构检查保留模块归属、唯一实现、真实消费者、行为合同及活文档引用可达性，历史 changelog/归档不再被当作当前引用门禁。模式版本迁移图只覆盖需要读侧显式拒绝/迁移的契约，检查改为其 schema_id 必须属于模式登记册，不再错误要求覆盖所有 DTO schema。另将 Paper/live 共用的账户准入规则迁入 `qx-service::risk_host`。

- 验证：四分片架构门禁全绿并成功聚合；成熟度报告一致性、架构/生成器脚本语法、`git diff --check` 通过；NATS consumer 管道预算行为用例 1/1。活动 tools/maturity/CI/build 路径对退役门禁的检索无结果。

### API/Console/SDK 研究主链路复核与 Windows 隔离用例修复（2026-10-21）

Console 研究菜单现已接通 Bar 后台回测、多账户/多市场任务与参数实验；API 比较回测通过共享 Rust 应用用例稳定排名，并拒绝混合输入血缘。Python 与 Rust SDK 新增/覆盖多市场数据清单、订单簿队列配置、多腿自定义策略工厂和 RunHandle 消费链。将 qx-api RunRegistry/共享限流测试的 scratch 目录从 Windows 系统 Temp 移到仓库 `target/` 隔离目录，修复权限受限环境下的伪失败。

- 定向验证：API/Console/SDK 结构合同 105/105、API 路由合同 8/8；qx-api/qx-sdk 包测试、全目标严格 Clippy 和格式检查通过；Python SDK 分层 38/38、应用用例 37/37、Provider 9/9、新增 backtest DTO/验收用例 5/5。

### Live 下单拒绝过期交易所余额快照（2026-10-21）

Binance/CCXT Live 下单和价差恢复现在校验账户/Venue 最新余额快照时间；快照超过 60 秒或时间戳晚于准入时钟时 fail-closed。最终下单在账户锁内刷新 EventLog 后取当前时钟，锁等待与刷新耗时计入快照年龄。Paper 风控不依赖该交易所快照。

- 定向验证：Live 余额/在途预留 9/9、价差恢复 4/4、Live 风控架构专项 1/1、qx-cli 全目标严格 Clippy 与格式检查通过。恢复用例改用仓库隔离目录后在 Windows runner 通过。

### 快速回测拒绝显式非法的可选 market spec（2026-10-21）

快速回测 manifest 现在要求显式 schema v1 并拒绝未知字段。`market_spec` 字段缺省时仍允许使用运行时配置；若显式为空或类型错误，则在解析时失败，不再静默降级为未配置。

- 定向验证：qx-app 快速回测用例 3/3、CLI manifest/多 venue/A 股用例 5/5、全目标严格 Clippy、格式检查和架构专项 4/4 通过。重编译 CLI 后 A 股/币圈双目录确定性与 replay 验收通过，hash 分别为 `6be034a4cb0760fa` / `26fdd6b52d020700`；A 股报告产品用例 1/1 通过。

### Live 现货按报价币余额快照准入（2026-10-21）

Live 现货买单现在直接使用交易所余额快照中的报价币 `free - borrowed` 购买力，即使账簿报告币种不同也不做汇率换算；快照后同报价币现货在途订单按冻结规格预留，其它报价币现货订单使用独立现金桶，不互相占用。衍生品遇到异结算币在途订单仍 fail-closed，跨币种抵押品估值仍未开放。

- 定向验证：Live 余额/预留行为 8/8；`qx-cli` 全目标严格 Clippy、`live_balance_order_reservation_check()`、workspace 格式和成熟度报告一致性检查通过。

### 为 Rust SDK 注册表发布补齐内部 crate 版本约束（2026-10-21）

工作区内可发布 crate 的所有本地 path 依赖现在同时声明 `0.1.0` registry 版本要求；架构检查逐 crate 校验生产、构建和开发依赖。`qx-sdk` 与其应用层依赖 `qx-app` 增补 repository/homepage/documentation 元数据。使用临时本地 patch 映射解析未上架的工作区依赖后，Cargo 对 `qx-sdk` crate 包默认校验通过；真实注册表发布仍需先按依赖顺序发布内部 crate。

- 定向验证：`qx-sdk` 单测 13/13、独立消费者/集成用例 31/31；`cargo check -p qx-sdk`、严格 Clippy 和本地 patch 环境下 `cargo package -p qx-sdk` 默认校验通过；`registry_dependency_version_check()`、workspace 格式与目标差异检查通过。

### 移除构建脚本的历史步骤数量门槛（2026-10-21）

删除架构检查中固定要求构建脚本恰有 8 个编号步骤和 7 条 Cargo 命令的快照阈值。保留有效合同：Windows 与 Unix 脚本步骤名称/顺序一致、编号连续且分母自洽，Cargo 命令集合一致；解释器交接与门禁执行顺序继续检查。

- 定向验证：`build_script_parity_check()` 通过；`cargo fmt --all -- --check` 与差异空白检查通过。

### Live 同币种跨市场在途订单预留（2026-10-21）

Live 余额快照后的同账户、同 Venue 在途订单现在按其冻结 market spec 计算剩余名义额或初始保证金，因而支持同币种跨市场共享余额预留。市场规格缺失/冲突，或预留资金币种与余额资产不一致时仍 fail-closed；Live 跨币种抵押品与跨市场持仓统一估值仍未关闭。

- 定向验证：Live 在途预留行为 6/6；`cargo clippy --offline -j 1 -p qx-cli --all-targets -- -D warnings`、`cargo fmt --all -- --check`、`live_balance_order_reservation_check()` 与成熟度报告一致性检查通过。

# Changelog

### 0.2.1 多语言策略样例与安装包（2026-10-21）

- 新增 Python 动量、SMA 与均值回归策略实例，整理 Rust、Python、C++ 策略入口和跨语言回测说明。
- Rust workspace、Python wheel、C++ CMake SDK 与发布基线统一为 0.2.1。
- 发布流水线为 Linux、Windows、macOS 构建 CLI 二进制和 Python wheel，并构建 C++ SDK 包。
- 修正性能采样源码指纹：Rust workspace 版本号不再伪装成实现变化；旧指纹格式先写入兼容基线，后续仍按原 10% 阈值比较。

### 复核并固定交易所用户流空闲节奏（2026-10-21）

清除能力台账中已过期的“用户流空闲可能忙轮询”限制：CCXT 的秒回 idle 有至少 100ms 本地让步，Binance 用户流 idle 来自有界 WebSocket 读窗，分片与控制帧循环有明确上限。新增架构合同核对两种节奏来源及行为回归，并更正 CCXT 对本地节流的旧注释。

- 定向验证：CCXT 空闲节流用例、Binance WebSocket 超时用例和 worker 停机结构合同均通过；新增架构合同通过。前后端/SDK 五组架构检查通过，Python SDK 分层/DTO 用例 38/38；`qx-api` 86 个单测 + 36 个集成用例、`qx-sdk` 13 个单测 + 31 个独立消费者用例、C++ SDK 两个 smoke 均通过；`qx-core`/`qx-xingban` 回归与严格 Clippy 通过，覆盖事件账簿估值、A 股 T+1/库存、无前视和风险准入；A 股/币圈双目录回测验收与 ReplayVerifier 均通过，hash 分别为 `6be034a4cb0760fa` / `26fdd6b52d020700`。成熟度报告与格式检查通过。

### Paper 跨币种现货候选资金投影闭环（2026-10-21）

Paper 现货新单按报价币现金建立购买力，报价币可不同于账户账簿报告币种；同币种在途买单先按剩余名义额预留，再通过已配置的 Spot FX 汇率统一换算，缺失或冲突汇率时 fail-closed。Live 对未经统一估值的异币种现货单继续拒绝。新增 EUR 报价的候选单/在途单行为回归，并修正 Paper 账户准入架构检查中已迁移的锁调用与旧结算币购买力判据。

- 定向验证：跨币种 Paper 现货用例 1/1；`paper_account_admission_serialization_check()` 通过；成熟度报告生成一致、workspace 格式检查与目标差异检查通过。

### 持仓开仓杠杆进入账簿重放与 Paper 保证金估值（2026-10-21）

衍生成交的 `TradePosition` 账簿事实现持久化开仓杠杆，账簿重放会恢复按持仓数量加权的倒数杠杆比例；Paper 估值与 OrderBook 组合准入据此扣除标记价下的初始保证金。EventLog 摘要纳入新增杠杆事实，同时不改变缺少该字段的旧事件摘要。旧杠杆持仓在估值/准入时 fail-closed，避免用新订单杠杆伪造历史仓位保证金。

- 定向验证：`qx-core` 80 项单测、45 项集成通过（另有 1 项伸缩量具忽略）；qx-xingban 全 crate 146 项单测与 22 项集成通过；Paper 保证金 7/7、OrderBook 杠杆投影/旧仓拒绝 3/3、EventLog 后端切换 6/6、分段日志恢复 10/10、SQLite EventLog 9/9 通过。新增架构检查确保成交事实、EventLog 摘要、加仓比例归并、Paper 与 OrderBook 保证金投影和未知历史杠杆拒绝逻辑连通；格式及 qx-core/qx-risk/qx-runtime/qx-xingban/qx-cli 全目标严格 Clippy 通过。A 股/币圈确定性回测验收 hash 未变化，分别为 `6be034a4cb0760fa` / `26fdd6b52d020700`。
- 跨币种现货候选资金投影、Live 跨市场资金统一估值与完整发布矩阵仍未关闭。

### 清除过期的测试数量验收口径（2026-10-21）

修正架构门禁脚本概述中残留的“用例条数只增不减”说法。当前门禁只检查测试模块挂载、旧单文件不复活和共享夹具唯一性；测试数量通过实际测试目标执行验证，不设历史数量地板。历史变更记录中的旧计数仍作为历史记录保留。

### 移除历史诊断输出数量棘轮（2026-10-21）

删除 `process_diagnostics_check` 中固定未标记输出上限与 crate 名册一致性两项历史数量门槛。它们只约束旧快照数字，且成熟度记录与实际门槛已不一致；保留“引入统一日志框架时必须一次完成标签迁移”的架构合同。同步更新能力记录并重生成 `maturity/unverified.json`。

### Live 余额快照与在途订单预留（2026-10-21）

Live 下单入口现与 Paper 一样按账户锁串行化，并在锁内刷新 EventLog 后执行风控到 Venue 提交。风控按最近交易所余额快照的事件序号，预留快照后本地提交、尚未终结的同市场现货买单名义额或衍生品保证金；快照已覆盖的订单不会重复预留。余额快照后出现成交/现金流，或存在无法由当前 worker 认证的跨市场在途订单时，拒绝新单直到新快照抵达。

- 新增行为回归覆盖同市场预留、避免对旧订单双重预留、跨市场无法估值时 fail-closed；Live 提交链架构检查验证 Binance/CCXT 的调用根、账户锁、刷新、风控顺序。
- 定向验证：Live 行为 5/5；Paper 在途订单 1/1、Paper 提交终态 5/5；qx-risk/qx-cli 全目标 Clippy `-D warnings` 与格式检查通过。
- 未实现 Live 多市场账户资金统一估值、跨币种现货候选资金投影及持仓开仓杠杆持久化；这些作为发布前风控残余继续保留。

### Paper 账户购买力串行与现货预留修复（2026-10-21）

最终提交风控复核发现 Paper 同账户多个 worker 可能同时基于各自打开的旧 EventLog 快照判断购买力；现新增账户级文件锁，覆盖风控到撮合落账，并在锁内刷新 EventLog。现货买入购买力只按报价币现金计算，持有资产市值不再冒充可重复消费的现金；现货卖出由账户库存/在途卖量约束，不再要求报价币保证金。OMS 在途现货买单按剩余名义额预留，在途衍生品保证金继续按组合/FX 估值；跨币种现货候选单目前 fail-closed。

- 新增旧快照提交回归：两个 pipeline 都在第一笔成交前打开，第二次提交在锁内刷新后因可用资金不足拒绝。
- 定向验证：`qx-risk` 9/9，Paper 保证金 7/7，Paper 提交终态 5/5，策略订单身份 4/4；qx-risk/qx-cli 全目标严格 Clippy 与目标文件差异检查通过；A 股/现货回测准入 11/11、现货库存集成 2/2、三入口风控 parity 5/5。
- A 股和币圈示例回测验收均通过双次同目录字节一致、独立目录产物一致与 ReplayVerifier：A 股 `6be034a4cb0760fa`，币圈 `26fdd6b52d020700`。
- 新增 `paper_account_admission_serialization_check`，仅检查锁→EventLog 刷新→风控→撮合顺序与具名行为合同，不以检查数量作为质量门槛。
- 同轮架构检查复核发现 `risk_spec_fail_closed_check` 还钉着迁移前的 `check_with_spec` 调用。现改为验证 Bar、单市场和多市场 OrderBook 全部走 `check_with_context` 并进入统一 `RuleSet`，保留缺规格 fail-closed 与三入口 parity 合同；定向 6/6 通过。
- Live 已按最新交易所余额快照的 EventLog 序号预留其后的同市场未终结订单，并在 Binance/CCXT 提交中用账户锁串行刷新与风控；快照后成交/现金流、跨市场在途订单会 fail-closed，等待下一次余额同步。持仓开仓杠杆持久化、跨币种现货候选资金投影、Live 跨市场共享资金统一估值与最终三轮全链路验收仍未关闭。

### 订单簿衍生品组合保证金准入修复（2026-10-21）

移除按单笔初始保证金与账户标记权益对比的衍生品旁路，改由统一 `OrderRiskContext` 对已结算持仓、未终结在途订单和候选订单的投影初始保证金进行组合判定；现货现金/库存预留与既有撮合语义不变。当前持仓保证金仍按候选订单杠杆估算，Ledger 尚未保存持仓杠杆元数据，因此该口径不能替代交易所 free-margin 快照。

- 新增组合保证金单测和“双订单合计超限”订单簿集成回归，验证第二笔订单被规范风险门拒绝。
- 定向验证：两项新增回归通过，既有衍生品订单簿成交/账簿语义回归通过，`qx-xingban` 全目标 Clippy `-D warnings` 通过。
- 派发链抽查确认策略订单经 Binance、CCXT、Paper 最终 worker 风控；当时发现 Paper 可用资金未预留 OMS 在途订单，后续已由本文件顶部的账户串行与购买力修复收敛。Live 本地在途单与交易所余额同步仍待审计。
- 该轮不代表发布矩阵完成；全量架构门禁与工作区发布矩阵仍待开发收敛后执行。

### 能力可执行性深检：修一条真缺陷 + 落一份真执行的观测记录（2026-10-11 第 11 轮）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§2/§4.2/§9

这一轮不再看"声明"，而是**把命令敲下去**：逐条执行多语言 SDK 点名的核心能力，看哪些真跑得起来、
哪些跑不了、跑不了的卡在哪。执行过程中撞到一条真缺陷并修掉。

- **立案（真缺陷）**：`qx-cli paper-e2e` 在仓库根第二次跑就死——
  `Paper 主链路验收失败: 加载 Scheduler 状态失败: Io("missing field `input_refs`" at line 1 column 427")`。
  根因不是状态文件坏了，而是 **`JobSpec` 是持久化类型**（`JsonStateStore::save_scheduler_at`
  写进 `scheduler.json`、`load_scheduler_at` 下一次启动读回），而 `input_refs` / `output_refs` /
  `permission_scope` 三格被按**必填**加进来——于是任何在本构建之前写下的状态文件都读不回来。
  更刺眼的是这三格**零生产读者**（`JobSpec::validate` 一格都不看）：`必填` 这个选择唯一能产生的
  可观测效果就是让旧文件读不回来。**升级路径从来没有被跑过**——所有 `deploy/qianxing.scheduler.*.json`
  模板在加字段时同步改过，只有仓库里那份陈旧的 `data/qianxing-paper/scheduler.json` 会暴露它。
- **修法（具名缺省，而不是拒绝）**：按 `maturity/artifact_migration.yaml` 的口径——"显式迁移或
  具名拒绝"——这里走第三条同样成立的路径：**旧文件本来就合法**，那就不该拒绝。给缺键一个具名缺省：
  `Vec` 两格空表；`permission_scope` 用 `"legacy-undeclared"` 而**不是空串**（空串会把"写于本字段
  存在之前"与"显式声明了空权限"读成同一件事），与 V13 R9「金额字段缺键不许静默变 0」同一条纪律。
  `#[serde(default)]` 只改反序列化，**新写的状态文件逐字节不变**，已 bless 的 `config_fingerprint`
  与 RunManifest 哈希不会因为这次修复集体失真。
- **读失败改成具名拒绝**：`load_scheduler_state` 的 `map_err` 现在点名状态文件路径并给出唯一处置
  （"若该文件由更早的构建写出，按迁移口径删除后重建，不要就地改写"），不再只报一句 `Io(missing field ...)`。
  实测：把状态文件里 `timeout_seconds` 改成一个字符串，现在报的是
  `...（状态文件 data/qianxing-paper\scheduler.json；若该文件由更早的构建写出，按迁移口径删除后重建，不要就地改写）` 并以 2 退出。
- **三条行为用例**（`crates/qx-scheduler/tests/persisted_state_upgrade.rs`）：把三格从序列化结果里删掉
  再反序列化必须成功且落到具名缺省；删 `job_id` / `idempotency_key` / `timeout_seconds` 这些**真必填**格
  仍要拒绝（否则"容忍缺键"会退化成"什么都能读"）；`Scheduler::from_json` 读一份缺那三格的**整份状态**
  必须成功——最后这条最贴近事故现场。
- **新增 `persisted_state_upgrade_path_check` 三颗**：① 这型真是持久化的（写侧与读侧都在盘）；
  ② 每一格零读者声明格都容忍缺键（`#[serde(default...)]` 或 `Option<...>`）；③ 读失败是具名拒绝。
  **恒发三颗、不早退**。
- **新增 `tools/capability_execution_probe.py` + 仓库资产 `maturity/capability_execution.yaml`**
  （真执行，不是手写）：八条不需要凭据、不访问网络的路径被**真的跑过**并记下带数字的观测——
  单腿回测（`fills=1` + 稳定 `result_hash`）、多市场双腿（`groups=3` + turnover/fees）、
  订单簿深度撮合（L2 内核 + RunManifest）、**跨语言多策略**（Rust 撮合内核 + 两个 Python 策略
  worker 各跑各的，实测 `strategy-momentum` 与 `strategy-mean-reversion` 各 `fills=3`）、
  **多数据源批量对比**（`fast-backtest`：`jobs=2 completed=2`，BINANCE 与 OKX 两套数据/两个市场
  各跑一次并各落四件产物）、内置策略目录（17 条）、事件与控制面（`ecosystem`）、
  多策略运行拓扑（`strategies=2 workers=3`）。
  `delegated` 段指向 `maturity/paper_acceptance.yaml` 与 `maturity/backtest_acceptance.yaml`
  ——**同一件事不许有两份说法**。`blocked` 段如实登记两条**跑不了**的：多交易所实盘（`ccxt` 未装入
  `python/.venv`、无凭据、testnet 记录 `outcome=skipped`）与一等多账户（账户只在策略级建模，
  `RuntimeConfig` 没有 `accounts` 段）——每条都写明卡在哪条前置上。
- **新增 `capability_execution_check` 六颗**守这份记录：记录自述齐全；每条 `executed` 的**子命令词**
  真在生成器里（记录是跑出来的，不是手写的）；每条 `anchor` 在盘；判定落在闭集里、`blocked` 写明前置、
  `delegated` 指向在盘记录；每条 `executed` 都记了**带数字**的观测值（跑出 0 笔成交的"通过"是空跑）；
  能力名唯一且 evidence 在盘。**恒发六颗、不早退**。
- **⑤ 当场抓到我自己记录里的一处弱点**：`config validate` 只说"通过"、不带任何数，于是那条 `executed`
  的观测值是空的——判据立刻把它判红，探针改为直接数模板里真声明了几个策略/worker，观测值才变成
  `strategies=2 workers=3`。判据先咬了自己一口，这是它有用的证明。
- **同轮把三条行为用例从 `qx-scheduler/src/lib.rs` 搬到 `crates/qx-scheduler/tests/`**：搬进去会让
  `lib.rs` 从 1464 行涨到 1527 行，越过 `maturity/line_budgets.yaml` 的登记值——而那份预算**只降不升**。
  搬到集成测试目录后 `lib.rs` 回到 1464 行（= 预算），用例照跑。
- **读数**：门禁 **1080 条 / 地板 1020 / 0 FAIL**；`qx-scheduler` 21 条用例全绿；
  `persisted_state_upgrade_path_check` 变异 **3/3 KILL**（零附带、逐字节还原）。

### Python SDK 接口分层：把 §6.2 的八个子层从方案变成真包（2026-10-11 第 10 轮）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§6.2

方案 §6.2 明写「接口分层：`qianxing.core` 本地 Rust 用例；`qianxing.client` 远端 API；
`qianxing.data` provider/dataset；`qianxing.strategy` 策略 contract/worker；`qianxing.paper` 与
`qianxing.live` 明确依赖服务端执行；`qianxing.experiments` 参数对比；`qianxing.reports` 报告与
Evidence。顶层命名空间稳定导出常用接口，optional dependencies 不改变核心语义」。

- **立案（真实缺口）**：此前这一条**只写在方案里**。`qianxing` 是一个平铺的包
  （`app.py` / `cli.py` / `strategy.py` / `ashare.py` / `ccxt.py`），八个子层一个都不存在。
  集成方拿到的是 63 个名字的一整面墙——"我到底需不需要一台服务"这个问题在 import 那一行
  没有答案；而 `AppClient`（要服务）与 `run_backtest`（不要服务）就并排躺在同一个名单里。
- **新增七个子层包**（`qianxing.core` / `.client` / `.data` / `.paper` / `.live` /
  `.experiments` / `.reports`）；`qianxing.strategy` 本来就在盘（它是 `qianxing_strategy`
  这个独立发行包的视图），本轮把它并入同一套口径核。
- **每一层只回答一个问题**——"要不要一台服务"：`core` / `data` / `strategy` / `experiments` /
  `reports` 在进程内，`client` / `paper` / `live` 要一台 `qx-api`。分层不是装饰：
  `core` **刻意不导出** `AppClient` / `RemoteRunHandle` / `PaperClient` / `LiveClient`，
  `client` **刻意不导出**本地自由函数——想在进程内跑用例的人不该被要求先起一个服务。
- **新增 `python_sdk_layering_check` 十颗**：① 八个子层都有落点（包或单文件模块）且每层
  `__all__` 非空；② 每层导出的名字都在该层自己的 import 语句里有来源；③ 每层导出的名字都
  取自顶层稳定名单（或在按层点名的子命名空间名册里）；④ 名册不烂——名册里的名字真在那一层里；
  ⑤ 顶层 `qianxing.__all__` 与 `qianxing/app.py` 的公开类/函数集合**双向相等**；
  ⑥ `core` 里没有远端面、`client` 里没有本地自由函数且摆出 `AppClient`/`RemoteRunHandle`；
  ⑦ `paper` 只摆 `PaperClient`、`live` 只摆 `LiveClient`，两者都摆共用的 `TradeWorkflowClient`；
  ⑧ 核心层的 import 语句不点名可选适配器；⑨ 分层用例在盘；⑩ 六条具名证据一条不少。
  这一族**恒发十条、不早退**。
- **⑤ 是这一族里最硬的一颗**：顶层稳定名单与 `app.py` 的公开面**双向**相等（实测 63 == 63）——
  多导出 = 顶层比实现宽（承诺了不存在的东西），少导出 = 实现里有公开面没人认领（孤儿）。
  两个方向都是本仓反复栽过的那两类。
- **⑧ 只读 import 语句、不读散文**：`qianxing/data/__init__.py` 的文档里点名
  "适配器在 `qianxing.ccxt`" 是**说明**，不是依赖。按散文判会把说明读成依赖。
- **缺口如实登记**：§6.2 还有三件没落——同步 API 与 `asyncio` API 共享同一 contract、
  `RunHandle.events`、CPython 3.10+ × OS/arch 支持矩阵与不带 Rust 工具链的干净环境安装冒烟。
  它们写进 `maturity/capability-matrix.yaml` 的 `python_sdk` 行 limitations，不假装已经有了。
  其中 `RunHandle.events` 与 Rust 门面那一格同源：`qx-app` 没有事件日志，远程事件游标住在
  `qx-api` 的 HTTP 面——门面不凭空长出一个没有生产端的事件流。
- **同轮自己踩到一次**：② 的第一版解析器只按行切 import，括号式跨行 import
  （`from qianxing.app import (\n … \n)`）只切到第一行的 `(`，于是把 `core` / `experiments` /
  `reports` 三层的整份 `__all__` 都误报成"没有来源"（44 条假红）。改成**括号式整块先取走**再按
  逗号切，两侧都对上了。
- **门禁 1061 → 1071**（`GATE_CHECK_FLOOR` 1001 → 1011），`--snapshot` 已刷新；
  **10 枪变异全部 KILL、零附带红**（`.task-tmp/mutate_py_layering.py`，逐枪按文件换行符拼锚点、
  逐字节还原）。其中 ③④ 两枪改的是门禁自己的名册常量（名册只有门禁读得到），
  ⑨ 一枪必然连带 ⑩（文件不在盘时那六条确实也不在），已注明是同一次变异的两条真话。
  另新增 `python/tests/test_sdk_layering.py` 十一条行为用例（含一条**子进程**用例：
  核心层 import 不得顺带拉起 `ccxt` / `qianxing_ccxt` / `qianxing_ashare`）。

### 第三方 Rust 门面 `qx-sdk`：把 §6.1 的「稳定边界」从零建出来（2026-10-11 第 9 轮）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§6.1 / §7 阶段 0 / §7 阶段 4

方案 §6.1 要的是一个**给仓库外调用方**的稳定边界：`Client/Engine`、capabilities、spec 构造校验、
sync/async run、`RunHandle`、events/cursors、cancel/wait/result、report/verify、Provider/Venue 扩展端口、
统一错误，外加 semver、迁移指南、examples 与独立消费者构建。此前这一格**完全不存在**——
`maturity/capability-matrix.yaml` 的 `rust_sdk` 行自己写着「当前交付是 qx-cli 运行程序」。

- **立案（真实缺口）**：`qx-app` 是按用例组织、为 CLI / HTTP / Python 三个**仓内**门面服务的应用层，
  不是给仓库外调用方用的边界。集成方只能自己挑 `qx-app` 加若干领域 crate 拼，拼错了没人拦；
  §6.1 要的 semver 承诺、迁移指南、examples、独立消费者构建一样都没有。
- **新增 `crates/qx-sdk`**（`Cargo.toml` + `src/lib.rs` + `src/client.rs` + `tests/independent_consumer.rs`
  + `examples/quickstart.rs`），并登记进 workspace members。门面**只**依赖 `qx-app` 加 §6.1 点名的
  两个端口 crate（`qx-provider` / `qx-execution`），外加 `qx-strategy`（`BuiltinStrategyKind` 是
  `BuiltinStrategySpec::new` 的**参数类型**，而 `qx-app` 没有重导出它——这不是"拼接领域 crate"，
  是补全 `qx-app` 自己的输入类型；`qx_sdk_facade_check` ③ 把这个集合钉成**恰好**相等）。
- **门面里没有任何业务逻辑**：每个 `pub fn` 的函数体不超过 4 行（非空非注释行），
  只做重导出与转发；用例逻辑全部仍住在 `qx-app`。第 ② 颗判据守着这条。
- **新增 `docs/qx-sdk-migration.md`**：从"直接用 `qx-app`"迁到门面的迁移指南，含 §6.1 逐项覆盖表
  与一节**已知缺口**（events/cursors、发布验收、能力预检、Live）。
- **新增 `qx_sdk_facade_check` 十颗**：① 门面 crate 在盘、进 workspace members、包名 `qx-sdk`、
  版本取自 workspace；② 门面里没有业务逻辑（函数体 ≤ 4 行）；③ 内部依赖**恰好**是那四个，
  且 `qx-app` 的被依赖集合点名了它（与 `LAYER_SOLE_DEPENDENTS` 同一把尺）；④ §6.1 每项能力
  要么有名册实物、要么在缺口表里写明理由；⑤ 名册里的**门面符号**真出现在门面源码里；
  ⑥ 名册里的**源符号**真在源 crate 的公开面里（凭空写一个名字 = 断链）；⑦ §6.1「async run」
  在 `qx-app` 里有六条 `*Spec::start` 实物；⑧ 独立消费者在盘、只说 `qx_sdk::` 一种前缀；
  ⑨ examples 与迁移指南在盘且都被门面 crate 文档点名；⑩ 每条能力缺口都在能力矩阵的
  `rust_sdk` 行如实登记。这一族**恒发十条、不早退**。
- **⑤ 与 ⑥ 两个方向刻意都核**：只做 ⑤ 会放过「文档说有、代码没有」，只做 ⑥ 会放过
  「凭空声明一个源符号」。⑥ 的源 crate 公开面用**构造性取法**（`pub use` 按 `{...}` 展开 +
  `pub fn/const/struct/enum/trait/type/static` 声明名，先剥 `#[cfg(test)]` 与注释）。
- **缺口如实登记，不造一个假的**：§6.1 点名的 events/cursors 在 `qx-app` 里**没有**对应物
  （远程 Run 的事件游标住在 `qx-api` 的 HTTP 面）。门面**不**凭空长出一个没有生产端的事件流——
  那正是本仓反复栽过的"声明替不存在的代码作保"；改成空名册 + 缺口表，并由 ⑩ 要求同一个令牌
  出现在能力矩阵的 `rust_sdk` limitations 里（两张登记面不许各说各话）。
- **同轮顺手补一条真缺陷**：`qx-app` 的 `DEFAULT_WAIT_TIMEOUT`（`wait()` 的有限缺省，方案 §9）
  是私有 `mod run_handle` 里的 `pub const`、**从未重导出**，所以仓库外根本够不着它。
  加进 `pub use run_handle::{...}` 之后门面才能把它摆到公开面上——`qx-app/src/lib.rs` 一行改动，
  但那是「门面暴露了 qx-app 自己的洞」而不是门面自己造的东西。
- **doctest 在沙箱起不来**（`rustdoc --test` → `os error 231 ERROR_PIPE_BUSY`，且全仓此前
  没有任何一份可运行的文档示例）。示例因此放进 `examples/quickstart.rs`——`cargo test --workspace`
  会**编译**它，证据强度比 doctest 高而不是低；`client.rs` 里那个块标成 ```text``` 并写明原因。
- **门禁 1051 → 1061**（`GATE_CHECK_FLOOR` 991 → 1001），`--snapshot` 已刷新；
  **10 枪变异全部 KILL、零附带红**（`.task-tmp/mutate_sdk_facade.py`，逐枪按文件换行符拼锚点、
  逐字节还原）。其中 ④⑤⑥ 三枪刻意改的是 `tools/check_architecture.py` **自己的常量**——
  这三颗完全由那份常量算出，改数据文件够不着它们；三枪都逐字节还原。

### 主链路调用图 + 孤儿/重复逻辑清单：把 §5.1/§5.2/§5.4 的三段箭头钉在真实符号上（2026-10-11 第 8 轮）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§5.1 / §5.2 / §5.4 / §7 阶段 0 / §13.2

方案 §7 阶段 0 的交付清单里，「三条主链路调用图」与「能力矩阵」并列；§13.2 又明写「完成阶段 0，
并将能力矩阵和**接口调用图**纳入仓库版本控制」。能力矩阵在第 7 轮落地，本轮补调用图这一半。

- **立案（真实缺口）**：方案 §5.1 / §5.2 / §5.4 用箭头写了三条主链（数据→研究/回测、
  Paper/Live 订单闭环、多语言策略），但那三段文字**没有任何东西把每一段钉在真实符号上**。
  符号改名、文件搬家、用例删掉，三段箭头照旧读起来很完整——「文档替不存在的代码作保」
  正是本仓反复栽过的那一类。
- **新增 `maturity/chain-map.yaml`**：三条链 × 19 个 step，每步点名两件事——`anchor`
  = `<文件> <符号>`（符号必须真在该文件里声明：Rust 认 fn/const/struct/enum/static/type/trait，
  `.py` 认 class/def）与 `case` = `<文件> <用例>`（用例必须真在该文件里，**读原文**）；
  每条链另有 `entry`/`terminal`/`evidence`/`note`。`orphans` 段收**已确认**的孤儿。
- **新增 `chain_map_check` 十一颗**：① 调用图在盘、自述 kind 与 schema_version 正确、三段非空；
  ② `chains` 名册 == 方案点名的三条主链；③ 每条 step 的 `chain` 都是声明过的链；
  ④ 每条链内 step 的 `order` 恰好是 1..N；⑤ anchor 文件在盘且符号真在其中声明；
  ⑥ `case` 用例真在那个文件里；⑦ 每条链的 `entry`/`terminal` 都是链内真实存在的 step 名；
  ⑧ 每条链的 `evidence` 逐条在盘；⑨ **声明不是装饰**——anchor 符号必须在**它自己文件之外**
  的生产文本里出现过；⑩ 每条 orphan 的 anchor 能解析、`kind` 与 `disposition` 落在闭集里；
  ⑪ **孤儿记录必须是真的**——`no_production_consumer` 在外面真找不到读者，且没有链上 step
  被同时记成孤儿。这一族**恒发十一条、不早退**。
- **⑨ 与 ⑪ 两个方向刻意相反**：只做 ⑨ 会放过「把不存在的调用画进图里」，只做 ⑪ 会放过
  「把活着的东西记成孤儿」。两向都核才闭合。
- **本轮 ⑨ 当场抓到一条真缺陷**：`data_fetch` 原先锚的 `InMemoryProvider` 在它自己文件之外
  **零读者**——`InMemoryProvider::new` 的两个调用点都在 `crates/qx-provider/src/lib.rs` 的
  `#[cfg(test)]` 里。它之所以一直没被零读者判据抓出来，是因为裸词计数把
  `pub struct InMemoryProvider` 与 `impl … for InMemoryProvider` 这两行**声明本身**当成了读者。
  改锚到真有下游的 `JsonBarFrameProvider`（消费方在 `qx-cli` 与 `qx-data::ingestion`），
  并把 `InMemoryProvider` 如实登记进 `orphans` 段（`kind: no_production_consumer` /
  `disposition: retained_public_api`）。同轮 `python_worker` 一步从只在自身文件内被调的
  `serve_framed` 改锚到 `python/qianxing_bridge/strategy.py StrategyInput`。
- **同轮自作自受一次**：第一版把「`段:` 下若干 `- name: X` 记录」的解析在 `_matrix_sections`
  与 `_chain_sections` 里各写了一份，`merge_duplicate_block_check` 当场打出 9 行逐字重复窗口
  （`17737==18069`）。收成**单一份** `_parse_named_records(path, wanted)`，两张登记面共用。
- **门禁 1040 → 1051**（`GATE_CHECK_FLOOR` 980 → 991），`--snapshot` 已刷新；
  **11 枪变异全部 KILL**（`.task-tmp/mutate_chain_map.py`，逐枪按文件换行符拼锚点、逐字节还原）。
  其中 ② `chain-renamed` 一枪刻意记下 ②③⑦ 三条同红——只改一行 `name:` 时它们是同一次变异的
  三条真话，不是噪声。

### 能力矩阵：方案 §2 的基线复核产物落地，五态缺口替代布尔位（2026-10-11 第 7 轮）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§2 / §4.2 / §13.2

方案 §2 明写「基线复核产物必须是 `capability-matrix.yaml`、语言/API 契约映射表、依赖图和问题清单。
每个缺口标记为 `implemented-and-verified`、`implemented-unverified`、`partial`、`unsupported` 或
`blocked`，**不得用布尔值 `supported=true` 掩盖限制**」；§13.2 又要求「完成阶段 0，并将能力矩阵和
接口调用图纳入仓库版本控制」。语言/API 契约映射表在第 5 轮已落地，本轮补上缺的那一半。

- **立案（真实缺口）**：`maturity/capabilities.yaml` 是**另一条轴**——它记的是内部子系统的四档
  布尔阶梯（implementation / code_tested / sandbox_tested / production_approved），回答"这套代码本身
  走到哪一档了"。方案 §2 要的是那张基线表里每个领域/Provider/Venue 的**缺口状态**。此前：
  - 方案 §2 的八个领域（Rust 内核 / Python / C++ / Rust SDK / 多数据源与交易所 / 多账户多策略 /
    Web-API / 发布）**没有一个被逐条评过档**；
  - 三个真实存在的 `impl Venue for`（`PaperVenue` / `BinanceSpotVenue` / `CcxtProcessVenue`）
    没有任何地方记着它们的限制落在哪一格；
  - 而布尔位恰恰是最容易把「未验证」读成「已支持」的写法——`sandbox: true` 分不出
    "沙盒验过""没验过""只验了一半"。
- **新增 `maturity/capability-matrix.yaml`**：
  - `levels` = 五态闭集（顺序即从强到弱）；`capability_fields` = 方案 §4.2 的十四格 venue 字段名册
    （market/history/realtime/frequency/depth/order_type/account_model/fee/cancel/query/idempotency/
    rate_limit/sandbox/precision）；`provider_fields` = 数据面六格（订单类字段对 Provider 不成立，
    硬套会逼出一堆无意义的档位）。
  - `domains`：方案 §2 那张表的八个领域，每条带 `level` + `limitations` + `evidence`（真实在盘路径）
    + `next_gate`。
  - `venues`：三个真实 venue 逐格评档，`impl` 指向含该 `impl Venue for` 的文件，`kind` 区分
    `internal_simulation` 与 `external_venue`。PaperVenue 的 realtime/depth/rate_limit/sandbox 四格
    老实写 `unsupported`（它没有实时行情源、没有订单簿深度、没有外部限流）；两个外部 venue 全部
    `partial` / `implemented-unverified`，`sandbox` 一律 `unsupported`——因为 testnet 记录
    `outcome` 是 `skipped`（缺凭据），没有一格够格进 `verified`。
  - `providers`：三个真实 `impl DataProvider for`（InMemoryProvider / JsonBarFrameProvider /
    DemoProvider），`DemoProvider` 如实标成"生态冒烟用的演示 Provider，不是生产数据源"。
- **新判据族 `capability_matrix_check`（十二颗）**：① 矩阵在盘、kind 与 schema_version 正确、三段非空；
  ② `levels` 恰是五态且顺序一致；③ 全表没有把能力写成布尔值的地方（**判据自带反例自证**：喂它
  `supported: true` 必须报，喂它 `market: partial` 必须不报——一条从不发声的空判据靠不住）；
  ④ `domains` 名册 == 方案 §2 的八个领域；⑤ 每个领域档位落在闭集里；⑥ 每个领域都写了至少一条限制
  （「不得掩盖限制」的可核验形态）；⑦ 每个领域证据里至少一条仓库路径且每条都在盘；⑧ 每个领域都写了
  下一个闸门；⑨ **venue 名册 == 生产代码里真实的 `impl Venue for X`**，且 `impl` 就是含该实现的文件
  （多一个 = 凭空声明一个不存在的 venue；少一个 = 有 venue 活着却没人给它评档）；⑩ 每个 venue 逐格
  给齐十四格且档位合法、`verified_version` 非空；⑪ provider 名册 == 真实的 `impl DataProvider for X`
  且逐格给齐数据面六格；⑫ 外部 venue 声称 `sandbox`/`order_type` 已验时必须有 outcome=pass 的
  testnet 记录（**与 `capabilities_check` 同一把尺：没有证据不得翻真**）。这一族**恒发十二条、不早退**。
- **历史记录（当时）**：门禁 1028 → 1040 条、地板 968 → 980；当时曾使用 `maturity/gate_snapshot.json`。该数量快照门禁已删除，这些读数不再代表当前验收口径。
- **变异取证 13 发全 KILL**（`.task-tmp/mutate_capability_matrix.py`，按字节还原，每发都断言
  锚点唯一）。覆盖十二颗判据里的每一颗：整份登记面消失 / 五态重排 / 能力写成 `supported: true` /
  领域改名 / 档位写成闭集外的词 / 限制清空 / 证据指向不存在的文件 / `next_gate` 清空 /
  venue 名册凭空多一个 / venue 的 `impl` 指错文件 / venue 少给一格 / provider 档位非法 /
  外部 venue 无凭据却把 `sandbox` 翻成已验。
  - **首轮 `matrix-file-missing` 是 MISS，抓到的是我自己期望集写漏了**：登记面整份消失时，
    `levels` 名册也读成空表，② 跟着红。那条红是同一次变异的**真话**（不是假阳），已列进期望集，
    复跑 6/6 KILL。与第 5 轮 `contract-file-missing` 连带打红快照那颗同型——**「别的判据跟着红」
    要逐条判它是真话还是噪声，不能一律当噪声抹掉。**

### 等待默认有限：两条腿的 `wait()` 不再一个无界一个 30 s（2026-10-11 第 6 轮）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§9

方案 §9 的原话是「`wait()` 默认有限时；无限等待仅由调用方显式请求并可取消」。这条纪律此前一条判据
都没有，而同一个 SDK 的两条腿对「等多久」给了两个相反的答案。

- **立案（真实缺陷，不是理论风险）**：
  - Python 原生句柄 `RunHandle.wait(timeout_ms=None)` 的缺省是 `None`，一路透到 `qx-python` 的
    `loop { … }` 上就是**一直等**——调用方一个参数都不写就拿到了无限等待，而远端句柄
    `RemoteRunHandle.wait(timeout=30.0)` 的缺省是 30 s。
  - Rust 核心 `qx_app::RunHandle::wait()` **连超时参数都没有**，直接 `receiver.recv()` 堵到底。
    调用方不仅默认拿到无限等待，而且**拿不到「最多等多久」这个选项**。
- **修复**：
  - `crates/qx-app/src/run_handle.rs`：新增 `pub const DEFAULT_WAIT_TIMEOUT: Duration = 30 s`
    （与远端句柄同值），`wait()` 改走新的私有 `wait_within(timeout)`；有界等待用 `recv_timeout`，
    并且**超时单独一支且不置 FAILED**——到点没结果是「还没好」，不是「坏了」；把超时并进断连那一支
    会让一次正常的到点返回把还在跑的运行记成失败。想真正无界等待的调用方自己写 `status()` 循环
    （缺省不是无界，不等于不可取消：`cancel()` 仍然收得回来）。
  - `python/qianxing/app.py`：新增 `DEFAULT_RUN_WAIT_MS = 30_000`，`RunHandle.wait` 的缺省从
    `None` 改成它；`None` 保留为**显式**的无限等待。
- **新判据族 `bounded_wait_default_check`（五颗）**：① 两条腿的有限缺省常量都在盘且是有限正值，
  且 Rust 那个不超过 60 s（「有限」不能靠把常量写成十年）；② Python 两条腿的 `wait` 缺省都不是
  `None`；③ Rust `wait()` 走有限缺省常量而不是裸 `receiver.recv()`；④ 有界等待用 `recv_timeout`
  且超时单独一支不置 FAILED；⑤ 有界等待有行为用例在盘。
- **门禁 1023 → 1028 条，地板 963 → 968**；`--snapshot` 已刷新。
- **变异取证 5 发全 KILL**（`.task-tmp/mutate_bounded_wait.py`，按字节还原）。**这一轮变异抓出了
  我自己写的一条假判据**：② 第一版比的是 `matched.group(1)`（参数名）而不是 `group(2)`（缺省值），
  于是把缺省改回 `None` 时它照印全绿——`python-default-back-to-none` 那一发当场打出
  `rc=0 caught=0/1`。修掉之后五发全杀。另一发 `rust-wait-bypasses-constant` 除目标红之外还
  连带打红「零读者」那颗：绕过常量会让它没人取用——那条红是同一次变异的真话，已列进期望集。

### 语言/API 契约映射表：11 个用例在五种语言面里逐一对应（2026-10-11 第 5 轮）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§2 / §8

方案 §2 把「语言/API 契约映射表」列为基线复核产物之一，§8 又要求「跨语言同一 Run」。本轮把这张表
做出来并给它装上牙齿——它此前**根本不存在**，而缺的正是最容易被读成"已经齐了"的那一格。

- **立案（真实缺口，不是理论风险）**：`maturity/app_use_cases.yaml` 已经把 11 个用例与 Rust 的
  `cases/` 目录、`pub use`、`cases/mod.rs` 的九项文档表三处对上了，但**一个字都没提另外三种语言面**。
  也就是说：新增（或删掉）一个用例时，Rust 侧三处齐全、门禁全绿，而 Python 进程内扩展、Python 远端
  HTTP 门面、C++ 远端 HTTP 门面可以静默地少一个入口或多一个孤儿入口——集成方要到运行时才知道
  「这个方法不存在」。这与上一轮立案的「声明表不是装饰」是同一族：**登记面只覆盖了一条腿**。
- **新登记面 `maturity/language_api_contract.yaml`**：11 行，每行五格——`rust` / `python_native` /
  `python_remote` / `cpp_remote` / `http`。两个显式名册回答"这些公开方法为什么不是用例入口"：
  `python_native_helpers: [doctor]`（安装自检）与 `cpp_lifecycle_methods: [run_status, cancel_run]`
  （run 生命周期的读/写面）。写成名册而不是「排除 `run_` 开头的」这类规则，是因为规则会把将来新加的
  同形状方法一起放行，名册则要求有人为每一条回答一次。
- **新判据族 `language_api_contract_check`（九颗）**：① 映射表在盘、自述 kind 正确、每行五格齐且用例名
  不重复；② 映射表用例集合 == `app_use_cases.yaml` 的用例集合（两张登记面不得各说各话）；③ 每格点名的
  Rust 入口都是 `cases/<use_case>.rs` 里的 `pub fn`；④ 每格点名的 Python 入口在盘（`python_native` 看
  模块级 `def`，`python_remote` 看 `AppClient` / `TradeWorkflowClient` 的公开方法）；⑤ 每格点名的 C++
  入口都是 `AppClient` 的公开方法；⑥ 每格点名的 `http` 路由都在两张客户端路由声明面里（用例 → 路由 →
  声明面，三段接得上）；⑦⑧⑨ **反向普查**三条腿——客户端公开面里没有未被任何用例认领的入口。反向那三颗
  是本族最贵的一格：正向只核「声明的东西在盘」，一个多出来的 `AppResponse frobnicate(...)` 照样绿。
  这一族**恒发九条、不早退**（缺文件时后八条按空文本判，自然落空）——早退会让一次缺文件把总条数打下来，
  快照漂移判据跟着红，第二条红盖住第一条。
- **门禁 1014 → 1023 条，地板 954 → 963**；`--snapshot` 已刷新。
- **变异取证 5 发全 KILL**（`.task-tmp/mutate_language_contract.py`，全部按字节还原）：
  `contract-row-removed` 4 红（用例集合不齐 + 三条腿各自出现未认领入口）/ `cpp-extra-method` 1 红
  （只有反向普查那一颗）/ `http-route-wrong` 1 红（路由对不上声明面）/ `python-remote-bogus-entry` 1 红
  （点名的入口不在盘）/ `contract-file-missing` 6 红（含 `labels_sha256` 那颗——去掉一份 `maturity/*.yaml`
  会改登记面份数，而份数写在另一颗判据的 label 里，那条红是同一次变异的真话）。

### 远端客户端「声明表不是装饰」+ C++ 最小消费者工程补牙齿（2026-10-11 第 4 轮）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§6.3 / §11

上一轮把 C++/Python 两条客户端腿的路由**声明面**接到 qx-api 分派表上（`client_route_wiring_check`
七颗）。本轮接着问那七颗没问的问题：**声明表里的东西是不是真的在驱动请求**。

- **立案（真实缺陷，不是理论风险）**：`cpp/include/qianxing_app.hpp` 里 `routes::kRunStatusTemplate`
  （`/app/runs/{run_id}`）与 `routes::kRunCancelTemplate`（`/app/runs/{run_id}/cancel`）被列进了
  `kDeclaredRoutes`、也被门禁逐条核对过，但**没有任何代码用它们**：真路径由另一对
  `kRunsPrefix`（`/app/runs/`）与 `kRunCancelSuffix`（`/cancel`）拼出来。于是门禁比的是模板（对的）、
  请求发的是前缀——把 `kRunsPrefix` 改成 `/app/run/`，门禁照印全绿，`run_status` / `cancel_run`
  却会 404。「声明表不是装饰」这句承诺恰好在两个 run 生命周期入口上落空。
- **修复**：新增 `routes::fill_run_id(template, run_id)`，`run_path` / `run_cancel_path` 改成消费
  **声明的模板**（Python 侧本来就是 `.format(run_id=...)`，两条腿从此同一构造方式），删掉那对
  前缀/后缀常量。`cl -std:c++17 -W4` 编译零告警，`app_client_smoke` 运行退出码 0——它断言的是
  具体路径 `/app/runs/run-1` 与 `/app/runs/run-1/cancel`，所以这条修复有端到端证据而不是只靠读代码。
- **`client_route_wiring_check` 七颗 -> 九颗**：
  - ⑧ 声明面里每条路由都被客户端代码**真的取用**——C++ 的 `kX` 名字必须出现在 `kDeclaredRoutes`
    数组与它自己的 `inline constexpr` 定义**之外**，Python 的键必须被 `APP_ROUTE_TEMPLATES["key"]`
    字面量取用。匹配前先剥注释，否则「退回旧写法 + 注释里留一句锚点」照样绿。
  - ⑨ 反向普查：客户端源码里不得出现声明面**之外**的 `/app/...` 路径字面量。刻意只做「多出」这一向——
    声明面的字面量按构造就在源码里，「声明的字面量找不到」那一向永远不可能为真，写上去只是凑一个
    不可达分支（本仓口径：判据的每一向都要能真的红）。
- **Python 侧同族修复**：`TradeWorkflowClient.submit` 原来用
  `APP_ROUTE_TEMPLATES[f"{self.workflow}_commands"]` 动态拼键——「工作流名」与「路由键名」被一条
  没写在任何地方的命名约定绑在一起，两个键在源码里没有字面量读者（门禁核不出它们到底有没有被用上），
  键改名只会拿到运行时 `KeyError`。改成两个字面量分支。
- **新增 `cpp_consumer_example_check`（三颗）**：`cpp/tests/consumer/` 是方案 §6.3 点名的**下游独立工程**
  （`find_package(QianxingStrategySDK)` 消费 `cmake --install` 出来的导出 target），此前全仓
  `grep tests/consumer tools/check_architecture.py` **零命中**——工程被删、CMake 退化成相对路径 include、
  示例里另抄一份路径，门禁一条都不红。三颗分别钉：① 工程两件在盘；② CMakeLists 真走
  `find_package` + `add_executable` + `target_link_libraries`（剥 `#` 注释后判；相对 include 蹭进仓库
  测不出「workspace 外消费」这条退出门）；③ 示例源码取用 `qianxing::routes::k*` 且不另抄 `/app/...` 字面量。
- **同轮修掉示例里的坏示范**：`cpp/tests/consumer/main.cpp` 硬写 `"/app/compare-runs"`，改成
  `qianxing::routes::kCompareRuns`。示例是使用者照抄的模板，硬写一份等于把「第二份声明面」教给下游，
  而 header 里那份常量就摆在那儿。
- **这一族恒发三条**：`cpp_consumer_example_check` 刻意**不早退**——第一版在「文件缺失」时 `return`，
  变异实测发现「把 `main.cpp` 挪走」会同时把总条数从 1014 打到 1012，快照漂移判据跟着红，
  红出来的第二条会盖住真正的那条。缺文件时后两条按空文本判，自然落空，条数不随盘上状态变。
- **变异取证（七枪全杀，还原后按字节断言与 pristine 一致）**：C++ 声明面摘掉一条的消费（`run_path`
  不再用模板）/ 只在注释里留锚点 / Python 键不再被取用 / 方法体另抄一份 `/app/backtst` /
  消费者 `main.cpp` 挪走 / CMakeLists 摘掉 `find_package`（真摘 + 只注释掉各一枪）/ 示例改回硬写字面量。
  脚本：`.task-tmp/mutate_client_declaration.py`、`.task-tmp/mutate_consumer_example.py`。
- **端到端验证**：`cl -std:c++17 -W4` 编译 C++ 冒烟零告警、运行退出码 0；`cmake --install` 出包后
  下游消费者 `find_package` 配置 → 编译 → 运行退出码 0；Python 10 份测试全过；
  `cargo test --workspace --offline` **1340 passed / 0 failed**（`os error 231` 噪声 0 条）。
- `GATE_CHECK_FLOOR` 949 -> 951 -> 954；门禁实测 **1009 -> 1011 -> 1014**。

### 多语言 SDK 契约映射（2026-10-11）· 方案《牵星Qianxing-多语言SDK全能力升级与重构实施方案-2026-10-11》§2「语言/API 契约映射表」

本轮是「前后端贯通」审计的第一条：把远端客户端的**路由接线**从「各自写各的路径字面量」收成
「单一声明面 + 门禁逐条对账」。

- **立案**：`web_console_check` 只核控制台（`web/console/app.js`）点名的路径都在 qx-api 路由表里。
  远端客户端的另外两条腿——C++ 头文件 `cpp/include/qianxing_app.hpp` 与 Python
  `python/qianxing/app.py`——**此前没有任何判据核过**：客户端把 `/app/backtst` 拼错、或后端把某个
  路由改名，两边都不红，集成方要到运行时才拿到 404。这正是方案 §11 点名的「多语言契约漂移」风险，
  而门禁里没有一颗牙齿。
- **两条腿各自收成单一声明面**：C++ 新增 `namespace routes`（20 个 `inline constexpr std::string_view`
  + `kDeclaredRoutes` 数组），Python 新增 `APP_ROUTE_TEMPLATES` 字典；方法体不再各抄一份字面量，
  一律走声明面（`run_path`/`run_cancel_path` 拼接 `{run_id}` 模板）。
- **新增门禁 `client_route_wiring_check`（七颗）**：① 两份声明面在盘；② 两份声明面非空；③ 客户端声称的
  每个路由都能在 qx-api 分派表里找到（正向断链）；④ C++ 与 Python 两条腿逐条相等（跨语言契约漂移）；
  ⑤ qx-api 的每个 `/app/*` 入口都被客户端声明（反向：没有客户端能调的端点就是孤儿）；⑥ 每个路由字面量
  在客户端源码里只有一处定义（声明表不是装饰）；⑦ 每个路由都出现在对应语言的冒烟用例里（声明了却
  没人跑过的路由是孤儿）。`GATE_CHECK_FLOOR` 936 → 943，门禁实测 **996 → 1003**。
- **同轮修掉一处真实覆盖缺口**：`cpp/tests/app_client_smoke.cpp` 此前只跑 16/20 条路由，
  `validate_dataset` / `run_backtest` / `verify_run` / `compare_runs` 四个方法从没被运行过——补齐四条断言。
- **变异取证（四枪全杀，还原后按字节断言与 pristine 一致）**：Python 路由表拼错路径 / C++ 声明表摘掉一条 /
  方法体另抄一份字面量 / C++ 冒烟用例删掉一条路由断言 —— 各只红对应那一颗。
- **两条口径的落地校验**：C++ 头文件在 MSVC 默认代码页（936）下必须 **ASCII-only**（新增 CJK 注释会让
  MSVC 把多字节序列读成标识符、整份头文件编译失败），故本轮新增注释一律英文；`cl -std:c++17 -W4` 编译
  并运行 `app_client_smoke` 退出码 0；Python `python/tests/test_app_use_cases.py` 14 条全过。

同轮第二件（跨语言二进制契约）：新增门禁 `ring_layout_cross_language_check`（六颗），把 **QXRB 共享内存
环布局**在 Rust / Python / C++ 三处手写镜像之间逐格对照——此前全仓 `grep QXRB tools/check_architecture.py`
零命中，三处没有编译期耦合、也没有任何静态判据：任一处偏移或魔数漂移，读出来的就是错位内存或
header mismatch，而门禁照绿。六颗分别钉：三处实现在盘 / 九格常量都取得到 / Rust 与 Python 逐格相等
（MAGIC、VERSION、HEADER_BYTES、SLOT_HEADER_BYTES、WRITE_SEQ_OFFSET、READ_SEQ_OFFSET、
SLOT_COMMIT_OFFSET、SLOT_LEN_OFFSET、SLOT_CRC_OFFSET）/ C++ 的 `kHeaderBytes`/`kSlotHeaderBytes`
等于 Rust 的对应常量 / C++ 头文件的魔数与 header·seq·slot 偏移逐条对齐 Rust（子串指纹）/ 环布局有跨
语言运行时用例在盘。三发变异全杀（Python `HEADER_BYTES` 漂移 / C++ `kSlotHeaderBytes` 漂移 /
C++ header 校验偏移换错）。`GATE_CHECK_FLOOR` 943 → 949，门禁实测 **1003 → 1009**。

### V13 R31（2026-10-10）· qx-api 读面支持按账户过滤——控制台多账户作用域的后端前置

R30 的审计把控制台的多账户作用域判成「后端读面缺前置」而暂缓（另立任务）。本轮只做那一半：
`qx-api` 的读面先能按账户过滤，前端的账户选择器仍作为独立任务跟进（#26），本轮不碰 `web/console/`。

- **`/account/ledger` 与 `/reconcile/reports` 从「四条整体现读端点一律不收窄」里拆出来，立成一条按账户过滤的通道**：
  读的还是整份现读模型，但底层类型本来就带账户归属列——`LedgerEntry` 有 `account_id` 一列、
  `ReconcileReportSnapshot` 两列都有（`account_id` + `venue_id`）——所以 `?account_id=` 真的能过滤。
  账簿那一格只认一把键不是取巧：`LedgerEntry` 没有 `venue_id` 字段，给它配第二把收窄键只会让调用方
  以为按 venue 读过一遍。反过来 `/scheduler/runs` 与 `/control/audit` 的 `JobRun` / `AuditRecord`
  没有账户归属列，只能继续回 `400` 而不是照常 `200`。
- **过滤读面不回 `404 account_projection_not_found`**：键形状合法但过滤不出条目时回 `200 []`。
  两族口径的差别是刻意的——投影是 `(account_id, venue_id)` 键控的一份独立副本，没有那份副本就是
  「这个账户不在这份部署里」；账簿与对账报告是整本台账加一轮对账结果，过滤不出条目只说明这一轮没有事实。
  把「这一轮没有事实」读成「这个账户不存在」，正是 #191 那类读侧误读。
- **收紧错误出口而不是合并**：两条过滤臂都保住 `400`（键形状错）/ `503`（现读模型取不到）的分工——
  键拼错不会被读成后端故障，后端故障也不会被读成「你没说清」。
- **读面分类收成一个单点 `crates/qx-api/src/read_scope.rs`**：三张路由名单
  （`PROJECTION_SCOPED_ROUTES` 七条 / `MODEL_FILTER_ROUTES` 两条 / `KEYLESS_READ_ROUTES` 两条）
  与五张查询键名单都住在这里。`admission::refused_query_param` 只借名单、不再自持一份 `*_PARAMS` 常量
  ——此前名单散在 `admission.rs` 与 `lib.rs` 两侧，挪走一半就会让准入侧与分派侧各说各话。
- **新增门禁 `read_face_scope_check`（十颗）**：模块在盘且低于行数门槛并挂载 / 三张名单条目数自洽、
  两两不相交、每条都有活的分派臂 / 查询键名单只有 `read_scope` 一份而 `admission` 不再自持 /
  `accepted_query_params` 是唯一取名册出口 / **收窄键名单与底层类型的归属列逐格一致** /
  两条过滤臂真在结果集上过滤且不借用投影那族的 `missing_projection_response` /
  空结果回 `200` 而非 `404 account_projection_not_found` / `400`/`503` 分工不合并 /
  端点表只在真读收窄键的入口承诺查询串 / 九条行为用例在册。
  立项动机：`api_surface_doc_check` 只比路由集合、`api_endpoint_table_routes.rs` 只比表与分派是否平，
  **没有任何一颗判据看「这条臂有没有真读 `query`」**——于是 `/account/ledger` 可以既在表里写 `[?account_id=]`
  又一行过滤都没有，读者拿到的是默认账户那份流水而被当成了自己点名的账户。
- **九条行为用例**（`crates/qx-cli/tests/unit/api_read_route_query_contract.rs`）：两条过滤臂按账户过滤且
  venue 单独成键 / 空结果口径是 `200 []` 而不是 `404 account_projection_not_found` / 名单外的键、
  空串键、纯空白键一律 `400` 并点名被拒那把 / 两条无键端点继续 `400` / 七条带键读面自己的键契约没被
  这次改动带歪 / `snapshot/diff` 只返回文档承诺的那几个非 200 码 / 三张名单两两不相交且每条都有活分派臂 /
  端点表不再为这两条端点承诺 404。
- **接口文档同步**：`deploy/README.md` 两张端点表按新口径重写（过滤端点写出各自的收窄键、无键端点
  不再写 `[?… ]`），并修掉一处**行号死引用**——原文写「`admission.rs:413` 那句 `else { return None }`」，
  该句在上一轮重构后已不存在、行号也对不上，改指 `accepted_query_params(route)?` 这个稳定锚点。

### V13 R30（2026-10-10）· 一次性提交入口的多账户拓扑判据收口 + 第二交易所验收链的门禁牙齿

R29 之后做三轮发布前审计（主体链路连通性 / 孤儿逻辑 / 死循环 / 前后端贯通），两轮都按「先修干净全部问题、再进下一轮」推进。

- **三条一次性提交入口共用同一份账户/交易所拓扑判据（V13 R28 的隔离口径在一次性入口不能断）**：
  常驻 worker 循环一直有 `*_submit_matches_worker` 守「命令归谁」，但**一次性提交入口**这半边有三格缺口——
  ① CCXT 一次性入口（`run_ccxt_submit_order`，R28 新增的那条）显式点名 worker 却从不做拓扑比对，
  订单的 `account_id` 或 `instrument` venue 与点名的 worker 对不上时会把订单事实写进另一台 worker 的
  EventLog；② Paper 一次性入口没有 `worker_id` 参数、按「第一台启用的 Paper Execution worker」挑选，
  多账户共享一份控制面时这条退化成「取第一台」，订单会落进别的账户账本；③ Binance 一次性入口把同一条
  规则**就地再写了一遍**（`account_id != expected_account || !is_binance()`），是同一规则的第二份实现，
  改口径时漏改一侧就会一边拦、一边放行。三处现在都改走同一条判据：Paper 按 `paper_submit_matches_worker`
  挑出与订单账户一致的 worker、挑不出就 fail-closed（不退回取第一台），Binance/CCXT 在动作函数之前比对
  并落成终态 `Failed`。Binance 那份就地实现随之删除，规则只剩一份。
- **新增门禁 `submit_topology_guard_check`**（`SUBMIT_TOPOLOGY_GUARDS` / `SUBMIT_TOPOLOGY_ORDER` 两张表）：
  三条入口的拓扑判据必须落在动作函数被调用**之前**——把比对删掉、挪进动作函数、或 Paper 退回取第一台
  都会红。三处反向变异（删 CCXT 判据 / 删 Binance 判据 / Paper 换成无差别匹配）逐处打红后字节还原。
- **补 `ccxt-submit-order` 的行为用例**：`binance_submit_rejects_order_from_another_account_as_terminal_failure`
  与 `ccxt_submit_rejects_order_from_another_account_as_terminal_failure` 各断四格——错误文案点名「拓扑不一致」、
  审计留下 Accepted + 终态 `Failed` 两条、终态原因码带出拓扑不一致、EventLog 无订单事实且账簿为空；
  `paper_submit_rejects_order_without_a_matching_worker_as_terminal_failure` 断 Paper 挑不出匹配 worker 时
  既不给订单入账、也不写初始资金。
- **门禁为 `--venue okx` 这条链补两组牙齿**（`external_acceptance_check` 内）：此前 `ACCEPTANCE_CONSTANT`
  只钉 Binance 那五格常量，OKX 的八格常量（`OKX_BASE_CONFIG` / `OKX_CCXT_CONFIG` /
  `OKX_EXECUTION_WORKER` / `OKX_RECONCILE_WORKER` / `OKX_KEY_ENV` / `OKX_SECRET_ENV` / `OKX_PASS_ENV`）
  不在名单里——`--venue okx` 整条链路可以断掉而门禁不响，「第二交易所已通」就没有支点。新增四颗判据：
  常数组齐备且 `choices` 里真有 `okx`（不复制脚本而是参数化）、两份 okx 配置文件都指向仓库真文件、
  okx 验收脚本点名的 worker 在 okx 验收配置里存在且已启用、CCXT 配置的凭据引用与脚本检查的环境变量
  同名（同 Binance 那组口径，否则脚本会以为凭据齐了）。两处反向变异（删掉 `OKX_PASS_ENV` / 从 choices
  里摘掉 `okx`）都当场打红。
- **接口文档同步**：`README.md` 命令清单与 `deploy/README.md` 的「订单提交」一节补上 `ccxt-submit-order`
  的完整参数顺序（`runtime.json <worker-id> <ccxt-config.json> <command.json>`，比 binance 多一个 CCXT
  配置路径）与它和三条入口共用的拓扑口径；`docs/外部链路验收执行方案-V1.md` 修掉那条过期的三参数签名
  并按仓库惯例加「回写批注」而不是删历史；`docs/外部链路验收-凭据与测试信息清单.md` 的收尾清单按实测结果更新。
- **CLI 表面四面对齐已核**：`ccxt-submit-order` 在 clap 命令枚举、`cli.rs` 派发臂、`cli_help.rs` 帮助文本、
  `tests/command_surface.rs` 的 `CLI_COMMANDS`（46 条）四处一致，且 `--help` 实跑通过。
- **验证**：`tools/check_architecture.py` **940 项全绿**（935 → 940，`GATE_CHECK_FLOOR` 882 不动）；
  `cargo test -p qx-cli --offline` **457 passed / 0 failed**（R29 的 453 → 457，新增 4 条拓扑用例）；
  `cargo test --workspace --offline` **1231 passed / 0 failed**；`cargo clippy -p qx-cli --all-targets` 零告警；
  本轮改动的 9 个 Rust 文件逐个 `rustfmt --check --config skip_children=true` 全干净。
- **仍挂账的两笔（如实登记，不假装完成）**：① 诊断债 `UNLABELED_DIAGNOSTIC_CEILING` 仍在 55——剩余站点里
  有若干（`main.rs` 的 `rejected`/`acked`、JSON 直出站点）被测试逐字断言，加标签会当场红，要连带改测试才能收口；
  ② 控制台多账户作用域（四条读面加 `account_id`/`venue_id` 选择）需后端 `qx-api` 读面先支持按账户过滤，
  另立任务。

### V13 R29（2026-10-09 续）· 诊断债偿还（棘轮下探 100 → 55）

承接 R28 把诊断公共组件面钉成的「写下来的决定 + 有上界的欠账」，本轮开始偿还这笔债，并证明棘轮确实只降不升（不是躺着不动的允许清单）。

- **`cli.rs` 整段错误/诊断输出收到 `[qx-cli · CLI]` 标签下（45 处）**：outbox/event-consumer/DLQ 中继失败、init/doctor/config 入口失败、回测各子命令失败、supervise/scheduler/各 worker 启动失败、binance/probe/submit 失败等内部错误报告统一带组件前缀，运维可按组件过滤。这些站点的 CLI 用例只断言退出码与 stderr 关键词、不逐字断言文案，加前缀零风险。
- **门禁 `UNLABELED_DIAGNOSTIC_CEILING` 从 100 下探到 55**（`process_diagnostics_check` 三颗仍全绿：`qx-cli` 实测 55 ≯ 55）。`_diagnostic_site_census()` 实测：`qx-cli` 未带标签站点 **100 → 55**。
- **故意留在债里的（逐字被测试断言，迁了会红）**：`main.rs` 的 `rejected`/`acked`（`process_recovery.rs` 的 `assert_eq!(stale, "rejected")` / `assert_eq!(acked, "acked")` 逐字匹配）、若干 JSON 直出站点（`config.to_json()` / `plan canonical_json` / `data_validate` / `report` 的 `println!("{}", …)`）。
- **验证**：`cargo test -p qx-cli --offline` **453 passed / 0 failed**（cli.rs 改动零回归）；反向变异——把上界临时改 55 → 54，门禁在 `只降不升` 那颗当场打红（越界 `{'qx-cli': 55}` / 登记 `{'qx-cli': 54}`），还原后复绿。
- 计划文档 `docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md` 新增 **§18.6** 记录本轮；§18.4 的欠账读数同步更新为 55。`maturity/capabilities.yaml` 的 `process_diagnostics_have_no_framework_single_source` 限制项同口径更新。门禁仍 **935 全绿**，`GATE_CHECK_FLOOR` 882。

### V13 R28（2026-10-09）· 全量验收与深度改进（多账户并行隔离、CI 特性全关腿、诊断面棘轮）

**口径**：承接用户对 `docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md` 的八问——「是否已按方案全部修复 / 全部优化改进 /
主体链路是否全部贯通 / 多种运行方式是否全部可行 / 多账户·多资产·多策略·多运行模式同时并行是否可行 / 易用性与快速执行是否到位 /
日志与报错等公共组件是否统一封装 / 继续深度改进」。逐问的答复与实测证据写进该文档新增的 **§18**；本条目只记**改了什么**。
每一处都按仓库纪律配齐**门禁牙齿 + 反向变异验证 + 行为用例**，`tools/check_architecture.py` 从 **922 → 935** 条判据
（`GATE_CHECK_FLOOR` 869 → **882**），全绿。

- **① 多账户 / 多 Venue 并行运行的隔离，从「名字拼得对」变成「事实不串账」**：此前这条不变量只有命名层用例
  （`account_event_log_name` 拼得对）与读侧去重用例（同一账户不分裂），**没有一条用例真把两个账户的 worker 各跑一遍再翻开
  两本账看有没有串账**——命名对不等于事实不串。新增 `crates/qx-cli/tests/unit/parallel_run_isolation.rs` 四条行为用例：
  (a) 两个账户在同一 venue 上各跑一次**真实** `run_paper_execution_worker`，两本账互查「对方账户现金为 0」且不落
  `paper-events.json`；(b) 一条 `SubmitOrder` 命令只按 `order.account_id == worker.account_id` 分派——先跑第二账户的 worker
  拿到空订单簿，再跑主账户才成交；(c) 每个配置的账户各拿到自己那份投影源；(d) **真并发**：`Barrier::new(2)` 卡住两条线程，
  同时跑两个账户的真实执行 worker，两本账仍各自独立。新判据 `parallel_run_isolation_check` **七颗**（用例在册 / 第二账户日志名
  从唯一构造点 `account_event_log_name` 派生且用例里无字面量日志名 / 写侧两账户各跑一次真实入口 / 写侧跨账户零现金互查 /
  并发用例真开两条线程 + `Barrier::new(2)` / `paper_submit_matches_worker` 拿订单账户比对 worker 声明账户 /
  `seed_paper_initial_cash` 按 worker 声明的账户入账而非硬编码 `main`）；8 处变异全红，逐处字节还原。
- **② CI 补上「特性全关」的另一半腿**（收口 §17.11 登记的覆盖面缺口）：`feature-matrix` 只 lint「特性开着」的组合，
  `#[cfg(not(feature = "sqlite"))]` 那半边在 workspace 构建里从不被 clippy 看到（`qx-cli` 的 `default = ["sqlite"]` 经转发
  把 `qx-runtime/sqlite` 一并打开）。`.github/workflows/ci.yml` 新增 `feature-off` job，按
  `--no-default-features --all-targets` 对四个声明了非默认特性的 crate 跑 clippy（带 `-D warnings`）与 check。
  `ci_feature_matrix_check` 从 4 颗扩到 **6 颗**：`feature-off` 腿在盘且 clippy 步骤**单独**带齐三个旗标（变异 ② 实测踩到——
  原先在整段 job 文本里搜旗标，旁边 `cargo check` 步骤的同名旗标会替它满足）、有 `cfg(not(feature = …))` 分支的 crate
  都在腿上、腿点名的 `-p` 集合恰好等于声明了非默认特性的每个 workspace crate；5 处变异全红。**这条腿当场见效**：
  它抓出了本轮新写的 `parallel_run_isolation.rs` 里一处 `needless_borrows_for_generic_args`。
- **③ 公共组件面：报错已统一封装，诊断这一格落成「写下来的决定 + 有上界的欠账」**：`qx-core::error` 的 `ErrorCode` 闭集 /
  `Retryability` / `ErrorContract` / `QxError::contract()` 唯一映射表已由 `error_code_contract_check` 八颗钉住，不需要动。
  缺的是**日志**：全仓 `crates/*/Cargo.toml` 没有任何 `tracing` / `log` / `env_logger` / `slog` / `fern` / `log4rs` 依赖，
  245 处诊断（`_production_sources()` 口径）直接走 `println!` / `eprintln!`，靠一条**非正式**的 `[组件 · 子域]` 前缀约定；
  `qx-api`（8 处）与 `qx-orchestrator`（2 处）已全量带标签，`qx-cli` 还有 **100** 处未带。本轮**刻意不**一次迁移完 100 处
  （那是一次横跨 45 个 CLI 入口的纯文案改动，收益只有过滤一致性，代价是 100 处 diff 与随之而来的用例文案漂移），
  改为新增 `process_diagnostics_check` **三颗**把它钉成不可回退的欠账：① 一旦引入诊断框架，未带标签站点必须为 0
  （半迁移会在既有约定之外多出**第三种**约定）；② 未带标签站点数只降不升（`UNLABELED_DIAGNOSTIC_CEILING` 棘轮，
  表外 crate 一律要求 0）；③ 棘轮表是**活名册**（归零的 crate 必须从表里删掉、新增未标签站点的 crate 必须登记）。
  4 处变异全红。同轮把这条决定登记进 `maturity/capabilities.yaml`（`storage_consistency_contract` 下的
  `process_diagnostics_have_no_framework_single_source`，与该块既有的进程日志保留策略、写侧超时文案两条 limitation 同族）。
- **文档面**：方案文档新增 §18（八问逐条回答 + 三件落地 + 主链/运行方式实测证据表 + 仍未关闭清单 + 本轮读数），
  附录 B/C 与收尾注记按 2026-10-09 现状回写；同时**修正本文件与方案文档里能力块计数的一处口误**——
  此前写作「27 条能力块」，实测是 **25 条**（`maturity/capabilities.yaml` 的 `capabilities:` 段下 25 个两格条目，
  与附录 C 表格行数、§3.13 的「3 个 profile + 25 条能力」逐一对上）。

**本轮实测读数**：`tools/check_architecture.py` **935 项 PASS / 0 FAIL / 150 个 `*_check()`**；
`cargo test --workspace --offline` **exit 0 / 1227 passed / 0 failed / 1 ignored（122 个 target）**；`cargo test -p qx-cli --offline` **453 passed / 0 failed**；
`cargo clippy --workspace --all-targets -- -D warnings` **0 告警**；`--no-default-features --all-targets` 四条腿全绿；
反向变异 **17 枪全红**（并行隔离 8 + CI 腿 5 + 诊断面 4），逐枪字节还原。三条主链与运行方式现跑证据见方案文档 §18.3。

### V13 R27（2026-10-09）· 架构梳理稿的 P0–P2 改进项全量落地（写面稳态成本、保留策略、锁中毒、机读快照）

**口径**：承接用户「`docs/牵星qianxing_架构梳理与缺点分析.md` 文档梳理出来的改进项，全面优化改进存在的问题」，
按该文档 §7 的 P0–P2 优先级表逐条落地。每一处都按仓库纪律配齐**门禁牙齿 + 反向变异验证 + 行为用例**，
`tools/check_architecture.py` 从 **895 → 922** 条判据（`GATE_CHECK_FLOOR` 843 → **869**），全绿。

- **P0-1 账户级金额字段接真实生产者**（此前 `unrealized_pnl_raw` / `realized_pnl_raw` 恒发布 `null`）：
  `Ledger::realized_pnl_for` / `Ledger::unrealized_pnl_for`（`crates/qx-core/src/ledger/query.rs`）成为两个算点，
  `qx-cli` 的账户快照投影接上它们。缺标记价即 `None`（不是 0）、累计全走 `checked_add`——`margin_raw` /
  `frozen_raw` / `funding_raw` 三格**没有生产者**（保证金要保证金模型、资金费要资金费台账），按零读者纪律
  如实保留在 `SNAPSHOT_UNCOMPUTED_FIELDS` 并由 `snapshot_money_honesty_check` 钉住。四处口径同步改口：
  `schemas/account-snapshot-v1.json` 的 description、`maturity/capabilities.yaml` 的两条 limitation 删除、
  `deploy/README.md` 的「有算点」分组、`api_snapshot_money_fields.rs` 的逐字段点名。新判据
  `account_pnl_producer_check` 三颗；5 处变异全红。
- **P0-2(a) 写面稳态追加不再整份拷贝状态**：`LiveEventPipeline::ingest_once` /
  `register_order_with_correlation_once` 原先每次追加都先 `self.clone()` 整份状态（O(日志长度)）。
  现在拆成「薄包装（失败即回滚）+ `*_commit`（就地提交）」，回滚走 `rebuild_from_store(true)`——
  store 只在 `persist` 成功时才写，所以失败时它仍是旧账本，重放即回滚。`force` 形参是必须的：
  `append_at_engine` 会在 `append_checked` 之前就 `alloc_seq` 推过 `next_seq`，而 `events` 一条没变，
  常规重建的"日志逐条相等就跳过"短路会让这一格漏回滚（这条正是变异 ③ 抓出来的）。四个方法从顶格文件
  `pipeline.rs` 切进新子模块 `crates/qx-runtime/src/pipeline/commit.rs`（同 crate inherent impl，子模块可见
  父模块私有项），`pipeline.rs` **2480 → 2160 行**，顺带把文档 §7 的 P3「大文件拆分顺路做」也做掉。
  新判据 `pipeline_commit_rollback_check` 四颗；新行为用例
  `failed_ingest_rolls_back_the_appended_fact_and_the_seq_cursor`（未知订单的 `Accepted` 在**追加之后**才失败，
  断事件数与 `next_seq` 都收回、且回滚后仍可继续）；8 处变异全红。`seen_fills` 台账 census 跟着写面搬家。
- **P0-2(b) EventLog 的保留/归档策略从「没有策略」改成「写下来的决定」**：这本日志是账簿与全部内存索引的
  **重放源**，所以策略是①**册内保留被禁止**（`EventLog` 公开面不提供也不得新增任何截断/淘汰/压缩入口——
  活日志变短会让账簿重放与 `refresh_latest` 的前缀校验同时失去参照物）；②**轮转是运维杠杆**（换一本账靠换
  `log_name`，旧日志原样留着）；③**归档粒度是封段**（分段后端只有尾段可增长、满段永远不可变，段清单带摘要
  校验）。正文写在写面模块文档（`pipeline.rs`），`EventLog` 的类型文档留一行指针（`sourcing.rs` 顶格在行数
  棘轮上，加行会红）。新判据 `event_log_retention_policy_check` 三颗；登记面两条 limitation 的旧读数
  （"`ingest_once` 每次追加仍先 `self.clone()`"）按"部分收敛"改口；8 处变异全红。
- **P1-5 名义额规则缺规格 fail-closed**：`legacy_spot_spec` 删净，`MaxNotionalRule` 体点名缺规格消息且不再
  合成临时规格，`RiskGate::check_with_spec` 在盘且两条回测路径都传规格。新判据 `risk_spec_fail_closed_check`
  六颗；8 处变异全红。**收口时才发现 parity 面被这次改动打红**（`cargo test --workspace` 里
  `contract-tests/tests/risk_parity.rs` 5 条挂 4 条）：三入口一致性用例的 (c) 腿原先走**无规格**的
  `check_with_price`，fail-closed 之后它把"缺规格"当成一条业务拒绝，于是 (c) 与 (a)/(b) 的 violations
  集合再也对不上。修法不是放宽判据而是**把 (c) 腿换成带规格的 `check_with_spec`**——这正是两条回测生产
  路径今天调用的形状，用例因此比改前更贴近生产。另加一颗判据（第 ⑥ 颗）钉住 `assert_gate_matches_canonical`
  的**函数体**必须出现 `check_with_spec(` 与规格实参，3 处变异全红（含"只在注释里写锚点"那一处，证明
  判据读代码不读散文）。reduce-only 用例同时补一条反向断言：**无规格入口只能多报"缺规格"，绝不能少报
  reduce-only 那条**——fail-closed 不得波及与规格无关的不变式。
- **历史记录（当时）**：P2-6 曾让 `--snapshot` 同时写行数棘轮与门禁数量/label 摘要，并要求实测值等于快照。
  后续复核已确认固定数量、标签摘要与静态源码行数都不代表质量；数量快照验收及 `maturity/line_budgets.yaml` 行数棘轮均已删除。
  README 两处写死的条数与地板改成引用文件名。**落地时踩到两个自己造的坑**：① `gate_snapshot_check` 在
  "快照缺失"与"快照在盘"两条路径上各发**不同条数**的判据，于是生成轮记下 N、复核轮算成 N+1，那条相等判据
  永远不可能成立（修法：恒发三条）；② 改了地板常量之后旧快照必然与常量不等，而那颗判据跑在写快照之前——
  生成本身被自己挡住（修法：生成轮把盘上那份当"不在盘"处理）。新判据 `gate_snapshot_check` 三颗 + 收尾一颗；
  4 处变异全红（其中一处走完整门禁）。
- **P2-7 发布版本单源 + CI 显式钉解释器**：wheel 版本 == workspace 版本；`rust-core` 作业设 `QX_PYTHON`。
  新判据 `release_version_single_source_check` 两颗。
- **P2-8 `qx-api` HTTP 路径上的 `.expect(` 改成显式 503**：19 处 `api state mutex poisoned` 全部走新的
  `crates/qx-api/src/state_lock.rs`（取锁收成可失败入口 `lock_state`，读面错误收口 `read_error_response`
  把中毒判成 **503**、其余 400），`QueryPort` 九个读法一并改回 `Result`。新判据 `api_lock_fail_closed_check`
  五颗；新用例 `crates/qx-api/tests/api_lock_fail_closed.rs`（真中毒 + 13 条读面断言 503 + 对照组）；
  5 处变异全红。
- **P1-3 / P1-4 复核结论：已在册，不重复实现**。权益 8 入口收敛成 `ValuationContext`/`ValuationResult`
  与 `qx-execution::RiskVerdict` 改名都是 V13 R23 已落地的（分别由 `valuation_single_source_check` 与
  源码注释点名 `qx_risk::RiskDecision` 守着）；`python/qianxing_ashare` 与 Rust 规则的跨语言对照已由
  `ashare_cross_language_contract_check`（14 颗）与三份夹具承载。按「不要图省事重做」处理，只做复核。
- **行数棘轮**：`--snapshot` 重生成 `maturity/line_budgets.yaml`，除本轮真实下降的
  `pipeline.rs`（2480 → 2160）外，还同步了几处**非本轮**的既有下降（`qx-scheduler/src/lib.rs` 1560 → 1464、
  `ccxt.rs` 1163 → 1160、`qx-api/src/lib.rs` 2969 → 2967、`event_pipeline.rs` 798 → 787、
  `strategy_host.rs` 915 → 908、`api_snapshot_money_fields.rs` 518 → 517），并移除了已不再超 500 行的
  `crates/qx-cli/src/config_commands.rs: 539`。棘轮只降不升，记录实际值是正确的收紧方向。

### V13 R26（2026-10-09）· 文档面产品化：README 重订、使用文档口径重钉、七份规划稿归档

**口径**：承接用户「更新 readme 说明文档，向产品说明文档一样，同时更新项目的使用文档，删掉旧版本无效的历史
文档信息，全部更新之后，提交最新变化」。这一遍**不动运行面代码**，改的是「文档说的数」与「代码里的数」之间
的那点差——而那点差是可数的。

- **README 产品化**：根 `README.md` 684 → **551 行**。新增「交付面一览」（八格，每格点名「谁在核对这个数」：
  CLI 入口名 46 / 内置策略 17 / HTTP 路由 18 / WebSocket 事件流 1 / `deploy/` 顶层模板 53 / workspace 成员 25 /
  架构判据与用例地板）与「看结果：离线报告、静态页面与同源 BFF 控制台」；把只在本机可复现的逐轮读数
  （`all` 链现场、产物身份那段完整措辞、第二棵源码树实测）摘进 `docs/archive/README-历史实测快照-2026-10-09.md`，
  正文只留**行为**与它的核对者。`demo` 段按 2026-10-09 本机重抓：退出码 0、`digest=915bfad3b6872a36`。
- **使用文档口径重钉**（`deploy/README.md` 八处，全部单行改，行数一根手指都没动）：HTTP 入口从「17 条」改成
  **18 条（17 条 `GET` 读面 + 1 条 `POST` 写面）**——`/schema/contract-matrix` 上线后两张端点表各含 18 条路由，
  文档、门禁与用例却还停在 17；认证豁免名册补 `/schema/contract-matrix`（判定体 `crates/qx-api/src/lib.rs:1498`
  的 `matches!` 里就是这四条）；「不判查询串的全局出口」四条 → **五条**，覆盖面算术随之改成 12 条读面入口 +
  5 条明写边界 = 17 条 GET；那处指向 `else { return None }` 的引用从 `crates/qx-api/src/admission.rs:404`
  改成 **:413**（404 是函数签名行，那句 `else {` 起于 413、`return None` 落在 414——行号能落地但落点说错了话，这类指针门禁抓不到，
  靠人核）；`GET /control/audit` 那格的「上面那四条」改成**三条**（它上面那行只摊了三条 keyless 入口）；
  带 `environment` 的 runtime 模板 18 → **19 份**（paper 15 / sandbox 2 / testnet 1 / production 1）。
- **文档契约用例跟着改口**（两份文件 8 处单行）：`api_endpoint_table_routes.rs` 四道地板 17 → **18**（分派集合、
  两张表各自的路由解析、语义表行数）加两道失败详情里的「17 条 / 17 行」；`api_response_field_doc.rs` 的
  `rows.len() >= 14` → **`>= 18`** 与详情里的「17 条路由」。收紧是加性的：原口径下只有整行被删才红，
  现在表被读窄也红。定向复跑 `cargo test -p qx-cli --bin qx-cli -- api_` → **42 passed / 0 failed**。
- **登记面**：`maturity/capabilities.yaml` 三处活口径（两条证据里的 `deploy/` 顶层 52 → **53** 份、
  `api_surface_doc_check` 那条的 17 条路由 → **18** 条）。**带日期的当轮读数一律不改**——CHANGELOG、
  `docs/archive/**`、门禁注释里的历史条目、V13 §1 与 2026-10-06 架构稿 §14 那批写着「本轮实测」的基线，
  它们记录的是当时成立的事实，追改等于销毁证据。
- **七份规划稿归档**：四份 2026-10-03 规划（架构审计 / 量化回测 Web 平台 / 桌面与 Web 客户端接入 / 与 tauron
  一体化桌面）、一份 2026-10-04 合订本、一份 2026-10-08 综合路线图、一份 2026-10-08 发布审计（V13 R22）
  `git mv` 进 `docs/archive/`。每份只在**文末**追加 14 行「归档状态」节——追加在标题下会让活文档里那些按行号
  取证的 `path:NN` 整体下移。路径提及同步 11 处：活文档 6 处（`CHANGELOG.md` 1 / V13 3 / 2026-10-06 架构稿 2）
  与同批互指 5 处。归档理由逐份写在自己的节里，其中 tauron 那一份靠**全仓扫描**立据：`crates/`、`web/`、
  `python/`、`deploy/`、`tools/`、`schemas/`、`.github/` 七处目录里 `tauron` 与 `tauri` 两个关键字各 **0 命中**。
  `docs/archive/README.md` 索引 33 → **48 行 / 8,519 字节**，十条在册，读法纪律里那处「README 的『当前状态』」
  指针同步改成「交付面一览」与「版本与现状」。
- **引用可达性重钉**：`LIVE_DOC_CITATION_FLOOR` **195 → 272**（终字节实测 272 条 `path:NN`，落在 30 份活 .md 上，
  落空 **0** 条；上一档 195 是 R1-G 在那七份还住在 `docs/` 时量的，中途 270 是这条 CHANGELOG 入库前测的那一档）。存档侧落空 9 条 = 天花板 9，**未升**，
  `ARCHIVE_DEAD_CITATION_CEILING` 不动；`GATE_CHECK_FLOOR` 与 `WORKSPACE_TEST_FLOOR` 也不动（本轮没加判据）。
- **环境腿的一次假红**：`cargo test --workspace --no-fail-fast` 首跑在默认 `%TEMP%` 上 **22 颗用例红（121 段 / 1196 passed / 22 failed）**，
  正文只有两种、同一族：**13 颗是「SQLite 打不开库文件」、9 颗是 `Os { code: 5, kind: PermissionDenied }`**——前者全在 `qx-storage`（按源文件落点：**`sqlite_event_log.rs` 7 颗 + `sqlite.rs` 5 颗 + `snapshot_rows_persist.rs` 1 颗**），
  文案是 `Io("SQLite: unable to open database file: …\\Temp\\qianxing-*.sqlite")`；后者散在
  **`qx-strategy` 6 颗、`qx-adapter` 2 颗、`qx-cli` 1 颗**，文案统一含 `code: 5, kind: PermissionDenied`（`Io(Os {...})` 与裸 `Os {...}` 两种写法都
  指向同一件事：`%TEMP%` 拒绝写入）。把 `TMP`/`TEMP` 指到仓内可写目录后整树复跑 → **121 段 / 1218 passed / 0 failed / 1 ignored，`TEST_RC=0`**；
  这一族用例都在 `%TEMP%` 下建文件，与被改的文档面无关。
- **验证**（全部本轮现跑）：`python -X utf8 tools/check_architecture.py` → **895 项 `[PASS]` / 0 项 `[FAIL]` / rc 0**；
  `cargo test --workspace --no-fail-fast` → **121 段 `test result:` / 1218 passed / 0 failed / 1 ignored**；`cargo clippy --workspace --all-targets -- -D warnings` →
  **rc 0，诊断行 0 条**；`cargo fmt --all --check` rc 0；`python -X utf8 -m unittest discover -s python/tests` →
  `Ran 68 tests / OK`；`git diff --check` 无空白错误。
- **本轮没做**：`docs/牵星qianxing_架构梳理与缺点分析.md`（2026-10-09 由仓库外的动作新建，391 行，
  0 处 `path:NN` 引用）留在未跟踪状态，不代它决定是否入库；§17.11 那两条 `--no-default-features` 的 clippy 腿
  仍在 CI 之外（既有登记，未动 `.github/workflows/ci.yml`）。


### V13 R25（2026-10-09）· 两轮复扫：字段级接线、游标口径与登记面自身的可机读性

**口径**：承接用户「继续检查还有哪些逻辑存在潜在问题，主体流程全部联通，核心链路不存在断链，逻辑连贯
不存在孤儿逻辑，不存在死循环，前后端全部贯通。至少检查 2 轮，每遍修复全部问题之后再执行下一轮」。
本轮把**前端 ⇔ 后端**这条链从"路径级全绿"下压到**字段级**，并把二轮复扫抓到的游标口径、登记面机读性、
死信台账增长三件事一并收口。全部加性：门禁 **853 → 895 全绿**（`[PASS]` 895 行 / `[FAIL]` 0 行 / rc 0），
`GATE_CHECK_FLOOR` **801 → 843**，`WORKSPACE_TEST_FLOOR` **1083 → 1233**，未删任何既有判据。

- **第一轮（前端字段级断链，全部修复）**：`web/console/app.js` 七格取数名册按后端线格式逐字段重订
  （快照身份回到 `header` 而不是顶层、`PositionSnapshot` 那三列名册外的幻影键删掉、`event_seq` 回到
  envelope）、定点金额走唯一标度出口（`RAW_DECIMALS = 9` ⇔ `qx-protocol` 的 `SCALE = 1_000_000_000`）、
  四个时间列走唯一毫秒出口、WS 帧按 `qx-api/src/ws.rs` 那份词表分派、写面补齐 `X-QX-CSRF` 双提交并把
  BFF 的三种拒绝如实转述、非 200 不再静默。`index.html` 三张表的列定义与 `<th>` 数逐格对齐。
- **第一轮的两条后端真缺陷**：① `crates/qx-api/src/lib.rs` 的审计身份覆盖原先住在 `match &self.policy`
  的 `Some(policy)` 臂里，`api.operators: {}`（出厂模板与 `--init` 的默认形状）这条路上**永远走不到**，
  页面表单里自称的 `operator_id` 原样进审计——现以认证边界为准，并新建
  `crates/qx-api/tests/console_operator_identity.rs` 走公开面 `ConsoleFront::handle` 做常驻反例（刻意用
  **没有**名册的 `ApiService::new`，`src/console.rs` 里既有的用例全都配了名册，恰好是缺陷藏得最深的那一侧）；
  ② `qx-orchestrator` 在 `spawn` 成功而 `stdin.take()` 落空时把子进程丢掉，现先挂进 `children` 台账，
  由唯一那处 `stop_managed_children` 按预算轮询 `try_wait` 收摊。另修 `qx-core/src/clock.rs` 的 `Ts`
  文档口径（**epoch 毫秒**，不是纳秒——当成纳秒的下游会把 2026 年渲染成 1970 年），以及
  `qx-runtime/src/pipeline.rs` 在非 `sqlite` 特性下的 `unused_variables`（关掉默认特性的那条构建腿会在
  依赖 crate 里冒出诊断，`-D warnings` 当场红）。
- **第二轮（复扫新抓到的四类）**：① 页面与 `qx-api` **并存两套游标约定**——后端 `after` 是"最后看到的
  seq"、回 `seq > after`，页面原先每追平一次就写 `seq + 1`，下一轮必然吃一发 409 `cursor_ahead`；现收成
  一个 `noteCursor(seq)` 出口，"没有基线"用 `null` 且**不带** `after`，WS 重定基走同一个出口；② 名册里
  留着一条**没有任何读者**的路径，而 WebSocket 通道名是第二份字面量；③ `maturity/` 的 8 份登记文件里
  **曾有 2 份**（`capabilities.yaml`、`http_surface.yaml`）对标准 YAML 解析器是 `ScannerError`——机读面
  只对门禁可读，对编辑器 / CI lint / 下一个工具是坏的，38 + 3 行标量加引号后**内容一字不改**（与
  `.tmp-audit/backup_*.yaml` 逐行对照）；④ 死信台账与已处理幂等台账在三条后端上只追加、无保留期。
- **牙齿**（全部加性）：`web_console_field_wiring_check` 新建 **三十一颗** → 二轮 **三十四颗**；
  `capabilities_check` 三颗 → **四颗**（自带四条自证：坏形态放得进、引号/块标量/`- key: value` 豁免得了）；
  `bounded_growth_and_reap_check` 九颗 → **十颗**；`console_front_check` 十七颗 → **十九颗**；
  `web_console_check` 九颗 → **十一颗**；`resource_lifecycle_and_lock_reentrancy_check` **+两颗**。
  `WORKSPACE_TEST_FLOOR` 1083 → **1233**：地板自 R12/R13 起一直停在 1083，而磁盘在册实测已到 1233——
  12.2% 的余量可以让一整族用例消失而门禁照印全绿，正是这颗地板立起来要抓的那种失效。
- **反向变异 35 枪全红**（KILLED 35 / COLLATERAL 0 / EQUIVALENT 0 / BAD 0，十六份被改文件逐份 sha256
  比对回原字节）：二轮 14 枪覆盖五颗新牙齿，一轮 16 枪里有 **三支是空枪**（F01/F14/R02 的锚点住在修复前
  的写法上，轮末字节上 `ANCHOR_BAD hits=0`——空枪不等于牙齿抓不到），重订锚点复打成 E01–E04 全红；
  F05 那一枪其实早已打红，只是点名子串写的是常量名而判据文案是中文，订正后复打。cargo 侧 I01：摘掉服务端
  身份覆盖 → 常驻反例红（`failed=1`），还原后同一条用例在还原字节上绿（`passed=1`）。脚本在
  `.tmp-audit/battery_r25*.py`、`.tmp-audit/refire_r25c.py`、`.tmp-audit/mutate_identity.py`。
- **本轮实测（收尾验证，读数全部由日志派生）**：`cargo fmt --all --check` rc 0；`cargo clippy --workspace
  --all-targets -- -D warnings` rc 0 / **诊断 0 行**，另加两条**关掉默认特性**的腿
  （`-p qx-runtime` / `-p qx-storage --no-default-features --all-targets`）各 rc 0 / 诊断 0 行；
  `cargo test --workspace --no-fail-fast` **121 段 `test result:` 行、1218 passed / 0 failed / 1 ignored**；
  `cargo test -p qx-cli --no-default-features --features sqlite` 19 段 **448 passed / 0 failed**；
  `python -m unittest discover -s python/tests` **`Ran 68 tests / OK`**；`python tools/check_architecture.py`
  **895 项全绿**；`git diff --check` rc 0。
- **文档回写**（同一轮内、按现读实测）：两份主文档与 README 的门禁条数、`GATE_CHECK_FLOOR`、
  `WORKSPACE_TEST_FLOOR`、门禁脚本规模（14,729 行 / **141** 个 `*_check()` / **814** 处 `check(...)`）、
  能力矩阵（**3 个 profile + 25 条能力**，`implementation` / `code_tested` 各 **21**）、CI job 数（**14**）、
  行数棘轮（**46** 份）、`deploy/` 顶层模板份数（**52 → 53**，逐处区分"今天的读数"与"那一轮的读数"）、
  workspace 成员数（23 → **25**：24 个 `crates/qx-*` + `crates/contract-tests`，架构图与分层表同步）
  一并回写；`crates/qx-cli/build.rs` 那句注释不再硬编份数（改由 glob 决定）。新增主文档 §17.11 记录这两轮。
- **按实测留在册、本轮不修的三条**（写进 `maturity/capabilities.yaml` 的 limitations，不是"发现而未处置"）：
  `idle_reply_windows_have_no_local_pacer__the_cadence_is_the_childs_cooperation`、
  `venue_adapter_capability_and_health_surface_has_no_production_reader`、
  `consumer_state_processed_ids_and_dead_letters_grow_without_retention`。另登记控制台**多账户作用域**
  【设计意图，未排期】：页面四条读面都不带 `account_id`/`venue_id`，走无键分支读全局投影，按账户投影部署时
  页面读不到那些账户，且无法区分"这个账户没账"与"没有全局投影"——现状是单 operator 单账户。
- **诚实登记的一处环境读数**：本机 `%TEMP%` 在这一轮里对 Rust 进程不可写（`std::fs::write` 回
  `Os { code: 5, PermissionDenied }`，同一目录 Python 写得进），三颗要写临时文件的用例因此红过；把
  `TEMP`/`TMP` 指到仓内可写目录后同一条链 0 failed。这是**机器状态不是代码**，Linux CI 不受影响，登记在此
  是为了下一轮不再去追这条。
- **仍未关闭**：`qx-cli serve` 自动挂载 console；桌面 Host、多机反代 / 第三方对外暴露（M4'）；
  Paper/sandbox/production 外部验收（M5'）。本轮不创建 tag、不触发 Release。


### V13 R24（2026-10-09）· 控制台易用性三件：令牌缺失自动生成 + `--init` 脚手架 + `--generate-token`

**口径**：承接用户「是否可以在本地启动 web 服务？易用性是否可以继续提升？」与「没有配置
`QX_CONSOLE_BOOTSTRAP_TOKEN` 时自动生成并提示、已配置则按配置启动」。本轮把这条链做成一等能力，并
**不改**安全口径：令牌仍优先来自环境变量、仍不进命令行/配置文件/日志；自动生成的只是**一次性兜底**。
全部加性：门禁 **848 → 853 全绿**，`GATE_CHECK_FLOOR` **796 → 801**，未删任何既有判据。

- **令牌兜底（`console_serve.rs`）**：`std::env::var(&console.bootstrap_token_env)` 仍是首选来源；
  缺失或为空时 `generate_bootstrap_token()` 临时生成一枚（96 个十六进制字符），并当场打印
  「已临时生成一次性引导令牌（进程退出即失效，重启换新）」。熵来自标准库 `RandomState`（进程内首次
  使用时由 OS 熵源随机化）——**不引入 RNG 依赖**，与 `console.rs` 用引导令牌经 SHA-1 派生会话 id 同一取舍。
  这枚令牌随进程生灭、不落盘，只服务本机回环控制台的首次引导，不替代运维显式配置的长期秘密。
- **`qx-cli console --generate-token`**：只打印一枚令牌与可直接粘贴的 `export` 行后退出（不启动服务）。
  刻意不写成裸输出：启动横幅走 stdout，`$(…)` 会把横幅一起吞进去。
- **`qx-cli console --init <path>`**：写出一份与部署模板**同形**的就绪配置（绑回环、令牌只给环境变量名、
  正文无令牌字面量字段），**拒绝覆盖**已有文件（静默改写等于替别人签字），并打印下一步。
- **门禁牙齿（新建 `console_usability_check`，五颗）**：令牌兜底在盘且环境变量仍是首选；生成令牌有行为
  用例在盘（长度下限 + 十六进制 + 两次不同）；`--generate-token` 声明且被读到；`--init` 声明且被读到、
  且对已存在文件拒绝覆盖；`--init` 模板与部署模板同形（当 JSON 解析后逐格核对）。`GATE_CHECK_FLOOR` **796 → 801**。
- **反向变异已实测（五处，各自单独红）**：改名生成函数 / 改测试名 / 改旗标声明 / 抹掉覆盖拒绝 / 模板绑
  `0.0.0.0`。脚本 `.audit/console_usability_mutation.py` 跑完逐字节还原，**5/5 全部打红**。
- **文档**：`deploy/README.md` 补 bash + PowerShell 双语起步、令牌兜底说明、两条旗标，以及
  「作为独立服务运行」小节（systemd unit + NSSM/Windows 服务），并诚实说清"独立服务 ≠ 对第三方开放"：
  跨主机仍需 TLS/mTLS + 独立反代 + operator 身份 + 显式 CORS（**M4' 未关闭**）。
- **仍未关闭（诚实登记）**：`serve` 自动挂载 console（受 `strategy_contract.rs` 行数棘轮与 supervisor
  生命周期复杂度约束，本轮未做）；桌面 Host；多机反代 / 第三方对外暴露（M4'）；Paper/sandbox/production
  外部验收（M5'）。本轮不创建 tag、不触发 Release。


### M4'/M5' 续（2026-10-09）· 同源 BFF 控制台（`qx-cli console`）+ Paper 轨验收

**口径**：用户确认「两条并行全量推进」。本轮一次做完两件事——**同源 BFF / CSRF / 权限·会话模型**
（三件事是**同一条链**，BFF 是后两者的载体），以及 **Paper 轨**（M5' 里不需要任何交易所凭据就能完整
验收的那条子项，与上一轮的回测轨同源）。**桌面 Host** 与 **sandbox/production 外部验收**仍未做，
本轮不创建 tag、不触发 GitHub Release。全部加性：门禁 **824 → 848 全绿**，
`GATE_CHECK_FLOOR` **772 → 796**，未删任何既有判据。

- **同源 BFF 本体**（`crates/qx-api/src/console.rs`）：`ConsoleFront` 把静态三件与 API 放在**同一个源**上，
  浏览器不再需要 CORS allowlist（同源请求根本不发 `Origin` 预检），会话与 CSRF 留在服务端。
  - **引导**：运维带 `?token=<bootstrap>` 打开一次页面，服务端校验后签发两枚 cookie——会话 cookie
    （`HttpOnly` + `SameSite=Strict`，页面脚本读不到）与一枚**非 HttpOnly** 的 CSRF cookie（页面读出来
    放进 `X-QX-CSRF` 头，这就是双提交模式）。此后地址栏里不再有凭据。
  - **会话表**：上限 64 条，超出按**签发顺序**退最老的一条（不是按字典序挑一个）；到期即失效。
    会话 id 与 CSRF 由引导令牌经 SHA-1 派生的伪随机函数给出——秘密只有一个来源，轮换因此可解释，
    也不必为这一层引入 RNG 依赖。
  - **两道独立的锁**：非 GET 必须带 `X-QX-CSRF` 且与会话记住的那一枚**逐字符相等**（空头与不等都 403），
    并且过 `origin_matches_host`（带 `Origin` 的请求必须与 `Host` 同源）。任一道都不许放宽成"有头就过"。
  - **身份注入**：`handle_inner` 收到的是**会话里那份 operator**；命令体里的 `operator_id` 不被采信
    （它是审计字段，不是认证）。
  - **回环边界**：`console_bind_is_loopback` 是唯一判据，只允许 `127.0.0.1` / `::1`。这一层没有 TLS、
    没有 mTLS、没有运维审批，绑到外部接口等于把控制面交给任何能连上的人。
  - **连接预算复用** API 那一枚（`DEFAULT_MAX_CONCURRENT_CONNECTIONS`），额度随会话线程一起归还。
- **`qx-cli console [runtime.json]`**（`console_serve.rs` + `console_args.rs`）：读配置 → 过回环判据 →
  从 `api.console.bootstrap_token_env` 点名的环境变量取令牌（**不走命令行、不写进 JSON、不回显进日志**：
  命令行会进 shell 历史、配置文件会进版本库）→ 绑监听 → 印**一次**入口 URL。静态目录按
  「配置所在目录 → 可执行文件同级/上一级 → 构建期源码树」三格找，一格都不中就报错点名找过哪里——
  静默回落一个空目录会让控制台在浏览器里 404，而服务端看起来一切正常。
- **配置面**：`api.console` 段（`bind` / `static_dir` / `operator` / `bootstrap_token_env` /
  `session_ttl_seconds`），边界校验独立成 `crates/qx-runtime/src/runtime_config/console_validation.rs`
  的 `console_boundary`，由 `topology_validation.rs` **fail-closed** 调用：回环、三格非空、TTL 非 0、
  以及 operator 必须在 `api.operators` 里。新模板 `deploy/qianxing.runtime.console.example.json`
  （明文 + 回环）进 `deploy_template_coverage` 的 COVERAGE 表。
- **门禁牙齿（新建 `console_front_check`，十七颗）**：模块在盘与挂载、六个公开常量、`ConsoleConfig::new`
  是唯一构造入口且四类误配当场拒绝、会话 cookie `HttpOnly`+`SameSite=Strict`、CSRF cookie 刻意非
  `HttpOnly`、非 GET 的 CSRF 与同源两道锁、身份由服务端注入、回环单源、令牌只从环境变量、
  `api.console` 段 fail-closed、CLI 派发接线、模板形状、行为用例在盘、**发布身份的产品能力声明由在盘
  代码背书**。`GATE_CHECK_FLOOR` **772 → 789**。
- **Paper 轨（M5' 的本地可验收子项，`tools/paper_acceptance.py` + `maturity/paper_acceptance.yaml`）**：
  跑仓库自己的 Paper venue（进程内撮合），主链 `scheduler -> strategy -> paper-execution -> ledger`
  在两个独立目录各跑一遍 + 同腿重跑，要求主链四段事实面相等、重跑不重复下单、无遗留待执行命令、
  审计链校验通过且走到 `Executed`；子进程环境**主动摘掉** `QX_BINANCE_*` / `QX_CCXT_*` / `QX_OKX_*` /
  `QX_CONSOLE_*`，所以「不需要凭据」是构造出来的，不是碰巧没配。实测
  `OrderSubmitted=1 / Filled=1 / LedgerApplied=4`，`independent_dirs: fact_surface_equal`。
  新建 `paper_track_check` **七颗**：记录六格自述齐全、脚本真有四件比对、主链四段且计数全为正
  （0 笔成交的"通过"是空跑）、记录正文不出现任何 venue 名称且实盘两档仍全 false、终态退场与幂等、
  默认跑仓库那份 paper 模板并摘凭据环境变量。`GATE_CHECK_FLOOR` **789 → 796**。
- **门禁合计**：**824 → 848 项全绿**，`GATE_CHECK_FLOOR` **772 → 796**。
- **反向变异已实测（二十四处，各自单独红）**：console 十七处（`console.rs` 改名 / 摘 `mod` 挂载 /
  资源表降成私有 / 抹掉一条拒绝文案 / 摘会话 cookie 的 `HttpOnly` / 给 CSRF cookie 加上 `HttpOnly` /
  CSRF 校验放宽成"有头就过" / 摘同源校验 / 身份注入改成 `None` / 装配处不过回环判据 /
  配置里出现令牌字面量字段 / 拓扑校验不再调边界校验 / 摘 CLI 派发臂 / 模板改名 / 模板绑 `0.0.0.0` /
  抹掉真套接字端到端用例 / 身份把 `product_csrf` 说成 `False`）+ Paper 七处（结论改 `skipped` /
  抹掉主链比对函数 / 成交数改 0 / 正文写进 venue 名 / 遗留一条待执行命令 / 悄悄换掉默认配置 / 能力档把 Paper 轨指错）。
  脚本 `.audit/console_mutation.py` 与 `.audit/paper_mutation.py` 跑完逐字节还原，24/24 全部打红。
- **顺带修正一处身份含糊**：`release-identity.json` 原用 `csrf_supported=false` /
  `session_permissions_supported=false` / `desktop_host_supported=false` 三个字段，容易被读成
  「**产品**没有 BFF」。现拆成 `package_scope=static-assets-only`（归档自己是什么）与 `product_*` 四格
  （产品现在有什么，`product_same_origin_bff`/`product_csrf`/`product_server_side_session` 为 `true`，
  `product_desktop_host` 仍为 `false`）。**归档仍不是 BFF**；变的是身份不再少报产品能力。
- **阈值漂移同步**（改数量类判据的既定纪律）：新模板使「仓库明文 runtime 模板」由 **17 → 18** 份，
  `api_transport_auth_boundary_doc.rs` 的阈值与模块文档、`deploy/README.md` 的份数说明一并改口；
  `docs/自研量化框架重构方案-V13.md` 那段是 #244 立案时的读数，按仓库惯例**保留不改**并加回写批注。
- **收尾验证**：`cargo check -p qx-cli --all-targets` 零错零警；
  `cargo clippy -p qx-api -p qx-runtime -p qx-cli --all-targets` **零告警**；
  `cargo test -p qx-api` 全绿（lib 48 条，含 console 13 条）；`cargo test -p qx-runtime` 默认特性 57 条 /
  `--features sqlite` 59 条全绿；**`cargo test --workspace --no-fail-fast` 120 个测试目标 / 1214 passed /
  0 failed / 1 ignored**（`WORKSPACE_TEST_FLOOR = 1083`）；`python -m unittest discover -s python/tests`
  用仓库 venv 跑 **68 条 OK**（托管 python 缺 `tzdata`，那 6 条 `test_ashare` 报错是环境噪声）；
  触碰过的 16 个 Rust 文件逐个 `rustfmt --check --config skip_children=true` 干净；
  `python tools/check_architecture.py` **848 项全绿**；`git diff --check` 干净。
- **仍未落地（诚实边界）**：**M4' 未关闭的部分**是桌面 Host 与多机反代形态（那属于部署件，不是代码）；
  **M5' 的 Paper/sandbox/production 外部验收**仍未进行；本轮没有创建 tag，也没有触发 GitHub Release。


### M4'/M5' 续（2026-10-08）· Web 控制台 local-only 边界硬化 + 发布身份状态字段

**口径**：继续把「静态 Web 控制台发布包」的边界说清，并把能本地验证的部分钉进门禁；仍不冒充完整 M4'/M5'。本轮不新增同源 BFF、CSRF、权限会话、桌面 Host，也不触发 GitHub Release。

- **浏览器侧 local-only 入口**：`web/console/app.js` 的连接入口只允许 `http://127.0.0.1` / `http://localhost`；提交 `POST /control/commands` 前再次检查。非本机地址会在健康面板显示「已拒绝」并说明原因：没有同源 BFF、CSRF token 或服务端会话时，静态包不能作为远端控制面。`web/console/index.html` 同步写明这只是本机控制台边界，不是认证授权。
- **发布身份机器可读化**：`tools/package_web_console.py` 的 `release-identity.json` 现在明确登记 `distribution_boundary=local-only`、`requires_same_origin_bff=true`、`csrf_supported=false`、`session_permissions_supported=false`、`desktop_host_supported=false`、`sandbox_accepted=false`、`production_accepted=false`，避免把静态包误读成生产控制台。
- **门禁牙齿**：`release_supply_chain_check` 扩展发布包身份字段判据；`web_console_check` 新增两颗 local-only 判据（连接入口只允许回环、提交前再次检查并给出拒绝路径说明）。`GATE_CHECK_FLOOR` **767 → 772**，实际门禁 **822 → 824 项全绿**。
- **验证**：`python tools/check_architecture.py` 全绿（824 项）；`python -m unittest discover -s python/tests -p test_web_console_package.py -v` 3 条通过；`node --check web/console/app.js` 通过；`git diff --check` 通过。
- **反向变异已实测**：移除 `localhost` 回环白名单会红「连接入口只允许本机回环」；移除提交前二次检查会红「命令提交前再次检查本机回环边界」。还原后复绿。
- **仍未落地（诚实边界）**：同源 BFF、CSRF、权限会话、桌面 Host、Paper/sandbox/production 外部验收仍未完成；本轮没有创建 tag，也没有触发 GitHub Release。


### V13 R23 续（2026-10-08）· 阶段二 P0/P1 逐项落地（native 信任门 / dev 环迁出 / EventLog 单事务批量追加 / 游标增量 refresh / 估值单点 / 错误码五元契约）

**口径**：按「先牙齿后功能」把 `docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md` 阶段二里**可当场关闭**的六项逐条落地，每项都带自己的门禁牙齿 + 反向变异实测。全部加性：门禁 **759 → 793 全绿**，`GATE_CHECK_FLOOR` **703 → 737**，未删任何既有判据。方案文档 §17.2 表格中已关闭的行标 ✅，新增 §17.4 逐项明细。

**续（同日）**：随后回答「P0-1 是否可以先做回测、不做实盘」——**可以，而且那才是本仓的验收边界**。P0-1 的卡点从来是真实交易所凭据（非代码），而 `sandbox_tested` / `production_approved` 两档**只对需要外部 venue 的能力有意义**；本仓主用法是回测 + Paper 闭环，一条凭据都不用。于是把「回测轨」做成**不需要任何凭据就能完整验收**的一条轨：新增 `tools/backtest_acceptance.py` → `maturity/backtest_acceptance.yaml`（**仓库资产**，与未跟踪的实盘证据不同）+ `crates/qx-cli/tests/backtest_acceptance_determinism.rs`（两条行为用例）+ 六颗判据 `backtest_track_check`。门禁 **793 → 799 全绿**，`GATE_CHECK_FLOOR` **737 → 743**。详见下面 P0-1 那一行。

- **P0-3 / DD-4 · in-process 原生策略信任门**（`crates/qx-cli/src/native_trust.rs`）：`admit_in_process_c_abi` + `target_triple_matches`；`load_c_abi_strategy` 在 `dlopen` **之前**先过信任门；配置面暴露 `c_abi_trusted_native` / `c_abi_target_triple`，且开关**缺省为 `false`（默认拒绝）**。牙齿 `native_trust_check` 5 颗（三件在盘且两个判定都非测试专用、`dlopen` 前先过门、配置面暴露、缺省 false）。**759 → 764**，floor **703 → 708**。
- **P1-12 / §8 WP-22 · dev 依赖环迁出**：新建 `crates/contract-tests`（纯测试宿主），把两条压在正常边上的 dev 环（`qx-execution --dev--> qx-runtime`、`qx-risk --dev--> qx-zhenlu`）连同它们的 **5 份跨 crate 契约用例（10 条）**搬出生产 crate。牙齿 `dev_dependency_cycle_check` 3 颗：两条已知环不许回来；全仓不得存在「A --dev/build--> B 且 B --normal--> A」这种二点环（`cargo tree` 默认视图看不见 dev 边，新写的 dev 边踩到任一正常边当场红）；搬家不是丢用例——迁走的五份用例仍在 `crates/contract-tests/tests/` 且该 crate 在 workspace members 里。`EXECUTION_TEST_FLOOR` **25 → 15**（本地板只覆盖 `src/tests` 的 15 条，全仓地板把新 crate 一并计入、总数不变）。**764 → 767**，floor **708 → 711**。
- **P1-1 / DD-2 / §7 M2 · EventLog 写侧单事务批量追加**：`EventLog::append_batch(&mut self, batch: &[Event])`（`append_checked` 委托它，逐条校验规则单源）+ `digest_of_prefix()`（全量摘要与前缀摘要同源，增量核对才成立）+ `SqliteEventLogStore::append_batch`（单事务里走增量路径，**不再 `load_event_log` 读回整条历史**——此前写放大随 N 线性增长，G4）+ 生产写面的 SQLite 分支真的调它。牙齿 `event_log_append_batch_check` 6 颗；原子性（Kernel）与增量性（SQLite）各有一条行为用例在盘。**767 → 773**，floor **711 → 717**。
- **P1-2 / §7 M2 · 游标增量 refresh（G5 收敛）**：`SqliteEventLogStore::read_since`（`WHERE name = ?1 AND seq > ?2 ORDER BY position ASC`，只读 manifest 校验前缀，O(1)）+ `RuntimeEventStore::read_since` 委派 SQLite、其余后端**显式**回落 `Ok(None)` + `LiveEventPipeline::refresh_latest` 三支（空尾部 no-op / 前缀一致只接尾部 / 前缀不符或后端无行级读则整份重建）+ `apply_tail` 走 P1-1 的 `append_batch` 且与整份重建共用 `apply_index_event`（归约单源）。牙齿 `pipeline_cursor_refresh_check` 6 颗。**773 → 779**，floor **717 → 723**。
  - **方法论要点**：增量与整份重建的**终态完全一致**，纯行为断言（订单状态、账簿条数）抓不到退化；牙齿因此同时钉住两个**内部计数器** `tail_appends` / `rebuilds`（不进运维快照 `PipelineMetricsSnapshot`，只由本 crate 用例读——仓库既有的「没有生产读者但被回归用例钉住」那一类）。**反向变异实测**：把 `Some(tail) => apply_tail` 改成 `rebuild_from_store`，门禁红「refresh_latest 分支不全」+ 用例红（`tail_appends` 0≠1）；还原复绿。
- **P1-6 / WP-19 · 估值单点**：`crates/qx-core/src/valuation.rs` 的 `ValuationContext` / `ValuationResult`；`Ledger::valuate` **单点派发**三把参数化尺子（现货按乘数 / 衍生按合约规格 / 跨币种再叠 FX），`MarginState::valuate` 共用结果形状（`available` 关系单源）；`qx-xingban` 回测两处手工派发（`equity_for`、订单簿回测）与 `qx-cli` paper 保证金派发全部改走它。牙齿 `valuation_single_source_check` 6 颗，含一条**生产源码全文扫描**：三把参数化尺子只能出现在定义点（`ledger/query.rs`）与估值单点——取 `production_text` 剥掉注释与测试项，否则"文档里提一句 / 用例里直接调原语"会让"还有谁在直接派发"数不准。**779 → 785**，floor **723 → 729**。**反向变异实测**：还原回测里的手工派发 → 红 2 条；还原 `MarginState::equity` 的内联算式 → 红 1 条。
- **P1-11 / DD-5 · 错误码五元契约**（`crates/qx-core/src/error.rs`）：`ErrorCode` **闭集**（8 码，`ALL` 可遍历，`as_str`/`parse`/`Display`/Serde）+ `Retryability` 四档（`Never`/`Allowed`/`AfterReconcile`/`AfterBackoff`，刻意**不是布尔**）+ `ErrorContract` 五格（`code`/`retryability`/`reconcile_required`/`safe_to_retry`/`user_message`）+ `QxError::contract()` **全仓唯一**映射表（`code()` 从它派生，自身不再 `match self`）。消费侧三处接上：`usage_errors::qx_context` 按 `reconcile_required` / `allows_retry` / `may_retry_eventually` 三档提示；`runtime_wiring/pipeline_storage.rs` 四处 `QxError`→字符串边界不再摊成 `{error:?}` 自由文本；`qx-runtime/src/pipeline.rs` 两处热路径重试循环改按 `retryability` 分支。牙齿 `error_code_contract_check` 8 颗。**785 → 793**，floor **729 → 737**。
  - **反向变异实测（三处，各自单独红）**：① `code()` 不再从 `contract()` 派生 → 红「唯一映射表 / 两份口径」；② `pipeline_storage.rs` 退回 `format!("…: {error:?}")` → 红「只有 3 处走 qx_context / 仍有 {error:?}」；③ 重试循环退回 `matches!(error, QxError::Transient(_))` → 红「重试循环没有消费契约」。还原后复绿 **793**。
  - **刻意不纳入**：HTTP/工作流层面的状态串（`qx-api` 的 status、`qx-control` 的 `CommandStatus`）。它们是各自的读面，不是"错误分类"；塞进 `ErrorCode` 会把闭集撑成开放集，"一型一码"那条判据当场数不准。
- **本轮另修两处真实缺陷**：
  - `crates/qx-runtime/src/pipeline.rs` 的 sqlite 游标用例缺 `#[cfg(feature = "sqlite")]` 门 —— 默认特性下 **`cargo test -p qx-runtime` 编译不过**（`open_sqlite` 不存在）。
  - `crates/qx-cli/tests/unit/worker_pipe_failure_diagnostics.rs::live_but_non_draining_worker_write_is_bounded_not_hanging` 断言的是**已不存在的文案**（`输入超时` / `worker 存活但不接收输入`）——R17 把三处子进程 stdin 写入统一到 `qx-adapter::io_budget::write_all_within` 之后，写侧超时只报「未在 N ms 预算内写完」，这条**回归面从那时起就是永久红的**（全仓 grep 该文案 0 命中）。改按新口径断言（走预算通道 + 交代 `程序=`/进程状态），保留原有的「耗时 < 5× timeout」有界性断言。
  - 顺带修掉 P0-3 引入的 `clippy::field_reassign_with_default`（`native_trust.rs` 的测试改用结构体字面量 + `..Default::default()`，保住那条 fail-closed 梯子的可读性）。
- **收尾验证**：`cargo fmt --all --check` rc 0；`cargo clippy -p qx-core -p qx-runtime -p qx-cli --all-targets` **零告警**；`cargo test -p qx-core` 全绿（60 + 2 + 4 + 25 + 6）；`cargo test -p qx-runtime`（默认特性 56 条、`--features sqlite` 58 条）全绿；`cargo test -p qx-cli --no-fail-fast` **441 passed / 0 failed**（bin 单元 363 + 16 个集成目标 78，含本轮新增的 `error_code_contract` 三条）；**`cargo test --workspace --no-fail-fast` 118 个测试目标 / 1193 passed / 0 failed / 1 ignored**（`WORKSPACE_TEST_FLOOR = 1083`）。行数棘轮：`crates/qx-runtime/src/pipeline.rs` 停在预算线上 **2480/2480**（靠把新增的说明性注释并句腾出行数，不升预算）。
- **P0-1 / §6.5 · 回测轨做成「无凭据可完整验收」的一条轨**：实盘轨的 `sandbox_tested` / `production_approved` 只能由 `maturity/evidence/` 下 `outcome=passed` 的记录翻真，而那份 testnet 记录是 `outcome:"skipped"`（缺 `QX_BINANCE_TESTNET_API_KEY` / `_SECRET`）——卡点是**凭据不是代码**。关键是那两档**只对需要外部 venue 的能力有意义**：本仓主用法是回测 + Paper 闭环，因此新增 **`maturity/capabilities.yaml` 的 `backtest_only` 档**（`credentials_required: false` / `external_venues: none` / `approval_scope` 明写那两档**不适用**）并设为 `default_profile`；那条轨的验收边界是 **`maturity/backtest_acceptance.yaml`**（**仓库资产**，与未跟踪的实盘证据不同），由 **`tools/backtest_acceptance.py`** 生成——两个独立目录各跑 `quickstart` + **同目录两轮 `backtest`**，要求：① 同目录重跑**逐格相等**（含产物文件名后缀与 `config_fingerprint`）；② 跨目录只要求 `result_hash` / `data_fingerprint` / `replay_verdict` 与**归一化后**的产物内容相等，`equity.csv` / `fills.csv` **逐字节相等**；③ `replay_verdict: verified`；④ `network_accessed` / `orders_sent` 双 `false`；⑤ 子进程环境**摘掉** `QX_BINANCE_*` / `QX_CCXT_*` / `QX_OKX_*`。归一化只抹三样：**绝对路径**（三种形态）、内容寻址的 `config_hash`、**由它派生的产物文件名后缀**——这正是 §46「artifact digest vs result_hash」那条区分：`result` 必须与路径无关，`artifact identity` 不必。实测 `result_hash=26fdd6b52d020700` 跨目录稳定。牙齿 `backtest_track_check` **6 颗**：① 默认档是 `backtest_only` 且声明「不需要凭据 / 无外部 venue / 那两档不适用」；② 记录在盘且六格自述齐全；③ 记录由在盘脚本生成、且脚本真有 `compare_reruns` / `compare_independent` / `one_leg` 三件事；④ `result_hash` 是 16 位十六进制且四类产物摘要齐全；⑤ 记录正文**不出现任何 venue 名称**（先剥 `#` 注释再判，免得「实盘证据在哪」那句散文被当成越界声明），且实盘两档仍全 `false`；⑥ 行为用例在盘。**793 → 799**，floor **737 → 743**。刻意**不做**「记录是否过时」判据——`generated_at_unix` 每跑一次都变，拿它当判据只会逼人写死时间戳。
  - 顺带修一处**文档失实**：`maturity/evidence/README.md` 声称「`maturity/evidence/**` 不进版本控制」，而 `git ls-files maturity/` 明明列得到 `README.md` 与 `testnet/20260920T005856Z-dryrun/` 下两份记录。README 改口到实测现状，并新增「两条轨别混读」的开篇说明。
- **仍未落地（诚实登记）**：**M4' 完整生产化**（桌面 Host/本地代理；Web 同源 BFF/CSRF/权限会话等）与 **M5' 外部验收**（Paper/sandbox/production）；本轮只补了静态 Web 控制台可校验发布包，不冒充完整 M4'/M5'。**P0-1 的实盘轨**仍待真实交易所凭据（非代码），其**回测轨已完整验收**。M1（`qx-core::contract` 稳定契约单点）/ M3'（控制面控制台）/ P2-2（开源 HTTP 库技术可行、当前暂不迁库，见 §17.6）本轮均已关闭。


- **§6.4 P2-2 · 自研 HTTP/WS 面评估：保持自研，缺口钉进门禁**：用户问「不要自研 HTTP、直接上开源成熟库是否可行」。结论：**技术上可行，但当前不建议迁库；自研面缺口尚未证明不可接受**。先落地 `maturity/http_surface.yaml`（自研 HTTP/WS 面逐格覆盖表，21 格：13 covered / 4 partial / 3 gap / 1 by_design），每格写清「由哪个符号负责、哪条用例守着、覆盖到什么程度」——缺口是数出来的，不是估出来的。配套 `tools/check_architecture.py` 新增 `http_surface_check`（七颗）：`self_built_file` 全部在盘、每行 `anchor` 符号真在登记文件里声明、每行 `case` 用例真在测试文件里、`verdict.decision=keep_self_built`、`accepted_gaps` 恰好等于「非 covered 行」、`migration_trigger` 在场。门禁 **813 → 819 全绿**，`GATE_CHECK_FLOOR` **757 → 764**。
  - **判定口径**：三格 gap（chunked 请求体 / 重复 Content-Length 该拒 / ping/pong 保活）全部落在「本仓不用的 HTTP 特性」上，且 fail closed；四格 partial 靠间接用例覆盖，已在表内逐格登记欠账。按 §9 排期原则 3「零新依赖」，先登记、后评估；迁库触发条件（`migration_trigger`）三条：需要本仓读面真正要用的 HTTP 特性 / 需要把 API 暴露给不受控客户端 / WS 需要双向载荷。
  - 顺带修一处**文档失实**：覆盖表 `reason` 原写「20 格 / 2 格 gap」，实际 21 格 / 3 格 gap（漏数 `ws_ping_pong`），已改口。
  - `maturity/capabilities.yaml` 新增能力 `qx_api_self_built_http_ws`（impl/code_tested 双 true，limitations 登记 keep-self-built 决策与 accepted_gaps），把「不自研改迁」的判定也接进门禁能核的登记面。
- **M4'/M5' 续 · Web 控制台版本化发布包**：新增 `tools/package_web_console.py`，只打包 `index.html`/`app.js`/`styles.css`，校验 HTML 只引用包内资源、拒绝外链/漏打资源；归档内含版本、完整 commit、Schema Registry 版本与逐文件 SHA256 的 `release-identity.json`，固定 gzip/tar 元数据，重跑逐字节一致。`python/tests/test_web_console_package.py` 三条用例覆盖可复现/身份与摘要、外链或漏打资源 fail closed、版本与提交身份校验。`.github/workflows/release.yml` 新增 `web-console` job：生成归档、attest provenance、上传 artifact；Release 依赖此 job，统一的 `SHA256SUMS` 覆盖该归档。`release_supply_chain_check` 新增三颗判据；`GATE_CHECK_FLOOR` **764 → 767**，实际门禁 **819 → 822 项全绿**（本轮复跑通过）。`deploy/README.md` 增本地重建办法并明确安全边界：静态包不是同源 BFF，没有会话/CSRF/权限代理，不可单独暴露公网或充作生产交易控制台。**只关闭了静态交付/打包子项，M4' 同源 BFF/CSRF 与 M5' 外部 Paper/sandbox/production 验收仍未完成**；未触发 GitHub Release。

### V13 R23 收口（2026-10-08）· 方案落地度核验 + 阶段四 M1'/M2' 只读 Web 控制台 + 文档口径回写

**口径**：用户问「是否已按新版方案（`docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md`）全部落地」。逐条把方案的每个工作包对到实际代码/门禁，结论是**未全部落地**，并按「先牙齿后功能」推进了可当场关闭的最高价值一项（阶段四只读控制台）。全部加性：门禁 **754 → 759 全绿**，未删任何既有判据。

- **方案落地度核验（结论：未全部落地）**：
  - **已落地**：**阶段一 15 族**（fam01–fam15 + fam16 手续费币种）全部有判据（`file_lock_single_source_check` / `io_budget_and_venue_cache_check` / `scheduler_owner_routing_check` / `environment_production_single_source_check` / `ops_read_surface_check` / `ci_feature_matrix_check` / `kernel_timeline_check` / `fee_settlement_currency_check` / `cli_surface_coverage_check` / `bounded_growth_and_reap_check` / `control_plane_honesty_check` / `storage_retry_check` / `strategy_input_schema_check` / `paper_check_delta_honesty_check` / `dead_public_entry_check`）；**M0 基线冻结**（`baseline_freeze_check` + `maturity/baseline.yaml`）；**G6 性能基线**（`performance_baseline_check` + `benchmarks/run_baseline.py` + `results/baseline.json`）；**P2-4 发布供应链**（`release_supply_chain_check` + `.github/workflows/release.yml`，SBOM/provenance/SHA256）；**M0 证据格式**（`maturity/evidence/README.md` + `evidence/testnet/…-dryrun`）。
  - **未落地（诚实登记）**：**P0-1 外部证据**（`sandbox_tested:true` = 0、`production_approved:true` = 0，唯一 testnet 记录是 dry-run——卡在真实交易所凭据，不是代码）；**P0-2** 文件后端单事务（无 `append_batch`）；**P0-3** native 信任根（有 Ed25519 签名校验，无 `trusted_native` profile）；**P1-1/P1-2**（无 `append_batch` / Pipeline 仍 clone 整体状态）；**P1-6** 无 `ValuationContext`；**P1-11** 错误码仍是 `Option<String>`；**P1-12** dev 依赖环仍在（`qx-adapter`/`qx-execution`/`qx-risk`，Cargo 合法但按方案要迁 contract-tests）；**M1** 无 `qx-contract` crate；**阶段四产品轨** 此前无任何前端（本轮补 M1'/M2'）；**P2-2** 仍自研 HTTP。
- **阶段四 M1'/M2' 落地：只读 Web 控制台**（`web/console/{index.html,app.js,styles.css}`）：消费 `qx-api` 的 `GET` 读面（`/health`、`/ready`、`/account/snapshot`、`/account/balances`、`/account/orders`、`/account/positions`、`/account/ledger`、`/reconcile/reports`、`/scheduler/runs`、`/control/audit`、`/events?after=`）与 WebSocket 增量；**不发送订单、不调用 `POST /control/commands`**。未算出的钱如实显示为 `—`（`null`），不折成 0。页面顶部印出 CORS 要求（`api.cors_allowed_origins`）与 `qx-cli serve` 启动入口。对应路线图 §18「不应把 HTTP/WS API 自动称为前后端贯通」——贯通要有一条**可核对的接线表**。
- **门禁牙齿（5 颗，新建 `web_console_check`）**：控制台三件在盘；`app.js` 点名的每个 API 路径都在 `qx-api` 路由表里（前端引用后端不存在的端点当场红）；页面写明 `cors_allowed_origins` 要求；写明 `qx-cli serve` 入口；控制台代码不得出现 `POST` 或 `/control/commands`（先剥注释再判，与门禁别处同口径）。`GATE_CHECK_FLOOR` **698 → 703**。
- **反向变异已实测**：往 `app.js` 的接线表塞一个 `/nonexistent/endpoint`，门禁当场红「控制台引用了后端不存在的路径」；还原后复绿 **759**。
- **端到端实测**（`quickstart` 出配置 → `qx-cli serve` 起服务 → curl）：控制台点名的 12 条端点全部按语义应答（`/health` 200、`/account/balances|orders|positions|ledger` 200、`/reconcile/reports|scheduler/runs|control/audit` 200、`/account/snapshot` 404 无快照、`/ready` 503 未就绪、`/events?after=0` 409 需先取快照、`/metrics` 200——404/409/503 是语义分支不是"路由不存在"）；配了 `cors_allowed_origins:["http://127.0.0.1:5173"]` 后，预检 `OPTIONS` 回 **204**、`GET` 带 `Vary: Origin` + `Access-Control-Allow-Origin: http://127.0.0.1:5173`。
- **文档口径回写（消除文档-代码漂移）**：方案文档 §3.13/§4.1/§4.2/附录 B/附录 C 的读数从成文时的「662 门禁 / 103 个 `*_check()` / 572 处断言 / `GATE_CHECK_FLOOR=633` / 20 条能力 17/17/0/0 / 44 份超线 / CI 7 job」回写到实测现状「**754（本轮 759）门禁 / 127 个 `*_check()` / 678 处 `check(...)` / floor 698（本轮 703）/ 24 条能力 20/20/0/0 / 45 份超线 / CI 12 job**」；§12.2「阶段一 15 族中的 14 族未落地」加 2026-10-08 回写批注（已被 R8–R22 推翻）；§12–§16 的历史读数保留不回改。`deploy/README.md` 增「只读 Web 控制台」一节，`README.md` 模块表增 `web/console/` 一行。
- **收尾**：门禁 **759 项全绿**；本轮改动为纯加性（新增 `web/` 目录、`web_console_check`、文档回写），未触碰任何 Rust 源与既有判据。


### V13 R22 收口（2026-10-08）· 第 22 轮三扫 · 门禁 `TEST_PATH` 的单文件测试盲区（#222）+ 发布条件复核

**口径**：本轮按 `docs/archive/qianxing_综合优化改进系统方案_竞品架构实现路线图_2026-10-08.md` 的 §43（孤儿）/ §44（死配置键）/ §45（配置不生效）/ §46（确定性）/ §47（重放）/ §67（验收）逐条过一遍。三扫结论：**A 连通性与前后端贯通 0 颗、C 终止性与无界 0 颗**；B 孤儿面命中一条**门禁自身的盲区**（`#222`，V13 R2 第十六遍立案、本遍落地）。全部加性：门禁 **753 → 754 全绿**，未删任何既有判据。

- **`#222` 落地：`TEST_PATH` 认不出单文件形态的 `src/tests.rs`**。`tools/check_architecture.py` 的 `TEST_PATH` 只认目录式 `src/tests/`（含 `tests/mod.rs`）与 `_tests.rs`，不认单文件式 `src/tests.rs`——而 V12 R2 为绕开行数棘轮，把八个 crate 的库内用例搬成了后者（`#[cfg(test)] mod tests;` 挂进来）。于是「唯一读者写在 `src/tests.rs` 里」的 `pub` 项被当成有生产读者，**零读者判据在它们面前是瞎的**；同一个过滤器还罩着 `PUBLIC_ENTRY_FAMILIES`（能力族入口清点）与枚举变体生产者两条判据，所以这是一条会同时松开三处前提的盲区。
  - **修法（两种形态定成一个口径）**：`TEST_PATH` 补 `(?:^|/)tests\.rs$`，目录式 `src/tests/` 与单文件式 `src/tests.rs` 都认得。
  - **逐模块清点（放宽后放出的 8 条零读者）**：`qx-datastruct::{close_at, from_view, resample_with_manifest, select_time_with_manifest}`、`qx-protocol::{from_wire_json, to_wire_json}`、`qx-xingban::{corporate_action_supported_by_ledger, corporate_actions_from_data}`——共同形状是**生产链路走 JSON/字符串入口**（`BarFrame::from_json`、`apply_corporate_actions_json`），这些类型化的程序化入口只有库内用例读者；按「保留 + 登记 + 双向钉住」逐条进 `PUBLIC_SURFACE_ALLOWLIST` 并写明理由（含 `qx-protocol` 两条的跨语言 wire 契约、`qx-datastruct` 两条的 Python 桥镜像实现、`qx-xingban` 两条对应的 `ashare_corporate_action_ledger` 台账）。
  - **与立案时的 10 条对账**：`#222` 立案记的是 10 条，本遍实测 8 条。差额两条已各自销案——`qx-protocol::to_qifi` 在 `crates/qx-cli/src/ecosystem_smoke.rs:208` 长出生产读者；`qx-runtime::to_json_for` 已改成 `#[cfg(test)] pub(crate) fn` 并搬到 `strategy_contract/contract.rs`（被 `_strip_cfg_test_items` 整项剥离），故不再在列。
  - **门禁牙齿（1 颗，住在 `zero_reference_public_surface_check`）**：`TEST_PATH` 必须同时认得目录式与单文件式，且不得把 `contest.rs`/`attestation.rs` 这类含 test 词干的生产文件误判。`GATE_CHECK_FLOOR` **697 → 698**。
  - **反向变异已实测**：把 `TEST_PATH` 改回旧写法（去掉 `(?:^|/)tests\.rs$`），门禁当场红 **2 条**——新牙齿（「TEST_PATH 认得测试模块的两种形态」）与「允许清单里的条目仍然没有生产读者」（8 条条目变过期）；还原后复绿 **754**。
- **发布条件复核（§67 逐条 + §46 实测）**：
  - **门禁**：753 → **754 项全绿**；**`cargo fmt --all --check`** rc 0；**`cargo clippy --workspace --all-targets -- -D warnings`** rc 0。
  - **`cargo test --workspace`**：114 个测试目标 / **1169 passed / 3 failed / 1 ignored**，3 条全是**环境相关**——2 条 `rust_invokes_python_*`（`QX_PYTHON` 未设置、回落 PATH `python` 被沙箱拒绝，`os error 5`）与 1 条已知 `os error 231` 管道噪声；把 `QX_PYTHON` 指到 `python/.venv/Scripts/python.exe` 后前两条当场转绿。
  - **§46 确定性实测**：同输入两次 `quickstart`，`result_hash=26fdd6b52d020700`、`input_data_hash`、`replay.log_digest`、全部 metrics、`fills.csv`、`equity.csv` **逐字节相等**；只有路径派生的 run_id 与 manifest 路径不同（正是 §46「artifact digest vs result_hash」那条区分）。`config fingerprint` 实测**内容寻址**：不同目录下逐字节相同的配置得同一指纹（`417a32e4…`）。
  - **§45 配置不生效**：`builtin_fast_window/slow_window/period/threshold_bps` 对 MACD 恒不生效（实测改这四项 `result_hash` 不变）——这是**已登记**的 limitation（`declared_unused_knobs_do_not_fail_the_run`，V12 #102 §14.5），且系统在 stdout 与摘要 `signal.declared_unused` 两处如实报告。
  - **§47 重放**：`ReplayVerifier::verify` 在写任何工件之前跑（`replay_kernel_check` 看守），不是独立 `replay` 命令但满足意图。
  - **§43 UNFED / §44 死配置键 / 终止性**：`RuntimeConfig` 全字段有生产读者（§44）；生产代码无 `while true`，`loop {` 均有出口或预算（`*_retry_*` / `stop_aware_worker_tick_sleep_check` / `api_accept_loop_exit_check` / `bounded_growth_and_reap_check` 覆盖，§43 无界方向 0 颗）。
- **环境发现（写给下一位跑测试的人）**：本沙箱里 `%TEMP%` 对**被测子进程**不可写，`cargo test -p qx-cli` 会有一片 `os error 5`（拒绝访问）**假失败**（实测 70 条，全部停在 `crates/qx-cli/tests/unit/mod.rs:54` 的 `create_dir_all`）；把 `TEMP`/`TMP` 指到工作区内的目录（`export TEMP=$PWD/.tmp-audit/tmp TMP=$PWD/.tmp-audit/tmp`）后降到 3 条已知噪声。**收尾跑测试必须先设 `TEMP`**，否则会把环境问题误读成回归。


### 报告导出安全与落点增强（2026-10-06）· 易用性 P3 收口

- **HTML 文本转义**：报告会显示摘要中的策略名、标的名、输入路径等用户数据；这些文本现统一转义 `& < > \" '`，避免被当作 HTML 标签或资源属性解析。新增恶意标签与带 URL 文本夹具，断言产物只呈现文本，不产生活标签。
- **完整图表导出**：除自包含 HTML 外，默认并列写 `<stem>.equity.svg` / `<stem>.fills.svg` / `<stem>.monthly.svg`；自选 HTML 落点 `--output <path>` / `-o <path>` 会为 HTML 与三张 SVG 共用输出前缀，并自动创建父目录。输出仅接受 `.html`/`.htm`；不带 `--html` 的 `--output` 会 fail-closed。`run report` 统一入口同样支持这些参数。
- **模块边界**：把摘要校验后的报告命令处理抽到 `report_command.rs`，让 `config_commands.rs` 回到预算内；报告落盘仍只有一个生产调用点，门禁同步跟踪该唯一出口。
- **机读输出纪律**：`--json --html` 不再把 `[Report]` 人类提示混进 stdout；JSON 的 `generated_artifacts` 列出 HTML 与 SVG 的实际路径，stdout 可直接解析。
- **本轮验证**：架构门禁 682 项全绿；`cargo fmt -p qx-cli --check` 与 `cargo clippy -p qx-cli --all-targets --offline -- -D warnings` 均通过；报告用例 10 条通过（含 JSON stdout 解析、直接 CLI 参数解析、自定义输出与 HTML 转义）；统一 `run report` 导出路径用例通过。全量 qx-cli 为 323 passed / 4 failed，4 条均是已知 Windows 沙箱 `os error 231` 子进程管道限制；无其他失败。


### 结果可读性层落地（2026-10-06）· 竞品对比 §6 P3「看不见」补上

**口径**：`docs/竞品对比与易用性改进优化计划-V1.md` §1 把最大的产品差距概括为三个词，第一个就是**「看不见」**——竞品默认交付一张标注买卖点的图或一份交互式 HTML，而我们只落 `*.equity.csv` / `*.fills.csv` / `summary.json`，把「读结果」整段留给了用户（`plot_/chart/.html` 在 `crates` 下 0 命中）。这一轮把那一格补上：`qx-cli report --html` 产出一份自包含 HTML 报告。全部加性：门禁 **676 → 682 全绿**，未删任何既有判据。

- **无依赖 SVG 生成器 `crates/qx-cli/src/report_svg.rs`**：三张图——净值曲线 + 最大回撤区间、成交点（▲ 买红 / ▼ 卖绿，中国市场口径）、月度收益热力表。为保住「单 exe + 运行期零依赖」这条相对 Python 栈的真实优势（竞品对比 §4.3 第 4 条），**不引任何绘图 crate**：只做「定点数 → 坐标 → 拼字符串」。内联 SVG **不带 `xmlns`**——HTML5 内联不需要它，带它就等于往产物里塞一个外链前缀字样。空序列画「无数据」占位而不是空白；0 成交**不画任何三角**（假买卖点比没有图更糟）。
- **自包含 HTML 报告 `crates/qx-cli/src/report_html.rs`**：指标卡 + 三张内嵌 SVG + 产物身份表 + 「未连接真实交易所」声明。两条纪律：① **不新增第二个指标算式来源**——卡片每个数都从 `summary.json` 已有格子读，曲线/成交点只从两份 csv 派生，月度收益由净值序列按自然月归并；② **确定性**——不用墙钟、不用随机、不遍历 HashMap，同输入两次生成**逐字节相等**（用例钉住）。产物无任何外链（无脚本/图片/外链样式），可离线双击打开。
- **命令面 `report --html`**：`cli_args.rs` 的 `report` 新增 `--html` 旗标（与 `--json` 正交，两者可同时给）；`config_commands.rs` 的 `run_report` 在**复核摘要之后**调用渲染，落 `<stem>.report.html`；统一入口 `run report --html` 与直接入口**同形状**（先摘出旗标再走通用参数读法）。`cli_help.rs` 同步印出形状。
  - **行数棘轮**：`cli.rs`(673)、`cli_args.rs`(513)、`config_commands.rs`(539) 都在预算顶。三个文件都做了**净零/净负**的行数腾挪（压缩既有注释、一处 `if empty {1} else {len}` → `len().max(1)`），没有动 `line_budgets.yaml` 的任何一个数。
- **门禁牙齿 `report_readability_check()`（5 颗）**：① 两个模块在盘；② 在单文件行数门槛内；③ 在 crate 根 `mod` + `pub(crate) use` 成对挂载；④ 模板（去注释行后）**无外部资源 token**（命名空间声明 / 外链前缀 / `<script` / `<img` / `<link ` / `<iframe` 出现即红）；⑤ 保留「未连接真实交易所」声明 + 落盘调用点唯一。**反向变异已实测**：注释掉 `mod report_svg;` 判据当场变红并点名 `缺配对 ['report_svg']`，随后还原复绿 682。
- **用例 `crates/qx-cli/tests/unit/report_html.rs`（7 条）**：同输入两次逐字节相等 / 无外链 token / 0 成交不画买卖点且每笔成交一个标记 / 卡片数值等于 summary 字段 / 坏 CSV 当场拒绝（走 `write_report_html` 真实落盘路径）/ epoch 毫秒到自然月映射。
- **顺带修一处显示口径**：回撤是「幅度」不是「方向」，卡片不带正号、为 0 时不染绿（否则「没回撤」看着像跌）。
- **本轮实测**：门禁 **682 项全绿 / 0 红**；`cargo fmt -p qx-cli --check` rc 0；`cargo clippy -p qx-cli --all-targets -- -D warnings` rc 0；`crates/qx-cli/tests/unit/report_html.rs` **7 passed**；真机跑通 `qx-cli report <summary> --html`（v1 与 v4 摘要各跑一次，生成 ~8 KB HTML，`http` 出现 0 次）。`deploy/data/qianxing-ashare/runs/` 下的示例产物集同步补一份 `.report.html`，与同一次运行的 `.summary.json`/`.equity.csv`/`.fills.csv` 并列。
- **未落地（诚实边界，已进 capabilities limitations）**：Sharpe/Sortino/Calmar/胜率/盈亏比尚未进摘要，故报告里也没有（不新增第二个算式来源，补它们要先在内核定唯一算式）；`--html` 是显式旗标，`quickstart` 首跑链默认不产报告（并入会改动既有产物份数不变量）；Grafana dashboard JSON 本轮未做（deploy 顶层 JSON 有登记表判据，新增模板要同步登记）。


### 契约层与登记层全量落地（2026-10-06）· 规划 §13.2 / §14.3 / §17 / §18 M0 / §6.2

**口径**：上一轮（`f424b6e`）落地了 §6.2 的七类地基对象；这一轮把它们**钉住**——用真实契约、真实场景、真实依赖规则和真实目标台账。范围是规划里**不需要外部凭据**的全部可本地落地项（M0 剩余 + 各阶段依赖的登记层）。全部加性：门禁 **667 → 676 全绿**，未删任何既有判据。

- **策略契约补齐输入方向（§18 M0 / §4.2 第 8 条）**：新增 `schemas/strategy-api-input-v1.json`，与既有 output schema 合成双向机器可读定义，消除「Rust 有结构、输入方向靠文档」这个点名缺口。`crates/qx-runtime/tests/strategy_contract_input_schema.rs` 五条用例把它与 `StrategyContractInput`/`StrategyContractBars` 逐键对齐：键全集、required（**删键看 serde 认不认**）、`additionalProperties`、定点值只走 JSON 整数、版本常量同源。
  - **用例当场抓出一处真实漂移**：我最初把 `available_margin_raw` 写进 `required`，但它是 `Option<i128>`——serde 对 `Option` 字段的缺省语义就是 `None`，删掉这个键 serde 照样认。schema 已按实测口径改回，描述里写明「这一条不是照抄字段表，而是被删键实验钉住的」。
- **七场景最小 fixture（§18 M0）**：`maturity/fixtures/scenarios/` 下 A 股 / 国内期货 / 国内期权 / 国际股票 / FX / 加密现货 / 加密永续 各一份，每份四块身份（`instrument_spec` / `dataset` / `experiment` / `run`）。`crates/qx-spec/tests/scenario_fixtures.rs` 七条用例把四块分别读进 `TradingInstrumentSpec` / `DatasetManifestV2` / `ExperimentSpec` / `RunRecord` 并逐块校验，另钉住五种产品类型都有样板。
- **补 `TradingProduct::Option`（§15.1）**：规划的产品类型表把 `option` 单列一格，而枚举里没有。已补上（刻意不计入 `is_derivative()`：衍生品那条分支要求 `linear`/`inverse` 恰好指定一个，而期权两者都为 false 才对），市场规格解析器同步接受 `option`/`options`。合约身份真正缺的几格（行权价/到期日/认购认沽/行权方式/组合保证金）已登记为 limitation。
- **`maturity/targets.yaml`（§17）**：工业级非功能目标台账，九个场景 profile × 十个维度，以 profile / 场景 / 硬件 / 版本 / 证据路径为键。牙齿是**诚实规则**：`measurement_status: measured` 必须有真实存在的证据路径，`unmeasured` 必须恰为 `unavailable`——「目标值不等于当前实现现状」从一句话变成会红的判据。
- **`maturity/levels.yaml`（§13.2）**：能力等级登记册，markets / venues / order_types / strategy_languages 四类主体各自登记 L0–L4。牙齿：`capabilities.yaml` 里 `sandbox_tested: true` 的条数为 0 时，**任何主体都不许声明 L3+**——「先把等级调高」这种改字行为当场变红。当前最高只到 L2。
- **`maturity/schema-registry.json`（§6.2 / M6）**：模式登记册实例，覆盖 `schemas/` 下全部 10 份契约，每份写清 id / 版本 / 生产方 / 消费方 / 兼容级别 / golden fixture；判据核对「无重复登记、无漏登记、指针全部落地」。
- **`layer_dependency_check`（§14.3）**：把文档里的**十条领域依赖规则**变成会红的判据——逐 crate 解析 `Cargo.toml` 内部依赖，六条禁止边 + 三条反向规则（`qx-adapter` 只能被 `qx-execution`/`qx-cli` 依赖 = 唯一转换层；`qx-api` 只能被 `qx-cli` 依赖 = 投影面；`qx-cli` 不得被任何 crate 反向依赖 = 叶子）。规则以**当前真实依赖图为基线**，不是「希望如此」。
  - **反向变异已实测**：临时给 `qx-data` 加一条 `qx-api` 依赖，判据当场变红并**同时点名 §14.3-2 与 §14.3-7**，随后已还原（`git diff` 为空）。
- **本轮实测**：`python tools/check_architecture.py` **676 项全绿 / 0 红**；`cargo fmt --check` rc 0；`clippy -p qx-core -p qx-cli -p qx-runtime -p qx-spec -p qx-data --all-targets -- -D warnings` **rc 0**；`qx-spec` **10+7 passed**、`qx-runtime` 输入 schema **5 passed**、`qx-core` **70 passed**、`qx-data` **30 passed**、`qx-cli --bin qx-cli` **311 passed / 4 failed**（4 条全是既有沙箱 `os error 231` 管道噪声，非代码面）。
- **未落地（诚实边界）**：规划的 P0（真实 testnet evidence）、P4（adapter conformance / SDK 兼容矩阵）、P5（application service 抽取 / EventLog 增量 replay / 基准套件）与 M2–M6 的功能面仍需外部凭据与多周工作量，本轮不碰；`qx plan` 用户级读入命令仍受 `cli.rs`/`cli_args.rs` 行数棘轮限制（上一轮已登记）。


### 地基规格对象落地（2026-10-06）· 规划 §6.2 七类「统一身份」声明式文档

**口径**：按 `docs/qianxing-项目结构与GitHub竞品对比及优化方案-2026-10-06.md` §6.2 / §7 落地七类地基对象，本轮只做**声明式文档 + 门禁牙齿**，不接实盘功能（功能面留后续轮次）。全部改动加性：门禁 **662 → 667 全绿**，未删任何既有判据。

- **新增第 24 个 crate `qx-spec`**：`ProjectManifest` / `ExperimentSpec` / `RunRecord` / `CapabilityManifest` / `EvidenceBundle` / `SchemaRegistry` 六类 + `qx-data::catalog_v2::DatasetManifestV2`（第七类，与既有 `DatasetManifest` 同 crate 以便复用 v1 解析）。统一走 `qx_spec::describe(kind, payload)` 这**唯一读入漏斗**，`FoundationKind` 七变体 `ALL` 数组驱动，`from_json` 一律 `deny_unknown_fields` 严格解析。
- **对象层硬约束（不只是字段校验）**：`RunRecord` 的 `status=completed` 必须同时带 `finished_at_ms` + `replay_verdict=verified` + 非空 `artifact_refs`（复现不了就不许标完成）；`CapabilityManifest` 声明 `sandbox_tested` 就必须有 `evidence`、`production_approved` 必须先 `sandbox_tested`、`level>=L3` 必须有沙盒证据、`level>=L4` 必须已生产批准；`EvidenceBundle` 的 `L4` 声明网络必须是 `mainnet`、`L3` 不得是 `mainnet`。这四条把「没拿到外部证据不得声明已通过」写成了**类型层拒绝**，而不是文档里的口头约定。
- **7 份严格 JSON Schema**（`schemas/*-v1.json`）：每份 `additionalProperties:false` + 版本键 `{"const": 1}`，与 Rust 侧 `*_SCHEMA_VERSION` 常量同源。
- **门禁牙齿 `foundation_specs_check()`（5 颗）**：① 七份 Schema 与 Rust 版本常量逐条一致；② 七类对象各只有一处 `pub struct` 定义（同名概念不得出现第二份）；③ `qx-spec` 是 workspace 成员且被 `qx-cli` 依赖（地基对象必须有生产读入者）；④ `describe` 派发臂覆盖七类且不重不漏；⑤ `capability.rs` 的四处证据闸门字面量必须仍在（防止有人把硬约束改软）。`GATE_CHECK_FLOOR` 仍为 633（667 实测已覆盖）。
- **生产读入者**：`qx-cli` 新增 `plan_commands.rs::verify_foundation_specs()`，由既有 `ecosystem` 自检驱动——七类各给一份最小合法夹具必须读入、再删掉版本号必须被拒。实测 `qx-cli ecosystem` 输出 `[规格 · qx-spec] 7 类规格读入/拒绝通过 ✓`，rc 0。
- **登记 limitation（诚实边界）**：① `foundation_objects_have_no_cli_readout_command`——规划里的 `qx plan` 只读命令**本轮未做**：`cli.rs`(679>673) 与 `cli_args.rs`(523>513) 已顶到 `maturity/line_budgets.yaml` 的**只降不升**棘轮，加一条新命令就会红。已改为走 `ecosystem` 自检接入，命令面留后续轮次（需先给这两个文件减行）。② `run_record_and_capability_not_yet_wired_to_backtest_artifacts`——两类对象目前只有声明与校验，回测产物尚未真正写入 `RunRecord`。
- **顺带修一处既有 clippy 红**：`crates/qx-cli/tests/unit/scheduler_dispatch_support.rs:220` 的 `&[accepted.clone()]` 触发 `clippy::cloned_ref_to_slice_refs`（`clippy --all-targets -- -D warnings` 下即 error，CI 会红）。改为 `std::slice::from_ref(&accepted)`，**净增 0 行**、行为不变。该告警在 `9d46305` 基线提交上就存在（rustc 1.98.1 的 lint 版本漂移），非本轮引入。
- **本轮实测**：`python tools/check_architecture.py` **667 项全绿 / 0 红**；`cargo clippy -p qx-spec -p qx-cli -p qx-data --all-targets -- -D warnings` **rc 0**；按「告警行号 ∩ 本次新增行区间」求交集，新增 **1,673 行里 0 条 clippy 告警**（唯一告警是上面那条既有项）；`cargo test -p qx-spec` **10 passed**；`cargo test -p qx-cli --bin qx-cli` **311 passed / 4 failed**，4 条全是既有沙箱 `os error 231`（`ERROR_PIPE_BUSY`，子进程管道环境噪声，与本轮改动无关，历史轮次同基线）；`qx-cli ecosystem` rc 0。


### 发布审计第二轮（2026-10-06）· 换角度 A/B/C · API 前后端 / 配置键 / 错误路径与状态机

**口径**：同第一轮（主体流程全联通、无断链、无孤儿逻辑、无死循环、前后端贯通），但换三个**不同**的扫描角度，避免与第一轮重复。详见 docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md 第 14 节。

- **第 A 轮 · API 前后端 + trait 方法零读者（1 条新登记）**：17 条路由与 deploy/README.md 端点表 17 个 token 逐一相等（门禁 `api_endpoint_table_routes` 双侧钉住）。全量扫 98 个 trait 方法，7 个无生产调用点——5 个 `JobQueueBackend` 方法已登记；新登记 `outbox_and_consumer_projection_contract_methods_have_no_production_caller`：`OutboxStore::append_outbox`（生产走各后端固有 `append`）与 `TransactionalConsumerStateStore::load_projection`（唯一消费者是契约用例），与已登记的 job_queue 那条同形。静态门禁的 `zero_reference_public_surface_check` 只扫 `pub fn`/`pub const`，trait 方法对它不可见（已登记盲区）。
- **第 B 轮 · 配置键接线（零发现）**：`runtime_config/schema.rs` 的 63 个字段全部有生产引用；deploy 下 18 份 runtime 模板的全部键都能对应到生产结构体字段（`ops` 是 map 键、`_comment` 由 `strip_config_comments` 剥离）；`deploy_template_coverage` 动态枚举 18==18，无漏检模板。
- **第 C 轮 · 错误路径 / fail-closed / 状态机（1 条新登记）**：`OrderStatus` 11 变体全部从 `PendingSubmit` 可达、7 个非终态全部有出边、无死锁状态；两处 `unreachable!`（`qx-runtime/src/lib.rs` 的 MarketQuote 臂、`qx-cli/src/strategy_host.rs` 的共享内存臂）经核实确实不可达；160 处 `let _ = ` 全为尽力而为语义；`StrategyContract{Input,Intent,Output}` 与 Python 侧逐字段相等。新登记 `legacy_no_spec_notional_fallback_synthesizes_a_spot_spec_instead_of_failing_closed`：`MaxNotionalRule::check` 缺 `instrument_spec` 时合成「现货 1:1」规格而非 fail-closed（源码两处 TODO 同源）。
- **本轮实测**：`python tools/check_architecture.py` **662 项全绿**（改动仍全部加性）。


### 发布审计三轮（2026-10-06）· 连通性 / 孤儿逻辑 / 终止性

**口径**：主体流程全部联通、核心链路无断链、无孤儿逻辑、无死循环、前后端贯通；每轮把当轮发现全部处理完（修掉，或按仓库先例登记为 capabilities.yaml limitation）才进下一轮。详见 docs/qianxing-架构设计与工业级优化改进方案-2026-10-06.md 第 13 节。

- **第 1 轮 · 连通性 / 断链（2 处已修）**：核心事件链（EventLog → pipeline → Ledger/Oms）完全连通，无断链。命中 2 处 README 过度声明并按代码事实改口：`qx-genglu` 行去掉「绩效指标、归因」（分别在 qx-xingban / qx-cli / qx-runtime），`qx-control` 行去掉「事件订阅游标」（在 qx-api）。`EventKind::Settle` 写侧「孤儿」经核实**不是断链**：它是派发循环写的结算标记事件，运行期 pipeline 刻意按 no-op 匹配，状态变更由同段落账簿分录单独承载。
- **第 2 轮 · 孤儿逻辑（3 条新登记 + 1 颗新判据）**：字段级零读者是仓库已登记的已知缺口（#174，静态门禁只扫类型与 pub fn/pub const，看不见结构体字段）。按 #118/#170/#171 先例「保留不删 + 登记 limitation + 双向钉住」，新登记 `risk_snapshot_carries_fields_no_decision_reads`（canonical_order_risk_decision）、`backtest_report_max_drawdown_raw_has_no_reader`（local_backtest）、`job_spec_declaration_fields_have_no_production_reader`（paper_execution）三条 limitation；新增双向判据 `zero_reader_struct_fields_stay_registered_in_capabilities`（crates/qx-cli/tests/unit/zero_reader_fields.rs）；在 qx-risk / qx-xingban / qx-scheduler / qx-data 的相应字段留下「零生产读者」路标（受行数预算只降不升约束，qx-scheduler/qx-xingban 用行尾注释、净增 0 行）。
- **第 3 轮 · 终止性 / 死循环 / 无界增长（2 条新登记）**：全部循环与阻塞调用有界，未发现死循环。已登记：EventLog 无保留策略、审计链无轮转、进程日志无轮转、内存投影共享 EventLog 保留缺口、成交去重台账刻意无界（由 bounded_growth_and_reap_check 守「只增」）。新登记 `scheduler_run_history_grows_without_retention`（paper_execution）：Scheduler.{runs, completed} 只增不减、落盘重启不缩小、dispatch 每 tick 遍历 runs() 判超时；`consumer_state_processed_ids_and_dead_letters_grow_without_retention`（storage_consistency_contract）：文件消费者 processed_event_ids / dead_letters 只增不减，SQLite/Postgres 同形状。
- **本轮实测**：`python tools/check_architecture.py` **662 项全绿**（三轮改动全部加性、未增删判据数量）；`cargo fmt` 改动过的 5 个 crate `--check` 无差异；新判据 `zero_reader_fields` 模块 **4/4 通过**；工作区用例与首轮同基线（少数失败为沙箱 os error 231 子进程环境噪声，非代码面）。


### V13 R14 + R15 + R16 收口（2026-10-04）· 第 17-19 轮三扫 · 数值精度 / 时间与时钟 / 序列化与 schema 演进

**三扫新角度（不重复 R8-R13 已查的面）**：R14 数值与精度面（u64 溢出回绕、除零、f64 舍入方向、跨语言同一数值的表示一致性）、R15 时间与时钟面（墙钟/单调时钟混用、ms/s 单位混淆、跨进程时间戳比较、审计链 ts 单调性、NTP 回拨行为）、R16 序列化与 schema 演进面（JSON 数值精度、字段名漂移与别名、缺失字段的默认值语义与 fail-closed 一致性、可选字段与空值的区分）。

- **R14 启动器闸门不对称（已修）**：`deploy/start-qianxing.sh` 在 `supervise` 之前没有 `runtime-check` 前置闸门，而 `start-qianxing.ps1` 有。`supervise` 只走 `plan_workers` 的拓扑校验，**不检查配置引用的文件是否存在**（数据集 bundle、研究快照、秘密文件的存在性只在 runtime-check / live-check 里查）——坏配置时 7 个子进程被拉起又在启动阶段各自失败，监督器按 fail-fast 把其余的停掉、process-logs 里落下日志，而不是在任何子进程起来之前就拒绝。Linux/macOS 与 Windows 走的是两条不同的失败路径。已在 `.sh` 补上同一道闸门。
- **R14 闸门退出码陷阱（已修）**：闸门失败时用 `if ! cmd; then echo ...; exit "$?"; fi`，此刻 `$?` 拿的是上一条 `echo` 的状态（**0**），调用方会以为闸门通过了：部署平台按 0 退出继续走下一步，而实际上一个子进程都没起来。已固定为 `exit 2`，与本脚本另外两处拒绝（配置缺失、可执行文件缺失）同码。
- **R16 三处廉价修法（已修）**：① Python `StrategyInput.from_dict` 补 `_reject_unknown_keys`（同文件的 `StrategyIntent` / `StrategyOutput` 都有，只有 Input 漏了：策略作者把 `positions` 拼成 `positionz` 时输入侧静默拿到空 dict，策略可能以为账户是空的而误触发）；Rust 侧 `StrategyContractInput` 同步加 `#[serde(deny_unknown_fields)]` 保持双侧对称（该结构在 Rust 侧只被构造、不从不可信输入反序列化，加注解无害且为将来防护）。② `FactorReport::validate` 补 `missing_policy` 的 `reject/skip/zero` 词表校验（`FactorAnalysisConfig::validate` 本来就有；报告能被 `from_json` 反序列化，只有空串校验时手改一份 JSON 能放行到下游 `resolve_missing` 才报「missing_policy 非法」，报错落点离改错的地方很远）。③ outbox `attempts` 读回改用 `parse_sqlite_u32` / `parse_u32` 而不是 `... as u32`：attempts 以 TEXT 落盘，u64→u32 是真截断，手工改库写成超过 `u32::MAX` 会回绕成小值、被误判成「仍在正常重试」而绕开死信判定。**注意 `schema_version` 分两格**：sqlite 侧以 i64 落盘、`i64 as u32` 是真截断（已修）；postgres 侧以 `INTEGER`(i32) 落盘，`i32 as u32` 是 32 位↔32 位按位重解释，`((x as i32) as u32) == x` 恒成立、无损，保留原样。
- **R14 数值与精度（0 真缺陷，1 处登记）**：台账记账（`apply_fill_with_multiplier_inner`）与撮合引擎、`numeric.rs`/`trading.rs`/`ledger`/Binance/CCXT 适配器全用 `checked_mul`/`checked_div`，外部数值入口全部过精度与范围验证。**登记**：`fee.rs` 的 `notional()`/`bp_amount()` 用 `saturating_mul` + 裸 `/`，与全库 checked 风格不一致；同族还有 `multi_leg.rs` 单位成本（注释自己写明「不能夹到 i64::MAX」却用了 `saturating_mul`）、`orderbook_backtest.rs` 预检（裸 `*`/`/`）、`ashare/trading.rs` 除权除息折算。全部当前不可达（bp 限 0..=10000、触发饱和需 `net_cost_raw > 1.7e34`、除零已有显式守卫），但收敛要改函数签名牵连三处调用方。`multi_leg_cost_bps` 的除零守卫本轮专门核实存在（`turnover_raw <= 0` 返回 `None`）。
- **R15 时间与时钟（0 真缺陷，1 处登记）**：全部 `*_ms` 字段统一 `Duration::from_millis`（`lease_clock` 正确 `/1000`）；111 处 `SystemTime::now()` 全部用于 epoch 时间戳值或临时目录命名，elapsed/deadline 统一 `Instant::now()`（NTP 回拨不会让超时提前或无限延长）；跨进程比较路径均为注入的单调时钟，CCXT 子进程的 `event_ts`（远端时钟）与 `received_ts`（父进程时钟）分离存储。**登记**：三家审计 store 的 `append` 均不保证 `ts` 单调不减，`validate_audit_chain` 与 `ControlPlane::from_json` 也不校；`retire_overflow_with` 按终态记录的 `(ts, command_id)` 升序退场，ts 倒退时退场顺序会偏向较新的命令——被退场的那条已终态、账目不错，但保留窗口会留下「较新的被留、较老的被退」。触发窗口很小（两次 `transact` 之间赶上一次 NTP 修正），hash 链与 sequence 完好。拒绝式修法（`Conflict`）会把一次 NTP 回拨变成控制面永久卡死；钳制式修法要在 `audit_entry_hash` 之前做且需处理既有非单调前缀，两种都不是改字能收口的。
- **R16 序列化与 schema（0 真缺陷，2 处登记）**：账户快照 schema_version 双读校验、8 个钱字段全 `Option<i128>` 无 default、`SnapshotHeader` 全 required、运行时配置全 `deny_unknown_fields`、Binance 全程 `String` 定点、CCXT `raw_i128_value` 显式拒 f64、A 股公司行为手工逐字段抽取（绕开 struct 默认值）、C ABI 5 个 struct 字段顺序与 C++ 头文件逐一比对一致、Python 与 Rust 的 StrategyInput/Intent/Output 字段名逐一对齐——**均已闭合**。**登记**：① serde_json 全仓未开 `arbitrary_precision`，`*_raw: i128` 遇 > u64::MAX 的 JSON 整数经 f64 中转丢约 1e6 量级精度；今天不可达（价格经 `Money::from_dec` 限 ≤9 位精度，qty 1e20 × price <1e9 → notional 1e29 远超任何合理的 `MaxNotionalRule` 上限；A 股实际值域 < 1e15），但 `validate_order` 无上界检查；收敛要么开 feature（`serde_json::Number` 内部表示从 u64/f64 换成 `Rc<str>`，波及全仓 `as_u64()`/`as_f64()` 形态与编译），要么逐字段加范围断言，两者都不是改字能收口的。② `DatasetComponentManifest.format` 的 `skip_serializing_if` 让 round-trip 字节漂移（值语义不丢，只影响血缘/指纹的字节级一致性）。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 636 / [FAIL] 0**（`GATE_CHECK_FLOOR` 629 → 633，新增 `launcher_pregate_check` 两颗 + `schema_defense_consistency_check` 四颗）；`cargo test --workspace --all-targets --no-fail-fast` rc 0 / **0 failed**；`python -m pytest python\tests -q` **64 passed**；`qx-storage --no-default-features --features sqlite,postgres,nats` **73 passed** rc 0；行数棘轮按 `--snapshot` 重登记（44 个超 500 行文件）。
- **工业级差距复核（结论不变：未达工业级）**：P0-1 sandbox evidence（`sandbox_tested: true` 与 `production_approved: true` 各命中 **0** 次）、P0-2 文件后端非事务性、P0-3 C ABI 信任根不锚定——本轮**没有**新动 P0 项，全部维持「未排期」。本轮修的都是启动器行为不对称与防御口径不一致，不触及 P0 的信任边界。


### V13 R12 + R13 收口（2026-10-04）· 第 15/16 轮 · Postgres 池自死锁、子进程回收、环文件泄漏

- **R13-A 修 PostgreSQL 控制面事务在 `pool_size = 1` 时当场自死锁（真缺陷，A 级）**：`PostgresControlStore::transact_control`（`crates/qx-storage/src/postgres.rs`）先 `lock_client()` 拿到 `clients[index]` 的 `MutexGuard`，事务提交之后在**同一个作用域内**调 `sync_control(&plane)`；`sync_control` 内部走 `read()` 会再取一次 `lock_client()`。池索引是 `next_client.fetch_add(1) % clients.len()`，池容量为 1 时两次都落到 `clients[0]`，而 `std::sync::Mutex` **不可重入**——同一线程重入就是当场自死锁。更关键的是：**事务已经提交成功**，但函数永远回不了调用方，`api_service.rs` 的 `with_control_submitter`（API 受理控制命令）与 worker 回写终态两条生产路径全停，控制面完全瘫痪。`postgres_pool_size` 的合法范围本就含 1（`topology_validation.rs` 校验 1..=128），不是假设中的边界值。已改为 `sync_control` 之前显式 `drop(client)`（`commit` 已经消费掉 transaction，要放的是它借用的那个连接锁），补 `#[ignore]` 回归用例 `postgres_control_transaction_completes_with_a_single_connection`（需挂 `QX_TEST_POSTGRES_DSN`，已在 postgres feature 下确认编译进用例名册）。
- **R12-C 修 CCXT 子进程 spawn 之后管道取失败不回收子进程（防御不一致）**：`CcxtProcessClient::spawn` 用 `child.stdin.take().ok_or_else(...)?` 取管道，失败直接 `?` 返回而子进程还在跑。`Stdio::piped()` 之后这两个 `Some` 是不变式、这条路径今天走不到，但上游一旦把 worker 改成无管道模式就会留下一个无人回收的孤儿 Python 进程——而 `strategy_host.rs` 的同一处早已经有 kill+wait。已改成先 `kill()` + `wait()` 再返回错误。注意 `call()` 里那次 `take()` 是「取用完就 `Some(handle)` 还给自己」的恢复式取用，语义不同、不在此列，门禁判据只读 `spawn` 的函数体。
- **R12-B 修共享环临时文件在设置失败路径上泄漏**：`.input`/`.output` 是 `memmap2` 打开的 mmap 文件（默认 64 MiB 一个、一对 128 MiB）。正常路径由 `PythonStrategyClient::drop` 回收，但 `SharedRingWriter::create` 与 `SharedRingReader::open` 任意一步失败时 `ring_paths` 还没被赋值，那条 `Drop` 不触发，`%TEMP%` 里就永久留下一对几十 MiB 的孤儿文件。已新增 RAII 守卫 `RingFileGuard`：从路径生成那一刻接管清理责任，成功时 `disown()` 把责任交还给客户端的 `Drop`（`remove_file` 对已删除文件幂等）。
- **R13-C 修 stderr 泵锁中毒静默返回空诊断**：`strategy_host.rs` 的 `diagnostics()` 在锁中毒时 `return String::new()`，`death_note()` 因此报「worker 无 stderr 输出」，运维会去看一个根本没坏的方向。已改为返回 `"; stderr=（stderr 泵锁中毒，尾部不可读）"`——诊断面自己交代自己坏了。
- **R12/R13 其余（0 处已修、4 处登记）**：① `process-logs/*.out.log`/`.err.log` 无轮转 / 无大小上限（与已登记的 EventLog/audit.json 无保留策略不同目录、不同形态，那条是不可丢的交易账本、这条是可丢的运行日志，处置策略本就该一起定）；② 写侧超时错误把 `Timeout`（handler 活着但不读 stdin）与 `Disconnected`（写线程自己死了）混成同一句「存活但不接收输入」，改字没有回归证据钉住；③ spread 恢复块与 scheduler 轮询只在循环头检查停机令牌，Ctrl+C 落在长恢复块内部要等整批跑完才生效（账目不错，错的是停机响应时间）；④ 全仓 25 处 `.expect("...poisoned")` 今天不可达（R10 已核持锁段内只返回 `Result`），但任何人往持锁段里加一个 `unwrap()` 这个面就从理论变成实路，且门禁没有一颗判据守「持锁段内不得有 panic 调用」。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 630 / [FAIL] 0**（`GATE_CHECK_FLOOR` 627 → 630，新增 `resource_lifecycle_and_lock_reentrancy_check` 三颗）；`WORKSPACE_TEST_FLOOR` 1082 → 1083（1 条 Postgres 池容量为 1 的回归用例，挂 `#[ignore]`）；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0 / **1068 passed / 0 failed / 1 ignored**（新增用例在默认 feature 集下不编译，postgres feature 下已在用例名册中确认）；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**；全 feature 矩阵两组全绿：`qx-storage --no-default-features --features sqlite,postgres,nats` **73 passed**、`qx-cli --no-default-features --features postgres,nats` **389 passed**，全 rc 0。行数棘轮按 `--snapshot` 重登记（44 个超 500 行文件）。
- **工业级差距复核（结论不变：未达工业级）**：P0-1 sandbox evidence（`sandbox_tested: true` 与 `production_approved: true` 各命中 **0** 次）、P0-2 文件后端非事务性、P0-3 C ABI 信任根不锚定——本轮**没有**新动 P0 项，全部维持「未排期」。本轮修的都是**可达的真缺陷**（Postgres 池自死锁在合法配置下必然复现），但不触及 P0 的信任边界。


### V13 R11 收口（2026-10-04）· 第 14 轮三扫 · WS 假 Close 断连、退出码丢失、C ABI 枚举 UB

- **R11-A 修 WebSocket 按整块缓冲逐字节扫 Close 帧（真缺陷）**：`crates/qx-api/src/ws.rs` 的读循环用 `client_buffer[..size].iter().any(|byte| (*byte & 0x0f) == 0x8)` 判 Close。但 Close 帧的 opcode 只出现在**帧起点**，这个表达式扫的是缓冲区里的每一个字节——256 个字节值里有 **16 个**（`0x08/0x18/…/0xF8`）会命中，二进制行情载荷里这类字节很常见，**客户端发一个完全正常的 text/binary 帧就把服务端静默断连**，而且 `return` 先于 `advanced = true` 执行，空闲计数也不推进。已改成 `WsCloseScanner`：按 RFC 6455 的帧头（2 字节基础头 + 126/127 的扩展长度 + 掩码键）跨读累积，帧没攒够就等下一次读，只有读到完整帧头才看 opcode，非 Close 帧整体跳过（本通道只推送事件、不消费客户端载荷，所以不解析载荷内容）。补 6 条回归用例，其中一条专门用 `[0x81, 0x03, 0x18, 0x00, 0x01]` 复现——`0x18`/`0x68`（'h'）都会命中旧判据。跨读累积同时引入了一个旧实现不存在的内存面：客户端可以每次只给一两字节、故意不完成帧头，让累积缓冲无限长大，所以另设 `WS_CLOSE_PENDING_CAP`（1 MiB）上界，越界即清空缓冲并按断连处理（fail-closed），补一条专门的溢出用例。
- **R11-A 修编排器子进程退出码被字符串化（真缺陷）**：`qx-orchestrator` 的 `ManagedProcess::poll_exit` 把 `status.code()` 转成 `String` 后传下去，父进程只能一律 `exit(2)`——panic 的 **101**、OOM 的 **137**、干净退出的 **0** 在监控里长得一样，故障分类的工作量被推回读日志。已把返回类型改成 `ProcessExit { raw_code: Option<i32>, label: String }`，`SupervisorStop::WorkerExited` 携带 `code: Option<i32>`，`supervise_workers` 返回新的 `SuperviseFailure`（`Usage` / `WorkerExit { raw_code, detail }`），`cli.rs` 把非零原始码原样上报、被信号杀死（`None`）走 **128**（POSIX 信号退出约定）。`SuperviseFailure::append` 保证收尾未收干净时原始码不被抹掉。补回归用例 `worker_exit_carries_the_raw_exit_code`。
- **R11-D 修 C ABI 的 `side` 是 `#[repr(C)]` 枚举导致宿主 UB（真缺陷）**：插件把 `side` 写进**宿主**内存。`QxOrderSide` 只有 `Buy = 1` / `Sell = 2` 两个判别值，而 Rust 读取一个不在已声明判别值里的 `#[repr(C)]` 枚举值**本身就是未定义行为**——`match` 里不存在可达的拒绝臂，一个签名过的插件写 `3` 就能在宿主上触发 UB。已改为 `QX_ORDER_SIDE_BUY/SELL: u32` 常量加显式拒绝（`"C ABI intent side 非法: {other}"`）。**布局不变**（都是 4 字节），既有插件无需重编译；C++ 侧本来就有 0 个消费点（`cpp/` 下 `qx_order_side` 只出现在头文件的声明与字段里），CI 不受影响。门禁的 `c_abi_header_check` 类型映射表补登记 `qx_order_side -> u32`（枚举集合比对自动收敛为只剩 `QxMarketEventKind`）。补回归用例 `decode_decision_rejects_an_invalid_c_side_value`。
- **R11-A 其余**：CLI 47 个长旗标全部有消费点、无 `ident: _` 丢弃；help 37 条入口 ≡ clap 37 个变体 ≡ `cli.rs` 37 条派发分支；配置 schema 全部字段（含 `WorkerConfig` 13 个、`StrategyRuntimeConfig` 39 个）均有生产读点；API 17 条路由覆盖完整（`KEYLESS_READ_ROUTES` 4 + `PROJECTION_SCOPED_ROUTES` 7 + 4 条全局出口 + `/account/snapshot/diff` + 1 条 POST）；WS 7 种下发帧类型与文档逐一对应；`paper-e2e`/`paper-check` 写入 `data_dir` 属验收链而非冒烟测试（同日重跑零新增已由 `paper_worker.rs` 显式处理）。**均为已验证连通，无新缺陷。**
- **R11-C 信任边界（0 处已修，5 处登记）**：A 级 3 处——① `decode_decision` 的 `slice::from_raw_parts(decision.intents, decision.intents_len)` 信任插件给的长度（只有 `MAX_PLUGIN_INTENTS = 100_000` 与「len>0 但指针为空」两道校验，没有由分配大小反推长度），插件只分配 1 个 intent 而声明 99_999 就越过分配边界读相邻堆内存；② `read_required_string` 用 `CStr::from_ptr` 读到 NUL 为止，不含终止符的指针会一路扫到页边界；③ `InstrumentId::parse` 的 symbol 段不做词表校验（`"../../etc/passwd.BINANCE"` 可解析）——**已核实今天不是活的穿越面**：`account_event_log_name` 只接收 account_id/venue_id，全仓 `crates/` 下无任何以 instrument 为路径片段的 `join` 调用。前两条的共同点是「这条 ABI 没有长度协商」，收敛要改契约（加容量字段或改迭代协议）；第三条收敛要么在 `parse` 加词表（影响历史产物形态）要么在每个把它当文件名的落点净化。另登记 3 条：`ControlCommand`/`Order` 未开 `deny_unknown_fields`（拼错字段被静默丢弃，Binance 侧有 `ReconcileRequired` 兜底但订单数量不会）、明文 API 下 `dry_run`/`permission` 由客户端控制（生产已被 `topology_validation` 拦死，剩 paper/sandbox 本地）、CCXT 子进程退出码不区分崩溃与被 kill（stderr 走 `inherit`，崩溃栈可见但无机器可读区分位）。五条全部按未排期登记，理由是每条的收敛都要改对外契约或砍掉现有验收能力。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 627 / [FAIL] 0**（`GATE_CHECK_FLOOR` 624 → 627，新增 `untrusted_input_boundaries_check` 三颗）；`WORKSPACE_TEST_FLOOR` 1073 → 1082（8 条回归用例：C ABI 非法 side 1 + WebSocket 帧扫描器 6 + 子进程退出码 1）；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0 / **1068 passed / 0 failed / 1 ignored**（R10 基线 1060 + 本轮 8 条）；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**；全 feature 矩阵两组全绿：`qx-storage --no-default-features --features sqlite,postgres,nats` **73 passed**、`qx-cli --no-default-features --features postgres,nats` **389 passed**，全 rc 0。行数棘轮按 `--snapshot` 重登记。
- **工业级差距复核（结论不变：未达工业级）**：P0-1 sandbox evidence（`capabilities.yaml` 里 `sandbox_tested: true` 与 `production_approved: true` 各命中 **0** 次）、P0-2 文件后端非事务性、P0-3 C ABI 信任根（本轮修的是宿主侧的 UB 与退出码，信任根仍不锚定）——本轮**没有**新动 P0 项，全部维持「未排期」。


### V13 R10 收口（2026-10-04）· 第 13 轮三扫 · 实盘去重键归零、恢复 worker 静默收窄、panic 丢诊断、确定性复核

- **R10-C 修三处实盘/恢复路径的静默降级（真缺陷）**：① **`binance.rs:1023` 成交回报的 `trade_id` 缺字段时 `unwrap_or(0)`**——`trade_id` 是 `seen_fill_keys` 去重键的一部分（`ingest_fill_wire` 的 `key = (client_order_id, trade_id, qty, price, fee)`），归零会让两笔都没有 `t` 的成交互相误去重、**漏计一笔且无人告警**；同一段代码对 `orderId`（字段 `i`）就是 `.ok_or_else(...)`，而 CCXT 侧（`ccxt.rs:548`）对同一情形本来就 fail-closed——两边口径不该不一致。已改为 `ReconcileRequired("Binance 回报缺少 trade_id(t)")`，补回归用例 `user_event_without_trade_id_requires_reconcile_instead_of_zeroing_it`。② **两个多腿恢复 worker 的 `venue_id` 缺省静默冒充**（`worker_entry.rs:136` → `"BINANCE"`、`:373` → `"ccxt"`）——`pending_spread_recovery_groups`（`spread.rs:230`）里**空 venue_id 是有意的通配符**，而默认值产出的是非空字符串，等于把通配符路径变成不可达：配置遗漏时 worker 会静默只扫一个 venue、**漏掉其余 venue 的裸腿**，而敞口正是"停止扫描等于永久晾着"的那种。已抽 `recovery_worker_venue_id()` 统一为启动即拒（点名 worker id 与角色），补回归用例 `recovery_worker_requires_an_explicit_venue_id`。③ **`worker_shutdown.rs:38` 的 `map_err(|_| "{label} panic")` 丢掉 JoinError 的 Debug 输出**——运维拿到的是"线程挂了"而看不到挂在哪里，等于把故障定位的工作量推回现场。已改为 `format!("{label} panic: {error:?}")`。门禁新增 `live_path_fail_closed_check` 三颗判据把这三处钉住。
- **R10-C 其余分级（不改，登记）**：A 级（生产可达 panic）**0 处**——6 处 `unwrap`/`expect`/`unreachable` 都依赖当前不变式成立（Binance 订单表 guard、strategy transport 枚举、API 快照刚置 Some 等），属"脆弱安全"而非活缺陷；C 级（错误上下文不足）7 处中已修最重的 1 处，余下 6 处（`event_pipeline.rs:424` 的 `.stderr(Stdio::null())` 丢弃 consumer 子进程 stderr、`runtime_check.rs:387` 的 JSON 报错缺文件路径、`readiness.rs:133` 的 `is_err()` 后只剩 `"control_store_unavailable"`、三处 `PoisonError` 消息笼统）属诊断增强，不改行为。
- **R10-A 跨重启幂等性（0 真缺陷）**：5 条崩溃/重启轴逐一取证——① EventLog 去重**不是独立已见集合**：`dedup_key` 持久化在事件元数据里随日志落盘，`seen_fills` 不直接落盘但由 `rebuild_runtime_indexes` 从 `Filled` 事件重建（`pipeline.rs:1177-1246`），不存在"事件已落盘、去重表未落盘"的中间态；`persist()` 是 `ingest_once` 最后一步，未落盘前崩溃＝整笔丢失、无重复。② outbox `event_id = {log_name}:{seq}` 确定性稳定；claim→publish→ack 顺序下若 publish 成功而 ack 前崩溃则重投，消费端按 `event_id` 幂等（`ConsumerEngine::consume` 第一步就是 `is_processed`）。③ 三后端 `transact_control` 都是"先提交快照、后补链"，且 `update(&mut plane)` 返回 Err 或 panic 都不会持久化——`validate` 在 mutation 之前，validate 失败则 mutation 不执行，不存在"Err 但 plane 已部分改"。④ 分段 EventLog 后端的崩溃窗口比单文件多一层（segment 已写、manifest 未更新），但 `read_if_exists` 按 digest/event_count **拒绝读取**（fail-closed），不是静默修复。⑤ JobQueue 租约恢复走 claim 路径的隐式接管（`expires_ts <= now` 则递增 fencing_token 接管），墙钟秒——回拨会延迟过期但不会永久不回收。SQLite/Postgres 的 EventLog+Outbox 在同一事务内，无 crash gap。
- **R10-B 无界增长（1 处新增登记）**：内存侧 14 个只增不减字段、磁盘侧 2 个只增不减文件，全部有淘汰调用计数的实测支撑。**新增登记** `audit_chain_has_no_retention_or_compaction_trigger`——持久化 `audit.json`（`AuditFileStore::append_unlocked`）只追加不退场（remove/retain/drain/truncate/pop/clear/delete/rotate/retention/compaction/archive/purge/prune **全词 0 处淘汰调用**），增长驱动量是控制命令数；内存侧 `ControlPlane.audit` 有 4096 的 `FINALIZED_COMMAND_RETENTION` 上限，持久化侧没有。**刻意不修**：内存上限省的是进程内存，持久化哈希链一旦轮转就会让"链落后自愈"与"前缀内容分叉检测"都失去参照物，轮转的前提是先定"轮转后从哪一条继续接哈希"。另登记 `event_log_in_memory_projections_share_its_retention_gap`：`Ledger.entries`、`Oms.orders`、`ApiState.events`、`ApiAccountProjection.events` 四个字段都是 EventLog 的内存投影，上界等于 EventLog 长度，不是独立风险面——EventLog 的保留策略定了它们跟着定。已验证有界 18+ 处（SnapshotHistory 1024、ApiEventBus 4096、ControlPlane 三表 4096、Outbox ack 删除、Reconcile 报告覆写）；**句柄泄漏 0 处**（ConnectionGuard 的 Drop 归还额度、SQLite 短生命周期连接、文件全部 open→write→close）。
- **R10-D 确定性与时钟面（0 缺陷）**：生产墙钟唯一入口是 `runtime_wiring.rs:289` 的 `runtime_timestamp_ms()`，全仓 194 处 `SystemTime::now` 里只有它在生产路径——读者是审计元数据 `ts`、API 请求戳、metrics 渲染、租约时钟（`lease_clock` 截断到秒）、实时行情的 `received_ts`、策略心跳，**无一路写进重放产物**；回测路径里那两处是临时文件名。EventLog 事件的 `ts` 来自事件自身（`ingest` 传入），不是墙钟。`qx-api/src/lib.rs:2042` 明确要求 `serve` 按连接读取墙钟而非调用方预取，`runtime_api_worker_identity.rs:168-174` 的用例点名钉住。HashMap 在架构规则层面被排除于顺序敏感迭代（`qx-core/src/lib.rs:9`：「HashMap 只用于查找，遍历一律排序」；`oms.rs:11`：「使用 BTreeMap 而非 HashMap，保证快照和回放的遍历顺序确定」）——全仓 `HashMap|HashSet|IndexMap` 仅 **3** 处命中，全部是那条规则本身、一处测试 `.collect::<HashSet<_>>()`、一处纯去重用途；抽查的 5 个 `.insert()` 落点（Binance 订单映射、factor artifact/report 表、rebalance targets）**全在 BTreeMap 上**。`sourcing.rs:13` 另有明写不用 `DefaultHasher` 的理由（跨版本不稳定会破坏"两次运行哈希一致"）。
- **R10-A/B 顺带核掉的 CI 覆盖缺口**：`qx-storage` 的 `default = []`，postgres 与 nats 后端**不在默认 `cargo test --workspace` 编译范围内**——本轮改动碰过 `postgres.rs`，若不单独验就会漏。已按 `.github/workflows/ci.yml` 的 matrix 四组补齐：`qx-storage --features sqlite,postgres,nats` **73 passed / 0 failed**、`qx-cli --no-default-features --features postgres,nats` **389 passed**、`qx-cli --features sqlite` **389 passed**，全 rc 0。CI 本身覆盖是完整的，缺口只在我此前的本地验证口径。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 624 / [FAIL] 0**（`GATE_CHECK_FLOOR` 621 → 624，新增 `live_path_fail_closed_check` 三颗）；`WORKSPACE_TEST_FLOOR` 1071 → 1073（两条回归用例）；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0 / **1060 passed / 0 failed / 1 ignored**；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**；全 feature 矩阵三组全绿（见上条）。行数棘轮按 `--snapshot` 重登记。
- **发布件按终树重打**：本轮改动横跨 `qx-adapter` / `qx-cli` / `tools` / `maturity` / 文档五个面，`qx-cli.exe` 与 wheel 里的 `.pyd` 都因此过期。`cargo build --release` 得 `qx-cli.exe` **12,560,384 字节**，`crates/**/*.rs` 比它新的 **0** 份；`tools/build_python_wheel.ps1` 得 **219,447 字节 / 17 条目**，sha256 `e98ec3102ff2c950e6f41711def218a7c0fb90c28e3b942dd40e605fe84545b9`，内嵌 `.pyd` md5 `6fb9285471edd527a8438d7a9ab89ee0` **≡** `target/release/_qianxing_native.dll` md5，**12 份 `.py` 与仓库逐字节不一致 0 份**；release `qx-cli` 三发 `all` / `ecosystem` / `runtime-check deploy\qianxing.runtime.example.json` 各 rc 0；把这份 wheel 装进**另一个**干净 py3.12 venv（`--no-deps --no-index`），`qianxing_bridge`/`qianxing_strategy`/`qianxing_ashare`/`qianxing_ccxt` 四个包 import 全 OK、`native.available() -> True`、`normalize_instrument('sz.000001') -> '000001.SZSE'`。
- **工业级差距复核（结论不变：未达工业级）**：P0-1 sandbox evidence（`capabilities.yaml` 里 `sandbox_tested: true` 与 `production_approved: true` 各命中 **0** 次）、P0-2 文件后端非事务性（`audit.json` 的无保留正是这条的延伸）、P0-3 C ABI 信任根——本轮**没有**新动 P0 项，全部维持「未排期」。


### V13 R9 收口（2026-10-04）· 第 12 轮三扫 · 审计链并发误报、WS 谎报路由、投影键分裂、策略超时错配

- **R9-A 修 `sync_control` 在 SQLite/Postgres 上的并发误报（真缺陷）**：三个后端的 `sync_control` 原先用 `existing.len() > records.len() || 前缀 zip 不等` 判分叉。但 **file 后端的整段 `transact_control` 都在同一把 `.control-plane.write.lock` 内**（读→改→存快照→补链一步到位），而 **SQLite/Postgres 的 `sync_control` 跑在控制面事务 `commit()` 之后**（`sqlite.rs:1084 → :1088`、`postgres.rs:1500 → :1507`）。`PRAGMA busy_timeout = 5000` 让两进程的写事务在提交时排队，于是进程 B 的事务必然读到 A 已提交的状态、它的 plane 是 A 的超集；B 补链在前、A 补链在后时，A 手里的 `existing`（含 B 追加的记录）就比 A 的 `records` 长——前缀逐条一致，旧判据却报 `Conflict`。**后果不是数据错，而是"控制面状态已经提交成功、`transact_control` 却返回 Err"**，调用方按失败重试会撞上重复请求。已把三后端改成只比公共前缀（`common = existing.len().min(records.len())`，前缀内容对不上才 `Conflict`，链更长按幂等空操作收口）。**篡改检测没被放宽**：`a_chain_ahead_of_the_snapshot_fails_the_transaction_inst_of_rewriting_history` 仍绿——它守的是同位内容分叉（`FORGED(99)` 落在 index 1，公共前缀内），不是长度。补回归用例 `audit_sync_control_treats_a_longer_chain_as_idempotent_not_conflict` 钉住三支语义（链更长＝幂等、前缀分叉＝仍拒、链更短＝正常追加）。`file/state.rs` 那段主张"链超前必须永久失败"的注释按新口径改写。**顺带核掉 R9-D2 另报的三条**：三后端 `sync_control` 前缀检查本来就逐字等价、哈希链共用同一份 `audit_entry_hash`（fnv1a）、`append` 幂等判定语义等价、空存储 `read` 三端一致；`sqlite.rs` 的 sequence 用 `max(sequence)+1` 而 file/postgres 用 `entries.len()`，稠密链等价，而 `validate_audit_chain` 读时强制 `entry.sequence == index` 直接拒掉 gap 链，实际不可达，按已验证一致登记。
- **R9-B 修 WS 查询键拒绝的谎报路由**：`crates/qx-api/src/ws.rs:77` 硬写 `"/events/live 不接受查询参数 {name}"`。但 **WS 通道不占路由表**（`deploy/README.md:299`："任何路径带 `Upgrade: websocket` 即在 HTTP 分派前转交"），客户端连 `ws://host/events?foo=bar` 收到的正文点名的是 `/events/live`——运维照那句话去查会查错地方。已取出请求真实 target（`split_target` 的 path 段）按实际路由点名，与 HTTP 侧 `lib.rs:1507` 的 `format!("{route} 不接受查询参数 {name}")` 同口径。同轮核掉 R9-B 其余四条：12 个文档登记码名与代码产出点逐一相等、`KEYLESS_READ_ROUTES`(4) 与 `PROJECTION_SCOPED_ROUTES`(7) 分派正确、OPTIONS 预检三态正确区分 `NotConfigured`/`Rejected`、`refuse_connection` 在 TLS 包装前判预算。**未修的登记面**：`/events` 的 500 分支文档没登记而 `/events/live` 登记了（两条都不可达）、`ApiProjectionKey::validate()` 的"两把都传但一个为空"分支文档没点名、`ControlError` 的 `{:?}` 复合串与 `serde_json` 原始错误串两处客户端无法精确匹配——都属契约文档补写，本轮按登记面处理。
- **R9-C 修投影键归一化不一致导致的同账户分裂**：`ApiProjectionKey::new` 原先不 trim。`market_bridges.rs:100-115` 的 `configured_account_event_logs` 返回**原始** `worker.account_id`/`venue_id`，而 `api_service.rs:343-344` 对快照 header 做 trim、`account_event_log_name` 内部也 trim——配置里写 `" main "` 这种带空白的账户，事件投影落到 `(" main ", venue)`、快照落到 `("main", venue)`，**同一账户在 `state.projections` 分裂成两格**，且两格都在表里、`missing_projection_response` 都返回 `None`，运维侧看不到告警。已在构造器统一 trim，三条路径（投影桥 / 查询串 / 快照 header）自动同一口径。**登记未修**：`/reconcile/reports` 把 `data_dir/reconcile/*.json` 全量返回（多账户下每个 worker 一份），而 `/account/ledger` 只读默认账户——同属 `KEYLESS_READ_ROUTES`，一给全部一给默认，已在 `capabilities.yaml` 登记为 `keyless_read_routes_do_not_scope_by_account`。
- **R9-D 修策略冷启动分支忽略 `python_timeout_ms`**：`strategy_host.rs:613` 的冷启动 helper 直接传写死的 `PYTHON_STRATEGY_TIMEOUT_MS = 2000`，而配置侧 `strategy_validation.rs:218-219` 允许 `python_timeout_ms ∈ [1, 60000]`。预热分支（`workers.rs:131/149`、`strategy_contract.rs:45/60`）都读配置，只有冷启动那条读死值——用户配 30s 在首轮被 2s 掐死。已把超时改为参数并接上 `config.strategy.python_timeout_ms`；那份常量只剩测试在用，改标 `#[cfg(test)]`，避免"常量从未使用"的告警（`cargo clippy -D warnings` 会红）。同轮修 `live_check.rs` 的本机回环判定：字符串前缀 `"127."` 覆盖不到 `[::1]:port`，改成 `SocketAddr::parse().is_ok_and(|b| b.ip().is_loopback())`，与 `topology_validation.rs:54` 的 `api_bind.ip().is_loopback()` 同口径，`localhost` 单列保留。
- **R9-E 类型面孤儿**：门禁三层零读者判据（`pub fn`/`pub const`/`pub(crate) fn`）**完全不索引类型**。独立脚本扫全仓 467 个 `pub struct`/`enum`/`type`：433 个有生产读者，`materialize` 家族的 5 个类型全部只被 `#[cfg(test)]` 消费（`materializer.rs` 的 `#[cfg(test)] mod tests` 在第 287 行，全部调用点在其后），与 R4-C 的 #119 断链登记一致。唯一真孤儿是 `qx-data::DataSchemaVersion`（结构体 + `impl Default`，全仓零引用；同文件 `DATA_SCHEMA_VERSION` 常量被 `provider.rs:25` 的 `BAR_FRAME_SCHEMA_VERSION` 使用，保留），已删除并同步 `lib.rs` 的 `pub use`。`InMemoryProvider`/`MemoryDataStorage`/`TcpHttpTransport` 三个测试专用类型按名字补登（`capabilities.yaml`）。`FactorEvaluator` trait 的两个实现者（`MomentumEvaluator`、`BadEvaluator`）都在 `#[cfg(test)]` 之后，属 #119 断链家族，不动。
- **R9-F 工业级差距复核（结论不变：未达工业级）**：R9-E 重扫终止性——11 处新增 `loop`/`while` 全部有界或属设计意图的常驻 supervisor（`shutdown.rs:65`、`supervisor_stop.rs:39` 等停机信号是本职）；无跨 crate static Mutex、无嵌套锁、无无超时 `Condvar::wait`；Python 三条 serve 路径都有父进程探测或 EOF 出口。14 处 `== "production"` 判定全部一致（都用 `eq_ignore_ascii_case`，且 environment 已由 `ENVIRONMENT_VOCAB` 收口），`scheduler.rs:102` 的 `dry_run` 判定是另一条独立轴不冲突。**0 项会导致进程永挂或真实资源泄漏**。硬缺口仍是 P0-1 sandbox evidence（`capabilities.yaml` 里 `sandbox_tested: true` 与 `production_approved: true` 各命中 **0** 次）、P0-2 文件后端事务语义、P0-3 C ABI 信任根——本轮**没有**新动 P0 项，全部维持「未排期」。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 621 / [FAIL] 0**（`GATE_CHECK_FLOOR` 619 → 621，新增 `audit_sync_control_prefix_check` 与 `websocket_refused_param_route_check` 两颗）；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0 / **1058 passed / 0 failed / 1 ignored**（R7 基线 1057 + 本轮 1 颗回归用例）；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**；`python tools/validate_core.py` 全部自校验通过。行数棘轮按 `--snapshot` 重登记。
- **发布件按终树重打**：本轮改动横跨 `qx-api` / `qx-cli` / `qx-storage` / `qx-data` / `tools` / `maturity` / 文档六个面，`qx-cli.exe` 与 wheel 里的 `.pyd` 都因此过期。`cargo build --release` 得 `qx-cli.exe` **12,557,824 字节**，`crates/**/*.rs` 比它新的 **0** 份；`tools/build_python_wheel.ps1` 得 **219,449 字节 / 17 条目**，sha256 `c36d25b700b049aa38c8de73109eb65fd6ca7c349005b28795f624f19d8650ec`，内嵌 `.pyd` md5 `55ced3bad29541d5cc1a89f5ce4805ed` **≡** `target/release/_qianxing_native.dll` md5，**12 份 `.py` 与仓库逐字节不一致 0 份**；release `qx-cli` 三发 `all` / `ecosystem` / `runtime-check deploy\qianxing.runtime.example.json` 各 rc 0；把这份 wheel 装进**另一个**干净 py3.12 venv（`--no-deps --no-index`），`qianxing_bridge`/`qianxing_strategy`/`qianxing_ashare`/`qianxing_ccxt` 四个包 import 全 OK、`native.available() -> True`。



### V13 R8 收口（2026-10-04）· 第 11 轮三扫 · 零读者判据第三层盲区、补单闸门漏档、超时误报、控制面口径

- **R8-A 补上零读者判据的第三层盲区**：`PUB_SURFACE_DEF` 只认 `pub fn` / `pub const`，全仓 **500 个 `pub(crate) fn` 整层逃过**——本轮之前没有任何一轮数过它们。新增 `zero_reference_pub_crate_surface_check`（含元判据共 3 颗，**616 → 619**）。`pub(crate)` 天生只在本 crate 可见，所以读者**按本 crate 计数是构造性正确的**，不需要像 `pub fn` 那样为跨 crate 的假活口付代价值得。那种匹配正是 `qx-storage::after` 活到本轮的原因：`AuditFileStore::after` 自己零读者（唯一调用点是 `#[cfg(test)]` 里的 `assert_eq!(store.after(0)…)`），却被 `qx-api` 事件游标里 **26 处**无关的 `after` 参数保活——`crates/qx-api/src/admission.rs:395` 的 `["account_id","venue_id","after"]`、`event_cursor.rs` 的参数与局部变量、`lib.rs` 十余处 `let after = match parse_after_cursor(query)`、`ws.rs` 与 `read_after(&self, after)`。`_production_lines` 其实**已经**剔掉注释行（`return [line for line in _strip_cfg_test_items(text) if not line.lstrip().startswith("//")]`），所以之前怀疑的「英文注释给 `after` 加读者」是错的——真正的原因跨了 crate。同轮把四个 AuditStore 镜像方法删掉：`AuditFileStore::root`（全仓零引用）、`AuditFileStore::after`（测试断言改用 `read()` + `filter(entry.sequence > 0)` 表达同一件事）、`SqliteAuditStore::path`（全仓零引用，`SqliteAuditStore` 只有 `::new` 与 `.sync_control` 两个生产入口）、`PostgresAuditStore::after`。**保留 `append` / `read`**：`read` 被 `append_unlocked`（`lib.rs:1451`）与 `sync_control`（`lib.rs:1473`、`postgres.rs:1324`、`sqlite.rs:1442`）在生产路径**内部**调用，`append` 是 `PostgresAuditStore::sync_control` 的生产入口（`postgres.rs:1338`），两者都不是孤儿——上一轮把「仅测试读者」误写成「零读者」，这一轮按源码内部调用点重判。唯一的新孤儿是 `qx-cli::for_test`：`#[cfg(all(test, feature = "nats"))]` 的复合 cfg 条件 `_strip_cfg_test_items` 剥离不掉（它只认恰好等于 `#[cfg(test)]` 的标记行），于是这个 test+nats 门控构造器看起来像一条生产定义；`EventConsumerHandler` 的三个字段是本模块私有，`tests/` 作为兄弟模块拿不到写权限，只能由定义模块开这个入口，已带理由登记。
- **R8-B 修 `submit()` 补单闸门漏了一档**：`crates/qx-execution/src/lib.rs:428` 的 `submit()` 对已存在的同 `client_id` 订单只挡两类——`is_terminal()` 或 `Accepted|Working|PartiallyFilled|Filled`（已确认，回 `ALREADY_APPLIED`）与 `Submitted|Unknown`（已出网但未确认，转 `ReconcileRequired` 并 Err）。`OrderStatus` 共 11 个变体，**`CancelPending` 掉出两个分支后直接落到 `venue.submit_order`**。而迁移表（`qx-core/src/order.rs:18-79`）里 `CancelPending` 只能走到 `Cancelled|Filled|Unknown`，`Submitted` **非法**——所以对一条撤单在途的订单补发等于用同一个 `client_order_id` 再下一单，这正是闸门本该挡住的那类事故。已并入 reconcile 闸门并改写错误信息（带状态名，原来那句「已离开本地但未有 Accepted 事实」对 `CancelPending` 并不成立）。**`PendingSubmit` 刻意放行**：已核对 `mark_reconcile`（`lib.rs:600`）只 append 一条 `ReconcileRequired` 事件、不写 status；而 `submit()` 里 `venue.submit_order` 的 Err 分支（`lib.rs:494-499`）经 `mark_reconcile` 转 `Unknown`、空回包（`lib.rs:489-492`）也 `mark_reconcile`，所以订单会停在 `PendingSubmit` 当且仅当**远端从未收到**（注册后、出网前失败，或跨重启恢复），补发是恢复动作而非重复下单。审计里那条「PendingSubmit 二次调用会重复记账」的结论不成立（`Accepted` 属 `semantic_replay`，EventLog 按 `correlation_id` 去重），但 `CancelPending` 那半是真的，已修。
- **R8-C 修策略宿主共享输入 ring 的超时误报**：实现现位于 `crates/qx-orchestrator/src/strategy_worker.rs:161` 的输入写入循环——`Ok` break、`Err(Full) if Instant::now() < deadline` 睡 1ms、`Full` 超时单独报错。到点仍是 `Full` 时，旧实现落入 catch-all，运维读到的错误是「ring 满/写入失败」，方向被引向共享内存实现本身；实际是 worker 停止消费输入——与紧接着的输出侧 `Err(Empty)`（`strategy_worker.rs:181` 起，报「worker 响应超时」并 `child.kill()` + `death_note`）**是同一种死法**。已改报「worker 输入写入超时 timeout_ms=…：共享输入 ring 满，worker 已停止消费」，并照输出侧补上 `child.kill()` 与 `death_note` 诊断，避免留下不再消费输入的 worker 让下一次请求再卡满整个预算。
- **R8-D 更正两处对控制面审计的失实口径**：`/metrics` 的 `qx_control_retired_*` 与 `/control/audit` 读的都是 `ApiState.control` 这份**进程内副本**——装配期从持久化控制面载入（`api_service.rs:22`），之后只有本进程 `submit_command` 走 `store.transact` 成功时才回写（`lib.rs:1800`）。worker 进程经 `control_store.transact(execute)` 落盘的命令终态与随之发生的退场**不回流**，所以在本进程下一次 `submit_command` 之前这两条计数都是**滞后上界而非实时值**。原注释写「这两条计数因此挂在每次 `/metrics` 的现读上」，`QueryPort` 那段也只说「`/control/audit` 走 `control_audit()`」——两处都已按实改写，实时化归入 P0 的 QueryService 拆分那层（登记项，本轮不动）。`/control/audit` 与它的三个兄弟端点（`/scheduler/runs`、`/account/ledger`、`/reconcile/reports`）错误口径不齐那半**未修**：`control_audit()` 直读内存不可能失败，要加 503 就必须改从 store 现读，那正是上一条的同一个改造，本轮不做半截。
- **R8-E 工业级差距复核（结论不变：未达工业级）**：硬缺口仍是 P0-1 sandbox evidence（全仓唯一一份 testnet 证据 `outcome:"skipped"`、`credentials_present:false`、`send_orders:false`；`capabilities.yaml` 里 `sandbox_tested: true` 与 `production_approved: true` 各命中 **0** 次）、P0-2 文件后端事务语义（EventLog/Outbox/队列在文件后端非单事务，`append_batch|BEGIN|COMMIT` 在 `crates/qx-storage/src` 只命中 sqlite.rs 两处且都不是这三条链）、P0-3 C ABI 信任根（`c_api.rs:645-865` 有 sha256 + 可选 Ed25519 `load_verified`，但 `ed25519_public_key` 是部署配置字段而非锚定的信任根）。`data_binding.rs` 整模块未接线那一条经核对**已在盘上登记**（允许清单 #53 + 计划 P1-7），本轮不删不接。
- **发布件按终树重打**：本轮改动横跨 `qx-storage` / `qx-execution` / `qx-cli` / `qx-api` 四个 crate，`qx-cli.exe` 与 wheel 里的 `.pyd` 都因此过期。`cargo build --release` 得 `qx-cli.exe` **12,555,264 字节**，`crates/**/*.rs` 比它新的 **0** 份；`tools/build_python_wheel.ps1` 得 **219,447 字节 / 17 条目**，sha256 `b299ecfe…`，内嵌 `.pyd` md5 `8618a4b524df2c4d899fad0fe2ffc459` **≡** `target/release/_qianxing_native.dll` md5，**12 份 `.py` 与仓库逐字节不一致 0 份**；release `qx-cli` 三发 `all` / `ecosystem` / `runtime-check deploy\qianxing.runtime.example.json` 各 rc 0；把这份 wheel 装进**另一个**干净临时 venv（`--no-deps --no-index --find-links dist`），`qianxing_bridge`/`qianxing_strategy`/`qianxing_ashare`/`qianxing_ccxt` 四个包 import 全 OK、`native.available() -> True`、`normalize_instrument('sz.000001') -> '000001.SZSE'`。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 619 / [FAIL] 0**；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0 / 87 段 / **1057 passed / 0 failed / 1 ignored**（与 R7 基线同数）；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**；`python tools/validate_core.py` 全部自校验通过。行数棘轮按 `--snapshot` 重登记。R8-A 删除 4 个方法净减约 25 行生产代码，门禁判据与注释净增约 90 行。


### V13 R7 收口（2026-10-04）· 第 10 轮三扫 · R6-A 的语义漏洞、零读者判据盲区里的三处零多态消费者



- **R7-A 修掉 R6-A 自己留下的语义漏洞**：R6-A 把两条投影失败都改成 `pipelines.remove(&pipeline_key)` + `continue`，注释写「不能留着继续轮询每 250ms 重复同一行错误日志」。但外层是 `while !stop { for in sources { … } thread::sleep(250ms) }`——下一轮 `while` 到这个 key，`pipelines.entry(...)` 是 Vacant，于是 `storage.open_read_only` 重开同一本账本 → `refresh()` → 再次投给 API。`project_event_log` 的失败是内容漂移或序号缺口，**不会自愈**，所以同一行 `eprintln!` 每 250 ms 复发一次，正好是我自己注释里说不会发生的那件事。新增 `permanently_retired: BTreeSet<(String,String)>`：两条投影失败各记一次、循环顶部跳过，退场成为终局。**`refresh()` 失败那一路刻意不入名单**——读盘失败可能是瞬时 IO，下一轮重开重试才正确，而投影失败是账本内容本身冲突，重试只会无限刷屏。顺带把残余的分叉窗口说明白：按账户投影成功、全局投影失败那一支，`state.projections[key]` 已推进一格而 `state.events` 停在原地，两个读面就此冻结在该点并由那行错误日志留证，不再继续发散。锁顺序复核过：`project_event_log` 与 `project_account_event_log` 各自只做一次 `self.state.lock()`，`ApiState` 内无二级锁，无死锁风险。**门禁新增第 616 颗判据**（615 → 616）把这三点钉住：永久退场名单定义恰 1 处、入名单点恰 2 处（两条投影失败各一）、循环跳过点恰 1 处，并按源码偏移量守「按账户投影调用点先于全局投影调用点」（R6-A 的提交顺序本身）。这颗判据补的正是它自己暴露的那个问题——R6-A 的注释与代码矛盾这件事，靠读代码才看得见。
- **R7-B 清掉零读者判据盲区里的三处零多态消费者**：这一轮查的是 `PUB_SURFACE_DEF` 的盲区——它只数 `pub fn` / `pub const`，**不认 trait 定义与 trait 方法**，所以这三处恒绿，前九轮每轮都绿过：①`qx-zhenlu` 的 `VenueAdapter` trait（`capabilities()` 返回 `ConnectorCapabilities`，全仓 `\.capabilities\(\)` 命中 **0** 条，连测试都没有；`health()` 返回 `AdapterHealth`，仅 `binance.rs` 三个 `#[cfg(test)]` 调用点）——trait 与只由它读的两个类型 `ConnectorCapabilities`/`AdapterHealth` 一并删除（后者若留下，删掉方法后就成了无读者的 `pub struct`），`capabilities()` 直接删、`health()` 的三个测试调用点改成直接读同模块的私有字段 `state` / `last_event_ts`。②`qx-execution` 的 `ReconcilePort` trait——全仓无 `dyn ReconcilePort`、无 `where T: ReconcilePort` 约束，三个生产调用点（`binance_reconcile.rs`、`ccxt_execution.rs`、`ccxt_reconcile_worker.rs`）都拿具体类型 `EventLogReconcilePort` 直接调，`reconcile_port_contract.rs` 那份「契约测试」也是直接调具体类型。删 trait，`require_reconcile` 改成该类型的固有 **`pub`** 方法——原来方法本身没有 `pub`，靠 `use ... ::ReconcilePort` 把 trait 拉进作用域才可见，trait 一删必须显式补 `pub`，否则三个跨 crate 调用点直接 `E0624`。三处 `ReconcilePort` 的 doc 引用（`lib.rs`、`reconcile_port_contract.rs` 文件头、`qx-genglu` 的 `VerdictAction` 注释）同轮改口。
- **R7-B 的裁定边界**：`JobQueueBackend` 按同一把尺子**保留**。它的五个 trait 方法（`enqueue_job`/`available_jobs`/`claim_job`/`ack_job_at`/`recover_expired_leases`）在生产链零调用者——生产队列面是 `ConfiguredJobQueue` 枚举，按 `match` 臂调各后端类型的**固有**方法（`enqueue`/`available`/`claim`/`ack_at`/`recover_expired`），两套接口并行存在、连名字都不同。但它唯一的多态消费者是 `crates/qx-storage/tests/queue_backend_semantics.rs` 的 `assert_job_queue_semantics(queue: &dyn JobQueueBackend)`，靠**同一份**断言代码逐个跑 file / SQLite / Postgres 三个后端——这是后端一致性 harness，不是死面，删掉等于删证据。改为在 `storage_consistency_contract.limitations` 登记为 `job_queue_backend_trait_methods_have_no_production_caller`，写明收敛方向（先让两套接口收敛成一份，或先定 `ConfiguredJobQueue` 为什么必须持有构造器而不是裸 trait 对象）。同一轮补 `AuditFileStore::after()` 这类「同名兄弟遮蔽」的失配：门禁按名字找读者，`PostgresAuditStore::after` 有生产读者就把 `AuditFileStore::after`（仅 `lib.rs:2070` 一处用例读者）判成活面——与 `latest_quote` / `query_command` 那次是同一个盲区家族。
- **R7-C 能力矩阵与家族登记同步**：fam02「API 读模型」加续修注——它名下的 6 格里今天已有 4 格在盘（**事件按账户键**由 R5-A 接通、**快照历史有界** `MAX_SNAPSHOT_HISTORY = 1_024`、**控制面现读** `submit_command` 整体换 `state.control`、**停机令牌与连接上界** `session_shutdown` / `connection_budget`），唯一未落的是 QueryService 拆分那层「API 自己那份查询读模型 `EventLog` 无淘汰水位」，R4 那段的技术理由不变（把窗口外的旧事件剪掉会让一次合法的重叠投影查不到而被报成内容漂移）。R6-A2 的第三本台账登记、R6-B 的六处公共面删除也一并补进对应条目。
- **R7-D 口径收口**：终版文档 §1.1 门禁行与 §1.3 标题从 615/六轮改到 616/七轮，§8 一句话结论补第 10 轮段落并把「九轮」改「十轮」。
- **发布件按终树重打**：本轮改动横跨 `qx-cli` / `qx-api` / `qx-storage` / `qx-execution` / `qx-zhenlu` / `qx-adapter` / `qx-genglu` 七个 crate，`qx-cli.exe` 与 wheel 里的 `.pyd` 都因此过期。`cargo build --release` 得 `qx-cli.exe` **12,582,400 字节**，`Get-ChildItem crates -Recurse -Filter '*.rs' | Where-Object LastWriteTime -gt exe` 得 **0** 份；`tools/build_python_wheel.ps1` 得 **219,447 字节 / 17 条目**，sha256 `5ed022fe…`，内嵌 `.pyd` md5 `497dcb08bd04e84cbb6154a5b9380e85` **≡** `target/release/_qianxing_native.dll` md5，**12 份 `.py` 与仓库逐字节不一致 0 份**；release `qx-cli` 三发 `all` / `ecosystem` / `runtime-check deploy\qianxing.runtime.example.json` 各 rc 0；把这份 wheel 装进**另一个**干净临时 venv（`--offline --no-deps --no-index`），`qianxing_bridge`/`qianxing_strategy`/`qianxing_ashare`/`qianxing_ccxt` 四个包 import 全 OK、`native.available() -> True`、`normalize_instrument('sz.000001') -> '000001.SZSE'`。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 616 / [FAIL] 0**；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0（**87 段 / 1057 passed / 0 failed / 1 ignored**）；`python -m unittest discover -s python/tests -q` **64 passed / OK**；行数棘轮按 `--snapshot` 重登记。R7-B 的三处删除净减约 40 行生产代码，R7-A 净增约 25 行（含永久退场名单与门禁判据）。**发布件因此过期**，需按同一套流程重打 `qx-cli.exe` 与 wheel。


### V13 R6 收口（2026-10-04）· 第 6 轮三扫 · R5 自身缺陷、第三本去重台账、六处零读者公共面

- **R6-A 收口 R5-A 自己的提交顺序缺陷**：`spawn_api_projection_bridge` 里 `project_account_event_log` 与 `project_event_log` 各自在内部加锁改状态、返回值只是结果，原写法是**两条都提交完才开始看错误**。于是默认账户的按账户投影失败（`project_event_log` 的内容漂移 / 序号缺口这类不会自愈的冲突）时，全局那格已经被写过——同一个账户的 `/events`（读 `state.events`）与 `/events?account_id=&venue_id=`（读 `projections[key].events`）从此各讲一份，而这恰是 R5-A 的 CHANGELOG 声称已经关掉的那个形状。改成先提交并当场判定按账户那条、成功后才写全局那格。连带修掉我自己在这轮引入的第二个问题：全局投影失败原先只 `eprintln!` 不摘管道、不 `continue`，而 `project_event_log` 的失败不会自愈，于是同一条错误每 250 ms 复刷一次（约 4 行/秒），且 `/ready` 的 `projection_readiness()` 只扫 `state.projections`、永远看不见全局那格，进程照报 200。现在两条失败都摘管道退场。
- **R6-A2 补上第三本只增不减的成交幂等台账**：R5-B 的正则 `seen_(?:fill_keys|trade_ids)\.` 不认 `seen_fills`，而且判据只读 `BINANCE_VENUE_FILE`/`CCXT_PUMP_FILE` 两个文件——`crates/qx-runtime/src/pipeline.rs` 的 `LiveEventPipeline.seen_fills: BTreeSet<(u64,u64,i128,i128,i128,String)>`（键 `(order_id, ts, qty, price, fee, venue_order_id)`，比 Binance 那本多一栏 venue 订单号）整本不在范围内。它只增：唯一读点 `.contains`、三处 `insert`（两条 ingest 路径各一、`rebuild_runtime_indexes` 重启时把整本 EventLog 的 `Filled` 全插回来一处）、全词无 `remove`/`retain`/`drain`/`pop`/`clear`/`truncate`；上界是进程一生 ingest 过的成交数，`LiveEventPipeline` 还是 `#[derive(Clone)]`，克隆一次多复制一整本。**淘汰是否安全：不安全**——它挡住 user stream / reconcile / Outbox 补投影三条路径重投同一笔 fill，去掉任何一条已见键同一笔成交就被 `oms.apply_fill` 记两次账，与 R5 注释写的同一个失败方式。所以该登记 + 扩门禁，不是加水位：正则扩成 `seen_(?:fill_keys|trade_ids|fills)\.`，pipeline.rs 纳入同一颗判据（Binance 1 处 `insert` / CCXT 1 处 `entry` / 运行期 3 处 `insert`），三处字段注释都写明「只增不减的成交幂等台账」，capabilities.yaml 的 `binance_direct.limitations` 从两本改三本。门禁条数不变（615）。
- **R6-B 清掉六处零调用者的公共面**：①`JobQueueBackend::ack_job` 及三个实现（sqlite / postgres / file-jobs）——被带 fencing token 的 `ack_job_at` 取代，全仓 `.ack_job(` 命中 0 条，留着会让读者以为无 token 的 ack 仍在契约里；`ack_at` 那条在用（`tests/queue_backend_semantics.rs` 108/115/119），且 `FileJobQueue::ack` 固有方法仍有用例读者（`lib.rs` 2061 附近的接管测试）。②`AuditStore` 整个 trait 与三份实现——四个方法（`append_record`/`read_entries`/`query_command_entries`/`entries_after`）零调用者，全仓无 `dyn AuditStore`、无 `impl Trait: AuditStore`，生产读审计走固有方法 `AuditFileStore::append` / `append_unlocked`；那份测试改用 `read()` + 内联 filter。③`MarketDataPort` 与 `LiveEventPipeline` 的实现——无 `dyn MarketDataPort`、无 `latest_quote(&dyn)` 调用点，生产用的是同名固有方法 `latest_quote_with_depth`（`spread.rs`、`venue_runtime/paper_submit.rs` 在读）。④`ControlPort` 与 `ApiService` 的实现——无 `dyn ControlPort`、也没有 `control_port()` 访问器（对比 `query_port()` 定义在用），与 `QueryPort` 那条在用路径不对称。⑤`QueryPort::events_after` 与实现——全仓 `events_after` 的其余命中全是另一个函数 `events_after_cursor`（`event_cursor.rs`，`/events`、`/events/live` 走它），trait 方法零调用者。⑥`DataStorage::load_range` 与 17 行默认实现——唯一读点是自己那个测试，用例改成只断言正序。
- **R6-B 的连带孤儿**：删完立刻被零读者判据反查出两条**新**孤儿——`qx-runtime::latest_quote`（唯一读者是刚删掉的 `MarketDataPort::latest_quote` 实现，测试 `paper_bridge_and_bundles.rs` 的那一处断言与紧随其后的 `latest_quote_with_depth` 断言重复，改成对 depth 版补 `ts`/`bid`/`ask` 三个断言）与 `qx-storage::query_command`（文件版与 Postgres 版，唯一读者是刚删掉的 `AuditStore::query_command_entries`）。门禁条数不变（615），但「删了公共面又长出零读者」这条链路被判据当场拦下来，这正是判据该做的事。
- **R6-C 更正一条不存在的节奏**：`crates/qx-api/src/snapshot_history.rs` 头注释原先写「读模型每 250 ms 重装一次账户快照」，于是 1_024 那份数被描述成「约 4 分钟的基线可回看窗口」。事实是投影桥只喂事件读模型（`project_event_log`/`project_account_event_log` 只写 `events`+`event_bus`，**不碰** `snapshot`/`snapshot_history`），快照只在 `build_configured_api_service` 装配时按域装载一次（全仓 `publish_snapshot` 的生产命中只有 `api_service.rs` 两处 boot 调用）。按真实契约改写：首次加载走快照、增量走事件、读侧自己拿 `/events` 做 reducer；1_024 从「4 分钟窗口」更正为内存天花板，窗口实际多长取决于本进程换过几次摘要。
- **R6-D 口径收口**：终版文档 §8 摘要停在 614 且把「第 4/5/6 轮」和「R4」两套编号混着数，§1.1 与 §1.3 已经是 615；统一到 615 与 R1…R6 一套编号，并补上 R6 的四颗。同轮更正 §1.3「三轮共同结论」里「ccxt/binance insert-only map 无界」仍在待办那格的旧说法（R5/R6-A 已补牙齿），以及投影桥那段注释只提 `project_event_log` 漏了 `project_account_event_log`。
- **发布件按终树重打**：本轮改动横跨 `qx-api` / `qx-storage` / `qx-runtime` / `qx-data` / `qx-execution` 五个 crate，`qx-cli.exe` 与 wheel 里的 `.pyd` 都因此过期。`cargo build --release` 得 `qx-cli.exe` 12,581,376 字节，`Get-ChildItem crates -Recurse -Filter '*.rs' | Where-Object LastWriteTime -gt exe` 得 **0** 份；`tools/build_python_wheel.ps1` 得 **219,448 字节 / 17 条目**，sha256 `79fac3b2…`，内嵌 `.pyd` md5 `1e2f3ab8611ee6289c8cbb212a435536` **≡** `target/release/_qianxing_native.dll` md5，**12 份 `.py` 与仓库逐字节不一致 0 份**；release `qx-cli` 三发 `all` / `ecosystem` / `runtime-check deploy\qianxing.runtime.example.json` 各 rc 0；把这份 wheel 装进**另一个**干净临时 venv（`--offline --no-deps --no-index`，补 `tzdata`），`qianxing_bridge`/`qianxing_strategy`/`qianxing_ashare`/`qianxing_ccxt` 四个包 import 全 OK、`native.available() -> True`、`normalize_instrument('sz.000001') -> '000001.SZSE'`。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 615 / [FAIL] 0**（含扩到三本台账的那颗「成交幂等台账只增不减」）；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets --no-fail-fast` rc 0 / 87 段 / **1057 passed / 0 failed / 1 ignored**；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**。行数棘轮按 `--snapshot` 重登记（`pipeline.rs` 2332→2338 注释、`snapshot_history.rs` 注释、四处 orphan 删除使 4 个文件下探）。行数变化为增的为**注释与判据正则**，删除均为零读者公共面，无一改动生产语义。尚未 `git add`、未推送。


### V13 R4/R5 收口（2026-10-04）· 第 4、5 轮三扫 · 断链与门禁盲区收口 · 发布件按终树重打

- **R4-A「无预算等待」族第三处**：`crates/qx-adapter/src/ccxt.rs` 的 `CcxtProcessClient::call` 原先直接 `self.stdin.write_all(payload)` 再等读侧——一旦 CCXT worker 存活但不再从 stdin 取字节，超出 OS 匿名管道缓冲（~64KB）的那段写入永久阻塞，`timeout_ms` 只守读侧，本进程就此卡死且连关停都读不到。修法与 #281（策略宿主）、#282（event consumer handler）同族：`stdin` 降为 `Option<ChildStdin>`，写 `write_all`+`flush` 交给 spawned 线程，主路径按 `write_done.recv_timeout(Duration::from_millis(self.timeout_ms))` 收口，超时即杀 worker 并回「CCXT Worker 输入超时」诊断。新增门禁 `ccxt_pipe_write_budget_check` 三条（写入被交给线程且有界等待、写侧预算取自 `self.timeout_ms`、超时诊断与句柄回收在位）。
- **R4-C 两处门禁盲区**：`PUB_SURFACE_DEF` 旧式 `pub (?:async )?(?:fn|const) (\w+)` 在 `pub const fn` 上把捕获组落到 `fn` 本身——全仓 23 处 `pub const fn` 因此全部映射成 `crate::fn`，而 `fn` 一词到处都是，「零读者」永远数不出来；`pub use` 重导出行曾被当成生产读者，而那行只是把名字摆到 crate 根、没有任何调用。两处修好后立刻放出 `qx-data::load_bar_batch`（批量装载逐条校验 provider 不得返回越界行，能力在盘但生产走 `ingest_bars`）与 `qx-core::default_maker_taker`（生产直接读 `DEFAULT_MAKER_BP`/`DEFAULT_TAKER_BP` 装配，只有用例在调构造器），各带理由登记进允许清单；新增一颗元判据把两处盲区钉住。同时复核 479 个 `pub struct/enum/trait/type` 与 82 条 `pub use` 重导出，除 `load_bar_batch` 外全部有生产引用，允许清单既有 27 条零读者登记无一长出读者。
- **R5-A 接通无键事件读面这条断链**：`GET /events`、`GET /events/live`、不带 `account_id`/`venue_id` 的 WebSocket 三条读面读的是那格全局兼容投影，而 `spawn_api_projection_bridge` 只喂按账户投影，于是生产上这三条读面恒为空数组。修法与快照那条判据同源：桥按 `default_account_event_log` 认出默认账户，把它的那本 EventLog 同时投影到全局兼容读模型，其余账户仍只进自己的投影，多账户部署下两个读面不会各讲一个账户。投影桥函数原有的文档注释（「调用 API 的 `project_event_log`」）此前与代码不符，本次一并落到真实调用。
- **R5-B 给成交幂等台账补牙齿**：Binance `seen_fill_keys` 与 CCXT `seen_trade_ids` 是**只增不减**的去重台账——user stream 与 `trades` 查询都会把同一笔成交重投，淘汰任何一条已见键就等于允许同一笔 fill 被 trace 两次（重复记账），这是本仓库唯一不接受的失败方式。它们刻意不设水位，所以新判据守的是「只增」而不是「有界」：入账点各恰一处、两个字段全词无 `remove`/`retain`/`drain`/`pop`/`clear`/`truncate`、两处字段注释都写明「只增不减的成交幂等台账」，并登记进 capabilities.yaml 的 `binance_direct.limitations`。同期把 `QueryPort` 的注释改诚实：它原先谎称「网络协议只依赖这些只读方法」，而 `/account/orders|positions|balances` 三个端点实际先走 `snapshot_for_query` 做身份解析再取分表，从不走 `account_orders()` 那三个**无身份的默认账户视图**——那三个方法只有用例读者，接到带身份的端点会在多账户部署下静默串账，因此注释写明它们需要身份感知变体才能上生产。
- **门禁 610 → 615（净增五颗）**：R4-A 三颗、R4-C 一颗元判据、R5-B 一颗。行数棘轮按 `--snapshot` 逃生门重登记（本轮涨的是 `qx-adapter/src/binance.rs` 2121→2127、`qx-adapter/src/ccxt.rs` 1174→1180、`crates/qx-api/src/lib.rs` 2979→2990，均为注释与写侧预算）。
- **发布件按终树重打（台账 #251，本轮落地）**：此前 `target/release/qx-cli.exe`（18:40）早于 R4-A 的 `contract.rs`（19:44）与本轮 R5 的 `market_bridges.rs`（22:08）、`binance.rs`/`ccxt.rs`（22:16），而 `dist/…whl`（18:22）里嵌的 `qianxing_bridge/strategy.py` 只有 14504 字节、仓库 15344 字节——**发布物仍是改前那份代码**。本轮：`cargo build --release` 后 `qx-cli.exe` 12,581,376 字节，`Get-ChildItem crates -Recurse -Filter '*.rs' | Where-Object LastWriteTime -gt exe` 得 **0** 份；`python\.venv` 不存在，按 `uv venv python\.venv --python 3.12 --clear` + `uv pip install pip tzdata` 补齐前置后跑 `tools/build_python_wheel.ps1`，得 **219,448 字节 / 17 条目**，`.pyd` md5 `dc8b48c61576334274ff1ba50811f92a` **≡** `target/release/_qianxing_native.dll` md5，**12 份 `.py` 与仓库逐字节不一致 0 份**；再把这份 wheel 装进**另一个**干净临时 venv（`--offline --no-deps --no-index`，补 `tzdata`），`qianxing_bridge`/`qianxing_strategy`/`qianxing_ashare`/`qianxing_ccxt` 四个包 import 全 OK、`native.available() -> True`、`normalize_instrument('sz.000001') -> '000001.SZSE'`。
- **本轮实测（读数全部由日志派生）**：门禁 rc 0、**[PASS] 615 / [FAIL] 0**；`cargo fmt --all -- --check` rc 0；`cargo clippy --workspace --all-targets -- -D warnings` rc 0；`cargo test --workspace --all-targets` rc 0 / 87 段 / **1057 passed / 0 failed / 1 ignored**；`python -m unittest discover -s python/tests -q` **Ran 64 / OK**；release `qx-cli` 三发 `all` / `ecosystem` / `runtime-check deploy\qianxing.runtime.example.json` 各 rc 0。尚未 `git add`、未推送。


### V13 R7 收口（2026-10-04）· 第 7 轮三扫 · 台账 #284 · R7-a —— 策略输出契约的六臂折叠诊断把「漏写 schema_version」念成「身份不一致」

- **这颗不是扫出来的，是 R6 那次干净 venv 冒烟撞出来的**：策略作者按 `schemas/strategy_api_v1.md` 返回一个 dict 而漏写 `schema_version`，两侧宿主都回同一句折叠诊断（Rust `StrategyContractOutput 与输入身份、标的或有效期不一致`、Python `strategy output identity or expiry does not match input`），于是他去查 `request_id`——那一格本来就没写错，永远查不出问题。跨语言分叉最常见的长相恰好是「worker 侧 Python 已放行、宿主侧 Rust 才挡下」，而那条链上第一格就是版本号；一句折叠话术把七种成因压成一条查不到的线索。
- **落地（两侧同一口径，拒绝集一字未动）**：`crates/qx-strategy/src/contract.rs:286` 的 `validate_for` 拆成八句各说各话的诊断、首臂点 `schema_version`；`python/qianxing_bridge/strategy.py:290` 同样八句 `ValueError`，空标的走自己那一臂；解码入口 `crates/qx-strategy/src/contract.rs:353` 的 `from_json_for` 与 `python/qianxing_bridge/strategy.py:337` 的 `from_dict` 各过一次 `validate_for`，Python 缺键读 `0` 而不采纳自家默认版本。牙齿按「折叠优先」走：Rust 侧把 `crates/qx-runtime/src/strategy_contract/tests.rs:54` 那颗既有用例扩成六臂点名表并新增一颗解码入口用例，Python 侧折进既有那颗 `python/tests/test_strategy_contract.py:41`。
- **三条诚实话**：「两侧过期极性不一致」这颗**从来没有存在过**（HEAD 两侧都是 `!= 0`），本轮差点把一句注释当缺陷——它现在被钉成不变量，是钉子不是修复；解码入口那次 `validate_for` 在 HEAD 也已在场，同样是钉子；台账里那句「+0 颗 `#[test]`」**半假**——Python 侧真折叠（`Ran 64` 保持），Rust 侧净增一颗（承担用例文件 HEAD 4 颗 / 工作树 5 颗，全仓磁盘在册 1070 → 1071），地板随之 1070 → 1071。
- **门禁 602 → 610（净增八颗运行时判据）**：轮函数 `strategy_output_arm_diagnostics_check()` 两侧都从源码现取诊断句集合，不写第二份手工清单——把某臂抄回折叠句、删一臂、首臂换人、两侧极性式子分叉、解码入口少一次校验、承担用例改名六种动作全部当场红；`tools/check_architecture.py:542` 的 `GATE_CHECK_FLOOR` 注释里那半句 `602 -> 610` 就是本轮。`_python_scope()` 取不到唯一锚点时返回空串让下游判 FAIL（方向对且不 `die`），剥 `#` 注释让「散文既不替代码定罪也不替代码作证」由 C1/C2 两颗对照枪实测。
- **变异取证 14 枪 + 4 枪重放**：主轮档位现读 `TALLY {'fired': 14, 'killed': 12, 'equivalent': 0, 'masked': 0, 'noconclusion': 0, 'control_green': 2, 'control_bad': 0, 'restore_ok': 14, 'collateral': 0}`、`IDENTITY fired=14 tiers_sum=14 equal=True restore_ok=14`，BASELINE 与 FINAL pristine 同为 `PASS=610 FAIL=0 八颗在场=8/8`，四份注入文件按 sha16 逐颗比回（`contract.rs=a55080f30dfe5b25` / `strategy.py=e1a3c0d255d0436f` / `tests.rs=b9e40c31c37fb113` / `check_architecture.py=7793ea3c13e54f43`）；形状枪 S1/S2 各读三颗 FAIL（`PASS=607 FAIL=3`）证明锚点取空的方向是红不是静默通过；因主轮开火后 `tests.rs` 又被本轮自己的 clippy 修复改过字节，G1/G2/G8/G10 四颗一律在终字节上重放击杀（4）。**最有价值的一格是 G7**：门禁红而 `Ran=64` 套件仍绿——「两侧同一个式子」这条判据眼下没有用例替它作保、它是唯一读者，按规矩登记为下一轮的活（给极性分叉补一颗常驻反例）。
- **本轮实测（九腿整跑在终树上，读数全部由日志派生，文档里没有一个手抄数字）**：树摘要九腿前后同为 `142085caa8745541`、rc 全 0、无 host-locked 腿；`fmt --check` rc 0（`Diff in` 0 行）、`clippy --workspace --all-targets -D warnings` rc 0（诊断 0 行，本轮新写的六臂表先被 `type_complexity` 抓过一次）；`cargo test --workspace` rc 0 / 107 段 / 1057 passed / 0 failed / 1 ignored（86 段 Running + 21 段 Doc-tests），`--all-targets` rc 0 / 87 段 / 1057 passed，`--doc` rc 0 / 21 段 / 0 passed；`--list` rc 0 / 1058 行 / 1056 唯一名，恒等式 1057 + 1 = 1058、1071 − 1069 = 1058 − 1056 = 13（`nats` 10 + `postgres` 3）；Python `Ran 64 / OK`（rc 0，本轮这遍 0 skip）；`validate_core.py` rc 0；门禁 rc 0、**[PASS] 610 / [FAIL] 0**。引用普查（落笔后重跑）：活文档 @CENSUS_LIVE_TOTAL@ 条 `path:NN`、@CENSUS_LIVE_DEAD@ 条落空，存档 @CENSUS_ARCHIVE_TOTAL@ 条、@CENSUS_ARCHIVE_DEAD@ 条落空（天花板 9，只降不升）。**Python 的 skip 口径本轮纠正**：此前 README 与终版计划写的 `OK (skipped=2)` 是「没构建 `_qianxing_native.pyd` / 没装 `pyarrow`」那台机器的读数，文档现在写「读数 + 口径」两句而不是把 2 换成 0 了事。
- **发布件重建义务（台账 #251，本轮未做，按依赖图与字面量普查现读定义务）**：`cargo tree -p qx-python -e normal` 不含 `qx-runtime`，而 `qx-cli` 直接依赖它——本轮改动落在 CLI 二进制与 wheel 的 Python 载荷上。wheel 侧 12 份 `.py` 里与仓库逐字节不符的恰好 1 份，正是 `qianxing_bridge/strategy.py`（wheel 内 14504 字节 vs 仓库 15344 字节，折叠句命中 1 次、逐臂句 0 次）——**wheel 内嵌的仍是改前那份代码**；`target/release/qx-cli.exe` 按字面量面已带八句（各 ≥ 1 次、旧折叠字面量 0 次）但字符串普查不担保产物字节；wheel 内 `.pyd` = `f88cd81076318c342659a35b3afb6b9d` 而 `target/release/_qianxing_native.dll` = `1b971292e74a2bbe2fd898b4a2c7d52c`，重打包时「dll → 包内 `.pyd`」那一拷贝步必须一起走。详见 `docs/自研量化框架审计与重构方案-V13.md` §9.54。**尚未 `git add`、未推送**：Rust 一侧、Python 一侧、门禁八颗、capabilities 那一条证据行与五份文档的活面全部仍在未暂存状态。


### V13 R5/R6 收口（2026-10-04）· 第 5、6 轮三扫 · 发布条件收敛

- **R5 落地的四颗**：`/ws` 的升级分支补上浏览器准入（此前 R1-B 那套 CORS 名单 / 预检 403 / 在途预算 503 / 握手 key 只长在 HTTP 那一臂上，跨源网页可带着凭据起一根有状态 WS——CSWSH），跨源·身份·握手 key 三种判定全部排到写 `101` 之前，握手 key 只读一次、缺失回 400 `missing_websocket_key`，`101` 带的 `Sec-WebSocket-Accept` 取自准入算出的那份会话，五条出口带同一份跨源头；`backtest book` 的 `--latency-snapshots` 越过「永远等不到成熟」那条线时从退 0 落一份空成交表变成本场拒绝并点名可用上限 `帧内快照数 − 2`；三条子进程管道逐行读补 16 MiB 行长闸（`read_capped_line` / `read_capped_worker_line`，越界即中止并点名，不截断——半行 JSON 交给解码器是更难归因的失败）；`qx outbox-relay` 三个出口都把 `parked` 念进摘要行，"这一页搬完了"不再被读成"中继干净"。
- **R6 的结论按两半写**：三扫在第 5 轮终树上**没有再发现可修的运行面缺陷（0 颗）**；剩下的是"有意边界"与"文档没跟上代码"两类。四条全局出口（`/health`、`/ready`、`/metrics`、`/schema/account-snapshot-v1`）不判查询串，裁定维持——它们是探针与网关的落点，判键等于给存活探测加一条 400 路径；接口文档现在明写这一支与"12 条读面入口 + WS 那一臂"的覆盖面。独立策略进程那段此前没写行长闸，现补齐两枚 16 MiB 常量与门禁的 1/2/0 调用形状判据。能力矩阵一处漂移重钉（provider 那四格字段挪成独立条目 `provider_capability_registry` 之后，README 六格按现读改成 **23 / 21 / 386 / 357 / 120 / 697**，并把"以仓库内路径开头"的口径点名为八个前缀——漏 `.github` 会把 357 读成 349）。
- **变异取证 16 枪**：现读 `fired=16 killed=14 equivalent=0 noconclusion=0 control_green=2 control_bad=0`，锚点普查 16、`restore=OK` 16（`ws.rs` / `admission.rs` / `lib.rs` 三份逐 sha 比回）。G1~G8 是 R5 的八条判据（其中 G4 是"行为等价、形状不等价"那一类——用例全绿而门禁 `601/1` 红；G5 额外被 clippy 抓住 `field 'accept' is never read`），H1~H6 是读面名册与 WS 查询串臂，C0/C1 两颗对照枪三腿全绿。
- **本轮自己的三处工具缺陷如实记**：取证脚本的三档恒等式漏算对照枪那一档，`14 != 16` 触发 exit 5，改五档后**从同一份日志重导**（没重开枪）；九腿驱动的 `--list` 分类器把 1057 行读成 289（`split(":")[0]` 撞上测试名里的 `::`），改成按行尾 `: test` 收口并同时报行数与唯一名数；本轮第一枪红在 clippy 的 `useless_format`，是本轮自己写的用例被抓，拆成 `&str` 字面量后 8 行全绿。
- **本轮实测（九腿整跑在终树上，读数全部由日志派生）**：树摘要九腿前后同为一个哈希、无 host-locked 腿、rc 全 0；`fmt --check` 干净；`clippy --workspace --all-targets -D warnings` 诊断 0 行；`cargo test --workspace` 107 段 / 1056 passed / 0 failed / 1 ignored，`--all-targets` 87 段同数，`--doc` 21 段 / 0 passed；`--list` 编译出 1057 行 / 1055 个唯一名（唯一那对重名是两份后端契约文件各写一遍的 `user_stream_runner_reconnects_*`）；磁盘在册 `#[test]` 1070、差 13 颗（nats 10 + postgres 3）；Python 腿 Ran 64 / OK；`validate_core.py` rc 0；门禁 rc 0、**[PASS] 602 / [FAIL] 0**，`GATE_CHECK_FLOOR` 不动——两轮 `#[test]` 净增 0、判据净增 0，四颗落地全部折叠进既有用例与既有判据。行数棘轮按 `--snapshot` 逃生门重登记（涨的三格是 `qx-adapter/src/lib.rs` 681→728、`qx-adapter/src/ccxt.rs` 1139→1144、`qx-cli/src/strategy_host.rs` 846→856）。详见 `docs/自研量化框架审计与重构方案-V13.md` §9.52 与 §9.53。尚未 `git add`、未推送。


### V13 R4 收口（2026-10-04）· 第 4 轮三扫 · API 与前后端贯通专项

- **落地的七颗**：CLI 位置参 `outbox-relay … 0` 不再"什么都不搬却退 0 报健康"（`parse_relay_limit` 挡 `1..=10000`，与配置孪生 `relay_batch_size` 同域）；缺 `Sec-WebSocket-Key` 的判定从 `serve_websocket` 提到 `admit_websocket`，握手之前就能回 400 `missing_websocket_key` 而不是掐断套接字；`schemas/strategy_api_v1.md` 那句"Python 侧不带 margin/position/leverage"按现读改口（Python 三格都带，只有 C ABI 头只带 `position_side`）；QIFI 信封在烟测链上第一次真的往返一遍（此前文档替它作保、代码零调用）；`JsonStateEnvelope` 从外销 `pub trait` 收为 `pub(crate)`；策略契约的 `uniqueItems` 折进既有用例拿到运行牙齿；两条 HTTP 读链补 16 MiB 总量界（`set_read_timeout` 只卡单个读窗口，慢速滴水上游可无限续期）。
- **登记的一颗**：API 事件读模型无淘汰（fam02）。本轮把"为什么不落"从裁定升级成技术理由——给 `Vec` 加长度上限写不出来：`project_event_log` 比较重叠前缀，尾部截断会把合法新事件判成「事件内容漂移」。需要的形状是 `retained_from_seq` 水位线，属策略决策。`ApiEventBus::publish` 那一半在 4096 处已淘汰。
- **按实测销案的两颗（假阳性，写明是为了下一轮别再捡）**：Binance `allOrders` 翻页 `loop` 有三条可点出口（页不满 / 游标未前进 / `checked_add` 溢出），人为页数上界会把"还没看完的资金事实"当"已核对"；Postgres 的 `Mutex<Client>` 全文件只有两处 `.lock()` 且都把毒锁映射成错误，锁内受 `statement_timeout=30s / lock_timeout=5s / idle_in_transaction=60s / connect_timeout` 约束。
- **变异取证 3 枪**：2 枪 KILLED、1 枪**无结论**。第一枪想把去重判据拆掉，写成 `insert(..) && false` 触发的是 `error[E0282]`（`BTreeSet` 推不出元素类型），整 crate 编译失败、`test result:` 没印出来——分类器读成"SURVIVED"是错的，那是断管道不是存活；重放成保留类型锚点后 KILLED。第二枪拆掉 HTTP 总量闸：KILLED。
- **本轮实测（10 腿电池，全部读数由日志派生）**：`cargo fmt --check` rc 0；`clippy --workspace --all-targets -D warnings` rc 0、诊断 0 行；`cargo test --workspace` rc 0、107 段 / 1056 passed / 0 failed / 1 ignored；`--all-targets` 87 段 / 1056 passed；`--doc` 21 段 / 0 passed（本仓无 doctest）；`--list` 编译出 1057 条；磁盘在册 `#[test]` 1070 颗、地板 1070，差 13 颗逐名可点；`python -m unittest discover -s python/tests` Ran 64（skipped=2）；门禁 rc 0、[PASS] 602 / [FAIL] 0，**本轮未新增判据**故 `GATE_CHECK_FLOOR` 不动。
- **两处口径随本轮改掉**：README 里「不带 `QX_PYTHON` 时那 2 条 Python 桥用例必红」按代码事实改口——解析是"`QX_PYTHON` 非空才用、否则回落 PATH `python`"，决定红不红的是回落点是不是可用解释器（本轮 `QX_PYTHON` 未设置、PATH 解析到 CPython 3.13.13，两条桥用例照跑照绿）；README 那句「`python/.venv` 本轮它在场」也已按事实改成"第 4 轮这遍它不在盘上，数字是用 PATH 上的解释器跑的"——这条风险第二次落地，正好说明 `build.bat`/`build.sh` 从 `[0/9]` 步探测解释器而不是信任 venv 是对的。
- **第三处口径纠正是本轮自己造的**（B 扫产物，记为 R4-D）：静态「门后」census 是**扫描器形状的读数**，四种形状本轮各现读一遍——三段齐 + 作用域按**花括号深度**收口 = 36 颗（sqlite 23 / nats 10 / postgres 3）；作用域只收到**列 0 的 `}`** = 44 颗（sqlite 31），**过计**（内联 `mod tests { … }` 里被门住那项的右括号落在列 4，门清不掉、漏到同块后面的用例上）；少认「**声明那个模块的行上的门**」那一段 = 30 / 38 颗，**少报**。判据取编译器真相：`36 − 23 = 13` 与「磁盘在册 − `--list` 编译出」同数，sqlite 那 23 颗逐名都在 `--list` 里、nats/postgres 那 13 颗一条都不在。本轮中途把 README 与终版计划 §1.1 写成过计那一格（44 / 31），已改回并把四种形状写进文档。
- **行数预算**：`crates/qx-cli/src/cli_args.rs` 513 行、`crates/qx-adapter/src/lib.rs` 697 行两颗越过 500 阈值，按该文件自订的逃生门 `--snapshot` 重登记，并核对预算名册全部 44 格与磁盘行数同数；`maturity/capabilities.yaml` 与 `crates/qx-storage/src/lib.rs` 的改动都是净零行数。尚未 `git add`、未推送。

### V13 R1 收口（2026-10-04）· 台账 #283 · 合流 `c07ad22` 回退面登记

- **登记（不重落）**：合流 `c07ad22` 按 `-s ours` 保留上游发布线，丢掉了本地 V11/V12/V13-R1 审计线的 135 份文件与 39 颗门禁判据。按用户裁定「以上游发布线为准（现状）」，本节把它们按 15 族逐名登记为**已回退，未排期**，口径不是「已修复」；名册写全在 V13 §9.48，取证目录不进仓库也不影响可读。
- **缺陷驱动落地的三颗**：控制面终态退场 + `RetirementSummary` 两条计数接上 `/metrics`；删掉零构造者的 `CommandStatus::Rejected`，终态词表收到 `is_final()` 一处；给 8 颗没有执行者的 `nats` 门后 `qx-storage` 用例在 CI 里补了一条非 ignore 的腿（本机实跑 8 passed / 1 ignored）。门禁新增 `command_status_vocabulary_single_source_check`，`GATE_CHECK_FLOOR` 515 → 602，`WORKSPACE_TEST_FLOOR` 866 → 1069。
- **`#152` 落地**：新增 `doc_citation_reachability_check`，活文档里每一处 `path:NN` 都要文件在盘上、行号不越界；`docs/archive/` 只降不升。
- **`#144` 关掉**：磁盘在册 `#[test]` 1069 颗 vs `--list` 编译 1056 颗，差 13 颗逐名点出（nats 10 + postgres 3）；README 里那条 `866 − 858 = 8` 的假恒等式删掉。同时纠正本轮自己的一处口径：静态 `#[cfg]` 扫描标出 36 颗「门后」，但 `qx-cli` 的 `default = ["sqlite"]` 让特性归一打开了 `qx-storage/sqlite`，那 23 颗其实照常执行。
- **本轮实测（7 腿电池，digests 稳定、无 host-locked 腿）**：`cargo fmt --check` 干净；`clippy --workspace --all-targets -D warnings` 诊断 0 行；`cargo test --workspace --all-targets` 87 段 / 1055 passed / 0 failed / 1 ignored；`cargo test --workspace` 107 段 / 同样 1055 passed；`python -m unittest discover -s python/tests` 64 条 OK；门禁 602 项全绿。
- **在册未修**：`docs/SECURITY.md` 已回退（全仓现读 35 处命中里 README 占 0 处，所以**没有**悬空指针——本条第一版写的「README 指向它的是悬空指针」是没现读就写下的假断言，已在 V13 §9.48 ② 改口）；新补的那条 NATS 腿只由 ci.yml 的文本存在性保证，门禁里没有判据核它（看守它的那颗在回退面里）；`cap_ledger_reading_check` 在回退面里，README 台账六格本轮按同一配方手工重钉（22 / 20 / 384 / 355 / 114 / 681）但没有牙齿。

### 量化场景探针与 Python/Rust SDK 结果契约修复（2026-10-19）

- 修复 qx-cli 回测装配构造改为 `Result` 后未更新的测试调用，以及 `AccountSnapshot::to_json()` 新错误类型导致的旧断言编译失败；多市场深度 Python outcome 补充可选 `execution_models` 字段，兼容既有无该字段响应。
- 探针改为每份冻结 BarFrame 使用隔离的数据注册表，并对齐当前 CLI 的 `[Strategy · Backtest]` 输出行，避免旧 manifest 冲突和过时正则产生假失败。
- `cargo test --offline -p qx-cli --no-run -j 1`、qx-python 原生扩展 wheel 构建、`cargo fmt --all -- --check` 通过；完整探针 **79 executed / 0 declared_not_executed / 0 missing / 1 blocked**（实盘外部前置）。

### 当前 Release A 股规则吞吐基线（2026-10-19）

- 以当前工作树重建 `target/release/qx-cli.exe`，同一 Ryzen 7 8700G/Windows 11 环境跑 1k/8k/64k 合成分钟 Bar，各 512 条 PIT 现金分红、5 次重复，绑定 A 股交易规则、费用与合成交易日历；P50/P95 分别为 304.459/313.926ms、350.769/353.258ms、616.956/672.753ms。每档重复运行 hash 稳定，单动作控制组 hash 不同。
- 证据 `maturity/evidence/ashare-rules-backtest-release-current-2026-10-19.json`。样本只用于当前终树吞吐基线，不是完整真实 A 股 profile；不改变 `maturity/targets.yaml` 九档 `unmeasured / unavailable` 状态。

### D16 全仓生产 unwrap/expect 清零复核（2026-10-19）

- workspace 全部生产 lib/bin 通过 Clippy `-D clippy::unwrap_used -D clippy::expect_used`；仓内 3 个 `build.rs` 逐文件正则检索无解包调用。该命令与构建脚本扫描已加入 CI，架构门禁同步核验其持续存在；门禁快照更新为 **1,200/1,200**。旧记录的 73 个命中及 qx-core 静态矩阵序列化 `expect` 已过期，当前生产闭集无残余项。D16 收口。

### D21 模块地图英文职能索引（2026-10-19）

- 为 26 个 crate/集成面补充一行英文职能说明，并在架构门禁中核对索引条目完整、唯一且为英文文本；四角色 CLI 指南和双语术语已有行为用例。门禁快照 **1,201/1,201** 通过。Rust 行为测试本轮未运行。

### D10 JetStream Outbox 发布复用稳定消息身份（2026-10-19）

- NATS publisher 将 Outbox `event_id` 作为 JetStream `Nats-Msg-Id`，使 ack 丢失后的 relay 重试能利用 stream 配置的有限去重窗口。EventLog 投影现以稳定 `log_name` 分区并写入源 `seq`；但 EventLog 尚无 append 顺序上的 seq 严格递增承诺，consumer checkpoint 接入及旧分区状态迁移仍未完成，因此不裁剪 `processed_ids`。qx-storage NATS feature 离线编译、全仓格式检查通过，架构门禁 **1,203/1,203**；未运行行为测试或连接 NATS 验收。

### 订单簿衍生品策略保证金快照（2026-10-21）

- OrderBook 回测的策略回调现在接收标记权益扣除已开仓保证金、在途订单保证金和预估费用后的模拟可用保证金；缺市场价时仍返回缺席，缺在途单风险策略时拒绝估算。该值不代表交易所 free-margin 快照。
- 定向复核：订单簿回测单测 24/24，`qx-xingban` 全目标严格 Clippy、估值单源与风险规格架构合同通过；未运行全量架构门禁或发布矩阵。

### Runtime 冻结 market spec 预检（2026-10-21）

- `config validate`/`runtime-check` 对启用 worker 已声明的标的，现在会通过唯一 market-spec 加载器校验文件内容和规格，而不止检查路径存在；进程监督器及独立 Binance、CCXT、Paper Execution/SpreadRecovery 启动路径也会在启动 worker 前执行该预检。未声明静态标的的 worker 仍在订单准入时按实际订单标的检查。
- 定向复核：预检有效/非法规格用例、deploy 模板覆盖 3/3、qx-cli all-targets 严格 Clippy 及运行时配置/路径/market-spec 架构合同通过；全量发布矩阵未运行。


### WP-7.68 · 清理成熟度台账中的历史门禁数量地板

成熟度能力台账仍残留历史 `GATE_CHECK_FLOOR` 与门禁总量递增描述，容易被误读为当前验收要求。已从当前能力台账移除这些过时数量阈值与快照措辞；活代码、CI、本地构建和成熟度工具中均无相关门禁或旧入口引用。保留具名行为/结构合同、实际测试执行及基准回归门禁，不改 A 股/币圈/Paper 验收链，也不删归档历史记录。

**定向验证**：当前执行路径检索无 `GATE_CHECK_FLOOR`、`WORKSPACE_TEST_FLOOR`、旧快照/行数预算和 `validate_core.py` 引用；完整架构及发布矩阵未运行。

### WP-7.69 · 运行时冻结规格与双市场回测定向复核

按当前工作树重跑运行时 market spec 预检回归，覆盖合法规格启动放行、非法精度配置拒绝及 runtime reference 诊断；对应架构合同检查运行时配置 fail-closed、market spec 单读者与路径所有权，共 18 项通过。A 股和币圈 CLI 确定性验收重新生成通过，hash 与已登记基线一致：`6be034a4cb0760fa` / `26fdd6b52d020700`。另重跑 A 股报告产品进程用例，验证多文档导出、便携证据重算以及同一输入比较，1/1 通过。

**定向验证**：`runtime_reference_preflight_parses_market_specs_for_declared_worker_symbols` 1/1；三组架构合同 18/18；A 股与币圈回测验收通过；`ashare_runs_export_documents_and_compare_on_the_same_input` 1/1。完整 workspace/全量架构门禁与三轮全链路审查仍待开发收敛后执行。

### WP-7.70 · Paper 本地执行闭环验收复跑

当前 qx-cli 二进制通过 Paper 端到端验收：提交 1 单、成交 1 单、生成 4 条账簿事实，重跑不重复下单，证据链可到 Executed。默认 Windows 临时目录的 Scheduler 状态文件遇到权限拒绝，改到事先确认不存在的 `target/qx-paper-acceptance-wp769-20261021` 隔离根后验收通过；该失败属于临时目录访问问题，未修改业务规则。

**定向验证**：`paper_acceptance.py` 退出码 0；严格 Clippy、A 股/币圈验收、A 股报告产品与运行时规格预检证据见 WP-7.69。本地外部 Venue/sandbox 尚未验收；全量 workspace/全量架构门禁与至少三轮全链路审查仍未完成。

### WP-7.71 · API/Console/SDK 当前树跨层复核

针对当前 API、控制台与 SDK 工作树改动重跑真实行为与结构合同：Python SDK 分层、应用用例、Provider 与新增回测 DTO/原生规格用例共 89/89；API 路由、Console 接线、研究菜单与 RunHandle、Rust/Python/C++ API 合同及 SDK 导出/认领合同共 105 项通过。`cargo test --offline -j 1 -q -p qx-api -p qx-sdk` 全部目标通过，qx-api 86 个单测和各 API/SDK 集成目标零失败；两个 crate 全目标严格 Clippy `-D warnings` 通过。重复测试没有发现新的断链或孤儿公开入口。

**边界/下一步**：WP-3 服务宿主目标仍未完全关闭，CLI `worker_entry.rs` 与 `venue_runtime/` 仍持有 venue 执行实现；该剩余项不能由这轮 API/SDK 用例替代。开发收敛后仍需完成终态三轮全链路发布复核与全量发布矩阵。

### WP-7.72 · Paper 验收目录隔离与非破坏性修复

Paper 验收脚本原先默认使用固定系统临时路径，且若目录存在就递归删除；这既与保留证据便于复核的说明不符，也在 Windows ACL 环境失败。现默认在仓库 `target/` 创建唯一临时目录；显式 `--root` 必须是不存在的新路径，已有目录会 fail closed 且保留原内容。架构 `paper_track_check()` 新增该路径安全合同。

**定向验证**：Paper 轨架构合同 8/8；Python 脚本语法检查通过；已有目录拒绝用例确认原目录 36 个文件未变；不传 `--root` 的完整 Paper 验收通过，OrderSubmitted=1、Filled=1、LedgerApplied=4，并写入当前成熟度记录。全量架构门禁与发布矩阵未运行。

### WP-7.73 · 服务层测试夹具 Windows 临时目录修复

`qx-service::backtest_bindings::builtin_snapshot_normalizes_strategy_identity_and_portable_frame_path` 曾将临时 BarFrame 写入系统 temp，并在当前 Windows ACL 下以 PermissionDenied 失败。夹具现改用仓库 `target/qx-service-tests` 下带进程号、纳秒时间戳及原子序号的新目录，并在断言后清理；不改生产路径或业务逻辑。

**定向验证**：`cargo test --offline -j 1 -q -p qx-service` 28/28；`cargo clippy --offline -j 1 -p qx-service --all-targets -- -D warnings`、`cargo fmt --check -p qx-service`、Paper 验收族架构合同、Python 脚本语法与成熟度生成一致性通过。WP-3 的 venue 领域 runner 迁移和开发完成后的全链路/发布矩阵仍未关闭。

### WP-3.41 · Worker pipeline 指标报告器迁入 qx-service

`PipelineMetricsReporter` 原由 qx-cli 持有，但其生命周期口径、worker-metrics 文件发布和 Prometheus counter 聚合都属于服务宿主职责。现将报告器迁入 `qx-service::pipeline_metrics_report`，依赖服务层唯一的 worker metrics IO；qx-cli 删除旧实现，仅重导出类型并继续把各 worker pipeline 观测交给它。同步更新 runtime 的生产出口说明、Grafana/字段文档及 pipeline 打开站点豁免文档；`health_snapshot_knob_check()` 新增唯一所有权判据，拒绝 CLI 旧实现复活。

迁移复核时 `paper_worker_publishes_pipeline_counters_to_its_metrics_file` 暴露一条旧断言：该夹具的四次 ingest 对应四次 refresh，订单准入在账户锁内另有一次显式刷新以追平锁等待期间的 EventLog，因此总刷新应为五次。行为逻辑正确，已将断言更新为 4 + 1，并在用例说明其新鲜度用途；ingest 仍精确为 4。

**定向验证**：`pipeline_metrics_surface` 7/7；pipeline 打开站点账本 2/2；`health_snapshot_knob_check()` 2/2；qx-service 28/28；qx-service/qx-cli/qx-runtime 全目标严格 Clippy 与格式检查通过。旧 CLI 报告器路径仅留在架构合同中用于确认其不存在。WP-3 其它 venue 领域 runner 所有权与终态三轮发布审查仍开放。

### WP-7.89 · A 股性能夹具改用沪深公开年度休市日历

将 A 股吞吐负载过去自动生成的 weekday-only 日期替换为版本化的沪深 2024-2025 交易日历夹具（485 个开市日），其节假日安排来自上交所、深交所当年年度通知；超过夹具日期范围会直接报错。性能证据分别绑定源夹具和实际传入 Rust 回测的裁剪日历摘要，保留合成分钟行情、未覆盖公告外临时休市与未覆盖完整规则分支的限制。

**定向验证**：夹具检查 3/3；以当前 Release 重建 qx-cli 后完成 1k/8k/64k 三档、每档 3 次与 4 条 PIT 公司行为的回测，P50 为 274.290/305.733/551.723ms，4 条动作相对单动作控制均改变结果哈希，A 股规则/成本绑定均被观察到。二进制 SHA-256 为 `af7801132b07c2c75d56a0e7490da42ccab8bf5c9acaace9ddfc3ddb5578ba0b`，证据：`maturity/evidence/ashare-rules-backtest-2026-10-21-calendar-release.json`。此吞吐证据不覆盖完整规则合同或 Paper 生命周期，`ashare-research` 仍保持 `unmeasured`；最终三轮全链路审查和全量发布矩阵继续开放。

### WP-4.14 · 最新 Release 重验币圈回测与 Crypto Paper 闭环

使用同一当前 Release 二进制重跑 Crypto 与 A 股仓内回测验收，结果哈希分别为 `26fdd6b52d020700` 和 `6be034a4cb0760fa`；两条路径都验证同目录重跑字节一致、独立目录产物一致及无凭据运行，并刷新两份成熟度验收记录。Crypto Paper E2E 以同一二进制重复 3 次，验证 scheduler→strategy→Paper execution→Ledger/EventLog/audit、幂等重跑及独立根一致；P50/P95 为 2862.415/2866.430ms，二进制 SHA-256 `af7801132b07c2c75d56a0e7490da42ccab8bf5c9acaace9ddfc3ddb5578ba0b`，证据：`maturity/evidence/crypto-paper-e2e-2026-10-21-current-release.json`。

此 E2E 不覆盖实时 funding Provider、8 小时 funding 调度和外部数据库，故 `crypto-paper` 仍为 `unmeasured`；`crypto-testnet` 还需要真实凭据/环境。完整三轮全链路审查及最终全量发布矩阵仍开放。

### Crypto · 精确市场规则核心类型（2026-10-24）

`TradingInstrumentSpec` 增加可选的 `ExactTradingRules`，精确表达合约乘数、tick、lot、最小数量和最小名义额；核心订单/成交校验可处理 1e-10 等步长，名义风险向上取整、最小报价金额向下取整。同步扩展 15 份回测/实验 JSON schema。

这不代表精度已贯通实盘：CCXT 市场解析仍输出 1e9 raw 字段，精确订单被执行入口拒绝，精确风控、资金预留、账簿与恢复尚未实现；超过 9 位小数继续 fail-closed。离线脚本 `scripts/crypto-live-offline-check.ps1` 通过，架构门禁 225 项、CCXT Python 57 项通过/1 项跳过、打包契约 6 项；未读取账户或发送订单。

### 发布审查第 1 轮 · 回测至 API、Console 与 SDK 主链路

第一轮沿 Crypto/A 股样例回测、qx-app/共享撮合账簿、qx-api/Console、Rust SDK 与 Python SDK 顺链核验。最新 Release 上两市场回测验收、Paper worker E2E、qx-cli 回测合同 50 项、qx-api/qx-sdk 全目标测试与 Python SDK 测试均通过；Console API 的页面/路由/认证/跨源/关闭行为包含在 qx-api 实测中，控制台 JS 通过 Node 语法检查。没有发现策略到共享回测用例、RunHandle/API 或 SDK facade 的新断链。首次 CLI 合同测试受 Windows 系统临时目录 ACL 阻断，切换到仓库隔离临时目录后同一组 50 项通过。

**证据**：回测合同 50/50；qx-api 86 单测与全部集成目标通过；qx-sdk 全部 44 个测试通过（含 31 个独立消费者用例）；Python 测试 199/199；Node `--check` 与对应差异检查通过。此轮没有替代运行时无限等待审计、全量架构门禁、全 workspace 测试或完整 Release 矩阵；尚有至少两轮全链路审查与最终发布矩阵待完成。

### 发布审查第 2 轮 · Rust 核心账簿、风控、OMS 与撮合

逐条复核账簿开/平/反手的杠杆记录及保证金估值、统一 RiskGate、Bar/OrderBook 现货库存与衍生品保证金预留、OMS/Paper venue 状态与取消边界。发现跨 Bar 卖单预留已有集成回归，但缺少同 Bar 多笔卖单覆盖；补入 BacktestEngine 合同，验证第一笔在途卖单预留库存后第二笔被拒、成交后仓位不为负。代码审查未发现资金预留双计/漏计、借余额放行或会阻塞退出的循环路径。

**定向验证**：`cargo test --offline -j1 -p qx-core -p qx-risk -p qx-zhenlu -p qx-xingban --no-fail-fast` 全部目标通过；`cash_spot_sell_inventory` 3/3；四 crate 全目标严格 Clippy `-D warnings` 与 workspace Rust 格式检查通过。此轮未审 API 生产部署、进程异常恢复及全部 venue 外部对账；发布审查第 3 轮和全量发布矩阵仍待完成。

### 发布审查第 3 轮 · Worker 停机、API/Console 接线与多语言 SDK

沿服务宿主、RuntimeSupervisor、HTTP/Console accept loop、API 投影桥、同步/异步 RunHandle 与 Python/Rust SDK 回测入口检查等待与终态。补入 ConsoleFront 无连接停机回归，验证它在空闲 accept 轮询时会响应停止令牌并返回；原 API 停机、Worker 长间隔取消、投影桥停止合同已一并复测。Console 前端声明路由与 qx-api 路由双向对账，异步研究入口无后端孤儿启动路由，SDK facade 与 Python 分层公开面也通过定向检查。

**定向验证**：新增 Console 停机用例 1/1；qx-api/qx-runtime/qx-service 全目标测试与严格 Clippy 通过；前端/API/SDK/Worker 等六组架构合同共 74 项、0 失败；qx-sdk 44 项、qx-cli 回测合同 50/50、Python SDK 199/199 通过。三轮审查中的定向改动均已通过对应回归。全 workspace / 全量架构门禁 / Release 矩阵仍未执行；版本候选需待最终全量门禁并修复其发现项后再定。

### Crypto 实盘链路收敛记录 · CCXT 精确现货/合约与组合只读核算（2026-10-24）

Binance、OKX、Bitget 的 CCXT 执行路径接通精确十进制订单量/限价、冻结市场规格、现货数量/价格步长校验、账户级在途资金预留、合约杠杆档位预检、逐笔成交/多币种手续费恢复及 EventLog 持久化。双向持仓按交易所已配置的持仓模式核验；不会在下单时暗中切换账户模式。多账户多腿由账户隔离的 worker 编排与补偿，不跨账户合并保证金。组合只读接口提供现金、权益的币种分组和可选报告币折算。

**本地离线验证**：`scripts/crypto-live-offline-check.ps1` 全量通过：Rust 格式与选定实盘合同测试、4 片架构门禁（225 项）、CCXT Python 请求/Worker 合同 63 项（62 通过、1 跳过）和 Python 包装合同 6/6。验证仅使用本地 fake/离线边界，没有读取账户密钥、连接交易所或提交订单。快捷命令与 sandbox 只读连接探针见 [`crypto-live-readonly-smoke.md`](../crypto-live-readonly-smoke.md)。

**剩余发布边界**：Paper 撮合仍拒绝高于 1e-9 的精确订单；内置策略、C ABI 与部分控制入口仍使用旧数量刻度。当前实盘订单类型限制为已声明的 Market/Limit，交易所条件单尚未纳入能力面；Hedge 账户的持仓模式需由操作者预先配置，运行时只核验、不自动切换。多账户不共享保证金/购买力，交易所之间没有原子组合下单。CCXT sandbox 的实际订单接受、撤单与成交对账尚未验收。因此当前代码已有可离线验证的多交易所核心路径，但这些边界完成并通过供应商 sandbox 验收前不应标记为完整实盘发布。

## 2026-10-24 CCXT 仓位精度恢复与 float 闸门

- Binance、OKX、Bitget 的 CCXT position parser 可能把原始十进制文本转成 `float`。归一化现在从 `info` 中按交易所字段恢复合约数量、开仓价、标记价、强平价、未实现盈亏及可验证的保证金；Bitget 合约数量按原始持仓量与精确合约乘数换算，Bitget 派生保证金和 Binance/OKX 派生初始保证金按与 CCXT 一致的十进制公式重算。
- 恢复后的精确值必须投影回同一 CCXT 数值以确认字段语义；原始值缺失、原始字段自身是 float、或关键持仓事实无法在支持尺度内表示时拒绝生成风控快照。无法从 position 响应重建的衍生维护保证金保持缺失，不拿 float 字符串伪装精确金额。
- 离线回归覆盖三家交易所的 float→原文精度恢复、Bitget 合约单位换算、原始字段缺失/本身为 float 时 fail-closed。`scripts/crypto-live-offline-check.ps1` 完整通过：架构 225/225、CCXT 请求/配置/Worker 72 项（71 通过、1 项因本机 CCXT 版本与锁定版不同跳过）、Strategy API 17/17、打包合同 6/6，Rust 选定实盘与账簿合同通过。未读取 API 环境变量、连接账户或提交订单。

## 2026-10-24 跨语言策略精确目标调仓

- Python/JSON Strategy API 可通过 `positions_exact` 读取账簿十进制仓位，并用 `target_qty: 0` + `target_qty_exact` 表达单向目标仓位。Worker 从账户 EventLog 恢复当前精确数量，计算精确差额后复用冻结市场规格、数量步长和账户级风控；旧 1e-9 `positions` 投影与精确值不一致时拒绝策略输入。
- 该目标接口明确限制为单向仓位；Hedge 模式下净目标无法区分 long/short 两条腿，运行时拒绝并要求使用显式方向 intents。Bar 回测没有精确数量撮合路径，遇到精确目标会拒绝而不静默截断。内置策略、C ABI 与部分控制入口仍采用旧 raw 数量刻度。
- 定向验证：`cargo check --workspace --all-targets -j 1` 通过；精确目标 CLI 用例 6/6、Runtime strategy contract 7/7、Python Strategy API 19/19 通过。随后 `scripts/crypto-live-offline-check.ps1` 全部通过，架构门禁覆盖 225 项，CCXT 合同 72 项（71 通过、1 项因本机 CCXT 版本与锁定版不同跳过），Strategy API 19/19、打包合同 6/6；未读取密钥、连接交易所或发送订单。

## 2026-10-25 实验输入规格 sidecar 的 Schema 契约

- 单市场 Bar/Depth 参数实验的新报告会在 `artifact_locations` 中公布 `experiment_input`，其内容由 RunRecord V2 的 ArtifactRef 绑定。两份严格 Schema 原先禁止该属性，导致 Rust/Python 可生成的报告无法通过消费者侧校验。
- Bar/Depth 实验结果 Schema 现在允许可选 `experiment_input`，仍要求 `experiment_report` 与 `run_manifest`，并保留 `additionalProperties: false`。旧两路径映射和新三路径映射均有效。
- Python app 用例检查新 sidecar 文件并对实际报告运行 JSON Schema 验证；SDK 合同用例覆盖旧、新映射形状。此修复只闭合 Bar/Depth 参数实验 Schema 接线，不代表其它 RunType 的报告/portable/recompute 覆盖已完成。

### Runtime BaseStrategy 类策略适配与经典接口优点收敛（2026-10-26，未发布工作区）

Python Runtime Strategy worker 现在可用显式 `python_strategy_spec` 实例化 `BaseStrategy` 类；未配置时旧模块级 `on_event(request)` 保持原合同。运行前复用完整 `STRATEGY_MANIFEST` 绑定默认参数、校验参数类型/范围，并运行可选纯 `validate_config(config)`；策略文件可导入同目录 helper。策略类的 Bar/Tick、订单状态和带 `event_seq` 的 fill 由 worker adapter 按序分发；Tick 经 Strategy input v6 与 EventLog cursor 恢复，OrderBook/Timer manifest fail-closed。JSONL、framed JSON、shared-memory JSON 和 shared-memory columnar 四种传输均已联通。

Runtime Tick 输入携带冻结行情 EventLog 范围、exact bid/ask 与挂单量 sidecar；策略 checkpoint 和行情 cursor 同步原子提交。首次启动最多回放最近 10,000 条报价，重启按 durable cursor 续接。本轮完成 Rust 编译检查；没有运行测试、连接真实沙箱供应商或构建发布包。

经典接口吸收决策已补入策略对比方案：吸收 Freqtrade 的研究/运行回调分层、Backtrader 的 warmup 与订单/成交反馈分离、VeighNa 的 typed event callback、RQAlpha 的交易日历阶段；只要求一个实际市场决策入口，A 股 session hooks 待交易日历事件合同，Freqtrade 向量化 `populate_*` 作为独立研究 adapter。策略回调继续只返回意图，不能直接操作 Gateway 或交易所。

- 多账户修复：update 游标改为按 `(account_id, venue)` 隔离，状态/成交回调按宿主规范顺序分派，且回调上下文使用该更新自己的账户、venue 和 instrument；相同 scope 的重复更新在本 worker 生命周期内去重。
- 定向验证：Python BaseStrategy/StrategyContract/进程 adapter 59/59；策略 manifest 与 CLI 文件入口 39/39；Rust→Python class worker 四种传输端到端 1/1；Rust Runtime 配置 schema 1/1；相关 Rust 文件 `rustfmt --check` 与 `git diff --check` 通过。
- 边界：Runtime process class adapter 支持 Bar 与显式 Tick；OrderBook/Timer 仍仅在 SDK callback 路径支持。去重游标与用户策略内存通过 checkpoint/cursor 原子快照跨 worker 重启恢复，但语义为 at-least-once。本轮全仓 `cargo fmt --all -- --check` 仍被其他既有脏文件格式差异挡住，策略改动涉及的 Rust 文件单独格式检查通过；未执行全仓测试、供应商验收或 release packaging。

### P0-C 行情精确数量 sidecar 首片（2026-10-26，未发布工作区）

`QuoteTick`、运行时行情事件、EventLog 恢复及 Paper 行情镜像现在保留 CCXT REST/Pro 与 Binance bookTicker 的原始 bid/ask 挂单量十进制；旧 `Quantity` raw 字段使用向零投影，入口校验投影一致性，精确数量也纳入事件 digest。定向验证：相关 Rust crate `cargo check` 通过；Binance 子纳米数量、Runtime EventLog 恢复、Paper bridge 精确回环三项 Rust 用例通过；CCXT ticker/stream 三项 Python 用例通过。

这只完成报价数量事实保真。Paper 仍拒绝精确订单，Bar 撮合、成交/费用、规格约束、账本与报告的 exact round-trip 仍未闭合；能力声明维持 fail closed，不代表 P0-C 已完成。

### 2026-10-26 精确成交贯通标准回测产物与报告

补齐上一节列出的 Bar 回测成交报告缺口：普通与自定义策略 Bar 回测共用 fills CSV v2 序列化器，在原有 raw 投影后增加 `qty_exact`、`price_exact` 与 `exact_fees_json`，按 RFC 4180 转义文本；RunRecord 对新写产物登记 v2，同时继续读取历史 fills v1。多腿组合 fills CSV 增加带腿身份的 v2 格式。报告 reader 校验精确十进制值及费用 JSON，兼容旧 4/6/11/12 列与新 14/15 列；HTML 与 XLSX 显示 exact 数量、价格和多币种费用，Python 研究报告保留相同字段。

定向验证：CLI `report_html` 21 项与 XLSX exact 成交单元用例 1 项通过（含小数位、带逗号的多币种费用 JSON、多腿 exact 字段和兼容旧格式）；构建 `qx-cli` 后子进程相关用例通过；Python 自定义策略 Bar 回测真实落盘的 fills.csv 与应用报告 exact round-trip 用例通过。深度/多账户供应商 reports 和真实交易所验收不在本改动的覆盖范围。

## 2026-10-26 MultiLeg 组合权益精度

- 共享账户策略型 MultiLeg 的 exact 权益与逐腿 PnL 通过 v5 artifact 输出并由 reader 复核；独立账户 MultiBuiltin 组合现在直接合并两腿 `BacktestReport.equity_exact`，以 exact 初始/期末权益计算组合收益，并从包含初始本金的精确曲线计算回撤。
- 组合摘要增加逐账户 exact 本金/期末权益与 `metrics_exact.equity`。现有摘要 schema v7 以可选字段兼容这些 exact 值和精确执行尝试价格；权益 CSV 保留两腿 raw 投影和组合 `equity_raw`，并增加 `equity_exact`；新组合表头登记为权益 CSV schema v4，RunRecord 能识别，摘要复核会对拍腿级 exact 权益、组合收益和精确曲线回撤。旧 v1–v3 表头继续读取。
- 子纳米回归覆盖组合收益 526 bps、最大回撤 500 bps。定向验收：`qx-app` 库 158/158、MultiBuiltin 8/8、共享账户 MultiLeg 11/11；`qx-app` 编译与所涉 Rust 格式通过；实际 CLI 生成的组合摘要及 60 行 v4 权益 CSV 均通过 JSON Schema 校验。OrderBook Depth 与 MultiMarket Depth 仍受 raw 核心行情/成交/估值限制，不因本改动宣称 exact。

## 2026-10-26 共享多市场 DepthFrame v2 精确 Spot 报告贯通

- 共享账户、多市场 L2 回测支持带 exact hooks 的策略在 DepthFrame v2 上提交基础币数量 Spot 订单；现金预留在市场间汇总，避免同一小额账户现金被多市场重复承诺。精确盘口、成交手续费、成交额、账簿及 mark valuation 保留 ExactDecimal。
- 多市场深度 artifact 升为 schema v4，研究报告 reader 复算并核对 exact equity、成交/手续费和 Ledger 手续费，输出 turnover、fees-by-currency 与明确的 legacy raw 投影；篡改 exact turnover 会被拒绝。队列位置估算、quote-budget 和合约精确深度仍不在本支持范围。
- 定向验证：`qx-core --test valuation` 12/12、`qx-xingban --lib` 190/190、`qx-xingban --test depth_frame_hash` 5/5、`qx-app --lib` 159/159、`qx-app --test multi_market_depth_backtest` 9/9；`qx-app --lib` 编译检查通过。此为本地离线验收，不表示供应商沙箱或实盘交易已验收。

## 2026-10-26 单市场 DepthFrame v2 精确线性/反向合约

- 单市场精确订单簿支持永续/期货基础数量订单。精确风险投影覆盖当前 Net/Long/Short 持仓（已记录杠杆比例向上保守修正 1e-9 兼容精度误差）、所有活跃在途订单和当前候选，按 ExactDecimal 汇总初始保证金与手续费，并用精确标记权益核验；exact derivative 风险不再依赖可能投影为零的 raw 数量/保证金字段。
- 精确撮合仍只接受冻结精确规格；合约 quote-budget 明确拒绝。合约成交不伪造 Spot `quote_cost`，精确手续费按 `settlement_currency` 入账，因此线性 USDT 与反向 BTC 结算均沿用 Ledger 的精确衍生处理。共享多市场 Depth 的衍生组合保证金继续 fail closed。
- 成交入账后再按实际精确成交价复核权益、已成交仓位、剩余挂单保证金与费用预留；滑点导致保证金不足时回测返回明确错误，不生成欠保证金报告。逆向合约的精确名义额随价格反向变化，风险价格按较低的标记/限价估算，避免用较高价格低估保证金。
- 验收：`qx-xingban --lib` **199/199**；线性/反向子纳米成交、逆向价格风险侧、杠杆聚合、精确保证金不足拒单及成交滑点后保证金复核定向覆盖通过；`qx-app --test custom_depth_strategy` **5/5**，报告 reader 能验证精确合约手续费/成交额；`qx-app --test depth_backtest` **10/10**。
- 边界：仍未覆盖多市场衍生组合保证金、跨币种抵押品折算、合约 Depth 资金费率、交易所数据 golden 对拍、真实供应商连接/隔离验收或发布包矩阵；不代表实盘交易验收。

## 2026-10-26 共享多市场 DepthFrame v2 精确线性/反向衍生品

- 共享多市场 Depth 现接受同一结算币种的线性/反向永续与期货组合。组合保证金聚合所有市场持仓、活动订单及候选订单；手续费按市场费率计算，持仓权益使用各市场精确 mark，保守处理线性/反向限价风险。
- 每笔成交后以实际成交价重新核验持仓保证金、剩余订单、费用和权益；策略上下文的可用保证金和 exact risk facts 也使用共享组合投影。研究报告 reader 能重算并验证线性 USDT、反向 BTC 两种共享账户结果。
- 新增 fail-closed 用例确保混合 Spot/衍生品或跨结算币种组合被拒绝；另验证实际成交价跳升导致共享保证金不足时回测失败，不输出成功报告。验收：`qx-app --test multi_market_depth_backtest --offline -j 1` **12/12**。
- 尚未支持跨币种抵押物 haircut/FX 折算、资金费率深度模型、精确队列估算或实盘交易所逐笔对拍。

## 2026-10-26 多腿提交响应丢失栅栏与队列恢复

- 多腿组快照在 OMS 注册和 Venue 调用之前持久化 `submission_in_flight_order_id`。不同腿在该提交得到权威结果前返回 `RETRYABLE`；Binance、CCXT 和 Paper 队列不终结或确认该命令，等租约到期后重试。首腿 worker 重启时会先用匹配账户/交易所的 EventLog 事实同步组快照：确认接受后释放栅栏；若只恢复到 `Submitted`，原命令进入对账而不重发；`Unknown` 仍由 `ReconcileRequired` 阻止后续腿。
- 新故障注入在模拟 Venue 记录唯一一次外部副作用后、响应返回前触发进程退出；第二腿保持未登记并返回可重试状态。随后应用首腿的权威 Accepted 快照可释放栅栏。另测跨账户 EventLog 不会被错误用于同步另一账户腿。
- 验收：`qx-execution --lib` **38/38**、`qx-service --lib` **85/85**、`qx-zhenlu --lib` **56/56** 通过；`cargo check -p qx-service --tests` 通过，定向 Rust 格式检查通过。
- 边界：当前多腿组状态仍由本地文件 store 管理；这些回归验证本机崩溃/队列重试，不证明跨节点组存储并发、Postgres 集群行为或 Binance/OKX/Bitget 真实网络请求幂等。队列重试等待现有 30 秒 command lease 到期。

## 2026-10-26 提交响应丢失后的恢复防重发

- 统一执行网关现在会将已存在的 `PendingSubmit` 与 `Submitted` 一样视为远端状态有歧义；先追加 `ReconcileRequired` 并拒绝再次调用 Venue。状态机允许 `PendingSubmit → Unknown` 表达该不确定结果。生产 `LiveEventPipeline` 仍会在首个网络副作用前持久化 `Submitted`。
- 这收紧了早期对 `PendingSubmit` 的推断：只有特定 EventLog 注册实现能证明该状态尚未触网，通用 `OrderStore` 接口并未保证这一点，因此重启后的既存状态必须统一按歧义处理。
- 故障注入先在 fake Venue 记录一次副作用，再模拟提交响应返回前进程退出；重开磁盘 EventLog 后重投同一订单不会再次触网，状态进入 `Unknown` 等待对账。独立端口用例覆盖存储仍保留旧 `PendingSubmit` 的适配器。
- 验收：`qx-core` 状态迁移定向用例通过、`qx-runtime --lib` **95/95**、`qx-execution --lib` **37/37**、`contract-tests --test recovery_and_replay` **3/3**；`cargo fmt -p qx-execution --check` 通过。
- 边界：模拟 Venue 与本地持久 EventLog 不替代 Binance/OKX/Bitget 网络断连验收；乱序私有流、跨节点并发和完整多腿补偿仍未闭合。
## 2026-10-27 模块函数策略错误观察钩子

- Python 模块函数策略可选实现 `on_error(request, error)`，补齐 legacy 函数入口的失败诊断能力；无钩子的策略仍直接调用原始 `on_event` / `on_decision`。
- 错误钩子异常只产生 stderr 诊断，策略请求仍返回原始错误，不能借观察回调改变失败结果。
- 验收：相关策略回归 **78/78**；完整 `python/tests` **530 通过、10 跳过、0 失败**；已跟踪文档差异 `git diff --check` 通过。
- 本条记录之后已由 §13.31–13.33 补齐 Rust/C ABI/Python 共用 DTO；history-requirements 与独立诊断路径继续由下方记录闭合。

## 2026-10-27 Python 预热声明和候选诊断生命周期错误合同

- 将本地 BaseStrategy 与 Runtime class worker 的 `history_requirements()` 延后到可取得请求身份、时间和标的的初始化 callback；失败时发出 `StrategyErrorV1(stage=history_requirements)`，并继续调用 legacy error observer。manifest warmup 与策略声明冲突、非 Bar 数据级声明 Bar warmup 也会在同一阶段报告。
- 候选偏差诊断的 `BaseStrategy` 路径现在复用正式 class adapter，按 `as_of` 截断主/peer Bar 历史，并执行 `on_init/on_start`、`history_requirements`、经典 `prenext/nextstart/next`、诊断快照和 stop 生命周期。诊断错误通知 class/module observer；观察 hook 异常不改变原错误。非 BaseStrategy 模块式回放保留兼容。
- 验收：Python Runtime adapter **26/26**、本地策略基类 **33/33**、候选诊断 **16/16**、策略文件 CLI **22/22**，定向合计 **97/97**；随后全量 `python/tests` **538 通过、10 跳过、0 失败**，相关 Python 文件 `py_compile` 通过。证据：`maturity/evidence/strategy-history-diagnostics-error-v1-2026-10-27.json`。
- 限制：导入失败且策略模块/实例尚不可用时没有实例级 error hook；W3 全生命周期跨宿主 golden matrix、真实 sandbox provider 与生产发布门仍开放。
