use super::{artifacts::write_text, attach, guard::guard_panics};
use crate::run_handle::CancellationToken;
use crate::{
    AppError, AppErrorCategory, CallerCapability, MultiLegBacktestOutcome, MultiLegBacktestSpec,
    MultiLegFillModel, RunContext,
};
use qx_core::{InstrumentId, MakerTakerFeeModel, Money, Quantity, RunManifest};
use qx_datastruct::BarFrame;
use qx_spec::CanonicalDatasetIdentity;
use qx_strategy::{BuiltinStrategy, BuiltinStrategyConfig, Strategy, StrategyContext};
use qx_xingban::{
    NativeBarStrategy, NoMargin, PortfolioBarEngine, PortfolioBarMarket, PortfolioBarSessionConfig,
    ZeroLatency,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(crate) mod canonical_spec;
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
use report_metrics::{fill_result_hash, leg_return_bps, leg_return_bps_exact};
use strategy_targets::{RecordingMultiLegStrategy, TargetSchedules};
use validation::{read_frame, validate_custom_sizing};

pub(super) fn read_bar_frame(path: &str, label: &str) -> Result<BarFrame, AppError> {
    validation::read_frame(path, label)
}

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
    context: &RunContext,
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
    let primary_fingerprint = qx_data::fingerprint_bar_frame(&primary)
        .map_err(|error| invalid(format!("primary BarFrame 内容指纹计算失败：{error}")))?;
    let reference_fingerprint = qx_data::fingerprint_bar_frame(&reference)
        .map_err(|error| invalid(format!("reference BarFrame 内容指纹计算失败：{error}")))?;
    let dataset_quality = match (
        spec.primary_dataset_manifest_path.as_deref(),
        spec.reference_dataset_manifest_path.as_deref(),
    ) {
        (Some(primary_manifest), Some(reference_manifest)) => Some(serde_json::json!({
            "primary": super::run_multi_market_manifest::verify_dataset_quality(
                primary_manifest,
                &spec.primary_bars_path,
                &primary,
                &primary_fingerprint,
            )?,
            "reference": super::run_multi_market_manifest::verify_dataset_quality(
                reference_manifest,
                &spec.reference_bars_path,
                &reference,
                &reference_fingerprint,
            )?,
        })),
        (None, None) => None,
        _ => return Err(invalid("多腿回测的数据质量清单必须成对提供")),
    };
    let data_fingerprint = if let Some(quality) = &dataset_quality {
        format!(
            "multi-bar:{primary_fingerprint}:{reference_fingerprint}:quality:{}:{}",
            quality["primary"]["manifest_fingerprint"]
                .as_str()
                .unwrap_or_default(),
            quality["reference"]["manifest_fingerprint"]
                .as_str()
                .unwrap_or_default(),
        )
    } else {
        format!("multi-bar:{primary_fingerprint}:{reference_fingerprint}")
    };
    let strategy_source_sha256 = injected
        .as_ref()
        .map(|custom| custom.identity.source_sha256.clone());
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
    let (initial_cash_exact, initial_cash_raw) = spec.combined_initial_cash()?;
    let cash = BTreeMap::from([(spec.settlement_currency.clone(), initial_cash_raw)]);
    let exact_context_cash = (spec.schema_version
        == crate::MULTI_LEG_BACKTEST_EXACT_SCHEMA_VERSION)
        .then_some(initial_cash_exact);
    let cash_exact = exact_context_cash
        .map(|amount| BTreeMap::from([(spec.settlement_currency.clone(), amount)]))
        .unwrap_or_default();
    let strategy_context = StrategyContext {
        strategy_id: strategy_id.clone(),
        strategy_version: strategy_version.clone(),
        account_id: shared_account_id.clone(),
        venue_id: primary.instrument.venue.to_string(),
        data_fingerprint: data_fingerprint.clone(),
        as_of: primary_bars[0].ts,
        positions_exact: std::collections::BTreeMap::new(),
        cash_exact,
        available_margin_exact: exact_context_cash,
        positions: BTreeMap::from([
            (primary.instrument.to_string(), 0),
            (reference.instrument.to_string(), 0),
        ]),
        available_margin_raw: crate::cases::strategy_available_margin_raw(
            &cash,
            &spec.settlement_currency,
        ),
        cash,
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
    if let Err(start_error) = strategy_adapter
        .strategy
        .on_start(&strategy_adapter.context)
    {
        let stop_error = strategy_adapter.strategy.on_stop().err();
        return Err(invalid(match stop_error {
            Some(stop_error) => {
                format!("多腿策略 on_start 失败: {start_error}; on_stop 也失败: {stop_error}")
            }
            None => format!("多腿策略 on_start 失败: {start_error}"),
        }));
    }
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
            initial_cash_exact: spec.primary_initial_cash_exact.map(|_| initial_cash_exact),
            risk: qx_zhenlu::RiskGate::conservative_default(),
            funding_bps: funding_rates,
            funding_events: spec.funding_events.clone(),
        },
        markets,
    )
    .map_err(|error| AppError::from_qx_error(&error))?;
    let portfolio_result = match cancellation {
        Some(token) => engine.run_with_cancel(&mut strategy_adapter, || token.is_cancelled()),
        None => engine.run(&mut strategy_adapter).map(Some),
    }
    .map_err(|error| AppError::from_qx_error(&error));
    let stop_result = strategy_adapter.strategy.on_stop().map_err(invalid);
    let portfolio_report = match (portfolio_result, stop_result) {
        (Ok(report), Ok(())) => report,
        (Err(primary_error), Ok(())) => return Err(primary_error),
        (Ok(_), Err(stop_error)) => return Err(stop_error),
        (Err(primary_error), Err(stop_error)) => {
            return Err(AppError::new(
                primary_error.category(),
                format!("{primary_error}; 多腿策略 on_stop 失败: {stop_error}"),
            ));
        }
    };
    let Some(portfolio_report) = portfolio_report else {
        return Ok(None);
    };
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
    let totals = crate::multi_leg_group_totals(&attribution.groups).map_err(invalid)?;
    let leg_integrity = [
        crate::multi_leg_leg_integrity(&primary_leg, &primary_buckets).map_err(invalid)?,
        crate::multi_leg_leg_integrity(&reference_leg, &reference_buckets).map_err(invalid)?,
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
        (spec.schema_version == crate::MULTI_LEG_BACKTEST_EXACT_SCHEMA_VERSION)
            .then_some(initial_cash_exact),
        &[primary.instrument.clone(), reference.instrument.clone()],
        &portfolio_report.ledger,
    )?;

    let final_equity_exact = portfolio_report
        .equity_exact
        .last()
        .map(|(_, _, equity)| *equity)
        .unwrap_or(initial_cash_exact);
    let combined = crate::report_analysis::exact_equity_total_return_bps_v1(
        initial_cash_exact,
        final_equity_exact,
    )
    .map_err(invalid)?;
    let (funding_raw, primary_funding, reference_funding, ungrouped_funding_raw) =
        attribute_execution_funding(
            &portfolio_report,
            &primary.instrument,
            &mut primary_buckets,
            &reference.instrument,
            &mut reference_buckets,
            totals.2,
        )?;
    let exact_leg_pnl = |instrument: &InstrumentId| {
        portfolio_report
            .instrument_equity_contributions_exact
            .get(instrument)
            .and_then(|curve| curve.last())
            .map(|(_, _, pnl)| *pnl)
            .ok_or_else(|| invalid(format!("多腿精确权益贡献缺少期末标的: {instrument}")))
    };
    let primary_pnl_exact = exact_leg_pnl(&primary.instrument)?;
    let reference_pnl_exact = exact_leg_pnl(&reference.instrument)?;
    let primary_pnl_replayed = crate::multi_leg_leg_pnl_exact(crate::MultiLegLegReturnFacts {
        ledger: &portfolio_report.ledger,
        account_id: &shared_account_id,
        instrument: &primary.instrument.to_string(),
        market_spec: &primary_execution_spec,
        frame: &primary,
        fills: primary_fills,
        funding_cost_raw: primary_funding,
        initial_cash_raw: spec.primary_initial_cash_raw,
    })
    .map_err(invalid)?;
    let reference_pnl_replayed = crate::multi_leg_leg_pnl_exact(crate::MultiLegLegReturnFacts {
        ledger: &portfolio_report.ledger,
        account_id: &shared_account_id,
        instrument: &reference.instrument.to_string(),
        market_spec: &reference_execution_spec,
        frame: &reference,
        fills: reference_fills,
        funding_cost_raw: reference_funding,
        initial_cash_raw: spec.reference_initial_cash_raw,
    })
    .map_err(invalid)?;
    if primary_pnl_exact != primary_pnl_replayed || reference_pnl_exact != reference_pnl_replayed {
        return Err(invariant("多腿引擎 exact 标的贡献与重放账簿复算不一致"));
    }
    let (primary_initial_cash_exact, reference_initial_cash_exact) =
        spec.initial_cash_exact_values()?;
    let (primary_return, reference_return) =
        if spec.schema_version == crate::MULTI_LEG_BACKTEST_EXACT_SCHEMA_VERSION {
            (
                leg_return_bps_exact(primary_pnl_exact, primary_initial_cash_exact)?,
                leg_return_bps_exact(reference_pnl_exact, reference_initial_cash_exact)?,
            )
        } else {
            (
                leg_return_bps(primary_pnl_exact, spec.primary_initial_cash_raw)?,
                leg_return_bps(reference_pnl_exact, spec.reference_initial_cash_raw)?,
            )
        };
    let primary_result_hash = fill_result_hash(&primary.instrument, primary_fills)?;
    let reference_result_hash = fill_result_hash(&reference.instrument, reference_fills)?;
    let spec_artifact = serde_json::to_value(spec)
        .map_err(|error| invariant(format!("多腿规格产物序列化失败: {error}")))?;
    let market_data = serde_json::json!([
        {
            "instrument": primary.instrument.to_string(),
            "path": spec.primary_bars_path,
            "content_hash": primary_fingerprint,
            "row_count": primary.ts.len(),
            "start_ts_ms": primary.ts.first(),
            "end_ts_ms": primary.ts.last(),
            "dataset_manifest_path": spec.primary_dataset_manifest_path,
        },
        {
            "instrument": reference.instrument.to_string(),
            "path": spec.reference_bars_path,
            "content_hash": reference_fingerprint,
            "row_count": reference.ts.len(),
            "start_ts_ms": reference.ts.first(),
            "end_ts_ms": reference.ts.last(),
            "dataset_manifest_path": spec.reference_dataset_manifest_path,
        }
    ]);
    let ledger_entries = serde_json::to_value(portfolio_report.ledger.entries())
        .map_err(|error| invariant(format!("多腿重放账簿产物序列化失败: {error}")))?;
    let equity_curve = portfolio_report
        .equity
        .iter()
        .map(|(ts, equity)| serde_json::json!([ts, equity.to_string()]))
        .collect::<Vec<_>>();
    let equity_curve_exact = portfolio_report
        .equity_exact
        .iter()
        .map(|(index, ts, equity)| serde_json::json!([index, ts, equity.to_string()]))
        .collect::<Vec<_>>();

    std::fs::create_dir_all(&spec.output_dir)
        .map_err(|error| AppError::from_io("创建多腿产物目录", &error))?;
    let output_root = PathBuf::from(&spec.output_dir)
        .canonicalize()
        .map_err(|error| AppError::from_io("解析多腿产物目录", &error))?;
    let artifact = output_root.join(format!("{}.multi-leg.json", spec.run_id));
    let artifact_path = artifact.to_string_lossy().into_owned();
    let run_manifest_path = output_root.join(format!("{}.multi-leg.run.json", spec.run_id));
    let run_manifest_path_string = run_manifest_path.to_string_lossy().into_owned();
    let shared_result_hash = format!("{:016x}", portfolio_report.result_hash());
    let event_hash = format!("{:016x}", portfolio_report.event_log.digest());
    let runtime_version = format!("qx-app-{}", env!("CARGO_PKG_VERSION"));
    let datasets = vec![
        CanonicalDatasetIdentity {
            role: "bars:primary".into(),
            instrument: primary.instrument.to_string(),
            content_fingerprint: primary_fingerprint.clone(),
            start_ts_ms: *primary
                .ts
                .first()
                .ok_or_else(|| invalid("多腿 primary BarFrame 缺少首时间戳"))?,
            end_ts_ms: *primary
                .ts
                .last()
                .ok_or_else(|| invalid("多腿 primary BarFrame 缺少末时间戳"))?,
        },
        CanonicalDatasetIdentity {
            role: "bars:reference".into(),
            instrument: reference.instrument.to_string(),
            content_fingerprint: reference_fingerprint.clone(),
            start_ts_ms: *reference
                .ts
                .first()
                .ok_or_else(|| invalid("多腿 reference BarFrame 缺少首时间戳"))?,
            end_ts_ms: *reference
                .ts
                .last()
                .ok_or_else(|| invalid("多腿 reference BarFrame 缺少末时间戳"))?,
        },
    ];
    let provider_provenance = funding_provider
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| invariant(format!("资金费 Provider provenance 序列化失败: {error}")))?;
    let canonical = canonical_spec::build_multi_leg_canonical_run_spec(
        spec,
        datasets.clone(),
        dataset_quality.as_ref(),
        &primary_execution_spec,
        &reference_execution_spec,
        &strategy_id,
        &strategy_version,
        strategy_source_sha256.as_deref(),
        &shared_account_id,
        provider_provenance.as_ref(),
        context.code_commit(),
        &runtime_version,
        portfolio_report.clock_start,
        portfolio_report.clock_end,
    )?;
    let canonical_json = canonical
        .canonical_json()
        .map_err(|error| invariant(format!("多腿 CanonicalRunSpec JSON 编码失败: {error}")))?;
    let canonical_fingerprint = canonical
        .fingerprint()
        .map_err(|error| invariant(format!("多腿 CanonicalRunSpec fingerprint 失败: {error}")))?;
    let config_hash = canonical_fingerprint;
    let input_components = canonical_spec::multi_leg_input_components(
        &datasets,
        dataset_quality.as_ref(),
        provider_provenance.as_ref(),
    )?;
    let run_manifest = RunManifest {
        run_id: spec.run_id.clone(),
        code_commit: context.code_commit().to_string(),
        config_hash: config_hash.clone(),
        data_fingerprint: data_fingerprint.clone(),
        input_components,
        clock_start: portfolio_report.clock_start,
        clock_end: portfolio_report.clock_end,
        global_seed: spec.seed,
        determinism_mode: true,
        result_hash: shared_result_hash.clone(),
        strategy_version: strategy_version.clone(),
        instrument_spec_version: "multi-leg-market-rules-v1".into(),
        model_fingerprint: qx_strategy::sha256_hex(
            &serde_json::to_vec(&canonical.execution_model).map_err(|error| {
                invariant(format!("多腿 execution_model 指纹编码失败: {error}"))
            })?,
        ),
        input_event_hash: data_fingerprint.clone(),
        output_event_hash: event_hash.clone(),
        runtime_version,
        canonical_run_spec_json: Some(canonical_json),
        canonical_run_spec_fingerprint: Some(config_hash.clone()),
    };
    run_manifest.validate().map_err(invariant)?;
    let run_manifest_json = run_manifest
        .to_json()
        .map_err(|error| invariant(format!("多腿 RunManifest 编码失败: {error}")))?;
    let mut payload = serde_json::json!({
        "schema_version": if spec.schema_version == crate::MULTI_LEG_BACKTEST_EXACT_SCHEMA_VERSION { 6 } else { 5 },
        "run_id": spec.run_id,
        "account_id": shared_account_id,
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
        "dataset_quality": dataset_quality,
        "config_hash": config_hash,
        "shared_result_hash": shared_result_hash,
        "event_hash": event_hash,
        "clock_start": portfolio_report.clock_start,
        "clock_end": portfolio_report.clock_end,
        "run_manifest_path": run_manifest_path_string,
        "shared_event_log": portfolio_report.event_log.events(),
        "primary_result_hash": primary_result_hash.clone(),
        "reference_result_hash": reference_result_hash.clone(),
        "primary_return_bps": primary_return,
        "reference_return_bps": reference_return,
        "leg_pnl_exact": {
            "primary": primary_pnl_exact.to_string(),
            "reference": reference_pnl_exact.to_string(),
        },
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
        "assumptions": ["both legs share one account Ledger, OMS, EventLog, and buying-power pool", "same-timestamp callbacks are deterministic and market orders compete by declared priority then instrument id", "all market events are replay-verified together", "each venue can declare its own maker/taker rates with shared rates as fallback", "constant funding settles at default UTC 8-hour boundaries; historical funding uses each exchange-declared settlement timestamp and the last observed mark, before callbacks at that timestamp", "funding settlement fails closed if its latest mark is older than the event's declared funding interval, or the default 8-hour interval when omitted", "funding debit or receipt is included exactly once in ending equity and each settlement is present in replay facts", "funding input is caller-supplied constant bps or timestamped historical events; event inputs are recorded in this artifact and do not imply an exchange history provider", "missing market specs retain the legacy spot assumption and are represented by a normalized execution spec"],
    });
    let payload_object = payload
        .as_object_mut()
        .ok_or_else(|| invariant("多腿产物 JSON 根节点必须是对象"))?;
    if spec.schema_version == crate::MULTI_LEG_BACKTEST_EXACT_SCHEMA_VERSION {
        payload_object.insert(
            "initial_cash_exact".into(),
            serde_json::Value::String(initial_cash_exact.to_string()),
        );
        payload_object.insert(
            "leg_initial_cash_exact".into(),
            serde_json::json!({
                "primary": primary_initial_cash_exact.to_string(),
                "reference": reference_initial_cash_exact.to_string(),
            }),
        );
    }
    payload_object.insert("spec".into(), spec_artifact);
    payload_object.insert("market_data".into(), market_data);
    payload_object.insert("ledger_entries".into(), ledger_entries);
    payload_object.insert(
        "equity_curve".into(),
        serde_json::Value::Array(equity_curve),
    );
    payload_object.insert(
        "equity_curve_exact".into(),
        serde_json::Value::Array(equity_curve_exact),
    );
    funding::apply_artifact_metadata(&mut payload, spec, funding_provider)?;
    let payload = serde_json::to_string(&payload)
        .map_err(|error| invariant(format!("多腿产物序列化失败: {error}")))?;
    let manifest_exists = match std::fs::read_to_string(&run_manifest_path) {
        Ok(existing) if existing == run_manifest_json => true,
        Ok(_) => {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!(
                    "相同 run_id 的多腿 RunManifest 已存在且内容不同: {run_manifest_path_string}"
                ),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(AppError::from_io("读取既有多腿 RunManifest", &error)),
    };
    let artifact_exists = match std::fs::read_to_string(&artifact_path) {
        Ok(existing) if existing == payload => true,
        Ok(_) => {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                "相同 run_id 的多腿产物已存在且内容不同",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(AppError::from_io("读取既有多腿产物", &error)),
    };
    if !manifest_exists {
        write_text(&run_manifest_path_string, &run_manifest_json)?;
    }
    if !artifact_exists {
        write_text(&artifact_path, &payload)?;
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
