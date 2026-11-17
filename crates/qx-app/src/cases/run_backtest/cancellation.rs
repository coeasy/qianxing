use super::super::experiment_search::{DrawdownPruningWindow, ExperimentCandidatePruning};
use crate::cases::strategy_lifecycle::run_bar_strategy_with_lifecycle_and_reason;
use crate::error::AppError;
use crate::run_handle::CancellationToken;
use qx_guanxing::Bar;
use qx_strategy::StrategyStopReason;
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
    run_bar_strategy_with_lifecycle_and_reason(
        strategy,
        |strategy| {
            if let Some(pruning) = pruning {
                engine
                    .run_with_drawdown_pruning(
                        bars,
                        strategy,
                        pruning.min_bars,
                        pruning.training_bars,
                        pruning.max_drawdown_bps,
                        || cancellation.is_some_and(CancellationToken::is_cancelled),
                    )
                    .map_err(|error| AppError::from_qx_error(&error))
                    .map(|(status, report)| match status {
                        BacktestRunStatus::Completed => (report, None),
                        BacktestRunStatus::Cancelled => (None, None),
                        BacktestRunStatus::Pruned {
                            processed_bars,
                            max_drawdown_bps,
                        } => (
                            None,
                            Some(ExperimentCandidatePruning {
                                processed_bars,
                                observed_max_drawdown_bps: max_drawdown_bps,
                                max_training_drawdown_bps: pruning.max_drawdown_bps,
                            }),
                        ),
                    })
            } else {
                run_engine(engine, bars, strategy, cancellation).map(|report| (report, None))
            }
        },
        |(report, pruning)| {
            if report.is_none() && pruning.is_none() {
                StrategyStopReason::Cancelled
            } else {
                StrategyStopReason::Completed
            }
        },
    )
}
