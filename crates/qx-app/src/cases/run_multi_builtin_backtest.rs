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
use qx_core::{
    InstrumentId, Order, OrderPolicy, OrderStatus, Quantity, Side, TradingInstrumentSpec,
};
use qx_datastruct::BarFrame;
use qx_guanxing::Bar;
use qx_strategy::{
    BuiltinStrategy, BuiltinStrategyConfig, MarketEvent, Strategy, StrategyContext,
    StrategyDecision,
};
use qx_xingban::BarStrategy;
use std::collections::BTreeMap;
use std::path::Path;

mod attribution;
mod input_snapshot;
use input_snapshot::MultiBuiltinScratch;
const MIN_BARS: usize = 3;

/// Validate the shared frame contract before a facade derives cash sizing or provenance.
/// The same check is repeated against the frozen frames at the execution boundary below.
pub fn validate_multi_builtin_frames(
    kind: qx_strategy::BuiltinStrategyKind,
    quantity_raw: i128,
    primary: &BarFrame,
    reference: &BarFrame,
) -> Result<(), AppError> {
    if quantity_raw <= 0 {
        return Err(invalid("多腿内置策略 quantity 必须为正整数"));
    }
    if !kind.needs_reference_leg() {
        return Err(invalid(format!(
            "多腿回测只接受套利策略，当前为 {}",
            kind.name()
        )));
    }
    if primary.instrument == reference.instrument {
        return Err(invalid("多腿内置回测要求两条腿使用不同标的"));
    }
    if primary.ts != reference.ts {
        return Err(invalid("多腿回测要求两条 BarFrame 的时间戳完全对齐"));
    }
    if primary.ts.len() < MIN_BARS {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            format!("多腿内置策略回测至少需要 {MIN_BARS} 根对齐 Bar"),
        ));
    }
    Ok(())
}

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

/// Normalized application inputs for the multi-builtin CLI workflow. Facades resolve files and
/// runtime configuration; the app owns the independent-leg specs, temporary output lifecycle, and
/// dispatch into the shared multi-builtin use case.
pub struct MultiBuiltinBacktestInputs<'a> {
    pub strategy: crate::BuiltinStrategySpec,
    pub primary_frame: &'a BarFrame,
    pub reference_frame: &'a BarFrame,
    pub initial_cash_raw: [i128; 2],
    /// A facade-resolved single-account override cannot describe two independent leg accounts.
    pub declared_initial_cash_override_raw: Option<i128>,
    pub market_specs: [Option<&'a TradingInstrumentSpec>; 2],
    pub margin_models: [Option<&'a crate::BacktestMarginConfig>; 2],
    pub funding_bps: i64,
    pub declared_product: Option<qx_core::TradingProduct>,
    pub execution_cost_rules: qx_xingban::ExecutionCostRules,
    pub risk_rules: Option<crate::BacktestRiskRules>,
    pub allow_short: bool,
    pub fill_model: Option<crate::BacktestFillModel>,
    pub max_bar_volume_participation_bps: Option<u16>,
}

/// Run the facade-normalized multi-builtin inputs through qx-app's shared Bar cases.
pub fn run_multi_builtin_backtest_with_inputs(
    input: MultiBuiltinBacktestInputs<'_>,
    context: &RunContext,
) -> Result<MultiBuiltinBacktestRun, AppError> {
    validate_multi_builtin_inputs(&input)?;
    let workspace = MultiBuiltinScratch::create()?;
    // Engine runs must consume the exact immutable frames the facade used for alignment,
    // strategy scheduling, and account sizing. Reopening caller-owned paths here creates a
    // time-of-check/time-of-use split if either file changes during workflow assembly.
    let primary_snapshot = workspace.write_frame("primary-bars.json", input.primary_frame)?;
    let reference_snapshot = workspace.write_frame("reference-bars.json", input.reference_frame)?;
    let primary_run_id = format!(
        "multi-builtin-{}-primary-{}",
        input.strategy.kind.name(),
        workspace.serial
    );
    let reference_run_id = format!(
        "multi-builtin-{}-reference-{}",
        input.strategy.kind.name(),
        workspace.serial
    );
    let spec = MultiBuiltinBacktestSpec {
        schema_version: crate::MULTI_BUILTIN_BACKTEST_SCHEMA_VERSION,
        strategy: input.strategy.clone(),
        primary: build_multi_builtin_leg_spec(MultiBuiltinLegAssembly {
            run_id: &primary_run_id,
            frame: input.primary_frame,
            path: &primary_snapshot,
            output_dir: &workspace.path.join("primary"),
            initial_cash_raw: input.initial_cash_raw[0],
            market_spec: input.market_specs[0],
            margin_model: input.margin_models[0],
            account_id: "multi-leg-primary",
            shared: &input,
        }),
        reference: build_multi_builtin_leg_spec(MultiBuiltinLegAssembly {
            run_id: &reference_run_id,
            frame: input.reference_frame,
            path: &reference_snapshot,
            output_dir: &workspace.path.join("reference"),
            initial_cash_raw: input.initial_cash_raw[1],
            market_spec: input.market_specs[1],
            margin_model: input.margin_models[1],
            account_id: "multi-leg-reference",
            shared: &input,
        }),
        funding_bps: input.funding_bps,
        declared_product: input.declared_product,
    };
    run_multi_builtin_backtest(&spec, context)
}

fn validate_multi_builtin_inputs(input: &MultiBuiltinBacktestInputs<'_>) -> Result<(), AppError> {
    validate_multi_builtin_frames(
        input.strategy.kind,
        input.strategy.quantity_raw,
        input.primary_frame,
        input.reference_frame,
    )?;
    if let Some(initial_cash_raw) = input.declared_initial_cash_override_raw {
        return Err(invalid(format!(
            "backtest multi-builtin 不接受 strategy.initial_cash_raw={initial_cash_raw}：两条腿的本金各按本腿行情由定资规则算出，一个账户数字无法同时是两条腿的本金。请改用 quantity 表达规模，或删掉这一格"
        )));
    }
    Ok(())
}

struct MultiBuiltinLegAssembly<'a> {
    run_id: &'a str,
    frame: &'a BarFrame,
    path: &'a Path,
    output_dir: &'a Path,
    initial_cash_raw: i128,
    market_spec: Option<&'a TradingInstrumentSpec>,
    margin_model: Option<&'a crate::BacktestMarginConfig>,
    account_id: &'a str,
    shared: &'a MultiBuiltinBacktestInputs<'a>,
}

fn build_multi_builtin_leg_spec(input: MultiBuiltinLegAssembly<'_>) -> crate::BacktestSpec {
    let placeholder = crate::BuiltinStrategySpec::new(
        qx_strategy::BuiltinStrategyKind::SmaCross,
        format!("{}-scheduled", input.run_id),
    );
    let settlement_currency = qx_core::resolve_settlement_currency(input.market_spec);
    let mut spec = crate::BacktestSpec::new(
        crate::BacktestDatasetInput {
            run_id: input.run_id.into(),
            instrument: input.frame.instrument.to_string(),
            bars_path: input.path.to_string_lossy().into_owned(),
        },
        settlement_currency,
        input.initial_cash_raw,
        20260914,
        input.output_dir.to_string_lossy().into_owned(),
        placeholder,
    );
    spec.account_id = Some(input.account_id.into());
    spec.market_spec = input.market_spec.cloned();
    spec.margin_model = input.margin_model.cloned();
    spec.execution_cost_rules = Some(input.shared.execution_cost_rules.clone());
    spec.risk_rules = input.shared.risk_rules.clone();
    spec.allow_short = Some(input.shared.allow_short);
    spec.fill_model = input.shared.fill_model;
    spec.max_bar_volume_participation_bps = input.shared.max_bar_volume_participation_bps;
    spec
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
            risk_rule_set_version: primary_details.risk_rule_set_version,
            targets: primary_targets.primary,
        },
        reference: MultiBuiltinLegRun {
            account_id: spec.reference.account_id.clone().unwrap_or_default(),
            outcome: reference_details.outcome,
            report: reference_details.report,
            risk_rule_set_version: reference_details.risk_rule_set_version,
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
        positions_exact: BTreeMap::new(),
        cash_exact: BTreeMap::new(),
        available_margin_exact: None,
    };
    strategy.on_init(&context).map_err(invalid)?;
    if let Err(start_error) = strategy.on_start(&context) {
        let stop_error = strategy.on_stop().err();
        return Err(invalid(match stop_error {
            Some(stop_error) => {
                format!("多腿策略 on_start 失败: {start_error}; on_stop 也失败: {stop_error}")
            }
            None => format!("多腿策略 on_start 失败: {start_error}"),
        }));
    }
    let targets_result = (|| {
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
        Ok(targets)
    })();
    let stop_result = strategy.on_stop().map_err(invalid);
    match (targets_result, stop_result) {
        (Ok(targets), Ok(())) => Ok(targets),
        (Err(primary_error), Ok(())) => Err(primary_error),
        (Ok(_), Err(stop_error)) => Err(stop_error),
        (Err(primary_error), Err(stop_error)) => Err(AppError::new(
            primary_error.category(),
            format!("{primary_error}; 多腿策略 on_stop 失败: {stop_error}"),
        )),
    }
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
        exact_ohlcv: None,
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
            exact_amounts: None,
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
