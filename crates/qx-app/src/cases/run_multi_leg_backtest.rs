use super::{artifacts::write_text, attach, guard::guard_panics};
use crate::run_handle::CancellationToken;
use crate::{
    AppError, AppErrorCategory, CallerCapability, MultiLegBacktestOutcome, MultiLegBacktestSpec,
    MultiLegFillModel, RunContext,
};
use qx_core::{Fnv1a, MakerTakerFeeModel, Money, Quantity};
use qx_datastruct::BarFrame;
use qx_strategy::{BuiltinStrategy, BuiltinStrategyConfig, Strategy, StrategyContext};
use qx_xingban::{
    NativeBarStrategy, NoMargin, PortfolioBarEngine, PortfolioBarMarket, PortfolioBarSessionConfig,
    ZeroLatency,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(super) mod custom_strategy;
mod funding;
mod market_spec;
mod models;
mod portfolio_ledger;
mod report_metrics;
mod strategy_targets;
mod validation;
pub(super) use custom_strategy::InjectedMultiLegStrategy;
use funding::{attribute_execution_funding, rates_for_legs};
pub(super) use market_spec::{execution_market_spec, validate_leg_spec};
pub(super) use models::{fill_model_for, fill_model_name};
use report_metrics::{fill_result_hash, leg_return_bps, LegReturnInput};
use strategy_targets::{RecordingMultiLegStrategy, TargetSchedules};
use validation::{read_frame, validate_custom_sizing};

const MIN_BARS: usize = 3;

pub fn run_multi_leg_backtest(
    spec: &MultiLegBacktestSpec,
    context: &RunContext,
) -> Result<MultiLegBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "多腿 Bar 回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        run_inner(spec, context, None, None, None)
    })
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
                run_inner(&self, &context, Some(&cancellation), None, None)
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }
}

pub(super) fn run_inner(
    spec: &MultiLegBacktestSpec,
    _context: &RunContext,
    cancellation: Option<&CancellationToken>,
    injected: Option<InjectedMultiLegStrategy>,
    funding_provider: Option<
        &super::run_multi_leg_backtest_with_funding_provider::FundingRateProviderProvenance,
    >,
) -> Result<Option<MultiLegBacktestOutcome>, AppError> {
    spec.validate()?;
    validate_custom_sizing(spec, injected.is_some())?;
    let primary = read_frame(&spec.primary_bars_path, "主腿")?;
    let reference = read_frame(&spec.reference_bars_path, "对冲腿")?;
    if primary.instrument == reference.instrument {
        return Err(invalid("多腿回测要求两条腿使用不同标的"));
    }
    let primary_bars: Vec<qx_guanxing::Bar> = (&primary).into();
    let reference_bars: Vec<qx_guanxing::Bar> = (&reference).into();
    if primary_bars.len() < MIN_BARS || reference_bars.len() < MIN_BARS {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            format!("多腿回测每条腿至少需要 {MIN_BARS} 根 Bar"),
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
    let strategy_source_sha256 = injected
        .as_ref()
        .map(|custom| custom.identity.source_sha256.clone());
    if let Some(custom) = injected.as_ref() {
        config_hasher.write_text(&custom.identity.strategy_id);
        config_hasher.write_text(&custom.identity.strategy_version);
        config_hasher.write_text(&custom.identity.source_sha256);
    }
    funding::hash_provider_source(&mut config_hasher, funding_provider);
    let config_hash = format!("{:016x}", config_hasher.finish());
    let strategy_id = injected.as_ref().map_or_else(
        || spec.strategy.strategy_id.clone(),
        |custom| custom.identity.strategy_id.clone(),
    );
    let strategy_version = injected.as_ref().map_or_else(
        || format!("{}-v1", strategy_id),
        |custom| custom.identity.strategy_version.clone(),
    );
    let strategy_config = BuiltinStrategyConfig {
        kind: spec.strategy.kind,
        strategy_id: strategy_id.clone(),
        strategy_version: format!("builtin-{}-v1", spec.strategy.kind.name()),
        instrument: primary.instrument.clone(),
        quantity: Quantity::from_raw(spec.strategy.quantity_raw),
        target_notional_raw: spec.strategy.target_notional_raw,
        primary_market_spec: primary_spec.cloned(),
        reference_market_spec: reference_spec.cloned(),
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
    let caller_strategy = injected.is_some();
    let strategy: Box<dyn Strategy> = match injected {
        Some(custom) => custom.strategy,
        None => Box::new(BuiltinStrategy::new(strategy_config).map_err(invalid)?),
    };
    let shared_account_id = spec
        .account_id
        .clone()
        .unwrap_or_else(|| format!("multi-leg-{}", spec.run_id));
    let initial_cash_raw = spec
        .primary_initial_cash_raw
        .checked_add(spec.reference_initial_cash_raw)
        .ok_or_else(|| invalid("多腿共享账户初始本金溢出"))?;
    let strategy_context = StrategyContext {
        strategy_id: strategy_id.clone(),
        strategy_version: strategy_version.clone(),
        account_id: shared_account_id.clone(),
        venue_id: primary.instrument.venue.to_string(),
        data_fingerprint: data_fingerprint.clone(),
        as_of: primary_bars[0].ts,
        positions: BTreeMap::from([
            (primary.instrument.to_string(), 0),
            (reference.instrument.to_string(), 0),
        ]),
        cash: BTreeMap::from([(spec.settlement_currency.clone(), initial_cash_raw)]),
        available_margin_raw: Some(initial_cash_raw),
        risk_state: "multi-leg-backtest".into(),
    };
    let target_schedules = std::sync::Arc::new(std::sync::Mutex::new(TargetSchedules {
        primary: BTreeMap::new(),
        reference: BTreeMap::new(),
    }));
    let recording_strategy = RecordingMultiLegStrategy::new(
        strategy,
        primary.instrument.clone(),
        reference.instrument.clone(),
        target_schedules.clone(),
        caller_strategy,
    );
    let mut strategy_adapter =
        NativeBarStrategy::new(recording_strategy, strategy_context).with_virtual_position();
    strategy_adapter.initialize().map_err(invalid)?;
    let primary_execution_spec =
        execution_market_spec(primary_spec, &primary.instrument, &spec.settlement_currency);
    let reference_execution_spec = execution_market_spec(
        reference_spec,
        &reference.instrument,
        &spec.settlement_currency,
    );
    let markets = vec![
        PortfolioBarMarket {
            instrument: primary.instrument.clone(),
            priority: 1,
            bars: &primary_bars,
            spec: primary_execution_spec.clone(),
            fill: fill_model_for(spec.fill_model, primary_spec)?,
            fee: Box::new(MakerTakerFeeModel {
                maker_bp: spec.primary_maker_fee_bps.unwrap_or(spec.maker_fee_bps),
                taker_bp: spec.primary_taker_fee_bps.unwrap_or(spec.taker_fee_bps),
            }),
            latency: Box::new(ZeroLatency),
            margin: Box::new(NoMargin),
            seed: spec.seed,
        },
        PortfolioBarMarket {
            instrument: reference.instrument.clone(),
            priority: 0,
            bars: &reference_bars,
            spec: reference_execution_spec.clone(),
            fill: fill_model_for(spec.fill_model, reference_spec)?,
            fee: Box::new(MakerTakerFeeModel {
                maker_bp: spec.reference_maker_fee_bps.unwrap_or(spec.maker_fee_bps),
                taker_bp: spec.reference_taker_fee_bps.unwrap_or(spec.taker_fee_bps),
            }),
            latency: Box::new(ZeroLatency),
            margin: Box::new(NoMargin),
            seed: spec.seed,
        },
    ];
    let funding_rates = rates_for_legs(
        spec.funding_bps,
        &primary.instrument,
        primary_spec,
        &reference.instrument,
        reference_spec,
    );
    let engine = PortfolioBarEngine::new(
        PortfolioBarSessionConfig {
            account_id: shared_account_id.clone(),
            currency: spec.settlement_currency.clone(),
            initial_cash: Money::from_raw(initial_cash_raw),
            risk: qx_zhenlu::RiskGate::conservative_default(),
            funding_bps: funding_rates,
            funding_events: spec.funding_events.clone(),
        },
        markets,
    )
    .map_err(|error| AppError::from_qx_error(&error))?;
    let portfolio_report = match cancellation {
        Some(token) => engine.run_with_cancel(&mut strategy_adapter, || token.is_cancelled()),
        None => engine.run(&mut strategy_adapter).map(Some),
    }
    .map_err(|error| AppError::from_qx_error(&error))?;
    let Some(portfolio_report) = portfolio_report else {
        return Ok(None);
    };
    strategy_adapter.strategy.on_stop().map_err(invalid)?;
    let targets = target_schedules
        .lock()
        .map_err(|_| invariant("多腿目标记录锁已中毒"))?
        .clone();
    let primary_fills = &portfolio_report.fills_by_instrument[&primary.instrument];
    let reference_fills = &portfolio_report.fills_by_instrument[&reference.instrument];
    let primary_rejections = &portfolio_report.rejections_by_instrument[&primary.instrument];
    let reference_rejections = &portfolio_report.rejections_by_instrument[&reference.instrument];
    let primary_leg = crate::MultiLegLegFacts {
        label: "primary",
        account_id: &shared_account_id,
        instrument: &primary.instrument,
        targets: &targets.primary,
        bars: &primary_bars,
        spec: primary_spec,
        fills: primary_fills,
        event_log: primary_rejections,
        clock_end: portfolio_report.clock_end,
    };
    let reference_leg = crate::MultiLegLegFacts {
        label: "reference",
        account_id: &shared_account_id,
        instrument: &reference.instrument,
        targets: &targets.reference,
        bars: &reference_bars,
        spec: reference_spec,
        fills: reference_fills,
        event_log: reference_rejections,
        clock_end: portfolio_report.clock_end,
    };
    let mut primary_buckets = crate::multi_leg_leg_buckets(&primary_leg, 0).map_err(invalid)?;
    let mut reference_buckets = crate::multi_leg_leg_buckets(&reference_leg, 0).map_err(invalid)?;
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
    let account_ledger = portfolio_ledger::shared_account_ledger(
        &shared_account_id,
        initial_cash_raw,
        &[primary.instrument.clone(), reference.instrument.clone()],
        &portfolio_report.ledger,
    )?;

    let final_equity = portfolio_report
        .equity
        .last()
        .map(|(_, equity)| *equity)
        .unwrap_or(initial_cash_raw);
    let pnl = final_equity
        .checked_sub(initial_cash_raw)
        .ok_or_else(|| invalid("多腿组合净盈亏溢出"))?;
    let funded = initial_cash_raw;
    let combined = pnl
        .checked_mul(10_000)
        .and_then(|x| x.checked_div(funded))
        .and_then(|x| i32::try_from(x).ok())
        .ok_or_else(|| invalid("多腿组合收益无法表示"))?;
    let (funding_raw, primary_funding, reference_funding, ungrouped_funding_raw) =
        attribute_execution_funding(
            &portfolio_report,
            &primary.instrument,
            &mut primary_buckets,
            &reference.instrument,
            &mut reference_buckets,
            totals.2,
        )?;
    let primary_return = leg_return_bps(LegReturnInput {
        ledger: &portfolio_report.ledger,
        account_id: &shared_account_id,
        instrument: &primary.instrument,
        spec: &primary_execution_spec,
        bars: &primary_bars,
        fills: primary_fills,
        funding_raw: primary_funding,
        initial_cash_raw: spec.primary_initial_cash_raw,
    })?;
    let reference_return = leg_return_bps(LegReturnInput {
        ledger: &portfolio_report.ledger,
        account_id: &shared_account_id,
        instrument: &reference.instrument,
        spec: &reference_execution_spec,
        bars: &reference_bars,
        fills: reference_fills,
        funding_raw: reference_funding,
        initial_cash_raw: spec.reference_initial_cash_raw,
    })?;
    let primary_result_hash = fill_result_hash(&primary.instrument, primary_fills)?;
    let reference_result_hash = fill_result_hash(&reference.instrument, reference_fills)?;

    let artifact = PathBuf::from(&spec.output_dir).join(format!("{}.multi-leg.json", spec.run_id));
    let mut payload = serde_json::json!({
        "schema_version": 2,
        "run_id": spec.run_id,
        "strategy_id": strategy_id,
        "strategy_version": strategy_version,
        "strategy_source_sha256": strategy_source_sha256,
        "primary_instrument": primary.instrument.to_string(),
        "reference_instrument": reference.instrument.to_string(),
        "market_specs": {
            "primary": primary_spec,
            "reference": reference_spec,
        },
        "effective_market_specs": {
            "primary": primary_execution_spec,
            "reference": reference_execution_spec,
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
        "shared_result_hash": format!("{:016x}", portfolio_report.result_hash()),
        "shared_event_log": portfolio_report.event_log.events(),
        "primary_result_hash": primary_result_hash.clone(),
        "reference_result_hash": reference_result_hash.clone(),
        "primary_return_bps": primary_return,
        "reference_return_bps": reference_return,
        "combined_return_bps": combined,
        "primary_fills": primary_fills.len(),
        "reference_fills": reference_fills.len(),
        "attribution_totals": {
            "fees_raw": totals.0.to_string(),
            "turnover_raw": totals.1.to_string(),
            "funding_raw": funding_raw.to_string(),
            "completed_groups_funding_raw": totals.2.to_string(),
            "ungrouped_funding_raw": ungrouped_funding_raw.to_string(),
            "filled_qty_raw": totals.3.to_string(),
            "margin_peak_raw": totals.4.to_string(),
            "cost_bps": crate::multi_leg_cost_bps(
                totals.0.checked_add(funding_raw).ok_or_else(|| invalid("多腿净成本溢出"))?,
                totals.1,
            ).map_err(invalid)?,
            "residual_fees_raw": attribution.residual_fees_raw.to_string(),
            "residual_filled_qty_raw": attribution.residual_filled_qty_raw.to_string(),
        },
        "legs": leg_integrity_json,
        "groups": attribution.groups,
        "pending_reconcile": pending_reconcile_json,
        "account_ledger": account_ledger,
        "replay_events": portfolio_report.event_log.events().len(),
        "replay_ledger_entries": portfolio_report.ledger.entries().len(),
        "execution_funding_by_instrument_raw": portfolio_report.funding_by_instrument_raw.iter().map(|(instrument, amount)| (instrument.to_string(), amount.to_string())).collect::<BTreeMap<_, _>>(),
        "execution_funding_events": portfolio_report.funding_events_by_instrument.iter().flat_map(|(instrument, events)| events.iter().map(|(ts, amount)| serde_json::json!({"instrument": instrument.to_string(), "ts": ts, "amount_raw": amount.to_string()}))).collect::<Vec<_>>(),
        "matching": "qx-xingban::PortfolioBarEngine/NextBarOpenFillModel",
        "leg_return_basis": "leg pnl divided by declared leg initial cash; execution uses the shared account and does not enforce leg capital reservations",
        "assumptions": ["both legs share one account Ledger, OMS, EventLog, and buying-power pool", "same-timestamp callbacks are deterministic and market orders compete by declared priority then instrument id", "all market events are replay-verified together", "each venue can declare its own maker/taker rates with shared rates as fallback", "signed funding settles at UTC 8-hour boundaries on the last observed mark using the open derivative position and is applied to the shared Ledger before callbacks at that timestamp", "funding settlement fails closed if its latest mark is more than one funding interval old", "funding debit or receipt is included exactly once in ending equity and each settlement is present in replay facts", "funding input is caller-supplied constant bps or timestamped historical events; event inputs are recorded in this artifact and do not imply an exchange history provider", "missing market specs retain the legacy spot assumption and are represented by a normalized execution spec"],
    });
    funding::apply_artifact_metadata(&mut payload, spec, funding_provider)?;
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
        primary_result_hash,
        reference_result_hash,
        combined_return_bps: combined,
        primary_fills: primary_fills.len() as u64,
        reference_fills: reference_fills.len() as u64,
        attribution_artifact: artifact_path,
        account_id: spec.account_id.clone(),
    }))
}

pub(super) fn invalid(message: impl std::fmt::Display) -> AppError {
    AppError::new(
        AppErrorCategory::InvalidInput,
        format!("多腿回测输入或策略非法: {message}"),
    )
}

fn invariant(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}
