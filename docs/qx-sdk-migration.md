# 从 `qx-app` 迁到 `qx-sdk`（方案 §6.1 迁移指南）

## 这份文档回答什么

方案 §6.1 要求第三方稳定边界"有 semver、迁移指南、examples 和独立消费者构建"。examples 在
`crates/qx-sdk/examples/quickstart.rs`，独立消费者在 `crates/qx-sdk/tests/independent_consumer.rs`，
本文是迁移指南。

初次体验可运行[独立快速入门工程](../examples/rust-sdk-quickstart/README.md)，
它自带教学数据并固定到 `v0.4.0` 提交，用于复现历史 Git 安装路径；当前正式版本请使用下方 crates.io 依赖。
需要连接运行中的 API，可参考源码工作区内的[可选 HTTP 适配器](../crates/qx-sdk-http/README.md)；
该适配器尚未发布，必须使用包含它的源码版本。

## 要不要迁

**要迁**，如果你属于下面任一种：

- 你把牵星嵌进自己的 Rust 程序，而不是用 `qx-cli`；
- 你在 `Cargo.toml` 里直接写了 `qx-app = { path = ... }`（或将来某个版本号）；
- 你想接自己的数据源 / 交易所。

**不用迁**，如果你在改这个仓库本身：`qx-cli` / `qx-api` / `qx-python` 仍然直接依赖 `qx-app`，
门面**不是**它们的中间层。门面只服务仓库外的调用方。

## 迁什么

### 1. 依赖

v0.4.1 crates.io 发布和独立消费者验收完成后，普通用户可使用 registry 依赖：

```toml
[dependencies]
qx-sdk = "0.4.1"
```

需要复现旧版源码或测试尚未发布的源码时，可使用下面固定到完整提交的 Git 依赖；一般用户应使用上面的 crates.io 版本。

仓库外消费者在内部 crate 尚未发布到公开 registry 时，可以直接锁定已推送的 Git 提交；SDK 及其同仓路径依赖都从同一个不可变提交解析：

```toml
[dependencies]
qx-sdk = { git = "https://github.com/coeasy/qianxing.git", rev = "e7fec623ffbb1b55acec3b4ce414f26b1a3ac961", package = "qx-sdk" }
```

该 Git 来源示例固定到已发布的 `v0.4.0` 标签指向的提交，用于可复现的历史消费路径。需要测试当前源码时应固定完整提交哈希并提交 `Cargo.lock`。验证独立消费者可运行：

```powershell
python tools/check_rust_sdk_git_consumer.py
```

此检查创建 workspace 外的临时 Cargo 工程，以当前 `HEAD` 的完整 Git SHA 作为依赖，构建后实际调用数据校验、Rust 单市场回测与结果复核、多市场多策略参数实验（3 个市场、4 个候选）；远端 Paper 生命周期和事件游标使用测试传输，不代表真实服务验收。它验证 Git 安装路径，不等同 crates.io 发布；发布到 crates.io 仍需先发布内部 crate 依赖并通过 `cargo package` 与 registry consumer 验收。
默认使用独立 Cargo 缓存；网络受限时可将 `QX_SDK_CARGO_HOME` 指向已有 Cargo 缓存目录。

`qx-sdk` 的公开面是 `qx-app` 公开面的**超集**，所以迁移期内两个依赖并存也不会冲突；
但迁完之后应当把 `qx-app` 从 `Cargo.toml` 里删掉——留着它意味着"我还在用内部层"。

### 查看 Rust 发布依赖

```sh
python tools/cargo_publish_plan.py qx-sdk
python tools/cargo_publish_plan.py qx-cli --json
python tools/cargo_publish_plan.py qx-sdk --check-crates-io --json
python tools/check_rust_sdk_packages.py qx-sdk --offline  # 本地改动尚未提交时加 --allow-dirty
python tools/publish_rust_sdk.py qx-sdk --allow-dirty --offline-package-check --json  # 本地包离线构建，注册表只读查询，不上传
```

该工具通过 Cargo metadata 生成 workspace 路径依赖的拓扑顺序，包含所有目标平台和可选的
normal/build 依赖，排除 dev 依赖，并列出 `publish` 限制及 workspace 外的路径依赖。默认不联网；
dev 路径依赖会单独列出供打包审查：带版本的 dev 依赖也可能要求先在注册表可解析，因此此顺序不能独自作为发布顺序。
`--check-crates-io` 才逐包查询公开元数据，区分没有记录、名称已有记录、目标版本已存在（包括是否已撤回）；
查询失败会中止后续查询并以退出码 2 标记未完成。无记录不保证名称可用；有记录或同仓库地址也不证明账户拥有发布权限，
已有版本还需核对构件身份。此工具不上传包，也不替代实际的 `cargo package` 和干净环境消费者验收。
当前工作区图中 SDK 包含自身共 14 个包，CLI 共 26 个；具体数量以运行结果为准。CI 会离线执行图生成和工具单元测试。

`check_rust_sdk_packages.py` 按依赖顺序生成实际 `.crate` 包，再从归档解出的内容编译每个包，
并让独立 Rust quickstart 使用这些归档进行回测和结果复核。它通过本地 patch 模拟依赖解析，
不上传任何包，因此不能证明 crates.io 的名称归属、索引可见性或从正式注册表安装成功；
正式发布仍需逐包确认权限、注册表传播以及 clean consumer 验收。`--offline` 要求第三方依赖已缓存。

在包归档生成后，可只读比对注册表已经存在的同版本构件身份：

```sh
python tools/check_crates_io_artifacts.py qx-sdk --json
```

工具逐个计算 `target/package/<name>-<version>.crate` 的 SHA-256，并与 crates.io 该版本公开的 checksum 对照；
已有版本若被撤回或字节不同会失败关闭。没有记录/只有其他版本时仅表示查询时的公开状态，不能保证名称仍可注册、
账户有上传权或之后不会被抢注。在线查询失败会返回非零状态，不会将“未知”判为可发布；该检查也不上传软件包。

`tools/publish_rust_sdk.py` 是依赖顺序发布入口。缺省只构建/验收归档并读取注册表元数据；上传必须显式传入
`--publish --confirm qx-sdk@<版本>`，同时要求 `CARGO_REGISTRY_TOKEN`、干净工作树及 `HEAD` 精确位于 `v<版本>` tag。
`--offline-package-check` 仅让本地 Cargo 归档闭包使用已缓存依赖；公开注册表 checksum 查询仍联网直连 crates.io。
已存在版本仅在非撤回且 SHA-256 与本地产物完全一致时跳过；新包发布后会等待 crates.io 返回相同摘要，再发布其下游。
全部包可解析后还会从 crates.io 在独立 Cargo home 安装 `qx-sdk`，实际运行 70 根 Bar quickstart 并要求回测复核通过。
网络中断或中途失败可重新运行，工具会从已核实的包继续。隔离 Cargo home 用于避免机器全局 registry mirror 覆盖官方发布源。
任何预检都不保证 crate 名可注册或账户有权发布；无权/名称冲突时发布会停止并保留已成功部分，修复权限或包名后可重新核验。

仓库另提供手动 GitHub Actions 工作流 `.github/workflows/publish-rust-crates.yml`：先在仓库 Actions secrets
配置 crates.io 上传 token `CARGO_REGISTRY_TOKEN`，然后从**默认分支**运行 `workflow_dispatch`，将 `release_tag`
设为精确的 `vX.Y.Z`，并把确认输入填写为 `qx-sdk@X.Y.Z`。工作流从默认分支读取已审阅的发布器，另行 checkout
精确源码 tag，再执行依赖闭包预检、顺序上传、逐包 checksum 等待与独立 crates.io quickstart。不得从 PR 分支调度，
也不要把 token 写入仓库文件或命令参数。
首次发布会永久占用成功上传的 crate 名称和版本；如果中途失败，先检查 crates.io 已发布记录与 checksum，修复权限或
名称冲突后从同一源码 tag 重新调度，工具只会跳过字节完全一致且未撤回的版本。续发 v0.4.1 时将 `release_tag` 设为
`v0.4.1`、确认值设为 `qx-sdk@0.4.1`。请以 GitHub Actions 的逐包 checksum 和独立 crates.io quickstart 结果作为发布证据。

### 2. 导入前缀

```diff
- use qx_app::{run_backtest, BacktestSpec, CallerCapability, RunContext, verify_run};
+ use qx_sdk::{run_backtest, BacktestSpec, CallerCapability, RunContext, verify_run};
```

一次机械替换即可：`qx-app` 重导出的每一项在 `qx-sdk` 里都有同名同签名的对应物。

### 3. 用 `Client` 收掉手工传的上下文（可选，但推荐）

迁移前，每个用例入口都要手工构造并传第二个参数：

```rust
let context = RunContext::new(CallerCapability::Research, "run-1");
let verdict = validate_dataset(&dataset, &context)?;
let outcome = run_backtest(&spec, &context)?;
let verification = verify_run(&outcome, &context)?;
```

迁移后，身份只构造一次：

```rust
let client = Client::new(CallerCapability::Research, "run-1");
let verdict = client.validate_dataset(&dataset)?;
let outcome = client.run_backtest(&spec)?;
let verification = client.verify_run(&outcome)?;
```

原来的自由函数**仍然在**（`run_backtest` / `validate_dataset` / `verify_run` …），
它们接受 `&RunContext`。想自己管上下文就继续用它们，两种写法可以混用。

### 4. 异步运行

`qx-app` 的 `*Spec::start(self, context)` 在门面里是 `Client::start_backtest(spec)`：

```rust
let mut handle = client.start_backtest(spec);
assert_eq!(handle.wait(), RunStatus::Succeeded);   // 缺省 30 s，有限
let outcome = handle.try_take_result().unwrap()?;
```

**如实说清两件事**：

- 这是**句柄式**异步，不是 `Future`。`qx-app` 的引擎是线程 + 通道，没有 executor；
  门面不套一个假的 `Future` 上去（那会让 `poll` 里出现阻塞等待）。
- `wait()` 的缺省是**有限**的（`DEFAULT_WAIT_TIMEOUT` = 30 s，方案 §9）。到点返回**当前**
  状态且**不**取消运行；要无限等就自己写 `status()` 循环。

## 门面覆盖了 §6.1 的哪些能力

| §6.1 能力 | 门面里的实物 |
|---|---|
| `Client/Engine` | `qx_sdk::Client` |
| capabilities | `qx_sdk::ProviderCapability` |
| spec 构造校验 | `DatasetSpec` / `BacktestSpec` / `DepthBacktestSpec` / `MultiLegBacktestSpec` / `RunExperimentSpec` / `RunDepthExperimentSpec` / `RunMultiLegExperimentSpec` / `CompareRunsSpec` / `TradeControlSpec` / `BuiltinStrategySpec`（+ `BuiltinStrategyKind`、`TradingProduct`、`TradingInstrumentSpec`） |
| sync run | `run_backtest` / `run_depth_backtest` / `run_multi_leg_backtest` / `run_experiment` / `run_depth_experiment` / `run_multi_leg_experiment` / `validate_dataset` / `validate_trade_control` |
| async run | 各 `*Spec::start` → `RunHandle`；门面侧 `Client::start_backtest` |
| `RunHandle` | `RunHandle` / `RunStatus` / `RunEvent` / `RunEventKind` / `DEFAULT_WAIT_TIMEOUT` |
| cancel / wait / result / events | `RunHandle::cancel` / `::wait` / `::try_take_result` / `::events_after(cursor)` |
| report / verify | `verify_run` / `verify_depth_run` / `compare_runs` |
| Provider / Venue 扩展端口 | `DataProvider` / `VenuePort` |
| 统一错误 | `AppError` / `AppErrorCategory` / `AppAction` / `AppRetry` |
| **events / cursors** | Rust/Python 本地 `RunHandle.events_after/events` 读取 Rust worker 生命周期事件；Rust/Python/C++ 远端 RunHandle 读取 `GET /app/runs/{run_id}/events?after_seq=N`；`get_live_events` → `GET /events/live` 仍是全局实时流 |

## Rust SDK Provider 数据载荷 v1

`ProviderResult` v1 将每条 `RawRecord` 与同索引 `ProviderPayload` 配对。载荷声明标的、content type 和 JSON 正文；
`payload_hash` 必须匹配载荷摘要，结果摘要覆盖请求、接收时间、每行血缘、实际载荷和确定性 failover 链。
Registry 还会把结果自述的 Provider ID/版本与实际被调用的 Provider capability 比对，不一致就拒绝并继续按序尝试。

Bar 查询应优先调用 `ProviderRegistry::fetch_bar_frame`：它在同一入口内执行确定性 Provider 选择/failover、
结果身份与血缘验证及单标的 strict 质量门，并同时返回 `ProviderResult` 和标准 `BarFrame`；
需要直接校验已有结果时，`ProviderResult::to_bar_frame` 仍可用。标准化仅接收
`application/vnd.qianxing.bar+json;v=1` 行载荷；校验时间戳、OHLCV 和载荷标的，再生成标准 BarFrame，
其来源标识绑定查询摘要与 Provider 结果摘要。仓外消费者验收覆盖 Provider→BarFrame→共享 Rust 回测→产物复核，
见 `crates/qx-sdk/tests/provider_consumer.rs`。

Rust 与 Python SDK 均已有 Provider 到共享回测内核的路径。Python adapter 只提供原始行，Rust 注册器负责选择、failover、来源/载荷摘要和 Bar 质量门。`DataRequest`/`DataRequirements` 在不改变既有 `DataQuery` 结构与 JSON 的前提下，为新调用增加授权范围、实时来源和毫秒新鲜度要求；注册表执行候选筛选与结果校验。`ProviderRegistry.run_backtest` 将经过验证的 BarFrame 冻结至 `BacktestSpec.bars_path` 并调用同一 `qx-app` 用例。Python 端到端用例见 `python/tests/test_provider_sdk.py`。

Bar Provider 的 `ProviderBarFrame::dataset_manifest(...)` 与 Python `fetch_bar_dataset(...)` 现在共用 `qx-data::manifest_from_bar_frame`，生成相同 `DatasetManifestV2` 指纹、内容 SHA256、质量缺口和来源血缘。Provider Bar/Depth 回测规格都要求完整快照上下文，并在 outcome 与可复算报告里保留 manifest/snapshot sidecar 身份。Rust 与 Python SDK 都有一步式冻结→DatasetSpec 校验→共享回测入口；Rust 仓外消费者还验证 sidecar 清单、回测结果和 ReplayVerifier。Quote Provider 也可归一为 Rust `DepthFrame` 并运行 Tick/OrderBook 回测。

受信 API 宿主可通过 `ApiService::with_provider_registry(Arc<ProviderRegistry>)` 注入部署管理的注册表，随后调用 `POST /app/provider-backtest`；请求体是 `ProviderBacktestSpec`，必须包含完整的 `DatasetSnapshotContextV1`，响应序列化同一 `ProviderBacktestOutcome`，其中含已发布的 snapshot 与 sidecar 路径。`qx-cli app provider-backtest <RuntimeConfig.json> <ProviderBacktestSpec.json>` 也从 `api.data_providers` 构造同一 Rust 注册表并直接调用 `qx-app::run_provider_backtest_with_manifest`，输出同一 outcome 文档；不会在 CLI 或 Python 侧复制数据选择、清单或回测逻辑。API 服务把数据和产物路径固定在 artifact root，远程请求不能自行指定文件位置。注册表未配置时两入口均失败关闭；API 返回 `DataUnavailable`。当前配置适配器为 CCXT 历史 Bar，实时声明会被拒绝。分页/实时订阅、盘口增量合并与各真实数据源 conformance 仍未闭环，不能把它们写成已完成。

## 已知缺口（不要在发布文案里说成"已有"）

**crates.io 状态更新（2026-11-22，已完成）：** PR [#13](https://github.com/coeasy/qianxing/pull/13) 已合并。发布工作流 [run #4](https://github.com/coeasy/qianxing/actions/runs/45019067095) 从默认分支读取发布器、从不可变 `v0.4.1` tag 读取源码，依序发布了 14 个 crate，并逐包核对 canonical SHA-256 与 crates.io。工作流还在独立 Cargo home 中从 crates.io 安装 `qx-sdk 0.4.1`，运行 70 根教学 Bar 快速入门并复核结果：1 笔成交，`Verified: true`。由于 crates.io 上传限流，工作流按服务器给出的时间从同一 tag 安全续跑；已确认 checksum 一致的版本会跳过，不会重复上传。样例使用教学数据，不代表真实历史行情或交易所验收。

已发布版本的跨平台 clean-consumer 检查可从默认分支运行 [Rust SDK crates.io consumer matrix](../.github/workflows/rust-sdk-registry-consumer.yml)：传入精确版本号后，会在独立 Cargo home 中从 registry 安装并运行同一 quickstart，覆盖 Ubuntu、Windows 和 macOS。`qx-sdk 0.4.1` 的三平台验收已通过 [workflow run #45030066158](https://github.com/coeasy/qianxing/actions/runs/45030066158)：每个平台均验证 70 根 Bar、1 笔成交和 `Verified: true`。当前 `qx-sdk` 未声明额外 Cargo features，因此此矩阵不把空的 `--no-default-features` 组合包装成独立能力。

当前源码中的 14-crate crates.io SDK 闭包统一声明 `rust-version = "1.87"`，发布分类门禁会检查每个包与目录台账一致，CI 在 Rust 1.87 上编译 `qx-sdk` 及其完整依赖闭包。选择该下限是因为 SDK 闭包使用的无符号整数 `is_multiple_of` 从 Rust 1.87 起稳定。此元数据和验收只适用于合入后的源码候选；已发布的 `qx-sdk 0.4.1` artifact 不会被追溯修改，仍需在后续版本发布后以 registry consumer 再验收该 MSRV。

## Rust 远端 Paper 算法生命周期与事件游标

`qx_sdk::RemoteApiClient<T>` 提供 Paper TWAP/VWAP 的 start/get/cancel/list 四个方法，
与 Python 和 C++ 共用 `/app/paper/algo-executions/*` 路由。宿主实现 `RemoteTransport`
并提供 HTTP、鉴权、TLS 和超时；有副作用的请求不应由 transport 自动重试。客户端负责请求编码、路由选择和响应解析，
执行状态与订单行为仍由服务端 Rust runtime 管理。返回值使用 `serde_json::Value`，以便
SDK 跟随服务端契约扩展而不复制领域模型。错误会保留 transport 错误、HTTP 状态/响应体或 JSON 解析错误。

启动/取消 Run、执行回测及启动/取消 Paper 算法时，传输失败、HTTP 408/409、非 4xx 的非成功响应或无法确认的成功响应会返回
`RemoteApiError::OutcomeUnknown { operation, recovery_id, cause }`。它表示服务端可能已经处理请求：
应使用 `recovery_id` 查询对应 Run 状态或 Paper 父单，结合服务端状态和事件核对后再决定下一步；
一次查询返回 404 也不自动证明重试安全。其他 HTTP 错误仍保留为 `RemoteApiError::Http`，
不能仅凭状态码断言服务端没有副作用；读取操作保留普通传输/解析错误。此新增错误变体会影响穷举匹配 `RemoteApiError` 的 Rust 调用方，
升级时需要增加该分支。这里的状态查询不等同于真实交易所对账或实盘验收。
客户端会检查 Run 状态和 Paper 父单响应中的身份及状态；Bar 回测和成功的产物复核响应还会核对所请求的
运行身份及结果指纹。身份不一致不能当作成功，仍需核查服务端和网络链路。

最小接入形状：

```rust,ignore
let remote = qx_sdk::RemoteApiClient::new(my_http_transport);
let snapshot = remote.start_paper_algo_execution(&request_json)?;
let current = remote.get_paper_algo_execution("parent-1")?;
```

`RemoteApiClient` 当前覆盖明确登记的 Paper 算法生命周期和实时事件游标读取；它不是通用 HTTP Client，
也不替代进程内 `qx_sdk::Client` 的回测/实验用例。Rust Live 下单仍须经过服务端授权与
Live/testnet 验收，不可据此宣称已发布可实盘 SDK。

## Python 远端副作用、恢复与传输边界

Python `AppClient`、`PaperClient` 与 `LiveClient` 共用有界 JSON HTTP transport。它不会跟随
HTTP 重定向，避免把授权头或写请求转发到另一个地址；单个响应体上限为 16 MiB，并拒绝冲突的
`Content-Length` / `Transfer-Encoding`、截断响应及非标准 NaN/Infinity JSON 数值。超限配置、
请求编码错误等在发出网络请求前失败，不会伪装成远端不确定结果。

远端副作用请求只发送一次，不自动重试。连接中断、超时、HTTP 408/409、服务端 5xx、成功响应
无法解析/校验，或回执身份不匹配时，Python 返回 `OutcomeUnknownError`，并提供 `operation`、
`recovery_id`、`cause` 与 `retry=False`。回测/实验的 recovery ID 是请求中的运行、实验或批次编号；
Paper/Live 控制命令使用 `request_id`。Run 可通过 `RemoteRunHandle(client, run_id)` 查询；控制命令
可通过 Rust `RemoteApiClient::get_command_audit_by_request_id(request_id)`、C++
`AppClient::get_command_audit(request_id)`，或 Python `PaperClient.get_command_audit(request_id)` /
`LiveClient.get_command_audit(request_id)` 在服务端审计记录中核对。回执统一为
`ControlAuditRecordV1`（schema registry 登记的 v1 合同）：Rust SDK 强类型解码，Python 校验版本、字段集合、整数范围、状态枚举和请求身份；HTTP `POST /app/paper/commands`、`POST /app/live/commands` 返回单条回执。`GET /control/audit` 返回 `ControlAuditPageV1`：最新优先、缺省 100 条、最大 1,000 条且序列化后不超过 1 MiB；`next_cursor` 由持久化单调序号和首屏水位构成，可跨 API 重启续读，且并发新命令不会延长既有扫描。提供 `request_id` 时服务端只返回该请求的受理/终态记录；cursor 因记录退场而过期时明确回 `409 control_audit_cursor_expired`，单条记录超过字节预算时回 `413 control_audit_record_exceeds_response_budget`。调用方遇到过期游标应从新首屏重新扫描。Rust 的 `get_command_audit_page` 与 Python `get_command_audit_page` 提供单页读取；便捷的 `get_command_audit` 收集上限为 10,000 条，超限时应改用分页接口。该接口不是永久审计归档导出。HTTP 4xx 错误仍按普通 API 错误返回，但客户端不把
HTTP 状态码解释为“无副作用”的普遍证明；一次查询 404 也不构成可以安全重试的证明。

Run 事件接口按真实 JSON 数组读取，校验运行身份、事件类型、严格递增的 u64 游标，并支持
`events(after_seq)` 的 exclusive 游标。异步启动被取消时，SDK 会在有限时长内尝试收取启动结果并
请求取消；未能确认的清理错误挂在 `CancelledError.remote_cleanup_error`（支持的 Python 版本也会
附加异常说明）。底层 `urllib` 阻塞线程不能被强制终止，因此调用方仍应保留运行/请求编号并查询服务端。
真实 Rust API contract test 覆盖回测与 Paper 命令的已受理后丢失响应恢复；这不是真实交易所、
testnet、生产网络或并发高压验收。

Rust、Python 与 C++ 远端客户端都提供 `get_live_events(after_seq)`，对应 `GET /events/live`。
传入游标时按 `after=<sequence>` 请求；服务器返回的 409（游标过旧或超前，需要重新取快照）
分别保留为 Rust `RemoteApiError::Http`、Python `QianxingError`、C++ 原始 `AppResponse`。
Rust transport 需实现 `RemoteTransport::get`；Python/C++ 复用各自已有的 GET transport。

异步应用 Run 另提供按身份隔离的事件读取：`GET /app/runs/{run_id}/events?after_seq=N`，游标为
exclusive（仅返回 `seq > N`）。Rust `RemoteApiClient::get_run_events`、Python
`RemoteRunHandle.events/async_events` 与 C++ `AppClient::run_events` 共用该路由。事件随现有
运行快照保存，重启后可读取最近已持久化的生命周期序列；快照由 watcher 周期更新，因此这不是逐事件
事务日志，也不提供订单/成交事件、强一致 append 或过期游标恢复协议。

1. **RunHandle 的事件仅覆盖生命周期。** Rust/Python 本地句柄从 Rust worker 读取
   started / cancellation_requested / terminal 事件；远端按 Run 事件随运行快照保存，但不保证逐事件原子落盘。
   订单/成交级事实事件与账簿变更仍不在此事件列表中；`/events/live` 依然是全局服务端流。
2. **历史发布前快照（2026-11-21）：当时 Git 安装已做独立消费者验收，crates.io 上传与 registry consumer 验收尚未完成。**
   `python tools/check_rust_sdk_git_consumer.py` 会从 GitHub 拉取当前完整提交哈希，在 workspace 外构建真实消费者并运行数据校验、回测和结果复核。
   `python tools/cargo_publish_plan.py qx-sdk` 当前生成包含 14 个包的拓扑顺序；
   `python tools/check_rust_sdk_packages.py qx-sdk --offline --allow-dirty` 已离线打包并构建完整闭包，
   再用独立 quickstart 从这些归档运行回测与结果复核。SDK 的 Cargo 包排除 `.task-tmp/` 与 `.tmp-test/`，
   包归档验证器会拒绝临时目录和缓存文件。2026-11-21 通过只读 crates.io API 对这 14 个 0.4.0 归档逐一核验，
   `qx-core`、`qx-control`、`qx-guanxing`、`qx-datastruct`、`qx-data`、`qx-provider`、`qx-risk`、
   `qx-spec`、`qx-strategy`、`qx-zhenlu`、`qx-xingban`、`qx-app`、`qx-execution`、`qx-sdk` 返回状态均为
   `crate_name_not_found_at_query_time`：当时没有任何同名 crate 的公开注册记录。
   这不代表名称已保留给本项目、名称一定可注册或账户有上传权；仍须以实际注册和发布权限为准。
    checksum 核验器当时可在版本记录存在时拦截已撤回版本和同版本异构件。该次只读检查早于随后发布的 `qx-core 0.4.1` 和
    `qx-control 0.4.1`；发布后的部分失败与当前续发步骤以上方状态为准。本机 Cargo dry-run 曾被全局 `crates-io → tuna`
    源替换及索引连接问题挡住，故当时没有完成正式 registry 解析/安装链，也没有上传任何 crate。
3. **统一启动能力预检尚未闭环。** 现有能力是分层实现的：`BacktestSpec::validate` 校验显式产品规格、
   费用/成本模型与 tick/lot 约束；Provider `DataRequest` 有不发起 I/O 的候选匹配；Live 服务提交前有
   精确订单形状/成熟度预检，私有账户读取也有操作范围预检。缺口是没有一个面向用户的统一 readiness 报告，
   把数据源候选、venue/产品/账户模式、市场规则有效期、身份与风险输入、订阅/账户权限汇总成一份启动结果；
   上述纯目录预检也不验证凭据是否可用、上游是否连通或 testnet 实际验收。对应各局部检查仍需保留 fail-closed。
4. **Live 与 testnet 未验收。** 门面不改变实盘授权边界：`CallerCapability::Live` 仍是默认关闭的独立授权。
   最新 CI 的 Binance testnet 作业只运行无凭据 fail-closed 检查；API key/secret 未配置，所以真实 testnet 步骤被跳过，
   不构成订单 conformance 证据。OKX/Bitget 当前通过的是无凭据的公共历史行情回测，不是 testnet 或生产下单验收。

## 兼容性承诺

- 门面跟随 workspace 版本（`version.workspace = true`），不单独发版。
- 门面**不做**语义隔离层：`qx-app` 的 API 变动会同时是门面的变动。这是有意的——多一层转发
  就多一份语义，而本仓反复栽过的正是"两份说法"。
- breaking change 按方案 §10 走：先新增、旧入口委托新用例、给至少一个小版本的迁移窗。

## 多账户本金和结算币种

运行时配置可选声明 `accounts: [{ id, settlement_currency, initial_cash_raw }]`。多策略 Bar 回测
按策略 `account_id` 选择本金；Paper 执行按账户注册表播种初始现金，并用账户/币种稳定键避免
同一账户的多个执行 worker 重复注资。未使用注册表的旧配置保留原 worker 级幂等键，升级重启不会
重复注入已有本金。账户引用必须在注册表中，显式本金须为正数；账户币种和
兼容字段若同时声明则必须一致。省略 `accounts` 时保留旧 worker/strategy 配置和序列化指纹。

当前账户 EventLog 按账户与交易所拆分，因此同一账户注册项配置初始本金并绑定多个交易所账簿会
被拒绝，避免把全部本金重复注入多个独立账本。账户本金隔离已接通；组合级资金竞争、风险聚合、
调度分配和跨交易所账户统一账本仍未实现，不能按完整组合账户能力发布。

## 多腿固定名义金额 sizing

多腿策略使用 `BuiltinStrategyConfig::target_notional_raw` 时，必须为两腿各自提供冻结的
`TradingInstrumentSpec`（`primary_market_spec`、`reference_market_spec`）。目前只允许永续/交割合约，
两腿必须使用相同报价币与结算币；线性合约以报价币结算，反向合约以基础币结算。数量按合约面值、
可见价格和 `qty_step` 向下取整，并校验最小数量与各腿未杠杆预算。现货做空借币、动态保证金/波动率/
Kelly sizing、跨币种资金换算和组合保证金仍未实现，缺少前置规格时会失败关闭。

## Provider market-data tiers

`ProviderCapability` must declare `data_tiers` for market payloads: `MarketDataTier::Bar` for Bar,
`L1` or `L2` for Quote. A `DataRequest` may set `DataRequirements.required_data_tier`; the registry filters
providers by that declaration before I/O. Bar/Quote tier declarations inconsistent with their `DataKind` are
rejected at registration. Provider depth backtests require `l1`/`l2` requests to match the backtest tier exactly.
