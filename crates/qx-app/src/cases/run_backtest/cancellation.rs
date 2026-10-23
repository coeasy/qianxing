use super::super::experiment_search::{DrawdownPruningWindow, ExperimentCandidatePruning};
use crate::error::AppError;
use crate::run_handle::CancellationToken;
use qx_guanxing::Bar;
use qx_xingban::{BacktestEngine, BacktestReport, BacktestRunStatus, BarStrategy};

pub(super) fn run_engine(
    engine: BacktestEngine,
    bars: &[Bar],
    strategy: &mut dyn BarStrategy,
    cancellation: Option<&CancellationToken>,
) -> Result<Option<BacktestReport>, AppError> {
    match cancellation {
        Some(token) => engine
            .run_with_cancel(bars, strategy, || token.is_cancelled())
            .map_err(|error| AppError::from_qx_error(&error)),
        None => engine
            .run(bars, strategy)
            .map(Some)
            .map_err(|error| AppError::from_qx_error(&error)),
    }
}

pub(super) fn run_candidate_engine(
    engine: BacktestEngine,
    bars: &[Bar],
    strategy: &mut dyn BarStrategy,
    cancellation: Option<&CancellationToken>,
    pruning: Option<DrawdownPruningWindow>,
) -> Result<(Option<BacktestReport>, Option<ExperimentCandidatePruning>), AppError> {
    let Some(pruning) = pruning else {
        return Ok((run_engine(engine, bars, strategy, cancellation)?, None));
    };
    let (status, report) = engine
        .run_with_drawdown_pruning(
            bars,
            strategy,
            pruning.min_bars,
            pruning.training_bars,
            pruning.max_drawdown_bps,
            || cancellation.is_some_and(CancellationToken::is_cancelled),
        )
        .map_err(|error| AppError::from_qx_error(&error))?;
    match status {
        BacktestRunStatus::Completed => Ok((report, None)),
        BacktestRunStatus::Cancelled => Ok((None, None)),
        BacktestRunStatus::Pruned {
            processed_bars,
            max_drawdown_bps,
        } => Ok((
            None,
            Some(ExperimentCandidatePruning {
                processed_bars,
                observed_max_drawdown_bps: max_drawdown_bps,
                max_training_drawdown_bps: pruning.max_drawdown_bps,
            }),
        )),
    }
}
