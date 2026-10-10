//! Deterministic two-leg application workflow; strategy and fills use the Rust domain engines.

use super::{artifacts::write_text, attach, guard::guard_panics};
use crate::run_handle::CancellationToken;
use crate::{
    AppError, AppErrorCategory, CallerCapability, MultiLegBacktestOutcome, MultiLegBacktestSpec,
    MultiLegFillModel, RunContext,
};
use qx_core::{
    Fnv1a, InstrumentId, MakerTakerFeeModel, Money, Order, OrderStatus, Quantity, ReplayVerifier,
    Side, TradingInstrumentSpec,
};
use qx_datastruct::BarFrame;
use qx_guanxing::Bar;
use qx_strategy::{BuiltinStrategy, BuiltinStrategyConfig, MarketEvent, Strategy, StrategyContext};
use qx_xingban::{
    BacktestConfig, BacktestEngine, BarStrategy, BestPriceFillModel, DataTier, FillModel,
    NextBarOpenFillModel, NoMargin, OneTickSlippageFillModel, VirtualTradingConfig, ZeroLatency,
};
use qx_zhenlu::RiskGate;
use std::collections::BTreeMap;
use std::path::PathBuf;

const MIN_BARS: usize = 3;

pub fn run_multi_leg_backtest(
    spec: &MultiLegBacktestSpec,
    context: &RunContext,
) -> Result<MultiLegBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "多腿 Bar 回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || run_inner(spec, context, None))
        .map_err(|error| attach(error, &correlation_id))?
        .ok_or_else(|| {
            attach(
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    "同步多腿回测未产生报告",
                ),
                &correlation_id,
            )
        })
}

impl MultiLegBacktestSpec {
    pub fn start(self, context: RunContext) -> crate::RunHandle<MultiLegBacktestOutcome> {
        let run_id = context.correlation_id().to_string();
        crate::RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "多腿 Bar 回测")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner(&self, &context, Some(&cancellation))
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }
}

pub(super) fn run_inner(
    spec: &MultiLegBacktestSpec,
    _context: &RunContext,
    cancellation: Option<&CancellationToken>,
) -> Result<Option<MultiLegBacktestOutcome>, AppError> {
    spec.validate()?;
    let primary = read_frame(&spec.primary_bars_path, "主腿")?;
    let reference = read_frame(&spec.reference_bars_path, "对冲腿")?;
    if primary.instrument == reference.instrument {
        return Err(invalid("多腿回测要求两条腿使用不同标的"));
    }
    if primary.ts != reference.ts {
        return Err(invalid("多腿回测要求两条 BarFrame 时间戳完全对齐"));
    }
    let primary_bars: Vec<Bar> = (&primary).into();
    let reference_bars: Vec<Bar> = (&reference).into();
    if primary_bars.len() < MIN_BARS {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            format!("多腿回测至少需要 {MIN_BARS} 根对齐 Bar"),
        ));
    }
    let primary_spec = validate_leg_spec(
        "主腿",
        spec.primary_market_spec.as_ref(),
        &primary.instrument,
        &spec.settlement_currency,
    )?;
    let reference_spec = validate_leg_spec(
        "对冲腿",
        spec.reference_market_spec.as_ref(),
        &reference.instrument,
        &spec.settlement_currency,
    )?;
    if spec.funding_bps != 0 {
        if primary_spec.is_none() || reference_spec.is_none() {
            return Err(invalid(
                "资金费非零时两条腿都必须提供 market spec，以确定产品类型和名义额",
            ));
        }
        if [primary_spec, reference_spec]
            .into_iter()
            .flatten()
            .all(|market| !market.product.is_derivative())
        {
            return Err(invalid(
                "资金费非零但两条腿都不是衍生品；资金费不能记到现货腿",
            ));
        }
    }
    let data_fingerprint = format!(
        "multi-bar:{:016x}:{:016x}",
        primary.digest(),
        reference.digest()
    );
    let config_json = spec.to_json()?;
    let mut config_hasher = Fnv1a::new();
    config_hasher.write_text(&config_json);
    let config_hash = format!("{:016x}", config_hasher.finish());
    let strategy_id = spec.strategy.strategy_id.clone();
    let strategy_version = format!("{}-v1", strategy_id);
    let strategy_config = BuiltinStrategyConfig {
        kind: spec.strategy.kind,
        strategy_id: strategy_id.clone(),
        strategy_version: format!("builtin-{}-v1", spec.strategy.kind.name()),
        instrument: primary.instrument.clone(),
        quantity: Quantity::from_raw(spec.strategy.quantity_raw),
        fast_window: spec.strategy.fast_window,
        slow_window: spec.strategy.slow_window,
        period: spec.strategy.period,
        threshold_bps: spec.strategy.threshold_bps,
        reference_instrument: Some(reference.instrument.clone()),
        primary_policy: None,
        reference_policy: None,
    };
    strategy_config.validate().map_err(invalid)?;
    let mut strategy = BuiltinStrategy::new(strategy_config.clone()).map_err(invalid)?;
    let mut strategy_context = StrategyContext {
        strategy_id: strategy_id.clone(),
        strategy_version: strategy_version.clone(),
        account_id: format!("multi-leg-{}", spec.run_id),
        venue_id: primary.instrument.venue.to_string(),
        data_fingerprint: data_fingerprint.clone(),
        as_of: primary_bars[0].ts,
        positions: BTreeMap::from([
            (primary.instrument.to_string(), 0),
            (reference.instrument.to_string(), 0),
        ]),
        cash: BTreeMap::from([(
            spec.settlement_currency.clone(),
            spec.primary_initial_cash_raw,
        )]),
        available_margin_raw: Some(spec.primary_initial_cash_raw),
        risk_state: "multi-leg-backtest".into(),
    };
    strategy.on_init(&strategy_context).map_err(invalid)?;
    let mut primary_targets = BTreeMap::new();
    let mut reference_targets = BTreeMap::new();
    for (primary_bar, reference_bar) in primary_bars.iter().zip(&reference_bars) {
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            return Ok(None);
        }
        strategy_context.as_of = primary_bar.ts;
        for (instrument, bar) in [
            (&reference.instrument, reference_bar),
            (&primary.instrument, primary_bar),
        ] {
            let event = MarketEvent::Bar {
                instrument: instrument.clone(),
                ts: bar.ts,
                open_raw: bar.open,
                high_raw: bar.high,
                low_raw: bar.low,
                close_raw: bar.close,
                volume_raw: bar.volume,
            };
            let decision = strategy
                .on_event(&strategy_context, &event)
                .map_err(invalid)?;
            for intent in decision.intents {
                let key = intent.instrument.to_string();
                let current = strategy_context.positions.get(&key).copied().unwrap_or(0);
                let delta = if intent.side == Side::Buy {
                    intent.qty.raw()
                } else {
                    -intent.qty.raw()
                };
                let target = current
                    .checked_add(delta)
                    .ok_or_else(|| invalid("策略目标仓位溢出"))?;
                strategy_context.positions.insert(key, target);
                if intent.instrument == primary.instrument {
                    primary_targets.insert(primary_bar.ts, target);
                } else if intent.instrument == reference.instrument {
                    reference_targets.insert(reference_bar.ts, target);
                }
            }
        }
    }

    let primary_report = run_leg(LegRunSpec {
        instrument: &primary.instrument,
        bars: &primary_bars,
        targets: primary_targets.clone(),
        label: "primary",
        initial_cash_raw: spec.primary_initial_cash_raw,
        seed: spec.seed,
        currency: &spec.settlement_currency,
        instrument_spec: primary_spec.cloned(),
        maker_fee_bps: spec.primary_maker_fee_bps.unwrap_or(spec.maker_fee_bps),
        taker_fee_bps: spec.primary_taker_fee_bps.unwrap_or(spec.taker_fee_bps),
        fill_model: spec.fill_model,
        cancellation,
    })?;
    let Some(primary_report) = primary_report else {
        return Ok(None);
    };
    let reference_report = run_leg(LegRunSpec {
        instrument: &reference.instrument,
        bars: &reference_bars,
        targets: reference_targets.clone(),
        label: "reference",
        initial_cash_raw: spec.reference_initial_cash_raw,
        seed: spec.seed,
        currency: &spec.settlement_currency,
        instrument_spec: reference_spec.cloned(),
        maker_fee_bps: spec.reference_maker_fee_bps.unwrap_or(spec.maker_fee_bps),
        taker_fee_bps: spec.reference_taker_fee_bps.unwrap_or(spec.taker_fee_bps),
        fill_model: spec.fill_model,
        cancellation,
    })?;
    let Some(reference_report) = reference_report else {
        return Ok(None);
    };
    let primary_replay =
        ReplayVerifier::verify(primary_report.event_log.events(), &primary_report.ledger)
            .map_err(|error| invariant(format!("主腿事件重放失败: {error}")))?;
    let reference_replay = ReplayVerifier::verify(
        reference_report.event_log.events(),
        &reference_report.ledger,
    )
    .map_err(|error| invariant(format!("对冲腿事件重放失败: {error}")))?;
    if primary_replay.log_digest != primary_report.result_hash()
        || reference_replay.log_digest != reference_report.result_hash()
    {
        return Err(invariant("多腿结果哈希与事件重放哈希不一致"));
    }
    let primary_leg = crate::MultiLegLegFacts {
        label: "primary",
        instrument: &primary.instrument,
        targets: &primary_targets,
        bars: &primary_bars,
        spec: primary_spec,
        report: &primary_report,
    };
    let reference_leg = crate::MultiLegLegFacts {
        label: "reference",
        instrument: &reference.instrument,
        targets: &reference_targets,
        bars: &reference_bars,
        spec: reference_spec,
        report: &reference_report,
    };
    let primary_buckets =
        crate::multi_leg_leg_buckets(&primary_leg, spec.funding_bps).map_err(invalid)?;
    let reference_buckets =
        crate::multi_leg_leg_buckets(&reference_leg, spec.funding_bps).map_err(invalid)?;
    let attribution = crate::multi_leg_group_attributions(
        &strategy_id,
        &primary_leg,
        &reference_leg,
        &primary_buckets,
        &reference_buckets,
    )
    .map_err(invalid)?;
    let totals = crate::multi_leg_group_totals(&attribution.groups);
    let leg_integrity = [
        crate::multi_leg_leg_integrity(&primary_leg, &primary_buckets),
        crate::multi_leg_leg_integrity(&reference_leg, &reference_buckets),
    ];
    let leg_integrity_json = leg_integrity
        .iter()
        .map(crate::multi_leg_integrity_json)
        .collect::<Vec<_>>();
    let pending_reconcile_json =
        crate::multi_leg_pending_reconcile_json(&attribution.pending_reconcile);

    let funding_raw = totals.2;
    let pnl = primary_report
        .final_equity()
        .checked_sub(spec.primary_initial_cash_raw)
        .and_then(|x| {
            x.checked_add(
                reference_report
                    .final_equity()
                    .checked_sub(spec.reference_initial_cash_raw)?,
            )
        })
        .and_then(|x| x.checked_sub(funding_raw))
        .ok_or_else(|| invalid("多腿组合净盈亏（含资金费）溢出"))?;
    let funded = spec
        .primary_initial_cash_raw
        .checked_add(spec.reference_initial_cash_raw)
        .ok_or_else(|| invalid("多腿组合本金溢出"))?;
    let combined = pnl
        .checked_mul(10_000)
        .and_then(|x| x.checked_div(funded))
        .and_then(|x| i32::try_from(x).ok())
        .ok_or_else(|| invalid("多腿组合收益无法表示"))?;

    let artifact = PathBuf::from(&spec.output_dir).join(format!("{}.multi-leg.json", spec.run_id));
    let payload = serde_json::json!({
        "schema_version": 1,
        "run_id": spec.run_id,
        "strategy_id": strategy_id,
        "strategy_version": strategy_version,
        "primary_instrument": primary.instrument.to_string(),
        "reference_instrument": reference.instrument.to_string(),
        "market_specs": {
            "primary": primary_spec,
            "reference": reference_spec,
        },
        "fee_model": {
            "name": "MakerTaker",
            "shared_default": {
                "maker_fee_bps": spec.maker_fee_bps,
                "taker_fee_bps": spec.taker_fee_bps,
            },
            "primary": {
                "maker_fee_bps": spec.primary_maker_fee_bps.unwrap_or(spec.maker_fee_bps),
                "taker_fee_bps": spec.primary_taker_fee_bps.unwrap_or(spec.taker_fee_bps),
            },
            "reference": {
                "maker_fee_bps": spec.reference_maker_fee_bps.unwrap_or(spec.maker_fee_bps),
                "taker_fee_bps": spec.reference_taker_fee_bps.unwrap_or(spec.taker_fee_bps),
            },
        },
        "fill_model": fill_model_name(spec.fill_model),
        "funding_bps": spec.funding_bps,
        "data_fingerprint": data_fingerprint,
        "config_hash": config_hash,
        "primary_result_hash": format!("{:016x}", primary_report.result_hash()),
        "reference_result_hash": format!("{:016x}", reference_report.result_hash()),
        "primary_return_bps": primary_report.return_bps,
        "reference_return_bps": reference_report.return_bps,
        "combined_return_bps": combined,
        "primary_fills": primary_report.fills.len(),
        "reference_fills": reference_report.fills.len(),
        "attribution_totals": {
            "fees_raw": totals.0.to_string(),
            "turnover_raw": totals.1.to_string(),
            "funding_raw": totals.2.to_string(),
            "filled_qty_raw": totals.3.to_string(),
            "margin_peak_raw": totals.4.to_string(),
            "cost_bps": crate::multi_leg_cost_bps(
                totals.0.checked_add(totals.2).ok_or_else(|| invalid("多腿净成本溢出"))?,
                totals.1,
            ).map_err(invalid)?,
            "residual_fees_raw": attribution.residual_fees_raw.to_string(),
            "residual_filled_qty_raw": attribution.residual_filled_qty_raw.to_string(),
        },
        "legs": leg_integrity_json,
        "groups": attribution.groups,
        "pending_reconcile": pending_reconcile_json,
        "replay_events": primary_replay.events + reference_replay.events,
        "replay_ledger_entries": primary_replay.ledger_entries + reference_replay.ledger_entries,
        "matching": "qx-xingban::BacktestEngine/NextBarOpenFillModel",
        "assumptions": ["each leg uses its own initial cash", "both legs are independently replay-verified", "each venue can declare its own maker/taker rates with shared rates as fallback", "funding is attributed only to derivative legs and is included in combined net return", "market specifications, when provided, are validated against each input and set contract size/margin semantics"],
    });
    let payload = serde_json::to_string(&payload)
        .map_err(|error| invariant(format!("多腿产物序列化失败: {error}")))?;
    std::fs::create_dir_all(&spec.output_dir)
        .map_err(|error| AppError::from_io("创建多腿产物目录", &error))?;
    let artifact_path = artifact.to_string_lossy().into_owned();
    match std::fs::read_to_string(&artifact_path) {
        Ok(existing) if existing == payload => {}
        Ok(_) => {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                "相同 run_id 的多腿产物已存在且内容不同",
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_text(&artifact_path, &payload)?
        }
        Err(error) => return Err(AppError::from_io("读取既有多腿产物", &error)),
    }
    Ok(Some(MultiLegBacktestOutcome {
        run_id: spec.run_id.clone(),
        primary_instrument: primary.instrument.to_string(),
        reference_instrument: reference.instrument.to_string(),
        primary_result_hash: format!("{:016x}", primary_report.result_hash()),
        reference_result_hash: format!("{:016x}", reference_report.result_hash()),
        combined_return_bps: combined,
        primary_fills: primary_report.fills.len() as u64,
        reference_fills: reference_report.fills.len() as u64,
        attribution_artifact: artifact_path,
    }))
}

fn read_frame(path: &str, label: &str) -> Result<BarFrame, AppError> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| AppError::from_io(&format!("读取{label} BarFrame {path}"), &error))?;
    BarFrame::from_json(&payload)
        .map_err(|error| invalid(format!("{label} BarFrame 无效: {error:?}")))
}
struct LegRunSpec<'a> {
    instrument: &'a InstrumentId,
    bars: &'a [Bar],
    targets: BTreeMap<u64, i128>,
    label: &'a str,
    initial_cash_raw: i128,
    seed: u64,
    currency: &'a str,
    instrument_spec: Option<TradingInstrumentSpec>,
    maker_fee_bps: i64,
    taker_fee_bps: i64,
    fill_model: MultiLegFillModel,
    cancellation: Option<&'a CancellationToken>,
}
fn run_leg(input: LegRunSpec<'_>) -> Result<Option<qx_xingban::BacktestReport>, AppError> {
    let mut strategy = ScheduledTargets {
        instrument: input.instrument.clone(),
        targets: input.targets,
        account_id: format!("multi-{}", input.label),
    };
    let config = BacktestConfig {
        instrument: input.instrument.clone(),
        instrument_spec: input.instrument_spec.clone(),
        account_id: strategy.account_id.clone(),
        currency: input.currency.into(),
        initial_cash: Money::from_raw(input.initial_cash_raw),
        multiplier: 1,
        fill: fill_model_for(input.fill_model, input.instrument_spec.as_ref())?,
        fee: Box::new(MakerTakerFeeModel {
            maker_bp: input.maker_fee_bps,
            taker_bp: input.taker_fee_bps,
        }),
        data_tier: DataTier::Bar,
        latency: Box::new(ZeroLatency),
        margin: Box::new(NoMargin),
        seed: input.seed,
        risk: RiskGate::conservative_default(),
        virtual_trading: VirtualTradingConfig::default(),
    };
    let engine = BacktestEngine::new(config);
    match input.cancellation {
        Some(token) => engine
            .run_with_cancel(input.bars, &mut strategy, || token.is_cancelled())
            .map_err(|error| AppError::from_qx_error(&error)),
        None => engine
            .run(input.bars, &mut strategy)
            .map(Some)
            .map_err(|error| AppError::from_qx_error(&error)),
    }
}

struct ScheduledTargets {
    instrument: InstrumentId,
    targets: BTreeMap<u64, i128>,
    account_id: String,
}
impl BarStrategy for ScheduledTargets {
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
            policy: None,
        })
    }
}

fn invalid(message: impl std::fmt::Display) -> AppError {
    AppError::new(
        AppErrorCategory::InvalidInput,
        format!("多腿回测输入或策略非法: {message}"),
    )
}

fn validate_leg_spec<'a>(
    label: &str,
    market_spec: Option<&'a TradingInstrumentSpec>,
    instrument: &InstrumentId,
    settlement_currency: &str,
) -> Result<Option<&'a TradingInstrumentSpec>, AppError> {
    let Some(market) = market_spec else {
        return Ok(None);
    };
    if market.instrument != *instrument {
        return Err(invalid(format!(
            "{label} market spec instrument {} 与 BarFrame 标的 {instrument} 不一致",
            market.instrument
        )));
    }
    if market.settlement_currency != settlement_currency {
        return Err(invalid(format!(
            "{label} market spec 结算币 {} 与 spec settlement_currency {settlement_currency} 不一致",
            market.settlement_currency
        )));
    }
    Ok(Some(market))
}

fn fill_model_for(
    kind: MultiLegFillModel,
    market_spec: Option<&TradingInstrumentSpec>,
) -> Result<Box<dyn FillModel>, AppError> {
    Ok(match kind {
        MultiLegFillModel::NextBarOpen => Box::new(NextBarOpenFillModel),
        MultiLegFillModel::BestPrice => Box::new(BestPriceFillModel),
        MultiLegFillModel::OneTickSlippage => {
            let spec = market_spec.ok_or_else(|| {
                invalid("one_tick_slippage 要求两条腿都提供含真实 price_tick 的 market spec")
            })?;
            Box::new(OneTickSlippageFillModel {
                tick: spec.price_tick,
            })
        }
    })
}

fn fill_model_name(kind: MultiLegFillModel) -> &'static str {
    match kind {
        MultiLegFillModel::NextBarOpen => "next_bar_open",
        MultiLegFillModel::BestPrice => "best_price",
        MultiLegFillModel::OneTickSlippage => "one_tick_slippage",
    }
}

fn invariant(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}
