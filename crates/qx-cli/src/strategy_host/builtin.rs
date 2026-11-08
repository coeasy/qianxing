//! Built-in strategy execution through the shared native strategy contract.

use super::imports::*;

pub(crate) fn invoke_builtin_strategy(
    root: &Path,
    config: &RuntimeConfig,
    runtime_config_path: Option<&Path>,
    instrument: &InstrumentId,
    request_id: &str,
    now: u64,
) -> Result<StrategyContractOutput, String> {
    let input = build_strategy_contract_input(root, config, instrument, request_id, now)?;
    let bars = input.bars.as_ref().ok_or_else(|| {
        "builtin_strategy 运行时需要 bars_snapshot_path 提供 K 线历史".to_string()
    })?;
    let context = super::native_strategy_context(&config.strategy, &input)?;
    let mut builtin_config =
        qx_runtime::builtin_strategy_config_from_runtime(&config.strategy, instrument)?;
    let mut reference_route = None;
    if builtin_config.reference_instrument.is_some() {
        builtin_config.primary_market_spec =
            strategy_execution_market_spec(config, instrument, runtime_config_path)?;
        let reference = builtin_config
            .reference_instrument
            .as_ref()
            .ok_or("双腿内置策略丢失 reference_instrument")?;
        let reference_account_id = strategy_execution_account_for_instrument(
            config,
            config.strategy.account_id.as_deref(),
            reference,
        )?;
        builtin_config.reference_market_spec = strategy_execution_market_spec_for_account(
            config,
            &reference_account_id,
            reference,
            runtime_config_path,
        )?;
        reference_route = Some((reference.clone(), reference_account_id));
    }
    let mut strategy = BuiltinStrategy::new(builtin_config)?;
    strategy.on_init(&context)?;
    let reference_bars =
        if let Some(reference_text) = config.strategy.builtin_reference_instrument.as_deref() {
            let reference = InstrumentId::parse(reference_text)
                .ok_or_else(|| format!("builtin_reference_instrument 非法: {reference_text}"))?;
            let reference_path = config
                .strategy
                .builtin_reference_bars_snapshot_path
                .as_deref()
                .ok_or_else(|| "双腿套利缺少 builtin_reference_bars_snapshot_path".to_string())?;
            let mut reference_config = config.clone();
            reference_config.strategy.instrument = Some(reference.to_string());
            reference_config.strategy.bars_snapshot_path = Some(reference_path.into());
            load_strategy_contract_bars(root, &reference_config, &reference, now)?
                .map(|(bars, _, _)| (reference, bars))
        } else {
            None
        };
    let mut events = bars
        .ts
        .iter()
        .enumerate()
        .map(|(index, ts)| (*ts, false, index))
        .collect::<Vec<_>>();
    if let Some((_, reference)) = reference_bars.as_ref() {
        events.extend(
            reference
                .ts
                .iter()
                .enumerate()
                .map(|(index, ts)| (*ts, true, index)),
        );
    }
    events.sort_by_key(|(ts, is_reference, _)| (*ts, !*is_reference));
    let mut decision = None;
    for (_, is_reference, index) in events {
        let (event_instrument, event_bars) = if is_reference {
            let (reference, bars) = reference_bars.as_ref().ok_or("套利对冲腿 BarFrame 缺失")?;
            (reference.clone(), bars)
        } else {
            (instrument.clone(), bars)
        };
        let event = NativeMarketEvent::Bar {
            instrument: event_instrument,
            ts: event_bars.ts[index],
            open_raw: event_bars.open_raw[index],
            high_raw: event_bars.high_raw[index],
            low_raw: event_bars.low_raw[index],
            close_raw: event_bars.close_raw[index],
            volume_raw: event_bars.volume_raw[index],
            exact_ohlcv: None,
        };
        decision = Some(strategy.on_event(&context, &event)?);
    }
    let decision = decision.ok_or_else(|| "builtin_strategy 可见 K 线为空".to_string())?;
    let mut output = native_decision_to_contract_output(&decision, &input)?;
    if let Some((reference, reference_account_id)) = reference_route {
        if config.strategy.account_id.as_deref() != Some(reference_account_id.as_str()) {
            for intent in &mut output.intents {
                if InstrumentId::parse(&intent.instrument).as_ref() == Some(&reference) {
                    intent.account_id = Some(reference_account_id.clone());
                }
            }
        }
    }
    Ok(output)
}
