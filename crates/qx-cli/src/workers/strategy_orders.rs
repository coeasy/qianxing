use super::super::*;
use crate::strategy_binding::strategy_current_qty_for_account;
use crate::strategy_contract::strategy_execution_market_spec_for_account;

#[allow(clippy::too_many_arguments)] // These are the explicit data-root, runtime, identity, and intent inputs of one order build.
pub(crate) fn build_strategy_contract_intent_order(
    root: &Path,
    config: &RuntimeConfig,
    runtime_config_path: &Path,
    strategy_id: &str,
    signal_id: u64,
    round_scope: &str,
    intent: &StrategyContractIntent,
    now: u64,
) -> Result<Order, String> {
    let instrument = InstrumentId::parse(&intent.instrument)
        .ok_or_else(|| format!("Strategy intent instrument 非法: {}", intent.instrument))?;
    let account_id = intent
        .account_id
        .as_deref()
        .or(config.strategy.account_id.as_deref())
        .ok_or_else(|| "Strategy intent 产生订单必须配置 account_id".to_string())?;
    if intent.account_id.is_some()
        && !config.workers.iter().any(|worker| {
            worker.enabled
                && worker.role == WorkerRole::Execution
                && worker.account_id.as_deref() == Some(account_id)
                && worker
                    .venue_id
                    .as_deref()
                    .is_some_and(|venue| venue.eq_ignore_ascii_case(instrument.venue.as_str()))
                && (worker.symbols.is_empty()
                    || worker
                        .symbols
                        .iter()
                        .any(|symbol| InstrumentId::parse(symbol).as_ref() == Some(&instrument)))
        })
    {
        return Err(format!(
            "Strategy intent account_id={} 没有匹配标的 {} 的启用 Execution worker",
            account_id, instrument
        ));
    }
    let current_qty = strategy_current_qty_for_account(root, config, account_id, &instrument)?;
    let market_spec = strategy_execution_market_spec_for_account(
        config,
        account_id,
        &instrument,
        Some(runtime_config_path),
    )?;
    build_strategy_order_from_contract_intent(
        config,
        strategy_id,
        signal_id,
        round_scope,
        intent,
        now,
        current_qty,
        market_spec.as_ref(),
    )
}

#[allow(clippy::too_many_arguments)] // Keep target, account context, and runtime identity visible at the worker boundary.
pub(crate) fn build_strategy_target_order(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
    strategy_id: &str,
    run_id: u64,
    now: u64,
    current_qty: i128,
    target_qty: i128,
    current_qty_exact: Option<&str>,
    contract_output: Option<&StrategyContractOutput>,
) -> Result<Option<Order>, String> {
    let instrument_text = config
        .strategy
        .instrument
        .as_deref()
        .ok_or_else(|| "Strategy 产生订单必须配置 instrument".to_string())?;
    let instrument = InstrumentId::parse(instrument_text)
        .ok_or_else(|| format!("Strategy instrument 非法: {instrument_text}"))?;
    let market_spec =
        strategy_execution_market_spec(config, &instrument, Some(runtime_config_path))?;
    build_strategy_order_with_signal(
        config,
        strategy_id,
        run_id,
        now,
        current_qty,
        target_qty,
        current_qty_exact,
        contract_output,
        market_spec.as_ref(),
    )
}
