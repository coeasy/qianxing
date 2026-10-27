use crate::error::AppError;
use qx_strategy::StrategyStopReason;
use qx_xingban::BarStrategy;

/// Run a Bar strategy with one consistent lifecycle across single- and multi-market hosts.
pub(crate) fn run_bar_strategy_with_lifecycle<T>(
    strategy: &mut dyn BarStrategy,
    run: impl FnOnce(&mut dyn BarStrategy) -> Result<T, AppError>,
) -> Result<T, AppError> {
    run_bar_strategy_with_lifecycle_and_reason(strategy, run, |_| StrategyStopReason::Completed)
}

pub(crate) fn run_bar_strategy_with_lifecycle_and_reason<T>(
    strategy: &mut dyn BarStrategy,
    run: impl FnOnce(&mut dyn BarStrategy) -> Result<T, AppError>,
    successful_stop_reason: impl FnOnce(&T) -> StrategyStopReason,
) -> Result<T, AppError> {
    if let Err(init_error) = strategy.on_init() {
        return Err(startup_error(
            "on_init",
            init_error,
            strategy.on_stop_with_reason(StrategyStopReason::Failed),
        ));
    }
    if let Err(start_error) = strategy.on_start() {
        return Err(startup_error(
            "on_start",
            start_error,
            strategy.on_stop_with_reason(StrategyStopReason::Failed),
        ));
    }

    let result = run(strategy);
    let stop_reason = match &result {
        Ok(value) => successful_stop_reason(value),
        Err(_) => StrategyStopReason::Failed,
    };
    let stop_result = strategy
        .on_stop_with_reason(stop_reason)
        .map_err(|error| AppError::from_qx_error(&error));
    match (result, stop_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(stop_error)) => Err(stop_error),
        (Err(primary_error), Ok(())) => Err(primary_error),
        (Err(primary_error), Err(stop_error)) => Err(AppError::new(
            primary_error.category(),
            format!("{primary_error}; 策略 on_stop 也失败: {stop_error}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppErrorCategory;
    use std::cell::Cell;
    use std::rc::Rc;

    struct StopRecorder {
        seen: Rc<Cell<Option<StrategyStopReason>>>,
        fail_init: bool,
    }

    impl BarStrategy for StopRecorder {
        fn on_init(&mut self) -> Result<(), qx_core::QxError> {
            if self.fail_init {
                Err(qx_core::QxError::BusinessViolation("init failed".into()))
            } else {
                Ok(())
            }
        }

        fn on_stop_with_reason(
            &mut self,
            reason: StrategyStopReason,
        ) -> Result<(), qx_core::QxError> {
            self.seen.set(Some(reason));
            Ok(())
        }
    }

    fn recorder(fail_init: bool) -> (StopRecorder, Rc<Cell<Option<StrategyStopReason>>>) {
        let seen = Rc::new(Cell::new(None));
        (
            StopRecorder {
                seen: seen.clone(),
                fail_init,
            },
            seen,
        )
    }

    #[test]
    fn bar_lifecycle_reports_completion_cancellation_and_failure_reasons() {
        let (mut completed, completed_reason) = recorder(false);
        assert_eq!(
            run_bar_strategy_with_lifecycle(&mut completed, |_| Ok(())).unwrap(),
            ()
        );
        assert_eq!(completed_reason.get(), Some(StrategyStopReason::Completed));

        let (mut cancelled, cancelled_reason) = recorder(false);
        run_bar_strategy_with_lifecycle_and_reason(
            &mut cancelled,
            |_| Ok(None::<()>),
            |_| StrategyStopReason::Cancelled,
        )
        .unwrap();
        assert_eq!(cancelled_reason.get(), Some(StrategyStopReason::Cancelled));

        let (mut failed, failed_reason) = recorder(false);
        let error = run_bar_strategy_with_lifecycle(&mut failed, |_| {
            Err::<(), _>(AppError::new(AppErrorCategory::InvalidInput, "run failed"))
        })
        .unwrap_err();
        assert!(error.to_string().contains("run failed"));
        assert_eq!(failed_reason.get(), Some(StrategyStopReason::Failed));
    }

    #[test]
    fn bar_lifecycle_marks_startup_failure_before_cleanup() {
        let (mut strategy, stop_reason) = recorder(true);
        assert!(run_bar_strategy_with_lifecycle(&mut strategy, |_| Ok(())).is_err());
        assert_eq!(stop_reason.get(), Some(StrategyStopReason::Failed));
    }
}

fn startup_error(
    stage: &str,
    primary: qx_core::QxError,
    stop: Result<(), qx_core::QxError>,
) -> AppError {
    let primary_error = AppError::from_qx_error(&primary);
    match stop {
        Ok(()) => primary_error,
        Err(stop_error) => AppError::new(
            primary_error.category(),
            format!("策略 {stage} 失败: {primary}; on_stop 也失败: {stop_error}"),
        ),
    }
}
