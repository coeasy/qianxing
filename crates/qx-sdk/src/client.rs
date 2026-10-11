//! 门面的调用句柄（方案 §6.1 点名的 `Client/Engine`）。
//!
//! 它做的事只有一件：把**调用方身份**（[`RunContext`]：能力档 + correlation id）收进一个值，
//! 让第三方不必在每个用例入口手工传第二个参数。真正的判定、装配、产物全部仍住在 `qx-app`
//! 的用例里——门面里没有任何一行业务逻辑，这是 `qx_sdk_facade_check` 第 ② 颗守的东西。
//!
//! 为什么不把 `Client` 做成 trait 或者泛型 `Engine`：门面是**稳定边界**，泛型参数一旦公开
//! 就进了 semver 承诺。一个持有 `RunContext` 的普通结构体把承诺面压到最小。

use qx_app::{
    run_backtest, validate_dataset, verify_run, AppError, BacktestOutcome, BacktestSpec,
    CallerCapability, DatasetSpec, DatasetVerdict, RunContext, RunHandle, VerificationResult,
};

/// 一次 SDK 会话的调用身份。
///
/// 最小上手见 `examples/quickstart.rs`：
///
/// ```text
/// let client = Client::new(CallerCapability::Research, "run-1");
/// let verdict = client.validate_dataset(&dataset)?;
/// let outcome = client.run_backtest(&spec)?;
/// let verification = client.verify_run(&outcome)?;
/// ```
///
/// 这里刻意**不**用 `no_run` 文档块：本沙箱的 `rustdoc --test` 起不来 rustc
/// （`os error 231 ERROR_PIPE_BUSY`），而全仓此前也没有任何一份可运行的文档示例。
/// 示例改成 `examples/quickstart.rs`——`cargo test --workspace` 会**编译**它，
/// 证据强度比 doctest 高而不是低。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Client {
    context: RunContext,
}

impl Client {
    /// 以能力档与 correlation id 建一个会话。
    ///
    /// `correlation_id` 通常就是 `run_id`：用例返回的每条 `AppError` 都带着它，
    /// 所以同一场失败在 CLI / Python / HTTP / SDK 四处拿到的是同一枚 id。
    pub fn new(capability: CallerCapability, correlation_id: impl Into<String>) -> Self {
        Self {
            context: RunContext::new(capability, correlation_id),
        }
    }

    /// 本会话的上下文。想直接调被重导出的自由函数时，把它传进去。
    pub fn context(&self) -> &RunContext {
        &self.context
    }

    /// 让门面把自己的构建身份带进来（默认是包版本）。
    pub fn with_code_commit(mut self, code_commit: impl Into<String>) -> Self {
        self.context = self.context.with_code_commit(code_commit);
        self
    }

    /// 数据可用性裁定。`usable: false` 是**结论**不是错误。
    pub fn validate_dataset(&self, spec: &DatasetSpec) -> Result<DatasetVerdict, AppError> {
        validate_dataset(spec, &self.context)
    }

    /// 同步回测：阻塞到终态，返回四份产物与读数。
    pub fn run_backtest(&self, spec: &BacktestSpec) -> Result<BacktestOutcome, AppError> {
        run_backtest(spec, &self.context)
    }

    /// 异步回测：立刻返回可协作取消的 [`RunHandle`]，由调用方决定等多久。
    ///
    /// 这是本门面的"异步"形态——**句柄式**，不是 `Future`。理由如实写在 crate 文档里：
    /// `qx-app` 的引擎是线程 + 通道，没有 executor；硬套一个 `Future` 只会让 `poll` 里
    /// 出现阻塞等待。
    pub fn start_backtest(&self, spec: BacktestSpec) -> RunHandle<BacktestOutcome> {
        spec.start(self.context.clone())
    }

    /// 独立复核一次回测的四份产物。被篡改的读数判**失败**而不是报错。
    pub fn verify_run(&self, outcome: &BacktestOutcome) -> Result<VerificationResult, AppError> {
        verify_run(outcome, &self.context)
    }
}
