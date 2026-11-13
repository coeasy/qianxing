use super::{run_backtest_inner_unchecked_with_details, InjectedStrategy};
use crate::cases::experiment_search::SearchBacktestAttempt;
use crate::context::RunContext;
use crate::error::AppError;
use crate::run_handle::CancellationToken;
use crate::spec::{BacktestOutcome, BacktestSpec};
use qx_xingban::{BacktestReport, BarStrategy};

pub(in crate::cases) struct BacktestRunDetails {
    pub(in crate::cases) outcome: BacktestOutcome,
    pub(in crate::cases) report: BacktestReport,
    pub(in crate::cases) risk_rule_set_version: String,
}

pub(in crate::cases) fn run_backtest_inner_with_bar_strategy(
    spec: &BacktestSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    strategy: Box<dyn BarStrategy>,
) -> Result<Option<BacktestRunDetails>, AppError> {
    match run_backtest_inner_unchecked_with_details(
        spec,
        context,
        cancellation,
        Vec::new(),
        Some(strategy),
        None,
        std::collections::BTreeMap::new(),
    )? {
        SearchBacktestAttempt::Completed(details) => Ok(details),
        SearchBacktestAttempt::Pruned(_) => Err(AppError::new(
            crate::error::AppErrorCategory::InternalInvariant,
            "未配置参数搜索剪枝的 Bar 策略意外被剪枝",
        )),
    }
}

pub(super) fn run_backtest_inner_unchecked(
    spec: &BacktestSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    injected: Vec<InjectedStrategy>,
) -> Result<Option<BacktestOutcome>, AppError> {
    run_backtest_inner_unchecked_with_identity_extensions(
        spec,
        context,
        cancellation,
        injected,
        std::collections::BTreeMap::new(),
    )
}

pub(super) fn run_backtest_inner_unchecked_with_identity_extensions(
    spec: &BacktestSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    injected: Vec<InjectedStrategy>,
    identity_extensions: std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<Option<BacktestOutcome>, AppError> {
    match run_backtest_inner_unchecked_with_details(
        spec,
        context,
        cancellation,
        injected,
        None,
        None,
        identity_extensions,
    )? {
        SearchBacktestAttempt::Completed(details) => Ok(details.map(|details| details.outcome)),
        SearchBacktestAttempt::Pruned(_) => Err(AppError::new(
            crate::error::AppErrorCategory::InternalInvariant,
            "未配置参数搜索剪枝的 Bar 回测意外被剪枝",
        )),
    }
}
