//! 门面的调用句柄（方案 §6.1 点名的 `Client/Engine`）。
//!
//! 它做的事只有一件：把**调用方身份**（[`RunContext`]：能力档 + correlation id）收进一个值，
//! 让第三方不必在每个用例入口手工传第二个参数。真正的判定、装配、产物全部仍住在 `qx-app`
//! 的用例里——门面里没有任何一行业务逻辑，这是 `qx_sdk_facade_check` 第 ② 颗守的东西。
//!
//! 为什么不把 `Client` 做成 trait 或者泛型 `Engine`：门面是**稳定边界**，泛型参数一旦公开
//! 就进了 semver 承诺。一个持有 `RunContext` 的普通结构体把承诺面压到最小。

use crate::{
    ProviderBacktestOutcome, ProviderBacktestSpec, ProviderDepthBacktestOutcome,
    ProviderDepthBacktestSpec, ProviderRegistry,
};
use qx_app::{
    compare_runs, plan_algo_execution, plan_capital_allocation, run_backtest, run_depth_backtest,
    run_depth_experiment, run_experiment, run_multi_account_backtest, run_multi_leg_backtest,
    run_multi_leg_experiment, validate_dataset, verify_depth_run, verify_run, AlgoExecutionSpec,
    AppError, BacktestOutcome, BacktestSpec, CallerCapability, CapitalAllocationResult,
    CapitalAllocationSpec, CompareRunsResult, CompareRunsSpec, DatasetSpec, DatasetVerdict,
    DepthBacktestOutcome, DepthBacktestSpec, MultiAccountBacktestResult, MultiAccountBacktestSpec,
    MultiLegBacktestOutcome, MultiLegBacktestSpec, RiskParityResult, RiskParitySpec, RunContext,
    RunDepthExperimentResult, RunDepthExperimentSpec, RunExperimentResult, RunExperimentSpec,
    RunHandle, RunMultiLegExperimentResult, RunMultiLegExperimentSpec, StrategyArtifactIdentity,
    VerificationResult,
};
use qx_strategy::Strategy;

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

    /// Plan deterministic TWAP/VWAP child orders through the shared app use case.
    /// The returned plan does not submit orders or bypass Risk/OMS.
    pub fn plan_algo_execution(
        &self,
        spec: &AlgoExecutionSpec,
    ) -> Result<qx_app::AlgoExecutionPlan, AppError> {
        plan_algo_execution(spec, &self.context)
    }

    /// Plan deterministic portfolio cash allocations without submitting orders.
    pub fn plan_capital_allocation(
        &self,
        spec: &CapitalAllocationSpec,
    ) -> Result<CapitalAllocationResult, AppError> {
        plan_capital_allocation(spec, &self.context)
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

    /// Run caller-authored strategy logic inside the shared deterministic Bar engine.
    /// Risk, OMS, matching, accounting, artifacts and replay verification use the same
    /// qx-app path as built-in strategies.
    pub fn run_backtest_with_strategy<S: Strategy + 'static>(
        &self,
        spec: &BacktestSpec,
        identity: &StrategyArtifactIdentity,
        strategy: S,
    ) -> Result<BacktestOutcome, AppError> {
        spec.run_with_strategy(identity, strategy, &self.context)
    }

    /// Start a cancellable caller-authored strategy run on the shared Rust Bar engine.
    pub fn start_backtest_with_strategy<S: Strategy + 'static>(
        &self,
        spec: BacktestSpec,
        identity: StrategyArtifactIdentity,
        strategy: S,
    ) -> Result<RunHandle<BacktestOutcome>, AppError> {
        spec.start_with_strategy(identity, strategy, self.context.clone())
    }

    /// Freeze a Provider BarFrame and its shared manifest, validate it, then run the same app use case.
    pub fn run_provider_backtest_with_manifest(
        &self,
        registry: &ProviderRegistry,
        spec: &ProviderBacktestSpec,
    ) -> Result<ProviderBacktestOutcome, AppError> {
        qx_app::run_provider_backtest_with_manifest(registry, spec, &self.context)
    }

    /// Freeze Provider Quote snapshots with lineage and run the shared Rust depth backtest.
    pub fn run_provider_depth_backtest(
        &self,
        registry: &ProviderRegistry,
        spec: &ProviderDepthBacktestSpec,
    ) -> Result<ProviderDepthBacktestOutcome, AppError> {
        qx_app::run_provider_depth_backtest(registry, spec, &self.context)
    }

    /// 异步回测：立刻返回可协作取消的 [`RunHandle`]，由调用方决定等多久。
    ///
    /// 这是本门面的"异步"形态——**句柄式**，不是 `Future`。理由如实写在 crate 文档里：
    /// `qx-app` 的引擎是线程 + 通道，没有 executor；硬套一个 `Future` 只会让 `poll` 里
    /// 出现阻塞等待。
    pub fn start_backtest(&self, spec: BacktestSpec) -> RunHandle<BacktestOutcome> {
        spec.start(self.context.clone())
    }

    /// Tick/OrderBook 回测，复用 Rust 深度撮合与产物复核链。
    pub fn run_depth_backtest(
        &self,
        spec: &DepthBacktestSpec,
    ) -> Result<DepthBacktestOutcome, AppError> {
        run_depth_backtest(spec, &self.context)
    }

    pub fn start_depth_backtest(&self, spec: DepthBacktestSpec) -> RunHandle<DepthBacktestOutcome> {
        spec.start(self.context.clone())
    }

    /// 双腿多市场 Bar 回测，费用、撮合和风险仍由共享 Rust 用例执行。
    pub fn run_multi_leg_backtest(
        &self,
        spec: &MultiLegBacktestSpec,
    ) -> Result<MultiLegBacktestOutcome, AppError> {
        run_multi_leg_backtest(spec, &self.context)
    }

    pub fn start_multi_leg_backtest(
        &self,
        spec: MultiLegBacktestSpec,
    ) -> RunHandle<MultiLegBacktestOutcome> {
        spec.start(self.context.clone())
    }

    /// Run independent account Bar backtests through the shared Rust app use case.
    pub fn run_multi_account_backtest(
        &self,
        spec: &MultiAccountBacktestSpec,
    ) -> Result<MultiAccountBacktestResult, AppError> {
        run_multi_account_backtest(spec, &self.context)
    }

    /// Start a batch with cooperative cancellation between accounts and inside
    /// the currently active Rust Bar engine run.
    pub fn start_multi_account_backtest(
        &self,
        spec: MultiAccountBacktestSpec,
    ) -> RunHandle<MultiAccountBacktestResult> {
        spec.start(self.context.clone())
    }

    /// 单腿参数实验，可配置样本外/走查与基准对照。
    pub fn run_experiment(
        &self,
        spec: &RunExperimentSpec,
    ) -> Result<RunExperimentResult, AppError> {
        run_experiment(spec, &self.context)
    }

    pub fn start_experiment(&self, spec: RunExperimentSpec) -> RunHandle<RunExperimentResult> {
        spec.start(self.context.clone())
    }

    pub fn run_depth_experiment(
        &self,
        spec: &RunDepthExperimentSpec,
    ) -> Result<RunDepthExperimentResult, AppError> {
        run_depth_experiment(spec, &self.context)
    }

    pub fn start_depth_experiment(
        &self,
        spec: RunDepthExperimentSpec,
    ) -> RunHandle<RunDepthExperimentResult> {
        spec.start(self.context.clone())
    }

    pub fn run_multi_leg_experiment(
        &self,
        spec: &RunMultiLegExperimentSpec,
    ) -> Result<RunMultiLegExperimentResult, AppError> {
        run_multi_leg_experiment(spec, &self.context)
    }

    /// Deterministic long-only risk-parity rebalance planning on frozen aligned returns.
    pub fn plan_risk_parity(&self, spec: &RiskParitySpec) -> Result<RiskParityResult, AppError> {
        qx_app::plan_risk_parity(spec, &self.context)
    }

    pub fn start_multi_leg_experiment(
        &self,
        spec: RunMultiLegExperimentSpec,
    ) -> RunHandle<RunMultiLegExperimentResult> {
        spec.start(self.context.clone())
    }

    /// 对已完成产物做跨运行可比性检查和稳定排名。
    pub fn compare_runs(&self, spec: &CompareRunsSpec) -> Result<CompareRunsResult, AppError> {
        compare_runs(spec, &self.context)
    }

    /// 独立复核一次回测的四份产物。被篡改的读数判**失败**而不是报错。
    pub fn verify_run(&self, outcome: &BacktestOutcome) -> Result<VerificationResult, AppError> {
        verify_run(outcome, &self.context)
    }

    pub fn verify_depth_run(
        &self,
        outcome: &DepthBacktestOutcome,
    ) -> Result<VerificationResult, AppError> {
        verify_depth_run(outcome, &self.context)
    }
}
