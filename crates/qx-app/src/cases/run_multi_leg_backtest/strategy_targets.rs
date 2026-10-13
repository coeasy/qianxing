use super::invalid;
use super::CancellationToken;
use qx_core::{InstrumentId, Side};
use qx_strategy::{MarketEvent, Strategy, StrategyContext};
use qx_xingban::PortfolioBarTimeline;
use std::collections::BTreeMap;

pub(super) struct TargetSchedules {
    pub(super) primary: BTreeMap<u64, i128>,
    pub(super) reference: BTreeMap<u64, i128>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_target_schedules(
    strategy: &mut dyn Strategy,
    context: &mut StrategyContext,
    timeline: &PortfolioBarTimeline<'_>,
    primary: &InstrumentId,
    reference: &InstrumentId,
    settlement_currency: &str,
    primary_initial_cash_raw: i128,
    reference_initial_cash_raw: i128,
    caller_strategy: bool,
    cancellation: Option<&CancellationToken>,
) -> Result<Option<TargetSchedules>, crate::AppError> {
    let mut targets = TargetSchedules {
        primary: BTreeMap::new(),
        reference: BTreeMap::new(),
    };
    for market_event in timeline.events() {
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            strategy.on_stop().map_err(invalid)?;
            return Ok(None);
        }
        let instrument = market_event.instrument;
        let bar = market_event.bar;
        context.as_of = bar.ts;
        let leg_initial_cash = if instrument == primary {
            primary_initial_cash_raw
        } else {
            reference_initial_cash_raw
        };
        context
            .cash
            .insert(settlement_currency.to_string(), leg_initial_cash);
        context.available_margin_raw = Some(leg_initial_cash);
        let event = MarketEvent::Bar {
            instrument: instrument.clone(),
            ts: bar.ts,
            open_raw: bar.open,
            high_raw: bar.high,
            low_raw: bar.low,
            close_raw: bar.close,
            volume_raw: bar.volume,
        };
        let decision = strategy.on_event(context, &event).map_err(invalid)?;
        decision.validate_for(context, bar.ts).map_err(invalid)?;
        for intent in decision.intents {
            if intent.instrument != *primary && intent.instrument != *reference {
                return Err(invalid(format!(
                    "多腿策略意图标的 {} 不属于本次双腿",
                    intent.instrument
                )));
            }
            if caller_strategy
                && (intent.limit.is_some()
                    || intent.policy.is_some()
                    || intent.reduce_only
                    || intent.post_only)
            {
                return Err(invalid(
                    "双腿目标仓位调度只支持无限价、无额外订单策略的市价意图",
                ));
            }
            let key = intent.instrument.to_string();
            let current = context.positions.get(&key).copied().unwrap_or(0);
            let delta = if intent.side == Side::Buy {
                intent.qty.raw()
            } else {
                -intent.qty.raw()
            };
            let target = current
                .checked_add(delta)
                .ok_or_else(|| invalid("策略目标仓位溢出"))?;
            context.positions.insert(key, target);
            if intent.instrument == *primary {
                targets.primary.insert(bar.ts, target);
            } else if intent.instrument == *reference {
                targets.reference.insert(bar.ts, target);
            }
        }
    }
    strategy.on_stop().map_err(invalid)?;
    Ok(Some(targets))
}
