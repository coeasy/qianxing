use crate::error::AppError;
use crate::run_handle::CancellationToken;
use qx_guanxing::Bar;
use qx_xingban::{BacktestEngine, BacktestReport, BarStrategy};

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
