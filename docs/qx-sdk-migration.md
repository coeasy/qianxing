# 从 `qx-app` 迁到 `qx-sdk`（方案 §6.1 迁移指南）

## 这份文档回答什么

方案 §6.1 要求第三方稳定边界"有 semver、迁移指南、examples 和独立消费者构建"。examples 在
`crates/qx-sdk/examples/quickstart.rs`，独立消费者在 `crates/qx-sdk/tests/independent_consumer.rs`，
本文是迁移指南。

## 要不要迁

**要迁**，如果你属于下面任一种：

- 你把牵星嵌进自己的 Rust 程序，而不是用 `qx-cli`；
- 你在 `Cargo.toml` 里直接写了 `qx-app = { path = ... }`（或将来某个版本号）；
- 你想接自己的数据源 / 交易所。

**不用迁**，如果你在改这个仓库本身：`qx-cli` / `qx-api` / `qx-python` 仍然直接依赖 `qx-app`，
门面**不是**它们的中间层。门面只服务仓库外的调用方。

## 迁什么

### 1. 依赖

```diff
- qx-app = "0.1"
+ qx-sdk = "0.1"
```

仓库外消费者在内部 crate 尚未发布到公开 registry 时，可以直接锁定已推送的 Git 提交；SDK 及其同仓路径依赖都从同一个不可变提交解析：

```toml
[dependencies]
qx-sdk = { git = "https://github.com/coeasy/qianxing.git", rev = "e9fa6ab23aa3f7a7dafbe50265674b8ffd24954d", package = "qx-sdk" }
```

该 Git 来源示例固定到包含 Paper 算法生命周期和 SDK facade 的提交。新版本发布时应替换为对应的完整提交哈希，并提交 `Cargo.lock`。验证独立消费者可运行：

```powershell
python tools/check_rust_sdk_git_consumer.py
```

此检查创建 workspace 外的临时 Cargo 工程，以当前 `HEAD` 的完整 Git SHA 作为依赖，构建后实际调用数据校验、Rust 回测与结果复核。它验证 Git 安装路径，不等同 crates.io 发布；发布到 crates.io 仍需先发布内部 crate 依赖并通过 `cargo package` 与 registry consumer 验收。

`qx-sdk` 的公开面是 `qx-app` 公开面的**超集**，所以迁移期内两个依赖并存也不会冲突；
但迁完之后应当把 `qx-app` 从 `Cargo.toml` 里删掉——留着它意味着"我还在用内部层"。

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
| spec 构造校验 | `DatasetSpec` / `BacktestSpec` / `DepthBacktestSpec` / `MultiLegBacktestSpec` / `RunExperimentSpec` / `RunDepthExperimentSpec` / `RunMultiLegExperimentSpec` / `CompareRunsSpec` / `TradeControlSpec` / `BuiltinStrategySpec`（+ `BuiltinStrategyKind`） |
| sync run | `run_backtest` / `run_depth_backtest` / `run_multi_leg_backtest` / `run_experiment` / `run_depth_experiment` / `run_multi_leg_experiment` / `validate_dataset` / `validate_trade_control` |
| async run | 各 `*Spec::start` → `RunHandle`；门面侧 `Client::start_backtest` |
| `RunHandle` | `RunHandle` / `RunStatus` / `RunEvent` / `RunEventKind` / `DEFAULT_WAIT_TIMEOUT` |
| cancel / wait / result / events | `RunHandle::cancel` / `::wait` / `::try_take_result` / `::events_after(cursor)` |
| report / verify | `verify_run` / `verify_depth_run` / `compare_runs` |
| Provider / Venue 扩展端口 | `DataProvider` / `VenuePort` |
| 统一错误 | `AppError` / `AppErrorCategory` / `AppAction` / `AppRetry` |
| **events / cursors** | Rust `RunHandle::events_after(cursor)` 与 Python `RunHandle.events(after_seq)` 读取 Rust worker 的有序本地生命周期事件；Rust/Python/C++ `get_live_events(after_seq)` → `GET /events/live` 读取服务端实时流 |

## 已知缺口（不要在发布文案里说成"已有"）

## Rust 远端 Paper 算法生命周期与事件游标

`qx_sdk::RemoteApiClient<T>` 提供 Paper TWAP/VWAP 的 start/get/cancel/list 四个方法，
与 Python 和 C++ 共用 `/app/paper/algo-executions/*` 路由。宿主实现 `RemoteTransport`
并提供 HTTP、鉴权、TLS、超时与重试策略；客户端只负责请求编码、路由选择和响应解析，
执行状态与订单行为仍由服务端 Rust runtime 管理。返回值使用 `serde_json::Value`，以便
SDK 跟随服务端契约扩展而不复制领域模型。错误会保留 transport 错误、HTTP 状态/响应体或 JSON 解析错误。

最小接入形状：

```rust,ignore
let remote = qx_sdk::RemoteApiClient::new(my_http_transport);
let snapshot = remote.start_paper_algo_execution(&request_json)?;
let current = remote.get_paper_algo_execution("parent-1")?;
```

`RemoteApiClient` 当前覆盖明确登记的 Paper 算法生命周期和实时事件游标读取；它不是通用 HTTP Client，
也不替代进程内 `qx_sdk::Client` 的回测/实验用例。Rust Live 下单仍须经过服务端授权与
Live/testnet 验收，不可据此宣称已发布可实盘 SDK。

Rust、Python 与 C++ 远端客户端都提供 `get_live_events(after_seq)`，对应 `GET /events/live`。
传入游标时按 `after=<sequence>` 请求；服务器返回的 409（游标过旧或超前，需要重新取快照）
分别保留为 Rust `RemoteApiError::Http`、Python `QianxingError`、C++ 原始 `AppResponse`。
Rust transport 需实现 `RemoteTransport::get`；Python/C++ 复用各自已有的 GET transport。

1. **进程内 RunHandle 只提供易失生命周期事件。** Rust/Python 本地句柄从 Rust worker 读取
   started / cancellation_requested / terminal 事件和序号游标；订单/成交级事件持久化、按 Run 查询、
   进程重启后游标恢复仍未实现。远端 `/events/live` 是服务端全局流，并非本地 Run 日志。
2. **Git 安装已做独立消费者验收，crates.io 发布验收仍未做。** `python tools/check_rust_sdk_git_consumer.py`
   会从 GitHub 拉取当前完整提交哈希，在 workspace 外构建真实消费者并运行数据校验、回测和结果复核。
   `cargo package -p qx-sdk` 仍因 `qx-app` 等内部 crate 尚未发布至配置的 registry 而失败；
   crates.io 发布前还需依赖拓扑发布和临时 registry 消费验收。
3. **能力预检未落地。** 方案 §4.2 要求"启动前对规格做能力匹配，缺少费用表 / tick-lot /
   时间范围 / 身份 / 风险规则 / 订阅权限时 fail closed"——这一层还没有。
4. **Live 未验收。** 门面不改变实盘授权边界：`CallerCapability::Live` 仍是默认关闭的独立授权，
   且没有任何 testnet conformance 记录。

## 兼容性承诺

- 门面跟随 workspace 版本（`version.workspace = true`），不单独发版。
- 门面**不做**语义隔离层：`qx-app` 的 API 变动会同时是门面的变动。这是有意的——多一层转发
  就多一份语义，而本仓反复栽过的正是"两份说法"。
- breaking change 按方案 §10 走：先新增、旧入口委托新用例、给至少一个小版本的迁移窗。
