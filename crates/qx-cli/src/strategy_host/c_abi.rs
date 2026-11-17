//! Verified C ABI plugin loading and ordered strategy callback adaptation.

use super::imports::*;

pub(super) enum ExecutionCallback<'a> {
    OrderStatus(&'a qx_strategy::StrategyOrderStatusUpdate),
    Fill(&'a qx_strategy::StrategyFillUpdateV2),
    Account(&'a qx_strategy::contract::StrategyAccountUpdateV1),
}

impl ExecutionCallback<'_> {
    fn key(&self) -> (u64, &str, &str, u64, u8, u64) {
        match self {
            Self::OrderStatus(update) => (
                update.ts,
                &update.order.account_id,
                &update.order.instrument,
                update.event_seq,
                0,
                update.order.order_id,
            ),
            Self::Fill(update) => (
                update.ts,
                &update.order.account_id,
                &update.order.instrument,
                update.event_seq,
                1,
                update.order.order_id,
            ),
            Self::Account(update) => (update.ts, &update.account_id, "", update.event_seq, 2, 0),
        }
    }
}

pub(super) fn sort_execution_callbacks(callbacks: &mut [ExecutionCallback<'_>]) {
    callbacks.sort_by(|left, right| left.key().cmp(&right.key()));
}

pub(crate) fn load_c_abi_strategy(
    strategy: &StrategyRuntimeConfig,
) -> Result<DynamicCAbiStrategy, String> {
    // P0-3 / DD-4：`dlopen` 之前先过信任门（trusted_native + 签名信任根 + 架构/平台匹配）。
    native_trust::admit_in_process_c_abi(strategy)?;
    let library = strategy
        .c_abi_library
        .as_deref()
        .ok_or_else(|| "C ABI 策略缺少 c_abi_library".to_string())?;
    let expected_sha256 = strategy
        .c_abi_sha256
        .as_deref()
        .ok_or_else(|| "C ABI 策略缺少 c_abi_sha256".to_string())?;
    let mut policy = DynamicCAbiLoadPolicy::new(expected_sha256)
        .with_max_library_bytes(strategy.c_abi_max_library_bytes);
    if let (Some(public_key), Some(signature)) = (
        strategy.c_abi_ed25519_public_key.as_deref(),
        strategy.c_abi_ed25519_signature.as_deref(),
    ) {
        policy = policy.with_ed25519_signature(public_key, signature);
    }
    let config_json = serde_json::to_string(strategy)
        .map_err(|error| format!("C ABI 策略配置编码失败: {error}"))?;
    unsafe {
        DynamicCAbiStrategy::load_verified_with_required_capabilities(
            library,
            &config_json,
            &policy,
            &strategy.required_capabilities,
            strategy.account_id.is_some() && strategy.venue_id.is_some(),
        )
    }
    .map_err(|error| format!("加载 C ABI 策略失败: {error}"))
}

pub(crate) fn invoke_c_abi_strategy(
    strategy: &mut DynamicCAbiStrategy,
    initialized: &mut bool,
    context: &qx_strategy::StrategyContext,
    input: &StrategyContractInput,
    event: &NativeMarketEvent,
) -> Result<StrategyContractOutput, String> {
    invoke_c_abi_strategy_with_updates(
        strategy,
        initialized,
        context,
        input,
        event,
        &input.execution_updates,
    )
}

fn invoke_c_abi_strategy_with_updates(
    strategy: &mut DynamicCAbiStrategy,
    initialized: &mut bool,
    context: &qx_strategy::StrategyContext,
    input: &StrategyContractInput,
    event: &NativeMarketEvent,
    execution_updates: &[qx_strategy::StrategyOrderStatusUpdate],
) -> Result<StrategyContractOutput, String> {
    invoke_c_abi_strategy_with_execution_updates(
        strategy,
        initialized,
        context,
        input,
        event,
        execution_updates,
        &input.fill_updates,
        &input.account_updates,
    )
}

pub(crate) fn invoke_c_abi_strategy_with_execution_updates(
    strategy: &mut DynamicCAbiStrategy,
    initialized: &mut bool,
    context: &qx_strategy::StrategyContext,
    input: &StrategyContractInput,
    event: &NativeMarketEvent,
    execution_updates: &[qx_strategy::StrategyOrderStatusUpdate],
    fill_updates: &[qx_strategy::StrategyFillUpdateV2],
    account_updates: &[qx_strategy::contract::StrategyAccountUpdateV1],
) -> Result<StrategyContractOutput, String> {
    context.validate()?;
    event.validate()?;
    if strategy.has_partial_state_checkpoint_exports() {
        return Err("C ABI Strategy checkpoint snapshot/restore 导出必须成对提供".into());
    }
    for update in execution_updates {
        update.validate_for(context)?;
    }
    for update in fill_updates {
        update.validate_for(context)?;
    }
    for update in account_updates {
        update.validate_for(context)?;
    }
    if !*initialized {
        if let Err(message) = strategy.on_init(context) {
            return Err(notify_c_abi_strategy_error(
                strategy,
                context,
                StrategyErrorStage::Initialization,
                Some(context.as_of),
                None,
                message,
            ));
        }
        *initialized = true;
    }

    if strategy.supports_state_checkpoint() {
        let checkpoint = input
            .strategy_checkpoint
            .as_ref()
            .ok_or_else(|| "C ABI Strategy durable checkpoint 缺失，拒绝无状态恢复".to_string())?;
        checkpoint.validate_for_input(input)?;
        if checkpoint
            .adapter_state
            .get("state_mode")
            .and_then(|value| value.as_str())
            != Some("c-abi")
        {
            return Err("C ABI Strategy checkpoint adapter_state 不匹配".into());
        }
        strategy.restore_strategy_checkpoint(&checkpoint.strategy_state)?;
    }

    let mut callbacks = execution_updates
        .iter()
        .filter(|_| strategy.supports_order_status_updates())
        .map(ExecutionCallback::OrderStatus)
        .chain(
            fill_updates
                .iter()
                .filter(|_| strategy.supports_fill_updates())
                .map(ExecutionCallback::Fill),
        )
        .chain(
            account_updates
                .iter()
                .filter(|_| strategy.supports_account_updates())
                .map(ExecutionCallback::Account),
        )
        .collect::<Vec<_>>();
    sort_execution_callbacks(&mut callbacks);

    let mut decisions = Vec::new();
    for callback in callbacks {
        let (stage, timestamp, instrument_text, result) = match callback {
            ExecutionCallback::OrderStatus(update) => (
                StrategyErrorStage::OrderUpdate,
                update.ts,
                Some(update.order.instrument.as_str()),
                strategy.on_strategy_order_status(context, update),
            ),
            ExecutionCallback::Fill(update) => (
                StrategyErrorStage::FillUpdate,
                update.ts,
                Some(update.order.instrument.as_str()),
                strategy.on_strategy_fill_update_v2(context, update),
            ),
            ExecutionCallback::Account(update) => (
                StrategyErrorStage::AccountUpdate,
                update.ts,
                None,
                strategy.on_strategy_account_update(context, update),
            ),
        };
        let instrument = instrument_text.and_then(InstrumentId::parse);
        let decision = match result {
            Ok(decision) => decision,
            Err(message) => {
                return Err(notify_c_abi_strategy_error(
                    strategy,
                    context,
                    stage,
                    Some(timestamp),
                    instrument.as_ref(),
                    message,
                ));
            }
        };
        if let Some(decision) = decision {
            decisions.push(decision);
        }
    }
    let mut decision = match strategy.on_event(context, event) {
        Ok(decision) => decision,
        Err(message) => {
            return Err(notify_c_abi_strategy_error(
                strategy,
                context,
                StrategyErrorStage::MarketEvent,
                Some(event.ts()),
                event.instrument(),
                message,
            ));
        }
    };
    let update_expiry = decisions
        .iter()
        .map(|update_decision| update_decision.expires_at)
        .filter(|expiry| *expiry > 0)
        .min();
    let mut ordered_intents = decisions
        .iter_mut()
        .flat_map(|update_decision| update_decision.intents.drain(..))
        .collect::<Vec<_>>();
    ordered_intents.append(&mut decision.intents);
    decision.intents = ordered_intents;
    decision.expires_at = match (decision.expires_at, update_expiry) {
        (0, Some(expiry)) => expiry,
        (expiry, None) => expiry,
        (left, Some(right)) => left.min(right),
    };
    native_decision_to_contract_output(&decision, input).map_err(|error| {
        notify_c_abi_strategy_error(
            strategy,
            context,
            StrategyErrorStage::MarketEvent,
            Some(event.ts()),
            event.instrument(),
            error.to_string(),
        )
    })
}

fn notify_c_abi_strategy_error(
    strategy: &mut DynamicCAbiStrategy,
    context: &qx_strategy::StrategyContext,
    stage: StrategyErrorStage,
    timestamp: Option<u64>,
    instrument: Option<&InstrumentId>,
    message: String,
) -> String {
    let error = StrategyErrorV1::new(
        stage,
        context,
        timestamp,
        instrument.cloned(),
        message.clone(),
    );
    qx_strategy::notify_strategy_error(strategy, &error);
    message
}
