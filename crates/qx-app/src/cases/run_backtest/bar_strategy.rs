use super::{run_backtest_inner_unchecked_with_details, InjectedStrategy};
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
    run_backtest_inner_unchecked_with_details(
        spec,
        context,
        cancellation,
        Vec::new(),
        Some(strategy),
    )
}

pub(super) fn run_backtest_inner_unchecked(
    spec: &BacktestSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    injected: Vec<InjectedStrategy>,
) -> Result<Option<BacktestOutcome>, AppError> {
    Ok(
        run_backtest_inner_unchecked_with_details(spec, context, cancellation, injected, None)?
            .map(|details| details.outcome),
    )
}
