use crate::load_worker_market_spec_for_instrument;
use qx_core::{InstrumentId, TradingInstrumentSpec, VenueFamily};
use qx_runtime::{RuntimeConfig, WorkerRole};
use std::collections::BTreeSet;
use std::path::Path;

pub(crate) fn strategy_execution_account_for_instrument(
    config: &RuntimeConfig,
    preferred_account_id: Option<&str>,
    instrument: &InstrumentId,
) -> Result<String, String> {
    let accounts = config
        .workers
        .iter()
        .filter(|worker| {
            worker.enabled
                && worker.role == WorkerRole::Execution
                && execution_worker_supports_instrument(worker, instrument)
        })
        .filter_map(|worker| worker.account_id.clone())
        .collect::<BTreeSet<_>>();

    if let Some(preferred) = preferred_account_id
        .filter(|preferred| accounts.iter().any(|account_id| account_id == preferred))
    {
        return Ok(preferred.to_string());
    }
    match accounts.iter().collect::<Vec<_>>().as_slice() {
        [account_id] => Ok((*account_id).clone()),
        [] => Err(format!(
            "Strategy 对冲腿没有匹配的启用 Execution worker: {}",
            instrument
        )),
        _ => Err(format!(
            "Strategy 对冲腿 {} 匹配多个执行账户，无法自动选择: {}",
            instrument,
            accounts.iter().cloned().collect::<Vec<_>>().join(", ")
        )),
    }
}

pub(crate) fn strategy_execution_market_spec(
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    runtime_config_path: Option<&Path>,
) -> Result<Option<TradingInstrumentSpec>, String> {
    let account_id = config.strategy.account_id.as_deref().unwrap_or_default();
    strategy_execution_market_spec_for_account(config, account_id, instrument, runtime_config_path)
}

pub(crate) fn strategy_execution_market_spec_for_account(
    config: &RuntimeConfig,
    account_id: &str,
    instrument: &InstrumentId,
    runtime_config_path: Option<&Path>,
) -> Result<Option<TradingInstrumentSpec>, String> {
    // A strategy can submit legs to venues other than its signal venue. Resolve
    // the frozen lot/tick rules from the actual order instrument and account.
    let venue_id = instrument.venue.as_str();
    let workers = config
        .workers
        .iter()
        .filter(|worker| {
            worker.enabled
                && worker.role == WorkerRole::Execution
                && worker.account_id.as_deref() == Some(account_id)
                && execution_worker_supports_instrument(worker, instrument)
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

/// A Paper execution worker routes arbitrary market instruments through the local
/// simulator, so its venue is `paper` while the instrument retains its market venue.
/// Live workers still have to match the instrument's exchange exactly.
pub(crate) fn execution_worker_supports_instrument(
    worker: &qx_runtime::WorkerConfig,
    instrument: &InstrumentId,
) -> bool {
    worker.venue_id.as_deref().is_some_and(|configured| {
        configured.eq_ignore_ascii_case(instrument.venue.as_str())
            || VenueFamily::parse(configured) == VenueFamily::Paper
    }) && (worker.symbols.is_empty()
        || worker
            .symbols
            .iter()
            .any(|symbol| InstrumentId::parse(symbol).as_ref() == Some(instrument)))
}
