use super::{attach, guard::guard_panics, run_backtest::run_backtest_inner_with_bar_strategy};
use crate::context::{CallerCapability, RunContext};
use crate::error::{AppError, AppErrorCategory};
use crate::run_handle::CancellationToken;
use crate::spec::{
    MultiBuiltinAttribution, MultiBuiltinBacktestRun, MultiBuiltinBacktestSpec, MultiBuiltinLegRun,
};
use crate::{
    multi_leg_combined_return_bps, multi_leg_cost_bps, multi_leg_group_attributions,
    multi_leg_group_totals, multi_leg_leg_buckets, multi_leg_leg_integrity, MultiLegLegFacts,
};
use qx_core::{InstrumentId, Order, OrderPolicy, OrderStatus, Quantity, Side};
use qx_datastruct::BarFrame;
use qx_guanxing::Bar;
use qx_strategy::{
    BuiltinStrategy, BuiltinStrategyConfig, MarketEvent, Strategy, StrategyContext,
    StrategyDecision,
};
use qx_xingban::BarStrategy;
use std::collections::BTreeMap;

mod attribution;
const MIN_BARS: usize = 3;

pub fn run_multi_builtin_backtest(
    spec: &MultiBuiltinBacktestSpec,
    context: &RunContext,
) -> Result<MultiBuiltinBacktestRun, AppError> {
    context.require(CallerCapability::Research, "独立账本多腿内置回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || run_inner(spec, context, None))
        .map_err(|error| attach(error, &correlation_id))?
        .ok_or_else(|| {
            attach(
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    "同步多腿内置回测意外返回取消状态",
                ),
                &correlation_id,
            )
        })
}

fn run_inner(
    spec: &MultiBuiltinBacktestSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
) -> Result<Option<MultiBuiltinBacktestRun>, AppError> {
    spec.validate()?;
    let primary = read_frame(&spec.primary.bars_path, "主腿")?;
    let reference = read_frame(&spec.reference.bars_path, "对冲腿")?;
    if primary.instrument == reference.instrument {
        return Err(invalid("多腿内置回测要求两条腿使用不同标的"));
    }
    if primary.ts != reference.ts {
        return Err(invalid("多腿内置回测要求两条 BarFrame 的时间戳完全对齐"));
    }
    let primary_bars: Vec<Bar> = (&primary).into();
    let reference_bars: Vec<Bar> = (&reference).into();
    if primary_bars.len() < MIN_BARS {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            format!("多腿内置回测至少需要 {MIN_BARS} 根对齐 Bar"),
        ));
    }
    let primary_targets =
        build_targets(spec, &primary, &reference, &primary_bars, &reference_bars)?;
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        return Ok(None);
    }
    let Some(primary_details) = run_backtest_inner_with_bar_strategy(
        &spec.primary,
        context,
        cancellation,
        Box::new(ScheduledTargetStrategy {
            instrument: primary.instrument.clone(),
            targets: primary_targets.primary.clone(),
            policy: None,
            account_id: spec.primary.account_id.clone().unwrap_or_default(),
        }),
    )?
    else {
        return Ok(None);
    };
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        return Ok(None);
    }
    let Some(reference_details) = run_backtest_inner_with_bar_strategy(
        &spec.reference,
        context,
        cancellation,
        Box::new(ScheduledTargetStrategy {
            instrument: reference.instrument.clone(),
            targets: primary_targets.reference.clone(),
            policy: None,
            account_id: spec.reference.account_id.clone().unwrap_or_default(),
        }),
    )?
    else {
        return Ok(None);
    };
    let attribution = attribution::build(attribution::AttributionInput {
        spec,
        primary_frame: &primary,
        reference_frame: &reference,
        primary_bars: &primary_bars,
        reference_bars: &reference_bars,
        primary_targets: &primary_targets.primary,
        reference_targets: &primary_targets.reference,
        primary_report: &primary_details.report,
        reference_report: &reference_details.report,
    })?;
    Ok(Some(MultiBuiltinBacktestRun {
        primary: MultiBuiltinLegRun {
            account_id: spec.primary.account_id.clone().unwrap_or_default(),
            outcome: primary_details.outcome,
            report: primary_details.report,
            targets: primary_targets.primary,
        },
        reference: MultiBuiltinLegRun {
            account_id: spec.reference.account_id.clone().unwrap_or_default(),
            outcome: reference_details.outcome,
            report: reference_details.report,
            targets: primary_targets.reference,
        },
        funding_bps: spec.funding_bps,
        attribution,
    }))
}

struct TargetSchedules {
    primary: BTreeMap<u64, i128>,
    reference: BTreeMap<u64, i128>,
}

fn build_targets(
    spec: &MultiBuiltinBacktestSpec,
    primary: &BarFrame,
    reference: &BarFrame,
    primary_bars: &[Bar],
    reference_bars: &[Bar],
) -> Result<TargetSchedules, AppError> {
    let strategy_id = spec.strategy.strategy_id.clone();
    let version = format!("builtin-{}-v1", spec.strategy.kind.name());
    let strategy_config = BuiltinStrategyConfig {
        kind: spec.strategy.kind,
        strategy_id: strategy_id.clone(),
        strategy_version: version.clone(),
        instrument: primary.instrument.clone(),
        quantity: Quantity::from_raw(spec.strategy.quantity_raw),
        target_notional_raw: spec.strategy.target_notional_raw,
        primary_market_spec: spec.primary.market_spec.clone(),
        reference_market_spec: spec.reference.market_spec.clone(),
        target_available_margin_bps: spec.strategy.target_available_margin_bps,
        target_volatility_bps: spec.strategy.target_volatility_bps,
        target_kelly_fraction_bps: spec.strategy.target_kelly_fraction_bps,
        kelly_lookback_bars: spec.strategy.kelly_lookback_bars,
        fast_window: spec.strategy.fast_window,
        slow_window: spec.strategy.slow_window,
        period: spec.strategy.period,
        threshold_bps: spec.strategy.threshold_bps,
        reference_instrument: Some(reference.instrument.clone()),
        primary_policy: None,
        reference_policy: None,
    };
    strategy_config.validate().map_err(invalid)?;
    let mut strategy = BuiltinStrategy::new(strategy_config).map_err(invalid)?;
    let primary_cash = spec.primary.initial_cash_raw;
    let cash = BTreeMap::from([(spec.primary.settlement_currency.clone(), primary_cash)]);
    let mut context = StrategyContext {
        strategy_id,
        strategy_version: version,
        account_id: "multi-leg-backtest".into(),
        venue_id: primary.instrument.venue.to_string(),
        data_fingerprint: format!(
            "{}:{}",
            qx_data::fingerprint_bar_frame(primary)
                .map_err(|error| invalid(format!("主腿内容指纹计算失败: {error}")))?,
            qx_data::fingerprint_bar_frame(reference)
                .map_err(|error| invalid(format!("对冲腿内容指纹计算失败: {error}")))?
        ),
        as_of: primary_bars.first().map(|bar| bar.ts).unwrap_or(1),
        positions: BTreeMap::from([
            (primary.instrument.to_string(), 0),
            (reference.instrument.to_string(), 0),
        ]),
        available_margin_raw: crate::cases::strategy_available_margin_raw(
            &cash,
            &spec.primary.settlement_currency,
        ),
        cash,
        risk_state: "multi-leg-backtest".into(),
    };
    strategy.on_init(&context).map_err(invalid)?;
    let mut targets = TargetSchedules {
        primary: BTreeMap::new(),
        reference: BTreeMap::new(),
    };
    for (primary_bar, reference_bar) in primary_bars.iter().zip(reference_bars) {
        context.as_of = primary_bar.ts;
        let reference_decision = strategy
            .on_event(&context, &bar_event(&reference.instrument, reference_bar))
            .map_err(invalid)?;
        record_targets(
            &mut context,
            &mut targets,
            &reference_decision,
            primary,
            reference,
            reference_bar.ts,
        )?;
        let primary_decision = strategy
            .on_event(&context, &bar_event(&primary.instrument, primary_bar))
            .map_err(invalid)?;
        record_targets(
            &mut context,
            &mut targets,
            &primary_decision,
            primary,
            reference,
            primary_bar.ts,
        )?;
    }
    strategy.on_stop().map_err(invalid)?;
    Ok(targets)
}

fn record_targets(
    context: &mut StrategyContext,
    targets: &mut TargetSchedules,
    decision: &StrategyDecision,
    primary: &BarFrame,
    reference: &BarFrame,
    ts: u64,
) -> Result<(), AppError> {
    decision.validate_for(context, ts).map_err(invalid)?;
    for intent in &decision.intents {
        let key = intent.instrument.to_string();
        let current = context.positions.get(&key).copied().unwrap_or(0);
        let delta = if intent.side == Side::Buy {
            intent.qty.raw()
        } else {
            intent
                .qty
                .raw()
                .checked_neg()
                .ok_or_else(|| invalid("多腿策略数量溢出"))?
        };
        let target = current
            .checked_add(delta)
            .ok_or_else(|| invalid("多腿回测目标仓位溢出"))?;
        context.positions.insert(key, target);
        if intent.instrument == primary.instrument {
            targets.primary.insert(ts, target);
        } else if intent.instrument == reference.instrument {
            targets.reference.insert(ts, target);
        } else {
            return Err(invalid(format!(
                "多腿策略意图指向未知标的 {}",
                intent.instrument
            )));
        }
    }
    Ok(())
}

fn bar_event(instrument: &InstrumentId, bar: &Bar) -> MarketEvent {
    MarketEvent::Bar {
        instrument: instrument.clone(),
        ts: bar.ts,
        open_raw: bar.open,
        high_raw: bar.high,
        low_raw: bar.low,
        close_raw: bar.close,
        volume_raw: bar.volume,
    }
}

struct ScheduledTargetStrategy {
    instrument: InstrumentId,
    targets: BTreeMap<u64, i128>,
    policy: Option<OrderPolicy>,
    account_id: String,
}

impl BarStrategy for ScheduledTargetStrategy {
    fn on_bar(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        position: i128,
    ) -> Option<Order> {
        if instrument != &self.instrument {
            return None;
        }
        let visible_ts = history.last()?.ts;
        let target = self
            .targets
            .range(..=visible_ts)
            .next_back()
            .map(|(_, target)| *target)
            .unwrap_or(0);
        let delta = target.checked_sub(position)?;
        if delta == 0 {
            return None;
        }
        Some(Order {
            client_id: 0,
            instrument: self.instrument.clone(),
            side: if delta > 0 { Side::Buy } else { Side::Sell },
            qty: Quantity::from_raw(delta.checked_abs()?),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: self.account_id.clone(),
            trace: None,
            policy: self.policy,
        })
    }
}

fn read_frame(path: &str, leg: &str) -> Result<BarFrame, AppError> {
    let payload = super::artifacts::read_text(path)?;
    BarFrame::from_json(&payload).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("{leg} BarFrame 校验失败 {path}: {error:?}"),
        )
    })
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}
