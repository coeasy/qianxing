use super::{artifacts::write_text, attach, guard::guard_panics};
use crate::run_handle::CancellationToken;
use crate::{
    AppError, AppErrorCategory, CallerCapability, MultiLegBacktestOutcome, MultiLegBacktestSpec,
    MultiLegFillModel, RunContext,
};
use qx_core::{Fnv1a, Quantity, ReplayVerifier};
use qx_datastruct::BarFrame;
use qx_strategy::{BuiltinStrategy, BuiltinStrategyConfig, Strategy, StrategyContext};
use qx_xingban::{PortfolioBarSeries, PortfolioBarTimeline};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(super) mod custom_strategy;
mod leg;
mod market_spec;
mod models;
mod portfolio_ledger;
mod strategy_targets;
mod targets;
mod validation;
pub(super) use custom_strategy::InjectedMultiLegStrategy;
use leg::{run_leg, LegRunSpec};
pub(super) use market_spec::validate_leg_spec;
pub(super) use models::{fill_model_for, fill_model_name};
use strategy_targets::build_target_schedules;
use validation::{read_frame, validate_custom_sizing};

const MIN_BARS: usize = 3;

pub fn run_multi_leg_backtest(
    spec: &MultiLegBacktestSpec,
    context: &RunContext,
) -> Result<MultiLegBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "多腿 Bar 回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || run_inner(spec, context, None, None))
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
                run_inner(&self, &context, Some(&cancellation), None)
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
    let timeline = PortfolioBarTimeline::new(&[
        PortfolioBarSeries {
            instrument: &primary.instrument,
            priority: 1,
            bars: &primary_bars,
        },
        PortfolioBarSeries {
            instrument: &reference.instrument,
            priority: 0,
            bars: &reference_bars,
        },
    ])
    .map_err(invalid)?;
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
    let mut strategy: Box<dyn Strategy> = match injected {
        Some(custom) => custom.strategy,
        None => Box::new(BuiltinStrategy::new(strategy_config).map_err(invalid)?),
    };
    let mut strategy_context = StrategyContext {
        strategy_id: strategy_id.clone(),
        strategy_version: strategy_version.clone(),
        account_id: spec
            .account_id
            .clone()
            .unwrap_or_else(|| format!("multi-leg-{}", spec.run_id)),
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
    let Some(targets) = build_target_schedules(
        strategy.as_mut(),
        &mut strategy_context,
        &timeline,
        &primary.instrument,
        &reference.instrument,
        &spec.settlement_currency,
        spec.primary_initial_cash_raw,
        spec.reference_initial_cash_raw,
        caller_strategy,
        cancellation,
    )?
    else {
        return Ok(None);
    };
    let primary_account_id = spec
        .account_id
        .clone()
        .unwrap_or_else(|| "multi-primary".into());
    let reference_account_id = spec
        .account_id
        .clone()
        .unwrap_or_else(|| "multi-reference".into());
    let primary_report = run_leg(LegRunSpec {
        instrument: &primary.instrument,
        bars: &primary_bars,
        targets: targets.primary.clone(),
        account_id: primary_account_id.clone(),
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
        targets: targets.reference.clone(),
        account_id: reference_account_id.clone(),
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
        account_id: &primary_account_id,
        instrument: &primary.instrument,
        targets: &targets.primary,
        bars: &primary_bars,
        spec: primary_spec,
        report: &primary_report,
    };
    let reference_leg = crate::MultiLegLegFacts {
        label: "reference",
        account_id: &reference_account_id,
        instrument: &reference.instrument,
        targets: &targets.reference,
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
    let account_ledger = portfolio_ledger::for_spec(
        spec,
        &primary.instrument,
        &reference.instrument,
        &primary_report.ledger,
        &reference_report.ledger,
    )?;

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
    let mut payload = serde_json::json!({
        "schema_version": 1,
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
        "account_ledger": account_ledger,
        "replay_events": primary_replay.events + reference_replay.events,
        "replay_ledger_entries": primary_replay.ledger_entries + reference_replay.ledger_entries,
        "matching": "qx-xingban::BacktestEngine/NextBarOpenFillModel",
        "assumptions": ["each leg uses its own initial cash", "both legs are independently replay-verified", "each venue can declare its own maker/taker rates with shared rates as fallback", "funding is attributed only to derivative legs and is included in combined net return", "market specifications, when provided, are validated against each input and set contract size/margin semantics"],
    });
    if let Some(account_id) = spec.account_id.as_deref() {
        if let Some(object) = payload.as_object_mut() {
            object.insert("account_id".into(), serde_json::json!(account_id));
            object.insert("primary_account_id".into(), serde_json::json!(account_id));
            object.insert("reference_account_id".into(), serde_json::json!(account_id));
        }
    }
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
