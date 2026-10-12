use qx_zhenlu::{Signal, SignalMerger, TargetPosition};

pub(super) fn merge_strategy_signals(
    signals: Vec<Signal>,
    now: u64,
) -> Result<Vec<TargetPosition>, String> {
    SignalMerger
        .merge(signals, now)
        .map_err(|error| error.to_string())
}
