use super::super::*;

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
    let current_qty = strategy_current_qty_for(root, config, &instrument)?;
    let market_spec =
        strategy_execution_market_spec(config, &instrument, Some(runtime_config_path))?;
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

pub(crate) fn build_strategy_target_order(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
    strategy_id: &str,
    run_id: u64,
    now: u64,
    current_qty: i128,
    target_qty: i128,
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
        contract_output,
        market_spec.as_ref(),
    )
}
