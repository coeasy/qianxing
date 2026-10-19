//! Shared Rust execution and evidence orchestration for depth runs.

use super::portfolio::{
    attach_portfolio_summary, build_portfolio_strategy, has_builtin_sizing, strategy_specs,
};
use super::{
    artifact_paths, invalid_input, publish, CallbackStrategy, InjectedDepthStrategy,
    DEFAULT_ACCOUNT_ID,
};
use crate::cases::artifacts::{equity_csv, existing_manifest, fills_csv, read_text};
use crate::run_handle::CancellationToken;
use crate::{AppError, AppErrorCategory, DepthBacktestOutcome, DepthBacktestSpec, RunContext};
use qx_core::{Fnv1a, Money, Quantity, ReplayVerifier, RunManifest};
use qx_strategy::{BuiltinStrategy, BuiltinStrategyConfig, Strategy, StrategyContext};
use qx_xingban::{
    depth_frame_to_ticks, DataTier, DepthBarStrategy, DepthFrame, MarketMakingStrategy,
    NativeOrderBookStrategy, NativeTickStrategy, OrderBookBacktestConfig, OrderBookBacktestEngine,
    OrderBookExecutionModel, RunManifestIdentity, TickBacktestConfig, TickBacktestEngine,
};
use qx_zhenlu::RiskGate;
use std::collections::BTreeMap;

pub(in crate::cases) fn run_inner(
    spec: &DepthBacktestSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
) -> Result<Option<DepthBacktestOutcome>, AppError> {
    run_inner_with_strategy(spec, context, cancellation, Vec::new())
}

pub(in crate::cases) fn run_inner_with_strategy(
    spec: &DepthBacktestSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    mut injected: Vec<InjectedDepthStrategy>,
) -> Result<Option<DepthBacktestOutcome>, AppError> {
    spec.validate()?;
    let mut seen_custom = std::collections::BTreeSet::new();
    for custom in &injected {
        if !seen_custom.insert(custom.identity.strategy_id.as_str()) {
            return Err(invalid_input(format!(
                "自定义 Tick/OrderBook 策略身份重复: {}",
                custom.identity.strategy_id
            )));
        }
        if custom.experiment_parameters {
            custom.identity.validate_for_depth_experiment(spec)?;
        } else {
            custom.identity.validate_for_depth(spec)?;
        }
        if strategy_specs(spec)
            .find(|strategy| strategy.strategy_id == custom.identity.strategy_id)
            .is_some_and(has_builtin_sizing)
        {
            return Err(invalid_input(format!(
                "自定义 Tick/OrderBook 策略 {} 不能同时声明仅适用于内置策略的 sizing 参数",
                custom.identity.strategy_id
            )));
        }
    }
    if spec.portfolio.is_none() && injected.len() > 1 {
        return Err(invalid_input(
            "多个自定义 Tick/OrderBook 策略必须声明同账户 portfolio 组合规格",
        ));
    }
    let payload = read_text(&spec.depth_path)?;
    let frame = DepthFrame::from_json(&payload).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("DepthFrame 校验失败 {}: {error}", spec.depth_path),
        )
    })?;
    if frame.snapshots.len() < 2 {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            "Tick/OrderBook 回测至少需要两份快照",
        ));
    }
    if spec.latency_snapshots > 0
        && spec.latency_snapshots >= (frame.snapshots.len() as u64).saturating_sub(1)
    {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            format!(
                "latency_snapshots={} 在 {} 份快照上无法成熟成交；最大有效延迟为 {}",
                spec.latency_snapshots,
                frame.snapshots.len(),
                (frame.snapshots.len() as u64).saturating_sub(2)
            ),
        ));
    }
    if spec.tier == "l1"
        && frame
            .snapshots
            .iter()
            .any(|snapshot| snapshot.bids.len() != 1 || snapshot.asks.len() != 1)
    {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            "L1 Tick 回测每份快照必须恰有一个 bid 和一个 ask 档位",
        ));
    }

    let instrument = frame.instrument.clone();
    if let Some(market_spec) = spec.market_spec.as_ref() {
        if market_spec.instrument != instrument {
            return Err(invalid_input(
                "market_spec.instrument 与 Tick/OrderBook 输入标的不一致",
            ));
        }
        let first_ts = frame
            .snapshots
            .first()
            .map(|snapshot| snapshot.ts)
            .ok_or_else(|| invalid_input("Tick/OrderBook DepthFrame has no snapshots"))?;
        let last_ts = frame
            .snapshots
            .last()
            .map(|snapshot| snapshot.ts)
            .ok_or_else(|| invalid_input("Tick/OrderBook DepthFrame has no snapshots"))?;
        if first_ts < market_spec.valid_from
            || market_spec
                .valid_to
                .is_some_and(|valid_to| last_ts >= valid_to)
        {
            return Err(invalid_input(format!(
                "market_spec 有效区间 [{}, {:?}) 未覆盖输入时间 [{first_ts}, {last_ts}]",
                market_spec.valid_from, market_spec.valid_to
            )));
        }
    }
    let data_fingerprint = format!("depth:{}:{:016x}", frame.source, frame.input_hash());
    let config_hash = {
        let mut hash = Fnv1a::new();
        hash.write_text(&spec.to_json()?);
        let mut identities = injected
            .iter()
            .map(|custom| &custom.identity)
            .collect::<Vec<_>>();
        identities.sort_by(|left, right| left.strategy_id.cmp(&right.strategy_id));
        for identity in identities {
            hash.write_text(&identity.strategy_id);
            hash.write_text(&identity.strategy_version);
            hash.write_text(&identity.source_sha256);
        }
        format!("{:016x}", hash.finish())
    };
    let strategy_id = injected
        .iter()
        .find(|custom| custom.identity.strategy_id == spec.strategy.strategy_id)
        .map(|custom| custom.identity.strategy_id.clone())
        .unwrap_or_else(|| spec.strategy.strategy_id.clone());
    let account_id = spec
        .account_id
        .as_deref()
        .unwrap_or(DEFAULT_ACCOUNT_ID)
        .to_string();
    let strategy_version = injected
        .iter()
        .find(|custom| custom.identity.strategy_id == spec.strategy.strategy_id)
        .map(|custom| custom.identity.strategy_version.clone())
        .unwrap_or_else(|| format!("{}-v1", strategy_id));
    let risk = RiskGate::conservative_default();
    let risk_rule_set_version = risk.rule_set().version().to_string();
    let mut model = OrderBookExecutionModel::new(spec.fee_bps).map_err(invalid_input)?;
    model.latency_snapshots = spec.latency_snapshots;
    model.queue_position_bps = spec.queue_position_bps;
    model.queue_position_model = spec.queue_position_model;
    model.market_impact_bps = spec.market_impact_bps;
    model.validate().map_err(invalid_input)?;
    let strategy_config = BuiltinStrategyConfig {
        fast_window: spec.strategy.fast_window,
        slow_window: spec.strategy.slow_window,
        period: spec.strategy.period,
        threshold_bps: spec.strategy.threshold_bps,
        target_notional_raw: spec.strategy.target_notional_raw,
        target_available_margin_bps: spec.strategy.target_available_margin_bps,
        target_volatility_bps: spec.strategy.target_volatility_bps,
        target_kelly_fraction_bps: spec.strategy.target_kelly_fraction_bps,
        kelly_lookback_bars: spec.strategy.kelly_lookback_bars,
        primary_policy: spec.order_policy,
        ..BuiltinStrategyConfig::new(
            spec.strategy.kind,
            strategy_id.clone(),
            instrument.clone(),
            Quantity::from_raw(spec.strategy.quantity_raw),
        )
        .map_err(invalid_input)?
    };
    strategy_config.validate().map_err(invalid_input)?;
    let strategy = BuiltinStrategy::new(strategy_config).map_err(invalid_input)?;
    let cash = BTreeMap::from([(spec.settlement_currency.clone(), spec.initial_cash_raw)]);
    let strategy_context = StrategyContext {
        strategy_id: strategy_id.clone(),
        strategy_version: strategy_version.clone(),
        account_id: account_id.clone(),
        venue_id: instrument.venue.to_string(),
        data_fingerprint: data_fingerprint.clone(),
        as_of: frame.snapshots[0].ts,
        positions: BTreeMap::new(),
        available_margin_raw: crate::cases::strategy_available_margin_raw(
            &cash,
            &spec.settlement_currency,
        ),
        cash,
        risk_state: "backtest".into(),
    };
    let mut depth_strategy =
        DepthBarStrategy::new(strategy, strategy_context.clone(), instrument.clone());
    depth_strategy.initialize().map_err(invalid_input)?;
    let standalone_custom = if spec.portfolio.is_none() {
        injected.pop()
    } else {
        None
    };
    let mut portfolio_strategy = spec
        .portfolio
        .as_ref()
        .map(|_| {
            build_portfolio_strategy(
                spec,
                &instrument,
                &account_id,
                &data_fingerprint,
                frame.snapshots[0].ts,
                injected,
            )
        })
        .transpose()?;

    let report = match spec.tier.as_str() {
        "l1" => {
            let ticks = depth_frame_to_ticks(&frame).map_err(invalid_input)?;
            let engine = TickBacktestEngine::new(TickBacktestConfig {
                instrument: instrument.clone(),
                account_id: account_id.clone(),
                currency: spec.settlement_currency.clone(),
                initial_cash: Money::from_raw(spec.initial_cash_raw),
                fee_bps: spec.fee_bps,
                instrument_spec: spec.market_spec.clone(),
                risk,
            })
            .with_execution_model(model);
            match (cancellation, portfolio_strategy.as_mut(), standalone_custom) {
                (_, _, Some(custom)) => {
                    let mut strategy = NativeTickStrategy::new(
                        CallbackStrategy(custom.strategy),
                        strategy_context.clone(),
                        instrument.clone(),
                    );
                    strategy.initialize().map_err(invalid_input)?;
                    match cancellation {
                        Some(cancellation) => engine
                            .run_with_cancel(&ticks, &mut strategy, || cancellation.is_cancelled())
                            .map_err(|error| AppError::from_qx_error(&error))?,
                        None => Some(
                            engine
                                .run(&ticks, &mut strategy)
                                .map_err(|error| AppError::from_qx_error(&error))?,
                        ),
                    }
                }
                (cancellation, portfolio, None) => match (cancellation, portfolio) {
                    (Some(cancellation), Some(strategy)) => engine
                        .run_with_cancel(&ticks, strategy, || cancellation.is_cancelled())
                        .map_err(|error| AppError::from_qx_error(&error))?,
                    (None, Some(strategy)) => Some(
                        engine
                            .run(&ticks, strategy)
                            .map_err(|error| AppError::from_qx_error(&error))?,
                    ),
                    (Some(cancellation), None) => engine
                        .run_with_cancel(&ticks, &mut depth_strategy, || {
                            cancellation.is_cancelled()
                        })
                        .map_err(|error| AppError::from_qx_error(&error))?,
                    (None, None) => Some(
                        engine
                            .run(&ticks, &mut depth_strategy)
                            .map_err(|error| AppError::from_qx_error(&error))?,
                    ),
                },
            }
        }
        "l2" => {
            let engine = OrderBookBacktestEngine::new(OrderBookBacktestConfig {
                instrument: instrument.clone(),
                account_id: account_id.clone(),
                currency: spec.settlement_currency.clone(),
                initial_cash: Money::from_raw(spec.initial_cash_raw),
                fee_bps: spec.fee_bps,
                instrument_spec: spec.market_spec.clone(),
                risk,
                data_tier: DataTier::L2L3,
            })
            .with_execution_model(model);
            if let Some(config) = spec.market_making {
                let mut maker = MarketMakingStrategy::new(
                    config,
                    instrument.clone(),
                    &account_id,
                    strategy_id.clone(),
                    spec.order_policy,
                )
                .map_err(invalid_input)?;
                match cancellation {
                    Some(cancellation) => engine
                        .run_with_cancel(&frame.snapshots, &mut maker, || {
                            cancellation.is_cancelled()
                        })
                        .map_err(|error| AppError::from_qx_error(&error))?,
                    None => Some(
                        engine
                            .run(&frame.snapshots, &mut maker)
                            .map_err(|error| AppError::from_qx_error(&error))?,
                    ),
                }
            } else if let Some(custom) = standalone_custom {
                let mut strategy = NativeOrderBookStrategy::new(
                    CallbackStrategy(custom.strategy),
                    strategy_context.clone(),
                );
                strategy
                    .strategy
                    .on_init(&strategy.context)
                    .map_err(invalid_input)?;
                match cancellation {
                    Some(cancellation) => engine
                        .run_with_cancel(&frame.snapshots, &mut strategy, || {
                            cancellation.is_cancelled()
                        })
                        .map_err(|error| AppError::from_qx_error(&error))?,
                    None => Some(
                        engine
                            .run(&frame.snapshots, &mut strategy)
                            .map_err(|error| AppError::from_qx_error(&error))?,
                    ),
                }
            } else if let Some(strategy) = portfolio_strategy.as_mut() {
                match cancellation {
                    Some(cancellation) => engine
                        .run_with_cancel(&frame.snapshots, strategy, || cancellation.is_cancelled())
                        .map_err(|error| AppError::from_qx_error(&error))?,
                    None => Some(
                        engine
                            .run(&frame.snapshots, strategy)
                            .map_err(|error| AppError::from_qx_error(&error))?,
                    ),
                }
            } else {
                match cancellation {
                    Some(cancellation) => engine
                        .run_with_cancel(&frame.snapshots, &mut depth_strategy, || {
                            cancellation.is_cancelled()
                        })
                        .map_err(|error| AppError::from_qx_error(&error))?,
                    None => Some(
                        engine
                            .run(&frame.snapshots, &mut depth_strategy)
                            .map_err(|error| AppError::from_qx_error(&error))?,
                    ),
                }
            }
        }
        _ => unreachable!("DepthBacktestSpec::validate constrains tier"),
    };
    let Some(report) = report else {
        return Ok(None);
    };

    let replay =
        ReplayVerifier::verify(report.event_log.events(), &report.ledger).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("Tick/OrderBook 事件日志未通过重放校验: {error}"),
            )
        })?;
    if replay.log_digest != report.result_hash() {
        return Err(AppError::new(
            AppErrorCategory::InternalInvariant,
            "Tick/OrderBook 结果哈希与重放哈希不一致",
        ));
    }
    if report.snapshot_ts.len() != report.equity.len() {
        return Err(AppError::new(
            AppErrorCategory::InternalInvariant,
            "Tick/OrderBook 权益曲线与输入快照数量不一致",
        ));
    }

    let artifacts = artifact_paths(spec);
    if let Some(existing) = existing_manifest(&artifacts.run_manifest)? {
        let existing = RunManifest::from_json(&existing).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("既有 RunManifest 无法解析: {error}"),
            )
        })?;
        if existing.config_hash != config_hash || existing.data_fingerprint != data_fingerprint {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("run_id {} 已绑定到不同的 Tick/OrderBook 输入", spec.run_id),
            ));
        }
    }
    let manifest = report
        .run_manifest(
            RunManifestIdentity {
                run_id: &spec.run_id,
                code_commit: context.code_commit(),
                config_hash: &config_hash,
                strategy_version: &strategy_version,
                instrument_spec_version: if spec.market_spec.is_some() {
                    "trading-instrument-spec-v1"
                } else {
                    "instrument-spec:none"
                },
                runtime_version: &format!("qx-app-{}", env!("CARGO_PKG_VERSION")),
            },
            &data_fingerprint,
        )
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("Tick/OrderBook RunManifest 构建失败: {error}"),
            )
        })?;
    std::fs::create_dir_all(&spec.output_dir)
        .map_err(|error| AppError::from_io(&format!("创建产物目录 {}", spec.output_dir), &error))?;

    let mut summary = serde_json::json!({
        "schema_version": 1,
        "run_id": spec.run_id,
        "instrument": instrument.to_string(),
        "tier": spec.tier,
        "market_spec": spec.market_spec,
        "order_policy": spec.order_policy,
        "market_making": spec.market_making,
        "strategy_id": strategy_id,
        "strategy_version": strategy_version,
        "snapshots": report.snapshot_ts.len(),
        "fills": report.fills.len(),
        "equity_points": report.equity.len(),
        "result_hash": format!("{:016x}", report.result_hash()),
        "data_fingerprint": data_fingerprint,
        "config_hash": config_hash,
        "return_bps": report.return_bps,
        "max_drawdown_bps": report.max_drawdown_bps,
        "final_equity_raw": report.final_equity(),
        "fees_raw": report.fees_raw,
        "turnover_raw": report.turnover_raw,
        "replay_events": replay.events,
        "replay_ledger_entries": replay.ledger_entries,
        "ledger_entries": report.ledger.entries().len(),
        "risk_rule_set_version": risk_rule_set_version,
        "execution_model": report.execution_model_descriptor,
        "model_descriptors": report.model_descriptors,
        "assumptions": report.assumptions,
    });
    attach_portfolio_summary(&mut summary, spec, &report.fills)?;
    if spec.account_id.is_some() {
        if let Some(object) = summary.as_object_mut() {
            object.insert("account_id".into(), serde_json::json!(account_id));
        }
    }
    let summary = serde_json::to_string(&summary).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("Tick/OrderBook 摘要序列化失败: {error}"),
        )
    })?;
    publish(&artifacts.summary, &summary)?;
    publish(
        &artifacts.equity,
        &equity_csv(&report.snapshot_ts, &report.equity),
    )?;
    publish(&artifacts.fills, &fills_csv(&report.fills))?;
    let manifest_json = manifest.to_json().map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("Tick/OrderBook RunManifest 序列化失败: {error}"),
        )
    })?;
    publish(&artifacts.run_manifest, &manifest_json)?;

    Ok(Some(DepthBacktestOutcome {
        run_id: spec.run_id.clone(),
        instrument: instrument.to_string(),
        tier: spec.tier.clone(),
        result_hash: format!("{:016x}", report.result_hash()),
        data_fingerprint,
        fills: report.fills.len() as u64,
        equity_points: report.equity.len() as u64,
        return_bps: report.return_bps,
        max_drawdown_bps: report.max_drawdown_bps,
        artifacts,
        account_id: spec.account_id.clone(),
    }))
}
