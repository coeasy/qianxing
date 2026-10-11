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
| `RunHandle` | `RunHandle` / `RunStatus` / `DEFAULT_WAIT_TIMEOUT` |
| cancel / wait / result | `RunHandle::cancel` / `::wait` / `::try_take_result` |
| report / verify | `verify_run` / `verify_depth_run` / `compare_runs` |
| Provider / Venue 扩展端口 | `DataProvider` / `VenuePort` |
| 统一错误 | `AppError` / `AppErrorCategory` / `AppAction` / `AppRetry` |
| **events / cursors** | **没有**——见下 |

## 已知缺口（不要在发布文案里说成"已有"）

1. **events / cursors 未提供。** `qx-app` 里没有事件日志，运行进度只能靠
   `RunHandle::status()` 轮询；远程 Run 的事件游标住在 `qx-api`（HTTP 面），不在 SDK 里。
   门面刻意**不**造一个没有生产端的事件流。缺口登记在 `maturity/capability-matrix.yaml`
   的 `rust_sdk` 行。
2. **发布验收未做。** `cargo package` 与临时本地 registry 消费、以及把路径依赖转成版本依赖，
   都还没跑过。当前门面只在 workspace 内被消费。
3. **能力预检未落地。** 方案 §4.2 要求"启动前对规格做能力匹配，缺少费用表 / tick-lot /
   时间范围 / 身份 / 风险规则 / 订阅权限时 fail closed"——这一层还没有。
4. **Live 未验收。** 门面不改变实盘授权边界：`CallerCapability::Live` 仍是默认关闭的独立授权，
   且没有任何 testnet conformance 记录。

## 兼容性承诺

- 门面跟随 workspace 版本（`version.workspace = true`），不单独发版。
- 门面**不做**语义隔离层：`qx-app` 的 API 变动会同时是门面的变动。这是有意的——多一层转发
  就多一份语义，而本仓反复栽过的正是"两份说法"。
- breaking change 按方案 §10 走：先新增、旧入口委托新用例、给至少一个小版本的迁移窗。
