use crate::load_worker_market_spec_for_instrument;
use qx_core::{InstrumentId, TradingInstrumentSpec};
use qx_runtime::{RuntimeConfig, WorkerRole};
use std::collections::BTreeSet;
use std::path::Path;

pub(crate) fn strategy_execution_market_spec(
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    runtime_config_path: Option<&Path>,
) -> Result<Option<TradingInstrumentSpec>, String> {
    let account_id = config.strategy.account_id.as_deref().unwrap_or_default();
    let venue_id = config.strategy.venue_id.as_deref().unwrap_or_default();
    let workers = config
        .workers
        .iter()
        .filter(|worker| {
            worker.enabled
                && worker.role == WorkerRole::Execution
                && worker.account_id.as_deref() == Some(account_id)
                && worker.venue_id.as_deref() == Some(venue_id)
                && (worker.symbols.is_empty()
                    || worker
                        .symbols
                        .iter()
                        .any(|symbol| InstrumentId::parse(symbol).as_ref() == Some(instrument)))
                && worker
                    .instrument_spec_path
                    .as_deref()
                    .is_some_and(|path| !path.trim().is_empty())
        })
        .collect::<Vec<_>>();
    let paths = workers
        .iter()
        .filter_map(|worker| worker.instrument_spec_path.as_deref())
        .collect::<BTreeSet<_>>();
    if paths.len() > 1 {
        return Err(format!(
            "Strategy 订单数量精度存在多个 execution market spec: {}@{} / {}",
            account_id, venue_id, instrument
        ));
    }
    let Some(worker) = workers.first() else {
        return Ok(None);
    };
    load_worker_market_spec_for_instrument(worker, instrument, runtime_config_path)
}
