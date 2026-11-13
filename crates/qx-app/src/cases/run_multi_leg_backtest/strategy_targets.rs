use qx_core::{InstrumentId, Side};
use qx_strategy::{MarketEvent, Strategy, StrategyContext, StrategyDecision};
use std::collections::BTreeMap;

#[derive(Clone, Default)]
pub(super) struct TargetSchedules {
    pub(super) primary: BTreeMap<u64, i128>,
    pub(super) reference: BTreeMap<u64, i128>,
}

/// Records strategy intents while the shared execution session is running.
/// Unlike the former pre-pass, the wrapped strategy receives fill-confirmed
/// account state and no future market events.
pub(super) struct RecordingMultiLegStrategy {
    inner: Box<dyn Strategy>,
    primary: InstrumentId,
    reference: InstrumentId,
    schedules: std::sync::Arc<std::sync::Mutex<TargetSchedules>>,
    planned_positions: BTreeMap<String, i128>,
    caller_strategy: bool,
}

impl RecordingMultiLegStrategy {
    pub(super) fn new(
        inner: Box<dyn Strategy>,
        primary: InstrumentId,
        reference: InstrumentId,
        schedules: std::sync::Arc<std::sync::Mutex<TargetSchedules>>,
        caller_strategy: bool,
    ) -> Self {
        Self {
            inner,
            primary,
            reference,
            schedules,
            planned_positions: BTreeMap::new(),
            caller_strategy,
        }
    }

    fn record(
        &mut self,
        context: &StrategyContext,
        decision: &StrategyDecision,
    ) -> Result<(), String> {
        decision.validate_for(context, context.as_of)?;
        for intent in &decision.intents {
            if intent.instrument != self.primary && intent.instrument != self.reference {
                return Err(format!(
                    "多腿策略意图标的 {} 不属于本次双腿",
                    intent.instrument
                ));
            }
            if self.caller_strategy
                && (intent.limit.is_some()
                    || intent.policy.is_some()
                    || intent.reduce_only
                    || intent.post_only)
            {
                return Err("双腿目标仓位调度只支持无限价、无额外订单策略的市价意图".into());
            }
            let key = intent.instrument.to_string();
            let current = self.planned_positions.entry(key).or_insert_with(|| {
                context
                    .positions
                    .get(&intent.instrument.to_string())
                    .copied()
                    .unwrap_or(0)
            });
            let delta = match intent.side {
                Side::Buy => intent.qty.raw(),
                Side::Sell => intent.qty.raw().checked_neg().ok_or("策略目标数量溢出")?,
            };
            *current = current.checked_add(delta).ok_or("策略目标仓位溢出")?;
            let mut schedules = self
                .schedules
                .lock()
                .map_err(|_| "多腿策略目标记录锁已中毒")?;
            if intent.instrument == self.primary {
                schedules.primary.insert(context.as_of, *current);
            } else {
                schedules.reference.insert(context.as_of, *current);
            }
        }
        Ok(())
    }
}

impl Strategy for RecordingMultiLegStrategy {
    fn on_init(&mut self, context: &StrategyContext) -> Result<(), String> {
        self.inner.on_init(context)
    }

    fn on_start(&mut self, context: &StrategyContext) -> Result<(), String> {
        self.inner.on_start(context)
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let decision = self.inner.on_event(context, event)?;
        self.record(context, &decision)?;
        Ok(decision)
    }

    fn on_order_update(
        &mut self,
        context: &StrategyContext,
        update: &str,
    ) -> Result<Option<StrategyDecision>, String> {
        let decision = self.inner.on_order_update(context, update)?;
        if let Some(decision) = decision.as_ref() {
            self.record(context, decision)?;
        }
        Ok(decision)
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.inner.on_stop()
    }
}
